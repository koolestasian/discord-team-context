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

    /// The UTC calendar date of this instant, or `None` outside years 1 to 9999.
    #[must_use]
    pub fn utc_date(self) -> Option<CalendarDate> {
        CalendarDate::from_days_since_epoch(self.0.div_euclid(MILLIS_PER_DAY)).ok()
    }

    /// The instant as ISO 8601 text in UTC with whole seconds, `2026-11-05T09:30:00Z`
    /// (fractions of a second are dropped). `None` outside years 1 to 9999.
    #[must_use]
    pub fn to_iso8601_utc(self) -> Option<String> {
        let date = self.utc_date()?;
        let seconds = self.0.rem_euclid(MILLIS_PER_DAY) / 1000;
        Some(format!(
            "{date}T{:02}:{:02}:{:02}Z",
            seconds / 3600,
            seconds % 3600 / 60,
            seconds % 60
        ))
    }
}

const MILLIS_PER_DAY: i64 = 86_400_000;

/// A day of the week. Weeks run Monday to Sunday (ISO 8601).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Weekday {
    /// Monday, the first day of the week.
    Monday,
    /// Tuesday.
    Tuesday,
    /// Wednesday.
    Wednesday,
    /// Thursday.
    Thursday,
    /// Friday.
    Friday,
    /// Saturday.
    Saturday,
    /// Sunday, the last day of the week.
    Sunday,
}

impl Weekday {
    /// Every day, Monday first.
    pub const ALL: [Self; 7] = [
        Self::Monday,
        Self::Tuesday,
        Self::Wednesday,
        Self::Thursday,
        Self::Friday,
        Self::Saturday,
        Self::Sunday,
    ];

    /// Days since Monday: Monday is 0, Sunday is 6.
    #[must_use]
    pub const fn index(self) -> i64 {
        self as i64
    }
}

