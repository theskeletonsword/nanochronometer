// SPDX-License-Identifier: Apache-2.0
//! Performance counters with no kernel underneath.
//!
//! Everywhere else in this toolkit the PMU is reached through the OS —
//! `perf_event_open`, the Windows thread-profiling API — because the kernel
//! owns the counters, schedules them per thread and corrects for multiplexing.
//! Here there is no kernel. This code *is* ring 0, so it programs the counters
//! itself and reads them with `RDPMC` (x86) or `PMCCNTR_EL0` (AArch64).
//!
//! That inverts every argument for avoiding those instructions in a hosted
//! build. There is no context switch to survive, no multiplexing to miss, and
//! no scheduler to move the thread — so a raw counter read is not merely
//! acceptable, it is the only correct option.
//!
//! # There is no Rust intrinsic for this
//!
//! `core::arch::x86_64` has `_rdtsc` and `__rdtscp` but **not** `_rdpmc` —
//! stdarch lists it in `missing_x86_common.txt`, so it is a known gap rather
//! than a naming question. `RDMSR`/`WRMSR` and every AArch64 system register
//! have no intrinsics either. All of it is `core::arch::asm!`, which is what
//! the rest of this project uses anyway.
//!
//! # Hybrid CPUs
//!
//! On a P-core/E-core part the two core types are different
//! microarchitectures that happen to share an instruction set. They report
//! **different `CPUID.0AH` values** — a different number of general-purpose
//! counters, and potentially a different counter width — so a PMU
//! configuration derived on one is not valid on the other. Worse, a cycle
//! count from a P-core and one from an E-core are not comparable at all: the
//! same work takes a different number of cycles by design.
//!
//! So [`CorePmu::detect`] must run **on the core it describes**, and every
//! reading carries the [`CoreType`] it came from. Nothing here averages
//! across core types, because that number would be meaningless.

use crate::arch;

// The decoding lives in `nanochrono-core` because it is pure logic that has
// to be tested with values this target cannot be driven with: on a hybrid
// part, one thread can only ever observe one of the two core types. What
// stays here is the privileged half — programming the counters and reading
// them — which needs ring 0 / EL1 and so cannot be tested from a host.
pub use nanochrono_core::pmu_leaf::{
    classify_core, decode_pmu_leaf, mask_to_width, CoreType, PmuLeaf, Reading,
};

/// Which counter [`CorePmu::enable`] found actually works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterRoute {
    /// The architectural fixed counter. Preferred: it means the same thing on
    /// every core type, which no general-purpose event encoding does.
    Fixed,
    /// A general-purpose counter programmed with an architectural event.
    General(u32),
    /// Nothing on this core counts. No measurement will be reported.
    None,
}

impl CounterRoute {
    pub const fn name(self) -> &'static str {
        match self {
            CounterRoute::Fixed => "fixed",
            CounterRoute::General(_) => "general-purpose",
            CounterRoute::None => "none",
        }
    }
}

/// What one core's PMU can do, and which core said so.
///
/// The description and the core type travel together because on a hybrid part
/// neither means anything without the other.
#[derive(Debug, Clone, Copy)]
pub struct CorePmu {
    pub leaf: PmuLeaf,
    pub core_type: CoreType,
    /// Filled in by [`enable`](Self::enable); `None` until then.
    pub route: CounterRoute,
}

