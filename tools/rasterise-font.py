#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
"""Turns a TrueType font into coverage bitmaps the freestanding kernel can draw.

A `.ttf` is an outline format: rendering one needs a rasteriser, hinting and an
allocator, none of which exist in a kernel with no operating system under it.
So the outlines are turned into tables here, before the kernel is built, and
what ships is a lookup.

Coverage is eight bits per pixel rather than one so glyphs antialias against
whatever they are drawn over — most of the difference between text that looks
like a bitmap font and text that looks like a typeface.

Sizes carry a character range rather than all sharing one. The readout face is
large enough that a full ASCII table would be most of a megabyte of kernel
image, and a clock only ever shows digits and separators — so that size covers
exactly those and nothing else.

Usage:
    tools/rasterise-font.py [font.ttf] [output.rs]

Regenerate after changing the font; the output is committed so a normal build
needs neither Python nor Pillow.
"""
import sys
import pathlib
from PIL import ImageFont, Image, ImageDraw

# (pixel size, Rust name, first char, last char)
#
# READOUT is the big elapsed-time display. Its range starts at '.' (0x2E) and
# ends at ':' (0x3A), which covers `. / 0-9 :` — every character a
# `hh:mm:ss:mmm:uuu:sss` readout can contain, and thirteen glyphs rather than
# ninety-five. There are two of them because the readout is prebaked and a
# bitmap cannot be scaled: the wider one is chosen at run time on a panel
# with room for it, which is how the display fills a 1080p screen without
# overflowing an 800x600 one.
SIZES = [
    (16, "BODY", 32, 126),
    (28, "TITLE", 32, 126),
    (22, "HEADING", 32, 126),
    (86, "READOUT", 0x2E, 0x3A),
    (150, "READOUT_BIG", 0x2E, 0x3A),
]

ttf = sys.argv[1] if len(sys.argv) > 1 else "assets/font/Nanoplex.ttf"
dest = sys.argv[2] if len(sys.argv) > 2 else "crates/nanochrono-baremetal/src/typeface.rs"


def clamp(value, low, high):
    return max(low, min(high, value))


def rasterise(font, ch):
    """Returns (coverage bytes, width, height, left, top, advance) for `ch`.

    `left` and `top` are the glyph box relative to the pen and the baseline,
    the same convention FreeType uses, because that is what lets a caller lay
    out by line box rather than by glyph box.
    """
    text = chr(ch)
    # `getbbox` gives the inked box relative to the origin with the baseline
    # at `ascent`; rendering into a padded canvas and cropping is more robust
    # across Pillow versions than trusting the metrics alone.
    ascent, descent = font.getmetrics()
    advance = int(round(font.getlength(text)))

    pad = 8
    canvas_w = advance + pad * 2 + 32
    canvas_h = ascent + descent + pad * 2
    image = Image.new("L", (canvas_w, canvas_h), 0)
    ImageDraw.Draw(image).text((pad, pad), text, font=font, fill=255)

    box = image.getbbox()
    if box is None:
        # A space: no ink, but the pen still moves.
        return b"", 0, 0, 0, 0, advance

    x0, y0, x1, y1 = box
    glyph = image.crop(box)
    left = x0 - pad
    # Baseline sits at `pad + ascent` in canvas coordinates; `top` is how far
    # the ink rises above it.
    top = (pad + ascent) - y0
    return glyph.tobytes(), x1 - x0, y1 - y0, left, top, advance


def emit(out, size, name, first, last, font):
    coverage = bytearray()
    glyphs = []
    for ch in range(first, last + 1):
        bits, w, h, left, top, advance = rasterise(font, ch)
        offset = len(coverage)
        coverage.extend(bits)
        glyphs.append((offset, w, h, left, top, advance, ch))

    ascent, descent = font.getmetrics()

    out.append(f"#[rustfmt::skip]")
    out.append(f"static {name}_GLYPHS: [Glyph; {len(glyphs)}] = [")
    for offset, w, h, left, top, advance, ch in glyphs:
        shown = chr(ch) if 33 <= ch <= 126 else "space"
        out.append(
            f"    Glyph {{ offset: {offset}, width: {clamp(w, 0, 255)}, "
            f"height: {clamp(h, 0, 255)}, left: {clamp(left, -128, 127)}, "
            f"top: {clamp(top, -128, 127)}, advance: {clamp(advance, 0, 255)} }}, // {shown}"
        )
    out.append("];")
    out.append("")

    out.append(f"#[rustfmt::skip]")
    out.append(f"static {name}_COVERAGE: [u8; {len(coverage)}] = [")
    for i in range(0, len(coverage), 32):
        row = ",".join(str(b) for b in coverage[i : i + 32])
        out.append(f"    {row},")
    out.append("];")
    out.append("")

    out.append(f"/// Nanoplex at {size} px, covering {first:#04x}..={last:#04x}.")
    out.append(f"pub static {name}: Face = Face {{")
    out.append(f"    glyphs: &{name}_GLYPHS,")
    out.append(f"    coverage: &{name}_COVERAGE,")
    out.append(f"    ascent: {ascent},")
    out.append(f"    line_height: {ascent + descent},")
    out.append(f"    first: {first},")
    out.append("};")
    out.append("")
    return len(coverage)


