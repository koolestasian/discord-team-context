//! Steps 2 and 3 of the sync (plan section 7.2) for one channel or thread: catch-up,
//! recheck window and backfill, each saving the cursor after every page.

use std::collections::HashSet;

use serde_json::Value;
use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{
    ApplyOutcome, DirectoryRead, IngestWrite, MessageCursor, MessageRead, RepoError, SyncCursor,
};
use teamctx_core::source::{MessageRef, SourceEvent};
use teamctx_core::time::UnixMillis;

use super::{
    ChannelOutcome, ChannelReport, DiscordRest, MAX_MESSAGE_PAGE, MessagesRequest, PagePosition,
    RestError, SkipReason, SyncConfig, SyncError, apply, days_before, window_before,
};
use crate::mapping::map_rest_messages;

/// Stored messages read per page when the recheck compares the stored side of its window.
/// The window is read page by page, so its size is not limited by this.
const RECHECK_STORED_PAGE: u32 = 500;

/// Why work on one channel stopped early.
enum Stop {
    /// 404: deleted on Discord.
    Gone,
    /// 403: not readable.
    Forbidden,
    /// A page did not map.
    Malformed,
    /// Stops the whole run.
    Fatal(SyncError),
}

impl From<RepoError> for Stop {
    fn from(error: RepoError) -> Self {
        Self::Fatal(SyncError::Store(error))
    }
}

impl From<SyncError> for Stop {
    fn from(error: SyncError) -> Self {
        Self::Fatal(error)
    }
}

/// One page of messages: every id in it, and the events for the messages we store.
struct Page {
    /// Ids of every message object, including system messages we do not store, so the
    /// cursor can move past a page that holds nothing storable.
    ids: Vec<Snowflake>,
    /// `MessageUpsert` events, oldest first.
    events: Vec<SourceEvent>,
}

impl Page {
    fn parse(value: &Value) -> Result<Self, Stop> {
        let entries = value.as_array().ok_or(Stop::Malformed)?;
        let mut ids = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .and_then(|text| text.parse::<Snowflake>().ok())
                .ok_or(Stop::Malformed)?;
            ids.push(id);
        }
        let mut events = map_rest_messages(value)
            .map_err(|_| Stop::Malformed)?
            .into_events();
        events.reverse();
        Ok(Self { ids, events })
    }

    fn newest(&self) -> Option<Snowflake> {
        self.ids.iter().copied().max()
    }

    fn oldest(&self) -> Option<Snowflake> {
        self.ids.iter().copied().min()
    }

    fn is_full(&self) -> bool {
        self.ids.len() >= usize::from(MAX_MESSAGE_PAGE)
    }

    /// The upserts for messages created at or after `since`.
    fn events_since(&self, since: UnixMillis) -> Vec<&SourceEvent> {
        self.events
            .iter()
            .filter(|event| match event {
                SourceEvent::MessageUpsert(message) => message.created_at >= since,
                _ => true,
            })
            .collect()
    }
}

/// Syncs one channel.
pub(super) struct ChannelSync<'a, R: ?Sized, S: ?Sized> {
    rest: &'a R,
    store: &'a mut S,
    source: SourceId,
    config: &'a SyncConfig,
    now: UnixMillis,
    channel: Snowflake,
    /// The thread was archived since the last run and is owed a final catch-up.
    final_catch_up: bool,
    report: ChannelReport,
}

