// SPDX-License-Identifier: Apache-2.0
//! CPU feature detection.
//!
//! Every ISA extension this crate can execute must pass through here first.
//! On x86-64 that means both a CPUID bit *and* an XCR0 check: a CPU can report
//! AVX-512 while the OS has not enabled ZMM state, and executing a ZMM
//! instruction in that situation is `#UD`, not a slow path.

#[cfg(feature = "std")]
use std::sync::OnceLock;

/// One flag per ISA extension the toolkit can dispatch to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CpuFeatures {
    pub mmx: bool,
    pub sse: bool,
    pub sse2: bool,
    pub sse3: bool,
    pub ssse3: bool,
    pub sse41: bool,
    pub sse42: bool,
    pub aesni: bool,
    pub pclmulqdq: bool,
    pub shani: bool,
    pub avx: bool,
    pub f16c: bool,
    pub fma: bool,
    pub avx2: bool,
    pub avx_vnni: bool,
    pub vaes: bool,
    pub avx512f: bool,
    pub avx512bw: bool,
    pub avx512vl: bool,
    pub avx512vnni: bool,
    pub neon: bool,
    pub sve: bool,
    pub sve2: bool,
    pub sme: bool,
    pub arm_aes: bool,
    pub arm_sha2: bool,
    /// True when the TSC is invariant, i.e. immune to frequency and C-state
    /// changes. Without this, cycle deltas across a long interval are not
    /// comparable to wall time.
    pub invariant_counter: bool,
}

/// Detected features for this machine, computed once.
///
/// Returned by value rather than by reference: the struct is a few dozen
/// bools and `Copy`, and a freestanding build has no `OnceLock` to hand out a
/// `&'static` from — `OnceLock` needs a blocking primitive the OS provides.
#[cfg(feature = "std")]
pub fn features() -> CpuFeatures {
    static CACHE: OnceLock<CpuFeatures> = OnceLock::new();
    *CACHE.get_or_init(detect)
}

/// Detects the feature set, without caching.
///
/// Detection is pure CPUID (or pure `MRS`) and costs a few dozen cycles, so
/// re-running it is cheaper than the synchronisation a cache would need — and
/// a bare-metal caller that wants it hot can hold the result itself, which
/// `nanochrono-baremetal` does.
#[cfg(not(feature = "std"))]
pub fn features() -> CpuFeatures {
    detect()
}

fn detect() -> CpuFeatures {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        detect_x86()
    }
    #[cfg(target_arch = "aarch64")]
    {
        detect_aarch64()
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        CpuFeatures::default()
    }
}

#[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
fn detect_x86() -> CpuFeatures {
    use crate::arch::x86::{cpuid, cpuid_max_leaf, xcr0_safe};

    let mut f = CpuFeatures::default();
    let max_leaf = cpuid_max_leaf();
    if max_leaf < 1 {
        return f;
    }

    let r1 = cpuid(1, 0);
    let (ecx1, edx1) = (r1[2], r1[3]);

    // XCR0 gates every register file wider than XMM. Bits 1|2 are SSE|AVX
    // state; bits 5|6|7 add opmask, ZMM_hi256 and Hi16_ZMM for AVX-512.
    let xcr0 = xcr0_safe();
    let os_ymm = xcr0 & 0x6 == 0x6;
    let os_zmm = xcr0 & 0xE6 == 0xE6;

    f.mmx = edx1 & (1 << 23) != 0;
    f.sse = edx1 & (1 << 25) != 0;
    f.sse2 = edx1 & (1 << 26) != 0;
    f.sse3 = ecx1 & 1 != 0;
    f.pclmulqdq = ecx1 & (1 << 1) != 0;
    f.ssse3 = ecx1 & (1 << 9) != 0;
    f.fma = ecx1 & (1 << 12) != 0 && os_ymm;
    f.sse41 = ecx1 & (1 << 19) != 0;
    f.sse42 = ecx1 & (1 << 20) != 0;
    f.aesni = ecx1 & (1 << 25) != 0;
    f.avx = ecx1 & (1 << 28) != 0 && os_ymm;
    f.f16c = ecx1 & (1 << 29) != 0 && os_ymm;

    if max_leaf >= 7 {
        let r7 = cpuid(7, 0);
        let (max_sub7, ebx7, ecx7) = (r7[0], r7[1], r7[2]);
        f.avx2 = ebx7 & (1 << 5) != 0 && os_ymm;
        f.shani = ebx7 & (1 << 29) != 0;
        f.avx512f = ebx7 & (1 << 16) != 0 && os_zmm;
        f.avx512bw = ebx7 & (1 << 30) != 0 && os_zmm;
        f.avx512vl = ebx7 & (1 << 31) != 0 && os_zmm;
        f.vaes = ecx7 & (1 << 9) != 0 && os_ymm;
        f.avx512vnni = ecx7 & (1 << 11) != 0 && os_zmm;

        if max_sub7 >= 1 {
            let r71 = cpuid(7, 1);
            f.avx_vnni = r71[0] & (1 << 4) != 0 && os_ymm && f.avx2;
        }
    }

    // CPUID.80000007H:EDX[8] — invariant TSC.
    let ext_max = cpuid(0x8000_0000, 0)[0];
    if ext_max >= 0x8000_0007 {
        f.invariant_counter = cpuid(0x8000_0007, 0)[3] & (1 << 8) != 0;
    }

    #[cfg(target_os = "windows")]
    cross_check_with_windows(&mut f);

    f
}

