// SPDX-License-Identifier: Apache-2.0
//! Output with no operating system to print through.
//!
//! There is no `write(2)`, no stdout and no console driver. What there is, on
//! both architectures, is a UART at a known address that needs no
//! initialisation beyond a handful of register writes — so that is where the
//! results go. Under QEMU it lands on the host's terminal with `-serial
//! stdio`, which is what makes this kernel testable at all.

use core::fmt;
use core::sync::atomic::{AtomicBool, Ordering};

/// Whether output is still being attempted.
///
/// Starts true and is only cleared by evidence: a probe that says there is no
/// UART is a *hint*, and a hint that turns out to be wrong would silence the
/// log on a machine that has a perfectly good port. What actually clears this
/// is the transmitter timing out — which cannot be wrong, because it means
/// the byte was not sent.
///
/// The point of clearing it at all is cost. Each timeout is bounded, but
/// paying one per character on a machine with no serial port turns a page of
/// output into a visible stall.
static LIVE: AtomicBool = AtomicBool::new(true);

/// A UART, wherever this architecture keeps one.
pub struct Serial;

impl Serial {
    /// Prepares the port for output.
    ///
    /// # Safety
    /// Touches device registers, so it requires ring 0 / EL1 and assumes
    /// nothing else is driving the same UART.
    pub unsafe fn init() {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: forwarded from this function's own contract.
        unsafe {
            x86_uart::init()
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: forwarded from this function's own contract.
        unsafe {
            pl011::init()
        }
        LIVE.store(true, Ordering::Relaxed);
    }

    /// Whether output is still going anywhere.
    ///
    /// Worth surfacing: on a machine with no serial port the log goes
    /// nowhere, and that is not the same as a kernel that produced none.
    pub fn is_live() -> bool {
        LIVE.load(Ordering::Relaxed)
    }

    fn put(byte: u8) {
        // The VGA text console, when there is no framebuffer to draw on. It
        // is not an alternative to the serial port but a parallel one: a
        // machine with neither has nowhere to report a failure, and that is
        // the case where a fault looks like a boot that never happened.
        #[cfg(target_arch = "x86_64")]
        crate::vga::put(byte);

        if !LIVE.load(Ordering::Relaxed) {
            return;
        }
        #[cfg(target_arch = "x86_64")]
        let sent = x86_uart::put(byte);
        #[cfg(target_arch = "aarch64")]
        let sent = pl011::put(byte);

        if !sent {
            // The transmitter never reported ready. There is no port, or
            // nothing is draining it; either way further attempts would only
            // pay the timeout again.
            LIVE.store(false, Ordering::Relaxed);
        }
    }
}

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            // A bare newline leaves the cursor in column zero on a real
            // terminal, so the carriage return is added here rather than in
            // every caller's format string.
            if byte == b'\n' {
                Serial::put(b'\r');
            }
            Serial::put(byte);
        }
        Ok(())
    }
}

/// Writes a line to the serial port.
///
/// A macro rather than a function because the freestanding build has no
/// allocator: `format_args!` renders straight into the UART without ever
/// building a string.
#[macro_export]
macro_rules! println {
    () => { $crate::serial::_print(format_args!("\n")) };
    ($($arg:tt)*) => {{
        $crate::serial::_print(format_args!($($arg)*));
        $crate::serial::_print(format_args!("\n"));
    }};
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments<'_>) {
    use fmt::Write as _;
    // The error can only come from the formatter, and there is nothing to
    // report it to.
    let _ = Serial.write_fmt(args);
}

#[cfg(target_arch = "x86_64")]
mod x86_uart {
    //! The 16550 UART at COM1.

    use crate::arch::x86::{inb, outb};

    /// The address every PC has had at this port since 1981.
    const COM1: u16 = 0x3F8;

    /// # Safety
    /// Writes device registers; requires CPL 0.
    pub(super) unsafe fn init() {
        // SAFETY: caller guarantees CPL 0. This is the documented 16550
        // initialisation order; writing it out of order leaves the divisor
        // latch open and the port silent.
        unsafe {
            outb(COM1 + 1, 0x00); // no interrupts: there is no handler
            outb(COM1 + 3, 0x80); // DLAB on, so the next two writes set speed
            outb(COM1, 0x01); // divisor 1 => 115200 baud
            outb(COM1 + 1, 0x00);
            outb(COM1 + 3, 0x03); // DLAB off, 8 bits, no parity, 1 stop bit
            outb(COM1 + 2, 0xC7); // enable and clear the FIFOs
            outb(COM1 + 4, 0x0B); // RTS/DSR set
        }
    }

    /// How long to wait for the transmitter, in spin iterations.
    ///
    /// Bounded, and that bound is not a nicety. A laptop has no UART at
    /// `0x3F8`: reading an unassigned port gives `0xFF` on most chipsets,
    /// where bit 5 happens to be set and the write is harmlessly discarded —
    /// but some return `0x00`, and an unbounded wait for a bit that will
    /// never set hangs the machine on the *first* character printed, before
    /// anything else has run.
    ///
    /// That is indistinguishable from a kernel that never started, which is
    /// exactly what it looks like from the outside.
    const TX_SPIN_LIMIT: u32 = 100_000;

    /// Sends one byte. `false` if the transmitter never became ready.
    pub(super) fn put(byte: u8) -> bool {
        // Bit 5 of the line status register is "transmit holding register
        // empty". Writing before it is set drops the byte.
        for _ in 0..TX_SPIN_LIMIT {
            // SAFETY: reading the line status register has no side effects,
            // and the freestanding build is always at CPL 0.
            if unsafe { inb(COM1 + 5) } & 0x20 != 0 {
                // SAFETY: as above; the port is initialised by `init`.
                unsafe { outb(COM1, byte) };
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}

#[cfg(target_arch = "aarch64")]
mod pl011 {
    //! The PL011 UART, at the address QEMU's `virt` machine maps it to.

    /// `virt` places UART0 here. A different board places it elsewhere; there
    /// is no way to discover it without parsing the device tree, which is
    /// more machinery than a self-test needs.
    const UART0: usize = 0x0900_0000;
    const UARTDR: usize = UART0;
    const UARTFR: usize = UART0 + 0x18;

    /// # Safety
    /// Writes device registers; requires EL1 and a `virt`-compatible map.
    pub(super) unsafe fn init() {
        // QEMU's PL011 is usable from reset, so there is nothing to program.
        // A real board would need the baud divisors and line control set
        // here.
    }

    /// Bounded for the same reason the x86 side is: a board that maps
    /// nothing at this address leaves TXFF set forever, and waiting on it
    /// hangs the kernel on its first character.
    const TX_SPIN_LIMIT: u32 = 100_000;

    /// Sends one byte. `false` if the FIFO never drained.
    pub(super) fn put(byte: u8) -> bool {
        // UARTFR bit 5 is TXFF, "transmit FIFO full".
        for _ in 0..TX_SPIN_LIMIT {
            // SAFETY: the flag register is a device MMIO read with no side
            // effects, at an address fixed by the machine model.
            if unsafe { core::ptr::read_volatile(UARTFR as *const u32) } & (1 << 5) == 0 {
                // SAFETY: the data register accepts a byte and is mapped by
                // the machine model.
                unsafe { core::ptr::write_volatile(UARTDR as *mut u32, byte as u32) };
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }
}
