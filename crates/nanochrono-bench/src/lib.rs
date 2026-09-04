// SPDX-License-Identifier: Apache-2.0
//! Benchmark modes, kernels and the three-pass harness.
//!
//! # The three modes
//!
//! The C build had *CPU intrinsics*, *OpenSSL EVP* and *libsodium*. The last
//! two existed because the project linked two independent crypto libraries and
//! wanted to compare them. With a single provider that comparison is gone, and
//! keeping two identical modes would be dishonest. So the third slot now
//! measures something the old build could not: a full TLS handshake.
//!
//! | Mode | Measures | Answers |
//! |---|---|---|
//! | [`BenchMode::CpuIsa`] | Inline-asm ISA kernels | What can this core's datapath do? |
//! | [`BenchMode::Crypto`] | rustls/`ring` primitives over real buffers | What does a byte of AEAD or hash cost? |
//! | [`BenchMode::TlsHandshake`] | End-to-end rustls handshakes | What does establishing a session cost? |
//!
//! # Why three passes
//!
//! Each run does three passes with increasing iteration counts. The first is
//! partly warm-up: caches are cold, the frequency governor has not responded
//! and the branch predictor is untrained. Reporting best/worst/average across
//! all three makes that visible instead of hiding it behind a single number —
//! if pass 1 is much slower than pass 3, the workload is dominated by warm-up,
//! and that is a finding rather than noise to be averaged away.

use std::fmt::Write as _;

use nanochrono_core::{
    arch, dispatch::Dispatcher, format, Backend, Chronometer, CpuFeatures, SimdFamily,
};
use nanochrono_crypto::{tls, AeadKey, Algorithm, NONCE_LEN};

/// Which family of work a benchmark run exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BenchMode {
    /// Inline-asm microbenchmarks per ISA family.
    #[default]
    CpuIsa,
    /// Cryptographic primitives through the rustls provider.
    Crypto,
    /// Full TLS handshakes against a remote host.
    TlsHandshake,
}

impl BenchMode {
    pub const fn name(self) -> &'static str {
        match self {
            BenchMode::CpuIsa => "CPU ISA kernels",
            BenchMode::Crypto => "Crypto (rustls/ring)",
            BenchMode::TlsHandshake => "TLS handshake (rustls)",
        }
    }

    /// Short label for the mode row in the benchmark panel.
    pub const fn label(self) -> &'static str {
        match self {
            BenchMode::CpuIsa => "Mode 1: CPU ISA kernels",
            BenchMode::Crypto => "Mode 2: Crypto (rustls/ring)",
            BenchMode::TlsHandshake => "Mode 3: TLS handshake (rustls)",
        }
    }

    /// Whether the mode can run here.
    ///
    /// The first two always can — the provider is compiled in, unlike the C
    /// build where a mode could be "NOT LINKED". TLS needs a network, which
    /// cannot be established without trying, so it is reported as available
    /// and allowed to fail with a message.
    pub const fn is_available(self) -> bool {
        true
    }

    pub const ALL: &'static [BenchMode] = &[
        BenchMode::CpuIsa,
        BenchMode::Crypto,
        BenchMode::TlsHandshake,
    ];
}

/// One selectable row in the benchmark panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BenchKernel {
    /// Portable scalar baseline.
    Scalar,
    /// One ISA family's kernel.
    Isa(Backend),
    /// One cryptographic primitive.
    Crypto(Algorithm),
    /// A TLS handshake against the configured host.
    Tls,
}

impl BenchKernel {
    pub fn name(self) -> String {
        match self {
            BenchKernel::Scalar => "Scalar baseline".to_string(),
            BenchKernel::Isa(b) => b.name().to_uppercase(),
            BenchKernel::Crypto(a) => a.name().to_string(),
            BenchKernel::Tls => "TLS 1.3 handshake".to_string(),
        }
    }

    /// Whether this machine can run the kernel.
    pub fn is_available(self) -> bool {
        match self {
            BenchKernel::Scalar | BenchKernel::Crypto(_) | BenchKernel::Tls => true,
            BenchKernel::Isa(b) => b.is_available(),
        }
    }

