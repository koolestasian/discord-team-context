//! Tests of the sync algorithm (plan section 7.2) against a fake read-only REST client and
//! a real in-memory store. No network, no sleeping: failures are injected and the clock is
//! a parameter.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

mod fake;

use std::time::Duration;

use fake::{DAY, Fail, FakeDiscord, GUILD, HOUR, MINUTE, NOW_MS, id_at};
use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{ChannelInfo, DirectoryRead, IngestWrite, MessageRead, StoredMessage};
use teamctx_core::source::{ChannelKind, Member, Message, MessageRef, SourceEvent};
use teamctx_core::time::UnixMillis;
use teamctx_discord::sync::{
    ChannelOutcome, PagePosition, SkipReason, SyncConfig, SyncError, SyncReport, sync_guild,
};
use teamctx_store::Store;

const GENERAL: u64 = 1_100_000_000_000_000_101;
const DEV: u64 = 1_100_000_000_000_000_102;
const THREAD: u64 = 1_100_000_000_000_000_501;
const THREAD_2: u64 = 1_100_000_000_000_000_502;
const DEV_THREAD: u64 = 1_100_000_000_000_000_503;

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).unwrap()
}

fn at(offset_ms: i64) -> UnixMillis {
    UnixMillis::from_millis(NOW_MS + offset_ms)
}

fn config() -> SyncConfig {
    SyncConfig::new(sf(GUILD), "Example Guild")
}

fn run(fake: &FakeDiscord, store: &mut Store, config: &SyncConfig, now: UnixMillis) -> SyncReport {
    sync_guild(fake, store, config, now).unwrap()
}

fn stored(store: &Store, source: SourceId, channel: u64, id: u64) -> Option<StoredMessage> {
    store
        .message_by_ref(
            source,
            MessageRef {
                channel: sf(channel),
                message: sf(id),
            },
        )
        .unwrap()
}

fn stored_count(store: &Store, source: SourceId, channel: u64, ids: &[u64]) -> usize {
    ids.iter()
        .filter(|id| stored(store, source, channel, **id).is_some())
        .count()
}

fn channel_info(store: &Store, source: SourceId, id: u64) -> Option<ChannelInfo> {
    store
        .channels(source)
        .unwrap()
        .into_iter()
        .find(|info| info.external_id == sf(id))
}

fn two_channels() -> FakeDiscord {
    let fake = FakeDiscord::new();
    fake.add_channel(GENERAL, "general");
    fake.add_channel(DEV, "dev");
    fake
}

/// `count` messages, one every `step_ms`, the newest `first_ms_before_now` old.
fn fill(
    fake: &FakeDiscord,
    channel: u64,
    count: i64,
    first_ms_before_now: i64,
    step_ms: i64,
) -> Vec<u64> {
    (0..count)
        .map(|i| {
            let id = id_at(first_ms_before_now + i * step_ms);
            fake.add_message(channel, id, &format!("message {i}"));
            id
        })
        .collect()
}