/// A calendar date in the proleptic Gregorian calendar, without a time zone.
///
/// Used by the search syntax `before:YYYY-MM-DD` / `after:YYYY-MM-DD` (plan section 9).
/// The query service resolves midnight in `server.timezone`; core retains the date.
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

    /// Days since 1970-01-01 (negative before it).
    #[must_use]
    pub const fn days_since_epoch(self) -> i64 {
        days_from_civil(self.year, self.month, self.day)
    }

    /// The date `days` after 1970-01-01.
    ///
    /// # Errors
    ///
    /// [`DateError::Year`] if the date falls outside years 1 to 9999.
    pub fn from_days_since_epoch(days: i64) -> Result<Self, DateError> {
        // Hinnant's `civil_from_days`. The range check keeps every step small.
        if !(-719_162..=2_932_896).contains(&days) {
            return Err(DateError::Year);
        }
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let day_of_era = z - era * 146_097;
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153; // March = 0
        let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        let year = u16::try_from(year).map_err(|_| DateError::Year)?;
        let month = u8::try_from(month).map_err(|_| DateError::Month)?;
        let day = u8::try_from(day).map_err(|_| DateError::Day)?;
        Self::new(year, month, day)
    }

    /// The date `days` later (negative: earlier).
    ///
    /// # Errors
    ///
    /// [`DateError::Year`] if the result falls outside years 1 to 9999.
    pub fn add_days(self, days: i64) -> Result<Self, DateError> {
        let target = self
            .days_since_epoch()
            .checked_add(days)
            .ok_or(DateError::Year)?;
        Self::from_days_since_epoch(target)
    }

    /// The day of the week.
    #[must_use]
    pub fn weekday(self) -> Weekday {
        // 1970-01-01 was a Thursday, which is index 3 when Monday is 0.
        let index = (self.days_since_epoch() + 3).rem_euclid(7);
        Weekday::ALL[usize::try_from(index).unwrap_or(0)]
    }

    /// Midnight at the start of this date in UTC.
    ///
    /// This is a pure calendar conversion. Whether a query date means UTC midnight or
    /// midnight in the host's time zone is an open question for the query compiler.
    #[must_use]
    pub const fn start_of_day_utc(self) -> UnixMillis {
        UnixMillis::from_millis(self.days_since_epoch() * MILLIS_PER_DAY)
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
    fn day_numbers_round_trip_over_the_whole_range() {
        let first = CalendarDate::new(1, 1, 1).unwrap();
        let last = CalendarDate::new(9999, 12, 31).unwrap();
        assert_eq!(CalendarDate::from_days_since_epoch(-719_162), Ok(first));
        assert_eq!(CalendarDate::from_days_since_epoch(2_932_896), Ok(last));
        assert_eq!(first.days_since_epoch(), -719_162);
        assert_eq!(last.days_since_epoch(), 2_932_896);
        assert_eq!(
            CalendarDate::from_days_since_epoch(-719_163),
            Err(DateError::Year)
        );
        assert_eq!(
            CalendarDate::from_days_since_epoch(2_932_897),
            Err(DateError::Year)
        );
        for days in (-719_162..-719_000)
            .chain(10_000..30_000)
            .chain(2_932_000..=2_932_896)
        {
            let date = CalendarDate::from_days_since_epoch(days).unwrap();
            assert_eq!(date.days_since_epoch(), days, "{date}");
        }
    }

    #[test]
    fn add_days_crosses_month_year_and_leap_days() {
        let date = |text: &str| text.parse::<CalendarDate>().unwrap();
        assert_eq!(date("2026-11-30").add_days(1), Ok(date("2026-12-01")));
        assert_eq!(date("2026-12-31").add_days(1), Ok(date("2027-01-01")));
        assert_eq!(date("2028-02-28").add_days(1), Ok(date("2028-02-29")));
        assert_eq!(date("2027-02-28").add_days(1), Ok(date("2027-03-01")));
        assert_eq!(date("2026-01-01").add_days(-1), Ok(date("2025-12-31")));
        assert_eq!(date("9999-12-31").add_days(1), Err(DateError::Year));
        assert_eq!(date("0001-01-01").add_days(-1), Err(DateError::Year));
        assert_eq!(date("2026-01-01").add_days(i64::MAX), Err(DateError::Year));
    }

    #[test]
    fn weekdays_match_known_dates() {
        let day = |text: &str| text.parse::<CalendarDate>().unwrap().weekday();
        assert_eq!(day("1970-01-01"), Weekday::Thursday);
        assert_eq!(day("2000-01-01"), Weekday::Saturday);
        assert_eq!(day("2026-11-09"), Weekday::Monday);
        assert_eq!(day("2026-11-15"), Weekday::Sunday);
        assert_eq!(day("1969-12-31"), Weekday::Wednesday);
        assert_eq!(Weekday::Monday.index(), 0);
        assert_eq!(Weekday::Sunday.index(), 6);
    }

    #[test]
    fn instants_format_as_utc_iso_8601() {
        let at = UnixMillis::from_millis;
        assert_eq!(
            at(0).to_iso8601_utc().as_deref(),
            Some("1970-01-01T00:00:00Z")
        );
        assert_eq!(
            at(1_793_836_800_000 + 12_345_678)
                .to_iso8601_utc()
                .as_deref(),
            Some("2026-11-05T03:25:45Z")
        );
        assert_eq!(
            at(-1).to_iso8601_utc().as_deref(),
            Some("1969-12-31T23:59:59Z")
        );
        assert_eq!(at(0).utc_date(), "1970-01-01".parse().ok());
        assert_eq!(at(i64::MAX).to_iso8601_utc(), None);
        assert_eq!(at(i64::MIN).to_iso8601_utc(), None);
    }

    #[test]
    fn dates_order_chronologically() {
        let a: CalendarDate = "2026-01-31".parse().unwrap();
        let b: CalendarDate = "2026-02-01".parse().unwrap();
        assert!(a < b);
        assert!(a.start_of_day_utc() < b.start_of_day_utc());
    }
}
