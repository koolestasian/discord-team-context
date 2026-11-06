//! Secrets guard for the auth and token paths (plan section 13, ADR 0005,
//! docs/testing/guards.md).
//!
//! A real `tracing_subscriber::fmt` subscriber at TRACE captures everything while access
//! tokens are created, accepted, revoked, rejected and rate limited, and while a fake
//! bot token and a fake LLM key are presented as credentials. Neither the logs nor any
//! `Debug`/`Display` text may contain one of the values in any common encoding.
//!
//! Every value here is fake. The single `#[tokio::test]` installs the global subscriber,
//! so no other test in this binary emits events before it.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; helpers panic on failure too"
)]

#[expect(dead_code, reason = "this binary does not need every harness helper")]
mod common;

use std::io;
use std::sync::{Arc, Mutex};

use common::{TestDb, TestServer};
use reqwest::{Client, StatusCode};
use serde_json::json;
use teamctx_core::repo::TokenWrite;
use teamctx_core::time::UnixMillis;
use teamctx_server::auth::{Auth, token};
use teamctx_server::secret::Secret;
use tracing_subscriber::fmt::format::FmtSpan;

// ---- fake secrets and the encodings searched for ----

const BOT_TOKEN: &str = "FAKE-discord-bot-token.guard-test.0000-not-a-real-credential";
const LLM_KEY: &str = "FAKE-llm-api-key+guard/test=0000-not-a-real-credential";
/// Right shape (`tctx_` + 64 hex digits), obviously not random.
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

/// Every form a secret is searched for: as is, hex (both cases), percent-encoded, and
/// base64 (standard and URL-safe) at all three byte alignments, so the value is found
/// even inside a longer encoded string such as a Basic auth header. The base64 forms
/// drop the edge characters that depend on neighbouring bytes.
fn forms(secret: &str) -> Vec<(String, String)> {
    let bytes = secret.as_bytes();
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    let percent: String = bytes
        .iter()
        .map(|&b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect();
    let mut out = vec![
        ("raw".to_owned(), secret.to_owned()),
        ("hex".to_owned(), hex.clone()),
        ("HEX".to_owned(), hex.to_uppercase()),
        ("percent".to_owned(), percent.clone()),
        ("percent-lower".to_owned(), percent.to_lowercase()),
    ];
    for (name, alphabet) in [("base64", STANDARD), ("base64url", URL_SAFE)] {
        for offset in 0..3 {
            let mut shifted = vec![b'x'; offset];
            shifted.extend_from_slice(bytes);
            let encoded = base64(&shifted, alphabet);
            let start = if offset == 0 { 0 } else { 4 };
            let core = &encoded[start..encoded.len() - 4];
            out.push((format!("{name}+{offset}"), core.to_owned()));
        }
    }
    out
}

/// Fails, naming the secret and its form but never printing `text`.
fn assert_free_of_secrets(text: &str, place: &str) {
    for (name, secret) in [
        ("bot token", BOT_TOKEN),
        ("LLM key", LLM_KEY),
        ("access token", ACCESS_TOKEN),
        ("access token body", &ACCESS_TOKEN[5..]),
    ] {
        for (form, value) in forms(secret) {
            assert!(
                !text.contains(&value),
                "the {name} appeared ({form}) in {place}"
            );
        }
    }
}

// ---- capture ----

/// A `MakeWriter` target: everything the fmt layer writes ends up in one buffer.
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

// ---- HTTP ----

async fn call(client: &Client, server: &TestServer, authorization: &str) -> StatusCode {
    client
        .post(&server.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .header("Mcp-Method", "tools/list")
        .header("Authorization", authorization)
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientInfo": { "name": "guard-test", "version": "0.0.0" },
                "io.modelcontextprotocol/clientCapabilities": {}
            } }
        }))
        .send()
        .await
        .expect("send")
        .status()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_and_token_paths_never_reveal_secrets() {
    let capture = Capture::install();
    let mut shown = String::new();

    // Token path: a fixed fake token stored by hash, and a freshly generated one.
    let db = TestDb::new();
    db.writer
        .client()
        .create_token(
            "guard-fixed",
            &token::hash(ACCESS_TOKEN.as_bytes()),
            "read",
            UnixMillis::from_millis(1),
        )
        .expect("store the fake token's hash");
    let generated = db.create_token("guard-random");
    let new = token::generate().expect("random bytes");
    shown.push_str(&format!("{new:?} {:?}\n", new.hash));
    shown.push_str(&format!("{:?}\n", db.token_row("guard-fixed")));
    shown.push_str(&format!("{:?}\n", Auth::new(Arc::new(db.backend()))));

    // A `Secret` passed to `tracing` as a Debug and a Display field.
    let secret = Secret::new(ACCESS_TOKEN.to_owned());
    tracing::info!(token = ?secret, shown = %secret, "guard canary: secret fields");

    // Auth path.
    let server = common::start(&db).await;
    let client = Client::new();
    assert_eq!(
        call(&client, &server, &format!("Bearer {ACCESS_TOKEN}")).await,
        StatusCode::OK
    );
    assert_eq!(
        call(&client, &server, &format!("Bearer {generated}")).await,
        StatusCode::OK
    );
    db.revoke_from_cli("guard-fixed");
    let basic = base64(format!("robin:{ACCESS_TOKEN}").as_bytes(), STANDARD);
    for header in [
        format!("Bearer {ACCESS_TOKEN}"),
        format!("Bearer {BOT_TOKEN}"),
        format!("Bot {BOT_TOKEN}"),
        format!("Bearer {LLM_KEY}"),
        format!("Basic {basic}"),
        format!("Bearer {ACCESS_TOKEN}0"),
        format!("Bearer  {ACCESS_TOKEN}"),
        format!("Bearer {}", &ACCESS_TOKEN[5..]),
    ] {
        assert_eq!(
            call(&client, &server, &header).await,
            StatusCode::UNAUTHORIZED,
            "a revoked, foreign or malformed credential must get 401"
        );
    }
    // Trip the failure limit so the lockout and refusal events are logged too.
    let mut locked = false;
    for _ in 0..10 {
        if call(&client, &server, &format!("Bearer {BOT_TOKEN}")).await
            == StatusCode::TOO_MANY_REQUESTS
        {
            locked = true;
            break;
        }
    }
    assert!(locked, "the failure limit never tripped");
    drop(server);
    drop(db);

    let logs = capture.text();
    // The capture saw TRACE-level output from this test, so an empty pass means nothing.
    assert!(
        logs.contains("guard canary: secret fields"),
        "canary missing"
    );
    assert!(
        logs.contains("token=*** shown=***"),
        "Secret fields not masked"
    );
    assert!(
        logs.contains("rejected request without a valid bearer token"),
        "auth rejection not captured"
    );
    assert!(logs.contains("client locked out"), "lockout not captured");
    assert_free_of_secrets(&logs, "the captured logs");
    assert_free_of_secrets(&shown, "Debug output of token and auth types");
    assert!(
        !logs.contains(&generated) && !logs.contains(&generated[5..]),
        "a generated token appeared in the captured logs"
    );
}

