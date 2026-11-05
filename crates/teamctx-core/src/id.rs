//! Identifiers.
//!
//! Two families, kept apart by type so they cannot be mixed up:
//!
//! - [`Snowflake`]: an id assigned by Discord (guild, channel, thread, message, user).
//!   Connectors and [`SourceEvent`](crate::source::SourceEvent)s use only these.
//! - Row ids ([`SourceId`], [`ChannelId`], [`MemberId`], [`MessageId`], [`EntryId`],
//!   [`TokenId`]): `INTEGER PRIMARY KEY` values assigned by the store (plan section 6).
//!   Only the store creates them; everything above the store passes them back opaquely.
//!
//! Short ids such as `D-12` live in [`crate::record::ShortId`].

use std::fmt;
use std::num::NonZeroU64;
use std::str::FromStr;

use thiserror::Error;

use crate::time::UnixMillis;

/// Discord's epoch (2015-01-01T00:00:00Z) in Unix milliseconds.
pub const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

/// A Discord snowflake id.
///
/// Invariants:
/// - never zero (Discord does not issue id 0), so the wrapped value is a `NonZeroU64`;
/// - the text form is canonical decimal with no sign, no leading zeros and no
///   whitespace. The store keeps external ids as `TEXT` (plan section 6), so one id must
///   have exactly one text form: `s.parse::<Snowflake>()?.to_string() == s` for every
///   accepted `s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Snowflake(NonZeroU64);

impl Snowflake {
    /// Wraps a raw id. Returns `None` for `0`.
    #[must_use]
    pub const fn new(raw: u64) -> Option<Self> {
        match NonZeroU64::new(raw) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }

    /// The raw id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }

    /// The creation time encoded in the id (top 42 bits plus the Discord epoch).
    ///
    /// Plan section 6 stores `messages.created_at` "from snowflake".
    #[must_use]
    pub const fn timestamp(self) -> UnixMillis {
        // `get() >> 22` is at most 2^42 - 1, which always fits in an i64, and adding the
        // epoch stays far below i64::MAX, so neither the cast nor the sum can overflow.
        let since_epoch = (self.get() >> 22) as i64;
        UnixMillis::from_millis(since_epoch + DISCORD_EPOCH_MS)
    }
}

impl fmt::Display for Snowflake {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

/// Why a text value is not a canonical snowflake.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseSnowflakeError {
    /// The input was empty.
    #[error("snowflake is empty")]
    Empty,
    /// The input contained something other than ASCII digits.
    #[error("snowflake must contain only digits")]
    NotDigits,
    /// The input had a leading zero (including the value `0` itself).
    #[error("snowflake must not start with 0")]
    LeadingZero,
    /// The value does not fit in 64 bits.
    #[error("snowflake is too large")]
    TooLarge,
}

impl FromStr for Snowflake {
    type Err = ParseSnowflakeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let first = s.bytes().next().ok_or(ParseSnowflakeError::Empty)?;
        if !s.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseSnowflakeError::NotDigits);
        }
        if first == b'0' {
            return Err(ParseSnowflakeError::LeadingZero);
        }
        let raw: u64 = s.parse().map_err(|_| ParseSnowflakeError::TooLarge)?;
        // Digits only and no leading zero, so the value is at least 1.
        Self::new(raw).ok_or(ParseSnowflakeError::LeadingZero)
    }
}

/// Declares a store row id: a positive `i64` assigned by SQLite.
macro_rules! row_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        ///
        /// Invariant: the value is positive. SQLite assigns `INTEGER PRIMARY KEY` values
        /// from 1 upwards, and short ids (`D-12`) rely on that.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(i64);

        impl $name {
            /// Wraps a row id. Returns `None` unless `raw > 0`.
            #[must_use]
            pub const fn new(raw: i64) -> Option<Self> {
                if raw > 0 { Some(Self(raw)) } else { None }
            }

            /// The raw row id, for binding as an SQL parameter.
            #[must_use]
            pub const fn get(self) -> i64 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

row_id!(
    /// Row id in `sources`: one connected workspace (a Discord guild in v0.1).
    SourceId
);
row_id!(
    /// Row id in `channels` (channels, threads and forum posts alike).
    ChannelId
);
row_id!(
    /// Row id in `members`.
    MemberId
);
row_id!(
    /// Row id in `messages`.
    MessageId
);
row_id!(
    /// Row id in `record_entries`. Shown to people as a [`ShortId`](crate::record::ShortId).
    EntryId
);
row_id!(
    /// Row id in `access_tokens`.
    TokenId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snowflake_text_round_trips() {
        for raw in [1_u64, 9, 10, 175_928_847_299_117_063, u64::MAX] {
            let id = Snowflake::new(raw).unwrap();
            let text = id.to_string();
            assert_eq!(text, raw.to_string());
            assert_eq!(text.parse::<Snowflake>().unwrap(), id);
        }
    }

    #[test]
    fn snowflake_rejects_non_canonical_text() {
        let cases = [
            ("", ParseSnowflakeError::Empty),
            ("0", ParseSnowflakeError::LeadingZero),
            ("012", ParseSnowflakeError::LeadingZero),
            ("+12", ParseSnowflakeError::NotDigits),
            ("-12", ParseSnowflakeError::NotDigits),
            (" 12", ParseSnowflakeError::NotDigits),
            ("12 ", ParseSnowflakeError::NotDigits),
            ("1_2", ParseSnowflakeError::NotDigits),
            ("١٢", ParseSnowflakeError::NotDigits), // non-ASCII digits
            ("18446744073709551616", ParseSnowflakeError::TooLarge), // u64::MAX + 1
        ];
        for (input, expected) in cases {
            assert_eq!(input.parse::<Snowflake>(), Err(expected), "input {input:?}");
        }
    }

    #[test]
    fn snowflake_zero_is_not_an_id() {
        assert_eq!(Snowflake::new(0), None);
    }

    #[test]
    fn snowflake_timestamp_uses_the_discord_epoch() {
        // Example from the Discord API reference ("Snowflakes"): 175928847299117063
        // was created at 2016-04-30T11:18:25.796Z.
        let id = Snowflake::new(175_928_847_299_117_063).unwrap();
        assert_eq!(id.timestamp(), UnixMillis::from_millis(1_462_015_105_796));
        // Timestamp bits of 1 mean one millisecond after the epoch.
        let first = Snowflake::new(1 << 22).unwrap();
        assert_eq!(
            first.timestamp(),
            UnixMillis::from_millis(DISCORD_EPOCH_MS + 1)
        );
        // The largest id stays in range.
        let last = Snowflake::new(u64::MAX).unwrap();
        assert_eq!(
            last.timestamp(),
            UnixMillis::from_millis(DISCORD_EPOCH_MS + (1_i64 << 42) - 1)
        );
    }

    #[test]
    fn row_ids_are_positive() {
        assert_eq!(EntryId::new(0), None);
        assert_eq!(EntryId::new(-1), None);
        assert_eq!(EntryId::new(1).map(EntryId::get), Some(1));
        assert_eq!(MessageId::new(i64::MAX).map(MessageId::get), Some(i64::MAX));
        assert_eq!(ChannelId::new(42).unwrap().to_string(), "42");
    }
}
