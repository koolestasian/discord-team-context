# 0006. MCP over HTTP with bearer tokens (OAuth later)

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D6, §10.1, §10.2, §13, §18 (M1, M4)

## Context

Teammates' AI coding agents need to read the team record from their own machines,
while the server runs on one host (ADR 0004). The agents the plan targets are Claude
Code, Codex and Cursor (§4). The MCP specification version 2026-07-28 uses streamable
HTTP, is stateless, and makes authorization optional. The `rmcp` crate at 3.8.0
supports that version, confirmed 2026-11-04 (§10.1).

Access must be revocable per person (US-10), and a leaked token should be detectable
by secret scanners (§13).

## Decision

Serve MCP at `/mcp` over streamable HTTP using `rmcp`, behind axum middleware that
checks `Authorization: Bearer tctx_...` before `rmcp` sees the request. A missing or
invalid token gets `401` with `WWW-Authenticate: Bearer`, and repeated failures are
rate limited.

Tokens are personal: one per teammate, created with `teamctx token create`. Only the
SHA-256 of the token is stored; the prefix `tctx_` is chosen for secret scanners; the
`last_used_at` time is visible; `teamctx token revoke <name>` makes the next request
return 401. The web view exchanges a token once for an `HttpOnly; SameSite=Strict`
cookie that maps to the token's row (§10.2). A `teamctx mcp --stdio` mode serves
same-machine use and clients without header support.

OAuth 2.1 for MCP is v0.2 (plan non-goals, §18 after v0.1).

Validated by spike S3 in M1: `rmcp` streamable HTTP behind bearer middleware, connected
from Claude Code. If the spike fails, this ADR is revisited before M4.

## Alternatives considered

- **OAuth 2.1 now.** The standard path for remote MCP and friendlier for clients, but
  it requires an authorisation server, consent flow and token lifecycle in a
  single-binary self-hosted tool. Deferred to v0.2 rather than half-built.
- **stdio only.** No network exposure and no auth design, but each teammate would have
  to run the whole program locally with their own copy of the data, which contradicts
  ADR 0004.
- **A shared team token.** Easiest to set up, but no per-person revocation and no
  usage visibility.
- **Network identity only** (for example relying on a Tailscale-only listener).
  Useful as a layer, but it is not application auth; tokens still identify the caller.

## Consequences

### Good

- One config line per teammate; `token create` prints the ready-to-paste command for
  Claude Code, Codex and Cursor.
- Revocation takes effect on the next request, including web sessions.

### Bad

- Bearer tokens are long-lived secrets that people paste into client configs.
  Mitigations are hashing at rest, the `tctx_` prefix, revocation and `last_used_at`.
- No standard OAuth discovery flow until v0.2; some clients may need header support or
  the stdio mode.

### Follow-ups

- M1 S3 result; M4 auth tests (401, revoked token) and rate limit on failures.
