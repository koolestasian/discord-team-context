# 0004. One host per team

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §1, §3 D4, §4, §13, §19

## Context

A team's Discord history, decisions and tasks are private. The program must hold a
bot token, a message store, personal access tokens and optionally an LLM key (plan
§13). Teammates reach it from their own machines (AI agents over MCP, browsers, the
CLI client), and non-coders use it from inside Discord.

The plan's non-goals exclude a hosted multi-team service, and multiple servers per
install (multi-guild is v0.2). The platform terms behind this choice are summarised
in §13 (Discord Developer Policy: no training on message content; Slack terms apply
from v0.2).

## Decision

Each team runs its own instance on one always-on machine. Teammates connect over the
network; there is no shared multi-tenant service and no data leaves the host except
optional LLM calls (ADR 0007).

Network rules (§4):

- The server binds to `127.0.0.1` by default.
- Remote access goes through Tailscale (recommended), a Cloudflare Tunnel, or a TLS
  reverse proxy such as Caddy.
- `teamctx doctor` warns if the server is bound publicly without TLS in front.

One instance serves one Discord server in v0.1.

## Alternatives considered

- **Hosted service run by the project.** Easiest onboarding, but we would hold many
  teams' private chat, take on the compliance and security burden that comes with it,
  and conflict with the platform rules noted in §13. Excluded from v0.1.
- **Local-only on each teammate's machine.** No network exposure, but every person
  would need their own bot and copy of the data, and sync would be duplicated.
- **Cloud function or serverless deployment.** Avoids an always-on machine, but the
  gateway connection is long-lived and catch-up after downtime is limited (§7.2), so
  it fits poorly.

## Consequences

### Good

- Data stays under the team's control; the privacy story in
  `docs/guides/privacy.md` is simple to state.
- The single-writer SQLite design (ADR 0003) needs no coordination between hosts.

### Bad

- Someone must keep a machine on. Offline periods can miss edits and deletes; the
  recheck window mitigates this and the limit is documented (§7.2, §19).
- Teams without a host need alternatives: a Docker image and guides for a Pi, an old
  laptop or a small VPS are the planned mitigation (§19), and `teamctx demo` lets
  people try the product first.
- The host carries the operational duties: updates, backups, token rotation.

### Follow-ups

- M8: hosting guides (Tailscale, tunnel, Docker), threat model, privacy guide.
