#!/usr/bin/env python3
"""Generate synthetic chroma test images.

Every image is written twice into ``analysis/chroma/images/``:

* ``<name>.png``  - for viewing
* ``<name>.rgba`` - raw, tightly packed RGBA8888 (top-down); the Rust harness
  reads this together with ``<name>.txt`` (``width height``)

All images are 320x128 so that they are multiples of 64 (RemoteFX tiles) and
16 (H.264 macroblocks).  Nothing here depends on the IronRDP code base.
"""

from pathlib import Path

import numpy as np
from PIL import Image, ImageDraw, ImageFont

W, H = 320, 128
OUT = Path(__file__).resolve().parent / "images"
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"
TEXT_LINES = [
    (9, "The quick brown fox jumps over the lazy dog 0123456789"),
    (11, "The quick brown fox jumps over the lazy dog"),
    (13, "Sphinx of black quartz, judge my vow"),
    (16, "Pack my box with five dozen"),
    (20, "liquor jugs! Illi1|"),
]


def text_mask() -> np.ndarray:
    """Anti-aliased grey-scale coverage mask (0..1) of the sample text."""
    img = Image.new("L", (W, H), 0)
    draw = ImageDraw.Draw(img)
    y = 4
    for size, line in TEXT_LINES:
        draw.text((4, y), line, fill=255, font=ImageFont.truetype(FONT, size))
        y += size + 6
    return np.asarray(img, dtype=np.float64) / 255.0


def colored_text(rgb: tuple[int, int, int]) -> np.ndarray:
    mask = text_mask()
    out = np.zeros((H, W, 3))
    for c in range(3):
        out[..., c] = mask * rgb[c]
    return np.rint(out).astype(np.uint8)


def cleartype_text() -> np.ndarray:
    """White-on-black text with ClearType-like sub-pixel rendering.

    The glyphs are rasterised at 3x horizontal resolution; each output pixel
    takes R/G/B from three adjacent sub-columns after a 5-tap [1 2 3 2 1]/9
    low-pass filter (the classic FreeType "LCD default" filter).
    """
    # Render with a horizontally stretched font instead of NEAREST scaling so
    # the sub-pixel positions carry real glyph detail.
    img = Image.new("L", (W * 3, H), 0)
    y = 4
    for size, line in TEXT_LINES:
        # Render at 3x size and squash vertically back to 1x: gives 3x
        # horizontal detail with the same vertical metrics.
        big = Image.new("L", (W * 3, (size + 8) * 3), 0)
        ImageDraw.Draw(big).text((12, 0), line, fill=255, font=ImageFont.truetype(FONT, size * 3))
        big = big.resize((W * 3, size + 8), Image.Resampling.BOX)
        img.paste(big, (0, y))
        y += size + 6
    m = np.asarray(img, dtype=np.float64) / 255.0
    k = np.array([1, 2, 3, 2, 1], dtype=np.float64) / 9.0
    mf = np.apply_along_axis(lambda r: np.convolve(r, k, mode="same"), 1, m)
    out = np.zeros((H, W, 3))
    for c in range(3):
        out[..., c] = mf[:, c::3][:, :W] * 255.0
    return np.clip(np.rint(out), 0, 255).astype(np.uint8)


