// SPDX-License-Identifier: Apache-2.0
//! Performance-monitoring counters, through each platform's own interface.
//!
//! The PMU is never touched directly. On both platforms the kernel owns the
//! counters, schedules them per thread and corrects for multiplexing, and
//! going around it produces numbers that look plausible and are wrong:
//!
//! * **Linux** — `perf_event_open`. See [`crate::perf`]. No `RDPMC`, no
//!   `PMCCNTR_EL0`: a raw counter read cannot see multiplexing, does not
//!   survive a context switch, and on a hybrid CPU silently reads whichever
//!   PMU it landed on.
//! * **Windows** — the NTOSKRNL thread-profiling API
//!   (`EnableThreadProfiling` / `ReadThreadProfilingData`), with
//!   `QueryThreadCycleTime` as the always-available floor. Not ETW: ETW is a
//!   system-wide tracing pipeline that needs a session and administrator
//!   rights, which is the wrong shape for reading this thread's counters. The
//!   thread-profiling API is the same kernel facility ETW's profile provider
//!   uses, reached directly.
//! * **macOS** — `CLOCK_THREAD_CPUTIME_ID`. The PMU proper is behind `kperf`,
//!   a private framework that needs an entitlement Apple does not grant, so
//!   there is no hardware counter to reach. What is available is the kernel's
//!   accounting of the thread's CPU time, which is a *duration*, not a cycle
//!   count — [`PmuBackend::unit`] says so, and nothing here pretends
//!   otherwise.
//! * **Elsewhere** — unavailable, and it says so rather than substituting a
//!   different clock and calling it a cycle count.
//!
//! Both platforms cost roughly a syscall per read. That is the price of a
//! counter the kernel owns; the hot-path counters are the architectural ones
//! in [`crate::arch`].

/// Which interface supplied a reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmuBackend {
    /// Linux `perf_event_open`, one event per CPU PMU.
    PerfEvent,
    /// Windows `EnableThreadProfiling` / `ReadThreadProfilingData`.
    WindowsThreadProfiling,
    /// Windows `QueryThreadCycleTime`. Always available, but it reports the
    /// thread's cycle *time* rather than a programmable PMU event.
    WindowsThreadCycleTime,
    /// macOS `CLOCK_THREAD_CPUTIME_ID`. Kernel accounting in nanoseconds, not
    /// a counter: the real PMU is behind a private, entitlement-gated
    /// framework.
    MacThreadCpuTime,
    /// No PMU interface on this platform.
    Unavailable,
}

impl PmuBackend {
    pub const fn name(self) -> &'static str {
        match self {
            PmuBackend::PerfEvent => "perf_event_open",
            PmuBackend::WindowsThreadProfiling => "ReadThreadProfilingData",
            PmuBackend::WindowsThreadCycleTime => "QueryThreadCycleTime",
            PmuBackend::MacThreadCpuTime => "CLOCK_THREAD_CPUTIME_ID",
            PmuBackend::Unavailable => "unavailable",
        }
    }

    /// Whether the backend reads a real hardware counter.
    ///
    /// `QueryThreadCycleTime` does not: it is the kernel's accounting of the
    /// thread's cycles, which is close enough for attribution but is not a
    /// PMU event and cannot be reprogrammed.
    pub const fn is_hardware_counter(self) -> bool {
        matches!(
            self,
            PmuBackend::PerfEvent | PmuBackend::WindowsThreadProfiling
        )
    }

    /// What the number a reading carries actually measures.
    ///
    /// Every backend but one reports cycles, and the field is named for that.
    /// macOS reports elapsed CPU time instead, and a caller that divides it by
    /// a clock rate would be wrong by that rate — so the unit is part of the
    /// reading rather than something to infer from the platform.
    pub const fn unit(self) -> PmuUnit {
        match self {
            PmuBackend::MacThreadCpuTime => PmuUnit::Nanoseconds,
            _ => PmuUnit::Cycles,
        }
    }
}

/// What a [`PmuReading`] counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PmuUnit {
    /// CPU cycles.
    Cycles,
    /// Nanoseconds of CPU time.
    Nanoseconds,
}

impl PmuUnit {
    pub const fn name(self) -> &'static str {
        match self {
            PmuUnit::Cycles => "cycles",
            PmuUnit::Nanoseconds => "ns",
        }
    }
}

/// A per-thread cycle count and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmuReading {
    pub cycles: u64,
    pub backend: PmuBackend,
}

