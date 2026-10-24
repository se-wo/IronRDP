//! Server-side connection sequence ([`Acceptor`]) state-machine oracle.

use std::borrow::Cow;
use std::sync::OnceLock;

use ironrdp_acceptor::{Acceptor, DesktopSize, MultitransportSecurityRng};
use ironrdp_connector::Sequence as _;
use ironrdp_core::{Encode, WriteBuf, decode, encode_vec, impl_as_any};
use ironrdp_pdu::gcc::{
    ChannelDef, ChannelName, ChannelOptions, ClientCoreData, ClientCoreOptionalData, ClientEarlyCapabilityFlags,
    ClientGccBlocks, ClientMessageChannelData, ClientNetworkData, ClientSecurityData, ColorDepth, EncryptionMethod,
    HighColorDepth, KeyboardType, MultiTransportChannelData, MultiTransportFlags, RdpVersion, SecureAccessSequence,
    SupportedColorDepths,
};
use ironrdp_pdu::mcs::{self, ConnectInitial};
use ironrdp_pdu::nego::{self, SecurityProtocol};
use ironrdp_pdu::rdp::ClientInfoPdu;
use ironrdp_pdu::rdp::capability_sets::{self, CapabilitySet, ClientConfirmActive, DemandActive};
use ironrdp_pdu::rdp::client_info::{
    AddressFamily, ClientInfo, ClientInfoFlags, CompressionType, Credentials, ExtendedClientInfo,
    ExtendedClientOptionalInfo,
};
use ironrdp_pdu::rdp::finalization_messages::{ControlAction, ControlPdu, FontPdu, SynchronizePdu};
use ironrdp_pdu::rdp::headers::{
    BasicSecurityHeader, BasicSecurityHeaderFlags, CompressionFlags, ShareControlHeader, ShareControlPdu,
    ShareDataHeader, ShareDataPdu, StreamPriority,
};
use ironrdp_pdu::rdp::multitransport::MultitransportResponsePdu;
use ironrdp_pdu::x224::{X224, X224Data};
use ironrdp_pdu::{PduResult, find_size};
use ironrdp_svc::{StaticChannelId, SvcMessage, SvcProcessor, SvcServerProcessor};

