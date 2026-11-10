//! The running Discord connector (plan sections 5.2 and 7.2): the gateway loop, the live
//! ingest thread and the sync worker, with a status snapshot for `/api/v1/status`.
//!
//! # Tasks
//!
//! - **Gateway loop** (async, [`Connector::run`]): reads frames from a [`Gateway`]. Live
//!   dispatches for the configured guild go to the ingest thread over a bounded queue
//!   (backpressure instead of unbounded memory). `GUILD_CREATE` for the guild and
//!   `RESUMED` ask for a sync run, so discovery and catch-up run on every start and after
//!   every reconnect, whether the shard resumed or identified again.
//! - **Ingest thread**: applies each dispatch with [`live::ingest`] through its own
//!   [`HostStore`]. Events about unknown channels ask for a sync run.
//! - **Sync worker**: runs [`sync_guild`] whenever asked, coalescing requests. A run that
//!   left work (backfill or catch-up page limits) is followed by another after
//!   [`Timing::continue_after`], so backfill finishes in the background without starving
//!   live events. Retryable failures back off ([`Timing::min_backoff`] doubling to
//!   [`Timing::max_backoff`], or Discord's `retry_after`); a rejected token stops syncing
//!   until the next request.
//!
//! # Shutdown
//!
//! Cancelling the token closes the gateway (normal close frame, bounded wait), then the
//! queues are closed. The ingest thread applies everything already queued (the drain) and
//! the sync worker stops; with [`crate::http::TwilightRest`] a running sync fails fast at
//! its next request and keeps its saved cursors. [`Connector::run`] returns once both
//! threads have exited, so the caller can then shut the writer down.
//!
//! # What is logged
//!
//! Event type names from a fixed list, counts and fixed messages. Never frame text,
//! payload fields, chat content, channel names or the token.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;
use teamctx_core::id::{Snowflake, SourceId};
use teamctx_core::repo::{DirectoryRead, IngestWrite, MessageRead, RepoError, RepoResult};
use teamctx_core::source::SourceKind;
use teamctx_core::time::UnixMillis;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::gateway::{Gateway, GatewayEvent, fatal_close_reason};
use crate::live::{self, LIVE_EVENTS, LiveOutcome};
use crate::sync::{DiscordRest, RestError, SyncConfig, SyncError, SyncReport, sync_guild};

/// Source of "now" (tests pass a fixed clock).
pub type Clock = Arc<dyn Fn() -> UnixMillis + Send + Sync>;

/// Queued dispatches before the gateway loop waits for the ingest thread.
const QUEUE: usize = 1024;

/// How long a normal gateway close may take at shutdown.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the sync worker checks for cancellation while idle.
const POLL: Duration = Duration::from_millis(200);

/// The fallback guild name before Discord has sent one.
const UNNAMED_GUILD: &str = "Discord server";

/// Delays of the sync worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Pause before the next run when a run left catch-up or backfill work.
    pub continue_after: Duration,
    /// First wait after a retryable failure.
    pub min_backoff: Duration,
    /// Longest wait after repeated retryable failures.
    pub max_backoff: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            continue_after: Duration::from_secs(2),
            min_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_secs(600),
        }
    }
}

/// The gateway connection as the status page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayState {
    /// Not connected yet.
    #[default]
    Starting,
    /// Identified or resumed; events are flowing.
    Connected,
    /// The connection dropped and the shard is reconnecting.
    Reconnecting,
    /// Closed for good (shutdown or a fatal close code).
    Stopped,
}

impl GatewayState {
    /// Lowercase name for JSON.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Stopped => "stopped",
        }
    }
}

