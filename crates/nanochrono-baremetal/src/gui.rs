// SPDX-License-Identifier: Apache-2.0
//! The interface, on a machine with no window system.
//!
//! # What this is, and what it is not
//!
//! The desktop build's GUI is `iced` drawing through `wgpu` onto a surface
//! `winit` obtained from a window server. None of that exists here: there is
//! no GPU driver, no compositor, no window and no event loop to hook into. So
//! this is **not the same code** as the Windows, Linux and macOS GUI, and no
//! amount of arrangement would make it so.
//!
//! What it is: the same *design*, drawn pixel by pixel. Same palette, same
//! panel structure, same readouts in the same order, so the freestanding
//! build is recognisably the same application. The one deliberate difference
//! is in the corner where a desktop window puts minimise and close: there is
//! no window to minimise and nothing to close to, so those two become restart
//! and shut down, through [`crate::acpi`].
//!
//! # Why it is a fixed frame rather than a loop
//!
//! Everything measured here is measured once, at boot, with interrupts masked
//! and nothing else running. Re-measuring on a timer would need an interrupt
//! controller and a timer handler, which would then be part of what the
//! numbers include. The display holds the boot measurement and the keyboard
//! is polled — which costs nothing when no key is pressed.

use crate::acpi;
use crate::draw::{self, Palette};
use crate::font;
use crate::framebuffer::Framebuffer;
use crate::input::{Event, Input, Motion};
use crate::pmu::{CorePmu, CounterRoute};
use nanochrono_core::arch as counters;

const SCAN_R: u8 = 0x13;
const SCAN_S: u8 = 0x1F;

/// Where the two controls are, so a click can be tested against them.
#[derive(Clone, Copy)]
struct Hitbox {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

impl Hitbox {
    fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x as i32
            && y >= self.y as i32
            && x < (self.x + self.w) as i32
            && y < (self.y + self.h) as i32
    }
}

/// A number rendered without an allocator.
struct Num {
    bytes: [u8; 24],
    len: usize,
}

impl Num {
    fn new(mut value: u64) -> Num {
        let mut n = Num {
            bytes: [0; 24],
            len: 0,
        };
        if value == 0 {
            n.bytes[0] = b'0';
            n.len = 1;
            return n;
        }
        // Built backwards, then reversed: division yields the least
        // significant digit first.
        let mut tmp = [0u8; 24];
        let mut i = 0;
        while value > 0 && i < tmp.len() {
            tmp[i] = b'0' + (value % 10) as u8;
            value /= 10;
            i += 1;
        }
        for j in 0..i {
            n.bytes[j] = tmp[i - 1 - j];
        }
        n.len = i;
        n
    }

    fn as_str(&self) -> &str {
        // Always ASCII digits by construction.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("?")
    }
}

/// Draws the interface and polls for the two controls. Never returns.
///
/// # Safety
/// Reads I/O ports and writes the framebuffer; requires ring 0.
pub unsafe fn run(fb: &Framebuffer) -> ! {
    let p = Palette::APP;
    // SAFETY: forwarded from this function's own contract.
    let power = unsafe { acpi::power_registers() };

    // SAFETY: forwarded from this function's own contract.
    let mut input = unsafe { Input::init() };

    fb.clear(p.background);
    let (restart, shutdown) = title_bar(fb, &p);
    // SAFETY: as above; the panels measure through privileged instructions.
    unsafe { panels(fb, &p) };
    status_bar(fb, &p, power.is_some(), &input);

    // The cursor starts centred, the way a display server places it.
    let mut cursor = Cursor::new(fb, fb.width as i32 / 2, fb.height as i32 / 2);
    if input.pointer_kind() != crate::input::PointerKind::None {
        cursor.draw(fb, &p);
    }

    loop {
        // SAFETY: forwarded from this function's own contract.
        let Some(event) = (unsafe { input.poll() }) else {
            core::hint::spin_loop();
            continue;
        };

        match event {
            Event::Key(k) if k.pressed && k.scancode == SCAN_R => {
                // SAFETY: as above.
                unsafe { acpi::reboot(power.as_ref()) }
            }
            Event::Key(k) if k.pressed && k.scancode == SCAN_S => {
                // SAFETY: as above.
                unsafe { power_off(fb, &p, power.as_ref()) }
            }
            Event::Key(_) => {}
            Event::Motion(m) => {
                cursor.step(fb, &p, m);
                if m.left {
                    if restart.contains(cursor.x, cursor.y) {
                        // SAFETY: as above.
                        unsafe { acpi::reboot(power.as_ref()) }
                    }
                    if shutdown.contains(cursor.x, cursor.y) {
                        // SAFETY: as above.
                        unsafe { power_off(fb, &p, power.as_ref()) }
                    }
                }
            }
        }
    }
}

