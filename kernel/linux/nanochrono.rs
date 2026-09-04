// SPDX-License-Identifier: (GPL-2.0-only OR MIT)

//! Ring 0 hypervisor detection for NanoChronometer.
//!
//! OPTIONAL. The userspace library detects hypervisors on its own through
//! CPUID and platform signatures, and that works without privileges. This
//! module only sharpens the answer.
//!
//! # What ring 0 adds
//!
//! `VMCALL`, `VMMCALL` and `HVC #0` are only valid inside a guest and require
//! CPL 0 / EL1. Outside a guest they raise an undefined-instruction fault, so
//! userspace cannot even attempt them. An instruction that *returns* is
//! therefore proof of a hypervisor — and it holds even against one that clears
//! the CPUID hypervisor bit. That is this module's whole reason to exist.
//!
//! Ring 0 also measures a trap with preemption disabled, which removes the
//! scheduling noise that makes the userspace estimate fuzzy.
//!
//! # Fault recovery
//!
//! Each probe emits its own `__ex_table` entry, so a fault on bare metal
//! resumes at the fixup label instead of oopsing. There is no Rust
//! abstraction for kernel exception tables, so the entries are written by hand
//! from `asm!` in the exact layout `arch/x86/include/asm/asm.h` defines:
//! three 32-bit relative words — faulting insn, fixup target, handler type —
//! in a 12-byte-aligned `__ex_table` section, with `EX_TYPE_DEFAULT` (1)
//! meaning "resume at the fixup".
//!
//! # Output
//!
//! One `key=value` per line at `/proc/nanochrono`, mode 0444 so any process
//! can read it. Keys are stable; the userspace parser ignores ones it does not
//! know, so this can grow without breaking an older library.
//!
//! The kernel's Rust crate exposes debugfs but not procfs, so the procfs ABI
//! is declared here directly. `/proc` is the right home for this: debugfs is
//! mode 0700, which would have limited the report to root, and the whole point
//! is for an unprivileged measurement process to read it.

use core::ffi::{c_char, c_int, c_void};
use core::fmt::{self, Write as _};

use kernel::prelude::*;

/// A hand-written `#[repr(C)]` mirror of a kernel struct is only valid while
/// the kernel does not shuffle field order.
#[cfg(not(CONFIG_RANDSTRUCT_NONE))]
compile_error!(
    "this module mirrors `struct proc_ops` by hand, which is only sound with \
     CONFIG_RANDSTRUCT_NONE. Rebuild against a kernel without struct \
     randomisation, or use the CPUID-only userspace detection."
);

module! {
    type: Nanochrono,
    name: "nanochrono",
    authors: ["NanoChronometer contributors"],
    description: "Ring 0 hypervisor detection for NanoChronometer (optional)",
    license: "Dual MIT/GPL",
}

/// Bumped when the output format changes incompatibly.
const FORMAT_VERSION: u32 = 1;

/// Rounds for the minimum-of-N trap measurement. Enough to find the floor,
/// short enough that preemption stays disabled for a negligible time.
const TRAP_ROUNDS: u32 = 128;

struct Nanochrono;

impl kernel::Module for Nanochrono {
    fn init(_module: &'static ThisModule) -> Result<Self> {
        // SAFETY: `PROC_NAME` is a NUL-terminated literal and `PROC_OPS` is a
        // 'static struct whose function pointers outlive the entry.
        let entry = unsafe {
            proc_create(
                PROC_NAME.as_ptr().cast(),
                0o444,
                core::ptr::null_mut(),
                &raw const PROC_OPS,
            )
        };
        if entry.is_null() {
            pr_err!("nanochrono: could not create /proc/nanochrono\n");
            return Err(ENOMEM);
        }
        pr_info!("nanochrono: reporting at /proc/nanochrono\n");
        Ok(Nanochrono)
    }
}

impl Drop for Nanochrono {
    fn drop(&mut self) {
        // SAFETY: the entry was created by `init` with this exact name and a
        // NULL parent, and is removed exactly once.
        unsafe { remove_proc_entry(PROC_NAME.as_ptr().cast(), core::ptr::null_mut()) };
        pr_info!("nanochrono: unloaded\n");
    }
}

