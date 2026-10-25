//! Server-side connection sequence ([`Acceptor`]) state-machine oracle.

use std::borrow::Cow;
use std::sync::OnceLock;

use arbitrary::Unstructured;

use ironrdp_acceptor::{Acceptor, DesktopSize, MultitransportSecurityRng};
use ironrdp_connector::Sequence as _;
use ironrdp_core::{Encode, WriteBuf, decode, impl_as_any};
use ironrdp_pdu::gcc::{
    ChannelDef, ChannelName, ChannelOptions, ClientCoreData, ClientCoreOptionalData, ClientEarlyCapabilityFlags,
    ClientGccBlocks, ClientMessageChannelData, ClientNetworkData, ClientSecurityData, ColorDepth, ConnectionType,
    EncryptionMethod, HighColorDepth, KeyboardType, MonitorFlags, MultiTransportChannelData, MultiTransportFlags,
    RdpVersion, SecureAccessSequence, SupportedColorDepths,
};
use ironrdp_pdu::mcs::{self, ConnectInitial};
use ironrdp_pdu::nego::{self, NegoRequestData, RequestFlags, SecurityProtocol};
use ironrdp_pdu::rdp::ClientInfoPdu;
use ironrdp_pdu::rdp::capability_sets::{
    self, CapabilitySet, ClientConfirmActive, CodecProperty, DemandActive, General, GeneralExtraFlags, Input,
    InputFlags, MajorPlatformType, MinorPlatformType, Order, OrderFlags, OrderSupportExFlags, PROTOCOL_VER,
    RemoteFxContainer,
};
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
use ironrdp_svc::{SvcMessage, SvcProcessor, SvcServerProcessor};

