// SPDX-License-Identifier: Apache-2.0
//! The chronometer: a calibrated counter plus the drift tracking that keeps it
//! honest.
//!
//! Replaces the C `nc_ctx_t`. The lifetime is Rust's rather than
//! `nc_create`/`nc_destroy`, and all `cycles → ns` arithmetic goes through
//! `u128` instead of the C code's `_umul128`/`_udiv128` intrinsic pair, so the
//! overflow-safe path is the only path.

use crate::arch;
use crate::backend::Backend;
use crate::platform;
use crate::redundancy::{Integrity, Protected};

/// Default calibration window. Long enough that counter quantisation is noise,
/// short enough not to stall a UI on startup.
pub const DEFAULT_CALIBRATION_MS: u32 = 200;

/// A calibrated view of the architectural counter.
#[derive(Debug, Clone)]
pub struct Chronometer {
    backend: Backend,
    /// Raw counter units per second.
    ///
    /// Carried under an error-correcting code because every conversion this
    /// type performs divides by it: a single flipped bit here would scale
    /// every duration the process reports, silently, for as long as it runs.
    /// Reads are a plain load; the code is verified at the once-a-second
    /// checkpoint in [`elapsed_units`](Self::elapsed_units).
    counter_hz: Protected,
    /// Counter value at the last `start`/`reset`.
    ///
    /// Protected on the same grounds as the frequency: every elapsed reading
    /// is a subtraction from it, so a flipped bit here offsets the whole
    /// interval rather than corrupting a single sample. Verified at the same
    /// once-a-second checkpoint.
    start_units: Protected,
    /// Monotonic nanoseconds at the last `start`/`reset`.
    start_ns: u64,
    /// Rolling drift window origin.
    window_units: u64,
    window_ns: u64,
    drift_ppm: f64,
    overhead_units: u64,
    /// What the last calibration checkpoint found.
    last_integrity: Integrity,
}

impl Chronometer {
    /// Builds a chronometer on the backend the runtime dispatcher selected.
    ///
    /// Going through [`Dispatcher::global`](crate::dispatch::Dispatcher::global)
    /// rather than calling `Backend::best()` directly means detection runs once
    /// per process and any `NANOCHRONO_BACKEND` override is honoured here too.
    pub fn new() -> Self {
        Self::with_backend(crate::dispatch::Dispatcher::global().backend())
    }

    /// Builds a chronometer on `backend`, degrading if the CPU lacks it.
    pub fn with_backend(backend: Backend) -> Self {
        let backend = backend.resolve();
        let mut ctx = Chronometer {
            backend,
            counter_hz: Protected::new(1),
            start_units: Protected::new(0),
            start_ns: 0,
            window_units: 0,
            window_ns: 0,
            drift_ppm: 0.0,
            overhead_units: arch::read_overhead(),
            last_integrity: Integrity::Clean,
        };
        ctx.calibrate(DEFAULT_CALIBRATION_MS);
        ctx.reset();
        ctx
    }

    /// Re-measures the counter frequency over a `ms` window.
    ///
    /// AArch64 states its rate in `CNTFRQ_EL0`, so that value is trusted
    /// directly. x86-64 has no such register and must be timed against the
    /// monotonic clock.
    pub fn calibrate(&mut self, ms: u32) -> bool {
        if let Some(hz) = arch::declared_counter_hz() {
            self.counter_hz.set(hz);
            return true;
        }

        let window_ns = (ms.max(1) as u64) * 1_000_000;
        let ns0 = platform::monotonic_ns();
        let c0 = arch::counter_start();
        loop {
            let now = platform::monotonic_ns();
            if now.wrapping_sub(ns0) >= window_ns {
                break;
            }
            arch::cpu_relax();
        }
        let c1 = arch::counter_end();
        let ns1 = platform::monotonic_ns();

        let d_units = c1.wrapping_sub(c0);
        let d_ns = ns1.wrapping_sub(ns0);
        if d_ns == 0 || d_units == 0 {
            return false;
        }
        let hz = ((d_units as u128 * 1_000_000_000u128) / d_ns as u128) as u64;
        self.counter_hz.set(hz.max(1));
        true
    }

