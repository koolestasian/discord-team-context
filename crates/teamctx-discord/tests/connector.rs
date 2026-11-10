//! The running connector against a fake gateway and a fake read-only Discord REST, with a
//! real SQLite file and the single writer (plan sections 5.2 and 7.2). No network.
//!
//! Every name, id and message here is fictional.

#![expect(clippy::expect_used, reason = "test helpers fail loudly")]

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teamctx_core::id::{DISCORD_EPOCH_MS, Snowflake};
use teamctx_core::repo::{DirectoryRead, MessageRead};
use teamctx_core::source::MessageRef;
use teamctx_core::time::UnixMillis;
use teamctx_discord::connector::{Connector, ConnectorError, GatewayState, Timing};
use teamctx_discord::gateway::{Gateway, GatewayEvent};
use teamctx_discord::store::HostStore;
use teamctx_discord::sync::{
    DiscordRest, MessagesRequest, PagePosition, RestError, SyncConfig, ThreadVisibility,
};
use teamctx_store::{Store, Writer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;

const GUILD: u64 = 1_100_000_000_000_000_001;
const OTHER_GUILD: u64 = 1_100_000_000_000_000_002;
const GENERAL: u64 = 1_100_000_000_000_000_101;
const NOW_MS: i64 = 1_800_000_000_000;
const MINUTE: i64 = 60_000;

fn id_at(ms_before_now: i64) -> u64 {
    let since_epoch =
        u64::try_from(NOW_MS - ms_before_now - DISCORD_EPOCH_MS).expect("after epoch");
    since_epoch << 22
}

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).expect("non-zero id")
}

fn message(channel: u64, id: u64, content: &str) -> Value {
    json!({
        "id": id.to_string(),
        "channel_id": channel.to_string(),
        "guild_id": GUILD.to_string(),
        "type": 0,
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
        "timestamp": "2027-01-15T08:00:00.000000+00:00",
        "edited_timestamp": null
    })
}

fn dispatch(kind: &str, data: &Value) -> GatewayEvent {
    GatewayEvent::Text(json!({ "op": 0, "s": 1, "t": kind, "d": data }).to_string())
}

fn guild_create(guild: u64) -> GatewayEvent {
    dispatch(
        "GUILD_CREATE",
        &json!({ "id": guild.to_string(), "name": "Example Team", "channels": [], "threads": [] }),
    )
}

// ---- fakes ----

#[derive(Default)]
struct Inner {
    messages: BTreeMap<u64, BTreeMap<u64, Value>>,
    message_requests: usize,
    listing_calls: usize,
}

/// A read-only fake of Discord's REST API with one text channel.
#[derive(Default, Clone)]
struct FakeRest(Arc<Mutex<Inner>>);

impl FakeRest {
    fn put(&self, channel: u64, id: u64, content: &str) {
        self.0
            .lock()
            .expect("lock")
            .messages
            .entry(channel)
            .or_default()
            .insert(id, message(channel, id, content));
    }

    fn listing_calls(&self) -> usize {
        self.0.lock().expect("lock").listing_calls
    }
}

impl DiscordRest for FakeRest {
    fn guild_channels(&self, guild: Snowflake) -> Result<Value, RestError> {
        assert_eq!(guild.get(), GUILD, "only the configured guild is listed");
        self.0.lock().expect("lock").listing_calls += 1;
        Ok(json!([{
            "id": GENERAL.to_string(),
            "guild_id": GUILD.to_string(),
            "type": 0,
            "name": "general",
            "position": 0,
            "nsfw": false,
            "permission_overwrites": [],
            "flags": 0
        }]))
    }

    fn active_threads(&self, _guild: Snowflake) -> Result<Value, RestError> {
        Ok(json!({ "threads": [], "members": [] }))
    }

    fn archived_threads(
        &self,
        _channel: Snowflake,
        _visibility: ThreadVisibility,
        _before: Option<&str>,
    ) -> Result<Value, RestError> {
        Ok(json!({ "threads": [], "members": [], "has_more": false }))
    }

