//! Interactions end to end, in process: a fake gateway delivers `READY` and
//! `INTERACTION_CREATE` frames from `fixtures/discord/interactions/`, the connector hands
//! them to the interaction service, the service answers through a recording fake of the
//! interaction API, and the record lands in a real SQLite file through the single writer.
//! No network. Every id, name, token and message is fictional.
//!
//! One `#[test]` installs a global log capture, so it is the only test in this file that
//! checks logs; the others do not depend on it.

#![expect(clippy::expect_used, reason = "test helpers fail loudly")]

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use teamctx_core::id::Snowflake;
use teamctx_core::record::{CreatedVia, EntryKind};
use teamctx_core::repo::{EntryFilter, PageRequest, RecordRead};
use teamctx_core::time::UnixMillis;
use teamctx_core::when::FixedOffset;
use teamctx_discord::connector::{Connector, Timing};
use teamctx_discord::gateway::{Gateway, GatewayEvent};
use teamctx_discord::interactions::payload::InteractionToken;
use teamctx_discord::interactions::{
    AskBackend, AskFuture, Cited, InteractionApi, InteractionConfig, InteractionEvent,
    InteractionService, LlmJob, LlmOutcome, LlmRequest, Refusal, Reply, ServiceTiming,
};
use teamctx_discord::store::HostStore;
use teamctx_discord::sync::{
    DiscordRest, MessagesRequest, RestError, SyncConfig, ThreadVisibility,
};
use teamctx_store::{Store, Writer};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio_util::sync::CancellationToken;
use twilight_model::application::command::Command;
use twilight_model::http::interaction::InteractionResponse;

const GUILD: u64 = 1_100_000_000_000_000_001;
const GENERAL: u64 = 1_100_000_000_000_000_101;
const APP: u64 = 1_100_000_000_000_000_900;
const NOW_MS: i64 = 1_800_000_000_000;

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).expect("non-zero id")
}

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/discord/interactions")
        .join(format!("{name}.json"));
    std::fs::read_to_string(path).expect("fixture")
}

fn frame(name: &str) -> GatewayEvent {
    GatewayEvent::Text(fixture(name))
}

fn dispatch(kind: &str, data: &Value) -> GatewayEvent {
    GatewayEvent::Text(json!({ "op": 0, "s": 1, "t": kind, "d": data }).to_string())
}

// ---- fakes ----

/// Read-only REST with one empty text channel.
struct FakeRest;

impl DiscordRest for FakeRest {
    fn guild_channels(&self, _guild: Snowflake) -> Result<Value, RestError> {
        Ok(json!([{
            "id": GENERAL.to_string(), "guild_id": GUILD.to_string(), "type": 0,
            "name": "general", "position": 0, "permission_overwrites": []
        }]))
    }
    fn active_threads(&self, _guild: Snowflake) -> Result<Value, RestError> {
        Ok(json!({ "threads": [], "members": [] }))
    }
    fn archived_threads(
        &self,
        _channel: Snowflake,
        _visibility: ThreadVisibility,
        _before: Option<&str>,
    ) -> Result<Value, RestError> {
        Ok(json!({ "threads": [], "members": [], "has_more": false }))
    }
    fn channel_messages(&self, _request: &MessagesRequest) -> Result<Value, RestError> {
        Ok(json!([]))
    }
}

#[derive(Debug, Clone)]
enum Call {
    Install {
        application: u64,
        guild: u64,
        names: Vec<String>,
    },
    Respond {
        interaction: u64,
        token: String,
        response: Value,
    },
    Fill {
        token: String,
        data: Value,
    },
    Access,
}

/// Records every call. `access_delay` slows the access fetch to force a deferral.
#[derive(Default)]
struct FakeApi {
    calls: Mutex<Vec<Call>>,
    access_delay: Duration,
}

impl FakeApi {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().expect("lock").clone()
    }

    fn push(&self, call: Call) {
        self.calls.lock().expect("lock").push(call);
    }
}

