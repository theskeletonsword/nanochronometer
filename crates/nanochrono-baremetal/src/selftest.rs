// SPDX-License-Identifier: Apache-2.0
//! What the freestanding kernel actually measures.
//!
//! Everything here runs with interrupts masked on a core nothing else is
//! using, which is the condition a hosted benchmark spends most of its effort
//! approximating. The numbers are therefore the floor: whatever a hosted
//! process measures for the same work is that floor plus the operating
//! system.

use crate::pmu::{CorePmu, CounterRoute, FIXED_CORE_CYCLES, FIXED_INSTRUCTIONS};
use crate::{arch, println};
use nanochrono_core::arch as counters;
use nanochrono_core::redundancy::Protected;

/// Runs every check and reports it over the serial port.
///
/// # Safety
/// Programs the PMU, so it requires ring 0 / EL1.
pub unsafe fn run() {
    println!("NanoChronometer {} — freestanding", crate::VERSION);
    println!("arch: {}", counters::ARCH.name());
    println!();

    report_cpu();
    // SAFETY: forwarded from this function's own contract.
    unsafe { report_pmu() };
    report_counter();
    report_integrity();
}

fn report_cpu() {
    let f = nanochrono_core::cpu::features();
    println!("== CPU ==");
    // Only the extensions this crate can dispatch to are listed; the full set
    // needs formatting machinery an allocator-free build does not have.
    #[cfg(target_arch = "x86_64")]
    {
        println!("  sse2={} avx={} avx2={}", f.sse2, f.avx, f.avx2);
        println!(
            "  avx512f={} aesni={} shani={}",
            f.avx512f, f.aesni, f.shani
        );
        println!("  invariant tsc={}", f.invariant_counter);

        // The boot stub is what makes the wide register files usable, so what
        // it managed to enable is worth reporting: CPUID can advertise AVX or
        // AVX-512 while XCR0 says the state is not being saved, and then
        // every VEX or EVEX instruction is #UD. Printing both sides makes a
        // mismatch visible instead of silently disabling a feature.
        use nanochrono_core::arch::x86::cpuid;
        let xcr0 = nanochrono_core::arch::x86::xcr0_safe();
        println!("  xcr0={xcr0:#x} (supported {:#x})", cpuid(0x0D, 0)[0]);
        println!(
            "  cpuid avx512f={} osxsave={}",
            cpuid(7, 0)[1] & (1 << 16) != 0,
            cpuid(1, 0)[2] & (1 << 27) != 0
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        println!("  neon={} sve={} sve2={}", f.neon, f.sve, f.sve2);
        println!("  aes={} sha2={} sme={}", f.arm_aes, f.arm_sha2, f.sme);
        println!("  el={}", crate::arch::arm::current_el());
    }
    println!("  backend: {}", nanochrono_core::Backend::best().name());
    println!();

    report_simd();
}

/// Runs the SIMD probes, which is the point of enabling the state at boot.
///
/// Only built when the `simd` feature is on, which needs the custom target:
/// the stable `x86_64-unknown-none` has a soft-float ABI where no vector
/// register can be allocated at all. If this prints numbers, the boot stub's
/// CR0/CR4/XCR0 sequence worked — a vector instruction with any of that
/// missing is `#UD`, not a slow path, so the probe either runs or the machine
/// stops.
#[cfg(feature = "simd")]
fn report_simd() {
    use nanochrono_core::simd::{self, ProbeBuffers, ProbeKind};
    use nanochrono_core::SimdFamily;

    println!("== SIMD (state enabled by the boot stub) ==");

    // Stack buffers: there is no allocator. 64 bytes covers every family up
    // to AVX-512's 64-byte vectors.
    let mut a = [0x5Au8; 64];
    let mut b = [0xA5u8; 64];
    let mut out = [0u8; 64];

    let mut ran = 0;
    for family in SimdFamily::ALL.iter().copied() {
        if !family.is_available() {
            continue;
        }
        let buffers = ProbeBuffers {
            a: &mut a,
            b: &mut b,
            out: &mut out,
        };
        match simd::probe(family, ProbeKind::VectorXor, Some(buffers), 1) {
            Some(r) => {
                println!("  {:<12} xor {} units", family.name(), r.raw_units);
                ran += 1;
            }
            None => println!("  {:<12} probe declined", family.name()),
        }
    }
    if ran == 0 {
        println!("  no family available on this CPU");
    }
    println!();
}

#[cfg(not(feature = "simd"))]
fn report_simd() {
    println!("== SIMD ==");
    println!("  not built: this target's ABI is soft-float");
    println!("  (build with the x86_64-nanochrono-none target for SIMD)");
    println!();
}

/// The part that needs no kernel and could not be done with one.
///
/// # Safety
/// Programs the PMU; requires ring 0 / EL1.
unsafe fn report_pmu() {
    println!("== PMU (direct, no kernel) ==");
    let mut pmu = CorePmu::detect();

    println!("  core type      : {}", pmu.core_type.name());
    println!("  version        : {}", pmu.leaf.version);
    println!(
        "  general        : {} counters, {} bits",
        pmu.leaf.general_counters, pmu.leaf.general_width
    );
    println!(
        "  fixed          : {} counters, {} bits",
        pmu.leaf.fixed_counters, pmu.leaf.fixed_width
    );

    if !pmu.is_available() {
        println!("  no PMU on this core; skipping the measurement");
        println!();
        return;
    }

    // Programming the PMU can succeed and still leave a counter that never
    // moves, so `enable` proves one counts before reporting which.
    // SAFETY: forwarded from this function's own contract.
    let route = unsafe { pmu.enable() };
    println!("  counter route  : {}", route.name());
    if route == CounterRoute::None {
        println!("  no counter advanced; the measurement would be zeros");
        println!();
        return;
    }

    // A dependent chain: each iteration needs the previous result, so the
    // core cannot overlap them and the cycle count reflects real work rather
    // than how wide the machine is.
    const ITERATIONS: u64 = 100_000;
    let mut acc = 0u64;
    // SAFETY: the PMU was just enabled on this core, at ring 0 / EL1.
    let (acc_out, cycles) = unsafe {
        pmu.measure(|| {
            for i in 0..ITERATIONS {
                acc = acc.wrapping_add(i).rotate_left(3);
            }
            acc
        })
    };
    core::hint::black_box(acc_out);

    match cycles {
        Some(cycles) => {
            println!("  {ITERATIONS} dependent ops");
            println!("    cycles       : {cycles}");
            // Integer arithmetic only: there is no floating-point formatter
            // here, and this is exact enough to read.
            println!(
                "    cycles/op    : {}.{:02}",
                cycles / ITERATIONS,
                (cycles % ITERATIONS) * 100 / ITERATIONS
            );
        }
        None => println!("  measurement discarded: the core type changed mid-run"),
    }

    // The instruction count is only meaningful from the fixed counter; the
    // general-purpose fallback is programmed for cycles alone.
    if route == CounterRoute::Fixed {
        // SAFETY: as above; the index is checked against what this core
        // reports.
        if let Some(insns) = unsafe { pmu.read(FIXED_INSTRUCTIONS) } {
            println!("    instructions : {}", insns.value);
        }
    }
    // SAFETY: as above.
    if let Some(cyc) = unsafe { pmu.read_cycles() } {
        println!("    core cycles  : {}", cyc.value);
    }
    let _ = FIXED_CORE_CYCLES;
    println!();
}

/// The architectural counter, and what one read of it costs.
fn report_counter() {
    println!("== Counter ==");

    // Minimum of N: anything above the minimum is interference, and with
    // interrupts masked there should be very little of it — which is itself
    // worth seeing.
    const ROUNDS: u32 = 1024;
    let mut min = u64::MAX;
    let mut max = 0u64;
    for _ in 0..ROUNDS {
        // The freestanding read, which on AArch64 is the physical counter
        // with a full barrier rather than the virtual one.
        let a = arch::counter_ordered();
        let b = arch::counter_ordered();
        let d = b.wrapping_sub(a);
        if d < min {
            min = d;
        }
        if d > max {
            max = d;
        }
    }
    println!("  read overhead  : {min} units (min of {ROUNDS})");
    println!("  worst read     : {max} units");

    // With no scheduler and no interrupts, the spread between the best and
    // worst read is the machine's own jitter and nothing else. On a hosted
    // build this number is dominated by the kernel.
    println!("  jitter         : {} units", max - min);

    #[cfg(target_arch = "aarch64")]
    println!("  cntfrq_el0     : {} Hz", counters::aarch64::cntfrq());
    println!();
}

/// The ECC/TMR machinery, which needs no kernel either — and matters more
/// here, since a freestanding kernel has no one to report a corrupted value
/// to and no ECC DRAM guarantee beneath it.
fn report_integrity() {
    println!("== Stored-state integrity ==");

    const VALUE: u64 = 0x0000_0002_4126_D2BC;
    let mut p = Protected::new(VALUE);
    println!("  clean          : {}", p.verify().name());

    p.inject_flip(40);
    let outcome = p.verify();
    println!(
        "  one bit        : {} (recovered: {})",
        outcome.name(),
        p.get() == VALUE
    );

    p.inject_flip(5);
    p.inject_flip(37);
    let outcome = p.verify();
    println!(
        "  two bits       : {} (recovered: {})",
        outcome.name(),
        p.get() == VALUE
    );

    let stats = nanochrono_core::redundancy::stats();
    println!(
        "  checks={} ecc={} tmr={} lost={}",
        stats.checks, stats.ecc_corrections, stats.tmr_corrections, stats.unrecoverable
    );
    println!();

    println!("selftest complete; halting");
}