/// # Safety
/// Requires ring 0.
unsafe fn power_off(fb: &Framebuffer, p: &Palette, power: Option<&acpi::PowerRegisters>) {
    // SAFETY: forwarded from this function's own contract.
    unsafe { acpi::shutdown(power) };
    // Every method returned, so none of them worked. Saying so beats a
    // machine that looks hung for no stated reason.
    draw::text(
        fb,
        24,
        fb.height.saturating_sub(24),
        "shutdown: no method this platform answers",
        p.accent,
        2,
    );
}

/// A software cursor: the framebuffer has no hardware overlay, so the pixels
/// under it are saved and restored as it moves.
struct Cursor {
    x: i32,
    y: i32,
    /// What was on screen before the cursor was drawn there.
    under: [u32; (CURSOR * CURSOR) as usize],
    drawn: bool,
}

/// Cursor side, in pixels. Square and small: every move copies this many
/// pixels twice, and it is drawn from a polled loop.
const CURSOR: u32 = 10;

impl Cursor {
    fn new(_fb: &Framebuffer, x: i32, y: i32) -> Cursor {
        Cursor {
            x,
            y,
            under: [0; (CURSOR * CURSOR) as usize],
            drawn: false,
        }
    }

    fn step(&mut self, fb: &Framebuffer, p: &Palette, m: Motion) {
        self.erase(fb);
        // Clamped rather than wrapped: a cursor that leaves one edge and
        // appears at the other is not a cursor.
        self.x = (self.x + m.dx).clamp(0, fb.width as i32 - 1);
        self.y = (self.y + m.dy).clamp(0, fb.height as i32 - 1);
        self.draw(fb, p);
    }

    fn draw(&mut self, fb: &Framebuffer, p: &Palette) {
        for row in 0..CURSOR {
            for col in 0..CURSOR {
                // A triangle, which reads as a pointer where a square does
                // not.
                if col > row {
                    continue;
                }
                let x = self.x as u32 + col;
                let y = self.y as u32 + row;
                self.under[(row * CURSOR + col) as usize] = fb.get(x, y);
                let edge = col == 0 || col == row;
                fb.set(x, y, if edge { p.background } else { p.accent });
            }
        }
        self.drawn = true;
    }

    fn erase(&mut self, fb: &Framebuffer) {
        if !self.drawn {
            return;
        }
        for row in 0..CURSOR {
            for col in 0..CURSOR {
                if col > row {
                    continue;
                }
                fb.set(
                    self.x as u32 + col,
                    self.y as u32 + row,
                    self.under[(row * CURSOR + col) as usize],
                );
            }
        }
        self.drawn = false;
    }
}