/// Drives an [`Acceptor`] (the server side of the [1.3.1.1] Connection
/// Sequence) with a client byte stream that starts out valid and turns into
/// fuzzer input partway through.
///
/// RDP's connection sequence only reaches a deep state after a valid prefix,
/// so feeding raw bytes to a fresh acceptor almost never gets past the first
/// decoder. Instead, the oracle replays the first frames of a valid client
/// handshake, then hands the rest of the stream over to the fuzzer.
///
/// Input layout:
///
/// - byte 0: how many frames of the valid handshake to replay, modulo the
///   handshake length plus one, so that the whole handshake can be replayed.
///   The handshake depends on the configuration byte.
/// - byte 1: configuration bits that vary the handshake, see `Config`.
/// - remaining bytes: the client byte stream from that point on, framed with
///   [`Sequence::next_pdu_hint`] the way the server's I/O driver frames it.
///
/// The security protocol is always TLS, so CredSSP never runs: it needs a
/// real authentication exchange this oracle cannot replay. The TLS upgrade
/// itself is marked as done without any I/O, as a server does after its
/// handshake completes. Client bytes that arrive while the upgrade is in
/// progress are therefore out of reach: buffering them belongs to the I/O
/// driver, which this oracle does not run.
///
/// With `Config::REACTIVATION`, the server runs a Deactivation-Reactivation
/// Sequence after accepting the connection: it rebuilds the acceptor with
/// [`Acceptor::new_deactivation_reactivation`], as `ironrdp-server` does on a
/// resize, and the client answers the new Demand Active again. The replayed
/// handshake then continues with that second round, and the fuzzer input
/// takes over in whichever round the replay ends.
///
/// Besides the absence of panics, every step must satisfy:
///
/// - The bytes written split into complete PDUs according to their own
///   length fields, and the PDU lengths add up to the size the step reports.
/// - Each PDU written decodes as what the state that wrote it sends.
/// - The state never moves backwards in the connection sequence.
/// - Reaching the accepted state yields a result, and unless the server was
///   configured to accept any credentials, the result carries exactly the
///   expected ones.
/// - The result of a reactivation is marked as one and keeps the MCS channel
///   IDs and the static channel binding of the connection it reactivates.
///
/// The oracle stops at the first error, as the server's driver does:
/// stepping a consumed or accepted acceptor is a usage error, not a protocol
/// one.
///
/// [1.3.1.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/023f1e69-cfe8-4ee6-9ee0-7e759fb4e4ee
/// [`Sequence::next_pdu_hint`]: ironrdp_connector::Sequence::next_pdu_hint
pub fn acceptor_state_machine(data: &[u8]) {
    let [replay_len, config, stream @ ..] = data else {
        return;
    };
    let mut stream = stream;

    // Building the handshake encodes every frame of it, so it is built once
    // per configuration rather than for every input.
    static HANDSHAKES: [OnceLock<ClientHandshake>; 256] = [const { OnceLock::new() }; 256];

    let config = Config(*config);
    let handshake = HANDSHAKES[usize::from(config.0)].get_or_init(|| client_handshake(config));
    let replay_len = usize::from(*replay_len) % (handshake.len() + 1);
    let connection_replay_len = replay_len.min(handshake.connection.len());
    let mut replay = handshake.connection[..connection_replay_len].iter();
    let reactivation_replay = handshake.reactivation[..replay_len - connection_replay_len].iter();

    // Channel state of the accepted connection, once a reactivation started.
    let mut reactivated: Option<ChannelState> = None;

    let mut acceptor = Acceptor::new(
        SecurityProtocol::SSL,
        SERVER_DESKTOP_SIZE,
        vec![CapabilitySet::Bitmap(capability_sets::Bitmap {
            pref_bits_per_pix: 32,
            desktop_width: SERVER_DESKTOP_SIZE.width,
            desktop_height: SERVER_DESKTOP_SIZE.height,
            desktop_resize_flag: true,
            drawing_flags: capability_sets::BitmapDrawingFlags::empty(),
        })],
        (!config.has(Config::ANY_CREDENTIALS)).then(client_credentials),
    );
    acceptor.set_multitransport_security_rng(Box::new(FixedSecurityRng));
    if config.has(Config::MULTITRANSPORT) {
        acceptor.set_multitransport_offer(Some(MULTITRANSPORT_FLAGS));
    }
    if config.has(Config::HONOR_DESKTOP_SIZE) {
        acceptor.set_honor_client_desktop_size(Some(MAX_HONORED_DESKTOP_SIZE));
    }
    if config.has(Config::STATIC_CHANNELS) {
        acceptor.attach_static_channel(FuzzChannel);
    }

    let mut output = WriteBuf::new();

    loop {
        if acceptor.state().is_terminal() {
            assert!(
                replay.next().is_none(),
                "the acceptor accepted the connection before the replayed handshake ended"
            );

            let result = acceptor
                .get_result()
                .unwrap_or_else(|| panic!("an accepted acceptor returned no result"));
            let channel_state = ChannelState {
                user_channel_id: result.user_channel_id,
                io_channel_id: result.io_channel_id,
                message_channel_id: result.message_channel_id,
                fuzz_channel_id: result.static_channels.get_channel_id_by_type::<FuzzChannel>(),
            };

            if let Some(accepted) = reactivated {
                assert!(
                    result.reactivation,
                    "the reactivated acceptor returned a result not marked as one"
                );
                assert_eq!(
                    channel_state, accepted,
                    "the reactivation changed the channel state of the connection"
                );
                return;
            }

            assert!(
                !result.reactivation,
                "the first acceptance returned a result marked as a reactivation"
            );

            if !config.has(Config::ANY_CREDENTIALS) {
                assert_eq!(
                    result.credentials,
                    Some(client_credentials()),
                    "the acceptor accepted a connection whose credentials differ from the expected ones"
                );
            }

            if !config.has(Config::REACTIVATION) {
                return;
            }

            acceptor =
                Acceptor::new_deactivation_reactivation(acceptor, result.static_channels, REACTIVATION_DESKTOP_SIZE)
                    .unwrap_or_else(|e| panic!("failed to rebuild the accepted acceptor for a reactivation: {e}"));
            replay = reactivation_replay.clone();
            reactivated = Some(channel_state);
            continue;
        }

        let Some(hint) = acceptor.next_pdu_hint() else {
            if acceptor.reached_security_upgrade().is_some() {
                acceptor.mark_security_upgrade_as_done();
                continue;
            }

            if !checked_step(&mut acceptor, None, &mut output) {
                return;
            }

            continue;
        };

        if let Some(frame) = replay.next() {
            let state = acceptor.state().name();
            assert_eq!(
                state, frame.state,
                "the replayed handshake reached {state} where it expected {}",
                frame.state
            );
            assert!(
                checked_step(&mut acceptor, Some(&frame.bytes), &mut output),
                "the acceptor rejected a valid {} frame from the replayed handshake",
                frame.state
            );
            continue;
        }

        // A fully replayed handshake leaves nothing for the acceptor to read
        // before it accepts the connection.
        assert!(
            replay_len < handshake.len(),
            "the acceptor waits for more input in {} after the whole replayed handshake",
            acceptor.state().name()
        );

        // Frame the stream the way the server's I/O driver does: a PDU the
        // hint does not match is read and dropped, and a stream that ends
        // mid-PDU leaves the acceptor waiting for more bytes.
        let Ok(Some((matched, length))) = hint.find_size(stream) else {
            return;
        };
        if length == 0 || stream.len() < length {
            return;
        }
        let (frame, rest) = stream.split_at(length);
        stream = rest;

        if matched && !checked_step(&mut acceptor, Some(frame), &mut output) {
            return;
        }
    }

    /// Runs one acceptor step and checks the output and state transition.
    ///
    /// Returns whether the step succeeded.
    fn checked_step(acceptor: &mut Acceptor, input: Option<&[u8]>, output: &mut WriteBuf) -> bool {
        let state_before = acceptor.state().name();

        output.clear();
        let result = match input {
            Some(input) => acceptor.step(input, None, output),
            None => acceptor.step_no_input(output),
        };

        // A failing step may still write a PDU telling the client why, such as
        // an RDP Negotiation Failure or a Set Error Info PDU. The async driver
        // currently drops the output of a failing step, but a driver that
        // sends it must not send a malformed PDU, so it is checked either way.
        let mut written = output.filled();
        while !written.is_empty() {
            let pdu_info = find_size(written)
                .unwrap_or_else(|e| panic!("{state_before} wrote bytes that do not start with a PDU header: {e}"))
                .unwrap_or_else(|| panic!("{state_before} wrote a truncated PDU header"));
            assert!(
                pdu_info.length <= written.len(),
                "{state_before} wrote a PDU whose length field ({}) exceeds the bytes written after it ({})",
                pdu_info.length,
                written.len()
            );

            let (pdu, rest) = written.split_at(pdu_info.length);
            written = rest;

            match state_before {
                "InitiationSendConfirm" => {
                    if let Err(e) = decode::<X224<nego::ConnectionConfirm>>(pdu) {
                        panic!("{state_before} wrote a PDU that is not a Connection Confirm: {e}");
                    }
                }
                "BasicSettingsSendResponse" => {
                    let payload = decode::<X224<X224Data<'_>>>(pdu)
                        .unwrap_or_else(|e| panic!("{state_before} wrote a PDU that is not X.224 data: {e}"))
                        .0;
                    if let Err(e) = decode::<mcs::ConnectResponse>(payload.data.as_ref()) {
                        panic!("{state_before} wrote a PDU that is not an MCS Connect Response: {e}");
                    }
                }
                _ => {
                    let message = decode::<X224<mcs::McsMessage<'_>>>(pdu)
                        .unwrap_or_else(|e| panic!("{state_before} wrote a PDU that is not an MCS message: {e}"))
                        .0;
                    let expected = if state_before == "ChannelConnection" {
                        matches!(
                            message,
                            mcs::McsMessage::AttachUserConfirm(_) | mcs::McsMessage::ChannelJoinConfirm(_)
                        )
                    } else {
                        matches!(message, mcs::McsMessage::SendDataIndication(_))
                    };
                    assert!(expected, "{state_before} wrote an unexpected MCS message: {message:?}");
                }
            }
        }

        let Ok(written) = result else {
            return false;
        };

        assert_eq!(
            output.filled_len(),
            written.size().unwrap_or(0),
            "{state_before} reported {written:?} but wrote {} bytes",
            output.filled_len()
        );

        let state_after = acceptor.state().name();
        assert!(
            sequence_position(state_before) <= sequence_position(state_after),
            "the acceptor moved backwards from {state_before} to {state_after}"
        );

        true
    }

    /// Position of the named state in the connection sequence.
    fn sequence_position(state: &str) -> usize {
        SEQUENCE_ORDER
            .iter()
            .position(|name| *name == state)
            .unwrap_or_else(|| panic!("unknown acceptor state {state}; add it to SEQUENCE_ORDER"))
    }
}

