//! The interaction transport (plan section 7.3): registration on `READY`, one answer per
//! `INTERACTION_CREATE`, deferral for slow commands.
//!
//! Discord invalidates an interaction that is not answered within 3 seconds. So:
//!
//! - `/catchup`, `/search` and `/ask` always defer first (callback type 5) and then edit
//!   the original response with the result; `/ask` and `/catchup summarize:true` call the
//!   host's [`AskBackend`] in between;
//! - every other command answers directly (type 4) if its work finishes within
//!   [`ServiceTiming::defer_after`], and otherwise defers and edits;
//! - "Mark as task" must answer with its modal (type 9), which cannot follow a deferral,
//!   so if storing the message takes too long it answers with a short "try again" note.
//!
//! The only calls this module makes to Discord are those of [`InteractionApi`]: register
//! the commands, answer an interaction, edit that answer, and two read-only `GET`s for
//! channel access. Follow-up messages and deletes are not part of it (ADR 0005,
//! `tests/readonly.rs`).
//!
//! Logged: command names from a fixed list, counts and fixed text. Never the token, option
//! values or chat text.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;
use teamctx_core::id::Snowflake;
use teamctx_core::record::EntryKind;
use teamctx_core::when::FixedOffset;
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use twilight_model::application::command::Command;
use twilight_model::http::interaction::InteractionResponse;

use super::access::AccessSnapshot;
use super::ask::{self, AskBackend, LlmJob, LlmOutcome, LlmTask, Refusal};
use super::commands::commands;
use super::deferred;
use super::handler::{self, Context, InteractionStore, Outcome, Reply};
use super::payload::{self, Envelope, InteractionToken, Request};
use crate::connector::{Clock, ConnectorStatus};
use crate::sync::RestError;

/// Interactions handled at the same time; more are answered with a "busy" note.
const CONCURRENT: usize = 16;
/// How long to wait for in-flight interactions at shutdown.
// The host's model request limit is 60 seconds, plus time to settle usage and reply.
const DRAIN: Duration = Duration::from_secs(65);

const UNREADABLE: &str = "I could not read that command. Please try again.";
const BUSY: &str = "teamctx is busy right now; please try again in a moment.";
const FAILED: &str =
    "Something went wrong on the teamctx host; nothing was changed. The host's log has details.";

/// Events from the gateway loop.
#[derive(Debug, Clone, PartialEq)]
pub enum InteractionEvent {
    /// `READY`, with the application id commands are registered for.
    Ready {
        /// `d.application.id`.
        application: Snowflake,
    },
    /// The `d` of one `INTERACTION_CREATE` (untrusted).
    Interaction(Value),
}

/// The Discord calls the service needs. `crate::http::TwilightRest` implements it with
/// `twilight-http`; tests use a recording fake.
pub trait InteractionApi: Send + Sync + 'static {
    /// Overwrites the guild's commands with `commands` (`PUT .../guilds/{guild}/commands`).
    fn install_commands(
        &self,
        application: Snowflake,
        guild: Snowflake,
        commands: &[Command],
    ) -> impl Future<Output = Result<(), RestError>> + Send;

    /// Answers an interaction (`POST /interactions/{id}/{token}/callback`).
    fn respond(
        &self,
        application: Snowflake,
        interaction: Snowflake,
        token: &InteractionToken,
        response: &InteractionResponse,
    ) -> impl Future<Output = Result<(), RestError>> + Send;

    /// Replaces a deferred answer with `reply`
    /// (`PATCH /webhooks/{application}/{token}/messages/@original`).
    fn fill_deferred(
        &self,
        application: Snowflake,
        token: &InteractionToken,
        reply: &Reply,
    ) -> impl Future<Output = Result<(), RestError>> + Send;

    /// The guild (with roles) and its channels (with overwrites), both read-only `GET`s.
    fn access_payloads(
        &self,
        guild: Snowflake,
    ) -> impl Future<Output = Result<(Value, Value), RestError>> + Send;
}