#[test]
fn fresh_sync_discovers_channels_and_threads_and_stores_history() {
    let fake = two_channels();
    fake.add_thread(THREAD, GENERAL, "Release plan", false);
    fake.add_thread(THREAD_2, GENERAL, "Old thread", true);
    let general = [id_at(3 * HOUR), id_at(2 * HOUR), id_at(HOUR)];
    for (i, id) in general.iter().enumerate() {
        fake.add_message(GENERAL, *id, &format!("general {i}"));
    }
    let dev = [id_at(5 * HOUR), id_at(4 * HOUR)];
    for id in dev {
        fake.add_message(DEV, id, "dev chatter");
    }
    let thread = [id_at(90 * MINUTE), id_at(80 * MINUTE)];
    for id in thread {
        fake.add_message(THREAD, id, "thread message");
    }
    let old_thread = [id_at(10 * HOUR)];
    fake.add_message(THREAD_2, old_thread[0], "archived thread message");

    let mut store = Store::in_memory().unwrap();
    let report = run(&fake, &mut store, &config(), at(0));
    let source = report.source;

    assert_eq!(report.channels_listed, 2);
    assert_eq!(report.threads_listed, 2);
    assert!(report.channels_removed.is_empty());
    assert_eq!(report.upserted(), 8);
    assert_eq!(report.deleted(), 0);

    let mut names: Vec<String> = store
        .channels(source)
        .unwrap()
        .into_iter()
        .map(|info| info.name)
        .collect();
    names.sort();
    assert_eq!(names, ["Old thread", "Release plan", "dev", "general"]);
    let thread_info = channel_info(&store, source, THREAD).unwrap();
    assert_eq!(thread_info.kind, ChannelKind::Thread);
    assert!(thread_info.parent.is_some());
    assert!(!thread_info.archived);
    assert!(channel_info(&store, source, THREAD_2).unwrap().archived);

    assert_eq!(stored_count(&store, source, GENERAL, &general), 3);
    assert_eq!(stored_count(&store, source, DEV, &dev), 2);
    assert_eq!(stored_count(&store, source, THREAD, &thread), 2);
    assert_eq!(stored_count(&store, source, THREAD_2, &old_thread), 1);

    // Each channel and thread has its own cursor.
    let general_cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert_eq!(general_cursor.newest_seen, Some(sf(general[2])));
    assert_eq!(general_cursor.oldest_backfilled, Some(sf(general[0])));
    assert!(general_cursor.backfill_done);
    let thread_cursor = store.cursor(source, sf(THREAD)).unwrap().unwrap();
    assert_eq!(
        thread_cursor.newest_seen,
        Some(sf(thread[1].max(thread[0])))
    );
    assert!(store.cursor(source, sf(THREAD_2)).unwrap().is_some());
    assert!(store.cursor(source, sf(DEV)).unwrap().is_some());

    // Discovery asked for archived threads of both visibilities.
    let calls = fake.discovery_calls();
    assert!(calls.iter().any(|c| c.contains("Public")));
    assert!(calls.iter().any(|c| c.contains("Private")));

    // Running again changes nothing.
    let again = run(&fake, &mut store, &config(), at(MINUTE));
    assert_eq!(again.upserted(), 0);
    assert_eq!(again.deleted(), 0);
}

#[test]
fn fresh_sync_stops_at_the_backfill_age_limit() {
    let fake = two_channels();
    let too_old = [id_at(100 * DAY), id_at(91 * DAY)];
    let kept = [id_at(89 * DAY), id_at(DAY)];
    for id in too_old.iter().chain(&kept) {
        fake.add_message(GENERAL, *id, "history");
    }
    let mut store = Store::in_memory().unwrap();
    let report = run(&fake, &mut store, &config(), at(0));

    assert_eq!(stored_count(&store, report.source, GENERAL, &too_old), 0);
    assert_eq!(stored_count(&store, report.source, GENERAL, &kept), 2);
    let channel = report.channel(sf(GENERAL)).unwrap();
    assert!(channel.backfill_complete);
}

#[test]
fn backfill_continues_from_its_cursor_across_runs() {
    let fake = two_channels();
    let ids = fill(&fake, GENERAL, 250, HOUR, HOUR);
    let mut config = config();
    config.backfill_page_limit = 1;
    config.recheck_window = Duration::from_secs(60);
    let mut store = Store::in_memory().unwrap();

    let first = run(&fake, &mut store, &config, at(0));
    let source = first.source;
    assert_eq!(stored_count(&store, source, GENERAL, &ids), 100);
    assert!(!first.channel(sf(GENERAL)).unwrap().backfill_complete);
    let cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert!(!cursor.backfill_done);
    assert_eq!(cursor.oldest_backfilled, Some(sf(ids[99])));

    let second = run(&fake, &mut store, &config, at(MINUTE));
    assert_eq!(stored_count(&store, source, GENERAL, &ids), 200);
    assert!(!second.channel(sf(GENERAL)).unwrap().backfill_complete);

    let third = run(&fake, &mut store, &config, at(2 * MINUTE));
    assert_eq!(stored_count(&store, source, GENERAL, &ids), 250);
    assert!(third.channel(sf(GENERAL)).unwrap().backfill_complete);
    assert!(
        store
            .cursor(source, sf(GENERAL))
            .unwrap()
            .unwrap()
            .backfill_done
    );
}

