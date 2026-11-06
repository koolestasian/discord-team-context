//! An in-memory Discord for the sync tests: it implements only the read-only
//! `DiscordRest` trait, pages messages like the real API and can be told to fail.
//! Every name and id is fictional.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::Duration;

use serde_json::{Value, json};
use teamctx_core::id::{DISCORD_EPOCH_MS, Snowflake};
use teamctx_discord::sync::{
    DiscordRest, MessagesRequest, PagePosition, RestError, ThreadVisibility,
};

pub(crate) const GUILD: u64 = 1_100_000_000_000_000_001;
/// The time of the first sync in the tests, in Unix milliseconds.
pub(crate) const NOW_MS: i64 = 1_800_000_000_000;
pub(crate) const MINUTE: i64 = 60_000;
pub(crate) const HOUR: i64 = 60 * MINUTE;
pub(crate) const DAY: i64 = 24 * HOUR;

/// A message id for a message created `ms_before_now` before [`NOW_MS`] (negative for
/// later). Ids made at the same millisecond collide, so tests use distinct offsets.
pub(crate) fn id_at(ms_before_now: i64) -> u64 {
    let since_epoch = u64::try_from(NOW_MS - ms_before_now - DISCORD_EPOCH_MS).unwrap_or(0);
    since_epoch << 22
}

/// How an injected failure looks.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Fail {
    RateLimited,
    Server,
    Unauthorized,
}

impl Fail {
    fn error(self) -> RestError {
        match self {
            Self::RateLimited => RestError::RateLimited {
                retry_after: Duration::from_secs(9),
                global: false,
            },
            Self::Server => RestError::Server { status: 503 },
            Self::Unauthorized => RestError::Unauthorized,
        }
    }
}

struct ChannelRecord {
    parent: Option<u64>,
    name: String,
    archived: bool,
    gone: bool,
}

#[derive(Default)]
struct Inner {
    channels: BTreeMap<u64, ChannelRecord>,
    messages: HashMap<u64, BTreeMap<u64, Value>>,
    forbidden: HashSet<u64>,
    malformed: HashSet<u64>,
    /// channel -> (calls still allowed, failure)
    failures: HashMap<u64, (usize, Fail)>,
    requests: Vec<MessagesRequest>,
    other_calls: Vec<String>,
    hide_listing: bool,
}

#[derive(Default)]
pub(crate) struct FakeDiscord {
    inner: RefCell<Inner>,
}

const ARCHIVED_AT: &str = "2027-01-15T08:00:00.000000+00:00";

impl FakeDiscord {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn add_channel(&self, id: u64, name: &str) {
        self.insert(id, None, name, false);
    }

    pub(crate) fn add_thread(&self, id: u64, parent: u64, name: &str, archived: bool) {
        self.insert(id, Some(parent), name, archived);
    }

    fn insert(&self, id: u64, parent: Option<u64>, name: &str, archived: bool) {
        self.inner.borrow_mut().channels.insert(
            id,
            ChannelRecord {
                parent,
                name: name.to_owned(),
                archived,
                gone: false,
            },
        );
    }

    pub(crate) fn archive_thread(&self, id: u64) {
        if let Some(record) = self.inner.borrow_mut().channels.get_mut(&id) {
            record.archived = true;
        }
    }

    /// Deletes a channel or thread: no longer listed, and its messages answer 404.
    pub(crate) fn remove(&self, id: u64) {
        let mut inner = self.inner.borrow_mut();
        let children: Vec<u64> = inner
            .channels
            .iter()
            .filter(|(_, record)| record.parent == Some(id))
            .map(|(child, _)| *child)
            .collect();
        for gone in std::iter::once(id).chain(children) {
            if let Some(record) = inner.channels.get_mut(&gone) {
                record.gone = true;
            }
        }
    }

    pub(crate) fn add_message(&self, channel: u64, id: u64, content: &str) {
        self.put(channel, id, 0, content);
    }

    /// A system message (type 7, a member join): it maps to no event.
    pub(crate) fn add_system_message(&self, channel: u64, id: u64) {
        self.put(channel, id, 7, "");
    }

    fn put(&self, channel: u64, id: u64, kind: u8, content: &str) {
        let value = json!({
            "id": id.to_string(),
            "channel_id": channel.to_string(),
            "type": kind,
            "content": content,
            "author": {
                "id": "1100000000000000201",
                "username": "robin_example",
                "global_name": "Robin Example",
                "discriminator": "0",
                "avatar": null,
                "bot": false
            },
            "mentions": [],
            "mention_roles": [],
            "attachments": [],
            "embeds": [],
            "pinned": false,
            "timestamp": ARCHIVED_AT,
            "edited_timestamp": null
        });
        self.inner
            .borrow_mut()
            .messages
            .entry(channel)
            .or_default()
            .insert(id, value);
    }

    pub(crate) fn edit_message(&self, channel: u64, id: u64, content: &str) {
        let mut inner = self.inner.borrow_mut();
        if let Some(message) = inner
            .messages
            .get_mut(&channel)
            .and_then(|messages| messages.get_mut(&id))
        {
            message["content"] = json!(content);
            message["edited_timestamp"] = json!(ARCHIVED_AT);
        }
    }

    pub(crate) fn delete_message(&self, channel: u64, id: u64) {
        if let Some(messages) = self.inner.borrow_mut().messages.get_mut(&channel) {
            messages.remove(&id);
        }
    }

    pub(crate) fn forbid(&self, channel: u64) {
        self.inner.borrow_mut().forbidden.insert(channel);
    }

