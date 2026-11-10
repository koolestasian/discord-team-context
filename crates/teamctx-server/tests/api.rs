//! The JSON API under `/api/v1` over real HTTP, on the fictional demo record.
//!
//! Every test starts the server on `127.0.0.1:0` with a temp-file database that holds the
//! demo dataset and a per-run token. Chat text in the demo set is untrusted data.

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
use reqwest::{Client, Response, StatusCode};
use serde_json::Value;
use teamctx_core::framing::{BLOCK_END, BLOCK_START, NOTICE};

struct Harness {
    db: TestDb,
    base: String,
    token: String,
    client: Client,
    _server: TestServer,
}

async fn start() -> Harness {
    let db = TestDb::with_demo();
    let token = db.create_token("t13-test");
    let server = common::start(&db).await;
    let base = server
        .url
        .strip_suffix("/mcp")
        .expect("harness url ends in /mcp")
        .to_owned();
    Harness {
        db,
        base,
        token,
        client: Client::new(),
        _server: server,
    }
}

impl Harness {
    async fn get(&self, path_and_query: &str) -> Response {
        self.client
            .get(format!("{}{path_and_query}", self.base))
            .bearer_auth(&self.token)
            .send()
            .await
            .expect("send")
    }

    /// A `200` JSON body.
    async fn ok(&self, path_and_query: &str) -> Value {
        let response = self.get(path_and_query).await;
        assert_eq!(response.status(), StatusCode::OK, "{path_and_query}");
        response.json().await.expect("JSON body")
    }

