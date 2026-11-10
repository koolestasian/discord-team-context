# teamctx (working name)

Self-hosted Rust program that turns a team's Discord into a searchable team record.

@AGENTS.md

## Status

Built and tested against fakes and fixtures: M0, spikes S3/S4, core contracts, the demo dataset
and retrieval eval, the store, MCP/JSON API/`teamctx q`/Agent Skill, `teamctx run` (gateway,
sync, shutdown), Discord commands and context menus, record export. Nothing has run against a
real Discord server yet: S1/S2 and the live checks need a test server; see
docs/guides/discord-bot.md. `/ask` live, the web view and the release are not done.
