# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0.0 the project is pre-release: breaking changes can happen in any minor (0.y.z)
release.

## [Unreleased]

### Added

- Access tokens for the MCP endpoint: `teamctx token create|list|revoke`, SHA-256-only
  storage, a failed-attempt rate limit and a `Secret` wrapper that never prints its value.
  A token revoked at any moment is rejected on the next use, and concurrent bad attempts
  cannot exceed the limit.
- The `/ask` pipeline (not wired to Discord yet): Anthropic and OpenAI-compatible adapters
  over plain HTTP, framed retrieval bundle, citation and URL validation that also removes
  encoded and indirect Markdown links, and per-member budgets reserved before each call.
- Mapping of Discord gateway and REST payloads to source events, with fictional recorded
  fixtures. Nothing connects to Discord yet.
- A retrieval eval (`cargo xtask eval`, recall@10 and MRR) with a CI baseline. Bare words in
  a search now match in any order (any word is enough); recall@10 rose from 0.0 to about 0.6.
- `teamctx demo` loads the bundled fictional team into a SQLite file (OS data directory
  by default; repeatable; refuses a database that is not the demo), and `teamctx search`
  searches messages and record entries from the command line with short ids, citations
  and chat text framed as untrusted data. `--timezone` accepts `UTC` and fixed offsets;
  named zones are rejected until a time zone database is approved.
- SQLite migration, repository implementations, FTS triggers, mention storage and a
  dedicated writer thread; deletion, exclusion and retention redact copied titles
  and update the audit log transactionally. Date queries require host timezone wiring.
- Offline `init`/`doctor`, typed TOML configuration with CLI/environment precedence,
  Discord permission calculations and a test bot setup guide. Live validation is
  explicitly deferred.
- Query parsing and literal FTS5 expression compilation, deterministic ranking,
  citation formatting and escaped untrusted-chat framing in core.
- Fictional demo data (314 hand-written messages of a six-person team, 76 record entries
  with reversed decisions, tasks and questions) and 62 retrieval evaluation questions.
  Fixture tests check the data's structure, that paraphrase questions share no words
  with their answers, and that no record follows the two planted prompt-injection
  messages.
- `teamctx eval` (also `cargo xtask eval`): imports the demo, runs the 62 questions through
  `search` and `context_for`, and prints recall@10 and MRR per category. The measured
  baseline is stored in `fixtures/demo/eval-baseline.toml` and CI fails when an overall
  number drops below it; see `docs/research/M2.md`.
- Natural-language search: bare words now match any term (OR) and rank by `bm25`, so more
  matched terms score higher; quoted phrases and filters stay required. A fixed stopword
  list ("what", "did", "we", ...) is dropped from bare words in `search` as in
  `context_for`. On the demo eval, overall recall@10 went from 0.0000 to 0.6102 and MRR
  from 0.0000 to 0.4286 for `search` (0.6142 and 0.4326 for `context_for`).
- Core domain types, record lifecycle rules, query AST and repository contracts,
  with invariant tests and a contract map in `docs/design/contracts.md`.
- Spike S4: `teamctx-store` links the bundled SQLite (`rusqlite`) and tests an FTS5
  external-content index on in-memory and file databases; CI builds and tests for
  `x86_64-unknown-linux-musl`. Result in `docs/research/S4.md`.
- Spike S3 (`docs/research/S3.md`): `teamctx-server` serves an `rmcp` streamable HTTP MCP
  endpoint at `/mcp` behind bearer-token middleware, with a read-only `ping` tool and
  integration tests over real HTTP. Result: ADR 0006 stands.
- Implementation plan (`docs/design/implementation-plan.md`).
- Architecture decisions ADR 0001 to 0014.
- Community files: MIT and Apache-2.0 licences, code of conduct, contributing guide,
  security policy, issue and pull request templates.
- Cargo workspace skeleton: six crates (`teamctx-core`, `teamctx-store`,
  `teamctx-discord`, `teamctx-llm`, `teamctx-server` and the `teamctx` binary) and
  `xtask`, with a test that enforces the crate dependency direction. Rust 1.89 minimum,
  edition 2024, pinned development toolchain 1.99.1. The `teamctx` binary only answers
  `--help` and `--version`.
- CI on Linux, macOS and Windows: format, clippy, tests, an MSRV check, `cargo deny
  check` and a coverage report; a daily advisory check.
- Security workflows: CodeQL (Rust and Actions), OpenSSF Scorecard and zizmor. All
  GitHub Actions are pinned by commit SHA.
- Dependabot for Cargo, GitHub Actions and the Rust toolchain, grouped and weekly or
  monthly with a 7-day cooldown.
- `AGENTS.md` with build commands, the architecture map and project rules; `CLAUDE.md`
  imports it.

### Changed

- Docs: work runs as parallel tasks that stop at maintainer steps; the Rust go/no-go bar,
  the eval acceptance threshold and the v0.1 token-file decision are recorded in the plan.
- The schema is a single migration (`0001_init.sql`), written one column per line with
  comments; queries name their columns instead of `SELECT *`.
- The contributing guide, security policy and code of conduct moved to `.github/`.
  Code of Conduct reports now go through GitHub's private reporting form.

### Removed

- `docs/guides/repo-settings.md`, a maintainer-only checklist of GitHub settings.