#[test]
fn catch_up_after_downtime_pages_forward_from_the_cursor() {
    let fake = two_channels();
    let before = fill(&fake, GENERAL, 5, 10 * HOUR, 10 * MINUTE);
    let mut store = Store::in_memory().unwrap();
    let first = run(&fake, &mut store, &config(), at(0));
    let source = first.source;
    let old_newest = before[0];

    // The bot is offline while 130 messages arrive.
    let missed = fill(&fake, GENERAL, 130, -MINUTE, -MINUTE);
    fake.reset_log();
    let second = run(&fake, &mut store, &config(), at(4 * HOUR));

    assert_eq!(stored_count(&store, source, GENERAL, &missed), 130);
    let channel = second.channel(sf(GENERAL)).unwrap();
    assert!(channel.catchup_complete);
    assert_eq!(channel.upserted, 130);
    let requests = fake.requests(GENERAL);
    assert_eq!(requests[0].position, PagePosition::After(sf(old_newest)));
    // The second page starts after the newest message of the first full page.
    assert_eq!(requests[1].position, PagePosition::After(sf(missed[99])));
    let cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert_eq!(cursor.newest_seen, Some(sf(missed[129])));
}

#[test]
fn catch_up_stops_at_the_page_limit_and_resumes_next_run() {
    let fake = two_channels();
    fill(&fake, GENERAL, 3, 10 * HOUR, MINUTE);
    let mut config = config();
    config.catchup_page_limit = 1;
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config, at(0)).source;

    let missed = fill(&fake, GENERAL, 150, -MINUTE, -MINUTE);
    let second = run(&fake, &mut store, &config, at(4 * HOUR));
    assert!(!second.channel(sf(GENERAL)).unwrap().catchup_complete);
    // The recheck window reads everything recent anyway, so check the cursor instead:
    let cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert_eq!(cursor.newest_seen, Some(sf(missed[99])));

    let third = run(&fake, &mut store, &config, at(4 * HOUR + MINUTE));
    assert!(third.channel(sf(GENERAL)).unwrap().catchup_complete);
    let cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert_eq!(cursor.newest_seen, Some(sf(missed[149])));
}

