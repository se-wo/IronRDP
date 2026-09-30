#!/usr/bin/env python3
"""T9: lower QP for the AVC444 main view x client reverse-filter variants.

Question (REVIEW.md C-04): does a server that encodes the main view (and so
U~, the 2x2 chroma mean) more finely help every client, and does a client
reverse filter that adapts its threshold to the bitstream QP add to that?

Server configurations (all spec conformant; U~ stays the 2x2 mean):

* ``d0``       main QP q, aux QP q (reference)
* ``d-3``      main QP q-3, aux QP q (what libx264's default ipratio 1.4
               does in avc_experiment.py, i.e. the setup behind T2/T3)
* ``d-6``      main QP q-6, aux QP q
* ``d-12``     main QP q-12, aux QP q
* ``c-6``      main and aux QP q, chroma_qp_index_offset -6 in the PPS
* ``c-12``     main and aux QP q, chroma_qp_index_offset -12 in the PPS

x264 always lowers the requested chroma offset by 2 when psy-rd is on
(medium preset), so ``d*`` streams carry chroma_qp_index_offset -2 and the
``c*`` streams request -4 / -10.  The effective values are read back from
the PPS.  The chroma offset is global (one encoder for both views, as in
avc_experiment.py), so ``c*`` also refines the aux view's chroma planes; the
aux luma planes are unaffected.

Client variants at the (2x,2y) sites, d = U~ - (4*U~ - three aux samples):

* ``none``      no reverse filter, U~ stays
* ``spec30``    MS-RDPEGFX 3.3.8.3.2: reconstruct if |d| > 30
* ``freerdp30`` FreeRDP CONDITIONAL_CLIP: reconstruct if |d| >= 30
* ``always``    always reconstruct (no threshold)
* ``adaptive``  reconstruct if |d| > k * sqrt(16 s(QPc_main)^2 + 2 s(QP_aux)^2
                + s(QPc_aux)^2), s = H.264 quantiser step size.  Only values
                a client can parse from the bitstream are used: slice QP
                (pic_init_qp + slice_qp_delta) and chroma_qp_index_offset.
                k is calibrated over all images and configurations (the
                sweep is written out, see T9d).

For an equal-bitrate view, ``d0`` is additionally encoded at every QP from
REF_QPS; make_tables.py interpolates that rate-distortion curve.

Writes results/avc_main_qp_filter.csv (full metrics, main QPs),
results/avc_main_qp_filter_rd.csv (chroma PSNR and bytes, every encode and
every filter incl. the k sweep).
"""

import csv
import math
import re
import subprocess
import sys
from concurrent.futures import ProcessPoolExecutor
from pathlib import Path

import numpy as np
from scipy import ndimage
from skimage.color import deltaE_ciede2000, rgb2lab
from skimage.metrics import structural_similarity

import avc_experiment as avc
from metrics import psnr, sobel_mag, to_ycbcr709

ROOT = Path(__file__).resolve().parent
IMAGES = ROOT / "images"
RESULTS = ROOT / "results"
WORK = RESULTS / "work" / "main_qp_filter"

QPS = [18, 22, 26, 30, 34]
REF_QPS = list(range(12, 41))
# name -> (main QP delta, requested x264 chroma offset)
CONFIGS = {
    "d0": (0, 0),
    "d-3": (-3, 0),
    "d-6": (-6, 0),
    "d-12": (-12, 0),
    "c-6": (0, -4),
    "c-12": (0, -10),
}
K_SWEEP = [0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.4, 0.5, 0.75, 1.0]
FIXED = ["none", "spec30", "freerdp30", "always"]

# H.264 Table 8-15: QPc for qPI >= 30
_QPC_HI = [29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39]


def qpc(qp: int, offset: int) -> int:
    qpi = min(max(qp + offset, 0), 51)
    return qpi if qpi < 30 else _QPC_HI[qpi - 30]


