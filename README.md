# teamctx (working name)

Small teams decide things in chat and then lose them there. teamctx is a self-hosted,
single-binary program that keeps a searchable local copy of a team's Discord channels and
a team record of decisions, tasks and open questions. Each entry links to the message it
came from. The record is served through Discord slash commands, an MCP server for AI
coding agents, and a minimal web view. Everything except `/ask` works without an LLM.

> **Status: pre-alpha, not usable yet.** The foundations are built and tested, but
> teamctx cannot connect to Discord yet. Follow the milestones below; v0.1.0 is the
> first release meant for real teams.

## Design principles

- **Self-hosted, one binary, one SQLite file.** Your chat copy stays on a machine you
  control. No cloud service.
- **Read-only towards Discord.** The bot never posts, edits, deletes or reacts, apart
  from replying to the slash commands people run. It asks for 66560 permissions (view
  channels and read message history), nothing more.
- **People decide what goes in the record.** Entries are captured on purpose (context
  menu, slash command or an opt-in reaction), never guessed by a model.
- **Chat is untrusted data.** Every surface that returns chat text frames it as data,
  so an agent reading it is told not to follow instructions inside it.
- **The LLM is optional.** Only `/ask` uses one, with your own key or a local model.

## Progress

| Milestone | Scope | State |
|---|---|---|
| M0 Foundation | Workspace, CI on Linux/macOS/Windows, security workflows, ADRs | Done |
| M1 Spikes | S3 MCP over HTTP with bearer auth, S4 bundled SQLite FTS5 | Done |
| | S1 gateway messages, S2 interaction replies | Needs a Discord test server |
| M2 Store + search | SQLite schema and repositories, query parser, ranking, demo data, eval questions | Done |
| | Retrieval eval runner (`cargo xtask eval`), `teamctx demo`, `teamctx search` | Done |
| | Natural-language questions: any-word matching lifts recall@10 from 0.0 to about 0.6 | Done, still improving |
| M3 Discord sync | Offline `init`/`doctor`, permission checks, bot setup guide | Done |
| | Live sync, backfill, live events (`teamctx run`) | Built and tested against fakes; not yet run against a real server |
| M4 Agent surfaces | MCP tools, tokens, JSON API, `teamctx q`, Agent Skill | Done, tested in-process and over loopback |
| M5 Record in Discord | Context menus, `/decide`, `/todo`, `/question`, `/answer`, `/catchup`, `/search`, export | Built and tested against fixtures; not yet run against a real server |
| M6-M8 | `/ask` live, web view, v0.1.0 release | Planned |

The [implementation plan](docs/design/implementation-plan.md) defines each milestone and
its "done when" criteria. [CHANGELOG.md](CHANGELOG.md) lists what has landed.

## Try what exists today

You need Rust 1.89 or newer ([rustup](https://rustup.rs)).

```sh
git clone https://github.com/koolestasian/discord-team-context.git
cd discord-team-context
cargo test --workspace          # unit, integration and fixture tests

# Print a config template and a guild-locked bot invite. Writes no files.
cargo run -p teamctx -- init --guild-id <server id> --application-id <app id>

# Check a config offline. Makes no network calls and never prints the token.
cargo run -p teamctx -- doctor --config teamctx.toml

# Load the fictional demo team into a SQLite file in your OS data directory
# (or pass --db PATH). Safe to repeat; refuses to touch a database that is not the demo.
cargo run -p teamctx -- demo

# Search it. Words, "phrases", from:NAME, in:#channel, after:/before:YYYY-MM-DD.
# Results show a short id (D-7, T-12, Q-3, m:40), a citation and the chat text
# framed as data, so the output is safe to paste into an agent.
cargo run -p teamctx -- search "serif"
cargo run -p teamctx -- search "in:#engineering epub" --limit 3
cargo run -p teamctx -- search "after:2026-04-01 crash" --timezone +02:00
```

`--timezone` sets the midnight that `before:` and `after:` mean. Only `UTC` and fixed
offsets such as `+02:00` work today; a named zone like `America/Toronto` is rejected
because no time zone database is built in.

### For coding agents

Agents reach the record over MCP, or through the `teamctx q` command-line client plus an
[Agent Skill](https://agentskills.io/specification) that tells them when to use it. Both
answer from the same query layer.

```sh
# Create a token for one person or device. The output also holds the commands that connect
# Claude Code, Codex and Cursor and a snippet for AGENTS.md or CLAUDE.md. The token is
# printed once; the commands read it from TEAMCTX_TOKEN.
cargo run -p teamctx -- token create robin-laptop --url https://team.example.net

# Copy skills/teamctx/SKILL.md to ~/.claude/skills/teamctx and ~/.agents/skills/teamctx
# (Claude Code; Codex and other agents that read the shared folder). Use --dir PATH for
# one folder; a differing existing copy is only replaced with --force.
cargo run -p teamctx -- skill install
```

To try the agent side without Discord, serve the demo record on loopback and query it:

```sh
cargo run -p teamctx -- demo --db demo.db
cargo run -p teamctx -- token create trial --db demo.db      # copy the tctx_... token
cargo run -p teamctx -- serve --db demo.db                   # leave running; Ctrl-C stops it
# in a second terminal:
export TEAMCTX_URL=http://127.0.0.1:7420 TEAMCTX_TOKEN=tctx_...
cargo run -p teamctx -- q search "beta date"
cargo run -p teamctx -- q context "pricing for the beta"
```

`serve` has no Discord connection and binds loopback only. A team host uses
`teamctx run`, which needs a bot token (see the guide below).

To prepare a throwaway test server and bot, follow the
[Discord test bot guide](docs/guides/discord-bot.md). The fictional dataset that the
tests, `teamctx demo` and the eval runner use is described in
[fixtures/demo/README.md](fixtures/demo/README.md).

## Repository layout

```text
crates/
  teamctx-core/      domain types, record rules, query parsing, ranking, framing (no I/O)
  teamctx-store/     SQLite migrations, repositories and full-text search
  teamctx-discord/   Discord connector: permissions today, sync next
  teamctx-server/    MCP server, JSON API and web view (HTTP)
  teamctx-llm/       optional /ask pipeline
  teamctx/           the binary: CLI and configuration
xtask/               developer tasks (`cargo xtask`)
fixtures/demo/       fictional team chat and retrieval eval questions
docs/
  design/            implementation plan and core contracts
  adr/               architecture decision records
  guides/            setup guides
  research/          prior-art research and spike results
.github/             CI workflows, contributing guide, security policy, code of conduct
```

Dependencies point one way: `teamctx` -> {`discord`, `server`, `llm`} -> `store` ->
`core`. A test enforces it.

## Contributing and security

- [Contributing guide](.github/CONTRIBUTING.md): checks to run, commit style, rules.
- [Security policy](.github/SECURITY.md): report vulnerabilities privately, never in an
  issue.
- [Code of conduct](.github/CODE_OF_CONDUCT.md).
- [AGENTS.md](AGENTS.md): instructions for AI coding agents working in this repository.

## Licence

Licensed under either of Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT
([LICENSE-MIT](LICENSE-MIT)) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
