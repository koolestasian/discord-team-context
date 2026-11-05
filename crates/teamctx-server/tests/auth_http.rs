//! Token authentication over real HTTP: 401 shapes, revocation (US-10), `last_used_at`,
//! the failure rate limit and one lookup per presented token. Log capture is in
//! `auth_logs.rs`.
//!
//! Tokens are generated per run (or are obviously fake) and never printed.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

mod common;

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{Extension, Router, middleware, routing::get};
use common::{TestDb, TestServer};
use reqwest::{Client, Response, StatusCode};
use serde_json::json;
use teamctx_core::{
    id::TokenId,
    repo::{AccessToken, RepoResult, TokenHash},
    time::UnixMillis,
};
use teamctx_server::auth::{self, Auth, Caller, TokenBackend, token};
use tokio_util::sync::CancellationToken;

const CHALLENGE_MISSING: &str = r#"Bearer realm="teamctx""#;
const CHALLENGE_INVALID: &str = r#"Bearer realm="teamctx", error="invalid_token""#;
/// Right shape, never issued.
const UNKNOWN: &str = "tctx_0000000000000000000000000000000000000000000000000000000000000000";

/// A stateless MCP `tools/list` call; succeeds with any valid token.
async fn call(client: &Client, server: &TestServer, authorization: Option<&str>) -> Response {
    let mut request = client
        .post(&server.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/list")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "auth-test", "version": "0.0.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        }));
    if let Some(value) = authorization {
        request = request.header("Authorization", value);
    }
    request.send().await.expect("send")
}

/// Status, challenge header and body, for comparing rejections byte for byte.
async fn shape(response: Response) -> (StatusCode, Option<String>, String) {
    let status = response.status();
    let challenge = response
        .headers()
        .get("www-authenticate")
        .map(|v| v.to_str().expect("ascii").to_owned());
    (status, challenge, response.text().await.expect("body"))
}

#[tokio::test]
async fn missing_malformed_unknown_and_revoked_tokens_get_401() {
    let db = TestDb::new();
    let valid = db.create_token("robin-laptop");
    let revoked = db.create_token("old-laptop");
    db.revoke_from_cli("old-laptop");
    let server = common::start(&db).await;
    let client = Client::new();

    let missing = shape(call(&client, &server, None).await).await;
    assert_eq!(
        missing,
        (
            StatusCode::UNAUTHORIZED,
            Some(CHALLENGE_MISSING.to_owned()),
            String::new()
        )
    );

    let expected = (
        StatusCode::UNAUTHORIZED,
        Some(CHALLENGE_INVALID.to_owned()),
        String::new(),
    );
    let upper = valid.to_uppercase();
    let rejected = [
        ("malformed scheme", "Basic dXNlcjpwdw==".to_owned()),
        ("empty token", "Bearer ".to_owned()),
        ("wrong shape", "Bearer tctx_test_fake".to_owned()),
        ("truncated", format!("Bearer {}", &valid[..valid.len() - 1])),
        ("case changed", format!("Bearer {upper}")),
        ("unknown", format!("Bearer {UNKNOWN}")),
        ("revoked", format!("Bearer {revoked}")),
    ];
    for (what, header) in rejected {
        let got = shape(call(&client, &server, Some(&header)).await).await;
        assert_eq!(got, expected, "{what}");
    }

    let ok = call(&client, &server, Some(&format!("Bearer {valid}"))).await;
    assert_eq!(ok.status(), StatusCode::OK);
}

/// US-10: `teamctx token revoke <name>` -> the next request with that token gets 401.
#[tokio::test]
async fn revocation_applies_to_the_very_next_request() {
    let db = TestDb::new();
    let token = db.create_token("robin-laptop");
    let other = db.create_token("sam-desktop");
    let server = common::start(&db).await;
    let client = Client::new();
    let bearer = format!("Bearer {token}");

    let before = call(&client, &server, Some(&bearer)).await;
    assert_eq!(before.status(), StatusCode::OK);

    db.revoke_from_cli("robin-laptop");

    let after = shape(call(&client, &server, Some(&bearer)).await).await;
    assert_eq!(after.0, StatusCode::UNAUTHORIZED);
    assert_eq!(after.1.as_deref(), Some(CHALLENGE_INVALID));
    // Revoking one person's token leaves the others working.
    let still = call(&client, &server, Some(&format!("Bearer {other}"))).await;
    assert_eq!(still.status(), StatusCode::OK);
}