impl<'a, R, S> ChannelSync<'a, R, S>
where
    R: DiscordRest + ?Sized,
    S: IngestWrite + DirectoryRead + MessageRead + ?Sized,
{
    pub(super) fn new(
        rest: &'a R,
        store: &'a mut S,
        source: SourceId,
        config: &'a SyncConfig,
        now: UnixMillis,
        channel: Snowflake,
        final_catch_up: bool,
    ) -> Self {
        Self {
            rest,
            store,
            source,
            config,
            now,
            channel,
            final_catch_up,
            report: ChannelReport::new(channel),
        }
    }

    /// Runs catch-up, recheck and backfill.
    pub(super) fn run(mut self) -> Result<ChannelReport, SyncError> {
        let channel = self.channel;
        match self.sync(channel) {
            Ok(()) => {}
            Err(Stop::Gone) => {
                self.apply(&SourceEvent::ChannelDelete { channel })?;
                self.report.outcome = ChannelOutcome::Removed;
            }
            Err(Stop::Forbidden) => {
                self.report.outcome = ChannelOutcome::Skipped(SkipReason::Forbidden);
            }
            Err(Stop::Malformed) => {
                self.report.outcome = ChannelOutcome::Skipped(SkipReason::MalformedPayload);
            }
            Err(Stop::Fatal(error)) => return Err(error),
        }
        Ok(self.report)
    }

    fn sync(&mut self, channel: Snowflake) -> Result<(), Stop> {
        let stored = self.store.cursor(self.source, channel)?;
        let mut cursor = stored.unwrap_or(SyncCursor {
            newest_seen: None,
            oldest_backfilled: None,
            backfill_done: false,
            updated_at: self.now,
        });
        let fresh = cursor.newest_seen.is_none();
        let mut backfill_finished = cursor.backfill_done;
        let mut final_catch_up_open = false;
        if fresh {
            // Nothing was read yet, so any earlier backfill progress means nothing.
            cursor.oldest_backfilled = None;
            cursor.backfill_done = false;
            backfill_finished = false;
        } else {
            if self.final_catch_up && backfill_finished {
                // Discovery already stored `archived`, so a later run no longer knows a
                // final catch-up is owed. Record the debt in the cursor (a thread with an
                // unfinished backfill is always read again) and pay it off below.
                cursor.backfill_done = false;
                self.save(channel, &mut cursor)?;
                final_catch_up_open = true;
            }
            self.catch_up(channel, &mut cursor)?;
            if final_catch_up_open && self.report.catchup_complete {
                cursor.backfill_done = true;
                self.save(channel, &mut cursor)?;
                final_catch_up_open = false;
            }
            self.recheck(channel, &cursor)?;
        }
        if fresh || (!cursor.backfill_done && !final_catch_up_open) {
            self.backfill(channel, &mut cursor)?;
            backfill_finished = cursor.backfill_done;
        }
        self.report.backfill_complete = backfill_finished;
        Ok(())
    }

    /// Pages `after=newest_seen` until a short page, saving the cursor after each.
    fn catch_up(&mut self, channel: Snowflake, cursor: &mut SyncCursor) -> Result<(), Stop> {
        let mut pages = 0;
        while let Some(newest) = cursor.newest_seen {
            if pages >= self.config.catchup_page_limit {
                self.report.catchup_complete = false;
                break;
            }
            pages += 1;
            let page = self.fetch(channel, PagePosition::After(newest))?;
            let Some(page_newest) = page.newest() else {
                break;
            };
            self.upsert(page.events_since(UnixMillis::from_millis(i64::MIN)))?;
            cursor.newest_seen = Some(newest.max(page_newest));
            self.save(channel, cursor)?;
            if !page.is_full() {
                break;
            }
        }
        Ok(())
    }

    /// Re-reads the last `recheck_window`, upserts changes and deletes stored messages
    /// Discord no longer has, when the whole window was read.
    fn recheck(&mut self, channel: Snowflake, cursor: &SyncCursor) -> Result<(), Stop> {
        let window_start = window_before(self.now, self.config.recheck_window);
        let mut in_window: HashSet<Snowflake> = HashSet::new();
        let mut newest_read: Option<Snowflake> = None;
        let mut position = PagePosition::Latest;
        let mut covered = false;
        for _ in 0..self.config.recheck_page_limit {
            let page = self.fetch(channel, position)?;
            let (Some(newest), Some(oldest)) = (page.newest(), page.oldest()) else {
                covered = true;
                break;
            };
            newest_read = newest_read.max(Some(newest));
            self.upsert(page.events_since(window_start))?;
            in_window.extend(
                page.ids
                    .iter()
                    .copied()
                    .filter(|id| id.timestamp() >= window_start),
            );
            if oldest.timestamp() < window_start || !page.is_full() {
                covered = true;
                break;
            }
            position = PagePosition::Before(oldest);
        }
        if covered {
            self.delete_missing(channel, cursor, window_start, newest_read, &in_window)?;
        }
        self.report.recheck_covered = covered;
        Ok(())
    }

    /// Deletes stored messages in the window that are not in `in_window`. The stored side
    /// of the window is read page by page, oldest first, so its size has no limit.
    fn delete_missing(
        &mut self,
        channel: Snowflake,
        cursor: &SyncCursor,
        window_start: UnixMillis,
        newest_read: Option<Snowflake>,
        in_window: &HashSet<Snowflake>,
    ) -> Result<(), Stop> {
        // Anything newer than the newest message read may have arrived after the read.
        let Some(upper) = newest_read.max(cursor.newest_seen) else {
            return Ok(());
        };
        let Some(row) = self
            .store
            .channels(self.source)?
            .into_iter()
            .find(|info| info.external_id == channel)
        else {
            return Ok(());
        };
        let page_size = usize::try_from(RECHECK_STORED_PAGE).unwrap_or(usize::MAX);
        // The page position is a value, so deleting the messages of a page does not move
        // the next page.
        let mut position = None;
        loop {
            let page = self.store.channel_messages_since(
                row.id,
                window_start,
                RECHECK_STORED_PAGE,
                position,
            )?;
            for stored in &page {
                if stored.external_id <= upper && !in_window.contains(&stored.external_id) {
                    let event = SourceEvent::MessageDelete {
                        message: MessageRef {
                            channel,
                            message: stored.external_id,
                        },
                    };
                    if self.apply(&event)? == ApplyOutcome::Applied {
                        self.report.deleted += 1;
                    }
                }
            }
            match page.last() {
                Some(last) if page.len() >= page_size => position = Some(MessageCursor::from(last)),
                _ => return Ok(()),
            }
        }
    }

    /// Pages `before=` the oldest message read until `backfill_days` or the channel start.
    /// For a fresh channel the first page is the newest one and sets `newest_seen`.
    fn backfill(&mut self, channel: Snowflake, cursor: &mut SyncCursor) -> Result<(), Stop> {
        let cutoff = days_before(self.now, self.config.backfill_days);
        let limit = self.config.backfill_page_limit.max(1);
        let mut pages = 0;
        while !cursor.backfill_done && pages < limit {
            pages += 1;
            let position = match cursor.oldest_backfilled.or(cursor.newest_seen) {
                Some(oldest) => PagePosition::Before(oldest),
                None => PagePosition::Latest,
            };
            let page = self.fetch(channel, position)?;
            let (Some(newest), Some(oldest)) = (page.newest(), page.oldest()) else {
                cursor.backfill_done = true;
                self.save(channel, cursor)?;
                break;
            };
            self.upsert(page.events_since(cutoff))?;
            cursor.newest_seen = cursor.newest_seen.max(Some(newest));
            cursor.oldest_backfilled = Some(oldest);
            cursor.backfill_done = oldest.timestamp() < cutoff || !page.is_full();
            self.save(channel, cursor)?;
        }
        Ok(())
    }

    /// Reads one page, translating REST errors into [`Stop`].
    fn fetch(&mut self, channel: Snowflake, position: PagePosition) -> Result<Page, Stop> {
        let request = MessagesRequest {
            channel,
            position,
            limit: MAX_MESSAGE_PAGE,
        };
        let value = self
            .rest
            .channel_messages(&request)
            .map_err(|error| match error {
                RestError::NotFound => Stop::Gone,
                RestError::Forbidden => Stop::Forbidden,
                other => Stop::Fatal(SyncError::Rest {
                    channel: Some(channel),
                    source: other,
                }),
            })?;
        self.report.pages += 1;
        Page::parse(&value)
    }

    /// Applies upserts and counts the ones that changed something.
    fn upsert(&mut self, events: Vec<&SourceEvent>) -> Result<(), Stop> {
        for event in events {
            match self.apply(event)? {
                ApplyOutcome::Applied => self.report.upserted += 1,
                ApplyOutcome::UnknownChannel(_) => {
                    return Err(Stop::Fatal(SyncError::Inconsistent(
                        "message for a channel that was not stored",
                    )));
                }
                ApplyOutcome::Unchanged | ApplyOutcome::Excluded | ApplyOutcome::NeedsFetch(_) => {}
            }
        }
        Ok(())
    }

    fn apply(&mut self, event: &SourceEvent) -> Result<ApplyOutcome, SyncError> {
        apply(self.store, self.source, event, self.now)
    }

    fn save(&mut self, channel: Snowflake, cursor: &mut SyncCursor) -> Result<(), Stop> {
        cursor.updated_at = self.now;
        self.store.set_cursor(self.source, channel, cursor)?;
        Ok(())
    }
}