// ---------------------------------------------------------------------------
// x86-64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{
        classify_core, decode_pmu_leaf, mask_to_width, CorePmu, CoreType, CounterRoute, Reading,
    };
    use crate::arch::x86::{cpuid, rdmsr, rdpmc, wrmsr};

    /// `IA32_FIXED_CTR_CTRL`. Four bits per fixed counter.
    const IA32_FIXED_CTR_CTRL: u32 = 0x38D;
    /// `IA32_PERF_GLOBAL_CTRL`. Bit `n` enables general counter `n`; bit
    /// `32 + n` enables fixed counter `n`.
    const IA32_PERF_GLOBAL_CTRL: u32 = 0x38F;

    /// `IA32_PERFEVTSEL0`, the first general-purpose event select.
    const IA32_PERFEVTSEL0: u32 = 0x186;
    /// `IA32_PMC0`, the first general-purpose counter.
    const IA32_PMC0: u32 = 0xC1;

    /// `IA32_PERF_GLOBAL_OVF_CTRL` / `..._STATUS_RESET`, which clears any
    /// overflow the firmware left latched. A latched overflow can leave a
    /// counter frozen depending on the control settings.
    const IA32_PERF_GLOBAL_OVF_CTRL: u32 = 0x390;

    /// `CPU_CLK_UNHALTED.THREAD` as a general-purpose event.
    ///
    /// Event `0x3C`, umask `0x00`. This is one of the seven *architectural*
    /// events — the set `CPUID.0AH:EBX` reports availability for — which is
    /// the whole reason it can be used without knowing whether this is a
    /// Raptor Cove or a Gracemont. A model-specific event select would mean
    /// something different on each, and that is precisely how a hybrid part
    /// turns a working measurement into a plausible wrong one.
    const ARCH_EVENT_UNHALTED_CORE_CYCLES: (u8, u8) = (0x3C, 0x00);

    /// The bit in `CPUID.0AH:EBX` that would mark it unavailable.
    const EBX_BIT_UNHALTED_CORE_CYCLES: u32 = 1;

    /// `RDPMC` selects a fixed counter by setting bit 30 of `ECX`.
    const RDPMC_FIXED: u32 = 1 << 30;

    /// Fixed counter 1 is `CPU_CLK_UNHALTED.THREAD` — this core's cycles,
    /// which is the quantity a chronometer wants. Fixed counter 0 is
    /// instructions retired and 2 is the reference (TSC-rate) clock.
    pub const FIXED_CORE_CYCLES: u32 = 1;
    pub const FIXED_INSTRUCTIONS: u32 = 0;
    pub const FIXED_REF_CYCLES: u32 = 2;

    fn core_type() -> CoreType {
        let max_leaf = cpuid(0, 0)[0];
        let leaf7_edx = if max_leaf >= 7 { cpuid(7, 0)[3] } else { 0 };
        let leaf1a_eax = if max_leaf >= 0x1A {
            cpuid(0x1A, 0)[0]
        } else {
            0
        };
        classify_core(max_leaf, leaf7_edx, leaf1a_eax)
    }

    /// Describes the PMU of the core this runs on.
    pub(super) fn detect() -> CorePmu {
        let core_type = core_type();
        if cpuid(0, 0)[0] < 0x0A {
            return CorePmu {
                leaf: Default::default(),
                core_type,
                route: CounterRoute::None,
            };
        }
        let [eax, ebx, _ecx, edx] = cpuid(0x0A, 0);
        CorePmu {
            leaf: decode_pmu_leaf(eax, ebx, edx),
            core_type,
            route: CounterRoute::None,
        }
    }

    /// Programs a general-purpose counter with the architectural
    /// core-cycles event, and returns its index.
    ///
    /// The fallback for a part with no usable fixed counters — some server
    /// SKUs and anything at PMU version 1. Only an *architectural* event is
    /// used, because a model-specific encoding means different things on a
    /// P-core and an E-core.
    ///
    /// # Safety
    /// Writes MSRs; requires CPL 0.
    pub(super) unsafe fn enable_general_cycles(pmu: &CorePmu) -> Option<u32> {
        if pmu.leaf.general_counters == 0 {
            return None;
        }
        // CPUID.0AH:EBX bit N set means architectural event N is *not*
        // available on this core — the one place the two core types of a
        // hybrid part genuinely can disagree about what they can count.
        if pmu.leaf.events_unavailable & EBX_BIT_UNHALTED_CORE_CYCLES != 0 {
            return None;
        }

        let (event, umask) = ARCH_EVENT_UNHALTED_CORE_CYCLES;
        // USR (bit 16) | OS (bit 17) | EN (bit 22): count in both privilege
        // levels so the number does not depend on where the caller runs, and
        // no interrupt on overflow because there is no handler for one.
        let select = (event as u64) | ((umask as u64) << 8) | (1 << 16) | (1 << 17) | (1 << 22);

        // SAFETY: caller guarantees CPL 0. Counter 0 exists whenever
        // `general_counters` is non-zero, which was just checked.
        unsafe {
            wrmsr(IA32_PMC0, 0);
            wrmsr(IA32_PERFEVTSEL0, select);
            let global = rdmsr(IA32_PERF_GLOBAL_CTRL);
            wrmsr(IA32_PERF_GLOBAL_CTRL, global | 1);
        }
        Some(0)
    }

    /// Reads a general-purpose counter, masked to its width.
    ///
    /// # Safety
    /// Requires CPL 0, or `CR4.PCE`.
    pub(super) unsafe fn read_general(pmu: &CorePmu, index: u32) -> Option<Reading> {
        if index >= pmu.leaf.general_counters as u32 {
            return None;
        }
        // SAFETY: caller guarantees the privilege level; the index was
        // bounds-checked against what this core reports.
        let raw = unsafe { rdpmc(index) };
        Some(Reading {
            value: mask_to_width(raw, pmu.leaf.general_width),
            core_type: pmu.core_type,
        })
    }

    /// Starts the fixed counters on this core.
    ///
    /// Must run on the core it is programming: the MSRs are per logical
    /// processor, so a configuration written on one core is simply absent on
    /// another — which on a hybrid part is the failure people meet, not the
    /// counter layout.
    ///
    /// # Safety
    /// Writes MSRs, so it requires CPL 0 — which a freestanding kernel always
    /// has. It clobbers any PMU configuration already in place.
    pub(super) unsafe fn enable_fixed(pmu: &CorePmu) {
        if pmu.leaf.fixed_counters == 0 {
            return;
        }

        // Four bits per counter: bit 0 counts in ring 0, bit 1 in ring > 0,
        // bit 2 is AnyThread, bit 3 raises a PMI on overflow. Ring 0 and ring
        // 3 are both enabled so the count does not depend on where a caller
        // ends up running; no PMI, because there is no handler for one.
        let mut ctrl: u64 = 0;
        for i in 0..pmu.leaf.fixed_counters.min(3) as u32 {
            ctrl |= 0b0011 << (i * 4);
        }
        // SAFETY: caller guarantees CPL 0. Both MSRs are architectural from
        // PMU version 2, which `detect` established before reporting any
        // fixed counters.
        unsafe {
            // Firmware can leave an overflow latched, which depending on the
            // control settings leaves the counter frozen and reading the same
            // value forever. Clearing it costs one write.
            wrmsr(IA32_PERF_GLOBAL_OVF_CTRL, u64::MAX);
            wrmsr(IA32_FIXED_CTR_CTRL, ctrl);

            // Enabling in the global control register is what actually starts
            // them; the per-counter bits above only say how to count.
            let mut global = rdmsr(IA32_PERF_GLOBAL_CTRL);
            for i in 0..pmu.leaf.fixed_counters.min(3) as u32 {
                global |= 1u64 << (32 + i);
            }
            wrmsr(IA32_PERF_GLOBAL_CTRL, global);
        }
    }

    /// Reads a fixed counter, masked to its architectural width.
    ///
    /// `RDPMC` returns `EDX:EAX` with the counter sign-extended above its
    /// real width, so the upper bits are not part of the count and must be
    /// discarded — otherwise a counter that has not yet passed its sign bit
    /// reads as an enormous number.
    ///
    /// # Safety
    /// `RDPMC` faults at CPL > 0 unless `CR4.PCE` is set. A freestanding
    /// kernel runs at CPL 0, where it is always permitted.
    pub(super) unsafe fn read_fixed(pmu: &CorePmu, index: u32) -> Option<Reading> {
        if index >= pmu.leaf.fixed_counters as u32 {
            return None;
        }
        // SAFETY: caller guarantees CPL 0, and the index was just bounds
        // checked against what this core reports.
        let raw = unsafe { rdpmc(RDPMC_FIXED | index) };
        Some(Reading {
            value: mask_to_width(raw, pmu.leaf.fixed_width),
            core_type: pmu.core_type,
        })
    }

    /// Allows `RDPMC` from ring 3 by setting `CR4.PCE`.
    ///
    /// Not needed by this kernel, which never leaves ring 0. It exists so a
    /// kernel built on this crate that *does* run user code can let it read
    /// the counters without a syscall — which is the whole reason `RDPMC`
    /// exists.
    ///
    /// # Safety
    /// Writes `CR4`; requires CPL 0.
    #[allow(dead_code)] // Part of the surface; this kernel never leaves ring 0.
    pub unsafe fn allow_rdpmc_from_user() {
        let mut cr4: u64;
        // SAFETY: caller guarantees CPL 0. Only bit 8 is touched, so no other
        // control-register state is disturbed.
        unsafe {
            core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
            cr4 |= 1 << 8;
            core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack));
        }
    }
}

