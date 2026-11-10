//! The production REST client: `twilight-http` behind the read-only [`DiscordRest`] trait
//! (plan sections 7.2 and 7.3).
//!
//! Every read in this file is a `GET`. The sync uses the four [`DiscordRest`] methods;
//! the live `doctor` and the interaction access check use [`TwilightRest::read`] with a
//! [`Read`], whose variants are the only other endpoints this crate reads. The only writes
//! are the interaction surface of plan section 7.3 ([`InteractionApi`]): registering the
//! commands, answering an interaction and editing that answer after a deferral. There is
//! no method here, or anywhere in the product, that creates, edits or deletes messages,
//! sends follow-ups or touches anything else; `tests/readonly.rs` fails the build if one
//! appears.
//!
//! Rate limits: `twilight-http`'s built-in ratelimiter waits out route buckets before a
//! request is sent. A `429` that still comes back is returned as
//! [`RestError::RateLimited`] with Discord's `retry_after`, and the sync stops at its last
//! saved cursor.
//!
//! Errors: `twilight-http` puts the response body into its error's `Display`. A body can
//! echo chat content, so no `twilight-http` error is kept, formatted or logged here: each
//! one is reduced to its status code or to fixed text.

use std::future::IntoFuture;
use std::num::NonZeroU64;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use serde_json::Value;
use teamctx_core::id::Snowflake;
use thiserror::Error;
use tokio::runtime::Handle;
use tokio_util::sync::CancellationToken;
use twilight_http::Client;
use twilight_http::api_error::ApiError;
use twilight_http::error::{Error as HttpError, ErrorType};
use twilight_http::response::Response;
use twilight_model::application::command::Command;
use twilight_model::http::interaction::InteractionResponse;
use twilight_model::id::Id;

use crate::interactions::payload::InteractionToken;
use crate::interactions::{InteractionApi, Reply};
use crate::secret::BotToken;
use crate::sync::{DiscordRest, MessagesRequest, PagePosition, RestError, ThreadVisibility};

/// How long one request may take before it counts as a transport failure.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Wait used when Discord answers `429` without a usable `retry_after`.
const DEFAULT_RETRY_AFTER: Duration = Duration::from_secs(5);

/// The extra `GET` endpoints the live `doctor` reads. Each maps to one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// `GET /users/@me`: the bot's own user.
    CurrentUser,
    /// `GET /applications/@me`: the application, whose `flags` say whether the Message
    /// Content intent is enabled.
    Application,
    /// `GET /guilds/{guild}`: the guild with its roles and owner.
    Guild(Snowflake),
    /// `GET /guilds/{guild}/members/{user}`: the bot's member object (its roles).
    Member(Snowflake, Snowflake),
    /// `GET /guilds/{guild}/channels`: channels with their permission overwrites.
    Channels(Snowflake),
}

/// The HTTP client could not be created.
#[derive(Debug, Error)]
pub enum RestSetupError {
    /// TLS setup in `twilight-http` failed (it panics internally; the panic is caught).
    #[error("could not set up TLS for the Discord REST client")]
    Tls,
}

/// `twilight-http` client for one bot token.
///
/// The [`DiscordRest`] methods block on `handle` and must be called from a plain thread
/// (the sync worker), never from inside the async runtime; there they return a transport
/// error instead of panicking. After `cancel` fires every call fails fast, so a running
/// sync stops at its last saved cursor during shutdown.
#[derive(Debug)]
pub struct TwilightRest {
    client: Client,
    handle: Handle,
    cancel: CancellationToken,
}

impl TwilightRest {
    /// Creates the client. No request is made.
    ///
    /// # Errors
    ///
    /// [`RestSetupError::Tls`] if TLS cannot be set up.
    pub fn new(
        token: &BotToken,
        handle: Handle,
        cancel: CancellationToken,
    ) -> Result<Self, RestSetupError> {
        let token = token.expose().to_owned();
        let client = catch_unwind(AssertUnwindSafe(move || {
            Client::builder()
                .token(token)
                .timeout(REQUEST_TIMEOUT)
                .build()
        }))
        .map_err(|_| RestSetupError::Tls)?;
        Ok(Self {
            client,
            handle,
            cancel,
        })
    }

