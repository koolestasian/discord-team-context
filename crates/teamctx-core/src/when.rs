//! Due dates and "since" windows typed by people (plan section 8, "Dates").
//!
//! A small parser with no dependency and no clock: the caller passes the current time
//! and a [`Zone`] that knows the host's configured timezone (`server.timezone`). Storage
//! never guesses a timezone, and neither does this module.
//!
//! Two entry points, each with its own vocabulary:
//!
//! | Phrase | [`parse_due`] (a day in the future) | [`parse_since`] (an instant in the past) |
//! |---|---|---|
//! | `today`, `tomorrow` | yes | `today` (midnight), `yesterday` (midnight) |
//! | `friday`, `fri` | the next such day after today | no |
//! | `next friday` | that weekday in next week (Monday to Sunday weeks) | no |
//! | `2026-11-20` | yes (not in the past) | midnight of that day (not in the future) |
//! | `nov 20`, `20 nov`, `november 20th 2027` | next occurrence, or the given year | no |
//! | `3d`, `2w`, `in 3 days` | that many days from today | `3d`, `2w`: that long before now |
//! | `24h` | rejected: due dates are whole days | `24h`: that long before now |
//!
//! Input is trimmed, lower-cased and split on whitespace, so `  Next   FRIDAY ` works.
//! Anything else is an error that names examples. Ambiguity is never guessed:
//! `11/20` and `03-04-2026` (day first or month first?) and a bare weekday on that same
//! weekday (today or next week?) are rejected with a hint. The text of the input is never
//! echoed in an error: it is untrusted.
//!
//! A due date resolves to midnight at the start of that day in the host's timezone, and
//! the resolved [`DueDate::date`] is what callers echo back for confirmation.

use thiserror::Error;

use crate::time::{CalendarDate, DateError, UnixMillis, Weekday};

/// The longest phrase accepted, in bytes. Longer input is rejected unread.
pub const MAX_PHRASE_BYTES: usize = 64;

const MILLIS_PER_HOUR: i64 = 3_600_000;
/// The largest amount in `Nd`, `Nw` and `Nh`.
const MAX_AMOUNT: i64 = 99_999;

const DUE_EXAMPLES: &str = "tomorrow, friday, next friday, nov 20, 2026-11-20 or 3d";
const SINCE_EXAMPLES: &str = "24h, 7d, 2w, yesterday or 2026-11-01";

/// The host's timezone, supplied by the caller. Core has no timezone database.
pub trait Zone {
    /// The calendar date at `at` in this zone.
    ///
    /// # Errors
    ///
    /// [`ZoneError`] if the zone cannot resolve that instant.
    fn local_date(&self, at: UnixMillis) -> Result<CalendarDate, ZoneError>;

    /// The instant at which `date` starts (local midnight) in this zone.
    ///
    /// # Errors
    ///
    /// [`ZoneError`] if the zone cannot resolve that date.
    fn start_of_day(&self, date: CalendarDate) -> Result<UnixMillis, ZoneError>;
}

/// A [`Zone`] could not resolve a date or instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("the configured timezone cannot resolve that date")]
pub struct ZoneError;

/// A fixed offset from UTC, for hosts whose `server.timezone` is `UTC` or `+02:00`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedOffset {
    seconds: i32,
}

impl FixedOffset {
    /// UTC.
    pub const UTC: Self = Self { seconds: 0 };

    /// An offset in seconds east of UTC, at most 14 hours either way.
    #[must_use]
    pub const fn from_seconds(seconds: i32) -> Option<Self> {
        if seconds >= -14 * 3600 && seconds <= 14 * 3600 {
            Some(Self { seconds })
        } else {
            None
        }
    }

    const fn millis(self) -> i64 {
        self.seconds as i64 * 1000
    }
}

impl Zone for FixedOffset {
    fn local_date(&self, at: UnixMillis) -> Result<CalendarDate, ZoneError> {
        let local = at.as_millis().checked_add(self.millis()).ok_or(ZoneError)?;
        UnixMillis::from_millis(local).utc_date().ok_or(ZoneError)
    }

    fn start_of_day(&self, date: CalendarDate) -> Result<UnixMillis, ZoneError> {
        date.start_of_day_utc()
            .as_millis()
            .checked_sub(self.millis())
            .map(UnixMillis::from_millis)
            .ok_or(ZoneError)
    }
}