// ---------------------------------------------------------------------------
// AArch64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::{CorePmu, CoreType, CounterRoute, PmuLeaf, Reading};
    use crate::arch::aarch64 as a;

    /// `PMCNTENSET_EL0` bit 31 enables the dedicated cycle counter.
    const PMCNTEN_CYCLE: u64 = 1 << 31;

    /// Classifies this core from `MIDR_EL1`.
    ///
    /// AArch64 has no `CPUID.1AH`. On a big.LITTLE part the clusters report
    /// different part numbers in `MIDR_EL1[15:4]`, which is the only thing
    /// available — it does not say which cluster is the big one, so the part
    /// number itself is carried and comparison is by equality.
    fn core_type() -> CoreType {
        // SAFETY: MIDR_EL1 is readable at EL1 and has no side effects.
        let midr: u64 = unsafe {
            let v: u64;
            core::arch::asm!("mrs {v}, MIDR_EL1", v = out(reg) v,
                             options(nomem, nostack, preserves_flags));
            v
        };
        // The part number does not fit in the `Unknown` byte, so it is folded
        // — different parts still compare unequal, which is all that is asked
        // of it.
        let part = ((midr >> 4) & 0xFFF) as u16;
        CoreType::Unknown((part ^ (part >> 8)) as u8)
    }

    pub(super) fn detect() -> CorePmu {
        // PMCR_EL0.N, bits [15:11]: the number of event counters. The cycle
        // counter is separate from those and always present when the PMU is.
        let pmcr = read_pmcr();
        CorePmu {
            leaf: PmuLeaf {
                version: 1,
                // PMCR_EL0.N, bits [15:11].
                general_counters: ((pmcr >> 11) & 0x1F) as u8,
                general_width: 32,
                // The cycle counter is the one fixed counter AArch64 has.
                fixed_counters: 1,
                // PMCR_EL0.LC is set in `enable_fixed`, making it 64 bits.
                fixed_width: 64,
                // AArch64 has no equivalent of the architectural-event
                // availability mask; PMCEID0/1_EL0 describe events, and the
                // cycle counter this uses is not one of them.
                events_unavailable: 0,
            },
            core_type: core_type(),
            route: CounterRoute::None,
        }
    }

    fn read_pmcr() -> u64 {
        // SAFETY: PMCR_EL0 is readable at EL1 with no side effects.
        unsafe {
            let v: u64;
            core::arch::asm!("mrs {v}, PMCR_EL0", v = out(reg) v,
                             options(nomem, nostack, preserves_flags));
            v
        }
    }

    /// Starts the cycle counter.
    ///
    /// # Safety
    /// Writes PMU control registers, which requires EL1.
    pub(super) unsafe fn enable_fixed(_pmu: &CorePmu) {
        let mut pmcr = read_pmcr();
        // E (bit 0) enables the counters at all.
        pmcr |= 1 << 0;
        // LC (bit 6) makes the cycle counter 64 bits. Without it the counter
        // is 32 bits and wraps every couple of seconds at gigahertz clocks,
        // which is useless for anything but the shortest interval.
        pmcr |= 1 << 6;
        // D (bit 3) divides the cycle count by 64. Clearing it is what makes
        // the counter report cycles rather than cycles/64 — a factor of 64
        // that would otherwise be silently wrong.
        pmcr &= !(1 << 3);

        // SAFETY: caller guarantees EL1, where all three registers are
        // writable.
        unsafe {
            core::arch::asm!("msr PMCR_EL0, {v}", v = in(reg) pmcr,
                             options(nomem, nostack, preserves_flags));
            core::arch::asm!("msr PMCNTENSET_EL0, {v}", v = in(reg) PMCNTEN_CYCLE,
                             options(nomem, nostack, preserves_flags));
            // PMUSERENR_EL0.EN (bit 0) lets EL0 read the counters without
            // trapping. This kernel stays at EL1, but a kernel built on it
            // that runs user code needs this for the same reason x86 needs
            // CR4.PCE.
            core::arch::asm!("msr PMUSERENR_EL0, {v}", v = in(reg) 1u64,
                             options(nomem, nostack, preserves_flags));
            a::isb();
        }
    }

    /// Reads `PMCCNTR_EL0`, the cycle counter.
    ///
    /// # Safety
    /// Requires EL1, or EL0 with `PMUSERENR_EL0.EN` set.
    pub(super) unsafe fn read_fixed(pmu: &CorePmu, index: u32) -> Option<Reading> {
        // AArch64 has exactly one fixed counter, and it counts cycles.
        if index != super::FIXED_CORE_CYCLES {
            return None;
        }
        // SAFETY: caller guarantees the access is permitted; the read has no
        // side effects.
        let value: u64 = unsafe {
            let v: u64;
            core::arch::asm!("mrs {v}, PMCCNTR_EL0", v = out(reg) v,
                             options(nomem, nostack, preserves_flags));
            v
        };
        Some(Reading {
            value,
            core_type: pmu.core_type,
        })
    }
}

