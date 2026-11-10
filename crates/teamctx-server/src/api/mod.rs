//! The JSON API under `/api/v1` (plan section 10.2) and `/healthz`.
//!
//! The API is a second front end to the MCP retrieval layer, not a second implementation:
//! each route runs the same function from [`crate::mcp::retrieve`] on the same read
//! connection, and builds its answer with [`crate::mcp::view::respond`]. So the entries,
//! ids, ordering, cursors, size limit and untrusted-content framing are the ones the MCP
//! tools return, and `teamctx q` (plan section 10.4) shows exactly what an agent gets over
//! MCP.
//!
//! Response body (`200`): the MCP `structuredContent` (`notice`, `items`, `truncated`,
//! `next_cursor`, `next_step`) plus `text`, the framed Markdown an MCP client receives as
//! its text content. Chat text appears only inside `text`, in the one delimited block.
//! Failures are `{"error": "..."}` with a fixed message: `400` for bad parameters, `500`
//! when the record cannot be read (the cause is logged, never sent).
//!
//! Routes (all `GET`, all behind the bearer middleware except `/healthz`):
//!
//! | Route | Parameters | MCP equivalent |
//! |---|---|---|
//! | `/api/v1/context` | `text`, `limit?` | `context_for` |
//! | `/api/v1/whats-new` | `since` | `whats_new` |
//! | `/api/v1/record` | `kind?`, `status?`, `q?`, `owner?`, `since?`, `cursor?` | `record` (`q` is `topic`) |
//! | `/api/v1/search` | `q`, `cursor?` | `search` |
//! | `/api/v1/read` | `id`, `before?`, `after?` | `read` |
//! | `/api/v1/status` | none | none |
//!
//! Unknown or repeated parameters are refused rather than ignored, so a typo cannot
//! silently widen a query.

mod params;

use std::sync::Arc;

use axum::{
    Router,
    extract::{RawQuery, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Map, Value, json};
use teamctx_core::repo::{EntryFilter, PageRequest, RecordRead};

use self::params::{ParamError, Params};
use crate::mcp::{
    McpState,
    input::{self, SINCE_HELP},
    retrieve::{self, Ctx, Done, Failure},
    view::respond,
};

type Shared = Arc<McpState>;

/// Work for the blocking pool, with its arguments already checked.
type Job = Box<dyn FnOnce(&mut Ctx<'_>) -> Done + Send>;

/// Turns a request's query string into a [`Job`], or a `400` message.
type Prepare = fn(&McpState, Option<&str>) -> Result<Job, String>;

const READ_FAILED: &str = "The record could not be read right now.";

/// The API routes, with the shared read state. The caller adds the auth layer.
pub(crate) fn routes(state: Shared) -> Router {
    Router::new()
        .route("/api/v1/context", get(context))
        .route("/api/v1/whats-new", get(whats_new))
        .route("/api/v1/record", get(record))
        .route("/api/v1/search", get(search))
        .route("/api/v1/read", get(read))
        .route("/api/v1/status", get(status))
        .with_state(state)
}

/// The unauthenticated liveness route. It touches no data and says nothing about it.
pub(crate) fn health() -> Router {
    Router::new().route("/healthz", get(healthz))
}

async fn healthz() -> Response {
    json_response(StatusCode::OK, &json!({ "status": "ok" }))
}

async fn context(State(state): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    run(state, raw, |_, raw| {
        let params = Params::parse(raw, &["text", "limit"]).map_err(ParamError::message)?;
        let text = params
            .required("text")
            .map_err(ParamError::message)?
            .to_owned();
        let limit = params.number("limit").map_err(ParamError::message)?;
        Ok(Box::new(move |ctx| {
            retrieve::context_for(ctx, &text, limit)
        }))
    })
    .await
}

async fn whats_new(State(state): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    run(state, raw, |state, raw| {
        let params = Params::parse(raw, &["since"]).map_err(ParamError::message)?;
        let since = since_of(
            state,
            params.required("since").map_err(ParamError::message)?,
        )?;
        Ok(Box::new(move |ctx| retrieve::whats_new(ctx, since)))
    })
    .await
}

async fn record(State(state): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    run(state, raw, |state, raw| {
        let params = Params::parse(raw, &["kind", "status", "q", "owner", "since", "cursor"])
            .map_err(ParamError::message)?;
        let since = params
            .get("since")
            .map(|text| since_of(state, text))
            .transpose()?;
        let own = |name: &str| params.get(name).map(str::to_owned);
        let (kind, status, topic, owner, cursor) = (
            own("kind"),
            own("status"),
            own("q"),
            own("owner"),
            own("cursor"),
        );
        Ok(Box::new(move |ctx| {
            retrieve::record(
                ctx,
                &retrieve::RecordArgs {
                    kind: kind.as_deref(),
                    status: status.as_deref(),
                    topic: topic.as_deref(),
                    owner: owner.as_deref(),
                    since,
                    cursor: cursor.as_deref(),
                },
            )
        }))
    })
    .await
}

async fn search(State(state): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    run(state, raw, |_, raw| {
        let params = Params::parse(raw, &["q", "cursor"]).map_err(ParamError::message)?;
        let query = params
            .required("q")
            .map_err(ParamError::message)?
            .to_owned();
        let cursor = params.get("cursor").map(str::to_owned);
        Ok(Box::new(move |ctx| {
            retrieve::search(ctx, &query, cursor.as_deref())
        }))
    })
    .await
}

async fn read(State(state): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    run(state, raw, |_, raw| {
        let params = Params::parse(raw, &["id", "before", "after"]).map_err(ParamError::message)?;
        let id = params
            .required("id")
            .map_err(ParamError::message)?
            .to_owned();
        let before = params.number("before").map_err(ParamError::message)?;
        let after = params.number("after").map_err(ParamError::message)?;
        Ok(Box::new(move |ctx| retrieve::read(ctx, &id, before, after)))
    })
    .await
}

/// A cheap read proves the record is reachable. When the host runs a connector, `sync`
/// adds its state and each stored channel's cursor ([`crate::status`]). LLM usage and
/// tokens belong to the web view's status screen and are not reported yet.
async fn status(State(state): State<Shared>, RawQuery(raw): RawQuery) -> Response {
    if let Err(error) = Params::parse(raw.as_deref(), &[]) {
        return error_response(StatusCode::BAD_REQUEST, &error.message());
    }
    let host = state.status.as_ref().map(|provider| provider());
    let joined = tokio::task::spawn_blocking(move || {
        let store = state.store.lock().map_err(|_| ())?;
        let read = store
            .list_entries(
                &EntryFilter::default(),
                PageRequest {
                    offset: 0,
                    limit: 1,
                },
            )
            .and_then(|_| {
                host.map(|host| crate::status::sync_json(&host, &*store))
                    .transpose()
            });
        read.map_err(|error| {
            tracing::error!(%error, "API status could not read the record");
        })
    })
    .await;
    match joined {
        Ok(Ok(sync)) => {
            let mut body = json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
                "record": "readable",
            });
            if let (Some(sync), Some(object)) = (sync, body.as_object_mut()) {
                object.insert("sync".to_owned(), sync);
            }
            json_response(StatusCode::OK, &body)
        }
        _ => error_response(StatusCode::SERVICE_UNAVAILABLE, READ_FAILED),
    }
}

