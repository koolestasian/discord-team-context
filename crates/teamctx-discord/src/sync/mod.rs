//! The REST sync algorithm of plan section 7.2: discovery, catch-up, backfill and the
//! recheck window, written against the read-only [`DiscordRest`] trait and the core
//! repository traits.
//!
//! There is no network, clock or sleeping here. The caller passes the current time, the
//! REST client does any rate-limit waiting, and failures come back as typed errors
//! ([`SyncError`]). The gateway, the live loop and the production REST adapter are separate
//! work; this module is what runs at start-up and after every gateway reconnect.
//!
//! # One run ([`sync_guild`])
//!
//! 1. **Discovery.** List the guild's channels, active threads and archived threads, mark
//!    each included or excluded from the allowlist and denylist, and store them. A stored
//!    top-level channel that Discord no longer lists is deleted (the store purges its
//!    messages and threads).
//! 2. **Per channel and thread**, in id order, each with its own cursor:
//!    - *Fresh* (no cursor): read the newest page, which sets `newest_seen`, then
//!      backfill older pages.
//!    - *Catch-up*: page `after=newest_seen` until a short page.
//!    - *Recheck*: re-read the last `recheck_window` (default 48 hours), upsert whatever
//!      changed and delete stored messages missing from Discord. Deletes only happen when
//!      the whole window was read, and never for messages newer than the newest one read,
//!      so a message that arrived mid-run is not mistaken for a deleted one.
//!    - *Backfill*: page `before=oldest_backfilled` until `backfill_days` or the channel
//!      start, then set `backfill_done`.
//!
//!    The cursor is saved after every page, so a run that stops half way resumes from the
//!    last saved page. Upserts are idempotent, so repeated work is harmless.
//! 3. A run reads each channel at most `*_page_limit` pages per phase, so a huge channel
//!    cannot starve the others. What is left is picked up by the next run.
//!
//! # Limits
//!
//! - Edits and deletes that happened while offline are only found inside the recheck
//!   window. Older ones stay as stored.
//! - Threads inherit the include or exclude decision of their parent channel.
//! - An archived thread is read again only if it has no cursor, its backfill is not done,
//!   or it was archived since the last run (one final catch-up). Archived threads cannot
//!   change without being unarchived, and that shows up as an active thread.
//! - To find stored messages in the recheck window the sync uses the stored neighbours of
//!   the newest message it holds ([`MessageRead::neighbours`]). If more than
//!   `RECHECK_STORED_LIMIT` stored messages would have to be compared, deletes are skipped
//!   for that channel and the report says the window was not covered.
//!
//! # Errors
//!
//! - Rate limits, Discord 5xx and transport errors stop the whole run
//!   ([`SyncError::Rest`], see [`SyncError::is_retryable`]). Progress is already saved.
//! - 401 stops the run: the token is wrong.
//! - 404 on a channel means it was deleted: the channel is removed and the run goes on.
//! - 403 on a channel skips it and goes on.
//! - A payload that does not map skips that channel for this run; the report records it
//!   without the payload text.

mod channel;
mod discovery;
mod rest;

use std::collections::HashMap;
use std::time::Duration;

use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{
    ApplyOutcome, ChannelInfo, DirectoryRead, IngestWrite, MessageRead, RepoError,
};
use teamctx_core::source::{ChannelKind, SourceEvent, SourceKind};
use teamctx_core::time::UnixMillis;
use thiserror::Error;

use crate::mapping::MapError;

pub use rest::{
    DiscordRest, MAX_MESSAGE_PAGE, MessagesRequest, PagePosition, RestError, ThreadVisibility,
};

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// Settings for one sync run (plan section 12: `[discord]` and `[sync]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    /// The guild to sync.
    pub guild: Snowflake,
    /// The guild's name (untrusted text), stored on the source row.
    pub guild_name: String,
    /// Allowlist of channel names or ids. Empty means every channel the bot can see.
    pub include_channels: Vec<String>,
    /// Denylist of channel names or ids. It wins over the allowlist.
    pub exclude_channels: Vec<String>,
    /// How far back backfill goes, in days. `0` stores no history before the first run.
    pub backfill_days: u32,
    /// How far back the recheck re-reads (`sync.recheck_window_hours`).
    pub recheck_window: Duration,
    /// Most message pages per channel for catch-up in one run.
    pub catchup_page_limit: u32,
    /// Most message pages per channel for backfill in one run (at least one is read, so
    /// a fresh channel always learns its newest message).
    pub backfill_page_limit: u32,
    /// Most message pages per channel for the recheck in one run.
    pub recheck_page_limit: u32,
}

