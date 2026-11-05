//! SQLite storage for teamctx: schema migrations, repositories and FTS5 queries.
//!
//! Boundary: this crate owns the database and is the only place that runs SQL. It
//! depends on `teamctx-core` and nothing else in the workspace. The connector, server and
//! LLM crates reach stored data only through it.
