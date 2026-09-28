**Title:** egfx server: sending an H.264 frame with a quantization parameter ≥ 64 panics

---

### What goes wrong

A server application that passes an H.264 quantization parameter (QP) of 64 or more to `send_avc420_frame` crashes with a panic. It does not get an error. A mistyped or unclamped value in the embedding application is enough.

Values from 52 to 63 do not panic but are also invalid: H.264 only allows QP 0 to 51. IronRDP sends them to the client unchanged.

### Why

The QP is a public `u8` in `Avc420Region`. On the wire it has only 6 bits. IronRDP writes it with `bit_field`'s `set_bits(0..6, qp)`, which asserts that the value fits, and the caller wraps the result in `.expect(...)`. Nothing checks the range before.

### How to reproduce

```rust
use ironrdp_egfx::pdu::{Avc420Region, encode_avc420_bitmap_stream};

let _ = encode_avc420_bitmap_stream(&[Avc420Region::new(0, 0, 16, 16, 64, 100)], &[0, 0, 0, 1, 0x65]);
// panicked at crates/ironrdp-egfx/src/pdu/avc.rs:45:14:
// value does not fit into bit range
```

I also confirmed it through `GraphicsPipelineServer::send_avc420_frame` on a negotiated server. From reading the code, the AVC444 senders and `send_mixed_frame` go through the same path.

### References

- MS-RDPEGFX 2.2.4.4.2: qp "MUST be in the range required by [ITU-H.264-201201] sections 7.4.2.1.1 and 7.4.3 for high profiles".
- [`avc.rs#L45`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/pdu/avc.rs#L45) (`set_bits(0..6, …)`), [`avc.rs#L587-L589`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/pdu/avc.rs#L587-L589) (`.expect(...)`).

### Suggested fix

Check the QP (0..=51) when the region is built, or reject the frame in the `send_*` methods, which already return `Option`. Clamping would also avoid the panic, but it silently changes what the application asked for.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
