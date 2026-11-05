# 0002. twilight

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D2, §7.2, §17

## Context

The Discord connector needs four things (plan §7): a gateway connection that delivers
message, thread, channel and reaction events with message content; a REST client for
discovery, catch-up and backfill with correct rate-limit handling; typed models for
the payloads; and helpers such as permission calculation, which `doctor` needs to check
effective permissions per channel (§7.1).

The product is read-only towards Discord (ADR 0005), so the Discord dependency should
expose as little as possible beyond reading and answering interactions. The connector
must also be testable without a network, from recorded payloads in
`fixtures/discord/` (§14).

## Decision

Use the `twilight` family of crates, and only these four: `twilight-gateway`,
`twilight-http`, `twilight-model` and `twilight-util` (§17). The built-in ratelimiter
of `twilight-http` handles REST rate limits (§7.2). Everything Discord-specific stays
inside `teamctx-discord`; the rest of the workspace sees only normalised `SourceEvent`
values (§5.3).

This is validated by spike S1 in M1: the gateway must receive MESSAGE_CREATE with
message content on a test server. If the spike shows the crates cannot do what §7
needs, this ADR is revisited before M3 starts.

## Alternatives considered

- **`serenity` (optionally with `poise` for commands).** A higher-level framework with
  more built in. The trade-off is a larger surface and a stronger opinion about
  application structure, which sits awkwardly with a connector that must emit
  normalised events into our own store. We have not benchmarked either library; this
  is a judgement about fit, and the spike would be the first hands-on test.
- **Raw gateway and REST code** on a WebSocket and HTTP client. Full control and no
  Discord dependency, but we would re-implement gateway handling,
  reconnect and resume logic, rate limiting and permission maths ourselves.
- **Another language's library** (for example in Go). Only relevant if ADR 0001 is
  superseded.

## Consequences

### Good

- Modular crates let us depend on exactly the gateway, HTTP, model and util pieces.
- Typed models and the permission helpers support the fixture-based tests and the
  `doctor` permission check.

### Bad

- A lower-level library means we write the command handlers, interaction deferral and
  event mapping ourselves.
- We depend on `twilight` keeping pace with Discord API changes (plan §19, policy
  changes risk).

### Follow-ups

- M1 spike S1, then record the result here (keep or change).
- Fixtures: record gateway and REST payloads for every event type in `SourceEvent`
  (M3).