/// What the service is configured with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteractionConfig {
    /// The configured guild; interactions from anywhere else get a fixed note.
    pub guild: Snowflake,
    /// `server.timezone` as seconds east of UTC (fixed offsets only, plan section 12).
    pub zone: FixedOffset,
}

/// Delays of the service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceTiming {
    /// Answer directly if the work is done by then, else defer.
    pub defer_after: Duration,
    /// How long a channel-access snapshot is reused.
    pub access_ttl: Duration,
    /// How long a failed access fetch is remembered.
    pub access_retry: Duration,
    /// The longest an access fetch may take.
    pub access_timeout: Duration,
}

impl Default for ServiceTiming {
    fn default() -> Self {
        Self {
            defer_after: Duration::from_millis(1800),
            access_ttl: Duration::from_secs(60),
            access_retry: Duration::from_secs(10),
            access_timeout: Duration::from_millis(1200),
        }
    }
}

type AccessCache = tokio::sync::Mutex<Option<(Instant, Option<Arc<AccessSnapshot>>)>>;

struct Shared<A, S> {
    api: Arc<A>,
    ask: Option<Arc<dyn AskBackend>>,
    store: Arc<Mutex<S>>,
    config: InteractionConfig,
    status: ConnectorStatus,
    clock: Clock,
    timing: ServiceTiming,
    access: AccessCache,
}

/// Answers interactions until cancelled.
pub struct InteractionService<A, S> {
    api: Arc<A>,
    ask: Option<Arc<dyn AskBackend>>,
    store: S,
    config: InteractionConfig,
    status: ConnectorStatus,
    clock: Clock,
    timing: ServiceTiming,
}

impl<A, S> std::fmt::Debug for InteractionService<A, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InteractionService")
            .field("guild", &self.config.guild)
            .field("llm", &self.ask.is_some())
            .field("timing", &self.timing)
            .finish_non_exhaustive()
    }
}

impl<A, S> InteractionService<A, S>
where
    A: InteractionApi,
    S: InteractionStore + Send + 'static,
{
    /// A service over `api` and `store` (writes through the single writer, reads through
    /// a read-only connection: [`crate::store::HostStore`]). `status` is the connector's,
    /// for the source row and `/teamctx status`.
    pub fn new(
        api: Arc<A>,
        store: S,
        config: InteractionConfig,
        status: ConnectorStatus,
        clock: Clock,
    ) -> Self {
        Self {
            api,
            ask: None,
            store,
            config,
            status,
            clock,
            timing: ServiceTiming::default(),
        }
    }

    /// Enables `/ask` and `/catchup summarize:true` through `backend` (the host's `/ask`
    /// pipeline). Without it neither is registered.
    #[must_use]
    pub fn with_ask(mut self, backend: Arc<dyn AskBackend>) -> Self {
        self.ask = Some(backend);
        self
    }

    /// Replaces the delays (tests use short ones).
    #[must_use]
    pub fn with_timing(mut self, timing: ServiceTiming) -> Self {
        self.timing = timing;
        self
    }

    /// Handles events until `cancel` fires or the sender is dropped, then waits briefly
    /// for answers in flight.
    pub async fn run(
        self,
        mut events: mpsc::Receiver<InteractionEvent>,
        cancel: CancellationToken,
    ) {
        let shared = Arc::new(Shared {
            api: self.api,
            ask: self.ask,
            store: Arc::new(Mutex::new(self.store)),
            config: self.config,
            status: self.status,
            clock: self.clock,
            timing: self.timing,
            access: tokio::sync::Mutex::new(None),
        });
        let registered = Arc::new(AtomicBool::new(false));
        let permits = Arc::new(Semaphore::new(CONCURRENT));
        let mut tasks = JoinSet::new();
        loop {
            let event = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                event = events.recv() => event,
            };
            let Some(event) = event else { break };
            while tasks.try_join_next().is_some() {}
            match event {
                InteractionEvent::Ready { application } => {
                    if !registered.load(Ordering::Acquire) {
                        let shared = Arc::clone(&shared);
                        let registered = Arc::clone(&registered);
                        tasks.spawn(async move {
                            if register(&shared, application).await {
                                registered.store(true, Ordering::Release);
                            }
                        });
                    }
                }
                InteractionEvent::Interaction(data) => {
                    let shared = Arc::clone(&shared);
                    match Arc::clone(&permits).try_acquire_owned() {
                        Ok(permit) => {
                            tasks.spawn(async move {
                                answer(&shared, data).await;
                                drop(permit);
                            });
                        }
                        Err(_) => {
                            tasks.spawn(async move { busy(&shared, &data).await });
                        }
                    }
                }
            }
        }
        if tokio::time::timeout(DRAIN, async { while tasks.join_next().await.is_some() {} })
            .await
            .is_err()
        {
            tracing::warn!("interactions still in flight at shutdown were abandoned");
            tasks.abort_all();
        }
    }
}

