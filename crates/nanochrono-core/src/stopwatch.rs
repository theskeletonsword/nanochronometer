// SPDX-License-Identifier: Apache-2.0
//! Stopwatch state machine.
//!
//! Elapsed time is accumulated in nanoseconds at every pause, and the live
//! segment is measured from a raw counter reading. That split is what the C
//! GUI got wrong for several releases: it reset the context on resume but kept
//! adding the pre-pause total, so a resumed stopwatch double-counted. Here the
//! accumulator and the live segment cannot overlap, because `start_units` is
//! `Some` only while running.
//!
//! # Bit-flip tolerance
//!
//! Every number the stopwatch carries across time is held in a
//! [`Protected`](crate::redundancy::Protected) word rather than a bare `u64`.
//! A stopwatch is the longest-lived state in the toolkit — a run can sit in
//! memory for hours — and it is exactly the state where a single flipped bit
//! is invisible: a corrupted total is still a plausible-looking duration, and
//! nothing downstream can tell it apart from a real one.
//!
//! Reads go through the error-correcting code and hand back the repaired
//! value, so a damaged word never reaches a caller. Repairing the *storage*
//! needs a `&mut`, so it happens at the state transitions — pause, stop, lap,
//! reset — which are user actions and cost nothing on the measurement path.
//! Triple redundancy stays what it is elsewhere in the toolkit: the emergency
//! tier, reached only when the code finds damage it cannot repair alone.

use crate::arch;
use crate::context::Chronometer;
use crate::format::{self, DetailMode};
use crate::redundancy::{Integrity, Protected};

/// Where the stopwatch is in its cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StopwatchState {
    /// Never started, or reset. Elapsed is zero.
    #[default]
    Reset,
    /// Counting.
    Running,
    /// Frozen, resumable.
    Paused,
    /// Frozen by an explicit stop. Resumable, same as paused, but reported
    /// differently so the UI can colour it.
    Stopped,
}

impl StopwatchState {
    pub const fn name(self) -> &'static str {
        match self {
            StopwatchState::Reset => "reset",
            StopwatchState::Running => "running",
            StopwatchState::Paused => "paused",
            StopwatchState::Stopped => "stopped",
        }
    }

    pub const fn is_running(self) -> bool {
        matches!(self, StopwatchState::Running)
    }
}

/// A start/pause/resume/stop stopwatch over a calibrated counter.
#[derive(Debug, Clone, Default)]
pub struct Stopwatch {
    state: StopwatchState,
    /// Total from all completed segments.
    accumulated_ns: Protected,
    /// Counter value at the start of the live segment; `None` unless running.
    start_units: Option<Protected>,
    /// Elapsed at each recorded lap. A lap is a measurement someone chose to
    /// keep, so it is protected on the same terms as the running total.
    laps: Vec<Protected>,
    /// What the last scrub found. Reported, not acted on.
    last_integrity: Integrity,
}

impl Stopwatch {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> StopwatchState {
        self.state
    }

