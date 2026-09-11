// SPDX-License-Identifier: Apache-2.0
//! The freestanding kernel.
//!
//! Boots, measures, prints, halts. On x86-64 it is entered from `boot32.S`
//! once long mode is up; on AArch64 the loader lands directly on the stub
//! below.

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(target_arch = "x86_64")]
use nanochrono_baremetal::acpi;
#[allow(unused_imports)]
use nanochrono_baremetal::println;
use nanochrono_baremetal::{arch, selftest, serial::Serial};
// Only the text-mode banner names it, and that path is x86 firmware.
#[cfg(target_arch = "x86_64")]
use nanochrono_baremetal::VERSION;
#[cfg(target_arch = "x86_64")]
use nanochrono_baremetal::{gui, multiboot, panic, progress};

/// What a multiboot2 loader leaves in `EAX`. GRUB2 uses this one.
///
/// Checked rather than assumed: booted some other way, the info pointer in
/// `RSI` is not a multiboot structure and reading it would be reading
/// whatever happened to be in the register.
#[cfg(target_arch = "x86_64")]
const MULTIBOOT2_BOOTLOADER_MAGIC: u64 = 0x36D7_6289;

/// What a multiboot1 loader leaves in `EAX`. QEMU's `-kernel` uses this one.
#[cfg(target_arch = "x86_64")]
const MULTIBOOT1_BOOTLOADER_MAGIC: u64 = 0x2BAD_B002;

/// Entry point, called from `boot32.S` with the multiboot magic and the info
/// structure pointer.
///
/// # Safety
/// Called once, by the boot stub, at CPL 0 with a valid stack and paging on.
#[cfg(target_arch = "x86_64")]
#[no_mangle]
pub unsafe extern "C" fn kmain(magic: u64, multiboot_info: u64) -> ! {
    // SAFETY: at CPL 0, and nothing else is driving COM1.
    unsafe { Serial::init() };

    let loader = match magic {
        MULTIBOOT2_BOOTLOADER_MAGIC => "multiboot2",
        MULTIBOOT1_BOOTLOADER_MAGIC => "multiboot1",
        other => {
            println!("warning: boot magic is {other:#x}, neither multiboot1 nor 2");
            println!("         the boot information structure is not being read");
            "unknown"
        }
    };
    println!("booted by: {loader}");

    // The loader was asked for a graphics mode in the multiboot header; this
    // is where it says whether it managed one. Without it everything still
    // works over serial, which is why the tag is marked optional.
    // SAFETY: `multiboot_info` is what the boot stub passed through from the
    // loader, and the magic above says whether it means anything.
    let mut fb = unsafe { multiboot::framebuffer(multiboot_info) };

    // Where ACPI's root table is. **Only the loader can say this on a UEFI
    // machine**: the RSDP's address comes from the EFI configuration table,
    // and nothing puts a copy where the legacy scan looks — so a kernel that
    // only scans finds no ACPI at all on a recent laptop, and everything that
    // depends on the DSDT fails with it.
    // SAFETY: as above.
    if let Some(rsdp) = unsafe { multiboot::acpi_rsdp(multiboot_info) } {
        acpi::set_root_table(rsdp);
    }

    // How much memory the machine has, which the interface reports and which
    // only the loader can say.
    // SAFETY: as above.
    let memory = unsafe { multiboot::memory(multiboot_info) };

    // The back buffer, before anything is drawn. Everything after this point
    // draws into RAM and copies out only what changed — see
    // `framebuffer` for why an uncached firmware framebuffer cannot be
    // animated directly.
    if let Some(surface) = fb.as_mut() {
        // SAFETY: called once, here, before any drawing.
        let composited = unsafe { surface.attach_back_buffer() };
        if !composited {
            println!("mode too large for the back buffer; drawing directly");
        }
    }
    // Handed to the panic handler, which takes no arguments and so cannot be
    // given one any other way.
    panic::set_framebuffer(fb);

    // And to the progress marker, before anything that could stop. On a
    // machine with no serial port this is the only way to see how far a boot
    // got — see `progress`.
    progress::attach(fb);
    progress::leave(progress::Phase::Entered);

    match fb {
        // SAFETY: at CPL 0, with a framebuffer the loader described.
        Some(ref fb) => {
            println!(
                "framebuffer: {}x{}, drawing the interface",
                fb.width, fb.height
            );
            // SAFETY: at CPL 0, which the selftest's PMU programming needs.
            unsafe { selftest::run() };
            // SAFETY: at CPL 0, with a framebuffer the loader described.
            unsafe { gui::run(fb, memory) }
        }
        None => {
            // No linear framebuffer. On a BIOS machine the loader left a VGA
            // text mode behind, and that is somewhere to say so — without it
            // this halts with no output at all and the loader's last message
            // stays on screen, which is indistinguishable from a kernel that
            // never started. That is exactly how this failed on real
            // hardware.
            // SAFETY: at CPL 0. On a UEFI machine the write reaches ordinary
            // RAM and is merely invisible.
            unsafe { nanochrono_baremetal::vga::activate() };
            println!();
            println!("NanoChronometer {} — freestanding", VERSION);
            println!();
            println!("No linear framebuffer: the loader handed over a text mode.");
            println!("The graphical interface needs one; the measurements below do not.");
            println!();
            // SAFETY: at CPL 0, which the selftest's PMU programming needs.
            unsafe { selftest::run() };
            println!();
            println!("Boot with gfxpayload=keep for the interface.");
            arch::halt()
        }
    }
}

