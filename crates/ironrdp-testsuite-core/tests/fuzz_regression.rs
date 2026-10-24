macro_rules! check {
    ($oracle:ident) => {{
        use ironrdp_fuzzing::oracles;

        const REGRESSION_DATA_FOLDER: &str = concat!(
            env!("CARGO_MANIFEST_DIR"),
            concat!("/test_data/fuzz_regression/", stringify!($oracle))
        );

        println!("Read directory {REGRESSION_DATA_FOLDER}");
        for entry in std::fs::read_dir(REGRESSION_DATA_FOLDER).unwrap() {
            let entry = entry.unwrap();
            println!("Check {}", entry.path().display());
            let test_case = std::fs::read(entry.path()).unwrap();
            oracles::$oracle(&test_case);
        }
    }};
}

#[test]
fn check_pdu_decode() {
    check!(pdu_decode);
}

#[test]
fn check_cliprdr_format() {
    check!(cliprdr_format);
}

#[test]
fn check_bulk_decompress_mppc() {
    check!(bulk_decompress_mppc);
}

#[test]
fn check_bulk_decompress_ncrush() {
    check!(bulk_decompress_ncrush);
}

#[test]
fn check_bulk_decompress_xcrush() {
    check!(bulk_decompress_xcrush);
}

#[test]
fn check_bulk_decompress_rdp8() {
    check!(bulk_decompress_rdp8);
}

#[test]
fn check_bulk_round_trip() {
    check!(bulk_round_trip);
}

#[test]
fn check_pdu_round_trip() {
    check!(pdu_round_trip);
}

#[test]
fn check_channel_process() {
    check!(channel_process);
}

#[test]
fn check_egfx_round_trip() {
    check!(egfx_round_trip);
}

#[test]
fn check_egfx_avc420_decode() {
    check!(egfx_avc420_decode);
}

#[test]
fn check_egfx_multi_frame() {
    check!(egfx_multi_frame);
}

#[test]
fn check_rdpeusb_decode() {
    check!(rdpeusb_decode);
}

#[test]
fn check_message_decoding_invariants() {
    check!(message_decoding_invariants);
}

#[test]
fn check_egfx_avc444_decode() {
    check!(egfx_avc444_decode);
}

#[test]
fn check_egfx_zgfx_decompress() {
    check!(egfx_zgfx_decompress);
}

#[test]
fn check_egfx_surface_state() {
    check!(egfx_surface_state);
}

// oracle: spec crates/ironrdp-fuzzing/src/oracles/mod.rs §rdpsnd_codec_decode (no panic for any accepted format; seeds built from the FFmpeg vectors in crates/ironrdp-testsuite-core/test_data/audio)
#[test]
fn check_rdpsnd_codec_decode() {
    check!(rdpsnd_codec_decode);
}

// oracle: spec MS-RDPBCGR §1.3.1.1 (each seed passes a valid connection sequence from the state it names on, so the server accepts it)
#[test]
#[expect(
    clippy::print_stdout,
    reason = "names the case an assertion inside the oracle fails on, as `check!` does"
)]
fn acceptor_state_machine_accepts_every_seed() {
    use ironrdp_fuzzing::oracles;

    for config in u8::MIN..=u8::MAX {
        for seed in oracles::acceptor_state_machine_seeds(config) {
            println!("Check {}", seed.name);
            let outcome = oracles::acceptor_state_machine(&seed.bytes);
            assert!(outcome.accepted, "{}", seed.name);
            // oracle: mutant seed whose replay length no longer stops at the state it is named for
            assert_eq!(outcome.takeover, seed.outcome.takeover, "{}", seed.name);
        }
    }
}

// oracle: spec crates/ironrdp-fuzzing/src/oracles/acceptor.rs §acceptor_state_machine (no panic: every step satisfies the invariants listed there, whatever client PDU a typed frame carries)
#[test]
#[expect(
    clippy::print_stdout,
    reason = "names the case an assertion inside the oracle fails on, as `check!` does"
)]
fn acceptor_state_machine_holds_for_typed_frames() {
    use ironrdp_fuzzing::oracles::{self, AcceptorFrameMode};

    // A configuration with every option off, two that cover every option between them, and every option on.
    const CONFIGS: [u8; 4] = [0x00, 0xbb, 0x44, 0xff];

    for config in CONFIGS {
        // The full-replay seed reads no fuzzer input, so only the takeover seeds can carry a typed frame.
        let seeds = oracles::acceptor_state_machine_seeds(config)
            .into_iter()
            .filter(|seed| seed.outcome.takeover.is_some());
        for seed in seeds {
            let (header, rest) = seed.bytes.split_at(2);
            // Chunks starting with every byte value cover every client PDU type a typed frame can carry.
            for first in u8::MIN..=u8::MAX {
                println!("Check {} with a typed frame starting with {first}", seed.name);
                let mut input = header.to_vec();
                input.extend([
                    AcceptorFrameMode::TYPED | AcceptorFrameMode::ANY_KIND,
                    4,
                    0,
                    first,
                    0,
                    0,
                    0,
                ]);
                input.extend(rest);
                // oracle: mutant typed frame builder that never yields a PDU, so no typed frame reaches the acceptor
                assert_eq!(
                    oracles::acceptor_state_machine(&input).typed_frames,
                    1,
                    "{} with a typed frame starting with {first}",
                    seed.name
                );
            }
        }
    }
}

// oracle: spec crates/ironrdp-acceptor/src/connection.rs §set_honor_client_desktop_size (a request is honored only within the [200, 8192] bound, so a width of 199 keeps the server size and one of 200 is adopted, which the oracle's Demand Active check enforces)
#[test]
#[expect(
    clippy::print_stdout,
    reason = "names the case an assertion inside the oracle fails on, as `check!` does"
)]
fn acceptor_state_machine_honors_desktop_sizes_from_the_floor() {
    use ironrdp_fuzzing::oracles::{self, AcceptorConfig, AcceptorSeed, Takeover};

    let seed = oracles::acceptor_state_machine_seeds(AcceptorConfig::HONOR_DESKTOP_SIZE)
        .into_iter()
        .find(|seed| {
            seed.outcome.takeover
                == Some(Takeover {
                    reactivation: false,
                    state: "BasicSettingsWaitInitial",
                })
        })
        .expect("a seed takes over at the Connect Initial");
    // The Client Core Data of the Connect Initial is the first place the seed carries the requested size.
    let requested = AcceptorSeed::CLIENT_DESKTOP_SIZE;
    let requested = [requested.width.to_le_bytes(), requested.height.to_le_bytes()].concat();
    let at = seed
        .bytes
        .windows(requested.len())
        .position(|window| window == requested)
        .expect("the seed carries the requested desktop size");

    for width in [199u16, 200] {
        println!("Check a requested width of {width}");
        let mut input = seed.bytes.clone();
        input[at..at + 2].copy_from_slice(&width.to_le_bytes());
        assert!(oracles::acceptor_state_machine(&input).accepted, "width {width}");
    }
}
