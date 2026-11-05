//! Search query syntax (plan section 9).
//!
//! This module holds the syntax tree ([`ast`]). The parser from text to [`Query`] and
//! its error type are added by task T04 in this directory (`src/query/**`); the store
//! compiles a [`Query`] into parameterised SQL and escapes every FTS term (plan sections
//! 9 and 13). User text is never concatenated into SQL.

pub mod ast;

pub use ast::{Clause, Query};
