// SPDX-License-Identifier: Apache-2.0
//! The interface on AArch64 — the port in progress.
//!
//! Where the x86 build boots a multiboot loader that hands over a linear
//! framebuffer and a memory map, an AArch64 board has neither. It boots into a
//! console over the serial port (an `ns16550a` or a `pl011`, found through the
//! devicetree QEMU places in memory), and any drawable screen is a framebuffer
//! described by that same tree, not by a boot protocol.
//!
//! This module is the shape the port will take, compiled as soon as the rest
//! of the kernel stabilises, and it is *content* missing on purpose for now:
//!
//! - **Framebuffer** — handed over by the devicetree `chosen` node, not by
//!   multiboot. [`crate::framebuffer`], `crate::draw` and `crate::typeface`
//!   are already portable; what is missing is the reader that walks the tree
//!   for the `simple-framebuffer` compatible node and its address, stride and
//!   depth.
//! - **Input** — there is no 8042 on ARM. [`crate::input`] is x86; the port
//!   reads a serial console instead, and only once the sideband exists.
//! - **Clock** — no PIT, no CMOS, no `TSC`. The counter and its frequency
//!   come from `CNTVCT_EL0` and `CNTFRQ_EL0`, which need no calibration.
//! - **Power** — [`crate::acpi`] owns shutdown on x86; here it is a PSCI
//!   `SYSTEM_OFF`/`SYSTEM_RESET` call, already reserved in this crate.
//!
//! Until those pieces exist this entry point is honest about it: it reports
//! over the only channel it can — the serial console — and halts, rather than
//! render a face that is wrong because no screen was handed over. Building the
//! real path is the separate porting effort; this is where it lands.

use crate::framebuffer::Framebuffer;

/// Drives the interface until the machine turns itself off.
///
/// `total_memory` is total usable RAM in bytes, `0` if unknown. On x86 the
/// equivalent is the loader's memory map; here it comes from the devicetree's
/// `/memory` node, so the call takes a plain number instead of the multiboot
/// [`crate::multiboot::Memory`] struct the x86 side consumes.
///
/// # Safety
/// Called once, at EL1, with a stack and the MMU as the boot stub set them up.
/// Passes `fb` only if the devicetree proved a real framebuffer at that
/// address.
pub unsafe fn run(fb: &Framebuffer, total_memory: u64) -> ! {
    let _ = fb;
    let _ = total_memory;
    // The port's first milestone. Everything below this line is where the
    // devicetree framebuffer reader and the serial-input sideband plug in.
    crate::println!(
        "interface : the AArch64 frame is in progress; halting ({} bytes usable)",
        total_memory
    );
    crate::arch::halt()
}