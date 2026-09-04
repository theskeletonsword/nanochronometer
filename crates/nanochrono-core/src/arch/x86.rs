// SPDX-License-Identifier: Apache-2.0
//! x86 counter and probe primitives, 32- and 64-bit.
//!
//! Every instruction the old `asm/x64/**/*.asm` files emitted is expressed here
//! as a `core::arch::asm!` block.
//!
//! # What 32-bit x86 gets
//!
//! The counter layer is width-independent: `RDTSC`, `RDTSCP`, `CPUID` and the
//! fences are identical instructions with identical encodings on i686, so the
//! Android `x86` ABI reads the TSC exactly like `x86_64` does. That matters —
//! without it the target falls back to `clock_gettime`, which costs some
//! 15,000 cycles per read pair against roughly 30 for `RDTSCP`.
//!
//! The SIMD probes and ISA kernels are 64-bit only: they are written against
//! the 64-bit register file, and rewriting fifteen assembly blocks to buy
//! microbenchmarks on an ABI that exists mainly for emulators is not a trade
//! worth making. [`crate::backend::Backend`] reports those families as
//! unavailable on 32-bit rather than silently substituting a scalar loop. There is no separate assembler source and no
//! build-time assembler step: the ISA sequences live next to the code that
//! dispatches them, so `cargo build` is the whole toolchain.
//!
//! Two invariants are preserved from the assembly originals:
//!
//! * Serialisation is explicit. `LFENCE`/`MFENCE`/`CPUID` placement around
//!   `RDTSC`/`RDTSCP` is part of the measurement contract, not an optimisation
//!   detail, so every probe spells it out.
//! * Nothing here is reachable without a CPUID + XGETBV gate. Functions that
//!   need an ISA extension carry `#[target_feature]` and are `unsafe`; the
//!   dispatchers in [`crate::simd`] and [`crate::backend`] own the gate.

use core::arch::asm;

// ---------------------------------------------------------------------------
// CPUID / XGETBV
// ---------------------------------------------------------------------------

/// `CPUID` with an explicit subleaf.
///
/// LLVM reserves `rbx` on x86-64, so the value is shuttled through a scratch
/// register: `mov tmp, rbx` saves it, `cpuid` overwrites it, `xchg tmp, rbx`
/// swaps the result out and the original back in one instruction.
#[inline]
pub fn cpuid(leaf: u32, subleaf: u32) -> [u32; 4] {
    let (eax, ebx, ecx, edx);
    unsafe {
        #[cfg(target_arch = "x86_64")]
        asm!(
            "mov {tmp:r}, rbx",
            "cpuid",
            "xchg {tmp:r}, rbx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack, preserves_flags),
        );
        // 32-bit: the same dance in 32-bit registers. `ebx` is also the PIC
        // base pointer here, which is exactly why LLVM reserves it and why the
        // value has to be shuttled rather than named as an operand.
        #[cfg(target_arch = "x86")]
        asm!(
            "mov {tmp:e}, ebx",
            "cpuid",
            "xchg {tmp:e}, ebx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack, preserves_flags),
        );
    }
    [eax, ebx, ecx, edx]
}

/// Highest standard CPUID leaf the CPU answers.
#[inline]
pub fn cpuid_max_leaf() -> u32 {
    cpuid(0, 0)[0]
}

/// Reads `XCR0`.
///
/// # Safety
/// The caller must have confirmed `CPUID.1:ECX.OSXSAVE[bit 27]`; `XGETBV`
/// faults with `#UD` otherwise.
#[inline]
pub unsafe fn xgetbv0() -> u64 {
    let (eax, edx): (u32, u32);
    unsafe {
        asm!(
            "xgetbv",
            in("ecx") 0u32,
            out("eax") eax,
            out("edx") edx,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((edx as u64) << 32) | eax as u64
}

/// `XCR0` when the OS advertises XSAVE, zero otherwise. Never faults.
#[inline]
pub fn xcr0_safe() -> u64 {
    if (cpuid(1, 0)[2] >> 27) & 1 == 0 {
        return 0;
    }
    unsafe { xgetbv0() }
}

// ---------------------------------------------------------------------------
// Counter reads
// ---------------------------------------------------------------------------

/// Bare `RDTSC`. No serialisation: the cheapest and least ordered read.
#[inline(always)]
pub fn rdtsc_raw() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        asm!("rdtsc", out("eax") lo, out("edx") hi,
             options(nomem, nostack, preserves_flags));
    }
    ((hi as u64) << 32) | lo as u64
}

