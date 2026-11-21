#!/usr/bin/env python3
"""Generate the original GIF artwork used by retro.html (run once; output is committed).

A tiny GIF89a writer: uncompressed-style LZW (a clear code every few pixels keeps
the code size fixed), global palette, optional transparency.
"""
import struct


def lzw(indices, min_bits):
    clear, end = 1 << min_bits, (1 << min_bits) + 1
    bits = min_bits + 1
    out, acc, nacc = bytearray(), 0, 0

    def emit(code):
        nonlocal acc, nacc
        acc |= code << nacc
        nacc += bits
        while nacc >= 8:
            out.append(acc & 0xFF)
            acc >>= 8
            nacc -= 8

    step = (1 << bits) - (1 << min_bits) - 2
    for i, v in enumerate(indices):
        if i % step == 0:
            emit(clear)
        emit(v)
    emit(end)
    if nacc:
        out.append(acc & 0xFF)
    return bytes(out)


def gif(path, w, h, palette, pixels, transparent=None):
    n = 2
    while (1 << n) < len(palette):
        n += 1
    pal = list(palette) + [(0, 0, 0)] * ((1 << n) - len(palette))
    data = bytearray(b"GIF89a" + struct.pack("<HHBBB", w, h, 0x80 | (n - 1), 0, 0))
    for r, g, b in pal:
        data += bytes((r, g, b))
    if transparent is not None:
        data += bytes((0x21, 0xF9, 4, 1, 0, 0, transparent, 0))
    data += b"\x2c" + struct.pack("<HHHHB", 0, 0, w, h, 0)
    min_bits = max(2, n)
    data.append(min_bits)
    stream = lzw(pixels, min_bits)
    for i in range(0, len(stream), 255):
        chunk = stream[i:i + 255]
        data += bytes((len(chunk),)) + chunk
    data += b"\x00\x3b"
    open(path, "wb").write(data)


# Tiled starfield background (32x32).
pal = [(0x10, 0x08, 0x30), (0xFF, 0xFF, 0xFF), (0x7C, 0x5C, 0xFF), (0x3D, 0xF2, 0xE0)]
px = [0] * (32 * 32)
for (x, y, c) in [(3, 4, 1), (20, 9, 2), (11, 22, 3), (27, 27, 1), (6, 15, 2), (16, 2, 3), (25, 18, 1)]:
    px[y * 32 + x] = c
gif("stars.gif", 32, 32, pal, px)

# "Under construction" stripe banner 200x40: diagonal yellow/black stripes, transparent frame.
pal = [(0, 0, 0), (0xFF, 0xD7, 0x00), (0xFF, 0x00, 0xFF)]
px = []
for y in range(40):
    for x in range(200):
        if y < 2 or y >= 38 or x < 2 or x >= 198:
            px.append(2)
        else:
            px.append(1 if ((x + y) // 10) % 2 == 0 else 0)
gif("construction.gif", 200, 40, pal, px, transparent=2)

# 88x31 buttons in three colors with a bevel.
for name, face in [("btn-violet", (0x7C, 0x5C, 0xFF)), ("btn-cyan", (0x3D, 0xF2, 0xE0)), ("btn-red", (0xE0, 0x30, 0x30))]:
    pal = [face, (0xFF, 0xFF, 0xFF), (0x40, 0x40, 0x40), (0, 0, 0)]
    px = []
    for y in range(31):
        for x in range(88):
            if x < 2 or y < 2:
                px.append(1)
            elif x >= 86 or y >= 29:
                px.append(2)
            elif 10 <= y < 21 and 10 <= x < 78 and (x // 4 + y // 4) % 2 == 0:
                px.append(3)
            else:
                px.append(0)
    gif(f"{name}.gif", 88, 31, pal, px)

# A 64x64 "photo" with a 16-step gradient for float tests.
pal = [(i * 16, 0x40 + i * 8, 0xFF - i * 16) for i in range(16)]
px = [((x + y) * 16 // 128) for y in range(64) for x in range(64)]
gif("photo.gif", 64, 64, pal, px)
print("ok")