    /// Reads one of the [`Read`] endpoints and returns the raw JSON (untrusted).
    ///
    /// # Errors
    ///
    /// [`RestError`] for any failure; never the response body.
    pub async fn read(&self, what: Read) -> Result<Value, RestError> {
        match what {
            Read::CurrentUser => self.body(self.client.current_user()).await,
            Read::Application => self.body(self.client.current_user_application()).await,
            Read::Guild(guild) => self.body(self.client.guild(id(guild))).await,
            Read::Member(guild, user) => {
                self.body(self.client.guild_member(id(guild), id(user)))
                    .await
            }
            Read::Channels(guild) => self.body(self.client.guild_channels(id(guild))).await,
        }
    }

    /// Sends one request and returns its body as JSON.
    async fn body<Q, T>(&self, request: Q) -> Result<Value, RestError>
    where
        Q: IntoFuture<Output = Result<Response<T>, HttpError>>,
    {
        if self.cancel.is_cancelled() {
            return Err(shutting_down());
        }
        let response = tokio::select! {
            () = self.cancel.cancelled() => return Err(shutting_down()),
            response = request.into_future() => response.map_err(|error| from_http(&error))?,
        };
        let bytes = response
            .bytes()
            .await
            .map_err(|_| transport("the response body could not be read"))?;
        serde_json::from_slice(&bytes).map_err(|_| transport("the response body is not JSON"))
    }

    /// Sends one interaction request and drops the response unread (an edited message
    /// body would echo chat text).
    async fn deliver<Q, T>(&self, request: Q) -> Result<(), RestError>
    where
        Q: IntoFuture<Output = Result<Response<T>, HttpError>>,
    {
        if self.cancel.is_cancelled() {
            return Err(shutting_down());
        }
        tokio::select! {
            () = self.cancel.cancelled() => Err(shutting_down()),
            response = request.into_future() => response.map(drop).map_err(|error| from_http(&error)),
        }
    }

    /// Runs a request to completion from the sync thread.
    fn blocking<F>(&self, future: F) -> Result<Value, RestError>
    where
        F: std::future::Future<Output = Result<Value, RestError>>,
    {
        if Handle::try_current().is_ok() {
            // Blocking inside the runtime would panic; this is a wiring bug, not a
            // Discord failure, and must not take the process down.
            return Err(transport("REST sync called from inside the async runtime"));
        }
        self.handle.block_on(future)
    }
}

impl DiscordRest for TwilightRest {
    fn guild_channels(&self, guild: Snowflake) -> Result<Value, RestError> {
        self.blocking(self.body(self.client.guild_channels(id(guild))))
    }

    fn active_threads(&self, guild: Snowflake) -> Result<Value, RestError> {
        self.blocking(self.body(self.client.active_threads(id(guild))))
    }

    fn archived_threads(
        &self,
        channel: Snowflake,
        visibility: ThreadVisibility,
        before: Option<&str>,
    ) -> Result<Value, RestError> {
        let channel = id(channel);
        match visibility {
            ThreadVisibility::Public => {
                let mut request = self.client.public_archived_threads(channel);
                if let Some(before) = before {
                    request = request.before(before);
                }
                self.blocking(self.body(request))
            }
            ThreadVisibility::Private => {
                let mut request = self.client.private_archived_threads(channel);
                if let Some(before) = before {
                    request = request.before(before);
                }
                self.blocking(self.body(request))
            }
        }
    }

    fn channel_messages(&self, request: &MessagesRequest) -> Result<Value, RestError> {
        let limit = u16::from(request.limit.clamp(1, crate::sync::MAX_MESSAGE_PAGE));
        let messages = self.client.channel_messages(id(request.channel));
        match request.position {
            PagePosition::Latest => self.blocking(self.body(messages.limit(limit))),
            PagePosition::Before(message) => {
                self.blocking(self.body(messages.before(id(message)).limit(limit)))
            }
            PagePosition::After(message) => {
                self.blocking(self.body(messages.after(id(message)).limit(limit)))
            }
        }
    }
}