def lines_1px() -> np.ndarray:
    out = np.zeros((H, W, 3), dtype=np.uint8)
    colors = [(255, 0, 0), (0, 0, 255), (128, 0, 0), (0, 0, 128), (0, 255, 0), (255, 255, 255)]
    # Vertical 1-px lines at even and odd columns (tests 2x2 phase).
    for i, c in enumerate(colors):
        x0 = 8 + i * 24
        out[4:60, x0] = c
        out[4:60, x0 + 3] = c
        out[4:60, x0 + 6 : x0 + 8] = c
    # Horizontal 1-px lines at even and odd rows.
    for i, c in enumerate(colors):
        y0 = 68 + i * 9
        out[y0, 8:150] = c
        out[y0 + 3, 8:150] = c
    # Adjacent red/blue columns (pure chroma edge with small luma step).
    for x in range(160, 312):
        out[4:60, x] = (255, 0, 0) if (x // 1) % 2 == 0 else (0, 0, 255)
    for x in range(160, 312):
        out[68:124, x] = (255, 0, 0) if (x // 2) % 2 == 0 else (0, 0, 255)
    # Diagonal.
    for t in range(0, 56):
        out[4 + t, 150 + t // 4] = (255, 0, 0)
    return out


def chroma_checker() -> np.ndarray:
    """Checkerboards whose two colours have (almost) equal BT.709 luma.

    magenta (255,0,255) -> Y ~ 72.6, dark green (0,100,0) -> Y ~ 71.5
    red (255,0,0) -> Y ~ 54.2, blue (0,0,255) -> Y ~ 18.4 (for contrast)
    """
    out = np.zeros((H, W, 3), dtype=np.uint8)
    a, b = np.array([255, 0, 255]), np.array([0, 100, 0])
    r, bl = np.array([255, 0, 0]), np.array([0, 0, 255])
    yy, xx = np.mgrid[0:H, 0:W]
    for i, cell in enumerate([1, 2, 4, 8]):
        x0, x1 = i * 80, (i + 1) * 80
        sel = (slice(0, 64), slice(x0, x1))
        chk = ((yy[sel] // cell + xx[sel] // cell) % 2) == 0
        out[sel][chk] = a
        out[sel][~chk] = b
        sel2 = (slice(64, 128), slice(x0, x1))
        chk2 = ((yy[sel2] // cell + xx[sel2] // cell) % 2) == 0
        out[sel2][chk2] = r
        out[sel2][~chk2] = bl
    return out


def gradient() -> np.ndarray:
    """Top half: hue sweep at full saturation; bottom half: smooth grey ramp
    plus a slow red ramp (banding detector)."""
    out = np.zeros((H, W, 3), dtype=np.float64)
    x = np.linspace(0.0, 1.0, W)
    hue = x * 6.0
    c = np.ones_like(hue)
    xh = c * (1 - np.abs(hue % 2 - 1))
    z = np.zeros_like(hue)
    seg = np.floor(hue).astype(int) % 6
    rgb = np.select(
        [seg[:, None] == k for k in range(6)],
        [
            np.stack([c, xh, z], 1),
            np.stack([xh, c, z], 1),
            np.stack([z, c, xh], 1),
            np.stack([z, xh, c], 1),
            np.stack([xh, z, c], 1),
            np.stack([c, z, xh], 1),
        ],
    )
    out[:64] = rgb[None, :, :] * 255.0
    out[64:96] = (x * 255.0)[None, :, None]
    out[96:128, :, 0] = (x * 255.0)[None, :]
    return np.clip(np.rint(out), 0, 255).astype(np.uint8)


SOLID_COLORS = ["FF0000", "0000FF", "800000", "000080", "00FF00", "FFFFFF", "808080", "00FFFF", "FF00FF", "C0C0C0"]


def solid_patches() -> np.ndarray:
    """Ten flat 64x64 tiles (5x2) for the cross-codec colour consistency check."""
    out = np.zeros((H, W, 3), dtype=np.uint8)
    for i, hexcol in enumerate(SOLID_COLORS):
        x0, y0 = (i % 5) * 64, (i // 5) * 64
        out[y0 : y0 + 64, x0 : x0 + 64] = [int(hexcol[j : j + 2], 16) for j in (0, 2, 4)]
    return out


def save(name: str, rgb: np.ndarray) -> None:
    OUT.mkdir(exist_ok=True)
    assert rgb.shape == (H, W, 3) and rgb.dtype == np.uint8
    Image.fromarray(rgb, "RGB").save(OUT / f"{name}.png")
    rgba = np.concatenate([rgb, np.full((H, W, 1), 255, np.uint8)], axis=2)
    (OUT / f"{name}.rgba").write_bytes(rgba.tobytes())
    (OUT / f"{name}.txt").write_text(f"{W} {H}\n")


def main() -> None:
    for hexcol in ["FF0000", "0000FF", "800000", "000080", "00FF00", "FFFFFF"]:
        rgb = tuple(int(hexcol[i : i + 2], 16) for i in (0, 2, 4))
        save(f"text_{hexcol.lower()}", colored_text(rgb))
    save("lines_1px", lines_1px())
    save("chroma_checker", chroma_checker())
    save("cleartype_text", cleartype_text())
    save("gradient", gradient())
    save("solid_patches", solid_patches())
    print(f"wrote images to {OUT}")


if __name__ == "__main__":
    main()