/// Cross-checks CPUID against `IsProcessorFeaturePresent`.
///
/// Windows is the one platform that publishes its own view of the CPU's
/// features, and it is the *authoritative* one: the OS must have enabled the
/// register state before an instruction is legal, and `IsProcessorFeaturePresent`
/// answers "may this process use it" where CPUID only answers "does the
/// silicon have it". Under a hypervisor that hides a feature from the guest,
/// or on a Windows build that has not enabled AVX state, CPUID can say yes
/// where Windows says no.
///
/// So the two are ANDed: a feature is reported only when both agree. That can
/// only ever remove a feature, never add one, which is the safe direction for
/// something that gates instruction dispatch.
#[cfg(all(
    target_os = "windows",
    any(target_arch = "x86_64", target_arch = "x86")
))]
fn cross_check_with_windows(f: &mut CpuFeatures) {
    use windows_sys::Win32::System::Threading::{
        IsProcessorFeaturePresent, PF_AVX2_INSTRUCTIONS_AVAILABLE,
        PF_AVX512F_INSTRUCTIONS_AVAILABLE, PF_AVX_INSTRUCTIONS_AVAILABLE,
        PF_SSE3_INSTRUCTIONS_AVAILABLE, PF_XMMI64_INSTRUCTIONS_AVAILABLE,
        PF_XMMI_INSTRUCTIONS_AVAILABLE,
    };

    // SAFETY: the function takes a feature constant and has no preconditions.
    let present = |feature| unsafe { IsProcessorFeaturePresent(feature) != 0 };

    f.sse &= present(PF_XMMI_INSTRUCTIONS_AVAILABLE);
    f.sse2 &= present(PF_XMMI64_INSTRUCTIONS_AVAILABLE);
    f.sse3 &= present(PF_SSE3_INSTRUCTIONS_AVAILABLE);
    f.avx &= present(PF_AVX_INSTRUCTIONS_AVAILABLE);
    f.avx2 &= present(PF_AVX2_INSTRUCTIONS_AVAILABLE);

    // Windows gates every AVX-512 subset behind the one AVX-512F flag.
    let avx512 = present(PF_AVX512F_INSTRUCTIONS_AVAILABLE);
    f.avx512f &= avx512;
    f.avx512bw &= avx512;
    f.avx512vl &= avx512;
    f.avx512vnni &= avx512;

    // Features that depend on AVX register state cannot outlive it.
    if !f.avx {
        f.f16c = false;
        f.fma = false;
        f.avx_vnni = false;
        f.vaes = false;
    }
}

/// Windows on AArch64 exposes the NEON and crypto flags the same way.
#[cfg(all(target_os = "windows", target_arch = "aarch64"))]
fn cross_check_with_windows(f: &mut CpuFeatures) {
    use windows_sys::Win32::System::Threading::{
        IsProcessorFeaturePresent, PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE,
        PF_ARM_VFP_32_REGISTERS_AVAILABLE,
    };

    // SAFETY: the function takes a feature constant and has no preconditions.
    let present = |feature| unsafe { IsProcessorFeaturePresent(feature) != 0 };

    f.neon &= present(PF_ARM_VFP_32_REGISTERS_AVAILABLE);
    let crypto = present(PF_ARM_V8_CRYPTO_INSTRUCTIONS_AVAILABLE);
    f.arm_aes &= crypto;
    f.arm_sha2 &= crypto;
}