impl InteractionApi for FakeApi {
    async fn install_commands(
        &self,
        application: Snowflake,
        guild: Snowflake,
        commands: &[Command],
    ) -> Result<(), RestError> {
        self.push(Call::Install {
            application: application.get(),
            guild: guild.get(),
            names: commands.iter().map(|c| c.name.clone()).collect(),
        });
        Ok(())
    }

    async fn respond(
        &self,
        _application: Snowflake,
        interaction: Snowflake,
        token: &InteractionToken,
        response: &InteractionResponse,
    ) -> Result<(), RestError> {
        self.push(Call::Respond {
            interaction: interaction.get(),
            token: token.expose().to_owned(),
            response: serde_json::to_value(response).expect("serialise"),
        });
        Ok(())
    }

    async fn fill_deferred(
        &self,
        _application: Snowflake,
        token: &InteractionToken,
        reply: &Reply,
    ) -> Result<(), RestError> {
        self.push(Call::Fill {
            token: token.expose().to_owned(),
            data: serde_json::to_value(reply.data()).expect("serialise"),
        });
        Ok(())
    }

    async fn access_payloads(&self, guild: Snowflake) -> Result<(Value, Value), RestError> {
        self.push(Call::Access);
        tokio::time::sleep(self.access_delay).await;
        let guild_json = json!({
            "id": guild.to_string(),
            "owner_id": "1100000000000000999",
            "roles": [{ "id": guild.to_string(), "permissions": "66560" }]
        });
        let channels =
            json!([{ "id": GENERAL.to_string(), "type": 0, "permission_overwrites": [] }]);
        Ok((guild_json, channels))
    }
}

/// A stand-in for the host's `/ask` pipeline: records requests, answers as scripted.
struct FakeAsk {
    requests: Mutex<Vec<LlmRequest>>,
    outcome: LlmOutcome,
}

impl FakeAsk {
    fn new(outcome: LlmOutcome) -> Arc<Self> {
        Arc::new(Self {
            requests: Mutex::new(Vec::new()),
            outcome,
        })
    }

    fn requests(&self) -> Vec<LlmRequest> {
        self.requests.lock().expect("lock").clone()
    }
}

impl AskBackend for FakeAsk {
    fn ask(&self, request: LlmRequest) -> AskFuture<'_> {
        self.requests.lock().expect("lock").push(request);
        let outcome = self.outcome.clone();
        Box::pin(async move { outcome })
    }
}

struct FakeGateway(UnboundedReceiver<GatewayEvent>);

impl Gateway for FakeGateway {
    async fn next_event(&mut self) -> Option<GatewayEvent> {
        self.0.recv().await
    }
    fn close(&mut self) {}
}

// ---- harness ----

struct Running {
    _dir: tempfile::TempDir,
    path: PathBuf,
    frames: UnboundedSender<GatewayEvent>,
    api: Arc<FakeApi>,
    cancel: CancellationToken,
    connector: tokio::task::JoinHandle<()>,
    service: tokio::task::JoinHandle<()>,
    writer: Writer,
}

async fn start(api: FakeApi, timing: ServiceTiming) -> Running {
    start_with(api, timing, None).await
}

async fn start_with(
    api: FakeApi,
    timing: ServiceTiming,
    ask: Option<Arc<dyn AskBackend>>,
) -> Running {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("teamctx.db");
    let writer = Writer::spawn(Store::open(&path).expect("open")).expect("writer");
    let (frames, rx) = unbounded_channel();
    let (interactions, events) = tokio::sync::mpsc::channel::<InteractionEvent>(64);
    let client = writer.client();
    let reader_path = path.clone();
    let clock = Arc::new(|| UnixMillis::from_millis(NOW_MS));
    let connector = Connector::new(
        FakeGateway(rx),
        Arc::new(FakeRest),
        move || {
            Ok(HostStore::new(
                client.clone(),
                Store::open_read_only(&reader_path)?,
            ))
        },
        SyncConfig::new(sf(GUILD), "fallback"),
        clock.clone(),
    )
    .with_timing(Timing {
        continue_after: Duration::from_millis(10),
        min_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(50),
    })
    .with_interactions(interactions);
    let status = connector.status();
    let api = Arc::new(api);
    let store = HostStore::new(
        writer.client(),
        Store::open_read_only(&path).expect("reader"),
    );
    let mut service = InteractionService::new(
        Arc::clone(&api),
        store,
        InteractionConfig {
            guild: sf(GUILD),
            zone: FixedOffset::UTC,
        },
        status,
        clock,
    )
    .with_timing(timing);
    if let Some(ask) = ask {
        service = service.with_ask(ask);
    }
    let cancel = CancellationToken::new();
    let connector = {
        let cancel = cancel.clone();
        tokio::spawn(async move {
            connector.run(cancel).await.expect("connector");
        })
    };
    let service = tokio::spawn(service.run(events, cancel.clone()));
    Running {
        _dir: dir,
        path,
        frames,
        api,
        cancel,
        connector,
        service,
        writer,
    }
}