    /// Which mode this kernel belongs to.
    pub const fn mode(self) -> BenchMode {
        match self {
            BenchKernel::Scalar | BenchKernel::Isa(_) => BenchMode::CpuIsa,
            BenchKernel::Crypto(_) => BenchMode::Crypto,
            BenchKernel::Tls => BenchMode::TlsHandshake,
        }
    }

    /// The rows a given mode offers, in display order.
    pub fn rows_for(mode: BenchMode) -> Vec<BenchKernel> {
        match mode {
            BenchMode::CpuIsa => std::iter::once(BenchKernel::Scalar)
                .chain(Backend::ALL.iter().copied().map(BenchKernel::Isa))
                .collect(),
            BenchMode::Crypto => Algorithm::ALL
                .iter()
                .copied()
                .map(BenchKernel::Crypto)
                .collect(),
            BenchMode::TlsHandshake => vec![BenchKernel::Tls],
        }
    }
}

/// Iteration schedule and unit accounting for one kernel.
#[derive(Debug, Clone)]
pub struct BenchProfile {
    pub title: String,
    /// What the kernel actually executes, in one sentence.
    pub description: String,
    /// What "one op" means, so a rate figure can be interpreted.
    pub unit: String,
    pub ops_per_loop: f64,
    pub bytes_per_op: f64,
    /// Loop counts for the three passes.
    pub loops: [usize; 3],
    /// Repeat counts for the three passes.
    pub repeats: [u32; 3],
}

impl BenchProfile {
    /// Schedule for `kernel`, sized so each pass runs long enough to dominate
    /// counter overhead but stays under a second.
    pub fn for_kernel(kernel: BenchKernel, payload_bytes: usize) -> BenchProfile {
        match kernel {
            BenchKernel::Scalar | BenchKernel::Isa(_) => BenchProfile {
                title: kernel.name(),
                description: "inline-asm ISA microbenchmark; dispatch is gated by CPUID + XGETBV \
                              before the kernel is reached"
                    .to_string(),
                unit: "1 op = 1 kernel iteration".to_string(),
                ops_per_loop: 1.0,
                bytes_per_op: 8.0,
                loops: [300_000, 600_000, 900_000],
                repeats: [1, 2, 3],
            },
            BenchKernel::Crypto(algorithm) => BenchProfile {
                title: algorithm.name().to_string(),
                description: format!(
                    "real {} over a {} buffer, through the rustls crypto provider",
                    algorithm.name(),
                    format::format_bytes(payload_bytes as f64)
                ),
                unit: format!("1 op = 1 {} call over the payload", algorithm.name()),
                ops_per_loop: 1.0,
                bytes_per_op: payload_bytes as f64,
                loops: [1_500, 3_000, 4_500],
                repeats: [1, 2, 3],
            },
            BenchKernel::Tls => BenchProfile {
                title: "TLS 1.3 handshake".to_string(),
                // Handshakes are seconds-scale and network-bound, so the
                // schedule is tiny: repeating them measures the remote host's
                // load, not this machine's.
                description: "full rustls handshake: TCP connect, ClientHello, certificate \
                              verification, key exchange"
                    .to_string(),
                unit: "1 op = 1 complete handshake".to_string(),
                ops_per_loop: 1.0,
                bytes_per_op: 0.0,
                loops: [1, 2, 3],
                repeats: [1, 1, 1],
            },
        }
    }
}

/// Payload size for a crypto kernel.
///
/// Large enough that per-call setup does not dominate, small enough to stay in
/// L2 so the number reflects the cipher rather than memory bandwidth.
pub const CRYPTO_PAYLOAD_BYTES: usize = 16 * 1024;

