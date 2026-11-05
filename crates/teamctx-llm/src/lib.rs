//! Optional `/ask` pipeline for teamctx: retrieval bundle, provider adapters, citation
//! validation and budgets (plan sections 11 and 13, ADR 0007).
//!
//! Boundary: the only crate that talks to an LLM provider. Everything else works without
//! one; nothing outside this crate needs it configured. It reads records through the
//! `teamctx-core` read traits (implemented by `teamctx-store`) and treats chat content as
//! untrusted data.
//!
//! The safeguards, in order of a question's life:
//!
//! 1. the question is cleaned and the member's budget is checked ([`budget`]);
//! 2. a bounded bundle is retrieved: record entries first, then messages with thread
//!    neighbours; bot messages and excluded channels never enter ([`bundle`]);
//! 3. all chat-derived text goes through `teamctx_core::framing::frame` ([`prompt`]);
//! 4. one provider call, no tools ([`LlmProvider`]; the request type has no tool fields);
//! 5. citations are checked against the bundle, unknown URLs are removed, and an answer
//!    with no valid citation becomes "not found" ([`pipeline`]).
//!
//! Tests never call a real API: [`FakeProvider`] and a local mock HTTP server stand in.

pub mod anthropic;
pub mod budget;
pub mod bundle;
pub mod error;
pub mod fake;
pub mod openai;
pub mod pipeline;
pub mod prompt;
pub mod provider;
pub mod secret;

mod http;
mod validate;

pub use anthropic::{AnthropicConfig, AnthropicMessages};
pub use budget::{
    Budget, BudgetLimit, InMemoryLedger, LedgerError, UsageEntry, UsageLedger, WindowUsage,
};
pub use bundle::{Bundle, BundleLimits, Retrieval, Source};
pub use error::{AskError, LlmError};
pub use fake::FakeProvider;
pub use http::DEFAULT_TIMEOUT;
pub use openai::{OpenAiCompatible, OpenAiConfig};
pub use pipeline::{
    Answer, AskConfig, AskPipeline, AskRequest, AskResult, Citation, FoundAnswer, NOT_FOUND_TEXT,
    NotFoundAnswer, NotFoundReason, Prepared,
};
pub use provider::{Completion, CompletionRequest, LlmProvider, TokenUsage};
pub use secret::ApiKey;
