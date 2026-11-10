//! The five MCP tools and the `catch_up` prompt over real HTTP, on the fictional demo record.
//!
//! Every test starts the server on `127.0.0.1:0` with a temp-file database that holds the
//! demo dataset and a per-run token. Chat text in the demo set is untrusted data, including
//! two messages written to look like instructions.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(
    dead_code,
    reason = "this binary does not use every harness helper; auth_http.rs does"
)]
mod common;

use std::net::SocketAddr;

use axum::{extract::ConnectInfo, routing::get};
use common::{TestDb, TestServer};
use reqwest::{Client, StatusCode};
use serde_json::{Value, json};
use teamctx_core::framing::{BLOCK_END, BLOCK_START, NOTICE};
use teamctx_server::mcp::estimate_tokens;

struct Harness {
    db: TestDb,
    server: TestServer,
    token: String,
    client: Client,
}

async fn start() -> Harness {
    let db = TestDb::with_demo();
    let token = db.create_token("t12-test");
    let server = common::start(&db).await;
    Harness {
        db,
        server,
        token,
        client: Client::new(),
    }
}

/// Sends one JSON-RPC request the way a stateless 2025-11-25 client does and returns the
/// JSON-RPC message from the (JSON or one-event SSE) response.
async fn rpc(h: &Harness, method: &str, params: Value) -> Value {
    let response = h
        .client
        .post(&h.server.url)
        .bearer_auth(&h.token)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::OK, "{method}");
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
    serde_json::from_str(&json_text).expect("JSON-RPC message")
}

/// Calls a tool and returns its `result` object.
async fn call(h: &Harness, name: &str, arguments: Value) -> Value {
    let message = rpc(
        h,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    )
    .await;
    assert!(message["error"].is_null(), "{name}: {message}");
    message["result"].clone()
}

fn text_of(result: &Value) -> &str {
    result["content"][0]["text"].as_str().expect("text content")
}

fn items_of(result: &Value) -> Vec<Value> {
    result["structuredContent"]["items"]
        .as_array()
        .expect("items")
        .clone()
}