impl SyncConfig {
    /// Defaults from plan section 12: 90 days of backfill and a 48 hour recheck window.
    #[must_use]
    pub fn new(guild: Snowflake, guild_name: impl Into<String>) -> Self {
        Self {
            guild,
            guild_name: guild_name.into(),
            include_channels: Vec::new(),
            exclude_channels: Vec::new(),
            backfill_days: 90,
            recheck_window: Duration::from_secs(48 * 3600),
            catchup_page_limit: 50,
            backfill_page_limit: 10,
            recheck_page_limit: 50,
        }
    }

    /// Whether a channel is included: the denylist wins, and an empty allowlist includes
    /// everything. Entries match the channel name or its id.
    #[must_use]
    pub fn includes_channel(&self, name: &str, id: Snowflake) -> bool {
        let id = id.to_string();
        let matches = |entry: &String| entry == name || *entry == id;
        !self.exclude_channels.iter().any(matches)
            && (self.include_channels.is_empty() || self.include_channels.iter().any(matches))
    }
}

/// Why a channel was not synced in this run, without any chat text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Discord answered 403: the bot cannot read it.
    Forbidden,
    /// A page did not map to events. Nothing from it was stored and the cursor stayed.
    MalformedPayload,
}

/// How one channel's sync ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChannelOutcome {
    /// The channel was read (maybe not completely: see the flags on the report).
    Synced,
    /// Discord answered 404, so the channel was deleted locally.
    Removed,
    /// The channel was skipped.
    Skipped(SkipReason),
}

/// What happened to one channel or thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelReport {
    /// The channel or thread.
    pub channel: Snowflake,
    /// How it ended.
    pub outcome: ChannelOutcome,
    /// Messages the store reported as new or changed.
    pub upserted: u64,
    /// Stored messages deleted because Discord no longer has them.
    pub deleted: u64,
    /// Message pages read.
    pub pages: u32,
    /// Catch-up reached the end of the channel.
    pub catchup_complete: bool,
    /// Backfill reached its limit or the channel start (now or earlier).
    pub backfill_complete: bool,
    /// The recheck window was read completely. `false` when it was not run, for example
    /// on a fresh channel.
    pub recheck_covered: bool,
}

impl ChannelReport {
    fn new(channel: Snowflake) -> Self {
        Self {
            channel,
            outcome: ChannelOutcome::Synced,
            upserted: 0,
            deleted: 0,
            pages: 0,
            catchup_complete: true,
            backfill_complete: false,
            recheck_covered: false,
        }
    }
}

/// The result of a full run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// The stored source row.
    pub source: SourceId,
    /// Top-level channels Discord listed.
    pub channels_listed: usize,
    /// Threads discovered (active and archived).
    pub threads_listed: usize,
    /// Top-level channels deleted because Discord no longer lists them.
    pub channels_removed: Vec<Snowflake>,
    /// One entry per channel and thread that was considered for message sync.
    pub channels: Vec<ChannelReport>,
}

impl SyncReport {
    /// Total messages stored or changed in this run.
    #[must_use]
    pub fn upserted(&self) -> u64 {
        self.channels.iter().map(|c| c.upserted).sum()
    }

    /// Total stored messages deleted in this run.
    #[must_use]
    pub fn deleted(&self) -> u64 {
        self.channels.iter().map(|c| c.deleted).sum()
    }

    /// The report for one channel or thread, if it was synced.
    #[must_use]
    pub fn channel(&self, id: Snowflake) -> Option<&ChannelReport> {
        self.channels.iter().find(|c| c.channel == id)
    }
}

/// Errors that stop a run. Progress made before the error is saved.
#[derive(Debug, Error)]
pub enum SyncError {
    /// A REST call failed in a way that stops the run.
    #[error("Discord REST call failed")]
    Rest {
        /// The channel being read, if the call was about one.
        channel: Option<Snowflake>,
        /// The cause.
        #[source]
        source: RestError,
    },
    /// The store failed.
    #[error("storage failed")]
    Store(#[from] RepoError),
    /// A discovery payload did not map to events.
    #[error("discovery payload is malformed")]
    Map(#[from] MapError),
    /// The store refused an event the sync had just prepared, for example a message for a
    /// channel it does not know. This is a bug, not a Discord problem.
    #[error("internal inconsistency: {0}")]
    Inconsistent(&'static str),
}

impl SyncError {
    /// Whether running the sync again later can succeed: rate limits, Discord server
    /// errors and transport failures.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Rest { source, .. } if source.is_retryable())
    }

    /// How long Discord asked us to wait, for a rate limit.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Rest {
                source: RestError::RateLimited { retry_after, .. },
                ..
            } => Some(*retry_after),
            _ => None,
        }
    }
}

