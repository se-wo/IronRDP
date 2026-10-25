//! C-06: the EGFX client must apply the AVC420 region mask.
//!
//! MS-RDPEGFX 2.2.2.1: for AVC codecs `destRect` "specifies a bounding
//! rectangle". 2.2.4.4: the H.264 frame "MUST be cropped by the region mask
//! specified in the regionRects field"; 3.3.8.3.2: "Color conversion MUST be
//! performed for the entire macroblock, after which the region mask in
//! regionRects MUST be applied." The frame therefore lives in surface
//! coordinates and only the region rectangles may change on the surface.
//!
//! The tests drive IronRDP's own server (`send_avc420_frame`, which sets
//! `destRect` to the union of the regions) into IronRDP's own client with the
//! OpenH264 decoder, and inspect the client's composited output.  They assert
//! the spec behaviour, so a failure confirms the bug.
//!
//! Colours: the surface is first painted pure blue with (lossless)
//! ClearCodec; the H.264 frames only use black and white, so the AVC colour
//! space problem (C-01) cannot mask the result (white decodes to ~235, black
//! to ~16, blue stays mean level 85).

mod common;

use common::EgfxPair;
use ironrdp_egfx::encode::{EncodeFrame, H264Encoder as _, OpenH264Encoder};
use ironrdp_egfx::pdu::{Avc420Region, annex_b_to_avc};
use ironrdp_graphics::clearcodec::ClearCodecEncoder;
use ironrdp_pdu::geometry::ExclusiveRectangle;

const W: u16 = 64;
const H: u16 = 64;
const QP: u8 = 22;

fn paint_blue(pair: &mut EgfxPair, surface: u16) {
    let bgra: Vec<u8> = (0..usize::from(W) * usize::from(H))
        .flat_map(|_| [255, 0, 0, 255])
        .collect();
    let cc = ClearCodecEncoder::new().encode(&bgra, W, H);
    let rect = ExclusiveRectangle {
        left: 0,
        top: 0,
        right: W,
        bottom: H,
    };
    pair.server
        .send_clearcodec_frame(surface, rect, cc, 0)
        .expect("clearcodec frame");
    pair.pump();
    assert_eq!(pair.pixel(32, 32), [0, 0, 255], "setup: surface must be blue");
}

/// Surface-sized RGBA frame: white inside `white`, black elsewhere.
fn h264_frame(white: impl Fn(usize, usize) -> bool) -> Vec<u8> {
    let mut rgba = vec![0u8; usize::from(W) * usize::from(H) * 4];
    for y in 0..usize::from(H) {
        for x in 0..usize::from(W) {
            let v = if white(x, y) { 255 } else { 0 };
            rgba[(y * usize::from(W) + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let mut enc = OpenH264Encoder::new().unwrap();
    enc.request_key_frame();
    let annex_b = enc
        .encode(EncodeFrame {
            data: &rgba,
            width: u32::from(W),
            height: u32::from(H),
        })
        .unwrap();
    annex_b_to_avc(&annex_b)
}

/// Control: one full-surface region, destRect at the origin.  This works
/// today and shows the fixture itself is sound.
#[test]
fn control_full_surface_region_is_painted() {
    let mut pair = EgfxPair::new(W, H);
    let surface = pair.server.surface_ids().next().unwrap();
    paint_blue(&mut pair, surface);

    let data = h264_frame(|_, _| true);
    let regions = [Avc420Region::full_frame(W, H, QP)];
    pair.server
        .send_avc420_frame(surface, &data, &regions, 1)
        .expect("avc420 frame");
    pair.pump();

    let level = pair.mean_level(0, 0, 64, 64);
    eprintln!("control: mean level over the surface = {level:.1} (white ~235)");
    assert!(
        level > 180.0,
        "full-surface AVC420 update must paint white, got {level:.1}"
    );
}

/// A region away from the origin: the white bottom-right quadrant of the
/// frame must land on the bottom-right quadrant of the surface.
#[test]
fn region_away_from_origin_takes_pixels_from_the_same_frame_position() {
    let mut pair = EgfxPair::new(W, H);
    let surface = pair.server.surface_ids().next().unwrap();
    paint_blue(&mut pair, surface);

    let data = h264_frame(|x, y| x >= 32 && y >= 32);
    let regions = [Avc420Region::new(32, 32, 64, 64, QP, 100)];
    pair.server
        .send_avc420_frame(surface, &data, &regions, 1)
        .expect("avc420 frame");
    pair.pump();

    let inside = pair.mean_level(36, 36, 60, 60);
    let outside = pair.pixel(8, 8);
    eprintln!("offset region: mean level inside region = {inside:.1} (spec: white ~235), pixel (8,8) = {outside:?}");
    assert_eq!(outside, [0, 0, 255], "pixels outside the region must stay blue");
    assert!(
        inside > 180.0,
        "region (32,32)-(64,64) must show the frame's white quadrant; mean level {inside:.1} means \
         the client copied the frame's top-left (black) corner instead"
    );
}

/// Two regions whose bounding box is the whole surface: pixels between the
/// regions are inside `destRect` but outside the region mask and must keep
/// the ClearCodec content.
#[test]
fn pixels_outside_the_region_mask_are_not_overwritten() {
    let mut pair = EgfxPair::new(W, H);
    let surface = pair.server.surface_ids().next().unwrap();
    paint_blue(&mut pair, surface);

    let data = h264_frame(|_, _| true);
    let regions = [
        Avc420Region::new(0, 0, 16, 16, QP, 100),
        Avc420Region::new(48, 48, 64, 64, QP, 100),
    ];
    pair.server
        .send_avc420_frame(surface, &data, &regions, 1)
        .expect("avc420 frame");
    pair.pump();

    let in_region = pair.mean_level(2, 2, 14, 14);
    let between = pair.pixel(32, 32);
    eprintln!("mask: region level = {in_region:.1}, pixel (32,32) between regions = {between:?} (spec: [0, 0, 255])");
    assert!(in_region > 180.0, "the regions themselves must be painted white");
    assert_eq!(
        between,
        [0, 0, 255],
        "pixel (32,32) lies outside both regions and must keep the ClearCodec blue"
    );
}