    /// Rebases the elapsed-time origin to now.
    pub fn reset(&mut self) {
        let units = arch::counter_end();
        let ns = platform::monotonic_ns();
        self.start_units.set(units);
        self.start_ns = ns;
        self.window_units = units;
        self.window_ns = ns;
        self.drift_ppm = 0.0;
    }

    /// Starts an interval and returns the raw counter value it began at.
    pub fn start(&mut self) -> u64 {
        let units = arch::counter_start();
        let ns = platform::monotonic_ns();
        self.start_units.set(units);
        self.start_ns = ns;
        self.window_units = units;
        self.window_ns = ns;
        units
    }

    /// Reads the counter without disturbing the interval origin.
    pub fn now_units(&self) -> u64 {
        arch::counter_end()
    }

    /// Raw counter units since the last [`start`](Self::start)/[`reset`](Self::reset).
    ///
    /// Also advances the drift estimate roughly once per second — cheap, and
    /// it means `drift_ppm` is fresh for any caller polling elapsed time.
    pub fn elapsed_units(&mut self) -> u64 {
        let now_units = arch::counter_end();
        let now_ns = platform::monotonic_ns();

        let d_ns = now_ns.wrapping_sub(self.window_ns);
        if d_ns >= 1_000_000_000 {
            // The once-a-second window is the natural integrity checkpoint: it
            // is already off the hot path, and it runs for exactly as long as
            // the measurement does — which is the interval over which an upset
            // has time to happen.
            self.last_integrity = self.counter_hz.verify().max(self.start_units.verify());

            let d_units = now_units.wrapping_sub(self.window_units);
            let instant_hz = ((d_units as u128 * 1_000_000_000u128) / d_ns as u128) as u64;
            let hz = self.counter_hz.get();
            if instant_hz != 0 && hz != 0 {
                let diff = instant_hz as f64 - hz as f64;
                self.drift_ppm = diff / hz as f64 * 1e6;
            }
            self.window_units = now_units;
            self.window_ns = now_ns;
        }

        now_units.wrapping_sub(self.start_units.get_checked().0)
    }

    /// Nanoseconds since the last start/reset.
    pub fn elapsed_ns(&mut self) -> u64 {
        let units = self.elapsed_units();
        self.units_to_ns(units)
    }

    /// Microseconds since the last start/reset.
    pub fn elapsed_us(&mut self) -> u64 {
        self.elapsed_ns() / 1_000
    }

    /// Milliseconds since the last start/reset.
    pub fn elapsed_ms(&mut self) -> u64 {
        self.elapsed_ns() / 1_000_000
    }

    /// Seconds since the last start/reset.
    pub fn elapsed_secs(&mut self) -> f64 {
        let units = self.elapsed_units();
        self.units_to_secs(units)
    }

    /// Converts raw counter units to nanoseconds.
    #[inline]
    pub fn units_to_ns(&self, units: u64) -> u64 {
        let hz = self.counter_hz.get();
        if hz == 0 {
            return 0;
        }
        ((units as u128 * 1_000_000_000u128) / hz as u128) as u64
    }

    #[inline]
    pub fn units_to_us(&self, units: u64) -> u64 {
        let hz = self.counter_hz.get();
        if hz == 0 {
            return 0;
        }
        ((units as u128 * 1_000_000u128) / hz as u128) as u64
    }

    #[inline]
    pub fn units_to_ms(&self, units: u64) -> u64 {
        let hz = self.counter_hz.get();
        if hz == 0 {
            return 0;
        }
        ((units as u128 * 1_000u128) / hz as u128) as u64
    }

    #[inline]
    pub fn units_to_secs(&self, units: u64) -> f64 {
        let hz = self.counter_hz.get();
        if hz == 0 {
            0.0
        } else {
            units as f64 / hz as f64
        }
    }

    /// Converts nanoseconds to raw counter units.
    #[inline]
    pub fn ns_to_units(&self, ns: u64) -> u64 {
        ((ns as u128 * self.counter_hz.get() as u128) / 1_000_000_000u128) as u64
    }

    /// Busy-waits for `ns` nanoseconds.
    ///
    /// Spins on the counter rather than sleeping: below a scheduler quantum
    /// there is no other way to be accurate, and that is the regime this
    /// function exists for. Above ~1 ms, prefer `std::thread::sleep`.
    pub fn spin_ns(&self, ns: u64) {
        if ns == 0 {
            return;
        }
        let target = arch::counter_end().wrapping_add(self.ns_to_units(ns));
        while arch::counter_end() < target {
            arch::cpu_relax();
        }
    }

