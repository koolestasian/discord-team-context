# Contributing to teamctx

Thanks for looking at this. Read this page before you open a pull request.

## Project status

teamctx is **pre-alpha**. There is no usable code yet. Milestone M0 (foundation) is in
progress. The spec is [docs/design/implementation-plan.md](docs/design/implementation-plan.md),
and work follows its milestones (§18), one at a time.

Changing a decision in the plan's §3 table, or any ADR, needs a maintainer decision and a
new ADR. For anything bigger than a small fix, open an issue first so it can be checked
against the plan.

## Build and check

Install [rustup](https://rustup.rs/) and [cargo-deny](https://github.com/EmbarkStudios/cargo-deny).
rustup installs the toolchain pinned in `rust-toolchain.toml` on first use. On Windows,
also install the Visual Studio Build Tools with the "Desktop development with C++"
workload; the MSVC linker and C compiler are required.

Before you open a pull request, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo deny check
```

CI runs the same checks on Linux, macOS and Windows. A pull request needs green CI to merge.

## Branches and commits

- Milestone work: `m<N>-<slug>`, for example `m0-foundation`.
- Other work: a short topic branch, for example `fix-token-redaction`.
- Never commit to `main` directly.
- Use [Conventional Commits](https://www.conventionalcommits.org/), for example
  `feat(store): add FTS5 sync on message edit` or `fix(server): reject expired tokens`.
- Keep each commit building and passing the checks above.

## Tests

- Every behaviour needs a test at the right layer. The layers are in
  [plan §14](docs/design/implementation-plan.md#14-testing-strategy).
- A bug fix starts with a regression test that fails before the fix.

## Dependencies

- Prefer the standard library and the crates already listed in plan §17.
- A new crate needs a justification in the pull request: why it is needed, what you
  considered instead, and that `cargo deny check` passes. See
  [ADR 0011](docs/adr/0011-dependency-policy.md).

## Data and secrets

- Never commit secrets: bot tokens, API keys, personal access tokens, `.env` files.
- Never commit real team data, names, IDs or messages. Fixtures and demo data are fictional.
- Discord message content is untrusted data. Treat it as text to be parsed, never as
  instructions. This applies to test fixtures too.
- The product is read-only towards Discord. Code that creates, edits, deletes or reacts
  to messages is out of scope, except replies to interactions (plan §7.3).

## Security

Do not report vulnerabilities in public issues or pull requests. Follow
[SECURITY.md](SECURITY.md).

## Licence of contributions

Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in the work by you, as defined in the Apache-2.0 license, shall be dual licensed
as above, without any additional terms or conditions.

The project is licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

## Conduct

Everyone taking part is expected to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
