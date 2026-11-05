//! Message events: create, update, delete and bulk delete (plan section 7.2).
//!
//! Messages are read from the raw JSON rather than through `twilight_model::channel::Message`
//! for two reasons. That type cannot represent a partial `MESSAGE_UPDATE` (it requires the
//! author, timestamps and every list), and its `Mention` drops `global_name`, which the
//! display-name rule needs. Channel, thread and delete events do use the twilight types.
//!
//! Everything read here is untrusted chat text. It is copied into core types as data and
//! never interpreted.

use std::collections::HashMap;

use serde_json::{Map, Value};
use teamctx_core::id::Snowflake;
use teamctx_core::source::{Member, Message, MessagePatch, MessageRef, SourceEvent};
use twilight_model::gateway::payload::incoming::{MessageDelete, MessageDeleteBulk};

use super::fields::{
    object, optional_millis, optional_snowflake, optional_str, required_snowflake, required_str,
};
use super::normalize::{display_name, resolve_mentions};
use super::{Ignored, MapError, Mapped};

/// Message types that carry something a person wrote: regular, reply, chat input command
/// and context menu command (a bot's reply to a command). Joins, pins, boosts and other
/// system messages are ignored.
const SUPPORTED_MESSAGE_TYPES: [u64; 4] = [0, 19, 20, 23];

/// Message reference type `DEFAULT` (a reply). Forwards (`1`) carry no reply target.
const REFERENCE_TYPE_DEFAULT: u64 = 0;

/// Maps a full message object (`MESSAGE_CREATE`, or a REST message) to a
/// [`SourceEvent::MessageUpsert`].
///
/// # Errors
///
/// [`MapError`] if a required field is missing or malformed.
pub fn map_message(data: &Value) -> Result<Mapped, MapError> {
    let map = object(data, "message")?;
    let kind = message_type(map);
    if !SUPPORTED_MESSAGE_TYPES.contains(&kind) {
        return Ok(Mapped::Ignored(Ignored::UnsupportedMessageType(kind)));
    }

    let id = message_ref(map)?;
    let mentions = mentions(map)?;
    let content = optional_str(map, "content").unwrap_or_default();
    let author = author(map)?;

    let reply_to = match map.get("message_reference").and_then(Value::as_object) {
        Some(reference)
            if reference
                .get("type")
                .and_then(Value::as_u64)
                .unwrap_or(REFERENCE_TYPE_DEFAULT)
                == REFERENCE_TYPE_DEFAULT =>
        {
            optional_snowflake(reference, "message_id")?
        }
        _ => None,
    };

    Ok(Mapped::event(SourceEvent::MessageUpsert(Message {
        id,
        author: Some(author),
        content: resolve_mentions(content, &mentions.names),
        mentions: mentions.ids.unwrap_or_default(),
        reply_to,
        attachment_names: attachment_names(map).unwrap_or_default(),
        created_at: id.message.timestamp(),
        edited_at: optional_millis(map, "edited_timestamp")?,
    })))
}

/// Maps a `MESSAGE_UPDATE` payload, which may be partial, to a
/// [`SourceEvent::MessageUpdate`].
///
/// Only fields present in the payload end up in the patch, so a field the payload left
/// out is never blanked. A payload that changes nothing we store (for example an embed
/// unfurl) maps to [`Ignored::EmptyUpdate`].
///
/// Mentions in `content` are resolved from the payload's own `mentions` list. A payload
/// that has `content` but no `mentions` leaves user mentions as written.
///
/// # Errors
///
/// [`MapError`] if the ids are missing or a present field is malformed.
pub fn map_message_update(data: &Value) -> Result<Mapped, MapError> {
    let map = object(data, "message update")?;
    let id = message_ref(map)?;
    let mentions = mentions(map)?;

    let patch = MessagePatch {
        id,
        content: optional_str(map, "content").map(|text| resolve_mentions(text, &mentions.names)),
        mentions: mentions.ids,
        attachment_names: attachment_names(map),
        edited_at: optional_millis(map, "edited_timestamp")?,
    };
    let empty = patch.content.is_none()
        && patch.mentions.is_none()
        && patch.attachment_names.is_none()
        && patch.edited_at.is_none();
    if empty {
        return Ok(Mapped::Ignored(Ignored::EmptyUpdate));
    }
    Ok(Mapped::event(SourceEvent::MessageUpdate(patch)))
}