    /// The recorded laps, each verified as it is yielded.
    pub fn laps(&self) -> impl ExactSizeIterator<Item = u64> + '_ {
        self.laps.iter().map(|lap| lap.get_checked().0)
    }

    pub fn lap_count(&self) -> usize {
        self.laps.len()
    }

    /// The most recent lap, or `None` if none were recorded.
    pub fn last_lap(&self) -> Option<u64> {
        self.laps.last().map(|lap| lap.get_checked().0)
    }

    /// Total elapsed nanoseconds, including the live segment when running.
    ///
    /// Both stored words are verified on the way out, so the returned duration
    /// is correct even if the storage behind it has been damaged. Fixing that
    /// storage needs [`verify`](Self::verify).
    pub fn elapsed_ns(&self, chrono: &Chronometer) -> u64 {
        let accumulated = self.accumulated_ns.get_checked().0;
        match &self.start_units {
            Some(start) => {
                let live = arch::counter_end().wrapping_sub(start.get_checked().0);
                accumulated.saturating_add(chrono.units_to_ns(live))
            }
            None => accumulated,
        }
    }

    /// Checks every stored word without repairing any of them.
    ///
    /// Takes a shared borrow, so it suits a UI that polls for a warning light.
    /// Returns the worst outcome found.
    pub fn integrity(&self) -> Integrity {
        let mut worst = self.accumulated_ns.get_checked().1;
        if let Some(start) = &self.start_units {
            worst = worst.max(start.get_checked().1);
        }
        for lap in &self.laps {
            worst = worst.max(lap.get_checked().1);
        }
        worst
    }

    /// Verifies and repairs every stored word, returning the worst outcome.
    ///
    /// Called automatically at each state transition. Call it directly to
    /// scrub a stopwatch that has been sitting idle — a paused run holds its
    /// total indefinitely, and nothing else will touch it until it resumes.
    pub fn verify(&mut self) -> Integrity {
        let mut worst = self.accumulated_ns.verify();
        if let Some(start) = &mut self.start_units {
            worst = worst.max(start.verify());
        }
        for lap in &mut self.laps {
            worst = worst.max(lap.verify());
        }
        self.last_integrity = worst;
        worst
    }

    /// What the last scrub found.
    pub fn last_integrity(&self) -> Integrity {
        self.last_integrity
    }

    /// Formats the current elapsed time at the requested detail.
    pub fn format(&self, chrono: &Chronometer, detail: DetailMode) -> String {
        format::format_elapsed(self.elapsed_ns(chrono), detail)
    }

    /// Starts from zero, discarding any previous total and laps.
    pub fn start(&mut self) {
        self.accumulated_ns = Protected::new(0);
        self.laps.clear();
        self.last_integrity = Integrity::Clean;
        self.start_units = Some(Protected::new(arch::counter_start()));
        self.state = StopwatchState::Running;
    }

    /// Resumes from the accumulated total without discarding it.
    ///
    /// The total has been sitting untouched since the pause, which is the
    /// longest any stopwatch word goes unexamined, so it is scrubbed here
    /// before the new segment starts adding to it.
    pub fn resume(&mut self) {
        if self.state == StopwatchState::Running {
            return;
        }
        self.verify();
        self.start_units = Some(Protected::new(arch::counter_start()));
        self.state = StopwatchState::Running;
    }

    /// Folds the live segment into the total and freezes.
    pub fn pause(&mut self, chrono: &Chronometer) {
        if self.state != StopwatchState::Running {
            return;
        }
        self.freeze(chrono);
        self.state = StopwatchState::Paused;
    }

    /// Freezes and marks the run as finished. Still resumable.
    pub fn stop(&mut self, chrono: &Chronometer) {
        if self.state == StopwatchState::Running {
            self.freeze(chrono);
        }
        self.state = StopwatchState::Stopped;
    }

    /// Clears everything back to zero.
    pub fn reset(&mut self) {
        self.accumulated_ns = Protected::new(0);
        self.start_units = None;
        self.laps.clear();
        self.last_integrity = Integrity::Clean;
        self.state = StopwatchState::Reset;
    }

    /// Records the current elapsed time as a lap and returns it.
    pub fn lap(&mut self, chrono: &Chronometer) -> u64 {
        self.verify();
        let now = self.elapsed_ns(chrono);
        self.laps.push(Protected::new(now));
        now
    }

    /// The single "primary action" the space bar drives: start, pause, resume.
    pub fn toggle(&mut self, chrono: &Chronometer) {
        match self.state {
            StopwatchState::Reset => self.start(),
            StopwatchState::Running => self.pause(chrono),
            StopwatchState::Paused | StopwatchState::Stopped => self.resume(),
        }
    }

    /// Folds the live segment into the total.
    ///
    /// The read-modify-write here is the one place a damaged total would be
    /// baked in permanently, so the scrub happens before it, not after.
    fn freeze(&mut self, chrono: &Chronometer) {
        self.verify();
        if let Some(start) = self.start_units.take() {
            let live = arch::counter_end().wrapping_sub(start.get());
            let total = self
                .accumulated_ns
                .get()
                .saturating_add(chrono.units_to_ns(live));
            self.accumulated_ns.set(total);
        }
    }

    /// Flips bit `bit` of the accumulated total, for drills and tests.
    ///
    /// Bits 0..64 hit the value, 64..72 the code, 72..200 the replicas. See
    /// [`Protected::inject_flip`](crate::redundancy::Protected::inject_flip).
    #[doc(hidden)]
    pub fn inject_total_flip(&mut self, bit: u32) {
        self.accumulated_ns.inject_flip(bit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_reset_at_zero() {
        let chrono = Chronometer::new();
        let sw = Stopwatch::new();
        assert_eq!(sw.state(), StopwatchState::Reset);
        assert_eq!(sw.elapsed_ns(&chrono), 0);
    }

    #[test]
    fn pause_freezes_the_reading() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        sw.pause(&chrono);
        let frozen = sw.elapsed_ns(&chrono);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert_eq!(sw.elapsed_ns(&chrono), frozen);
    }

    /// The bug the C GUI carried: resuming must continue from the accumulated
    /// total, not restart it and not double-count it.
    #[test]
    fn resume_continues_without_double_counting() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(10));
        sw.pause(&chrono);
        let first = sw.elapsed_ns(&chrono);

        sw.resume();
        std::thread::sleep(std::time::Duration::from_millis(10));
        sw.pause(&chrono);
        let total = sw.elapsed_ns(&chrono);

        assert!(total > first, "resume must keep counting");
        // Two ~10 ms segments: well under a 3x blowup from double counting.
        assert!(
            total < first * 3,
            "resume double-counted: first={first} total={total}"
        );
    }

    #[test]
    fn restart_after_stop_discards_the_total() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        sw.stop(&chrono);
        assert!(sw.elapsed_ns(&chrono) > 0);
        sw.start();
        sw.pause(&chrono);
        assert!(sw.elapsed_ns(&chrono) < 5_000_000);
    }

    #[test]
    fn reset_clears_laps_and_total() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        sw.lap(&chrono);
        sw.reset();
        assert_eq!(sw.state(), StopwatchState::Reset);
        assert_eq!(sw.elapsed_ns(&chrono), 0);
        assert_eq!(sw.lap_count(), 0);
        assert_eq!(sw.last_lap(), None);
    }

    /// The point of the exercise: a particle flips a bit in a running total,
    /// and the next reading is still right.
    #[test]
    fn a_flipped_bit_in_the_total_does_not_reach_the_reading() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(10));
        sw.pause(&chrono);
        let truth = sw.elapsed_ns(&chrono);

        // Bit 40 of a ~10 ms total is a trillion nanoseconds: a corrupted
        // reading would be off by about twenty minutes.
        sw.inject_total_flip(40);
        assert_eq!(
            sw.elapsed_ns(&chrono),
            truth,
            "a single flipped bit changed the reported time"
        );
        assert!(matches!(
            sw.integrity(),
            Integrity::CorrectedByEcc { bit: 40 }
        ));

        // And the scrub puts the storage back, so it stops reporting damage.
        assert!(matches!(sw.verify(), Integrity::CorrectedByEcc { bit: 40 }));
        assert_eq!(sw.integrity(), Integrity::Clean);
        assert_eq!(sw.elapsed_ns(&chrono), truth);
    }

    /// Two flips are past what the code can repair, so the emergency tier
    /// votes. It is not reached by a single flip.
    #[test]
    fn a_double_flip_escalates_to_the_vote() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        sw.pause(&chrono);
        let truth = sw.elapsed_ns(&chrono);

        sw.inject_total_flip(11);
        sw.inject_total_flip(46);
        assert!(matches!(
            sw.verify(),
            Integrity::CorrectedByTmr { outvoted: 1 }
        ));
        assert_eq!(sw.elapsed_ns(&chrono), truth);
    }

    /// Resuming folds a stale total into a live segment, so the total is
    /// scrubbed on the way in rather than carried forward damaged.
    #[test]
    fn resume_scrubs_the_total_it_continues_from() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        sw.pause(&chrono);
        let first = sw.elapsed_ns(&chrono);

        sw.inject_total_flip(35);
        sw.resume();
        assert_eq!(
            sw.integrity(),
            Integrity::Clean,
            "resume left damage behind"
        );
        sw.pause(&chrono);
        assert!(sw.elapsed_ns(&chrono) >= first);
    }

    /// A recorded lap is a measurement someone kept, and it is protected on
    /// the same terms as the running total.
    #[test]
    fn laps_are_protected_too() {
        let chrono = Chronometer::new();
        let mut sw = Stopwatch::new();
        sw.start();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let recorded = sw.lap(&chrono);

        sw.laps[0].inject_flip(52);
        assert_eq!(sw.last_lap(), Some(recorded));
        assert_eq!(sw.laps().next(), Some(recorded));
        assert!(matches!(sw.verify(), Integrity::CorrectedByEcc { bit: 52 }));
    }
}
