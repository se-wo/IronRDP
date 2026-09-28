#!/usr/bin/env python3
"""Render the measurement CSVs into Markdown tables (results/tables.md) that
analysis/REVIEW.md quotes.  Run after harness, avc_experiment.py,
freerdp_crosscheck.py and metrics.py."""

import csv
from collections import defaultdict
from pathlib import Path

import numpy as np
from skimage.color import deltaE_ciede2000, rgb2lab

ROOT = Path(__file__).resolve().parent
RES = ROOT / "results"

metrics = list(csv.DictReader(open(RES / "metrics.csv")))
M = {(r["variant"], r["image"]): r for r in metrics}
sizes = {(r["codec"], r["image"]): int(r["encoded_bytes"]) for r in csv.DictReader(open(RES / "sizes.csv"))}
avc_sizes = {(r["variant"], r["image"], int(r["qp"])): int(r["bytes"]) for r in csv.DictReader(open(RES / "avc_sizes.csv"))}
out = []


def table(header, rows):
    out.append("| " + " | ".join(header) + " |")
    out.append("|" + "|".join("---" for _ in header) + "|")
    for r in rows:
        out.append("| " + " | ".join(str(c) for c in r) + " |")
    out.append("")


def f(v):
    return "∞" if v == "inf" else v


COLS = ["psnr_y", "psnr_u", "psnr_v", "ssim_rgb", "de2000_mean", "edge_de", "edge_chroma_err", "edge_sharpness", "max_abs_err"]
HDR = ["PSNR Y", "PSNR U", "PSNR V", "SSIM", "ΔE00 Ø", "ΔE00 Kante", "Chroma-Fehler Kante", "Kantenschärfe", "max |Δ|"]

# --- T1: IronRDP codec round trips ---------------------------------------
out.append("### T1 Round-Trips mit IronRDP-Encoder und -Decoder\n")
codecs = [
    "clearcodec", "clearcodec_glyph_tiles", "planar", "nscodec_cll1", "nscodec_cll3", "nscodec_cll7",
    "rfx_quant_6", "rfx_quant_default", "whatif_rfx_quant_default_rounded",
    "progressive_simple_q6", "progressive_simple_q6_re", "progressive_simple_default",
    "avc420_colorconv_only_current", "avc420_colorconv_only_bt709full", "avc420_ironrdp_openh264",
]
for img in ["text_ff0000", "text_0000ff", "text_000080", "text_ffffff"]:
    out.append(f"**{img}**\n")
    rows = []
    for c in codecs:
        r = M.get((c, img))
        if not r:
            continue
        rows.append([c] + [f(r[k]) for k in COLS] + [sizes.get((c, img), "")])
    table(["Pfad"] + HDR + ["Bytes"], rows)

# --- T2: AVC420 vs AVC444 vs AVC444v2 over QP ------------------------------
out.append("### T2 AVC420 / AVC444 v1 / AVC444v2 (Spec-Modell, libx264, Rückfilter mit Schwelle 30)\n")
for img in ["text_ff0000", "text_0000ff", "text_000080", "cleartype_text", "text_ffffff"]:
    out.append(f"**{img}**\n")
    rows = []
    for qp in [18, 22, 26, 30, 34]:
        for label, var, sv in (("AVC420", f"avc420_qp{qp}", "avc420"), ("AVC444 v1", f"avc444v1_filter_t30_qp{qp}", "avc444v1"),
                               ("AVC444v2", f"avc444v2_filter_t30_qp{qp}", "avc444v2")):
            r = M[(var, img)]
            rows.append([qp, label, r["psnr_u"], r["psnr_v"], r["ssim_rgb"], r["edge_de"], r["edge_chroma_err"], r["edge_sharpness"],
                         avc_sizes[(sv, img, qp)]])
    table(["QP", "Modus", "PSNR U", "PSNR V", "SSIM", "ΔE00 Kante", "Chroma-Fehler Kante", "Kantenschärfe", "Bytes"], rows)

# --- T3: combination variants ----------------------------------------------
out.append("### T3 Kombinationsvarianten AVC444 v1 (text_ff0000 / text_0000ff)\n")
rows = []
for tag in ["noenc", "qp22", "qp30"]:
    for mode, label in (("nofilter", "kein Rückfilter (Ũ bleibt)"), ("filter", "Rückfilter ohne Schwelle"),
                        ("filter_t30", "Rückfilter, Schwelle 30 (Spec)"), ("pointsample", "Was-wäre-wenn: Punktabtastung")):
        var = f"avc444v1_{mode}_{tag}"
        if (var, "text_ff0000") not in M:
            continue
        a, b = M[(var, "text_ff0000")], M[(var, "text_0000ff")]
        rows.append([tag, label, a["psnr_v"], a["edge_chroma_err"], a["max_abs_err"], b["psnr_u"], b["edge_chroma_err"], b["max_abs_err"]])