    async fn error(&self, path_and_query: &str, status: StatusCode) -> String {
        let response = self.get(path_and_query).await;
        assert_eq!(response.status(), status, "{path_and_query}");
        let body: Value = response.json().await.expect("JSON body");
        body["error"].as_str().expect("error text").to_owned()
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

const ROUTES: [&str; 6] = [
    "/api/v1/context?text=reading+position",
    "/api/v1/whats-new?since=2026-04-14",
    "/api/v1/record",
    "/api/v1/search?q=libspine",
    "/api/v1/read?id=m:1",
    "/api/v1/status",
];

#[tokio::test]
async fn healthz_needs_no_token_and_reveals_nothing_else() {
    let h = start().await;
    let response = Client::new()
        .get(format!("{}/healthz", h.base))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = response.json().await.expect("JSON body");
    assert_eq!(body, serde_json::json!({ "status": "ok" }));
}

/// The failed-auth limiter allows 10 failures a minute per address, so each kind of
/// failure gets its own server and makes at most one request per route.
fn assert_challenge(response: &Response, route: &str) {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{route}");
    assert!(
        response
            .headers()
            .get("www-authenticate")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("Bearer")),
        "{route}"
    );
}

#[tokio::test]
async fn every_api_route_rejects_a_missing_token_with_401() {
    let h = start().await;
    for route in ROUTES {
        let response = Client::new()
            .get(format!("{}{route}", h.base))
            .send()
            .await
            .expect("send");
        assert_challenge(&response, route);
    }
}

#[tokio::test]
async fn every_api_route_rejects_a_wrong_token_or_one_in_the_query_with_401() {
    let h = start().await;
    for route in &ROUTES[..3] {
        let wrong = Client::new()
            .get(format!("{}{route}", h.base))
            .bearer_auth("tctx_not_a_real_token")
            .send()
            .await
            .expect("send");
        assert_challenge(&wrong, route);
    }
    // A token in the query string is not accepted (spec: MUST NOT use the URI).
    for route in &ROUTES[3..] {
        let sep = if route.contains('?') { '&' } else { '?' };
        let in_query = Client::new()
            .get(format!("{}{route}{sep}access_token={}", h.base, h.token))
            .send()
            .await
            .expect("send");
        assert_challenge(&in_query, route);
    }
}

#[tokio::test]
async fn a_revoked_token_gets_401_on_the_next_request_to_every_route() {
    // US-10 over the API.
    let h = start().await;
    assert_eq!(h.get("/api/v1/status").await.status(), StatusCode::OK);
    h.db.revoke_from_cli("t13-test");
    for route in ROUTES {
        assert_challenge(&h.get(route).await, route);
    }
}

#[tokio::test]
async fn context_for_a_branch_returns_entries_first_then_cited_messages() {
    // US-7 over HTTP: an agent's helper asks for the context of its branch.
    let h = start().await;
    let body = h
        .ok("/api/v1/context?text=fix%2Freading-position-conflicts")
        .await;
    let kinds: Vec<&str> = body["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|i| i["kind"].as_str().expect("kind"))
        .collect();
    let first_message = kinds
        .iter()
        .position(|k| *k == "message")
        .expect("messages follow the entries");
    assert!(first_message > 0, "entries come first: {kinds:?}");
    assert!(kinds[..first_message].contains(&"decision"), "{kinds:?}");
    let text = body["text"].as_str().expect("text");
    assert!(text.contains("Reading position"), "{text}");
    for item in body["items"].as_array().expect("items") {
        let id = item["id"].as_str().expect("id");
        assert!(text.contains(id), "{id} is shown in the text");
        if item["kind"] == "message" {
            assert!(
                item["link"]
                    .as_str()
                    .is_some_and(|l| l.starts_with("https://discord.com/channels/")),
                "{item}"
            );
        }
    }
    // `limit` is honoured and clamped like the MCP tool.
    let one = h.ok("/api/v1/context?text=reading+position&limit=1").await;
    let entries = one["items"]
        .as_array()
        .expect("items")
        .iter()
        .filter(|i| i["kind"] != "message")
        .count();
    assert_eq!(entries, 1);
}

#[tokio::test]
async fn record_filters_pages_and_takes_q_as_the_topic() {
    let h = start().await;
    let decisions = h.ok("/api/v1/record?kind=decision&status=active&q=").await;
    let items = decisions["items"].as_array().expect("items");
    assert!(!items.is_empty() && items.len() <= 8);
    assert!(
        items
            .iter()
            .all(|i| i["kind"] == "decision" && i["status"] == "active")
    );
    let cursor = decisions["next_cursor"].as_str().expect("a second page");
    let page_two = h
        .ok(&format!(
            "/api/v1/record?kind=decision&status=active&cursor={cursor}"
        ))
        .await;
    assert!(
        ids(&decisions)
            .iter()
            .all(|id| !ids(&page_two).contains(id))
    );

    let topic = h.ok("/api/v1/record?q=reading+position").await;
    assert!(!ids(&topic).is_empty());
    assert!(
        topic["text"]
            .as_str()
            .expect("text")
            .contains("Reading position")
    );
    let owned = h.ok("/api/v1/record?owner=wren+calloway").await;
    assert!(
        owned["text"]
            .as_str()
            .expect("text")
            .contains("owner Wren Calloway")
    );
    let recent = h.ok("/api/v1/record?since=2026-04-14").await;
    assert!(!ids(&recent).is_empty());
}

#[tokio::test]
async fn search_whats_new_read_and_status_answer() {
    let h = start().await;
    let found = h.ok("/api/v1/search?q=libspine").await;
    let message = ids(&found).first().cloned().expect("a hit");
    assert!(message.starts_with("m:"));
    let thread = h
        .ok(&format!("/api/v1/read?id={message}&before=1&after=1"))
        .await;
    assert!(ids(&thread).contains(&message));
    let entry = h.ok("/api/v1/read?id=D-1").await;
    assert_eq!(ids(&entry).first().map(String::as_str), Some("D-1"));

    let news = h.ok("/api/v1/whats-new?since=2026-04-14").await;
    assert!(!ids(&news).is_empty());
    assert!(
        news["text"]
            .as_str()
            .expect("text")
            .contains("messages since 2026-04-14")
    );

    let status = h.ok("/api/v1/status").await;
    assert_eq!(status["status"], "ok");
    assert_eq!(status["record"], "readable");
    assert!(status["version"].is_string());
}

#[tokio::test]
async fn bad_requests_get_400_and_never_echo_the_input() {
    let h = start().await;
    let canary = "canary-zq81";
    let bad = [
        format!("/api/v1/search?q=%22{canary}"),
        format!("/api/v1/search?q={canary}&unknown=1"),
        format!("/api/v1/search?q={canary}&q=again"),
        format!("/api/v1/search?q=%FF{canary}"),
        format!("/api/v1/search?q={canary}&cursor={canary}"),
        "/api/v1/search".to_owned(),
        format!("/api/v1/context?text={canary}&limit={canary}"),
        format!("/api/v1/whats-new?since={canary}"),
        "/api/v1/whats-new".to_owned(),
        format!("/api/v1/record?kind={canary}"),
        format!("/api/v1/record?status={canary}"),
        format!("/api/v1/record?owner={canary}"),
        format!("/api/v1/record?since={canary}"),
        format!("/api/v1/read?id={canary}"),
        "/api/v1/read?id=D-99999".to_owned(),
        "/api/v1/status?x=1".to_owned(),
    ];
    for path in &bad {
        let message = h.error(path, StatusCode::BAD_REQUEST).await;
        assert!(!message.contains(canary), "{path}: {message}");
        assert!(!message.is_empty());
    }
}

#[tokio::test]
async fn only_get_is_allowed() {
    let h = start().await;
    let response = h
        .client
        .post(format!("{}/api/v1/search?q=libspine", h.base))
        .bearer_auth(&h.token)
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn responses_are_private_json_and_start_with_the_notice() {
    let h = start().await;
    let response = h.get("/api/v1/search?q=libspine").await;
    let headers = response.headers().clone();
    assert_eq!(headers["content-type"], "application/json");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    let body: Value = response.json().await.expect("JSON body");
    let text = body["text"].as_str().expect("text");
    assert!(text.starts_with(NOTICE));
    assert_eq!(body["notice"], NOTICE);
    assert_eq!(text.matches(BLOCK_START).count(), 1);
    assert_eq!(text.matches(BLOCK_END).count(), 1);
}
