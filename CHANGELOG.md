# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0.0 the project is pre-release: breaking changes can happen in any minor (0.y.z)
release.

## [Unreleased]

### Changed

- The retrieval eval now measures what users get: `search` and `context_for` go through
  `core::rank` (the merge `teamctx search` runs), and a new `ask_retrieval` row runs the
  `/ask` bundle (`Bundle::retrieve`, with thread neighbours). A broken ranking now fails
  `--check`. Baseline rewritten; overall combined recall@10 of `search` moved from 0.6102
  to 0.5941 because the raw store order was kinder than the production ranking.
- The eval cuts the merged ranking at ten results in total, exactly as `teamctx search`
  does, instead of ten per collection; `search` combined recall@10 moved from 0.5941 to
  0.5363. Baseline rewritten; see `docs/research/M2.md`.

### Fixed

- REST sync: an archived thread whose final catch-up failed or hit the page limit is
  caught up again on the next run instead of being skipped for good. The recheck can
  delete a stored message when the newest messages Discord returns are system messages
  the store never keeps; when no stored message can anchor the check it now reports the
  window as not covered instead of silently deleting nothing.
- REST sync: the deletion recheck now pages through the stored messages of its window
  with `MessageRead::channel_messages_since` instead of a neighbour window around an
  anchor message. A channel with more than 5,000 stored messages in the recheck window no
  longer skips deletes, and the "no stored message to anchor on" case is gone.

### Fixed

- MCP `record` and `search` paging. A page cut for size now continues right after the last
  entry it returned, so none are skipped. `search` no longer offers a cursor past the
  100-result limit that it would reject; its last page says the limit was reached.
  `record`'s `owner` filter finds a member by display name through a parameterised store
  query (`Store::member_by_display_name`) instead of probing member ids 1 to 500.

### Added

- `MessageRead::channel_messages_since`: oldest-first keyset paging of one channel's
  stored messages since a time, served by the `messages_channel_time` index.
- The read-only JSON API and the `teamctx q` client. `GET /healthz` (no token) and, behind
  the same bearer auth as `/mcp`, `GET /api/v1/{context,whats-new,record,search,read,status}`.
  The routes run the MCP retrieval and view code on the same connection, so a response is
  the MCP `structuredContent` plus the framed `text`, with identical ids, order, cursors and
  untrusted-data framing; parity tests compare both over HTTP on the demo record. Unknown
  or repeated parameters are refused with a fixed `400` message. `teamctx q <context|
  whats-new|record|search|read> ... [--json]` reads `TEAMCTX_URL` and the token from
  `TEAMCTX_TOKEN` or a token file (mode 0600 on Unix, looser is refused), never prints the
  token, does not follow redirects and replaces terminal control characters in the output.
  Not yet: `POST /api/v1/ask`, sessions and the web view; `status` only proves the record
  is readable.
- A prompt-injection corpus in CI (`fixtures/injection/`): 40 fictional hostile messages,
  plus hostile titles, display names and a thread name, loaded through the demo importer.
  Tests check that every MCP tool keeps all of it inside the one framed block and out of
  `structuredContent` and the tool definitions, and that `/ask` keeps it out of the
  system prompt, drops citations of ids outside the retrieved bundle and strips URLs
  missing from the retrieved sources. CI fails if the corpus has fewer than 30 messages.
  Known gap, kept as an ignored test: an MCP display name with a line break can add a
  line inside the block (it cannot close the block).
- CI guards for the two promises of plan section 13 (see `docs/testing/guards.md`). A
  source scan fails on any Discord write call outside interaction replies: twilight-http
  write methods and builders, unreviewed write-shaped twilight names, and HTTP write
  methods in files that hold a Discord REST route. Log-capture tests run the sync, auth,
  token, config and `/ask` paths with fake secrets under a TRACE subscriber and fail if a
  secret appears in logs, `Debug`/`Display` text or error chains, raw or hex, percent or
  base64 encoded. Two known leaks are recorded as ignored tests: an LLM key pasted into
  `llm.api_key_env` is echoed by the "not set" error, and a malformed Discord payload value
  is quoted into `SyncError`'s source chain.
- The MCP server's five read-only tools (`context_for`, `whats_new`, `record`, `search`,
  `read`) and the `catch_up` prompt, over streamable HTTP behind token auth. Results start
  with the untrusted-data notice, hold chat text only inside one framed block, cite message
  and entry ids with jump links, and carry `structuredContent` with an `outputSchema`.
  CI tests keep the tool definitions under 1,500 tokens and default responses under 2,000
  (estimated as characters / 4). `teamctx_server::serve` serves the router with client
  addresses so the failed-auth limit counts per client. Not yet in `whats_new`: messages
  that mention the caller (tokens are not linked to members).
- Access tokens for the MCP endpoint: `teamctx token create|list|revoke`, SHA-256-only
  storage, a failed-attempt rate limit and a `Secret` wrapper that never prints its value.
  A token revoked at any moment is rejected on the next use, and concurrent bad attempts
  cannot exceed the limit.
- A SQLite-backed usage ledger for `/ask` budgets (`StoreLedger`). Reserving a question
  and settling it each run in one transaction, so concurrent requests cannot exceed the
  hourly or daily limits, and an unsettled reservation keeps counting after a restart.
- The `/ask` pipeline (not wired to Discord yet): Anthropic and OpenAI-compatible adapters
  over plain HTTP, framed retrieval bundle, citation and URL validation that also removes
  encoded and indirect Markdown links, and per-member budgets reserved before each call.
- The Discord REST sync algorithm (not connected to Discord yet): channel and thread
  discovery, per-channel cursors for catch-up and backfill, and a 48 hour recheck window
  that applies missed edits and deletes. It runs against a read-only `DiscordRest` trait
  with no write methods, saves its cursor after every page so a failed run resumes where it
  stopped, and returns rate limits and other failures as typed errors without sleeping.
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

### Security

- Search queries are limited to 2,000 bytes and 32 words, phrases and filters; longer
  ones fail with "query too long" or "too many terms" (exit code 2 in `teamctx search`).
  Compiled full-text expressions and `context_for` terms are capped at 32 as well. Before,
  a query of 100,000 words took seconds of CPU and built an unbounded FTS5 expression.
