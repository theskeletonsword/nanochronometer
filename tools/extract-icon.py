#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Extracts the bitmap out of assets/nanochrono.ico.

One icon asset is checked in — the `.ico` — because Windows needs that exact
container and there is no sense keeping three copies of one drawing in the
tree. Linux and macOS want a PNG, so this pulls it back out rather than
storing a second copy that can drift from the first.

Only the PNG-in-ICO form is handled, which is what the asset uses and what
every icon editor has produced for a decade. A classic BMP-in-ICO would need
a whole decoder, and the moment this is pointed at one it says so rather than
writing out a broken file.

The output format follows the extension:

* `.png` — the bitmap, as it sits inside the container.
* `.icns` — the same bitmap wrapped in an Apple icon file, so a macOS bundle
  can be built on a machine that has no `iconutil`. Cross-compiling to macOS
  from Linux is the normal case here, and `iconutil` is macOS-only.

Usage:
    extract-icon.py <input.ico> <output.png|output.icns>
"""

import struct
import sys

PNG_MAGIC = b"\x89PNG\r\n\x1a\n"


def extract(ico: bytes) -> bytes:
    """Returns the largest image in the container, as PNG bytes."""
    if len(ico) < 6:
        raise SystemExit("not an ICO: too short for a header")

    reserved, kind, count = struct.unpack("<HHH", ico[:6])
    if reserved != 0 or kind != 1:
        raise SystemExit(f"not an ICO: reserved={reserved} type={kind}")
    if count == 0:
        raise SystemExit("the ICO declares no images")

    best = None
    for index in range(count):
        at = 6 + index * 16
        if at + 16 > len(ico):
            raise SystemExit("the ICO's directory runs past the end of the file")
        width, height, _colours, _r, _planes, _bpp, size, offset = struct.unpack(
            "<BBBBHHII", ico[at : at + 16]
        )
        # Zero means 256 in the directory: the field is one byte and 256 does
        # not fit in it. The embedded PNG may be larger still, and its own
        # header is what actually says how big it is.
        width = width or 256
        height = height or 256
        if offset + size > len(ico):
            raise SystemExit("an ICO entry points past the end of the file")
        if not ico[offset : offset + 8] == PNG_MAGIC:
            continue
        area = width * height
        if best is None or area > best[0]:
            best = (area, ico[offset : offset + size])

    if best is None:
        raise SystemExit(
            "no PNG image in this ICO; only the PNG-in-ICO form is supported"
        )
    return best[1]


# ICNS chunk types that carry a PNG, by the square size they declare.
#
# Apple's format is a magic, a length, and then typed chunks — and since OS X
# 10.7 a chunk may hold a PNG directly rather than the packed RGB and mask the
# older types needed. Only the sizes that exist here are listed: writing a
# chunk that claims a size the image does not have is how an icon ends up
# blurry in one slot and sharp in another.
ICNS_TYPES = {
    16: b"icp4",
    32: b"icp5",
    64: b"icp6",
    128: b"ic07",
    256: b"ic08",
    512: b"ic09",
    1024: b"ic10",
}


def make_icns(png: bytes) -> bytes:
    """Wraps a square PNG in an Apple icon file."""
    width, height = struct.unpack(">II", png[16:24])
    if width != height:
        raise SystemExit(f"an icon must be square; this one is {width}x{height}")
    kind = ICNS_TYPES.get(width)
    if kind is None:
        known = ", ".join(str(size) for size in sorted(ICNS_TYPES))
        raise SystemExit(f"no ICNS chunk type for {width}x{width}; known sizes: {known}")

    # Chunk length counts its own eight-byte header, and so does the file's.
    chunk = kind + struct.pack(">I", len(png) + 8) + png
    return b"icns" + struct.pack(">I", len(chunk) + 8) + chunk


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit(f"usage: {sys.argv[0]} <input.ico> <output.png|output.icns>")
    source, target = sys.argv[1], sys.argv[2]
    with open(source, "rb") as handle:
        png = extract(handle.read())

    # The PNG's own IHDR, which is authoritative where the ICO directory's
    # single byte is not.
    width, height = struct.unpack(">II", png[16:24])

    if target.endswith(".icns"):
        payload = make_icns(png)
        described = f"{width}x{height} ICNS"
    else:
        payload = png
        described = f"{width}x{height} PNG"

    with open(target, "wb") as handle:
        handle.write(payload)
    print(f"{target}: {described}, {len(payload)} bytes")


if __name__ == "__main__":
    main()
