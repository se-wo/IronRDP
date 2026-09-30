#!/usr/bin/env python3
"""AVC420 vs. AVC444 (v1) vs. AVC444v2 round trips with a reference H.264 encoder.

IronRDP has neither a YUV444 split helper (server) nor an AVC444 combiner
(client), so this script implements both sides *from MS-RDPEGFX*:

* colour conversion: 3.3.8.3.1 (full-range BT.709, the integer matrix shown
  in the spec, ``>> 8`` with the same flooring as FreeRDP's RGB2Y/U/V)
* main view: B1-B3 of 3.3.8.3.2 / 3.3.8.3.3, chroma = 2x2 mean ("U~")
* auxiliary view: B4-B7 (v1, macroblock-interleaved) and B4-B9 (v2)
* combination: reverse filter ``4*U~ - (three aux samples)`` with the
  optional "cut-off threshold 30" rule, or without it, or with no reverse
  filter at all (what a naive client does)

Both views are encoded as a 2-frame stream by one libx264 instance (the spec
requires one encoder/decoder for both views), constant QP, no B-frames, and
decoded by ffmpeg's H.264 decoder.  Decoded RGBA goes to
``results/decoded/<variant>_qp<NN>/<image>.rgba`` so ``metrics.py`` can
score everything uniformly.  The intermediate YUV planes are kept under
``results/avc_planes/`` for the FreeRDP cross-check (``freerdp_check.c``).
"""

import csv
import subprocess
import sys
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent
IMAGES = ROOT / "images"
RESULTS = ROOT / "results"
QPS = [18, 22, 26, 30, 34]
THRESHOLD = 30


# ---------------------------------------------------------------------------
# colour conversion, MS-RDPEGFX 3.3.8.3.1
# ---------------------------------------------------------------------------


def rgb_to_yuv444(rgb: np.ndarray):
    r, g, b = (rgb[..., i].astype(np.int32) for i in range(3))
    y = (54 * r + 183 * g + 18 * b) >> 8
    u = ((-29 * r - 99 * g + 128 * b) >> 8) + 128
    v = ((128 * r - 116 * g - 12 * b) >> 8) + 128
    return [np.clip(p, 0, 255).astype(np.int32) for p in (y, u, v)]


def yuv444_to_rgb(y, u, v) -> np.ndarray:
    y = y.astype(np.int32)
    d = u.astype(np.int32) - 128
    e = v.astype(np.int32) - 128
    r = (256 * y + 403 * e) >> 8
    g = (256 * y - 48 * d - 120 * e) >> 8
    b = (256 * y + 475 * d) >> 8
    return np.clip(np.stack([r, g, b], -1), 0, 255).astype(np.uint8)


def mean2x2(p: np.ndarray) -> np.ndarray:
    s = p[0::2, 0::2] + p[1::2, 0::2] + p[0::2, 1::2] + p[1::2, 1::2]
    return s // 4  # FreeRDP: ((UINT16)a+b+c+d)/4


def up2(p: np.ndarray) -> np.ndarray:
    return np.repeat(np.repeat(p, 2, 0), 2, 1)


# ---------------------------------------------------------------------------
# view construction (server side)
# ---------------------------------------------------------------------------


def main_view(y, u, v, point_sample=False):
    if point_sample:
        return y, u[0::2, 0::2].copy(), v[0::2, 0::2].copy()
    return y, mean2x2(u), mean2x2(v)


def aux_view_v1(u, v):
    h, w = u.shape
    ay = np.zeros((h, w), np.int32)
    for r in range(h):
        mb, within = divmod(r, 16)
        if within < 8:
            ay[r] = u[16 * mb + 2 * within + 1]
        else:
            ay[r] = v[16 * mb + 2 * (within - 8) + 1]
    au = u[0::2, 1::2].copy()  # B6: U444(2x+1, 2y)
    av = v[0::2, 1::2].copy()  # B7: V444(2x+1, 2y)
    return ay, au, av


