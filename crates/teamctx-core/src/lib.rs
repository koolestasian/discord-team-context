//! Domain types and rules for teamctx: record rules, search query parsing, ranking,
//! citation formatting and the framing that marks chat content as untrusted data.
//!
//! Boundary: this crate does no I/O, has no async code and has no dependencies, so every
//! rule in it can be tested without a database, a network or a clock. All other crates
//! depend on it; it depends on none of them. The workspace test in
//! `crates/teamctx/tests/architecture.rs` enforces this.
