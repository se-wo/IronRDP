# Chroma-Messwerkzeuge (Analyse, Phase 1)

Alles hier ist Analyse-Tooling. Kein Teil davon ist Workspace-Mitglied oder wird von `cargo xtask` gebaut.
Der Bericht dazu liegt in [`../REVIEW.md`](../REVIEW.md).

| Datei | Zweck |
|---|---|
| `gen_images.py` | synthetische Testbilder (320×128) → `images/*.png` + `*.rgba` |
| `harness/` | eigenständiges Cargo-Projekt (eigener `[workspace]`): Round-Trips durch IronRDP-Encoder und -Decoder (RFX, Progressive, NSCodec, ClearCodec, Planar, AVC420/openh264), DWT-Reversibilität, Heuristik-Kostenprobe |
| `avc_experiment.py` | AVC420 / AVC444 v1 / AVC444v2 nach MS-RDPEGFX 3.3.8.3, libx264 mit konstantem QP, drei Client-Kombinationsvarianten |
| `freerdp_check.c` | kleines C-Programm gegen libfreerdp3 (FreeRDP 3.32.2): AVC444-Split/-Combine, NSCodec/ClearCodec/Planar-Decode |
| `freerdp_crosscheck.py` | treibt `freerdp_check` und vergleicht mit Spec-Modell und IronRDP-Decodern |
| `inspect_windows_fixtures.py` | liest Windows-Server-2025-Captures (Haven-Fixtures im Testsuite-Crate) aus: NSCodec-CLL, Progressive-Quant-Tabellen |
| `metrics.py` | PSNR Y/U/V, SSIM, CIEDE2000, Kantenmaße; Differenzbilder |
| `make_tables.py` | rendert `results/tables.md` |

## Reproduktion

```sh
pip install numpy pillow scikit-image          # scipy kommt mit scikit-image
apt-get install ffmpeg                          # libx264 + H.264-Decoder
cd analysis/chroma
python3 gen_images.py
(cd harness && cargo run --release -- ..)       # braucht einen C-Compiler (openh264-bundled)
python3 avc_experiment.py
# FreeRDP 3.x mit -DWITH_UNICODE_BUILTIN=ON bauen, dann:
gcc -O2 -o freerdp_check freerdp_check.c -I<freerdp>/include -I<build>/include \
    -I<freerdp>/winpr/include -I<build>/winpr/include -L<build>/libfreerdp -L<build>/winpr/libwinpr \
    -lfreerdp3 -lwinpr3
FREERDP_CHECK=./freerdp_check python3 freerdp_crosscheck.py
python3 inspect_windows_fixtures.py > results/windows_fixtures.txt
python3 metrics.py
python3 make_tables.py
```

Große Zwischenergebnisse (`results/decoded`, `results/streams`, `results/avc_planes`, `results/work`, `images/*.rgba`)
sind per `.gitignore` ausgeschlossen und werden von den Skripten neu erzeugt.
Versioniert sind die CSVs, `results/tables.md` und die Differenzbilder unter `results/diff/`.
