//! HTTP interfaces for teamctx: the MCP endpoint, the JSON API, the static web view and
//! token authentication.
//!
//! Boundary: serves the record from `teamctx-store`. It does not talk to Discord or to an
//! LLM provider directly.

pub mod auth;
pub mod mcp;
pub mod secret;

use std::sync::Arc;

use axum::{Router, middleware};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use tokio_util::sync::CancellationToken;

use crate::{auth::BearerAuth, mcp::PingServer, secret::Secret};

/// Settings for [`router`].
#[derive(Debug)]
pub struct HttpConfig {
    /// The one accepted bearer token (spike scope; T11 replaces it with a token store).
    pub token: Secret<String>,
    /// `Host` values the MCP endpoint accepts, as `host` or `host:port`. rmcp rejects any
    /// other `Host` with 403 to stop DNS-rebinding attacks. The default list is loopback
    /// names only, so a deployment behind a real hostname must set this.
    pub allowed_hosts: Vec<String>,
    /// Stops the MCP service (open SSE streams) when cancelled.
    pub shutdown: CancellationToken,
}

/// Builds the application router: `/mcp` behind bearer auth.
pub fn router(config: HttpConfig) -> Router {
    let auth = BearerAuth::new(config.token);
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
    let mcp = StreamableHttpService::new(
        || Ok(PingServer::new()),
        Arc::new(LocalSessionManager::default()),
        mcp_config,
    );
    // The layer wraps only this sub-router, so routes added later (such as `/healthz`)
    // are not accidentally put behind, or left outside, the MCP auth.
    let protected = Router::new()
        .nest_service("/mcp", mcp)
        .layer(middleware::from_fn_with_state(auth, auth::require_bearer));
    Router::new().merge(protected)
}