/// A point-in-time view of the connector. No chat text, names or secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusSnapshot {
    /// The gateway connection.
    pub gateway: GatewayState,
    /// The stored source row, once the guild is known.
    pub source: Option<SourceId>,
    /// When the last live event for the guild arrived.
    pub last_event_at: Option<UnixMillis>,
    /// Live events received for the guild.
    pub events: u64,
    /// Times the connection dropped.
    pub disconnects: u64,
    /// When the last sync run started.
    pub last_sync_started_at: Option<UnixMillis>,
    /// When the last sync run ended.
    pub last_sync_finished_at: Option<UnixMillis>,
    /// Whether the last sync run completed without an error.
    pub last_sync_ok: Option<bool>,
    /// Completed sync runs.
    pub sync_runs: u64,
    /// The current problem, as fixed text, if any.
    pub problem: Option<&'static str>,
}

/// Shared, cheap-to-clone handle to the connector's [`StatusSnapshot`].
#[derive(Debug, Clone, Default)]
pub struct ConnectorStatus(Arc<Mutex<StatusSnapshot>>);

impl ConnectorStatus {
    /// A copy of the current state.
    #[must_use]
    pub fn snapshot(&self) -> StatusSnapshot {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn update(&self, change: impl FnOnce(&mut StatusSnapshot)) {
        change(&mut self.0.lock().unwrap_or_else(PoisonError::into_inner));
    }
}

/// Why the connector stopped on its own.
#[derive(Debug, Error)]
pub enum ConnectorError {
    /// A store connection for a worker could not be opened.
    #[error("could not open the store for the connector")]
    Store(#[from] RepoError),
    /// A worker thread could not be started.
    #[error("could not start a connector thread")]
    Spawn(#[source] std::io::Error),
    /// The gateway closed for good with a fatal close code.
    #[error("the Discord gateway closed: {0}")]
    Fatal(&'static str),
    /// The gateway ended without a close code the connector understands.
    #[error("the Discord gateway connection ended unexpectedly")]
    Ended,
    /// A worker thread panicked.
    #[error("a connector thread stopped unexpectedly")]
    Worker,
}

/// Work for the ingest thread.
enum Work {
    /// The guild's name from `GUILD_CREATE` (untrusted text): creates the source row.
    Guild(String),
    /// One live dispatch; `kind` is from [`LIVE_EVENTS`].
    Dispatch { kind: &'static str, data: Value },
}

/// One Discord connector: a gateway, a REST client and a way to open store handles.
pub struct Connector<G, R, F> {
    gateway: G,
    rest: Arc<R>,
    open_store: F,
    config: SyncConfig,
    timing: Timing,
    clock: Clock,
    status: ConnectorStatus,
}

impl<G, R, F> std::fmt::Debug for Connector<G, R, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connector")
            .field("guild", &self.config.guild)
            .field("timing", &self.timing)
            .finish_non_exhaustive()
    }
}

impl<G, R, F, S> Connector<G, R, F>
where
    G: Gateway,
    R: DiscordRest + Send + Sync + 'static,
    F: Fn() -> RepoResult<S>,
    S: IngestWrite + DirectoryRead + MessageRead + Send + 'static,
{
    /// A connector for `config.guild`. `config.guild_name` is only the fallback name until
    /// Discord sends the real one. `open_store` is called once per worker thread.
    pub fn new(gateway: G, rest: Arc<R>, open_store: F, config: SyncConfig, clock: Clock) -> Self {
        Self {
            gateway,
            rest,
            open_store,
            config,
            timing: Timing::default(),
            clock,
            status: ConnectorStatus::default(),
        }
    }

    /// Replaces the sync worker's delays (tests use short ones).
    #[must_use]
    pub fn with_timing(mut self, timing: Timing) -> Self {
        self.timing = timing;
        self
    }

    /// The status handle; clone it before [`Connector::run`].
    #[must_use]
    pub fn status(&self) -> ConnectorStatus {
        self.status.clone()
    }

    /// Runs until `cancel` fires or the gateway closes for good, then drains and joins
    /// the workers (see the module documentation).
    ///
    /// # Errors
    ///
    /// [`ConnectorError`] if a worker cannot start, or the gateway closed for good
    /// without being asked to.
    pub async fn run(self, cancel: CancellationToken) -> Result<(), ConnectorError> {
        let Self {
            mut gateway,
            rest,
            open_store,
            config,
            timing,
            clock,
            status,
        } = self;
        let guild_name: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

        let (work_tx, work_rx) = tokio::sync::mpsc::channel::<Work>(QUEUE);
        let (sync_tx, sync_rx) = mpsc::sync_channel::<()>(1);

        let ingest = {
            let store = open_store()?;
            let (rest, config, clock, status) = (
                Arc::clone(&rest),
                config.clone(),
                Arc::clone(&clock),
                status.clone(),
            );
            let sync_tx = sync_tx.clone();
            spawn("teamctx-ingest", move || {
                ingest_worker(store, &*rest, &config, work_rx, &sync_tx, &clock, &status);
            })?
        };
        let syncer = {
            let store = open_store()?;
            let worker = SyncWorker {
                rest: Arc::clone(&rest),
                config: config.clone(),
                guild_name: Arc::clone(&guild_name),
                timing,
                clock: Arc::clone(&clock),
                status: status.clone(),
                cancel: cancel.clone(),
            };
            spawn("teamctx-sync", move || worker.run(store, &sync_rx))?
        };

        let mut fatal: Option<&'static str> = None;
        let mut ended = false;
        loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                event = gateway.next_event() => event,
            };
            let Some(event) = event else {
                ended = true;
                break;
            };
            match event {
                GatewayEvent::Text(text) => {
                    let keep_going = on_frame(
                        &text,
                        &config,
                        &guild_name,
                        &work_tx,
                        &sync_tx,
                        &clock,
                        &status,
                    )
                    .await;
                    if !keep_going {
                        tracing::error!("the live ingest thread stopped; closing the gateway");
                        break;
                    }
                }
                GatewayEvent::Closed(code) => {
                    status.update(|s| {
                        s.gateway = GatewayState::Reconnecting;
                        s.disconnects += 1;
                    });
                    if let Some(reason) = code.and_then(fatal_close_reason) {
                        tracing::error!(reason, "Discord gateway closed for good");
                        status.update(|s| s.problem = Some(reason));
                        fatal = Some(reason);
                    } else {
                        tracing::info!(code, "Discord gateway disconnected; reconnecting");
                    }
                }
                GatewayEvent::ReceiveFailed => {
                    tracing::warn!("a Discord gateway frame could not be received");
                }
            }
        }

        if !ended {
            gateway.close();
            let drained = tokio::time::timeout(CLOSE_TIMEOUT, async {
                while let Some(event) = gateway.next_event().await {
                    if matches!(event, GatewayEvent::Closed(_)) {
                        break;
                    }
                }
            })
            .await;
            if drained.is_err() {
                tracing::warn!("the Discord gateway did not confirm the close in time");
            }
        }
        drop(work_tx);
        drop(sync_tx);
        let joined = tokio::task::spawn_blocking(move || {
            let ingest = ingest.join().is_ok();
            let syncer = syncer.join().is_ok();
            ingest && syncer
        })
        .await
        .unwrap_or(false);
        status.update(|s| s.gateway = GatewayState::Stopped);
        if !joined {
            return Err(ConnectorError::Worker);
        }
        if cancel.is_cancelled() {
            return Ok(());
        }
        Err(fatal.map_or(ConnectorError::Ended, ConnectorError::Fatal))
    }
}

fn spawn(
    name: &str,
    work: impl FnOnce() + Send + 'static,
) -> Result<JoinHandle<()>, ConnectorError> {
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(work)
        .map_err(ConnectorError::Spawn)
}

/// Handles one text frame. Returns `false` when the ingest thread is gone.
async fn on_frame(
    text: &str,
    config: &SyncConfig,
    guild_name: &Mutex<Option<String>>,
    work: &tokio::sync::mpsc::Sender<Work>,
    sync: &SyncSender<()>,
    clock: &Clock,
    status: &ConnectorStatus,
) -> bool {
    let Ok(mut frame) = serde_json::from_str::<Value>(text) else {
        tracing::warn!("a Discord gateway frame is not JSON");
        return true;
    };
    // Only dispatches (op 0) carry events; hello, heartbeats and the rest are the shard's.
    if frame.get("op").and_then(Value::as_u64) != Some(0) {
        return true;
    }
    let Some(kind) = frame.get("t").and_then(Value::as_str) else {
        return true;
    };
    match kind {
        "READY" => status.update(|s| s.gateway = GatewayState::Connected),
        "RESUMED" => {
            status.update(|s| s.gateway = GatewayState::Connected);
            request_sync(sync);
        }
        "GUILD_CREATE" => {
            let data = frame.get("d").unwrap_or(&Value::Null);
            let ours = data
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| id.parse::<Snowflake>().ok())
                == Some(config.guild);
            let unavailable = data
                .get("unavailable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !ours || unavailable {
                return true;
            }
            let name = data
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .unwrap_or(UNNAMED_GUILD)
                .to_owned();
            *guild_name.lock().unwrap_or_else(PoisonError::into_inner) = Some(name.clone());
            status.update(|s| s.gateway = GatewayState::Connected);
            if work.send(Work::Guild(name)).await.is_err() {
                return false;
            }
            request_sync(sync);
        }
        other => {
            let Some(kind) = LIVE_EVENTS.iter().copied().find(|known| *known == other) else {
                return true;
            };
            let data = frame.get_mut("d").map(Value::take).unwrap_or(Value::Null);
            let now = clock();
            status.update(|s| {
                s.last_event_at = Some(now);
                s.events += 1;
            });
            if work.send(Work::Dispatch { kind, data }).await.is_err() {
                return false;
            }
        }
    }
    true
}

