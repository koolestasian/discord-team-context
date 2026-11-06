//! Secrets never reach the logs: every `tracing` event emitted while tokens are accepted,
//! rejected and rate limited is captured and searched for the tokens.
//!
//! This file holds a single test and installs a global subscriber, so no other test can
//! race it for `tracing`'s per-callsite interest cache.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(dead_code, reason = "this binary does not need every harness helper")]
mod common;

use std::{
    fmt::{self, Write as _},
    sync::{Arc, Mutex},
};

use common::{TestDb, TestServer};
use reqwest::{Client, Response, StatusCode};
use serde_json::json;

/// Right shape, never issued.
const UNKNOWN: &str = "tctx_0000000000000000000000000000000000000000000000000000000000000000";

/// A stateless MCP `tools/list` call; succeeds with any valid token.
async fn call(client: &Client, server: &TestServer, authorization: &str) -> Response {
    client
        .post(&server.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/list")
        .header("Authorization", authorization)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "auth-test", "version": "0.0.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        }))
        .send()
        .await
        .expect("send")
}

/// Minimal `tracing` subscriber that writes every event and span field into a string.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<String>>);

struct Fields<'a>(&'a mut String);

impl tracing::field::Visit for Fields<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn fmt::Debug) {
        let _ = write!(self.0, "{}={:?} ", field.name(), value);
    }
}

impl tracing::Subscriber for Capture {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        let mut text = self.0.lock().expect("capture lock");
        let _ = write!(text, "span {} ", span.metadata().name());
        span.record(&mut Fields(&mut text));
        text.push('\n');
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        values.record(&mut Fields(&mut self.0.lock().expect("capture lock")));
    }
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut text = self.0.lock().expect("capture lock");
        let metadata = event.metadata();
        let _ = write!(text, "{} {} ", metadata.level(), metadata.target());
        event.record(&mut Fields(&mut text));
        text.push('\n');
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn logs_never_contain_presented_or_stored_tokens() {
    let capture = Capture::default();
    tracing::subscriber::set_global_default(capture.clone()).expect("first subscriber");

    let db = TestDb::new();
    let valid = db.create_token("robin-laptop");
    let revoked = db.create_token("old-laptop");
    db.revoke_from_cli("old-laptop");
    let server = common::start(&db).await;
    let client = Client::new();

    let ok = call(&client, &server, &format!("Bearer {valid}")).await;
    assert_eq!(ok.status(), StatusCode::OK);
    let fake = "tctx_test_fake_but_secret_looking";
    for header in [
        format!("Bearer {revoked}"),
        format!("Bearer {fake}"),
        format!("Basic {valid}"),
        format!("Bearer {UNKNOWN}"),
    ] {
        let response = call(&client, &server, &header).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
    // Enough failures to trip the limiter, so the lockout warning is logged too.
    for _ in 0..6 {
        call(&client, &server, &format!("Bearer {valid}x")).await;
    }
    let locked = call(&client, &server, &format!("Bearer {valid}")).await;
    assert_eq!(locked.status(), StatusCode::TOO_MANY_REQUESTS);
    drop(server);
    drop(db);

    let logs = capture.0.lock().expect("capture lock").clone();
    // The capture works: the auth events are there.
    assert!(
        logs.contains("rejected request without a valid bearer token"),
        "auth rejection not captured"
    );
    assert!(logs.contains("client locked out"), "lockout not captured");
    assert!(
        logs.contains("over the auth failure limit"),
        "refusal not captured"
    );
    for secret in [valid.as_str(), &valid[5..], &revoked, &revoked[5..], fake] {
        // The message names no token; a failure here must not print the logs.
        assert!(
            !logs.contains(secret),
            "a token appeared in the captured logs"
        );
    }
}