#[tokio::test]
async fn a_valid_token_updates_last_used_at_and_a_rejected_one_does_not() {
    let db = TestDb::new();
    let token = db.create_token("robin-laptop");
    let revoked = db.create_token("old-laptop");
    db.revoke_from_cli("old-laptop");
    assert_eq!(db.token_row("robin-laptop").last_used_at, None);
    let server = common::start(&db).await;
    let client = Client::new();

    let rejected = call(&client, &server, Some(&format!("Bearer {revoked}"))).await;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(db.token_row("old-laptop").last_used_at, None);

    let ok = call(&client, &server, Some(&format!("Bearer {token}"))).await;
    assert_eq!(ok.status(), StatusCode::OK);
    let used = db
        .token_row("robin-laptop")
        .last_used_at
        .expect("last_used_at set");
    // Stamped with the wall clock (after this test file was written), not a test value.
    assert!(
        used > UnixMillis::from_millis(1_700_000_000_000),
        "{used:?}"
    );
}

#[tokio::test]
async fn repeated_failures_lock_the_client_out_even_with_a_valid_token() {
    let db = TestDb::new();
    let token = db.create_token("robin-laptop");
    let server = common::start(&db).await;
    let client = Client::new();
    let bearer = format!("Bearer {token}");

    // Documented default: 10 failures per 60 seconds per client address.
    for attempt in 1..=10 {
        let response = call(&client, &server, Some(&format!("Bearer {UNKNOWN}"))).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{attempt}");
    }
    for _ in 0..2 {
        let response = call(&client, &server, Some(&bearer)).await;
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry: u64 = response
            .headers()
            .get("retry-after")
            .expect("Retry-After")
            .to_str()
            .expect("ascii")
            .parse()
            .expect("seconds");
        assert!((1..=61).contains(&retry), "{retry}");
    }
    // A locked-out client's requests are refused before the token is checked, so a
    // valid token was not used.
    assert_eq!(db.token_row("robin-laptop").last_used_at, None);
}

/// Counts lookups and returns a token only for one digest.
struct CountingBackend {
    valid: TokenHash,
    lookups: AtomicUsize,
}

impl TokenBackend for CountingBackend {
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok((*hash == self.valid).then(|| AccessToken {
            id: TokenId::new(7).expect("id"),
            name: "robin-laptop".to_owned(),
            scopes: "read".to_owned(),
            created_at: UnixMillis::from_millis(1),
            last_used_at: None,
            revoked_at: None,
        }))
    }
    fn touch_token(&self, _: TokenId, _: UnixMillis) -> RepoResult<()> {
        Ok(())
    }
}

#[tokio::test]
async fn every_presented_header_costs_exactly_one_lookup() {
    let valid = token::generate().expect("random bytes");
    let backend = Arc::new(CountingBackend {
        valid: valid.hash,
        lookups: AtomicUsize::new(0),
    });
    let state = Auth::new(Arc::clone(&backend) as Arc<dyn TokenBackend>);
    let app = Router::new()
        .route(
            "/",
            get(|Extension(caller): Extension<Caller>| async move { caller.name }),
        )
        .layer(middleware::from_fn_with_state(state, auth::require_bearer));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}/", listener.local_addr().expect("addr"));
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let _ = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move { stop.cancelled_owned().await })
        .await;
    });
    let client = Client::new();
    let get = |authorization: Option<String>| {
        let mut request = client.get(&url);
        if let Some(value) = authorization {
            request = request.header("Authorization", value);
        }
        request.send()
    };

    // A missing header reveals nothing secret and is not looked up.
    let response = get(None).await.expect("send");
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(backend.lookups.load(Ordering::SeqCst), 0);

    // Wrong scheme, wrong shape and unknown token all do the same work.
    let presented = [
        "Basic dXNlcjpwdw==".to_owned(),
        "Bearer tctx_test_fake".to_owned(),
        format!("Bearer {UNKNOWN}"),
    ];
    for (done, header) in presented.into_iter().enumerate() {
        let response = get(Some(header)).await.expect("send");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(backend.lookups.load(Ordering::SeqCst), done + 1);
    }

    // A valid token reaches the handler, which sees who called.
    let response = get(Some(format!("Bearer {}", valid.plaintext.expose())))
        .await
        .expect("send");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text().await.expect("body"), "robin-laptop");
    assert_eq!(backend.lookups.load(Ordering::SeqCst), 4);
    shutdown.cancel();
}
