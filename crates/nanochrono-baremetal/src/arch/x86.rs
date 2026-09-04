// SPDX-License-Identifier: Apache-2.0
//! Privileged x86-64 instructions, and the one unprivileged instruction Rust
//! has no intrinsic for.
//!
//! `core::arch::x86_64` provides `_rdtsc` and `__rdtscp`, but **not**
//! `_rdpmc`: stdarch lists it in `missing_x86_common.txt`, so it is a known
//! gap rather than a different name. `RDMSR`, `WRMSR` and port I/O have no
//! intrinsics either, being privileged. All of it is `core::arch::asm!`.

pub use nanochrono_core::arch::x86::cpuid;

/// `RDPMC` — reads performance counter `index`.
///
/// The counter number goes in `ECX`; bit 30 selects a fixed-function counter.
/// The result arrives as `EDX:EAX`, sign-extended above the counter's real
/// width, so the caller must mask it — see `pmu::CorePmu::read`.
///
/// # Safety
/// Faults with `#GP` at CPL > 0 unless `CR4.PCE` is set, and with `#GP` at any
/// privilege level if `index` names a counter the CPU does not implement. The
/// caller must be at CPL 0, or have enabled user access, and must have checked
/// the index against `CPUID.0AH`.
#[inline]
pub unsafe fn rdpmc(index: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller guarantees the privilege level and a valid index.
    // RDPMC reads a counter and writes only EDX:EAX.
    unsafe {
        core::arch::asm!(
            "rdpmc",
            in("ecx") index,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | low as u64
}

/// `RDMSR` — reads model-specific register `msr`.
///
/// # Safety
/// Privileged: `#GP` at CPL > 0. Also `#GP` if the MSR is not implemented,
/// which is not detectable in advance for most of them — the caller must know
/// the MSR exists on this part.
#[inline]
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let low: u32;
    let high: u32;
    // SAFETY: the caller guarantees CPL 0 and that the MSR exists.
    unsafe {
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") low,
            out("edx") high,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((high as u64) << 32) | low as u64
}

/// `WRMSR` — writes model-specific register `msr`.
///
/// # Safety
/// Privileged, and far more dangerous than the read: many MSRs change how the
/// processor executes, and a reserved-bit write raises `#GP`. The caller must
/// be at CPL 0 and must know the MSR's layout on this part.
#[inline]
pub unsafe fn wrmsr(msr: u32, value: u64) {
    // SAFETY: the caller guarantees CPL 0 and a valid value for this MSR.
    unsafe {
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") value as u32,
            in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags),
        );
    }
}

/// `OUT` — writes a byte to an I/O port.
///
/// # Safety
/// Privileged, and the effect depends entirely on what is wired to the port.
#[inline]
pub unsafe fn outb(port: u16, value: u8) {
    // SAFETY: the caller guarantees CPL 0 and that the port is safe to write.
    unsafe {
        core::arch::asm!("out dx, al", in("dx") port, in("al") value,
                         options(nomem, nostack, preserves_flags));
    }
}

/// `IN` — reads a byte from an I/O port.
///
/// # Safety
/// Privileged. Reading some ports has side effects on the device behind them.
#[inline]
pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: the caller guarantees CPL 0 and that the read is harmless.
    unsafe {
        core::arch::asm!("in al, dx", in("dx") port, out("al") value,
                         options(nomem, nostack, preserves_flags));
    }
    value
}