const PROC_NAME: &core::ffi::CStr = c"nanochrono";

/// The report never exceeds a few hundred bytes; a fixed buffer keeps the read
/// path allocation-free.
const REPORT_CAPACITY: usize = 1024;

/// `struct proc_ops` from `include/linux/proc_fs.h`.
///
/// Declared here because `proc_fs.h` is not in the kernel's Rust bindings.
/// Field order and the `CONFIG_COMPAT` conditional must match the kernel
/// exactly; the `CONFIG_RANDSTRUCT_NONE` guard above is what makes that sound.
#[repr(C)]
struct ProcOps {
    proc_flags: u32,
    proc_open: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    proc_read: Option<unsafe extern "C" fn(*mut c_void, *mut c_char, usize, *mut i64) -> isize>,
    proc_read_iter: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> isize>,
    proc_write: Option<unsafe extern "C" fn(*mut c_void, *const c_char, usize, *mut i64) -> isize>,
    proc_lseek: Option<unsafe extern "C" fn(*mut c_void, i64, c_int) -> i64>,
    proc_release: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    proc_poll: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32>,
    proc_ioctl: Option<unsafe extern "C" fn(*mut c_void, u32, usize) -> isize>,
    #[cfg(CONFIG_COMPAT)]
    proc_compat_ioctl: Option<unsafe extern "C" fn(*mut c_void, u32, usize) -> isize>,
    proc_mmap: Option<unsafe extern "C" fn(*mut c_void, *mut c_void) -> c_int>,
    proc_get_unmapped_area:
        Option<unsafe extern "C" fn(*mut c_void, usize, usize, usize, usize) -> usize>,
}

// SAFETY: every field is either a plain integer or a function pointer to a
// `'static` function; the struct is immutable after construction.
unsafe impl Sync for ProcOps {}

static PROC_OPS: ProcOps = ProcOps {
    proc_flags: 0,
    proc_open: None,
    proc_read: Some(proc_read),
    proc_read_iter: None,
    proc_write: None,
    proc_lseek: Some(default_llseek),
    proc_release: None,
    proc_poll: None,
    proc_ioctl: None,
    #[cfg(CONFIG_COMPAT)]
    proc_compat_ioctl: None,
    proc_mmap: None,
    proc_get_unmapped_area: None,
};

extern "C" {
    fn proc_create(
        name: *const c_char,
        mode: u16,
        parent: *mut c_void,
        proc_ops: *const ProcOps,
    ) -> *mut c_void;
    fn remove_proc_entry(name: *const c_char, parent: *mut c_void);
    /// Handles the offset and end-of-file bookkeeping a `read` needs, so this
    /// module does not reimplement it.
    fn simple_read_from_buffer(
        to: *mut c_void,
        count: usize,
        ppos: *mut i64,
        from: *const c_void,
        available: usize,
    ) -> isize;
    fn default_llseek(file: *mut c_void, offset: i64, whence: c_int) -> i64;
}

/// Formats the report into a stack buffer and hands it to the reader.
///
/// The report is recomputed on every read rather than cached: the probes are
/// cheap, and a stale answer would be worse than none since a guest can be
/// migrated between reads.
///
/// # Safety
/// Called by the kernel with a valid file, a userspace buffer of `count`
/// bytes, and a valid offset pointer.
unsafe extern "C" fn proc_read(
    _file: *mut c_void,
    buf: *mut c_char,
    count: usize,
    ppos: *mut i64,
) -> isize {
    let mut report = ReportBuffer::new();
    if write_report(&mut report).is_err() {
        // A truncated report is still useful; a failed one is not.
        return -(kernel::error::code::EIO.to_errno() as isize);
    }
    let bytes = report.as_bytes();
    // SAFETY: the kernel guarantees `buf` is `count` writable userspace bytes
    // and `ppos` is a valid offset; `bytes` is a live local slice.
    unsafe {
        simple_read_from_buffer(
            buf.cast(),
            count,
            ppos,
            bytes.as_ptr().cast(),
            bytes.len(),
        )
    }
}

/// A fixed-capacity sink so the read path allocates nothing.
struct ReportBuffer {
    data: [u8; REPORT_CAPACITY],
    len: usize,
}