// ---------------------------------------------------------------------------
// The neutral surface
// ---------------------------------------------------------------------------

/// Fixed counter indices, named so a caller does not pass a bare number.
#[cfg(target_arch = "x86_64")]
pub use x86::{FIXED_CORE_CYCLES, FIXED_INSTRUCTIONS, FIXED_REF_CYCLES};

// AArch64 has one fixed counter, the cycle counter. The other two indices
// exist so the API is the same shape on both architectures; reading them
// returns `None`.
/// Instructions retired. Not implemented as a fixed counter on AArch64.
#[cfg(target_arch = "aarch64")]
pub const FIXED_INSTRUCTIONS: u32 = 0;
/// The cycle counter, `PMCCNTR_EL0`.
#[cfg(target_arch = "aarch64")]
pub const FIXED_CORE_CYCLES: u32 = 1;
/// A reference-rate clock. Not implemented as a fixed counter on AArch64.
#[cfg(target_arch = "aarch64")]
pub const FIXED_REF_CYCLES: u32 = 2;

impl CorePmu {
    /// Describes the PMU of the core this runs on.
    ///
    /// **Must be called on the core it will be used for.** On a hybrid part
    /// the answer differs between core types, and a configuration derived on
    /// one core is not valid on another.
    pub fn detect() -> CorePmu {
        #[cfg(target_arch = "x86_64")]
        {
            x86::detect()
        }
        #[cfg(target_arch = "aarch64")]
        {
            arm::detect()
        }
    }