/// Result of one pass.
#[derive(Debug, Clone, Copy, Default)]
pub struct PassResult {
    pub pass: u32,
    pub repeats: u32,
    pub loops: usize,
    pub cycles: u64,
    pub seconds: f64,
    pub total_ops: f64,
    pub total_bytes: f64,
    pub mops: f64,
    pub cycles_per_op: f64,
    pub ns_per_op: f64,
    pub mib_per_second: f64,
    /// Accumulated output, kept so the optimiser cannot delete the work.
    pub sink: u64,
}

/// Aggregate over all passes of a run.
#[derive(Debug, Clone, Default)]
pub struct BenchSummary {
    pub kernel_name: String,
    pub mode: &'static str,
    pub passes: Vec<PassResult>,
    pub best_mops: f64,
    pub worst_mops: f64,
    pub mean_mops: f64,
    pub best_ns_per_op: f64,
    pub worst_ns_per_op: f64,
    pub total_cycles: u64,
    pub total_seconds: f64,
    /// Present only for TLS runs.
    pub tls: Option<tls::HandshakeTiming>,
}

/// A completed benchmark run: the numbers plus the log the UI displays.
#[derive(Debug, Clone)]
pub struct BenchReport {
    pub summary: BenchSummary,
    pub log: String,
    /// Set when the run could not proceed; `summary` is then empty.
    pub error: Option<String>,
}

impl BenchReport {
    fn failed(kernel: BenchKernel, mode: BenchMode, reason: impl Into<String>) -> BenchReport {
        let reason = reason.into();
        BenchReport {
            summary: BenchSummary {
                kernel_name: kernel.name(),
                mode: mode.name(),
                ..Default::default()
            },
            log: format!("status: {reason}\n"),
            error: Some(reason),
        }
    }
}

/// Knobs for a run.
#[derive(Debug, Clone)]
pub struct BenchConfig {
    pub mode: BenchMode,
    pub kernel: BenchKernel,
    /// Host for TLS runs.
    pub tls_host: String,
    pub tls_port: u16,
}

impl Default for BenchConfig {
    fn default() -> Self {
        BenchConfig {
            mode: BenchMode::CpuIsa,
            kernel: BenchKernel::Scalar,
            tls_host: "www.rust-lang.org".to_string(),
            tls_port: 443,
        }
    }
}

/// Runs the three-pass benchmark described by `config`.
///
/// Never panics and never faults: an unavailable kernel or a failed handshake
/// comes back as [`BenchReport::error`].
pub fn run(chrono: &Chronometer, config: &BenchConfig) -> BenchReport {
    let kernel = config.kernel;
    if !kernel.is_available() {
        return BenchReport::failed(kernel, config.mode, "NOT AVAILABLE on this CPU");
    }

    let mut log = String::new();
    write_header(&mut log, chrono, config);

    if kernel == BenchKernel::Tls {
        return run_tls(config, log);
    }

    let profile = BenchProfile::for_kernel(kernel, CRYPTO_PAYLOAD_BYTES);
    let _ = writeln!(log, "warmup: {}", profile.description);
    let _ = writeln!(log, "unit: {}\n", profile.unit);

    let dispatcher = Dispatcher::global();
    let mut workload = match Workload::new(kernel) {
        Ok(w) => w,
        Err(reason) => return BenchReport::failed(kernel, config.mode, reason),
    };

    let mut passes = Vec::with_capacity(3);
    for index in 0..3 {
        let pass = run_pass(
            chrono,
            dispatcher,
            &mut workload,
            &profile,
            index as u32 + 1,
            profile.repeats[index],
            profile.loops[index],
        );
        write_pass(&mut log, &profile, &pass);
        passes.push(pass);
    }

    let summary = summarise(kernel, config.mode, passes, None);
    write_summary(&mut log, &profile, &summary);
    let _ = writeln!(log, "\nstatus: completed successfully.");

    BenchReport {
        summary,
        log,
        error: None,
    }
}

