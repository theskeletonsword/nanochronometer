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

use core::sync::atomic::{AtomicU8, Ordering};

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

/// Which AArch64 counter [`counter_ordered`] reads, and why it is a choice.
///
/// Defaults to [`CounterSource::Virtual`]: the virtual counter is the safe
/// choice for any kernel that might run inside a hypervisor, because a guest
/// sees a timeline that starts when the guest did. The physical counter
/// exposes — and splices together — the host's real timeline, which is what
/// this toggle exists to opt into for bare metal only.
///
/// `core::sync::atomic` because it is shared without a lock: a kernel built
/// on [`crate::abi`] may set it from one call while the interface reads it
/// from the render loop, on one core with interrupts masked, so the atomic is
/// ordering only against itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterSource {
    /// `CNTVCT_EL0` — the virtual counter, minus `CNTVOFF_EL2`. The safe
    /// default: a timeline that starts when this guest did.
    Virtual,
    /// `CNTPCT_EL0` — the physical counter, what the hardware really ticks.
    /// Not recommended inside a VM.
    Physical,
}

impl CounterSource {
    pub const fn as_u8(self) -> u8 {
        match self {
            CounterSource::Virtual => 0,
            CounterSource::Physical => 1,
        }
    }

    pub const fn from_u8(v: u8) -> CounterSource {
        match v {
            1 => CounterSource::Physical,
            _ => CounterSource::Virtual,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            CounterSource::Virtual => "cntvct_el0",
            CounterSource::Physical => "cntpct_el0",
        }
    }
}

/// The selected counter source, as a `u8` in the encoding of
/// [`CounterSource::as_u8`].
static COUNTER_SOURCE: AtomicU8 = AtomicU8::new(CounterSource::Virtual.as_u8());

/// Selects which AArch64 counter the freestanding kernel reads.
///
/// This is the backing store for the interface's *Enable Physical Counter*
/// toggle. It only means something on AArch64; on x86-64 there is one counter
/// and no choice to make, so the call is a no-op.
///
/// The default is [`CounterSource::Virtual`], which is safe under a
/// hypervisor. Set [`CounterSource::Physical`] for bare metal only.
pub fn set_counter_source(source: CounterSource) {
    COUNTER_SOURCE.store(source.as_u8(), Ordering::Relaxed);
}

/// Which counter source is currently selected.
pub fn counter_source() -> CounterSource {
    CounterSource::from_u8(COUNTER_SOURCE.load(Ordering::Relaxed))
}

/// The counter a freestanding kernel should read.
///
/// On AArch64 this reads either the virtual counter, `CNTVCT_EL0` (the
/// default, safe under a hypervisor), or — when the physical counter is
/// enabled through [`set_counter_source`] — `CNTPCT_EL0`. Both are wrapped in
/// `DSB`+`ISB`; see
/// [`nanochrono_core::arch::aarch64::cntvct_ordered`] and
/// [`nanochrono_core::arch::aarch64::cntpct_ordered`] for why the memory
/// barrier is not optional.
///
/// On x86-64 the shared `RDTSCP`+`LFENCE` route already is the ordered read;
/// there is no privileged alternative to switch to.
#[inline(always)]
pub fn counter_ordered() -> u64 {
    #[cfg(target_arch = "aarch64")]
    {
        match counter_source() {
            CounterSource::Virtual => aarch64::cntvct_ordered(),
            CounterSource::Physical => aarch64::cntpct_ordered(),
        }
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
