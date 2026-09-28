//! C-07 (decoder side): does IronRDP's Progressive decoder reconstruct
//! Windows-style coefficients as precisely as an 11.5 fixed-point decoder?
//!
//! Windows and FreeRDP run RemoteFX colour conversion and DWT in 11.5 fixed
//! point (values x32) and quantise by `2^(q-1)`. On the wire that is the same
//! scale as IronRDP's 8-bit domain quantised by `2^(q-6)` (MS-RDPRFX
//! 3.1.8.1.5 scale value `1 << (q - 6)` on the [-128, 127] range), so IronRDP
//! can decode those streams, but its inverse DWT then runs on plain integers
//! and loses the fractional bits MS-RDPRFX 3.1.8.1.4's lifting steps need.
//!
//! Construction of the "Windows-like" stream, for a 64x64 tile:
//!   1. RGB -> YCbCr in 11.5 (`to_64x64_ycbcr_tile`, what RemoteFX uses)
//!   2. forward DWT in 11.5 (reduce-extrapolate, as the Windows captures
//!      use, or standard)
//!   3. quantise with q = 6 for every band: round(c / 2^(6-1))
//!   4. LL3 delta + RLGR1, packed as one TILE_SIMPLE with quant table all 6
//!
//! Reference decode (11.5): c << 5, inverse DWT in 11.5, YCbCr -> RGB (11.5).
//! IronRDP decode: `ProgressiveDecoder::decode_bitmap` on the same stream.
//!
//! The test asserts that IronRDP stays within 2 levels of the 11.5 reference;
//! a failure confirms the hypothesis that its 8-bit inverse DWT costs
//! precision on real (Windows) Progressive streams.

use ironrdp_graphics::color_conversion::{YCbCrBuffer, to_64x64_ycbcr_tile, ycbcr_to_rgba};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::progressive::ProgressiveDecoder;
use ironrdp_graphics::{dwt, dwt_extrapolate, rlgr, subband_reconstruction};
use ironrdp_pdu::codecs::rfx::progressive::{
    ComponentCodecQuant, ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu,
    ProgressiveRegion, ProgressiveSyncPdu, ProgressiveTile, TileSimple, encode_progressive_stream,
};
use ironrdp_pdu::codecs::rfx::{EntropyAlgorithm, RfxRectangle};

const Q6: ComponentCodecQuant = ComponentCodecQuant {
    ll3: 6,
    hl3: 6,
    lh3: 6,
    hh3: 6,
    hl2: 6,
    lh2: 6,
    hh2: 6,
    hl1: 6,
    lh1: 6,
    hh1: 6,
};