#[cfg(target_arch = "aarch64")]
/// AArch64 feature detection without an OS, by reading the ID registers.
///
/// `is_aarch64_feature_detected!` lives in `std::arch` because it has to ask
/// the OS: at EL0 the ID registers trap, so a hosted process cannot read them
/// and must go through HWCAP or a sysctl. A freestanding kernel runs at EL1,
/// where they are simply readable — the one place this is the *easier* path
/// rather than the forbidden one.
///
/// Field positions are from the kernel's `arch/arm64/tools/sysreg` table,
/// which is generated from the ARM ARM.
#[cfg(all(target_arch = "aarch64", not(feature = "std")))]
fn detect_aarch64() -> CpuFeatures {
    /// Extracts a 4-bit ID register field.
    fn field(reg: u64, shift: u32) -> u64 {
        (reg >> shift) & 0xF
    }

    let isar0: u64;
    let pfr0: u64;
    let pfr1: u64;
    // SAFETY: at EL1 these are readable with no trap and no side effects. A
    // freestanding build is the only configuration this function compiles in.
    unsafe {
        core::arch::asm!("mrs {v}, ID_AA64ISAR0_EL1", v = out(reg) isar0,
                         options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {v}, ID_AA64PFR0_EL1", v = out(reg) pfr0,
                         options(nomem, nostack, preserves_flags));
        core::arch::asm!("mrs {v}, ID_AA64PFR1_EL1", v = out(reg) pfr1,
                         options(nomem, nostack, preserves_flags));
    }

    // ID_AA64ISAR0_EL1.AES: 0b0001 = AES, 0b0010 = AES + PMULL.
    let aes = field(isar0, 4);
    let sve = field(pfr0, 32) >= 1;

    // ID_AA64ZFR0_EL1 is only architecturally valid when SVE is implemented;
    // reading it otherwise is UNDEFINED, so it is gated on that.
    let sve2 = sve && {
        let zfr0: u64;
        // Named by its raw encoding, `S3_0_C0_C4_4`: the assembler only
        // accepts the mnemonic `ID_AA64ZFR0_EL1` when built with `+sve`, and
        // the base freestanding target is not. The numbers are op0=3, op1=0,
        // CRn=0, CRm=4, op2=4, from the kernel's `arch/arm64/tools/sysreg`
        // table.
        //
        // SAFETY: guarded on ID_AA64PFR0_EL1.SVE above, which is the
        // architectural precondition for this register existing.
        unsafe {
            core::arch::asm!("mrs {v}, S3_0_C0_C4_4", v = out(reg) zfr0,
                             options(nomem, nostack, preserves_flags));
        }
        // SVEver: 0b0000 = SVE, 0b0001 = SVE2.
        field(zfr0, 0) >= 1
    };

    CpuFeatures {
        neon: true,
        invariant_counter: true,
        arm_aes: aes >= 1,
        pclmulqdq: aes >= 2,
        // ID_AA64ISAR0_EL1.SHA2: 0b0001 = SHA256.
        arm_sha2: field(isar0, 12) >= 1,
        sve,
        sve2,
        // ID_AA64PFR1_EL1.SME: 0b0001 = SME, 0b0010 = SME2.
        sme: field(pfr1, 24) >= 1,
        ..Default::default()
    }
}

