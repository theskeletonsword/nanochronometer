// SPDX-License-Identifier: Apache-2.0
//! Text and widgets on a raw framebuffer.
//!
//! Everything is a loop over pixels: there is no renderer, no glyph cache and
//! no allocator. The palette matches the desktop GUI's so the two look like
//! the same product, which is as close as they can get — see
//! [`crate::framebuffer`] for why the actual `iced` code cannot run here.

use crate::font;
use crate::framebuffer::{Colour, Framebuffer};

/// The colours one screen uses.
///
/// Values taken from `nanochrono-gui`'s `style` module so the freestanding
/// build is recognisably the same application.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub background: Colour,
    pub panel: Colour,
    pub title: Colour,
    pub text: Colour,
    pub muted: Colour,
    pub accent: Colour,
    pub button: Colour,
    pub button_edge: Colour,
}

impl Palette {
    /// The stop screen: green, and deliberately not the blue every other
    /// system uses for the same situation.
    pub const STOP: Palette = Palette {
        background: 0x0011_3322,
        panel: 0x0017_4A33,
        title: 0x00D8_FFE8,
        text: 0x00E6_FFF0,
        muted: 0x0085_C79E,
        accent: 0x0046_E58C,
        button: 0x0020_6644,
        button_edge: 0x0046_E58C,
    };

    /// The measurement interface, matching the desktop GUI's dark theme.
    pub const APP: Palette = Palette {
        background: 0x000C_0F14,
        panel: 0x0014_1A22,
        title: 0x00E8_EEF5,
        text: 0x00D4_DCE6,
        muted: 0x0074_8397,
        accent: 0x0035_D6A0,
        button: 0x001C_2530,
        button_edge: 0x0035_D6A0,
    };
}

/// Draws one line of text, `scale` pixels per font pixel.
pub fn text(fb: &Framebuffer, x: u32, y: u32, s: &str, colour: Colour, scale: u32) {
    let mut cursor = x;
    for byte in s.bytes() {
        // Non-ASCII is rendered as the replacement glyph rather than skipped,
        // so a mangled string is visible rather than silently shortened.
        let glyph = font::glyph(if byte.is_ascii() { byte } else { b'?' });
        for (row, bits) in glyph.iter().enumerate() {
            for col in 0..font::WIDTH {
                if bits & (0x80 >> col) == 0 {
                    continue;
                }
                fb.fill(
                    cursor + col * scale,
                    y + row as u32 * scale,
                    scale,
                    scale,
                    colour,
                );
            }
        }
        cursor += (font::WIDTH + 1) * scale;
        if cursor >= fb.width {
            return;
        }
    }
}

/// Draws a labelled button.
pub fn button(fb: &Framebuffer, x: u32, y: u32, w: u32, h: u32, label: &str, p: &Palette) {
    fb.fill(x, y, w, h, p.button);
    fb.outline(x, y, w, h, p.button_edge);

    // Centre the label. The glyph advance is nine pixels at scale one.
    let scale = 2;
    let text_w = label.len() as u32 * (font::WIDTH + 1) * scale;
    let text_x = x + w.saturating_sub(text_w) / 2;
    let text_y = y + h.saturating_sub(font::HEIGHT * scale) / 2;
    text(fb, text_x, text_y, label, p.title, scale);
}

/// Splits `s` into chunks of at most `columns` characters, at spaces where
/// there are any.
///
/// Returns an iterator rather than a `Vec`: there is no allocator.
pub fn wrap(s: &str, columns: usize) -> impl Iterator<Item = &str> {
    let mut rest = s;
    core::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        if rest.len() <= columns {
            let out = rest;
            rest = "";
            return Some(out);
        }
        // Break at the last space inside the budget; if the word is longer
        // than a line, break it rather than overflow.
        let split = rest[..columns].rfind(' ').map(|i| i + 1).unwrap_or(columns);
        let (line, remainder) = rest.split_at(split);
        rest = remainder;
        Some(line.trim_end())
    })
}
