//! Codec round-trip harness for the chroma analysis.
//!
//! Reads every `images/<name>.rgba` (+ `<name>.txt` with `width height`),
//! pushes it through each IronRDP codec path (encoder *and* decoder are
//! IronRDP's own code) and writes the decoded picture to
//! `results/decoded/<codec>/<name>.rgba`.  Compressed sizes go to
//! `results/sizes.csv`, the raw encoded streams (for the FreeRDP cross-check)
//! to `results/streams/<codec>/<name>.bin`.
//!
//! Usage: `cargo run --release -- <analysis/chroma dir>`

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use ironrdp_graphics::clearcodec::{ClearCodecDecoder, ClearCodecEncoder};
use ironrdp_graphics::color_conversion::{YCbCrBuffer, to_64x64_ycbcr_tile, ycbcr_to_rgba};
use ironrdp_graphics::image_processing::PixelFormat;
use ironrdp_graphics::progressive::{COEFFICIENTS_PER_COMPONENT, ProgressiveDecoder, encode_first_pass, rgba_to_ycbcr};
use ironrdp_graphics::rdp6::{BitmapStreamDecoder, BitmapStreamEncoder, RgbAChannels};
use ironrdp_graphics::{dwt, quantization, rfx_encode_component, rlgr, subband_reconstruction};
use ironrdp_pdu::codecs::rfx::progressive::{
    ComponentCodecQuant, ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu,
    ProgressiveRegion, ProgressiveSyncPdu, ProgressiveTile, TileSimple, encode_progressive_stream,
};
use ironrdp_pdu::codecs::rfx::{EntropyAlgorithm, Quant, RfxRectangle};

struct Image {
    name: String,
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

struct Output {
    decoded_rgba: Vec<u8>,
    encoded: Vec<u8>,
    note: String,
}

fn main() {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: chroma-harness <analysis/chroma dir>"),
    );
    let images = load_images(&root.join("images"));
    let results = root.join("results");
    fs::create_dir_all(&results).unwrap();

    let mut csv = String::from("codec,image,encoded_bytes,note\n");

    #[expect(clippy::type_complexity)]
    let codecs: Vec<(&str, Box<dyn Fn(&Image) -> Output>)> = vec![
        (
            "rfx_quant_default",
            Box::new(|img| rfx_roundtrip(img, &Quant::default())),
        ),
        ("rfx_quant_6", Box::new(|img| rfx_roundtrip(img, &quant_all(6)))),
        // What-if: identical pipeline, but quantisation rounds as MS-RDPRFX 3.1.8.1.5 says.
        (
            "whatif_rfx_quant_default_rounded",
            Box::new(|img| rfx_roundtrip_rounded(img, &Quant::default())),
        ),
        (
            "whatif_rfx_quant_6_rounded",
            Box::new(|img| rfx_roundtrip_rounded(img, &quant_all(6))),
        ),
        (
            "progressive_simple_q6",
            Box::new(|img| progressive_roundtrip(img, &ccq_all(6), false)),
        ),
        (
            "progressive_simple_q6_re",
            Box::new(|img| progressive_roundtrip(img, &ccq_all(6), true)),
        ),
        (
            "progressive_simple_default",
            Box::new(|img| progressive_roundtrip(img, &ccq_rfx_default(), false)),
        ),
        ("nscodec_cll1", Box::new(|img| nscodec_roundtrip(img, 1))),
        ("nscodec_cll3", Box::new(|img| nscodec_roundtrip(img, 3))),
        ("nscodec_cll7", Box::new(|img| nscodec_roundtrip(img, 7))),
        ("clearcodec", Box::new(clearcodec_roundtrip)),
        ("clearcodec_glyph_tiles", Box::new(clearcodec_glyph_roundtrip)),
        ("planar", Box::new(planar_roundtrip)),
        ("avc420_ironrdp_openh264", Box::new(avc420_ironrdp_roundtrip)),
        ("avc420_colorconv_only_current", Box::new(avc420_colorconv_current)),
        ("avc420_colorconv_only_bt709full", Box::new(avc420_colorconv_matched)),
    ];

