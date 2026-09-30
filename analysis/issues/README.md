# Entwürfe für Upstream-Issues (Devolutions/IronRDP)

Stand der Prüfung: upstream `master` = `8d91a2cc` (28.09.2026). Die Duplikatsuche habe ich zweimal mit unterschiedlichen Begriffen gemacht, ohne Treffer.

| Datei | Befund | Beleg | offen |
|---|---|---|---|
| `01-progressive-dwt-flag.md` | C-16 | Spec-Text, FreeRDP, Test mit Windows-Capture | welcher Server beide Bits verschieden setzt |
| `02-avc420-region-rects.md` | C-06 | Spec-MUSTs, FreeRDP, End-to-End-Test IronRDP-Server → -Client | Häufigkeit bei Windows-Servern (kein AVC-Capture) |
| `03-avc420-qp-panic.md` | C-12 | Test (Panic reproduziert) | – |
| `04-rfx-quant-rounding.md` | C-08 | Spec-Text, FreeRDP, Unit-Test, Messung | – |

Die Code-Schnipsel aus 01 und 03 laufen 1:1 als `analysis/chroma/harness/tests/issue_snippets.rs`, der aus 04 als `c08_rfx_quantization_rounding.rs`. Alle schlagen auf dem aktuellen Stand wie beschrieben fehl.

Aufbau jedes Entwurfs: *What goes wrong* (ohne Fachbegriffe, was man sieht) → *Why* → *How to reproduce* → *References* (Spec, Code, FreeRDP) → *Suggested fix*.

## Vor dem Einreichen

1. `git fetch upstream` und prüfen, ob sich die verlinkten Stellen geändert haben (Permalinks zeigen auf `8d91a2cc`).
2. Noch einmal selbst in Issues **und** PRs suchen, auch in geschlossenen.
3. Die Schnipsel selbst laufen lassen (`cd analysis/chroma/harness && cargo test --release --test issue_snippets --test c08_rfx_quantization_rounding`).
4. Die Links in 02 und 04 zeigen auf `se-wo/IronRDP` Branch `analysis/chroma`; der Branch muss öffentlich erreichbar sein. Wenn du keine Fork-Links willst, die Links streichen, die Tabellen reichen als Beleg.
5. Die Fußzeile „Human-reviewed, LLM-assisted content“ nur stehen lassen, wenn du den Text tatsächlich geprüft hast; sonst ehrlich anpassen.
6. Bei 01 das Bild `01-screenshot.png` anhängen (links IronRDP, Mitte FreeRDP, rechts Differenz ×8; derselbe Windows-Stream mit CONTEXT-Bit 0). Ohne Bild die Zeile „*(Screenshot: …)*“ streichen.
7. Einzeln einreichen, nicht als Sammel-Issue. Bei 03 wäre ein kleiner PR mit Validierung eventuell passender als ein Issue.