/// Glyph-like red strokes and a 1-px red/blue checker on black: the content
/// the chroma analysis is about.
fn tile_red_strokes() -> Vec<u8> {
    let mut t = vec![0u8; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            let rgb = if y < 32 {
                if (x * 7 + y * 3) % 11 < 3 || x % 13 == 0 {
                    [255, 0, 0]
                } else {
                    [0, 0, 0]
                }
            } else if (x + y) % 2 == 0 {
                [255, 0, 0]
            } else {
                [0, 0, 255]
            };
            t[(y * 64 + x) * 4..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    t
}

/// Smooth colour gradient (should be easy for any decoder).
fn tile_gradient() -> Vec<u8> {
    let mut t = vec![0u8; 64 * 64 * 4];
    for y in 0..64 {
        for x in 0..64 {
            t[(y * 64 + x) * 4..][..4].copy_from_slice(&[(x * 4) as u8, (y * 4) as u8, 128, 255]);
        }
    }
    t
}

struct Result {
    max_ironrdp_vs_reference: i32,
    max_reference_vs_source: i32,
    max_ironrdp_vs_source: i32,
}

fn run(tile: &[u8], reduce_extrapolate: bool) -> Result {
    let mut comps = [[0i16; 4096]; 3];
    {
        let [y, cb, cr] = &mut comps;
        to_64x64_ycbcr_tile(tile, 64, 64, 64 * 4, PixelFormat::RgbA32, y, cb, cr).unwrap();
    }
    let mut temp = [0i16; 4096];

    // Steps 2 and 3: forward DWT in 11.5, quantise with rounding by 2^5.
    let mut wire = comps;
    for c in &mut wire {
        if reduce_extrapolate {
            dwt_extrapolate::encode(c, &mut temp);
        } else {
            dwt::encode(c, &mut temp);
        }
        for v in c.iter_mut() {
            *v = i16::try_from((i32::from(*v) + 16) >> 5).unwrap();
        }
    }

    // Reference decode in 11.5.
    let mut reference = wire;
    for c in &mut reference {
        for v in c.iter_mut() {
            *v = i16::try_from(i32::from(*v) << 5).unwrap();
        }
        if reduce_extrapolate {
            dwt_extrapolate::decode(c, &mut temp);
        } else {
            dwt::decode(c, &mut temp);
        }
    }
    let mut reference_rgba = vec![0u8; 64 * 64 * 4];
    ycbcr_to_rgba(
        YCbCrBuffer {
            y: &reference[0],
            cb: &reference[1],
            cr: &reference[2],
        },
        &mut reference_rgba,
    )
    .unwrap();

    // Step 4: pack as TILE_SIMPLE and let IronRDP decode it.
    let ll3 = if reduce_extrapolate { 4015 } else { 4032 };
    let encoded: Vec<Vec<u8>> = wire
        .iter()
        .map(|c| {
            let mut c = *c;
            subband_reconstruction::encode(&mut c[ll3..]);
            let mut buf = vec![0u8; 64 * 64 * 4];
            let len = rlgr::encode(EntropyAlgorithm::Rlgr1, &c, &mut buf).unwrap();
            buf.truncate(len);
            buf
        })
        .collect();
    let stream = encode_progressive_stream(&[
        ProgressiveBlock::Sync(ProgressiveSyncPdu),
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 0x40,
            flags: u8::from(reduce_extrapolate),
        }),
        ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
            frame_index: 0,
            region_count: 1,
        }),
        ProgressiveBlock::Region(ProgressiveRegion {
            tile_size: 0x40,
            rects: vec![RfxRectangle {
                x: 0,
                y: 0,
                width: 64,
                height: 64,
            }],
            quant_vals: vec![Q6],
            quant_prog_vals: vec![],
            flags: u8::from(reduce_extrapolate),
            tiles: vec![ProgressiveTile::Simple(TileSimple {
                quant_idx_y: 0,
                quant_idx_cb: 0,
                quant_idx_cr: 0,
                x_idx: 0,
                y_idx: 0,
                flags: 0,
                y_data: &encoded[0],
                cb_data: &encoded[1],
                cr_data: &encoded[2],
                tail_data: &[],
            })],
        }),
        ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
    ])
    .unwrap();
    let mut decoder = ProgressiveDecoder::new();
    decoder.begin_frame();
    let tiles = decoder.decode_bitmap(1, 0, 64, 64, &stream).unwrap();
    decoder.end_frame();
    let ironrdp_rgba = &tiles[0].pixels;

    let max_diff = |a: &[u8], b: &[u8]| {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .flat_map(|(p, q)| (0..3).map(move |i| (i32::from(p[i]) - i32::from(q[i])).abs()))
            .max()
            .unwrap()
    };
    Result {
        max_ironrdp_vs_reference: max_diff(ironrdp_rgba, &reference_rgba),
        max_reference_vs_source: max_diff(&reference_rgba, tile),
        max_ironrdp_vs_source: max_diff(ironrdp_rgba, tile),
    }
}

fn check(name: &str, tile: &[u8], reduce_extrapolate: bool) {
    let r = run(tile, reduce_extrapolate);
    eprintln!(
        "{name} (reduce_extrapolate={reduce_extrapolate}): max |IronRDP - 11.5 reference| = {}, \
         max |reference - source| = {}, max |IronRDP - source| = {}",
        r.max_ironrdp_vs_reference, r.max_reference_vs_source, r.max_ironrdp_vs_source
    );
    assert!(
        r.max_ironrdp_vs_reference <= 2,
        "{name}: IronRDP's Progressive decoder deviates by {} levels from an 11.5 fixed-point decode \
         of the same Windows-style coefficients",
        r.max_ironrdp_vs_reference
    );
}

#[test]
fn red_strokes_reduce_extrapolate_like_windows() {
    check("red strokes", &tile_red_strokes(), true);
}

#[test]
fn red_strokes_standard_dwt() {
    check("red strokes", &tile_red_strokes(), false);
}

#[test]
fn gradient_reduce_extrapolate_like_windows() {
    check("gradient", &tile_gradient(), true);
}
