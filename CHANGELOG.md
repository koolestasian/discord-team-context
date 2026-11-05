# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Before 1.0.0 the project is pre-release: breaking changes can happen in any minor (0.y.z)
release.

## [Unreleased]

### Added

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
