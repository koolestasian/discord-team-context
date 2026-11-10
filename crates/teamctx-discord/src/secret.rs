//! The bot token, kept out of logs, panic messages and error text (plan section 13).
//!
//! [`BotToken`] has no `Display`, `Clone`, `Serialize` or `PartialEq`, and its `Debug`
//! prints `BotToken(***)`. Only this crate can read the value, to hand it to the
//! `twilight` gateway and HTTP client, whose own `Debug` output redacts it too.

use std::fmt;

/// A Discord bot token. Read from the environment variable named by
/// `discord.token_env`; never from a file or command-line argument.
pub struct BotToken(String);

impl BotToken {
    /// Wraps a token. Returns `None` for an empty or all-whitespace value, so an unset
    /// variable and an empty one are treated alike.
    #[must_use]
    pub fn new(value: String) -> Option<Self> {
        if value.trim().is_empty() {
            None
        } else {
            Some(Self(value))
        }
    }

    /// The value, for the gateway and the HTTP client only. Never format the result.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for BotToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("BotToken(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::BotToken;

    const FAKE: &str = "FAKE.bot-token.not-a-real-credential";

    #[test]
    fn debug_never_prints_the_value() {
        let token = BotToken::new(FAKE.to_owned()).unwrap();
        for text in [format!("{token:?}"), format!("{token:#?}")] {
            assert_eq!(text, "BotToken(***)");
        }
        assert_eq!(token.expose(), FAKE);
    }

    #[test]
    fn blank_values_are_not_tokens() {
        assert!(BotToken::new(String::new()).is_none());
        assert!(BotToken::new(" \t\n".to_owned()).is_none());
    }
}
