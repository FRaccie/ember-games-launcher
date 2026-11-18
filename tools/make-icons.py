#!/usr/bin/env python3
"""Draws the launcher icon (an ember flame on ink) as RGBA PNGs and an ICO,
with the standard library only. Run from the repo root."""
import math, struct, zlib, os

INK, EMBER, LIGHT = (0x14, 0x13, 0x12), (0xFF, 0x87, 0x38), (0xFF, 0xB1, 0x6F)

def flame(x, y):
    """Inside test on a 0..1 square: an outer flame and a lighter core."""
    def drop(x, y, cx, cy, r, tip):
        # A circle with a pointed top: inside the circle, or inside the
        # triangle from the circle's top tangent points up to the tip.
        if (x - cx) ** 2 + (y - cy) ** 2 <= r * r:
            return True
        if y < cy and y > tip:
            half = r * (y - tip) / (cy - tip)
            lean = (cy - y) / (cy - tip) * r * 0.35
            return abs(x - (cx + lean)) <= half
        return False
    if drop(x, y, 0.5, 0.62, 0.24, 0.10):
        return LIGHT if drop(x, y, 0.5, 0.70, 0.12, 0.40) else EMBER
    return None

def render(size):
    rows = []
    rad = size * 0.18
    for py in range(size):
        row = bytearray([0])
        for px in range(size):
            acc = [0, 0, 0, 0]
            for sy in range(4):
                for sx in range(4):
                    x = (px + (sx + 0.5) / 4) / size
                    y = (py + (sy + 0.5) / 4) / size
                    # rounded square background
                    dx = max(abs(x - 0.5) * size - (size / 2 - rad), 0)
                    dy = max(abs(y - 0.5) * size - (size / 2 - rad), 0)
                    if dx * dx + dy * dy > rad * rad:
                        continue
                    c = flame(x, y) or INK
                    acc[0] += c[0]; acc[1] += c[1]; acc[2] += c[2]; acc[3] += 255
            n = 16
            a = acc[3] // n
            if a:
                row += bytes([acc[0] * 255 // acc[3], acc[1] * 255 // acc[3], acc[2] * 255 // acc[3], a])
            else:
                row += bytes(4)
        rows.append(bytes(row))
    raw = b"".join(rows)
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw, 9)) + chunk(b"IEND", b"")

out = "app/src-tauri/icons"
pngs = {}
for name, size in [("32x32.png", 32), ("128x128.png", 128), ("128x128@2x.png", 256), ("icon.png", 512)]:
    pngs[size] = render(size)
    open(os.path.join(out, name), "wb").write(pngs[size])
sizes = [16, 32, 48, 256]
imgs = [pngs.get(s) or render(s) for s in sizes]
head = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
dirs = b""
for s, d in zip(sizes, imgs):
    dirs += struct.pack("<BBBBHHII", s % 256, s % 256, 0, 0, 1, 32, len(d), offset)
    offset += len(d)
open(os.path.join(out, "icon.ico"), "wb").write(head + dirs + b"".join(imgs))
print("icons written")