    /// Whether this core has a usable PMU.
    pub fn is_available(&self) -> bool {
        self.leaf.is_available()
    }

    /// Starts the counters on this core and confirms one of them counts.
    ///
    /// **This is the fix for "RDPMC does not work".** Programming the PMU can
    /// fail silently in several ways that report success: firmware may have
    /// left a counter frozen behind a latched overflow, a hypervisor may
    /// swallow the MSR writes, a general-purpose event may not exist on this
    /// core type. In every case `RDPMC` then returns a fixed value — usually
    /// zero — and a caller subtracting two of them gets a plausible-looking
    /// duration of nothing.
    ///
    /// So rather than trusting the enumeration, this runs a short known
    /// workload and checks the counter moved. If the fixed counters do not
    /// count it falls back to a general-purpose counter programmed with an
    /// *architectural* event, which is the only encoding that means the same
    /// thing on a P-core and an E-core. If neither counts it returns
    /// [`CounterRoute::None`] and nothing downstream reports a measurement.
    ///
    /// # Safety
    /// Requires ring 0 / EL1 and clobbers any existing PMU configuration.
    pub unsafe fn enable(&mut self) -> CounterRoute {
        #[cfg(target_arch = "x86_64")]
        {
            // SAFETY: forwarded from this function's own contract.
            unsafe { x86::enable_fixed(self) };
            // SAFETY: as above; reads only.
            if unsafe { self.counter_advances(CounterRoute::Fixed) } {
                self.route = CounterRoute::Fixed;
                return self.route;
            }

            // SAFETY: as above.
            if let Some(index) = unsafe { x86::enable_general_cycles(self) } {
                let route = CounterRoute::General(index);
                // SAFETY: as above.
                if unsafe { self.counter_advances(route) } {
                    self.route = route;
                    return route;
                }
            }
            self.route = CounterRoute::None;
            self.route
        }
        #[cfg(target_arch = "aarch64")]
        {
            // SAFETY: forwarded from this function's own contract.
            unsafe { arm::enable_fixed(self) };
            // SAFETY: as above; reads only.
            self.route = if unsafe { self.counter_advances(CounterRoute::Fixed) } {
                CounterRoute::Fixed
            } else {
                CounterRoute::None
            };
            self.route
        }
    }