/// Drives an [`Acceptor`] (the server side of the [1.3.1.1] Connection Sequence) with a client byte stream that starts
/// out valid and turns into fuzzer input partway through.
///
/// RDP's connection sequence only reaches a deep state after a valid prefix, so feeding raw bytes to a fresh acceptor
/// almost never gets past the first decoder.
/// Instead, the oracle replays the first frames of a valid client handshake, then hands the rest of the stream over to
/// the fuzzer.
///
/// Input layout:
///
/// - byte 0: how many frames of the valid handshake to replay, modulo the handshake length plus one, so that the whole
///   handshake can be replayed.
///   The handshake depends on the configuration byte.
/// - byte 1: configuration bits that vary the handshake, see [`AcceptorConfig`].
/// - remaining bytes: the client frames from that point on, each starting with an [`AcceptorFrameMode`] byte:
///   - Without [`AcceptorFrameMode::TYPED`], a raw frame follows, framed with [`Sequence::next_pdu_hint`] the way the
///     server's I/O driver frames it.
///   - With [`AcceptorFrameMode::TYPED`], a two-byte little-endian length and that many bytes follow, from which
///     [`arbitrary`] builds a client PDU of a type the current state reads, or of any type with
///     [`AcceptorFrameMode::ANY_KIND`].
///     Most typed frames carry the channel IDs, credentials and request ID the server expects, so that they get past
///     those checks to the logic behind them.
///
/// Raw frames reach the acceptor's decoders with malformed framing.
/// Typed frames reach the state logic with well-formed PDUs that byte mutation would rarely build, such as a Connect
/// Initial with arbitrary GCC blocks or a Confirm Active with arbitrary capability sets.
///
/// The security protocol is always TLS, so CredSSP never runs: it needs a real authentication exchange this oracle
/// cannot replay.
/// The TLS upgrade itself is marked as done without any I/O, as a server does after its handshake completes.
/// Client bytes that arrive after the Connection Request but before the TLS handshake are therefore out of reach: the
/// I/O driver hands the stream over to TLS with `ironrdp_async::Framed::into_inner_no_leftover`, which drops them, and
/// this oracle does not run the driver.
///
/// With [`AcceptorConfig::REACTIVATION`], the server runs a Deactivation-Reactivation Sequence after accepting the
/// connection: it rebuilds the acceptor with [`Acceptor::new_deactivation_reactivation`], as `ironrdp-server` does on a
/// resize, and the client answers the new Demand Active again.
/// The replayed handshake then continues with that second round, and the fuzzer input takes over in whichever round the
/// replay ends.
///
/// Besides the absence of panics, every step must satisfy:
///
/// - The bytes written split into complete PDUs according to their own length fields, and the PDU lengths add up to the
///   size the step reports.
/// - Each PDU written decodes as what the state that wrote it sends.
/// - The state never moves backwards in the connection sequence.
/// - The Connect Response advertises one static channel per channel the client requested, and a message channel only
///   when the client requested one.
/// - Every channel join the server confirms answers the join the client just requested, names a channel the server
///   advertised, and is the first confirm of that channel.
/// - The Monitor Layout PDU carries a zero `pduSource`.
/// - The Demand Active advertises the server's own desktop size, or with [`AcceptorConfig::HONOR_DESKTOP_SIZE`] the
///   size the client requested, clamped to the configured maximum, when neither dimension is below the floor the
///   acceptor honors.
///   A reactivation advertises the size the server switched to.
/// - A Monitor Layout PDU describes a desktop of the size the Demand Active advertised.
/// - Reaching the accepted state yields a result, and unless the server was configured to accept any credentials, the
///   result of the first acceptance carries exactly the expected ones.
/// - The result of a reactivation is marked as one and keeps the MCS channel IDs of the connection it reactivates.
///
/// The oracle stops at the first error, as the server's driver does: stepping a consumed or accepted acceptor is a
/// usage error, not a protocol one.
///
/// Returns whether the acceptor accepted the connection and where the fuzzer input took over, so that a test can tell a
/// valid handshake that still gets through from one that stops early.
/// [`acceptor_state_machine_seeds`] builds valid inputs for each configuration.
///
/// [1.3.1.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/023f1e69-cfe8-4ee6-9ee0-7e759fb4e4ee
/// [`Sequence::next_pdu_hint`]: ironrdp_connector::Sequence::next_pdu_hint
pub fn acceptor_state_machine(data: &[u8]) -> AcceptorOutcome {
    let [replay_len, config, stream @ ..] = data else {
        return AcceptorOutcome {
            accepted: false,
            takeover: None,
            typed_frames: Vec::new(),
        };
    };
    let mut stream = stream;

    let config = AcceptorConfig(*config);
    let handshake = cached_client_handshake(config);
    let replay_len = usize::from(*replay_len) % (handshake.len() + 1);
    let connection_replay_len = replay_len.min(handshake.connection.len());
    let mut replay = handshake.connection[..connection_replay_len].iter();
    let reactivation_replay = handshake.reactivation[..replay_len - connection_replay_len].iter();

    let replayed_connect_initial = handshake.connection[..connection_replay_len]
        .iter()
        .any(|frame| frame.state == "BasicSettingsWaitInitial");
    let mut checks = OutputChecks {
        expected_channels: replayed_connect_initial.then_some(&handshake.channels),
        honor_desktop_size: config.has(AcceptorConfig::HONOR_DESKTOP_SIZE),
        desktop_size: SERVER_DESKTOP_SIZE,
        requested_channels: None,
        channels: None,
        user_channel_id: None,
        pending_join: None,
        joined: Vec::new(),
        advertised_desktop_size: None,
    };

    // Where the fuzzer input took over from the replayed handshake.
    let mut takeover: Option<Takeover> = None;
    let mut typed_frames = Vec::new();

    // Channel state of the accepted connection, once a reactivation started.
    let mut reactivated: Option<ChannelState> = None;

    let mut acceptor = Acceptor::new(
        SecurityProtocol::SSL,
        SERVER_DESKTOP_SIZE,
        vec![bitmap_capability_set(SERVER_DESKTOP_SIZE)],
        (!config.has(AcceptorConfig::ANY_CREDENTIALS)).then(client_credentials),
    );
    acceptor.set_multitransport_security_rng(Box::new(FixedSecurityRng));
    if config.has(AcceptorConfig::MULTITRANSPORT) {
        acceptor.set_multitransport_offer(Some(MULTITRANSPORT_FLAGS));
    }
    if config.has(AcceptorConfig::HONOR_DESKTOP_SIZE) {
        acceptor.set_honor_client_desktop_size(Some(MAX_HONORED_DESKTOP_SIZE));
    }
    if config.has(AcceptorConfig::STATIC_CHANNELS) {
        acceptor.attach_static_channel(FuzzChannel);
    }

    let mut output = WriteBuf::new();

    let accepted = loop {
        if acceptor.state().is_terminal() {
            // oracle: mutant handshake builder adding a frame after the one that completes the acceptance
            assert!(
                replay.next().is_none(),
                "the acceptor accepted the connection before the replayed handshake ended"
            );

            // oracle: reference crates/ironrdp-acceptor/src/lib.rs §accept_finalize (the driver stops stepping once the
            // acceptor yields its result)
            let result = acceptor.get_result().expect("an accepted acceptor returned no result");
            let channel_state = ChannelState {
                user_channel_id: result.user_channel_id,
                io_channel_id: result.io_channel_id,
                message_channel_id: result.message_channel_id,
            };

            if let Some(accepted) = reactivated {
                // oracle: spec crates/ironrdp-acceptor/src/connection.rs §AcceptorResult::reactivation
                assert!(
                    result.reactivation,
                    "the reactivated acceptor returned a result not marked as one"
                );
                // oracle: spec MS-RDPBCGR §1.3.1.3 (a reactivation only re-runs the capability exchange and
                // finalization, so the MCS channels stay)
                assert_eq!(
                    channel_state, accepted,
                    "the reactivation changed the MCS channel IDs of the connection"
                );
                break true;
            }

            // oracle: spec crates/ironrdp-acceptor/src/connection.rs §AcceptorResult::reactivation
            assert!(
                !result.reactivation,
                "the first acceptance returned a result marked as a reactivation"
            );

            if !config.has(AcceptorConfig::ANY_CREDENTIALS) {
                // oracle: spec crates/ironrdp-acceptor/src/connection.rs §Acceptor::new (an acceptor given credentials
                // accepts no others)
                assert_eq!(
                    result.credentials,
                    Some(client_credentials()),
                    "the acceptor accepted a connection whose credentials differ from the expected ones"
                );
            }

            if !config.has(AcceptorConfig::REACTIVATION) {
                break true;
            }

            acceptor =
                Acceptor::new_deactivation_reactivation(acceptor, result.static_channels, REACTIVATION_DESKTOP_SIZE)
                    .unwrap_or_else(|e| panic!("failed to rebuild the accepted acceptor for a reactivation: {e}"));
            replay = reactivation_replay.clone();
            reactivated = Some(channel_state);
            checks.desktop_size = REACTIVATION_DESKTOP_SIZE;
            continue;
        }

        let Some(hint) = acceptor.next_pdu_hint() else {
            if acceptor.reached_security_upgrade().is_some() {
                acceptor.mark_security_upgrade_as_done();
                continue;
            }

            if !checked_step(&mut acceptor, &mut checks, &mut output, None) {
                break false;
            }

            continue;
        };

        if let Some(frame) = replay.next() {
            let state = acceptor.state().name();
            // oracle: mutant handshake builder labelling a frame for a state other than the one that reads it
            assert_eq!(
                state, frame.state,
                "the replayed handshake reached {state} where it expected {}",
                frame.state
            );
            // oracle: spec MS-RDPBCGR §1.3.1.1 (each replayed frame is the valid client PDU for the state that reads
            // it)
            assert!(
                checked_step(&mut acceptor, &mut checks, &mut output, Some(&frame.bytes)),
                "the acceptor rejected a valid {} frame from the replayed handshake",
                frame.state
            );
            continue;
        }

        // A fully replayed handshake leaves nothing for the acceptor to read before it accepts the connection.
        // oracle: mutant handshake builder leaving out a frame the acceptor needs
        assert!(
            replay_len < handshake.len(),
            "the acceptor waits for more input in {} after the whole replayed handshake",
            acceptor.state().name()
        );
        takeover.get_or_insert_with(|| Takeover {
            reactivation: reactivated.is_some(),
            state: acceptor.state().name(),
        });

        let [mode, rest @ ..] = stream else {
            break false;
        };
        stream = rest;

        let frame = if mode & AcceptorFrameMode::TYPED == 0 {
            // Frame the stream the way the server's I/O driver does: a PDU the hint does not match is read and
            // dropped, and a stream that ends mid-PDU leaves the acceptor waiting for more bytes.
            let Ok(Some((matched, length @ 1..))) = hint.find_size(stream) else {
                break false;
            };
            let Some((frame, rest)) = stream.split_at_checked(length) else {
                break false;
            };
            stream = rest;
            if !matched {
                continue;
            }
            Cow::Borrowed(frame)
        } else {
            let [length_low, length_high, rest @ ..] = stream else {
                break false;
            };
            let (chunk, rest) =
                rest.split_at(usize::from(u16::from_le_bytes([*length_low, *length_high])).min(rest.len()));
            stream = rest;
            let kinds = if mode & AcceptorFrameMode::ANY_KIND == 0 {
                AcceptorPduKind::read_in(acceptor.state().name())
            } else {
                &AcceptorPduKind::ALL
            };
            let mut u = Unstructured::new(chunk);
            let Ok(&kind) = u.choose(kinds) else {
                continue;
            };
            let Some(frame) = typed_frame(kind, &checks, &mut u) else {
                continue;
            };
            typed_frames.push(kind);
            Cow::Owned(frame)
        };

        if !checked_step(&mut acceptor, &mut checks, &mut output, Some(&frame)) {
            break false;
        }
    };

    return AcceptorOutcome {
        accepted,
        takeover,
        typed_frames,
    };

    /// Runs one acceptor step and checks the output and state transition.
    ///
    /// Returns whether the step succeeded.
    fn checked_step(
        acceptor: &mut Acceptor,
        checks: &mut OutputChecks<'_>,
        output: &mut WriteBuf,
        input: Option<&[u8]>,
    ) -> bool {
        let state_before = acceptor.state().name();

        // Record what the client asks for, so the server's answers can be checked against it.
        match (state_before, input) {
            ("BasicSettingsWaitInitial", Some(input)) => {
                let gcc_blocks = decode::<X224<X224Data<'_>>>(input)
                    .ok()
                    .and_then(|payload| decode::<ConnectInitial>(payload.0.data.as_ref()).ok())
                    .map(|connect_initial| connect_initial.conference_create_request.into_gcc_blocks());
                if let Some(gcc_blocks) = gcc_blocks {
                    checks.requested_channels = Some(RequestedChannels {
                        statics: gcc_blocks.network.map_or(0, |network| network.channels.len()),
                        message: gcc_blocks.message_channel.is_some(),
                    });

                    // The server clamps the request per dimension to the maximum, and keeps its own size when a clamped
                    // dimension falls below the floor it honors.
                    if checks.honor_desktop_size {
                        let requested = DesktopSize {
                            width: gcc_blocks.core.desktop_width.min(MAX_HONORED_DESKTOP_SIZE.width),
                            height: gcc_blocks.core.desktop_height.min(MAX_HONORED_DESKTOP_SIZE.height),
                        };
                        if MIN_HONORED_DESKTOP_DIMENSION <= requested.width
                            && MIN_HONORED_DESKTOP_DIMENSION <= requested.height
                        {
                            checks.desktop_size = requested;
                        }
                    }
                }
            }
            ("ChannelConnection", Some(input)) => {
                if let Ok(request) = decode::<X224<mcs::ChannelJoinRequest>>(input) {
                    checks.pending_join = Some(request.0.channel_id);
                }
            }
            _ => {}
        }

        output.clear();
        let result = match input {
            Some(input) => acceptor.step(input, None, output),
            None => acceptor.step_no_input(output),
        };

        // A failing step may still write a PDU telling the client why, such as an RDP Negotiation Failure or a Set
        // Error Info PDU.
        // A driver that sends the output of a failing step must not send a malformed PDU, so it is checked like any
        // other.
        let mut written = output.filled();
        while !written.is_empty() {
            // oracle: spec ITU-T T.123 §8 (the TPKT header length covers the whole packet)
            let pdu_info = find_size(written)
                .unwrap_or_else(|e| panic!("{state_before} wrote bytes that do not start with a PDU header: {e}"))
                .expect("the acceptor wrote a truncated PDU header");
            assert!(
                pdu_info.length <= written.len(),
                "{state_before} wrote a PDU whose length field ({}) exceeds the bytes written after it ({})",
                pdu_info.length,
                written.len()
            );

            let (pdu, rest) = written.split_at(pdu_info.length);
            written = rest;

            // oracle: spec MS-RDPBCGR §1.3.1.1 (the PDU each phase of the connection sequence sends)
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
                    let response = decode::<mcs::ConnectResponse>(payload.data.as_ref()).unwrap_or_else(|e| {
                        panic!("{state_before} wrote a PDU that is not an MCS Connect Response: {e}")
                    });
                    let gcc_blocks = response.conference_create_response.into_gcc_blocks();
                    let channels = ServerChannels {
                        io: gcc_blocks.network.io_channel,
                        statics: gcc_blocks.network.channel_ids,
                        message: gcc_blocks.message_channel.map(|data| data.mcs_message_channel_id),
                    };

                    if let Some(requested) = checks.requested_channels {
                        // oracle: spec MS-RDPBCGR §2.2.1.4.4 (each channel ID corresponds in position to a requested
                        // channel)
                        assert_eq!(
                            channels.statics.len(),
                            requested.statics,
                            "{state_before} advertised a static channel count other than the requested one"
                        );
                        // oracle: spec MS-RDPBCGR §2.2.1.4 (serverMessageChannelData MUST NOT be included if the client
                        // did not request it)
                        assert!(
                            requested.message || channels.message.is_none(),
                            "{state_before} advertised a message channel the client did not request"
                        );
                    }

                    if let Some(expected) = checks.expected_channels {
                        // oracle: mutant acceptor allocating channel IDs other than the ones the replayed joins assume
                        // (a harness assumption: MS-RDPBCGR lets the server choose them)
                        assert_eq!(
                            &channels, expected,
                            "the server advertised channels other than the ones the replayed handshake joins"
                        );
                    }
                    checks.channels = Some(channels);
                }
                _ => {
                    let message = decode::<X224<mcs::McsMessage<'_>>>(pdu)
                        .unwrap_or_else(|e| panic!("{state_before} wrote a PDU that is not an MCS message: {e}"))
                        .0;
                    match (state_before, &message) {
                        ("ChannelConnection", mcs::McsMessage::AttachUserConfirm(confirm)) => {
                            if checks.expected_channels.is_some() {
                                // oracle: mutant acceptor assigning a user channel other than the one the replayed
                                // joins assume
                                assert_eq!(
                                    confirm.initiator_id, USER_CHANNEL_ID,
                                    "the server assigned a user channel other than the one the replayed handshake joins"
                                );
                            }
                            checks.user_channel_id = Some(confirm.initiator_id);
                        }
                        ("ChannelConnection", mcs::McsMessage::ChannelJoinConfirm(confirm)) => {
                            // oracle: spec MS-RDPBCGR §2.2.1.9 (a Channel Join Confirm is the response to a Channel
                            // Join Request)
                            assert_eq!(
                                Some(confirm.requested_channel_id),
                                checks.pending_join.take(),
                                "{state_before} confirmed a join the client did not just request"
                            );

                            // A failed join only needs to answer the request.
                            if confirm.result == 0 {
                                let Some(channels) = &checks.channels else {
                                    panic!("{state_before} confirmed a channel join before advertising any channel");
                                };
                                let advertised = Some(confirm.channel_id) == checks.user_channel_id
                                    || confirm.channel_id == channels.io
                                    || channels.statics.contains(&confirm.channel_id)
                                    || Some(confirm.channel_id) == channels.message;
                                // oracle: spec MS-RDPBCGR §1.3.1.1 (the client joins the user, I/O and static channels
                                // the server advertised, and the server confirms each)
                                assert!(
                                    confirm.channel_id == confirm.requested_channel_id && advertised,
                                    "{state_before} confirmed channel {} for a join of channel {}, or a channel it never advertised",
                                    confirm.channel_id,
                                    confirm.requested_channel_id
                                );
                                // oracle: spec MS-RDPBCGR §1.3.1.1 (the server confirms each channel once)
                                assert!(
                                    !checks.joined.contains(&confirm.channel_id),
                                    "{state_before} confirmed a second join of channel {}",
                                    confirm.channel_id
                                );
                                checks.joined.push(confirm.channel_id);
                            }
                        }
                        ("CapabilitiesSendServer", mcs::McsMessage::SendDataIndication(data)) => {
                            let header = decode::<ShareControlHeader>(&data.user_data).unwrap_or_else(|e| {
                                panic!("{state_before} wrote a PDU that is not a Share Control PDU: {e}")
                            });
                            let ShareControlPdu::ServerDemandActive(demand_active) = header.share_control_pdu else {
                                panic!(
                                    "{state_before} wrote a {} instead of a Demand Active",
                                    header.share_control_pdu.as_short_name()
                                );
                            };
                            let size = demand_active
                                .pdu
                                .capability_sets
                                .iter()
                                .find_map(|capability_set| match capability_set {
                                    CapabilitySet::Bitmap(bitmap) => Some(DesktopSize {
                                        width: bitmap.desktop_width,
                                        height: bitmap.desktop_height,
                                    }),
                                    _ => None,
                                })
                                .expect("the Demand Active carries no Bitmap Capability Set");

                            // oracle: spec crates/ironrdp-acceptor/src/connection.rs §set_honor_client_desktop_size
                            // (first round: the request clamped per dimension to the maximum)
                            // oracle: mutant acceptor honoring a dimension below 200 or ignoring one at 200 (first
                            // round: the server size when a clamped dimension is below the floor, which MS-RDPBCGR
                            // §3.3.5.3.3 leaves to the server)
                            // oracle: spec crates/ironrdp-acceptor/src/connection.rs
                            // §Acceptor::new_deactivation_reactivation (reactivation)
                            assert_eq!(
                                size, checks.desktop_size,
                                "{state_before} advertised a desktop size other than the expected one"
                            );
                            checks.advertised_desktop_size = Some(size);
                        }
                        ("MonitorLayoutSend", mcs::McsMessage::SendDataIndication(data)) => {
                            let header = decode::<ShareControlHeader>(&data.user_data).unwrap_or_else(|e| {
                                panic!("{state_before} wrote a PDU that is not a Share Control PDU: {e}")
                            });
                            let ShareControlPdu::Data(ShareDataHeader {
                                share_data_pdu: ShareDataPdu::MonitorLayout(layout),
                                ..
                            }) = header.share_control_pdu
                            else {
                                panic!(
                                    "{state_before} wrote a {} instead of a Monitor Layout PDU",
                                    header.share_control_pdu.as_short_name()
                                );
                            };

                            // oracle: spec MS-RDPBCGR §2.2.12.1 (the pduSource field MUST be set to zero)
                            assert_eq!(
                                header.pdu_source, 0,
                                "{state_before} wrote a Monitor Layout PDU with a nonzero pduSource"
                            );

                            // oracle: spec MS-RDPBCGR §2.2.1.3.6.1 (the upper-left corner of the primary monitor is
                            // always (0, 0))
                            assert!(
                                layout
                                    .monitors
                                    .iter()
                                    .any(|monitor| monitor.flags.contains(MonitorFlags::PRIMARY)
                                        && monitor.left == 0
                                        && monitor.top == 0),
                                "{state_before} wrote a monitor layout without a primary monitor at (0, 0)"
                            );

                            let advertised = checks
                                .advertised_desktop_size
                                .expect("the acceptor wrote a monitor layout before any Demand Active");
                            let left = layout.monitors.iter().map(|monitor| i64::from(monitor.left)).min();
                            let top = layout.monitors.iter().map(|monitor| i64::from(monitor.top)).min();
                            let right = layout.monitors.iter().map(|monitor| i64::from(monitor.right)).max();
                            let bottom = layout.monitors.iter().map(|monitor| i64::from(monitor.bottom)).max();
                            // oracle: mutant acceptor describing a desktop other than the one it advertised (the
                            // bounding rectangle of all monitors, whose right and bottom edges MS-RDPBCGR §2.2.1.3.6.1
                            // makes inclusive)
                            assert_eq!(
                                left.zip(right).map(|(left, right)| right - left + 1),
                                Some(i64::from(advertised.width)),
                                "{state_before} wrote a monitor layout narrower or wider than the advertised desktop"
                            );
                            assert_eq!(
                                top.zip(bottom).map(|(top, bottom)| bottom - top + 1),
                                Some(i64::from(advertised.height)),
                                "{state_before} wrote a monitor layout shorter or taller than the advertised desktop"
                            );
                        }
                        (state, mcs::McsMessage::SendDataIndication(_)) if state != "ChannelConnection" => {}
                        _ => panic!("{state_before} wrote an unexpected MCS message: {message:?}"),
                    }
                }
            }
        }

        let Ok(written) = result else {
            return false;
        };

        // oracle: reference crates/ironrdp-async/src/framed.rs §single_sequence_step_write (the driver sends exactly
        // the size a step reports)
        assert_eq!(
            output.filled_len(),
            written.size().unwrap_or(0),
            "{state_before} reported {written:?} but wrote {} bytes",
            output.filled_len()
        );

        let state_after = acceptor.state().name();
        // oracle: spec MS-RDPBCGR §1.3.1.1 (the phases of the connection sequence are strictly sequential)
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

