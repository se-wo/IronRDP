**Title:** progressive: DWT variant is read from the CONTEXT flags instead of the REGION flags

---

`ProgressiveDecoder::decode_bitmap` chooses between the standard and the reduce-extrapolate DWT from bit 0 of the `RFX_PROGRESSIVE_CONTEXT` flags ([`progressive.rs#L1358-L1373`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-graphics/src/progressive.rs#L1358-L1373), via [`ProgressiveContextPdu::uses_reduce_extrapolate`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-pdu/src/codecs/rfx/progressive.rs#L397-L413)).

MS-RDPEGFX gives bit 0 a different meaning in each block:

- 2.2.4.2.1.4 `RFX_PROGRESSIVE_CONTEXT`: `RFX_SUBBAND_DIFFING` (0x01), "Indicates that sub-band diffing is enabled."
- 2.2.4.2.1.5 `RFX_PROGRESSIVE_REGION`: `RFX_DWT_REDUCE_EXTRAPOLATE` (0x01), "Indicates that the discrete wavelet transform (DWT) uses the "Reduce-Extrapolate" method."

`ProgressiveRegion::uses_reduce_extrapolate` exists ([`#L792-L795`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-pdu/src/codecs/rfx/progressive.rs#L792-L795)) but does not affect decoding. FreeRDP reads each flag from its own block ([`progressive.c#L958-L959`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/progressive.c#L958-L959)):

```c
sub = context->flags & RFX_SUBBAND_DIFFING;
extrapolate = region->flags & RFX_DWT_REDUCE_EXTRAPOLATE;
```

### Reproduction

Take the 16 non-difference tiles of `wts2_progressive_tile_first_mixed_25tiles.bin` (Windows capture, REGION flags = 0x01), put SYNC + CONTEXT in front, and decode twice, once with CONTEXT flags 0x01 and once with 0x00. Non-difference tiles must decode identically. They don't: the two outputs differ by up to 253 per channel.
FreeRDP 3.32.2 `progressive_decompress` returns the same image for both streams. Compared with that image, IronRDP's output is within 4 levels for CONTEXT flags 0x01 and off by 69 levels on average for 0x00.

Test for `ironrdp-testsuite-core/tests/egfx/wire_to_surface_real_world.rs` (fails on current master):

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

### When it matters

Decoding only goes wrong when a server sets the two bits differently.

- FreeRDP's encoder writes 0 in both blocks ([`rfx.c#L2300`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx.c#L2300), [`#L2338`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/rfx.c#L2338)), so it decodes correctly by coincidence.
- The Windows capture does not include the CONTEXT block, so I don't know which value Windows sends. If Windows enables sub-band diffing together with reduce-extrapolate, its streams also decode correctly by coincidence.
- I have not checked xrdp or GNOME Remote Desktop.

### Suggested fix

Take the DWT variant from each REGION (`region.uses_reduce_extrapolate()`) and treat the CONTEXT bit as sub-band diffing. The stored CONTEXT value and its fallbacks would then no longer be needed for the DWT choice.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