    for (codec, f) in &codecs {
        let dec_dir = results.join("decoded").join(codec);
        let stream_dir = results.join("streams").join(codec);
        fs::create_dir_all(&dec_dir).unwrap();
        fs::create_dir_all(&stream_dir).unwrap();
        for img in &images {
            let out = f(img);
            assert_eq!(
                out.decoded_rgba.len(),
                img.width * img.height * 4,
                "{codec}/{}",
                img.name
            );
            fs::write(dec_dir.join(format!("{}.rgba", img.name)), &out.decoded_rgba).unwrap();
            fs::write(stream_dir.join(format!("{}.bin", img.name)), &out.encoded).unwrap();
            writeln!(csv, "{codec},{},{},{}", img.name, out.encoded.len(), out.note).unwrap();
        }
        eprintln!("done: {codec}");
    }

    fs::write(results.join("sizes.csv"), csv).unwrap();
    fs::write(results.join("dwt_reversibility.csv"), dwt_reversibility(&images)).unwrap();
    fs::write(results.join("chroma_heuristic.txt"), chroma_heuristic(&images)).unwrap();
}

fn load_images(dir: &Path) -> Vec<Image> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            (p.extension()? == "rgba").then(|| p.file_stem().unwrap().to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let dims = fs::read_to_string(dir.join(format!("{name}.txt"))).unwrap();
            let mut it = dims.split_whitespace().map(|v| v.parse::<usize>().unwrap());
            let (width, height) = (it.next().unwrap(), it.next().unwrap());
            let rgba = fs::read(dir.join(format!("{name}.rgba"))).unwrap();
            assert_eq!(rgba.len(), width * height * 4);
            Image {
                name,
                width,
                height,
                rgba,
            }
        })
        .collect()
}

fn quant_all(v: u8) -> Quant {
    Quant {
        ll3: v,
        lh3: v,
        hl3: v,
        hh3: v,
        lh2: v,
        hl2: v,
        hh2: v,
        lh1: v,
        hl1: v,
        hh1: v,
    }
}

fn ccq_all(v: u8) -> ComponentCodecQuant {
    ComponentCodecQuant {
        ll3: v,
        hl3: v,
        lh3: v,
        hh3: v,
        hl2: v,
        lh2: v,
        hh2: v,
        hl1: v,
        lh1: v,
        hh1: v,
    }
}

/// Same numbers as `Quant::default()` (FreeRDP's RemoteFX default table).
fn ccq_rfx_default() -> ComponentCodecQuant {
    ComponentCodecQuant {
        ll3: 6,
        hl3: 6,
        lh3: 6,
        hh3: 6,
        hl2: 7,
        lh2: 7,
        hh2: 8,
        hl1: 8,
        lh1: 8,
        hh1: 9,
    }
}

fn tile_rgba(img: &Image, tx: usize, ty: usize) -> Vec<u8> {
    let mut out = vec![0u8; 64 * 64 * 4];
    for row in 0..64 {
        let y = (ty * 64 + row).min(img.height - 1);
        for col in 0..64 {
            let x = (tx * 64 + col).min(img.width - 1);
            let s = (y * img.width + x) * 4;
            out[(row * 64 + col) * 4..][..4].copy_from_slice(&img.rgba[s..s + 4]);
        }
    }
    out
}

fn put_tile(dst: &mut [u8], img: &Image, tx: usize, ty: usize, tile: &[u8]) {
    for row in 0..64 {
        let y = ty * 64 + row;
        if y >= img.height {
            break;
        }
        for col in 0..64 {
            let x = tx * 64 + col;
            if x >= img.width {
                break;
            }
            dst[(y * img.width + x) * 4..][..4].copy_from_slice(&tile[(row * 64 + col) * 4..][..4]);
        }
    }
}

