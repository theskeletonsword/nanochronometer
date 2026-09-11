// SPDX-License-Identifier: Apache-2.0
//! Hypervisor detection and host time, from ring 0.
//!
//! # Why this is mandatory here and optional in the hosted build
//!
//! The hosted toolkit treats hypercalls as an optional extra behind a kernel
//! module, because `VMCALL` at CPL 3 raises `#UD` and `HVC` at EL0 is
//! undefined — a userspace process simply cannot issue one, so detection there
//! rests on the passive CPUID leaf and the platform's files.
//!
//! This kernel *is* ring 0. The hypercall is available, it is the only source
//! that cannot be spoofed by clearing a CPUID bit, and — the part that
//! matters for a chronometer — it is the only way to get the host's clock
//! paired with this machine's counter. So it is not optional: detection runs
//! CPUID *and* the hypercall, and reports both.
//!
//! # Negotiating nanoseconds with the host
//!
//! Inside a VM the guest counter is not the host's. `KVM_HC_CLOCK_PAIRING`
//! asks the host to sample its own clock and the guest TSC at the same
//! instant, which turns a guest timestamp into a host timestamp. That is the
//! call `ptp_kvm` makes in the Linux guest kernel and publishes through a PTP
//! device; with no kernel there is no device, so the call is made directly.
//!
//! Constants verified against the running kernel's headers:
//! `KVM_HC_CLOCK_PAIRING` is 9 and `KVM_CLOCK_PAIRING_WALLCLOCK` is 0 in
//! `arch/x86/include/uapi/asm/kvm_para.h`; the AArch64 PTP function ID is
//! `ARM_SMCCC_VENDOR_HYP_KVM_PTP_FUNC_ID`, `0x86000001`.

use core::sync::atomic::{AtomicU32, Ordering};

/// Every hypercall this machine has issued.
///
/// # Why it is counted at all
///
/// Because the measurement depends on it staying at one. A hypercall is a
/// `VMCALL`: a trap out of the guest, into the host kernel, and back. It costs
/// microseconds — thousands of times a counter read — and, worse for a
/// chronometer, it costs a *variable* number of them, because what happens on
/// the other side is another operating system's scheduler.
///
/// So the host clock is asked for exactly once, at boot, to learn the offset
/// between this machine's counter and the host's. After that the stopwatch
/// reads `RDTSC` and nothing else: the pairing is arithmetic applied to a
/// counter, not a question asked again. A stopwatch that issued a hypercall
/// per sample would be measuring the hypercall.
///
/// Counting them turns that from a claim in a comment into something the
/// screen shows. The hypervisor panel reports this number; if a change ever
/// puts a hypercall in the frame loop, it stops reading `1` and starts
/// climbing, in front of whoever is looking.
static HYPERCALLS: AtomicU32 = AtomicU32::new(0);

/// How many hypercalls have been issued since power-on.
pub fn hypercalls() -> u32 {
    HYPERCALLS.load(Ordering::Relaxed)
}

/// What was found, and how.
#[derive(Debug, Clone, Copy, Default)]
pub struct Report {
    /// `CPUID.1:ECX[31]`, the architectural hypervisor bit. Passive: a
    /// hypervisor that wants to hide clears it.
    pub cpuid_bit: bool,
    /// The 12-byte signature at `CPUID.40000000H`, when there is one.
    pub signature: [u8; 12],
    /// Highest hypervisor leaf the platform answers.
    pub max_leaf: u32,
    /// A hypercall returned instead of faulting. Proof, not inference — and
    /// it holds even against a hypervisor that cleared the CPUID bit.
    pub hypercall_ok: bool,
    /// Host time paired with this machine's counter, if the host offered it.
    pub pairing: Option<ClockPairing>,
    /// How many hypercalls the machine has issued, in total, ever.
    ///
    /// The whole point is that this is a small number and never grows. See
    /// [`hypercalls`].
    pub hypercalls: u32,
}

impl Report {
    /// Whether anything says this is a guest.
    pub fn is_virtualized(&self) -> bool {
        self.cpuid_bit || self.hypercall_ok
    }

    /// The signature as text, empty if there is none.
    pub fn signature_str(&self) -> &str {
        let end = self
            .signature
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(self.signature.len());
        core::str::from_utf8(&self.signature[..end]).unwrap_or("")
    }
}

/// One host/guest timestamp pair, captured together by the host.
#[derive(Debug, Clone, Copy)]
pub struct ClockPairing {
    /// The host's clock, in nanoseconds.
    pub host_ns: u64,
    /// This machine's counter at the same instant.
    pub counter: u64,
}

