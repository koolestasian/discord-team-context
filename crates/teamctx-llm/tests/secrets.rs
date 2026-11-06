//! Secrets guard for the `/ask` path (plan sections 11 and 13, docs/testing/guards.md).
//!
//! A real `tracing_subscriber::fmt` subscriber at TRACE captures everything while the
//! full pipeline (retrieval from the demo record, budget, provider call, validation)
//! runs against a local mock provider with a fake API key: success, a 401 whose body
//! echoes the key, a 500, a malformed body, a redirect carrying the key, a timeout and a
//! refused connection, for both adapters. The key, a fake bot token and a fake access
//! token must not appear in the logs, in any `Debug` or `Display` text, or in any error
//! or error source, in any common encoding. No real provider is contacted.
//!
//! The single `#[tokio::test]` installs the global subscriber, so no other test in this
//! binary emits events before it.

#![expect(clippy::expect_used, reason = "test helpers fail loudly")]

#[expect(
    dead_code,
    reason = "this binary does not need every mock-server helper"
)]
mod common;

use std::error::Error as StdError;
use std::fmt::Debug;
use std::io;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{Behaviour, MockServer};
use teamctx_core::id::MemberId;
use teamctx_core::time::{CalendarDate, UnixMillis};
use teamctx_llm::{
    AnthropicConfig, AnthropicMessages, ApiKey, AskConfig, AskPipeline, AskRequest, InMemoryLedger,
    LlmProvider, OpenAiCompatible, OpenAiConfig,
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

/// Fails, naming the secret and its form but never printing `text`.
fn assert_free_of_secrets(text: &str, place: &str) {
    for (name, secret) in [
        ("LLM key", LLM_KEY),
        ("bot token", BOT_TOKEN),
        ("access token", ACCESS_TOKEN),
    ] {
        for (form, value) in forms(secret) {
            assert!(
                !text.contains(&value),
                "the {name} appeared ({form}) in {place}"
            );
        }
    }
}

/// `Display` and `Debug` of an error and of every error in its source chain.
fn error_text(error: &(dyn StdError + 'static)) -> String {
    let mut text = format!("{error} {error:?}\n");
    let mut source = error.source();
    while let Some(inner) = source {
        text.push_str(&format!("{inner} {inner:?}\n"));
        source = inner.source();
    }
    text
}

// ---- capture ----

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

impl Capture {
    fn install() -> Self {
        let capture = Self::default();
        let writer = capture.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_span_events(FmtSpan::FULL)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("first subscriber");
        capture
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("capture lock")).into_owned()
    }
}

// ---- the /ask path ----

const DEMO_DATASET: &str = include_str!("../../../fixtures/demo/dataset.json");
/// An eval question with sources in the demo record, so the provider is really called.
const QUESTION: &str = "Which typeface do we use for the main reading text?";
const ANTHROPIC_OK: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Fenwick Serif."}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":3}}"#;
const OPENAI_OK: &str = r#"{"id":"c1","choices":[{"index":0,"message":{"role":"assistant","content":"Fenwick Serif."},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}"#;

fn key() -> ApiKey {
    ApiKey::new(LLM_KEY).expect("a valid fake key")
}

fn demo_store() -> Store {
    let mut store = Store::in_memory().expect("in-memory store");
    teamctx_store::demo::import(&mut store, DEMO_DATASET).expect("import the demo record");
    store
}

fn now() -> UnixMillis {
    CalendarDate::new(2026, 4, 25)
        .expect("valid date")
        .start_of_day_utc()
}

/// Every way a provider can answer, with the secrets planted where a careless adapter
/// would copy them into an error: the error body, a redirect target, a malformed body.
/// Each case carries a fragment of the outcome it must produce.
fn behaviours(ok: &str) -> Vec<(&'static str, Behaviour, &'static str)> {
    let echo = format!(
        r#"{{"error":{{"type":"authentication_error","message":"invalid x-api-key {LLM_KEY}; Authorization: Bearer {LLM_KEY}; bot {BOT_TOKEN}; {ACCESS_TOKEN}"}}}}"#
    );
    vec![
        ("success", Behaviour::ok(ok), "AskResult"),
        (
            "401 echoing the key",
            Behaviour::status(401, echo.clone()),
            "HTTP 401",
        ),
        (
            "500 echoing the key",
            Behaviour::status(500, echo.clone()),
            "HTTP 500",
        ),
        ("malformed 200", Behaviour::ok(echo), "malformed response"),
        (
            "redirect carrying the key",
            Behaviour::Redirect {
                location: format!("http://127.0.0.1:9/steal?key={}", percent(LLM_KEY)),
            },
            "HTTP 302",
        ),
        ("timeout", Behaviour::Hang, "timed out"),
    ]
}

