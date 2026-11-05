# 0008. Static web UI, no framework

- Status: Accepted
- Date: 2026-11-04
- Deciders: maintainer
- Plan reference: §3 D8, §10.2, §13, §15

## Context

The web view is a convenience, not the product's centre: three screens (Record, Ask &
search, Status) over a JSON API (plan §10.2). The program ships as a single binary
(ADR 0001) and its security model includes a strict content security policy and no
third-party assets (§13).

A JavaScript framework would bring a build step, a node toolchain in CI and a large
dependency tree that also needs auditing. The plan's supply-chain stance is few
dependencies (§13).

## Decision

The UI is static HTML, CSS and vanilla JavaScript in `web/` (`index.html`, `app.css`,
`app.js`), embedded into the binary with `include_str!` and served by axum. There is
no framework and no build step.

- CSP is `default-src 'self'`; there are no inline scripts and no third-party assets.
- Design tokens are CSS custom properties in `web/app.css`, mirrored as Figma
  variables (§15).
- Authentication is the cookie session from ADR 0006; all data comes from
  `/api/v1/*`.
- The screens are designed in Figma first, then implemented (§15). Done when each
  screen works at 360 px and 1280 px, passes keyboard navigation, and meets WCAG AA
  contrast.

## Alternatives considered

- **A framework such as React or Svelte with a bundler.** Better structure for large
  UIs and a bigger ecosystem. It costs a build pipeline, more dependencies to audit,
  and complexity out of proportion to three screens.
- **Server-rendered HTML templates.** Avoids client JS, but needs a template crate not
  on the approved list (ADR 0011) and couples presentation to the Rust build.
- **No web view; Discord and MCP only.** Smallest scope, but the host would have no
  visual status screen and teammates would have no place to browse the record outside
  Discord. Not adopted for v0.1.

## Consequences

### Good

- No build step; the repository builds with `cargo` alone.
- A small, auditable client with a strict CSP.
- The UI cannot drift from the API, because both ship in one binary.

### Bad

- Hand-written DOM code grows messy if the UI gets bigger than three screens.
- No component library; accessibility and responsive behaviour are our work.
- A UI change needs a new binary.

### Follow-ups

- M7: Figma frames, then static UI and API tests. If the UI outgrows this approach,
  write a new ADR rather than adding a framework piecemeal.
