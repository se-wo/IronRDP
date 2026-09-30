**Title:** egfx client: H.264 (AVC420) partial updates are drawn with the wrong pixels

---

### What goes wrong

When an H.264 update only covers part of the screen, the IronRDP client draws the wrong part of the video frame. It also paints over areas that the update was not supposed to touch.

- An update for the bottom-right corner of the screen shows the content of the frame's **top-left** corner instead.
- An update made of several separate rectangles also overwrites everything **between** them. In a mixed frame, that destroys text tiles which were just drawn with ClearCodec.

Full-screen updates are drawn correctly.

### Why

With AVC420, the server always sends a video frame the size of the whole surface. Next to it, it sends a list of rectangles (`regionRects`) saying which parts of that frame are new. The client must copy exactly those rectangles, from the same position in the frame to the same position on the surface.

IronRDP's client does something else:

- It ignores the rectangle list.
- It takes one bounding box (`destRect`) and always copies from the frame's origin (0, 0) into that box.

So a box at (32, 32) receives the frame's pixels from (0, 0). A bounding box around two small rectangles overwrites everything in between.

### How to reproduce

I connected IronRDP's own server (`GraphicsPipelineServer`) to IronRDP's own client (`GraphicsPipelineClient` with OpenH264), exchanging the real channel bytes. Setup: a 64×64 surface, first painted blue with ClearCodec. Then one H.264 update, with frames containing only black and white.

| Case | Rectangles sent | Expected | What the client shows |
|---|---|---|---|
| Whole surface (control) | (0,0)-(64,64) | white | white ✔ |
| Bottom-right quadrant, frame white only there | (32,32)-(64,64) | white quadrant | black quadrant: the frame's top-left corner ✘ |
| Two small corners | (0,0)-(16,16) and (48,48)-(64,64) | middle stays blue | middle turns white ✘ |

Tests: https://github.com/se-wo/IronRDP/blob/analysis/chroma/analysis/chroma/harness/tests/c06_avc420_region_rects.rs (shared setup in [`tests/common/mod.rs`](https://github.com/se-wo/IronRDP/blob/analysis/chroma/analysis/chroma/harness/tests/common/mod.rs)).

### What I don't know

- **Windows servers:** I have no H.264 capture from a Windows server, so I can't say how often they send partial updates away from the origin. IronRDP's own server does: it sets `destRect` to the bounding box of the rectangles ([`server.rs#L1405-L1433`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/server.rs#L1405-L1433)).
- **Other conventions:** I found no documented IronRDP convention that an H.264 frame starts at `destRect`. `EncodeFrame` only mentions padding to 16 pixels and letting the destination rectangle crop ([`encode.rs#L19-L21`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/encode.rs#L19-L21)).

### References

- MS-RDPEGFX 2.2.2.1: for AVC420, AVC444 and AVC444v2, `destRect` "specifies a bounding rectangle".
- MS-RDPEGFX 2.2.4.4: the bitstream "MUST be cropped by the region mask specified in the regionRects field".
- MS-RDPEGFX 3.3.8.3.2: "Color conversion MUST be performed for the entire macroblock, after which the region mask in regionRects MUST be applied."
- IronRDP: [`decode_avc420`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/client.rs#L961-L1006) decodes `regionRects` but only uses the bitstream. [`crop_decoded_frame`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/client.rs#L1265-L1300) always starts at (0, 0).
- FreeRDP decodes into surface coordinates and passes the region rects to the decoder ([`gdi/gfx.c#L750-L752`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/gdi/gfx.c#L750-L752)).

### Suggested fix

Treat the decoded frame as aligned with the surface. For each rectangle in `regionRects`, clip it to surface and frame, then copy that rectangle from frame position (x, y) to surface position (x, y). Emit one update per rectangle.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
