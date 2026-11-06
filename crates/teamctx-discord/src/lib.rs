//! Discord connector for teamctx: gateway and REST sync, mapping of Discord payloads to
//! core events, and the slash commands and context menus.
//!
//! Boundary: read-only towards Discord. The only calls that write are replies to
//! interactions. Everything it learns goes through `teamctx-store`; it never talks to the
//! server or LLM crates.

pub mod mapping;
pub mod permissions;
pub mod sync;
