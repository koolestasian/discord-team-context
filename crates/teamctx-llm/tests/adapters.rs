//! Both adapters against a local mock HTTP server: request shape, success, errors,
//! timeouts, malformed and oversized responses, redirects, and key safety. No real API
//! is contacted and every key here is fictional.

#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

mod common;

use std::time::Duration;

use common::{Behaviour, MockServer};
use serde_json::json;
use teamctx_llm::{
    AnthropicConfig, AnthropicMessages, ApiKey, CompletionRequest, LlmError, LlmProvider,
    OpenAiCompatible, OpenAiConfig,
};

const KEY: &str = "sk-fictional-key-0123456789";

fn request() -> CompletionRequest {
    CompletionRequest {
        system: "system rules".to_owned(),
        user: "user question".to_owned(),
        max_output_tokens: 321,
    }
}

fn key() -> ApiKey {
    ApiKey::new(KEY).unwrap()
}

fn anthropic(base_url: &str, timeout: Duration) -> AnthropicMessages {
    let mut config = AnthropicConfig::new(base_url, "claude-fictional", key());
    config.timeout = timeout;
    AnthropicMessages::new(config).unwrap()
}

fn openai(base_url: &str, timeout: Duration) -> OpenAiCompatible {
    let mut config = OpenAiConfig::new(format!("{base_url}/v1"), "local-fictional", Some(key()));
    config.timeout = timeout;
    OpenAiCompatible::new(config).unwrap()
}

const ANTHROPIC_OK: &str = r#"{"id":"msg_1","type":"message","role":"assistant","content":[{"type":"text","text":"Serif [D-1]"}],"stop_reason":"end_turn","usage":{"input_tokens":11,"output_tokens":3}}"#;
const OPENAI_OK: &str = r#"{"id":"c1","choices":[{"index":0,"message":{"role":"assistant","content":"Serif [D-1]"},"finish_reason":"stop"}],"usage":{"prompt_tokens":11,"completion_tokens":3,"total_tokens":14}}"#;

// ---- request shape and success ----

