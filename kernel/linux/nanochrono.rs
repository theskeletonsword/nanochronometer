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

use core::ffi::{c_char, c_int, c_uint, c_void};
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

/// Bumped when the report gains or changes fields.
///
/// Version 2 added the CPU's own vendor and lineage, the Centaur extended
/// leaf maximum, and the in-kernel crypto timings. A reader that finds
/// version 1 is talking to a module built before those existed — which is
/// what an already-loaded module from an earlier build looks like, and why
/// this number is worth checking rather than assuming a rebuild reached the
/// running kernel.
const FORMAT_VERSION: u32 = 2;

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
    write_arch_report(f)?;
    write_crypto_report(f)
}

// ---------------------------------------------------------------------------
// Ring 0 crypto, the optional half of the crypto benchmark
// ---------------------------------------------------------------------------

// The userspace side of this measurement reaches the kernel's crypto API
// through `AF_ALG`: a socket, a `sendmsg` and a `read` per operation. That
// number is the honest cost of *using* kernel crypto from a program, and it
// is the one that always gets measured, because it needs no module.
//
// It cannot separate the primitive from the transport. This can: the same
// algorithms called here run with no socket, no syscall and no copy across
// the privilege boundary, so the difference between the two numbers is what
// `AF_ALG` costs. That is the whole reason this half exists, and it is why
// it is optional — it answers a sharper question, at the price of building
// and loading a module.
//
// Hashes only. A `shash` is a single exported call over a flat buffer;
// a symmetric cipher needs a request object, scatterlists and a completion,
// and a benchmark that got any of those wrong would report a number for
// something other than what it named. What is here is certainly right.

/// `struct crypto_shash` is opaque to this module: it is only ever held as a
/// pointer and handed back to the kernel.
#[repr(C)]
struct CryptoShash {
    _opaque: [u8; 0],
}

unsafe extern "C" {
    /// `crypto_alloc_shash(const char *alg_name, u32 type, u32 mask)`.
    ///
    /// Returns an `ERR_PTR` rather than null on failure, which is why the
    /// caller checks the pointer's magnitude rather than testing for null.
    fn crypto_alloc_shash(alg_name: *const c_char, ty: u32, mask: u32) -> *mut CryptoShash;
    /// `crypto_shash_tfm_digest(tfm, data, len, out)` — hash a flat buffer in
    /// one call, with the descriptor allocated by the kernel. Exported since
    /// 5.8 precisely so a caller need not build a `SHASH_DESC_ON_STACK`.
    fn crypto_shash_tfm_digest(
        tfm: *mut CryptoShash,
        data: *const u8,
        len: c_uint,
        out: *mut u8,
    ) -> c_int;
    /// `crypto_destroy_tfm(void *mem, struct crypto_tfm *tfm)`. The shash
    /// free is a macro over this in C; from here it is the exported symbol.
    fn crypto_destroy_tfm(mem: *mut c_void, tfm: *mut CryptoShash);
}

/// The last address that can be an `ERR_PTR`.
///
/// The kernel returns errors as pointers in the top page. Anything at or
/// above this is a negative errno wearing a pointer's clothes, and
/// dereferencing it is how a module oopses.
const ERR_PTR_FLOOR: usize = usize::MAX - 4095;

/// Algorithms measured here, by the kernel's own names.
///
/// The same five the userspace side asks for through `AF_ALG`, so every row
/// of the ring-0 mode has a ring-3 row measuring the same algorithm over the
/// same buffer, and the two can simply be subtracted. An algorithm this
/// kernel was not built with is skipped rather than reported as zero.
const CRYPTO_ALGORITHMS: &[&str] = &["sha1", "sha256", "sha512", "sha3-256", "crc32c"];

/// Bytes hashed per operation.
///
/// The same size the userspace benchmark uses, so the two numbers are
/// comparable. A different buffer would make the comparison meaningless
/// while still looking like one.
const CRYPTO_PAYLOAD: usize = 16 * 1024;

/// Operations per algorithm. Small: this runs inside a `/proc` read, with
/// preemption enabled and no business holding a CPU for long.
const CRYPTO_ROUNDS: usize = 64;

/// The largest digest any algorithm here produces.
const MAX_DIGEST: usize = 64;

/// Times each algorithm and writes one line per success.
///
/// Emitted as repeated `crypto=` keys rather than one key per algorithm,
/// because an algorithm's kernel name can contain characters — `cbc(aes)` —
/// that have no place on the left of an `=`.
///
/// A failure is silent by design: an algorithm this kernel was not built with
/// is an ordinary kernel, and a missing line says that more clearly than an
/// error line would.
fn write_crypto_report(f: &mut ReportBuffer) -> fmt::Result {
    writeln!(f, "crypto_payload_bytes={CRYPTO_PAYLOAD}")?;
    writeln!(f, "crypto_rounds={CRYPTO_ROUNDS}")?;

    for name in CRYPTO_ALGORITHMS {
        if let Some(cycles) = time_shash(name) {
            writeln!(f, "crypto={name},{cycles}")?;
        }
    }
    Ok(())
}