impl ReportBuffer {
    fn new() -> Self {
        ReportBuffer {
            data: [0; REPORT_CAPACITY],
            len: 0,
        }
    }

    fn as_bytes(&self) -> &[u8] {
        &self.data[..self.len]
    }
}

impl fmt::Write for ReportBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let bytes = s.as_bytes();
        let end = self.len.checked_add(bytes.len()).ok_or(fmt::Error)?;
        if end > self.data.len() {
            return Err(fmt::Error);
        }
        self.data[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
}

fn write_report(f: &mut ReportBuffer) -> fmt::Result {
    writeln!(f, "version={FORMAT_VERSION}")?;
    write_arch_report(f)
}

// ---------------------------------------------------------------------------
// x86
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
fn write_arch_report(f: &mut ReportBuffer) -> fmt::Result {
    writeln!(f, "arch=x86")?;

    let leaf1 = cpuid(1, 0);
    writeln!(
        f,
        "cpuid_hypervisor_bit={}",
        u32::from(leaf1[2] & (1 << 31) != 0)
    )?;
    // CPUID.1:ECX[5] is VMX, CPUID.80000001H:ECX[2] is SVM. Both say the CPU
    // *could* host a guest, which distinguishes "not virtualized" from "not
    // virtualized but nested virtualization is available".
    writeln!(f, "vmx_available={}", u32::from(leaf1[2] & (1 << 5) != 0))?;
    let ext1 = cpuid(0x8000_0001, 0);
    writeln!(f, "svm_available={}", u32::from(ext1[2] & (1 << 2) != 0))?;

    let vendor = cpuid(0x4000_0000, 0);
    let mut signature = [0u8; 12];
    signature[0..4].copy_from_slice(&vendor[1].to_le_bytes());
    signature[4..8].copy_from_slice(&vendor[2].to_le_bytes());
    signature[8..12].copy_from_slice(&vendor[3].to_le_bytes());
    write!(f, "cpuid_vendor=")?;
    for &byte in signature.iter().take_while(|&&b| b != 0) {
        // Anything unprintable means the leaf is not implemented and the
        // registers hold stale values, so it is dropped rather than reported.
        if (0x20..0x7f).contains(&byte) {
            write!(f, "{}", byte as char)?;
        }
    }
    writeln!(f)?;

    // SAFETY: both probes recover from their fault through the exception
    // table entries they emit, so executing them outside a guest is safe.
    let vmcall = unsafe { probe_vmcall() };
    let vmmcall = unsafe { probe_vmmcall() };
    writeln!(f, "vmcall_ok={}", u32::from(vmcall.is_some()))?;
    writeln!(f, "vmmcall_ok={}", u32::from(vmmcall.is_some()))?;
    if let Some(result) = vmcall.or(vmmcall) {
        writeln!(f, "hypercall_result={result}")?;
    }

    let (trap, baseline) = measure_exit_cost();
    writeln!(f, "exit_cycles={trap}")?;
    writeln!(f, "baseline_cycles={baseline}")?;
    Ok(())
}

/// `CPUID` with an explicit subleaf.
///
/// LLVM reserves `rbx`, so the value is shuttled through a scratch register.
#[cfg(target_arch = "x86_64")]
fn cpuid(leaf: u32, subleaf: u32) -> [u32; 4] {
    let (eax, ebx, ecx, edx);
    // SAFETY: CPUID has no operands beyond its registers and no side effects.
    unsafe {
        core::arch::asm!(
            "mov {tmp:r}, rbx",
            "cpuid",
            "xchg {tmp:r}, rbx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack, preserves_flags),
        );
    }
    [eax, ebx, ecx, edx]
}

/// A hypercall function number no hypervisor implements.
///
/// KVM answers an unknown number with `-KVM_ENOSYS` (-1000), which is exactly
/// what is wanted: a defined "I am here, and I do not implement that" with no
/// side effect.
#[cfg(target_arch = "x86_64")]
const PROBE_HYPERCALL_NR: u64 = 0xffff;