/// The title bar, and the two controls that replace minimise and close.
///
/// Returns their hitboxes so a click can be tested against them.
fn title_bar(fb: &Framebuffer, p: &Palette) -> (Hitbox, Hitbox) {
    let h = 64;
    fb.fill(0, 0, fb.width, h, p.panel);
    fb.fill(0, h - 1, fb.width, 1, p.button_edge);

    // Scaled to the surface: at 800x600 a triple-size title plus a subtitle
    // runs straight into the controls on the right.
    let title_scale = if fb.width >= 1024 { 3 } else { 2 };
    draw::text(fb, 24, 22, "NANOCHRONOMETER", p.title, title_scale);
    let subtitle_x = 24 + 15 * (font::WIDTH + 1) * title_scale + 20;
    // Only if it fits before the controls, which own the last 150 pixels.
    if subtitle_x + 12 * 9 * 2 < fb.width.saturating_sub(150) {
        draw::text(fb, subtitle_x, 28, "FREESTANDING", p.accent, 2);
    }

    // Where a desktop window would put minimise and close. Restart and shut
    // down instead: there is no window manager to minimise into and nothing
    // to close to.
    let bw = 56;
    let bh = 40;
    let y = (h - bh) / 2;
    let restart_x = fb.width.saturating_sub(bw * 2 + 36);
    let off_x = fb.width.saturating_sub(bw + 20);

    fb.fill(restart_x, y, bw, bh, p.button);
    fb.outline(restart_x, y, bw, bh, p.button_edge);
    glyph_restart(fb, restart_x + bw / 2, y + bh / 2, p.title);

    fb.fill(off_x, y, bw, bh, p.button);
    fb.outline(off_x, y, bw, bh, p.button_edge);
    glyph_power(fb, off_x + bw / 2, y + bh / 2, p.accent);

    (
        Hitbox {
            x: restart_x,
            y,
            w: bw,
            h: bh,
        },
        Hitbox {
            x: off_x,
            y,
            w: bw,
            h: bh,
        },
    )
}

/// A circular arrow: restart.
///
/// Drawn rather than loaded. `assets/` holds an `.ico` and an `.svg`, and
/// decoding either would mean a PNG or SVG parser in a kernel — far more
/// code, and more attack surface, than two dozen plotted pixels.
fn glyph_restart(fb: &Framebuffer, cx: u32, cy: u32, colour: u32) {
    let r = 10i32;
    // Three quarters of a circle, leaving a gap for the arrow head.
    for step in 0..48 {
        let a = step as f32 * 0.13;
        if !(0.6..5.6).contains(&a) {
            continue;
        }
        let (sin, cos) = sin_cos(a);
        let x = cx as i32 + (cos * r as f32) as i32;
        let y = cy as i32 + (sin * r as f32) as i32;
        fb.fill(x.max(0) as u32, y.max(0) as u32, 2, 2, colour);
    }
    // The arrow head, at the open end.
    fb.fill(cx + 6, cy - 12, 8, 2, colour);
    fb.fill(cx + 12, cy - 12, 2, 8, colour);
}

/// A power symbol: a broken ring with a stem.
fn glyph_power(fb: &Framebuffer, cx: u32, cy: u32, colour: u32) {
    let r = 10i32;
    for step in 0..48 {
        let a = step as f32 * 0.13;
        // Gap at the top, where the stem goes.
        if (4.0..5.4).contains(&a) {
            continue;
        }
        let (sin, cos) = sin_cos(a);
        let x = cx as i32 + (cos * r as f32) as i32;
        let y = cy as i32 + (sin * r as f32) as i32;
        fb.fill(x.max(0) as u32, y.max(0) as u32, 2, 2, colour);
    }
    fb.fill(cx - 1, cy - 13, 3, 12, colour);
}

/// Sine and cosine by Taylor series.
///
/// `libm` is not linked and `core` has no floating-point maths. Five terms is
/// far more precision than a twenty-pixel circle can show.
fn sin_cos(a: f32) -> (f32, f32) {
    // Reduce to [-pi, pi] so the series stays accurate.
    const TAU: f32 = 6.283_185_5;
    let mut x = a;
    while x > TAU / 2.0 {
        x -= TAU;
    }
    while x < -TAU / 2.0 {
        x += TAU;
    }
    let x2 = x * x;
    let sin = x * (1.0 - x2 / 6.0 * (1.0 - x2 / 20.0 * (1.0 - x2 / 42.0)));
    let cos = 1.0 - x2 / 2.0 * (1.0 - x2 / 12.0 * (1.0 - x2 / 30.0));
    (sin, cos)
}