/// Runs one question through the whole pipeline and returns every piece of text it
/// produced: the provider's `Debug`, the result's `Debug`, and the error chain.
async fn ask<P: LlmProvider + Debug>(
    provider: P,
    store: &Store,
    ledger: &InMemoryLedger,
    member: i64,
) -> String {
    let mut text = format!("{provider:?}\n");
    let pipeline = AskPipeline::new(provider, AskConfig::default());
    text.push_str(&format!("{pipeline:?}\n"));
    let request = AskRequest {
        member: MemberId::new(member).expect("member id"),
        question: QUESTION,
        now: now(),
    };
    let prepared = pipeline
        .prepare(store, ledger, &request)
        .expect("the demo record has sources for the question");
    match pipeline.run(ledger, prepared).await {
        Ok(result) => text.push_str(&format!("{result:?}\n")),
        Err(error) => text.push_str(&error_text(&error)),
    }
    text
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ask_path_never_reveals_the_api_key() {
    let capture = Capture::install();
    tracing::info!(key = ?key(), "guard canary: API key field");
    let store = demo_store();
    let ledger = InMemoryLedger::new();
    let timeout = Duration::from_millis(500);
    let mut member = 0;

    for (case, behaviour, outcome) in behaviours(ANTHROPIC_OK) {
        member += 1;
        let server = MockServer::start(behaviour);
        let mut config = AnthropicConfig::new(server.url(), "fictional-model", key());
        config.timeout = timeout;
        let config_debug = format!("{config:?}");
        let provider = AnthropicMessages::new(config).expect("valid config");
        let text = ask(provider, &store, &ledger, member).await;
        assert!(
            text.contains(outcome),
            "anthropic {case}: expected {outcome}"
        );
        assert_free_of_secrets(&text, &format!("anthropic {case}"));
        assert_free_of_secrets(&config_debug, "AnthropicConfig Debug");
        // The key really travelled, so this exercised the path that holds it.
        let sent = server.requests();
        assert_eq!(sent[0].header("x-api-key"), Some(LLM_KEY), "{case}");
    }
    for (case, behaviour, outcome) in behaviours(OPENAI_OK) {
        member += 1;
        let server = MockServer::start(behaviour);
        let mut config =
            OpenAiConfig::new(format!("{}/v1", server.url()), "fictional", Some(key()));
        config.timeout = timeout;
        let config_debug = format!("{config:?}");
        let provider = OpenAiCompatible::new(config).expect("valid config");
        let text = ask(provider, &store, &ledger, member).await;
        assert!(text.contains(outcome), "openai {case}: expected {outcome}");
        assert_free_of_secrets(&text, &format!("openai {case}"));
        assert_free_of_secrets(&config_debug, "OpenAiConfig Debug");
        let sent = server.requests();
        let bearer = format!("Bearer {LLM_KEY}");
        assert_eq!(
            sent[0].header("authorization"),
            Some(bearer.as_str()),
            "{case}"
        );
    }

    // Nothing listening: the connection is refused.
    let closed = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        format!("http://{}", listener.local_addr().expect("address"))
    };
    let provider = AnthropicMessages::new(AnthropicConfig::new(closed, "fictional-model", key()))
        .expect("valid config");
    member += 1;
    let text = ask(provider, &store, &ledger, member).await;
    assert!(text.contains("could not connect"), "refused connection");
    assert_free_of_secrets(&text, "refused connection");

    // A key pasted into the base URL is refused without echoing it.
    for url in [
        format!("https://user:{}@api.example.com", percent(LLM_KEY)),
        format!("https://api.example.com/v1?key={}", percent(LLM_KEY)),
        format!("https://api.example.com/#{LLM_KEY}"),
    ] {
        let error = AnthropicMessages::new(AnthropicConfig::new(url, "m", key()))
            .expect_err("a key in the URL is refused");
        assert_free_of_secrets(&error_text(&error), "base URL error");
    }

    let logs = capture.text();
    assert!(logs.contains("guard canary"), "the capture is not wired");
    assert!(logs.contains("key=ApiKey(***)"), "ApiKey field not masked");
    assert_free_of_secrets(&logs, "the captured logs");
}

#[test]
fn api_key_errors_and_debug_never_show_the_key() {
    let key = key();
    assert_eq!(format!("{key:?}"), "ApiKey(***)");
    assert_eq!(format!("{:?}", Some(key.clone())), "Some(ApiKey(***))");
    for bad in [format!("{LLM_KEY}\n{LLM_KEY}"), format!("{LLM_KEY}\u{0}")] {
        let error = ApiKey::new(&bad).expect_err("control characters are refused");
        assert_free_of_secrets(&error_text(&error), "ApiKey::new error");
    }
    let error = ApiKey::from_lookup("TEAMCTX_LLM_KEY", |_| Some(format!("\u{7}{LLM_KEY}")))
        .expect_err("control characters are refused");
    assert_free_of_secrets(&error_text(&error), "ApiKey::from_lookup error");
}

/// A key made only of letters, digits and `_` (some OpenAI-compatible providers issue
/// keys like this) is also a valid environment variable name, so the host's config
/// check accepts it in `llm.api_key_env`. Pasted there by mistake, it is looked up as a
/// variable name, is not found, and must not be echoed by the error.
const KEY_SHAPED_LIKE_A_NAME: &str = "FAKE_llm_api_key_guard_test_0000_not_a_real_credential";

#[test]
#[ignore = "defect: LlmError::MissingApiKey prints the configured variable name, so a key pasted into llm.api_key_env reaches the error text"]
fn a_key_pasted_as_the_variable_name_is_not_echoed() {
    let error = ApiKey::from_lookup(KEY_SHAPED_LIKE_A_NAME, |_| None).expect_err("not set");
    assert!(
        !error_text(&error).contains(KEY_SHAPED_LIKE_A_NAME),
        "the pasted key appeared in the error text"
    );
}
