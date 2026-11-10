//! The gateway connection (plan section 7.2 step 4): one `twilight-gateway` shard behind
//! the small [`Gateway`] trait, so the live loop can be tested with a fake.
//!
//! The shard is asked for exactly `GUILDS`, `GUILD_MESSAGES` and `MESSAGE_CONTENT`
//! ([`INTENTS`]). Reaction capture would add `GUILD_MESSAGE_REACTIONS`; it is not built
//! yet, so `teamctx run` refuses a configuration that enables it.
//!
//! Frames are handed over as raw JSON text, so [`crate::mapping`] stays the only code that
//! interprets Discord payloads. `twilight-gateway` itself handles hello, identify,
//! heartbeats, reconnects and resumes; see `docs/research/M3.md` for the pitfalls this
//! relies on it for. The only frame the bot ever sends is the close frame at shutdown.
//!
//! Errors never carry frame text: `twilight-gateway`'s deserialisation error prints the
//! whole event, which can hold chat messages, so only the fact that receiving failed is
//! kept ([`GatewayEvent::ReceiveFailed`]).

use std::future::{Future, poll_fn};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::task::{Context, Poll};

use thiserror::Error;
use twilight_gateway::{CloseFrame, Intents, Message, Shard, ShardId, StreamExt};

use crate::secret::BotToken;

/// The intents the connector requests (plan section 7.1), and nothing else.
pub const INTENTS: Intents = Intents::GUILDS
    .union(Intents::GUILD_MESSAGES)
    .union(Intents::MESSAGE_CONTENT);

/// What the gateway produced next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayEvent {
    /// One JSON text frame. Untrusted: dispatches carry chat content.
    Text(String),
    /// The connection closed with this close code, if any. The shard reconnects by itself
    /// unless the code is fatal (see [`fatal_close_reason`]).
    Closed(Option<u16>),
    /// A frame could not be received or decoded. Its content is deliberately dropped.
    ReceiveFailed,
}

/// A source of gateway frames. The production implementation is [`TwilightGateway`].
pub trait Gateway: Send {
    /// The next event, or `None` once the connection is closed for good (after
    /// [`Gateway::close`], or a fatal close code).
    fn next_event(&mut self) -> impl Future<Output = Option<GatewayEvent>> + Send;

    /// Starts a normal close. Keep calling [`Gateway::next_event`] until it returns
    /// `None` or a [`GatewayEvent::Closed`].
    fn close(&mut self);
}

/// Why the gateway closed for good, for close codes Discord documents as fatal. The text
/// is fixed and safe to log.
#[must_use]
pub fn fatal_close_reason(code: u16) -> Option<&'static str> {
    Some(match code {
        4004 => "Discord rejected the bot token (gateway close 4004)",
        4010 | 4011 => "Discord refused the shard settings (gateway close 4010/4011)",
        4012 => "Discord refused the gateway API version (gateway close 4012)",
        4013 => "Discord refused the requested intents as invalid (gateway close 4013)",
        4014 => {
            "the Message Content intent is not enabled for this bot in the Discord \
             Developer Portal (gateway close 4014)"
        }
        _ => return None,
    })
}

/// The shard could not be created.
#[derive(Debug, Error)]
pub enum GatewaySetupError {
    /// TLS setup in `twilight-gateway` failed (it panics internally in that case; the
    /// panic is caught and reported as this error).
    #[error("could not set up TLS for the Discord gateway")]
    Tls,
}

/// One `twilight-gateway` shard for the configured bot.
#[derive(Debug)]
pub struct TwilightGateway {
    shard: Shard,
}

impl TwilightGateway {
    /// Creates the shard. No connection is made until [`Gateway::next_event`] is polled.
    ///
    /// # Errors
    ///
    /// [`GatewaySetupError::Tls`] if TLS cannot be set up.
    pub fn new(token: &BotToken) -> Result<Self, GatewaySetupError> {
        let token = token.expose().to_owned();
        // `Shard::new` unwraps the TLS connector setup. A panic there is turned into an
        // error so the host can report it and exit cleanly.
        catch_unwind(AssertUnwindSafe(move || {
            Shard::new(ShardId::ONE, token, INTENTS)
        }))
        .map(|shard| Self { shard })
        .map_err(|_| GatewaySetupError::Tls)
    }
}

/// Polls the next websocket message of a shard. `StreamExt` has `Stream` as its supertrait,
/// so its `poll_next` is reachable here without depending on `futures-core` directly.
fn poll_message<S: StreamExt + Unpin + ?Sized>(
    stream: &mut S,
    cx: &mut Context<'_>,
) -> Poll<Option<S::Item>> {
    Pin::new(stream).poll_next(cx)
}

impl Gateway for TwilightGateway {
    async fn next_event(&mut self) -> Option<GatewayEvent> {
        let item = poll_fn(|cx| poll_message(&mut self.shard, cx)).await?;
        Some(match item {
            Ok(Message::Text(text)) => GatewayEvent::Text(text),
            Ok(Message::Close(frame)) => GatewayEvent::Closed(frame.map(|f| f.code)),
            Err(_) => GatewayEvent::ReceiveFailed,
        })
    }

    fn close(&mut self) {
        self.shard.close(CloseFrame::NORMAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_exactly_the_planned_intents() {
        assert_eq!(
            INTENTS,
            Intents::GUILDS | Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT
        );
        assert!(!INTENTS.contains(Intents::GUILD_MEMBERS));
        assert!(!INTENTS.contains(Intents::GUILD_PRESENCES));
        assert!(!INTENTS.contains(Intents::GUILD_MESSAGE_REACTIONS));
        assert!(!INTENTS.contains(Intents::DIRECT_MESSAGES));
        // GUILDS (1) + GUILD_MESSAGES (512) + MESSAGE_CONTENT (32768).
        assert_eq!(INTENTS.bits(), 1 | 512 | 32768);
    }

    #[test]
    fn fatal_codes_have_fixed_reasons() {
        assert!(
            fatal_close_reason(4014)
                .is_some_and(|reason| reason.contains("Message Content intent"))
        );
        assert!(fatal_close_reason(4004).is_some_and(|reason| reason.contains("token")));
        for code in [1000, 1001, 1006, 4000, 4007, 4008, 4009] {
            assert_eq!(fatal_close_reason(code), None, "{code}");
        }
    }
}