/// Classic RemoteFX, same per-tile pipeline as `ironrdp-server`'s `RfxEncoder`
/// (`to_64x64_ycbcr_tile` + `rfx_encode_component`, one quant table for Y, Cb
/// and Cr) and as `ironrdp-session`'s decoder (`decode_component`).
fn rfx_roundtrip(img: &Image, quant: &Quant) -> Output {
    rfx_roundtrip_impl(img, quant, |comp, buf, quant| {
        rfx_encode_component(comp, buf, quant, EntropyAlgorithm::Rlgr3).unwrap()
    })
}

/// Same as `rfx_roundtrip`, but the encoder rounds when quantising
/// (`(c + 2^(f-1)) >> f`) instead of `ironrdp-graphics`' plain `c >> f`.
fn rfx_roundtrip_rounded(img: &Image, quant: &Quant) -> Output {
    rfx_roundtrip_impl(img, quant, |comp, buf, quant| {
        let mut temp = [0i16; 4096];
        dwt::encode(comp, &mut temp);
        // Band layout as in ironrdp_graphics::quantization.
        let factors = [
            (0, 1024, quant.hl1),
            (1024, 1024, quant.lh1),
            (2048, 1024, quant.hh1),
            (3072, 256, quant.hl2),
            (3328, 256, quant.lh2),
            (3584, 256, quant.hh2),
            (3840, 64, quant.hl3),
            (3904, 64, quant.lh3),
            (3968, 64, quant.hh3),
            (4032, 64, quant.ll3),
        ];
        for (start, len, q) in factors {
            let f = i32::from(q) - 1;
            if f > 0 {
                for c in &mut comp[start..start + len] {
                    *c = i16::try_from((i32::from(*c) + (1 << (f - 1))) >> f).unwrap();
                }
            }
        }
        subband_reconstruction::encode(&mut comp[4032..]);
        rlgr::encode(EntropyAlgorithm::Rlgr3, comp, buf).unwrap()
    })
}

fn rfx_roundtrip_impl(img: &Image, quant: &Quant, encode: impl Fn(&mut [i16], &mut [u8], &Quant) -> usize) -> Output {
    let mut decoded = vec![0u8; img.width * img.height * 4];
    let mut encoded = Vec::new();
    for ty in 0..img.height.div_ceil(64) {
        for tx in 0..img.width.div_ceil(64) {
            let tile = tile_rgba(img, tx, ty);
            let mut y = [0i16; 4096];
            let mut cb = [0i16; 4096];
            let mut cr = [0i16; 4096];
            to_64x64_ycbcr_tile(&tile, 64, 64, 64 * 4, PixelFormat::RgbA32, &mut y, &mut cb, &mut cr).unwrap();
            let mut planes = [vec![0i16; 4096], vec![0i16; 4096], vec![0i16; 4096]];
            for (comp, plane) in [&mut y[..], &mut cb[..], &mut cr[..]]
                .into_iter()
                .zip(planes.iter_mut())
            {
                let mut buf = vec![0u8; 64 * 64 * 4];
                let len = encode(comp, &mut buf, quant);
                encoded.extend_from_slice(&buf[..len]);
                // Decode exactly like ironrdp-session::rfx::decode_component.
                let mut temp = vec![0i16; 4096];
                rlgr::decode(EntropyAlgorithm::Rlgr3, &buf[..len], plane).unwrap();
                subband_reconstruction::decode(&mut plane[4032..]);
                quantization::decode(plane, quant);
                dwt::decode(plane, &mut temp);
            }
            let mut out = vec![0u8; 64 * 64 * 4];
            ycbcr_to_rgba(
                YCbCrBuffer {
                    y: &planes[0],
                    cb: &planes[1],
                    cr: &planes[2],
                },
                &mut out,
            )
            .unwrap();
            put_tile(&mut decoded, img, tx, ty, &out);
        }
    }
    Output {
        decoded_rgba: decoded,
        encoded,
        note: String::new(),
    }
}

