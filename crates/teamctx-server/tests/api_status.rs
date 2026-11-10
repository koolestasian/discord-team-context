//! `GET /api/v1/status` with a host status provider: the connector state and each stored
//! channel's cursor, with ids only (no names, topics or chat text). Fictional data.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(
    dead_code,
    reason = "this binary does not use every harness helper; auth_http.rs does"
)]
mod common;

use std::sync::{Arc, Mutex};

use common::TestDb;
use reqwest::{Client, StatusCode};
use serde_json::Value;
use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{IngestWrite, SyncCursor};
use teamctx_core::source::{Channel, ChannelKind, SourceEvent, SourceKind};
use teamctx_core::time::UnixMillis;
use teamctx_server::status::HostStatus;
use teamctx_server::{HttpConfig, mcp::McpConfig, router, serve};
use teamctx_store::Store;
use tokio_util::sync::CancellationToken;

const NAME_MARKER: &str = "FAKE-channel-name-marker";
const TOPIC_MARKER: &str = "FAKE-topic-marker";

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).expect("id")
}

fn seed(store: &mut Store) -> SourceId {
    let source = store
        .upsert_source(
            SourceKind::Discord,
            sf(1_100_000_000_000_000_001),
            "Example Team",
        )
        .expect("source");
    for (id, included) in [
        (1_100_000_000_000_000_101, true),
        (1_100_000_000_000_000_102, false),
    ] {
        store
            .apply_event(
                source,
                &SourceEvent::ChannelUpsert(Channel {
                    id: sf(id),
                    parent: None,
                    kind: ChannelKind::Text,
                    name: NAME_MARKER.to_owned(),
                    topic: Some(TOPIC_MARKER.to_owned()),
                    included,
                    archived: false,
                }),
                UnixMillis::from_millis(1),
            )
            .expect("channel");
    }
    store
        .set_cursor(
            source,
            sf(1_100_000_000_000_000_101),
            &SyncCursor {
                newest_seen: Some(sf(1_100_000_000_000_001_001)),
                oldest_backfilled: Some(sf(1_100_000_000_000_001_000)),
                backfill_done: true,
                updated_at: UnixMillis::from_millis(1_700_000_000_000),
            },
        )
        .expect("cursor");
    source
}

#[tokio::test]
async fn status_reports_connector_state_and_cursors_without_names() {
    let source = Arc::new(Mutex::new(None));
    let seeded = Arc::clone(&source);
    let db = TestDb::with_seed(move |store| {
        *seeded.lock().expect("lock") = Some(seed(store));
    });
    let source = source.lock().expect("lock").expect("seeded source");
    let token = db.create_token("status-test");
    let shutdown = CancellationToken::new();
    let host = HostStatus {
        source: Some(source),
        gateway: "connected",
        last_event_at: Some(UnixMillis::from_millis(1_700_000_000_500)),
        events: 3,
        disconnects: 1,
        last_sync_started_at: Some(UnixMillis::from_millis(1_700_000_000_000)),
        last_sync_finished_at: Some(UnixMillis::from_millis(1_700_000_000_100)),
        last_sync_ok: Some(true),
        sync_runs: 2,
        problem: None,
    };
    let mcp = McpConfig::new(Store::open_read_only(&db.path).expect("reader"))
        .with_status(Arc::new(move || host.clone()));
    let app = router(HttpConfig {
        tokens: Arc::new(db.backend()),
        allowed_hosts: vec!["127.0.0.1".to_owned()],
        shutdown: shutdown.clone(),
        mcp,
        ask: None,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(serve(listener, app, shutdown.clone()));

    let client = Client::new();
    let url = format!("http://{addr}/api/v1/status");
    assert_eq!(
        client.get(&url).send().await.expect("send").status(),
        StatusCode::UNAUTHORIZED,
        "status stays behind auth"
    );
    let response = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.expect("body");
    assert!(
        !text.contains(NAME_MARKER),
        "channel names are not in the status"
    );
    assert!(!text.contains(TOPIC_MARKER), "topics are not in the status");
    let body: Value = serde_json::from_str(&text).expect("json");
    assert_eq!(body["status"], "ok");
    let sync = &body["sync"];
    assert_eq!(sync["gateway"], "connected");
    assert_eq!(sync["last_event_at"], 1_700_000_000_500_i64);
    assert_eq!(sync["sync_runs"], 2);
    assert_eq!(sync["last_sync_ok"], true);
    assert!(sync["problem"].is_null());
    let channels = sync["channels"].as_array().expect("channels");
    assert_eq!(channels.len(), 2);
    assert_eq!(channels[0]["id"], "1100000000000000101");
    assert_eq!(channels[0]["kind"], "text");
    assert_eq!(channels[0]["included"], true);
    assert_eq!(channels[0]["newest_seen"], "1100000000000001001");
    assert_eq!(channels[0]["backfill_done"], true);
    assert_eq!(channels[1]["included"], false);
    assert!(channels[1]["newest_seen"].is_null());
    shutdown.cancel();
}