    /// Makes message pages of this channel something that is not a message list.
    pub(crate) fn serve_garbage(&self, channel: u64) {
        self.inner.borrow_mut().malformed.insert(channel);
    }

    /// After `ok_calls` more successful message requests for the channel, every request
    /// for it fails with `fail` until [`Self::clear_failures`].
    pub(crate) fn fail_after(&self, channel: u64, ok_calls: usize, fail: Fail) {
        self.inner
            .borrow_mut()
            .failures
            .insert(channel, (ok_calls, fail));
    }

    pub(crate) fn clear_failures(&self) {
        self.inner.borrow_mut().failures.clear();
    }

    /// Every message request made so far for a channel, in order.
    pub(crate) fn requests(&self, channel: u64) -> Vec<MessagesRequest> {
        self.inner
            .borrow()
            .requests
            .iter()
            .filter(|request| request.channel.get() == channel)
            .copied()
            .collect()
    }

    /// Makes the guild channel listing come back empty, as a glitchy response might.
    pub(crate) fn hide_channel_listing(&self) {
        self.inner.borrow_mut().hide_listing = true;
    }

    /// The discovery calls made so far, as text.
    pub(crate) fn discovery_calls(&self) -> Vec<String> {
        self.inner.borrow().other_calls.clone()
    }

    pub(crate) fn reset_log(&self) {
        let mut inner = self.inner.borrow_mut();
        inner.requests.clear();
        inner.other_calls.clear();
    }

    pub(crate) fn message_request_count(&self) -> usize {
        self.inner.borrow().requests.len()
    }

    fn channel_json(&self, id: u64, record: &ChannelRecord) -> Value {
        match record.parent {
            None => json!({
                "id": id.to_string(),
                "guild_id": GUILD.to_string(),
                "type": 0,
                "name": record.name,
                "position": 0,
                "nsfw": false,
                "permission_overwrites": [],
                "flags": 0
            }),
            Some(parent) => json!({
                "id": id.to_string(),
                "guild_id": GUILD.to_string(),
                "parent_id": parent.to_string(),
                "owner_id": "1100000000000000201",
                "type": 11,
                "name": record.name,
                "thread_metadata": {
                    "archived": record.archived,
                    "archive_timestamp": ARCHIVED_AT,
                    "auto_archive_duration": 1440,
                    "locked": false
                }
            }),
        }
    }
}

impl DiscordRest for FakeDiscord {
    fn guild_channels(&self, guild: Snowflake) -> Result<Value, RestError> {
        let mut inner = self.inner.borrow_mut();
        inner.other_calls.push(format!("guild_channels {guild}"));
        let list: Vec<Value> = inner
            .channels
            .iter()
            .filter(|(_, record)| record.parent.is_none() && !record.gone && !inner.hide_listing)
            .map(|(id, record)| self.channel_json(*id, record))
            .collect();
        Ok(Value::Array(list))
    }

    fn active_threads(&self, guild: Snowflake) -> Result<Value, RestError> {
        let mut inner = self.inner.borrow_mut();
        inner.other_calls.push(format!("active_threads {guild}"));
        let threads: Vec<Value> = inner
            .channels
            .iter()
            .filter(|(_, r)| r.parent.is_some() && !r.archived && !r.gone)
            .map(|(id, record)| self.channel_json(*id, record))
            .collect();
        Ok(json!({ "threads": threads, "members": [] }))
    }

    fn archived_threads(
        &self,
        channel: Snowflake,
        visibility: ThreadVisibility,
        _before: Option<&str>,
    ) -> Result<Value, RestError> {
        let mut inner = self.inner.borrow_mut();
        inner
            .other_calls
            .push(format!("archived_threads {channel} {visibility:?}"));
        if visibility == ThreadVisibility::Private {
            return Err(RestError::Forbidden);
        }
        let threads: Vec<Value> = inner
            .channels
            .iter()
            .filter(|(_, r)| r.parent == Some(channel.get()) && r.archived && !r.gone)
            .map(|(id, record)| self.channel_json(*id, record))
            .collect();
        Ok(json!({ "threads": threads, "members": [], "has_more": false }))
    }

    fn channel_messages(&self, request: &MessagesRequest) -> Result<Value, RestError> {
        let mut inner = self.inner.borrow_mut();
        inner.requests.push(*request);
        let channel = request.channel.get();
        if let Some((remaining, fail)) = inner.failures.get_mut(&channel) {
            if *remaining == 0 {
                return Err(fail.error());
            }
            *remaining -= 1;
        }
        if inner
            .channels
            .get(&channel)
            .is_none_or(|record| record.gone)
        {
            return Err(RestError::NotFound);
        }
        if inner.forbidden.contains(&channel) {
            return Err(RestError::Forbidden);
        }
        if inner.malformed.contains(&channel) {
            return Ok(json!({"message": "not a list"}));
        }
        let empty = BTreeMap::new();
        let messages = inner.messages.get(&channel).unwrap_or(&empty);
        let limit = usize::from(request.limit);
        let page: Vec<Value> = match request.position {
            PagePosition::Latest => messages.values().rev().take(limit).cloned().collect(),
            PagePosition::Before(id) => messages
                .range(..id.get())
                .rev()
                .take(limit)
                .map(|(_, value)| value.clone())
                .collect(),
            PagePosition::After(id) => {
                let mut oldest_first: Vec<Value> = messages
                    .range(id.get() + 1..)
                    .take(limit)
                    .map(|(_, value)| value.clone())
                    .collect();
                oldest_first.reverse();
                oldest_first
            }
        };
        Ok(Value::Array(page))
    }
}
