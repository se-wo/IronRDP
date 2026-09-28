#!/usr/bin/env python3
"""Score every decoded picture under results/decoded/<variant>/<image>.rgba
against images/<image>.rgba.

Metrics (per variant x image):

* PSNR of Y, U, V separately, computed in floating-point full-range BT.709
  YCbCr (the colour space MS-RDPEGFX uses for AVC), plus RGB PSNR
* SSIM on RGB (skimage, channel_axis) and on Y
* CIEDE2000 (mean and 99th percentile) in CIELAB (sRGB, D65)
* edge measures on a glyph-edge mask (reference Sobel magnitude, any channel,
  above 48, dilated by one pixel):
    - edge_de: mean CIEDE2000 inside the mask
    - edge_chroma_err: mean |(dU,dV)| inside the mask (colour fringing)
    - edge_sharpness: sum of decoded gradient magnitude / reference gradient
      magnitude inside the mask (1.0 = as sharp as the source, <1 = blurred)
* max absolute RGB error and a bit-exact flag

Writes results/metrics.csv and difference PNGs (|error| x 4) for a fixed set
of variants into results/diff/.
"""

import csv
import math
from pathlib import Path

import numpy as np
from PIL import Image
from scipy import ndimage
from skimage.color import deltaE_ciede2000, rgb2lab
from skimage.metrics import structural_similarity

ROOT = Path(__file__).resolve().parent
IMAGES = ROOT / "images"
RESULTS = ROOT / "results"

DIFF_VARIANTS = [
    "rfx_quant_default",
    "progressive_simple_default",
    "nscodec_cll3",
    "avc420_ironrdp_openh264",
    "avc420_colorconv_only_current",
    "avc420_qp22",
    "avc444v1_filter_t30_qp22",
    "avc444v2_filter_t30_qp22",
    "avc444v1_nofilter_qp22",
]
DIFF_IMAGES = ["text_ff0000", "text_0000ff", "text_000080", "cleartype_text", "lines_1px", "chroma_checker"]


def to_ycbcr709(rgb: np.ndarray):
    r, g, b = (rgb[..., i].astype(np.float64) for i in range(3))
    y = 0.2126 * r + 0.7152 * g + 0.0722 * b
    u = (b - y) / 1.8556 + 128.0
    v = (r - y) / 1.5748 + 128.0
    return y, u, v


def psnr(a: np.ndarray, b: np.ndarray) -> float:
    mse = float(np.mean((a.astype(np.float64) - b.astype(np.float64)) ** 2))
    return math.inf if mse < 1e-9 else 10 * math.log10(255.0**2 / mse)


def sobel_mag(p: np.ndarray) -> np.ndarray:
    p = p.astype(np.float64)
    return np.hypot(ndimage.sobel(p, 0), ndimage.sobel(p, 1))


def load(path: Path, h: int, w: int) -> np.ndarray:
    return np.frombuffer(path.read_bytes(), np.uint8).reshape(h, w, 4)[..., :3]