// AArch64 entry.
//
// `global_asm!` rather than a second `.S` file: nothing here has to sit at a
// fixed offset the way a multiboot header does, and the loader enters in
// 64-bit mode with the ABI already valid — so the only work is enabling
// FP/SIMD, a stack, a zeroed `.bss`, and parking the secondary cores. That is
// why boot32.S is the only loose assembly file in this project.
#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    r#"
.section .text.boot, "ax"
.global _start
_start:
    // Only CPU 0 runs the kernel. The others are parked rather than left to
    // race through the same code with the same stack.
    mrs x0, mpidr_el1
    and x0, x0, #0xFF
    cbz x0, 1f
0:  wfe
    b 0b

1:  // Enable FP and SIMD before any Rust runs.
    //
    // The AArch64 counterpart of what boot32.S does with CR0/CR4 on x86:
    // `aarch64-unknown-none` has NEON on, so the compiler emits vector
    // instructions freely, and every one of them traps until this is set.
    // The trap is EC 0x07 and it fires on the first Rust function that
    // touches a `v` register, which in practice is almost immediately.
    //
    // CPACR_EL1.FPEN, bits 21:20 = 0b11: no trapping at EL0 or EL1.
    // ZEN, bits 17:16, does the same for SVE; the bits are RES0 on a part
    // without it, so setting them unconditionally is safe.
    mrs x0, CurrentEL
    lsr x0, x0, #2
    cmp x0, #2
    b.eq 5f

    mrs x0, cpacr_el1
    orr x0, x0, #(3 << 20)
    orr x0, x0, #(3 << 16)
    msr cpacr_el1, x0
    b   6f

5:  // At EL2 the control lives elsewhere. CPTR_EL2.TFP, bit 10, *traps* FP
    // when set, so the sense is inverted: it has to be cleared.
    mrs x0, cptr_el2
    bic x0, x0, #(1 << 10)
    msr cptr_el2, x0

6:  isb

    // The loader's stack, if any, is not ours. Point sp at the reserved
    // region below.
    adrp x0, __boot_stack_top
    add  x0, x0, :lo12:__boot_stack_top
    mov  sp, x0

    // Rust assumes .bss is zeroed; nothing has done that yet.
    adrp x0, __bss_start
    add  x0, x0, :lo12:__bss_start
    adrp x1, __bss_end
    add  x1, x1, :lo12:__bss_end
2:  cmp  x0, x1
    b.hs 3f
    str  xzr, [x0], #8
    b    2b

3:  bl kmain
    // kmain does not return; if it somehow does, park.
4:  wfe
    b 4b

.section .bss
.align 16
__boot_stack_bottom:
    .skip 65536
__boot_stack_top:
"#
);

/// AArch64 entry point, called from the stub above.
///
/// # Safety
/// Called once, at EL1 or EL2, with a valid stack and `.bss` zeroed.
#[cfg(target_arch = "aarch64")]
#[no_mangle]
pub unsafe extern "C" fn kmain() -> ! {
    // SAFETY: at EL1+, and nothing else is driving the UART.
    unsafe { Serial::init() };
    // SAFETY: at EL1+, which the PMU programming requires.
    unsafe { selftest::run() };
    arch::halt()
}
