//! C-08: RemoteFX quantisation must round, not truncate.
//!
//! MS-RDPRFX 3.1.8.1.5: "The encoder determines a scale value for each
//! sub-band and uses it to quantize all the coefficients in that sub-band,
//! which is done by dividing each coefficient by the scale value and
//! rounding it." `ironrdp_graphics::quantization::encode` shifts right
//! (`quantization.rs:40-46`), i.e. floors.
//!
//! The first 1024 coefficients are the HL1 band (see `quantization.rs`); with
//! `hl1 = 7` the shift is 6 (scale 64 in IronRDP's 11.5 fixed-point domain).

use ironrdp_graphics::quantization;
use ironrdp_pdu::codecs::rfx::Quant;

fn quant_hl1(v: u8) -> Quant {
    Quant {
        hl1: v,
        ..Quant::default()
    }
}

#[test]
fn positive_coefficient_rounds_to_nearest() {
    let mut buf = [0i16; 4096];
    buf[0] = 63; // 63 / 64 = 0.98 -> 1
    quantization::encode(&mut buf, &quant_hl1(7));
    assert_eq!(buf[0], 1, "63 / 64 must round to 1, got {}", buf[0]);
}

#[test]
fn small_negative_coefficient_rounds_to_zero() {
    let mut buf = [0i16; 4096];
    buf[0] = -1; // -1 / 64 = -0.016 -> 0
    quantization::encode(&mut buf, &quant_hl1(7));
    assert_eq!(buf[0], 0, "-1 / 64 must round to 0, got {}", buf[0]);
}
