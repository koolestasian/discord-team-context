//! The read-only REST surface the sync algorithm needs (plan sections 7.2 and 7.3).
//!
//! [`DiscordRest`] has one method per `GET` endpoint the sync uses and no method that
//! writes: there is no way to create, edit, delete or react to anything through it. The
//! production adapter (a `twilight-http` client, added with the live gateway work) and the
//! test fake both implement it. A unit test pins the method list, so adding a method
//! forces a review of this file.
//!
//! The methods are synchronous because the sync worker runs on its own thread, like the
//! store's single writer (plan section 5.2). An adapter over an async HTTP client blocks
//! on its runtime handle. Waiting out short rate limits is the adapter's job; when a limit
//! is too long to wait out it returns [`RestError::RateLimited`] and the sync stops at the
//! last saved cursor (see [`SyncError`](super::SyncError)).
//!
//! Responses are raw JSON so that [`crate::mapping`] stays the only place that interprets
//! Discord payloads. Everything in them is untrusted chat data.

use std::error::Error as StdError;
use std::time::Duration;

use serde_json::Value;
use teamctx_core::id::Snowflake;
use thiserror::Error;

/// The most messages Discord returns for one `GET /channels/{id}/messages`.
pub const MAX_MESSAGE_PAGE: u8 = 100;

/// Where in a channel's history a message page starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagePosition {
    /// The newest messages (no `before` or `after`).
    Latest,
    /// The messages immediately older than this one (`before=`).
    Before(Snowflake),
    /// The messages immediately newer than this one (`after=`).
    After(Snowflake),
}

/// One `GET /channels/{channel}/messages` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessagesRequest {
    /// The channel, thread or forum post.
    pub channel: Snowflake,
    /// Where to start.
    pub position: PagePosition,
    /// Page size, at most [`MAX_MESSAGE_PAGE`].
    pub limit: u8,
}

/// Which archived threads to list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadVisibility {
    /// `GET /channels/{id}/threads/archived/public`.
    Public,
    /// `GET /channels/{id}/threads/archived/private`. Needs Manage Threads, so a
    /// [`RestError::Forbidden`] is normal and the sync ignores it.
    Private,
}

/// Why a REST call failed. The messages are fixed text: they never include a URL, a
/// header or a response body, so they are safe to log.
#[derive(Debug, Error)]
pub enum RestError {
    /// HTTP 429 that the adapter chose not to wait out.
    #[error("rate limited by Discord; retry after {retry_after:?}")]
    RateLimited {
        /// How long Discord asked us to wait.
        retry_after: Duration,
        /// Whether the limit is global rather than per route.
        global: bool,
    },
    /// HTTP 404: the channel, thread or guild no longer exists.
    #[error("not found")]
    NotFound,
    /// HTTP 403: the bot cannot see or read it.
    #[error("missing access")]
    Forbidden,
    /// HTTP 401: the bot token was rejected. Retrying cannot help.
    #[error("the bot token was rejected")]
    Unauthorized,
    /// HTTP 5xx from Discord.
    #[error("Discord server error (HTTP {status})")]
    Server {
        /// The status code.
        status: u16,
    },
    /// The request failed before a response (network, TLS, timeout). The source error is
    /// only for debug logs.
    #[error("transport error")]
    Transport(#[source] Box<dyn StdError + Send + Sync>),
}

impl RestError {
    /// Whether trying the same sync again later can succeed without anyone changing
    /// configuration: rate limits, server errors and transport failures.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. } | Self::Server { .. } | Self::Transport(_)
        )
    }
}

/// Read-only access to the Discord REST endpoints used by sync (plan section 7.2).
///
/// Adding a method that writes is forbidden by plan section 7.3; the test
/// `trait_declares_only_reviewed_read_methods` pins the method list.
pub trait DiscordRest {
    /// `GET /guilds/{guild}/channels`: top-level channels (threads are not included).
    ///
    /// # Errors
    ///
    /// [`RestError`] as documented there.
    fn guild_channels(&self, guild: Snowflake) -> Result<Value, RestError>;

    /// `GET /guilds/{guild}/threads/active`: an object with a `threads` array.
    ///
    /// # Errors
    ///
    /// [`RestError`] as documented there.
    fn active_threads(&self, guild: Snowflake) -> Result<Value, RestError>;

    /// `GET /channels/{channel}/threads/archived/{public|private}`: an object with a
    /// `threads` array and `has_more`. `before` is the `archive_timestamp` (ISO 8601) of
    /// the last thread of the previous page.
    ///
    /// # Errors
    ///
    /// [`RestError`] as documented there.
    fn archived_threads(
        &self,
        channel: Snowflake,
        visibility: ThreadVisibility,
        before: Option<&str>,
    ) -> Result<Value, RestError>;

    /// `GET /channels/{channel}/messages`: an array of message objects, newest first.
    ///
    /// # Errors
    ///
    /// [`RestError`] as documented there.
    fn channel_messages(&self, request: &MessagesRequest) -> Result<Value, RestError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trait_declares_only_reviewed_read_methods() {
        let source = include_str!("rest.rs");
        let start = source
            .find("pub trait DiscordRest {")
            .expect("trait is declared in this file");
        let body = &source[start..];
        let end = body.find("\n}\n").expect("trait body ends");
        let methods: Vec<&str> = body[..end]
            .lines()
            .filter_map(|line| line.trim_start().strip_prefix("fn "))
            .map(|rest| rest.split(['(', '<']).next().unwrap_or(rest))
            .collect();
        assert_eq!(
            methods,
            [
                "guild_channels",
                "active_threads",
                "archived_threads",
                "channel_messages"
            ]
        );
    }

    #[test]
    fn rest_errors_never_print_their_source() {
        let source: Box<dyn StdError + Send + Sync> =
            "https://discord.com/api/v10/channels/1/messages Authorization: Bot SECRET".into();
        let error = RestError::Transport(source);
        assert_eq!(error.to_string(), "transport error");
        assert!(error.source().is_some());
    }

    #[test]
    fn only_transient_errors_are_retryable() {
        let limited = RestError::RateLimited {
            retry_after: Duration::from_secs(3),
            global: false,
        };
        assert!(limited.is_retryable());
        assert!(RestError::Server { status: 503 }.is_retryable());
        assert!(RestError::Transport("timeout".into()).is_retryable());
        assert!(!RestError::NotFound.is_retryable());
        assert!(!RestError::Forbidden.is_retryable());
        assert!(!RestError::Unauthorized.is_retryable());
    }
}
