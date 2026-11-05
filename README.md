# teamctx (working name)

Small teams decide things in chat and then lose them there. teamctx is a self-hosted,
single-binary program that keeps a searchable local copy of a team's Discord channels and
a team record of decisions, tasks and open questions. Each entry links to the message it
came from. The record is served through Discord slash commands, an MCP server for AI
coding agents, and a minimal web view. Everything except `/ask` works without an LLM.

**Status: pre-alpha — nothing is usable yet.**

## Documents

- [Implementation plan](docs/design/implementation-plan.md): the spec and source of truth.
- [Architecture decisions](docs/adr/): decisions and their reasons.
- [Contributing](CONTRIBUTING.md), [security policy](SECURITY.md) and
  [code of conduct](CODE_OF_CONDUCT.md).

## Licence

Licensed under either of Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE)) or MIT
([LICENSE-MIT](LICENSE-MIT)) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.