    fn channel_messages(&self, request: &MessagesRequest) -> Result<Value, RestError> {
        let mut inner = self.0.lock().expect("lock");
        inner.message_requests += 1;
        let empty = BTreeMap::new();
        let messages = inner.messages.get(&request.channel.get()).unwrap_or(&empty);
        let limit = usize::from(request.limit);
        let page: Vec<Value> = match request.position {
            PagePosition::Latest => messages.values().rev().take(limit).cloned().collect(),
            PagePosition::Before(id) => messages
                .range(..id.get())
                .rev()
                .take(limit)
                .map(|(_, v)| v.clone())
                .collect(),
            PagePosition::After(id) => {
                let mut page: Vec<Value> = messages
                    .range(id.get() + 1..)
                    .take(limit)
                    .map(|(_, v)| v.clone())
                    .collect();
                page.reverse();
                page
            }
        };
        Ok(Value::Array(page))
    }
}

/// A gateway fed by the test. `close` answers with a normal close frame, then ends.
struct FakeGateway {
    frames: UnboundedReceiver<GatewayEvent>,
    closing: bool,
    closed: bool,
}

impl Gateway for FakeGateway {
    async fn next_event(&mut self) -> Option<GatewayEvent> {
        if self.closed {
            return None;
        }
        if self.closing {
            self.closed = true;
            return Some(GatewayEvent::Closed(Some(1000)));
        }
        self.frames.recv().await
    }

    fn close(&mut self) {
        self.closing = true;
    }
}

fn gateway() -> (UnboundedSender<GatewayEvent>, FakeGateway) {
    let (tx, rx) = unbounded_channel();
    (
        tx,
        FakeGateway {
            frames: rx,
            closing: false,
            closed: false,
        },
    )
}

// ---- harness ----

