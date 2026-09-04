# Font credits

## Nanoplex

`Nanoplex.ttf` is used by the desktop GUI. It is not part of this project's
source and is not covered by its Apache-2.0 licence.

| | |
|---|---|
| Font | **Nanoplex** (Medium) |
| Author | **Jack** — <https://www.dafont.com/jack.d11538> |
| Page | <https://www.dafont.com/nanoplex.font> |
| Copyright string in the file | `Partialism` |

The `name` table in the shipped file carries only the copyright string above —
no licence, licence URL, or vendor URL fields — so the terms are whatever the
dafont listing states at the point of download. **Check that listing before
redistributing this font with a binary.** dafont lists fonts under a range of
terms, and "free for commercial use" there sometimes still requires
attribution or excludes redistribution of the font file itself; the listing is
the authority, not this file.

If the terms turn out to exclude redistribution, the GUI degrades to the
platform's default font without any change to the code — see
[`README.md`](README.md) for how loading works.

## The bare-metal build does not use it

`nanochrono-baremetal` renders with an 8×8 bitmap font written for this
project (`crates/nanochrono-baremetal/src/font.rs`), which is Apache-2.0 with
the rest of the source.

That is not a stylistic choice. A TrueType file is an outline format: drawing
from it needs a glyph rasteriser, hinting, and a memory allocator to hold the
results — none of which exist in a freestanding kernel, and all of which would
be a large amount of code running before anything has been measured. A bitmap
font is a table lookup and a loop over set bits.

It also keeps the licensing simple: a kernel image is redistributed as a
single binary, and embedding a third-party font in it raises exactly the
question the section above cannot answer for you.
