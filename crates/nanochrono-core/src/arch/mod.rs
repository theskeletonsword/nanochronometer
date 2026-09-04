// SPDX-License-Identifier: Apache-2.0
//! Architecture-specific counter and probe primitives.
//!
//! Exactly one of the submodules below is compiled per target. Everything the
//! old `asm/` tree provided is expressed as `core::arch::asm!` inside them —
//! there are no `.S`/`.asm` files and no assembler in the build graph.
//!
//! Callers should prefer the neutral re-exports at the bottom of this module
//! ([`counter_raw`], [`counter_ordered`], [`read_overhead`], …) so that code
//! outside `arch` never needs a `cfg`.

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
pub mod x86;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

#[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
pub mod generic;

/// The counter family a target actually exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    /// x86, 32- or 64-bit: `RDTSC`/`RDTSCP`, raw units are core cycles.
    X86,
    /// AArch64: `CNTVCT_EL0`, raw units are architectural counter ticks.
    Aarch64,
    /// No architectural counter; raw units are nanoseconds from the OS clock.
    Portable,
}

impl Arch {
    pub const fn name(self) -> &'static str {
        match self {
            Arch::X86 => "x86",
            Arch::Aarch64 => "arm64",
            Arch::Portable => "portable",
        }
    }
}

/// The counter family this binary was built for.
pub const ARCH: Arch = {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        Arch::X86
    }
    #[cfg(target_arch = "aarch64")]
    {
        Arch::Aarch64
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        Arch::Portable
    }
};

/// Cheapest counter read, with no ordering guarantee.
#[inline(always)]
pub fn counter_raw() -> u64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::rdtsc_raw()
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::cntvct_raw()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        generic::counter_ns()
    }
}

/// Ordered counter read for the *start* of an interval.
#[inline(always)]
pub fn counter_start() -> u64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::rdtsc_lfence()
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::cntvct_isb()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        generic::counter_ns()
    }
}

/// Ordered counter read for the *end* of an interval.
///
/// On x86-64 this is `RDTSCP`, which additionally waits for older instructions
/// to retire — the asymmetry with [`counter_start`] is deliberate.
#[inline(always)]
pub fn counter_end() -> u64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::rdtscp_lfence().0
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::cntvct_isb()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        generic::counter_ns()
    }
}

/// Alias for [`counter_end`], kept for call sites that read as "now".
#[inline(always)]
pub fn counter_ordered() -> u64 {
    counter_end()
}

/// The core/socket id the last counter read came from, when the architecture
/// exposes one. Used to detect thread migration mid-measurement.
#[inline]
pub fn counter_aux() -> Option<u32> {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        Some(x86::tsc_aux())
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86")))]
    {
        None
    }
}

/// Cost of a back-to-back counter read pair, in raw units.
#[inline]
pub fn read_overhead() -> u64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::read_overhead_cycles()
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::read_overhead_ticks()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        generic::read_overhead_ns()
    }
}

/// Cost of `iterations` back-to-back memory barriers, in raw units.
#[inline]
pub fn barrier_overhead(iterations: u32) -> u64 {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::probe_barrier_cycles(iterations)
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::probe_barrier_ticks(iterations)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        generic::barrier_overhead_ns(iterations)
    }
}

/// Spin hint for calibration loops: `PAUSE` on x86, `YIELD` on ARM.
#[inline(always)]
pub fn cpu_relax() {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::pause()
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::yield_hint()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        core::hint::spin_loop()
    }
}

/// Full memory barrier.
#[inline(always)]
pub fn memory_barrier() {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        x86::mfence()
    }
    #[cfg(target_arch = "aarch64")]
    {
        aarch64::dmb_sy()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst)
    }
}

/// Native tick rate of the architectural counter, when the hardware states it.
///
/// AArch64 reports `CNTFRQ_EL0` directly. x86-64 has no equivalent register,
/// so its TSC frequency has to be measured — see
/// [`crate::clock::calibrate_cycles_per_ns`].
#[inline]
pub fn declared_counter_hz() -> Option<u64> {
    #[cfg(target_arch = "aarch64")]
    {
        let hz = aarch64::cntfrq();
        if hz > 0 {
            Some(hz)
        } else {
            None
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        None
    }
}