table(["Transport", "Client-Kombination", "rot: PSNR V", "rot: Chroma-Fehler Kante", "rot: max |Δ|",
       "blau: PSNR U", "blau: Chroma-Fehler Kante", "blau: max |Δ|"], rows)

# --- T4: error amplification ------------------------------------------------
amp = list(csv.DictReader(open(RES / "avc_error_amplification.csv")))
agg = defaultdict(list)
for r in amp:
    if r["ratio"]:
        agg[(r["version"], int(r["qp"]))].append(float(r["ratio"]))
out.append("### T4 Fehlerverstärkung des Rückfilters an den (2x,2y)-Samples\n")
out.append("RMS-Fehler des rekonstruierten U/V(2x,2y) geteilt durch den RMS-Codierfehler von Ũ in der Main View, über alle Bilder und beide Komponenten.\n")
table(["Version", "QP", "Median", "Min", "Max"],
      [[f"v{v}", qp, round(float(np.median(x)), 2), min(x), max(x)] for (v, qp), x in sorted(agg.items())])

# --- T5: equal bitrate ------------------------------------------------------
out.append("### T5 Gleiche Bitrate: AVC420 bei QP q vs. AVC444 v1 bei QP q' mit ähnlicher Größe\n")
rows = []
for img in ["text_ff0000", "text_0000ff", "cleartype_text"]:
    for qp420 in [18, 22]:
        b420 = avc_sizes[("avc420", img, qp420)]
        best = min([18, 22, 26, 30, 34], key=lambda q: abs(avc_sizes[("avc444v1", img, q)] - b420))
        r420, r444 = M[(f"avc420_qp{qp420}", img)], M[(f"avc444v1_filter_t30_qp{best}", img)]
        rows.append([img, f"AVC420 QP{qp420}: {b420} B", r420["edge_chroma_err"], r420["edge_de"], r420["ssim_rgb"],
                     f"AVC444 QP{best}: {avc_sizes[('avc444v1', img, best)]} B", r444["edge_chroma_err"], r444["edge_de"], r444["ssim_rgb"]])
table(["Bild", "AVC420", "Chroma-Fehler Kante", "ΔE00 Kante", "SSIM", "AVC444 v1", "Chroma-Fehler Kante", "ΔE00 Kante", "SSIM"], rows)

# --- T6: solid colours across codecs ---------------------------------------
sc = list(csv.reader(open(RES / "solid_colors.csv")))
cols = sc[0][1:]
out.append("### T6 Volltonfarben über alle Pfade (Mitte je 64×64-Kachel, dekodiert, hex RGB; ΔE00 zum Original in Klammern)\n")
rows = []
for row in sc[1:]:
    cells = []
    for c, got in zip(cols, row[1:]):
        ref = np.array([[[int(c[i : i + 2], 16) for i in (0, 2, 4)]]]) / 255.0
        dec = np.array([[[int(got[i : i + 2], 16) for i in (0, 2, 4)]]]) / 255.0
        de = float(deltaE_ciede2000(rgb2lab(ref), rgb2lab(dec))[0, 0])
        cells.append(f"{got} ({de:.1f})")
    rows.append([row[0]] + cells)
table(["Pfad"] + cols, rows)

# --- T7: FreeRDP cross-check -------------------------------------------------
fc = list(csv.DictReader(open(RES / "freerdp_crosscheck.csv")))
agg = defaultdict(list)
for r in fc:
    agg[r["check"]].append(r)
out.append("### T7 Gegenprobe FreeRDP 3.32.2 (über alle 11 Testbilder)\n")
rows = []
for k, v in agg.items():
    mx = [r["max_abs_diff"] for r in v]
    pct = [float(r["pct_samples_diff"]) for r in v if r["pct_samples_diff"]]
    rows.append([k, max(mx, key=lambda s: int(s) if s.isdigit() else 10**9),
                 round(sum(pct) / len(pct), 3) if pct else "", max(pct) if pct else ""])
table(["Prüfung", "max |Δ|", "Ø % Samples verschieden", "max % Samples verschieden"], rows)

# --- T8: DWT ----------------------------------------------------------------
dw = list(csv.DictReader(open(RES / "dwt_reversibility.csv")))
agg = defaultdict(list)
for r in dw:
    agg[r["path"]].append(float(r["max_err_8bit_units"]))
out.append("### T8 DWT hin und zurück ohne Quantisierung (max. Fehler in 8-Bit-Stufen, über alle Bilder)\n")
table(["Pfad", "max", "Median der Bild-Maxima"], [[k, max(v), float(np.median(v))] for k, v in agg.items()])

(RES / "tables.md").write_text("\n".join(out) + "\n")
print("results/tables.md written")