/// Asks the sync worker for a run; a run already queued covers this request too.
fn request_sync(sync: &SyncSender<()>) {
    match sync.try_send(()) {
        Ok(()) | Err(TrySendError::Full(())) => {}
        Err(TrySendError::Disconnected(())) => {
            tracing::warn!("the sync worker has stopped; sync request dropped");
        }
    }
}

/// The ingest thread: applies queued dispatches until the queue closes and is empty.
fn ingest_worker<S, R>(
    mut store: S,
    rest: &R,
    config: &SyncConfig,
    mut work: tokio::sync::mpsc::Receiver<Work>,
    sync: &SyncSender<()>,
    clock: &Clock,
    status: &ConnectorStatus,
) where
    S: IngestWrite + DirectoryRead + MessageRead,
    R: DiscordRest + ?Sized,
{
    let mut source: Option<SourceId> = None;
    while let Some(item) = work.blocking_recv() {
        match item {
            Work::Guild(name) => {
                match store.upsert_source(SourceKind::Discord, config.guild, &name) {
                    Ok(id) => {
                        source = Some(id);
                        status.update(|s| s.source = Some(id));
                    }
                    Err(_) => tracing::error!("could not store the Discord server"),
                }
            }
            Work::Dispatch { kind, data } => {
                // Before GUILD_CREATE there is no source row; the sync run that follows it
                // catches up on anything dropped here.
                let Some(source) = source else { continue };
                match live::ingest(&mut store, rest, config, source, kind, &data, clock()) {
                    Ok(LiveOutcome::NeedsSync) => request_sync(sync),
                    Ok(LiveOutcome::Applied | LiveOutcome::Ignored) => {}
                    Err(error) => {
                        tracing::warn!(event = kind, %error, "a live Discord event was not applied");
                    }
                }
            }
        }
    }
}