/// Reads the calling thread's cycle count.
///
/// `None` where no interface is available — routine in containers, in VMs with
/// no exposed PMU, and on Windows without profiling privileges.
pub fn read_thread_cycles() -> Option<PmuReading> {
    #[cfg(target_os = "linux")]
    {
        crate::perf::read_thread_cycles().map(|cycles| PmuReading {
            cycles,
            backend: PmuBackend::PerfEvent,
        })
    }
    #[cfg(target_os = "windows")]
    {
        windows::read_thread_cycles()
    }
    #[cfg(target_os = "macos")]
    {
        macos::read_thread_cpu_time().map(|cycles| PmuReading {
            cycles,
            backend: PmuBackend::MacThreadCpuTime,
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// The interface this platform will use, without taking a reading.
pub fn backend() -> PmuBackend {
    #[cfg(target_os = "linux")]
    {
        if crate::perf::is_available() {
            PmuBackend::PerfEvent
        } else {
            PmuBackend::Unavailable
        }
    }
    #[cfg(target_os = "windows")]
    {
        windows::backend()
    }
    #[cfg(target_os = "macos")]
    {
        // The clock is mandatory in POSIX 2008 and present on every supported
        // macOS, so this is a floor rather than a guess.
        PmuBackend::MacThreadCpuTime
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        PmuBackend::Unavailable
    }
}

/// How many hardware PMUs the counter spans. Two on a hybrid Intel CPU.
pub fn pmu_count() -> usize {
    #[cfg(target_os = "linux")]
    {
        crate::perf::pmu_count()
    }
    #[cfg(not(target_os = "linux"))]
    {
        usize::from(backend() != PmuBackend::Unavailable)
    }
}

#[cfg(target_os = "macos")]
mod macos {
    //! Thread CPU time on Darwin.
    //!
    //! `CLOCK_THREAD_CPUTIME_ID` rather than `thread_info(THREAD_BASIC_INFO)`:
    //! both are the same kernel accounting, but `thread_info` reports it in
    //! whole microseconds, which is a hundred times coarser than this crate's
    //! resolution and would quantise away most of what it measures.

    /// Nanoseconds of CPU time this thread has consumed.
    pub(super) fn read_thread_cpu_time() -> Option<u64> {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `ts` is a live, correctly typed out-parameter and the clock
        // id is a documented constant.
        let ok = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) } == 0;
        ok.then(|| (ts.tv_sec as u64).saturating_mul(1_000_000_000) + ts.tv_nsec as u64)
    }
}

#[cfg(target_os = "windows")]
mod windows {
    //! Windows PMU access through the NTOSKRNL thread-profiling API.

    use super::{PmuBackend, PmuReading};
    use std::sync::OnceLock;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Performance::HardwareCounterProfiling::{
        DisableThreadProfiling, EnableThreadProfiling, ReadThreadProfilingData, PERFORMANCE_DATA,
    };
    use windows_sys::Win32::System::Threading::GetCurrentThread;
    use windows_sys::Win32::System::WindowsProgramming::QueryThreadCycleTime;

    /// `THREAD_PROFILING_FLAG_DISPATCH`: account across context switches, so
    /// the reading follows the thread rather than the core.
    const THREAD_PROFILING_FLAG_DISPATCH: u32 = 0x0000_0001;

    /// `READ_THREAD_PROFILING_FLAG_DISPATCHING`, which asks for the
    /// context-switch count alongside the cycle time.
    const READ_FLAG_DISPATCHING: u32 = 0x0000_0001;

    /// The profiling session for this thread, opened once.
    ///
    /// `EnableThreadProfiling` fails without the profiling privilege, which is
    /// the common case for an unprivileged process — hence the `Option` rather
    /// than an error the caller has to handle on every read.
    struct Session(HANDLE);

    // SAFETY: the handle belongs to the thread that opened it and is only ever
    // used from that thread, which the thread-local storage below enforces.
    unsafe impl Send for Session {}

    impl Drop for Session {
        fn drop(&mut self) {
            // SAFETY: the handle came from a successful EnableThreadProfiling.
            unsafe { DisableThreadProfiling(self.0) };
        }
    }

    thread_local! {
        /// One profiling session per thread, matching what the API measures.
        static SESSION: Option<Session> = open_session();
    }

    fn open_session() -> Option<Session> {
        let mut handle: HANDLE = std::ptr::null_mut();
        // SAFETY: `GetCurrentThread` returns a pseudo-handle that is always
        // valid for the calling thread; `handle` is a writable out-parameter.
        let status = unsafe {
            EnableThreadProfiling(
                GetCurrentThread(),
                THREAD_PROFILING_FLAG_DISPATCH,
                // A zero counter mask asks for cycle time only. Programming
                // specific PMU events needs a hardware counter set configured
                // by an administrator, which is out of scope for a library
                // that must work unprivileged.
                0,
                &mut handle,
            )
        };
        (status == 0 && !handle.is_null()).then_some(Session(handle))
    }

    /// Whether the thread-profiling API is usable in this process.
    fn profiling_available() -> bool {
        static AVAILABLE: OnceLock<bool> = OnceLock::new();
        *AVAILABLE.get_or_init(|| SESSION.with(|s| s.is_some()))
    }

    pub(super) fn backend() -> PmuBackend {
        if profiling_available() {
            PmuBackend::WindowsThreadProfiling
        } else {
            // QueryThreadCycleTime needs no privileges and is always present
            // on any supported Windows, so this is a floor, not a failure.
            PmuBackend::WindowsThreadCycleTime
        }
    }

    pub(super) fn read_thread_cycles() -> Option<PmuReading> {
        if let Some(cycles) = read_profiling_data() {
            return Some(PmuReading {
                cycles,
                backend: PmuBackend::WindowsThreadProfiling,
            });
        }
        read_cycle_time().map(|cycles| PmuReading {
            cycles,
            backend: PmuBackend::WindowsThreadCycleTime,
        })
    }

    /// Reads the profiling session, when one was opened.
    fn read_profiling_data() -> Option<u64> {
        SESSION.with(|session| {
            let handle = session.as_ref()?.0;
            // SAFETY: zeroing is valid for this POD struct, and the API
            // requires `Size` to be set before the call.
            let mut data: PERFORMANCE_DATA = unsafe { std::mem::zeroed() };
            data.Size = std::mem::size_of::<PERFORMANCE_DATA>() as u16;
            data.Version = 1;

            // SAFETY: `handle` came from a successful EnableThreadProfiling on
            // this thread, and `data` is a correctly sized out-parameter.
            let status =
                unsafe { ReadThreadProfilingData(handle, READ_FLAG_DISPATCHING, &mut data) };
            (status == 0).then_some(data.CycleTime)
        })
    }

    /// The always-available floor.
    fn read_cycle_time() -> Option<u64> {
        let mut cycles: u64 = 0;
        // SAFETY: the pseudo-handle is always valid for the calling thread and
        // `cycles` is a writable out-parameter.
        let ok = unsafe { QueryThreadCycleTime(GetCurrentThread(), &mut cycles) };
        (ok != 0).then_some(cycles)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_and_reading_agree() {
        let backend = backend();
        match read_thread_cycles() {
            Some(reading) => {
                assert_ne!(reading.backend, PmuBackend::Unavailable);
                assert_ne!(backend, PmuBackend::Unavailable);
            }
            None => {
                // A platform with no interface must say so consistently.
                if cfg!(not(any(target_os = "linux", target_os = "windows"))) {
                    assert_eq!(backend, PmuBackend::Unavailable);
                }
            }
        }
    }

    #[test]
    fn cycles_advance_over_real_work() {
        let Some(before) = read_thread_cycles() else {
            return;
        };
        let mut acc = 0u64;
        for i in 0..500_000u64 {
            acc = acc.wrapping_add(i).rotate_left(3);
        }
        std::hint::black_box(acc);
        let after = read_thread_cycles().expect("interface stayed available");
        assert_eq!(after.backend, before.backend);
        assert!(
            after.cycles > before.cycles,
            "{} did not advance: {} -> {}",
            after.backend.name(),
            before.cycles,
            after.cycles
        );
    }

    /// `QueryThreadCycleTime` is kernel accounting, not a PMU event, and the
    /// distinction has to survive into the report.
    #[test]
    fn only_real_counters_claim_to_be_hardware() {
        assert!(PmuBackend::PerfEvent.is_hardware_counter());
        assert!(PmuBackend::WindowsThreadProfiling.is_hardware_counter());
        assert!(!PmuBackend::WindowsThreadCycleTime.is_hardware_counter());
        assert!(!PmuBackend::MacThreadCpuTime.is_hardware_counter());
        assert!(!PmuBackend::Unavailable.is_hardware_counter());
    }

    /// A duration and a cycle count are not interchangeable, and the reading
    /// has to say which it is holding.
    #[test]
    fn only_the_time_based_backend_reports_nanoseconds() {
        assert_eq!(PmuBackend::MacThreadCpuTime.unit(), PmuUnit::Nanoseconds);
        for cycles in [
            PmuBackend::PerfEvent,
            PmuBackend::WindowsThreadProfiling,
            PmuBackend::WindowsThreadCycleTime,
        ] {
            assert_eq!(cycles.unit(), PmuUnit::Cycles, "{}", cycles.name());
        }
    }

    #[test]
    fn every_backend_names_itself() {
        for b in [
            PmuBackend::PerfEvent,
            PmuBackend::WindowsThreadProfiling,
            PmuBackend::WindowsThreadCycleTime,
            PmuBackend::MacThreadCpuTime,
            PmuBackend::Unavailable,
        ] {
            assert!(!b.name().is_empty());
        }
    }

    #[test]
    fn pmu_count_matches_availability() {
        if backend() == PmuBackend::Unavailable {
            assert_eq!(pmu_count(), 0);
        } else {
            assert!(pmu_count() >= 1);
        }
    }
}