/// Runs every detector.
///
/// # Safety
/// Issues a hypercall and reads MSRs; requires ring 0 / EL1.
pub unsafe fn detect() -> Report {
    #[cfg(target_arch = "x86_64")]
    // SAFETY: forwarded from this function's own contract.
    unsafe {
        x86::detect()
    }
    #[cfg(target_arch = "aarch64")]
    // SAFETY: forwarded from this function's own contract.
    unsafe {
        arm::detect()
    }
}

#[cfg(target_arch = "x86_64")]
mod x86 {
    use super::{ClockPairing, Report};
    use crate::arch::x86::cpuid;

    /// `KVM_HC_CLOCK_PAIRING`, from `asm/kvm_para.h`.
    const KVM_HC_CLOCK_PAIRING: u64 = 9;
    /// `KVM_CLOCK_PAIRING_WALLCLOCK`, the only defined pairing type.
    const KVM_CLOCK_PAIRING_WALLCLOCK: u64 = 0;

    /// `struct kvm_clock_pairing`, which the host fills in.
    ///
    /// The layout is ABI: `{ s64 sec; s64 nsec; u64 tsc; u32 flags; u32
    /// pad[9]; }`, sixty-four bytes. The host writes by offset, so this must
    /// match exactly.
    #[repr(C, align(64))]
    #[derive(Default)]
    struct KvmClockPairing {
        sec: i64,
        nsec: i64,
        tsc: u64,
        flags: u32,
        pad: [u32; 9],
    }

    /// The buffer the host writes the pairing into.
    ///
    /// A static rather than a stack local because the hypercall takes a
    /// *physical* address: the identity map makes the two the same here, and
    /// a static has a fixed one that cannot move under the call.
    static mut PAIRING: KvmClockPairing = KvmClockPairing {
        sec: 0,
        nsec: 0,
        tsc: 0,
        flags: 0,
        pad: [0; 9],
    };

    /// # Safety
    /// Requires CPL 0.
    pub(super) unsafe fn detect() -> Report {
        let mut report = Report {
            cpuid_bit: cpuid(1, 0)[2] & (1 << 31) != 0,
            ..Default::default()
        };

        // The vendor leaf. Present even on some hypervisors that clear the
        // feature bit, which is why both are read.
        let leaf = cpuid(0x4000_0000, 0);
        if (0x4000_0000..=0x4001_0000).contains(&leaf[0]) {
            report.max_leaf = leaf[0];
            report.signature[0..4].copy_from_slice(&leaf[1].to_le_bytes());
            report.signature[4..8].copy_from_slice(&leaf[2].to_le_bytes());
            report.signature[8..12].copy_from_slice(&leaf[3].to_le_bytes());
        }

        // The active probe, and the guard it needs.
        //
        // `VMCALL` at CPL 0 under a hypervisor that implements it returns.
        // Anywhere else it is `#UD`, and this kernel has no IDT — so the
        // fault is a triple fault, not an error code. "Something looks like a
        // hypervisor" is *not* a sufficient guard: QEMU's TCG advertises the
        // vendor leaf as `TCGTCGTCGTCG` and implements no KVM hypercall at
        // all, which was exactly how this first went wrong.
        //
        // So the signature has to be KVM's specifically. `KVM_HC_*` is KVM's
        // interface; no other hypervisor answers it, and guessing costs the
        // machine.
        if report.signature_str().starts_with("KVMKVMKVM") {
            // SAFETY: the vendor leaf identifies KVM, which implements this
            // hypercall, and the caller guarantees CPL 0.
            unsafe {
                report.pairing = clock_pairing();
                report.hypercall_ok = report.pairing.is_some();
            }
        }
        report.hypercalls = super::hypercalls();
        report
    }

    /// Asks the host to pair its clock with this machine's counter.
    ///
    /// # Safety
    /// Issues `VMCALL`; requires CPL 0 *and* a hypervisor that implements it.
    /// Without one this is `#UD` with no handler.
    unsafe fn clock_pairing() -> Option<ClockPairing> {
        super::HYPERCALLS.fetch_add(1, super::Ordering::Relaxed);

        // The identity map means the virtual address is the physical one.
        let gpa = &raw const PAIRING as u64;
        let ret: i64;

        // SAFETY: the caller guarantees CPL 0 and that a hypervisor is
        // present. The host writes only into the buffer `gpa` names.
        //
        // RBX is shuttled through another register: LLVM reserves it and
        // rejects it as an operand, which is the same reason `cpuid` in
        // `nanochrono-core` is written this way.
        unsafe {
            core::arch::asm!(
                "xchg rbx, {gpa}",
                "vmcall",
                "xchg rbx, {gpa}",
                gpa = inout(reg) gpa => _,
                inlateout("rax") KVM_HC_CLOCK_PAIRING => ret,
                in("rcx") KVM_CLOCK_PAIRING_WALLCLOCK,
                options(nostack),
            );
        }
        if ret != 0 {
            return None;
        }

        // SAFETY: the host filled the buffer, and this is the only reader.
        let (sec, nsec, tsc) = unsafe { (PAIRING.sec, PAIRING.nsec, PAIRING.tsc) };
        if sec <= 0 {
            return None;
        }
        Some(ClockPairing {
            host_ns: (sec as u64).saturating_mul(1_000_000_000) + nsec.max(0) as u64,
            counter: tsc,
        })
    }
}