def main():
    refs = {}
    for p in sorted(IMAGES.glob("*.rgba")):
        w, h = map(int, (IMAGES / f"{p.stem}.txt").read_text().split())
        ref = load(p, h, w)
        mag = np.max(np.stack([sobel_mag(ref[..., c]) for c in range(3)]), axis=0)
        mask = ndimage.binary_dilation(mag > 48, iterations=1)
        refs[p.stem] = (ref, h, w, mask, rgb2lab(ref / 255.0), to_ycbcr709(ref))

    rows = []
    (RESULTS / "diff").mkdir(parents=True, exist_ok=True)
    for vdir in sorted((RESULTS / "decoded").iterdir()):
        for name, (ref, h, w, mask, lab_ref, yuv_ref) in refs.items():
            f = vdir / f"{name}.rgba"
            if not f.exists():
                continue
            dec = load(f, h, w)
            yuv_dec = to_ycbcr709(dec)
            de = deltaE_ciede2000(lab_ref, rgb2lab(dec / 255.0))
            grad_ref = sum(sobel_mag(ref[..., c]) for c in range(3))
            grad_dec = sum(sobel_mag(dec[..., c]) for c in range(3))
            chroma_err = np.hypot(yuv_dec[1] - yuv_ref[1], yuv_dec[2] - yuv_ref[2])
            rows.append(
                dict(
                    variant=vdir.name,
                    image=name,
                    psnr_y=round(psnr(yuv_ref[0], yuv_dec[0]), 2),
                    psnr_u=round(psnr(yuv_ref[1], yuv_dec[1]), 2),
                    psnr_v=round(psnr(yuv_ref[2], yuv_dec[2]), 2),
                    psnr_rgb=round(psnr(ref, dec), 2),
                    ssim_rgb=round(float(structural_similarity(ref, dec, channel_axis=2, data_range=255)), 4),
                    ssim_y=round(float(structural_similarity(yuv_ref[0], yuv_dec[0], data_range=255)), 4),
                    de2000_mean=round(float(de.mean()), 3),
                    de2000_p99=round(float(np.percentile(de, 99)), 3),
                    edge_de=round(float(de[mask].mean()), 3),
                    edge_chroma_err=round(float(chroma_err[mask].mean()), 3),
                    edge_sharpness=round(float(grad_dec[mask].sum() / max(grad_ref[mask].sum(), 1e-9)), 3),
                    max_abs_err=int(np.abs(ref.astype(int) - dec.astype(int)).max()),
                    bit_exact=bool(np.array_equal(ref, dec)),
                )
            )
            if vdir.name in DIFF_VARIANTS and name in DIFF_IMAGES:
                diff = np.clip(np.abs(ref.astype(int) - dec.astype(int)) * 4, 0, 255).astype(np.uint8)
                # ref | decoded | diff, left 96 px of the first two text lines, 4x nearest zoom
                crop = (slice(0, 48), slice(0, 96))
                strip = np.concatenate([ref[crop], dec[crop], diff[crop]], axis=1)
                strip = np.repeat(np.repeat(strip, 4, 0), 4, 1)
                Image.fromarray(strip).save(RESULTS / "diff" / f"{vdir.name}__{name}__zoom.png")
                Image.fromarray(diff).save(RESULTS / "diff" / f"{vdir.name}__{name}__diff.png")

    with open(RESULTS / "metrics.csv", "w", newline="") as fh:
        wr = csv.DictWriter(fh, fieldnames=list(rows[0].keys()))
        wr.writeheader()
        wr.writerows(rows)
    print(f"{len(rows)} rows -> results/metrics.csv")
    solid_colors()


SOLID = ["FF0000", "0000FF", "800000", "000080", "00FF00", "FFFFFF", "808080", "00FFFF", "FF00FF", "C0C0C0"]
SOLID_VARIANTS = [
    "clearcodec", "planar", "nscodec_cll1", "nscodec_cll3", "rfx_quant_6", "rfx_quant_default",
    "whatif_rfx_quant_default_rounded", "progressive_simple_q6", "progressive_simple_default",
    "avc420_ironrdp_openh264", "avc420_colorconv_only_current", "avc420_colorconv_only_bt709full",
    "yuv444_colorconv_only", "avc420_qp22", "avc444v1_filter_t30_qp22",
]


def solid_colors():
    """Decoded colour at the centre of each flat 64x64 tile of solid_patches."""
    w, h = map(int, (IMAGES / "solid_patches.txt").read_text().split())
    with open(RESULTS / "solid_colors.csv", "w", newline="") as fh:
        wr = csv.writer(fh)
        wr.writerow(["variant"] + SOLID)
        for v in SOLID_VARIANTS:
            dec = load(RESULTS / "decoded" / v / "solid_patches.rgba", h, w)
            wr.writerow([v] + ["%02X%02X%02X" % tuple(dec[(i // 5) * 64 + 32, (i % 5) * 64 + 32]) for i in range(len(SOLID))])


if __name__ == "__main__":
    main()