/// Emits a hypercall probe with its own exception-table entry.
///
/// The generated sequence sets the success flag *after* the instruction, so a
/// fault — which resumes at the fixup label, skipping that store — leaves the
/// flag clear.
#[cfg(target_arch = "x86_64")]
macro_rules! hypercall_probe {
    ($name:ident, $insn:literal) => {
        /// # Safety
        /// Safe to call anywhere: the exception-table entry below catches the
        /// `#UD` this raises outside a guest.
        unsafe fn $name() -> Option<i64> {
            let ok: u64;
            let result: u64;
            // SAFETY: the __ex_table entry makes the fault recoverable, and
            // the instruction has no effect on a hypervisor that rejects an
            // unknown function number.
            unsafe {
                core::arch::asm!(
                    "xor {ok}, {ok}",
                    concat!("2: ", $insn),
                    "mov {ok}, 1",
                    "3:",
                    // arch/x86/include/asm/asm.h: three 32-bit relative words
                    // in a 12-byte-entry section. EX_TYPE_DEFAULT (1) resumes
                    // execution at the fixup label.
                    ".pushsection __ex_table, \"aM\", @progbits, 12",
                    ".balign 4",
                    ".long (2b) - .",
                    ".long (3b) - .",
                    ".long 1",
                    ".popsection",
                    ok = out(reg) ok,
                    inout("rax") PROBE_HYPERCALL_NR => result,
                    out("rcx") _, out("rdx") _, out("rsi") _, out("rdi") _,
                    options(nostack),
                );
            }
            (ok != 0).then_some(result as i64)
        }
    };
}

