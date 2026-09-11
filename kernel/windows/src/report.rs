// SPDX-License-Identifier: MIT

//! Builds the `key=value` report handed back to user mode, mirroring the
//! output of the Linux module (`/dev/nanochrono`'s `ReportBuffer`).
//!
//! The report is written directly into the METHOD_BUFFERED `SystemBuffer`
//! supplied by the caller (never into a driver-owned static), so the driver
//! holds no shared report state.

use core::fmt::{self, Write};
use core::ptr;

use crate::hypercall;
use crate::nt;

/// Maximum bytes we are willing to put into a METHOD_BUFFERED output buffer.
pub const REPORT_CAPACITY: usize = 1500;

/// A `core::fmt::Write` sink over a caller-owned byte slice.
struct ReportBuffer<'a> {
    data: &'a mut [u8],
    len: usize,
}

impl<'a> ReportBuffer<'a> {
    fn new(data: &'a mut [u8]) -> Self {
        Self { data, len: 0 }
    }
}

impl<'a> fmt::Write for ReportBuffer<'a> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let src = s.as_bytes();
        let avail = self.data.len().saturating_sub(self.len);
        let n = src.len().min(avail);
        self.data[self.len..self.len + n].copy_from_slice(&src[..n]);
        self.len += n;
        Ok(())
    }
}

/// Writes the full hypervisor report into `out`, returning how many bytes were
/// produced (never more than `out.len()`).
pub fn write_report(out: &mut [u8]) -> usize {
    let mut r = ReportBuffer::new(out);

    let _ = writeln!(r, "version=1");

    // Architecture line, mirroring the Linux module's `mapping`.
    #[cfg(target_arch = "x86_64")]
    let _ = writeln!(r, "arch=x86");
    #[cfg(target_arch = "aarch64")]
    let _ = writeln!(r, "arch=arm64");

    // KeIsHypervisorPresent() is the WDM hypervisor hint. We combine it with
    // the CPUID bit before deciding to run hypercalls, because unlike the
    // Linux module (which has __ex_table fixups) a MinGW driver cannot recover
    // from a fault caused by executing a hypercall on bare metal.
    // SAFETY: kernel ABI import.
    let hv_present = unsafe { nt::KeIsHypervisorPresent() != 0 };
    let _ = writeln!(r, "hv_present={}", if hv_present { 1 } else { 0 });

    #[cfg(target_arch = "x86_64")]
    {
        let _ = writeln!(
            r,
            "cpuid_hypervisor_bit={}",
            if hypercall::hypervisor_present() { 1 } else { 0 }
        );
        let _ = writeln!(r, "vmx_available={}", if hypercall::vmx_available() { 1 } else { 0 });
        let _ = writeln!(r, "svm_available={}", if hypercall::svm_available() { 1 } else { 0 });

        let vendor = hypercall::vendor();
        let _ = writeln!(r, "cpuid_vendor={}", core::str::from_utf8(&vendor).unwrap_or("?"));

        // Detection gating: only run the hypercall trampolines when a
        // hypervisor is actually reported. See hypercall.rs module docs.
        let gate = hypercall::hypervisor_present() || hv_present;
        if gate {
            // SAFETY: hypervisor detected; see hypercall.rs.
            let vm = unsafe { hypercall::probe_vmcall() };
            let vmm = unsafe { hypercall::probe_vmmcall() };
            let _ = writeln!(r, "vmcall_ok=1");
            let _ = writeln!(r, "vmcall_result={:#x}", vm);
            let _ = writeln!(r, "vmmcall_ok={}", if vmm != 0 { 1 } else { 0 });
            let _ = writeln!(r, "vmmcall_result={:#x}", vmm);

            // Hypercall exit cost via KeQueryPerformanceCounter, the WDM
            // replacement for the Linux module's rdtsc delta.
            let t0 = kern_counter();
            // SAFETY: gated as above; diagnostics only.
            let _ = unsafe { hypercall::probe_vmcall() };
            let t1 = kern_counter();
            let _ = writeln!(r, "exit_cycles={}", t1.saturating_sub(t0));
        } else {
            let _ = writeln!(r, "vmcall_ok=0");
            let _ = writeln!(r, "vmcall_result=0x0");
            let _ = writeln!(r, "vmmcall_ok=0");
            let _ = writeln!(r, "vmmcall_result=0x0");
            let _ = writeln!(r, "exit_cycles=0");
        }
    }

    #[cfg(target_arch = "aarch64")]
    {
        let el = hypercall::current_el();
        let _ = writeln!(r, "current_el={}", el);

        if hv_present {
            // SAFETY: KeIsHypervisorPresent() reported a hypervisor; see
            // hypercall.rs for the EL1+ gating.
            let uid = unsafe { hypercall::probe_hvc(hypercall::SMCCC_VENDOR_HYP_CALL_UID_FUNC_ID) };
            let _ = writeln!(r, "hvc_ok=1");
            // SMCCC UID is 128 bits; print as four host-order u32 chunks.
            let _ = writeln!(
                r,
                "hvc_vendor_uid={:08x}-{:08x}-{:08x}-{:08x}",
                uid[0] as u32,
                (uid[0] >> 32) as u32,
                uid[1] as u32,
                (uid[1] >> 32) as u32
            );
        } else {
            let _ = writeln!(r, "hvc_ok=0");
            let _ = writeln!(r, "hvc_vendor_uid=0-0-0-0");
        }
    }

    phys_memory_demo(&mut r);
    let _ = writeln!(r, "done_ok=1");

    r.len
}

