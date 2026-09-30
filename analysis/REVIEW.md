# IronRDP-Fork: Stärken, Schwächen und Chroma-Behandlung

Analyse, Phase 1 (nur lesen und messen, kein Produktcode geändert). Stand: 28.09.2026.

- **Fork-Revision:** `8d91a2cc` (`se-wo/IronRDP`, `master` = `claude/confident-mccarthy-g1bvzm`).
  Abstand zu `upstream/master` (`Devolutions/IronRDP`): **0 Commits in beide Richtungen**. Der Fork hat keine eigenen Commits (Fakt: `git log HEAD..upstream/master` und `upstream/master..HEAD` sind leer).
- **Spec-Revisionen:** MS-RDPEGFX **19.0 (11.05.2026)**, MS-RDPRFX, MS-RDPNSC 14.0, MS-RDPEGDI, MS-RDPBCGR aus dem `windows-protocols`-Korpus (awakecoding/openspecs, Stand 28.09.2026).
- **Zweitreferenz:** FreeRDP `master` @ `dca5e651` (28.09.2026), gebaut als libfreerdp3 3.32.2.
- **Messwerkzeuge:** [`analysis/chroma/`](chroma/README.md) (Branch `analysis/chroma`). Rohtabellen: [`chroma/results/tables.md`](chroma/results/tables.md).
- **Kennzeichnung:** **[F]** Fakt (Code, Spec oder Messung belegt), **[H]** Hypothese, **[U]** Upstream-Aussage (aus PR- oder Issue-Text übernommen, nicht selbst verifiziert).

---

## 1. Zusammenfassung

**Kernaussage:** Für farbigen Text mit geringem Luma-Kontrast ist IronRDP heute nicht einsatzbereit.
- Der einzige mitgelieferte H.264-Pfad (AVC420 über `OpenH264Encoder`) verfälscht die Farben schon ohne Kompression: SSIM 0,12 bei rotem Text, Weiß wird zu `#ECECEC`.
- AVC444 ist nur als PDU-Hülle vorhanden. Es gibt weder einen Split-Helfer im Server noch einen Decoder im Client. Die Trait-Schnittstellen (RGBA rein bzw. raus) schließen AVC444 sogar aus.
- Der einzige wirklich chroma-treue Pfad ist ClearCodec. IronRDPs Encoder arbeitet bit-exakt und ist damit besser als Windows, das in ClearCodec NSCodec mit Color-Loss-Level 3 einsetzt.

### Die fünf wichtigsten Stärken

