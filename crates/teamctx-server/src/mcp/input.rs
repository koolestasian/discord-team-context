//! Parsing of tool arguments that need more than serde: `since`, cursors and ids.
//!
//! All of it is untrusted input. Errors are fixed strings that never echo the input.

use teamctx_core::{
    citation::CitationId,
    time::{CalendarDate, UnixMillis},
};

const MILLIS_PER_HOUR: i64 = 3_600_000;
const MILLIS_PER_DAY: i64 = 86_400_000;
/// The longest relative window `since` accepts: one year.
const MAX_HOURS: i64 = 24 * 365;
/// The most results a cursor can reach, so a page request has a bounded cost.
pub(crate) const MAX_CURSOR_OFFSET: u32 = 100;

/// The `since` message shown to the agent when parsing fails.
pub(crate) const SINCE_HELP: &str =
    "`since` must be a date like 2026-04-01 or a window like 24h or 7d (at most 365d).";

/// Resolves `since`: `Nh`, `Nd` (relative to `now`) or `YYYY-MM-DD` (midnight at the
/// configured UTC offset). `None` when it is neither.
pub(crate) fn parse_since(text: &str, now: UnixMillis, offset_seconds: i32) -> Option<UnixMillis> {
    let text = text.trim();
    let relative = |digits: &str, unit_millis: i64| -> Option<UnixMillis> {
        if digits.is_empty() || digits.len() > 5 || !digits.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let n: i64 = digits.parse().ok()?;
        let millis = n.checked_mul(unit_millis)?;
        if n == 0 || millis > MAX_HOURS * MILLIS_PER_HOUR {
            return None;
        }
        Some(UnixMillis::from_millis(
            now.as_millis().saturating_sub(millis),
        ))
    };
    if let Some(digits) = text.strip_suffix(['h', 'H']) {
        return relative(digits, MILLIS_PER_HOUR);
    }
    if let Some(digits) = text.strip_suffix(['d', 'D']) {
        return relative(digits, MILLIS_PER_DAY);
    }
    let date: CalendarDate = text.parse().ok()?;
    Some(midnight(date, offset_seconds))
}

/// Midnight of `date` at the given offset from UTC.
pub(crate) fn midnight(date: CalendarDate, offset_seconds: i32) -> UnixMillis {
    UnixMillis::from_millis(
        date.start_of_day_utc()
            .as_millis()
            .saturating_sub(i64::from(offset_seconds) * 1000),
    )
}

/// Formats an instant as `YYYY-MM-DD` at the given offset.
pub(crate) fn format_date(at: UnixMillis, offset_seconds: i32) -> String {
    let local = at
        .as_millis()
        .saturating_add(i64::from(offset_seconds) * 1000);
    // Days since 1970-01-01 to a calendar date (Howard Hinnant's civil_from_days).
    let z = local.div_euclid(MILLIS_PER_DAY) + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Encodes a page offset as an opaque cursor.
pub(crate) fn encode_cursor(offset: u32) -> String {
    format!("v1.{offset}")
}

/// Decodes a cursor made by [`encode_cursor`]. `None` for anything else.
pub(crate) fn decode_cursor(text: &str) -> Option<u32> {
    let digits = text.strip_prefix("v1.")?;
    if digits.is_empty() || digits.len() > 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let offset: u32 = digits.parse().ok()?;
    (offset <= MAX_CURSOR_OFFSET).then_some(offset)
}

/// Parses a citation id (`m:123`, `D-12`); entry prefixes are case-insensitive for people.
pub(crate) fn parse_id(text: &str) -> Option<CitationId> {
    let text = text.trim();
    text.parse().ok().or_else(|| {
        teamctx_core::record::ShortId::parse_user(text)
            .ok()
            .map(CitationId::Entry)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_836_800_000 + 5 * MILLIS_PER_HOUR; // 2026-11-05 05:00 UTC

    fn since(text: &str, offset: i32) -> Option<i64> {
        parse_since(text, UnixMillis::from_millis(NOW), offset).map(UnixMillis::as_millis)
    }

    #[test]
    fn since_accepts_windows_and_dates() {
        assert_eq!(since("24h", 0), Some(NOW - 24 * MILLIS_PER_HOUR));
        assert_eq!(since("7d", 0), Some(NOW - 7 * MILLIS_PER_DAY));
        assert_eq!(since(" 2h ", 0), Some(NOW - 2 * MILLIS_PER_HOUR));
        assert_eq!(since("2026-11-05", 0), Some(1_793_836_800_000));
        // Midnight at UTC+02:00 is two hours before midnight UTC.
        assert_eq!(
            since("2026-11-05", 7200),
            Some(1_793_836_800_000 - 2 * MILLIS_PER_HOUR)
        );
    }

    #[test]
    fn since_rejects_everything_else() {
        for text in [
            "",
            "0h",
            "h",
            "-1h",
            "+1h",
            "1.5h",
            "24",
            "24x",
            "366d",
            "99999h",
            "2026-13-01",
            "2026-1-1",
            "yesterday",
            "24h ago",
            "１h",
        ] {
            assert_eq!(since(text, 0), None, "{text:?}");
        }
    }

    #[test]
    fn dates_format_at_the_offset() {
        let at = UnixMillis::from_millis(1_793_836_800_000 - 1);
        assert_eq!(format_date(at, 0), "2026-11-04");
        assert_eq!(format_date(at, 3600), "2026-11-05");
        assert_eq!(format_date(UnixMillis::from_millis(0), 0), "1970-01-01");
    }

    #[test]
    fn cursors_round_trip_and_reject_forgeries() {
        assert_eq!(decode_cursor(&encode_cursor(40)), Some(40));
        for text in [
            "",
            "v1.",
            "v2.5",
            "5",
            "v1.-1",
            "v1.101",
            "v1.5 ",
            "v1.9999999999",
        ] {
            assert_eq!(decode_cursor(text), None, "{text:?}");
        }
    }

    #[test]
    fn ids_parse_in_both_forms() {
        assert_eq!(
            parse_id("m:12").map(|i| i.to_string()).as_deref(),
            Some("m:12")
        );
        assert_eq!(
            parse_id("d-7").map(|i| i.to_string()).as_deref(),
            Some("D-7")
        );
        assert_eq!(parse_id("x-7"), None);
        assert_eq!(parse_id("m:0"), None);
    }
}
