// SPDX-License-Identifier: Apache-2.0
//! Fallback for targets with no architectural cycle counter.
//!
//! Raw units are nanoseconds from the OS monotonic clock, so `cycles_per_ns`
//! calibrates to 1.0 and every conversion becomes the identity. Resolution is
//! whatever the platform clock offers — typically tens of nanoseconds — which
//! is honest about what the target can measure rather than pretending to
//! cycle granularity.

use crate::platform;

#[inline(always)]
pub fn counter_ns() -> u64 {
    platform::monotonic_ns()
}

#[inline]
pub fn read_overhead_ns() -> u64 {
    let a = counter_ns();
    let b = counter_ns();
    b.saturating_sub(a)
}

#[inline]
pub fn barrier_overhead_ns(iterations: u32) -> u64 {
    let n = iterations.max(1);
    let a = counter_ns();
    for _ in 0..n {
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    }
    let b = counter_ns();
    b.saturating_sub(a)
}

/// Portable stand-in for the ISA microbenchmark kernels.
#[inline(never)]
pub fn kernel_scalar(loops: usize) -> u64 {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for _ in 0..loops {
        x ^= x << 7;
        x ^= x >> 9;
        x = x.wrapping_add(0xD1B5_4A32_D192_ED03);
        x = core::hint::black_box(x);
    }
    x
}