/// The three readout panels, in the order the desktop GUI shows them.
///
/// # Safety
/// Programs and reads the PMU; requires ring 0.
unsafe fn panels(fb: &Framebuffer, p: &Palette) {
    let margin = 24;
    let top = 96;
    let gap = 20;
    let panel_w = (fb.width - margin * 2 - gap * 2) / 3;
    let panel_h = fb.height.saturating_sub(top + 96);

    // --- CPU
    let mut y = panel(fb, p, margin, top, panel_w, panel_h, "CPU");
    let f = nanochrono_core::cpu::features();
    y = row(
        fb,
        p,
        margin,
        panel_w,
        y,
        "backend",
        nanochrono_core::Backend::best().name(),
    );
    #[cfg(target_arch = "x86_64")]
    {
        y = row(fb, p, margin, panel_w, y, "avx2", yes_no(f.avx2));
        y = row(fb, p, margin, panel_w, y, "avx-512f", yes_no(f.avx512f));
        y = row(fb, p, margin, panel_w, y, "aes-ni", yes_no(f.aesni));
        y = row(fb, p, margin, panel_w, y, "sha-ni", yes_no(f.shani));
        y = row(
            fb,
            p,
            margin,
            panel_w,
            y,
            "invariant tsc",
            yes_no(f.invariant_counter),
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        y = row(fb, p, margin, panel_w, y, "neon", yes_no(f.neon));
        y = row(fb, p, margin, panel_w, y, "sve2", yes_no(f.sve2));
        y = row(fb, p, margin, panel_w, y, "sme", yes_no(f.sme));
    }
    let _ = y;

    // --- PMU
    let x2 = margin + panel_w + gap;
    let mut y = panel(fb, p, x2, top, panel_w, panel_h, "PMU (DIRECT)");
    let mut pmu = CorePmu::detect();
    y = row(fb, p, x2, panel_w, y, "core type", pmu.core_type.name());
    y = row(
        fb,
        p,
        x2,
        panel_w,
        y,
        "version",
        Num::new(pmu.leaf.version as u64).as_str(),
    );
    y = row(
        fb,
        p,
        x2,
        panel_w,
        y,
        "fixed ctrs",
        Num::new(pmu.leaf.fixed_counters as u64).as_str(),
    );

    if pmu.leaf.is_available() {
        // SAFETY: forwarded from this function's own contract.
        let route = unsafe { pmu.enable() };
        y = row(fb, p, x2, panel_w, y, "route", route.name());
        if route != CounterRoute::None {
            const ITERATIONS: u64 = 100_000;
            let mut acc = 0u64;
            // SAFETY: the PMU was just enabled on this core, at ring 0.
            let (out, cycles) = unsafe {
                pmu.measure(|| {
                    for i in 0..ITERATIONS {
                        acc = acc.wrapping_add(i).rotate_left(3);
                    }
                    acc
                })
            };
            core::hint::black_box(out);
            if let Some(c) = cycles {
                y = row(fb, p, x2, panel_w, y, "cycles", Num::new(c).as_str());
                y = row(
                    fb,
                    p,
                    x2,
                    panel_w,
                    y,
                    "cycles/op",
                    Num::new(c / ITERATIONS).as_str(),
                );
            }
        }
    } else {
        y = row(fb, p, x2, panel_w, y, "route", "unavailable");
    }
    let _ = y;

    // --- Counter
    let x3 = margin + (panel_w + gap) * 2;
    let mut y = panel(fb, p, x3, top, panel_w, panel_h, "COUNTER");
    const ROUNDS: u32 = 1024;
    let mut min = u64::MAX;
    let mut max = 0;
    for _ in 0..ROUNDS {
        let a = crate::arch::counter_ordered();
        let b = crate::arch::counter_ordered();
        let d = b.wrapping_sub(a);
        min = min.min(d);
        max = max.max(d);
    }
    y = row(
        fb,
        p,
        x3,
        panel_w,
        y,
        "read overhead",
        Num::new(min).as_str(),
    );
    y = row(fb, p, x3, panel_w, y, "worst read", Num::new(max).as_str());
    y = row(
        fb,
        p,
        x3,
        panel_w,
        y,
        "jitter",
        Num::new(max - min).as_str(),
    );
    #[cfg(target_arch = "aarch64")]
    {
        y = row(
            fb,
            p,
            x3,
            panel_w,
            y,
            "cntfrq hz",
            Num::new(counters::aarch64::cntfrq()).as_str(),
        );
    }
    let _ = (y, counters::ARCH);
}

fn yes_no(v: bool) -> &'static str {
    if v {
        "yes"
    } else {
        "no"
    }
}

