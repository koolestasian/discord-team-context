//! Bearer-token middleware for the MCP endpoint (ADR 0006).
//!
//! Spike scope: one static token, compared in constant time. The real token store
//! (hashed per-person tokens, `last_used_at`, revocation, failure rate limit) arrives with the token store (M4).
//!
//! Challenge format: the MCP authorization spec (2026-07-28) says a protected server
//! answers `401` with a `WWW-Authenticate: Bearer ...` header and SHOULD add a
//! `resource_metadata` parameter when it runs the OAuth flow. This server uses static
//! bearer tokens and does not publish protected resource metadata (OAuth is v0.2), so the
//! challenge carries only `realm` and, for a rejected token, `error="invalid_token"`
//! (RFC 6750 section 3).

use std::{hint::black_box, sync::Arc};

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::secret::Secret;

const CHALLENGE_MISSING: &str = r#"Bearer realm="teamctx""#;
const CHALLENGE_INVALID: &str = r#"Bearer realm="teamctx", error="invalid_token""#;

/// The expected bearer token, shared by all requests.
#[derive(Clone, Debug)]
pub struct BearerAuth {
    expected: Arc<Secret<String>>,
}

impl BearerAuth {
    /// Accepts exactly one token.
    pub fn new(token: Secret<String>) -> Self {
        Self {
            expected: Arc::new(token),
        }
    }
}

/// Rejects the request with `401` unless it carries `Authorization: Bearer <token>`.
///
/// The scheme name is case-insensitive (RFC 9110 section 11.1). A missing header gets a
/// bare challenge; a malformed header or a wrong token gets `error="invalid_token"`.
pub async fn require_bearer(
    State(auth): State<BearerAuth>,
    request: Request,
    next: Next,
) -> Response {
    let Some(value) = request.headers().get(header::AUTHORIZATION) else {
        return unauthorized(CHALLENGE_MISSING);
    };
    match bearer_token(value) {
        Some(candidate) if constant_time_eq(candidate, auth.expected.expose().as_bytes()) => {
            next.run(request).await
        }
        _ => unauthorized(CHALLENGE_INVALID),
    }
}

/// Extracts the token from `Bearer <token>`: one space, non-empty, no further spaces.
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

/// Compares two byte strings without exiting at the first difference. The loop always
/// runs over the longer input, so timing does not reveal where a guess diverges.
/// (A length difference is not hidden; tokens have a fixed, public length.)
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= usize::from(x ^ y);
    }
    black_box(diff) == 0
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::{bearer_token, constant_time_eq};

    #[test]
    fn equal_and_unequal_inputs() {
        assert!(constant_time_eq(b"tctx_test_a", b"tctx_test_a"));
        assert!(!constant_time_eq(b"tctx_test_a", b"tctx_test_b"));
        assert!(!constant_time_eq(b"tctx_test_a", b"tctx_test_ab"));
        assert!(!constant_time_eq(b"", b"x"));
        assert!(constant_time_eq(b"", b""));
    }

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
