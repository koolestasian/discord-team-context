//! Live events (plan section 7.2 step 4): one gateway dispatch at a time, through
//! [`crate::mapping`], into the store.
//!
//! [`ingest`] is synchronous and has no network or clock of its own. It runs on the
//! connector's ingest thread, which owns a writer client and a read-only connection.
//!
//! Rules:
//! - Only dispatches for the configured guild are used. A dispatch without a `guild_id`
//!   (a direct message) or for another guild is dropped, so one host never stores a second
//!   server's chat (multi-guild is v0.2, ADR 0004).
//! - Top-level channels get their include flag from the allowlist and denylist; threads
//!   and forum posts inherit their stored parent's flag (the same rules as discovery).
//! - A partial `MESSAGE_UPDATE` is merged with the stored row by the store. When the row
//!   is missing the message is fetched with one read-only page request
//!   (`before = id + 1`, `limit = 1`) and stored in full, if Discord still has it.
//! - An event about a channel the store does not know asks for a sync run instead of
//!   guessing: discovery stores the channel and catch-up reads its messages.
//! - `GUILD_CREATE` is not ingested here: it starts a sync run, whose discovery lists the
//!   channels over REST with the same include rules.

use serde_json::Value;
use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{ApplyOutcome, DirectoryRead, IngestWrite, MessageRead, RepoError};
use teamctx_core::source::{MessageRef, SourceEvent};
use teamctx_core::time::UnixMillis;
use thiserror::Error;

use crate::mapping::{MapError, Mapped, map_dispatch, map_rest_messages};
use crate::sync::{DiscordRest, MessagesRequest, PagePosition, RestError, SyncConfig};

/// Dispatch types the ingest thread handles. Everything else is ignored before mapping.
pub const LIVE_EVENTS: &[&str] = &[
    "MESSAGE_CREATE",
    "MESSAGE_UPDATE",
    "MESSAGE_DELETE",
    "MESSAGE_DELETE_BULK",
    "CHANNEL_CREATE",
    "CHANNEL_UPDATE",
    "CHANNEL_DELETE",
    "THREAD_CREATE",
    "THREAD_UPDATE",
    "THREAD_DELETE",
    "THREAD_LIST_SYNC",
];

/// What one dispatch led to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveOutcome {
    /// Every event was applied (or was a no-op replay, or for an excluded channel).
    Applied,
    /// The dispatch is not for this host: another guild, no guild, an event type it does
    /// not store, or a payload the mapping ignores.
    Ignored,
    /// The store lacks a channel the event needs. Run a sync; the event's content is
    /// picked up by discovery and catch-up.
    NeedsSync,
}

