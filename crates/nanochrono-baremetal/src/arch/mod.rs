// SPDX-License-Identifier: Apache-2.0
//! The instructions a freestanding build needs that a hosted one never may.
//!
//! The counters themselves — `RDTSC`, `CNTVCT_EL0`, the SIMD probes — come
//! from `nanochrono_core::arch`, unchanged, because they are the same
//! instructions either way. What lives here is the privileged half: `RDPMC`,
//! `RDMSR`/`WRMSR`, port I/O. A hosted process cannot execute any of it, so it
//! has no place in the shared crate.

#[cfg(target_arch = "aarch64")]
pub use nanochrono_core::arch::aarch64;

#[cfg(target_arch = "x86_64")]
pub mod x86;

#[cfg(target_arch = "aarch64")]
pub mod arm;

/// Orders the instruction stream around a measurement.
///
/// Without this the CPU is free to hoist the work past the counter read, or
/// sink the read past the work, and the interval measured is not the interval
/// asked for.
#[inline(always)]
pub fn serialize() {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: `LFENCE` has no operands and no memory effects beyond ordering.
    unsafe {
        core::arch::asm!("lfence", options(nostack, preserves_flags));
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `ISB` has no operands and no memory effects beyond ordering.
    unsafe {
        core::arch::asm!("isb", options(nostack, preserves_flags));
    }
}

/// The counter a freestanding kernel should read.
///
/// On AArch64 this is `CNTPCT_EL0`, the physical counter, with `DSB`+`ISB`
/// around it — not the `CNTVCT_EL0` the hosted build uses. See
/// [`nanochrono_core::arch::aarch64::cntpct_ordered`] for why the physical
/// counter is the right one here and why the memory barrier is not optional.
///
/// On x86-64 the shared `RDTSCP`+`LFENCE` route already is the ordered read;
/// there is no privileged alternative to switch to.
#[inline(always)]
pub fn counter_ordered() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::cntpct_ordered()
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        nanochrono_core::arch::counter_end()
    }
}

/// Stops this core permanently, with interrupts masked.
///
/// A freestanding `main` cannot return: there is nothing to return *to*, and
/// falling off the end would execute whatever bytes follow.
pub fn halt() -> ! {
    loop {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: `cli` masks interrupts and `hlt` waits for one; with
        // interrupts masked this never wakes, which is the intent.
        unsafe {
            core::arch::asm!("cli", "hlt", options(nomem, nostack));
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: masks interrupts, then waits for an event that cannot
        // arrive.
        unsafe {
            core::arch::asm!("msr daifset, #0xf", "wfi", options(nomem, nostack));
        }
    }
}