1. **Verlustfreie Codecs exakt und interoperabel [F].** ClearCodec (inkl. Glyph-Cache) und Planar sind in allen 30 Round-Trips bit-exakt. FreeRDP dekodiert IronRDPs ClearCodec-, Planar- und NSCodec-Streams identisch zum IronRDP-Decoder (T7). Der ClearCodec-Encoder nutzt keinen verlustbehafteten Subcodec.
2. **Robustheit gegen feindliche Eingaben [F].** Core- und Codec-Crates enthalten kein `unsafe` (A-02). Allokationen aus Längenfeldern sind begrenzt (ZGFX 64 MiB je Segment und 256 MiB gesamt, ClearCodec ≤ 8192 px je Achse, AVC-Regionen gegen die Restlänge gedeckelt). 27 Fuzz-Targets, darunter zustandsbehaftetes EGFX-Client-Fuzzing über mehrere Frames.
3. **Sans-IO-Kern für EGFX [F].** `GraphicsPipelineServer` und `GraphicsPipelineClient` sind synchrone `DvcProcessor` ohne tokio. Die Codec-Backends sind über Traits steckbar.
4. **Breite der EGFX-Server-Funktionen [F].** Volle Capability-Leiter V8 bis V10.7, Frame-Tracker mit Backpressure, QoE-Aggregation, ZGFX, Mixed-Codec-Frames sowie Sendepfade für AVC420, AVC444, AVC444v2, ClearCodec, Planar, Progressive und Uncompressed.
5. **Test- und Spec-Disziplin [F].**
   - 3246 Tests grün; der einzige rote Test hängt an der Umgebung (fehlendes IPv6).
   - Echte Captures von Windows Server 2025 und Windows 11 als Fixtures.
   - Spec-Zitate im Code.
   - Rechteck-Semantik aller `RDPGFX_RECT16` durchgängig exklusiv (#1238, #1246, #1788).

### Die zehn wichtigsten Schwächen (nach Schweregrad)

| # | ID | Schwere | Befund |
|---|---|---|---|
| 1 | C-01 | kritisch | AVC420-Encoder konvertiert mit BT.601 limited, der Decoder mit BT.709 full. SSIM 0,12 (rot), Weiß → `#ECECEC`, Grün ΔE00 11,8. Upstream-Fix offen (#1976). |
| 2 | C-02 | hoch | Kein YUV444→Main/Aux-Split im Server. `H264Encoder` nimmt nur RGBA, damit ist die Aux-View gar nicht codierbar. |
| 3 | C-03 | hoch | Der Client kann AVC444 nicht dekodieren. `H264Decoder` liefert RGBA statt YUV-Planes, eine Kombination nach Spec ist damit unmöglich. |
| 4 | C-06 | hoch | Der Client-AVC420-Pfad ignoriert `regionRects` und schneidet immer ab (0,0) aus. Bei `destRect ≠ (0,0)` landen falsche Pixel auf der Surface; ClearCodec-Kacheln im Bounding-Rect werden überschrieben. |
| 5 | C-07 | hoch | Progressive rechnet die DWT im 8-Bit-Ganzzahlbereich. Hin und zurück ohne Quantisierung entstehen bis zu 15 Stufen Fehler. Progressive kann nie verlustfrei konvergieren. |
| 6 | C-04 | hoch | Designgrenze AVC444: Der Rückfilter verstärkt den Codierfehler der Main-View-Chroma um Faktor 4,1 bis 5,0 (Median, gemessen). Der Schwellwert 30 kostet schon ohne Kompression 13 dB PSNR V. Ein QP-abhängiger Rückfilter im Client holt ohne Mehrbytes Ø 1,8 dB zurück (T9). |
| 7 | C-08 | mittel | Der RFX-Encoder quantisiert per Floor-Shift statt gerundet (Verstoß gegen MS-RDPRFX 3.1.8.1.5). Mit Rundung: +5,6 dB PSNR V und −7 % bis −27 % Bytes. |
| 8 | C-09 | mittel | NSCodec verwendet die maximale CLL des Clients (typisch 3) statt CLL 1. Vermeidbarer Chroma-Verlust: max. Fehler 5 bis 6 statt 1. |
| 9 | C-10 | mittel | Keine Klassifikation oder Routing-Schnittstelle. AVC444 fehlt in Mixed Frames (offener PR #2002). `AVC_THINCLIENT` („bevorzugt AVC444“) wird ignoriert. AVC444-Sendeversuche scheitern still. |
| 10 | A-01 | mittel | Core-Tier-Invarianten verletzt: kein `no_std` in pdu, graphics, egfx, session, connector; `Instant::now()` im EGFX-Server; `arithmetic_side_effects` in graphics global erlaubt. |

Zusätzlich gefunden bei der Plausibilitätsprüfung: **C-16** (mittel, latent). Der Progressive-Decoder liest Reduce-Extrapolate aus dem CONTEXT- statt aus dem REGION-Flag.

### Empfehlung in einem Satz

Zuerst den AVC-Farbraum reparieren. Dann AVC444 mit einer planaren Encoder-Schnittstelle und einem Split-Helfer bauen. Parallel farbigen Text per Routing-Hook auf den bit-exakten ClearCodec legen. Details in Abschnitt 7.

---

## 2. Orientierung und Pipeline-Karte

### 2.1 Projektdokumentation und Korrekturen am Handover

- `AGENTS.md` und `CLAUDE.md` sind byte-identisch [F].
- **Korrektur:** `ironrdp-egfx` steht bereits in `ARCHITECTURE.md`, und zwar im **Core Tier** (`ARCHITECTURE.md:105`) [F]. `ironrdp-bulk` und `ironrdp-nscodec` fehlen dort tatsächlich [F].
- Relevante Tier-Invarianten (`ARCHITECTURE.md`, Core Tier):
  - kein I/O
  - muss gefuzzt sein
  - `#[no_std]`-kompatibel mit `std`-Feature
  - kein plattformabhängiger Code
  - keine nicht-essentiellen Abhängigkeiten
  - keine Proc-Macros
  - wenig Monomorphisierung

  `ironrdp-server` gehört zum **Community Tier**.
- **Korrektur:** Die `gh`-CLI steht in dieser Umgebung nicht zur Verfügung. Den Upstream-Stand habe ich per GitHub-Suche (nur lesend) und aus der Git-Historie von `Devolutions/IronRDP` erhoben. Einzelne Issue-Seiten waren nicht abrufbar; Titel und Beschreibung kamen über die Suche.
- **Korrektur:** Die Rechteck-Semantik ist in allen Verwendungen von `RDPGFX_RECT16` bereits exklusiv:
  - #1238 (`WireToSurface1`)
  - #1246 (SolidFill, SurfaceToSurface, SurfaceToCache)
  - #1788 (AVC-Regionen)

  `CacheToSurface` enthält nur Zielpunkte, kein Rechteck. Die Handover-Aussage „blieben inklusiv“ ist überholt.
- **Korrektur:** Einen `ChromaFilter` gibt es in FreeRDP unter diesem Namen nicht mehr.
  - Im Januar 2025 wurde „broken chroma filter“ aus dem Combine-Schritt entfernt (`901cf28d1`).
  - Der Filter sitzt seitdem in `YUV444ToRGB` als `CONDITIONAL_CLIP` (`7e4d13374`, `prim_internal.h:215`, `prim_YUV.c:350-364`).
- **Bestätigt:**
  - Es gibt keinen orchestrierenden Progressive-Encoder, nur Bausteine: `encode_first_pass`, `encode_upgrade_pass`, `rgba_to_ycbcr`, `encode_progressive_stream`.
  - Issue #1316 ist geschlossen, trotzdem werden die Encoder nicht gefuzzt (A-03).

### 2.2 Baseline

| Lauf | Ergebnis | Laufzeit |
|---|---|---|
| `cargo xtask check tests --no-run -v` | grün (nach `apt install libasound2-dev`, das die CI ebenfalls installiert) | 448 s kalt |
| `cargo test --workspace --locked --no-fail-fast` | **3246 bestanden, 1 fehlgeschlagen, 5 ignoriert** | 31 s warm |
| fehlgeschlagen | `rdpeudp_tokio::full_stack_loopback_handshake_ipv6`: `Address family not supported` (Container ohne IPv6, **kein Codefehler**) | |
| `cargo xtask check lints -v` | grün; Warnung: MSRV in `clippy.toml` (1.87) ≠ `Cargo.toml` (1.94) | 90 s |
| `cargo test -p ironrdp-server --features egfx,nscodec,qoi,qoiz,helper` | grün (5 Tests; `test = false` in der lib) | 135 s |
| `cargo test -p ironrdp-egfx --features openh264-bundled` | 55 bestanden | 42 s |
| `cargo test -p ironrdp-testsuite-core --features openh264-bundled` | 1719 bestanden, 2 ignoriert | 114 s |

Hinweis: `cargo xtask check tests -v` bricht nach dem ersten roten Test-Binary ab. Den vollständigen Stand liefert erst `--no-fail-fast` [F].

### 2.3 Pipeline-Karte

| Aufgabe | Server | Client |
|---|---|---|
| Capability-Negotiation | `ironrdp-egfx/src/server.rs:730-887` (`CodecCapabilities`, `negotiate_capabilities`, `intersect_flags`, `sanitize_…`), `:2039-2108` (Advertise-Handler), Default-Leiter `:932`; Legacy-Bitmap-Codecs `ironrdp-server/src/server.rs:3712-3761` | `ironrdp-egfx/src/client.rs:243` (Default nur V8.1 und V8), `:1147-1165` (Filter ohne H.264-Decoder) |
| Surface-Management | `server.rs:125-236` (`Surface`, `Surfaces`), `:1169-1333` (create, delete, map, resize) | `client.rs` (Surface-Map) + `compositor.rs` (`apply_bitmap` `:294`, `solid_fill` `:306` …) |
| Framing, Frame-Ack | `server.rs:493-712` (`FrameTracker`), `send_*` `:1439-1943`, Ack/QoE `:2110-2168` | EndFrame → `FrameAcknowledge` `client.rs:1129` |
| Codec-Encode (Server) | H.264: `encode.rs:76` (Trait), `:111-201` (openh264); AVC-PDU-Helfer `pdu/avc.rs:282-592`; ClearCodec `ironrdp-graphics/src/clearcodec/mod.rs:409`; Progressive-Bausteine `progressive.rs:366,459,530` + `ironrdp-pdu/src/codecs/rfx/progressive.rs:1037`; Planar `rdp6/bitmap_stream/encoder.rs`; Legacy `ironrdp-server/src/encoder/{mod,rfx,bitmap}.rs`; NSCodec `ironrdp-nscodec/src/encoder.rs`; QOI/QOIZ `encoder/mod.rs:890` | – |
| Codec-Decode (Client) | – | Dispatch `client.rs:813-872`; AVC420 `:961` via `decode.rs:162` (Trait) / `:247` (openh264); ClearCodec `graphics/clearcodec/mod.rs:48` (+ NSCodec-Subcodec `clearcodec/nscodec.rs`); Planar `rdp6/bitmap_stream/decoder.rs`; Progressive `progressive.rs:1287`; Legacy RFX `ironrdp-session/src/rfx.rs`, QOI `fast_path.rs:839` |
| Farbkonvertierung | `graphics/color_conversion.rs:48` (RFX, `yuv::rdp_*`), `progressive.rs:530`, `nscodec/encoder.rs:158`, `egfx/encode.rs:181` (openh264) | `color_conversion.rs:27`, `progressive.rs:675`, `clearcodec/nscodec.rs:191`, `rdp6/…/decoder.rs:261`, `egfx/decode.rs:285-301` |
| Bulk (ZGFX) | `graphics/zgfx/compressor.rs:34`, `wrapper.rs` (`compress_and_wrap_egfx`), angewandt in `server.rs:1961-2029` (Default `CompressionMode::Never`, `:1088`) | `graphics/zgfx/mod.rs:69` (`Decompressor`), `client.rs:412` |
| Server-Einbettung | `ironrdp-server/src/gfx.rs` (`Arc<Mutex<GraphicsPipelineServer>>`-Bridge), `server.rs:1928-1938` | nativer Client `ironrdp-client/src/rdp.rs:1592` (**ohne** H.264-Decoder) |

---

## 3. Chroma-Inventar (B1)

Alle Angaben **[F]** aus dem Code, Messwerte aus T6 und T8.

### 3.1 Server (Encode)

| Codec/Pfad | Farbraum | Matrix | Range | Subsampling | Quantisierung pro Komponente | Fundstelle |
|---|---|---|---|---|---|---|
| AVC420 (`OpenH264Encoder`, Referenz) | YCbCr | **BT.601** (openh264 intern, Koeffizienten 66/129/25 [U #1976]) | **limited** | 4:2:0 (openh264 intern) | H.264-QP aus openh264-Default-Rate-Control, über den Trait **nicht steuerbar** | `ironrdp-egfx/src/encode.rs:180-181` |
| AVC444 / AVC444v2 | – | keine Konvertierung in IronRDP; der Embedder liefert fertige H.264-Streams für Main und Aux | – | – | `QuantQuality` pro Region nur als Metadatum durchgereicht | `server.rs:1488,1530,1556` |
| RemoteFX (Legacy, SurfaceBits) | YCbCr (ICT) | BT.601-Koeffizienten nach MS-RDPRFX (0,299/0,587/0,114; −0,168935 …) | full | 4:4:4 | eine Tabelle für Y, Cb und Cr (`Quant::default` = 6,6,6,6,7,7,8,8,8,9), **Floor**-Shift um (q−1) im 11.5-Festkommaraum | `color_conversion.rs:48` → `yuv-0.8.16/src/rdp.rs:250-326`; `encoder/rfx.rs:171-173`; `quantization.rs:40-46`; `ironrdp-pdu/…/data_messages.rs:493` |
| RemoteFX Progressive (Bausteine) | YCbCr | BT.601, 16-Bit-Festkomma mit Rundung | full | 4:4:4 | Basis 2^(q−6) mit Rundung + BitPos, Tabellenwahl durch den Embedder; **8-Bit-Ganzzahlbereich ohne Nachkommastellen** | `progressive.rs:530-553,401-427,310-340` |
| NSCodec (SurfaceBits) | YCoCg | Y = R>>2 + G>>1 + B>>2 (einzeln abgeschnitten), Co = R−B, Cg = G−(R+B)/2 | full | **keins** (CSL = 0) | Co und Cg >> CLL mit **CLL = Maximum aus Client-Cap** | `nscodec/src/encoder.rs:128,158-178`; `ironrdp-server/src/server.rs:3743-3749` |
| ClearCodec (EGFX) | RGB | – | – | – | verlustfrei (Residual-BGR-RLE, Glyph-Cache ≤ 1024 px), **nutzt keine Subcodecs** | `graphics/clearcodec/mod.rs:409-493` |
| Planar (Legacy-Bitmap, EGFX vorcodiert) | RGB | – | – | – | verlustfrei (ARGB-Planes, CLL = 0, kein CS) | `rdp6/bitmap_stream/encoder.rs:174`; `encoder/bitmap.rs:102-111` |
| Uncompressed (EGFX) | XRGB | – | – | – | verlustfrei | `server.rs:1726-1768` |
| QOI / QOIZ (IronRDP-eigene Codecs) | RGB | – | – | – | verlustfrei (Alpha verworfen) | `encoder/mod.rs:890-919` |

### 3.2 Client (Decode)

| Codec/Pfad | Farbraum | Matrix | Range | Upsampling | Dequantisierung | Fundstelle |
|---|---|---|---|---|---|---|
| AVC420 (`OpenH264Decoder`) | YCbCr → RGBA | **BT.709** | **full** | `yuv::yuv420_to_rgba` | – | `egfx/src/decode.rs:282-302` |
| AVC444 / AVC444v2 | **nicht unterstützt** (`on_unhandled_pdu`) | | | | | `client.rs:850-853` |
| RemoteFX (Legacy) | YCbCr → RGBA | BT.601 invers (1,402525 …), Floor `>>21` | full | – | `<< (q−1)` | `session/src/rfx.rs:216,221-233`; `yuv-0.8.16/src/rdp.rs:379-430` |
| Progressive | YCbCr → RGBA | BT.601, 16-Bit-Festkomma mit Rundung, i64 | full | – | 2^(q−6) + BitPos, **8-Bit-Bereich** | `progressive.rs:675-684,561-590` |
| NSCodec | nur als ClearCodec-Subcodec (Legacy-Session hat keinen NSCodec-Decoder) | YCoCg, Shift (CLL−1) | full | Nearest (CSL 1 unterstützt) | – | `clearcodec/nscodec.rs:20-190,191` |
| Planar | YCoCg bzw. RGB | inverse AYCoCg mit CLL, R/B-Tausch bei 24 bpp (MS-RDPEGDI 3.1.9.1.2) | full | Nearest (CS) | – | `rdp6/bitmap_stream/decoder.rs:197-230,261-288` |
| ClearCodec, Uncompressed, QOI | RGB | – | – | – | – | `clearcodec/mod.rs:48`, `client.rs`, `session/src/fast_path.rs:839` |

---

## 4. Befunde

Jeder Befund nennt Kategorie, Schwere, Status (F/H/U), Beleg, Upstream-Status und einen Lösungsvorschlag mit Aufwand (S ≤ 1 Tag, M ≤ 1 Woche, L > 1 Woche).

### Teil B: Chroma

#### C-01 AVC420: Encoder BT.601 limited, Decoder BT.709 full

- **Kategorie:** Farbkonvertierung, Spec-Konformität. **Schwere:** kritisch. **Status:** F (Code und Messung).
- **Beleg:**
  - `encode.rs:180-181` nutzt `YUVBuffer::from_rgb_source`.
  - `decode.rs:295-301` nutzt `YuvRange::Full, Bt709`.
  - MS-RDPEGFX 3.3.8.3.1 schreibt full-range BT.709 vor.
- **Messung (ohne H.264, nur Farbkonvertierung, T1/T6):**
  - roter Text: SSIM 0,125 und PSNR Y 23,5 dB (mit angepasster Matrix: 0,760 bzw. 36,0 dB)
  - Weiß `#FFFFFF` → `#ECECEC` (ΔE00 3,9)
  - Grün `#00FF00` → `#00CB07` (ΔE00 11,8)
  - Cyan → `#00D8EF` (ΔE00 11,3)
  - der schwarze Hintergrund wird grau (Luma-Offset 16)

  Der ausgelieferte Pfad mit echter Kompression liefert praktisch dieselben Werte (SSIM 0,123).
- **Upstream:**
  - **offener PR #1976** [U: „openh264 hardcodes limited-range BT.601“].
  - Issue #1924 wurde beim Merge des Decoder-Fixes #1923 geschlossen, bevor der Encoder-Fix existierte [U].
- **Vorschlag:** #1976 übernehmen (`yuv::rgba_to_yuv420` full/BT.709 → `YUVSlices`). Dazu einen chromatischen Round-Trip-Test, der z. B. `#00FF00` innerhalb ΔE < 1 fordert. Aufwand S.

#### C-02 Kein YUV444-Split-Helfer, `H264Encoder` kann die Aux-View nicht codieren

- **Kategorie:** Server-API, AVC444. **Schwere:** hoch. **Status:** F.
- **Beleg:**
  - `send_avc444_frame` und `send_avc444v2_frame` erwarten fertige H.264-Bitstreams (`server.rs:1488-1554`).
  - Kein Crate enthält Code für die Views B1 bis B9. Eine Suche nach `yuv444`, `auxiliary` und `main_view` findet nur PDU- und Test-Code.
  - `H264Encoder::encode(EncodeFrame)` nimmt ausschließlich RGBA (`encode.rs:15-30,84`). Die Aux-View ist kein RGB-Bild, sondern umgepackte U- und V-Samples in den Y-, U- und V-Planes. Über diesen Trait lässt sie sich nicht einspeisen.
  - Die Spec verlangt zudem einen Encoder und einen Decoder für beide Views als einen Stream (2.2.4.5 und 2.2.4.6).
  - Das Layout für v1 und v2 ist in IronRDP nirgends dokumentiert. Es gibt keine Referenzimplementierung und keine Tests jenseits der PDU-Serialisierung.
- **Folge:** Jeder Downstream-Server muss Layout, Filter und Encoder-Anbindung selbst bauen. Die Fehlerwahrscheinlichkeit ist hoch, wie selbst FreeRDPs SIMD-Pfad zeigt (C-14).
- **Upstream:** Kein Issue oder PR zu Split-Helfer oder planarer Encoder-API gefunden (Suche „AVC444 auxiliary view chroma split YUV444 helper“). PR #2002 (offen) ergänzt nur `MixedTilePayload::Avc444`.
- **Vorschlag:**
  - `ironrdp-egfx::avc444::{split_v1, split_v2}` bereitstellen: RGBA oder YUV444 → Main- und Aux-YUV420 nach 3.3.8.3.2 und 3.3.8.3.3, Main-Chroma als 2×2-Mittel.
  - Referenztest gegen FreeRDPs `RGBToAVC444YUV(v2)`: Das Spec-Modell in `avc_experiment.py` ist nachweislich bit-identisch (T7).
  - `H264Encoder` um `encode_yuv420(&YuvPlanes)` erweitern.

  Aufwand M.

#### C-03 Client dekodiert kein AVC444; `H264Decoder` liefert RGBA

- **Kategorie:** Client, AVC444. **Schwere:** hoch. **Status:** F.
- **Beleg:**
  - `client.rs:850-853` leitet `Avc444` und `Avc444v2` an `on_unhandled_pdu` weiter.
  - Die Default-Caps enthalten deshalb nur V8.1 und V8 (`client.rs:222-251`).
  - `H264Decoder::decode` gibt ein RGBA-`DecodedFrame` zurück (`decode.rs:29-35,168`). Die Kombination nach 3.3.8.3.2 braucht aber die YUV-Planes beider Views vor der Farbkonvertierung.
  - Der mitgelieferte native Client registriert EGFX ganz ohne H.264-Decoder (`ironrdp-client/src/rdp.rs:1592`). IronRDPs eigener Client nutzt also überhaupt kein AVC.
- **Upstream:** #1563 (geschlossen) hat die Caps korrigiert, damit Windows kein AVC444 mehr schickt [U]. Ein AVC444-Decoder ist nicht in Arbeit.
- **Vorschlag:**
  - Trait um `decode_yuv420() -> YuvPlanes` erweitern.
  - Kombination v1 und v2 inkl. optionalem Rückfilter mit Schwelle implementieren; Referenz ist `avc_experiment.py::combine`.
  - Test gegen FreeRDP-generic (T7: Abweichung nur bei |d| = 30).

  Aufwand M.

#### C-04 AVC444-Kombination: Fehlerverstärkung durch den Rückfilter (Kern-Hypothese bestätigt)

- **Kategorie:** Chroma, Spec-Design. **Schwere:** hoch (Designgrenze, kein IronRDP-Bug). **Status:** F (Spec, FreeRDP, Messung).
- **Spec (MS-RDPEGFX 3.3.8.3.2, 3.3.8.3.3):**
  - Die Main View enthält Ũ(2x,2y), das 2×2-Mittel. Die explizite Definition steht nur in der v2-Abbildung („image22“), siehe C-14.
  - Die Aux View enthält die übrigen drei Samples.
  - Der Client rechnet U(2x,2y) = 4·Ũ − (drei Aux-Samples).
  - Optional gilt: Weicht der rekonstruierte Wert um höchstens 30 von Ũ ab, bleibt Ũ stehen (Bedingung `abs(Ũ − U) > 30 ? U : Ũ`).
- **FreeRDP:**
  - Der Encoder mittelt (`prim_YUV.c:1411-1417`, `(U1e+U2e+U1o+U2o)/4`).
  - Der Client filtert mit `CONDITIONAL_CLIP` (`prim_internal.h:215-226`, **`diff < 30` → Ũ**). Die Grenze ist also invertiert: Bei |d| = 30 rekonstruiert FreeRDP, die Spec nicht (C-14).
- **IronRDP:** Weder Aufteilung noch Rekonstruktion vorhanden (C-02, C-03).
- **Messung (T4):**
  - RMS-Fehler des rekonstruierten U/V(2x,2y) geteilt durch den RMS-Codierfehler von Ũ: **Median 4,1 bis 5,0**, Spanne 2,1 bis 7,1 (QP 18 bis 34, v1 und v2).
  - Das passt zur Theorie √(16σm² + 3σa²)/σm ≈ 4,1 bis 4,4 plus Clipping.
- **Folgen (T3):**
  - Ohne Rückfilter bleibt ein Viertel der Chroma-Samples gemittelt (rot, PSNR V 30,0 dB).
  - Mit Rückfilter ohne Schwelle ist die Übertragung ohne Kompression nahezu verlustfrei (54,5 dB). Bei QP 30 kippt es aber: SSIM 0,60 statt 0,80.
  - Der Spec-Schwellwert ist ein Kompromiss.
  - Die Was-wäre-wenn-Variante mit **punktabgetasteter** Main-Chroma ist bei allen QPs besser (QP 22: PSNR V 47,7 statt 39,4 dB; Kanten-Chroma-Fehler 1,48 statt 2,83). Sie ist aber **nicht spec-konform** und würde bei Clients mit Rückfilter Fehler erzeugen.
- **Einordnung:** Das erklärt plausibel, warum AVC444 in der Praxis bei moderatem QP weniger gewinnt als erhofft [H]. Gegenüber AVC420 bleibt der Gewinn für farbigen Text trotzdem groß (C-05, T2, T5). AVC444 ist also auch bei rotem Text auf Schwarz nicht schlechter als 4:2:0; die Verstärkung begrenzt nur den Gewinn.
- **Aufbau von T2 bis T4:** libx264 kodiert mit dem Standard-`ipratio` 1,4 die Main View (I-Frame) mit QP q−3 und die Aux View mit q, dazu kommt ein Chroma-Offset von −2 (psy-rd). Die Main View war dort also schon feiner kodiert.
- **Messung T9 (niedrigerer Main-QP × Client-Filter; alle 11 Bilder, v1 und v2, QP 18 bis 34):**
  - Referenz `d0`: beide Views mit q. `d-N`: Main View mit q−N. `c-N`: `chroma_qp_index_offset` −N im PPS. Client-Filter: keiner, Spec (> 30), FreeRDP (≥ 30), ohne Schwelle, adaptiv.
  - **Bei gleichem QP hilft ein niedrigerer Main-QP jedem Client-Typ im Mittel (T9c).** Beispiel `d-6` (+17 % Bytes): kein Filter +0,27 dB, Spec +1,03, ohne Schwelle +2,05, adaptiv +1,77 dB Chroma-PSNR. Die feste Schwelle halbiert den Gewinn. Einzelne Ausreißer bis −2,4 dB (`lines_1px`, `c-6`, QP 18).
  - **Bei gleicher Bitrate lohnt sich ein niedrigerer Main-QP nicht (T9d).** Gegenüber einem global niedrigeren QP mit gleich vielen Bytes bringt er im Mittel höchstens +0,1 dB (Spec-Schwelle) bzw. +0,55 dB (ohne Schwelle, `c-6`). Ab `d-6` und `c-12` wird es schlechter, bis −1,25 dB. Gemessen ist nur Chroma; ein globaler QP verbessert zusätzlich Luma. Der libx264-Standard (`d-3`) liegt schon nahe am Optimum.
  - **Ein adaptiver Rückfilter im Client wirkt ohne zusätzliche Bytes (T9e).** Er rekonstruiert, wenn |Ũ − U| > 0,4·√(16·s(QPc_main)² + 2·s(QP_aux)² + s(QPc_aux)²), s = H.264-Quantisierungsschritt. Slice-QP und PPS-Offset liest der Client aus dem Bitstream. Gegenüber der Spec-Schwelle auf denselben Streams: Ø +1,81 dB Chroma-PSNR (QP 18 +4,2; QP 22 +2,9; QP 26 +1,4; QP 30 +0,5; QP 34 +0,1), Kanten-Chroma-Fehler Ø −0,31. Schlechtester Einzelfall −0,49 dB (Verlauf, QP 18); 31 von 660 Fällen sind mehr als 0,1 dB schlechter.
  - k ist zwischen 0,3 und 0,5 flach. Leave-one-image-out wählt für jedes der 11 Bilder k = 0,4.
  - Rot auf Schwarz, v1, QP 22 (T9a), PSNR V: `d0` mit Spec-Schwelle 38,9 dB, adaptiv 41,9 dB; `d-3` 39,4 → 43,5 dB.
  - FreeRDPs Grenze (≥ 30) ist minimal besser als die der Spec (> 30): Ø +0,1 dB.
- **Upstream:** kein Bezug.
- **Vorschlag:**
  - Client (sobald C-03 behoben ist): QP-adaptiven Rückfilter statt der festen Schwelle 30. Der Filter ist laut Spec optional, das bleibt also konform. Er funktioniert mit jedem Server und kostet keine Bytes. Voraussetzung: Slice-QP und `chroma_qp_index_offset` aus SPS, PPS und Slice-Header parsen. Aufwand S bis M. Vorbehalt: Wie der Windows-Encoder Ũ und die QPs wählt, ist unbekannt (Abschnitt 6, Frage 2). Mit AVC444-Captures prüfen.
  - Server: Main View etwa 3 QP unter der Aux View kodieren (`d-3`), nicht mehr. Größere Offsets kosten mehr Bytes, als sie gegenüber einem globalen QP bringen.
  - Forschung, nicht priorisiert: Aux für statische Textregionen nachreichen (LC = 2). Closed-Loop-Ũ (Ũ' aus den rekonstruierten Aux-Samples) entfernt nur den Aux-Term, rechnerisch ≤ ~10 %; nicht gemessen.

#### C-05 AVC420 gegen AVC444: der Gewinn für farbigen Text ist groß

- **Kategorie:** Chroma, Messung. **Schwere:** – (Einordnung). **Status:** F (Messung, libx264).
- **Bei gleichem QP 22 (T2):**
  - Kanten-Chroma-Fehler rot 18,5 → 2,83 (v1) bzw. 2,86 (v2); blau 19,4 → 2,68 bzw. 2,70; ClearType 11,2 → 3,93.
  - Kosten: Faktor 1,9 bis 2,4 an Bytes bei farbigem Text, 1,00 bis 1,01 bei weißem Text (die Aux View ist dort fast leer), 1,4 bis 1,8 bei ClearType.
- **Bei gleicher Bitrate (T5):**
  - Rot: AVC420 QP 18 (12 010 B) liefert Kanten-Chroma-Fehler 18,2 und ΔE00 an der Kante 8,4; AVC444 QP 30 (12 252 B) liefert 5,8 und 3,4.
  - Blau: 19,3 und 7,8 gegenüber 5,2 und 2,7.
  - Der Chroma-Fehler von AVC420 ist durch das 4:2:0-Subsampling nach unten begrenzt, auch ohne Kompression liegt er bei rund 18 (`avc420_noenc`).
- **v1 gegen v2:** innerhalb des Rauschens. v2 ist bei QP ≥ 26 minimal besser und 2 bis 4 % kleiner (T2).
- **Vorbehalt:** Die Bytes stammen von libx264, nicht vom Microsoft-Encoder [H: Die Relationen sind übertragbar].

#### C-06 Client-AVC420 ignoriert `regionRects` und schneidet ab dem Ursprung

- **Kategorie:** Spec-Konformität, Mixed Frames. **Schwere:** hoch. **Status:** F (Code und Test); Auswirkung gegen Windows H.
- **Beleg:**
  - `decode_avc420` (`client.rs:961-1006`) dekodiert den Frame und kopiert `crop_decoded_frame(frame, dest_w, dest_h)` (`:993`, Funktion `:1265-1300`). Dabei werden immer die Zeilen und Spalten ab (0,0) des H.264-Frames genommen.
  - Das Ergebnis landet vollflächig auf `destRect`. Die `regionRects` aus `Avc420BitmapStream` werden gelesen, aber nicht angewandt.
  - Spec 2.2.2.1: Bei AVC-Codecs ist `destRect` ein **Bounding-Rect**. 2.2.4.4 und 3.3.8.3.2: Der Frame „MUST be cropped by the region mask“; „Color conversion MUST be performed for the entire macroblock, after which the region mask in regionRects MUST be applied“.
  - FreeRDP dekodiert AVC420 in Surface-Koordinaten und maskiert mit `regionRects` (`gdi/gfx.c`, `avc420_decompress(…, surface->width, surface->height, regionRects, numRegionRects)`).
- **Folgen:**
  - Ist `destRect.left/top ≠ 0`, landen falsche Pixel auf der Surface.
  - Pixel im Bounding-Rect, aber außerhalb der Regionen, werden überschrieben. In Mixed Frames trifft das ClearCodec-Kacheln, sobald AVC später in der PDU-Folge steht.
  - IronRDPs eigener Server erzeugt genau solche Bounding-Rects (`compute_dest_rect`, `server.rs:1405-1433`).
- **Upstream:** kein Issue gefunden (Suche „AVC420 regionRects destination rectangle crop offset“).
- **Test:** `chroma/harness/tests/c06_avc420_region_rects.rs` verbindet IronRDPs Server (`send_avc420_frame`) mit IronRDPs Client (OpenH264-Decoder) und prüft den komponierten Output.
  - Kontrolle (eine Region über die ganze Surface): besteht, Mittelwert 235,9.
  - Region (32,32)–(64,64) mit weißem Quadranten an genau dieser Stelle im Frame: **schlägt fehl**, die Surface zeigt Mittelwert 16,0 (Schwarz aus der linken oberen Frame-Ecke).
  - Zwei Regionen in gegenüberliegenden Ecken: **schlägt fehl**, Pixel (32,32) liegt außerhalb beider Regionen und wird von ClearCodec-Blau `[0,0,255]` zu `[236,236,236]` überschrieben.
- **Vorschlag:** Den Frame in Surface-Koordinaten behandeln und je Region-Rect den Ausschnitt (left, top, right, bottom) kopieren. Die beiden Tests dienen als Regressionstests. Aufwand S.

#### C-07 Progressive: DWT im 8-Bit-Ganzzahlbereich, nicht verlustfrei erreichbar

- **Kategorie:** Codec-Präzision. **Schwere:** hoch (Qualität), mittel (Interop). **Status:** F (Messung und Test); Decoder-Auswirkung mit synthetischen Windows-artigen Streams F, mit echten Captures offen.
- **Beleg:**
  - `rgba_to_ycbcr` erzeugt ganzzahlige Werte in [−128, 127] ohne Nachkommabits (`progressive.rs:546-548`). Die DWT läuft darauf (`:377-381`).
  - Die Spec-DWT (MS-RDPRFX 3.1.8.1.4, Abb. 6: H[n] = ⌊(X[2n+1] − ⌊(X[2n]+X[2n+2])/2⌋)/2⌋) verwirft pro Stufe das LSB des Hochpasses und ist bei Ganzzahlpräzision prinzipiell verlustbehaftet.
  - Der klassische RFX-Pfad in IronRDP (wie FreeRDP) rechnet deshalb im 11.5-Festkommaraum (×32, `yuv/src/rdp.rs:250-326`).
- **Messung (T8), DWT hin und zurück ohne jede Quantisierung, maximaler Fehler:**
  - Progressive 8 Bit: **15 Stufen** (Median der Bild-Maxima 13)
  - mit Reduce-Extrapolate: 6
  - RFX 11.5: 0,47
- **Folge (T1):**
  - Progressive mit feinster Basis-Quantisierung (q = 6) und vollem Pass: max. Fehler 26 (rot), PSNR V 41,6 dB.
  - Die Upgrade-Passes für Chroma können deshalb nie verlustfrei werden.
  - Die Windows-Captures nutzen Reduce-Extrapolate (REGION flags 0x01, T9); dort liegt der Fehler bei 11 bis 14 (T1 `_re`).
  - Decoderseite (Test `chroma/harness/tests/c07_progressive_decoder_precision.rs`): Koeffizienten, wie Windows und FreeRDP sie erzeugen (11.5-Festkomma, gerundet durch 2^5 bei q = 6, eine TILE_SIMPLE), dekodiert IronRDP mit **4 bis 8 Stufen** Abweichung von einer 11.5-Referenzdekodierung derselben Koeffizienten.
    - rote Striche mit Reduce-Extrapolate: 7 (Referenz gegen Original 6, IronRDP gegen Original 8)
    - rote Striche mit Standard-DWT: 8 (IronRDP gegen Original 12)
    - glatter Verlauf mit Reduce-Extrapolate: 4

    Einschränkungen: Die Referenz nutzt IronRDPs eigene DWT-Funktionen im 11.5-Bereich, nicht FreeRDPs Decoder. Der Stream ist synthetisch, kein echter Capture. Die Toleranz von 2 Stufen habe ich gesetzt.
  - Plausibilitätsprüfung mit dem echten Windows-Capture (`chroma/harness/src/bin/progressive_fixture.rs`, `chroma/progressive_fixture_compare.py`, Ergebnis `chroma/results/progressive_fixture/summary.txt`): Die 16 Basiskacheln aus `wts2_progressive_tile_first_mixed_25tiles.bin` wurden mit IronRDP, mit FreeRDP 3.32.2 (`progressive_decompress`) und mit der 11.5-Referenz dekodiert, 31 424 Pixel.
    - IronRDP gegen FreeRDP: max. 4, Mittel 0,66, 3,3 % der Pixel weichen um mehr als 2 ab.
    - 11.5-Referenz gegen FreeRDP: max. 1, Mittel 0,045.

    Die Referenz trifft FreeRDP also fast exakt, die Abweichung von IronRDP erklärt sich durch die 8-Bit-Rekonstruktion. Die Richtung der Hypothese ist damit bestätigt; das Ausmaß ist auf diesem Inhalt klein (glatte UI-Fläche, Qualitätsstufe 0). Ohne Originalbild bleibt es ein Vergleich zwischen Decodern.
- **Upstream:**
  - kein Issue gefunden
  - #1400 (geschlossen, nicht gemergt) betraf eine andere Farbskalierung
  - #1499 hat die Quant-Skala korrigiert, nicht die Präzision
- **Vorschlag:** Progressive auf den 11.5-Festkommaraum umstellen (Quantisierung q−1 wie im klassischen Pfad). Regressionstest „q = 6, voller Pass ⇒ max. Fehler ≤ 1“. Aufwand M.

#### C-08 RFX-Encoder quantisiert per Floor statt gerundet

- **Kategorie:** Spec-Konformität, Qualität. **Schwere:** mittel. **Status:** F (Spec und Messung).
- **Beleg:**
  - `quantization.rs:40-46`: `*value >>= factor`.
  - MS-RDPRFX 3.1.8.1.5: „dividing each coefficient by the scale value **and rounding it**“.
  - FreeRDP rundet (`rfx_quantization_encode_block`).
- **Messung, identische Pipeline nur mit Rundung (Was-wäre-wenn, T1):**
  - Rot mit Default-Quant: PSNR V 39,0 → 44,6 dB, max. Fehler 63 → 33, Bytes −7,0 %.
  - Blau: PSNR U 38,1 → 44,0 dB, Bytes −14,8 %.
  - Marineblau: Bytes −26,5 %.
  - Flächen (T6): Floor erzeugt einen systematischen −1-Bias (`#FFFFFF` → `#FEFEFE`, `#808080` → `#7F7F7F`); mit Rundung exakt.
- **Upstream:** kein Issue gefunden (#1179 hat nur RLGR-Bugs behoben).
- **Vorschlag:** Rundung `(c + 2^(f−1)) >> f` für f > 0. Aufwand S. Nebenbei: Die inverse Farbkonvertierung im `yuv`-Crate endet ebenfalls mit Floor (`rdp.rs:358-360`) [F].

#### C-09 NSCodec: maximale CLL des Clients statt CLL 1; Y-Trunkierung

- **Kategorie:** Chroma-Quantisierung. **Schwere:** mittel. **Status:** F.
- **Beleg:**
  - `server.rs:3743-3749` übernimmt `client_ns.color_loss_level`. Der Kommentar „so the server encodes at the same shift the client decodes against“ trifft nicht zu: Die CLL steht in jedem Bitstream-Header (MS-RDPNSC 2.2.2), und die Cap nennt nur das **Maximum** (2.2.1: „maximum supported Color Loss Level“).
  - MS-RDPEGDI 3.1.9.1.4: „The server MUST choose a value between 1 and 7“.
  - `fAllowDynamicFidelity` wird ignoriert.
- **Messung (T1):**
  - CLL 1: max. Fehler 1 (rot), blau bit-exakt.
  - CLL 3 (FreeRDP-Client-Default, `nsc.c:363`): max. Fehler 5 bis 6, PSNR U 52 bis 53 dB.
  - CLL 7: max. Fehler 95 bis 126.
- **Zusatz:** `Y = (R>>2)+(G>>1)+(B>>2)` kappt Weiß auf 253 (`#FDFDFD`, T6; FreeRDP identisch, `nsc_encode.c:235`). `(R+2G+B)>>2` wäre exakt.
- **Positiv:** Chroma-Subsampling ist nie aktiv (CSL = 0, `encoder.rs:128`).
- **Upstream:** keins gefunden.
- **Vorschlag:** Immer CLL 1 codieren (bzw. konfigurierbar) und Y ohne Einzeltrunkierung berechnen. Aufwand S.

#### C-10 Kein Routing, AVC444 fehlt in Mixed Frames, `AVC_THINCLIENT` ignoriert, stille Fehlschläge

- **Kategorie:** Server-API, Routing. **Schwere:** mittel. **Status:** F.
- **Beleg:**
  - Es gibt keine Klassifikations- oder Routing-Schnittstelle. Der Legacy-Pfad wählt pro Sitzung statisch QOIZ > QOI > RFX > NSCodec (`encoder/mod.rs:163-186`).
  - In EGFX entscheidet der Embedder pro Aufruf; `MixedTilePayload` kennt nur ClearCodec, Progressive und AVC420 (`server.rs:1044-1065`).
  - Farbiger Text lässt sich gezielt zu ClearCodec routen, aber nur über `send_mixed_frame` oder `send_clearcodec_frame` mit eigener Klassifikation.
  - Mixed Frames: Die PDU-Reihenfolge entspricht der Reihenfolge im `Vec` (`server.rs:1904-1938`). Nichts verhindert, dass AVC-Bounding-Rects ClearCodec-Kacheln überdecken, und es gibt keine Dokumentation zur richtigen Reihenfolge (AVC zuerst, dann ClearCodec).
  - `CodecCapabilities::thin_client` bildet `RDPGFX_CAPS_FLAG_AVC_THINCLIENT` ab, das laut Spec 2.2.3.6 „the client **prefers** … YUV444“ bedeutet. Das Flag wird nirgends ausgewertet (`server.rs:725`).
  - `send_avc444_frame` liefert bei `!supports_avc444()` ohne Log einfach `None` (`server.rs:1581-1583`). Es gibt keinen automatischen Rückfall auf AVC420 und kein sichtbares Signal außer dem Rückgabewert.
  - Die ausgehandelte Version wird nur auf Debug-Level geloggt.
- **Upstream:** PR #2002 (offen): AVC444-Kacheln in Mixed Frames. PR #2000 (offen): Kacheln vor dem Queuen prüfen.
- **Vorschlag:**
  - Trait `ContentClassifier` (pro Dirty-Rect: Text-Farbe, Foto, Video).
  - Dokumentierte Reihenfolge in `send_mixed_frame`; AVC-Regionen um ClearCodec-Kacheln kürzen.
  - `thin_client` in `supports_avc444_preferred()` exponieren.
  - Einmaliges `warn!` bei abgelehnten AVC444-Frames.

  Aufwand M.

#### C-11 LC-Feld, Aux-Strategie, Backpressure

- **Kategorie:** AVC444-Protokoll. **Schwere:** niedrig. **Status:** F.
- **Server setzt LC:**
  - v1: `send_avc444_frame` kennt nur LC 0 (mit Chroma) oder LC 1 (ohne), **LC 2 ist mit der v1-API nicht sendbar** (`server.rs:1496-1502`).
  - v2: alle drei Werte (`:1530-1554`), Form geprüft (`:1567-1576`).
- **Spec-Abweichung:** Der Encoder schreibt bei LC 2 die Länge von Stream 1 statt 0 in `cbAvc420EncodedBitstream1` (`pdu/avc.rs:204`, Spec 2.2.4.5: „If no YUV420 frame is present, then this field MUST be set to zero“). Upstream: **offener PR #1999** [U: FreeRDPs Client liest das Feld nur bei LC 0].
- **Client:** entfällt (C-03). Die Frage, ob Luma-only-Frames mit veraltetem Chroma kombiniert werden, stellt sich im IronRDP-Client nicht.
- **Keine Strategie im Crate:** Kein Aufschieben der Aux View für Textregionen, keine Heuristik.
- **Backpressure:** Sie lehnt den **ganzen** Frame ab (`None`, `server.rs:1584-1587`) und erzeugt keine Luma-only-Frames. Der Embedder entscheidet.
- **Regionen:** Main- und Aux-Regionen sind unabhängig. `destRect` ist die Vereinigung (`:1647-1662`). Es wird nicht geprüft, dass Aux-Regionen in früheren Luma-Regionen liegen (Spec: „last corresponding rectangle in a luma subframe“).
- **Vorschlag:** LC 2 in der v1-API ergänzen und #1999 übernehmen. Optional prüfen, ob Aux ⊆ bekannte Luma-Regionen. Aufwand S.

#### C-12 `QuantQuality` und QP: durchgereicht, nicht steuerbar, Panic bei qp ≥ 64

- **Kategorie:** API-Robustheit. **Schwere:** niedrig bis mittel. **Status:** F.
- **Beleg:**
  - `Avc420Region` trägt `quantization_parameter` und `quality` pro Region, die als `RDPGFX_AVC420_QUANT_QUALITY` auf den Draht gehen (`pdu/avc.rs:282-367`).
  - Main und Aux können getrennte Werte tragen (separate Regionslisten), das wird aber nirgends genutzt.
  - Laut Spec 2.2.4.4.1 ist der Metablock „purely informational and SHOULD NOT be used by the client when decoding“. Die tatsächliche Qualität legt allein der Encoder fest, und `H264Encoder` bietet weder QP noch ROI oder Qualität an (`encode.rs:76-104`).
  - `QuantQuality::encode` ruft `set_bits(0..6, qp)` auf (`pdu/avc.rs:45`). `bit_field` 0.10.3 asserted „value does not fit into bit range“ (`lib.rs:264-267`), also **Panic bei qp ≥ 64**, ausgelöst durch einen öffentlichen `u8` über `send_avc420_frame` → `encode_avc420_bitmap_stream(...).expect(...)` (`avc.rs:587-589`).
  - qp 52 bis 63 (für H.264 ungültig) wird kommentarlos gesendet.
- **Upstream:** keins gefunden.
- **Test:** `chroma/harness/tests/c12_qp_panic.rs`: Kontrolle qp 51 besteht. `encode_avc420_bitmap_stream` und `GraphicsPipelineServer::send_avc420_frame` mit qp 64 **paniken** beide mit „value does not fit into bit range“ (`pdu/avc.rs:45:14`).
- **Vorschlag:** QP in `Avc420Region::new` auf 0 bis 51 klemmen oder validieren. `EncodeParams { qp, roi }` im Encoder-Trait. Aufwand S.

#### C-13 Farbkonsistenz über Codecs (Mixed Frames)

- **Kategorie:** Chroma, Mixed Frames. **Schwere:** mittel. **Status:** F (Messung T6).
- **Messung:** Dieselben Volltonfarben dekodiert:
  - ClearCodec und Planar exakt.
  - RFX mit −1-Bias (C-08).
  - Progressive fast exakt (`#000080` → `#010080`).
  - AVC nach wörtlicher Spec-Integer-Matrix (`>>8` mit Floor, wie FreeRDP): `#FF0000` → `#FC0000` (ΔE00 0,6), `#00FFFF` → `#00FEFB` (1,0).
  - Aktueller IronRDP-AVC-Pfad: bis ΔE00 11,8 (C-01).
- **Folge:** An Kachelgrenzen ClearCodec ↔ AVC entstehen mit dem aktuellen Encoder deutlich sichtbare Kanten. Nach dem Fix für C-01 bleiben Sprünge von ≤ 3 Stufen, knapp an der Wahrnehmungsschwelle.
- **Vorschlag:** Beim RGB→YUV-Schritt runden statt floor. Der Client ist davon unabhängig, er sieht nur YUV. Aufwand S.

#### C-14 Referenzen: Spec-Inkonsistenz und FreeRDP-Abweichungen (Interop-relevant)

- **Kategorie:** Referenzen. **Schwere:** niedrig für IronRDP, wichtig für Tests. **Status:** F (Messung).
- **Spec:** In 3.3.8.3.3 (v2) steht unter „The following reverse filter must be applied“ die **Vorwärts**-Definition Ũ = Mittel der vier Samples (Abb. image22). In 3.3.8.3.2 (v1) steht dort die Rückrechnung (image18). Ũ ist im Text sonst nicht definiert.
- **FreeRDP, Grenzfall:** FreeRDP rekonstruiert bei |Ũ − U| = 30, die Spec nicht. Die gemessenen Abweichungen fallen exakt auf diese Samples (30/30/50 Pixel in drei Bildern, max. RGB-Differenz 56 ≈ 1,855·30).
- **FreeRDP, SIMD-Pfad AVC444 v1:** Der optimierte Pfad (SSE4.1/AVX2 auf dem Testrechner) weicht vom eigenen generischen Pfad in **19,5 % (bis 29,5 %)** der Pixel ab. Betroffen sind nur gerade Zeilen, also B6/B7. PSNR für roten Text: 21,9 dB statt 39,7 dB. v2 ist unauffällig (T7). Ursache nicht untersucht [H: `sse41_ChromaV1ToYUV444`].
- **Konsequenz:** FreeRDP ist als Referenz für AVC444 v1 nur im generischen Pfad brauchbar. Interop-Tests sollten beide Pfade prüfen.

#### C-15 Windows nutzt verlustbehaftetes NSCodec innerhalb von ClearCodec (Chance)

- **Kategorie:** Referenz (Windows). **Schwere:** – (Stärke von IronRDP). **Status:** F (Capture-Analyse der Haven-Fixtures, Windows Server 2025 / Windows 11 24H2).
- **Beleg (`chroma/results/windows_fixtures.txt`):**
  - `wts1_576x128_nscodec_subregion.bin` enthält einen NSCodec-Subcodec mit **ColorLossLevel = 3**, ChromaSubsamplingLevel = 0.
  - Andere Kacheln nutzen RLEX oder Bands.
  - Progressive von Windows: **eine** Quant-Tabelle für Y, Cb und Cr (6,6,6,7,8,8,9,9,9,10), Reduce-Extrapolate, Qualität 255 bzw. 0.
- **Folge:** Selbst Microsofts „verlustfreier“ ClearCodec-Pfad verliert bei farbigem, texturiertem Inhalt Chroma (CLL 3 ≈ max. Fehler 5 bis 6, C-09). Chroma wird bei Progressive nicht gröber quantisiert als Luma, das gilt für Windows wie für IronRDP.
- **Chance:** IronRDPs ClearCodec-Encoder ist bit-exakt (T1, T7) und damit ein Hebel für „besser als Microsoft“.

#### C-16 Progressive-Decoder liest Reduce-Extrapolate aus dem falschen Flag

- **Kategorie:** Spec-Konformität, Interop. **Schwere:** mittel (latent). **Status:** F (Spec, Code, FreeRDP, Test mit Windows-Capture); Auftreten in der Praxis H.
- **Beleg:**
  - `ProgressiveDecoder::decode_bitmap` entnimmt die DWT-Variante dem CONTEXT-Block (`ironrdp-graphics/src/progressive.rs:1358-1373` über `ProgressiveContextPdu::uses_reduce_extrapolate`, `ironrdp-pdu/src/codecs/rfx/progressive.rs:397-413`). Das REGION-Flag wird zwar geparst (`:792-795`), steuert die Dekodierung aber nicht.
  - Laut Spec ist Bit 0 im CONTEXT `RFX_SUBBAND_DIFFING` (MS-RDPEGFX 2.2.4.2.1.4) und Bit 0 in der REGION `RFX_DWT_REDUCE_EXTRAPOLATE` (2.2.4.2.1.5).
  - FreeRDP macht es spec-konform: `sub = context->flags & RFX_SUBBAND_DIFFING; extrapolate = region->flags & RFX_DWT_REDUCE_EXTRAPOLATE;` (`progressive.c:958-959,1374-1375`).
- **Test** (`chroma/harness/tests/c16_progressive_reduce_extrapolate_flag.rs`): Die 16 Basiskacheln des Windows-Captures (REGION-Flag 0x01) wurden einmal mit CONTEXT-Flag 0x01 und einmal mit 0x00 dekodiert. Das Ergebnis darf sich nicht unterscheiden, weicht aber um bis zu **253** ab. FreeRDP liefert in beiden Fällen dasselbe korrekte Bild; IronRDP liefert mit CONTEXT 0 grobe Artefakte (mittlere Abweichung 69, `results/progressive_fixture/zoom_ctx0.png`).
- **Wann es passiert:** Falsch dekodiert wird nur, wenn beide Bits verschieden sind.
  - FreeRDPs Encoder schreibt beide als 0 (`rfx.c:2300` und der Region-Writer), das passt zufällig.
  - Für Windows ist das CONTEXT-Flag im Capture nicht enthalten. Wegen der Differenz-Kacheln ist `RFX_SUBBAND_DIFFING` = 1 wahrscheinlich, zusammen mit REGION 0x01 würde es dann ebenfalls zufällig passen [H].
  - Andere Server (xrdp, GNOME Remote Desktop, Eigenbauten) wurden nicht geprüft.
- **Upstream:** kein Issue oder PR gefunden (Suche „progressive reduce extrapolate flag context region subband diffing“).
- **Vorschlag:** Die DWT-Variante pro REGION aus `region.uses_reduce_extrapolate()` nehmen und das CONTEXT-Bit als Subband-Diffing behandeln. Den Test hier als Regressionstest übernehmen. Aufwand S.

### Teil A: Allgemein

#### A-01 Core-Tier-Invarianten

- **Kategorie:** Architektur. **Schwere:** mittel. **Status:** F.
- **Beleg:**
  - `ironrdp-pdu`, `ironrdp-graphics`, `ironrdp-egfx`, `ironrdp-session`, `ironrdp-connector`, `ironrdp-svc` und `ironrdp-cliprdr` enthalten kein `no_std` in `lib.rs`. `ironrdp-pdu` hat zwar `std`/`alloc`-Features (`Cargo.toml:19-22`), setzt aber kein `no_std`-Attribut.
  - `ironrdp-graphics` nutzt `std::io` (`color_conversion.rs:1`) und erlaubt global `clippy::arithmetic_side_effects` (`lib.rs:3`, „FIXME“).
  - `ironrdp-egfx` (Core) liest die Uhr (`Instant::now()`, `server.rs:603,1971`). Das ist ein Seiteneffekt und passt nicht zum Sans-IO-Anspruch. `Instant::now()` panikt zudem auf `wasm32-unknown-unknown`, betrifft aber nur den Serverteil [F: Rust-std-Verhalten].
  - Optional wird der C-Code `openh264` in einem Core-Crate eingebunden.
  - `ironrdp-graphics` hängt noch an `num-derive` und `num-traits`, die ausgephast werden (`Cargo.toml`).
- **Stärke:** Es gibt keine tokio-Abhängigkeit in Core-Crates [F].
- **Upstream:** #1352 und #1353 (offen, Aufteilung von `ironrdp-pdu`). Zu `no_std` kein eigenes Issue gefunden.
- **Vorschlag:** Zeit als Parameter injizieren (wie #1530 im Connector). Das `openh264`-Backend in ein Extra-Tier-Crate verschieben. Aufwand M.

#### A-02 Robustheit: `unsafe`, Panics, Decompression Bombs

- **Kategorie:** Sicherheit. **Schwere:** niedrig (Stärke). **Status:** F.
- **`unsafe`:**
  - 0 Vorkommen in core, pdu, graphics, egfx, nscodec, session, server, connector, dvc und svc.
  - 1 Vorkommen im Kommentar von `ironrdp-bulk` (`#![forbid(unsafe_code)]`).
  - Alle echten `unsafe`-Blöcke liegen in Windows-, COM- und FFI-Crates (activex 1338, rdpdr-native, vmconnect, rdpewa-native, cliprdr-native, rpc, agent). Workspace-Lints: `undocumented_unsafe_blocks`, `multiple_unsafe_ops_per_block` (`Cargo.toml:47-84`).
- **Panics in Decode-Pfaden:** überwiegend Invarianten-Asserts auf Puffergrößen (`progressive.rs:109-110,179-180`, `dwt.rs:176,227`) und nachweislich unfehlbare `expect`s (`cmd.rs:1019-1025` Bitfelder, `bands.rs:152-153`). Keine netzgesteuerten Panics gefunden.
- **Allokationen aus Längenfeldern:**
  - ZGFX: 64 MiB je Segment und 256 MiB gesamt (`zgfx/mod.rs:52,67`).
  - ClearCodec: 8192 px je Achse (`clearcodec/mod.rs:112-120`), das erlaubt aber weiterhin bis zu 256 MiB pro Decode.
  - AVC-Metablock: Vorallokation gegen die Restlänge gedeckelt (`pdu/avc.rs:138-146`).
- **Vorschlag:** ClearCodec-Obergrenze an die Surface-Größe koppeln. Aufwand S.

#### A-03 Tests, Fixtures, Fuzzing

- **Kategorie:** Qualitätssicherung. **Schwere:** niedrig bis mittel. **Status:** F.
- **Abdeckung:**
  - Round-Trip-Tests für ClearCodec, RLGR, Progressive (Bausteine) und NSCodec-RLE.
  - AVC-Tests nur auf PDU-Ebene plus ein openh264-Round-Trip, der nur die Abmessungen prüft (`encode.rs:266-302`). Deshalb blieb C-01 unentdeckt.
- **Fixtures:** echte Windows-Captures (Haven, `test_data/egfx/haven/README.md`) nur für ClearCodec und Progressive. Für AVC420 und AVC444 gibt es keine [F]. Laut Upstream enthielten frühere Fixtures zur Rechteck-Semantik denselben Fehler wie der Code [U, Handover].
- **Fuzzing:**
  - 27 Targets (`fuzz/fuzz_targets/`), darunter `egfx_multi_frame`. Dieses Target treibt den Client mit beliebigen `GfxPdu`s und erreicht so auch die ClearCodec-, Planar- und Progressive-Decoder (`oracles/mod.rs:455-500`).
  - **Keine** Targets für ClearCodec-, NSCodec-, RFX- oder Progressive-Encoder oder den ZGFX-Compressor. `bulk_round_trip` deckt nur MPPC/NCRUSH/XCRUSH ab.
- **Upstream:** #1316 geschlossen.
- **Vorschlag:** Encoder→Decoder-Oracles (bit-exakt für die verlustfreien Codecs, Fehlerschranke für die verlustbehafteten) und chromatische AVC-Tests. Aufwand M.

#### A-04 Performance

- **Kategorie:** Performance. **Schwere:** niedrig. **Status:** F.
- **SIMD:**
  - nur inverse DWT (`wide`, `dwt.rs`)
  - RFX-Farbkonvertierung über das `yuv`-Crate (dieses hat SIMD-Backends)
  - Progressive-Farbkonvertierung, NSCodec, RLGR und ClearCodec skalar
- **Kopien pro Frame:**
  - `planar_data.to_vec()` und `bitmap_data.to_vec()` (`server.rs:1710,1762`)
  - `drain_output` codiert jede PDU in einen neuen `Vec` und wrappt ihn in einen zweiten (`:1981-1990`)
  - Client: RGBA-Allokation pro Frame (`decode.rs:280`), Crop-Kopie (`client.rs:993`), Compositor-Kopie
  - RFX-Encoder: Puffer pro Update (`encoder/rfx.rs:136`)
- **Algorithmik:** Die Glyph-Suche im ClearCodec-Encoder scannt linear bis zu 4000 Einträge mit bis zu 4 KiB Vergleich (`clearcodec/mod.rs:501-513`).
- **Threading:**
  - Legacy-Encoding in `spawn_blocking` (`encoder/mod.rs:431`), RFX-Kacheln parallel über rayon (`rfx.rs:148`).
  - EGFX: `Arc<Mutex<GraphicsPipelineServer>>` (`gfx.rs:24`). ZGFX läuft in `drain_output` unter Lock auf dem Event-Thread (Kommentar `server.rs:1964-1969`); ZGFX ist standardmäßig aus (`:1088`).

#### A-05 Server-API: was der Embedder liefern muss

- **Kategorie:** API. **Schwere:** mittel. **Status:** F.
- **Der Embedder liefert:**
  - Capture: `RdpServerDisplay` und `RdpServerDisplayUpdates` (`display.rs:275,319`)
  - H.264-Encoder (Trait plus openh264-Referenz)
  - Split für AVC444 (fehlt, C-02)
  - Content-Klassifikation (fehlt, C-10)
  - Dirty-Region-Erkennung für EGFX. Im Legacy-Pfad macht der Server selbst Bitmap-Diffs (`encoder/mod.rs:333-380`).
- **Zu dünn:** keine QP- oder ROI-Steuerung, kein planarer Encoder-Eingang, kein Progressive-Encoder, keine Mixed-Frame-Validierung (PR #2000 offen).
- **Zu starr:** Die Legacy-Codec-Wahl ist pro Sitzung fest; die AVC444-v1-API kennt kein LC 2.

#### A-06 Capability-Rückfall

- **Kategorie:** Spec-Konformität. **Schwere:** niedrig. **Status:** F.
- **Beleg:** Ohne Überschneidung bestätigt der Server die höchste Version des Clients (`server.rs:2087-2094`), auch wenn er sie selbst nicht unterstützt. Spec 3.2.5.19: „the server SHOULD close the dynamic virtual channel“.
- **Hinweis:** Rev. 19.0 dokumentiert neue Client-Cap-Versionen 0x000B0101, 0x000B0200 und 0x000B0300 (Fußnote <5>), die Windows als V10.7 behandelt. `CapabilitySet` kennt sie nicht.

#### A-07 Abhängigkeiten und Lizenzen

- **Kategorie:** Abhängigkeiten. **Schwere:** neutral. **Status:** F.
- **H.264 im Client:** optional `openh264` 0.9 (BSD-2-Clause) mit `openh264-bundled` (Quellbuild, **ohne** Cisco-Patentlizenz) oder `openh264-libloading` (Cisco-Binary mit Hash-Prüfung). Der mitgelieferte native Client nutzt kein H.264 (C-03).
- **Weitere:** `yuv` 0.8 (BSD-3/Apache-2.0), `wide` 0.7 (Zlib/Apache/MIT), `qoicoubeh` 0.5 (MIT/Apache).
- **Pflegezustand:** Die Crates sind aktuell (openh264 0.9.8 im Lockfile). Eine vertiefte Prüfung wurde nicht durchgeführt [H].

#### A-08 Rechteck-Semantik: vollständig geprüft

- **Kategorie:** Spec-Konformität. **Schwere:** – (Stärke). **Status:** F.
- **Beleg:** Alle `RDPGFX_RECT16`-Felder sind `ExclusiveRectangle`:
  - `cmd.rs:322` (WireToSurface1)
  - `:525` (SolidFill)
  - `:589` (SurfaceToSurface)
  - `:657` (SurfaceToCache)
  - `avc.rs:81` (Regionen)

  Server- und Client-Code arbeiten konsistent exklusiv. Spec 2.2.1.2: „exclusive coordinates“.
- **Rest:** `ExclusiveRectangle::decode` prüft `left ≤ right` nicht (`geometry.rs:250-265`). Der EGFX-Client prüft das selbst (`client.rs:819-830`).

---

## 5. Messergebnisse (B6)

Methodik:
- 11 synthetische Bilder à 320×128:
  - Text in `#FF0000`, `#0000FF`, `#800000`, `#000080`, `#00FF00` und `#FFFFFF` auf Schwarz
  - 1-px-Linien
  - Chroma-Schachbrett mit luma-gleichen Farben
  - ClearType-artiger Subpixel-Text
  - Verlauf
  - Vollton-Kacheln
- Metriken:
  - PSNR getrennt für Y, U und V (full-range BT.709, Gleitkomma)
  - SSIM
  - CIEDE2000
  - Kantenmaske: Sobel > 48, dilatiert. Darauf mittleres ΔE00, mittlerer Chroma-Fehler |ΔU,ΔV| und Kantenschärfe (Gradientensumme dekodiert/Original).
- Codecs: mit IronRDP-eigenem Encoder und Decoder.
- AVC:
  - Spec-Modell nach MS-RDPEGFX, libx264 High Profile, konstanter QP, kein B-Frame, Main- und Aux-View als ein Stream.
  - Das Spec-Modell ist bit-identisch zu FreeRDPs Server-Split (T7).
  - libx264 kodiert die Main View mit QP q−3 (Standard-`ipratio`), die Aux View mit q. T9 setzt die QPs pro View explizit.
- Differenzbilder (|Fehler|×4, plus 4×-Zoom Original | dekodiert | Differenz): [`chroma/results/diff/`](chroma/results/diff/).

**Auszug roter Text `#FF0000` (vollständig in T1 bis T8):**

| Pfad | PSNR Y | PSNR U | PSNR V | SSIM | ΔE00 Kante | Chroma-Fehler Kante | max. Fehler | Bytes |
|---|---|---|---|---|---|---|---|---|
| ClearCodec / Planar | ∞ | ∞ | ∞ | 1,0 | 0 | 0 | 0 | 25 972 / 10 952 |
| NSCodec CLL 1 / 3 / 7 | 74,2 / 64,4 / 37,9 | 79,5 / 52,4 / 26,6 | 66,7 / 56,3 / 30,5 | 1,0 / 0,98 / 0,85 | 0,04 / 0,31 / 7,40 | 0,09 / 0,70 / 14,8 | 1 / 5 / 95 | ≈ 26 900 |
| RFX Default-Quant | 43,1 | 43,2 | 39,0 | 0,85 | 2,65 | 3,97 | 63 | 21 491 |
| RFX Default-Quant, gerundet (Was-wäre-wenn) | 45,1 | 46,9 | 44,6 | 0,91 | 1,37 | 2,12 | 33 | 19 984 |
| Progressive q6 / q6+RE / Default | 48,7 / 50,1 / 44,8 | 54,0 / 53,6 / 49,5 | 41,6 / 51,1 / 41,0 | 0,98 / 0,96 / 0,94 | 1,40 / 0,71 / 1,61 | 2,41 / 1,09 / 2,66 | 26 / 11 / 34 | 30 818 / 30 125 / 19 949 |
| **AVC420 IronRDP heute (openh264)** | **23,4** | 36,5 | **23,1** | **0,12** | 10,3 | 21,9 | 141 | 8 661 |
| AVC420 nur Farbkonvertierung, BT.709 full | 36,0 | 37,1 | 24,1 | 0,76 | 8,18 | 18,1 | 151 | – |
| AVC420 Spec-Modell QP 22 | 35,9 | 36,9 | 24,0 | 0,75 | 8,52 | 18,5 | 162 | 10 057 |
| AVC444 v1, QP 22, Schwelle 30 | 47,2 | 43,9 | 39,4 | 0,91 | 1,61 | 2,83 | 80 | 20 105 |
| AVC444v2, QP 22, Schwelle 30 | 47,1 | 44,0 | 39,0 | 0,91 | 1,63 | 2,86 | 114 | 19 550 |

**Weitere Kernergebnisse:**
- Fehlerverstärkung (T4): Median 4,1 bis 5,0.
- AVC444 bei gleicher Bitrate klar besser (T5).
- Schwellwert 30 ohne Kompression: 13 dB Verlust in PSNR V (T3).
- Niedrigerer Main-QP gegen Client-Filter (T9): Bei gleicher Bitrate bringt ein niedrigerer Main-QP nichts gegenüber einem globalen QP. Ein QP-adaptiver Rückfilter im Client bringt Ø +1,8 dB Chroma-PSNR ohne Mehrbytes.
- DWT 8 Bit: 15 Stufen (T8).
- FreeRDP-Gegenprobe (T7):
  - Split bit-identisch.
  - Generische Kombination identisch bis auf den Grenzfall |d| = 30.
  - SIMD-v1 fehlerhaft.
  - IronRDPs NSCodec-, ClearCodec- und Planar-Streams dekodieren in FreeRDP identisch.
- Heuristik-Probe (B5): siehe Abschnitt 5.1.

**Nicht durchgeführt:**
- Gegenprobe mit echten AVC-Captures von Windows Server 2025: Es liegen keine AVC-Fixtures vor. Für Progressive gibt es einen Decoder-Vergleich mit einem Windows-Capture (C-07, C-16).
- Dekodierung derselben Streams mit einem Microsoft-Client: nicht verfügbar.
- Klassisches RFX durch FreeRDP: Das Harness erzeugt nur Kachel-Komponenten, keine vollständige RFX-Nachricht.

### 5.1 Machbarkeit der Routing-Heuristik (B5, nicht implementiert)

- **Andockpunkt:**
  - Ein Trait (z. B. `ContentClassifier`) in `ironrdp-server` bzw. beim Embedder vor `send_mixed_frame`.
  - Eingabe: Dirty-Rects plus Frame.
  - Ausgabe: je Rechteck ein Codec. Farbiger Text geht auf ClearCodec, der Rest auf AVC444 oder AVC420.
  - Im Legacy-Pfad käme ein Hook in `UpdateEncoder::update` (`encoder/mod.rs:229`) mit Codec-Wahl pro Rechteck in Frage. Dafür müsste die Codec-Wahl von „pro Sitzung“ auf „pro Rechteck“ umgestellt werden.
- **Signal pro 16×16-Block:** Luma-Spanne ≤ 160 und mittlere Chroma-Gradientenenergie ≥ 12, gerechnet in der Spec-Integer-Matrix.
- **Treffer auf den Testbildern:**
  - roter, blauer und dunkler Text: 73 bis 90 von 160 Blöcken (Textfläche)
  - weißer und grüner Text sowie ClearType: 0
  - Schachbrett: 160
  - Vollton: 28 (Kachelkanten)
- **Kosten:** naiv, skalar, ein Thread, ohne Dirty-Rect-Einschränkung **14,8 ms pro 1920×1088-Frame** (Xeon, 2,1 GHz). Mit Beschränkung auf Dirty-Rects, einmaliger Konvertierung und SIMD sollten es deutlich unter 2 ms sein [H].
- **Offen:** ClearType-Farbsäume erkennt die Heuristik nicht (große Luma-Spanne). Ob sie ClearCodec brauchen, ist offen.

---

## 6. Offene Fragen (nur mit Captures oder Microsoft-Clients klärbar)

1. Wendet mstsc bzw. Windows App den AVC444-Rückfilter an, mit welcher Schwelle, und auf welcher Seite der Grenze bei |d| = 30?
2. Legt der Windows-Server Ũ (Mittel) oder ein Punkt-Sample in die Main-View-Chroma? Welchen QP-Offset nutzt er für die Aux View, und nutzt er LC 1/2 für statischen Text?
3. Sind Windows-AVC420-Frames surface-groß mit `destRect` als Bounding-Box (Voraussetzung für die Auswirkung von C-06), und wie oft ist `destRect ≠ (0,0)`?
4. Welche NSCodec-CLL kündigen mstsc und Windows App an? Akzeptieren sie jederzeit CLL 1?
5. Wie reagieren Microsoft-Clients auf LC 2 mit `cb ≠ 0` (heutiges IronRDP) und auf überlappende AVC- und ClearCodec-Kacheln innerhalb eines Frames (Reihenfolge, Region-Masken)?
6. Wertet der Windows-Server `AVC_THINCLIENT` aus, und bevorzugt er v1 oder v2?
7. Nutzen Microsoft-Decoder die Integer-Matrix mit Floor? Das bestimmt, ob Rundung im Encoder (C-13) Kanten zu ClearCodec verringert.
8. Tritt die FreeRDP-SIMD-v1-Abweichung (C-14) auch in echten Sitzungen auf?

## 7. Empfehlung: drei Änderungen mit dem größten Effekt auf farbigen Text

1. **AVC-Farbpfad reparieren und AVC444 ermöglichen (C-01, C-02, C-03, C-06, C-12).**
   - Zuerst #1976 übernehmen und chromatische Tests ergänzen.
   - Dann einen planaren Eingang für `H264Encoder` und `split_v1/v2`-Helfer schaffen, die bit-identisch zu FreeRDP sind. Referenz sind das Spec-Modell und die Gegenprobe in `analysis/chroma`.
   - Im Client einen `decode_yuv420`-Pfad mit Kombination ergänzen und die Region-Rects anwenden. Für die Kombination einen QP-adaptiven Rückfilter statt der festen Schwelle 30 verwenden (C-04, T9: Ø +1,8 dB Chroma-PSNR ohne Mehrbytes).
   - **Effekt (gemessen):** Kanten-Chroma-Fehler bei rotem Text bei gleichem QP 18,5 → 2,8, bei gleicher Bitrate 18,2 → 5,8. Der heutige Pfad liegt bei 21,9 und SSIM 0,12.
   - Aufwand: M bis L.
2. **Farbigen Text per Routing auf ClearCodec legen (C-10, C-15).**
   - Klassifikations-Hook im Server einführen.
   - Mixed Frames mit dokumentierter Reihenfolge bauen und AVC-Region-Rects um ClearCodec-Kacheln kürzen.
   - Die Heuristik „Chroma-Energie hoch, Luma-Spanne klein“ kostet naiv 15 ms pro 1080p-Frame, eingeschränkt vermutlich < 2 ms.
   - **Effekt:** ΔE00 = 0 für farbigen Text. Das ist besser als Windows, das in ClearCodec NSCodec mit CLL 3 nutzt.
   - Aufwand: M.
3. **Präzisionsfehler der Tile-Codecs beheben (C-07, C-08, C-09, C-13).**
   - RFX gerundet quantisieren: +5,6 dB PSNR V und −7 % bis −27 % Bytes.
   - Progressive auf 11.5-Festkomma umstellen: DWT-Fehler 15 → < 0,5 Stufen, Voraussetzung für verlustfreie Upgrade-Passes.
   - NSCodec immer mit CLL 1 codieren: max. Fehler 6 → 1.
   - Beim RGB→YUV-Schritt runden.
   - **Effekt:** kleinere Kanten in Mixed Frames und keine vermeidbaren Chroma-Verluste auf den Nicht-AVC-Pfaden.
   - Aufwand: je S bis M.