    /// Busy-waits for `us` microseconds.
    pub fn spin_us(&self, us: u64) {
        self.spin_ns(us.saturating_mul(1_000));
    }

    /// The backend this chronometer resolved to.
    pub fn backend(&self) -> Backend {
        self.backend
    }

    /// Calibrated counter rate, in raw units per second.
    pub fn counter_hz(&self) -> u64 {
        self.counter_hz.get()
    }

    /// Verifies the calibration constant and repairs it if it can.
    ///
    /// Runs automatically once a second while elapsed time is being polled.
    /// Call it directly before trusting a reading after a long idle period,
    /// or in a scrubbing loop on a machine where upsets are expected.
    ///
    /// [`Integrity::Unrecoverable`] means the constant is gone and the
    /// chronometer must be recalibrated before its output means anything.
    pub fn verify_calibration(&mut self) -> Integrity {
        self.last_integrity = self.counter_hz.verify();
        self.last_integrity
    }

    /// What the last integrity checkpoint found.
    pub fn last_integrity(&self) -> Integrity {
        self.last_integrity
    }

    /// Flips one bit of the stored calibration, for fault-injection drills.
    ///
    /// The repair paths are unreachable in testing without this, and a system
    /// that claims to tolerate upsets should be exercised against injected
    /// ones rather than trusted on the strength of its unit tests alone.
    pub fn inject_calibration_flip(&mut self, bit: u32) {
        self.counter_hz.inject_flip(bit);
    }

    /// Drift of the live counter rate against the calibrated one, in ppm.
    ///
    /// A large magnitude means the calibration has gone stale — typically a
    /// non-invariant TSC under frequency scaling, or a migrated thread.
    pub fn drift_ppm(&self) -> f64 {
        self.drift_ppm
    }

    /// Cost of one counter read pair, cached from construction.
    pub fn overhead_units(&self) -> u64 {
        self.overhead_units
    }

    /// Re-measures counter read overhead, taking the best of many attempts.
    ///
    /// The minimum is the right statistic here: any sample above it contains
    /// interference, and interference is exactly what we want excluded from a
    /// figure that gets subtracted from other measurements.
    pub fn measure_overhead(&mut self) -> u64 {
        let best = (0..64).map(|_| arch::read_overhead()).min().unwrap_or(0);
        self.overhead_units = best;
        best
    }

    /// Best-case cost of crossing the FFI boundary, in counter units.
    pub fn measure_ffi_overhead(&self, iterations: u32) -> u64 {
        let n = iterations.max(1);
        (0..n)
            .map(|_| {
                let a = arch::counter_start();
                let b = arch::counter_end();
                b.wrapping_sub(a)
            })
            .min()
            .unwrap_or(0)
    }

    /// Best-case cost of one call to `f`, in counter units.
    pub fn measure_call_overhead<F: FnMut()>(&self, mut f: F, iterations: u32) -> u64 {
        let n = iterations.max(1);
        let mut best = u64::MAX;
        for _ in 0..n {
            let a = arch::counter_start();
            f();
            let b = arch::counter_end();
            best = best.min(b.wrapping_sub(a));
        }
        if best == u64::MAX {
            0
        } else {
            best
        }
    }

    /// Mean cost of one call to `f`, in counter units.
    pub fn measure_call_avg<F: FnMut()>(&self, mut f: F, iterations: u32) -> u64 {
        let n = iterations.max(1);
        let mut sum = 0u64;
        for _ in 0..n {
            let a = arch::counter_start();
            f();
            let b = arch::counter_end();
            sum = sum.wrapping_add(b.wrapping_sub(a));
        }
        sum / n as u64
    }

    /// Collects `count` per-call timings of `f` into a fresh vector.
    pub fn collect_samples<F: FnMut()>(&self, mut f: F, count: u32) -> Vec<u64> {
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let a = arch::counter_start();
            f();
            let b = arch::counter_end();
            out.push(b.wrapping_sub(a));
        }
        out
    }
}

impl Default for Chronometer {
    fn default() -> Self {
        Self::new()
    }
}