def qstep(qp: int) -> float:
    return [0.625, 0.6875, 0.8125, 0.875, 1.0, 1.125][qp % 6] * 2 ** (qp // 6)


def encode(main, aux, w, h, qp_main, qp_aux, chroma_req, work: Path):
    """One x264 instance, two frames (main I, aux P), forced per-frame QP."""
    work.mkdir(parents=True, exist_ok=True)
    raw, es, qpf = work / "in.yuv", work / "out.h264", work / "qp.txt"
    frames_in = avc.to_i420(*main) + avc.to_i420(*aux)
    qps = f"0 I {qp_main}\n1 P {qp_aux}\n"
    cmd = [
        "x264", "--quiet", "--profile", "high", "--preset", "medium", "--qp", str(qp_aux),
        "--bframes", "0", "--keyint", "1000", "--scenecut", "0", "--ref", "1",
        # CQP clamps forced QPs to [min, max] of the default I/P/B QPs; ipratio 4 widens that to q-12.
        "--ipratio", "4.0", "--qpfile", str(qpf), "--chroma-qp-offset", str(chroma_req),
        "--demuxer", "raw", "--input-csp", "i420", "--input-res", f"{w}x{h}", "--fps", "30",
        "-o", str(es), str(raw),
    ]
    # Reuse a stream from an earlier run only if input, QPs and command line are unchanged.
    stamp = work / "cmd.txt"
    fresh = not (es.exists() and raw.exists() and raw.read_bytes() == frames_in and qpf.exists()
                 and qpf.read_text() == qps and stamp.exists() and stamp.read_text() == " ".join(cmd))
    if fresh:
        raw.write_bytes(frames_in)
        qpf.write_text(qps)
        subprocess.run(cmd, check=True, capture_output=True)
        stamp.write_text(" ".join(cmd))
    dec = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(es), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"],
        check=True, capture_output=True,
    ).stdout
    fsz = w * h * 3 // 2
    assert len(dec) == 2 * fsz
    sizes = [int(s) for s in subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "packet=size", "-of", "csv=p=0", str(es)],
        check=True, capture_output=True, text=True,
    ).stdout.split()]
    # What a client can parse: PPS chroma offset and slice QPs.
    trace = subprocess.run(
        ["ffmpeg", "-hide_banner", "-i", str(es), "-c", "copy", "-bsf:v", "trace_headers", "-f", "null", "-"],
        check=True, capture_output=True, text=True,
    ).stderr
    init = int(re.search(r"pic_init_qp_minus26\s+\d+ = (-?\d+)", trace).group(1)) + 26
    coff = int(re.search(r" chroma_qp_index_offset\s+\d+ = (-?\d+)", trace).group(1))
    deltas = [int(x) for x in re.findall(r"slice_qp_delta\s+\d+ = (-?\d+)", trace)]
    slice_qps = [init + d for d in deltas]
    assert slice_qps == [qp_main, qp_aux], (slice_qps, qp_main, qp_aux)
    frames = [avc.from_i420(dec[i * fsz : (i + 1) * fsz], w, h) for i in range(2)]
    return frames, sizes, coff


def reconstruct(m, a, version, rule, tau=None):
    y, u, v = avc.combine(m, a, version, "nofilter")  # (2x,2y) still holds U~
    for p, mean in ((u, m[1]), (v, m[2])):
        rec = np.clip(4 * mean - p[0::2, 1::2] - p[1::2, 0::2] - p[1::2, 1::2], 0, 255)
        d = np.abs(mean - rec)
        if rule == "none":
            continue
        if rule == "spec30":
            use = d > 30
        elif rule == "freerdp30":
            use = d >= 30
        elif rule == "always":
            use = np.ones_like(d, bool)
        else:
            use = d > tau
        p[0::2, 0::2] = np.where(use, rec, mean)
    return avc.yuv444_to_rgb(y, u, v)


def chroma_psnr(yuv_ref, rgb):
    yuv = to_ycbcr709(rgb)
    mse = (np.mean((yuv[1] - yuv_ref[1]) ** 2) + np.mean((yuv[2] - yuv_ref[2]) ** 2)) / 2
    return math.inf if mse < 1e-9 else 10 * math.log10(255.0**2 / mse)


