**Title:** Progressive: tiles can decode garbled because the wavelet variant is read from the wrong flag

---

### What goes wrong

IronRDP's RemoteFX Progressive decoder can undo the wavelet transform with the wrong method. The decoded tiles are then garbled: colours and shapes are wrong, not just slightly blurry.

This only happens when a server sets two particular flags to different values. With the servers I could check, both flags have the same value, so the bug stays hidden. It is still wrong according to the spec, and it takes only a small difference in server behaviour to trigger it.

*(Screenshot: left IronRDP, middle FreeRDP, right the difference ×8, same input stream.)*

### Why

RemoteFX Progressive has two variants of the wavelet transform, "standard" and "reduce-extrapolate". The stream tells the decoder which one it used:

- **Where the spec puts it:** in every REGION block, bit 0 of `flags` (`RFX_DWT_REDUCE_EXTRAPOLATE`).
- **Where IronRDP looks:** in the CONTEXT block, bit 0 of `flags`. The spec defines that bit as something unrelated: "sub-band diffing is enabled" (`RFX_SUBBAND_DIFFING`).

So IronRDP decodes correctly only while a server happens to set both bits to the same value.

### How to reproduce

Take the Windows capture that is already in the test data (`wts2_progressive_tile_first_mixed_25tiles.bin`; its REGION says "reduce-extrapolate"). Decode its 16 normal tiles twice. The only difference between the two runs is the CONTEXT bit, which has nothing to do with the wavelet variant.

| | CONTEXT bit = 1 | CONTEXT bit = 0 |
|---|---|---|
| FreeRDP 3.32.2 | correct image | same correct image |
| IronRDP | matches FreeRDP within 4 levels | differs by 69 levels on average, up to 253 |

Test for `ironrdp-testsuite-core/tests/egfx/wire_to_surface_real_world.rs`. It fails on current master:

```rust
#[test]
fn progressive_dwt_variant_comes_from_region_flags() {
    use ironrdp_graphics::progressive::ProgressiveDecoder;
    use ironrdp_pdu::codecs::rfx::progressive::{
        ProgressiveBlock, ProgressiveContextPdu, ProgressiveFrameBeginPdu, ProgressiveFrameEndPdu,
        ProgressiveRegion, ProgressiveSyncPdu, ProgressiveTile, TILE_FLAG_DIFFERENCE,
        decode_progressive_stream, encode_progressive_stream,
    };

    let bytes = include_bytes!("../../test_data/egfx/haven/wts2_progressive_tile_first_mixed_25tiles.bin");
    let GfxPdu::WireToSurface2(pdu) = decode(bytes) else {
        panic!("expected WireToSurface2");
    };
    let region = decode_progressive_stream(&pdu.bitmap_data)
        .unwrap()
        .into_iter()
        .find_map(|b| match b {
            ProgressiveBlock::Region(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert!(region.uses_reduce_extrapolate()); // REGION flags = 0x01
    // Difference tiles need a reference that is not part of the capture.
    let base: Vec<_> = region
        .tiles
        .iter()
        .filter(|t| matches!(t, ProgressiveTile::First(f) if f.flags & TILE_FLAG_DIFFERENCE == 0))
        .cloned()
        .collect();

    let decode_with = |context_flags: u8| {
        let stream = encode_progressive_stream(&[
            ProgressiveBlock::Sync(ProgressiveSyncPdu),
            ProgressiveBlock::Context(ProgressiveContextPdu { context_id: 0, tile_size: 0x40, flags: context_flags }),
            ProgressiveBlock::FrameBegin(ProgressiveFrameBeginPdu { frame_index: 0, region_count: 1 }),
            ProgressiveBlock::Region(ProgressiveRegion { tiles: base.clone(), ..region.clone() }),
            ProgressiveBlock::FrameEnd(ProgressiveFrameEndPdu),
        ])
        .unwrap();
        let mut decoder = ProgressiveDecoder::new();
        decoder.begin_frame();
        let tiles = decoder.decode_bitmap(0, 3, 1280, 800, &stream).unwrap();
        decoder.end_frame();
        tiles.into_iter().map(|t| t.pixels).collect::<Vec<_>>()
    };

    // CONTEXT bit 0 is RFX_SUBBAND_DIFFING and must not change non-difference tiles.
    assert!(decode_with(0x01) == decode_with(0x00));
}
```

### Which servers are affected

- **FreeRDP:** writes 0 into both flags ([`rfx.c#L2300`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx.c#L2300), [`#L2338`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx.c#L2338)). Decodes correctly by coincidence.
- **Windows:** The capture does not contain the CONTEXT block, so I don't know its value. If Windows turns on sub-band diffing together with reduce-extrapolate, it also decodes correctly by coincidence.
- **xrdp, GNOME Remote Desktop, others:** not checked.

### References

- MS-RDPEGFX 2.2.4.2.1.5 `RFX_PROGRESSIVE_REGION`: `RFX_DWT_REDUCE_EXTRAPOLATE` (0x01), "Indicates that the discrete wavelet transform (DWT) uses the "Reduce-Extrapolate" method."
- MS-RDPEGFX 2.2.4.2.1.4 `RFX_PROGRESSIVE_CONTEXT`: `RFX_SUBBAND_DIFFING` (0x01), "Indicates that sub-band diffing is enabled."
- IronRDP picks the variant from the CONTEXT flags: [`progressive.rs#L1358-L1373`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-graphics/src/progressive.rs#L1358-L1373), via [`ProgressiveContextPdu::uses_reduce_extrapolate`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-pdu/src/codecs/rfx/progressive.rs#L397-L413). The REGION flag is parsed ([`ProgressiveRegion::uses_reduce_extrapolate`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-pdu/src/codecs/rfx/progressive.rs#L792-L795)) but never used.
- FreeRDP reads each flag from its own block ([`progressive.c#L958-L959`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/progressive.c#L958-L959)): `sub = context->flags & RFX_SUBBAND_DIFFING; extrapolate = region->flags & RFX_DWT_REDUCE_EXTRAPOLATE;`

### Suggested fix

Take the wavelet variant from each REGION (`region.uses_reduce_extrapolate()`). Treat the CONTEXT bit as what it is, sub-band diffing. The stored CONTEXT value and its fallbacks are then no longer needed for this decision.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
