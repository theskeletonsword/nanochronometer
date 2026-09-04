// SPDX-License-Identifier: Apache-2.0
//! OS-level time sources, CPU affinity and processor identity.
//!
//! Everything the timing core needs from the operating system lives here, so
//! the rest of the crate stays free of `cfg(windows)` / `cfg(unix)` branches.

use std::time::Duration;

/// Nanoseconds since the Unix epoch, from the highest-resolution wall clock
/// the platform offers.
///
/// This is the only clock that can be compared against NTP; it is *not*
/// monotonic and may step.
pub fn unix_time_ns() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;

        // FILETIME counts 100 ns intervals from 1601-01-01.
        const UNIX_EPOCH_IN_100NS: u64 = 116_444_736_000_000_000;
        let mut ft = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        unsafe { GetSystemTimePreciseAsFileTime(&mut ft) };
        let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
        ticks
            .saturating_sub(UNIX_EPOCH_IN_100NS)
            .saturating_mul(100)
    }
    #[cfg(unix)]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_REALTIME, &mut ts) } == 0 {
            (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
        } else {
            0
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }
}

/// Nanoseconds on a monotonic clock. The reference for calibrating raw counter
/// units against real time.
pub fn monotonic_ns() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Performance::{
            QueryPerformanceCounter, QueryPerformanceFrequency,
        };
        let mut freq: i64 = 0;
        let mut ctr: i64 = 0;
        unsafe {
            QueryPerformanceFrequency(&mut freq);
            QueryPerformanceCounter(&mut ctr);
        }
        if freq <= 0 {
            return 0;
        }
        ((ctr as u128 * 1_000_000_000u128) / freq as u128) as u64
    }
    #[cfg(unix)]
    {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) } == 0 {
            (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
        } else {
            0
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        use std::sync::OnceLock;
        static ORIGIN: OnceLock<std::time::Instant> = OnceLock::new();
        ORIGIN
            .get_or_init(std::time::Instant::now)
            .elapsed()
            .as_nanos() as u64
    }
}

/// Total CPU time consumed by this process, in nanoseconds.
pub fn process_time_ns() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
        let mut c = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut e = c;
        let mut k = c;
        let mut u = c;
        let ok = unsafe { GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u) };
        if ok == 0 {
            return monotonic_ns();
        }
        filetime_pair_ns(k, u)
    }
    #[cfg(unix)]
    {
        clock_ns(libc::CLOCK_PROCESS_CPUTIME_ID)
    }
    #[cfg(not(any(windows, unix)))]
    {
        monotonic_ns()
    }
}

/// Total CPU time consumed by the calling thread, in nanoseconds.
pub fn thread_time_ns() -> u64 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::FILETIME;
        use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadTimes};
        let mut c = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let mut e = c;
        let mut k = c;
        let mut u = c;
        let ok = unsafe { GetThreadTimes(GetCurrentThread(), &mut c, &mut e, &mut k, &mut u) };
        if ok == 0 {
            return monotonic_ns();
        }
        filetime_pair_ns(k, u)
    }
    #[cfg(unix)]
    {
        clock_ns(libc::CLOCK_THREAD_CPUTIME_ID)
    }
    #[cfg(not(any(windows, unix)))]
    {
        monotonic_ns()
    }
}

#[cfg(windows)]
fn filetime_pair_ns(
    kernel: windows_sys::Win32::Foundation::FILETIME,
    user: windows_sys::Win32::Foundation::FILETIME,
) -> u64 {
    let k = ((kernel.dwHighDateTime as u64) << 32) | kernel.dwLowDateTime as u64;
    let u = ((user.dwHighDateTime as u64) << 32) | user.dwLowDateTime as u64;
    k.saturating_add(u).saturating_mul(100)
}

#[cfg(unix)]
fn clock_ns(id: libc::clockid_t) -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(id, &mut ts) } == 0 {
        (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
    } else {
        monotonic_ns()
    }
}

/// Sentinel for "the platform will not tell us which CPU we are on".
pub const CPU_UNKNOWN: u32 = u32::MAX;

