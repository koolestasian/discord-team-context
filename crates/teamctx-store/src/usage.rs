//! The `llm_usage` table as a budget ledger (plan sections 6 and 11).
//!
//! # Reservations
//!
//! A question is reserved before the provider is called and settled afterwards. A
//! reservation is a normal `llm_usage` row whose `ok` column is [`PENDING`], holding the
//! tokens the call may use. It therefore counts in every window exactly like a finished
//! call, survives a restart, and keeps counting at its reserved size if it is never
//! settled (a crash mid-call refuses rather than gives a free call). Settling rewrites
//! the same row with the real usage in one statement.
//!
//! # Atomicity
//!
//! [`UsageDb::reserve`] runs the budget check and the insert inside one
//! `BEGIN IMMEDIATE` transaction, so two reservations can never both see the budget as
//! free, whether they come from threads sharing one [`UsageDb`] or from other
//! connections to the same file. [`UsageDb::settle`] is one transaction as well.
//!
//! The budget rules themselves (limits, windows, which limit is named) live with the
//! caller; this module hands them the window totals through a closure and keeps the SQL.

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use teamctx_core::{
    id::MemberId,
    repo::{RepoError, RepoResult},
    time::UnixMillis,
};

use crate::{Store, backend, invalid};

/// The `ok` value of a reservation that has not been settled. Finished calls use 0
/// (failed) and 1 (answered).
pub const PENDING: i64 = 2;

/// What one member used inside a window, reservations included.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UsageWindow {
    /// Calls made, successful or not, plus outstanding reservations.
    pub questions: u32,
    /// Input plus output tokens.
    pub tokens: u64,
}

/// One finished call, as stored in `llm_usage`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRecord {
    /// When the call was made.
    pub at: UnixMillis,
    /// Who asked.
    pub member: MemberId,
    /// The provider name, such as `"anthropic"`.
    pub provider: String,
    /// The model name.
    pub model: String,
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Whether the provider answered.
    pub ok: bool,
}

/// A held reservation: the id of its pending `llm_usage` row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Reservation(u64);

impl Reservation {
    /// The row id.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Wraps a row id previously returned by [`UsageDb::reserve`].
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
}

/// The reads and writes the ledger needs, over a connection with interior mutability.
///
/// Share it by reference or `Arc`. All methods are synchronous; call them from a
/// blocking worker, never directly on an async executor.
#[derive(Debug)]
pub struct UsageDb {
    conn: Mutex<Connection>,
}

fn to_i64(value: u64) -> RepoResult<i64> {
    i64::try_from(value).map_err(|_| invalid("token count is too large"))
}

fn to_u64(value: i64) -> RepoResult<u64> {
    u64::try_from(value).map_err(|_| invalid("stored usage is negative"))
}

fn window(conn: &Connection, member: MemberId, since: UnixMillis) -> RepoResult<UsageWindow> {
    let (questions, tokens): (i64, i64) = conn
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(input_tokens + output_tokens), 0)
             FROM llm_usage WHERE member_id = ? AND at >= ?",
            params![member.get(), since.as_millis()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(backend)?;
    Ok(UsageWindow {
        questions: u32::try_from(questions).unwrap_or(u32::MAX),
        tokens: to_u64(tokens)?,
    })
}

fn insert(conn: &Connection, record: &UsageRecord) -> RepoResult<()> {
    conn.execute(
        "INSERT INTO llm_usage (at, member_id, provider, model, input_tokens, output_tokens, ok)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![
            record.at.as_millis(),
            record.member.get(),
            record.provider,
            record.model,
            to_i64(record.input_tokens)?,
            to_i64(record.output_tokens)?,
            i64::from(record.ok),
        ],
    )
    .map_err(backend)?;
    Ok(())
}

impl UsageDb {
    /// Opens (and migrates, if needed) the database file at `path`. Open the runtime's
    /// writer first so only one connection ever applies migrations.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] if the file cannot be opened or migrated.
    pub fn open(path: impl AsRef<Path>) -> RepoResult<Self> {
        Ok(Self::from_store(Store::open(path)?))
    }

    /// Takes over the connection of an opened store (for an in-memory database in tests).
    #[must_use]
    pub fn from_store(store: Store) -> Self {
        Self {
            conn: Mutex::new(store.conn),
        }
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Calls and tokens by `member` with `at >= since`, reservations counted at their
    /// reserved size.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] if the query fails.
    pub fn usage_since(&self, member: MemberId, since: UnixMillis) -> RepoResult<UsageWindow> {
        window(&self.conn(), member, since)
    }

    /// Appends one finished call.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] if the insert fails.
    pub fn record(&self, record: &UsageRecord) -> RepoResult<()> {
        insert(&self.conn(), record)
    }

    /// Holds one question and `tokens` tokens for `member` at `now`, if `admit` allows.
    ///
    /// `admit` receives a function that reports the member's usage from a window start
    /// to now (reservations included) and returns `Ok` when the question fits. The check
    /// and the hold share one `BEGIN IMMEDIATE` transaction, so concurrent reservations
    /// never interleave between them. If `admit` refuses, nothing is held.
    ///
    /// # Errors
    ///
    /// The error `admit` returns, or a [`RepoError`] converted into `E` if the database
    /// fails.
    pub fn reserve<E: From<RepoError>>(
        &self,
        member: MemberId,
        now: UnixMillis,
        tokens: u64,
        admit: impl FnOnce(&mut dyn FnMut(UnixMillis) -> RepoResult<UsageWindow>) -> Result<(), E>,
    ) -> Result<Reservation, E> {
        let reserved = to_i64(tokens)?;
        let mut guard = self.conn();
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        admit(&mut |since| window(&tx, member, since))?;
        tx.execute(
            "INSERT INTO llm_usage (at, member_id, provider, model, input_tokens, output_tokens, ok)
             VALUES (?, ?, '', '', ?, 0, ?)",
            params![now.as_millis(), member.get(), reserved, PENDING],
        )
        .map_err(backend)?;
        let id = to_u64(tx.last_insert_rowid())?;
        tx.commit().map_err(backend)?;
        Ok(Reservation(id))
    }

    /// Replaces the reservation with `record`, in one transaction. `record` is appended
    /// even if the reservation is unknown or already settled, so a call is never lost.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] if the database fails; the reservation then keeps counting.
    pub fn settle(&self, reservation: Reservation, record: &UsageRecord) -> RepoResult<()> {
        let id = i64::try_from(reservation.0).map_err(|_| invalid("reservation id is invalid"))?;
        let mut guard = self.conn();
        let tx = guard
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(backend)?;
        let pending: Option<i64> = tx
            .query_row(
                "SELECT id FROM llm_usage WHERE id = ? AND ok = ?",
                params![id, PENDING],
                |r| r.get(0),
            )
            .optional()
            .map_err(backend)?;
        if pending.is_some() {
            tx.execute(
                "UPDATE llm_usage
                 SET at = ?, member_id = ?, provider = ?, model = ?,
                     input_tokens = ?, output_tokens = ?, ok = ?
                 WHERE id = ?",
                params![
                    record.at.as_millis(),
                    record.member.get(),
                    record.provider,
                    record.model,
                    to_i64(record.input_tokens)?,
                    to_i64(record.output_tokens)?,
                    i64::from(record.ok),
                    id,
                ],
            )
            .map_err(backend)?;
        } else {
            insert(&tx, record)?;
        }
        tx.commit().map_err(backend)
    }
}