struct Harness {
    _dir: tempfile::TempDir,
    path: PathBuf,
    writer: Option<Writer>,
    rest: FakeRest,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("teamctx.db");
        let writer = Writer::spawn(Store::open(&path).expect("open")).expect("writer");
        Self {
            _dir: dir,
            path,
            writer: Some(writer),
            rest: FakeRest::default(),
        }
    }

    fn connector(
        &self,
        gateway: FakeGateway,
    ) -> Connector<
        FakeGateway,
        FakeRest,
        impl Fn() -> teamctx_core::repo::RepoResult<HostStore> + use<>,
    > {
        let client = self.writer.as_ref().expect("writer").client();
        let path = self.path.clone();
        let mut config = SyncConfig::new(sf(GUILD), "fallback");
        config.backfill_days = 30;
        Connector::new(
            gateway,
            Arc::new(self.rest.clone()),
            move || {
                Ok(HostStore::new(
                    client.clone(),
                    Store::open_read_only(&path)?,
                ))
            },
            config,
            Arc::new(|| UnixMillis::from_millis(NOW_MS)),
        )
        .with_timing(Timing {
            continue_after: Duration::from_millis(10),
            min_backoff: Duration::from_millis(10),
            max_backoff: Duration::from_millis(50),
        })
    }

    fn reader(&self) -> Store {
        Store::open_read_only(&self.path).expect("reader")
    }

    fn content(&self, id: u64) -> Option<String> {
        let reader = self.reader();
        let source = reader
            .channels(teamctx_core::id::SourceId::new(1).expect("source id"))
            .ok()?
            .first()
            .map(|channel| channel.source)?;
        reader
            .message_by_ref(
                source,
                MessageRef {
                    channel: sf(GENERAL),
                    message: sf(id),
                },
            )
            .expect("read")
            .map(|m| m.content)
    }
}

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn wal_size(path: &Path) -> io::Result<u64> {
    let wal = path.with_extension("db-wal");
    match std::fs::metadata(wal) {
        Ok(meta) => Ok(meta.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

// ---- tests ----

#[test]
fn live_events_catch_up_and_reconnect_reach_the_store() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mut h = Harness::new();
    let history = id_at(60 * MINUTE);
    h.rest
        .put(GENERAL, history, "Earlier: we keep the weekly demo.");
    let (frames, gateway) = gateway();
    let connector = h.connector(gateway);
    let status = connector.status();
    let cancel = CancellationToken::new();
    let task = runtime.spawn(connector.run(cancel.clone()));

    // Start: READY, then GUILD_CREATE runs discovery, catch-up and backfill.
    frames
        .send(dispatch("READY", &json!({"v": 10})))
        .expect("send");
    frames.send(guild_create(GUILD)).expect("send");
    wait_for("the first sync run", || status.snapshot().sync_runs >= 1);
    assert_eq!(
        h.content(history).as_deref(),
        Some("Earlier: we keep the weekly demo.")
    );
    assert_eq!(status.snapshot().gateway, GatewayState::Connected);

    // Live create, then a partial update that is merged with the stored row.
    let live = id_at(30 * MINUTE);
    frames
        .send(dispatch(
            "MESSAGE_CREATE",
            &message(GENERAL, live, "Decision: ship on Friday."),
        ))
        .expect("send");
    frames
        .send(dispatch(
            "MESSAGE_UPDATE",
            &json!({
                "id": live.to_string(),
                "channel_id": GENERAL.to_string(),
                "guild_id": GUILD.to_string(),
                "content": "Decision: ship on Monday.",
                "edited_timestamp": "2027-01-15T09:00:00.000000+00:00"
            }),
        ))
        .expect("send");
    wait_for("the merged update", || {
        h.content(live).as_deref() == Some("Decision: ship on Monday.")
    });

    // A partial update for a message never stored: fetched read-only and stored in full.
    let missed = id_at(20 * MINUTE);
    h.rest.put(GENERAL, missed, "Edited while we were offline.");
    frames
        .send(dispatch(
            "MESSAGE_UPDATE",
            &json!({
                "id": missed.to_string(),
                "channel_id": GENERAL.to_string(),
                "guild_id": GUILD.to_string(),
                "content": "Edited while we were offline.",
            }),
        ))
        .expect("send");
    wait_for("the fetched message", || h.content(missed).is_some());

    // Another guild's message is never stored, even in a channel id we know.
    let foreign = id_at(10 * MINUTE);
    let mut other = message(GENERAL, foreign, "Another server's chat.");
    other["guild_id"] = json!(OTHER_GUILD.to_string());
    frames
        .send(dispatch("MESSAGE_CREATE", &other))
        .expect("send");

    // Disconnect, a message arrives while offline, then RESUMED: catch-up runs again.
    let runs_before = status.snapshot().sync_runs;
    frames.send(GatewayEvent::Closed(Some(1006))).expect("send");
    wait_for("the reconnecting state", || {
        status.snapshot().gateway == GatewayState::Reconnecting
    });
    let offline = id_at(5 * MINUTE);
    h.rest.put(GENERAL, offline, "Posted during the outage.");
    frames
        .send(dispatch("RESUMED", &json!(null)))
        .expect("send");
    wait_for("catch-up after the reconnect", || {
        status.snapshot().sync_runs > runs_before && h.content(offline).is_some()
    });
    assert!(
        h.content(foreign).is_none(),
        "another guild's message was stored"
    );
    assert!(
        h.rest.listing_calls() >= 2,
        "discovery ran on start and after reconnect"
    );

    // Graceful shutdown: the connector returns, then the writer checkpoints the WAL.
    cancel.cancel();
    let result = runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), task).await })
        .expect("connector stopped in time")
        .expect("task joined");
    assert!(result.is_ok(), "{result:?}");
    let snapshot = status.snapshot();
    assert_eq!(snapshot.gateway, GatewayState::Stopped);
    assert_eq!(snapshot.disconnects, 1);
    assert!(snapshot.last_event_at.is_some());
    h.writer
        .take()
        .expect("writer")
        .shutdown()
        .expect("writer shutdown");
    assert_eq!(wal_size(&h.path).expect("wal"), 0, "WAL checkpointed");
}