/// Index of the logical CPU currently running this thread.
///
/// Comparing this before and after a measurement is how migration is
/// detected: a thread that moved cores may have read two unrelated counters.
pub fn current_cpu() -> u32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::GetCurrentProcessorNumber;
        unsafe { GetCurrentProcessorNumber() }
    }
    #[cfg(target_os = "linux")]
    {
        let c = unsafe { libc::sched_getcpu() };
        if c < 0 {
            CPU_UNKNOWN
        } else {
            c as u32
        }
    }
    // No portable equivalent on macOS: Darwin exposes no `sched_getcpu`, and
    // on Apple Silicon there is no EL0-readable core identifier either. An
    // unknown CPU disables migration detection rather than faking it.
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        CPU_UNKNOWN
    }
}

/// Pins the calling thread to one logical CPU.
///
/// Returns `false` where the platform has no affinity API. Pinning is the
/// single most effective way to keep a calibration stable, because it removes
/// migration between counters that need not agree.
pub fn pin_thread_to_cpu(cpu_index: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::Threading::{GetCurrentThread, SetThreadAffinityMask};
        let bits = usize::BITS;
        let mask: usize = 1usize << (cpu_index % bits);
        unsafe { SetThreadAffinityMask(GetCurrentThread(), mask) != 0 }
    }
    #[cfg(target_os = "linux")]
    {
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_ZERO(&mut set);
            libc::CPU_SET(cpu_index as usize, &mut set);
            libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0
        }
    }
    // macOS has no thread-pinning API to call. `THREAD_AFFINITY_POLICY` sets
    // an affinity *tag*, which asks the scheduler to co-locate threads sharing
    // a tag — it never names a core — and Apple documents it as unsupported on
    // Apple Silicon entirely. Returning false is the honest answer: the
    // caller then knows its calibration is unpinned, which is what
    // `StableClockState::migrated` exists to report.
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = cpu_index;
        false
    }
}

/// Restores the default disposition for `SIGPIPE`.
///
/// Rust sets `SIGPIPE` to `SIG_IGN` at startup, which turns a closed pipe into
/// a write error and, for the `println!` family, a panic. That makes
/// `nanochrono catalog | head` — an ordinary thing to type — abort with a
/// backtrace instead of exiting quietly. Restoring the default disposition
/// makes the process die on `SIGPIPE` the way every other Unix tool does.
///
/// Call once, early in `main`, before any output. Library code should not call
/// this: it changes process-wide state that the host application owns.
pub fn restore_default_sigpipe() {
    #[cfg(unix)]
    {
        // SAFETY: setting a signal disposition to SIG_DFL is always valid, and
        // this runs before any thread has been spawned.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_DFL);
        }
    }
}

/// Sleeps for `ms` milliseconds, resuming after signal interruption.
pub fn sleep_ms(ms: u32) {
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms as u64));
    }
}

/// Local UTC offset in minutes for the given instant.
///
/// Positive is east of Greenwich. DST is resolved for that instant rather than
/// for "now", so formatting a historical timestamp stays correct.
pub fn utc_offset_minutes(unix_ns: u64) -> i32 {
    #[cfg(windows)]
    {
        use windows_sys::Win32::System::SystemServices::{
            TIME_ZONE_ID_DAYLIGHT, TIME_ZONE_ID_STANDARD,
        };
        use windows_sys::Win32::System::Time::GetTimeZoneInformation;
        let _ = unix_ns;
        let mut tzi = unsafe { std::mem::zeroed() };
        let kind = unsafe { GetTimeZoneInformation(&mut tzi) };
        let bias: i32 = match kind {
            TIME_ZONE_ID_DAYLIGHT => tzi.Bias + tzi.DaylightBias,
            TIME_ZONE_ID_STANDARD => tzi.Bias + tzi.StandardBias,
            _ => tzi.Bias,
        };
        -bias
    }
    #[cfg(unix)]
    {
        let secs = (unix_ns / 1_000_000_000) as libc::time_t;
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        if unsafe { libc::localtime_r(&secs, &mut tm) }.is_null() {
            return 0;
        }
        (tm.tm_gmtoff / 60) as i32
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = unix_ns;
        0
    }
}