#[test]
fn secret_debug_and_display_print_only_stars() {
    #[derive(Debug)]
    #[expect(dead_code, reason = "only read through Debug")]
    struct Settings {
        bot: Secret<String>,
        key: Option<Secret<&'static str>>,
        tokens: Vec<Secret<String>>,
    }
    #[derive(Debug, thiserror::Error)]
    #[error("credential {0} was rejected")]
    struct Rejected(Secret<String>);

    for value in [BOT_TOKEN, LLM_KEY, ACCESS_TOKEN] {
        let secret = Secret::new(value.to_owned());
        for text in [
            format!("{secret}"),
            format!("{secret:?}"),
            format!("{secret:#?}"),
            format!("{secret:>80}"),
            format!("{secret:.200}"),
        ] {
            assert_eq!(text, "***");
        }
        assert_eq!(secret.expose(), value);

        let settings = Settings {
            bot: Secret::new(value.to_owned()),
            key: Some(Secret::new(LLM_KEY)),
            tokens: vec![Secret::new(value.to_owned())],
        };
        let error = Rejected(Secret::new(value.to_owned()));
        let result: Result<(), Secret<String>> = Err(Secret::new(value.to_owned()));
        for (place, text) in [
            ("struct Debug", format!("{settings:?}")),
            ("struct pretty Debug", format!("{settings:#?}")),
            ("error Display", error.to_string()),
            ("error Debug", format!("{error:?}")),
            ("Result Debug", format!("{result:?}")),
        ] {
            assert_free_of_secrets(&text, place);
            assert!(text.contains("***"), "{place} lost the mask");
        }
    }
}

#[test]
fn the_encodings_are_right() {
    // RFC 4648 test vectors, so a wrong encoder cannot make the guard pass vacuously.
    assert_eq!(base64(b"", STANDARD), "");
    assert_eq!(base64(b"f", STANDARD), "Zg==");
    assert_eq!(base64(b"fo", STANDARD), "Zm8=");
    assert_eq!(base64(b"foobar", STANDARD), "Zm9vYmFy");
    assert_eq!(base64(&[0xfb, 0xff], URL_SAFE), "-_8=");
    // An embedded value is found at every alignment.
    for prefix in ["", "u:", "user:"] {
        let embedded = base64(format!("{prefix}{LLM_KEY}").as_bytes(), STANDARD);
        assert!(
            forms(LLM_KEY)
                .iter()
                .any(|(form, value)| form.starts_with("base64+") && embedded.contains(value)),
            "{prefix:?}"
        );
    }
    let all = forms(LLM_KEY);
    let percent = &all.iter().find(|(f, _)| f == "percent").expect("percent").1;
    assert!(percent.contains("%2B") && percent.contains("%2F") && percent.contains("%3D"));
}
