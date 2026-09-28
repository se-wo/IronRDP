**Title:** graphics: RemoteFX quantization truncates instead of rounding

---

`ironrdp_graphics::quantization::encode` quantizes with `*value >>= factor` ([`quantization.rs#L40-L46`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-graphics/src/quantization.rs#L40-L46)), which rounds toward negative infinity. `ironrdp-server`'s RemoteFX encoder uses it through `rfx_encode_component`.

MS-RDPRFX 3.1.8.1.5: "The encoder determines a scale value for each sub-band and uses it to quantize all the coefficients in that sub-band, which is done by dividing each coefficient by the scale value and rounding it." FreeRDP adds half the scale before shifting ([`rfx_quantization.c#L92-L96`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx_quantization.c#L92-L96)).

### Reproduction

The first 1024 coefficients are the HL1 band. With `hl1 = 7` the shift is 6:

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

### Effect

I measured this with a harness that calls the same public functions as `RfxEncoder` (`to_64x64_ycbcr_tile` + `rfx_encode_component`) and as `ironrdp-session`'s decoder. It used `Quant::default()` and synthetic 320×128 images, comparing the current code against the same pipeline with rounding:

| | truncation (current) | rounding |
|---|---|---|
| flat `#FFFFFF` / `#808080` / `#C0C0C0` | `#FEFEFE` / `#7F7F7F` / `#BFBFBF` | exact |
| red text on black: PSNR V / max error | 39.0 dB / 63 | 44.6 dB / 33 |
| encoded size: red / blue / navy text | 21 491 / 18 403 / 15 066 B | −7 % / −15 % / −27 % |

Harness: https://github.com/se-wo/IronRDP/blob/analysis/chroma/analysis/chroma/harness/src/main.rs (`rfx_roundtrip_rounded`).

### Suggested fix

For `factor > 0`: `*value = ((i32::from(*value) + (1 << (factor - 1))) >> factor) as i16`. Widen to `i32` so values near `i16::MAX` don't overflow when half is added.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