/// A resolved due date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueDate {
    /// The calendar day in the host's timezone. Echo this back for confirmation.
    pub date: CalendarDate,
    /// The instant that day starts in the host's timezone; this is what a task stores
    /// in `due_at`.
    pub at: UnixMillis,
}

/// What made a phrase ambiguous.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ambiguity {
    /// A numeric date that is not `YYYY-MM-DD`: `11/20` or `03-04-2026` could be day
    /// first or month first.
    NumericDate,
    /// A bare weekday on that same weekday: today, or in a week?
    SameWeekday,
}

/// Why a phrase is not a date or window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WhenError {
    /// The phrase is empty or whitespace.
    #[error("give a date, for example {DUE_EXAMPLES}")]
    Empty,
    /// The phrase is longer than [`MAX_PHRASE_BYTES`].
    #[error("that is too long to be a date")]
    TooLong,
    /// The phrase is not in the vocabulary of a due date.
    #[error("not a due date I understand; try {DUE_EXAMPLES}")]
    UnrecognisedDue,
    /// The phrase is not in the vocabulary of a "since" window.
    #[error("not a time I understand; try {SINCE_EXAMPLES}")]
    UnrecognisedSince,
    /// The phrase could mean two dates and is not guessed.
    #[error("{}", .0.hint())]
    Ambiguous(Ambiguity),
    /// A due date that is before today.
    #[error("that date is in the past; due dates start today")]
    InPast,
    /// A "since" time that is after today.
    #[error("that date is in the future")]
    InFuture,
    /// An amount of zero, or more than 99999.
    #[error("the amount must be between 1 and 99999")]
    Amount,
    /// Hours for a due date.
    #[error("due dates are whole days; use 1d, tomorrow or a date")]
    HoursForDue,
    /// The date does not exist, for example 30 February.
    #[error(transparent)]
    Date(#[from] DateError),
    /// The timezone could not resolve the date.
    #[error(transparent)]
    Zone(#[from] ZoneError),
}

impl Ambiguity {
    const fn hint(self) -> &'static str {
        match self {
            Self::NumericDate => {
                "that numeric date could be day first or month first; write it as 2026-11-20 or nov 20"
            }
            Self::SameWeekday => {
                "today is that weekday: say 'today' for today or 'next <weekday>' for a week later"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    Hours,
    Days,
    Weeks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phrase {
    Today,
    Tomorrow,
    Yesterday,
    Weekday(Weekday),
    NextWeekday(Weekday),
    Iso(CalendarDate),
    MonthDay {
        month: u8,
        day: u8,
        year: Option<u16>,
    },
    /// `3d`: a span, counted forward from today for a due date, back from now for a window.
    Relative {
        amount: i64,
        unit: Unit,
    },
    /// `in 3 days`: forward only.
    In {
        amount: i64,
        unit: Unit,
    },
}

/// Resolves a due-date phrase (see the module table) against `now` in `zone`.
///
/// # Errors
///
/// A [`WhenError`] naming what is wrong; see its variants.
pub fn parse_due(text: &str, now: UnixMillis, zone: &impl Zone) -> Result<DueDate, WhenError> {
    let phrase = classify(text, WhenError::UnrecognisedDue)?;
    let today = zone.local_date(now)?;
    let date = match phrase {
        Phrase::Today => today,
        Phrase::Tomorrow => today.add_days(1)?,
        Phrase::Weekday(day) => {
            let delta = (day.index() - today.weekday().index()).rem_euclid(7);
            if delta == 0 {
                return Err(WhenError::Ambiguous(Ambiguity::SameWeekday));
            }
            today.add_days(delta)?
        }
        Phrase::NextWeekday(day) => {
            let monday_next_week = today.add_days(7 - today.weekday().index())?;
            monday_next_week.add_days(day.index())?
        }
        Phrase::Iso(date) => date,
        Phrase::MonthDay {
            month,
            day,
            year: Some(year),
        } => CalendarDate::new(year, month, day)?,
        Phrase::MonthDay {
            month,
            day,
            year: None,
        } => next_occurrence(today, month, day)?,
        Phrase::Relative {
            amount,
            unit: Unit::Days,
        }
        | Phrase::In {
            amount,
            unit: Unit::Days,
        } => today.add_days(amount)?,
        Phrase::Relative {
            amount,
            unit: Unit::Weeks,
        }
        | Phrase::In {
            amount,
            unit: Unit::Weeks,
        } => today.add_days(amount * 7)?,
        Phrase::Relative {
            unit: Unit::Hours, ..
        }
        | Phrase::In {
            unit: Unit::Hours, ..
        } => return Err(WhenError::HoursForDue),
        Phrase::Yesterday => return Err(WhenError::UnrecognisedDue),
    };
    if date < today {
        return Err(WhenError::InPast);
    }
    Ok(DueDate {
        date,
        at: zone.start_of_day(date)?,
    })
}

/// Resolves a "since" phrase (see the module table) against `now` in `zone` to the
/// instant the window starts.
///
/// # Errors
///
/// A [`WhenError`] naming what is wrong; see its variants.
pub fn parse_since(text: &str, now: UnixMillis, zone: &impl Zone) -> Result<UnixMillis, WhenError> {
    let phrase = classify(text, WhenError::UnrecognisedSince)?;
    if let Phrase::Relative { amount, unit } = phrase {
        // A window measured back from now does not depend on the timezone.
        let hours = match unit {
            Unit::Hours => amount,
            Unit::Days => amount * 24,
            Unit::Weeks => amount * 24 * 7,
        };
        return now
            .as_millis()
            .checked_sub(hours * MILLIS_PER_HOUR)
            .map(UnixMillis::from_millis)
            .ok_or(WhenError::Amount);
    }
    let today = zone.local_date(now)?;
    let date = match phrase {
        Phrase::Today => today,
        Phrase::Yesterday => today.add_days(-1)?,
        Phrase::Iso(date) => date,
        Phrase::Relative { .. }
        | Phrase::In { .. }
        | Phrase::Tomorrow
        | Phrase::Weekday(_)
        | Phrase::NextWeekday(_)
        | Phrase::MonthDay { .. } => return Err(WhenError::UnrecognisedSince),
    };
    if date > today {
        return Err(WhenError::InFuture);
    }
    Ok(zone.start_of_day(date)?)
}

/// The first `month`/`day` on or after `today`, trying the following years when the day
/// does not exist (29 February).
fn next_occurrence(today: CalendarDate, month: u8, day: u8) -> Result<CalendarDate, WhenError> {
    let mut last_error = DateError::Day;
    for year in today.year()..=today.year().saturating_add(8) {
        match CalendarDate::new(year, month, day) {
            Ok(date) if date >= today => return Ok(date),
            Ok(_) => {}
            Err(error) => last_error = error,
        }
    }
    Err(WhenError::Date(last_error))
}

fn classify(text: &str, unrecognised: WhenError) -> Result<Phrase, WhenError> {
    if text.len() > MAX_PHRASE_BYTES {
        return Err(WhenError::TooLong);
    }
    let lowered = text.to_ascii_lowercase();
    let tokens: Vec<&str> = lowered
        .split_whitespace()
        .map(|token| token.trim_end_matches(','))
        .collect();
    match tokens.as_slice() {
        [] => Err(WhenError::Empty),
        [one] => single(one, unrecognised),
        ["next", day] => weekday(day).map(Phrase::NextWeekday).ok_or(unrecognised),
        ["in", amount, unit] => relative_words(amount, unit).unwrap_or(Err(unrecognised)),
        [a, b] => month_day(a, b, None).ok_or(unrecognised),
        [a, b, c] => {
            let year = year(c).ok_or(unrecognised)?;
            month_day(a, b, Some(year)).ok_or(unrecognised)
        }
        _ => Err(unrecognised),
    }
}

fn single(token: &str, unrecognised: WhenError) -> Result<Phrase, WhenError> {
    match token {
        "today" => return Ok(Phrase::Today),
        "tomorrow" => return Ok(Phrase::Tomorrow),
        "yesterday" => return Ok(Phrase::Yesterday),
        _ => {}
    }
    if let Some(day) = weekday(token) {
        return Ok(Phrase::Weekday(day));
    }
    if let Some(relative) = relative_compact(token) {
        return relative;
    }
    let numeric = token
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'/' | b'.' | b'-'));
    let separators = token
        .bytes()
        .filter(|b| matches!(b, b'/' | b'.' | b'-'))
        .count();
    if numeric && separators > 0 && token.bytes().any(|b| b.is_ascii_digit()) {
        return match token.parse::<CalendarDate>() {
            Ok(date) => Ok(Phrase::Iso(date)),
            Err(DateError::Format) => Err(WhenError::Ambiguous(Ambiguity::NumericDate)),
            Err(error) => Err(WhenError::Date(error)),
        };
    }
    Err(unrecognised)
}

/// `3d`, `2w`, `24h`.
fn relative_compact(token: &str) -> Option<Result<Phrase, WhenError>> {
    let unit = match token.as_bytes().last()? {
        b'h' => Unit::Hours,
        b'd' => Unit::Days,
        b'w' => Unit::Weeks,
        _ => return None,
    };
    let digits = &token[..token.len() - 1];
    amount(digits).map(|amount| amount.map(|amount| Phrase::Relative { amount, unit }))
}

/// `in 3 days`, `in 1 week`, `in 12 hours`.
fn relative_words(amount_text: &str, unit_text: &str) -> Option<Result<Phrase, WhenError>> {
    let unit = match unit_text {
        "hour" | "hours" => Unit::Hours,
        "day" | "days" => Unit::Days,
        "week" | "weeks" => Unit::Weeks,
        _ => return None,
    };
    amount(amount_text).map(|amount| amount.map(|amount| Phrase::In { amount, unit }))
}

/// Digits only: `None` if the text is not a number, `Err` if it is out of range.
fn amount(digits: &str) -> Option<Result<i64, WhenError>> {
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(match digits.parse::<i64>() {
        Ok(value) if (1..=MAX_AMOUNT).contains(&value) => Ok(value),
        _ => Err(WhenError::Amount),
    })
}

fn weekday(token: &str) -> Option<Weekday> {
    Some(match token {
        "mon" | "monday" => Weekday::Monday,
        "tue" | "tues" | "tuesday" => Weekday::Tuesday,
        "wed" | "weds" | "wednesday" => Weekday::Wednesday,
        "thu" | "thur" | "thurs" | "thursday" => Weekday::Thursday,
        "fri" | "friday" => Weekday::Friday,
        "sat" | "saturday" => Weekday::Saturday,
        "sun" | "sunday" => Weekday::Sunday,
        _ => return None,
    })
}

fn month(token: &str) -> Option<u8> {
    Some(match token {
        "jan" | "january" => 1,
        "feb" | "february" => 2,
        "mar" | "march" => 3,
        "apr" | "april" => 4,
        "may" => 5,
        "jun" | "june" => 6,
        "jul" | "july" => 7,
        "aug" | "august" => 8,
        "sep" | "sept" | "september" => 9,
        "oct" | "october" => 10,
        "nov" | "november" => 11,
        "dec" | "december" => 12,
        _ => return None,
    })
}

/// A day of the month with an optional ordinal suffix: `20`, `1st`, `22nd`, `3rd`, `4th`.
fn day_of_month(token: &str) -> Option<u8> {
    let digits = token
        .strip_suffix("st")
        .or_else(|| token.strip_suffix("nd"))
        .or_else(|| token.strip_suffix("rd"))
        .or_else(|| token.strip_suffix("th"))
        .unwrap_or(token);
    if digits.is_empty() || digits.len() > 2 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn year(token: &str) -> Option<u16> {
    if token.len() == 4 && token.bytes().all(|b| b.is_ascii_digit()) {
        token.parse().ok()
    } else {
        None
    }
}

/// `nov 20` or `20 nov`. An impossible day (`feb 30`) is kept as written so the date
/// check reports it instead of the phrase looking unrecognised.
fn month_day(first: &str, second: &str, year: Option<u16>) -> Option<Phrase> {
    let (month, day) = match (month(first), month(second)) {
        (Some(month), None) => (month, day_of_month(second)?),
        (None, Some(month)) => (month, day_of_month(first)?),
        _ => return None,
    };
    Some(Phrase::MonthDay { month, day, year })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(text: &str) -> CalendarDate {
        text.parse().unwrap()
    }

    /// Monday 2026-11-09 at 10:00 UTC.
    fn now() -> UnixMillis {
        UnixMillis::from_millis(date("2026-11-09").start_of_day_utc().as_millis() + 10 * 3_600_000)
    }

    fn at(offset_hours: i32) -> FixedOffset {
        FixedOffset::from_seconds(offset_hours * 3600).unwrap()
    }

    fn due(text: &str) -> Result<String, WhenError> {
        parse_due(text, now(), &FixedOffset::UTC).map(|due| due.date.to_string())
    }

    fn since(text: &str) -> Result<i64, WhenError> {
        parse_since(text, now(), &FixedOffset::UTC).map(UnixMillis::as_millis)
    }

    #[test]
    fn due_phrases_resolve_to_a_day() {
        // 2026-11-09 is a Monday.
        let cases = [
            ("today", "2026-11-09"),
            ("tomorrow", "2026-11-10"),
            ("TOMORROW", "2026-11-10"),
            ("  Tomorrow  ", "2026-11-10"),
            ("friday", "2026-11-13"),
            ("fri", "2026-11-13"),
            ("tuesday", "2026-11-10"),
            ("sunday", "2026-11-15"),
            ("next monday", "2026-11-16"),
            ("next friday", "2026-11-20"),
            ("next   Friday", "2026-11-20"),
            ("next sun", "2026-11-22"),
            ("2026-11-20", "2026-11-20"),
            ("2026-11-09", "2026-11-09"),
            ("2028-02-29", "2028-02-29"),
            ("3d", "2026-11-12"),
            ("1d", "2026-11-10"),
            ("2w", "2026-11-23"),
            ("in 3 days", "2026-11-12"),
            ("in 1 week", "2026-11-16"),
            ("nov 20", "2026-11-20"),
            ("november 20", "2026-11-20"),
            ("20 nov", "2026-11-20"),
            ("Nov 20th", "2026-11-20"),
            ("nov 9", "2026-11-09"),
            ("nov 8", "2027-11-08"),
            ("jan 5", "2027-01-05"),
            ("dec 31, 2026", "2026-12-31"),
            ("20 nov 2027", "2027-11-20"),
            ("sept 1st", "2027-09-01"),
            ("feb 29", "2028-02-29"),
        ];
        for (input, expected) in cases {
            assert_eq!(due(input).as_deref(), Ok(expected), "{input:?}");
        }
    }

    #[test]
    fn weekday_rules_cover_every_day_of_the_week() {
        // From each weekday of 2026-11-09 (Mon) to 2026-11-15 (Sun), both forms of Friday.
        let expected = [
            // (today, bare friday, next friday)
            ("2026-11-09", Ok("2026-11-13"), "2026-11-20"),
            ("2026-11-10", Ok("2026-11-13"), "2026-11-20"),
            ("2026-11-11", Ok("2026-11-13"), "2026-11-20"),
            ("2026-11-12", Ok("2026-11-13"), "2026-11-20"),
            (
                "2026-11-13",
                Err(WhenError::Ambiguous(Ambiguity::SameWeekday)),
                "2026-11-20",
            ),
            ("2026-11-14", Ok("2026-11-20"), "2026-11-20"),
            ("2026-11-15", Ok("2026-11-20"), "2026-11-20"),
        ];
        for (today, bare, next) in expected {
            let now = date(today).start_of_day_utc();
            let zone = FixedOffset::UTC;
            let got = parse_due("friday", now, &zone).map(|d| d.date.to_string());
            assert_eq!(got, bare.map(str::to_owned), "friday on {today}");
            let got = parse_due("next friday", now, &zone).map(|d| d.date.to_string());
            assert_eq!(got.as_deref(), Ok(next), "next friday on {today}");
        }
    }

    #[test]
    fn due_resolves_to_local_midnight_and_the_local_day() {
        // 2026-11-09 20:00 UTC is already 2026-11-10 09:00 in +13:00, so "today" is the
        // 10th there and "tomorrow" is the 11th, starting at 2026-11-10 11:00 UTC.
        let late = UnixMillis::from_millis(
            date("2026-11-09").start_of_day_utc().as_millis() + 20 * 3_600_000,
        );
        let plus13 = at(13);
        let due = parse_due("tomorrow", late, &plus13).unwrap();
        assert_eq!(due.date, date("2026-11-11"));
        assert_eq!(
            due.at.as_millis(),
            date("2026-11-11").start_of_day_utc().as_millis() - 13 * 3_600_000
        );
        assert_eq!(
            parse_due("today", late, &plus13).unwrap().date,
            date("2026-11-10")
        );
        // The same instant in UTC and in -08:00 (12:00 local on the 9th).
        assert_eq!(
            parse_due("today", late, &FixedOffset::UTC).unwrap().date,
            date("2026-11-09")
        );
        let minus8 = at(-8);
        let due = parse_due("friday", late, &minus8).unwrap();
        assert_eq!(due.date, date("2026-11-13"));
        assert_eq!(
            due.at.as_millis(),
            date("2026-11-13").start_of_day_utc().as_millis() + 8 * 3_600_000
        );
    }

    #[test]
    fn due_in_utc_stores_midnight_utc() {
        let due = parse_due("2026-11-20", now(), &FixedOffset::UTC).unwrap();
        assert_eq!(due.at, date("2026-11-20").start_of_day_utc());
    }

    #[test]
    fn ambiguous_phrases_are_rejected_with_a_hint() {
        let numeric = WhenError::Ambiguous(Ambiguity::NumericDate);
        for input in [
            "11/20",
            "03/04/2026",
            "03-04-2026",
            "20.11.2026",
            "2026-1-5",
            "1/2",
        ] {
            assert_eq!(due(input), Err(numeric), "{input:?}");
            assert_eq!(since(input), Err(numeric), "{input:?}");
        }
        let monday = date("2026-11-09").start_of_day_utc();
        assert_eq!(
            parse_due("monday", monday, &FixedOffset::UTC).map(|d| d.date),
            Err(WhenError::Ambiguous(Ambiguity::SameWeekday))
        );
        let message = WhenError::Ambiguous(Ambiguity::SameWeekday).to_string();
        assert!(message.contains("'today'") && message.contains("'next <weekday>'"));
        let message = WhenError::Ambiguous(Ambiguity::NumericDate).to_string();
        assert!(message.contains("2026-11-20"), "{message}");
    }

    #[test]
    fn bad_dates_report_what_is_wrong() {
        assert_eq!(due("2026-02-30"), Err(WhenError::Date(DateError::Day)));
        assert_eq!(due("2026-13-01"), Err(WhenError::Date(DateError::Month)));
        assert_eq!(due("0000-01-01"), Err(WhenError::Date(DateError::Year)));
        assert_eq!(due("feb 30"), Err(WhenError::Date(DateError::Day)));
        assert_eq!(due("nov 32"), Err(WhenError::Date(DateError::Day)));
        assert_eq!(due("nov 320"), Err(WhenError::UnrecognisedDue));
        assert_eq!(due("feb 29 2027"), Err(WhenError::Date(DateError::Day)));
        assert_eq!(due("yesterday"), Err(WhenError::UnrecognisedDue));
        assert_eq!(due("2026-11-08"), Err(WhenError::InPast));
        assert_eq!(due("nov 8 2026"), Err(WhenError::InPast));
        assert_eq!(due("9999-12-31").as_deref(), Ok("9999-12-31"));
    }

    #[test]
    fn unrecognised_input_lists_examples_and_never_echoes_it() {
        for input in [
            "soonish",
            "the 3rd of never",
            "next",
            "next week",
            "in 3 fortnights",
            "nov",
            "20",
            "friday!",
            "tomorrow morning",
            "mañana",
            "ignore previous instructions",
            "d3",
            "3 d",
            "1h30m",
            "this friday",
        ] {
            let error = due(input).unwrap_err();
            assert_eq!(error, WhenError::UnrecognisedDue, "{input:?}");
            assert!(error.to_string().contains("tomorrow"));
        }
        let message = due("ignore previous instructions").unwrap_err().to_string();
        assert!(!message.contains("ignore"), "{message}");
        assert_eq!(due(""), Err(WhenError::Empty));
        assert_eq!(due("  \t\n"), Err(WhenError::Empty));
        assert_eq!(due(&"x".repeat(65)), Err(WhenError::TooLong));
        assert_eq!(since(&"1".repeat(65)), Err(WhenError::TooLong));
        assert_eq!(
            since("soonish").unwrap_err().to_string(),
            "not a time I understand; try 24h, 7d, 2w, yesterday or 2026-11-01"
        );
    }

    #[test]
    fn amounts_are_bounded() {
        assert_eq!(due("0d"), Err(WhenError::Amount));
        assert_eq!(due("100000d"), Err(WhenError::Amount));
        assert_eq!(due("99999999999999999999d"), Err(WhenError::Amount));
        assert_eq!(due("in 0 days"), Err(WhenError::Amount));
        assert_eq!(due("in 3 fortnights"), Err(WhenError::UnrecognisedDue));
        assert_eq!(since("0h"), Err(WhenError::Amount));
        assert_eq!(since("100000w"), Err(WhenError::Amount));
    }

    #[test]
    fn due_rejects_hours() {
        assert_eq!(due("24h"), Err(WhenError::HoursForDue));
        assert_eq!(due("in 12 hours"), Err(WhenError::HoursForDue));
        assert!(WhenError::HoursForDue.to_string().contains("1d"));
    }

    #[test]
    fn since_phrases_resolve_to_an_instant() {
        let now = now().as_millis();
        let day = date("2026-11-09").start_of_day_utc().as_millis();
        let cases = [
            ("24h", now - 24 * 3_600_000),
            ("1h", now - 3_600_000),
            ("3d", now - 72 * 3_600_000),
            ("7d", now - 7 * 24 * 3_600_000),
            ("2w", now - 14 * 24 * 3_600_000),
            ("2W", now - 14 * 24 * 3_600_000),
            ("today", day),
            ("yesterday", day - 86_400_000),
            (
                "2026-11-01",
                date("2026-11-01").start_of_day_utc().as_millis(),
            ),
            ("2026-11-09", day),
        ];
        for (input, expected) in cases {
            assert_eq!(since(input), Ok(expected), "{input:?}");
        }
    }

    #[test]
    fn since_rejects_the_future_and_due_only_phrases() {
        assert_eq!(since("2026-11-10"), Err(WhenError::InFuture));
        for input in ["tomorrow", "friday", "next friday", "nov 20", "in 3 days"] {
            assert_eq!(since(input), Err(WhenError::UnrecognisedSince), "{input:?}");
        }
    }

    #[test]
    fn since_dates_use_the_host_timezone() {
        let zone = at(2);
        let got = parse_since("2026-11-01", now(), &zone).unwrap();
        assert_eq!(
            got.as_millis(),
            date("2026-11-01").start_of_day_utc().as_millis() - 2 * 3_600_000
        );
        // 23:30 UTC on the 9th is already the 10th at +02:00, so the 10th is "today".
        let late = UnixMillis::from_millis(
            date("2026-11-09").start_of_day_utc().as_millis() + 23 * 3_600_000 + 1_800_000,
        );
        assert!(parse_since("2026-11-10", late, &zone).is_ok());
        assert_eq!(
            parse_since("2026-11-10", late, &FixedOffset::UTC),
            Err(WhenError::InFuture)
        );
    }

    #[test]
    fn relative_since_ignores_the_zone() {
        let a = parse_since("24h", now(), &at(-8)).unwrap();
        let b = parse_since("24h", now(), &at(13)).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn zone_failures_surface() {
        struct Broken;
        impl Zone for Broken {
            fn local_date(&self, _: UnixMillis) -> Result<CalendarDate, ZoneError> {
                Err(ZoneError)
            }
            fn start_of_day(&self, _: CalendarDate) -> Result<UnixMillis, ZoneError> {
                Err(ZoneError)
            }
        }
        assert_eq!(
            parse_due("tomorrow", now(), &Broken),
            Err(WhenError::Zone(ZoneError))
        );
        assert_eq!(
            parse_since("today", now(), &Broken),
            Err(WhenError::Zone(ZoneError))
        );
        // A window measured back from now needs no timezone.
        assert_eq!(
            parse_since("24h", now(), &Broken),
            Ok(UnixMillis::from_millis(now().as_millis() - 86_400_000))
        );
    }

    #[test]
    fn fixed_offsets_are_limited_to_fourteen_hours() {
        assert!(FixedOffset::from_seconds(14 * 3600).is_some());
        assert!(FixedOffset::from_seconds(-14 * 3600).is_some());
        assert!(FixedOffset::from_seconds(14 * 3600 + 1).is_none());
        assert!(FixedOffset::from_seconds(-14 * 3600 - 1).is_none());
    }

    #[test]
    fn year_boundaries_do_not_overflow() {
        let end = date("9999-12-31").start_of_day_utc();
        assert_eq!(
            parse_due("tomorrow", end, &FixedOffset::UTC).map(drop),
            Err(WhenError::Date(DateError::Year))
        );
        let start = date("0001-01-01").start_of_day_utc();
        assert_eq!(
            parse_since("yesterday", start, &FixedOffset::UTC),
            Err(WhenError::Date(DateError::Year))
        );
        assert_eq!(
            parse_since(
                "1h",
                UnixMillis::from_millis(i64::MIN + 1),
                &FixedOffset::UTC
            ),
            Err(WhenError::Amount)
        );
    }
}
