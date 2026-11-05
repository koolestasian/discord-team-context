# 0012. Agent surfaces: MCP, CLI client and Agent Skill

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D11, §10.1, §10.4, §14

## Context

Coding agents need to know what the team decided. There is more than one way for an
agent to reach that knowledge, and teams differ. Practitioners on r/mcp report that
quality drops as tool count and description length grow, and that every tool response
is effectively a prompt (plan §10.1; a lead to verify, not a measured fact). Coding
agents also increasingly call tools from the shell and write results to files: Uber
says this "Code Mode" is its default for coding agents (X, 2026), and Cloudflare's
Code Mode cut context by 99.9% for a large API (blog, 2026-02-20) (§10.4).

## Decision

Offer three ways in, all served by the same query layer so results are identical:

1. **MCP server** (ADR 0006) with five intent-shaped, read-only tools: `context_for`,
   `whats_new`, `record`, `search`, `read`. Responses are concise Markdown plus
   `structuredContent`, end with a next-step line, and are paginated with an opaque
   cursor. Message text never appears in tool names, descriptions or schemas.
2. **CLI client**: `teamctx q <context|whats-new|record|search|read> ... [--json]`
   calls the host's JSON API, reading `TEAMCTX_URL` and `TEAMCTX_TOKEN` from the
   environment or the OS keychain.
3. **Agent Skill**: `skills/teamctx/SKILL.md` (agentskills.io open standard) says when
   to use either route and carries five example commands. `teamctx skill install`
   copies it into the user's skill folders.

Teams choose MCP, CLI plus skill, or both; no feature exists in only one of them. All
surfaces use the untrusted-content framing notice from §10.1.

Context budgets are tested: the five tool definitions together stay under 1,500
tokens, and default responses stay under 2,000 tokens. A CI test fails if either is
exceeded. A parity test checks that the CLI client and MCP return the same results.

## Alternatives considered

- **MCP only.** One surface to maintain, but it fixes how agents consume the data and
  pushes tool definitions into every session's context.
- **CLI and skill only.** Light on context and simple, but not every client runs shell
  commands, and MCP is the common interface for those that do not.
- **One tool per table or endpoint.** Faster to build, but more definitions to carry
  in context and a worse fit for what an agent is trying to do.
- **A single generic query tool.** Fewer definitions, but it shifts query design onto
  the agent and hides the entry points (`context_for` is the default start).

## Consequences

### Good

- Small context footprint, a tested budget, and a choice of route per team.
- A single query layer, so a fix lands on all surfaces.

### Bad

- Three surfaces mean more documentation and compatibility testing.
- The keychain integration for `TEAMCTX_TOKEN` may need a crate outside the list in
  ADR 0011; that would need its own justification.

### Follow-ups

- M4: parity and token-budget tests; connect from Claude Code over HTTP and via CLI
  plus skill.
