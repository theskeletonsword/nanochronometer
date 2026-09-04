// SPDX-License-Identifier: Apache-2.0
//! Turning the machine off and restarting it, without an operating system.
//!
//! A hosted process asks the kernel. Here there is no kernel, so the firmware
//! tables have to be read directly: find the RSDP, follow it to the FADT, and
//! use the registers the FADT names.
//!
//! # What is and is not implemented
//!
//! ACPI's real shutdown path requires evaluating the `\_S5` object in the
//! DSDT, which is AML — a bytecode with a full interpreter behind it. This
//! does not have one. It scans the DSDT for the `_S5_` name and reads the
//! sleep type out of the package that follows, which is the shape every
//! firmware emits in practice but is a pattern match rather than an
//! evaluation. Where it fails, [`shutdown`] falls through to the emulator
//! ports and finally reports that it could not.
//!
//! That is stated plainly because "ACPI shutdown" usually means the full
//! interpreter, and this is not that.

#[cfg(target_arch = "x86_64")]
mod x86_acpi {
    use crate::arch::x86::{inb, outb};

    /// `RSDPtr `, the signature that starts the Root System Description Pointer.
    const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";

    /// Where the RSDP is allowed to live, per the ACPI specification: the first
    /// kilobyte of the Extended BIOS Data Area, or the BIOS read-only region.
    const BIOS_AREA: (usize, usize) = (0x000E_0000, 0x0010_0000);
    const EBDA_POINTER: usize = 0x0000_040E;