/// What [`acceptor_state_machine`] observed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcceptorOutcome {
    /// Whether the acceptor accepted the connection, and when the input asks for a reactivation, its reactivation too.
    pub accepted: bool,
    /// Where the fuzzer input took over from the replayed handshake, or `None` when the whole handshake was replayed.
    pub takeover: Option<Takeover>,
    /// The kind of each typed frame the acceptor was stepped with, in order.
    pub typed_frames: Vec<AcceptorPduKind>,
}

/// Acceptor state that waited for input when the replayed handshake ran out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Takeover {
    /// Whether this happened in the round answering a Deactivation-Reactivation Sequence.
    pub reactivation: bool,
    /// Name of the acceptor state.
    pub state: &'static str,
}

/// A valid input for [`acceptor_state_machine`].
pub struct AcceptorSeed {
    /// Names the configuration and the state in which the fuzzer input takes over.
    pub name: String,
    /// What the oracle reports for this seed.
    pub outcome: AcceptorOutcome,
    pub bytes: Vec<u8>,
}

impl AcceptorSeed {
    /// Desktop size the client of every seed requests in its Client Core Data.
    pub const CLIENT_DESKTOP_SIZE: DesktopSize = CLIENT_DESKTOP_SIZE;
}

/// Builds the seeds for the handshake configuration `config`: one taking over in each state that reads client input,
/// and one replaying the whole handshake.
///
/// Each seed passes the rest of the valid handshake as raw fuzzer frames, so the acceptor accepts every seed.
pub fn acceptor_state_machine_seeds(config: u8) -> Vec<AcceptorSeed> {
    let handshake = cached_client_handshake(AcceptorConfig(config));
    let frames: Vec<&ClientFrame> = handshake.connection.iter().chain(&handshake.reactivation).collect();
    let config_byte = |replay_len: usize| {
        let replay_len = u8::try_from(replay_len).expect("the handshake has more frames than a seed can replay");
        [replay_len, config]
    };

    let takeover_seeds = (0..frames.len())
        .filter(|&index| {
            index == 0 || index == handshake.connection.len() || frames[index - 1].state != frames[index].state
        })
        .map(|index| {
            let reactivation = handshake.connection.len() <= index;
            let state = frames[index].state;
            let mut bytes = config_byte(index).to_vec();
            for frame in &frames[index..] {
                bytes.push(AcceptorFrameMode::RAW);
                bytes.extend_from_slice(&frame.bytes);
            }
            AcceptorSeed {
                name: format!(
                    "seed-config{config:02x}-{}{state}",
                    if reactivation { "Reactivation" } else { "" }
                ),
                outcome: AcceptorOutcome {
                    accepted: true,
                    takeover: Some(Takeover { reactivation, state }),
                    typed_frames: Vec::new(),
                },
                bytes,
            }
        });
    let full_replay = AcceptorSeed {
        name: format!("seed-config{config:02x}-Accepted"),
        outcome: AcceptorOutcome {
            accepted: true,
            takeover: None,
            typed_frames: Vec::new(),
        },
        bytes: config_byte(frames.len()).to_vec(),
    };

    takeover_seeds.chain([full_replay]).collect()
}

