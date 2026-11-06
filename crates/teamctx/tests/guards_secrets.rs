//! Secrets guard for the host: configuration, the token CLI and the Discord sync (plan
//! section 13, ADR 0005, docs/testing/guards.md).
//!
//! - **Config and token CLI**: the real binary runs with a fake bot token and a fake LLM
//!   key in its environment (`RUST_LOG=trace`, `RUST_BACKTRACE=1`), and with the secrets
//!   pasted into the wrong places: config fields, CLI options, override variables. Its
//!   stdout and stderr are the logs here; none may contain a secret. `token create` is
//!   the one documented exception: it prints the new token once, on stdout.
//! - **Sync**: `sync_guild` runs in process under a `tracing_subscriber::fmt` subscriber
//!   at TRACE against a fake Discord whose chat text (message, channel name, topic,
//!   author, attachment) carries the secrets, plus malformed, forbidden and failing
//!   responses. Logs, reports and error chains must stay free of them.
//!
//! Every value is fake and every check searches the raw value and its hex, percent and
//! base64 forms. Failures name the secret and the place, never the captured text.

#![expect(clippy::expect_used, reason = "test helpers fail loudly")]

use std::error::Error as StdError;
use std::io;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use teamctx_core::id::{DISCORD_EPOCH_MS, Snowflake};
use teamctx_core::time::UnixMillis;
use teamctx_discord::sync::{
    ChannelOutcome, DiscordRest, MessagesRequest, PagePosition, RestError, SkipReason, SyncConfig,
    ThreadVisibility, sync_guild,
};
use teamctx_store::Store;
use tracing_subscriber::fmt::format::FmtSpan;

// ---- fake secrets and the encodings searched for ----

