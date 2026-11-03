"""Generates playterm.png (256x256), playterm.ico (16-256, Windows) and playterm.icns (16-1024,
macOS) with only the standard library.

The icon is a dark rounded square with a green terminal prompt ">_".
Run: python assets/make_icon.py
"""
import math
import struct
import zlib
from pathlib import Path

OUT = Path(__file__).resolve().parent
SS = 4  # supersampling per axis


def seg_dist(px, py, ax, ay, bx, by):
    dx, dy = bx - ax, by - ay
    t = max(0.0, min(1.0, ((px - ax) * dx + (py - ay) * dy) / (dx * dx + dy * dy)))
    return math.hypot(px - (ax + t * dx), py - (ay + t * dy))


def in_round_rect(x, y, x0, y0, x1, y1, r):
    cx = min(max(x, x0 + r), x1 - r)
    cy = min(max(y, y0 + r), y1 - r)
    return (x - cx) ** 2 + (y - cy) ** 2 <= r * r


def shade(x, y):
    """Colour (r, g, b, a) of a point in 256-unit icon space."""
    if not in_round_rect(x, y, 8, 8, 248, 248, 52):
        return (0, 0, 0, 0)
    # Prompt: chevron ">" and underscore "_".
    green = (78, 224, 122, 255)
    if seg_dist(x, y, 70, 82, 128, 128) <= 13 or seg_dist(x, y, 128, 128, 70, 174) <= 13:
        return green
    if 140 <= x <= 196 and 160 <= y <= 184:
        return green
    # Thin lighter rim, then a vertical gradient.
    if not in_round_rect(x, y, 14, 14, 242, 242, 46):
        return (70, 74, 92, 255)
    t = (y - 8) / 240
    top, bottom = (46, 49, 64), (22, 23, 30)
    return tuple(round(top[i] + (bottom[i] - top[i]) * t) for i in range(3)) + (255,)


def render(size):
    scale = 256 / size
    rows = []
    for py in range(size):
        row = bytearray()
        for px in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(SS):
                for sx in range(SS):
                    r, g, b, a = shade((px + (sx + 0.5) / SS) * scale, (py + (sy + 0.5) / SS) * scale)
                    acc[0] += r * a
                    acc[1] += g * a
                    acc[2] += b * a
                    acc[3] += a
            alpha = acc[3] / (SS * SS)
            if acc[3]:
                row += bytes((round(acc[0] / acc[3]), round(acc[1] / acc[3]), round(acc[2] / acc[3]), round(alpha)))
            else:
                row += b"\0\0\0\0"
        rows.append(bytes(row))
    return rows


def png(size, rows):
    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    raw = b"".join(b"\0" + r for r in rows)
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)  # 8-bit RGBA
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")


def ico(images):
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = 6 + 16 * len(images)
    entries, blobs = b"", b""
    for size, data in images:
        dim = 0 if size >= 256 else size
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        blobs += data
        offset += len(data)
    return header + entries + blobs


# icns element types holding PNG data, by pixel size (the @2x types reuse the larger images).
ICNS_TYPES = [
    (b"icp4", 16), (b"icp5", 32), (b"icp6", 64), (b"ic07", 128), (b"ic08", 256), (b"ic09", 512), (b"ic10", 1024),
    (b"ic11", 32), (b"ic12", 64), (b"ic13", 256), (b"ic14", 512),
]


def icns(pngs):
    body = b"".join(t + struct.pack(">I", 8 + len(pngs[s])) + pngs[s] for t, s in ICNS_TYPES)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


if __name__ == "__main__":
    pngs = {s: png(s, render(s)) for s in (16, 24, 32, 48, 64, 128, 256, 512, 1024)}
    (OUT / "playterm.png").write_bytes(pngs[256])
    (OUT / "playterm.ico").write_bytes(ico([(s, pngs[s]) for s in (16, 24, 32, 48, 64, 128, 256)]))
    (OUT / "playterm.icns").write_bytes(icns(pngs))
    print("wrote", OUT / "playterm.png", OUT / "playterm.ico", OUT / "playterm.icns")