/// Handshakes span milliseconds and several scheduler slices, so they are
/// timed off the wall clock rather than the calibrated counter — hence no
/// `Chronometer` parameter here.
fn run_tls(config: &BenchConfig, mut log: String) -> BenchReport {
    let profile = BenchProfile::for_kernel(BenchKernel::Tls, 0);
    let _ = writeln!(log, "warmup: {}", profile.description);
    let _ = writeln!(log, "unit: {}", profile.unit);
    let _ = writeln!(log, "target: {}:{}\n", config.tls_host, config.tls_port);

    let mut passes = Vec::with_capacity(3);
    let mut last_timing = None;

    for index in 0..3 {
        let start = arch::counter_start();
        let timing = tls::probe_handshake(&config.tls_host, config.tls_port, tls::DEFAULT_TIMEOUT);
        let cycles = arch::counter_end().wrapping_sub(start);

        match timing {
            Ok(t) => {
                let _ = writeln!(log, "pass {}: {}", index + 1, t.summary());
                let seconds = t.total_ns as f64 / 1e9;
                passes.push(PassResult {
                    pass: index as u32 + 1,
                    repeats: 1,
                    loops: 1,
                    cycles,
                    seconds,
                    total_ops: 1.0,
                    mops: if seconds > 0.0 { 1e-6 / seconds } else { 0.0 },
                    cycles_per_op: cycles as f64,
                    ns_per_op: t.total_ns as f64,
                    ..Default::default()
                });
                last_timing = Some(t);
            }
            Err(e) => {
                let reason = format!("handshake failed: {e}");
                let _ = writeln!(log, "pass {}: {reason}", index + 1);
                if passes.is_empty() {
                    let mut report = BenchReport::failed(BenchKernel::Tls, config.mode, reason);
                    report.log = log;
                    return report;
                }
            }
        }
    }

    let summary = summarise(BenchKernel::Tls, config.mode, passes, last_timing);
    write_summary(&mut log, &profile, &summary);
    if let Some(t) = &summary.tls {
        let _ = writeln!(
            log,
            "  negotiated: {} / {}   certificates: {}   tls share of total: {:.1}%",
            t.protocol,
            t.cipher_suite,
            t.peer_certificates,
            t.tls_fraction() * 100.0
        );
    }
    let _ = writeln!(log, "\nstatus: completed successfully.");

    BenchReport {
        summary,
        log,
        error: None,
    }
}

/// Owns whatever buffers and keys a kernel needs, so per-pass setup cost does
/// not land inside the timed region.
///
/// The variants differ in size, which does not matter here: exactly one
/// `Workload` exists per benchmark run, and it is constructed before the
/// timed region opens.
#[allow(clippy::large_enum_variant)]
enum Workload {
    Isa(BenchKernel),
    Hash {
        payload: Vec<u8>,
        hmac: bool,
    },
    Aead {
        key: AeadKey,
        payload: Vec<u8>,
        buffer: Vec<u8>,
        nonce: [u8; NONCE_LEN],
    },
}

impl Workload {
    fn new(kernel: BenchKernel) -> Result<Workload, String> {
        match kernel {
            BenchKernel::Scalar | BenchKernel::Isa(_) => Ok(Workload::Isa(kernel)),
            BenchKernel::Crypto(Algorithm::Sha256) => Ok(Workload::Hash {
                payload: payload(),
                hmac: false,
            }),
            BenchKernel::Crypto(Algorithm::HmacSha256) => Ok(Workload::Hash {
                payload: payload(),
                hmac: true,
            }),
            BenchKernel::Crypto(algorithm) => {
                let key = AeadKey::new(algorithm, &[0x42u8; 32])
                    .map_err(|e| format!("could not build {} key: {e}", algorithm.name()))?;
                let payload = payload();
                Ok(Workload::Aead {
                    key,
                    buffer: Vec::with_capacity(payload.len() + 16),
                    payload,
                    nonce: [0u8; NONCE_LEN],
                })
            }
            BenchKernel::Tls => Err("TLS is measured by run_tls".to_string()),
        }
    }