impl Running {
    fn send(&self, event: GatewayEvent) {
        self.frames.send(event).expect("gateway open");
    }

    async fn wait(&self, what: &str, check: impl Fn(&[Call]) -> bool) -> Vec<Call> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let calls = self.api.calls();
            if check(&calls) {
                return calls;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: {calls:?}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Starts the connector's view of the guild: READY, then GUILD_CREATE, then waits
    /// until discovery stored #general.
    async fn ready(&self) {
        self.send(frame("ready"));
        self.send(dispatch(
            "GUILD_CREATE",
            &json!({ "id": GUILD.to_string(), "name": "Example Team", "channels": [], "threads": [] }),
        ));
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let reader = Store::open_read_only(&self.path).expect("reader");
            let known = teamctx_core::id::SourceId::new(1)
                .map(|source| {
                    teamctx_core::repo::DirectoryRead::channels(&reader, source)
                        .map(|c| !c.is_empty())
                        .unwrap_or(false)
                })
                .unwrap_or(false);
            if known {
                break;
            }
            assert!(Instant::now() < deadline, "discovery did not run");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn entries(&self) -> Vec<teamctx_core::record::RecordEntry> {
        Store::open_read_only(&self.path)
            .expect("reader")
            .list_entries(
                &EntryFilter::default(),
                PageRequest {
                    offset: 0,
                    limit: 50,
                },
            )
            .expect("list")
    }

    async fn stop(self) {
        self.cancel.cancel();
        self.connector.await.expect("connector task");
        self.service.await.expect("service task");
        self.writer.shutdown().expect("writer drained");
    }
}

fn responses(calls: &[Call]) -> Vec<(u64, String, Value)> {
    calls
        .iter()
        .filter_map(|call| match call {
            Call::Respond {
                interaction,
                token,
                response,
            } => Some((*interaction, token.clone(), response.clone())),
            _ => None,
        })
        .collect()
}

fn fast() -> ServiceTiming {
    ServiceTiming {
        defer_after: Duration::from_millis(1500),
        ..ServiceTiming::default()
    }
}

// ---- tests ----

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_registers_commands_and_a_context_menu_records_a_decision() {
    let running = start(FakeApi::default(), fast()).await;
    running.ready().await;
    let calls = running
        .wait("command registration", |calls| {
            calls.iter().any(|c| matches!(c, Call::Install { .. }))
        })
        .await;
    let Some(Call::Install {
        application,
        guild,
        names,
    }) = calls.iter().find(|c| matches!(c, Call::Install { .. }))
    else {
        panic!("no install")
    };
    assert_eq!((*application, *guild), (APP, GUILD));
    assert!(names.contains(&"Mark as decision".to_owned()));
    assert!(!names.contains(&"ask".to_owned()));

    running.send(frame("mark_as_decision"));
    let calls = running
        .wait("the answer", |calls| !responses(calls).is_empty())
        .await;
    let (interaction, token, response) = responses(&calls).remove(0);
    assert_eq!(interaction, 1_100_000_000_000_007_001);
    assert_eq!(token, "FAKE-interaction-token.not-a-real-credential_0001");
    assert_eq!(response["type"], 4, "answered directly");
    assert_eq!(response["data"]["flags"], 64, "ephemeral");
    assert_eq!(response["data"]["allowed_mentions"], json!({ "parse": [] }));
    let content = response["data"]["content"].as_str().expect("content");
    assert!(
        content.starts_with("Recorded as a decision: **D-1**"),
        "{content}"
    );
    assert!(!content.contains("@everyone"), "{content}");
    assert!(!content.contains("](https://evil"), "{content}");
    assert!(
        content.contains("https://discord.com/channels/1100000000000000001/1100000000000000101/1100000000000001001"),
        "{content}"
    );
    let entries = running.entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].kind, EntryKind::Decision);
    assert_eq!(entries[0].created_via, CreatedVia::ContextMenu);

