**Title:** egfx: send_avc420_frame panics when an Avc420Region QP does not fit in 6 bits

---

`Avc420Region::quantization_parameter` is a public `u8`. `QuantQuality::encode` packs it with `set_bits(0..6, qp)` ([`avc.rs#L45`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/pdu/avc.rs#L45)). `bit_field` asserts "value does not fit into bit range" for values ≥ 64, and `encode_avc420_bitmap_stream` wraps the encode in `.expect(...)` ([`#L587-L589`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/pdu/avc.rs#L587-L589)).

As a result, an out-of-range QP passed by the embedder panics the server:

- confirmed with a test: `encode_avc420_bitmap_stream` and `send_avc420_frame` on a negotiated server
- from reading the code, the same packing path: the AVC444 senders and `send_mixed_frame`

QP values 52..=63 do not panic, but they are sent unchanged. MS-RDPEGFX 2.2.4.4.2 says qp "MUST be in the range required by [ITU-H.264-201201] sections 7.4.2.1.1 and 7.4.3 for high profiles".

### Reproduction

```rust
use ironrdp_egfx::pdu::{Avc420Region, encode_avc420_bitmap_stream};

let _ = encode_avc420_bitmap_stream(&[Avc420Region::new(0, 0, 16, 16, 64, 100)], &[0, 0, 0, 1, 0x65]);
// panicked at crates/ironrdp-egfx/src/pdu/avc.rs:45:14:
// value does not fit into bit range
```

### Suggested fix

Validate the QP (0..=51) in `Avc420Region::new` / `full_frame`, or reject the frame in the `send_*` methods (they already return `Option`). Clamping would also avoid the panic, but it silently changes what the embedder asked for.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