/// macOS system queries, through `sysctlbyname`.
///
/// The Linux paths this crate uses elsewhere — `/sys`, `/proc`, the auxiliary
/// vector — do not exist on macOS, and Darwin publishes the same information
/// through the sysctl namespace instead. This is the one place that knows how
/// to read it; `cpu`, `pmu` and `hypervisor` go through here.
#[cfg(target_os = "macos")]
pub mod darwin {
    /// Reads an integer sysctl.
    ///
    /// Darwin's integer sysctls are a mix of 32- and 64-bit, and asking for
    /// the wrong width returns `ENOMEM` rather than converting. The length the
    /// kernel reports is used to pick, so callers do not have to know which
    /// any given name is.
    pub fn sysctl_u64(name: &str) -> Option<u64> {
        let key = std::ffi::CString::new(name).ok()?;
        let mut value: u64 = 0;
        let mut len = std::mem::size_of::<u64>();

        // SAFETY: `key` is NUL-terminated and outlives the call; `value` and
        // `len` are a matched out-parameter pair, with `len` the capacity of
        // `value` in bytes.
        let status = unsafe {
            libc::sysctlbyname(
                key.as_ptr(),
                (&mut value as *mut u64).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        // A 4-byte sysctl leaves the upper half of `value` untouched, which is
        // zero here, so the read is already correct either way. The width is
        // still checked so an unexpected size is refused rather than
        // reinterpreted.
        match len {
            4 | 8 => Some(value),
            _ => None,
        }
    }

    /// Reads a string sysctl.
    pub fn sysctl_string(name: &str) -> Option<String> {
        let key = std::ffi::CString::new(name).ok()?;
        let mut len = 0usize;

        // A null buffer asks for the required size, which is the documented
        // way to size the second call.
        // SAFETY: `key` is NUL-terminated; a null value pointer with a live
        // length is the size query.
        let status = unsafe {
            libc::sysctlbyname(
                key.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || len == 0 || len > 4096 {
            return None;
        }

        let mut buffer = vec![0u8; len];
        // SAFETY: `buffer` has exactly `len` bytes, which is what the size
        // query reported.
        let status = unsafe {
            libc::sysctlbyname(
                key.as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 {
            return None;
        }
        // The kernel includes the terminator in the length it reports.
        let text = buffer
            .split(|&b| b == 0)
            .next()
            .map(|s| String::from_utf8_lossy(s).trim().to_string())?;
        (!text.is_empty()).then_some(text)
    }

    /// Reads the `hw.optional.arm.caps` bit buffer.
    ///
    /// Apple publishes every AArch64 architectural extension as one bit here,
    /// so the whole feature set costs a single syscall instead of one per
    /// extension. The bit numbers are ABI, fixed in the SDK's
    /// `arm/cpu_capabilities_public.h`, which says existing entries are never
    /// renumbered.
    pub fn arm_capability_bits() -> Option<Vec<u8>> {
        let key = std::ffi::CString::new("hw.optional.arm.caps").ok()?;
        // CAP_BIT_NB is 80 in the macOS 26 SDK and grows over time, so this
        // asks for the size rather than assuming one.
        let mut len = 0usize;
        // SAFETY: NUL-terminated key, null buffer with a live length is the
        // documented size query.
        let status = unsafe {
            libc::sysctlbyname(
                key.as_ptr(),
                std::ptr::null_mut(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        if status != 0 || len == 0 || len > 256 {
            return None;
        }

        let mut buffer = vec![0u8; len];
        // SAFETY: `buffer` is exactly the size the kernel asked for.
        let status = unsafe {
            libc::sysctlbyname(
                key.as_ptr(),
                buffer.as_mut_ptr().cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        (status == 0).then(|| {
            buffer.truncate(len);
            buffer
        })
    }

    /// Whether capability `bit` is set in a buffer from
    /// [`arm_capability_bits`].
    pub fn has_capability(caps: &[u8], bit: u32) -> bool {
        let byte = (bit / 8) as usize;
        caps.get(byte).is_some_and(|b| b & (1 << (bit % 8)) != 0)
    }
}
