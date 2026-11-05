# 0009. Dual licence: MIT OR Apache-2.0

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D9, §16

## Context

`teamctx` is an open-source program. Credibility and adoption by teams depend on a
licence that is permissive, well understood and compatible with the crates it uses.
The repository must also pass `cargo deny check`, whose `deny.toml` covers licences,
advisories, bans and sources (plan §5.4, §16).

The plan lists LICENSE-MIT and LICENSE-APACHE among the files the repository ships
(§16).

## Decision

License the project under `MIT OR Apache-2.0`, at the user's option. The repository
contains both `LICENSE-MIT` and `LICENSE-APACHE`, and every crate's `Cargo.toml` sets
`license = "MIT OR Apache-2.0"`.

`deny.toml` lists the licences dependencies may use and CI fails on anything else, so
the dependency tree stays compatible with this choice.

## Alternatives considered

- **MIT only.** Shortest and most familiar, but gives no express patent grant.
- **Apache-2.0 only.** Includes an express patent grant, but some downstream projects
  prefer to combine with MIT-only code. The dual licence lets each user pick.
- **GPL or AGPL.** Would keep derivatives open, including hosted forks. The plan's
  position is self-hosted per team, and a copyleft licence would deter some
  companies from adopting or contributing; not chosen. The plan does not weigh
  copyleft options further.
- **Source-available or non-commercial licences.** Not open source in the usual sense
  and at odds with the credibility goal.

## Consequences

### Good

- Users can choose whichever of the two licences suits them.
- Contributors and downstream packagers see a familiar licence pair.

### Bad

- Two licence files to keep in step, and contributors must accept both.
- Permissive terms mean a third party could run a closed hosted fork. The plan treats
  this as acceptable for v0.1 and names a funded competitor as a risk (§19), mitigated
  by being open source, self-hosted, non-coder friendly and community-driven.

### Follow-ups

- M0: add `LICENSE-MIT`, `LICENSE-APACHE`, the `license` field in each crate, and the
  licence allow-list in `deny.toml`.
- Bundled SQLite and other third-party licences are covered by the `cargo deny` check.
