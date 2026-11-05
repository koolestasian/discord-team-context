//! HTTP interfaces for teamctx: the MCP endpoint, the JSON API, the static web view and
//! token authentication.
//!
//! Boundary: serves the record from `teamctx-store`. It does not talk to Discord or to an
//! LLM provider directly.
