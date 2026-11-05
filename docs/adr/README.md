# Architecture decision records

An architecture decision record (ADR) is a short document that captures one decision
about how `teamctx` is built: the context that forced a choice, the choice itself, the
alternatives we weighed, and what follows from it. ADRs explain why the code is the way
it is, so a new contributor does not have to guess or reopen settled questions. The
build plan in [`../design/implementation-plan.md`](../design/implementation-plan.md)
says what to build; ADRs record why key choices were made.

## Format

We use a lightweight MADR style (plan §17). Copy [`0000-template.md`](0000-template.md).
Each ADR has a title `# NNNN. Title`, a header (Status, Date, Deciders, Plan
reference) and four sections: **Context**, **Decision**, **Alternatives considered**
and **Consequences** (good, bad, follow-ups). Keep it to one page. Cite the plan
section or a dated source for any fact that is not obvious.

Statuses: `Proposed`, `Accepted`, `Superseded by NNNN`, `Deprecated`.

## Adding or changing an ADR

- **New decision.** Take the next free number, copy the template, name the file
  `NNNN-kebab-title.md`, add a row to the index below, and submit it in a pull request.
- **Changing a decision.** Do not rewrite an accepted ADR. Write a new ADR that says
  what it replaces, and set the old one's status to `Superseded by NNNN`. Small
  corrections (typos, broken links, a validation result from a spike) may be made in
  place with a dated note.
- **Plan decisions D1 to D13** (plan §3) can only be changed with the maintainer's OK,
  recorded in the superseding ADR.
- If a decision rests on a spike or test (for example S1 to S4 in milestone M1), the ADR
  names it, and the result is written back to the ADR.

## Index

| No. | Title | Status |
|---|---|---|
| [0001](0001-rust.md) | Rust | Accepted |
| [0002](0002-twilight.md) | twilight | Accepted |
| [0003](0003-sqlite-fts5.md) | SQLite with FTS5 | Accepted |
| [0004](0004-one-host-per-team.md) | One host per team | Accepted |
| [0005](0005-read-only-boundary.md) | Read-only boundary and interaction-only replies | Accepted |
| [0006](0006-mcp-over-http-bearer-tokens.md) | MCP over HTTP with bearer tokens (OAuth later) | Accepted |
| [0007](0007-optional-llm-two-adapters.md) | Optional LLM, two adapters, no SDKs | Accepted |
| [0008](0008-static-web-ui.md) | Static web UI, no framework | Accepted |
| [0009](0009-dual-licence.md) | Dual licence: MIT OR Apache-2.0 | Accepted |
| [0010](0010-clean-rebuild-from-prototype.md) | Clean rebuild from the prototype | Accepted |
| [0011](0011-dependency-policy.md) | Dependency policy | Accepted |
| [0012](0012-agent-surfaces.md) | Agent surfaces: MCP, CLI client and Agent Skill | Accepted |
| [0013](0013-okf-export.md) | Open Knowledge Format export | Accepted |
| [0014](0014-measured-retrieval-quality.md) | Measured retrieval quality | Accepted |
