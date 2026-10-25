**Follow-up for Devolutions/IronRDP#2043**

---

Two notes on the suggested fix.

**The `as i16` cast in my snippet trips the workspace lints** (`clippy::as_conversions`). The result always fits, because a value is shifted right by at least 1 after half the divisor is added. So either form works:

```rust
fn encode_block(buffer: &mut [i16], factor: i16) {
    if factor > 0 {
        let half = 1i32 << (factor - 1);
        for value in buffer {
            let rounded = (i32::from(*value) + half) >> factor;
            // INVARIANT: |rounded| <= (32767 + 2^(factor-1)) >> factor < 2^15 for factor >= 1.
            *value = i16::try_from(rounded).expect("fits in i16 because factor >= 1");
        }
    }
}
```

or keep the `as` cast with an `#[expect(clippy::as_conversions, reason = "...")]`, as `client.rs` does elsewhere.

**Rounding is half-up, not symmetric.** +32/64 becomes 1, but −32/64 becomes 0. This is what FreeRDP does ([`rfx_quantization.c#L92-L96`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx_quantization.c#L92-L96)), and the spec only says "rounding". The measurements in the issue use exactly this rounding. Symmetric rounding (away from zero at .5) is an alternative, but it would be a separate choice and I have not measured it.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
