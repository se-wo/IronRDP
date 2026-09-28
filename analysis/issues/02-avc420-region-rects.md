**Title:** egfx client: AVC420 ignores regionRects and copies the frame's top-left corner into destRect

---

`GraphicsPipelineClient::decode_avc420` decodes the H.264 frame, then copies a block the size of `destRect` into `destRect`. The block is always taken from (0, 0) of the decoded frame ([`client.rs#L961-L1006`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/client.rs#L961-L1006), [`crop_decoded_frame`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/client.rs#L1265-L1300)). The `regionRects` of the `RFX_AVC420_METABLOCK` are parsed but never used.

MS-RDPEGFX:

- 2.2.2.1: for AVC420, AVC444 and AVC444v2, `destRect` "specifies a bounding rectangle".
- 2.2.4.4: the bitstream "MUST be cropped by the region mask specified in the regionRects field".
- 3.3.8.3.2: "Color conversion MUST be performed for the entire macroblock, after which the region mask in regionRects MUST be applied."

FreeRDP decodes AVC420 into surface coordinates and copies only the region rects ([`gdi/gfx.c#L750-L752`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/gdi/gfx.c#L750-L752)).

### Reproduction

I connected IronRDP's own `GraphicsPipelineServer` to `GraphicsPipelineClient` with the OpenH264 decoder, using the real DVC bytes in both directions. The test surface was 64×64: first painted blue with ClearCodec, then updated with `send_avc420_frame`. The H.264 frames contain only black and white.

| Case | regions → destRect | Expected | Observed |
|---|---|---|---|
| Control | (0,0)-(64,64) → same | white | white (mean level 235.9) |
| Region away from the origin, frame white only in that quadrant | (32,32)-(64,64) → same | quadrant white | quadrant black (mean level 16.0), i.e. the frame's top-left corner |
| Two regions, bounding box = whole surface | (0,0)-(16,16) and (48,48)-(64,64) → (0,0)-(64,64) | pixel (32,32) stays blue `[0,0,255]` | `[236,236,236]` |

The third case is how a mixed frame loses the ClearCodec tiles that lie inside the AVC bounding box.

Tests: https://github.com/se-wo/IronRDP/blob/analysis/chroma/analysis/chroma/harness/tests/c06_avc420_region_rects.rs (fixture in [`tests/common/mod.rs`](https://github.com/se-wo/IronRDP/blob/analysis/chroma/analysis/chroma/harness/tests/common/mod.rs)).

### What I don't know

I have no AVC capture from a Windows server, so I can't say how often Windows sends a `destRect` that does not start at the origin. IronRDP's own server produces such rects: `compute_dest_rect` returns the union of the regions ([`server.rs#L1405-L1433`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/server.rs#L1405-L1433)).

I found no documented convention that an IronRDP H.264 frame starts at `destRect`. `EncodeFrame` only talks about padding to 16 pixels and letting the destination rectangle crop ([`encode.rs#L19-L21`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/encode.rs#L19-L21)).

### Suggested fix

Treat the decoded frame as surface-aligned. For each region rect, clip it to the surface and the frame, then copy that rect from frame (x, y) to surface (x, y), with one update per region.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