/// Valid client handshake for `config`, built once per configuration because building it encodes every frame.
fn cached_client_handshake(config: AcceptorConfig) -> &'static ClientHandshake {
    static HANDSHAKES: [OnceLock<ClientHandshake>; 256] = [const { OnceLock::new() }; 256];

    HANDSHAKES[usize::from(config.0)].get_or_init(|| client_handshake(config))
}

/// Mode byte that starts each frame of [`acceptor_state_machine`] input after the replayed handshake.
pub struct AcceptorFrameMode;

impl AcceptorFrameMode {
    /// A raw frame.
    pub const RAW: u8 = 0;
    /// A frame built from structured fuzzer input.
    pub const TYPED: u8 = 1 << 0;
    /// With [`AcceptorFrameMode::TYPED`], a frame that may carry any client PDU, not only one the current state reads.
    pub const ANY_KIND: u8 = 1 << 1;
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

/// Desktop size the client requests, wider than [`MAX_HONORED_DESKTOP_SIZE`] so that honoring it clamps the width.
const CLIENT_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 2560,
    height: 1024,
};

const MAX_HONORED_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1920,
    height: 1080,
};

/// Smallest desktop dimension the acceptor honors from a client, the floor of the [200, 8192] bound
/// `Acceptor::set_honor_client_desktop_size` documents.
///
/// The floor is the acceptor's own choice: MS-RDPBCGR 3.3.5.3.3 only clamps a requested dimension above the maximum.
/// Every honored request is first clamped to [`MAX_HONORED_DESKTOP_SIZE`], which is below the 8192 ceiling.
const MIN_HONORED_DESKTOP_DIMENSION: u16 = 200;