def main():
    print(f"rasterising {ttf} -> {dest}")
    out = [
        "// SPDX-License-Identifier: Apache-2.0",
        "//! Nanoplex, rasterised at build time.",
        "//!",
        "//! A `.ttf` is an outline format: drawing from one needs a rasteriser,",
        "//! hinting and an allocator, none of which exist in a freestanding kernel.",
        "//! So the outlines are turned into coverage bitmaps by",
        "//! `tools/rasterise-font.py` before the kernel is built, and what ships is",
        "//! a table.",
        "//!",
        "//! Coverage is eight bits per pixel rather than one, so glyphs are",
        "//! antialiased against whatever they are drawn over. That is most of the",
        "//! difference between text that looks like a bitmap font and text that",
        "//! looks like a typeface.",
        "//!",
        "//! Each face carries its own `first` character rather than sharing one:",
        "//! the readout size is large enough that a full ASCII table would be most",
        "//! of a megabyte of kernel image, and a clock only ever shows digits and",
        "//! separators.",
        "//!",
        "//! Generated. Do not edit by hand.",
        "//!",
        "//! The font is third-party and not Apache-2.0 — see",
        "//! `assets/font/CREDITS.md`.",
        "",
        "/// One glyph: coverage, its box, and where the pen goes next.",
        "#[derive(Debug, Clone, Copy)]",
        "pub struct Glyph {",
        "    /// Offset into the size's coverage blob.",
        "    pub offset: u32,",
        "    pub width: u8,",
        "    pub height: u8,",
        "    /// Where the box sits relative to the pen and the baseline.",
        "    pub left: i8,",
        "    pub top: i8,",
        "    /// How far the pen moves after drawing, which is not the box width —",
        "    /// using the box width instead is what makes proportional text look",
        "    /// like it was kerned by accident.",
        "    pub advance: u8,",
        "}",
        "",
        "/// A rasterised size.",
        "#[derive(Debug, Clone, Copy)]",
        "pub struct Face {",
        "    pub glyphs: &'static [Glyph],",
        "    pub coverage: &'static [u8],",
        "    pub ascent: u8,",
        "    pub line_height: u8,",
        "    /// First character this face's table covers.",
        "    pub first: u8,",
        "}",
        "",
        "impl Face {",
        "    /// The glyph for `ch`, or the first in the table if it is outside it.",
        "    ///",
        "    /// A face that does not cover the whole of ASCII is normal here, so an",
        "    /// out-of-range character is a layout question rather than a bug: the",
        "    /// fallback keeps the pen moving instead of panicking in a kernel with",
        "    /// nowhere to report a panic to.",
        "    pub fn glyph(&self, ch: u8) -> &Glyph {",
        "        let index = ch.wrapping_sub(self.first) as usize;",
        "        self.glyphs.get(index).unwrap_or(&self.glyphs[0])",
        "    }",
        "",
        "    /// Whether this face has a glyph for `ch`.",
        "    pub fn covers(&self, ch: u8) -> bool {",
        "        (ch.wrapping_sub(self.first) as usize) < self.glyphs.len()",
        "    }",
        "",
        "    /// Width of `s` in pixels, by advance.",
        "    ///",
        "    /// By character, not by byte, so it agrees with what `draw::text`",
        "    /// actually draws. Measuring by byte would give a multi-byte",
        "    /// character three advances and one glyph, and every",
        "    /// right-aligned label containing one would sit wrong.",
        "    pub fn width_of(&self, s: &str) -> u32 {",
        "        s.chars()",
        "            .map(|c| self.glyph(ascii_fallback(c)).advance as u32)",
        "            .sum()",
        "    }",
        "}",
        "",
        "/// The byte a character is drawn as.",
        "///",
        "/// The faces are ASCII tables. Iterating a `&str` by byte would draw",
        "/// one substitute per byte of a multi-byte character — a single em",
        "/// dash coming out as `???`. Iterating by character fixes the count;",
        "/// this fixes what the substitute is, transliterating the few",
        "/// typographic characters an interface actually reaches for rather",
        "/// than replacing them all with a question mark.",
        "pub fn ascii_fallback(ch: char) -> u8 {",
        "    match ch {",
        "        c if c.is_ascii() => c as u8,",
        "        \'\\u{b7}\' | \'\\u{2014}\' | \'\\u{2013}\' | \'\\u{2011}\' => b\'-\',",
        "        \'\\u{d7}\' => b\'x\',",
        "        \'\\u{2248}\' => b\'~\',",
        "        \'\\u{b0}\' => b\'o\',",
        "        \'\\u{b5}\' | \'\\u{3bc}\' => b\'u\',",
        "        \'\\u{201c}\' | \'\\u{201d}\' => b\'\\\'\',",
        "        \'\\u{2026}\' => b\'.\',",
        "        _ => b\'?\',",
        "    }",
        "}",
        "",
    ]

    total = 0
    for size, name, first, last in SIZES:
        font = ImageFont.truetype(ttf, size)
        written = emit(out, size, name, first, last, font)
        total += written
        print(f"  {name:8} {size:3}px  {last - first + 1:3} glyphs  {written:7} bytes")

    pathlib.Path(dest).write_text("\n".join(out) + "\n")
    print(f"total coverage: {total} bytes")
    print("note: the font is third-party; see assets/font/CREDITS.md")


if __name__ == "__main__":
    main()
