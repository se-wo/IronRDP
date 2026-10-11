//! C-12: an out-of-range quantisation parameter must not panic the server.
//!
//! `Avc420Region::quantization_parameter` is a public `u8`. The wire field
//! `RDPGFX_AVC420_QUANT_QUALITY.qp` has 6 bits and MUST be a valid H.264 QP
//! (MS-RDPEGFX 2.2.4.4.2: range per ITU-H.264 7.4.2.1.1 / 7.4.3, i.e. 0..=51).
//! `QuantQuality::encode` packs it with `bit_field::set_bits(0..6, qp)`,
//! which asserts "value does not fit into bit range" for qp >= 64, and
//! `encode_avc420_bitmap_stream` wraps the encode in `.expect(...)`.
//!
//! Spec-conformant behaviour would be to reject (`None`/`Err`) or clamp; the
//! tests assert "no panic", so a failure confirms the bug. Control: qp 51.

mod common;

use std::panic::{AssertUnwindSafe, catch_unwind};

use common::EgfxPair;
use ironrdp_egfx::pdu::{Avc420Region, encode_avc420_bitmap_stream};

const FAKE_H264: &[u8] = &[0, 0, 0, 1, 0x65];

fn helper_panics(qp: u8) -> bool {
    catch_unwind(|| encode_avc420_bitmap_stream(&[Avc420Region::new(0, 0, 16, 16, qp, 100)], FAKE_H264)).is_err()
}

#[test]
fn control_qp_51_encodes() {
    assert!(!helper_panics(51));
}

#[test]
fn encode_helper_does_not_panic_on_qp_64() {
    assert!(!helper_panics(64), "encode_avc420_bitmap_stream panicked for qp = 64");
}

#[test]
fn send_avc420_frame_does_not_panic_on_qp_64() {
    let mut pair = EgfxPair::new(64, 64);
    let surface = pair.server.surface_ids().next().unwrap();
    let regions = [Avc420Region::full_frame(64, 64, 64)];
    let result = catch_unwind(AssertUnwindSafe(|| {
        pair.server.send_avc420_frame(surface, FAKE_H264, &regions, 0)
    }));
    assert!(
        result.is_ok(),
        "GraphicsPipelineServer::send_avc420_frame panicked for qp = 64 (public API input)"
    );
}