/// Desktop size the server switches to in a Deactivation-Reactivation Sequence.
const REACTIVATION_DESKTOP_SIZE: DesktopSize = DesktopSize {
    width: 1600,
    height: 900,
};

/// Keyboard layout (US) and function key count the client announces in its Client Core Data and Input Capability Set.
const CLIENT_KEYBOARD_LAYOUT: u32 = 0x409;
const CLIENT_FUNCTION_KEYS: u32 = 12;

const USER_CHANNEL_ID: u16 = 1002;
const IO_CHANNEL_ID: u16 = 1003;

const MULTITRANSPORT_FLAGS: MultiTransportFlags =
    MultiTransportFlags::TRANSPORT_TYPE_UDP_FECR.union(MultiTransportFlags::SOFT_SYNC_TCP_TO_UDP);

const MULTITRANSPORT_REQUEST_ID: u32 = 0x1234_5678;

/// Static channels the client asks for when [`AcceptorConfig::STATIC_CHANNELS`] is set.
///
/// The server only has a processor for the first one, so the acceptor assigns an ID to both but only binds the first.
const CLIENT_STATIC_CHANNELS: [&[u8; 8]; 2] = [b"fuzzsvc\0", b"unknown\0"];

/// Variations of the handshake, selected by the second input byte of [`acceptor_state_machine`] input.
#[derive(Clone, Copy)]
pub struct AcceptorConfig(u8);

impl AcceptorConfig {
    /// The client requests an MCS message channel.
    pub const MESSAGE_CHANNEL: u8 = 1 << 0;
    /// Both sides advertise UDP multitransport.
    /// With a message channel, the server sends an Initiate Multitransport Request and the client answers.
    pub const MULTITRANSPORT: u8 = 1 << 1;
    /// The client supports skipping the MCS channel joins.
    pub const SKIP_CHANNEL_JOIN: u8 = 1 << 2;
    /// The client supports the Monitor Layout PDU.
    pub const MONITOR_LAYOUT: u8 = 1 << 3;
    /// The client asks for two static virtual channels, of which the server only has a processor for the first.
    pub const STATIC_CHANNELS: u8 = 1 << 4;
    /// The server adopts the desktop size the client asks for.
    pub const HONOR_DESKTOP_SIZE: u8 = 1 << 5;
    /// The server accepts any credentials.
    /// Otherwise it expects the ones the valid handshake sends, so the oracle can check it never accepts others.
    pub const ANY_CREDENTIALS: u8 = 1 << 6;
    /// After accepting the connection, the server runs a Deactivation-Reactivation Sequence, and the client answers it.
    pub const REACTIVATION: u8 = 1 << 7;

