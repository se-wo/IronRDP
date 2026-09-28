**Title:** RemoteFX encoder rounds coefficients down instead of to the nearest value (colours slightly too dark, larger output)

---

### What goes wrong

Images sent by IronRDP's RemoteFX encoder come out slightly darker and less accurate than they should, and they need more bandwidth.

- **Flat colours:** White arrives as `#FEFEFE`, mid grey `#808080` as `#7F7F7F`.
- **Coloured text:** Edges are noticeably less accurate. For red text on black the worst pixel error doubles (33 → 63).
- **Size:** The encoded images are 7 to 27 % larger than necessary in my test images.

### Why

RemoteFX compresses by dividing wavelet coefficients by a scale value. The spec says to divide **and round**. IronRDP divides with a plain right shift, which always rounds down: 63/64 becomes 0 instead of 1, and −1/64 becomes −1 instead of 0. The small downward error in every coefficient adds up to the colour bias. It also produces more non-zero values, so the output compresses worse.

### How to reproduce

The first 1024 coefficients are the HL1 band. With `hl1 = 7` the divisor is 64:

```rust
use ironrdp_graphics::quantization;
use ironrdp_pdu::codecs::rfx::Quant;

let quant = Quant { hl1: 7, ..Quant::default() };

let mut buf = [0i16; 4096];
buf[0] = 63; // 63 / 64 = 0.98
quantization::encode(&mut buf, &quant);
assert_eq!(buf[0], 1); // fails: 0

let mut buf = [0i16; 4096];
buf[0] = -1; // -1 / 64 = -0.016
quantization::encode(&mut buf, &quant);
assert_eq!(buf[0], 0); // fails: -1
```

### Measured effect

Test setup:
- a harness that calls the same public functions as `ironrdp-server`'s `RfxEncoder` (`to_64x64_ycbcr_tile`, `rfx_encode_component`) and as `ironrdp-session`'s RemoteFX decoder
- `Quant::default()`
- synthetic 320×128 images
- current code compared with the same pipeline using rounding

| | current (rounds down) | with rounding |
|---|---|---|
| flat `#FFFFFF` / `#808080` / `#C0C0C0` | `#FEFEFE` / `#7F7F7F` / `#BFBFBF` | exact |
| red text on black: colour accuracy (PSNR V) / worst pixel error | 39.0 dB / 63 | 44.6 dB / 33 |
| encoded size: red / blue / dark-blue text | 21 491 / 18 403 / 15 066 bytes | −7 % / −15 % / −27 % |

Harness: https://github.com/se-wo/IronRDP/blob/analysis/chroma/analysis/chroma/harness/src/main.rs (`rfx_roundtrip_rounded`).

### References

- MS-RDPRFX 3.1.8.1.5: "The encoder determines a scale value for each sub-band and uses it to quantize all the coefficients in that sub-band, which is done by dividing each coefficient by the scale value and rounding it."
- IronRDP: [`quantization.rs#L40-L46`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-graphics/src/quantization.rs#L40-L46) (`*value >>= factor`). It is used by `rfx_encode_component` and therefore by `ironrdp-server`.
- FreeRDP adds half the divisor before shifting ([`rfx_quantization.c#L92-L96`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx_quantization.c#L92-L96)).

### Suggested fix

For `factor > 0`: `*value = ((i32::from(*value) + (1 << (factor - 1))) >> factor) as i16`. Widen to `i32` so values near `i16::MAX` cannot overflow when half is added.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