/// Acceptor state names, in the order the connection sequence goes through them.
///
/// `Connected` is the name of the accepted state.
const SEQUENCE_ORDER: [&str; 16] = [
    "InitiationWaitRequest",
    "InitiationSendConfirm",
    "SecurityUpgrade",
    "Credssp",
    "BasicSettingsWaitInitial",
    "BasicSettingsSendResponse",
    "ChannelConnection",
    "RdpSecurityCommencement",
    "SecureSettingsExchange",
    "LicensingExchange",
    "MultitransportBootstrapping",
    "CapabilitiesSendServer",
    "MonitorLayoutSend",
    "CapabilitiesWaitConfirm",
    "ConnectionFinalization",
    "Connected",
];

const SERVER_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1024,
    height: 768,
};

const CLIENT_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1280,
    height: 1024,
};

const MAX_HONORED_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1920,
    height: 1080,
};

/// Desktop size the server switches to in a Deactivation-Reactivation Sequence.
const REACTIVATION_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1600,
    height: 900,
};

const USER_CHANNEL_ID: u16 = 1002;
const IO_CHANNEL_ID: u16 = 1003;

const MULTITRANSPORT_FLAGS: MultiTransportFlags =
    MultiTransportFlags::TRANSPORT_TYPE_UDP_FECR.union(MultiTransportFlags::SOFT_SYNC_TCP_TO_UDP);

