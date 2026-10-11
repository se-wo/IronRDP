//! Plausibility check on a real Windows RemoteFX Progressive capture
//! (`wts2_progressive_tile_first_mixed_25tiles.bin`, vendored Haven fixture,
//! Windows Server 2025 / Windows 11 24H2).
//!
//! The capture carries only FRAME_BEGIN / REGION / FRAME_END; SYNC + CONTEXT
//! were sent earlier in the session and are not part of the fixture, and 9 of
//! its 25 tiles are difference tiles whose reference was never captured.
//! This tool therefore builds a self-contained stream:
//!
//!   SYNC + CONTEXT(flags = F) + FRAME_BEGIN + REGION(16 base tiles) + FRAME_END
//!
//! keeping the capture's rects, quant tables, progressive quant table, region
//! flags and tile payloads byte for byte.  It writes the stream for F = 0 and
//! F = 1 (MS-RDPEGFX 2.2.4.2.1.4: CONTEXT bit 0 is RFX_SUBBAND_DIFFING;
//! 2.2.4.2.1.5: REGION bit 0 is RFX_DWT_REDUCE_EXTRAPOLATE, which the capture
//! sets), and for each stream:
//!
//! * `ironrdp_ctx<F>.rgba`  - IronRDP's `ProgressiveDecoder` output
//! * `ref115_ctx<F>.rgba`   - IronRDP's own first-pass entropy decode
//!   (`decode_first_pass`), then inverse DWT + YCbCr->RGB in 11.5 fixed point,
//!   using the DWT variant the REGION flags select (independent of F)
//! * `mask.bin`             - 1 where a base tile intersects a region rect
//!
//! All buffers are 1280x800 RGBA (the capture's surface size).
//! FreeRDP decodes `stream_ctx<F>.bin` via `freerdp_check progressive`.
//!
//! Usage: `cargo run --release --bin progressive_fixture -- <repo root> <out dir>`

use std::fs;
use std::path::PathBuf;

use ironrdp_core::{Decode as _, ReadCursor};
use ironrdp_egfx::pdu::GfxPdu;
use ironrdp_graphics::color_conversion::{YCbCrBuffer, ycbcr_to_rgba};
use ironrdp_graphics::progressive::{ProgressiveDecoder, decode_first_pass};
use ironrdp_graphics::{dwt, dwt_extrapolate};
use ironrdp_pdu::codecs::rfx::progressive::{
    ComponentCodecQuant, ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu,
    ProgressiveRegion, ProgressiveSyncPdu, ProgressiveTile, TILE_FLAG_DIFFERENCE, decode_progressive_stream,
    encode_progressive_stream,
};

const SW: usize = 1280;
const SH: usize = 800;