/// RemoteFX Progressive, single TILE_SIMPLE pass per tile (i.e. the
/// best a progressive stream can converge to for the given base quant),
/// encoded with IronRDP's building blocks and decoded by
/// `ProgressiveDecoder::decode_bitmap`.
fn progressive_roundtrip(img: &Image, base_quant: &ComponentCodecQuant, reduce_extrapolate: bool) -> Output {
    let tiles_x = img.width.div_ceil(64);
    let tiles_y = img.height.div_ceil(64);
    let mut comp_data: Vec<[Vec<u8>; 3]> = Vec::new();
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            let tile = tile_rgba(img, tx, ty);
            let mut y = vec![0i16; COEFFICIENTS_PER_COMPONENT];
            let mut cb = vec![0i16; COEFFICIENTS_PER_COMPONENT];
            let mut cr = vec![0i16; COEFFICIENTS_PER_COMPONENT];
            rgba_to_ycbcr(&tile, &mut y, &mut cb, &mut cr);
            let enc = |c: &mut [i16]| {
                let mut buf = vec![0u8; 64 * 64 * 4];
                let len = encode_first_pass(
                    c,
                    &mut buf,
                    base_quant,
                    &ComponentCodecQuant::LOSSLESS,
                    reduce_extrapolate,
                )
                .unwrap();
                buf.truncate(len);
                buf
            };
            comp_data.push([enc(&mut y), enc(&mut cb), enc(&mut cr)]);
        }
    }
    let tiles: Vec<ProgressiveTile<'_>> = comp_data
        .iter()
        .enumerate()
        .map(|(i, [y, cb, cr])| {
            ProgressiveTile::Simple(TileSimple {
                quant_idx_y: 0,
                quant_idx_cb: 0,
                quant_idx_cr: 0,
                x_idx: u16::try_from(i % tiles_x).unwrap(),
                y_idx: u16::try_from(i / tiles_x).unwrap(),
                flags: 0,
                y_data: y,
                cb_data: cb,
                cr_data: cr,
                tail_data: &[],
            })
        })
        .collect();
    let width = u16::try_from(img.width).unwrap();
    let height = u16::try_from(img.height).unwrap();
    let region = ProgressiveRegion {
        tile_size: 0x40,
        rects: vec![RfxRectangle {
            x: 0,
            y: 0,
            width,
            height,
        }],
        quant_vals: vec![base_quant.clone()],
        quant_prog_vals: vec![],
        flags: 0,
        tiles,
    };
    let stream = encode_progressive_stream(&[
        ProgressiveBlock::Sync(ProgressiveSyncPdu),
        ProgressiveBlock::Context(ProgressiveContextPdu {
            context_id: 0,
            tile_size: 0x0040,
            flags: u8::from(reduce_extrapolate),
        }),
        ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu {
            frame_index: 0,
            region_count: 1,
        }),
        ProgressiveBlock::Region(region),
        ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
    ])
    .unwrap();

    let mut decoder = ProgressiveDecoder::new();
    decoder.begin_frame();
    let tiles = decoder.decode_bitmap(1, 0, width, height, &stream).unwrap();
    decoder.end_frame();
    let mut decoded = vec![0u8; img.width * img.height * 4];
    for t in &tiles {
        put_tile(&mut decoded, img, usize::from(t.x_idx), usize::from(t.y_idx), &t.pixels);
    }
    Output {
        decoded_rgba: decoded,
        encoded: stream,
        note: format!("tiles={}", tiles.len()),
    }
}