#[cfg(target_arch = "aarch64")]
mod arm {
    use super::{ClockPairing, Report};

    /// `ARM_SMCCC_VERSION`: every conforming implementation answers it.
    const SMCCC_VERSION: u64 = 0x8000_0000;
    /// `ARM_SMCCC_VENDOR_HYP_CALL_UID_FUNC_ID`: identifies the hypervisor.
    const VENDOR_HYP_UID: u64 = 0x8600_FF01;
    /// `ARM_SMCCC_VENDOR_HYP_KVM_PTP_FUNC_ID`: host time, the AArch64
    /// counterpart of `KVM_HC_CLOCK_PAIRING`.
    const KVM_PTP: u64 = 0x8600_0001;
    /// `KVM_PTP_VIRT_COUNTER`.
    const KVM_PTP_VIRT_COUNTER: u64 = 0;
    /// `SMCCC_RET_NOT_SUPPORTED`.
    const NOT_SUPPORTED: i64 = -1;

    /// # Safety
    /// Requires EL1 or above.
    pub(super) unsafe fn detect() -> Report {
        let mut report = Report::default();

        // AArch64 has no CPUID bit to read. `HVC` from EL1 either reaches an
        // EL2 handler or is undefined — and unlike x86 there is no recovery
        // from the undefined case without a vector table, so the counter
        // frequency is checked first as a cheap filter.
        let hz = nanochrono_core::arch::aarch64::cntfrq();
        let plausible_guest = hz == 62_500_000 || hz == 1_000_000_000;
        if !plausible_guest {
            return report;
        }

        // SAFETY: caller guarantees EL1+, and the frequency indicates a
        // virtual timer, so an EL2 handler is very likely present.
        unsafe {
            let version = hvc(SMCCC_VERSION, 0);
            if version[0] as i64 == NOT_SUPPORTED {
                return report;
            }
            report.hypercall_ok = true;

            let uid = hvc(VENDOR_HYP_UID, 0);
            if uid[0] as i64 != NOT_SUPPORTED {
                // The four words are reported raw: the byte order that
                // assembles them into a UUID has never been testable here
                // against a real AArch64 guest.
                for (i, word) in uid.iter().take(3).enumerate() {
                    report.signature[i * 4..i * 4 + 4]
                        .copy_from_slice(&(*word as u32).to_le_bytes());
                }
            }

            let ptp = hvc(KVM_PTP, KVM_PTP_VIRT_COUNTER);
            if (ptp[0] as i64) >= 0 {
                // a0:a1 is the host's ktime in nanoseconds, a2:a3 the guest
                // counter, each as two 32-bit halves — the convention
                // `drivers/ptp/ptp_kvm_arm.c` uses.
                report.pairing = Some(ClockPairing {
                    host_ns: (ptp[0] << 32) | (ptp[1] & 0xFFFF_FFFF),
                    counter: (ptp[2] << 32) | (ptp[3] & 0xFFFF_FFFF),
                });
            }
        }
        report.hypercalls = super::hypercalls();
        report
    }

    /// # Safety
    /// Requires EL1, and an EL2 handler — an `HVC` with none is undefined and
    /// this kernel has no vector table to recover through.
    unsafe fn hvc(function: u64, arg: u64) -> [u64; 4] {
        super::HYPERCALLS.fetch_add(1, super::Ordering::Relaxed);

        let mut regs = [0u64; 4];
        // SAFETY: forwarded from this function's own contract. Both function
        // IDs used here are read-only queries.
        unsafe {
            core::arch::asm!(
                "hvc #0",
                inlateout("x0") function => regs[0],
                inlateout("x1") arg => regs[1],
                out("x2") regs[2],
                out("x3") regs[3],
                options(nostack),
            );
        }
        regs
    }
}