#[test]
fn a_page_of_system_messages_still_moves_the_cursor() {
    let fake = two_channels();
    let first_ids = fill(&fake, GENERAL, 1, HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;

    let joins: Vec<u64> = (0..100).map(|i| id_at(-(i + 1) * MINUTE)).collect();
    for id in &joins {
        fake.add_system_message(GENERAL, *id);
    }
    let report = run(&fake, &mut store, &config(), at(4 * HOUR));
    assert_eq!(report.upserted(), 0);
    let cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert_eq!(cursor.newest_seen, Some(sf(joins[99])));
    assert_eq!(stored_count(&store, source, GENERAL, &first_ids), 1);
}

/// Messages: old (5 days), distant (4 days), a (30h), b (20h), c (1h).
struct History {
    old: u64,
    distant: u64,
    a: u64,
    b: u64,
    c: u64,
}

fn history(fake: &FakeDiscord) -> History {
    let history = History {
        old: id_at(5 * DAY),
        distant: id_at(4 * DAY),
        a: id_at(30 * HOUR),
        b: id_at(20 * HOUR),
        c: id_at(HOUR),
    };
    fake.add_message(GENERAL, history.old, "old text");
    fake.add_message(GENERAL, history.distant, "distant text");
    fake.add_message(GENERAL, history.a, "a text");
    fake.add_message(GENERAL, history.b, "b text");
    fake.add_message(GENERAL, history.c, "c text");
    history
}

#[test]
fn edits_and_deletes_inside_the_recheck_window_are_applied() {
    let fake = two_channels();
    let h = history(&fake);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;
    assert_eq!(
        stored(&store, source, GENERAL, h.a).unwrap().content,
        "a text"
    );

    fake.edit_message(GENERAL, h.a, "a text, edited");
    fake.delete_message(GENERAL, h.b);
    // Outside the 48 hour window: the sync cannot see these.
    fake.edit_message(GENERAL, h.old, "old text, edited");
    fake.delete_message(GENERAL, h.distant);

    let report = run(&fake, &mut store, &config(), at(HOUR));
    let channel = report.channel(sf(GENERAL)).unwrap();
    assert!(channel.recheck_covered);
    assert_eq!(channel.upserted, 1);
    assert_eq!(channel.deleted, 1);

    assert_eq!(
        stored(&store, source, GENERAL, h.a).unwrap().content,
        "a text, edited"
    );
    assert!(stored(&store, source, GENERAL, h.b).is_none());
    assert!(stored(&store, source, GENERAL, h.c).is_some());
    assert_eq!(
        stored(&store, source, GENERAL, h.old).unwrap().content,
        "old text",
        "edits older than the window are a documented limit"
    );
    assert!(stored(&store, source, GENERAL, h.distant).is_some());
}

#[test]
fn recheck_deletes_when_every_message_in_the_window_is_gone() {
    let fake = two_channels();
    let h = history(&fake);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;

    for id in [h.a, h.b, h.c] {
        fake.delete_message(GENERAL, id);
    }
    let report = run(&fake, &mut store, &config(), at(HOUR));
    assert_eq!(report.channel(sf(GENERAL)).unwrap().deleted, 3);
    assert_eq!(
        stored_count(&store, source, GENERAL, &[h.old, h.distant, h.a, h.b, h.c]),
        2
    );
}

#[test]
fn recheck_keeps_messages_newer_than_anything_it_read() {
    let fake = two_channels();
    let h = history(&fake);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;

    // A message that arrived live (gateway) after the REST read of the next run began is
    // stored but newer than Discord's newest as the sync saw it.
    let live = id_at(-5 * MINUTE);
    let event = SourceEvent::MessageUpsert(Message {
        id: MessageRef {
            channel: sf(GENERAL),
            message: sf(live),
        },
        author: Some(Member {
            id: sf(1_100_000_000_000_000_201),
            display_name: "Robin Example".to_owned(),
            is_bot: false,
        }),
        content: "arrived live".to_owned(),
        mentions: Vec::new(),
        reply_to: None,
        attachment_names: Vec::new(),
        created_at: sf(live).timestamp(),
        edited_at: None,
    });
    store.apply_event(source, &event, at(0)).unwrap();

    let report = run(&fake, &mut store, &config(), at(HOUR));
    assert_eq!(report.channel(sf(GENERAL)).unwrap().deleted, 0);
    assert!(stored(&store, source, GENERAL, live).is_some());
    assert!(stored(&store, source, GENERAL, h.c).is_some());
}

#[test]
fn a_channel_missing_from_the_listing_is_deleted_with_its_messages() {
    let fake = two_channels();
    fake.add_thread(THREAD, DEV, "Dev thread", false);
    let dev = fill(&fake, DEV, 2, HOUR, MINUTE);
    let thread = fill(&fake, THREAD, 2, 2 * HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;
    assert_eq!(stored_count(&store, source, DEV, &dev), 2);

    fake.remove(DEV);
    let report = run(&fake, &mut store, &config(), at(HOUR));
    assert_eq!(report.channels_removed, [sf(DEV)]);
    assert!(channel_info(&store, source, DEV).is_none());
    assert!(
        channel_info(&store, source, THREAD).is_none(),
        "threads go with their parent"
    );
    assert_eq!(stored_count(&store, source, DEV, &dev), 0);
    assert_eq!(stored_count(&store, source, THREAD, &thread), 0);
    assert!(channel_info(&store, source, GENERAL).is_some());
}

#[test]
fn a_thread_that_answers_404_is_deleted() {
    let fake = two_channels();
    fake.add_thread(THREAD, GENERAL, "Release plan", false);
    let thread = fill(&fake, THREAD, 2, 2 * HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;
    assert_eq!(stored_count(&store, source, THREAD, &thread), 2);

    fake.remove(THREAD);
    let report = run(&fake, &mut store, &config(), at(HOUR));
    assert_eq!(
        report.channel(sf(THREAD)).unwrap().outcome,
        ChannelOutcome::Removed
    );
    assert!(channel_info(&store, source, THREAD).is_none());
    assert_eq!(stored_count(&store, source, THREAD, &thread), 0);
    assert!(channel_info(&store, source, GENERAL).is_some());
}

#[test]
fn an_empty_channel_listing_does_not_delete_stored_channels() {
    let fake = two_channels();
    let general = fill(&fake, GENERAL, 2, HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;

    fake.hide_channel_listing();
    let report = run(&fake, &mut store, &config(), at(HOUR));
    assert!(report.channels_removed.is_empty());
    assert!(channel_info(&store, source, GENERAL).is_some());
    assert_eq!(stored_count(&store, source, GENERAL, &general), 2);
}

#[test]
fn an_archived_thread_gets_one_last_catch_up_then_is_left_alone() {
    let fake = two_channels();
    fake.add_thread(THREAD, GENERAL, "Release plan", false);
    let early = fill(&fake, THREAD, 2, 5 * HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;
    assert!(!channel_info(&store, source, THREAD).unwrap().archived);

    // Two more messages, then the thread is archived before the next run.
    let late = fill(&fake, THREAD, 2, -MINUTE, -MINUTE);
    fake.archive_thread(THREAD);
    fake.reset_log();
    run(&fake, &mut store, &config(), at(2 * HOUR));
    assert!(channel_info(&store, source, THREAD).unwrap().archived);
    assert_eq!(stored_count(&store, source, THREAD, &early), 2);
    assert_eq!(stored_count(&store, source, THREAD, &late), 2);
    assert!(!fake.requests(THREAD).is_empty());

    // It is still archived: no more message requests for it.
    fake.reset_log();
    run(&fake, &mut store, &config(), at(3 * HOUR));
    assert!(fake.requests(THREAD).is_empty());
    assert!(!fake.requests(GENERAL).is_empty());
}

#[test]
fn a_newly_found_archived_thread_is_backfilled_once() {
    let fake = two_channels();
    fake.add_thread(THREAD_2, GENERAL, "Old thread", true);
    let ids = fill(&fake, THREAD_2, 3, 6 * HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    let source = run(&fake, &mut store, &config(), at(0)).source;
    assert_eq!(stored_count(&store, source, THREAD_2, &ids), 3);
    assert!(
        store
            .cursor(source, sf(THREAD_2))
            .unwrap()
            .unwrap()
            .backfill_done
    );

    fake.reset_log();
    run(&fake, &mut store, &config(), at(HOUR));
    assert!(fake.requests(THREAD_2).is_empty());
}

#[test]
fn a_failure_part_way_resumes_from_the_saved_cursor() {
    let fake = two_channels();
    let ids = fill(&fake, GENERAL, 250, HOUR, HOUR);
    let mut config = config();
    config.recheck_window = Duration::from_secs(60);
    let mut store = Store::in_memory().unwrap();

    // The second page request fails.
    fake.fail_after(GENERAL, 1, Fail::Server);
    let error = sync_guild(&fake, &mut store, &config, at(0)).unwrap_err();
    assert!(error.is_retryable());
    assert!(matches!(
        error,
        SyncError::Rest {
            channel: Some(channel),
            ..
        } if channel == sf(GENERAL)
    ));

    // The first source row of a new database.
    let source = SourceId::new(1).unwrap();
    assert_eq!(stored_count(&store, source, GENERAL, &ids), 100);
    let cursor = store.cursor(source, sf(GENERAL)).unwrap().unwrap();
    assert_eq!(cursor.newest_seen, Some(sf(ids[0])));
    assert_eq!(cursor.oldest_backfilled, Some(sf(ids[99])));
    assert!(!cursor.backfill_done);

    fake.clear_failures();
    fake.reset_log();
    let report = run(&fake, &mut store, &config, at(MINUTE));
    assert_eq!(stored_count(&store, source, GENERAL, &ids), 250);
    assert!(report.channel(sf(GENERAL)).unwrap().backfill_complete);

    let requests = fake.requests(GENERAL);
    assert_eq!(requests[0].position, PagePosition::After(sf(ids[0])));
    assert!(
        requests
            .iter()
            .any(|r| r.position == PagePosition::Before(sf(ids[99]))),
        "backfill resumes exactly where the saved cursor left off"
    );
}

#[test]
fn a_rate_limit_is_a_typed_error_that_stops_the_run_without_waiting() {
    let fake = two_channels();
    fill(&fake, GENERAL, 3, HOUR, MINUTE);
    fill(&fake, DEV, 3, HOUR, MINUTE);
    fake.fail_after(GENERAL, 0, Fail::RateLimited);
    let mut store = Store::in_memory().unwrap();

    let error = sync_guild(&fake, &mut store, &config(), at(0)).unwrap_err();
    assert!(error.is_retryable());
    assert_eq!(error.retry_after(), Some(Duration::from_secs(9)));
    assert!(
        fake.requests(DEV).is_empty(),
        "the run stops instead of hammering Discord"
    );
}

#[test]
fn a_rejected_token_is_not_retryable() {
    let fake = two_channels();
    fill(&fake, GENERAL, 1, HOUR, MINUTE);
    fake.fail_after(GENERAL, 0, Fail::Unauthorized);
    let mut store = Store::in_memory().unwrap();
    let error = sync_guild(&fake, &mut store, &config(), at(0)).unwrap_err();
    assert!(!error.is_retryable());
    assert_eq!(error.retry_after(), None);
}

#[test]
fn an_unreadable_channel_is_skipped_and_the_others_still_sync() {
    let fake = two_channels();
    fill(&fake, GENERAL, 2, HOUR, MINUTE);
    let dev = fill(&fake, DEV, 2, HOUR, MINUTE);
    fake.forbid(GENERAL);
    let mut store = Store::in_memory().unwrap();

    let report = run(&fake, &mut store, &config(), at(0));
    assert_eq!(
        report.channel(sf(GENERAL)).unwrap().outcome,
        ChannelOutcome::Skipped(SkipReason::Forbidden)
    );
    assert_eq!(stored_count(&store, report.source, DEV, &dev), 2);
    assert!(store.cursor(report.source, sf(GENERAL)).unwrap().is_none());
}

#[test]
fn a_malformed_page_skips_the_channel_without_moving_its_cursor() {
    let fake = two_channels();
    fill(&fake, GENERAL, 2, HOUR, MINUTE);
    let dev = fill(&fake, DEV, 2, HOUR, MINUTE);
    fake.serve_garbage(GENERAL);
    let mut store = Store::in_memory().unwrap();

    let report = run(&fake, &mut store, &config(), at(0));
    assert_eq!(
        report.channel(sf(GENERAL)).unwrap().outcome,
        ChannelOutcome::Skipped(SkipReason::MalformedPayload)
    );
    assert!(store.cursor(report.source, sf(GENERAL)).unwrap().is_none());
    assert_eq!(stored_count(&store, report.source, DEV, &dev), 2);
}

#[test]
fn excluded_channels_and_their_threads_are_recorded_but_never_read() {
    let fake = two_channels();
    fake.add_thread(DEV_THREAD, DEV, "Dev thread", false);
    fill(&fake, DEV, 2, HOUR, MINUTE);
    fill(&fake, DEV_THREAD, 2, HOUR, MINUTE);
    let general = fill(&fake, GENERAL, 2, HOUR, MINUTE);
    let mut config = config();
    config.exclude_channels = vec!["dev".to_owned()];
    let mut store = Store::in_memory().unwrap();

    let report = run(&fake, &mut store, &config, at(0));
    let dev = channel_info(&store, report.source, DEV).unwrap();
    assert!(!dev.included);
    assert!(
        !channel_info(&store, report.source, DEV_THREAD)
            .unwrap()
            .included
    );
    assert!(fake.requests(DEV).is_empty());
    assert!(fake.requests(DEV_THREAD).is_empty());
    assert_eq!(stored_count(&store, report.source, GENERAL, &general), 2);
    assert!(report.channel(sf(DEV)).is_none());
}

#[test]
fn sync_only_reads_it_never_writes_to_discord() {
    // The fake implements only `DiscordRest`, so every call it saw is a read. This test
    // documents the guard: a full run makes message-page and discovery calls only.
    let fake = two_channels();
    fake.add_thread(THREAD, GENERAL, "Release plan", false);
    fill(&fake, GENERAL, 3, HOUR, MINUTE);
    let mut store = Store::in_memory().unwrap();
    run(&fake, &mut store, &config(), at(0));
    assert!(fake.message_request_count() > 0);
    assert!(
        fake.discovery_calls()
            .iter()
            .all(|call| call.starts_with("guild_channels")
                || call.starts_with("active_threads")
                || call.starts_with("archived_threads"))
    );
}