/// Best-of-N cycles for one full digest of [`CRYPTO_PAYLOAD`] bytes.
///
/// Best rather than mean, for the same reason every other measurement in this
/// project takes a minimum: the fastest observed run is the one least
/// disturbed by everything else the machine was doing, and in a kernel with
/// preemption on that is the only figure with a defensible meaning.
///
/// `None` when the algorithm is not available, which is not an error.
fn time_shash(name: &str) -> Option<u64> {
    // The C API takes a NUL-terminated name and these are compile-time
    // constants, so the terminator is added here rather than requiring every
    // entry in the table to carry one.
    let mut zname = [0u8; 32];
    let bytes = name.as_bytes();
    if bytes.len() >= zname.len() {
        return None;
    }
    zname[..bytes.len()].copy_from_slice(bytes);

    // SAFETY: `zname` is NUL-terminated and outlives the call; type and mask
    // of zero ask for any implementation, which is what the kernel's own
    // callers pass.
    let tfm = unsafe { crypto_alloc_shash(zname.as_ptr().cast(), 0, 0) };
    if tfm.is_null() || (tfm as usize) >= ERR_PTR_FLOOR {
        return None;
    }

    let mut best = u64::MAX;
    let mut digest = [0u8; MAX_DIGEST];
    // A static rather than a stack buffer: sixteen kilobytes is far past what
    // a kernel stack will hold, and this runs single-threaded under the
    // procfs read lock.
    let payload = payload_buffer();

    for _ in 0..CRYPTO_ROUNDS {
        let start = counter_start();
        // SAFETY: `tfm` was allocated above and not freed; the payload and
        // digest pointers are valid for the lengths given, and the digest
        // buffer is the largest any listed algorithm produces.
        let rc = unsafe {
            crypto_shash_tfm_digest(
                tfm,
                payload.as_ptr(),
                CRYPTO_PAYLOAD as c_uint,
                digest.as_mut_ptr(),
            )
        };
        let end = counter_end();
        if rc != 0 {
            best = u64::MAX;
            break;
        }
        let elapsed = end.wrapping_sub(start);
        if elapsed != 0 && elapsed < best {
            best = elapsed;
        }
    }

    // SAFETY: `tfm` came from `crypto_alloc_shash` and is freed exactly once.
    unsafe { crypto_destroy_tfm(core::ptr::null_mut(), tfm) };

    (best != u64::MAX).then_some(best)
}

/// The buffer every digest runs over.
///
/// Zero-filled and never written: its contents do not change what a hash
/// costs, and a constant makes runs comparable across boots.
fn payload_buffer() -> &'static [u8; CRYPTO_PAYLOAD] {
    static PAYLOAD: [u8; CRYPTO_PAYLOAD] = [0u8; CRYPTO_PAYLOAD];
    &PAYLOAD
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

    // Who made the part. Read before anything else because it decides how the
    // rest is interpreted — and because a Zhaoxin is close enough to an Intel
    // that code assuming "not AMD means Intel" runs on one and misreads it.
    //
    // The register order is EBX, EDX, ECX. That is not a transcription slip:
    // it is the order `CPUID.0H` defines, and reading them as EBX, ECX, EDX
    // spells `GenuntelineI`.
    let identity = cpuid(0, 0);
    let mut cpu_vendor = [0u8; 12];
    cpu_vendor[0..4].copy_from_slice(&identity[1].to_le_bytes());
    cpu_vendor[4..8].copy_from_slice(&identity[3].to_le_bytes());
    cpu_vendor[8..12].copy_from_slice(&identity[2].to_le_bytes());
    write!(f, "cpu_vendor=")?;
    for &byte in cpu_vendor.iter() {
        if (0x20..0x7f).contains(&byte) {
            write!(f, "{}", byte as char)?;
        }
    }
    writeln!(f)?;
    writeln!(f, "cpu_family={}", cpu_family_name(&cpu_vendor))?;

    // The Centaur extended range. Only the VIA/Centaur lineage and its
    // Zhaoxin successor implement it — it is to them what `0x8000_0000` is to
    // AMD — so a maximum inside the range it describes is positive
    // identification even where the vendor string has been overridden, which
    // firmware and hypervisors both do.
    let centaur_max = cpuid(0xC000_0000, 0)[0];
    if (0xC000_0000..=0xC000_FFFF).contains(&centaur_max) {
        writeln!(f, "centaur_max_leaf={centaur_max:#x}")?;
    }

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

/// Names the lineage a vendor signature belongs to.
///
/// Zhaoxin is the reason this exists. Its parts are x86-64 descended from
/// VIA's Centaur line: they carry Intel-style architectural performance
/// counters, Intel-style machine-check banks, and VMX — so KVM drives them
/// through the same `VMCALL` path an Intel part uses, and the hypercall probe
/// below needs no special case. What they do *not* share is Intel's
/// trustworthy `CPUID.15H`/`16H` counter-rate leaves, which Linux reads on
/// Intel alone. Naming the part is what lets a reader of this report know
/// which of those two facts applies.
fn cpu_family_name(signature: &[u8; 12]) -> &'static str {
    match signature {
        b"GenuineIntel" => "intel",
        b"AuthenticAMD" => "amd",
        // Spaces included: the string is exactly twelve bytes and Zhaoxin
        // pads it on both sides.
        b"  Shanghai  " => "zhaoxin",
        b"CentaurHauls" => "centaur",
        _ => "unknown",
    }
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
