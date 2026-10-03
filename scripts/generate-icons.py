#!/usr/bin/env python3
"""Generate the console app icon with only Python's standard library.

The bracket/arrow geometry matches desktop/src/design.rs. The packaged icon
adds the rounded navy tile; the tray keeps the transparent glyph.
"""
from pathlib import Path
import struct
import zlib

ROOT = Path(__file__).resolve().parents[1]


def chunk(kind, data):
    return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))


def generate(size=1024):
    rows = bytearray()
    for y in range(size):
        rows.append(0)
        fy = (y + 0.5) / size
        for x in range(size):
            fx = (x + 0.5) / size
            dx = max(abs(fx - 0.5) - 0.30, 0)
            dy = max(abs(fy - 0.5) - 0.30, 0)
            tile = dx * dx + dy * dy <= 0.15 * 0.15
            bracket = ((0.18 <= fx < 0.24 or 0.76 <= fx < 0.82) and 0.22 <= fy < 0.78
                       or (0.18 <= fx < 0.34 or 0.66 <= fx < 0.82)
                       and (0.22 <= fy < 0.28 or 0.72 <= fy < 0.78))
            arrow = (0.36 <= fx < 0.65 and (0.37 <= fy < 0.42 or 0.58 <= fy < 0.63)
                     or abs(fx - 0.61) + abs(fy - 0.395) < 0.10 and fx > 0.56
                     or abs(fx - 0.39) + abs(fy - 0.605) < 0.10 and fx < 0.44)
            rgba = ((255, 176, 0, 255) if bracket else (89, 209, 239, 255) if arrow
                    else (11, 15, 26, 255) if tile else (0, 0, 0, 0))
            rows.extend(rgba)
    header = struct.pack('>IIBBBBB', size, size, 8, 6, 0, 0, 0)
    png = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', header) + chunk(b'IDAT', zlib.compress(rows)) + chunk(b'IEND', b'')
    name = 'transferbuddy@2x.png' if size == 1024 else 'transferbuddy.png'
    path = ROOT / 'packaging/icons' / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(png)
    print(path)


if __name__ == '__main__':
    generate()
    generate(512)