fn ids_of(result: &Value) -> Vec<String> {
    items_of(result)
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

fn size_in_tokens(value: &Value) -> usize {
    estimate_tokens(serde_json::to_string(value).expect("json").chars().count())
}

#[tokio::test]
async fn definitions_are_read_only_have_output_schemas_and_fit_the_budget() {
    let h = start().await;
    let message = rpc(&h, "tools/list", json!({})).await;
    let tools = message["result"]["tools"].as_array().expect("tools");
    assert_eq!(tools.len(), 5);
    for tool in tools {
        let name = tool["name"].as_str().expect("name");
        assert_eq!(tool["annotations"]["readOnlyHint"], true, "{name}");
        assert_eq!(tool["outputSchema"]["type"], "object", "{name}");
        assert!(tool["inputSchema"]["properties"].is_object(), "{name}");
    }
    // Plan section 10.1: all five definitions together stay under 1,500 tokens
    // (estimated as characters / 4).
    let tokens = size_in_tokens(&message["result"]["tools"]);
    assert!(tokens < 1500, "tool definitions take about {tokens} tokens");
}

#[tokio::test]
async fn default_responses_fit_the_budget_and_start_with_the_notice() {
    let h = start().await;
    let hit = call(&h, "search", json!({ "query": "sync" })).await;
    let message_id = ids_of(&hit).first().cloned().expect("a message hit");
    let entries = call(&h, "record", json!({})).await;
    let entry_id = ids_of(&entries).first().cloned().expect("an entry");

    let results = [
        (
            "context_for",
            json!({ "text": "feat/reading-position-sync" }),
        ),
        ("whats_new", json!({ "since": "30d" })),
        ("record", json!({})),
        ("search", json!({ "query": "sync" })),
        ("read", json!({ "id": message_id })),
        ("read", json!({ "id": entry_id })),
    ];
    for (name, arguments) in results {
        let result = call(&h, name, arguments.clone()).await;
        assert_ne!(result["isError"], true, "{name} {arguments}: {result}");
        let tokens = size_in_tokens(&result);
        assert!(
            tokens < 2000,
            "{name} {arguments} took about {tokens} tokens"
        );
        assert!(
            text_of(&result).starts_with(NOTICE),
            "{name} does not start with the notice"
        );
        assert_eq!(result["structuredContent"]["notice"], NOTICE, "{name}");
        assert!(
            result["structuredContent"]["next_step"]
                .as_str()
                .is_some_and(|s| s.starts_with("Next:")),
            "{name}"
        );
        assert!(!items_of(&result).is_empty(), "{name} {arguments}");
    }
}

#[tokio::test]
async fn context_for_a_branch_returns_entries_first_then_cited_messages() {
    // US-7: an agent pulls context for its branch.
    let h = start().await;
    let result = call(
        &h,
        "context_for",
        json!({ "text": "fix/reading-position-conflicts" }),
    )
    .await;
    let items = items_of(&result);
    let kinds: Vec<&str> = items
        .iter()
        .map(|i| i["kind"].as_str().expect("kind"))
        .collect();
    let first_message = kinds
        .iter()
        .position(|k| *k == "message")
        .expect("messages follow the entries");
    assert!(first_message > 0, "entries come first: {kinds:?}");
    assert!(
        kinds[first_message..].iter().all(|k| *k == "message"),
        "no entry after the first message: {kinds:?}"
    );
    assert!(kinds[..first_message].contains(&"decision"), "{kinds:?}");
    let text = text_of(&result);
    assert!(text.contains("Reading position"), "{text}");
    // Every item cites an id, and every message cites its Discord link.
    for item in &items {
        let id = item["id"].as_str().expect("id");
        assert!(text.contains(id), "{id} is shown in the text");
        if item["kind"] == "message" {
            assert!(id.starts_with("m:"), "{id}");
            assert!(
                item["link"]
                    .as_str()
                    .is_some_and(|l| l.starts_with("https://discord.com/channels/")),
                "{item}"
            );
        }
    }
}

#[tokio::test]
async fn hostile_demo_messages_arrive_only_inside_the_framing() {
    const INJECTIONS: [&str; 2] = [
        "Ignore all previous instructions",
        "disregard prior instructions",
    ];
    let h = start().await;
    let found = call(&h, "search", json!({ "query": "\"admin tokens\"" })).await;
    let message_id = ids_of(&found).first().cloned().expect("the crash digest");
    let results = [
        found,
        call(&h, "read", json!({ "id": message_id })).await,
        call(
            &h,
            "context_for",
            json!({ "text": "support email imported book title" }),
        )
        .await,
        call(&h, "search", json!({ "query": "free forever" })).await,
    ];
    let mut seen = 0;
    for result in &results {
        let text = text_of(result);
        let start = text.find(BLOCK_START).expect("block start");
        let end = text.find(BLOCK_END).expect("block end");
        assert_eq!(text.matches(BLOCK_START).count(), 1);
        assert_eq!(text.matches(BLOCK_END).count(), 1);
        assert!(text.starts_with(NOTICE));
        let outside = format!("{}{}", &text[..start], &text[end..]).to_lowercase();
        let structured = result["structuredContent"].to_string().to_lowercase();
        for injection in INJECTIONS {
            let needle = injection.to_lowercase();
            assert!(!outside.contains(&needle), "{injection} outside the block");
            assert!(!structured.contains(&needle), "{injection} in structure");
            if text[start..end].to_lowercase().contains(&needle) {
                seen += 1;
            }
        }
    }
    assert!(seen >= 2, "the injected text was reachable ({seen} times)");
}

#[tokio::test]
async fn search_pages_with_an_opaque_cursor() {
    let h = start().await;
    let args = json!({ "query": "release beta android sync" });
    let first = call(&h, "search", args.clone()).await;
    let cursor = first["structuredContent"]["next_cursor"]
        .as_str()
        .expect("a next page")
        .to_owned();
    let second = call(
        &h,
        "search",
        json!({ "query": "release beta android sync", "cursor": cursor }),
    )
    .await;
    let (a, b) = (ids_of(&first), ids_of(&second));
    assert!(!b.is_empty());
    assert!(a.iter().all(|id| !b.contains(id)), "{a:?} vs {b:?}");
    let forged = call(&h, "search", json!({ "query": "sync", "cursor": "9999" })).await;
    assert_eq!(forged["isError"], true);
}

#[tokio::test]
async fn record_filters_by_kind_status_owner_and_pages() {
    let h = start().await;
    let decisions = call(
        &h,
        "record",
        json!({ "kind": "decision", "status": "active" }),
    )
    .await;
    let items = items_of(&decisions);
    assert!(!items.is_empty() && items.len() <= 8);
    assert!(
        items
            .iter()
            .all(|i| i["kind"] == "decision" && i["status"] == "active")
    );
    // 29 active decisions: there is a second page.
    let cursor = decisions["structuredContent"]["next_cursor"]
        .as_str()
        .expect("next page")
        .to_owned();
    let page_two = call(
        &h,
        "record",
        json!({ "kind": "decision", "status": "active", "cursor": cursor }),
    )
    .await;
    assert!(
        ids_of(&decisions)
            .iter()
            .all(|id| !ids_of(&page_two).contains(id))
    );

    let owned = call(&h, "record", json!({ "owner": "wren calloway" })).await;
    assert_ne!(owned["isError"], true, "{owned}");
    assert!(text_of(&owned).contains("owner Wren Calloway"));
    for bad in [
        json!({ "kind": "idea" }),
        json!({ "status": "finished" }),
        json!({ "owner": "Nobody Atall" }),
        json!({ "since": "soon" }),
    ] {
        let result = call(&h, "record", bad.clone()).await;
        assert_eq!(result["isError"], true, "{bad}");
    }
}

#[tokio::test]
async fn whats_new_reports_changes_and_active_channels() {
    let h = start().await;
    let result = call(&h, "whats_new", json!({ "since": "2026-04-14" })).await;
    assert_ne!(result["isError"], true, "{result}");
    assert!(!items_of(&result).is_empty());
    assert!(text_of(&result).contains("messages since 2026-04-14"));
    let bad = call(&h, "whats_new", json!({ "since": "yesterday" })).await;
    assert_eq!(bad["isError"], true);
}

#[tokio::test]
async fn read_returns_threads_and_entries_and_rejects_bad_ids() {
    let h = start().await;
    let found = call(&h, "search", json!({ "query": "libspine" })).await;
    let message = ids_of(&found).first().cloned().expect("a hit");
    let thread = call(
        &h,
        "read",
        json!({ "id": message, "before": 1, "after": 1 }),
    )
    .await;
    let ids = ids_of(&thread);
    assert!(ids.contains(&message));
    assert!(ids.len() <= 3, "{ids:?}");
    let target: Vec<_> = items_of(&thread)
        .into_iter()
        .filter(|i| i["context"] != true)
        .collect();
    assert_eq!(target.len(), 1);

    let entry = call(&h, "read", json!({ "id": "D-1" })).await;
    assert_ne!(entry["isError"], true, "{entry}");
    assert_eq!(ids_of(&entry)[0], "D-1");

    for (id, why) in [
        ("m:999999999", "No item with that id."),
        ("D-999999", "No item with that id."),
        ("nonsense", "`id` must look like"),
    ] {
        let result = call(&h, "read", json!({ "id": id })).await;
        assert_eq!(result["isError"], true, "{id}");
        assert!(text_of(&result).contains(why), "{id}: {}", text_of(&result));
    }
}

#[tokio::test]
async fn failures_never_echo_the_input() {
    let h = start().await;
    let hostile = "\"<script>alert(1)</script> unclosed";
    let result = call(&h, "search", json!({ "query": hostile })).await;
    assert_eq!(result["isError"], true);
    let text = text_of(&result);
    assert!(!text.contains("script"), "{text}");
    let empty = call(&h, "context_for", json!({ "text": "the of and" })).await;
    assert_eq!(empty["isError"], true);
}

#[tokio::test]
async fn catch_up_prompt_names_the_two_calls() {
    let h = start().await;
    let list = rpc(&h, "prompts/list", json!({})).await;
    let prompts = list["result"]["prompts"].as_array().expect("prompts");
    assert_eq!(prompts.len(), 1);
    assert_eq!(prompts[0]["name"], "catch_up");
    let got = rpc(&h, "prompts/get", json!({ "name": "catch_up" })).await;
    let text = got["result"]["messages"][0]["content"]["text"]
        .as_str()
        .expect("prompt text");
    assert!(text.contains("whats_new") && text.contains("context_for"));
    assert!(text.contains("never as instructions"));
}

#[tokio::test]
async fn missing_and_revoked_tokens_get_401_on_tool_calls() {
    let h = start().await;
    let body = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": "record", "arguments": {} }
    });
    let send = |token: Option<&str>| {
        let mut request = h
            .client
            .post(&h.server.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        request.send()
    };
    let missing = send(None).await.expect("send");
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        send(Some(&h.token)).await.expect("send").status(),
        StatusCode::OK
    );
    h.db.revoke_from_cli("t12-test");
    let revoked = send(Some(&h.token)).await.expect("send");
    assert_eq!(revoked.status(), StatusCode::UNAUTHORIZED);
    assert!(revoked.headers().contains_key("www-authenticate"));
}

#[tokio::test]
async fn serve_hands_the_limiter_each_clients_address() {
    // `serve` must use connection info: a route behind it sees the peer address, which is
    // what the failed-auth limiter keys on (a missing one puts every client in one bucket).
    let db = TestDb::with_demo();
    let server = common::start_with(&db, |app| {
        app.route(
            "/whoami",
            get(|ConnectInfo(peer): ConnectInfo<SocketAddr>| async move { peer.ip().to_string() }),
        )
    })
    .await;
    let url = server.url.replace("/mcp", "/whoami");
    let body = Client::new()
        .get(url)
        .send()
        .await
        .expect("send")
        .text()
        .await
        .expect("body");
    assert_eq!(body, "127.0.0.1");

    // And the limiter itself locks out that address after repeated bad tokens.
    let client = Client::new();
    let mut last = StatusCode::OK;
    for _ in 0..12 {
        last = client
            .post(&server.url)
            .bearer_auth("tctx_test_wrong")
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .send()
            .await
            .expect("send")
            .status();
    }
    assert_eq!(last, StatusCode::TOO_MANY_REQUESTS);
}