    /// The subset of the FADT this needs.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PowerRegisters {
        /// `PM1a_CNT_BLK`: the control register the sleep request goes into.
        pub pm1a_control: u16,
        /// `PM1b_CNT_BLK`, when the platform splits the register. Often zero.
        pub pm1b_control: u16,
        /// `SLP_TYPa` from `\_S5`, if it was found.
        pub slp_typ_a: Option<u16>,
        pub slp_typ_b: Option<u16>,
        /// The FADT's reset register, when it declares one.
        pub reset_port: Option<u16>,
        pub reset_value: u8,
    }

    /// Reads the firmware tables. `None` if no RSDP was found.
    ///
    /// # Safety
    /// Reads physical memory below 1 MiB directly, which requires an identity
    /// mapping and ring 0. Both hold in this kernel.
    pub unsafe fn power_registers() -> Option<PowerRegisters> {
        // SAFETY: forwarded from this function's own contract.
        let rsdt = unsafe { find_rsdt()? };
        // SAFETY: as above.
        let fadt = unsafe { find_table(rsdt, b"FACP")? };

        let mut regs = PowerRegisters::default();

        // SAFETY: the FADT pointer came from a table whose checksum verified, so
        // its fixed-offset fields are present.
        unsafe {
            // Offsets are from the ACPI specification's FADT layout.
            regs.pm1a_control = read_u32(fadt, 64) as u16;
            regs.pm1b_control = read_u32(fadt, 68) as u16;

            // RESET_REG is only valid from FADT revision 2 with the
            // RESET_REG_SUP flag (bit 10 of the Flags field at offset 112).
            let length = read_u32(fadt, 4) as usize;
            if length > 128 && read_u32(fadt, 112) & (1 << 10) != 0 {
                // The reset register is a Generic Address Structure at offset
                // 116; byte 0 is the address space, and 1 means system I/O.
                if read_u8(fadt, 116) == 1 {
                    regs.reset_port = Some(read_u32(fadt, 116 + 4) as u16);
                    regs.reset_value = read_u8(fadt, 128);
                }
            }

            // DSDT pointer at offset 40, where `\_S5` lives.
            let dsdt = read_u32(fadt, 40) as usize;
            if dsdt != 0 {
                let (a, b) = find_s5(dsdt);
                regs.slp_typ_a = a;
                regs.slp_typ_b = b;
            }
        }
        Some(regs)
    }

    /// Powers the machine off.
    ///
    /// Tries, in order: the ACPI sleep register with the type from `\_S5`, then
    /// the two ports emulators answer on. Returns only if every one failed, which
    /// is why it is not `-> !`.
    ///
    /// # Safety
    /// Writes I/O ports; requires ring 0. On success it does not return.
    pub unsafe fn shutdown(regs: Option<&PowerRegisters>) {
        if let Some(r) = regs {
            if let (Some(typ), true) = (r.slp_typ_a, r.pm1a_control != 0) {
                // SLP_TYP in bits 12:10, SLP_EN in bit 13.
                let value = (typ << 10) | (1 << 13);
                // SAFETY: caller guarantees ring 0; the port came from the FADT.
                unsafe { outw(r.pm1a_control, value) };
                if let (Some(typ_b), true) = (r.slp_typ_b, r.pm1b_control != 0) {
                    // SAFETY: as above.
                    unsafe { outw(r.pm1b_control, (typ_b << 10) | (1 << 13)) };
                }
            }
        }

        // The emulator fallbacks, in the order they appeared historically. On
        // real hardware these are unassigned and the writes do nothing.
        // SAFETY: caller guarantees ring 0. Writing an unassigned port is inert.
        unsafe {
            outw(0x604, 0x2000); // QEMU 2.0 and later
            outw(0xB004, 0x2000); // Bochs, and older QEMU
            outw(0x4004, 0x3400); // VirtualBox
        }
    }

    /// Restarts the machine.
    ///
    /// Tries the FADT's reset register, then the keyboard controller, then
    /// triggers a triple fault. The last one always works, because a CPU with no
    /// usable IDT has nothing left to do but reset.
    ///
    /// # Safety
    /// Writes I/O ports and loads a null IDT; requires ring 0. Does not return.
    pub unsafe fn reboot(regs: Option<&PowerRegisters>) -> ! {
        if let Some(r) = regs {
            if let Some(port) = r.reset_port {
                // SAFETY: caller guarantees ring 0; the port came from the FADT.
                unsafe { outb(port, r.reset_value) };
            }
        }

        // The keyboard controller's pulse line, which has reset PCs since the AT.
        // SAFETY: caller guarantees ring 0. Waiting for the input buffer to drain
        // first is what keeps the command from being dropped.
        unsafe {
            for _ in 0..0x1_0000 {
                if inb(0x64) & 0x02 == 0 {
                    break;
                }
            }
            outb(0x64, 0xFE);
        }

        // Nothing answered. A triple fault is not elegant, but it is the one
        // reset that cannot be ignored: with a null IDT the CPU cannot deliver
        // the fault, cannot deliver the double fault either, and resets.
        // SAFETY: caller guarantees ring 0. This is the intended effect.
        unsafe {
            core::arch::asm!(
                "lidt [{null}]",
                "int3",
                null = in(reg) &NULL_IDT,
                options(nostack)
            );
        }
        crate::arch::halt()
    }

    /// A zero-length IDT: loading it makes every interrupt unhandleable.
    static NULL_IDT: [u16; 5] = [0; 5];

    /// # Safety
    /// Requires ring 0.
    unsafe fn outw(port: u16, value: u16) {
        // SAFETY: caller guarantees ring 0 and that the port is safe to write.
        unsafe {
            core::arch::asm!("out dx, ax", in("dx") port, in("ax") value,
                             options(nomem, nostack, preserves_flags));
        }
    }

    /// # Safety
    /// `address` must be readable physical memory.
    unsafe fn read_u8(base: usize, offset: usize) -> u8 {
        // SAFETY: forwarded from this function's own contract.
        unsafe { core::ptr::read_volatile((base + offset) as *const u8) }
    }

    /// # Safety
    /// `address` must be readable physical memory.
    unsafe fn read_u32(base: usize, offset: usize) -> u32 {
        // SAFETY: forwarded; the read is unaligned-safe because ACPI tables are
        // byte-packed and this uses `read_unaligned`.
        unsafe { core::ptr::read_unaligned((base + offset) as *const u32) }
    }

    /// Locates the RSDT through the RSDP.
    ///
    /// # Safety
    /// Reads low physical memory; requires an identity mapping and ring 0.
    unsafe fn find_rsdt() -> Option<usize> {
        // The EBDA base is a segment address in the BIOS data area.
        // SAFETY: forwarded from this function's own contract.
        let ebda = (unsafe { core::ptr::read_volatile(EBDA_POINTER as *const u16) } as usize) << 4;

        for (start, end) in [(ebda, ebda + 1024), BIOS_AREA] {
            if start == 0 {
                continue;
            }
            // The RSDP is 16-byte aligned by specification.
            let mut address = start;
            while address < end {
                // SAFETY: as above; the range is within the first megabyte.
                let signature = unsafe { core::ptr::read_volatile(address as *const [u8; 8]) };
                if &signature == RSDP_SIGNATURE {
                    // Byte 15 is the revision; from 2 the RSDP also carries an
                    // XSDT, but the RSDT it still provides is enough here.
                    // SAFETY: the signature matched, so the structure is present.
                    let rsdt = unsafe { read_u32(address, 16) } as usize;
                    if rsdt != 0 {
                        return Some(rsdt);
                    }
                }
                address += 16;
            }
        }
        None
    }

    /// Finds a table by signature in the RSDT.
    ///
    /// # Safety
    /// `rsdt` must point at a valid RSDT in readable memory.
    unsafe fn find_table(rsdt: usize, signature: &[u8; 4]) -> Option<usize> {
        // SAFETY: forwarded from this function's own contract.
        let length = unsafe { read_u32(rsdt, 4) } as usize;
        if length < 36 {
            return None;
        }
        let entries = (length - 36) / 4;

        for i in 0..entries {
            // SAFETY: `i` is bounded by the length the header declares.
            let table = unsafe { read_u32(rsdt, 36 + i * 4) } as usize;
            // SAFETY: the RSDT's entries point at tables with a standard header.
            let found = unsafe { core::ptr::read_volatile(table as *const [u8; 4]) };
            if &found == signature {
                return Some(table);
            }
        }
        None
    }

    /// Scans the DSDT for `\_S5` and reads the sleep types out of it.
    ///
    /// A pattern match, not an AML evaluation — see the module documentation. The
    /// shape searched for is the one every firmware emits: the `_S5_` name,
    /// a package opcode, its length and element count, then the two sleep types
    /// as byte constants.
    ///
    /// # Safety
    /// `dsdt` must point at a valid DSDT in readable memory.
    unsafe fn find_s5(dsdt: usize) -> (Option<u16>, Option<u16>) {
        // SAFETY: forwarded from this function's own contract.
        let length = unsafe { read_u32(dsdt, 4) } as usize;
        if !(36..=0x10_0000).contains(&length) {
            return (None, None);
        }

        let mut i = 36;
        while i + 8 < length {
            // SAFETY: `i` is bounded by the declared table length.
            let name = unsafe { core::ptr::read_volatile((dsdt + i) as *const [u8; 4]) };
            if &name != b"_S5_" {
                i += 1;
                continue;
            }

            // Skip the name, then the PackageOp (0x12) and its PkgLength and
            // element count, to reach the first element.
            let mut p = i + 4;
            // SAFETY: bounded by the table length checked above.
            if unsafe { read_u8(dsdt, p) } == 0x12 {
                // PkgLength's first byte encodes how many follow in bits 7:6.
                // SAFETY: as above.
                let lead = unsafe { read_u8(dsdt, p + 1) };
                p += 2 + (lead >> 6) as usize;
                p += 1; // element count
            } else {
                i += 1;
                continue;
            }

            // Each element is either a byte constant (0x0A, value) or a
            // zero/one opcode (0x00 / 0x01) standing for that value.
            // SAFETY: bounded by the table length.
            let a = unsafe { read_constant(dsdt, &mut p) };
            // SAFETY: as above.
            let b = unsafe { read_constant(dsdt, &mut p) };
            return (a, b);
        }
        (None, None)
    }

    /// Reads one AML integer constant, advancing `p`.
    ///
    /// # Safety
    /// `*p` must be a readable offset within the table.
    unsafe fn read_constant(base: usize, p: &mut usize) -> Option<u16> {
        // SAFETY: forwarded from this function's own contract.
        let opcode = unsafe { read_u8(base, *p) };
        match opcode {
            0x00 => {
                *p += 1;
                Some(0)
            }
            0x01 => {
                *p += 1;
                Some(1)
            }
            0x0A => {
                // SAFETY: the opcode byte guarantees a value byte follows.
                let value = unsafe { read_u8(base, *p + 1) };
                *p += 2;
                Some(value as u16)
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// AArch64: PSCI, not ACPI
// ---------------------------------------------------------------------------

/// Power control on AArch64.
///
/// There are no I/O ports and no FADT registers to write. The platform's
/// power state is owned by firmware at EL3, reached through PSCI — the same
/// interface Linux and FreeBSD use, and the same `HVC`/`SMC` mechanism the
/// hypervisor detection already issues.
///
/// Function IDs are from FreeBSD's `sys/dev/psci/psci.h`, which matches the
/// ARM PSCI specification: `SYSTEM_OFF` is `0x84000008` and `SYSTEM_RESET` is
/// `0x84000009`.
#[cfg(target_arch = "aarch64")]
mod psci {
    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

    /// Nothing to read: PSCI needs no table walk, so this only reports that
    /// the mechanism exists.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct PowerRegisters;

    /// # Safety
    /// Reads `CurrentEL`; requires EL1 or above.
    pub unsafe fn power_registers() -> Option<PowerRegisters> {
        Some(PowerRegisters)
    }

    /// Issues a PSCI call.
    ///
    /// `HVC` from EL1 when there is an EL2 below the firmware, `SMC`
    /// otherwise. Choosing by exception level rather than by a conduit field
    /// in a device tree, which this kernel does not parse: at EL2 there is
    /// nothing above to take an `HVC`, so it must be `SMC`.
    ///
    /// # Safety
    /// Requires EL1 or EL2. Both function IDs are terminal.
    unsafe fn call(function: u64) {
        let el = crate::arch::arm::current_el();
        // SAFETY: caller guarantees the exception level. Both calls end the
        // machine, so nothing after them can observe a clobbered register.
        unsafe {
            if el >= 2 {
                core::arch::asm!("smc #0", in("x0") function, options(nostack));
            } else {
                core::arch::asm!("hvc #0", in("x0") function, options(nostack));
            }
        }
    }

    /// # Safety
    /// Requires EL1 or above.
    pub unsafe fn shutdown(_regs: Option<&PowerRegisters>) {
        // SAFETY: forwarded from this function's own contract.
        unsafe { call(PSCI_SYSTEM_OFF) };
        // Returning means firmware does not implement it, which the caller
        // reports rather than hiding.
    }

    /// # Safety
    /// Requires EL1 or above.
    pub unsafe fn reboot(_regs: Option<&PowerRegisters>) -> ! {
        // SAFETY: forwarded from this function's own contract.
        unsafe { call(PSCI_SYSTEM_RESET) };
        // PSCI declined. There is no equivalent of x86's triple fault here —
        // no way to force a reset from EL1 — so the honest outcome is to stop.
        crate::arch::halt()
    }
}

#[cfg(target_arch = "x86_64")]
pub use x86_acpi::{power_registers, reboot, shutdown, PowerRegisters};

#[cfg(target_arch = "aarch64")]
pub use psci::{power_registers, reboot, shutdown, PowerRegisters};