def aux_view_v2(u, v):
    h, w = u.shape
    ay = np.concatenate([u[:, 1::2], v[:, 1::2]], axis=1)  # B4 | B5
    au = np.concatenate([u[1::2, 0::4], v[1::2, 0::4]], axis=1)  # B6 | B7
    av = np.concatenate([u[1::2, 2::4], v[1::2, 2::4]], axis=1)  # B8 | B9
    assert ay.shape == (h, w) and au.shape == (h // 2, w // 2) and av.shape == (h // 2, w // 2)
    return ay, au, av


# ---------------------------------------------------------------------------
# combination (client side)
# ---------------------------------------------------------------------------


def combine(main, aux, version, mode):
    """mode: 'avc420' | 'nofilter' | 'filter' | 'filter_t30'"""
    my, mu, mv = main
    u = up2(mu).astype(np.int32)
    v = up2(mv).astype(np.int32)
    if mode == "avc420":
        return my, u, v
    ay, au, av = aux
    h, w = my.shape
    if version == 1:
        for r in range(h):
            mb, within = divmod(r, 16)
            if within < 8:
                u[16 * mb + 2 * within + 1] = ay[r]
            else:
                v[16 * mb + 2 * (within - 8) + 1] = ay[r]
        u[0::2, 1::2] = au
        v[0::2, 1::2] = av
    else:
        u[:, 1::2] = ay[:, : w // 2]
        v[:, 1::2] = ay[:, w // 2 :]
        u[1::2, 0::4] = au[:, : w // 4]
        v[1::2, 0::4] = au[:, w // 4 :]
        u[1::2, 2::4] = av[:, : w // 4]
        v[1::2, 2::4] = av[:, w // 4 :]
    if mode in ("filter", "filter_t30"):
        for p, m in ((u, mu), (v, mv)):
            rec = 4 * m - p[0::2, 1::2] - p[1::2, 0::2] - p[1::2, 1::2]
            rec = np.clip(rec, 0, 255)
            if mode == "filter_t30":
                # Spec: use the reversed value only if it differs from U~ by more than 30.
                rec = np.where(np.abs(m - rec) > THRESHOLD, rec, m)
            p[0::2, 0::2] = rec
    return my, u, v


# ---------------------------------------------------------------------------
# H.264 through libx264 / ffmpeg
# ---------------------------------------------------------------------------


def to_i420(y, u, v) -> bytes:
    return b"".join(np.clip(p, 0, 255).astype(np.uint8).tobytes() for p in (y, u, v))


def from_i420(buf: bytes, w: int, h: int):
    a = np.frombuffer(buf, np.uint8)
    ys, cs = w * h, (w // 2) * (h // 2)
    y = a[:ys].reshape(h, w).astype(np.int32)
    u = a[ys : ys + cs].reshape(h // 2, w // 2).astype(np.int32)
    v = a[ys + cs : ys + 2 * cs].reshape(h // 2, w // 2).astype(np.int32)
    return y, u, v


def x264_roundtrip(frames, w, h, qp, work: Path):
    work.mkdir(parents=True, exist_ok=True)
    raw = work / "in.yuv"
    raw.write_bytes(b"".join(to_i420(*f) for f in frames))
    es = work / "out.h264"
    subprocess.run(
        [
            "ffmpeg", "-v", "error", "-y",
            "-f", "rawvideo", "-pix_fmt", "yuv420p", "-s", f"{w}x{h}", "-r", "30", "-i", str(raw),
            "-c:v", "libx264", "-profile:v", "high", "-preset", "medium", "-qp", str(qp),
            "-x264-params", "bframes=0:keyint=1000:scenecut=0:ref=1",
            "-f", "h264", str(es),
        ],
        check=True,
    )
    dec = subprocess.run(
        ["ffmpeg", "-v", "error", "-i", str(es), "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"],
        check=True,
        capture_output=True,
    ).stdout
    fsz = w * h * 3 // 2
    assert len(dec) == fsz * len(frames), (len(dec), fsz)
    sizes = subprocess.run(
        ["ffprobe", "-v", "error", "-show_entries", "packet=size", "-of", "csv=p=0", str(es)],
        check=True, capture_output=True, text=True,
    ).stdout.split()
    return [from_i420(dec[i * fsz : (i + 1) * fsz], w, h) for i in range(len(frames))], [int(s) for s in sizes]


def save_rgba(variant: str, name: str, rgb: np.ndarray):
    d = RESULTS / "decoded" / variant
    d.mkdir(parents=True, exist_ok=True)
    rgba = np.concatenate([rgb, np.full(rgb.shape[:2] + (1,), 255, np.uint8)], -1)
    (d / f"{name}.rgba").write_bytes(rgba.tobytes())


def save_planes(tag: str, name: str, planes):
    d = RESULTS / "avc_planes" / tag
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{name}.i420").write_bytes(to_i420(*planes))


def main():
    names = sorted(p.stem for p in IMAGES.glob("*.rgba"))
    rows = []
    amp_rows = []
    for name in names:
        w, h = map(int, (IMAGES / f"{name}.txt").read_text().split())
        rgba = np.frombuffer((IMAGES / f"{name}.rgba").read_bytes(), np.uint8).reshape(h, w, 4)
        y, u, v = rgb_to_yuv444(rgba[..., :3])
        save_rgba("yuv444_colorconv_only", name, yuv444_to_rgb(y, u, v))

        main_avg = main_view(y, u, v)
        main_pt = main_view(y, u, v, point_sample=True)
        aux1, aux2 = aux_view_v1(u, v), aux_view_v2(u, v)
        save_planes("main", name, main_avg)
        save_planes("aux_v1", name, aux1)
        save_planes("aux_v2", name, aux2)

        # Transport without compression: shows what the layout + filter lose by themselves.
        save_rgba("avc420_noenc", name, yuv444_to_rgb(*combine(main_avg, None, 1, "avc420")))
        for ver, aux in ((1, aux1), (2, aux2)):
            for mode in ("nofilter", "filter", "filter_t30"):
                save_rgba(f"avc444v{ver}_{mode}_noenc", name, yuv444_to_rgb(*combine(main_avg, aux, ver, mode)))

        for qp in QPS:
            work = RESULTS / "work" / f"{name}_qp{qp}"
            (m420,), s420 = x264_roundtrip([main_avg], w, h, qp, work / "avc420")
            save_rgba(f"avc420_qp{qp}", name, yuv444_to_rgb(*combine(m420, None, 1, "avc420")))
            rows.append(dict(image=name, variant="avc420", qp=qp, bytes=sum(s420)))
            for ver, aux in ((1, aux1), (2, aux2)):
                (m, a), s = x264_roundtrip([main_avg, aux], w, h, qp, work / f"v{ver}")
                save_planes(f"dec_main_v{ver}_qp{qp}", name, m)
                save_planes(f"dec_aux_v{ver}_qp{qp}", name, a)
                for mode in ("nofilter", "filter", "filter_t30"):
                    save_rgba(f"avc444v{ver}_{mode}_qp{qp}", name, yuv444_to_rgb(*combine(m, a, ver, mode)))
                rows.append(dict(image=name, variant=f"avc444v{ver}", qp=qp, bytes=sum(s)))

                # Error amplification of the reverse filter at the (2x,2y) sites:
                # err(main U~) vs err(reconstructed U(2x,2y)), both against the source.
                for comp, src, mplane, dplane in (("U", u, main_avg[1], m[1]), ("V", v, main_avg[2], m[2])):
                    _, cu, cv = combine(m, a, ver, "filter")
                    rec = (cu if comp == "U" else cv)[0::2, 0::2]
                    e_main = dplane - mplane
                    e_rec = rec - src[0::2, 0::2]
                    rms_main = float(np.sqrt(np.mean(e_main.astype(float) ** 2)))
                    rms_rec = float(np.sqrt(np.mean(e_rec.astype(float) ** 2)))
                    amp_rows.append(
                        dict(image=name, version=ver, qp=qp, comp=comp, rms_err_main_mean=round(rms_main, 3),
                             rms_err_reconstructed=round(rms_rec, 3),
                             ratio=round(rms_rec / rms_main, 2) if rms_main > 0 else ""))

            # What-if: point-sampled main chroma, client uses it as is (NOT spec conformant).
            (mp, ap), _ = x264_roundtrip([main_pt, aux1], w, h, qp, work / "v1pt")
            save_rgba(f"avc444v1_pointsample_qp{qp}", name, yuv444_to_rgb(*combine(mp, ap, 1, "nofilter")))
        print(f"{name}: done", file=sys.stderr)

    with open(RESULTS / "avc_sizes.csv", "w", newline="") as f:
        wr = csv.DictWriter(f, fieldnames=list(rows[0].keys()))
        wr.writeheader()
        wr.writerows(rows)
    with open(RESULTS / "avc_error_amplification.csv", "w", newline="") as f:
        wr = csv.DictWriter(f, fieldnames=list(amp_rows[0].keys()))
        wr.writeheader()
        wr.writerows(amp_rows)


if __name__ == "__main__":
    main()