/// The interaction surface of plan section 7.3, and nothing else: register the commands
/// (bulk overwrite), answer an interaction, edit that answer after a deferral. The access
/// check reads two of the [`Read`] endpoints.
impl InteractionApi for TwilightRest {
    async fn install_commands(
        &self,
        application: Snowflake,
        guild: Snowflake,
        commands: &[Command],
    ) -> Result<(), RestError> {
        let client = self.client.interaction(id(application));
        self.deliver(client.set_guild_commands(id(guild), commands))
            .await
    }

    async fn respond(
        &self,
        application: Snowflake,
        interaction: Snowflake,
        token: &InteractionToken,
        response: &InteractionResponse,
    ) -> Result<(), RestError> {
        let client = self.client.interaction(id(application));
        self.deliver(client.create_response(id(interaction), token.expose(), response))
            .await
    }

    async fn fill_deferred(
        &self,
        application: Snowflake,
        token: &InteractionToken,
        reply: &Reply,
    ) -> Result<(), RestError> {
        let data = reply.data();
        let embeds = data.embeds.unwrap_or_default();
        let mentions = data.allowed_mentions.unwrap_or_default();
        let client = self.client.interaction(id(application));
        let request = client
            .update_response(token.expose())
            .content(data.content.as_deref())
            .embeds(Some(embeds.as_slice()))
            .allowed_mentions(Some(&mentions));
        self.deliver(request).await
    }

    async fn access_payloads(&self, guild: Snowflake) -> Result<(Value, Value), RestError> {
        let guild_json = self.read(Read::Guild(guild)).await?;
        let channels = self.read(Read::Channels(guild)).await?;
        Ok((guild_json, channels))
    }
}

/// A core snowflake as a twilight id. Both are non-zero `u64`s, so the fallback is
/// unreachable; it only avoids a panic path.
fn id<T>(snowflake: Snowflake) -> Id<T> {
    Id::from(NonZeroU64::new(snowflake.get()).unwrap_or(NonZeroU64::MIN))
}

/// Reduces a `twilight-http` error to a [`RestError`] without keeping its text.
fn from_http(error: &HttpError) -> RestError {
    match error.kind() {
        ErrorType::Unauthorized => RestError::Unauthorized,
        ErrorType::Response { status, error, .. } => match status.get() {
            401 => RestError::Unauthorized,
            403 => RestError::Forbidden,
            404 => RestError::NotFound,
            429 => {
                let (retry_after, global) = match error {
                    ApiError::Ratelimited(limit) => (
                        Duration::try_from_secs_f64(limit.retry_after)
                            .unwrap_or(DEFAULT_RETRY_AFTER),
                        limit.global,
                    ),
                    _ => (DEFAULT_RETRY_AFTER, false),
                };
                RestError::RateLimited {
                    retry_after,
                    global,
                }
            }
            status @ 500..=599 => RestError::Server { status },
            _ => transport("Discord answered with an unexpected HTTP status"),
        },
        ErrorType::RequestTimedOut => transport("the request timed out"),
        ErrorType::RequestCanceled => transport("the request was cancelled"),
        _ => transport("the request failed before Discord answered"),
    }
}

fn transport(reason: &'static str) -> RestError {
    RestError::Transport(reason.into())
}

fn shutting_down() -> RestError {
    transport("the host is shutting down")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_convert_without_loss() {
        let snowflake = Snowflake::new(1_100_000_000_000_000_001).unwrap();
        let converted: Id<twilight_model::id::marker::ChannelMarker> = id(snowflake);
        assert_eq!(converted.get(), snowflake.get());
    }

    #[test]
    fn transport_errors_carry_only_fixed_text() {
        let error = shutting_down();
        assert_eq!(error.to_string(), "transport error");
        assert!(error.is_retryable());
    }
}