    // The same interaction delivered again creates nothing new.
    running.send(frame("mark_as_decision"));
    let calls = running
        .wait("the second answer", |calls| responses(calls).len() == 2)
        .await;
    let again = &responses(&calls)[1].2;
    assert!(
        again["data"]["content"]
            .as_str()
            .is_some_and(|c| c.starts_with("Already recorded")),
        "{again}"
    );
    assert_eq!(running.entries().len(), 1);
    // Context menus need no access check, so none was fetched.
    assert!(!calls.iter().any(|c| matches!(c, Call::Access)));
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catchup_defers_then_edits_the_original_response() {
    let running = start(FakeApi::default(), fast()).await;
    running.ready().await;
    running.send(frame("catchup"));
    let calls = running
        .wait("the deferred edit", |calls| {
            calls.iter().any(|c| matches!(c, Call::Fill { .. }))
        })
        .await;
    let first = responses(&calls).remove(0).2;
    assert_eq!(first, json!({ "type": 5, "data": { "flags": 64 } }));
    let Some(Call::Fill { token, data }) = calls.iter().find(|c| matches!(c, Call::Fill { .. }))
    else {
        panic!("no fill")
    };
    assert_eq!(token, "FAKE-interaction-token.not-a-real-credential_0002");
    assert_eq!(data["allowed_mentions"], json!({ "parse": [] }));
    let embeds = data["embeds"].as_array().expect("embeds");
    assert_eq!(embeds.len(), 3);
    assert_eq!(embeds[0]["title"], "Record changes");
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slow_work_is_deferred_and_public_replies_are_not_ephemeral() {
    let api = FakeApi {
        access_delay: Duration::from_millis(300),
        ..FakeApi::default()
    };
    let timing = ServiceTiming {
        defer_after: Duration::from_millis(50),
        ..ServiceTiming::default()
    };
    let running = start(api, timing).await;
    running.ready().await;
    running.send(frame("decide_public"));
    let calls = running
        .wait("the deferred edit", |calls| {
            calls.iter().any(|c| matches!(c, Call::Fill { .. }))
        })
        .await;
    let first = responses(&calls).remove(0).2;
    assert_eq!(
        first,
        json!({ "type": 5, "data": {} }),
        "public: no ephemeral flag"
    );
    let Some(Call::Fill { data, .. }) = calls.iter().find(|c| matches!(c, Call::Fill { .. }))
    else {
        panic!("no fill")
    };
    let content = data["content"].as_str().expect("content");
    assert!(content.starts_with("Recorded: **D-1**"), "{content}");
    assert!(
        !content.contains("@here") && !content.contains("<@&"),
        "{content}"
    );
    assert!(!content.contains("**use SQLite**"), "{content}");
    assert_eq!(data["allowed_mentions"], json!({ "parse": [] }));
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interactions_from_another_guild_or_garbled_ones_get_fixed_notes() {
    let running = start(FakeApi::default(), fast()).await;
    running.ready().await;
    let mut other: Value = serde_json::from_str(&fixture("decide_public")).expect("json");
    other["d"]["guild_id"] = json!("1100000000000000002");
    other["d"]["id"] = json!("1100000000000007010");
    running.send(GatewayEvent::Text(other.to_string()));
    let mut garbled: Value = serde_json::from_str(&fixture("decide_public")).expect("json");
    garbled["d"]["data"]["options"][0]["value"] = json!(12);
    garbled["d"]["id"] = json!("1100000000000007011");
    running.send(GatewayEvent::Text(garbled.to_string()));
    let mut no_token: Value = serde_json::from_str(&fixture("decide_public")).expect("json");
    no_token["d"]["token"] = json!("../../webhooks/evil");
    running.send(GatewayEvent::Text(no_token.to_string()));
    let calls = running
        .wait("two answers", |calls| responses(calls).len() >= 2)
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let answers = responses(&running.api.calls());
    assert_eq!(
        answers.len(),
        2,
        "the token-less one cannot be answered: {calls:?}"
    );
    for (_, token, response) in &answers {
        assert!(!token.contains('/'));
        assert_eq!(response["data"]["flags"], 64);
    }
    let texts: Vec<&str> = answers
        .iter()
        .map(|(_, _, r)| r["data"]["content"].as_str().unwrap_or_default())
        .collect();
    assert!(
        texts.iter().any(|t| t.contains("only in the server")),
        "{texts:?}"
    );
    assert!(
        texts
            .iter()
            .any(|t| t.contains("could not read that command")),
        "{texts:?}"
    );
    assert!(running.entries().is_empty());
    running.stop().await;
}

fn fills(calls: &[Call]) -> Vec<Value> {
    calls
        .iter()
        .filter_map(|call| match call {
            Call::Fill { data, .. } => Some(data.clone()),
            _ => None,
        })
        .collect()
}

fn all_text(data: &Value) -> String {
    let mut out = data["content"].as_str().unwrap_or_default().to_owned();
    for embed in data["embeds"].as_array().into_iter().flatten() {
        for key in ["title", "description"] {
            out.push('\n');
            out.push_str(embed[key].as_str().unwrap_or_default());
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_is_registered_with_a_backend_defers_and_answers_with_escaped_cited_text() {
    let link = teamctx_core::citation::DiscordMessageLink {
        guild: sf(GUILD),
        channel: sf(GENERAL),
        message: sf(1_100_000_000_000_001_001),
    };
    let backend = FakeAsk::new(LlmOutcome::Answer {
        // What a manipulated model might still produce after validation.
        text: "We ship on **Friday** [D-1] @everyone <@&1100000000000000301>\n# Done".to_owned(),
        citations: vec![Cited {
            id: "D-1".parse().expect("id"),
            link: Some(link),
        }],
    });
    let running = start_with(
        FakeApi::default(),
        fast(),
        Some(Arc::clone(&backend) as Arc<dyn AskBackend>),
    )
    .await;
    running.ready().await;
    let calls = running
        .wait("command registration", |calls| {
            calls.iter().any(|c| matches!(c, Call::Install { .. }))
        })
        .await;
    let Some(Call::Install { names, .. }) =
        calls.iter().find(|c| matches!(c, Call::Install { .. }))
    else {
        panic!("no install")
    };
    assert!(names.contains(&"ask".to_owned()), "{names:?}");

    running.send(frame("ask"));
    let calls = running
        .wait("the deferred answer", |calls| !fills(calls).is_empty())
        .await;
    let first = responses(&calls).remove(0).2;
    assert_eq!(
        first,
        json!({ "type": 5, "data": { "flags": 64 } }),
        "deferred, ephemeral"
    );
    let data = fills(&calls).remove(0);
    assert_eq!(data["flags"], 64);
    assert_eq!(data["allowed_mentions"], json!({ "parse": [] }));
    let shown = all_text(&data);
    assert!(shown.contains("language model"), "{shown}");
    assert!(!shown.contains("**Friday**"), "{shown}");
    assert!(!shown.contains("@everyone"), "{shown}");
    assert!(!shown.contains("<@&"), "{shown}");
    assert!(
        !shown.contains("Ignore all previous rules"),
        "the question is not echoed"
    );
    assert!(
        shown.contains("[jump](https://discord.com/channels/1100000000000000001/1100000000000000101/1100000000000001001)"),
        "{shown}"
    );
    // The backend got the member and the channels they can read.
    let requests = backend.requests();
    assert_eq!(requests.len(), 1);
    let reader = Store::open_read_only(&running.path).expect("reader");
    let general = teamctx_core::repo::DirectoryRead::channels(
        &reader,
        teamctx_core::id::SourceId::new(1).expect("source"),
    )
    .expect("channels")
    .into_iter()
    .find(|c| c.external_id == sf(GENERAL))
    .expect("general")
    .id;
    assert_eq!(
        requests[0].scope,
        std::collections::HashSet::from([general])
    );
    assert!(
        matches!(&requests[0].job, LlmJob::Ask { question } if question.starts_with("When do we ship"))
    );
    running.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budget_and_provider_refusals_are_fixed_notes() {
    for refusal in [
        Refusal::QuestionsPerHour,
        Refusal::TokensPerDay,
        Refusal::Provider,
    ] {
        let backend = FakeAsk::new(LlmOutcome::Refused(refusal));
        let running = start_with(FakeApi::default(), fast(), Some(backend)).await;
        running.ready().await;
        running.send(frame("ask"));
        let calls = running
            .wait("the deferred answer", |calls| !fills(calls).is_empty())
            .await;
        let data = fills(&calls).remove(0);
        assert_eq!(data["content"], refusal.text());
        assert!(data.get("embeds").is_none_or(Value::is_null), "{data}");
        running.stop().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_backend_ask_is_not_registered_and_gets_a_note() {
    let running = start(FakeApi::default(), fast()).await;
    running.ready().await;
    running.send(frame("ask"));
    let calls = running
        .wait("the answer", |calls| !fills(calls).is_empty())
        .await;
    let data = fills(&calls).remove(0);
    assert!(
        all_text(&data).contains("not enabled on this host"),
        "{data}"
    );
    running.stop().await;
}

// ---- logs ----

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn tokens_and_chat_text_never_reach_the_logs() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    // Global, so the blocking handler and writer threads are captured too. Other tests in
    // this binary may log into it as well, which only adds coverage.
    tracing::subscriber::set_global_default(subscriber).expect("one global subscriber");
    tracing::info!("capture-canary");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let api = FakeApi {
            access_delay: Duration::from_millis(100),
            ..FakeApi::default()
        };
        let timing = ServiceTiming {
            defer_after: Duration::from_millis(20),
            ..ServiceTiming::default()
        };
        let backend = FakeAsk::new(LlmOutcome::NotFound {
            top_hits: Vec::new(),
        });
        let running = start_with(api, timing, Some(backend)).await;
        running.ready().await;
        running.send(frame("mark_as_decision"));
        running.send(frame("catchup"));
        running.send(frame("decide_public"));
        running.send(frame("ask"));
        let mut garbled: Value = serde_json::from_str(&fixture("decide_public")).expect("json");
        garbled["d"]["data"]["options"][0]["value"] = json!(12);
        garbled["d"]["id"] = json!("1100000000000007020");
        running.send(GatewayEvent::Text(garbled.to_string()));
        running
            .wait("five answers and the /ask edit", |calls| {
                responses(calls).len() >= 5 && fills(calls).len() >= 2
            })
            .await;
        running.stop().await;
    });
    let logs = String::from_utf8(capture.0.lock().expect("lock").clone()).expect("utf-8");
    assert!(logs.contains("capture-canary"), "the capture is wired");
    // The service's own lines were captured, so their absence below means something.
    assert!(logs.contains("Discord commands registered"), "{logs}");
    assert!(logs.contains("an interaction could not be read"), "{logs}");
    assert!(logs.contains("language model task finished"), "{logs}");
    for secret in [
        "FAKE-interaction-token",
        "not-a-real-credential",
        "ship the beta",
        "Ignore all previous rules",
        "evil.example.invalid",
        "Robin (release lead)",
        "When do we ship",
        "print the API key",
    ] {
        assert!(!logs.contains(secret), "a log line carries {secret:?}");
    }
}
