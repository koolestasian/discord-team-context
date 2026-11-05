//! Time values. Core never reads a clock: callers pass the current time in.
//!
//! Plan section 5.2: all timestamps are stored as UTC Unix milliseconds.

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

/// A UTC instant as milliseconds since 1970-01-01T00:00:00Z (plan section 5.2).
///
/// Ordering is chronological. Negative values (before 1970) are representable but never
/// produced by Discord snowflakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixMillis(i64);

impl UnixMillis {
    /// Wraps a millisecond count.
    #[must_use]
    pub const fn from_millis(millis: i64) -> Self {
        Self(millis)
    }

    /// The millisecond count, for binding as an SQL parameter.
    #[must_use]
    pub const fn as_millis(self) -> i64 {
        self.0
    }
}

/// A calendar date in the proleptic Gregorian calendar, without a time zone.
///
/// Used by the search syntax `before:YYYY-MM-DD` / `after:YYYY-MM-DD` (plan section 9).
/// Which time zone turns a date into an instant is not decided here; see
/// `docs/design/contracts.md`, open questions.
///
/// Invariants: `1 <= year <= 9999` (four digits), `1 <= month <= 12`, and `day` exists
/// in that month (leap years included). The text form is exactly `YYYY-MM-DD`, and
/// `FromStr` and `Display` are inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CalendarDate {
    year: u16,
    month: u8,
    day: u8,
}

/// Why a date is invalid or not in `YYYY-MM-DD` form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum DateError {
    /// The text is not exactly four digits, `-`, two digits, `-`, two digits.
    #[error("date must look like YYYY-MM-DD")]
    Format,
    /// Year 0 is outside the supported range 1..=9999.
    #[error("year must be between 1 and 9999")]
    Year,
    /// The month is not 1..=12.
    #[error("month must be between 1 and 12")]
    Month,
    /// The day does not exist in that month.
    #[error("day does not exist in that month")]
    Day,
}

impl CalendarDate {
    /// Builds a date, checking that it exists.
    ///
    /// # Errors
    ///
    /// [`DateError::Year`], [`DateError::Month`] or [`DateError::Day`] when the date
    /// does not exist.
    pub const fn new(year: u16, month: u8, day: u8) -> Result<Self, DateError> {
        if year == 0 || year > 9999 {
            return Err(DateError::Year);
        }
        if month == 0 || month > 12 {
            return Err(DateError::Month);
        }
        if day == 0 || day > days_in_month(year, month) {
            return Err(DateError::Day);
        }
        Ok(Self { year, month, day })
    }

    /// The year, 1..=9999.
    #[must_use]
    pub const fn year(self) -> u16 {
        self.year
    }

    /// The month, 1..=12.
    #[must_use]
    pub const fn month(self) -> u8 {
        self.month
    }

    /// The day of the month, starting at 1.
    #[must_use]
    pub const fn day(self) -> u8 {
        self.day
    }

    /// Midnight at the start of this date in UTC.
    ///
    /// This is a pure calendar conversion. Whether a query date means UTC midnight or
    /// midnight in the host's time zone is an open question for the query compiler.
    #[must_use]
    pub const fn start_of_day_utc(self) -> UnixMillis {
        UnixMillis::from_millis(days_from_civil(self.year, self.month, self.day) * 86_400_000)
    }
}

impl fmt::Display for CalendarDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

impl FromStr for CalendarDate {
    type Err = DateError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let bytes = s.as_bytes();
        let shape_ok = bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| i == 4 || i == 7 || b.is_ascii_digit());
        if !shape_ok {
            return Err(DateError::Format);
        }
        let number = |range: std::ops::Range<usize>| -> u16 {
            bytes[range]
                .iter()
                .fold(0_u16, |acc, b| acc * 10 + u16::from(b - b'0'))
        };
        let month = u8::try_from(number(5..7)).map_err(|_| DateError::Month)?;
        let day = u8::try_from(number(8..10)).map_err(|_| DateError::Day)?;
        Self::new(number(0..4), month, day)
    }
}

const fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
}

const fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a valid date (Howard Hinnant's `days_from_civil`).
const fn days_from_civil(year: u16, month: u8, day: u8) -> i64 {
    let month = month as i64;
    let day = day as i64;
    let year = year as i64 - if month <= 2 { 1 } else { 0 };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12; // March = 0
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_text_round_trips() {
        for text in [
            "0001-01-01",
            "1970-01-01",
            "2024-02-29",
            "2026-11-05",
            "9999-12-31",
        ] {
            let date: CalendarDate = text.parse().unwrap();
            assert_eq!(date.to_string(), text);
        }
    }

    #[test]
    fn date_rejects_bad_text() {
        let cases = [
            ("", DateError::Format),
            ("2026-1-05", DateError::Format),
            ("2026/11/05", DateError::Format),
            ("2026-11-05T00:00", DateError::Format),
            (" 2026-11-05", DateError::Format),
            ("+026-11-05", DateError::Format),
            ("0000-01-01", DateError::Year),
            ("2026-00-10", DateError::Month),
            ("2026-13-10", DateError::Month),
            ("2026-11-00", DateError::Day),
            ("2026-11-31", DateError::Day),
            ("2025-02-29", DateError::Day),
            ("1900-02-29", DateError::Day), // divisible by 100, not a leap year
        ];
        for (input, expected) in cases {
            assert_eq!(
                input.parse::<CalendarDate>(),
                Err(expected),
                "input {input:?}"
            );
        }
        // Divisible by 400, so a leap year.
        assert!("2000-02-29".parse::<CalendarDate>().is_ok());
    }

    #[test]
    fn start_of_day_utc_matches_known_instants() {
        let cases = [
            ("1970-01-01", 0),
            ("1969-12-31", -86_400_000),
            ("2000-03-01", 951_868_800_000),
            ("2015-01-01", crate::id::DISCORD_EPOCH_MS),
            ("2026-11-05", 1_793_836_800_000),
        ];
        for (text, millis) in cases {
            let date: CalendarDate = text.parse().unwrap();
            assert_eq!(date.start_of_day_utc().as_millis(), millis, "{text}");
        }
    }

    #[test]
    fn dates_order_chronologically() {
        let a: CalendarDate = "2026-01-31".parse().unwrap();
        let b: CalendarDate = "2026-02-01".parse().unwrap();
        assert!(a < b);
        assert!(a.start_of_day_utc() < b.start_of_day_utc());
    }
}
