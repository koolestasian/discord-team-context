//! Log guard for the live connector (plan section 13, docs/testing/guards.md): chat text
//! and anything shaped like a credential that arrives through the gateway never reaches
//! the logs, on the normal path or on any error path.
//!
//! One test per file, because it installs a global `tracing` subscriber at TRACE to see
//! the worker threads' events too. Every value is fictional.

#![expect(clippy::expect_used, reason = "test helpers fail loudly")]

use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teamctx_core::id::Snowflake;
use teamctx_core::time::UnixMillis;
use teamctx_discord::connector::{Connector, ConnectorError, Timing};
use teamctx_discord::gateway::{Gateway, GatewayEvent};
use teamctx_discord::store::HostStore;
use teamctx_discord::sync::{
    DiscordRest, MessagesRequest, RestError, SyncConfig, ThreadVisibility,
};
use teamctx_store::{Store, Writer};
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
use tokio_util::sync::CancellationToken;

const GUILD: &str = "1100000000000000001";
const CHAT_SECRET: &str = "FAKE-chat-marker-7f3a-not-a-real-credential";
const TOKEN_SHAPED: &str = "FAKE.bot-token-shaped.0000-not-a-real-credential";

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// REST that fails every message read with a body-less error, after listing one channel
/// whose name and topic carry the markers.
struct FailingRest;

impl DiscordRest for FailingRest {
    fn guild_channels(&self, _guild: Snowflake) -> Result<Value, RestError> {
        Ok(json!([{
            "id": "1100000000000000101",
            "guild_id": GUILD,
            "type": 0,
            "name": CHAT_SECRET,
            "topic": TOKEN_SHAPED,
            "position": 0,
            "permission_overwrites": []
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
        Err(RestError::Transport(format!("echo {TOKEN_SHAPED}").into()))
    }
    fn channel_messages(&self, _request: &MessagesRequest) -> Result<Value, RestError> {
        Ok(json!({ "message": CHAT_SECRET }))
    }
}

struct FakeGateway(UnboundedReceiver<GatewayEvent>);

impl Gateway for FakeGateway {
    async fn next_event(&mut self) -> Option<GatewayEvent> {
        self.0.recv().await
    }
    fn close(&mut self) {}
}

fn dispatch(kind: &str, data: &Value) -> GatewayEvent {
    GatewayEvent::Text(json!({ "op": 0, "t": kind, "d": data }).to_string())
}

#[test]
fn chat_text_and_credentials_never_reach_the_logs() {
    let capture = Capture::default();
    let writer_capture = capture.clone();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_ansi(false)
            .with_writer(move || writer_capture.clone())
            .finish(),
    )
    .expect("global subscriber");

    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("teamctx.db");
    let writer = Writer::spawn(Store::open(&path).expect("open")).expect("writer");
    let client = writer.client();
    let reader_path = path.clone();
    let (frames, rx) = unbounded_channel();
    let connector = Connector::new(
        FakeGateway(rx),
        Arc::new(FailingRest),
        move || {
            Ok(HostStore::new(
                client.clone(),
                Store::open_read_only(&reader_path)?,
            ))
        },
        SyncConfig::new(Snowflake::new(1_100_000_000_000_000_001).expect("id"), "x"),
        Arc::new(|| UnixMillis::from_millis(1_800_000_000_000)),
    )
    .with_timing(Timing {
        continue_after: Duration::from_millis(10),
        min_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(20),
    });
    let status = connector.status();
    let task = runtime.spawn(connector.run(CancellationToken::new()));

    let message = |content: &str| {
        json!({
            "id": "1100000000000001001",
            "channel_id": "1100000000000000101",
            "guild_id": GUILD,
            "type": 0,
            "content": content,
            "author": { "id": "1100000000000000201", "username": CHAT_SECRET },
            "mentions": [], "attachments": [], "embeds": [],
            "timestamp": "2027-01-15T08:00:00.000000+00:00"
        })
    };
    let events = [
        dispatch(
            "READY",
            &json!({ "v": 10, "user": { "username": CHAT_SECRET } }),
        ),
        dispatch("GUILD_CREATE", &json!({ "id": GUILD, "name": CHAT_SECRET })),
        dispatch(
            "MESSAGE_CREATE",
            &message(&format!("{CHAT_SECRET} {TOKEN_SHAPED}")),
        ),
        // Malformed: the author is a number. The error path must not echo the payload.
        dispatch(
            "MESSAGE_CREATE",
            &json!({ "id": "1100000000000001002", "channel_id": "1100000000000000101",
                     "guild_id": GUILD, "type": 0, "content": CHAT_SECRET, "author": 7 }),
        ),
        dispatch(
            "MESSAGE_UPDATE",
            &json!({ "id": "1100000000000001003", "channel_id": "1100000000000000101",
                     "guild_id": GUILD, "content": TOKEN_SHAPED }),
        ),
        dispatch(
            CHAT_SECRET,
            &json!({ "guild_id": GUILD, "text": TOKEN_SHAPED }),
        ),
        GatewayEvent::Text(format!("not json {CHAT_SECRET} {TOKEN_SHAPED}")),
        GatewayEvent::ReceiveFailed,
        GatewayEvent::Closed(Some(4004)),
    ];
    for event in events {
        frames.send(event).expect("send");
    }
    drop(frames);
    let deadline = Instant::now() + Duration::from_secs(20);
    while status.snapshot().sync_runs < 2 {
        assert!(Instant::now() < deadline, "sync never ran");
        std::thread::sleep(Duration::from_millis(20));
    }
    let result = runtime.block_on(task).expect("task joined");
    assert!(
        matches!(result, Err(ConnectorError::Fatal(_))),
        "{result:?}"
    );
    writer.shutdown().expect("writer shutdown");

    let logs = String::from_utf8(capture.0.lock().expect("lock").clone()).expect("utf-8 logs");
    assert!(
        logs.contains("not applied"),
        "the error paths were exercised"
    );
    assert!(
        logs.contains("Discord sync run"),
        "the sync paths were exercised"
    );
    for (name, secret) in [("chat text", CHAT_SECRET), ("credential", TOKEN_SHAPED)] {
        assert!(!logs.contains(secret), "{name} reached the logs");
    }
}
