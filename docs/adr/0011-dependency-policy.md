# 0011. Dependency policy

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §13 (supply chain), §16, §17

## Context

The program holds a Discord token, personal access tokens and a copy of private chat.
Every dependency is code that runs with that access. The plan's supply-chain stance is
few dependencies, `cargo-deny` and `cargo-audit` in CI, a committed lockfile, actions
pinned by SHA, and releases with attestations (§13, §16).

## Decision

Every new crate needs a line in the pull request saying why it is needed, what was
considered instead, and that `cargo-deny` passes. Prefer the standard library and
crates already in the tree.

**Runtime dependencies at v0.1 start** (plan §17): `tokio`,
`twilight-{gateway,http,model,util}`, `rmcp`, `axum`, `rusqlite` (bundled), `reqwest`
(rustls), `serde`, `serde_json`, `toml`, `tracing`, `tracing-subscriber`, `thiserror`,
`anyhow` (binary crate only), `clap`, `sha2`, `getrandom`, `tokio-util`.

**Dev-only dependencies:** `insta`, `tempfile`.

Versions are the latest stable at scaffolding time and are recorded in `Cargo.lock`,
which is committed.

**Tools are not dependencies.** `cargo-deny`, `cargo-audit`, `cargo-dist` and
`cargo-llvm-cov` run in CI or on a developer machine; they are not runtime
dependencies and do not appear in the binary's dependency tree.

`deny.toml` encodes the policy: allowed licences (ADR 0009), advisories, bans and
sources. CI runs `cargo deny check` and `cargo audit` on every change.

## Alternatives considered

- **No written policy; judge each case in review.** Cheapest, but the list grows
  quietly, and reviewers cannot point to an agreed rule.
- **Vendor all dependencies.** Gives reproducibility and offline builds, but adds
  repository weight and does not by itself tell us what to trust. The committed
  lockfile plus `cargo-deny` covers the main need.
- **A strict allow-list that can only change by ADR.** Stronger control, but slows
  routine additions such as a date or hash helper. A PR-level justification, checked
  by tooling, was chosen as the lighter option.

## Consequences

### Good

- A short, reviewable list of what the program trusts.
- Licence, advisory and source problems fail CI instead of being found later.
- Small helpers (for example the due-date parser in `core`, §8) are written in-tree
  when a crate would add more risk than value.

### Bad

- Some code is written by hand that a crate would provide, and a hand-written helper
  can have bugs. Those helpers need tests.
- Transitive dependencies of the listed crates still enter the tree; `cargo-deny`
  reports them, but they are not individually approved.

### Follow-ups

- M0: add `deny.toml`, the CI jobs and the pull-request template line.
- Any crate outside the list needs a short note in the PR, and a new ADR only if it
  changes an earlier decision.