const MULTITRANSPORT_REQUEST_ID: u32 = 0x1234_5678;

/// Static channels the client asks for when [`Config::STATIC_CHANNELS`] is set.
///
/// The server only has a processor for the first one, so the acceptor assigns
/// an ID to both but only binds the first.
const CLIENT_STATIC_CHANNELS: [&[u8; 8]; 2] = [b"fuzzsvc\0", b"unknown\0"];

/// Variations of the handshake, selected by the second input byte.
#[derive(Clone, Copy)]
struct Config(u8);

impl Config {
    /// The client requests an MCS message channel.
    const MESSAGE_CHANNEL: u8 = 1 << 0;
    /// Both sides advertise UDP multitransport. With a message channel, the
    /// server sends an Initiate Multitransport Request and the client answers.
    const MULTITRANSPORT: u8 = 1 << 1;
    /// The client supports skipping the MCS channel joins.
    const SKIP_CHANNEL_JOIN: u8 = 1 << 2;
    /// The client supports the Monitor Layout PDU.
    const MONITOR_LAYOUT: u8 = 1 << 3;
    /// The client asks for static virtual channels, see [`CLIENT_STATIC_CHANNELS`].
    const STATIC_CHANNELS: u8 = 1 << 4;
    /// The server adopts the desktop size the client asks for.
    const HONOR_DESKTOP_SIZE: u8 = 1 << 5;
    /// The server accepts any credentials. Otherwise it expects the ones the
    /// valid handshake sends, so the oracle can check it never accepts others.
    const ANY_CREDENTIALS: u8 = 1 << 6;
    /// After accepting the connection, the server runs a
    /// Deactivation-Reactivation Sequence, and the client answers it.
    const REACTIVATION: u8 = 1 << 7;

    fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// Valid client side of a connection sequence, see [`client_handshake`].
struct ClientHandshake {
    /// Frames up to the accepted connection.
    connection: Vec<ClientFrame>,
    /// Frames answering a Deactivation-Reactivation Sequence, empty unless
    /// [`Config::REACTIVATION`] is set.
    reactivation: Vec<ClientFrame>,
}

impl ClientHandshake {
    fn len(&self) -> usize {
        self.connection.len() + self.reactivation.len()
    }
}

/// One client-to-server frame of the valid handshake.
#[derive(Clone)]
struct ClientFrame {
    /// Name of the acceptor state that reads this frame.
    state: &'static str,
    bytes: Vec<u8>,
}

/// Builds the valid client side of a TLS connection sequence for `config`.
fn client_handshake(config: Config) -> ClientHandshake {
    let static_channels: &[&[u8; 8]] = if config.has(Config::STATIC_CHANNELS) {
        &CLIENT_STATIC_CHANNELS
    } else {
        &[]
    };

    // The acceptor numbers every channel the client asks for after the I/O
    // channel, in request order, then allocates the message channel.
    let mut next_channel_ids = IO_CHANNEL_ID + 1..;
    let static_channel_ids: Vec<u16> = next_channel_ids.by_ref().take(static_channels.len()).collect();
    let message_channel_id = config.has(Config::MESSAGE_CHANNEL).then_some(next_channel_ids.start);

    let mut early_capability_flags =
        ClientEarlyCapabilityFlags::SUPPORT_ERR_INFO_PDU | ClientEarlyCapabilityFlags::WANT_32_BPP_SESSION;
    if config.has(Config::SKIP_CHANNEL_JOIN) {
        early_capability_flags |= ClientEarlyCapabilityFlags::SUPPORT_SKIP_CHANNELJOIN;
    }
    if config.has(Config::MONITOR_LAYOUT) {
        early_capability_flags |= ClientEarlyCapabilityFlags::SUPPORT_MONITOR_LAYOUT_PDU;
    }

    let gcc_blocks = ClientGccBlocks {
        core: ClientCoreData {
            version: RdpVersion::V5_PLUS,
            desktop_width: CLIENT_DESKTOP_SIZE.width,
            desktop_height: CLIENT_DESKTOP_SIZE.height,
            color_depth: ColorDepth::Bpp8,
            sec_access_sequence: SecureAccessSequence::Del,
            keyboard_layout: 0x409,
            client_build: 0,
            client_name: "fuzz".to_owned(),
            keyboard_type: KeyboardType::IBM_ENHANCED,
            keyboard_subtype: 0,
            keyboard_functional_keys_count: 12,
            ime_file_name: String::new(),
            optional_data: ClientCoreOptionalData {
                post_beta2_color_depth: Some(ColorDepth::Bpp8),
                client_product_id: Some(1),
                serial_number: Some(0),
                high_color_depth: Some(HighColorDepth::Bpp24),
                supported_color_depths: Some(SupportedColorDepths::BPP32 | SupportedColorDepths::BPP24),
                early_capability_flags: Some(early_capability_flags),
                ..Default::default()
            },
        },
        security: ClientSecurityData {
            encryption_methods: EncryptionMethod::empty(),
            ext_encryption_methods: 0,
        },
        network: (!static_channels.is_empty()).then(|| ClientNetworkData {
            channels: static_channels
                .iter()
                .map(|name| ChannelDef {
                    name: ChannelName::from_static(name),
                    options: ChannelOptions::INITIALIZED,
                })
                .collect(),
        }),
        cluster: None,
        monitor: None,
        message_channel: config.has(Config::MESSAGE_CHANNEL).then_some(ClientMessageChannelData),
        multi_transport_channel: config.has(Config::MULTITRANSPORT).then_some(MultiTransportChannelData {
            flags: MULTITRANSPORT_FLAGS,
        }),
        monitor_extended: None,
    };

    let mut frames = vec![
        ClientFrame {
            state: "InitiationWaitRequest",
            bytes: encode_frame(&X224(nego::ConnectionRequest {
                nego_data: None,
                flags: nego::RequestFlags::empty(),
                protocol: SecurityProtocol::SSL | SecurityProtocol::HYBRID,
                correlation_info: None,
            })),
        },
        ClientFrame {
            state: "BasicSettingsWaitInitial",
            bytes: {
                let connect_initial = ConnectInitial::with_gcc_blocks(gcc_blocks)
                    .unwrap_or_else(|e| panic!("failed to build the MCS Connect Initial: {e}"));
                encode_frame(&X224(X224Data {
                    data: Cow::Owned(encode_frame(&connect_initial)),
                }))
            },
        },
        ClientFrame {
            state: "ChannelConnection",
            bytes: encode_frame(&X224(mcs::ErectDomainPdu {
                sub_height: 0,
                sub_interval: 0,
            })),
        },
        ClientFrame {
            state: "ChannelConnection",
            bytes: encode_frame(&X224(mcs::AttachUserRequest)),
        },
    ];

    if !config.has(Config::SKIP_CHANNEL_JOIN) {
        let channel_ids = [USER_CHANNEL_ID, IO_CHANNEL_ID]
            .into_iter()
            .chain(static_channel_ids)
            .chain(message_channel_id);
        frames.extend(channel_ids.map(|channel_id| ClientFrame {
            state: "ChannelConnection",
            bytes: encode_frame(&X224(mcs::ChannelJoinRequest {
                initiator_id: USER_CHANNEL_ID,
                channel_id,
            })),
        }));
    }

    frames.push(ClientFrame {
        state: "SecureSettingsExchange",
        bytes: send_data_request(
            IO_CHANNEL_ID,
            &ClientInfoPdu {
                security_header: BasicSecurityHeader {
                    flags: BasicSecurityHeaderFlags::INFO_PKT,
                },
                client_info: ClientInfo {
                    credentials: client_credentials(),
                    password_cookie: None,
                    code_page: 0,
                    flags: ClientInfoFlags::MOUSE | ClientInfoFlags::UNICODE | ClientInfoFlags::LOGON_NOTIFY,
                    compression_type: CompressionType::K8,
                    alternate_shell: String::new(),
                    work_dir: String::new(),
                    extra_info: ExtendedClientInfo {
                        address_family: AddressFamily::INET,
                        address: "192.0.2.1".to_owned(),
                        dir: String::new(),
                        optional_data: ExtendedClientOptionalInfo::default(),
                    },
                },
            },
        ),
    });

    // The server only sends an Initiate Multitransport Request on the message
    // channel; the client answers before reading the Demand Active.
    if let Some(message_channel_id) = message_channel_id.filter(|_| config.has(Config::MULTITRANSPORT)) {
        frames.push(ClientFrame {
            state: "CapabilitiesWaitConfirm",
            bytes: send_data_request(
                message_channel_id,
                &MultitransportResponsePdu {
                    security_header: BasicSecurityHeader {
                        flags: BasicSecurityHeaderFlags::TRANSPORT_RSP,
                    },
                    request_id: MULTITRANSPORT_REQUEST_ID,
                    hr_response: 0, // S_OK
                },
            ),
        });
    }

    // The client answers every Demand Active the same way, whether it comes
    // during the connection sequence or a Deactivation-Reactivation Sequence.
    let mut activation = vec![ClientFrame {
        state: "CapabilitiesWaitConfirm",
        bytes: send_data_request(
            IO_CHANNEL_ID,
            &ShareControlHeader {
                share_id: 0,
                pdu_source: USER_CHANNEL_ID,
                share_control_pdu: ShareControlPdu::ClientConfirmActive(ClientConfirmActive {
                    originator_id: capability_sets::SERVER_CHANNEL_ID,
                    pdu: DemandActive {
                        source_descriptor: "MSTSC".to_owned(),
                        capability_sets: Vec::new(),
                    },
                }),
            },
        ),
    }];

    let finalization = [
        ShareDataPdu::Synchronize(SynchronizePdu {
            target_user_id: USER_CHANNEL_ID,
        }),
        ShareDataPdu::Control(ControlPdu {
            action: ControlAction::Cooperate,
            grant_id: 0,
            control_id: 0,
        }),
        ShareDataPdu::Control(ControlPdu {
            action: ControlAction::RequestControl,
            grant_id: 0,
            control_id: 0,
        }),
        ShareDataPdu::FontList(FontPdu::default()),
    ];
    activation.extend(finalization.into_iter().map(|share_data_pdu| ClientFrame {
        state: "ConnectionFinalization",
        bytes: send_data_request(
            IO_CHANNEL_ID,
            &ShareControlHeader {
                share_id: 0,
                pdu_source: USER_CHANNEL_ID,
                share_control_pdu: ShareControlPdu::Data(ShareDataHeader {
                    share_data_pdu,
                    stream_priority: StreamPriority::Undefined,
                    compression_flags: CompressionFlags::empty(),
                    compression_type: CompressionType::K8,
                }),
            },
        ),
    }));

    let reactivation = if config.has(Config::REACTIVATION) {
        activation.clone()
    } else {
        Vec::new()
    };
    frames.extend(activation);

    return ClientHandshake {
        connection: frames,
        reactivation,
    };

    fn encode_frame(pdu: &dyn Encode) -> Vec<u8> {
        encode_vec(pdu).unwrap_or_else(|e| panic!("failed to encode a handshake frame: {e}"))
    }

    fn send_data_request(channel_id: u16, user_data: &dyn Encode) -> Vec<u8> {
        encode_frame(&X224(mcs::SendDataRequest {
            initiator_id: USER_CHANNEL_ID,
            channel_id,
            user_data: Cow::Owned(encode_frame(user_data)),
        }))
    }
}

/// Channel state an accepted connection keeps across reactivations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelState {
    user_channel_id: u16,
    io_channel_id: u16,
    message_channel_id: Option<u16>,
    /// Channel ID bound to [`FuzzChannel`], if the client asked for it.
    fuzz_channel_id: Option<StaticChannelId>,
}

fn client_credentials() -> Credentials {
    Credentials {
        username: "user".to_owned(),
        password: "password".to_owned(),
        domain: None,
    }
}

/// Deterministic security cookie and request ID, so the same input always
/// produces the same Initiate Multitransport Request.
struct FixedSecurityRng;

impl MultitransportSecurityRng for FixedSecurityRng {
    fn fill_security_cookie(&mut self, cookie: &mut [u8; 16]) {
        cookie.fill(0x5A);
    }

    fn next_request_id(&mut self) -> u32 {
        MULTITRANSPORT_REQUEST_ID
    }
}

/// Server-side processor for the first of [`CLIENT_STATIC_CHANNELS`].
#[derive(Debug)]
struct FuzzChannel;

impl_as_any!(FuzzChannel);

impl SvcProcessor for FuzzChannel {
    fn channel_name(&self) -> ChannelName {
        ChannelName::from_static(CLIENT_STATIC_CHANNELS[0])
    }

    fn process(&mut self, _payload: &[u8]) -> PduResult<Vec<SvcMessage>> {
        Ok(Vec::new())
    }
}

impl SvcServerProcessor for FuzzChannel {}
