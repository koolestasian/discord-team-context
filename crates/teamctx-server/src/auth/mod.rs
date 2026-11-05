//! Bearer-token authentication for the HTTP interfaces (plan sections 10.1 and 13,
//! ADR 0006).
//!
//! Every protected request must carry `Authorization: Bearer tctx_...`. The middleware
//! hashes what was presented with SHA-256 and looks the digest up in `access_tokens`,
//! where only digests are stored and revoked rows never match. There is no cache, so a
//! revocation applies to the very next request (US-10).
//!
//! Timing and error shape:
//! - A malformed header, a token of the wrong shape, an unknown token and a revoked token
//!   all take the same path: the presented bytes are hashed and looked up, and only then
//!   are the shape check and the lookup result combined. They get byte-identical `401`
//!   responses with `error="invalid_token"`. A missing header gets a bare challenge
//!   (RFC 6750 section 3.1); it reveals nothing the caller does not already know.
//! - No secret is compared in this process. The comparison happens in SQLite's index on
//!   the SHA-256 digest, so its timing can at most reveal how many leading bytes of
//!   `SHA-256(guess)` match some stored digest. Turning that into a token needs a SHA-256
//!   preimage, so it reveals nothing usable; this is why digests, not tokens, are
//!   compared.
//!
//! Challenge format: the MCP authorization spec (2026-07-28) says a protected server
//! answers `401` with `WWW-Authenticate: Bearer ...` and SHOULD add `resource_metadata`
//! when it runs the OAuth flow. OAuth is v0.2, so the challenge carries only `realm` and,
//! for a rejected token, `error="invalid_token"`. The `401` body is empty.
//!
//! Repeated failures from one client are rate limited; see [`FailureLimiter`] for the
//! numbers. Scope is always `read` in v0.1, so a valid token grants every protected
//! route.

mod limiter;
pub mod token;

pub use limiter::FailureLimiter;

use std::{
    fmt,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use axum::{
    extract::{ConnectInfo, Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use teamctx_core::{
    id::TokenId,
    repo::{AccessToken, RepoError, RepoResult, TokenHash, TokenRead, TokenWrite},
    time::UnixMillis,
};
use teamctx_store::{Store, WriterClient};

use self::limiter::ClientKey;

const CHALLENGE_MISSING: &str = r#"Bearer realm="teamctx""#;
const CHALLENGE_INVALID: &str = r#"Bearer realm="teamctx", error="invalid_token""#;

/// What the middleware needs from storage. Methods block; the middleware calls them on
/// the blocking thread pool.
pub trait TokenBackend: Send + Sync + 'static {
    /// The non-revoked token with this digest, if any.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] when storage fails.
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>>;

    /// Records a successful use.
    ///
    /// # Errors
    ///
    /// [`RepoError`] when the row is gone or storage fails.
    fn touch_token(&self, id: TokenId, at: UnixMillis) -> RepoResult<()>;
}

/// The production backend: a read-only connection for lookups and the writer for
/// `last_used_at`.
pub struct StoreTokens {
    reader: Mutex<Store>,
    writer: WriterClient,
}

impl StoreTokens {
    /// Uses `reader` (open it with [`Store::open_read_only`]) for lookups and `writer`
    /// for updates.
    #[must_use]
    pub fn new(reader: Store, writer: WriterClient) -> Self {
        Self {
            reader: Mutex::new(reader),
            writer,
        }
    }
}

impl fmt::Debug for StoreTokens {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoreTokens").finish_non_exhaustive()
    }
}

impl TokenBackend for StoreTokens {
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>> {
        let reader = self
            .reader
            .lock()
            .map_err(|_| RepoError::Backend("token reader lock poisoned".into()))?;
        reader.active_token(hash)
    }

    fn touch_token(&self, id: TokenId, at: UnixMillis) -> RepoResult<()> {
        self.writer.clone().touch_token(id, at)
    }
}

/// Shared middleware state: the token backend and the failure limiter.
#[derive(Clone)]
pub struct Auth {
    backend: Arc<dyn TokenBackend>,
    limiter: Arc<FailureLimiter>,
}

impl Auth {
    /// Authenticates against `backend` with the default failure limit.
    #[must_use]
    pub fn new(backend: Arc<dyn TokenBackend>) -> Self {
        Self::with_limiter(backend, FailureLimiter::default())
    }

    /// Authenticates against `backend` with a custom failure limit.
    #[must_use]
    pub fn with_limiter(backend: Arc<dyn TokenBackend>, limiter: FailureLimiter) -> Self {
        Self {
            backend,
            limiter: Arc::new(limiter),
        }
    }
}

impl fmt::Debug for Auth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Auth")
            .field("limiter", &self.limiter)
            .finish_non_exhaustive()
    }
}