/// Registers the guild commands. Returns whether it worked.
async fn register<A: InteractionApi, S>(shared: &Shared<A, S>, application: Snowflake) -> bool {
    let commands = commands(shared.ask.is_some());
    match shared
        .api
        .install_commands(application, shared.config.guild, &commands)
        .await
    {
        Ok(()) => {
            tracing::info!(commands = commands.len(), "Discord commands registered");
            true
        }
        Err(error) => {
            tracing::warn!(%error, "Discord commands could not be registered; is the applications.commands scope granted?");
            false
        }
    }
}

/// Answers an interaction that arrived while every slot was busy.
async fn busy<A: InteractionApi, S>(shared: &Shared<A, S>, data: &Value) {
    if let Ok(envelope) = payload::envelope(data) {
        send(
            shared,
            &envelope,
            &Outcome::Reply(Reply::note(BUSY)).response(),
        )
        .await;
    }
}

async fn send<A: InteractionApi, S>(
    shared: &Shared<A, S>,
    envelope: &Envelope,
    response: &InteractionResponse,
) -> bool {
    match shared
        .api
        .respond(envelope.application, envelope.id, &envelope.token, response)
        .await
    {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "an interaction could not be answered");
            false
        }
    }
}

async fn fill<A: InteractionApi, S>(shared: &Shared<A, S>, envelope: &Envelope, outcome: Outcome) {
    let reply = match outcome {
        Outcome::Reply(reply) => reply,
        // A modal cannot follow a deferral; `answer` never defers a request that can open one.
        Outcome::Modal(_) => Reply::note(FAILED),
        Outcome::Llm(task) => run_llm(shared, *task).await,
    };
    if let Err(error) = shared
        .api
        .fill_deferred(envelope.application, &envelope.token, &reply)
        .await
    {
        tracing::warn!(%error, "a deferred interaction could not be completed");
    }
}

/// Runs a language-model task through the backend and renders the reply. Logs only the
/// kind of task and a fixed outcome name: never the question, the answer or the sources.
async fn run_llm<A, S>(shared: &Shared<A, S>, task: LlmTask) -> Reply {
    let kind = match task.request.job {
        LlmJob::Ask { .. } => "ask",
        LlmJob::Summarize { .. } => "summarize",
    };
    let outcome = match &shared.ask {
        Some(backend) => backend.ask(task.request.clone()).await,
        // The handler only builds a task when a backend is configured.
        None => LlmOutcome::Refused(Refusal::Host),
    };
    let result = match &outcome {
        LlmOutcome::Answer { .. } => "answered",
        LlmOutcome::NotFound { .. } => "not-found",
        LlmOutcome::Refused(refusal) => refusal.name(),
    };
    tracing::info!(task = kind, result, "language model task finished");
    ask::render(task, &outcome)
}