    /// Whether a counter actually moves over a short known workload.
    ///
    /// The workload is a dependent chain the optimiser cannot remove, long
    /// enough that even a coarse counter has to tick and short enough to cost
    /// nothing at boot.
    ///
    /// # Safety
    /// Requires ring 0 / EL1.
    unsafe fn counter_advances(&self, route: CounterRoute) -> bool {
        // SAFETY: forwarded from this function's own contract.
        let Some(before) = (unsafe { self.read_route(route) }) else {
            return false;
        };
        arch::serialize();
        let mut acc = 0x9E37_79B9_7F4A_7C15u64;
        for i in 0..4096u64 {
            acc = acc.wrapping_add(i).rotate_left(7);
        }
        core::hint::black_box(acc);
        arch::serialize();
        // SAFETY: as above.
        let Some(after) = (unsafe { self.read_route(route) }) else {
            return false;
        };
        after.value != before.value
    }

    /// Reads one fixed counter by index.
    ///
    /// # Safety
    /// Requires ring 0 / EL1, or user access explicitly enabled.
    pub unsafe fn read(&self, index: u32) -> Option<Reading> {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: forwarded from this function's own contract.
        unsafe {
            x86::read_fixed(self, index)
        }
        #[cfg(target_arch = "aarch64")]
        // SAFETY: forwarded from this function's own contract.
        unsafe {
            arm::read_fixed(self, index)
        }
    }

    /// Reads whichever counter [`enable`](Self::enable) settled on.
    ///
    /// # Safety
    /// Requires ring 0 / EL1.
    pub unsafe fn read_cycles(&self) -> Option<Reading> {
        // SAFETY: forwarded from this function's own contract.
        unsafe { self.read_route(self.route) }
    }

    /// # Safety
    /// Requires ring 0 / EL1.
    unsafe fn read_route(&self, route: CounterRoute) -> Option<Reading> {
        match route {
            CounterRoute::None => None,
            // SAFETY: forwarded from this function's own contract.
            CounterRoute::Fixed => unsafe { self.read(FIXED_CORE_CYCLES) },
            #[cfg(target_arch = "x86_64")]
            // SAFETY: as above.
            CounterRoute::General(i) => unsafe { x86::read_general(self, i) },
            #[cfg(not(target_arch = "x86_64"))]
            CounterRoute::General(_) => None,
        }
    }

    /// Cycles elapsed while running `body`, from the PMU rather than the
    /// timestamp counter.
    ///
    /// `None` if the PMU is unavailable, or if the reading before and after
    /// came from different core types — which on a hybrid part means the
    /// measurement was migrated and is not a duration at all. Nothing here
    /// can prevent that migration; it can only refuse to report the result.
    ///
    /// # Safety
    /// Requires ring 0 / EL1.
    pub unsafe fn measure<T>(&self, body: impl FnOnce() -> T) -> (T, Option<u64>) {
        // SAFETY: forwarded from this function's own contract.
        let before = unsafe { self.read_cycles() };
        arch::serialize();
        let out = body();
        arch::serialize();
        // SAFETY: as above.
        let after = unsafe { self.read_cycles() };

        let delta = match (before, after) {
            (Some(a), Some(b)) => b.delta_since(a),
            _ => None,
        };
        (out, delta)
    }
}