/// The caller of an authenticated request, added to the request extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    /// The token's row id.
    pub token: TokenId,
    /// The token's name, for example `"robin-laptop"`.
    pub name: String,
}

enum Verdict {
    Allowed(AccessToken),
    Missing,
    Invalid,
    Unavailable,
}

/// Lets the request through only with a valid, non-revoked token.
///
/// Responses: `429` while the client is locked out by the failure limit; `401` with a
/// `WWW-Authenticate: Bearer` challenge for a missing, malformed, unknown or revoked
/// token; `503` when storage fails (not counted as an auth failure).
pub async fn require_bearer(
    State(auth): State<Auth>,
    mut request: Request,
    next: Next,
) -> Response {
    let now = Instant::now();
    let client = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map_or(ClientKey::Unknown, |info| ClientKey::from_ip(info.0.ip()));
    if let Some(wait) = auth.limiter.blocked(client, now) {
        tracing::debug!(%client, "refused request from a client over the auth failure limit");
        return too_many_requests(wait.as_secs().saturating_add(1));
    }

    let verdict = match request.headers().get(header::AUTHORIZATION) {
        None => Verdict::Missing,
        Some(value) => verify(&auth, value).await,
    };
    match verdict {
        Verdict::Allowed(token) => {
            request.extensions_mut().insert(Caller {
                token: token.id,
                name: token.name,
            });
            next.run(request).await
        }
        Verdict::Unavailable => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Verdict::Missing | Verdict::Invalid => {
            let missing = matches!(verdict, Verdict::Missing);
            tracing::debug!(%client, missing, "rejected request without a valid bearer token");
            if auth.limiter.record_failure(client, now) {
                tracing::warn!(%client, "too many failed authentications; client locked out");
            }
            unauthorized(if missing {
                CHALLENGE_MISSING
            } else {
                CHALLENGE_INVALID
            })
        }
    }
}

/// Hashes and looks up whatever was presented, then decides. Every presented header,
/// well-formed or not, costs exactly one lookup.
async fn verify(auth: &Auth, value: &HeaderValue) -> Verdict {
    let candidate = bearer_token(value);
    let well_formed = candidate.is_some_and(token::is_well_formed);
    let hash = token::hash(candidate.unwrap_or_default());
    let backend = Arc::clone(&auth.backend);
    let lookup = tokio::task::spawn_blocking(move || {
        let found = backend.active_token(&hash)?.filter(|_| well_formed);
        let touched = found
            .as_ref()
            .map(|token| backend.touch_token(token.id, now_millis()));
        Ok::<_, RepoError>((found, touched))
    })
    .await;
    match lookup {
        Ok(Ok((Some(token), touched))) => {
            if let Some(Err(error)) = touched {
                // Bookkeeping only: the token is valid, so the request proceeds.
                tracing::warn!(%error, "could not record token use");
            }
            Verdict::Allowed(token)
        }
        Ok(Ok((None, _))) => Verdict::Invalid,
        Ok(Err(error)) => {
            // RepoError's Display is generic and never includes row data.
            tracing::error!(%error, "token lookup failed");
            Verdict::Unavailable
        }
        Err(_) => {
            tracing::error!("token lookup task failed");
            Verdict::Unavailable
        }
    }
}

fn now_millis() -> UnixMillis {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        });
    UnixMillis::from_millis(millis)
}

/// Extracts the token from `Bearer <token>`: one space, non-empty, no further spaces.
/// The scheme name is case-insensitive (RFC 9110 section 11.1).
fn bearer_token(value: &HeaderValue) -> Option<&[u8]> {
    let bytes = value.as_bytes();
    let (scheme, rest) = bytes.split_at_checked(7)?;
    if !scheme.eq_ignore_ascii_case(b"Bearer ") {
        return None;
    }
    if rest.is_empty() || rest.contains(&b' ') {
        return None;
    }
    Some(rest)
}

fn unauthorized(challenge: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge)],
    )
        .into_response()
}

fn too_many_requests(retry_after_secs: u64) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        [(header::RETRY_AFTER, retry_after_secs.to_string())],
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::bearer_token;

    #[test]
    fn parses_only_well_formed_bearer_values() {
        let ok = |s: &'static str| bearer_token(&HeaderValue::from_static(s)).map(<[u8]>::to_vec);
        let expected = Some(b"tctx_test_a".to_vec());
        assert_eq!(ok("Bearer tctx_test_a"), expected);
        assert_eq!(ok("bearer tctx_test_a"), expected);
        assert_eq!(ok("BEARER tctx_test_a"), expected);
        assert_eq!(ok("Bearer "), None);
        assert_eq!(ok("Bearer"), None);
        assert_eq!(ok("Bearer a b"), None);
        assert_eq!(ok("Basic dXNlcjpwdw=="), None);
        assert_eq!(ok("Bearertctx_test_a"), None);
        assert_eq!(ok(""), None);
    }
}