/// Handles one `INTERACTION_CREATE`.
async fn answer<A, S>(shared: &Arc<Shared<A, S>>, data: Value)
where
    A: InteractionApi,
    S: InteractionStore + Send + 'static,
{
    let started = Instant::now();
    let Ok(envelope) = payload::envelope(&data) else {
        tracing::warn!("an interaction without a usable id or token was dropped");
        return;
    };
    let interaction = match payload::parse(&data) {
        Ok(interaction) => interaction,
        Err(error) => {
            tracing::warn!(%error, "an interaction could not be read");
            send(
                shared,
                &envelope,
                &Outcome::Reply(Reply::note(UNREADABLE)).response(),
            )
            .await;
            return;
        }
    };
    drop(data);
    let name = interaction.request.name();
    let public = interaction.public;
    let defer_first = matches!(
        interaction.request,
        Request::Catchup { .. } | Request::Search { .. } | Request::Ask { .. }
    );
    let opens_modal = matches!(
        interaction.request,
        Request::Mark {
            kind: EntryKind::Task,
            ..
        }
    );
    let needs_access = !matches!(
        interaction.request,
        Request::Mark { .. } | Request::Status | Request::Unsupported
    );

    let work = {
        let shared = Arc::clone(shared);
        async move {
            let access = if needs_access {
                access(&shared).await
            } else {
                None
            };
            let store = Arc::clone(&shared.store);
            let config = shared.config;
            let llm = shared.ask.is_some();
            let status = shared.status.snapshot();
            let now = (shared.clock)();
            tokio::task::spawn_blocking(move || {
                let ctx = Context {
                    guild: config.guild,
                    source: status.source,
                    now,
                    zone: config.zone,
                    access: access.as_deref(),
                    status: &status,
                    llm,
                };
                let mut store = store.lock().unwrap_or_else(PoisonError::into_inner);
                handler::handle(&mut *store, &ctx, &interaction)
            })
            .await
            .unwrap_or_else(|_| {
                tracing::error!("an interaction handler panicked");
                Outcome::Reply(Reply::note(FAILED))
            })
        }
    };
    tokio::pin!(work);

    if defer_first {
        if send(shared, &envelope, &deferred(public)).await {
            let outcome = work.await;
            fill(shared, &envelope, outcome).await;
        }
    } else {
        let deadline = shared.timing.defer_after.saturating_sub(started.elapsed());
        tokio::select! {
            outcome = &mut work => {
                send(shared, &envelope, &outcome.response()).await;
            }
            () = tokio::time::sleep(deadline) => {
                if opens_modal {
                    send(shared, &envelope, &Outcome::Reply(Reply::note(BUSY)).response()).await;
                    // Let the message be stored anyway, so a retry is quick.
                    let _ = work.await;
                } else if send(shared, &envelope, &deferred(public)).await {
                    let outcome = work.await;
                    fill(shared, &envelope, outcome).await;
                }
            }
        }
    }
    tracing::debug!(
        command = name,
        millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "interaction answered"
    );
}

/// The current access snapshot, fetched at most once per TTL. `None` when it cannot be
/// fetched: replies then show nothing chat-derived.
async fn access<A: InteractionApi, S>(shared: &Shared<A, S>) -> Option<Arc<AccessSnapshot>> {
    let mut cache = shared.access.lock().await;
    if let Some((at, snapshot)) = cache.as_ref() {
        let ttl = if snapshot.is_some() {
            shared.timing.access_ttl
        } else {
            shared.timing.access_retry
        };
        if at.elapsed() < ttl {
            return snapshot.clone();
        }
    }
    let guild = shared.config.guild;
    let fetched = tokio::time::timeout(
        shared.timing.access_timeout,
        shared.api.access_payloads(guild),
    )
    .await;
    let snapshot = match fetched {
        Ok(Ok((guild_json, channels_json))) => {
            match AccessSnapshot::from_payloads(guild, &guild_json, &channels_json) {
                Ok(snapshot) => Some(Arc::new(snapshot)),
                Err(error) => {
                    tracing::warn!(%error, "channel access could not be read");
                    None
                }
            }
        }
        Ok(Err(error)) => {
            tracing::warn!(%error, "channel access could not be fetched");
            None
        }
        Err(_) => {
            tracing::warn!("channel access took too long to fetch");
            None
        }
    };
    *cache = Some((Instant::now(), snapshot.clone()));
    snapshot
}