    /// Executes `loops` iterations and returns an accumulator derived from the
    /// output, which the caller keeps live.
    fn execute(&mut self, dispatcher: &Dispatcher, loops: usize) -> u64 {
        match self {
            Workload::Isa(BenchKernel::Scalar) => scalar_kernel(loops),
            Workload::Isa(BenchKernel::Isa(backend)) => {
                dispatcher.run_kernel_for(*backend, loops).unwrap_or(0)
            }
            Workload::Isa(_) => 0,
            Workload::Hash { payload, hmac } => {
                let mut sink = 0u64;
                for i in 0..loops {
                    let digest = if *hmac {
                        nanochrono_crypto::hmac_sha256(&[0x5Au8; 32], payload)
                    } else {
                        nanochrono_crypto::sha256(payload)
                    };
                    sink ^= u64::from_le_bytes(digest[..8].try_into().unwrap()) ^ i as u64;
                }
                sink
            }
            Workload::Aead {
                key,
                payload,
                buffer,
                nonce,
            } => {
                let mut sink = 0u64;
                for i in 0..loops {
                    // A fresh nonce per iteration: reuse under one key would
                    // be a real vulnerability even in a benchmark, and it also
                    // keeps the cipher from short-circuiting anything.
                    nonce[..8].copy_from_slice(&(i as u64).to_le_bytes());
                    buffer.clear();
                    buffer.extend_from_slice(payload);
                    if key.seal(nonce, &[], buffer).is_err() {
                        break;
                    }
                    sink ^= u64::from_le_bytes(buffer[buffer.len() - 8..].try_into().unwrap());
                }
                sink
            }
        }
    }
}

fn payload() -> Vec<u8> {
    (0..CRYPTO_PAYLOAD_BYTES)
        .map(|i| (i.wrapping_mul(31) & 0xFF) as u8)
        .collect()
}

fn scalar_kernel(loops: usize) -> u64 {
    // `arch::x86::kernel_scalar` compiles for both x86 widths; only targets
    // with no architectural counter at all fall through to `generic`.
    #[cfg(any(target_arch = "x86_64", target_arch = "x86"))]
    {
        arch::x86::kernel_scalar(loops)
    }
    #[cfg(target_arch = "aarch64")]
    {
        arch::aarch64::kernel_scalar(loops)
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "x86", target_arch = "aarch64")))]
    {
        arch::generic::kernel_scalar(loops)
    }
}

#[allow(clippy::too_many_arguments)]
fn run_pass(
    chrono: &Chronometer,
    dispatcher: &Dispatcher,
    workload: &mut Workload,
    profile: &BenchProfile,
    pass: u32,
    repeats: u32,
    loops: usize,
) -> PassResult {
    let mut sink = 0u64;

    let start = arch::counter_start();
    for _ in 0..repeats {
        sink ^= workload.execute(dispatcher, loops);
    }
    let cycles = arch::counter_end().wrapping_sub(start);

    // Keeps the whole loop observably alive: without this the optimiser is
    // entitled to delete a pure computation whose result is discarded, and the
    // benchmark would report the cost of nothing.
    let sink = std::hint::black_box(sink);

    let seconds = chrono.units_to_secs(cycles);
    let total_ops = repeats as f64 * loops as f64 * profile.ops_per_loop;
    let total_bytes = total_ops * profile.bytes_per_op;

    PassResult {
        pass,
        repeats,
        loops,
        cycles,
        seconds,
        total_ops,
        total_bytes,
        mops: if seconds > 0.0 {
            total_ops / seconds / 1e6
        } else {
            0.0
        },
        cycles_per_op: if total_ops > 0.0 {
            cycles as f64 / total_ops
        } else {
            0.0
        },
        ns_per_op: if total_ops > 0.0 {
            seconds * 1e9 / total_ops
        } else {
            0.0
        },
        mib_per_second: if seconds > 0.0 && total_bytes > 0.0 {
            total_bytes / (1024.0 * 1024.0) / seconds
        } else {
            0.0
        },
        sink,
    }
}