/// NSCodec: `ironrdp_nscodec::encoder::encode` (what `ironrdp-server` sends in
/// SurfaceBits).  IronRDP has no stand-alone NSCodec decoder, so the stream is
/// wrapped as a ClearCodec NSCodec subcodec and decoded by `ClearCodecDecoder`
/// (which uses `clearcodec::nscodec::decode`).  The encoder emits bottom-up
/// rows (SurfaceBits convention) while the ClearCodec subcodec is top-down, so
/// the decoded rows are flipped back.
fn nscodec_roundtrip(img: &Image, cll: u8) -> Output {
    let w = u16::try_from(img.width).unwrap();
    let h = u16::try_from(img.height).unwrap();
    let ns = ironrdp_nscodec::encoder::encode(&img.rgba, w, h, img.width * 4, PixelFormat::RgbA32, cll);

    let mut sub = Vec::new();
    sub.extend_from_slice(&0u16.to_le_bytes()); // xStart
    sub.extend_from_slice(&0u16.to_le_bytes()); // yStart
    sub.extend_from_slice(&w.to_le_bytes());
    sub.extend_from_slice(&h.to_le_bytes());
    sub.extend_from_slice(&u32::try_from(ns.len()).unwrap().to_le_bytes());
    sub.push(1); // subCodecId = NSCodec
    sub.extend_from_slice(&ns);
    let mut cc = vec![0u8 /* flags */, 0u8 /* seq */];
    cc.extend_from_slice(&0u32.to_le_bytes()); // residualByteCount
    cc.extend_from_slice(&0u32.to_le_bytes()); // bandsByteCount
    cc.extend_from_slice(&u32::try_from(sub.len()).unwrap().to_le_bytes());
    cc.extend_from_slice(&sub);

    let bgra = ClearCodecDecoder::new().decode(&cc, w, h).unwrap();
    let mut decoded = vec![0u8; img.width * img.height * 4];
    for y in 0..img.height {
        let src_row = img.height - 1 - y;
        for x in 0..img.width {
            let s = (src_row * img.width + x) * 4;
            let d = (y * img.width + x) * 4;
            decoded[d] = bgra[s + 2];
            decoded[d + 1] = bgra[s + 1];
            decoded[d + 2] = bgra[s];
            decoded[d + 3] = 255;
        }
    }
    Output {
        decoded_rgba: decoded,
        encoded: ns,
        note: format!("cll={cll}"),
    }
}

fn rgba_to_bgra(rgba: &[u8]) -> Vec<u8> {
    rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], p[3]]).collect()
}

fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
    bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect()
}

fn clearcodec_roundtrip(img: &Image) -> Output {
    let w = u16::try_from(img.width).unwrap();
    let h = u16::try_from(img.height).unwrap();
    let enc = ClearCodecEncoder::new().encode(&rgba_to_bgra(&img.rgba), w, h);
    let dec = ClearCodecDecoder::new().decode(&enc, w, h).unwrap();
    let decoded = bgra_to_rgba(&dec);
    let exact = decoded == img.rgba;
    Output {
        decoded_rgba: decoded,
        encoded: enc,
        note: format!("bit_exact={exact}"),
    }
}

/// 32x32 tiles (<= 1024 px, glyph-cache eligible).  Every tile is encoded
/// twice with the same encoder: the second encode must be a glyph-cache hit
/// and the decoder must reproduce the first result from its glyph cache.
fn clearcodec_glyph_roundtrip(img: &Image) -> Output {
    const T: usize = 32;
    let mut enc = ClearCodecEncoder::new();
    let mut dec = ClearCodecDecoder::new();
    let mut decoded = vec![0u8; img.width * img.height * 4];
    let mut encoded = Vec::new();
    let (mut hits, mut mismatch) = (0usize, 0usize);
    for ty in 0..img.height / T {
        for tx in 0..img.width / T {
            let mut tile = Vec::with_capacity(T * T * 4);
            for row in 0..T {
                let s = ((ty * T + row) * img.width + tx * T) * 4;
                tile.extend_from_slice(&img.rgba[s..s + T * 4]);
            }
            let bgra = rgba_to_bgra(&tile);
            let first = enc.encode(&bgra, 32, 32);
            let second = enc.encode(&bgra, 32, 32);
            if second.len() == 4 {
                hits += 1;
            }
            let d1 = dec.decode(&first, 32, 32).unwrap();
            let d2 = dec.decode(&second, 32, 32).unwrap();
            if d1 != d2 {
                mismatch += 1;
            }
            encoded.extend_from_slice(&first);
            encoded.extend_from_slice(&second);
            let rgba = bgra_to_rgba(&d2);
            for row in 0..T {
                let d = ((ty * T + row) * img.width + tx * T) * 4;
                decoded[d..d + T * 4].copy_from_slice(&rgba[row * T * 4..(row + 1) * T * 4]);
            }
        }
    }
    let exact = decoded == img.rgba;
    Output {
        decoded_rgba: decoded,
        encoded,
        note: format!("bit_exact={exact} glyph_hits={hits} hit_mismatch={mismatch}"),
    }
}

