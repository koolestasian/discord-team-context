//! Search ranking (plan section 9): `bm25()` combined with recency decay and a boost for
//! record entries, with table-driven tests on the demo dataset.
//!
//! Placeholder module. Task T04 owns this file and adds the formula. Its inputs are the
//! raw `bm25` scores and timestamps the store returns in
//! [`MessageHit`](crate::repo::MessageHit) and [`EntryHit`](crate::repo::EntryHit),
//! plus the current time passed in by the caller (core reads no clock).