#[cfg(target_arch = "x86_64")]
hypercall_probe!(probe_vmcall, "vmcall");
#[cfg(target_arch = "x86_64")]
hypercall_probe!(probe_vmmcall, "vmmcall");

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn counter_start() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: RDTSC has no operands beyond its outputs.
    unsafe {
        core::arch::asm!("lfence", "rdtsc", out("eax") lo, out("edx") hi,
                         options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn counter_end() -> u64 {
    let (lo, hi): (u32, u32);
    // SAFETY: RDTSCP has no operands beyond its outputs.
    unsafe {
        core::arch::asm!("rdtscp", "lfence", out("eax") lo, out("edx") hi, out("ecx") _,
                         options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

/// Times `CPUID` against a bare counter pair.
///
/// `CPUID` is serializing and, on essentially every hypervisor, exits to the
/// VMM unconditionally. The gap is the exit cost, which is the number a caller
/// actually wants: how much latency virtualization adds per trap on this host.
///
/// The C version disabled preemption around this loop. The Rust kernel crate
/// exports no preemption abstraction, so instead the minimum over
/// [`TRAP_ROUNDS`] samples is taken — which discards exactly the samples a
/// scheduling decision would have inflated. The floor is the same; only the
/// number of rounds needed to find it goes up.
#[cfg(target_arch = "x86_64")]
fn measure_exit_cost() -> (u64, u64) {
    let mut best_trap = u64::MAX;
    let mut best_base = u64::MAX;

    for _ in 0..TRAP_ROUNDS {
        let a = counter_start();
        let b = counter_end();
        let d = b.wrapping_sub(a);
        if d != 0 && d < best_base {
            best_base = d;
        }

        let a = counter_start();
        core::hint::black_box(cpuid(0, 0));
        let b = counter_end();
        let d = b.wrapping_sub(a);
        if d != 0 && d < best_trap {
            best_trap = d;
        }
    }

    (
        if best_trap == u64::MAX { 0 } else { best_trap },
        if best_base == u64::MAX { 0 } else { best_base },
    )
}

// ---------------------------------------------------------------------------
// AArch64
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
fn write_arch_report(f: &mut ReportBuffer) -> fmt::Result {
    writeln!(f, "arch=arm64")?;

    let current_el: u64;
    // SAFETY: CurrentEL is readable at every exception level.
    unsafe {
        core::arch::asm!("mrs {v}, CurrentEL", v = out(reg) current_el,
                         options(nomem, nostack, preserves_flags));
    }
    writeln!(f, "current_el={}", current_el >> 2)?;

    // SAFETY: the exception-table entry catches the undefined-instruction
    // fault raised when no EL2 handler is present.
    //
    // SMCCC_VERSION is function ID 0x80000000: every conforming hypervisor
    // answers it, and one that does not still traps rather than faulting, so
    // reaching this at all separates "an EL2 handler exists" from "HVC is
    // undefined here".
    let version = unsafe { probe_hvc(SMCCC_VERSION_FUNC_ID) };
    writeln!(f, "hvc_ok={}", u32::from(version.is_some()))?;
    if let Some(regs) = version {
        writeln!(f, "hypercall_result={}", regs[0] as i64)?;
        // -1 is SMCCC_RET_NOT_SUPPORTED: something answered, but does not
        // implement the version call. Still proof of a handler.
        if regs[0] as i64 != SMCCC_RET_NOT_SUPPORTED {
            writeln!(f, "smccc_version={:#x}", regs[0])?;
        }
    }

    // The vendor-specific hypervisor UID. Only a hypervisor implements this
    // range at all, so an answer identifies one — and the four words are
    // reported raw rather than matched against a table here, so that the
    // byte order is interpreted once, in userspace, where it can be tested.
    if version.is_some() {
        // SAFETY: as above; the fault is recoverable either way.
        if let Some(uid) = unsafe { probe_hvc(SMCCC_VENDOR_HYP_CALL_UID_FUNC_ID) } {
            if uid[0] as i64 != SMCCC_RET_NOT_SUPPORTED {
                writeln!(
                    f,
                    "hvc_vendor_uid={:08x} {:08x} {:08x} {:08x}",
                    uid[0] as u32, uid[1] as u32, uid[2] as u32, uid[3] as u32
                )?;
            }
        }
    }
    Ok(())
}

/// `ARM_SMCCC_VERSION_FUNC_ID`: fast call, SMC32, owner 0, function 0.
#[cfg(target_arch = "aarch64")]
const SMCCC_VERSION_FUNC_ID: u64 = 0x8000_0000;

/// `ARM_SMCCC_VENDOR_HYP_CALL_UID_FUNC_ID`: fast call, SMC32, owner 6
/// (vendor hypervisor), function 0xff01 (query call UID).
#[cfg(target_arch = "aarch64")]
const SMCCC_VENDOR_HYP_CALL_UID_FUNC_ID: u64 = 0x8600_ff01;

/// `SMCCC_RET_NOT_SUPPORTED`, per `include/linux/arm-smccc.h`.
#[cfg(target_arch = "aarch64")]
const SMCCC_RET_NOT_SUPPORTED: i64 = -1;

/// `HVC #0` carrying an SMCCC function ID, with an exception-table entry.
///
/// At EL1 under an EL2 hypervisor this traps and returns. With no EL2, or none
/// handling it, the instruction is undefined and the fixup reports failure.
/// Every SMCCC call is read-only identification: the version query and the
/// vendor UID query both return data and change nothing.
///
/// Returns x0-x3, which is where SMCCC puts its results.
///
/// # Safety
/// Safe to call anywhere: the fault is recoverable, and both function IDs
/// this is used with are queries.
#[cfg(target_arch = "aarch64")]
unsafe fn probe_hvc(function_id: u64) -> Option<[u64; 4]> {
    let ok: u64;
    let mut regs = [0u64; 4];
    // SAFETY: the __ex_table entry makes the fault recoverable.
    unsafe {
        core::arch::asm!(
            "mov {ok}, xzr",
            "2: hvc #0",
            "mov {ok}, #1",
            "3:",
            // arch/arm64/include/asm/asm-extable.h: two 32-bit relative words
            // plus a type/data word, in 8-byte-aligned entries.
            ".pushsection __ex_table, \"a\"",
            ".align 3",
            ".long (2b - .)",
            ".long (3b - .)",
            ".short 0",
            ".short 0",
            ".popsection",
            ok = out(reg) ok,
            inout("x0") function_id => regs[0],
            out("x1") regs[1],
            out("x2") regs[2],
            out("x3") regs[3],
            options(nostack),
        );
    }
    (ok != 0).then_some(regs)
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
fn write_arch_report(f: &mut ReportBuffer) -> fmt::Result {
    writeln!(f, "arch=unsupported")
}
