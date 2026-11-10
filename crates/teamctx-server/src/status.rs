//! What `GET /api/v1/status` reports about sync (plan section 10.2, status screen): the
//! connector's state from the host, and each stored channel's sync cursor from the store.
//!
//! The server does not depend on the Discord crate, so the host passes a
//! [`StatusProvider`] that returns a [`HostStatus`]. Everything in it is ids, times,
//! counts and fixed text. Channel names, topics and chat text are not part of the status,
//! so the route never needs untrusted-content framing.

use std::sync::Arc;

use serde_json::{Value, json};
use teamctx_core::id::SourceId;
use teamctx_core::repo::{DirectoryRead, RepoResult};
use teamctx_core::time::UnixMillis;

/// Returns the current connector state. Called on each status request.
pub type StatusProvider = Arc<dyn Fn() -> HostStatus + Send + Sync>;

/// The connector state as the host reports it. Strings are fixed text chosen by the host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostStatus {
    /// The stored source row being synced, once known.
    pub source: Option<SourceId>,
    /// The gateway state, for example `"connected"`.
    pub gateway: &'static str,
    /// When the last live event arrived.
    pub last_event_at: Option<UnixMillis>,
    /// Live events received.
    pub events: u64,
    /// Times the gateway connection dropped.
    pub disconnects: u64,
    /// When the last sync run started.
    pub last_sync_started_at: Option<UnixMillis>,
    /// When the last sync run ended.
    pub last_sync_finished_at: Option<UnixMillis>,
    /// Whether the last sync run completed without error.
    pub last_sync_ok: Option<bool>,
    /// Completed sync runs.
    pub sync_runs: u64,
    /// The current problem, if any (fixed text).
    pub problem: Option<&'static str>,
}

fn millis(at: Option<UnixMillis>) -> Value {
    at.map_or(Value::Null, |at| json!(at.as_millis()))
}

/// The `sync` object of the status response: the host's state plus one entry per stored
/// channel or thread of the source (ids, kind, flags and cursor; no names).
pub(crate) fn sync_json(host: &HostStatus, store: &dyn DirectoryRead) -> RepoResult<Value> {
    let mut channels = Vec::new();
    if let Some(source) = host.source {
        for channel in store.channels(source)? {
            let cursor = store.cursor(source, channel.external_id)?;
            channels.push(json!({
                "id": channel.external_id.to_string(),
                "kind": channel.kind.as_str(),
                "included": channel.included,
                "archived": channel.archived,
                "newest_seen": cursor.and_then(|c| c.newest_seen).map(|id| id.to_string()),
                "backfill_done": cursor.is_some_and(|c| c.backfill_done),
                "cursor_updated_at": millis(cursor.map(|c| c.updated_at)),
            }));
        }
    }
    Ok(json!({
        "gateway": host.gateway,
        "last_event_at": millis(host.last_event_at),
        "events": host.events,
        "disconnects": host.disconnects,
        "last_sync_started_at": millis(host.last_sync_started_at),
        "last_sync_finished_at": millis(host.last_sync_finished_at),
        "last_sync_ok": host.last_sync_ok,
        "sync_runs": host.sync_runs,
        "problem": host.problem,
        "channels": channels,
    }))
}
