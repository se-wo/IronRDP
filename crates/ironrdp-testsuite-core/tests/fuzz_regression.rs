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

#[test]
fn check_acceptor_state_machine() {
    check!(acceptor_state_machine);
}

// oracle: spec MS-RDPBCGR §1.3.1.1 (each seed passes a valid connection sequence from the state it names on, so the server accepts it)
#[test]
fn acceptor_state_machine_accepts_every_seed() {
    use ironrdp_fuzzing::oracles;

    for config in u8::MIN..=u8::MAX {
        for seed in oracles::acceptor_state_machine_seeds(config) {
            assert_eq!(
                oracles::acceptor_state_machine(&seed.bytes),
                seed.outcome,
                "{}",
                seed.name
            );
        }
    }
}

// oracle: mutant committed seed corpus drifting from the handshake the acceptor oracle replays
#[test]
fn acceptor_state_machine_corpus_holds_the_generated_seeds() {
    use ironrdp_fuzzing::oracles;

    const FOLDER: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/test_data/fuzz_regression/acceptor_state_machine"
    );
    // A configuration with every option off, two that cover every option between them, and the second one with a
    // reactivation.
    const CORPUS_CONFIGS: [u8; 4] = [0x00, 0x3b, 0x44, 0xbb];

    let update = std::env::var("UPDATE_EXPECT").is_ok_and(|value| value == "1");
    let seeds: Vec<_> = CORPUS_CONFIGS
        .into_iter()
        .flat_map(oracles::acceptor_state_machine_seeds)
        .collect();

    for seed in &seeds {
        let path = format!("{FOLDER}/{}.bin", seed.name);
        if update {
            std::fs::write(&path, &seed.bytes).unwrap();
        } else {
            assert_eq!(
                std::fs::read(&path).ok().as_ref(),
                Some(&seed.bytes),
                "{path} does not hold the generated seed; run with UPDATE_EXPECT=1 to regenerate the corpus"
            );
        }
    }

    for entry in std::fs::read_dir(FOLDER).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
        if !name.starts_with("seed-") || seeds.iter().any(|seed| seed.name == name) {
            continue;
        }
        if update {
            std::fs::remove_file(&path).unwrap();
        } else {
            panic!(
                "{} is not a generated seed; run with UPDATE_EXPECT=1 to regenerate the corpus",
                path.display()
            );
        }
    }
}