fn summarise(
    kernel: BenchKernel,
    mode: BenchMode,
    passes: Vec<PassResult>,
    tls: Option<tls::HandshakeTiming>,
) -> BenchSummary {
    let mut summary = BenchSummary {
        kernel_name: kernel.name(),
        mode: mode.name(),
        tls,
        ..Default::default()
    };
    if passes.is_empty() {
        return summary;
    }

    summary.best_mops = f64::MIN;
    summary.worst_mops = f64::MAX;
    summary.best_ns_per_op = f64::MAX;
    summary.worst_ns_per_op = f64::MIN;

    for p in &passes {
        summary.best_mops = summary.best_mops.max(p.mops);
        summary.worst_mops = summary.worst_mops.min(p.mops);
        summary.best_ns_per_op = summary.best_ns_per_op.min(p.ns_per_op);
        summary.worst_ns_per_op = summary.worst_ns_per_op.max(p.ns_per_op);
        summary.total_cycles = summary.total_cycles.saturating_add(p.cycles);
        summary.total_seconds += p.seconds;
        summary.mean_mops += p.mops;
    }
    summary.mean_mops /= passes.len() as f64;
    summary.passes = passes;
    summary
}

fn write_header(log: &mut String, chrono: &Chronometer, config: &BenchConfig) {
    let features: CpuFeatures = nanochrono_core::cpu::features();
    let _ = writeln!(log, "=== NanoChronometer benchmark ===");
    let _ = writeln!(log, "operation: {}", config.kernel.name());
    let _ = writeln!(log, "mode: {}", config.mode.name());
    let _ = writeln!(log, "backend: {}", chrono.backend().name());
    let _ = writeln!(
        log,
        "simd: {}",
        SimdFamily::best().map(SimdFamily::name).unwrap_or("none")
    );
    let _ = writeln!(
        log,
        "counter: {:.3} MHz  invariant={}",
        chrono.counter_hz() as f64 / 1e6,
        features.invariant_counter
    );
    let _ = writeln!(log, "crypto provider: {}", nanochrono_crypto::PROVIDER);
    let _ = writeln!(
        log,
        "cpu flags: AES={} SHA={} VAES={} PCLMUL={} AVX={} AVX2={} AVX-VNNI={} AVX512F={}\n",
        features.aesni as u8,
        features.shani as u8,
        features.vaes as u8,
        features.pclmulqdq as u8,
        features.avx as u8,
        features.avx2 as u8,
        features.avx_vnni as u8,
        features.avx512f as u8,
    );
}

fn write_pass(log: &mut String, profile: &BenchProfile, r: &PassResult) {
    let _ = write!(
        log,
        "pass {}: repeats={}  loops={}  cycles={}  time={:.6} s  rate={:.3} Mops/s  \
         cyc/op={:.4}  ns/op={:.4}",
        r.pass, r.repeats, r.loops, r.cycles, r.seconds, r.mops, r.cycles_per_op, r.ns_per_op
    );
    if profile.bytes_per_op > 0.0 {
        let _ = write!(log, "  MiB/s={:.3}", r.mib_per_second);
    }
    let _ = writeln!(log, "  sink={:016X}", r.sink);
}

