// SPDX-License-Identifier: Apache-2.0
//! The VGA text console, for when there is no framebuffer.
//!
//! A last resort, and the difference between a diagnosable failure and a
//! machine that appears never to have started.
//!
//! If the loader hands over a text mode rather than a linear framebuffer —
//! because `gfxpayload` was not set, because the firmware refused the mode,
//! because the machine is old — then a kernel that can only draw pixels has
//! nothing to draw on. With no serial port either, it halts silently and the
//! loader's last message stays on screen.
//!
//! This is not a fallback interface. It prints the report, and it says why
//! the interface is not there.
//!
//! BIOS only: a UEFI machine has no VGA text mode, and writing to `0xB8000`
//! there reaches nothing. Which is safe — it is RAM either way — but silent.

/// The text buffer every PC has had at this address since the EGA.
const VGA_BUFFER: usize = 0x000B_8000;
const WIDTH: usize = 80;
const HEIGHT: usize = 25;

/// Light grey on black, the attribute byte a BIOS leaves behind.
const ATTRIBUTE: u16 = 0x0700;

/// Where the next character goes.
///
/// A `static mut` because this is reached from the print macro, which takes
/// no context. Single core, interrupts masked, and nothing else touches it.
static mut CURSOR: usize = 0;

/// Whether the text console is in use.
static mut ACTIVE: bool = false;

/// Turns the text console on and clears it.
///
/// # Safety
/// Writes the VGA text buffer; requires ring 0 and a BIOS-mode machine.
pub unsafe fn activate() {
    // SAFETY: forwarded from this function's own contract.
    unsafe {
        for i in 0..WIDTH * HEIGHT {
            core::ptr::write_volatile((VGA_BUFFER as *mut u16).add(i), ATTRIBUTE | b' ' as u16);
        }
        CURSOR = 0;
        ACTIVE = true;
    }
}

/// Whether output is going to the text console.
pub fn is_active() -> bool {
    // SAFETY: single core, interrupts masked; written once by `activate`.
    unsafe { ACTIVE }
}

/// Writes one byte, scrolling at the bottom.
pub fn put(byte: u8) {
    if !is_active() {
        return;
    }
    // SAFETY: single core with interrupts masked, and every index below is
    // bounded by the buffer's own dimensions.
    unsafe {
        if byte == b'\n' {
            CURSOR = (CURSOR / WIDTH + 1) * WIDTH;
        } else if byte == b'\r' {
            CURSOR = CURSOR / WIDTH * WIDTH;
        } else {
            core::ptr::write_volatile(
                (VGA_BUFFER as *mut u16).add(CURSOR),
                ATTRIBUTE | byte as u16,
            );
            CURSOR += 1;
        }

        if CURSOR >= WIDTH * HEIGHT {
            scroll();
            CURSOR = WIDTH * (HEIGHT - 1);
        }
    }
}

/// Moves everything up one line.
///
/// # Safety
/// Requires the buffer to be writable.
unsafe fn scroll() {
    // SAFETY: forwarded from this function's own contract; both indices stay
    // inside the buffer.
    unsafe {
        let buffer = VGA_BUFFER as *mut u16;
        for row in 1..HEIGHT {
            for col in 0..WIDTH {
                let value = core::ptr::read_volatile(buffer.add(row * WIDTH + col));
                core::ptr::write_volatile(buffer.add((row - 1) * WIDTH + col), value);
            }
        }
        for col in 0..WIDTH {
            core::ptr::write_volatile(
                buffer.add((HEIGHT - 1) * WIDTH + col),
                ATTRIBUTE | b' ' as u16,
            );
        }
    }
}
