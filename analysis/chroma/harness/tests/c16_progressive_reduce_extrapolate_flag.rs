//! C-16: the Progressive decoder must take the DWT variant from the REGION
//! flags, not from the CONTEXT flags.
//!
//! MS-RDPEGFX 2.2.4.2.1.4 (RFX_PROGRESSIVE_CONTEXT): flags bit 0 is
//! RFX_SUBBAND_DIFFING. 2.2.4.2.1.5 (RFX_PROGRESSIVE_REGION): flags bit 0 is
//! RFX_DWT_REDUCE_EXTRAPOLATE. FreeRDP follows this
//! (`progressive.c`: `sub = context->flags & RFX_SUBBAND_DIFFING;
//! extrapolate = region->flags & RFX_DWT_REDUCE_EXTRAPOLATE;`). IronRDP's
//! `ProgressiveDecoder::decode_bitmap` reads reduce-extrapolate from the
//! CONTEXT block (`ProgressiveContextPdu::uses_reduce_extrapolate`).
//!
//! Input: the 16 base (non-difference) tiles of the Windows capture
//! `wts2_progressive_tile_first_mixed_25tiles.bin` (REGION flags = 0x01),
//! prefixed with SYNC + CONTEXT. The only difference between the two streams
//! is CONTEXT bit 0, which for base tiles must not change a single pixel.

use ironrdp_core::{Decode as _, ReadCursor};
use ironrdp_egfx::pdu::GfxPdu;
use ironrdp_graphics::progressive::ProgressiveDecoder;
use ironrdp_pdu::codecs::rfx::progressive::{
    ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu, ProgressiveRegion,
    ProgressiveSyncPdu, ProgressiveTile, TILE_FLAG_DIFFERENCE, decode_progressive_stream, encode_progressive_stream,
};

const FIXTURE: &[u8] = include_bytes!(
    "../../../../crates/ironrdp-testsuite-core/test_data/egfx/haven/wts2_progressive_tile_first_mixed_25tiles.bin"
);

fn decode_with_context_flags(ctx_flags: u8) -> Vec<(u16, u16, Vec<u8>)> {
    let GfxPdu::WireToSurface2(pdu) = GfxPdu::decode(&mut ReadCursor::new(FIXTURE)).unwrap() else {
        panic!("expected WireToSurface2");
    };
    let blocks = decode_progressive_stream(&pdu.bitmap_data).unwrap();
    let region = blocks
        .iter()
        .find_map(|b| match b {
            ProgressiveBlock::Region(r) => Some(r.clone()),
            _ => None,
        })
        .unwrap();
    assert!(
        region.uses_reduce_extrapolate(),
        "fixture REGION must select reduce-extrapolate"
    );
    let tiles = region
        .tiles
        .iter()
        .filter(|t| matches!(t, ProgressiveTile::First(f) if f.flags & TILE_FLAG_DIFFERENCE == 0))
        .cloned()
        .collect();
    let stream = encode_progressive_stream(&[
        ProgressiveBlock::Sync(ProgressiveSyncPdu),
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 0x40,
            flags: ctx_flags,
        }),
        ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
            frame_index: 0,
            region_count: 1,
        }),
        ProgressiveBlock::Region(ProgressiveRegion { tiles, ..region }),
        ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
    ])
    .unwrap();
    let mut decoder = ProgressiveDecoder::new();
    decoder.begin_frame();
    let out = decoder.decode_bitmap(0, 3, 1280, 800, &stream).expect("decode");
    decoder.end_frame();
    let mut tiles: Vec<_> = out.into_iter().map(|t| (t.x_idx, t.y_idx, t.pixels)).collect();
    tiles.sort_by_key(|t| (t.0, t.1));
    tiles
}

#[test]
fn context_subband_diffing_bit_does_not_change_base_tiles() {
    let with_bit = decode_with_context_flags(0x01);
    let without_bit = decode_with_context_flags(0x00);
    assert_eq!(with_bit.len(), 16);
    assert_eq!(without_bit.len(), 16);

    let max_diff = with_bit
        .iter()
        .zip(&without_bit)
        .flat_map(|(a, b)| a.2.iter().zip(&b.2).map(|(p, q)| (i32::from(*p) - i32::from(*q)).abs()))
        .max()
        .unwrap();
    eprintln!("max channel difference between CONTEXT flags 0x01 and 0x00: {max_diff}");
    assert_eq!(
        max_diff, 0,
        "decoded base tiles depend on CONTEXT bit 0 (RFX_SUBBAND_DIFFING); the DWT variant must come \
         from REGION bit 0 (RFX_DWT_REDUCE_EXTRAPOLATE)"
    );
}
