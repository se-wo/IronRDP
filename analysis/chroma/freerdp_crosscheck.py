#!/usr/bin/env python3
"""Run FreeRDP (via freerdp_check.c) against the same data IronRDP / the
spec-model produced and record the differences.

Needs: FREERDP_CHECK=<path to compiled freerdp_check>, and results from
harness (cargo run) + avc_experiment.py.

Checks:
1. server split: FreeRDP RGBToAVC444YUV(v2) vs. avc_experiment.py main/aux planes
2. client combine: FreeRDP YUV420CombineToYUV444 + YUV444ToRGB (threshold filter)
   on the *decoded* x264 planes vs. avc_experiment.py "filter_t30"
3. IronRDP-encoded NSCodec / ClearCodec / Planar streams decoded by FreeRDP
   vs. the IronRDP decoder and vs. the source image
"""

import csv
import os
import subprocess
from pathlib import Path

import numpy as np

ROOT = Path(__file__).resolve().parent
RES = ROOT / "results"
BIN = os.environ["FREERDP_CHECK"]


def rgb(path: Path, h: int, w: int) -> np.ndarray:
    return np.frombuffer(path.read_bytes(), np.uint8).reshape(h, w, 4)[..., :3].astype(int)


def cmp(a: np.ndarray, b: np.ndarray):
    d = np.abs(a.astype(int) - b.astype(int))
    return int(d.max()), round(float((d.reshape(-1, d.shape[-1]) if d.ndim == 3 else d.reshape(-1, 1)).any(1).mean()) * 100, 3)


def run(*args):
    subprocess.run([BIN, *map(str, args)], check=True)


def main():
    rows = []
    names = sorted(p.stem for p in (ROOT / "images").glob("*.rgba"))
    tmp = RES / "work" / "freerdp"
    tmp.mkdir(parents=True, exist_ok=True)
    for name in names:
        w, h = map(int, (ROOT / "images" / f"{name}.txt").read_text().split())
        src = rgb(ROOT / "images" / f"{name}.rgba", h, w)

        # 1. server-side split
        for ver in ("v1", "v2"):
            m, a = tmp / f"{name}_{ver}_main.i420", tmp / f"{name}_{ver}_aux.i420"
            run("avc-split", ROOT / "images" / f"{name}.rgba", ver, w, h, m, a)
            ours_m = np.frombuffer((RES / "avc_planes" / "main" / f"{name}.i420").read_bytes(), np.uint8)
            ours_a = np.frombuffer((RES / "avc_planes" / f"aux_{ver}" / f"{name}.i420").read_bytes(), np.uint8)
            fm, fa = np.frombuffer(m.read_bytes(), np.uint8), np.frombuffer(a.read_bytes(), np.uint8)
            rows.append(dict(check=f"split_main_{ver}", image=name, max_abs_diff=int(np.abs(ours_m.astype(int) - fm).max()),
                             pct_samples_diff=round(float((ours_m != fm).mean()) * 100, 3)))
            rows.append(dict(check=f"split_aux_{ver}", image=name, max_abs_diff=int(np.abs(ours_a.astype(int) - fa).max()),
                             pct_samples_diff=round(float((ours_a != fa).mean()) * 100, 3)))

        # 2. client-side combine on decoded planes (QP 22) and on un-encoded planes
        for ver in ("v1", "v2"):
            for tag, mdir, adir, ours in (
                ("qp22", f"dec_main_{ver}_qp22", f"dec_aux_{ver}_qp22", f"avc444{ver}_filter_t30_qp22"),
                ("noenc", "main", f"aux_{ver}", f"avc444{ver}_filter_t30_noenc"),
            ):
                for flavour in ("generic", "opt"):
                    variant = f"freerdp_avc444{ver}_{flavour}_{tag}"
                    out_dir = RES / "decoded" / variant
                    out_dir.mkdir(parents=True, exist_ok=True)
                    out = out_dir / f"{name}.rgba"
                    run("avc-combine", RES / "avc_planes" / mdir / f"{name}.i420", RES / "avc_planes" / adir / f"{name}.i420",
                        ver, w, h, out, flavour)
                    mx, pct = cmp(rgb(out, h, w), rgb(RES / "decoded" / ours / f"{name}.rgba", h, w))
                    rows.append(dict(check=f"combine_{ver}_{flavour}_{tag}_vs_spec_model", image=name, max_abs_diff=mx, pct_samples_diff=pct))

        # 3. IronRDP streams decoded by FreeRDP
        for codec, ftool in (("nscodec_cll1", "nsc"), ("nscodec_cll3", "nsc"), ("nscodec_cll7", "nsc"),
                             ("clearcodec", "clear"), ("planar", "planar")):
            variant = f"freerdp_decode_{codec}"
            out_dir = RES / "decoded" / variant
            out_dir.mkdir(parents=True, exist_ok=True)
            out = out_dir / f"{name}.rgba"
            try:
                run(ftool, RES / "streams" / codec / f"{name}.bin", w, h, out)
            except subprocess.CalledProcessError:
                rows.append(dict(check=f"{codec}_freerdp_decode", image=name, max_abs_diff="DECODE_FAILED", pct_samples_diff=""))
                out.unlink(missing_ok=True)
                continue
            f = rgb(out, h, w)
            mx, pct = cmp(f, rgb(RES / "decoded" / codec / f"{name}.rgba", h, w))
            rows.append(dict(check=f"{codec}_freerdp_vs_ironrdp_decoder", image=name, max_abs_diff=mx, pct_samples_diff=pct))
            mx, pct = cmp(f, src)
            rows.append(dict(check=f"{codec}_freerdp_vs_source", image=name, max_abs_diff=mx, pct_samples_diff=pct))

    with open(RES / "freerdp_crosscheck.csv", "w", newline="") as fh:
        wr = csv.DictWriter(fh, fieldnames=list(rows[0].keys()))
        wr.writeheader()
        wr.writerows(rows)
    print(f"{len(rows)} rows -> results/freerdp_crosscheck.csv")


if __name__ == "__main__":
    main()