fn planar_roundtrip(img: &Image) -> Output {
    let mut enc = BitmapStreamEncoder::new(img.width, img.height);
    let mut buf = vec![0u8; img.width * img.height * 4 + 64];
    let len = enc.encode_bitmap::<RgbAChannels>(&img.rgba, &mut buf, true).unwrap();
    buf.truncate(len);
    let mut rgb = Vec::new();
    BitmapStreamDecoder::default()
        .decode_bitmap_stream_to_rgb24(&buf, &mut rgb, img.width, img.height)
        .unwrap();
    let decoded: Vec<u8> = rgb.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    let exact = decoded == img.rgba;
    Output {
        decoded_rgba: decoded,
        encoded: buf,
        note: format!("bit_exact={exact}"),
    }
}

/// The AVC420 path exactly as shipped: `OpenH264Encoder` (default config,
/// openh264's own RGB->YUV) -> Annex B -> AVC -> `OpenH264Decoder`
/// (full-range BT.709 YUV->RGB since #1923).
fn avc420_ironrdp_roundtrip(img: &Image) -> Output {
    use ironrdp_egfx::decode::{H264Decoder as _, OpenH264Decoder};
    use ironrdp_egfx::encode::{EncodeFrame, H264Encoder as _, OpenH264Encoder};

    let mut enc = OpenH264Encoder::new().unwrap();
    enc.request_key_frame();
    let annex_b = enc
        .encode(EncodeFrame {
            data: &img.rgba,
            width: u32::try_from(img.width).unwrap(),
            height: u32::try_from(img.height).unwrap(),
        })
        .unwrap();
    let avc = ironrdp_egfx::pdu::annex_b_to_avc(&annex_b);
    let frame = OpenH264Decoder::new().unwrap().decode(&avc).unwrap();
    assert_eq!(frame.width() as usize, img.width);
    Output {
        decoded_rgba: frame.into_data(),
        encoded: annex_b,
        note: "openh264 default EncoderConfig".to_owned(),
    }
}

/// Colour conversion only (no H.264): openh264's `YUVBuffer::from_rgb_source`
/// (what `OpenH264Encoder` does) followed by the full-range BT.709 inverse
/// that `OpenH264Decoder` applies.
fn avc420_colorconv_current(img: &Image) -> Output {
    use openh264::formats::{RgbaSliceU8, YUVBuffer, YUVSource as _};
    let src = RgbaSliceU8::new(&img.rgba, (img.width, img.height));
    let yuv = YUVBuffer::from_rgb_source(src);
    let (ys, us, vs) = yuv.strides();
    Output {
        decoded_rgba: yuv420_to_rgba_bt709_full(img, yuv.y(), ys, yuv.u(), us, yuv.v(), vs),
        encoded: Vec::new(),
        note: "no compression".to_owned(),
    }
}

