//! `teamctx mcp --stdio` transport: the same tools over newline-delimited JSON-RPC on an
//! in-memory pipe, on the fictional demo record.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(
    dead_code,
    reason = "this binary does not use every harness helper; auth_http.rs does"
)]
mod common;

use std::sync::Arc;
use std::time::Duration;

use common::TestDb;
use serde_json::{Value, json};
use teamctx_core::framing::NOTICE;
use teamctx_server::mcp::McpConfig;
use teamctx_server::stdio::{StdioError, serve_io};
use teamctx_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_util::sync::CancellationToken;

struct Client {
    lines: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    writer: tokio::io::WriteHalf<tokio::io::DuplexStream>,
}

impl Client {
    async fn send(&mut self, message: &Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .expect("write request");
    }

    async fn receive(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("response in time")
            .expect("read")
            .expect("a line");
        serde_json::from_str(&line).expect("JSON-RPC line")
    }
}

fn start(
    db: &TestDb,
) -> (
    Client,
    tokio::task::JoinHandle<Result<(), StdioError>>,
    CancellationToken,
) {
    let (client_side, server_side) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, client_write) = tokio::io::split(client_side);
    let config = McpConfig::new(Store::open_read_only(&db.path).expect("read connection"))
        .with_clock(Arc::new(common::demo_now));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(serve_io(
        config,
        server_read,
        server_write,
        shutdown.clone(),
    ));
    (
        Client {
            lines: BufReader::new(client_read).lines(),
            writer: client_write,
        },
        task,
        shutdown,
    )
}

#[tokio::test]
async fn stdio_serves_the_same_read_only_tools() {
    let db = TestDb::with_demo();
    let (mut client, task, _shutdown) = start(&db);
    client
        .send(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "stdio-test-client", "version": "0.0.0" }
            }
        }))
        .await;
    let init = client.receive().await;
    assert_eq!(init["id"], 1);
    assert!(init["result"]["serverInfo"].is_object(), "{init}");
    client
        .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    client
        .send(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
        .await;
    let list = client.receive().await;
    let mut names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .expect("tools")
        .iter()
        .map(|tool| tool["name"].as_str().expect("name"))
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["context_for", "read", "record", "search", "whats_new"]
    );
    assert!(
        list["result"]["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .all(|tool| tool["annotations"]["readOnlyHint"] == true)
    );

    client
        .send(&json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "search", "arguments": { "query": "sync" } }
        }))
        .await;
    let hit = client.receive().await;
    let text = hit["result"]["content"][0]["text"].as_str().expect("text");
    assert!(
        text.starts_with(NOTICE),
        "results are framed as untrusted data"
    );

    // Closing stdin ends the session cleanly.
    drop(client);
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("stopped in time")
        .expect("joined");
    assert!(result.is_ok(), "{result:?}");
}

#[tokio::test]
async fn a_client_that_skips_initialize_gets_a_fixed_error() {
    let db = TestDb::with_demo();
    let (mut client, task, _shutdown) = start(&db);
    client
        .send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "x": "FAKE-secret-marker" } }))
        .await;
    drop(client);
    let result = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("stopped in time")
        .expect("joined");
    let error = result.expect_err("no handshake");
    assert!(!format!("{error} {error:?}").contains("FAKE-secret-marker"));
}

#[tokio::test]
async fn shutdown_stops_a_waiting_session() {
    let db = TestDb::with_demo();
    let (_client, task, shutdown) = start(&db);
    shutdown.cancel();
    let joined = tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .expect("stopped in time");
    assert!(joined.is_ok());
}
