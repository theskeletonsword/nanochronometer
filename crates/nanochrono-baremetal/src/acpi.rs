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
    use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    use crate::arch::x86::{inb, outb};

    /// `RSDPtr `, the signature that starts the Root System Description Pointer.
    const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";

    /// The end of the identity map, from `boot32.S`.
    ///
    /// Every address that comes out of a firmware table is checked against
    /// this before it is dereferenced. A table can name anything, and this
    /// kernel has no IDT: a read outside the map is a page fault with no
    /// handler, which is a dead machine rather than an error.
    const MAPPED_LIMIT: usize = 512 << 30;

    /// Whether `length` bytes at `address` are inside the identity map.
    fn mapped(address: usize, length: usize) -> bool {
        address != 0 && address < MAPPED_LIMIT && length <= MAPPED_LIMIT - address
    }

    /// Whether an ACPI structure's bytes sum to zero, as they are defined to.
    ///
    /// This is the whole point of the checksum: `"RSD PTR "` is eight bytes
    /// scanned across 128 KiB of ROM, and a byte sequence that happens to
    /// match is not a system description. Without this check a false positive
    /// is followed into whatever it points at.
    ///
    /// # Safety
    /// `address` must name `length` readable bytes.
    unsafe fn checksum_ok(address: usize, length: usize) -> bool {
        if !mapped(address, length) || length == 0 {
            return false;
        }
        let mut sum = 0u8;
        for i in 0..length {
            // SAFETY: the range was just checked to be inside the map.
            sum = sum.wrapping_add(unsafe { read_u8(address, i) });
        }
        sum == 0
    }

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
        let root = unsafe { find_root()? };
        // SAFETY: as above.
        let fadt = unsafe { find_table(root, b"FACP")? };

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
            if mapped(dsdt, 36) {
                let (a, b) = find_s5(dsdt);
                regs.slp_typ_a = a;
                regs.slp_typ_b = b;
            }
        }
        Some(regs)
    }

    /// The DSDT, as bytes, for something that can read AML properly.
    ///
    /// `find_s5` above scans this table for a byte pattern, which is enough
    /// for one well-known name and not enough for anything else. Finding a
    /// touchpad means walking the namespace and running a method, and that
    /// wants the whole table — see `nanochrono_core::aml`.
    ///
    /// # Safety
    /// Reads firmware tables; requires ring 0 and an identity map. The
    /// returned slice borrows firmware memory, which is why it is `'static`:
    /// nothing frees it and nothing else writes it.
    pub unsafe fn dsdt() -> Option<&'static [u8]> {
        // SAFETY: forwarded from this function's own contract.
        let root = unsafe { find_root()? };
        // SAFETY: as above.
        let fadt = unsafe { find_table(root, b"FACP")? };
        // SAFETY: the FADT's checksum verified, so its fields are present.
        let length = unsafe { read_u32(fadt, 4) } as usize;

        // The 32-bit pointer at offset 40, or the 64-bit one at 140 where the
        // table sits above four gigabytes. Both are in the specification and
        // firmware fills whichever fits, so the wide one is preferred and the
        // narrow one is the fallback.
        // SAFETY: as above; the wide field only exists in a long enough FADT.
        let address = unsafe {
            let wide = if length >= 148 {
                let low = read_u32(fadt, 140) as u64;
                let high = read_u32(fadt, 144) as u64;
                (high << 32) | low
            } else {
                0
            };
            if wide != 0 {
                wide as usize
            } else {
                read_u32(fadt, 40) as usize
            }
        };

        if !mapped(address, 36) {
            return None;
        }
        // SAFETY: the header is inside the identity map, checked above.
        let table_length = unsafe { read_u32(address, 4) } as usize;
        // A DSDT is tens of kilobytes and a large one is a megabyte. Anything
        // outside that is a length field this should not follow.
        if !(36..=0x40_0000).contains(&table_length) || !mapped(address, table_length) {
            return None;
        }
        // SAFETY: the range is inside the identity map, checked above.
        if !unsafe { checksum_ok(address, table_length) } {
            return None;
        }
        // SAFETY: the whole declared length is inside the identity map, and
        // firmware tables are neither freed nor written after boot.
        Some(unsafe { core::slice::from_raw_parts(address as *const u8, table_length) })
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
    /// What the loader said, when it said anything.
    ///
    /// Set once from `kmain`, before anything reads a table. A static because
    /// the ACPI entry points take no arguments — they are called from places
    /// that have no reason to know how the machine was booted.
    static LOADER_RSDP: AtomicU64 = AtomicU64::new(0);
    /// Whether that address is an XSDT rather than an RSDT.
    static LOADER_IS_XSDT: AtomicBool = AtomicBool::new(false);

    /// Records the root table the loader found.
    ///
    /// On UEFI this is the *only* way to find ACPI: the RSDP's address comes
    /// from the EFI configuration table, and nothing has put a copy where the
    /// legacy scan looks.
    pub fn set_root_table(rsdp: crate::multiboot::Rsdp) {
        // The XSDT is preferred from ACPI 2.0, and firmware is allowed to
        // leave the 32-bit address zero — which several do.
        if rsdp.xsdt != 0 {
            LOADER_RSDP.store(rsdp.xsdt, Ordering::Relaxed);
            LOADER_IS_XSDT.store(true, Ordering::Relaxed);
        } else if rsdp.rsdt != 0 {
            LOADER_RSDP.store(rsdp.rsdt as u64, Ordering::Relaxed);
            LOADER_IS_XSDT.store(false, Ordering::Relaxed);
        }
    }

    /// How the root table was found, for reporting.
    ///
    /// Worth surfacing rather than assuming: "no ACPI" and "ACPI the loader
    /// knew about and this did not ask for" look identical from the outside,
    /// and the second is a bug in this kernel rather than a property of the
    /// machine.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum RootSource {
        /// The loader copied the RSDP into the boot information. The only way
        /// on a UEFI machine.
        Loader,
        /// Found by scanning the EBDA and the top of the first megabyte.
        LegacyScan,
        /// Neither worked.
        None,
    }

    impl RootSource {
        pub const fn name(self) -> &'static str {
            match self {
                RootSource::Loader => "loader (multiboot2 tag)",
                RootSource::LegacyScan => "legacy scan",
                RootSource::None => "not found",
            }
        }
    }

    /// Where the root table came from, and what shape it is.
    ///
    /// # Safety
    /// Reads firmware memory; requires ring 0.
    pub unsafe fn root_source() -> (RootSource, bool) {
        if LOADER_RSDP.load(Ordering::Relaxed) != 0 {
            return (RootSource::Loader, LOADER_IS_XSDT.load(Ordering::Relaxed));
        }
        // SAFETY: forwarded from this function's own contract.
        match unsafe { scan_for_rsdp() } {
            Some(root) => (RootSource::LegacyScan, root.is_xsdt()),
            None => (RootSource::None, false),
        }
    }

    /// Whether the legacy scan finds anything, regardless of what was used.
    ///
    /// Reported alongside the source because the difference is the whole
    /// point: on a BIOS machine both work, and on a UEFI machine only the
    /// loader does. Seeing "loader: yes, scan: no" on one boot and "yes, yes"
    /// on another is what distinguishes the two firmware paths from the
    /// outside, and it is the evidence that the loader path is load-bearing
    /// rather than redundant.
    ///
    /// # Safety
    /// Reads low memory; requires ring 0.
    pub unsafe fn legacy_scan_works() -> bool {
        // SAFETY: forwarded from this function's own contract.
        unsafe { scan_for_rsdp() }.is_some()
    }

    /// A root table and how wide its entries are.
    #[derive(Debug, Clone, Copy)]
    pub struct Root {
        pub address: usize,
        /// Eight for an XSDT, four for an RSDT. Reading a 64-bit table with
        /// 32-bit strides finds tables at addresses made of two halves of two
        /// different pointers.
        pub entry_bytes: usize,
    }

    impl Root {
        pub const fn is_xsdt(&self) -> bool {
            self.entry_bytes == 8
        }
    }

    /// Finds the root table: what the loader passed, or a legacy scan.
    ///
    /// # Safety
    /// Reads firmware memory; requires ring 0 and an identity map.
    pub unsafe fn find_root() -> Option<Root> {
        let from_loader = LOADER_RSDP.load(Ordering::Relaxed);
        if from_loader != 0 && mapped(from_loader as usize, 36) {
            return Some(Root {
                address: from_loader as usize,
                entry_bytes: if LOADER_IS_XSDT.load(Ordering::Relaxed) {
                    8
                } else {
                    4
                },
            });
        }
        // SAFETY: forwarded from this function's own contract.
        unsafe { scan_for_rsdp() }
    }

    /// The legacy scan, for a machine booted through a BIOS.
    ///
    /// # Safety
    /// Reads low memory; requires ring 0 and an identity map.
    unsafe fn scan_for_rsdp() -> Option<Root> {
        // The EBDA base is a segment address in the BIOS data area.
        // SAFETY: forwarded from this function's own contract.
        let ebda = (unsafe { core::ptr::read_volatile(EBDA_POINTER as *const u16) } as usize) << 4;

        for (start, end) in [(ebda, ebda + 1024), BIOS_AREA] {
            // The EBDA pointer is a byte out of low memory, and on a UEFI
            // machine there is no BIOS data area to have written it. A
            // nonsensical one is skipped rather than scanned.
            if start == 0 || !mapped(start, end.saturating_sub(start)) {
                continue;
            }
            // The RSDP is 16-byte aligned by specification.
            let mut address = start;
            while address < end {
                // SAFETY: as above; the range is within the first megabyte.
                let signature = unsafe { core::ptr::read_volatile(address as *const [u8; 8]) };
                if &signature == RSDP_SIGNATURE {
                    // Eight bytes is short enough to appear by accident. The
                    // checksum is what the specification provides to tell a
                    // real RSDP from a coincidence, and following an address
                    // out of a false positive is a walk through arbitrary
                    // memory.
                    // SAFETY: the signature matched, so twenty bytes are present.
                    if unsafe { checksum_ok(address, 20) } {
                        // SAFETY: as above.
                        let revision = unsafe { read_u8(address, 15) };
                        // From revision 2 the XSDT is authoritative and the
                        // 32-bit address may be zero.
                        // SAFETY: as above; the extended fields exist from
                        // revision 2, and their own checksum covers them.
                        if revision >= 2 && unsafe { checksum_ok(address, 36) } {
                            let low = unsafe { read_u32(address, 24) } as u64;
                            let high = unsafe { read_u32(address, 28) } as u64;
                            let xsdt = (high << 32) | low;
                            if xsdt != 0 && mapped(xsdt as usize, 36) {
                                return Some(Root {
                                    address: xsdt as usize,
                                    entry_bytes: 8,
                                });
                            }
                        }
                        // SAFETY: as above.
                        let rsdt = unsafe { read_u32(address, 16) } as usize;
                        if rsdt != 0 && mapped(rsdt, 36) {
                            return Some(Root {
                                address: rsdt,
                                entry_bytes: 4,
                            });
                        }
                    }
                }
                address += 16;
            }
        }
        None
    }

    /// Visits every SSDT the root table lists, in order.
    ///
    /// # Why this exists
    ///
    /// The ACPI namespace is not the DSDT. It is the DSDT *plus* every SSDT,
    /// loaded in the order the root table lists them, all sharing one name
    /// space — and firmware uses that. On the machine this was developed
    /// against the touchpad's `Device (TPD0)` is in the DSDT while the
    /// controller it hangs off, `Device (I2C5)` with the `_ADR` that says
    /// where it is on the PCI bus, is in one of sixteen SSDTs. Reading only
    /// the DSDT finds the touchpad, resolves its `_CRS` to a controller by
    /// name, and then cannot find that controller anywhere — which is
    /// precisely the failure this was written to fix.
    ///
    /// A table is offered only once its own checksum verifies, so a caller
    /// gets bytes it can walk rather than bytes it has to validate.
    ///
    /// Stops early if `visit` returns false.
    ///
    /// # Safety
    /// Reads firmware tables; requires ring 0 and an identity map. The slices
    /// borrow firmware memory, which nothing frees and nothing else writes.
    pub unsafe fn for_each_ssdt(mut visit: impl FnMut(&'static [u8]) -> bool) {
        // SAFETY: forwarded from this function's own contract.
        let Some(root) = (unsafe { find_root() }) else {
            return;
        };
        let rsdt = root.address;
        // SAFETY: as above.
        let length = unsafe { read_u32(rsdt, 4) } as usize;
        if !(36..=0x10_000).contains(&length) || !mapped(rsdt, length) {
            return;
        }
        // SAFETY: the length is bounded and inside the map.
        if !unsafe { checksum_ok(rsdt, length) } {
            return;
        }

        let entries = (length - 36) / root.entry_bytes;
        for i in 0..entries {
            let at = 36 + i * root.entry_bytes;
            // SAFETY: `i` is bounded by the verified length; the stride
            // matches the root table's kind.
            let table = unsafe {
                if root.is_xsdt() {
                    let low = read_u32(rsdt, at) as u64;
                    let high = read_u32(rsdt, at + 4) as u64;
                    ((high << 32) | low) as usize
                } else {
                    read_u32(rsdt, at) as usize
                }
            };
            if !mapped(table, 36) {
                continue;
            }
            // SAFETY: the address was just checked to be inside the map.
            let signature = unsafe { core::ptr::read_volatile(table as *const [u8; 4]) };
            if &signature != b"SSDT" {
                continue;
            }
            // SAFETY: as above; the header declares its own length.
            let table_length = unsafe { read_u32(table, 4) } as usize;
            if !(36..=0x40_0000).contains(&table_length) || !mapped(table, table_length) {
                continue;
            }
            // SAFETY: bounded and inside the map.
            if !unsafe { checksum_ok(table, table_length) } {
                continue;
            }
            // SAFETY: the whole table is inside the identity map and its
            // checksum verified. Firmware memory outlives the kernel.
            let bytes =
                unsafe { core::slice::from_raw_parts(table as *const u8, table_length) };
            if !visit(bytes) {
                return;
            }
        }
    }

    /// Visits the signature of every table the root table lists.
    ///
    /// A plain inventory, and the first thing worth knowing when a device is
    /// not where it was expected: it says whether the SSDTs are there at all,
    /// and whether the root table walks, before any question about what is
    /// inside them.
    ///
    /// # Safety
    /// Reads firmware tables; requires ring 0 and an identity map.
    pub unsafe fn for_each_table_signature(mut visit: impl FnMut([u8; 4]) -> bool) {
        // SAFETY: forwarded from this function's own contract.
        let Some(root) = (unsafe { find_root() }) else {
            return;
        };
        let rsdt = root.address;
        // SAFETY: as above.
        let length = unsafe { read_u32(rsdt, 4) } as usize;
        if !(36..=0x10_000).contains(&length) || !mapped(rsdt, length) {
            return;
        }
        // SAFETY: the length is bounded and inside the map.
        if !unsafe { checksum_ok(rsdt, length) } {
            return;
        }

        let entries = (length - 36) / root.entry_bytes;
        for i in 0..entries {
            let at = 36 + i * root.entry_bytes;
            // SAFETY: `i` is bounded by the verified length.
            let table = unsafe {
                if root.is_xsdt() {
                    let low = read_u32(rsdt, at) as u64;
                    let high = read_u32(rsdt, at + 4) as u64;
                    ((high << 32) | low) as usize
                } else {
                    read_u32(rsdt, at) as usize
                }
            };
            if !mapped(table, 36) {
                continue;
            }
            // SAFETY: the address was just checked to be inside the map.
            let signature = unsafe { core::ptr::read_volatile(table as *const [u8; 4]) };
            if !visit(signature) {
                return;
            }
        }
    }

    /// How many SSDTs the root table lists and this can read.
    ///
    /// # Safety
    /// As [`for_each_ssdt`].
    pub unsafe fn ssdt_count() -> usize {
        let mut count = 0usize;
        // SAFETY: forwarded from this function's own contract.
        unsafe {
            for_each_ssdt(|_| {
                count += 1;
                true
            })
        };
        count
    }

    /// Finds a table by signature in the RSDT.
    ///
    /// # Safety
    /// `rsdt` must point at a valid RSDT in readable memory.
    unsafe fn find_table(root: Root, signature: &[u8; 4]) -> Option<usize> {
        let rsdt = root.address;
        // SAFETY: forwarded from this function's own contract.
        let length = unsafe { read_u32(rsdt, 4) } as usize;

        // A declared length is a number out of firmware, not a fact. An
        // absurd one means the pointer was not an RSDT: without this bound a
        // garbage length of 0xFFFFFFFF becomes a billion dereferences of
        // arbitrary addresses, and the first unmapped one ends the machine.
        if !(36..=0x10_000).contains(&length) || !mapped(rsdt, length) {
            return None;
        }
        // SAFETY: the length is bounded and inside the map.
        if !unsafe { checksum_ok(rsdt, length) } {
            return None;
        }

        let entries = (length - 36) / root.entry_bytes;
        for i in 0..entries {
            let at = 36 + i * root.entry_bytes;
            // SAFETY: `i` is bounded by the verified length. An XSDT's
            // entries are eight bytes and an RSDT's four; reading one with
            // the other's stride finds tables at addresses made of halves of
            // two different pointers.
            let table = unsafe {
                if root.is_xsdt() {
                    let low = read_u32(rsdt, at) as u64;
                    let high = read_u32(rsdt, at + 4) as u64;
                    ((high << 32) | low) as usize
                } else {
                    read_u32(rsdt, at) as usize
                }
            };
            if !mapped(table, 36) {
                continue;
            }
            // SAFETY: the address was just checked to be inside the map.
            let found = unsafe { core::ptr::read_volatile(table as *const [u8; 4]) };
            if &found != signature {
                continue;
            }
            // SAFETY: as above; the header declares its own length.
            let table_length = unsafe { read_u32(table, 4) } as usize;
            if !(36..=0x10_0000).contains(&table_length) {
                continue;
            }
            // SAFETY: bounded and inside the map.
            if unsafe { checksum_ok(table, table_length) } {
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
        if !(36..=0x10_0000).contains(&length) || !mapped(dsdt, length) {
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
pub use x86_acpi::{
    dsdt, for_each_ssdt, for_each_table_signature, legacy_scan_works, power_registers, reboot, root_source, set_root_table,
    shutdown, ssdt_count, PowerRegisters, RootSource,
};

#[cfg(target_arch = "aarch64")]
pub use psci::{power_registers, reboot, shutdown, PowerRegisters};