#[tokio::test]
async fn anthropic_sends_the_messages_api_request() {
    let server = MockServer::start(Behaviour::ok(ANTHROPIC_OK));
    let provider = anthropic(&server.url(), Duration::from_secs(5));
    let completion = provider.complete(request()).await.unwrap();

    assert_eq!(completion.text, "Serif [D-1]");
    assert_eq!(completion.usage.input, 11);
    assert_eq!(completion.usage.output, 3);
    assert!(!completion.usage.estimated);
    assert_eq!(provider.provider_name(), "anthropic");
    assert_eq!(provider.model(), "claude-fictional");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/v1/messages");
    assert_eq!(sent.header("x-api-key"), Some(KEY));
    assert_eq!(sent.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(sent.header("content-type"), Some("application/json"));
    assert_eq!(
        sent.json(),
        json!({
            "model": "claude-fictional",
            "max_tokens": 321,
            "system": "system rules",
            "messages": [{"role": "user", "content": "user question"}],
        })
    );
}

#[tokio::test]
async fn openai_compatible_sends_the_chat_completions_request() {
    let server = MockServer::start(Behaviour::ok(OPENAI_OK));
    let provider = openai(&server.url(), Duration::from_secs(5));
    let completion = provider.complete(request()).await.unwrap();

    assert_eq!(completion.text, "Serif [D-1]");
    assert_eq!(completion.usage.total(), 14);
    assert_eq!(provider.provider_name(), "openai_compatible");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let sent = &requests[0];
    assert_eq!(sent.method, "POST");
    assert_eq!(sent.path, "/v1/chat/completions");
    assert_eq!(
        sent.header("authorization"),
        Some(format!("Bearer {KEY}").as_str())
    );
    assert_eq!(
        sent.json(),
        json!({
            "model": "local-fictional",
            "max_tokens": 321,
            "stream": false,
            "messages": [
                {"role": "system", "content": "system rules"},
                {"role": "user", "content": "user question"},
            ],
        })
    );
}

#[tokio::test]
async fn openai_compatible_works_without_a_key_for_local_servers() {
    let server = MockServer::start(Behaviour::ok(
        r#"{"choices":[{"message":{"content":"local answer [m:1]"}}]}"#,
    ));
    let config = OpenAiConfig::new(format!("{}/v1/", server.url()), "llama-fictional", None);
    let provider = OpenAiCompatible::new(config).unwrap();
    let completion = provider.complete(request()).await.unwrap();

    assert_eq!(completion.text, "local answer [m:1]");
    // No `usage` in the reply: an estimate keeps budgets working.
    assert!(completion.usage.estimated);
    assert!(completion.usage.total() > 0);
    let sent = &server.requests()[0];
    assert_eq!(sent.path, "/v1/chat/completions");
    assert_eq!(sent.header("authorization"), None);
}

#[tokio::test]
async fn requests_never_carry_tools() {
    for body in [ANTHROPIC_OK, OPENAI_OK] {
        let server = MockServer::start(Behaviour::ok(body));
        let sent = if body == ANTHROPIC_OK {
            anthropic(&server.url(), Duration::from_secs(5))
                .complete(request())
                .await
                .unwrap();
            server.requests().remove(0).json()
        } else {
            openai(&server.url(), Duration::from_secs(5))
                .complete(request())
                .await
                .unwrap();
            server.requests().remove(0).json()
        };
        let object = sent.as_object().unwrap();
        for forbidden in ["tools", "tool_choice", "functions", "function_call"] {
            assert!(!object.contains_key(forbidden), "{forbidden}");
        }
    }
}

// ---- failures, for both adapters ----

/// Runs the failure scenarios against any adapter. `make` builds the adapter for a
/// server URL and a timeout.
async fn failure_scenarios<P: LlmProvider>(make: impl Fn(&str, Duration) -> P) {
    // A non-success status becomes Http{status}; the body is discarded, even when it
    // contains the key or other secrets.
    for status in [400_u16, 401, 429, 500, 503] {
        let leak = format!("{{\"error\":\"bad key {KEY} and private chat text\"}}");
        let server = MockServer::start(Behaviour::status(status, leak));
        let error = make(&server.url(), Duration::from_secs(5))
            .complete(request())
            .await
            .unwrap_err();
        assert_eq!(error, LlmError::Http { status });
        assert_no_leak(&error);
    }

    // The server never answers.
    let server = MockServer::start(Behaviour::Hang);
    let error = make(&server.url(), Duration::from_millis(250))
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(error, LlmError::Timeout);

    // Nothing is listening.
    let unused = MockServer::start(Behaviour::Hang);
    let url = unused.url();
    drop(unused);
    let error = make(&url, Duration::from_secs(5))
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(error, LlmError::Connect);

    // Malformed answers: not JSON, wrong shape, empty text.
    for body in [
        "not json at all",
        "{}",
        "[]",
        r#"{"content":[],"choices":[]}"#,
        r#"{"content":[{"type":"text","text":""}],"choices":[{"message":{"content":""}}],"usage":{"input_tokens":1,"output_tokens":1}}"#,
    ] {
        let server = MockServer::start(Behaviour::ok(body));
        let error = make(&server.url(), Duration::from_secs(5))
            .complete(request())
            .await
            .unwrap_err();
        assert!(
            matches!(error, LlmError::MalformedResponse(_)),
            "{body}: {error:?}"
        );
        assert!(!error.to_string().contains("not json"));
    }

    // An answer above the size limit.
    let huge = format!(r#"{{"pad":"{}"}}"#, "x".repeat(2 * 1024 * 1024));
    let server = MockServer::start(Behaviour::ok(huge));
    let error = make(&server.url(), Duration::from_secs(5))
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(error, LlmError::ResponseTooLarge);

    // A redirect is not followed, so a key in a header cannot reach another host.
    let elsewhere = MockServer::start(Behaviour::ok(ANTHROPIC_OK));
    let server = MockServer::start(Behaviour::Redirect {
        location: format!("{}/v1/messages", elsewhere.url()),
    });
    let error = make(&server.url(), Duration::from_secs(5))
        .complete(request())
        .await
        .unwrap_err();
    assert_eq!(error, LlmError::Http { status: 302 });
    assert!(elsewhere.requests().is_empty());
}

fn assert_no_leak(error: &LlmError) {
    let shown = format!("{error} {error:?}");
    assert!(!shown.contains(KEY), "{shown}");
    assert!(!shown.contains("private chat text"), "{shown}");
    assert!(!shown.contains("127.0.0.1"), "{shown}");
}

#[tokio::test]
async fn anthropic_handles_errors_timeouts_and_bad_responses() {
    failure_scenarios(anthropic).await;
}

#[tokio::test]
async fn openai_compatible_handles_errors_timeouts_and_bad_responses() {
    failure_scenarios(openai).await;
}

// ---- keys ----

#[test]
fn keys_never_appear_in_debug_output() {
    let anthropic_config = AnthropicConfig::new("https://api.example.com", "m", key());
    let openai_config = OpenAiConfig::new("https://api.example.com/v1", "m", Some(key()));
    let adapters = [
        format!("{anthropic_config:?}"),
        format!("{openai_config:?}"),
        format!(
            "{:?}",
            AnthropicMessages::new(anthropic_config.clone()).unwrap()
        ),
        format!(
            "{:?}",
            OpenAiCompatible::new(openai_config.clone()).unwrap()
        ),
        format!("{:#?}", key()),
    ];
    for shown in adapters {
        assert!(!shown.contains(KEY), "{shown}");
        assert!(!shown.contains("0123456789"), "{shown}");
    }
}

#[test]
fn a_missing_key_never_echoes_the_variable_name() {
    let error = ApiKey::from_env("TEAMCTX_TEST_SURELY_UNSET_VARIABLE").unwrap_err();
    assert_eq!(
        error.to_string(),
        "the environment variable named in llm.api_key_env is not set or is empty"
    );
    assert!(
        !error
            .to_string()
            .contains("TEAMCTX_TEST_SURELY_UNSET_VARIABLE")
    );
}

#[test]
fn unsafe_endpoints_are_refused_at_construction() {
    let cases = [
        // A key must not cross a network in clear text.
        "http://api.example.com",
        // User info would end up in logs and errors.
        "https://user:hunter2@api.example.com",
        "ftp://api.example.com",
        "not a url",
    ];
    for base in cases {
        let error = AnthropicMessages::new(AnthropicConfig::new(base, "m", key())).unwrap_err();
        assert!(matches!(error, LlmError::InvalidConfig(_)), "{base}");
        assert!(!error.to_string().contains("hunter2"));
        assert!(OpenAiCompatible::new(OpenAiConfig::new(base, "m", Some(key()))).is_err());
    }
    // An empty model name is a configuration error, not a provider error.
    assert!(
        AnthropicMessages::new(AnthropicConfig::new("https://api.example.com", " ", key()))
            .is_err()
    );
    // A local server without a key may use plain http on a LAN address.
    assert!(
        OpenAiCompatible::new(OpenAiConfig::new("http://192.168.1.20:11434/v1", "m", None)).is_ok()
    );
}