const BOT_TOKEN: &str = "FAKE-discord-bot-token.guard-test.0000-not-a-real-credential";
const LLM_KEY: &str = "FAKE-llm-api-key+guard/test=0000-not-a-real-credential";
const ACCESS_TOKEN: &str = "tctx_feedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedfacefeedface";

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL_SAFE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64(bytes: &[u8], alphabet: &[u8; 64]) -> String {
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .chain([0, 0].iter())
            .take(3)
            .fold(0_u32, |n, b| (n << 8) | u32::from(*b));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(alphabet[((n >> (18 - 6 * i)) & 63) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn percent(secret: &str) -> String {
    secret
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// As is, hex (both cases), percent-encoded (both cases), and base64 (standard and
/// URL-safe) at all three byte alignments without the edge characters.
fn forms(secret: &str) -> Vec<(String, String)> {
    let hex: String = secret.bytes().map(|b| format!("{b:02x}")).collect();
    let mut out = vec![
        ("raw".to_owned(), secret.to_owned()),
        ("hex".to_owned(), hex.clone()),
        ("HEX".to_owned(), hex.to_uppercase()),
        ("percent".to_owned(), percent(secret)),
        ("percent-lower".to_owned(), percent(secret).to_lowercase()),
    ];
    for (name, alphabet) in [("base64", STANDARD), ("base64url", URL_SAFE)] {
        for offset in 0..3 {
            let mut shifted = vec![b'x'; offset];
            shifted.extend_from_slice(secret.as_bytes());
            let encoded = base64(&shifted, alphabet);
            let start = if offset == 0 { 0 } else { 4 };
            out.push((
                format!("{name}+{offset}"),
                encoded[start..encoded.len() - 4].to_owned(),
            ));
        }
    }
    out
}

fn assert_free_of(text: &str, place: &str, secrets: &[(&str, &str)]) {
    for (name, secret) in secrets {
        for (form, value) in forms(secret) {
            assert!(
                !text.contains(&value),
                "the {name} appeared ({form}) in {place}"
            );
        }
    }
}

fn assert_free_of_secrets(text: &str, place: &str) {
    assert_free_of(
        text,
        place,
        &[
            ("bot token", BOT_TOKEN),
            ("LLM key", LLM_KEY),
            ("access token", ACCESS_TOKEN),
        ],
    );
}

fn error_text(error: &(dyn StdError + 'static)) -> String {
    let mut text = format!("{error} {error:?}\n");
    let mut source = error.source();
    while let Some(inner) = source {
        text.push_str(&format!("{inner} {inner:?}\n"));
        source = inner.source();
    }
    text
}

// ---- the binary: configuration and the token CLI ----

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `teamctx` in `dir` with only the fake secrets and log switches in its
/// environment, plus `extra`.
fn teamctx(dir: &Path, args: &[&str], extra: &[(&str, &str)]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_teamctx"))
        .current_dir(dir)
        .env_clear()
        .env("TEAMCTX_DISCORD_TOKEN", BOT_TOKEN)
        .env("TEAMCTX_LLM_KEY", LLM_KEY)
        .env("RUST_LOG", "trace")
        .env("RUST_BACKTRACE", "1")
        .envs(extra.iter().copied())
        .args(args)
        .output()
        .expect("run teamctx");
    Run {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

impl Run {
    fn all(&self) -> String {
        format!("{}\n{}", self.stdout, self.stderr)
    }
}

const LLM_SECTION: &str = "[llm]\nprovider = 'anthropic'\nbase_url = 'https://api.example.com'\n\
                           model = 'fictional-model'\n";

#[test]
fn config_paths_never_print_a_secret() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path();
    let file = |name: &str, text: &str| {
        let file = path.join(name);
        std::fs::write(&file, text).expect("write config");
        file.to_string_lossy().into_owned()
    };

    // Normal use, with the secrets set where they belong.
    let llm = file(
        "llm.toml",
        &format!("[discord]\nguild_id = '123'\n{LLM_SECTION}"),
    );
    for args in [
        vec!["doctor", "--guild-id", "123"],
        vec!["init", "--guild-id", "123", "--application-id", "456"],
        vec!["doctor", "--config", &llm],
        vec!["init", "--config", &llm],
    ] {
        let run = teamctx(path, &args, &[]);
        assert_eq!(run.code, Some(0), "{args:?}");
        assert_free_of_secrets(&run.all(), &format!("output of {args:?}"));
    }
    let doctor = teamctx(path, &["doctor", "--guild-id", "123"], &[]);
    assert!(
        doctor
            .stdout
            .contains("Discord token environment variable: set")
    );

    // Secrets pasted where a name or a setting belongs: refused, never echoed.
    let pasted_files = [
        format!("[discord]\nguild_id = '123'\ntoken = '{BOT_TOKEN}'\n"),
        format!("[discord]\nguild_id = '123'\ntoken_env = '{BOT_TOKEN}'\n"),
        format!("[discord]\nguild_id = '{BOT_TOKEN}'\n"),
        format!("[discord]\nguild_id = '123'\n{LLM_SECTION}api_key = '{LLM_KEY}'\n"),
        format!("[discord]\nguild_id = '123'\n{LLM_SECTION}api_key_env = '{LLM_KEY}'\n"),
        format!("[discord]\nguild_id = '123'\n[server]\nbind = '{ACCESS_TOKEN}'\n"),
        format!("[discord\nguild_id = '{BOT_TOKEN}'\n"),
        format!("{LLM_KEY} = {BOT_TOKEN}\n"),
    ];
    for (i, text) in pasted_files.iter().enumerate() {
        let config = file(&format!("pasted-{i}.toml"), text);
        for command in ["doctor", "init"] {
            let run = teamctx(path, &[command, "--config", &config], &[]);
            assert_ne!(run.code, Some(0), "{command} pasted file {i}");
            assert!(run.stderr.contains("error"), "{command} pasted file {i}");
            assert_free_of_secrets(&run.all(), &format!("{command} with pasted file {i}"));
        }
    }
    let public_url = format!("https://robin:{BOT_TOKEN}@team.example.com");
    let pasted_options: [&[&str]; 5] = [
        &["--token-env", BOT_TOKEN],
        &["--bind", ACCESS_TOKEN],
        &["--public-url", &public_url],
        &["--backfill-days", LLM_KEY],
        &["--include-channels", "general", "--guild-id", BOT_TOKEN],
    ];
    for options in pasted_options {
        for command in ["doctor", "init"] {
            let mut args = vec![command, "--guild-id", "123"];
            args.extend_from_slice(options);
            let run = teamctx(path, &args, &[]);
            assert_ne!(run.code, Some(0), "{command} {}", options[0]);
            assert_free_of_secrets(&run.all(), &format!("{command} {}", options[0]));
        }
    }
    let pasted_env: [(&str, &str); 6] = [
        ("TEAMCTX_TOKEN_ENV", BOT_TOKEN),
        ("TEAMCTX_GUILD_ID", BOT_TOKEN),
        ("TEAMCTX_BIND", ACCESS_TOKEN),
        ("TEAMCTX_LLM_KEY_ENV", LLM_KEY),
        ("TEAMCTX_RECHECK_WINDOW_HOURS", LLM_KEY),
        ("TEAMCTX_REACTION_CAPTURE", BOT_TOKEN),
    ];
    for (name, value) in pasted_env {
        let run = teamctx(
            path,
            &["doctor", "--config", &llm],
            &[(name, value), ("TEAMCTX_LLM_PROVIDER", "anthropic")],
        );
        assert_ne!(run.code, Some(0), "{name}");
        assert_free_of_secrets(&run.all(), &format!("doctor with {name}"));
    }
}

/// The first `tctx_` token in `text`.
fn token_in(text: &str) -> String {
    let start = text.find("tctx_").expect("a token was printed");
    text[start..start + 69].to_owned()
}

#[test]
fn token_cli_prints_a_token_once_and_never_again() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path();
    let db = path.join("teamctx.db").to_string_lossy().into_owned();

    let create = teamctx(path, &["token", "create", "guard-laptop", "--db", &db], &[]);
    assert_eq!(create.code, Some(0), "create");
    assert!(create.stderr.is_empty(), "create wrote to stderr");
    let created = token_in(&create.stdout);
    assert_eq!(create.stdout.matches(&created).count(), 1, "shown once");
    assert_free_of_secrets(&create.all(), "token create");

    let runs = [
        teamctx(path, &["token", "list", "--db", &db], &[]),
        teamctx(path, &["token", "create", "guard-laptop", "--db", &db], &[]),
        teamctx(path, &["token", "revoke", "guard-laptop", "--db", &db], &[]),
        teamctx(path, &["token", "revoke", "guard-laptop", "--db", &db], &[]),
        teamctx(path, &["token", "list", "--db", &db], &[]),
        // A token pasted where the name belongs.
        teamctx(path, &["token", "revoke", &created, "--db", &db], &[]),
        teamctx(path, &["token", "revoke", ACCESS_TOKEN, "--db", &db], &[]),
        teamctx(path, &["token", "create", ACCESS_TOKEN, "--db", &db], &[]),
        teamctx(path, &["token", "create", BOT_TOKEN, "--db", &db], &[]),
    ];
    for (i, run) in runs.iter().enumerate() {
        let place = format!("token command {i}");
        assert_free_of(
            &run.all(),
            &place,
            &[("created token", &created), ("its body", &created[5..])],
        );
        assert_free_of_secrets(&run.all(), &place);
        assert!(!run.stdout.contains("tctx_"), "{place} printed a token");
    }
    assert!(runs[2].stdout.contains("Revoked token"), "revoke ran");
    assert!(
        runs[4].stdout.contains("revoked"),
        "list shows the revocation"
    );
}

// ---- the sync, in process ----

const GUILD: u64 = 1_100_000_000_000_000_001;
const GENERAL: u64 = 1_100_000_000_000_000_010;
const GARBLED: u64 = 1_100_000_000_000_000_011;
const PRIVATE: u64 = 1_100_000_000_000_000_012;
const NOW_MS: i64 = 1_800_000_000_000;

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).expect("non-zero id")
}

fn message_id(minutes_ago: i64) -> u64 {
    let millis = NOW_MS - minutes_ago * 60_000 - DISCORD_EPOCH_MS;
    u64::try_from(millis).expect("after the Discord epoch") << 22
}

fn channel_json(id: u64, name: &str, topic: &str) -> Value {
    json!({
        "id": id.to_string(),
        "guild_id": GUILD.to_string(),
        "type": 0,
        "name": name,
        "topic": topic,
        "position": 0,
        "nsfw": false,
        "permission_overwrites": [],
        "flags": 0
    })
}

fn message_json(channel: u64, id: u64, content: &str) -> Value {
    json!({
        "id": id.to_string(),
        "channel_id": channel.to_string(),
        "type": 0,
        "content": content,
        "author": {
            "id": "1100000000000000201",
            "username": "robin_example",
            "global_name": format!("Robin {ACCESS_TOKEN}"),
            "discriminator": "0",
            "avatar": null,
            "bot": false
        },
        "mentions": [],
        "mention_roles": [],
        "attachments": [{
            "id": "1100000000000000301",
            "filename": format!("{LLM_KEY}.txt"),
            "size": 12,
            "url": "https://cdn.example.com/a.txt",
            "proxy_url": "https://cdn.example.com/a.txt"
        }],
        "embeds": [],
        "pinned": false,
        "timestamp": "2027-01-15T08:00:00.000000+00:00",
        "edited_timestamp": null
    })
}

/// A Discord whose chat is full of pasted secrets and whose responses go wrong in
/// every way the sync handles.
enum FakeDiscord {
    /// One readable channel with secrets in its chat, one with a garbled page, one
    /// the bot cannot read.
    Hostile,
    /// The guild listing is not a list.
    GarbledDiscovery,
    /// Every call fails with this error.
    Failing(fn() -> RestError),
}

impl DiscordRest for FakeDiscord {
    fn guild_channels(&self, _guild: Snowflake) -> Result<Value, RestError> {
        let pasted = format!("{BOT_TOKEN} {LLM_KEY} {ACCESS_TOKEN}");
        match self {
            Self::Hostile => Ok(json!([
                channel_json(GENERAL, &format!("general-{ACCESS_TOKEN}"), &pasted),
                channel_json(GARBLED, "garbled", &pasted),
                channel_json(PRIVATE, "private", &pasted),
            ])),
            Self::GarbledDiscovery => Ok(json!({ "message": pasted, "code": 0 })),
            Self::Failing(error) => Err(error()),
        }
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

    fn channel_messages(&self, request: &MessagesRequest) -> Result<Value, RestError> {
        if request.position != PagePosition::Latest {
            return Ok(json!([]));
        }
        match request.channel.get() {
            GENERAL => Ok(json!([
                message_json(
                    GENERAL,
                    message_id(1),
                    &format!("the bot token is {BOT_TOKEN}")
                ),
                message_json(GENERAL, message_id(2), &format!("my key: {LLM_KEY}")),
                message_json(GENERAL, message_id(3), &format!("use {ACCESS_TOKEN}")),
            ])),
            GARBLED => Ok(json!({ "message": BOT_TOKEN, "token": LLM_KEY, "id": ACCESS_TOKEN })),
            _ => Err(RestError::Forbidden),
        }
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The only test in this binary that installs a subscriber, so none races it.
#[test]
fn sync_path_never_reveals_secrets_from_chat_or_failures() {
    let capture = Capture::default();
    let writer = capture.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::FULL)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::set_global_default(subscriber).expect("first subscriber");
    tracing::info!("guard canary: sync");

    let now = UnixMillis::from_millis(NOW_MS);
    let config = SyncConfig::new(sf(GUILD), "Lantern Shelf (fictional)");
    let mut shown = format!("{config:?}\n");

    let mut store = Store::in_memory().expect("store");
    let report = sync_guild(&FakeDiscord::Hostile, &mut store, &config, now).expect("sync runs");
    let general = report.channel(sf(GENERAL)).expect("general synced");
    assert_eq!(general.outcome, ChannelOutcome::Synced);
    assert_eq!(
        general.upserted, 3,
        "the messages with secrets were processed"
    );
    assert_eq!(
        report.channel(sf(GARBLED)).map(|c| c.outcome),
        Some(ChannelOutcome::Skipped(SkipReason::MalformedPayload))
    );
    assert_eq!(
        report.channel(sf(PRIVATE)).map(|c| c.outcome),
        Some(ChannelOutcome::Skipped(SkipReason::Forbidden))
    );
    shown.push_str(&format!("{report:?}\n{report:#?}\n"));

    let failing: [FakeDiscord; 5] = [
        FakeDiscord::GarbledDiscovery,
        FakeDiscord::Failing(|| RestError::Unauthorized),
        FakeDiscord::Failing(|| RestError::RateLimited {
            retry_after: Duration::from_secs(30),
            global: true,
        }),
        FakeDiscord::Failing(|| RestError::Server { status: 502 }),
        // What an HTTP client's transport error looks like: a URL, no headers.
        FakeDiscord::Failing(|| {
            RestError::Transport(
                "error sending request for url \
                 (https://discord.com/api/v10/guilds/1100000000000000001/channels)"
                    .into(),
            )
        }),
    ];
    for fake in &failing {
        let mut store = Store::in_memory().expect("store");
        let error = sync_guild(fake, &mut store, &config, now).expect_err("the run stops");
        shown.push_str(&error_text(&error));
    }

    let logs = String::from_utf8_lossy(&capture.0.lock().expect("capture lock")).into_owned();
    assert!(
        logs.contains("guard canary: sync"),
        "the capture is not wired"
    );
    assert_free_of_secrets(&logs, "the captured sync logs");
    assert_free_of_secrets(&shown, "sync reports and errors");
}
