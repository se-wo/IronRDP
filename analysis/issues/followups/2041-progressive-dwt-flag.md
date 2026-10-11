**Follow-up for Devolutions/IronRDP#2041**

---

Two additions after checking more encoders.

**All servers I know of set both flags to the same value.** So none of them triggers this today:

| Server | CONTEXT flags | REGION flags |
|---|---|---|
| FreeRDP | 0 | 0 |
| xrdp (librfxcodec) | `RFX_SUBBAND_DIFFING` ([`rfxencode_compose.c#L636`](https://github.com/neutrinolabs/librfxcodec/blob/0badc062e96d170af57870480a4aa7cf469302b0/src/rfxencode_compose.c#L636)) | `RFX_DWT_REDUCE_EXTRAPOLATE` ([`#L805`](https://github.com/neutrinolabs/librfxcodec/blob/0badc062e96d170af57870480a4aa7cf469302b0/src/rfxencode_compose.c#L805)) |
| Windows | not in the capture, probably 1 | 1 |

For Windows: FreeRDP's decoder logs a warning when the CONTEXT bit is *not* set ([`progressive.c#L1445-L1446`](https://github.com/FreeRDP/FreeRDP/blob/dca5e65158ea2f716ced5d38b526ed1c94a89b9c/libfreerdp/codec/progressive.c#L1445-L1446)). That suggests Windows normally sets it.

So this is a spec violation that is hidden today, not a bug users currently see. It breaks as soon as a server sets the two bits differently.

**The fix also removes a failure mode.** The decoder looks for the CONTEXT block only to get this flag. That is why it keeps the per-context and per-surface fallbacks, and why it can fail with `MissingBlock("CONTEXT")` ([`progressive.rs#L1342-L1372`](https://github.com/Devolutions/IronRDP/blob/8d91a2cc3a3fa04f2537cad87e7065604789f784/crates/ironrdp-graphics/src/progressive.rs#L1342-L1372)). The code comments there describe the xrdp/GNOME and Windows cases where the CONTEXT block is not repeated.

Every REGION carries its own flag. If the variant comes from there, the decoder no longer needs the CONTEXT block to decode a tile, and that error path together with the fallback state can go.

> [!NOTE]
> Human-reviewed, LLM-assisted content.
