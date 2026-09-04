// SPDX-License-Identifier: Apache-2.0
//! AArch64 registers a hosted process cannot reach.
//!
//! Unlike x86, the counters themselves are already unprivileged here — the
//! shared crate reads `CNTVCT_EL0` and `CNTFRQ_EL0` from EL0. What needs EL1
//! is the PMU control block, which lives in `pmu`, and the exception-level
//! query below.

/// The exception level this code is executing at.
///
/// `CurrentEL[3:2]`. A kernel loaded by QEMU's `-kernel` starts at EL2 on a
/// machine with virtualization, and at EL1 otherwise, so this is worth knowing
/// before touching a register whose availability depends on it.
pub fn current_el() -> u8 {
    let v: u64;
    // SAFETY: `CurrentEL` is readable at every exception level and has no
    // side effects.
    unsafe {
        core::arch::asm!("mrs {v}, CurrentEL", v = out(reg) v,
                         options(nomem, nostack, preserves_flags));
    }
    ((v >> 2) & 0b11) as u8
}