/// Resolves a `since` value like the MCP tools do.
fn since_of(state: &McpState, text: &str) -> Result<teamctx_core::time::UnixMillis, String> {
    input::parse_since(text, (state.clock)(), state.utc_offset_seconds)
        .ok_or_else(|| SINCE_HELP.to_owned())
}

/// Checks the request with `prepare`, runs the job on the blocking pool and shapes the
/// result. Storage errors are logged and answered with a fixed message, as in MCP.
async fn run(state: Shared, raw: Option<String>, prepare: Prepare) -> Response {
    let job = match prepare(&state, raw.as_deref()) {
        Ok(job) => job,
        Err(message) => return error_response(StatusCode::BAD_REQUEST, &message),
    };
    let joined = tokio::task::spawn_blocking(move || {
        let store = state.store.lock().map_err(|_| ())?;
        let mut ctx = Ctx::new(&store, (state.clock)(), state.utc_offset_seconds);
        Ok::<_, ()>(job(&mut ctx))
    })
    .await;
    match joined {
        Ok(Ok(Ok((items, summary)))) => match success_body(&respond(items, &summary)) {
            Some(body) => json_response(StatusCode::OK, &body),
            None => {
                tracing::error!("API result could not be shaped");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, READ_FAILED)
            }
        },
        Ok(Ok(Err(Failure::Rejected(message)))) => error_response(StatusCode::BAD_REQUEST, message),
        Ok(Ok(Err(Failure::Repo(error)))) => {
            tracing::error!(%error, "API could not read the record");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, READ_FAILED)
        }
        Ok(Err(())) | Err(_) => {
            tracing::error!("API worker failed");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, READ_FAILED)
        }
    }
}

/// The MCP result as an API body: its `structuredContent` plus the framed `text`.
fn success_body(result: &rmcp::model::CallToolResult) -> Option<Value> {
    let mut value = serde_json::to_value(result).ok()?;
    let text = value.pointer("/content/0/text")?.as_str()?.to_owned();
    let Value::Object(mut body) = value.get_mut("structuredContent")?.take() else {
        return None;
    };
    body.insert("text".to_owned(), Value::String(text));
    Some(Value::Object(body))
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let mut body = Map::new();
    body.insert("error".to_owned(), Value::String(message.to_owned()));
    json_response(status, &Value::Object(body))
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let Ok(text) = serde_json::to_string(body) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            // Results are private to the token holder and change as the record does.
            (header::CACHE_CONTROL, "no-store"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        text,
    )
        .into_response()
}
