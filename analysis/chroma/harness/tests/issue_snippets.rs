//! The reproduction snippets quoted in `analysis/issues/*.md`, compiled and
//! run here so the issue texts only contain code that is known to behave as
//! described. Only the `include_bytes!` path differs from the issue text
//! (which uses the path relative to `ironrdp-testsuite-core/tests/egfx/`).

use ironrdp_core::{Decode as _, ReadCursor};
use ironrdp_egfx::pdu::{Avc420Region, GfxPdu, encode_avc420_bitmap_stream};
use ironrdp_graphics::progressive::ProgressiveDecoder;
use ironrdp_pdu::codecs::rfx::progressive::{
    ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu, ProgressiveRegion,
    ProgressiveSyncPdu, ProgressiveTile, TILE_FLAG_DIFFERENCE, decode_progressive_stream, encode_progressive_stream,
};

// --- issue 01 (C-16) ---------------------------------------------------------
#[test]
fn progressive_dwt_variant_comes_from_region_flags() {
    let bytes = include_bytes!(
        "../../../../crates/ironrdp-testsuite-core/test_data/egfx/haven/wts2_progressive_tile_first_mixed_25tiles.bin"
    );
    let GfxPdu::WireToSurface2(pdu) = GfxPdu::decode(&mut ReadCursor::new(bytes)).unwrap() else {
        panic!("expected WireToSurface2");
    };
    let region = decode_progressive_stream(&pdu.bitmap_data)
        .unwrap()
        .into_iter()
        .find_map(|b| match b {
            ProgressiveBlock::Region(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(region.uses_reduce_extrapolate()); // REGION flags = 0x01
    // Difference tiles need a reference that is not part of the capture.
    let base: Vec<_> = region
        .tiles
        .iter()
        .filter(|t| matches!(t, ProgressiveTile::First(f) if f.flags & TILE_FLAG_DIFFERENCE == 0))
        .cloned()
        .collect();

    let decode_with = |context_flags: u8| {
        let stream = encode_progressive_stream(&[
            ProgressiveBlock::Sync(ProgressiveSyncPdu),
            ProgressiveBlock::Context(ProgressiveContextPdu {
                context_id: 0,
                tile_size: 0x40,
                flags: context_flags,
            }),
            ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
                frame_index: 0,
                region_count: 1,
            }),
            ProgressiveBlock::Region(ProgressiveRegion {
                tiles: base.clone(),
                ..region.clone()
            }),
            ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
        ])
        .unwrap();
        let mut decoder = ProgressiveDecoder::new();
        decoder.begin_frame();
        let tiles = decoder.decode_bitmap(0, 3, 1280, 800, &stream).unwrap();
        decoder.end_frame();
        tiles.into_iter().map(|t| t.pixels).collect::<Vec<_>>()
    };

    // CONTEXT bit 0 is RFX_SUBBAND_DIFFING and must not change non-difference tiles.
    assert!(decode_with(0x01) == decode_with(0x00));
}

// --- issue 03 (C-12) ---------------------------------------------------------
#[test]
fn avc420_qp_64_does_not_panic() {
    // panicked at crates/ironrdp-egfx/src/pdu/avc.rs:45:14: value does not fit into bit range
    let _ = encode_avc420_bitmap_stream(&[Avc420Region::new(0, 0, 16, 16, 64, 100)], &[0, 0, 0, 1, 0x65]);
}