/// Why a dispatch could not be applied. The messages are fixed text.
#[derive(Debug, Error)]
pub enum LiveError {
    /// The payload claims a handled type but is malformed.
    #[error("a gateway event payload is malformed")]
    Map(#[from] MapError),
    /// The store failed.
    #[error("storage failed")]
    Store(#[from] RepoError),
    /// Fetching a message for a partial update failed.
    #[error("fetching an updated message failed")]
    Rest(#[source] RestError),
}

/// Applies one dispatch (`t` = `event_type`, `d` = `data`) for `source` at `now`.
///
/// # Errors
///
/// [`LiveError`] for a malformed payload, a storage failure or a failed fetch.
pub fn ingest<S, R>(
    store: &mut S,
    rest: &R,
    config: &SyncConfig,
    source: SourceId,
    event_type: &str,
    data: &Value,
    now: UnixMillis,
) -> Result<LiveOutcome, LiveError>
where
    S: IngestWrite + DirectoryRead + MessageRead + ?Sized,
    R: DiscordRest + ?Sized,
{
    if !LIVE_EVENTS.contains(&event_type) || !for_guild(data, config.guild) {
        return Ok(LiveOutcome::Ignored);
    }
    let events = match map_dispatch(event_type, data)? {
        Mapped::Events(events) => events,
        Mapped::Ignored(_) => return Ok(LiveOutcome::Ignored),
    };
    let mut outcome = LiveOutcome::Applied;
    for event in events {
        let event = match event {
            SourceEvent::ChannelUpsert(mut channel) => {
                let included = match channel.parent {
                    None => Some(config.includes_channel(&channel.name, channel.id)),
                    Some(parent) => parent_included(store, source, parent)?,
                };
                let Some(included) = included else {
                    outcome = LiveOutcome::NeedsSync;
                    continue;
                };
                channel.included = included;
                SourceEvent::ChannelUpsert(channel)
            }
            other => other,
        };
        match store.apply_event(source, &event, now)? {
            ApplyOutcome::Applied | ApplyOutcome::Unchanged | ApplyOutcome::Excluded => {}
            ApplyOutcome::UnknownChannel(_) => outcome = LiveOutcome::NeedsSync,
            ApplyOutcome::NeedsFetch(message) => {
                if fetch_and_store(store, rest, source, message, now)? == LiveOutcome::NeedsSync {
                    outcome = LiveOutcome::NeedsSync;
                }
            }
        }
    }
    Ok(outcome)
}

/// Whether a dispatch belongs to the configured guild (`d.guild_id`).
fn for_guild(data: &Value, guild: Snowflake) -> bool {
    data.get("guild_id")
        .and_then(Value::as_str)
        .and_then(|text| text.parse::<Snowflake>().ok())
        == Some(guild)
}

/// The stored include flag of a thread's parent, or `None` when the parent is unknown.
fn parent_included<S>(
    store: &S,
    source: SourceId,
    parent: Snowflake,
) -> Result<Option<bool>, RepoError>
where
    S: DirectoryRead + ?Sized,
{
    Ok(store
        .channels(source)?
        .into_iter()
        .find(|info| info.external_id == parent)
        .map(|info| info.included))
}

/// Fetches one message that a partial update referred to and stores it in full.
fn fetch_and_store<S, R>(
    store: &mut S,
    rest: &R,
    source: SourceId,
    message: MessageRef,
    now: UnixMillis,
) -> Result<LiveOutcome, LiveError>
where
    S: IngestWrite + ?Sized,
    R: DiscordRest + ?Sized,
{
    // `before` is exclusive, so `before = id + 1` with `limit = 1` returns the message
    // itself if it still exists, else an older one (which is ignored).
    let Some(after_it) = message
        .message
        .get()
        .checked_add(1)
        .and_then(Snowflake::new)
    else {
        return Ok(LiveOutcome::Ignored);
    };
    let page = match rest.channel_messages(&MessagesRequest {
        channel: message.channel,
        position: PagePosition::Before(after_it),
        limit: 1,
    }) {
        Ok(page) => page,
        Err(RestError::NotFound | RestError::Forbidden) => return Ok(LiveOutcome::Ignored),
        Err(error) => return Err(LiveError::Rest(error)),
    };
    let wanted = map_rest_messages(&page)?
        .into_events()
        .into_iter()
        .find(|event| matches!(event, SourceEvent::MessageUpsert(m) if m.id == message));
    let Some(event) = wanted else {
        return Ok(LiveOutcome::Ignored);
    };
    Ok(match store.apply_event(source, &event, now)? {
        ApplyOutcome::UnknownChannel(_) => LiveOutcome::NeedsSync,
        _ => LiveOutcome::Applied,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_the_configured_guild_is_used() {
        let guild = Snowflake::new(1_100_000_000_000_000_001).unwrap();
        assert!(for_guild(
            &json!({"guild_id": "1100000000000000001"}),
            guild
        ));
        assert!(!for_guild(
            &json!({"guild_id": "1100000000000000002"}),
            guild
        ));
        assert!(!for_guild(
            &json!({"guild_id": 1_100_000_000_000_000_001_u64}),
            guild
        ));
        assert!(!for_guild(&json!({}), guild));
        assert!(!for_guild(
            &json!({"guild_id": "01100000000000000001"}),
            guild
        ));
    }
}
