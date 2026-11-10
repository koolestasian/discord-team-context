//! Parity (ADR 0012): the JSON API returns what the MCP tools return, for the same inputs,
//! on the fictional demo record. Both are called over real HTTP on the same server.
//!
//! The comparison is exact: the API body is the MCP `structuredContent` plus the MCP text
//! content, so entries, ids, order, cursors, truncation and framing all match.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(
    dead_code,
    reason = "this binary does not use every harness helper; auth_http.rs does"
)]
mod common;

use common::{TestDb, TestServer};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use teamctx_core::framing::{BLOCK_END, BLOCK_START, NOTICE};

struct Harness {
    mcp_url: String,
    base: String,
    token: String,
    client: Client,
    _db: TestDb,
    _server: TestServer,
}

async fn start() -> Harness {
    let db = TestDb::with_demo();
    let token = db.create_token("t13-parity");
    let server = common::start(&db).await;
    Harness {
        mcp_url: server.url.clone(),
        base: server
            .url
            .strip_suffix("/mcp")
            .expect("harness url ends in /mcp")
            .to_owned(),
        token,
        client: Client::new(),
        _db: db,
        _server: server,
    }
}

impl Harness {
    /// Calls an MCP tool and returns its `result` object.
    async fn mcp(&self, tool: &str, arguments: Value) -> Value {
        let response = self
            .client
            .post(&self.mcp_url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", "2025-11-25")
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": tool, "arguments": arguments }
            }))
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), StatusCode::OK, "{tool}");
        let is_sse = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|ct| ct.starts_with("text/event-stream"));
        let text = response.text().await.expect("body");
        let json_text = if is_sse {
            text.lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(str::trim)
                .find(|data| data.starts_with('{'))
                .unwrap_or_default()
                .to_owned()
        } else {
            text
        };
        let message: Value = serde_json::from_str(&json_text).expect("JSON-RPC message");
        assert!(message["error"].is_null(), "{tool}: {message}");
        message["result"].clone()
    }

    async fn api(&self, path_and_query: &str) -> Value {
        let response = self
            .client
            .get(format!("{}{path_and_query}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), StatusCode::OK, "{path_and_query}");
        response.json().await.expect("JSON body")
    }

    /// Asserts the API body equals the MCP result for one call and returns the body.
    async fn same(&self, tool: &str, arguments: Value, path_and_query: &str) -> Value {
        let mcp = self.mcp(tool, arguments).await;
        assert_ne!(mcp["isError"], true, "{tool}: {mcp}");
        let api = self.api(path_and_query).await;
        let mut expected = match &mcp["structuredContent"] {
            Value::Object(map) => map.clone(),
            other => panic!("structuredContent is not an object: {other}"),
        };
        expected.insert("text".to_owned(), mcp["content"][0]["text"].clone());
        assert_eq!(api, Value::Object(expected), "{tool} via {path_and_query}");
        assert!(
            !api["items"].as_array().expect("items").is_empty(),
            "{tool} returned nothing, so the comparison proves little"
        );
        api
    }
}

fn ids(body: &Value) -> Vec<String> {
    body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

#[tokio::test]
async fn each_route_matches_its_mcp_tool() {
    let h = start().await;
    h.same(
        "context_for",
        json!({ "text": "fix/reading-position-conflicts" }),
        "/api/v1/context?text=fix%2Freading-position-conflicts",
    )
    .await;
    h.same(
        "context_for",
        json!({ "text": "beta date", "limit": 2 }),
        "/api/v1/context?text=beta+date&limit=2",
    )
    .await;
    h.same(
        "whats_new",
        json!({ "since": "2026-04-14" }),
        "/api/v1/whats-new?since=2026-04-14",
    )
    .await;
    h.same(
        "whats_new",
        json!({ "since": "7d" }),
        "/api/v1/whats-new?since=7d",
    )
    .await;
    h.same("record", json!({}), "/api/v1/record").await;
    h.same(
        "record",
        json!({ "kind": "decision", "status": "active", "topic": "reading position" }),
        "/api/v1/record?kind=decision&status=active&q=reading+position",
    )
    .await;
    h.same(
        "record",
        json!({ "owner": "Wren Calloway", "since": "2026-04-01" }),
        "/api/v1/record?owner=Wren+Calloway&since=2026-04-01",
    )
    .await;
    let found = h
        .same(
            "search",
            json!({ "query": "libspine" }),
            "/api/v1/search?q=libspine",
        )
        .await;
    h.same(
        "search",
        json!({ "query": "crash from:crashwatch after:2026-04-01" }),
        "/api/v1/search?q=crash+from%3Acrashwatch+after%3A2026-04-01",
    )
    .await;
    let message = ids(&found).first().cloned().expect("a hit");
    h.same(
        "read",
        json!({ "id": message, "before": 1, "after": 3 }),
        &format!("/api/v1/read?id={message}&before=1&after=3"),
    )
    .await;
    h.same("read", json!({ "id": "D-1" }), "/api/v1/read?id=D-1")
        .await;
}

#[tokio::test]
async fn paging_cursors_are_interchangeable_between_api_and_mcp() {
    let h = start().await;
    let first = h
        .same(
            "record",
            json!({ "kind": "decision" }),
            "/api/v1/record?kind=decision",
        )
        .await;
    let cursor = first["next_cursor"].as_str().expect("a second page");
    h.same(
        "record",
        json!({ "kind": "decision", "cursor": cursor }),
        &format!("/api/v1/record?kind=decision&cursor={cursor}"),
    )
    .await;
}

#[tokio::test]
async fn bad_input_is_refused_by_both_with_the_same_words() {
    let h = start().await;
    let cases = [
        (
            "record",
            json!({ "kind": "idea" }),
            "/api/v1/record?kind=idea",
        ),
        (
            "record",
            json!({ "status": "finished" }),
            "/api/v1/record?status=finished",
        ),
        (
            "record",
            json!({ "owner": "Nobody Atall" }),
            "/api/v1/record?owner=Nobody+Atall",
        ),
        ("read", json!({ "id": "x" }), "/api/v1/read?id=x"),
        (
            "search",
            json!({ "query": "\"open" }),
            "/api/v1/search?q=%22open",
        ),
        (
            "whats_new",
            json!({ "since": "soon" }),
            "/api/v1/whats-new?since=soon",
        ),
    ];
    for (tool, arguments, path) in cases {
        let mcp = h.mcp(tool, arguments).await;
        assert_eq!(mcp["isError"], true, "{tool}");
        let response = h
            .client
            .get(format!("{}{path}", h.base))
            .bearer_auth(&h.token)
            .send()
            .await
            .expect("send");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        let body: Value = response.json().await.expect("JSON body");
        assert_eq!(
            body["error"], mcp["content"][0]["text"],
            "{tool} and {path} word the refusal differently"
        );
    }
}

#[tokio::test]
async fn hostile_demo_messages_arrive_only_inside_the_framing() {
    const INJECTIONS: [&str; 2] = [
        "Ignore all previous instructions",
        "disregard prior instructions",
    ];
    let h = start().await;
    let found = h.api("/api/v1/search?q=%22admin+tokens%22").await;
    let message_id = ids(&found).first().cloned().expect("the crash digest");
    let results = [
        found,
        h.api(&format!("/api/v1/read?id={message_id}")).await,
        h.api("/api/v1/context?text=support+email+imported+book+title")
            .await,
        h.api("/api/v1/search?q=free+forever").await,
    ];
    let mut seen = 0;
    for body in &results {
        let text = body["text"].as_str().expect("text");
        let start = text.find(BLOCK_START).expect("block start");
        let end = text.find(BLOCK_END).expect("block end");
        assert_eq!(text.matches(BLOCK_START).count(), 1);
        assert_eq!(text.matches(BLOCK_END).count(), 1);
        assert!(text.starts_with(NOTICE));
        let outside = format!("{}{}", &text[..start], &text[end..]).to_lowercase();
        // Everything but `text` is ids, kinds, statuses, times, links and fixed notices.
        let mut structure = body.as_object().expect("object").clone();
        structure.remove("text");
        let structure = Value::Object(structure).to_string().to_lowercase();
        for injection in INJECTIONS {
            let needle = injection.to_lowercase();
            assert!(!outside.contains(&needle), "{injection} outside the block");
            assert!(!structure.contains(&needle), "{injection} in structure");
            if text[start..end].to_lowercase().contains(&needle) {
                seen += 1;
            }
        }
    }
    assert!(seen >= 2, "the injected text was reachable ({seen} times)");
}