#[test]
fn shutdown_drains_every_queued_event() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mut h = Harness::new();
    let (frames, gateway) = gateway();
    let connector = h.connector(gateway);
    let status = connector.status();
    let cancel = CancellationToken::new();
    let task = runtime.spawn(connector.run(cancel.clone()));
    frames.send(guild_create(GUILD)).expect("send");
    wait_for("the first sync run", || status.snapshot().sync_runs >= 1);

    let ids: Vec<u64> = (0..200).map(|n| id_at(40 * MINUTE - n * 1000)).collect();
    for (n, id) in ids.iter().enumerate() {
        frames
            .send(dispatch(
                "MESSAGE_CREATE",
                &message(GENERAL, *id, &format!("note {n}")),
            ))
            .expect("send");
    }
    // Every frame has left the gateway; some may still wait in the ingest queue.
    wait_for("all frames forwarded", || status.snapshot().events >= 200);
    cancel.cancel();
    runtime
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), task).await })
        .expect("connector stopped in time")
        .expect("task joined")
        .expect("clean stop");
    for id in &ids {
        assert!(
            h.content(*id).is_some(),
            "queued event {id} was dropped at shutdown"
        );
    }
    h.writer
        .take()
        .expect("writer")
        .shutdown()
        .expect("writer shutdown");
}

#[test]
fn a_fatal_close_code_stops_the_connector_with_a_fixed_reason() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let h = Harness::new();
    let (frames, gateway) = gateway();
    let connector = h.connector(gateway);
    let status = connector.status();
    frames.send(GatewayEvent::Closed(Some(4014))).expect("send");
    drop(frames);
    let result = runtime.block_on(connector.run(CancellationToken::new()));
    let Err(ConnectorError::Fatal(reason)) = result else {
        panic!("expected a fatal close, got {result:?}");
    };
    assert!(reason.contains("Message Content intent"), "{reason}");
    assert_eq!(status.snapshot().problem, Some(reason));
    assert_eq!(status.snapshot().gateway, GatewayState::Stopped);
}

#[test]
fn events_for_unknown_channels_ask_for_a_sync_instead_of_guessing() {
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let mut h = Harness::new();
    let (frames, gateway) = gateway();
    let connector = h.connector(gateway);
    let status = connector.status();
    let cancel = CancellationToken::new();
    let task = runtime.spawn(connector.run(cancel.clone()));
    frames.send(guild_create(GUILD)).expect("send");
    wait_for("the first sync run", || status.snapshot().sync_runs >= 1);
    let runs = status.snapshot().sync_runs;
    // Nothing is left to do, so no further run starts on its own.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        status.snapshot().sync_runs,
        runs,
        "an idle connector kept syncing"
    );

    // A thread under a parent the store does not know: not stored, a sync is requested.
    frames
        .send(dispatch(
            "THREAD_CREATE",
            &json!({
                "id": "1100000000000000900",
                "guild_id": GUILD.to_string(),
                "parent_id": "1100000000000000999",
                "owner_id": "1100000000000000201",
                "type": 11,
                "name": "unknown parent",
                "thread_metadata": {
                    "archived": false,
                    "archive_timestamp": "2027-01-15T08:00:00.000000+00:00",
                    "auto_archive_duration": 1440,
                    "locked": false
                }
            }),
        ))
        .expect("send");
    wait_for("a sync requested by the unknown parent", || {
        status.snapshot().sync_runs > runs
    });
    let reader = h.reader();
    let source = status.snapshot().source.expect("source");
    assert!(
        reader
            .channels(source)
            .expect("channels")
            .iter()
            .all(|c| c.external_id.get() != 1_100_000_000_000_000_900),
        "a thread with an unknown parent was stored"
    );
    cancel.cancel();
    runtime
        .block_on(task)
        .expect("task joined")
        .expect("clean stop");
    h.writer
        .take()
        .expect("writer")
        .shutdown()
        .expect("writer shutdown");
}
