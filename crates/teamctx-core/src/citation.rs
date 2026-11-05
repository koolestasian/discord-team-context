//! Stable citation ids and Discord source links (plan sections 10.1 and 11).
//!
//! Parsing an id only validates its syntax. The caller must also check membership
//! in the retrieved source bundle before accepting a model-generated citation.

use std::{fmt, str::FromStr};

use thiserror::Error;

use crate::{
    id::{MessageId, Snowflake},
    record::ShortId,
};

/// An id accepted by `read` and used inside an `/ask` citation marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CitationId {
    /// A stored message, spelled `m:123`.
    Message(MessageId),
    /// A record entry, spelled `D-12`, `T-7` or `Q-3`.
    Entry(ShortId),
}

impl CitationId {
    /// Formats the marker a model must emit, such as `[m:123]` or `[D-12]`.
    #[must_use]
    pub fn marker(self) -> String {
        format!("[{self}]")
    }
}

impl fmt::Display for CitationId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Message(id) => write!(f, "m:{id}"),
            Self::Entry(id) => fmt::Display::fmt(id, f),
        }
    }
}

/// An invalid citation id. Input is omitted from errors because it is untrusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("citation must be m:<positive id>, D-<id>, T-<id> or Q-<id>")]
pub struct ParseCitationError;

impl FromStr for CitationId {
    type Err = ParseCitationError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        if let Some(digits) = text.strip_prefix("m:") {
            if !digits.bytes().all(|b| b.is_ascii_digit())
                || !digits.bytes().next().is_some_and(|b| b != b'0')
            {
                return Err(ParseCitationError);
            }
            let raw = digits.parse().map_err(|_| ParseCitationError)?;
            return MessageId::new(raw)
                .map(Self::Message)
                .ok_or(ParseCitationError);
        }
        text.parse()
            .map(Self::Entry)
            .map_err(|_| ParseCitationError)
    }
}

/// A Discord message location. All path components are validated numeric ids;
/// callers cannot inject a URL scheme, hostname or Markdown into this link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscordMessageLink {
    /// The guild that owns the channel.
    pub guild: Snowflake,
    /// The channel or thread containing the message.
    pub channel: Snowflake,
    /// The source message's external id, not its SQLite row id.
    pub message: Snowflake,
}

impl fmt::Display for DiscordMessageLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "https://discord.com/channels/{}/{}/{}",
            self.guild, self.channel, self.message
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_in_both_citation_forms() {
        for text in ["m:1", "m:9223372036854775807", "D-12", "T-7", "Q-3"] {
            let id: CitationId = text.parse().unwrap();
            assert_eq!(id.to_string(), text);
            assert_eq!(id.marker(), format!("[{text}]"));
            assert_eq!(id.to_string().parse::<CitationId>().unwrap(), id);
        }
    }

    #[test]
    fn malformed_and_non_canonical_ids_are_rejected() {
        for text in [
            "",
            "m:",
            "m:0",
            "m:01",
            "m:+1",
            "m:-1",
            "m:1 ",
            " m:1",
            "m:１",
            "m:9223372036854775808",
            "M:1",
            "[m:1]",
            "D-0",
            "d-12",
            "D-01",
            "m:1](https://evil.example)",
        ] {
            assert_eq!(
                text.parse::<CitationId>(),
                Err(ParseCitationError),
                "{text:?}"
            );
        }
    }

    #[test]
    fn jump_link_uses_only_external_ids_and_fixed_host() {
        let link = DiscordMessageLink {
            guild: Snowflake::new(101).unwrap(),
            channel: Snowflake::new(202).unwrap(),
            message: Snowflake::new(303).unwrap(),
        };
        assert_eq!(link.to_string(), "https://discord.com/channels/101/202/303");
    }
}
