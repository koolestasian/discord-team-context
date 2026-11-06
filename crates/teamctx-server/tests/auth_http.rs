//! Token authentication over real HTTP: 401 shapes, revocation (US-10) including a
//! revocation racing the request, `last_used_at`, the failure rate limit under
//! concurrency and one lookup per presented token. Log capture is in `auth_logs.rs`.
//!
//! Tokens are generated per run (or are obviously fake) and never printed.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

mod common;

use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use axum::{Extension, Router, middleware, routing::get};
use common::{TestDb, TestServer};
use reqwest::{Client, Response, StatusCode};
use serde_json::json;
use teamctx_core::{
    id::TokenId,
    repo::{AccessToken, RepoError, RepoResult, TokenHash, TokenWrite},
    time::UnixMillis,
};
use teamctx_server::auth::{self, Auth, Caller, StoreTokens, TokenBackend, token};
use teamctx_store::Store;
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
type Shape = (StatusCode, Option<String>, String);

async fn shape(response: Response) -> Shape {
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

/// A minimal app behind the auth middleware with its default limits; the handler
/// answers with the caller's token name.
fn protected(backend: Arc<dyn TokenBackend>) -> Router {
    Router::new()
        .route(
            "/",
            get(|Extension(caller): Extension<Caller>| async move { caller.name }),
        )
        .layer(middleware::from_fn_with_state(
            Auth::new(backend),
            auth::require_bearer,
        ))
}

/// Serves `app` on `127.0.0.1:0` with per-address connection info.
async fn serve(app: Router) -> TestServer {
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
    TestServer { url, shutdown }
}

fn sample_token() -> AccessToken {
    AccessToken {
        id: TokenId::new(7).expect("id"),
        name: "robin-laptop".to_owned(),
        scopes: "read".to_owned(),
        created_at: UnixMillis::from_millis(1),
        last_used_at: None,
        revoked_at: None,
    }
}

/// Counts lookups and returns a token only for one digest.
struct CountingBackend {
    valid: TokenHash,
    lookups: AtomicUsize,
}

impl TokenBackend for CountingBackend {
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok((*hash == self.valid).then(sample_token))
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
    let server = serve(protected(Arc::clone(&backend) as Arc<dyn TokenBackend>)).await;
    let client = Client::new();
    let get = |authorization: Option<String>| {
        let mut request = client.get(&server.url);
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
}

/// The real store backend, except that it revokes each token right after looking it up
/// and before its use is recorded: the revocation race, forced to happen every time.
struct RevokeAfterLookup {
    inner: StoreTokens,
    db_path: PathBuf,
}

impl TokenBackend for RevokeAfterLookup {
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>> {
        let found = self.inner.active_token(hash)?;
        if let Some(token) = &found {
            // A separate connection, as `teamctx token revoke` runs in another process.
            Store::open(&self.db_path)
                .expect("cli connection")
                .revoke_token(&token.name, UnixMillis::from_millis(2))
                .expect("revoke");
        }
        Ok(found)
    }
    fn touch_token(&self, id: TokenId, at: UnixMillis) -> RepoResult<()> {
        self.inner.touch_token(id, at)
    }
}

/// US-10 under a race: a token revoked after the lookup but before the request is
/// authorised must not authorise it.
#[tokio::test]
async fn a_token_revoked_between_lookup_and_use_is_rejected() {
    let db = TestDb::new();
    let token = db.create_token("robin-laptop");
    let backend = RevokeAfterLookup {
        inner: db.backend(),
        db_path: db.path.clone(),
    };
    let server = serve(protected(Arc::new(backend))).await;

    let response = Client::new()
        .get(&server.url)
        .header("Authorization", format!("Bearer {token}"))
        .send()
        .await
        .expect("send");
    assert_eq!(
        shape(response).await,
        (
            StatusCode::UNAUTHORIZED,
            Some(CHALLENGE_INVALID.to_owned()),
            String::new()
        )
    );
    let row = db.token_row("robin-laptop");
    assert!(row.revoked_at.is_some(), "the race was not forced");
    assert_eq!(
        row.last_used_at, None,
        "a revoked token was recorded as used"
    );
}

type MakeError = fn() -> RepoError;

/// Accepts any well-formed token on lookup, then fails to record its use.
struct TouchFails(MakeError);

impl TokenBackend for TouchFails {
    fn active_token(&self, _: &TokenHash) -> RepoResult<Option<AccessToken>> {
        Ok(Some(sample_token()))
    }
    fn touch_token(&self, _: TokenId, _: UnixMillis) -> RepoResult<()> {
        Err((self.0)())
    }
}

/// Recording the use is the authorisation step, so its failure fails closed: a token
/// that is gone or revoked by then gets the usual 401, a storage failure the usual 503.
#[tokio::test]
async fn a_failed_use_update_never_authorises_the_request() {
    let cases: [(&str, MakeError, Shape); 2] = [
        (
            "revoked or deleted meanwhile",
            || RepoError::NotFound { what: "token" },
            (
                StatusCode::UNAUTHORIZED,
                Some(CHALLENGE_INVALID.to_owned()),
                String::new(),
            ),
        ),
        (
            "storage failure",
            || RepoError::Backend("disk full".into()),
            (StatusCode::SERVICE_UNAVAILABLE, None, String::new()),
        ),
    ];
    for (what, error, expected) in cases {
        let server = serve(protected(Arc::new(TouchFails(error)))).await;
        let response = Client::new()
            .get(&server.url)
            .header("Authorization", format!("Bearer {UNKNOWN}"))
            .send()
            .await
            .expect("send");
        assert_eq!(shape(response).await, expected, "{what}");
    }
}

#[derive(Default)]
struct Gate {
    lookups: usize,
    open: bool,
}

/// Holds every lookup until the gate opens and counts how many reached it; finds no
/// token. Stands in for a slow or stalled database.
#[derive(Default)]
struct GatedBackend {
    gate: Mutex<Gate>,
    changed: Condvar,
}

impl GatedBackend {
    fn lookups(&self) -> usize {
        self.gate.lock().expect("gate").lookups
    }

    fn open(&self) {
        self.gate.lock().expect("gate").open = true;
        self.changed.notify_all();
    }
}

impl TokenBackend for GatedBackend {
    fn active_token(&self, _: &TokenHash) -> RepoResult<Option<AccessToken>> {
        let mut gate = self.gate.lock().expect("gate");
        gate.lookups += 1;
        // Bounded, so a failing test cannot leave blocking threads behind forever.
        let _gate = self
            .changed
            .wait_timeout_while(gate, Duration::from_secs(30), |gate| !gate.open)
            .expect("gate");
        Ok(None)
    }
    fn touch_token(&self, _: TokenId, _: UnixMillis) -> RepoResult<()> {
        Ok(())
    }
}

/// Opens the gate when dropped, including when an assertion fails.
struct OpenOnDrop(Arc<GatedBackend>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// An attacker controls concurrency: a burst of guesses against a slow backend must not
/// get more lookups than the failure limit allows.
#[tokio::test]
async fn a_concurrent_burst_cannot_exceed_the_failure_limit() {
    const BURST: usize = 30;
    // Documented default: 10 failures per 60 seconds per client address.
    const LIMIT: usize = 10;
    let backend = Arc::new(GatedBackend::default());
    let _release = OpenOnDrop(Arc::clone(&backend));
    let server = serve(protected(Arc::clone(&backend) as Arc<dyn TokenBackend>)).await;
    let client = Client::new();
    let answered = Arc::new(AtomicUsize::new(0));

    let mut requests = Vec::new();
    for _ in 0..BURST {
        let (client, url, answered) = (client.clone(), server.url.clone(), Arc::clone(&answered));
        requests.push(tokio::spawn(async move {
            let response = client
                .get(&url)
                .header("Authorization", format!("Bearer {UNKNOWN}"))
                .send()
                .await
                .expect("send");
            answered.fetch_add(1, Ordering::SeqCst);
            let retry = response
                .headers()
                .get("retry-after")
                .map(|v| v.to_str().expect("ascii").parse::<u64>().expect("seconds"));
            (response.status(), retry)
        }));
    }

    // Wait until every request has been answered or is held in the backend.
    let settled = {
        let (backend, answered) = (Arc::clone(&backend), Arc::clone(&answered));
        tokio::task::spawn_blocking(move || {
            let deadline = Instant::now() + Duration::from_secs(20);
            while Instant::now() < deadline {
                if backend.lookups() + answered.load(Ordering::SeqCst) >= BURST {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            false
        })
        .await
        .expect("wait")
    };
    assert!(
        settled,
        "requests were neither answered nor held in the backend"
    );
    assert_eq!(backend.lookups(), LIMIT, "lookups admitted by the burst");

    backend.open();
    let mut unauthorized = 0;
    let mut limited = 0;
    for request in requests {
        match request.await.expect("request task") {
            (StatusCode::UNAUTHORIZED, None) => unauthorized += 1,
            (StatusCode::TOO_MANY_REQUESTS, Some(retry)) => {
                assert!((1..=61).contains(&retry), "{retry}");
                limited += 1;
            }
            other => panic!("unexpected response {other:?}"),
        }
    }
    assert_eq!((unauthorized, limited), (LIMIT, BURST - LIMIT));

    // Every failure was counted: the client is now locked out without another lookup.
    let after = client
        .get(&server.url)
        .header("Authorization", format!("Bearer {UNKNOWN}"))
        .send()
        .await
        .expect("send");
    assert_eq!(after.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(backend.lookups(), LIMIT);
}