fn main() {
    let mut args = std::env::args().skip(1);
    let root = PathBuf::from(args.next().expect("repo root"));
    let out = PathBuf::from(args.next().expect("out dir"));
    fs::create_dir_all(&out).unwrap();

    let bytes = fs::read(
        root.join("crates/ironrdp-testsuite-core/test_data/egfx/haven/wts2_progressive_tile_first_mixed_25tiles.bin"),
    )
    .unwrap();
    let GfxPdu::WireToSurface2(pdu) = GfxPdu::decode(&mut ReadCursor::new(&bytes)).unwrap() else {
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

    let base_tiles: Vec<ProgressiveTile<'_>> = region
        .tiles
        .iter()
        .filter(|t| matches!(t, ProgressiveTile::First(f) if f.flags & TILE_FLAG_DIFFERENCE == 0))
        .cloned()
        .collect();
    let region_re = region.uses_reduce_extrapolate();
    eprintln!(
        "capture: {} tiles, {} base tiles, region flags=0x{:02x} (reduce-extrapolate={region_re}), {} rects, \
         {} quant tables, {} progressive quant tables",
        region.tiles.len(),
        base_tiles.len(),
        region.flags,
        region.rects.len(),
        region.quant_vals.len(),
        region.quant_prog_vals.len()
    );

    // Mask: base-tile area intersected with the region rects.
    let mut mask = vec![0u8; SW * SH];
    for t in &base_tiles {
        let ProgressiveTile::First(f) = t else { unreachable!() };
        for r in &region.rects {
            let (x0, y0) = (usize::from(f.x_idx) * 64, usize::from(f.y_idx) * 64);
            let (rx0, ry0) = (usize::from(r.x), usize::from(r.y));
            let (rx1, ry1) = (rx0 + usize::from(r.width), ry0 + usize::from(r.height));
            for y in y0.max(ry0)..(y0 + 64).min(ry1).min(SH) {
                for x in x0.max(rx0)..(x0 + 64).min(rx1).min(SW) {
                    mask[y * SW + x] = 1;
                }
            }
        }
    }
    fs::write(out.join("mask.bin"), &mask).unwrap();

    for ctx_flags in [0u8, 1u8] {
        let stripped = ProgressiveRegion {
            tiles: base_tiles.clone(),
            ..region.clone()
        };
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
            ProgressiveBlock::Region(stripped),
            ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
        ])
        .unwrap();
        fs::write(out.join(format!("stream_ctx{ctx_flags}.bin")), &stream).unwrap();

        // IronRDP decoder.
        let mut surface = vec![0u8; SW * SH * 4];
        let mut decoder = ProgressiveDecoder::new();
        decoder.begin_frame();
        let tiles = decoder
            .decode_bitmap(0, 3, SW as u16, SH as u16, &stream)
            .expect("IronRDP decode");
        decoder.end_frame();
        for t in &tiles {
            blit(&mut surface, usize::from(t.x_idx), usize::from(t.y_idx), &t.pixels);
        }
        fs::write(out.join(format!("ironrdp_ctx{ctx_flags}.rgba")), &surface).unwrap();

        // 11.5 reference from IronRDP's entropy decode, DWT variant per REGION flags.
        let mut reference = vec![0u8; SW * SH * 4];
        for t in &base_tiles {
            let ProgressiveTile::First(f) = t else { unreachable!() };
            let prog = if f.quality == 0xFF {
                None
            } else {
                Some(&region.quant_prog_vals[usize::from(f.quality)])
            };
            let mut planes = [vec![0i16; 4096], vec![0i16; 4096], vec![0i16; 4096]];
            let comps = [
                (f.y_data, f.quant_idx_y, prog.map(|p| &p.y_quant)),
                (f.cb_data, f.quant_idx_cb, prog.map(|p| &p.cb_quant)),
                (f.cr_data, f.quant_idx_cr, prog.map(|p| &p.cr_quant)),
            ];
            for ((data, qi, pq), plane) in comps.into_iter().zip(planes.iter_mut()) {
                let base = &region.quant_vals[usize::from(qi)];
                let pq = pq.cloned().unwrap_or(ComponentCodecQuant::LOSSLESS);
                let mut sign = vec![0i8; 4096];
                decode_first_pass(data, base, &pq, region_re, plane, &mut sign).unwrap();
                // 8-bit domain -> 11.5 fixed point, inverse DWT at that precision.
                for v in plane.iter_mut() {
                    *v = i16::try_from((i32::from(*v) << 5).clamp(-32768, 32767)).unwrap();
                }
                let mut temp = vec![0i16; 4096];
                if region_re {
                    dwt_extrapolate::decode(plane, &mut temp);
                } else {
                    dwt::decode(plane, &mut temp);
                }
            }
            let mut rgba = vec![0u8; 64 * 64 * 4];
            ycbcr_to_rgba(
                YCbCrBuffer {
                    y: &planes[0],
                    cb: &planes[1],
                    cr: &planes[2],
                },
                &mut rgba,
            )
            .unwrap();
            blit(&mut reference, usize::from(f.x_idx), usize::from(f.y_idx), &rgba);
        }
        fs::write(out.join(format!("ref115_ctx{ctx_flags}.rgba")), &reference).unwrap();
        eprintln!("ctx flags {ctx_flags}: IronRDP returned {} tiles", tiles.len());
    }
}

fn blit(surface: &mut [u8], tx: usize, ty: usize, tile: &[u8]) {
    for row in 0..64 {
        let y = ty * 64 + row;
        if y >= SH {
            break;
        }
        for col in 0..64 {
            let x = tx * 64 + col;
            if x >= SW {
                break;
            }
            surface[(y * SW + x) * 4..][..4].copy_from_slice(&tile[(row * 64 + col) * 4..][..4]);
        }
    }
}
