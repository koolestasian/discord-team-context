//! Channel, thread and forum events (plan sections 5.3 and 7.2).

use serde_json::Value;
use teamctx_core::source::{Channel, ChannelKind, SourceEvent};
use twilight_model::channel::{Channel as DiscordChannel, ChannelType};
use twilight_model::gateway::payload::incoming::ThreadDelete;

use super::{Ignored, MapError, Mapped, json_error, snowflake_from_id};

/// Maps a channel, thread or forum post object to a [`SourceEvent::ChannelUpsert`].
///
/// Used for `CHANNEL_CREATE`, `CHANNEL_UPDATE`, `THREAD_CREATE`, `THREAD_UPDATE`, the
/// channels and threads inside `GUILD_CREATE` and thread lists, and REST channel objects.
///
/// The result always has `included: true`: the allowlist and denylist belong to the
/// connector's configuration, which it applies afterwards. Voice, stage, category, direct
/// message and other kinds teamctx does not store map to
/// [`Ignored::UnsupportedChannelType`].
///
/// # Errors
///
/// [`MapError`] if the payload is not a channel, has no name, or breaks the parent
/// invariant of `Channel::validate`.
pub fn map_channel(data: &Value) -> Result<Mapped, MapError> {
    let channel: DiscordChannel =
        serde_json::from_value(data.clone()).map_err(json_error("channel"))?;
    Ok(match upsert(&channel)? {
        Some(upsert) => Mapped::event(SourceEvent::ChannelUpsert(upsert)),
        None => Mapped::Ignored(Ignored::UnsupportedChannelType(u8::from(channel.kind))),
    })
}

/// Maps `CHANNEL_DELETE` to [`SourceEvent::ChannelDelete`], for the kinds teamctx stores.
///
/// # Errors
///
/// [`MapError`] if the payload is not a channel.
pub fn map_channel_delete(data: &Value) -> Result<Mapped, MapError> {
    let channel: DiscordChannel =
        serde_json::from_value(data.clone()).map_err(json_error("channel"))?;
    if kind_of(&channel).is_none() {
        return Ok(Mapped::Ignored(Ignored::UnsupportedChannelType(u8::from(
            channel.kind,
        ))));
    }
    Ok(Mapped::event(SourceEvent::ChannelDelete {
        channel: snowflake_from_id(channel.id)?,
    }))
}

/// Maps `THREAD_DELETE` to [`SourceEvent::ChannelDelete`].
///
/// # Errors
///
/// [`MapError`] if the payload is not a thread delete.
pub fn map_thread_delete(data: &Value) -> Result<Mapped, MapError> {
    let delete: ThreadDelete =
        serde_json::from_value(data.clone()).map_err(json_error("THREAD_DELETE"))?;
    Ok(Mapped::event(SourceEvent::ChannelDelete {
        channel: snowflake_from_id(delete.id)?,
    }))
}

/// Maps the `threads` array of a `THREAD_LIST_SYNC` payload or a REST thread list.
///
/// Each thread becomes a [`SourceEvent::ChannelUpsert`]; threads of kinds teamctx does not
/// store are skipped.
///
/// # Errors
///
/// [`MapError`] if the payload has no `threads` array or one entry is malformed.
pub fn map_thread_list(data: &Value) -> Result<Mapped, MapError> {
    map_channel_array(data.get("threads"), "threads")
}

/// Maps the `channels` and `threads` arrays of a `GUILD_CREATE` payload (plan section 7.2
/// step 1, discovery).
///
/// # Errors
///
/// [`MapError`] if an entry is malformed.
pub fn map_guild_create(data: &Value) -> Result<Mapped, MapError> {
    let mut events = Vec::new();
    for key in ["channels", "threads"] {
        if let Some(list) = data.get(key) {
            events.extend(map_channel_array(Some(list), key)?.into_events());
        }
    }
    Ok(Mapped::Events(events))
}

/// Maps a REST list of channel objects (`GET /guilds/{id}/channels`).
///
/// # Errors
///
/// [`MapError`] if the payload is not an array of channels.
pub fn map_rest_channels(data: &Value) -> Result<Mapped, MapError> {
    map_channel_array(Some(data), "channels")
}

/// Maps an array of channel objects, skipping unsupported kinds.
fn map_channel_array(list: Option<&Value>, what: &'static str) -> Result<Mapped, MapError> {
    let entries = list
        .and_then(Value::as_array)
        .ok_or_else(|| MapError::malformed(what, "expected an array of channels"))?;
    let mut events = Vec::with_capacity(entries.len());
    for entry in entries {
        events.extend(map_channel(entry)?.into_events());
    }
    Ok(Mapped::Events(events))
}

/// The core kind for a Discord channel, or `None` for kinds teamctx does not store.
///
/// Discord does not say what a thread's parent is. A public thread with an `applied_tags`
/// list (even an empty one) is a forum or media post, so it maps to
/// [`ChannelKind::ForumPost`]. Media channels map to [`ChannelKind::Forum`].
fn kind_of(channel: &DiscordChannel) -> Option<ChannelKind> {
    match channel.kind {
        ChannelType::GuildText => Some(ChannelKind::Text),
        ChannelType::GuildAnnouncement => Some(ChannelKind::Announcement),
        ChannelType::GuildForum | ChannelType::GuildMedia => Some(ChannelKind::Forum),
        ChannelType::PublicThread if channel.applied_tags.is_some() => Some(ChannelKind::ForumPost),
        ChannelType::PublicThread
        | ChannelType::PrivateThread
        | ChannelType::AnnouncementThread => Some(ChannelKind::Thread),
        _ => None,
    }
}

/// Builds the core channel, or `None` for unsupported kinds.
fn upsert(channel: &DiscordChannel) -> Result<Option<Channel>, MapError> {
    let Some(kind) = kind_of(channel) else {
        return Ok(None);
    };
    let name = channel
        .name
        .clone()
        .ok_or_else(|| MapError::malformed("name", "missing field"))?;
    // `parent_id` on a text channel is its category, which teamctx does not model.
    let parent = if kind.has_parent() {
        channel.parent_id.map(snowflake_from_id).transpose()?
    } else {
        None
    };
    let mapped = Channel {
        id: snowflake_from_id(channel.id)?,
        parent,
        kind,
        name,
        topic: channel
            .topic
            .clone()
            .filter(|topic| !topic.trim().is_empty()),
        included: true,
        archived: kind.has_parent()
            && channel
                .thread_metadata
                .as_ref()
                .is_some_and(|metadata| metadata.archived),
    };
    mapped.validate()?;
    Ok(Some(mapped))
}