/// The sync worker's settings and shared state.
struct SyncWorker<R> {
    rest: Arc<R>,
    config: SyncConfig,
    guild_name: Arc<Mutex<Option<String>>>,
    timing: Timing,
    clock: Clock,
    status: ConnectorStatus,
    cancel: CancellationToken,
}

impl<R: DiscordRest> SyncWorker<R> {
    fn run<S>(self, mut store: S, requests: &Receiver<()>)
    where
        S: IngestWrite + DirectoryRead + MessageRead,
    {
        let mut due: Option<Instant> = None;
        let mut backoff = self.timing.min_backoff;
        loop {
            if self.cancel.is_cancelled() {
                return;
            }
            let wait = due.map_or(POLL, |at| {
                at.saturating_duration_since(Instant::now()).min(POLL)
            });
            match requests.recv_timeout(wait) {
                Ok(()) => {
                    while requests.try_recv().is_ok() {}
                    due = Some(Instant::now());
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if self.cancel.is_cancelled() {
                return;
            }
            match due {
                Some(at) if Instant::now() >= at => due = None,
                _ => continue,
            }
            let name = self
                .guild_name
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            // Without a GUILD_CREATE the guild is not known to be visible yet; the request
            // that comes with it starts the run.
            let Some(name) = name else { continue };
            let mut config = self.config.clone();
            config.guild_name = name;
            let started = (self.clock)();
            self.status
                .update(|s| s.last_sync_started_at = Some(started));
            let result = sync_guild(&*self.rest, &mut store, &config, started);
            let finished = (self.clock)();
            match result {
                Ok(report) => {
                    backoff = self.timing.min_backoff;
                    let more = work_left(&report);
                    tracing::info!(
                        channels = report.channels.len(),
                        upserted = report.upserted(),
                        deleted = report.deleted(),
                        more,
                        "Discord sync run finished"
                    );
                    self.status.update(|s| {
                        s.source = Some(report.source);
                        s.last_sync_finished_at = Some(finished);
                        s.last_sync_ok = Some(true);
                        s.sync_runs += 1;
                        s.problem = None;
                    });
                    if more {
                        due = Some(Instant::now() + self.timing.continue_after);
                    }
                }
                Err(error) => {
                    let problem = sync_problem(&error);
                    tracing::warn!(%error, problem, "Discord sync run stopped");
                    self.status.update(|s| {
                        s.last_sync_finished_at = Some(finished);
                        s.last_sync_ok = Some(false);
                        s.sync_runs += 1;
                        s.problem = Some(problem);
                    });
                    if error.is_retryable() {
                        let wait = error.retry_after().unwrap_or(backoff).max(backoff);
                        due = Some(Instant::now() + wait);
                        backoff = (backoff * 2).min(self.timing.max_backoff);
                    }
                }
            }
        }
    }
}

/// Whether a run stopped at a page limit somewhere, so another run should follow.
fn work_left(report: &SyncReport) -> bool {
    report.channels.iter().any(|channel| {
        channel.outcome == crate::sync::ChannelOutcome::Synced
            && (!channel.catchup_complete || !channel.backfill_complete)
    })
}

/// Fixed text for the status page.
fn sync_problem(error: &SyncError) -> &'static str {
    match error {
        SyncError::Rest {
            source: RestError::Unauthorized,
            ..
        } => "Discord rejected the bot token; sync stopped",
        SyncError::Rest {
            source: RestError::Forbidden | RestError::NotFound,
            channel: None,
        } => "the bot cannot see the configured Discord server",
        SyncError::Rest { .. } if error.is_retryable() => {
            "Discord sync is waiting to retry after a temporary failure"
        }
        SyncError::Rest { .. } => "Discord sync stopped after a REST failure",
        SyncError::Store(_) => "Discord sync stopped: the store failed",
        SyncError::Map(_) => "Discord sync stopped: Discord sent a malformed channel list",
        SyncError::Inconsistent(_) => "Discord sync stopped: internal inconsistency",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_problems_are_fixed_text() {
        let unauthorized = SyncError::Rest {
            channel: None,
            source: RestError::Unauthorized,
        };
        assert!(sync_problem(&unauthorized).contains("token"));
        let limited = SyncError::Rest {
            channel: None,
            source: RestError::RateLimited {
                retry_after: Duration::from_secs(1),
                global: false,
            },
        };
        assert!(sync_problem(&limited).contains("retry"));
    }

    #[test]
    fn gateway_states_have_stable_names() {
        assert_eq!(GatewayState::default().as_str(), "starting");
        assert_eq!(GatewayState::Connected.as_str(), "connected");
        assert_eq!(GatewayState::Reconnecting.as_str(), "reconnecting");
        assert_eq!(GatewayState::Stopped.as_str(), "stopped");
    }
}