#[cfg(all(target_arch = "aarch64", feature = "std"))]
fn detect_aarch64() -> CpuFeatures {
    // The only writes after this are platform-gated — the macOS capability
    // buffer and the Windows cross-check — so on a target with neither, `mut`
    // is genuinely unused rather than an oversight.
    #[allow(unused_mut)]
    let mut f = CpuFeatures {
        // NEON is architecturally mandatory on AArch64.
        neon: true,
        // CNTVCT_EL0 runs off a fixed-frequency system counter, so it is
        // invariant by construction — unlike the x86 TSC, which had to earn
        // the label.
        invariant_counter: true,

        // `std::arch::is_aarch64_feature_detected!` reads HWCAP on Linux and
        // the equivalent OS query elsewhere, which is the only supported way
        // to probe these: the ID registers trap at EL0.
        sve: std::arch::is_aarch64_feature_detected!("sve"),
        sve2: std::arch::is_aarch64_feature_detected!("sve2"),
        arm_aes: std::arch::is_aarch64_feature_detected!("aes"),
        arm_sha2: std::arch::is_aarch64_feature_detected!("sha2"),
        pclmulqdq: std::arch::is_aarch64_feature_detected!("pmull"),

        // SME has no stable detection macro yet (rust-lang/rust#127764), so
        // the OS is asked directly. That is what the macro would do anyway,
        // and it keeps the crate building on stable.
        sme: hwcap2_has(HWCAP2_SME),

        ..Default::default()
    };

    // macOS has no auxiliary vector, and `is_aarch64_feature_detected!`
    // resolves several of these through `sysctl` already — but not SME, which
    // Apple Silicon does have from the M4 onwards. Apple publishes the whole
    // extension set as a bit buffer, so one query settles everything the
    // macro could not answer.
    #[cfg(target_os = "macos")]
    if let Some(caps) = crate::platform::darwin::arm_capability_bits() {
        use crate::platform::darwin::has_capability;
        // Bit numbers from the SDK's `arm/cpu_capabilities_public.h`, which
        // states that existing entries are ABI and never renumbered.
        const CAP_BIT_FEAT_SHA256: u32 = 7;
        const CAP_BIT_FEAT_AES: u32 = 10;
        const CAP_BIT_FEAT_PMULL: u32 = 11;
        const CAP_BIT_FEAT_SME: u32 = 40;

        f.sme = has_capability(&caps, CAP_BIT_FEAT_SME);
        // ORed rather than assigned: the detection macro above is
        // authoritative when it answers, and this only fills gaps.
        f.arm_aes |= has_capability(&caps, CAP_BIT_FEAT_AES);
        f.arm_sha2 |= has_capability(&caps, CAP_BIT_FEAT_SHA256);
        f.pclmulqdq |= has_capability(&caps, CAP_BIT_FEAT_PMULL);
    }

    #[cfg(target_os = "windows")]
    cross_check_with_windows(&mut f);

    f
}

/// `HWCAP2_SME`, from `arch/arm64/include/uapi/asm/hwcap.h`.
#[cfg(target_arch = "aarch64")]
#[cfg(all(target_arch = "aarch64", feature = "std"))]
const HWCAP2_SME: u64 = 1 << 23;

/// Tests a bit of the second hardware-capability word the kernel passes in the
/// auxiliary vector.
#[cfg(target_arch = "aarch64")]
#[cfg(all(target_arch = "aarch64", feature = "std"))]
fn hwcap2_has(bit: u64) -> bool {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        // SAFETY: `getauxval` reads the process's own auxiliary vector and
        // returns 0 for an unknown key; it has no preconditions.
        let hwcap2 = unsafe { libc::getauxval(libc::AT_HWCAP2) };
        hwcap2 & bit != 0
    }
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = bit;
        false
    }
}

/// Vendor brand string, when the architecture exposes one.
#[cfg(feature = "std")]
pub fn brand_string() -> Option<String> {
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        use crate::arch::x86::cpuid;
        if cpuid(0x8000_0000, 0)[0] < 0x8000_0004 {
            return None;
        }
        let mut bytes = Vec::with_capacity(48);
        for leaf in 0x8000_0002u32..=0x8000_0004 {
            for reg in cpuid(leaf, 0) {
                bytes.extend_from_slice(&reg.to_le_bytes());
            }
        }
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        Some(String::from_utf8_lossy(&bytes[..end]).trim().to_string())
    }
    // AArch64 has no brand-string instruction. macOS publishes one anyway,
    // which is how an Apple Silicon part gets named rather than reported as
    // an anonymous ARM core.
    #[cfg(all(
        not(any(target_arch = "x86_64", target_arch = "x86")),
        target_os = "macos"
    ))]
    {
        crate::platform::darwin::sysctl_string("machdep.cpu.brand_string")
            .or_else(|| crate::platform::darwin::sysctl_string("hw.model"))
    }
    #[cfg(all(
        not(any(target_arch = "x86_64", target_arch = "x86")),
        not(target_os = "macos")
    ))]
    {
        None
    }
}
