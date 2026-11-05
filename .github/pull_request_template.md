## Summary

<!-- What changes, and why. Link the plan section or ADR if one applies. -->

## Linked issue

<!-- Closes #NNN, or "none" -->

## Checklist

- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass locally
- [ ] Tests added for new behaviour, at the layer named in plan section 14
- [ ] New dependency justified in this PR per ADR 0011, and `cargo deny check` passes (or no new dependency)
- [ ] Docs, ADRs and CHANGELOG (Unreleased) updated where behaviour or decisions changed
- [ ] No secrets, tokens or real team data in code, fixtures, docs or logs
- [ ] Commits follow Conventional Commits
