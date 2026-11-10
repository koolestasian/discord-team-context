//! Injected `/ask` backend. The binary owns the LLM pipeline; the server has no
//! dependency on the LLM or Discord crates (plan section 5.1).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use axum::{
    Extension, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::Response,
    routing::post,
};
use serde::Deserialize;
use serde_json::{Value, json};
use teamctx_core::id::TokenId;

use crate::api::{error_response, json_response};
use crate::auth::Caller;

/// A validated answer, already framed as untrusted data by the host.
pub type AskResult = Result<Value, AskFailure>;

/// Fixed failures: no provider text, question or credentials.
#[derive(Debug, Clone, Copy)]
pub enum AskFailure {
    /// Invalid or oversized question.
    InvalidQuestion,
    /// An hourly or daily budget is exhausted.
    Budget,
    /// All model request slots are occupied.
    Busy,
    /// The provider failed or timed out.
    Provider,
    /// The host could not read or account for the request.
    Host,
}

impl AskFailure {
    fn response(self) -> Response {
        let (status, message) = match self {
            Self::InvalidQuestion => (
                StatusCode::BAD_REQUEST,
                "Ask a question of at most 500 characters.",
            ),
            Self::Budget => (
                StatusCode::TOO_MANY_REQUESTS,
                "The model usage limit has been reached. Please try again later.",
            ),
            Self::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "The model is busy. Please try again later.",
            ),
            Self::Provider => (
                StatusCode::BAD_GATEWAY,
                "The language model could not answer right now.",
            ),
            Self::Host => (
                StatusCode::SERVICE_UNAVAILABLE,
                "The host could not answer right now.",
            ),
        };
        error_response(status, message)
    }
}

/// A single asynchronous answer.
pub type AskFuture<'a> = Pin<Box<dyn Future<Output = AskResult> + Send + 'a>>;

/// The host charges each request to the authenticated token. Tokens have no member
/// association in v0.1 and can read the whole included record, just as over MCP.
pub trait AskBackend: Send + Sync + 'static {
    /// Answers without returning raw provider failures or unframed model text.
    fn ask(&self, token: TokenId, question: String) -> AskFuture<'_>;
}

/// Adds an authenticated endpoint even when disabled, so stale clients get a clear
/// fixed response. The caller wraps these routes in bearer authentication.
pub(crate) fn routes(backend: Option<Arc<dyn AskBackend>>) -> Router {
    Router::new()
        .route("/api/v1/ask", post(ask))
        .layer(DefaultBodyLimit::max(4096))
        .with_state(backend)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    question: String,
}

async fn ask(
    State(backend): State<Option<Arc<dyn AskBackend>>>,
    Extension(caller): Extension<Caller>,
    headers: HeaderMap,
    body: Result<Bytes, axum::extract::rejection::BytesRejection>,
) -> Response {
    let Some(backend) = backend else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "/ask is not enabled on this host.",
        );
    };
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(';')
                .next()
                .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"))
        })
    {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Send application/json with a question field.",
        );
    }
    let Ok(body) = body else {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "The question body is too large.",
        );
    };
    let Ok(input) = serde_json::from_slice::<Input>(&body) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "Send a JSON object with one question field.",
        );
    };
    if input.question.trim().is_empty() || input.question.chars().count() > 500 {
        return AskFailure::InvalidQuestion.response();
    }
    match backend.ask(caller.token, input.question).await {
        Ok(body) => json_response(StatusCode::OK, &body),
        Err(error) => error.response(),
    }
}

/// Shapes a result without putting any model text outside its untrusted-data frame.
#[must_use]
pub fn answer(text: &str, found: bool, citations: &[Value]) -> Value {
    json!({
        "notice": teamctx_core::framing::NOTICE,
        "found": found,
        "text": teamctx_core::framing::frame(text),
        "citations": citations,
    })
}