/// `LFENCE` + `RDTSC`. Drains prior loads; the standard interval start.
#[inline(always)]
pub fn rdtsc_lfence() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        asm!(
            "lfence",
            "rdtsc",
            out("eax") lo, out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | lo as u64
}

/// `MFENCE` + `LFENCE` + `RDTSC`. Adds store ordering to the interval start.
#[inline(always)]
pub fn rdtsc_mfence() -> u64 {
    let (lo, hi): (u32, u32);
    unsafe {
        asm!(
            "mfence",
            "lfence",
            "rdtsc",
            out("eax") lo, out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | lo as u64
}

/// `RDTSCP` + `LFENCE`, returning the counter and `IA32_TSC_AUX`.
///
/// `RDTSCP` waits for older instructions to retire, so it is the interval
/// *end* counterpart to [`rdtsc_lfence`]. `TSC_AUX` carries the core/socket id
/// the read came from, which is how migration is detected.
#[inline(always)]
pub fn rdtscp_lfence() -> (u64, u32) {
    let (lo, hi, aux): (u32, u32, u32);
    unsafe {
        asm!(
            "rdtscp",
            "lfence",
            out("eax") lo, out("edx") hi, out("ecx") aux,
            options(nomem, nostack, preserves_flags),
        );
    }
    (((hi as u64) << 32) | lo as u64, aux)
}

/// `CPUID`-serialised `RDTSC`: the legacy backend's interval start.
#[inline(always)]
pub fn tsc_start_serialising() -> u64 {
    let _ = cpuid(0, 0);
    rdtsc_raw()
}

/// `RDTSCP` followed by a `CPUID` drain: the legacy backend's interval end.
#[inline(always)]
pub fn tsc_end_serialising() -> u64 {
    let (t, _) = rdtscp_lfence();
    let _ = cpuid(0, 0);
    t
}

/// `IA32_TSC_AUX` alone — the core/socket id of the current thread.
#[inline]
pub fn tsc_aux() -> u32 {
    rdtscp_lfence().1
}

// ---------------------------------------------------------------------------
// Fences, hints and cache control
// ---------------------------------------------------------------------------

#[inline(always)]
pub fn lfence() {
    unsafe { asm!("lfence", options(nostack, preserves_flags)) }
}

#[inline(always)]
pub fn sfence() {
    unsafe { asm!("sfence", options(nostack, preserves_flags)) }
}

#[inline(always)]
pub fn mfence() {
    unsafe { asm!("mfence", options(nostack, preserves_flags)) }
}

#[inline(always)]
pub fn pause() {
    unsafe { asm!("pause", options(nomem, nostack, preserves_flags)) }
}

/// Drops the line containing `ptr` from every cache level.
///
/// # Safety
/// `ptr` must be readable; `CLFLUSH` faults on unmapped addresses.
#[inline(always)]
pub unsafe fn clflush(ptr: *const u8) {
    unsafe { asm!("clflush [{p}]", p = in(reg) ptr, options(nostack, preserves_flags)) }
}

/// # Safety
/// `ptr` must be readable.
#[inline(always)]
pub unsafe fn prefetch_t0(ptr: *const u8) {
    unsafe { asm!("prefetcht0 [{p}]", p = in(reg) ptr, options(nostack, preserves_flags)) }
}

/// # Safety
/// `ptr` must be readable.
#[inline(always)]
pub unsafe fn prefetch_nta(ptr: *const u8) {
    unsafe { asm!("prefetchnta [{p}]", p = in(reg) ptr, options(nostack, preserves_flags)) }
}

/// Clears the upper 128 bits of every YMM register.
///
/// # Safety
/// Requires AVX.
#[inline]
#[target_feature(enable = "avx")]
pub unsafe fn vzeroupper() {
    unsafe { asm!("vzeroupper", options(nostack, preserves_flags)) }
}

// ---------------------------------------------------------------------------
// Scalar side-channel probes
// ---------------------------------------------------------------------------

/// Cycles for one dependent 64-bit load, fenced on both sides.
///
/// # Safety
/// `ptr` must be a valid, aligned `*const u64`.
#[inline]
pub unsafe fn probe_load_cycles(ptr: *const u64) -> u64 {
    let start = rdtsc_lfence();
    // A machine word, so the probe is one instruction on both widths: `reg`
    // resolves to a 64-bit register on x86-64 and a 32-bit one on i686.
    let sink: usize;
    unsafe {
        asm!("mov {v}, [{p}]", p = in(reg) ptr, v = out(reg) sink,
             options(nostack, preserves_flags, readonly));
    }
    let (end, _) = rdtscp_lfence();
    core::hint::black_box(sink);
    end.wrapping_sub(start)
}

/// Cycles for one 64-bit store, drained with `MFENCE` before the end read.
///
/// # Safety
/// `ptr` must be a valid, aligned, writable `*mut u64`.
#[inline]
pub unsafe fn probe_store_cycles(ptr: *mut u64, value: u64) -> u64 {
    let start = rdtsc_lfence();
    unsafe {
        asm!("mov [{p}], {v}", "mfence",
             p = in(reg) ptr, v = in(reg) value as usize,
             options(nostack, preserves_flags));
    }
    let (end, _) = rdtscp_lfence();
    end.wrapping_sub(start)
}

/// Flush + reload latency for one line: the classic cache-residency signal.
///
/// This measures a buffer the caller owns. It is a self-audit primitive for
/// checking whether your own code leaks residency, not a cross-process probe.
///
/// # Safety
/// `ptr` must be a valid, aligned `*const u64`.
#[inline]
pub unsafe fn probe_flush_reload_cycles(ptr: *const u64) -> u64 {
    unsafe {
        clflush(ptr as *const u8);
        mfence();
        probe_load_cycles(ptr)
    }
}

/// Prefetch + reload latency for one line — the warm counterpart to
/// [`probe_flush_reload_cycles`].
///
/// # Safety
/// `ptr` must be a valid, aligned `*const u64`.
#[inline]
pub unsafe fn probe_prefetch_reload_cycles(ptr: *const u64) -> u64 {
    unsafe {
        prefetch_t0(ptr as *const u8);
        lfence();
        probe_load_cycles(ptr)
    }
}

/// Cycles to walk `pattern` as a data-dependent branch sequence.
///
/// # Safety
/// `pattern` must point to `count` readable bytes.
#[inline]
pub unsafe fn probe_branch_cycles(pattern: *const u8, count: usize) -> u64 {
    let start = rdtsc_lfence();
    let mut acc: u64 = 0;
    for i in 0..count {
        // A real branch, not a cmov: the misprediction is the thing being timed.
        if unsafe { *pattern.add(i) } & 1 != 0 {
            acc = acc.wrapping_add(1);
        } else {
            acc = acc.wrapping_mul(3);
        }
    }
    let (end, _) = rdtscp_lfence();
    core::hint::black_box(acc);
    end.wrapping_sub(start)
}

/// Cycles to follow `steps` links of a pointer-chase list.
///
/// # Safety
/// `first` must head a chain of at least `steps` valid `*const *const u8`
/// links, each pointing at the next.
#[inline]
pub unsafe fn probe_pointer_chase_cycles(first: *const *const u8, steps: usize) -> u64 {
    let start = rdtsc_lfence();
    let mut p = first;
    for _ in 0..steps {
        if p.is_null() {
            break;
        }
        p = unsafe { *p } as *const *const u8;
    }
    let (end, _) = rdtscp_lfence();
    core::hint::black_box(p);
    end.wrapping_sub(start)
}

/// Cycles for `iterations` back-to-back `LFENCE`s: the fence's own cost.
#[inline]
pub fn probe_barrier_cycles(iterations: u32) -> u64 {
    let n = iterations.max(1);
    let start = rdtsc_lfence();
    for _ in 0..n {
        lfence();
    }
    let (end, _) = rdtscp_lfence();
    end.wrapping_sub(start)
}

/// Back-to-back counter reads: the floor below which no measurement is real.
#[inline]
pub fn read_overhead_cycles() -> u64 {
    let a = rdtsc_lfence();
    let (b, _) = rdtscp_lfence();
    b.saturating_sub(a)
}

// ---------------------------------------------------------------------------
// Per-family SIMD probes
// ---------------------------------------------------------------------------

// The probes and kernels below name 64-bit registers (`rdi`, `rsi`, `rdx`,
// `rax`) and move 64-bit values into vector registers, so the two macros that
// generate them gate every item on `x86_64` *and* on the `simd` feature. On
// i686 the counter layer above still works and the SIMD families report as
// unavailable; the same is true of a freestanding build, where the target ABI
// is soft-float and no XMM register can be allocated.

/// Emits `counter`, `vector_load`, `vector_xor` and `barrier` probes for one
/// ISA family, each timed with `LFENCE`+`RDTSC` .. `RDTSCP`+`LFENCE`.
///
/// The `$body` fragments are the exact instruction sequences the old
/// per-family `.asm` files used, so a family's number still measures that
/// family's datapath rather than a generic 128-bit fallback.
macro_rules! simd_family {
    (
        $module:ident,
        feature = $feat:literal,
        load = { $($load:literal),+ $(,)? },
        xor = { $($xor:literal),+ $(,)? }
        $(, clobber = ($($cl:tt)*))?
        $(,)?
    ) => {
        #[cfg(all(target_arch = "x86_64", feature = "simd"))]
        pub mod $module {
            use super::*;

            /// Counter read tagged to this family's probe path.
            #[inline]
            pub fn counter() -> u64 {
                rdtsc_lfence()
            }

            /// Cycles for one vector load out of `ptr`.
            ///
            /// # Safety
            /// Requires the family's ISA extension (gated by the dispatcher)
            /// and at least one vector width of readable bytes at `ptr`.
            #[inline]
            #[target_feature(enable = $feat)]
            pub unsafe fn vector_load_cycles(ptr: *const u8) -> u64 {
                let start = rdtsc_lfence();
                unsafe {
                    asm!(
                        $($load,)+
                        in("rdi") ptr,
                        $($($cl)*,)?
                        options(nostack, preserves_flags, readonly),
                    );
                }
                let (end, _) = rdtscp_lfence();
                end.wrapping_sub(start)
            }

            /// Cycles for one vector ALU op over `a` and `b` into `out`.
            ///
            /// # Safety
            /// Requires the family's ISA extension and at least one vector
            /// width of readable bytes at `a`/`b` and writable bytes at `out`.
            #[inline]
            #[target_feature(enable = $feat)]
            pub unsafe fn vector_xor_cycles(a: *const u8, b: *const u8, out: *mut u8) -> u64 {
                let start = rdtsc_lfence();
                unsafe {
                    asm!(
                        $($xor,)+
                        in("rdi") a,
                        in("rsi") b,
                        in("rdx") out,
                        $($($cl)*,)?
                        options(nostack, preserves_flags),
                    );
                }
                let (end, _) = rdtscp_lfence();
                end.wrapping_sub(start)
            }

            /// Fence cost measured on this family's path.
            #[inline]
            pub fn barrier_cycles(iterations: u32) -> u64 {
                probe_barrier_cycles(iterations)
            }
        }
    };
}

// MMX has no `target_feature` name of its own: rustc dropped it because every
// x86-64 CPU implements it unconditionally. Gating on `sse` — also baseline on
// x86-64 — keeps the macro uniform without weakening anything, and the CPUID
// bit is still checked by `SimdFamily::is_available`.
simd_family! {
    mmx,
    feature = "sse",
    load = { "movq mm0, [rdi]", "emms" },
    xor = { "movq mm0, [rdi]", "pxor mm0, [rsi]", "movq [rdx], mm0", "emms" },
    clobber = (out("mm0") _)
}

simd_family! {
    sse,
    feature = "sse",
    load = { "movups xmm0, [rdi]" },
    xor = { "movups xmm0, [rdi]", "xorps xmm0, [rsi]", "movups [rdx], xmm0" },
    clobber = (out("xmm0") _)
}

simd_family! {
    sse2,
    feature = "sse2",
    load = { "movdqu xmm0, [rdi]" },
    xor = { "movdqu xmm0, [rdi]", "pxor xmm0, [rsi]", "movdqu [rdx], xmm0" },
    clobber = (out("xmm0") _)
}

simd_family! {
    sse3,
    feature = "sse3",
    load = { "lddqu xmm0, [rdi]" },
    xor = { "lddqu xmm0, [rdi]", "lddqu xmm1, [rsi]", "xorps xmm0, xmm1", "movdqu [rdx], xmm0" },
    clobber = (out("xmm0") _, out("xmm1") _)
}

simd_family! {
    ssse3,
    feature = "ssse3",
    load = { "movdqu xmm0, [rdi]" },
    xor = { "movdqu xmm0, [rdi]", "movdqu xmm1, [rsi]", "pshufb xmm0, xmm1", "movdqu [rdx], xmm0" },
    clobber = (out("xmm0") _, out("xmm1") _)
}

simd_family! {
    sse41,
    feature = "sse4.1",
    load = { "movdqu xmm0, [rdi]" },
    xor = { "movdqu xmm0, [rdi]", "movdqu xmm1, [rsi]", "pblendw xmm0, xmm1, 0xAA", "movdqu [rdx], xmm0" },
    clobber = (out("xmm0") _, out("xmm1") _)
}

simd_family! {
    sse42,
    feature = "sse4.2",
    load = { "movdqu xmm0, [rdi]" },
    xor = { "movdqu xmm0, [rdi]", "movdqu xmm1, [rsi]", "pcmpgtq xmm0, xmm1", "movdqu [rdx], xmm0" },
    clobber = (out("xmm0") _, out("xmm1") _)
}

simd_family! {
    f16c,
    feature = "f16c",
    load = { "vcvtph2ps xmm0, qword ptr [rdi]", "vzeroupper" },
    xor = { "vcvtph2ps xmm0, qword ptr [rdi]", "vmovups [rdx], xmm0", "vzeroupper" },
    clobber = (out("xmm0") _)
}

simd_family! {
    fma,
    feature = "fma",
    load = { "vmovups ymm0, [rdi]", "vzeroupper" },
    xor = {
        "vmovups ymm0, [rdi]",
        "vmovups ymm1, [rsi]",
        "vfmadd132ps ymm0, ymm1, ymm1",
        "vmovups [rdx], ymm0",
        "vzeroupper",
    },
    clobber = (out("ymm0") _, out("ymm1") _)
}

simd_family! {
    avx,
    feature = "avx",
    load = { "vmovups ymm0, [rdi]", "vzeroupper" },
    xor = { "vmovups ymm0, [rdi]", "vxorps ymm0, ymm0, [rsi]", "vmovups [rdx], ymm0", "vzeroupper" },
    clobber = (out("ymm0") _)
}

simd_family! {
    avx2,
    feature = "avx2",
    load = { "vmovdqu ymm0, [rdi]", "vzeroupper" },
    xor = { "vmovdqu ymm0, [rdi]", "vpxor ymm0, ymm0, [rsi]", "vmovdqu [rdx], ymm0", "vzeroupper" },
    clobber = (out("ymm0") _)
}

// `vpdpbusd` exists in two encodings: VEX (AVX-VNNI) and EVEX (AVX512-VNNI +
// VL). Inline asm is assembled with the module's base target features, not the
// function's `#[target_feature]`, so without a hint the assembler picks EVEX
// and the probe faults with SIGILL on every AVX-VNNI CPU that lacks AVX-512 —
// which is most of them. The `{vex}` prefix pins the encoding.
simd_family! {
    avx_vnni,
    feature = "avxvnni",
    load = { "vmovdqu ymm0, [rdi]", "vzeroupper" },
    xor = {
        "vpxor ymm0, ymm0, ymm0",
        "vpcmpeqb ymm1, ymm1, ymm1",
        "{{vex}} vpdpbusd ymm0, ymm1, [rsi]",
        "vmovdqu [rdx], ymm0",
        "vzeroupper",
    },
    clobber = (out("ymm0") _, out("ymm1") _)
}

simd_family! {
    avx512,
    feature = "avx512f",
    load = { "vmovdqu64 zmm0, [rdi]", "vzeroupper" },
    xor = {
        "vmovdqu64 zmm0, [rdi]",
        "vpxord zmm0, zmm0, [rsi]",
        "vmovdqu64 [rdx], zmm0",
        "vzeroupper",
    },
    clobber = (out("zmm0") _)
}

simd_family! {
    avx512_vnni,
    feature = "avx512vnni",
    load = { "vmovdqu64 zmm0, [rdi]", "vzeroupper" },
    xor = {
        "vpxord zmm0, zmm0, zmm0",
        "mov eax, -1",
        "vpbroadcastd zmm1, eax",
        "vpdpbusd zmm0, zmm1, [rsi]",
        "vmovdqu64 [rdx], zmm0",
        "vzeroupper",
    },
    clobber = (out("zmm0") _, out("zmm1") _, out("eax") _)
}

// ---------------------------------------------------------------------------
// ISA microbenchmark kernels
// ---------------------------------------------------------------------------

/// Scalar ALU loop. The baseline every SIMD family is compared against.
///
/// The accumulator is a `usize` rather than a `u64` so the same block compiles
/// on i686, where the `reg` class is 32 bits wide and a 64-bit operand has no
/// single register to live in.
#[inline(never)]
pub fn kernel_scalar(loops: usize) -> u64 {
    let mut x: usize = 0x9E37_79B9;
    for _ in 0..loops {
        // `asm!` keeps the chain intact: an autovectoriser would otherwise be
        // free to turn a dependent scalar loop into something wider.
        unsafe {
            asm!(
                "rol {x}, 7",
                "xor {x}, {c}",
                "add {x}, {c}",
                x = inout(reg) x,
                c = in(reg) 0xD192_ED03usize,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
    x as u64
}

/// Emits one `kernel_*` per SIMD family from its per-iteration instructions.
macro_rules! simd_kernel {
    ($name:ident, $feat:literal, [$($insn:literal),+ $(,)?], ($($cl:tt)*)) => {
        /// # Safety
        /// Requires the family's ISA extension; callers must gate on CPUID.
        #[cfg(all(target_arch = "x86_64", feature = "simd"))]
        #[inline(never)]
        #[target_feature(enable = $feat)]
        pub unsafe fn $name(loops: usize) -> u64 {
            let mut acc: u64 = 0x9E37_79B9_7F4A_7C15;
            for _ in 0..loops {
                unsafe {
                    asm!(
                        $($insn,)+
                        inout("rax") acc,
                        $($cl)*
                        options(nomem, nostack, preserves_flags),
                    );
                }
            }
            acc
        }
    };
}

simd_kernel!(kernel_mmx, "sse", [
    "movq mm0, rax",
    "pxor mm0, mm0",
    "paddd mm0, mm0",
    "movq rax, mm0",
    "add rax, 1",
    "emms",
], (out("mm0") _,));

simd_kernel!(kernel_sse, "sse", [
    "movq xmm0, rax",
    "cvtdq2ps xmm1, xmm0",
    "mulps xmm1, xmm1",
    "addps xmm1, xmm1",
    "cvtps2dq xmm0, xmm1",
    "movq rax, xmm0",
    "add rax, 1",
], (out("xmm0") _, out("xmm1") _,));

simd_kernel!(kernel_sse2, "sse2", [
    "movq xmm0, rax",
    "pshufd xmm0, xmm0, 0x1B",
    "paddq xmm0, xmm0",
    "pxor xmm0, xmm0",
    "movq rax, xmm0",
    "add rax, 1",
], (out("xmm0") _,));

simd_kernel!(kernel_sse3, "sse3", [
    "movq xmm0, rax",
    "cvtdq2ps xmm0, xmm0",
    "haddps xmm0, xmm0",
    "haddps xmm0, xmm0",
    "cvtps2dq xmm0, xmm0",
    "movq rax, xmm0",
    "add rax, 1",
], (out("xmm0") _,));

simd_kernel!(kernel_ssse3, "ssse3", [
    "movq xmm0, rax",
    "pshufb xmm0, xmm0",
    "pabsd xmm0, xmm0",
    "movq rax, xmm0",
    "add rax, 1",
], (out("xmm0") _,));

simd_kernel!(kernel_sse41, "sse4.1", [
    "movq xmm0, rax",
    "pmulld xmm0, xmm0",
    "pminsd xmm0, xmm0",
    "movq rax, xmm0",
    "add rax, 1",
], (out("xmm0") _,));

simd_kernel!(
    kernel_sse42,
    "sse4.2",
    ["crc32 rax, rax", "add rax, 1",],
    ()
);

simd_kernel!(kernel_avx, "avx", [
    "vmovq xmm0, rax",
    "vcvtdq2ps ymm0, ymm0",
    "vmulps ymm0, ymm0, ymm0",
    "vaddps ymm0, ymm0, ymm0",
    "vcvttps2dq ymm0, ymm0",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("ymm0") _,));

simd_kernel!(kernel_f16c, "f16c", [
    "vmovq xmm0, rax",
    "vcvtps2ph xmm1, xmm0, 0",
    "vcvtph2ps xmm0, xmm1",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("xmm0") _, out("xmm1") _,));

simd_kernel!(kernel_fma, "fma", [
    "vmovq xmm0, rax",
    "vcvtdq2ps ymm0, ymm0",
    "vfmadd213ps ymm0, ymm0, ymm0",
    "vcvttps2dq ymm0, ymm0",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("ymm0") _,));

simd_kernel!(kernel_avx2, "avx2", [
    "vmovq xmm0, rax",
    "vpbroadcastq ymm0, xmm0",
    "vpaddq ymm0, ymm0, ymm0",
    "vpxor ymm0, ymm0, ymm0",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("ymm0") _,));

// `{vex}` pins the AVX-VNNI encoding; see the note on the `avx_vnni` probes.
simd_kernel!(kernel_avx_vnni, "avxvnni", [
    "vmovq xmm0, rax",
    "vpbroadcastq ymm0, xmm0",
    "vpcmpeqb ymm1, ymm1, ymm1",
    "{{vex}} vpdpbusd ymm0, ymm1, ymm1",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("ymm0") _, out("ymm1") _,));

simd_kernel!(kernel_avx512, "avx512f", [
    "vmovq xmm0, rax",
    "vpbroadcastq zmm0, xmm0",
    "vpaddq zmm0, zmm0, zmm0",
    "vpxord zmm0, zmm0, zmm0",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("zmm0") _,));

simd_kernel!(kernel_avx512_vnni, "avx512vnni", [
    "vmovq xmm0, rax",
    "vpbroadcastq zmm0, xmm0",
    "vpternlogd zmm1, zmm1, zmm1, 0xFF",
    "vpdpbusd zmm0, zmm1, zmm1",
    "vmovq rax, xmm0",
    "add rax, 1",
    "vzeroupper",
], (out("zmm0") _, out("zmm1") _,));

/// AES round-trip loop over four independent state registers.
///
/// This is an *instruction-latency* kernel, not a cipher: it measures what
/// `AESENC` costs on this core. Real encryption goes through
/// `nanochrono-crypto`, which is backed by the rustls provider.
///
/// # Safety
/// Requires AES-NI.
#[cfg(all(target_arch = "x86_64", feature = "simd"))]
#[inline(never)]
#[target_feature(enable = "aes")]
pub unsafe fn kernel_aesni(loops: usize) -> u64 {
    let mut out: u64 = 0;
    unsafe {
        asm!(
            "movq xmm4, {seed}",
            // Four *independent* states, so the four AES chains do not run in
            // lockstep. Seeding them identically made every register hold the
            // same value forever, which measured one chain's latency four times
            // and reduced the checksum to a constant zero.
            "movdqa xmm0, xmm4",
            "movdqa xmm1, xmm4",
            "pslld xmm1, 1",
            "movdqa xmm2, xmm4",
            "pslld xmm2, 2",
            "movdqa xmm3, xmm4",
            "pslld xmm3, 3",
            "test {n}, {n}",
            "jz 3f",
            "2:",
            "aesenc xmm0, xmm4",
            "aesenc xmm1, xmm4",
            "aesenc xmm2, xmm4",
            "aesenc xmm3, xmm4",
            "aesenclast xmm0, xmm4",
            "aesenclast xmm1, xmm4",
            "aesenclast xmm2, xmm4",
            "aesenclast xmm3, xmm4",
            "dec {n}",
            "jnz 2b",
            "3:",
            "pxor xmm0, xmm1",
            "pxor xmm2, xmm3",
            "pxor xmm0, xmm2",
            "movq {out}, xmm0",
            seed = in(reg) 0x0F1E_2D3C_4B5A_6978u64,
            n = inout(reg) loops => _,
            out = out(reg) out,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _,
            options(nomem, nostack),
        );
    }
    out
}

/// Carry-less multiply loop: the GHASH/CRC datapath's instruction cost.
///
/// # Safety
/// Requires PCLMULQDQ.
#[cfg(all(target_arch = "x86_64", feature = "simd"))]
#[inline(never)]
#[target_feature(enable = "pclmulqdq")]
pub unsafe fn kernel_pclmul(loops: usize) -> u64 {
    let mut out: u64 = 0;
    unsafe {
        asm!(
            "movq xmm0, {a}",
            "movq xmm1, {b}",
            "movq xmm4, {a}",
            "pxor xmm2, xmm2",
            "test {n}, {n}",
            "jz 3f",
            "2:",
            "movdqa xmm3, xmm0",
            "pclmulqdq xmm3, xmm1, 0x00",
            // `paddq`, not `pxor`. Carry-less multiply, XOR and shuffle are all
            // GF(2)-linear, so a chain built only from them telescopes: the
            // accumulator XORed to exactly zero at every power-of-two iteration
            // count, and the kernel reported nothing while still burning the
            // cycles. Integer addition carries between bits, which breaks the
            // linearity and keeps the accumulator meaningful.
            "paddq xmm2, xmm3",
            // Swapping the halves feeds the product's high bits back into the
            // multiplicand; without it the state accumulates trailing zeros
            // until it is absorbed at zero.
            "pshufd xmm3, xmm3, 0x4E",
            "pxor xmm0, xmm3",
            "paddq xmm0, xmm4",
            "dec {n}",
            "jnz 2b",
            "3:",
            "movq {out}, xmm2",
            a = in(reg) 0x0123_4567_89AB_CDEFu64,
            b = in(reg) 0xFEDC_BA98_7654_3210u64,
            n = inout(reg) loops => _,
            out = out(reg) out,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _,
            options(nomem, nostack),
        );
    }
    out
}

/// SHA-256 round instruction loop.
///
/// # Safety
/// Requires SHA-NI (and SSE4.1, which `sha` implies on every CPU that has it).
#[cfg(all(target_arch = "x86_64", feature = "simd"))]
#[inline(never)]
#[target_feature(enable = "sha,sse4.1")]
pub unsafe fn kernel_shani(loops: usize) -> u64 {
    let mut out: u64 = 0;
    unsafe {
        asm!(
            "movq xmm0, {seed}",
            "movdqa xmm1, xmm0",
            "movdqa xmm2, xmm0",
            "test {n}, {n}",
            "jz 3f",
            "2:",
            "sha256msg1 xmm0, xmm1",
            "sha256msg2 xmm0, xmm1",
            "sha256rnds2 xmm1, xmm2",
            "dec {n}",
            "jnz 2b",
            "3:",
            "pxor xmm0, xmm1",
            "movq {out}, xmm0",
            seed = in(reg) 0x6A09_E667_BB67_AE85u64,
            n = inout(reg) loops => _,
            out = out(reg) out,
            // XMM0 is `sha256rnds2`'s implicit operand, but this block writes
            // it from {seed} before reading it, so it is a clobber. Declaring
            // it as an input would promise the compiler a value that survives.
            out("xmm0") _, out("xmm1") _, out("xmm2") _,
            options(nomem, nostack),
        );
    }
    out
}
