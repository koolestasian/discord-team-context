//! Domain types and rules for teamctx: record rules, search query parsing, ranking,
//! citation formatting and the framing that marks chat content as untrusted data.
//!
//! Boundary: this crate does no I/O, has no async code and reads no clock, so every rule
//! in it can be tested without a database, a network or real time. Its only external
//! dependency is `thiserror` (for error types; maintainer decision, 2026-11-05). All
//! other workspace crates depend on it; it depends on none of them. The workspace test
//! in `crates/teamctx/tests/architecture.rs` enforces both rules.
//!
//! Modules and the plan sections they implement are mapped in
//! `docs/design/contracts.md`.
//!
//! All chat-derived text in these types is untrusted data, never instructions.

mod text_enum;

pub mod id;
pub mod query;
pub mod rank;
pub mod record;
pub mod repo;
pub mod source;
pub mod time;

pub use text_enum::ParseEnumError;
