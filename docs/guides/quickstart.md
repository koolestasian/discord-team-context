# Quickstart

Three ways in, from "no Discord needed" to "run it for a team". You need Rust 1.89 or
newer ([rustup](https://rustup.rs)).

```sh
git clone https://github.com/koolestasian/discord-team-context.git
cd discord-team-context
cargo test --workspace          # unit, integration and fixture tests
```

## 1. Try the demo (no Discord, no account)

The demo is a fictional six-person team. Everything in it is made up; see
[fixtures/demo/README.md](../../fixtures/demo/README.md).

```sh
# Load the demo team into a SQLite file in your OS data directory (or pass --db PATH).
# Safe to repeat; refuses to touch a database that is not the demo.
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

## 2. Connect an AI coding agent

Agents reach the record over MCP, or through the `teamctx q` command-line client plus an
[Agent Skill](https://agentskills.io/specification) that tells them when to use it. Both
answer from the same query layer.

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
`teamctx run`, which needs a bot token (see section 3).

To connect a real agent, create a token for one person or device. The output also holds
the commands that connect Claude Code, Codex and Cursor and a snippet for AGENTS.md or
CLAUDE.md. The token is printed once; the commands read it from `TEAMCTX_TOKEN`.

```sh
cargo run -p teamctx -- token create robin-laptop --url https://team.example.net

# Copy skills/teamctx/SKILL.md to ~/.claude/skills/teamctx and ~/.agents/skills/teamctx
# (Claude Code; Codex and other agents that read the shared folder). Use --dir PATH for
# one folder; a differing existing copy is only replaced with --force.
cargo run -p teamctx -- skill install
```

## 3. Run it for a team (needs a Discord bot)

Follow the [Discord test bot guide](discord-bot.md) to create a throwaway server and bot.
Then:

```sh
# Print a config template and a guild-locked bot invite. Writes no files.
cargo run -p teamctx -- init --guild-id <server id> --application-id <app id>

# Check a config offline. Makes no network calls and never prints the token.
cargo run -p teamctx -- doctor --config teamctx.toml
```

`teamctx run` is the team host: it syncs Discord, serves the record and registers the
slash commands. It has been built and tested against fakes; it has not yet been run
against a real Discord server.

## Optional: `/ask` with a language model

Only `/ask` and catch-up summaries use a model; leave the `[llm]` table out and
everything else still works. teamctx talks to Anthropic directly (`provider =
"anthropic"`) or to any Chat Completions endpoint (`provider = "openai_compatible"`),
which covers aggregators such as OpenRouter and local servers such as LM Studio or
Ollama. The key is read from an environment variable and never from the config file.

```toml
[llm]
provider = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"   # or http://127.0.0.1:1234/v1 for LM Studio
model = "<a model id from your provider>"
api_key_env = "TEAMCTX_LLM_KEY"
```

```sh
export TEAMCTX_LLM_KEY=...   # set in your shell, never in a file you commit
```

The model sees only the retrieved, framed excerpts, has no tools, and every citation in
its answer is checked against them. Per-member and per-token budgets cap usage. Details
and limits are in the [language model guide](llm.md).
