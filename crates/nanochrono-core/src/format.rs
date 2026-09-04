// SPDX-License-Identifier: Apache-2.0
//! Time formatting.
//!
//! Two families: elapsed durations (`hh:mm:ss:mmm:uuu:nnn`, the signature
//! NanoChrono display) and wall-clock instants with zone handling.
//!
//! Date arithmetic is done here rather than through `libc::gmtime`, which is
//! not reentrant, needs `unsafe`, and silently clamps outside its range. The
//! civil-from-days algorithm below is exact for every value a `u64` of
//! nanoseconds can hold.

use crate::platform;

/// How a wall-clock instant should be rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimeZoneMode {
    /// The machine's configured zone, DST resolved for that instant.
    #[default]
    Local,
    /// UTC.
    Utc,
    /// A fixed offset in minutes east of UTC.
    CustomOffset(i32),
}

impl TimeZoneMode {
    pub const fn name(self) -> &'static str {
        match self {
            TimeZoneMode::Local => "local",
            TimeZoneMode::Utc => "utc",
            TimeZoneMode::CustomOffset(_) => "utc-offset",
        }
    }

    /// Offset from UTC in minutes for the given instant.
    pub fn offset_minutes(self, unix_ns: u64) -> i32 {
        match self {
            TimeZoneMode::Local => platform::utc_offset_minutes(unix_ns),
            TimeZoneMode::Utc => 0,
            TimeZoneMode::CustomOffset(m) => m,
        }
    }
}

/// Digit depth of the elapsed display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailMode {
    /// `mm:ss.mmm` — readable at a glance.
    Simple,
    /// `hh:mm:ss:mmm:uuu:nnn` — every digit the counter can justify.
    #[default]
    Nano,
}

impl DetailMode {
    pub const fn name(self) -> &'static str {
        match self {
            DetailMode::Simple => "simple",
            DetailMode::Nano => "nano",
        }
    }
}

/// A broken-down civil date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CivilTime {
    pub year: i64,
    /// 1-12.
    pub month: u32,
    /// 1-31.
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub nanosecond: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

const WEEKDAY_NAMES: [&str; 7] = [
    "SUNDAY",
    "MONDAY",
    "TUESDAY",
    "WEDNESDAY",
    "THURSDAY",
    "FRIDAY",
    "SATURDAY",
];

const MONTH_NAMES: [&str; 12] = [
    "JANUARY",
    "FEBRUARY",
    "MARCH",
    "APRIL",
    "MAY",
    "JUNE",
    "JULY",
    "AUGUST",
    "SEPTEMBER",
    "OCTOBER",
    "NOVEMBER",
    "DECEMBER",
];

impl CivilTime {
    pub fn weekday_name(&self) -> &'static str {
        WEEKDAY_NAMES[(self.weekday % 7) as usize]
    }

    pub fn month_name(&self) -> &'static str {
        MONTH_NAMES[((self.month.max(1) - 1) % 12) as usize]
    }
}

/// Splits Unix nanoseconds plus an offset into civil fields.
///
/// Uses Howard Hinnant's `civil_from_days`, which is branch-free and exact
/// over the whole proleptic Gregorian range.
pub fn civil_from_unix_ns(unix_ns: u64, offset_minutes: i32) -> CivilTime {
    let adjusted = unix_ns as i128 + offset_minutes as i128 * 60 * 1_000_000_000;
    let adjusted = adjusted.max(0);

    let total_secs = (adjusted / 1_000_000_000) as i64;
    let nanosecond = (adjusted % 1_000_000_000) as u32;

    let days = total_secs.div_euclid(86_400);
    let secs_of_day = total_secs.rem_euclid(86_400);

    // 1970-01-01 was a Thursday (weekday 4).
    let weekday = (days + 4).rem_euclid(7) as u32;

    // Shift the epoch to 0000-03-01 so leap days land at the end of the cycle.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if month <= 2 { y + 1 } else { y };

    CivilTime {
        year,
        month,
        day,
        hour: (secs_of_day / 3600) as u32,
        minute: ((secs_of_day / 60) % 60) as u32,
        second: (secs_of_day % 60) as u32,
        nanosecond,
        weekday,
    }
}

/// Formats an elapsed duration as `hh:mm:ss:mmm:uuu:nnn`.
///
/// Hours are not wrapped at 24: this is a stopwatch reading, not a clock.
pub fn format_elapsed_nano(ns: u64) -> String {
    let h = ns / 3_600_000_000_000;
    let m = (ns / 60_000_000_000) % 60;
    let s = (ns / 1_000_000_000) % 60;
    let ms = (ns / 1_000_000) % 1_000;
    let us = (ns / 1_000) % 1_000;
    let n = ns % 1_000;
    format!("{h:02}:{m:02}:{s:02}:{ms:03}:{us:03}:{n:03}")
}

