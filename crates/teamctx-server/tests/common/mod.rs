//! Shared harness: a temp-file database with real tokens and a server on `127.0.0.1:0`.
//!
//! Tokens are generated per test run and never printed.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; these helpers panic on failure too"
)]

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::Router;
use teamctx_core::{
    repo::{AccessToken, TokenRead, TokenWrite},
    time::{CalendarDate, UnixMillis},
};
use teamctx_server::{
    HttpConfig,
    auth::{StoreTokens, token},
    mcp::McpConfig,
    router, serve,
};
use teamctx_store::{Store, Writer};
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;

/// A migrated database file plus the runtime writer.
pub(crate) struct TestDb {
    pub(crate) path: PathBuf,
    pub(crate) writer: Writer,
    _dir: TempDir,
}

/// The fictional demo dataset (`fixtures/demo/dataset.json`).
const DEMO_DATASET: &str = include_str!("../../../../fixtures/demo/dataset.json");

/// "Now" for tests that read the demo record: the day after its last conversation.
pub(crate) fn demo_now() -> UnixMillis {
    CalendarDate::new(2026, 4, 25)
        .expect("valid date")
        .start_of_day_utc()
}

impl TestDb {
    pub(crate) fn new() -> Self {
        Self::build(false)
    }

    /// A database holding the demo record.
    pub(crate) fn with_demo() -> Self {
        Self::build(true)
    }

    fn build(demo: bool) -> Self {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("teamctx.db");
        let mut store = Store::open(&path).expect("open db");
        if demo {
            teamctx_store::demo::import(&mut store, DEMO_DATASET).expect("import demo");
        }
        let writer = Writer::spawn(store).expect("writer");
        Self {
            path,
            writer,
            _dir: dir,
        }
    }

    /// Creates a token the way `teamctx token create` does and returns its plaintext.
    pub(crate) fn create_token(&self, name: &str) -> String {
        let new = token::generate().expect("random bytes");
        self.writer
            .client()
            .create_token(name, &new.hash, "read", UnixMillis::from_millis(1))
            .expect("create token");
        new.plaintext.expose().clone()
    }

    /// Revokes from a separate connection, as the CLI does from another process.
    pub(crate) fn revoke_from_cli(&self, name: &str) {
        let mut cli = Store::open(&self.path).expect("cli connection");
        cli.revoke_token(name, UnixMillis::from_millis(2))
            .expect("revoke");
    }

    pub(crate) fn token_row(&self, name: &str) -> AccessToken {
        Store::open_read_only(&self.path)
            .expect("read connection")
            .tokens()
            .expect("tokens")
            .into_iter()
            .find(|t| t.name == name)
            .expect("token row")
    }

    pub(crate) fn backend(&self) -> StoreTokens {
        StoreTokens::new(
            Store::open_read_only(&self.path).expect("read connection"),
            self.writer.client(),
        )
    }
}

pub(crate) struct TestServer {
    pub(crate) url: String,
    pub(crate) shutdown: CancellationToken,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Serves the real application router for `db`, with per-address connection info.
pub(crate) async fn start(db: &TestDb) -> TestServer {
    start_with(db, |app| app).await
}

/// Like [`start`], but lets the test add routes to the application router first.
pub(crate) async fn start_with(db: &TestDb, extend: impl FnOnce(Router) -> Router) -> TestServer {
    let shutdown = CancellationToken::new();
    let mcp = McpConfig::new(Store::open_read_only(&db.path).expect("read connection"))
        .with_clock(Arc::new(demo_now));
    let app = extend(router(HttpConfig {
        tokens: Arc::new(db.backend()),
        allowed_hosts: vec!["127.0.0.1".to_owned(), "localhost".to_owned()],
        shutdown: shutdown.clone(),
        mcp,
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind 127.0.0.1:0");
    let addr: SocketAddr = listener.local_addr().expect("local addr");
    let stop = shutdown.clone();
    tokio::spawn(async move {
        let _ = serve(listener, app, stop).await;
    });
    TestServer {
        url: format!("http://{addr}/mcp"),
        shutdown,
    }
}
