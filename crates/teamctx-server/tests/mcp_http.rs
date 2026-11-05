//! Spike S3: the MCP endpoint over real HTTP, behind bearer auth.
//!
//! Every test starts the server on `127.0.0.1:0` with a temp-file token store and talks to
//! it with `reqwest`. Tokens are generated per run and never printed.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(
    dead_code,
    reason = "this binary does not need the revocation helpers; auth_http.rs does"
)]
mod common;

use common::{TestDb, TestServer};
use reqwest::{Client, Response, StatusCode};
use serde_json::{Value, json};

/// A server with one real token, created through the token store.
async fn start() -> (TestDb, TestServer, String) {
    let db = TestDb::new();
    let token = db.create_token("s3-test");
    let server = common::start(&db).await;
    (db, server, token)
}

/// POST a JSON-RPC body the way a streamable-HTTP client does.
fn post(client: &Client, server: &TestServer, body: &Value) -> reqwest::RequestBuilder {
    client
        .post(&server.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(body)
}

/// Read a JSON-RPC message from a response that is either plain JSON or one SSE stream.
async fn rpc_message(response: Response) -> (Option<String>, Value) {
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let text = response.text().await.expect("body");
    let is_sse = content_type
        .as_deref()
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
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
    let value = serde_json::from_str(&json_text).unwrap_or(Value::Null);
    (content_type, value)
}

fn initialize_body(version: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": { "name": "s3-test-client", "version": "0.0.0" }
        }
    })
}

fn assert_challenge(response: &Response) {
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let challenge = response
        .headers()
        .get("www-authenticate")
        .expect("401 carries WWW-Authenticate")
        .to_str()
        .expect("ascii challenge");
    assert!(challenge.starts_with("Bearer"), "challenge: {challenge}");
}

#[tokio::test]
async fn rejects_missing_malformed_and_wrong_tokens_with_401() {
    let (_db, server, token) = start().await;
    let client = Client::new();
    let body = initialize_body("2025-11-25");

    let bad_headers = [
        None,
        Some("Bearer".to_owned()),
        Some("Bearer ".to_owned()),
        Some("Basic dXNlcjpwdw==".to_owned()),
        Some("Bearer tctx_test_wrong".to_owned()),
        Some(format!("Bearer {token} extra")),
        // A prefix of the right token must not match.
        Some(format!("Bearer {}", &token[..token.len() - 1])),
        Some(token.clone()),
    ];
    for header in bad_headers {
        let mut request = post(&client, &server, &body);
        if let Some(value) = header {
            request = request.header("Authorization", value);
        }
        let response = request.send().await.expect("send");
        assert_challenge(&response);
    }

    // The token in the query string is not accepted (spec: MUST NOT use the URI).
    let response = client
        .post(format!("{}?access_token={token}", server.url))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .json(&body)
        .send()
        .await
        .expect("send");
    assert_challenge(&response);

    // GET without a token is also challenged before rmcp sees it.
    let response = client.get(&server.url).send().await.expect("send");
    assert_challenge(&response);
}

#[tokio::test]
async fn scheme_name_is_case_insensitive() {
    let (_db, server, token) = start().await;
    let response = post(&Client::new(), &server, &initialize_body("2025-11-25"))
        .header("Authorization", format!("bearer {token}"))
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn initialize_initialized_tools_list_with_the_right_token() {
    let (_db, server, token) = start().await;
    let client = Client::new();

    // 1. initialize
    let response = post(&client, &server, &initialize_body("2025-11-25"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("initialize");
    assert_eq!(response.status(), StatusCode::OK);
    // Stateless mode: no session is minted.
    assert!(response.headers().get("mcp-session-id").is_none());
    let (content_type, message) = rpc_message(response).await;
    // The single reply still arrives as a one-event SSE stream, not plain JSON.
    assert_eq!(content_type.as_deref(), Some("text/event-stream"));
    assert_eq!(message["id"], 1);
    assert_eq!(message["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(message["result"]["serverInfo"]["name"], "teamctx");

    // 2. notifications/initialized
    let response = post(
        &client,
        &server,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .bearer_auth(&token)
    .header("MCP-Protocol-Version", "2025-11-25")
    .send()
    .await
    .expect("initialized");
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // 3. tools/list
    let response = post(
        &client,
        &server,
        &json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }),
    )
    .bearer_auth(&token)
    .header("MCP-Protocol-Version", "2025-11-25")
    .send()
    .await
    .expect("tools/list");
    assert_eq!(response.status(), StatusCode::OK);
    let (_, message) = rpc_message(response).await;
    let tools = message["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1, "tools: {tools:?}");
    assert_eq!(tools[0]["name"], "ping");
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
}

#[tokio::test]
async fn call_ping_returns_the_fixed_line() {
    let (_db, server, token) = start().await;
    let client = Client::new();
    let response = post(
        &client,
        &server,
        &json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": { "name": "ping", "arguments": {} }
        }),
    )
    .bearer_auth(&token)
    .header("MCP-Protocol-Version", "2025-11-25")
    .send()
    .await
    .expect("tools/call");
    assert_eq!(response.status(), StatusCode::OK);
    let (_, message) = rpc_message(response).await;
    assert_eq!(
        message["result"]["content"][0]["text"],
        "pong from the Example Team record"
    );
}

#[tokio::test]
async fn stateless_2026_07_28_request_needs_no_initialize() {
    let (_db, server, token) = start().await;
    let client = Client::new();
    let response = post(
        &client,
        &server,
        &json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "s3-test-client", "version": "0.0.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        }),
    )
    .bearer_auth(&token)
    .header("MCP-Protocol-Version", "2026-07-28")
    .header("Mcp-Method", "tools/list")
    .send()
    .await
    .expect("tools/list");
    let status = response.status();
    let session = response.headers().get("mcp-session-id").cloned();
    let (_, message) = rpc_message(response).await;
    assert_eq!(status, StatusCode::OK);
    assert!(session.is_none());
    assert_eq!(message["result"]["tools"][0]["name"], "ping");
}

#[tokio::test]
async fn browser_origin_is_refused_even_with_a_valid_token() {
    let (_db, server, token) = start().await;
    let response = post(&Client::new(), &server, &initialize_body("2025-11-25"))
        .bearer_auth(&token)
        .header("Origin", "https://evil.example")
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn get_and_delete_with_a_valid_token_are_not_allowed() {
    let (_db, server, token) = start().await;
    let client = Client::new();
    let get = client
        .get(&server.url)
        .bearer_auth(&token)
        .header("Accept", "text/event-stream")
        .send()
        .await
        .expect("get");
    let delete = client
        .delete(&server.url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete");
    assert_eq!(get.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(delete.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn unlisted_host_header_is_refused_even_with_a_valid_token() {
    let (_db, server, token) = start().await;
    let response = post(&Client::new(), &server, &initialize_body("2025-11-25"))
        .bearer_auth(&token)
        .header("Host", "evil.example")
        .send()
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
