//! Optional `/ask` pipeline for teamctx: retrieval bundle, provider adapters, citation
//! validation and budgets.
//!
//! Boundary: the only crate that talks to an LLM provider. Everything else works without
//! one. It reads records through `teamctx-store` and treats chat content as untrusted data.