/// Maps `MESSAGE_DELETE` to [`SourceEvent::MessageDelete`].
///
/// # Errors
///
/// [`MapError`] if the payload is not a message delete.
pub fn map_message_delete(data: &Value) -> Result<Mapped, MapError> {
    let delete: MessageDelete =
        serde_json::from_value(data.clone()).map_err(super::json_error("MESSAGE_DELETE"))?;
    Ok(Mapped::event(SourceEvent::MessageDelete {
        message: MessageRef {
            channel: super::snowflake_from_id(delete.channel_id)?,
            message: super::snowflake_from_id(delete.id)?,
        },
    }))
}

/// Maps `MESSAGE_DELETE_BULK` to [`SourceEvent::MessageBulkDelete`].
///
/// # Errors
///
/// [`MapError`] if the payload is not a bulk delete.
pub fn map_message_delete_bulk(data: &Value) -> Result<Mapped, MapError> {
    let bulk: MessageDeleteBulk =
        serde_json::from_value(data.clone()).map_err(super::json_error("MESSAGE_DELETE_BULK"))?;
    let ids = bulk
        .ids
        .into_iter()
        .map(super::snowflake_from_id)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Mapped::event(SourceEvent::MessageBulkDelete {
        channel: super::snowflake_from_id(bulk.channel_id)?,
        ids,
    }))
}

/// The numeric `type`; an absent or non-numeric value means a regular message.
fn message_type(map: &Map<String, Value>) -> u64 {
    map.get("type").and_then(Value::as_u64).unwrap_or(0)
}

/// The channel and message ids.
fn message_ref(map: &Map<String, Value>) -> Result<MessageRef, MapError> {
    Ok(MessageRef {
        channel: required_snowflake(map, "channel_id")?,
        message: required_snowflake(map, "id")?,
    })
}

/// The author, with the display name rule applied. Messages sent by webhooks count as
/// bots.
fn author(map: &Map<String, Value>) -> Result<Member, MapError> {
    let author = object(
        map.get("author")
            .ok_or_else(|| MapError::malformed("author", "missing field"))?,
        "author",
    )?;
    let nick = map
        .get("member")
        .and_then(Value::as_object)
        .and_then(|member| optional_str(member, "nick"));
    let is_bot = author.get("bot").and_then(Value::as_bool).unwrap_or(false)
        || map.get("webhook_id").is_some_and(|id| !id.is_null());
    Ok(Member {
        id: required_snowflake(author, "id")?,
        display_name: display_name(
            nick,
            optional_str(author, "global_name"),
            required_str(author, "username")?,
        ),
        is_bot,
    })
}

/// The users a message mentions.
struct Mentions {
    /// Mentioned user ids in payload order, without duplicates. `None` when the payload
    /// has no `mentions` array, which lets an update leave the stored mentions alone.
    ids: Option<Vec<Snowflake>>,
    /// Display names by user id, for resolving mentions in the text.
    names: HashMap<u64, String>,
}

/// Reads the `mentions` array.
fn mentions(map: &Map<String, Value>) -> Result<Mentions, MapError> {
    let mut names = HashMap::new();
    let Some(list) = map.get("mentions").and_then(Value::as_array) else {
        return Ok(Mentions { ids: None, names });
    };
    let mut ids = Vec::with_capacity(list.len());
    for entry in list {
        let user = object(entry, "mention")?;
        let id = required_snowflake(user, "id")?;
        if ids.contains(&id) {
            continue;
        }
        let nick = user
            .get("member")
            .and_then(Value::as_object)
            .and_then(|member| optional_str(member, "nick"));
        let name = display_name(
            nick,
            optional_str(user, "global_name"),
            required_str(user, "username")?,
        );
        names.insert(id.get(), name);
        ids.push(id);
    }
    Ok(Mentions {
        ids: Some(ids),
        names,
    })
}

/// Attachment file names only. Urls, sizes and contents are never read. `None` when the
/// payload has no `attachments` array.
fn attachment_names(map: &Map<String, Value>) -> Option<Vec<String>> {
    let list = map.get("attachments")?.as_array()?;
    Some(
        list.iter()
            .filter_map(|attachment| attachment.get("filename")?.as_str())
            .map(str::to_owned)
            .collect(),
    )
}