def encode_image(name):
    """All encodes of one image (runs in a worker process)."""
    w, h = map(int, (IMAGES / f"{name}.txt").read_text().split())
    ref = np.frombuffer((IMAGES / f"{name}.rgba").read_bytes(), np.uint8).reshape(h, w, 4)[..., :3]
    mag = np.max(np.stack([sobel_mag(ref[..., c]) for c in range(3)]), axis=0)
    mask = ndimage.binary_dilation(mag > 48, iterations=1)
    y, u, v = avc.rgb_to_yuv444(ref)
    main_avg = avc.main_view(y, u, v)
    encodes = []
    for ver, aux in ((1, avc.aux_view_v1(u, v)), (2, avc.aux_view_v2(u, v))):
        jobs = [(c, q) for c in CONFIGS for q in QPS] + [("d0", q) for q in REF_QPS if q not in QPS]
        for cfg, qp in jobs:
            dq, creq = CONFIGS[cfg]
            qm = qp + dq
            (m, a), sizes, coff = encode(main_avg, aux, w, h, qm, qp, creq, WORK / f"{name}_v{ver}_{cfg}_qp{qp}")
            # Expected std of the reconstruction error, in quantiser-step units (see module doc).
            tau_unit = math.sqrt(16 * qstep(qpc(qm, coff)) ** 2 + 2 * qstep(qp) ** 2 + qstep(qpc(qp, coff)) ** 2)
            encodes.append(dict(image=name, version=ver, config=cfg, qp=qp, qp_main=qm, qp_aux=qp,
                                chroma_qp_offset=coff, bytes_main=sizes[0], bytes_aux=sizes[1],
                                tau_unit=round(tau_unit, 3), planes=(m, a)))
    print(f"{name}: encoded", file=sys.stderr, flush=True)
    return name, (ref, mask, rgb2lab(ref / 255.0), to_ycbcr709(ref)), encodes


def main():
    names = sorted(p.stem for p in IMAGES.glob("*.rgba"))
    encodes = []
    refs = {}
    with ProcessPoolExecutor() as pool:
        for name, r, es in pool.map(encode_image, names):
            refs[name] = r
            encodes += es

    # Pass 1: chroma PSNR for every encode, fixed rules and the k sweep.
    rd_rows = []
    for e in encodes:
        _, _, _, yuv_ref = refs[e["image"]]
        m, a = e["planes"]
        base = {k: v for k, v in e.items() if k != "planes"}
        for rule in FIXED:
            rd_rows.append(dict(base, filter=rule, k="", tau="", chroma_psnr=round(chroma_psnr(yuv_ref, reconstruct(m, a, e["version"], rule)), 3)))
        for k in K_SWEEP:
            tau = k * e["tau_unit"]
            rgb = reconstruct(m, a, e["version"], "adaptive", tau)
            rd_rows.append(dict(base, filter="adaptive", k=k, tau=round(tau, 2), chroma_psnr=round(chroma_psnr(yuv_ref, rgb), 3)))

    # Calibrate k: best mean chroma PSNR over all images, versions, configurations and main QPs.
    def score(k):
        xs = [r["chroma_psnr"] for r in rd_rows if r["filter"] == "adaptive" and r["k"] == k and r["qp"] in QPS]
        return float(np.mean([min(x, 99.0) for x in xs]))

    k_best = max(K_SWEEP, key=score)
    print(f"k = {k_best}", file=sys.stderr)

    # Pass 2: full metrics for the main QPs.
    rows = []
    for e in encodes:
        if e["qp"] not in QPS:
            continue
        ref, mask, lab_ref, yuv_ref = refs[e["image"]]
        m, a = e["planes"]
        base = {k: v for k, v in e.items() if k != "planes"}
        for rule in FIXED + ["adaptive"]:
            tau = k_best * e["tau_unit"] if rule == "adaptive" else None
            dec = reconstruct(m, a, e["version"], rule, tau)
            yuv_dec = to_ycbcr709(dec)
            de = deltaE_ciede2000(lab_ref, rgb2lab(dec / 255.0))
            chroma_err = np.hypot(yuv_dec[1] - yuv_ref[1], yuv_dec[2] - yuv_ref[2])
            rows.append(dict(
                base, filter=rule, k=k_best if rule == "adaptive" else "",
                psnr_u=round(psnr(yuv_ref[1], yuv_dec[1]), 2),
                psnr_v=round(psnr(yuv_ref[2], yuv_dec[2]), 2),
                chroma_psnr=round(chroma_psnr(yuv_ref, dec), 3),
                ssim_rgb=round(float(structural_similarity(ref, dec, channel_axis=2, data_range=255)), 4),
                edge_de=round(float(de[mask].mean()), 3),
                edge_chroma_err=round(float(chroma_err[mask].mean()), 3),
                max_abs_err=int(np.abs(ref.astype(int) - dec.astype(int)).max()),
            ))

    for path, data in ((RESULTS / "avc_main_qp_filter.csv", rows), (RESULTS / "avc_main_qp_filter_rd.csv", rd_rows)):
        with open(path, "w", newline="") as fh:
            wr = csv.DictWriter(fh, fieldnames=list(data[0].keys()))
            wr.writeheader()
            wr.writerows(data)
    print(f"{len(rows)} + {len(rd_rows)} rows written", file=sys.stderr)


if __name__ == "__main__":
    main()
