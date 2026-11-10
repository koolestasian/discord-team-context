//! HTTP interfaces for teamctx: the MCP endpoint, the JSON API, the static web view and
//! token authentication.
//!
//! Boundary: serves the record from `teamctx-store`. It does not talk to Discord or to an
//! LLM provider directly.

mod api;
pub mod ask;
pub mod auth;
pub mod mcp;
pub mod secret;
pub mod status;
pub mod stdio;

use std::{net::SocketAddr, sync::Arc};

use axum::{Router, middleware};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{Auth, TokenBackend},
    mcp::{McpConfig, McpServer},
};

/// Settings for [`router`].
pub struct HttpConfig {
    /// Where access tokens are looked up (normally [`auth::StoreTokens`]).
    pub tokens: Arc<dyn TokenBackend>,
    /// `Host` values the MCP endpoint accepts, as `host` or `host:port`. rmcp rejects any
    /// other `Host` with 403 to stop DNS-rebinding attacks. The default list is loopback
    /// names only, so a deployment behind a real hostname must set this.
    pub allowed_hosts: Vec<String>,
    /// Stops the MCP service (open SSE streams) when cancelled.
    pub shutdown: CancellationToken,
    /// The record the MCP tools read.
    pub mcp: McpConfig,
    /// Optional model backend, supplied by the binary.
    pub ask: Option<Arc<dyn ask::AskBackend>>,
}

impl std::fmt::Debug for HttpConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpConfig")
            .field("allowed_hosts", &self.allowed_hosts)
            .field("mcp", &self.mcp)
            .finish_non_exhaustive()
    }
}

/// Builds the application router: `/mcp` and `/api/v1/*` behind bearer auth, and the
/// unauthenticated `/healthz`.
///
/// Serve it with [`serve`] (or `into_make_service_with_connect_info::<SocketAddr>()`) so
/// the auth failure limit counts per client address; without connection info every
/// client shares one bucket.
pub fn router(config: HttpConfig) -> Router {
    let auth = Auth::new(config.tokens);
    // One read connection serves both the MCP tools and the JSON API, so they cannot
    // disagree about the record.
    let state = Arc::new(config.mcp.into_state());
    let mcp_config = StreamableHttpServerConfig::default()
        // 2026-07-28 removed protocol-level sessions. With this off, an `initialize` from an
        // older client is still served, but no `Mcp-Session-Id` is minted, so a server
        // restart does not strand clients with a session id the server no longer knows.
        .with_legacy_session_mode(false)
        .with_allowed_hosts(config.allowed_hosts)
        // The spec requires a 403 for a present, invalid `Origin`. Agents send no `Origin`;
        // browsers always do, so an empty allow-list means "no browser origin is welcome".
        .enforce_origin_validation()
        .with_cancellation_token(config.shutdown);
    let api = api::routes(Arc::clone(&state)).merge(ask::routes(config.ask));
    let mcp = StreamableHttpService::new(
        move || Ok(McpServer::new(Arc::clone(&state))),
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );
    // The layer wraps only this sub-router, so routes added later (such as `/healthz`)
    // are not accidentally put behind, or left outside, the MCP auth.
    let protected = Router::new()
        .nest_service("/mcp", mcp)
        .merge(api)
        .layer(middleware::from_fn_with_state(auth, auth::require_bearer));
    Router::new().merge(api::health()).merge(protected)
}

/// Serves `app` on `listener` until `shutdown` is cancelled.
///
/// This is the one place the server is started: it hands every request the peer's
/// socket address (`into_make_service_with_connect_info`), which the failed-auth limiter
/// uses to count per client.
///
/// # Errors
///
/// The I/O error that stopped the server.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { shutdown.cancelled_owned().await })
    .await
}
