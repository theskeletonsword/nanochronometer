// SPDX-License-Identifier: Apache-2.0
//! The screen a freestanding kernel shows when it cannot continue.
//!
//! Green rather than blue, and it stops rather than restarting. A kernel that
//! reboots on panic loses the one thing worth having — what went wrong — and
//! a machine that reboots into the same fault does it forever. This waits,
//! shows the reason, and lets the operator choose.
//!
//! There is no window manager and no mouse, so the two controls are drawn as
//! buttons and driven from the keyboard: `R` restarts, `S` powers off. Both go
//! through [`crate::acpi`].

use crate::acpi;
use crate::arch::x86::inb;
use crate::draw::{self, Palette};
use crate::framebuffer::Framebuffer;
use crate::typeface::{BODY, TITLE};

/// The 8042 keyboard controller's data and status ports.
const PS2_DATA: u16 = 0x60;
const PS2_STATUS: u16 = 0x64;

/// Set 1 scancodes for the keys this screen listens for.
const SCAN_R: u8 = 0x13;
const SCAN_S: u8 = 0x1F;

/// Draws the stop screen and waits for a choice. Never returns.
///
/// # Safety
/// Reads I/O ports and writes the framebuffer; requires ring 0.
pub unsafe fn show(fb: Option<&Framebuffer>, reason: &str) -> ! {
    // SAFETY: forwarded from this function's own contract.
    let power = unsafe { acpi::power_registers() };

    match fb.filter(|f| f.is_usable()) {
        // SAFETY: as above.
        Some(fb) => unsafe { graphical(fb, reason, power.as_ref()) },
        // No graphics mode. The serial port has already carried the reason,
        // so the only thing left is to offer the same choice over it.
        // SAFETY: as above.
        None => unsafe { serial_only(power.as_ref()) },
    }
}

/// # Safety
/// Requires ring 0.
unsafe fn graphical(fb: &Framebuffer, reason: &str, power: Option<&acpi::PowerRegisters>) -> ! {
    let p = Palette::STOP;
    fb.clear(p.background);

    let w = fb.width;

    // A header band, the way the desktop GUI carries its title bar.
    draw::gradient(fb, 0, 0, w, 76, p.header_from, p.header_to);
    fb.fill(0, 75, w, 1, p.divider);
    draw::text(fb, &TITLE, 28, 20, "Stopped", p.title);

    let mut y = 116;
    draw::text(fb, &BODY, 28, y, "The kernel cannot continue.", p.text);
    y += BODY.line_height as u32 + 10;
    draw::text(
        fb,
        &BODY,
        28,
        y,
        "It is waiting rather than restarting, so the reason below is not",
        p.muted,
    );
    y += BODY.line_height as u32;
    draw::text(fb, &BODY, 28, y, "lost to a reboot loop.", p.muted);

    y += 42;
    draw::text(fb, &BODY, 28, y, "REASON", p.accent);
    y += BODY.line_height as u32 + 8;
    // The reason can be long; wrap it rather than running off the edge.
    // Characters per line is estimated from the widest common glyph, since
    // the face is proportional and an exact fit would need measuring twice.
    let columns = ((w - 56) / BODY.width_of("m").max(1)) as usize;
    for chunk in draw::wrap(reason, columns.max(16)) {
        draw::text(fb, &BODY, 28, y, chunk, p.text);
        y += BODY.line_height as u32;
    }

    // The two controls, in the corner the desktop GUI puts its window buttons
    // — except these restart and power off, because on bare metal there is no
    // window to minimise and nothing to close to.
    let button_w = 240;
    let button_h = 56;
    let by = fb.height.saturating_sub(button_h + 40);
    let reboot_x = w.saturating_sub(button_w * 2 + 60);
    let off_x = w.saturating_sub(button_w + 30);

    draw::button(fb, reboot_x, by, button_w, button_h, "R    Restart", &p);
    draw::button(fb, off_x, by, button_w, button_h, "S    Shut down", &p);

    if power.is_none() {
        draw::text(
            fb,
            &BODY,
            28,
            by + 18,
            "no ACPI tables found; using fallbacks",
            p.muted,
        );
    }

    // SAFETY: forwarded from this function's own contract.
    unsafe { wait_for_choice(power) }
}

/// # Safety
/// Requires ring 0.
unsafe fn serial_only(power: Option<&acpi::PowerRegisters>) -> ! {
    crate::println!();
    crate::println!("=== STOPPED ===");
    crate::println!("No graphics mode; not restarting.");
    crate::println!("  R = restart      S = shut down");
    // SAFETY: forwarded from this function's own contract.
    unsafe { wait_for_choice(power) }
}

/// Polls the keyboard until one of the two keys is pressed.
///
/// # Safety
/// Reads I/O ports; requires ring 0.
unsafe fn wait_for_choice(power: Option<&acpi::PowerRegisters>) -> ! {
    loop {
        // Status bit 0 is "output buffer full", meaning a byte is waiting.
        // SAFETY: reading the 8042 status port has no side effects.
        if unsafe { inb(PS2_STATUS) } & 1 == 0 {
            core::hint::spin_loop();
            continue;
        }
        // SAFETY: the status bit says a byte is there to read.
        let code = unsafe { inb(PS2_DATA) };

        // Bit 7 marks a release; only presses count.
        match code {
            SCAN_R => {
                crate::println!("restarting");
                // SAFETY: forwarded from this function's own contract.
                unsafe { acpi::reboot(power) }
            }
            SCAN_S => {
                crate::println!("shutting down");
                // SAFETY: as above.
                unsafe { acpi::shutdown(power) };
                // Every method failed. Saying so beats a machine that looks
                // hung for no stated reason.
                crate::println!("shutdown failed: no method this platform answers");
                crate::arch::halt()
            }
            _ => {}
        }
    }
}