/// Formats an elapsed duration as `mm:ss.mmm`.
pub fn format_elapsed_simple(ns: u64) -> String {
    let total_ms = ns / 1_000_000;
    let ms = total_ms % 1_000;
    let total_s = total_ms / 1_000;
    let s = total_s % 60;
    let m = total_s / 60;
    format!("{m:02}:{s:02}.{ms:03}")
}

/// Formats an elapsed duration at the requested detail.
pub fn format_elapsed(ns: u64, detail: DetailMode) -> String {
    match detail {
        DetailMode::Simple => format_elapsed_simple(ns),
        DetailMode::Nano => format_elapsed_nano(ns),
    }
}

/// Formats a Unix instant as an RFC-3339-shaped UTC timestamp with nanosecond
/// precision: `YYYY-MM-DD HH:MM:SS.nnnnnnnnnZ`.
pub fn format_unix_utc(unix_ns: u64) -> String {
    let c = civil_from_unix_ns(unix_ns, 0);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.nanosecond
    )
}

/// Formats a Unix instant in the given zone, with an explicit offset suffix.
pub fn format_unix_zoned(unix_ns: u64, mode: TimeZoneMode) -> String {
    let offset = mode.offset_minutes(unix_ns);
    let c = civil_from_unix_ns(unix_ns, offset);
    let suffix = match mode {
        TimeZoneMode::Utc => "Z".to_string(),
        _ => format_offset(offset),
    };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:09} {}",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.nanosecond, suffix
    )
}

/// Just the time-of-day part of a zoned instant.
///
/// `fractional` keeps the nanosecond digits; the big clock face uses that, the
/// title bar does not.
pub fn format_clock_face(unix_ns: u64, mode: TimeZoneMode, fractional: bool) -> String {
    let offset = mode.offset_minutes(unix_ns);
    let c = civil_from_unix_ns(unix_ns, offset);
    if fractional {
        format!(
            "{:02}:{:02}:{:02}.{:09}",
            c.hour, c.minute, c.second, c.nanosecond
        )
    } else {
        format!("{:02}:{:02}:{:02}", c.hour, c.minute, c.second)
    }
}

/// `UTC±HH:MM` for an offset in minutes.
pub fn format_offset(offset_minutes: i32) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let abs = offset_minutes.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", abs / 60, abs % 60)
}

/// `WEDNESDAY, SEPTEMBER 03, 2026` for the date line under the clock.
pub fn format_long_date(unix_ns: u64, mode: TimeZoneMode) -> String {
    let c = civil_from_unix_ns(unix_ns, mode.offset_minutes(unix_ns));
    format!(
        "{}, {} {:02}, {:04}",
        c.weekday_name(),
        c.month_name(),
        c.day,
        c.year
    )
}

/// Formats a byte count as the largest binary unit that keeps it readable.
pub fn format_bytes(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.3} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nano_format_matches_legacy_layout() {
        // 1h 2m 3s 456ms 789us 123ns
        let ns = 3_600_000_000_000 + 2 * 60_000_000_000 + 3_000_000_000 + 456_789_123;
        assert_eq!(format_elapsed_nano(ns), "01:02:03:456:789:123");
    }

    #[test]
    fn simple_format_matches_legacy_layout() {
        assert_eq!(format_elapsed_simple(83_456_000_000), "01:23.456");
    }

    #[test]
    fn epoch_is_thursday() {
        let c = civil_from_unix_ns(0, 0);
        assert_eq!((c.year, c.month, c.day), (1970, 1, 1));
        assert_eq!(c.weekday_name(), "THURSDAY");
    }

    #[test]
    fn leap_day_round_trips() {
        // 2024-02-29T12:24:56.000000789Z
        let ns = 1_709_209_496_000_000_789;
        let c = civil_from_unix_ns(ns, 0);
        assert_eq!((c.year, c.month, c.day), (2024, 2, 29));
        assert_eq!((c.hour, c.minute, c.second), (12, 24, 56));
        assert_eq!(c.nanosecond, 789);
    }

    #[test]
    fn negative_offset_shifts_backwards() {
        // Midnight UTC minus three hours is 21:00 the previous day.
        let midnight = 1_709_164_800_000_000_000;
        let c = civil_from_unix_ns(midnight, -180);
        assert_eq!(c.hour, 21);
        assert_eq!(c.day, 28);
    }

    #[test]
    fn offset_formats_with_sign_and_padding() {
        assert_eq!(format_offset(-180), "UTC-03:00");
        assert_eq!(format_offset(330), "UTC+05:30");
        assert_eq!(format_offset(0), "UTC+00:00");
    }
}