/// Applies one event and turns an impossible outcome into an error.
fn apply<S: IngestWrite + ?Sized>(
    store: &mut S,
    source: SourceId,
    event: &SourceEvent,
    now: UnixMillis,
) -> Result<ApplyOutcome, SyncError> {
    Ok(store.apply_event(source, event, now)?)
}

/// Runs one sync of a guild at time `now` (see the module documentation).
///
/// # Errors
///
/// [`SyncError`] for failures that stop the run: a retryable REST error, a rejected token,
/// a storage failure or a malformed discovery payload.
pub fn sync_guild<R, S>(
    rest: &R,
    store: &mut S,
    config: &SyncConfig,
    now: UnixMillis,
) -> Result<SyncReport, SyncError>
where
    R: DiscordRest + ?Sized,
    S: IngestWrite + DirectoryRead + MessageRead + ?Sized,
{
    let source = store.upsert_source(SourceKind::Discord, config.guild, &config.guild_name)?;
    let before: HashMap<Snowflake, ChannelInfo> = store
        .channels(source)?
        .into_iter()
        .map(|info| (info.external_id, info))
        .collect();

    let found = discovery::discover(rest, store, source, config, now)?;

    let mut report = SyncReport {
        source,
        channels_listed: found.channels_listed,
        threads_listed: found.threads_listed,
        channels_removed: found.removed,
        channels: Vec::new(),
    };

    let mut stored = store.channels(source)?;
    stored.sort_by_key(|info| info.external_id);
    for info in stored {
        if !info.included || info.kind == ChannelKind::Forum {
            continue;
        }
        if info.archived {
            let cursor = store.cursor(source, info.external_id)?;
            let was_active = before
                .get(&info.external_id)
                .is_some_and(|previous| !previous.archived);
            let finished = cursor.is_some_and(|c| c.backfill_done && c.newest_seen.is_some());
            if finished && !was_active {
                continue;
            }
        }
        let run = channel::ChannelSync::new(rest, store, source, config, now, info.external_id);
        report.channels.push(run.run()?);
    }
    Ok(report)
}

/// The time `days` before `now`.
fn days_before(now: UnixMillis, days: u32) -> UnixMillis {
    UnixMillis::from_millis(now.as_millis().saturating_sub(i64::from(days) * DAY_MS))
}

/// The time `window` before `now`, saturating.
fn window_before(now: UnixMillis, window: Duration) -> UnixMillis {
    let millis = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
    UnixMillis::from_millis(now.as_millis().saturating_sub(millis))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(raw: u64) -> Snowflake {
        Snowflake::new(raw).unwrap()
    }

    #[test]
    fn channel_filters_follow_the_config_rules() {
        let mut config = SyncConfig::new(sf(1), "Example Guild");
        assert!(config.includes_channel("general", sf(10)));

        config.exclude_channels = vec!["random".to_owned(), "20".to_owned()];
        assert!(!config.includes_channel("random", sf(11)));
        assert!(!config.includes_channel("other", sf(20)));
        assert!(config.includes_channel("general", sf(10)));

        config.include_channels = vec!["general".to_owned(), "random".to_owned()];
        assert!(config.includes_channel("general", sf(10)));
        assert!(!config.includes_channel("random", sf(11)), "denylist wins");
        assert!(!config.includes_channel("dev", sf(12)));
    }

    #[test]
    fn window_helpers_saturate() {
        let now = UnixMillis::from_millis(10 * DAY_MS);
        assert_eq!(days_before(now, 2).as_millis(), 8 * DAY_MS);
        assert_eq!(
            window_before(now, Duration::from_secs(3600)).as_millis(),
            10 * DAY_MS - 3_600_000
        );
        assert_eq!(
            window_before(UnixMillis::from_millis(0), Duration::MAX).as_millis(),
            0_i64.saturating_sub(i64::MAX)
        );
    }

    #[test]
    fn only_transient_sync_errors_report_retry() {
        let limited = SyncError::Rest {
            channel: None,
            source: RestError::RateLimited {
                retry_after: Duration::from_secs(7),
                global: true,
            },
        };
        assert!(limited.is_retryable());
        assert_eq!(limited.retry_after(), Some(Duration::from_secs(7)));
        let unauthorized = SyncError::Rest {
            channel: Some(sf(5)),
            source: RestError::Unauthorized,
        };
        assert!(!unauthorized.is_retryable());
        assert_eq!(unauthorized.retry_after(), None);
        assert!(!SyncError::Inconsistent("x").is_retryable());
    }
}
