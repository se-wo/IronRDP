**Follow-up for Devolutions/IronRDP#2042**

---

Two points I left out of the description.

**This is not an IronRDP convention, as far as I can tell.** One could read the `EncodeFrame` doc comment ("let the destination rectangle crop, exactly as the decode side does in reverse", [`encode.rs#L19-L21`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/encode.rs#L19-L21)) as meaning that a server may send an H.264 frame that starts at `destRect`. mstsc and FreeRDP place the frame at the surface origin instead. A server that sends such offset frames is therefore already drawn wrong by those clients.

The fix changes behaviour only for a stream like that. For such a stream it makes IronRDP draw what the other clients draw. If that convention is intended, it would be good to document it, because it only works with IronRDP's own client.

**The size check has to change with the fix.** `decode_avc420` rejects a frame that is smaller than `destRect` ([`client.rs#L979-L991`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-egfx/src/client.rs#L979-L991)). Once the frame is aligned with the surface, the question is whether each rectangle in `regionRects` lies inside the frame and the surface. `destRect`'s width and height no longer matter. Rectangles outside the decoded frame should be clipped (or rejected) rather than read from outside the buffer.

**Still open:** I have no Windows capture with H.264, so I have not shown how often Windows sends partial updates away from the origin. If someone has an AVC420 capture from a Windows server, one frame with a small `regionRects` list away from (0, 0) would settle it.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