/// Runs the physical-memory demo: allocate one nonpaged page, resolve its
/// physical address, map and read it back, then release everything — the WDM
/// analogue of the Linux module's `virt_to_phys` / `ioremap` probe.
fn phys_memory_demo(r: &mut ReportBuffer<'_>) {
    const PAGE: usize = 4 * 1024;
    // SAFETY: NonPagedPoolNx allocation; freed in every path below.
    unsafe {
        let va = nt::ExAllocatePoolWithTag(nt::POOL_NON_PAGED_NX, PAGE, nt::POOL_TAG);
        if va.is_null() {
            let _ = writeln!(r, "phys_ok=0");
            let _ = writeln!(r, "phys_error=alloc_failed");
            return;
        }
        // Write a recognizable pattern so the readback is meaningful.
        ptr::write_volatile(va.cast::<u32>(), 0x4E414E4F);
        // Barrier so the store is visible before the MMIO-style read.
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

        let phys = nt::MmGetPhysicalAddress(va);
        let mapped = nt::MmMapIoSpace(phys, PAGE, nt::MM_NON_CACHED);
        if mapped.is_null() {
            let _ = writeln!(r, "phys_ok=0");
            let _ = writeln!(r, "phys_error=map_failed");
            let _ = writeln!(r, "phys_addr={:#x}", phys.quad_part as u64);
            nt::ExFreePoolWithTag(va, nt::POOL_TAG);
            return;
        }
        let readback = ptr::read_volatile(mapped.cast::<u32>());
        nt::MmUnmapIoSpace(mapped, PAGE);
        nt::ExFreePoolWithTag(va, nt::POOL_TAG);

        let _ = writeln!(r, "phys_ok=1");
        let _ = writeln!(r, "phys_addr={:#x}", phys.quad_part as u64);
        let _ = writeln!(r, "phys_readback={:#x}", readback);
    }
}

/// `KeQueryPerformanceCounter` wrapper (x64 timing).
#[cfg(target_arch = "x86_64")]
fn kern_counter() -> i64 {
    // SAFETY: kernel ABI import; passing NULL as the optional argument is
    // accepted by the API.
    unsafe { nt::KeQueryPerformanceCounter(ptr::null_mut()).quad_part }
}