/// Colour conversion only, matched full-range BT.709 on both sides (what open
/// upstream PR #1976 proposes for the encoder).
fn avc420_colorconv_matched(img: &Image) -> Output {
    let w = u32::try_from(img.width).unwrap();
    let h = u32::try_from(img.height).unwrap();
    let mut planar = yuv::YuvPlanarImageMut::<u8>::alloc(w, h, yuv::YuvChromaSubsampling::Yuv420);
    yuv::rgba_to_yuv420(
        &mut planar,
        &img.rgba,
        w * 4,
        yuv::YuvRange::Full,
        yuv::YuvStandardMatrix::Bt709,
        yuv::YuvConversionMode::Balanced,
    )
    .unwrap();
    let (ys, us, vs) = (
        planar.y_stride as usize,
        planar.u_stride as usize,
        planar.v_stride as usize,
    );
    Output {
        decoded_rgba: yuv420_to_rgba_bt709_full(
            img,
            planar.y_plane.borrow(),
            ys,
            planar.u_plane.borrow(),
            us,
            planar.v_plane.borrow(),
            vs,
        ),
        encoded: Vec::new(),
        note: "no compression".to_owned(),
    }
}

fn yuv420_to_rgba_bt709_full(img: &Image, y: &[u8], ys: usize, u: &[u8], us: usize, v: &[u8], vs: usize) -> Vec<u8> {
    let w = u32::try_from(img.width).unwrap();
    let h = u32::try_from(img.height).unwrap();
    let planar = yuv::YuvPlanarImage {
        y_plane: y,
        y_stride: u32::try_from(ys).unwrap(),
        u_plane: u,
        u_stride: u32::try_from(us).unwrap(),
        v_plane: v,
        v_stride: u32::try_from(vs).unwrap(),
        width: w,
        height: h,
    };
    let mut rgba = vec![0u8; img.width * img.height * 4];
    yuv::yuv420_to_rgba(
        &planar,
        &mut rgba,
        w * 4,
        yuv::YuvRange::Full,
        yuv::YuvStandardMatrix::Bt709,
    )
    .unwrap();
    rgba
}

/// Forward + inverse DWT without any quantisation, on the exact inputs the
/// codecs feed it: RemoteFX (`to_64x64_ycbcr_tile`, 11.5 fixed point) and
/// Progressive (`rgba_to_ycbcr`, plain 8-bit integers), standard and
/// reduce-extrapolate.  Reports the maximum coefficient error after the
/// round trip, in 8-bit units.
fn dwt_reversibility(images: &[Image]) -> String {
    let mut out = String::from("image,path,max_err_8bit_units\n");
    for img in images {
        let mut worst = [0f64; 3];
        for ty in 0..img.height.div_ceil(64) {
            for tx in 0..img.width.div_ceil(64) {
                let tile = tile_rgba(img, tx, ty);
                let mut y = [0i16; 4096];
                let mut cb = [0i16; 4096];
                let mut cr = [0i16; 4096];
                to_64x64_ycbcr_tile(&tile, 64, 64, 64 * 4, PixelFormat::RgbA32, &mut y, &mut cb, &mut cr).unwrap();
                let mut py = vec![0i16; 4096];
                let mut pcb = vec![0i16; 4096];
                let mut pcr = vec![0i16; 4096];
                rgba_to_ycbcr(&tile, &mut py, &mut pcb, &mut pcr);
                let mut temp = [0i16; 4096];
                for c in [&y[..], &cb[..], &cr[..]] {
                    let mut b = c.to_vec();
                    dwt::encode(&mut b, &mut temp);
                    dwt::decode(&mut b, &mut temp);
                    let e = c
                        .iter()
                        .zip(&b)
                        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                        .max()
                        .unwrap();
                    worst[0] = worst[0].max(f64::from(e) / 32.0);
                }
                for c in [&py, &pcb, &pcr] {
                    let mut b = c.clone();
                    dwt::encode(&mut b, &mut temp);
                    dwt::decode(&mut b, &mut temp);
                    let e = c
                        .iter()
                        .zip(&b)
                        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                        .max()
                        .unwrap();
                    worst[1] = worst[1].max(f64::from(e));
                    let mut b = c.clone();
                    ironrdp_graphics::dwt_extrapolate::encode(&mut b, &mut temp);
                    ironrdp_graphics::dwt_extrapolate::decode(&mut b, &mut temp);
                    let e = c
                        .iter()
                        .zip(&b)
                        .map(|(a, b)| (i32::from(*a) - i32::from(*b)).abs())
                        .max()
                        .unwrap();
                    worst[2] = worst[2].max(f64::from(e));
                }
            }
        }
        for (path, w) in [
            "rfx_11.5_fixed",
            "progressive_8bit",
            "progressive_8bit_reduce_extrapolate",
        ]
        .iter()
        .zip(worst)
        {
            writeln!(out, "{},{path},{w}", img.name).unwrap();
        }
    }
    out
}

