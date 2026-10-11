#!/usr/bin/env python3
"""Inspect the Windows EGFX captures vendored in IronRDP
(crates/ironrdp-testsuite-core/test_data/egfx/haven, Windows Server 2025 /
Windows 11 24H2, see README there) for chroma-relevant parameters:

* ClearCodec: which layers are used, and for NSCodec subcodecs the
  ColorLossLevel and ChromaSubsamplingLevel Windows picks
* RemoteFX Progressive: the quantisation tables and which table index is
  used for Y vs. Cb vs. Cr, plus the per-tile quality value

Pure parser per MS-RDPEGFX 2.2.2.1 / 2.2.4.1 / 2.2.4.2 and MS-RDPNSC 2.2.2;
nothing from IronRDP is imported.
"""

import struct
import sys
from pathlib import Path

FIX = Path(__file__).resolve().parents[2] / "crates/ironrdp-testsuite-core/test_data/egfx/haven"


def clearcodec(bmp: bytes, name: str):
    flags, seq = bmp[0], bmp[1]
    off = 2
    if flags & 0x01:
        off += 2  # glyphIndex
    if flags & 0x02:
        print(f"  {name}: glyph hit")
        return
    res, bands, sub = struct.unpack_from("<III", bmp, off)
    off += 12
    print(f"  {name}: flags=0x{flags:02x} residual={res} bands={bands} subcodecs={sub}")
    p = off + res + bands
    end = p + sub
    while p < end:
        x, y, w, h, n, cid = struct.unpack_from("<HHHHIB", bmp, p)
        data = bmp[p + 13 : p + 13 + n]
        desc = {0: "uncompressed", 1: "NSCodec", 2: "RLEX"}.get(cid, f"id{cid}")
        extra = ""
        if cid == 1:
            cll, css = data[16], data[17]
            extra = f" ColorLossLevel={cll} ChromaSubsamplingLevel={css}"
        print(f"    subcodec {desc} at ({x},{y}) {w}x{h} bytes={n}{extra}")
        p += 13 + n


def progressive(bmp: bytes, name: str):
    p = 0
    while p + 6 <= len(bmp):
        btype, blen = struct.unpack_from("<HI", bmp, p)
        if btype == 0xCCC4:  # REGION
            tile_size, nrects, nquant, nprog, rflags, ntiles, tsize = struct.unpack_from("<BHBBBHI", bmp, p + 6)
            q = p + 6 + 12 + 8 * nrects
            tables = []
            for _ in range(nquant):
                raw = bmp[q : q + 5]
                nib = []
                for b in raw:
                    nib += [b & 0x0F, b >> 4]
                # RFX_COMPONENT_CODEC_QUANT order: LL3 HL3 LH3 HH3 HL2 LH2 HH2 HL1 LH1 HH1
                tables.append(dict(zip(["LL3", "HL3", "LH3", "HH3", "HL2", "LH2", "HH2", "HL1", "LH1", "HH1"], nib)))
                q += 5
            q += 16 * nprog
            uses = {}
            quals = set()
            t = q
            for _ in range(ntiles):
                ttype, tlen = struct.unpack_from("<HI", bmp, t)
                qy, qcb, qcr = bmp[t + 6], bmp[t + 7], bmp[t + 8]
                uses[(qy, qcb, qcr)] = uses.get((qy, qcb, qcr), 0) + 1
                if ttype in (0xCCC6, 0xCCC7):  # TILE_FIRST / TILE_UPGRADE carry a quality byte
                    quals.add(bmp[t + 14])
                t += tlen
            print(f"  {name}: region flags=0x{rflags:02x} tiles={ntiles} quant_tables={nquant} prog_quant={nprog}")
            for i, tb in enumerate(tables):
                print(f"    quant[{i}] = {tb}")
            print(f"    (quantIdxY, quantIdxCb, quantIdxCr) usage: {uses}; quality bytes: {sorted(quals)}")
        p += blen if blen else len(bmp)


def main():
    for f in sorted(FIX.glob("*.bin")):
        b = f.read_bytes()
        cmd = struct.unpack_from("<H", b, 0)[0]
        if cmd == 0x0001:  # WIRE_TO_SURFACE_1
            sid, codec, fmt = struct.unpack_from("<HHB", b, 8)
            blen = struct.unpack_from("<I", b, 8 + 5 + 8)[0]
            bmp = b[8 + 5 + 8 + 4 : 8 + 5 + 8 + 4 + blen]
            if codec == 0x0008:
                clearcodec(bmp, f.name)
            else:
                print(f"  {f.name}: WTS1 codec 0x{codec:04x}")
        elif cmd == 0x0002:  # WIRE_TO_SURFACE_2
            sid, codec, ctx, fmt = struct.unpack_from("<HHIB", b, 8)
            blen = struct.unpack_from("<I", b, 8 + 9)[0]
            progressive(b[8 + 9 + 4 : 8 + 9 + 4 + blen], f.name)
        else:
            print(f"  {f.name}: cmdId 0x{cmd:04x}")


if __name__ == "__main__":
    sys.exit(main())
