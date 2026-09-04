// SPDX-License-Identifier: Apache-2.0
//! NanoChronometer with no operating system underneath.
//!
//! A freestanding build for `x86_64-unknown-none` and `aarch64-unknown-none`:
//! no syscalls, no allocator, no runtime. It exists because the measurement
//! floor a hosted process can reach is set by the kernel underneath it —
//! scheduling, interrupts, the syscall boundary itself — and the only way to
//! see past that floor is to remove the kernel.
//!
//! # What changes without an OS
//!
//! | | Hosted | Here |
//! |---|---|---|
//! | Counter | `RDTSC` / `CNTVCT_EL0` | the same instructions |
//! | PMU | `perf_event_open`, thread-profiling API | `RDPMC` / `PMCCNTR_EL0`, programmed directly |
//! | Calibration | against the monotonic clock | against the PMU's reference counter |
//! | Output | `write(2)` | a UART |
//!
//! The counter layer is *literally the same code*: this crate depends on
//! `nanochrono-core` with `default-features = false`, which keeps `arch`,
//! `backend`, `cpu`, `simd` and `redundancy` and drops everything that needs
//! a kernel. A freestanding kernel therefore executes the same instruction
//! sequences as a hosted process rather than a reimplementation that has
//! drifted.
//!
//! # Why RDPMC is right here and wrong there
//!
//! Every other target in this project refuses to touch `RDPMC` or
//! `PMCCNTR_EL0`, because a raw counter read cannot see the kernel's
//! multiplexing, does not survive a context switch, and on a hybrid CPU
//! silently reads whichever PMU it landed on. None of those hazards exist
//! without a kernel: nothing multiplexes the counters, nothing deschedules
//! this code, and it never migrates because there is no scheduler. So the
//! instruction that is a trap in a hosted build is the only correct option
//! here.
//!
//! The hybrid hazard does survive, in a different form — see [`pmu`].

// Unconditionally `no_std`: there is no operating system to provide the
// library, and pulling it in even for a test build would let a dependency on
// it reach the kernel unnoticed. The logic that needs testing lives in
// `nanochrono_core::pmu_leaf`, which is `no_std` too but is part of a crate
// that has a hosted test build.
#![no_std]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod abi;
pub mod acpi;
pub mod arch;
pub mod draw;
pub mod font;
pub mod framebuffer;
/// x86 only: the 8042 controller and the multiboot framebuffer are PC
/// firmware. An AArch64 board reports its console over the serial port.
#[cfg(target_arch = "x86_64")]
pub mod gui;
/// x86 only: the 8042 controller and the multiboot framebuffer are PC
/// firmware. An AArch64 board reports its console over the serial port.
#[cfg(target_arch = "x86_64")]
pub mod input;
pub mod multiboot;
pub mod panic;
/// x86 only: the 8042 controller and the multiboot framebuffer are PC
/// firmware. An AArch64 board reports its console over the serial port.
#[cfg(target_arch = "x86_64")]
pub mod panic_screen;
pub mod pmu;
pub mod selftest;
pub mod serial;

pub use nanochrono_core::{arch as core_arch, cpu, Backend, Integrity, Protected, SimdFamily};

/// Crate version, from Cargo.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
