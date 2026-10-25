#!/usr/bin/env python3
"""Compare IronRDP, FreeRDP and an 11.5 reference on the Windows Progressive
capture (see harness/src/bin/progressive_fixture.rs for how the inputs are
built).  Plausibility check only: the original screen content is unknown, so
this compares decoders with each other, not with ground truth.

Needs FREERDP_CHECK=<freerdp_check binary>.  Writes
results/progressive_fixture/summary.txt and a zoomed PNG strip.
"""

import os
import subprocess
from pathlib import Path

import numpy as np
from PIL import Image

ROOT = Path(__file__).resolve().parent
D = ROOT / "results" / "progressive_fixture"
W, H = 1280, 800


def load(name):
    return np.frombuffer((D / name).read_bytes(), np.uint8).reshape(H, W, 4)[..., :3].astype(int)


def stats(a, b, mask):
    d = np.abs(a - b)[mask]
    per_px = d.max(axis=1)
    return (
        f"max {int(d.max()):3d} | mean {d.mean():6.3f} | "
        f"px >2: {100 * (per_px > 2).mean():5.1f} % | px >8: {100 * (per_px > 8).mean():5.1f} %"
    )


def main():
    mask = np.frombuffer((D / "mask.bin").read_bytes(), np.uint8).reshape(H, W).astype(bool)
    lines = [f"compared pixels (base tiles within region rects): {int(mask.sum())}", ""]
    for f in (0, 1):
        subprocess.run(
            [os.environ["FREERDP_CHECK"], "progressive", D / f"stream_ctx{f}.bin", str(W), str(H), D / f"freerdp_ctx{f}.rgba"],
            check=True,
        )
        iron, free, ref = load(f"ironrdp_ctx{f}.rgba"), load(f"freerdp_ctx{f}.rgba"), load(f"ref115_ctx{f}.rgba")
        lines.append(f"CONTEXT flags = {f} (REGION flags = 0x01 reduce-extrapolate in every case)")
        lines.append(f"  IronRDP  vs FreeRDP : {stats(iron, free, mask)}")
        lines.append(f"  11.5 ref vs FreeRDP : {stats(ref, free, mask)}")
        lines.append(f"  IronRDP  vs 11.5 ref: {stats(iron, ref, mask)}")
        lines.append("")
        # Zoomed strip of the first base tile column (x 80..139, y 256..320): IronRDP | FreeRDP | 8x|diff|
        crop = (slice(256, 320), slice(80, 139))
        diff = np.clip(np.abs(iron - free) * 8, 0, 255)
        strip = np.concatenate([iron[crop], free[crop], diff[crop]], axis=1).astype(np.uint8)
        Image.fromarray(np.repeat(np.repeat(strip, 4, 0), 4, 1)).save(D / f"zoom_ctx{f}.png")
    text = "\n".join(lines)
    (D / "summary.txt").write_text(text + "\n")
    print(text)


if __name__ == "__main__":
    main()
