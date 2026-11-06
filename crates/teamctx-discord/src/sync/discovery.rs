//! Step 1 of the sync (plan section 7.2): channel and thread discovery.

use std::collections::{HashMap, HashSet};

use serde_json::Value;
use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{ApplyOutcome, DirectoryRead, IngestWrite};
use teamctx_core::source::{ChannelKind, SourceEvent};
use teamctx_core::time::UnixMillis;

use super::{DiscordRest, RestError, SyncConfig, SyncError, ThreadVisibility, apply};
use crate::mapping::{map_rest_channels, map_thread_list};

/// The most pages of archived threads read per channel and visibility.
const MAX_ARCHIVED_PAGES: u32 = 50;

/// What discovery found.
pub(super) struct Discovered {
    pub(super) channels_listed: usize,
    pub(super) threads_listed: usize,
    pub(super) removed: Vec<Snowflake>,
}

/// Lists channels, active threads and archived threads, stores them with their inclusion
/// flag and deletes top-level channels Discord no longer lists.
pub(super) fn discover<R, S>(
    rest: &R,
    store: &mut S,
    source: SourceId,
    config: &SyncConfig,
    now: UnixMillis,
) -> Result<Discovered, SyncError>
where
    R: DiscordRest + ?Sized,
    S: IngestWrite + DirectoryRead + ?Sized,
{
    let listing = rest
        .guild_channels(config.guild)
        .map_err(|source| SyncError::Rest {
            channel: None,
            source,
        })?;

    // Top-level channels first: threads need their parent stored.
    let mut parents: HashMap<Snowflake, bool> = HashMap::new();
    let mut thread_parents: Vec<Snowflake> = Vec::new();
    for event in map_rest_channels(&listing)?.into_events() {
        let SourceEvent::ChannelUpsert(mut channel) = event else {
            continue;
        };
        channel.included = config.includes_channel(&channel.name, channel.id);
        parents.insert(channel.id, channel.included);
        if channel.included && has_threads(channel.kind) {
            thread_parents.push(channel.id);
        }
        apply(store, source, &SourceEvent::ChannelUpsert(channel), now)?;
    }

    let mut removed = Vec::new();
    if !parents.is_empty() {
        for stored in store.channels(source)? {
            if stored.parent.is_none() && !parents.contains_key(&stored.external_id) {
                let event = SourceEvent::ChannelDelete {
                    channel: stored.external_id,
                };
                apply(store, source, &event, now)?;
                removed.push(stored.external_id);
            }
        }
    }

    let mut seen_threads: HashSet<Snowflake> = HashSet::new();
    let active = rest
        .active_threads(config.guild)
        .map_err(|source| SyncError::Rest {
            channel: None,
            source,
        })?;
    store_threads(store, source, &active, &parents, now, &mut seen_threads)?;

    for parent in thread_parents {
        for visibility in [ThreadVisibility::Public, ThreadVisibility::Private] {
            list_archived(
                rest,
                store,
                source,
                parent,
                visibility,
                &parents,
                now,
                &mut seen_threads,
            )?;
        }
    }

    Ok(Discovered {
        channels_listed: parents.len(),
        threads_listed: seen_threads.len(),
        removed,
    })
}

/// Whether threads or forum posts can live under this kind.
fn has_threads(kind: ChannelKind) -> bool {
    matches!(
        kind,
        ChannelKind::Text | ChannelKind::Announcement | ChannelKind::Forum
    )
}

/// Stores the threads in a thread-list payload. A thread inherits its parent's inclusion;
/// one whose parent is unknown (a kind teamctx does not store) is skipped.
fn store_threads<S>(
    store: &mut S,
    source: SourceId,
    payload: &Value,
    parents: &HashMap<Snowflake, bool>,
    now: UnixMillis,
    seen: &mut HashSet<Snowflake>,
) -> Result<(), SyncError>
where
    S: IngestWrite + ?Sized,
{
    for event in map_thread_list(payload)?.into_events() {
        let SourceEvent::ChannelUpsert(mut thread) = event else {
            continue;
        };
        let Some(&included) = thread.parent.and_then(|parent| parents.get(&parent)) else {
            continue;
        };
        thread.included = included;
        let id = thread.id;
        let outcome = apply(store, source, &SourceEvent::ChannelUpsert(thread), now)?;
        if !matches!(outcome, ApplyOutcome::UnknownChannel(_)) {
            seen.insert(id);
        }
    }
    Ok(())
}

/// Pages through one channel's archived threads of one visibility.
#[expect(
    clippy::too_many_arguments,
    reason = "a private helper called from one place; a struct would only rename the arguments"
)]
fn list_archived<R, S>(
    rest: &R,
    store: &mut S,
    source: SourceId,
    parent: Snowflake,
    visibility: ThreadVisibility,
    parents: &HashMap<Snowflake, bool>,
    now: UnixMillis,
    seen: &mut HashSet<Snowflake>,
) -> Result<(), SyncError>
where
    R: DiscordRest + ?Sized,
    S: IngestWrite + ?Sized,
{
    let mut before: Option<String> = None;
    for _ in 0..MAX_ARCHIVED_PAGES {
        let page = match rest.archived_threads(parent, visibility, before.as_deref()) {
            Ok(page) => page,
            // Private archived threads need Manage Threads, and a channel can vanish
            // between listing and here. Neither stops the run.
            Err(RestError::Forbidden | RestError::NotFound) => return Ok(()),
            Err(source) => {
                return Err(SyncError::Rest {
                    channel: Some(parent),
                    source,
                });
            }
        };
        store_threads(store, source, &page, parents, now, seen)?;
        let has_more = page
            .get("has_more")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let next = last_archive_timestamp(&page);
        match next {
            Some(next) if has_more && before.as_deref() != Some(next.as_str()) => {
                before = Some(next);
            }
            _ => return Ok(()),
        }
    }
    Ok(())
}

/// The `archive_timestamp` of the last thread of a page, the `before` value for the next.
fn last_archive_timestamp(page: &Value) -> Option<String> {
    page.get("threads")?
        .as_array()?
        .last()?
        .get("thread_metadata")?
        .get("archive_timestamp")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn next_archive_cursor_comes_from_the_last_thread() {
        let page = json!({"threads": [
            {"thread_metadata": {"archive_timestamp": "2026-03-04T10:10:00.000000+00:00"}},
            {"thread_metadata": {"archive_timestamp": "2026-03-03T10:10:00.000000+00:00"}},
        ]});
        assert_eq!(
            last_archive_timestamp(&page).as_deref(),
            Some("2026-03-03T10:10:00.000000+00:00")
        );
        assert_eq!(last_archive_timestamp(&json!({"threads": []})), None);
        assert_eq!(last_archive_timestamp(&json!({})), None);
    }

    #[test]
    fn only_text_like_channels_hold_threads() {
        assert!(has_threads(ChannelKind::Text));
        assert!(has_threads(ChannelKind::Forum));
        assert!(has_threads(ChannelKind::Announcement));
        assert!(!has_threads(ChannelKind::Thread));
        assert!(!has_threads(ChannelKind::ForumPost));
    }
}
