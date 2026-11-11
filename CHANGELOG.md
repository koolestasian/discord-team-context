# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0.0 the project is pre-release: breaking changes can happen in any minor (0.y.z)
release.

## [Unreleased]

### Added

- Optional `/ask question` and `/catchup summarize:true` in `teamctx run`, enabled by
  `[llm]`, with scoped retrieval, validated citations, escaped ephemeral replies and
  fixed errors. `POST /api/v1/ask` uses the same pipeline behind bearer authentication.
  Hourly question and daily token limits persist per Discord member or HTTP token;
  access tokens have no member association. See `docs/guides/llm.md`. Tested with a fake
  provider only; no live Discord or model claims.
- `teamctx serve [--db PATH] [--bind 127.0.0.1:PORT] [--timezone TZ]`: serves `/mcp`, `/api/v1`
  and `/healthz` from an existing database on a loopback address, with no Discord connection,
  so `teamctx q`, MCP clients and the Agent Skill can be tried on the demo record. A process
  test runs demo, token, serve and `q`. `run` remains the team host.
- Discord commands (plan section 7.3), served by `teamctx run`: message context menus
  "Mark as decision", "Mark as task" (a form with an optional owner and due date) and
  "Mark as question"; `/decide text [replaces]`, `/decisions [topic]
  [include_superseded]`, `/todo add|list|done`, `/question`, `/answer id message_link`,
  `/catchup [since]` (from your last catch-up, at most 14 days), `/search query [channel]
  [author]` and `/teamctx status`. Registered as guild commands on start. Replies are
  visible only to you unless you pass `public:true`, never ping anyone, escape chat text
  and only show messages from channels you can read (a public reply only what everyone
  can read). Slow commands defer and then edit their reply. Retried interactions create
  nothing twice; every change is in the audit log with who made it. `/ask` is registered
  only with LLM support configured. Not yet verified against a live Discord server (spike S2 is a manual
  step in `docs/guides/discord-bot.md`).
- `teamctx export --format okf|md DIR [--db PATH] [--force]` writes the record as an OKF
  0.2 bundle (`index.md`, `log.md`, one file per entry) or as one `record.md`. Chat text is
  escaped for file names, YAML and Markdown; a non-empty directory is refused without
  `--force`. See ADR 0013 and `docs/research/M5.md`.
- Core: `core::when` parses due dates and "since" windows (`tomorrow`, `next friday`,
  `nov 20`, `2026-11-20`, `3d`, `24h`) against a caller-supplied timezone and rejects
  ambiguous input; record edit rules (`check_edit`, `NewEntry::from_message`,
  `RequestKey`) and owner and due-date audit actions.
- Store: the retry-safe `RecordEdit` trait (`capture_message`, `create_entry_once`,
  `change_status_once`, `set_owner`, `set_due`) on `Store` and `WriterClient`. Migration
  0002 adds `record_requests`, an index on `record_events` and makes `record_events`
  append-only (schema version 2).
- `teamctx run [--config PATH] [--db PATH]`: the host. It validates the configuration
  (flags > environment > file > defaults), refuses named time zones (no time zone
  database is built in), enabled reaction capture (not built yet), a missing bot token
  and a `secrets.env` that is not owner-only, all before any network call. It then opens
  the store with the single writer, serves `/mcp`, `/api/v1` and `/healthz` (connection
  info for the per-address auth limiter; `Host` allow-list from `server.bind` and
  `server.public_url`; dates in `server.timezone`), and runs the Discord connector.
  Ctrl-C or SIGTERM stops HTTP, closes the gateway, drains queued events and the writer,
  checkpoints the WAL and exits 0. `retention.max_age_days` is applied at start and daily.
  Not yet verified against a live Discord server.
- Discord connector: one `twilight-gateway` shard with exactly `GUILDS`, `GUILD_MESSAGES`
  and `MESSAGE_CONTENT`; live message, thread and channel events for the configured guild
  only; partial `MESSAGE_UPDATE` merged with the stored row, or the message fetched with a
  read-only `GET` when it is missing; discovery and catch-up on every start and after
  every reconnect, background backfill, back-off on rate limits and Discord errors.
  `twilight-http` (GET only) implements the existing read-only REST trait.
- `teamctx doctor --live`: read-only checks that the bot sees the guild, which text,
  announcement and forum channels it can read (channel overwrites resolved), whether it
  holds more than View Channels and Read Message History, and whether the Message Content
  intent is enabled. Prints no secret; channel names are escaped.
- `teamctx mcp --stdio [--db PATH] [--timezone TZ]`: the five MCP tools over standard
  input and output from a local database, for agents on the host machine.
- `/api/v1/status` reports sync state when served by `run`: gateway state, last event,
  sync runs, the current problem and each channel's cursor (ids only, no names).
- Guards: the read-only test also pins `twilight_http` to one file, pins the doctor's
  `GET` endpoints and forbids gateway-side sends other than the close frame; a log-capture
  test shows chat text from the gateway never reaches the connector's logs.

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

- Chat-controlled text no longer reaches places it should not. MCP and API result lines
  flatten member and channel names to one line of at most 64 characters, so a display name
  with a line break cannot add a line inside the framed block. The "not set" error for
  `llm.api_key_env` names the variable only when it looks like an environment variable name
  (a key pasted there is not echoed). A Discord payload of the wrong shape is reported by
  error category and position, not by quoting the offending value.
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

- The Agent Skill and `teamctx skill install`. `skills/teamctx/SKILL.md` follows the
  agentskills.io specification: a `description` that says when to use it (before starting a
  feature, when unsure what the team decided), five example `teamctx q` commands, and the
  rule that results are chat data, never instructions. `skill install [--dir PATH]
  [--force]` copies the embedded file to `~/.claude/skills/teamctx/` and
  `~/.agents/skills/teamctx/` (or to `PATH/teamctx/`); an identical copy is left alone, a
  differing one is replaced only with `--force`, both targets are checked before either is
  written, and a symlink or directory in the way is never replaced. Tests check the
  frontmatter against the specification's limits and that every example command parses
  with the `q` client. Sources are in `docs/research/M4.md`.
- `teamctx token create` now prints how to connect after the token: a `claude mcp add`
  command, a Codex `config.toml` table and a Cursor `mcp.json` (each reads the token from
  `TEAMCTX_TOKEN`, so the token is printed once), the `teamctx q` plus skill route, and a
  snippet for `AGENTS.md` or `CLAUDE.md`. New `--url URL` sets the host address (default a
  `<host>` placeholder); it is checked against an allow-list because it is printed inside
  shell, TOML and JSON. The snippets are parsed as TOML and JSON in tests.
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