fn write_summary(log: &mut String, profile: &BenchProfile, s: &BenchSummary) {
    let _ = writeln!(log, "\nsummary:");
    let _ = writeln!(log, "  unit: {}", profile.unit);
    let _ = writeln!(
        log,
        "  rate: mean {:.3} Mops/s   best {:.3}   worst {:.3}",
        s.mean_mops, s.best_mops, s.worst_mops
    );
    let _ = writeln!(
        log,
        "  ns/op: best {:.4}   worst {:.4}",
        s.best_ns_per_op, s.worst_ns_per_op
    );
    let _ = writeln!(log, "  aggregate cycles: {}", s.total_cycles);
    let _ = writeln!(log, "  aggregate time: {:.6} s", s.total_seconds);

    // Pass-to-pass spread is the warm-up signal; call it out rather than
    // leaving the reader to compare three lines by eye.
    if s.best_mops > 0.0 && s.worst_mops > 0.0 {
        let spread = (s.best_mops - s.worst_mops) / s.best_mops * 100.0;
        if spread > 15.0 {
            let _ = writeln!(
                log,
                "  note: {spread:.1}% spread between passes — the workload is still warming up, \
                 so trust the last pass over the mean."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_offers_rows() {
        for mode in BenchMode::ALL.iter().copied() {
            assert!(!BenchKernel::rows_for(mode).is_empty(), "{mode:?}");
        }
    }

    #[test]
    fn rows_report_the_mode_they_came_from() {
        for mode in BenchMode::ALL.iter().copied() {
            for kernel in BenchKernel::rows_for(mode) {
                assert_eq!(kernel.mode(), mode);
            }
        }
    }

    #[test]
    fn scalar_baseline_produces_a_rate() {
        let chrono = Chronometer::new();
        let config = BenchConfig {
            mode: BenchMode::CpuIsa,
            kernel: BenchKernel::Scalar,
            ..Default::default()
        };
        let report = run(&chrono, &config);
        assert!(report.error.is_none(), "{:?}", report.error);
        assert_eq!(report.summary.passes.len(), 3);
        assert!(report.summary.best_mops > 0.0);
        assert!(report.log.contains("status: completed successfully"));
    }

    #[test]
    fn every_available_isa_kernel_runs() {
        let chrono = Chronometer::new();
        for backend in Backend::ALL.iter().copied() {
            let config = BenchConfig {
                mode: BenchMode::CpuIsa,
                kernel: BenchKernel::Isa(backend),
                ..Default::default()
            };
            let report = run(&chrono, &config);
            if backend.is_available() {
                assert!(
                    report.error.is_none(),
                    "{backend} failed: {:?}",
                    report.error
                );
            } else {
                assert!(report.error.is_some(), "{backend} ran but is unsupported");
            }
        }
    }

    #[test]
    fn unavailable_kernels_report_rather_than_fault() {
        let chrono = Chronometer::new();
        let unavailable = Backend::ALL.iter().copied().find(|b| !b.is_available());
        let Some(backend) = unavailable else {
            return; // every backend runs here; nothing to check
        };
        let report = run(
            &chrono,
            &BenchConfig {
                kernel: BenchKernel::Isa(backend),
                ..Default::default()
            },
        );
        assert!(report.error.is_some());
        assert!(report.summary.passes.is_empty());
    }

    #[test]
    fn every_crypto_primitive_benchmarks() {
        let chrono = Chronometer::new();
        for algorithm in Algorithm::ALL.iter().copied() {
            let config = BenchConfig {
                mode: BenchMode::Crypto,
                kernel: BenchKernel::Crypto(algorithm),
                ..Default::default()
            };
            let report = run(&chrono, &config);
            assert!(
                report.error.is_none(),
                "{} failed: {:?}",
                algorithm.name(),
                report.error
            );
            assert!(report.summary.best_mops > 0.0, "{}", algorithm.name());
            assert!(report.log.contains("rustls/ring"));
        }
    }

    #[test]
    fn aead_benchmark_throughput_is_plausible() {
        let chrono = Chronometer::new();
        let report = run(
            &chrono,
            &BenchConfig {
                mode: BenchMode::Crypto,
                kernel: BenchKernel::Crypto(Algorithm::Aes256Gcm),
                ..Default::default()
            },
        );
        let mib = report.summary.passes[2].mib_per_second;
        // AES-NI does gigabytes per second; software AES does hundreds of MiB.
        // Anything below 1 MiB/s means the workload was optimised away.
        assert!(mib > 1.0, "AES-256-GCM reported {mib:.3} MiB/s");
    }

    #[test]
    fn profiles_describe_their_unit() {
        for mode in BenchMode::ALL.iter().copied() {
            for kernel in BenchKernel::rows_for(mode) {
                let p = BenchProfile::for_kernel(kernel, CRYPTO_PAYLOAD_BYTES);
                assert!(!p.unit.is_empty());
                assert!(!p.description.is_empty());
                assert!(p.loops.iter().all(|&l| l > 0));
            }
        }
    }
}