/// Feasibility probe for a routing signal "high chroma energy at low luma
/// contrast" (analysis only).  Per 16x16 block, using the MS-RDPEGFX
/// 3.3.8.3.1 integer matrix:
///   luma_range   = max(Y) - min(Y)
///   chroma_energy = mean(|dU/dx| + |dU/dy| + |dV/dx| + |dV/dy|)
/// A block is flagged when chroma_energy >= 12 and luma_range <= 160.
/// Reports the fraction of flagged blocks per image and the single-thread
/// cost on a 1920x1080 frame (tiled from the test images).
fn chroma_heuristic(images: &[Image]) -> String {
    fn yuv(p: &[u8]) -> (i32, i32, i32) {
        let (r, g, b) = (i32::from(p[0]), i32::from(p[1]), i32::from(p[2]));
        (
            (54 * r + 183 * g + 18 * b) >> 8,
            ((-29 * r - 99 * g + 128 * b) >> 8) + 128,
            ((128 * r - 116 * g - 12 * b) >> 8) + 128,
        )
    }
    fn classify(rgba: &[u8], w: usize, h: usize, flags: &mut Vec<bool>) {
        flags.clear();
        for by in (0..h - h % 16).step_by(16) {
            for bx in (0..w - w % 16).step_by(16) {
                let (mut ymin, mut ymax, mut energy) = (255, 0, 0i32);
                for y in by..by + 16 {
                    for x in bx..bx + 16 {
                        let i = (y * w + x) * 4;
                        let (yy, u, v) = yuv(&rgba[i..i + 4]);
                        ymin = ymin.min(yy);
                        ymax = ymax.max(yy);
                        if x + 1 < w && y + 1 < h {
                            let (_, ur, vr) = yuv(&rgba[i + 4..i + 8]);
                            let (_, ud, vd) = yuv(&rgba[i + w * 4..i + w * 4 + 4]);
                            energy += (u - ur).abs() + (u - ud).abs() + (v - vr).abs() + (v - vd).abs();
                        }
                    }
                }
                flags.push(energy / 256 >= 12 && ymax - ymin <= 160);
            }
        }
    }
    let mut out = String::from("image,flagged_blocks,total_blocks\n");
    let mut flags = Vec::new();
    for img in images {
        classify(&img.rgba, img.width, img.height, &mut flags);
        writeln!(
            out,
            "{},{},{}",
            img.name,
            flags.iter().filter(|f| **f).count(),
            flags.len()
        )
        .unwrap();
    }
    // 1920x1080 frame tiled from all images.
    let (fw, fh) = (1920usize, 1088usize);
    let mut frame = vec![0u8; fw * fh * 4];
    for y in 0..fh {
        for x in 0..fw {
            let img = &images[(x / 320 + (y / 128) * 6) % images.len()];
            let s = ((y % 128) * img.width + (x % 320)) * 4;
            frame[(y * fw + x) * 4..][..4].copy_from_slice(&img.rgba[s..s + 4]);
        }
    }
    let runs = 20;
    let t = std::time::Instant::now();
    for _ in 0..runs {
        classify(&frame, fw, fh, &mut flags);
    }
    let ms = t.elapsed().as_secs_f64() * 1000.0 / f64::from(runs);
    writeln!(
        out,
        "# 1920x1088 full frame, scalar, single thread: {ms:.2} ms per frame ({} blocks)",
        flags.len()
    )
    .unwrap();
    out
}