    fn has(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

/// Valid client side of a connection sequence, see [`client_handshake`].
struct ClientHandshake {
    /// Frames up to the accepted connection.
    connection: Vec<ClientFrame>,
    /// Frames answering a Deactivation-Reactivation Sequence, empty unless [`AcceptorConfig::REACTIVATION`] is set.
    reactivation: Vec<ClientFrame>,
    /// Channels the handshake joins, which the server must advertise.
    channels: ServerChannels,
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

/// Channels a Connect Response advertises: the I/O channel, the static virtual channels and the message channel.
#[derive(Debug, PartialEq, Eq)]
struct ServerChannels {
    io: u16,
    statics: Vec<u16>,
    message: Option<u16>,
}

/// What the server advertised so far, and what it must advertise next.
struct OutputChecks<'a> {
    /// Channels the server must advertise, known when the Connect Initial that decides them is the replayed one.
    expected_channels: Option<&'a ServerChannels>,
    /// Whether the server honors the desktop size the client requests, up to [`MAX_HONORED_DESKTOP_SIZE`].
    honor_desktop_size: bool,
    /// Desktop size the next Demand Active must advertise.
    desktop_size: DesktopSize,
    /// Channels the client's Connect Initial asked for.
    requested_channels: Option<RequestedChannels>,
    /// Channels the Connect Response advertised.
    channels: Option<ServerChannels>,
    /// User channel the Attach User Confirm assigned.
    user_channel_id: Option<u16>,
    /// Channel the client's last Channel Join Request asked for, until the server answers it.
    pending_join: Option<u16>,
    /// Channels the server confirmed joins of.
    joined: Vec<u16>,
    /// Desktop size the last Demand Active advertised.
    advertised_desktop_size: Option<DesktopSize>,
}

/// Channels a Connect Initial asks for.
#[derive(Clone, Copy)]
struct RequestedChannels {
    /// Number of static virtual channels.
    statics: usize,
    /// Whether the client asks for a message channel.
    message: bool,
}

/// Builds the valid client side of a TLS connection sequence for `config`.
fn client_handshake(config: AcceptorConfig) -> ClientHandshake {
    let static_channels: &[&[u8; 8]] = if config.has(AcceptorConfig::STATIC_CHANNELS) {
        &CLIENT_STATIC_CHANNELS
    } else {
        &[]
    };

    // The channel joins are built before the server assigns any ID, so they assume the acceptor's numbering: every
    // channel the client asks for after the I/O channel, in request order, then the message channel.
    // The oracle checks this assumption against the Connect Response, so a change in numbering fails there rather than
    // as a rejected join.
    let mut next_channel_ids = IO_CHANNEL_ID + 1..;
    let channels = ServerChannels {
        io: IO_CHANNEL_ID,
        statics: next_channel_ids.by_ref().take(static_channels.len()).collect(),
        message: config
            .has(AcceptorConfig::MESSAGE_CHANNEL)
            .then_some(next_channel_ids.start),
    };

    let mut early_capability_flags =
        ClientEarlyCapabilityFlags::SUPPORT_ERR_INFO_PDU | ClientEarlyCapabilityFlags::WANT_32_BPP_SESSION;
    if config.has(AcceptorConfig::SKIP_CHANNEL_JOIN) {
        early_capability_flags |= ClientEarlyCapabilityFlags::SUPPORT_SKIP_CHANNELJOIN;
    }
    if config.has(AcceptorConfig::MONITOR_LAYOUT) {
        early_capability_flags |= ClientEarlyCapabilityFlags::SUPPORT_MONITOR_LAYOUT_PDU;
    }

    let gcc_blocks = ClientGccBlocks {
        core: ClientCoreData {
            version: RdpVersion::V5_PLUS,
            desktop_width: CLIENT_DESKTOP_SIZE.width,
            desktop_height: CLIENT_DESKTOP_SIZE.height,
            color_depth: ColorDepth::Bpp8,
            sec_access_sequence: SecureAccessSequence::Del,
            keyboard_layout: CLIENT_KEYBOARD_LAYOUT,
            client_build: 0,
            client_name: "fuzz".to_owned(),
            keyboard_type: KeyboardType::IBM_ENHANCED,
            keyboard_subtype: 0,
            keyboard_functional_keys_count: CLIENT_FUNCTION_KEYS,
            ime_file_name: String::new(),
            optional_data: ClientCoreOptionalData {
                post_beta2_color_depth: Some(ColorDepth::Bpp8),
                client_product_id: Some(1),
                serial_number: Some(0),
                high_color_depth: Some(HighColorDepth::Bpp24),
                supported_color_depths: Some(SupportedColorDepths::BPP32 | SupportedColorDepths::BPP24),
                early_capability_flags: Some(early_capability_flags),
                dig_product_id: Some(String::new()),
                connection_type: Some(ConnectionType::NotUsed),
                // MS-RDPBCGR 2.2.1.3.2: serverSelectedProtocol MUST be present after an RDP Negotiation Request.
                server_selected_protocol: Some(SecurityProtocol::SSL),
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
        message_channel: config
            .has(AcceptorConfig::MESSAGE_CHANNEL)
            .then_some(ClientMessageChannelData),
        multi_transport_channel: config
            .has(AcceptorConfig::MULTITRANSPORT)
            .then_some(MultiTransportChannelData {
                flags: MULTITRANSPORT_FLAGS,
            }),
        monitor_extended: None,
    };

    let mut frames = vec![
        ClientFrame {
            state: "InitiationWaitRequest",
            bytes: encode_frame(&X224(nego::ConnectionRequest {
                nego_data: None,
                flags: RequestFlags::empty(),
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

    if !config.has(AcceptorConfig::SKIP_CHANNEL_JOIN) {
        let channel_ids = [USER_CHANNEL_ID, channels.io]
            .into_iter()
            .chain(channels.statics.iter().copied())
            .chain(channels.message);
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
        bytes: client_send_data(
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

    // The server only sends an Initiate Multitransport Request on the message channel; the client answers before
    // reading the Demand Active.
    if let Some(message_channel_id) = channels.message.filter(|_| config.has(AcceptorConfig::MULTITRANSPORT)) {
        frames.push(ClientFrame {
            state: "CapabilitiesWaitConfirm",
            bytes: client_send_data(
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

    // The client answers every Demand Active the same way, whether it comes during the connection sequence or a
    // Deactivation-Reactivation Sequence.
    let mut activation = vec![ClientFrame {
        state: "CapabilitiesWaitConfirm",
        bytes: client_send_data(
            IO_CHANNEL_ID,
            &ShareControlHeader {
                share_id: 0,
                pdu_source: USER_CHANNEL_ID,
                share_control_pdu: ShareControlPdu::ClientConfirmActive(ClientConfirmActive {
                    originator_id: capability_sets::SERVER_CHANNEL_ID,
                    pdu: DemandActive {
                        source_descriptor: "MSTSC".to_owned(),
                        // [2.2.7.1.1] General Capability Set, [2.2.7.1.2] Bitmap Capability Set, [2.2.7.1.3] Order
                        // Capability Set and [2.2.7.1.6] Input Capability Set say that both client and server send
                        // these sets, and the values follow what those sections require of a client.
                        //
                        // [2.2.7.1.1]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/41dc6845-07dc-4af6-bc14-d8281acd4877
                        // [2.2.7.1.2]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/76670547-e35c-4b95-a242-5729a21b83f6
                        // [2.2.7.1.3]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/9f409c29-480c-4751-9665-510b8ffff294
                        // [2.2.7.1.6]: https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-rdpbcgr/b3bc76ae-9ee5-454f-b197-ede845ca69cc
                        capability_sets: vec![
                            CapabilitySet::General(General {
                                major_platform_type: MajorPlatformType::UNIX,
                                minor_platform_type: MinorPlatformType::UNSPECIFIED,
                                protocol_version: PROTOCOL_VER,
                                extra_flags: GeneralExtraFlags::FASTPATH_OUTPUT_SUPPORTED
                                    | GeneralExtraFlags::NO_BITMAP_COMPRESSION_HDR,
                                refresh_rect_support: false,
                                suppress_output_support: false,
                            }),
                            bitmap_capability_set(CLIENT_DESKTOP_SIZE),
                            CapabilitySet::Order(Order::new(
                                OrderFlags::NEGOTIATE_ORDER_SUPPORT | OrderFlags::ZERO_BOUNDS_DELTAS_SUPPORT,
                                OrderSupportExFlags::empty(),
                                480 * 480,
                                0,
                            )),
                            CapabilitySet::Input(Input {
                                input_flags: InputFlags::SCANCODES | InputFlags::MOUSEX | InputFlags::UNICODE,
                                keyboard_layout: CLIENT_KEYBOARD_LAYOUT,
                                keyboard_type: Some(KeyboardType::IBM_ENHANCED),
                                keyboard_subtype: 0,
                                keyboard_function_key: CLIENT_FUNCTION_KEYS,
                                keyboard_ime_filename: String::new(),
                            }),
                        ],
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
        bytes: client_send_data(
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

    let reactivation = if config.has(AcceptorConfig::REACTIVATION) {
        activation.clone()
    } else {
        Vec::new()
    };
    frames.extend(activation);

    return ClientHandshake {
        connection: frames,
        reactivation,
        channels,
    };

    fn encode_frame(pdu: &dyn Encode) -> Vec<u8> {
        encode_capped(pdu).expect("failed to encode a handshake frame")
    }

    fn client_send_data(channel_id: u16, user_data: &dyn Encode) -> Vec<u8> {
        send_data_request(USER_CHANNEL_ID, channel_id, user_data).expect("failed to encode a handshake frame")
    }
}

/// Channel state an accepted connection keeps across reactivations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChannelState {
    user_channel_id: u16,
    io_channel_id: u16,
    message_channel_id: Option<u16>,
}

/// Client PDUs a typed frame can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcceptorPduKind {
    ConnectionRequest,
    ConnectInitial,
    ErectDomain,
    AttachUser,
    ChannelJoin,
    ClientInfo,
    ConfirmActive,
    MultitransportResponse,
    Synchronize,
    Control,
    FontList,
}

impl AcceptorPduKind {
    /// Every kind a typed frame can carry.
    pub const ALL: [Self; 11] = [
        Self::ConnectionRequest,
        Self::ConnectInitial,
        Self::ErectDomain,
        Self::AttachUser,
        Self::ChannelJoin,
        Self::ClientInfo,
        Self::ConfirmActive,
        Self::MultitransportResponse,
        Self::Synchronize,
        Self::Control,
        Self::FontList,
    ];

    /// Client PDUs the acceptor state named `state` reads.
    fn read_in(state: &str) -> &'static [Self] {
        match state {
            "InitiationWaitRequest" => &[Self::ConnectionRequest],
            "BasicSettingsWaitInitial" => &[Self::ConnectInitial],
            "ChannelConnection" => &[Self::ErectDomain, Self::AttachUser, Self::ChannelJoin],
            "SecureSettingsExchange" => &[Self::ClientInfo],
            "CapabilitiesWaitConfirm" => &[Self::ConfirmActive, Self::MultitransportResponse],
            "ConnectionFinalization" => &[Self::Synchronize, Self::Control, Self::FontList],
            _ => &Self::ALL,
        }
    }
}

/// Builds a client PDU of `kind` from `u` and encodes it as a frame.
///
/// Returns `None` when `u` runs out or the encoder rejects the generated values.
/// Unless `u` says otherwise, the fields the acceptor checks against its earlier answers carry the values it expects.
fn typed_frame(kind: AcceptorPduKind, checks: &OutputChecks<'_>, u: &mut Unstructured<'_>) -> Option<Vec<u8>> {
    // One frame in eight keeps the generated values, so that mismatches are fuzzed too.
    let correlate = u.ratio(7u8, 8u8).ok()?;

    let user_channel_id = checks.user_channel_id.unwrap_or(USER_CHANNEL_ID);
    let io_channel_id = checks.channels.as_ref().map_or(IO_CHANNEL_ID, |channels| channels.io);
    let message_channel_id = checks
        .channels
        .as_ref()
        .and_then(|channels| channels.message)
        .unwrap_or(io_channel_id);
    let (initiator_id, share_id) = if correlate {
        (user_channel_id, 0)
    } else {
        (u.arbitrary().ok()?, u.arbitrary().ok()?)
    };
    let channel_id =
        |expected: u16, u: &mut Unstructured<'_>| if correlate { Some(expected) } else { u.arbitrary().ok() };

    match kind {
        AcceptorPduKind::ConnectionRequest => {
            let mut protocol: SecurityProtocol = u.arbitrary().ok()?;
            if correlate {
                protocol |= SecurityProtocol::SSL;
            }
            let nego_data = match u.int_in_range(0..=2u8).ok()? {
                0 => None,
                1 => Some(NegoRequestData::cookie(u.arbitrary().ok()?)),
                _ => Some(NegoRequestData::routing_token(u.arbitrary().ok()?)),
            };
            encode_capped(&X224(nego::ConnectionRequest {
                nego_data,
                flags: RequestFlags::from_bits_retain(u.arbitrary().ok()?),
                protocol,
                correlation_info: None,
            }))
        }
        AcceptorPduKind::ConnectInitial => encode_capped(&u.arbitrary::<ConnectInitial>().ok()?)
            .and_then(|data| encode_capped(&X224(X224Data { data: Cow::Owned(data) }))),
        AcceptorPduKind::ErectDomain => encode_capped(&X224(u.arbitrary::<mcs::ErectDomainPdu>().ok()?)),
        AcceptorPduKind::AttachUser => encode_capped(&X224(mcs::AttachUserRequest)),
        AcceptorPduKind::ChannelJoin => {
            let mut request: mcs::ChannelJoinRequest = u.arbitrary().ok()?;
            if correlate {
                let advertised: Vec<u16> = [user_channel_id, io_channel_id]
                    .into_iter()
                    .chain(
                        checks
                            .channels
                            .iter()
                            .flat_map(|channels| channels.statics.iter().copied().chain(channels.message)),
                    )
                    .collect();
                request.initiator_id = user_channel_id;
                request.channel_id = *u.choose(&advertised).ok()?;
            }
            encode_capped(&X224(request))
        }
        AcceptorPduKind::ClientInfo => {
            let mut pdu: ClientInfoPdu = u.arbitrary().ok()?;
            if correlate {
                pdu.security_header.flags = BasicSecurityHeaderFlags::INFO_PKT;
                pdu.client_info.credentials = client_credentials();
            }
            send_data_request(initiator_id, channel_id(io_channel_id, u)?, &pdu)
        }
        AcceptorPduKind::MultitransportResponse => {
            let mut response: MultitransportResponsePdu = u.arbitrary().ok()?;
            if correlate {
                response.security_header.flags = BasicSecurityHeaderFlags::TRANSPORT_RSP;
                response.request_id = MULTITRANSPORT_REQUEST_ID;
            }
            send_data_request(initiator_id, channel_id(message_channel_id, u)?, &response)
        }
        AcceptorPduKind::ConfirmActive => {
            let mut confirm: ClientConfirmActive = u.arbitrary().ok()?;
            if correlate {
                confirm.originator_id = capability_sets::SERVER_CHANNEL_ID;
            }
            // A RemoteFX server container is a byte count behind a two-byte length field, but the generated count can
            // be any `usize`, which no encoder can write and whose size computation overflows.
            for set in &mut confirm.pdu.capability_sets {
                let CapabilitySet::BitmapCodecs(codecs) = set else {
                    continue;
                };
                for codec in &mut codecs.0 {
                    if let CodecProperty::RemoteFx(RemoteFxContainer::ServerContainer(size))
                    | CodecProperty::ImageRemoteFx(RemoteFxContainer::ServerContainer(size)) = &mut codec.property
                    {
                        *size %= usize::from(u16::MAX) + 1;
                    }
                }
            }
            let header = ShareControlHeader {
                share_id,
                pdu_source: initiator_id,
                share_control_pdu: ShareControlPdu::ClientConfirmActive(confirm),
            };
            send_data_request(initiator_id, channel_id(io_channel_id, u)?, &header)
        }
        AcceptorPduKind::Synchronize | AcceptorPduKind::Control | AcceptorPduKind::FontList => {
            let share_data_pdu = match kind {
                AcceptorPduKind::Synchronize => ShareDataPdu::Synchronize(u.arbitrary().ok()?),
                AcceptorPduKind::Control => ShareDataPdu::Control(u.arbitrary().ok()?),
                _ => ShareDataPdu::FontList(u.arbitrary().ok()?),
            };
            let mut data_header: ShareDataHeader = u.arbitrary().ok()?;
            data_header.share_data_pdu = share_data_pdu;
            if correlate {
                data_header.stream_priority = StreamPriority::Undefined;
                data_header.compression_flags = CompressionFlags::empty();
                data_header.compression_type = CompressionType::K8;
            }
            let header = ShareControlHeader {
                share_id,
                pdu_source: initiator_id,
                share_control_pdu: ShareControlPdu::Data(data_header),
            };
            send_data_request(initiator_id, channel_id(io_channel_id, u)?, &header)
        }
    }
}

/// Builds the Bitmap Capability Set that both the server and the replayed client send, for a desktop of `size`.
fn bitmap_capability_set(size: DesktopSize) -> CapabilitySet {
    CapabilitySet::Bitmap(capability_sets::Bitmap {
        pref_bits_per_pix: 32,
        desktop_width: size.width,
        desktop_height: size.height,
        desktop_resize_flag: true,
        drawing_flags: capability_sets::BitmapDrawingFlags::empty(),
    })
}

/// Encodes `user_data` in an MCS Send Data Request frame, see [`encode_capped`].
fn send_data_request(initiator_id: u16, channel_id: u16, user_data: &dyn Encode) -> Option<Vec<u8>> {
    encode_capped(&X224(mcs::SendDataRequest {
        initiator_id,
        channel_id,
        user_data: Cow::Owned(encode_capped(user_data)?),
    }))
}

/// Encodes `pdu`, or returns `None` when the encoder rejects it or it would not fit in a TPKT frame.
///
/// Unlike `encode_vec`, a size that disagrees with what the encoder writes is not reported: this target checks the
/// acceptor, not the encoders, which the round-trip targets cover.
fn encode_capped(pdu: &dyn Encode) -> Option<Vec<u8>> {
    let size = pdu.size();
    // A TPKT length field holds at most `u16::MAX`.
    if u16::try_from(size).is_err() {
        return None;
    }
    let mut frame = vec![0; size];
    let written = ironrdp_core::encode(pdu, &mut frame).ok()?;
    frame.truncate(written);
    Some(frame)
}

fn client_credentials() -> Credentials {
    Credentials {
        username: "user".to_owned(),
        password: "password".to_owned(),
        domain: None,
    }
}

/// Deterministic security cookie and request ID, so the same input always produces the same Initiate Multitransport
/// Request.
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
