//! Bearer-token authentication for the HTTP interfaces (plan sections 10.1 and 13,
//! ADR 0006).
//!
//! Every protected request must carry `Authorization: Bearer tctx_...`. The middleware
//! hashes what was presented with SHA-256 and looks the digest up in `access_tokens`,
//! where only digests are stored and revoked rows never match. There is no cache, so a
//! revocation applies to the very next request (US-10). A request is authorised only
//! when recording the token's use succeeds, and that write itself checks `revoked_at`,
//! so a revocation that commits between the lookup and the authorisation still wins.
//! Any storage failure rejects the request (fail closed).
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

use self::limiter::{Attempt, ClientKey};

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

    /// Records a use of the token, but only if it is still not revoked, in one atomic
    /// step (see [`TokenWrite::touch_token`]). The middleware authorises a request only
    /// when this succeeds.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] when the token is revoked or gone (the request gets
    /// `401`); any other [`RepoError`] when storage fails (`503`).
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
/// Responses: `429` while the client is locked out by the failure limit, or while its
/// attempts in flight use up what is left of it; `401` with a `WWW-Authenticate: Bearer`
/// challenge for a missing, malformed, unknown or revoked token; `503` when storage fails
/// (not counted as an auth failure).
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
    // Reserving before the lookup, not counting after it, is what stops a concurrent
    // burst from getting more guesses checked than the limit allows.
    let attempt = match auth.limiter.admit(client, now) {
        Ok(attempt) => attempt,
        Err(wait) => {
            tracing::debug!(%client, "refused request from a client over the auth failure limit");
            return too_many_requests(wait.as_secs().saturating_add(1));
        }
    };

    let verdict = match request.headers().get(header::AUTHORIZATION) {
        None => {
            count_failure(attempt, now);
            Verdict::Missing
        }
        Some(value) => verify(&auth, value, attempt, now).await,
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
            unauthorized(if missing {
                CHALLENGE_MISSING
            } else {
                CHALLENGE_INVALID
            })
        }
    }
}

fn count_failure(attempt: Attempt, now: Instant) {
    let client = attempt.client();
    if attempt.fail(now) {
        tracing::warn!(%client, "too many failed authentications; client locked out");
    }
}

/// Hashes and looks up whatever was presented, then decides. Every presented header,
/// well-formed or not, costs exactly one lookup.
///
/// The attempt is settled inside the blocking task, so it stays reserved until the
/// lookup has finished even if the client disconnects and this future is dropped.
async fn verify(auth: &Auth, value: &HeaderValue, attempt: Attempt, now: Instant) -> Verdict {
    let candidate = bearer_token(value);
    let well_formed = candidate.is_some_and(token::is_well_formed);
    let hash = token::hash(candidate.unwrap_or_default());
    let backend = Arc::clone(&auth.backend);
    let checked = tokio::task::spawn_blocking(move || {
        let verdict = check(backend.as_ref(), &hash, well_formed);
        if matches!(verdict, Verdict::Invalid) {
            count_failure(attempt, now);
        }
        // Otherwise (allowed, or storage failed) dropping the attempt refunds it.
        verdict
    })
    .await;
    checked.unwrap_or_else(|_| {
        tracing::error!("token lookup task failed");
        Verdict::Unavailable
    })
}

/// Looks the digest up, then records the use. Recording re-checks revocation in the
/// same statement that writes, and only its success authorises the request: a token
/// revoked after the lookup is rejected, and a failed write never lets a request
/// through. `RepoError`'s Display is generic and never includes row data.
fn check(backend: &dyn TokenBackend, hash: &TokenHash, well_formed: bool) -> Verdict {
    let token = match backend.active_token(hash) {
        Ok(Some(token)) if well_formed => token,
        Ok(_) => return Verdict::Invalid,
        Err(error) => {
            tracing::error!(%error, "token lookup failed");
            return Verdict::Unavailable;
        }
    };
    match backend.touch_token(token.id, now_millis()) {
        Ok(()) => Verdict::Allowed(token),
        // Revoked (or gone) since the lookup: the revocation wins.
        Err(RepoError::NotFound { .. }) => Verdict::Invalid,
        Err(error) => {
            tracing::error!(%error, "could not record token use");
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