/// Draws a panel frame and returns the y to start its rows at.
fn panel(fb: &Framebuffer, p: &Palette, x: u32, y: u32, w: u32, h: u32, title: &str) -> u32 {
    fb.fill(x, y, w, h, p.panel);
    fb.outline(x, y, w, h, p.button_edge);
    draw::text(fb, x + 16, y + 14, title, p.accent, 2);
    fb.fill(x + 16, y + 38, w - 32, 1, p.muted);
    y + 54
}

/// One label/value row, with the value right-aligned inside the panel.
///
/// The column is derived from the panel width rather than fixed: a fixed
/// offset wider than the panel puts every value outside it, which is exactly
/// what a first attempt at this did.
fn row(fb: &Framebuffer, p: &Palette, x: u32, w: u32, y: u32, label: &str, value: &str) -> u32 {
    let advance = (font::WIDTH + 1) * SMALL;
    let value_w = value.len() as u32 * advance;
    let value_x = (x + w).saturating_sub(16 + value_w).max(x + 16);

    // The value is what the row exists to show, so it keeps its place and the
    // label gives way. Without this the two are simply drawn over each other,
    // which is legible as neither.
    let label_room = value_x.saturating_sub(x + 16 + advance) / advance;
    let label = truncate(label, label_room as usize);

    draw::text(fb, x + 16, y, label, p.muted, SMALL);
    draw::text(fb, value_x, y, value, p.text, SMALL);
    y + font::HEIGHT * SMALL + 10
}

/// Shortens `s` to `columns`, marking it with a `.` so a cut is visible.
fn truncate(s: &str, columns: usize) -> &str {
    if s.len() <= columns {
        return s;
    }
    // A cut label is better than an overlapping one, and `is_char_boundary`
    // keeps the slice valid if a label ever carries non-ASCII.
    let mut end = columns;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Text scale for panel rows. One place, so the row height and the value
/// column cannot disagree about it.
const SMALL: u32 = 2;

/// The bar along the bottom, carrying the same verdict line as the desktop
/// build's status bar.
fn status_bar(fb: &Framebuffer, p: &Palette, acpi_present: bool, input: &Input) {
    let h = 56;
    let y = fb.height.saturating_sub(h);
    fb.fill(0, y, fb.width, h, p.panel);
    fb.fill(0, y, fb.width, 1, p.button_edge);

    // The pointer is named rather than assumed: on a machine whose firmware
    // does not translate USB to 8042 there is none, and a cursor that never
    // appears with no explanation is worse than a line saying why.
    let keys = match (acpi_present, input.pointer_kind()) {
        (true, crate::input::PointerKind::None) => "[R] restart   [S] shut down",
        (true, _) => "[R] / click   [S] / click",
        (false, _) => "[R] restart   [S] off (no acpi)",
    };
    let advance = (font::WIDTH + 1) * 2;
    let keys_w = keys.len() as u32 * advance;
    let keys_x = fb.width.saturating_sub(keys_w + 24);
    draw::text(fb, keys_x, y + 20, keys, p.text, 2);

    // The longest note that still fits beside the keys, or none. Checked
    // against the *chosen* string rather than the longest one, which is how
    // a first attempt still drew them over each other.
    for note in [input.pointer_kind().name(), "no operating system", ""] {
        if note.is_empty() {
            break;
        }
        if 24 + note.len() as u32 * advance + 24 <= keys_x {
            draw::text(fb, 24, y + 20, note, p.muted, 2);
            break;
        }
    }
}
