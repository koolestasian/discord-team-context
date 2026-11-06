//! Pure mapping from Discord gateway and REST payloads to core [`SourceEvent`]s (plan
//! sections 5.3 and 7.2).
//!
//! There is no network, no gateway and no clock here: the functions take JSON payloads and
//! return events, so they are tested against recorded-shape fixtures in
//! `fixtures/discord/`. Nothing in this module can create, edit, delete or react to
//! anything in Discord (plan section 7.3).
//!
//! # Entry points
//!
//! - [`map_gateway_payload`] takes a full gateway frame (`{"t": ..., "d": ...}`) and
//!   [`map_dispatch`] takes the event name and its data.
//! - [`map_rest_messages`], [`map_rest_channels`] and [`map_thread_list`] cover REST
//!   pages for catch-up, backfill and discovery.
//!
//! # Rules
//!
//! - Every chat-controlled string (message text, names, topics, attachment names) is
//!   untrusted data. It is copied into events and never interpreted.
//! - Event types we do not handle, unsupported channel kinds and system messages map to
//!   [`Mapped::Ignored`] with a reason. They are never an error and never a panic.
//! - A payload that claims to be a handled event but is malformed is a [`MapError`].

mod channel;
mod fields;
mod message;
mod normalize;

#[cfg(test)]
mod tests;

use serde_json::Value;
use teamctx_core::id::Snowflake;
use teamctx_core::source::{ChannelError, SourceEvent};
use thiserror::Error;
use twilight_model::id::Id;

pub use channel::{
    map_channel, map_channel_delete, map_guild_create, map_rest_channels, map_thread_delete,
    map_thread_list,
};
pub use message::{map_message, map_message_delete, map_message_delete_bulk, map_message_update};

/// The result of mapping one payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mapped {
    /// Zero or more events to apply, in order.
    Events(Vec<SourceEvent>),
    /// The payload is valid but not something teamctx stores.
    Ignored(Ignored),
}

impl Mapped {
    /// A result with exactly one event.
    fn event(event: SourceEvent) -> Self {
        Self::Events(vec![event])
    }

    /// The events, or none for an ignored payload.
    #[must_use]
    pub fn into_events(self) -> Vec<SourceEvent> {
        match self {
            Self::Events(events) => events,
            Self::Ignored(_) => Vec::new(),
        }
    }
}

/// Why a payload produced no events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ignored {
    /// An event type this mapping does not handle (typing, presence, voice, reactions
    /// until reaction capture exists, and anything Discord adds later).
    UnknownEventType(String),
    /// A channel kind teamctx does not store: voice, stage, category, direct message and
    /// others. Holds Discord's numeric channel type.
    UnsupportedChannelType(u8),
    /// A system message such as a join or pin notice. Holds Discord's numeric message type.
    UnsupportedMessageType(u64),
    /// A `MESSAGE_UPDATE` that changes nothing we store, such as an embed unfurl.
    EmptyUpdate,
}

/// A payload that is malformed for the event it claims to be.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MapError {
    /// A required field is missing or has the wrong shape.
    #[error("malformed Discord payload: {what}: {reason}")]
    Malformed {
        /// The field or payload that is wrong.
        what: String,
        /// What is wrong with it.
        reason: String,
    },
    /// A channel that breaks the parent invariant (for example a thread without a parent).
    #[error("invalid channel: {0}")]
    InvalidChannel(#[from] ChannelError),
}

impl MapError {
    /// A [`MapError::Malformed`] for `what`.
    fn malformed(what: &str, reason: impl Into<String>) -> Self {
        Self::Malformed {
            what: what.to_owned(),
            reason: reason.into(),
        }
    }
}

/// Maps one gateway dispatch by event name (`t`) and data (`d`).
///
/// # Errors
///
/// [`MapError`] if a handled event's payload is malformed. Unknown event names are
/// [`Mapped::Ignored`], not errors.
pub fn map_dispatch(event_type: &str, data: &Value) -> Result<Mapped, MapError> {
    match event_type {
        "MESSAGE_CREATE" => map_message(data),
        "MESSAGE_UPDATE" => map_message_update(data),
        "MESSAGE_DELETE" => map_message_delete(data),
        "MESSAGE_DELETE_BULK" => map_message_delete_bulk(data),
        "CHANNEL_CREATE" | "CHANNEL_UPDATE" | "THREAD_CREATE" | "THREAD_UPDATE" => {
            map_channel(data)
        }
        "CHANNEL_DELETE" => map_channel_delete(data),
        "THREAD_DELETE" => map_thread_delete(data),
        "THREAD_LIST_SYNC" => map_thread_list(data),
        "GUILD_CREATE" => map_guild_create(data),
        other => Ok(Mapped::Ignored(Ignored::UnknownEventType(other.to_owned()))),
    }
}

/// Maps a full gateway dispatch frame, `{"t": "<EVENT>", "d": { ... }}`.
///
/// # Errors
///
/// [`MapError`] if the frame has no string `t` or `d`, or the event payload is malformed.
pub fn map_gateway_payload(frame: &Value) -> Result<Mapped, MapError> {
    let event_type = frame
        .get("t")
        .and_then(Value::as_str)
        .ok_or_else(|| MapError::malformed("t", "missing or not a string"))?;
    let data = frame
        .get("d")
        .ok_or_else(|| MapError::malformed("d", "missing field"))?;
    map_dispatch(event_type, data)
}

/// Maps a REST page of message objects (`GET /channels/{id}/messages`).
///
/// System messages are skipped. The events keep the page's order.
///
/// # Errors
///
/// [`MapError`] if the payload is not an array or an entry is malformed.
pub fn map_rest_messages(page: &Value) -> Result<Mapped, MapError> {
    let entries = page
        .as_array()
        .ok_or_else(|| MapError::malformed("messages", "expected an array of messages"))?;
    let mut events = Vec::with_capacity(entries.len());
    for entry in entries {
        events.extend(map_message(entry)?.into_events());
    }
    Ok(Mapped::Events(events))
}

/// Builds the error for a payload that does not deserialise into its twilight model.
fn json_error(what: &'static str) -> impl FnOnce(serde_json::Error) -> MapError {
    // serde_json's message quotes the offending value, which is untrusted chat data, so
    // only the error category and position are kept.
    move |error| {
        let kind = match error.classify() {
            serde_json::error::Category::Io => "unreadable",
            serde_json::error::Category::Syntax => "not valid JSON",
            serde_json::error::Category::Data => "a field has the wrong type or value",
            serde_json::error::Category::Eof => "ends too early",
        };
        MapError::malformed(
            what,
            format!("{kind} (line {}, column {})", error.line(), error.column()),
        )
    }
}

/// Converts a twilight id to a core [`Snowflake`]. Twilight ids are never zero, so the
/// error arm is unreachable in practice; it exists to avoid a panic path.
fn snowflake_from_id<T>(id: Id<T>) -> Result<Snowflake, MapError> {
    Snowflake::new(id.get()).ok_or_else(|| MapError::malformed("id", "zero is not a valid id"))
}
