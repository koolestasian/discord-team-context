//! Per-member budgets (plan section 11, step 6), backed by the `llm_usage` table.
//!
//! The pipeline talks to the table through [`UsageLedger`]. [`StoreLedger`] implements
//! it over `llm_usage` (all SQL stays in the store crate); [`InMemoryLedger`] is for
//! tests and demos.
//!
//! # Reserve, call, settle
//!
//! A question is paid for before the provider is called, never after:
//!
//! 1. [`UsageLedger::reserve`] checks the budget and holds one question plus the most
//!    tokens the call can use (the prompt estimate plus the output allowance) in one
//!    atomic step. Concurrent questions see each other's reservations, so with a limit
//!    of one, exactly one of them reaches the provider.
//! 2. The provider is called.
//! 3. [`UsageLedger::settle`] swaps the reservation for the real `llm_usage` row.
//!
//! If the request is dropped while the call is in flight (a timeout, a closed
//! interaction, a cancelled task), the provider may still do and bill the work, so the
//! reservation is settled as a failed call at its full reserved size. Cancellation
//! therefore never makes a call free. A reservation that cannot be settled at all keeps
//! counting at its reserved size, which errs on the side of refusing.

use std::error::Error as StdError;
use std::fmt;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use thiserror::Error;

use teamctx_core::id::MemberId;
use teamctx_core::repo::RepoError;
use teamctx_core::time::UnixMillis;
use teamctx_store::usage::{Reservation as StoreReservation, UsageDb, UsageRecord};

use crate::error::AskError;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// The limits for one member. Windows are rolling: the last hour and the last 24 hours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Most questions in the last hour (plan: `max_questions_per_member_per_hour`).
    /// Failed calls count too, so a broken provider cannot be hammered.
    pub max_questions_per_hour: u32,
    /// Most input plus output tokens in the last hour.
    pub max_tokens_per_hour: u64,
    /// Most input plus output tokens in the last 24 hours (plan: `max_tokens_per_day`).
    pub max_tokens_per_day: u64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_questions_per_hour: 10,
            max_tokens_per_hour: 100_000,
            max_tokens_per_day: 200_000,
        }
    }
}

/// Which limit a refused question ran into. `Display` is a polite sentence that is safe
/// to show to the member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetLimit {
    /// Too many questions in the last hour.
    QuestionsPerHour,
    /// Too many tokens in the last hour.
    TokensPerHour,
    /// Too many tokens in the last 24 hours.
    TokensPerDay,
}

impl fmt::Display for BudgetLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::QuestionsPerHour => {
                "You have reached the hourly limit for questions. Please try again later."
            }
            Self::TokensPerHour => {
                "You have reached the hourly limit for model usage. Please try again later."
            }
            Self::TokensPerDay => {
                "You have reached the daily limit for model usage. Please try again tomorrow."
            }
        })
    }
}

/// What one member used inside a window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowUsage {
    /// Calls made, successful or not.
    pub questions: u32,
    /// Input plus output tokens.
    pub tokens: u64,
}

/// One `llm_usage` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageEntry {
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

/// A usage ledger failure. The source is for debug logs only.
#[derive(Debug, Error)]
#[error("usage ledger error")]
pub struct LedgerError(#[source] Box<dyn StdError + Send + Sync>);

impl LedgerError {
    /// Wraps a backend error.
    pub fn new(error: impl StdError + Send + Sync + 'static) -> Self {
        Self(Box::new(error))
    }
}

/// Why a reservation was not granted.
#[derive(Debug, Error)]
pub enum ReserveError {
    /// The question would exceed a limit.
    #[error("{0}")]
    Refused(BudgetLimit),
    /// The ledger failed.
    #[error(transparent)]
    Ledger(#[from] LedgerError),
}

impl From<ReserveError> for AskError {
    fn from(error: ReserveError) -> Self {
        match error {
            ReserveError::Refused(limit) => Self::BudgetExceeded(limit),
            ReserveError::Ledger(error) => Self::Ledger(error),
        }
    }
}

/// A held reservation, as handed out by [`UsageLedger::reserve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ReservationId(u64);

impl ReservationId {
    /// Wraps a ledger's own handle, such as a row id.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// The ledger's handle.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl Budget {
    /// Whether one more question that may use up to `tokens` (prompt plus output
    /// allowance) fits. `usage_since` reports a member's usage from a window start to
    /// `now`, including outstanding reservations. A ledger calls this inside the same
    /// lock or transaction that stores the reservation.
    ///
    /// # Errors
    ///
    /// [`ReserveError::Refused`] naming the first limit hit, or the error of
    /// `usage_since`.
    pub fn admit(
        &self,
        now: UnixMillis,
        tokens: u64,
        mut usage_since: impl FnMut(UnixMillis) -> Result<WindowUsage, LedgerError>,
    ) -> Result<(), ReserveError> {
        let hour = usage_since(UnixMillis::from_millis(
            now.as_millis().saturating_sub(HOUR_MS),
        ))?;
        if hour.questions >= self.max_questions_per_hour {
            return Err(ReserveError::Refused(BudgetLimit::QuestionsPerHour));
        }
        if hour.tokens.saturating_add(tokens) > self.max_tokens_per_hour {
            return Err(ReserveError::Refused(BudgetLimit::TokensPerHour));
        }
        let day = usage_since(UnixMillis::from_millis(
            now.as_millis().saturating_sub(DAY_MS),
        ))?;
        if day.tokens.saturating_add(tokens) > self.max_tokens_per_day {
            return Err(ReserveError::Refused(BudgetLimit::TokensPerDay));
        }
        Ok(())
    }
}

/// Reads and writes `llm_usage`. All methods are synchronous like the other store
/// traits; a host runs them off the async threads.
///
/// See the [module docs](self) for how reservations keep concurrent and cancelled
/// questions inside the budget.
pub trait UsageLedger {
    /// Calls and tokens by `member` with `at >= since`, counting outstanding
    /// reservations at their reserved size.
    ///
    /// # Errors
    ///
    /// [`LedgerError`] if the backend fails.
    fn usage_since(&self, member: MemberId, since: UnixMillis) -> Result<WindowUsage, LedgerError>;

    /// Appends one row.
    ///
    /// # Errors
    ///
    /// [`LedgerError`] if the backend fails.
    fn record(&self, entry: &UsageEntry) -> Result<(), LedgerError>;

    /// Atomically checks `budget` with [`Budget::admit`] and, if the question fits,
    /// holds one question and `tokens` tokens for `member` at `now`. No other
    /// `reserve` for the member may interleave between the check and the hold.
    ///
    /// Until it is settled, the reservation counts in [`usage_since`](Self::usage_since)
    /// exactly like a row at `now` with `tokens` tokens.
    ///
    /// # Errors
    ///
    /// [`ReserveError::Refused`] if a limit would be exceeded (nothing is held), or
    /// [`ReserveError::Ledger`] if the backend fails.
    fn reserve(
        &self,
        member: MemberId,
        now: UnixMillis,
        budget: &Budget,
        tokens: u64,
    ) -> Result<ReservationId, ReserveError>;

    /// Atomically replaces the reservation with `entry`. `entry` is appended even if
    /// the reservation is unknown, so a call is never lost.
    ///
    /// # Errors
    ///
    /// [`LedgerError`] if the backend fails; the reservation then keeps counting.
    fn settle(&self, reservation: ReservationId, entry: &UsageEntry) -> Result<(), LedgerError>;
}

impl From<RepoError> for LedgerError {
    fn from(error: RepoError) -> Self {
        Self::new(error)
    }
}

impl From<RepoError> for ReserveError {
    fn from(error: RepoError) -> Self {
        Self::Ledger(error.into())
    }
}

/// The ledger over the `llm_usage` table, so budgets survive restarts.
///
/// Reserve and settle each run in one SQLite transaction (see
/// [`teamctx_store::usage`]), so concurrent or crashed questions cannot exceed a
/// budget. It holds its own connection, separate from the runtime's single writer
/// thread; SQLite's write lock serializes the two. Call it from a blocking worker.
#[derive(Debug)]
pub struct StoreLedger {
    db: UsageDb,
}

impl StoreLedger {
    /// Opens the database file at `path`, applying migrations if needed.
    ///
    /// # Errors
    ///
    /// [`LedgerError`] if the file cannot be opened.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, LedgerError> {
        Ok(Self {
            db: UsageDb::open(path)?,
        })
    }

    /// Wraps an existing usage handle.
    #[must_use]
    pub fn new(db: UsageDb) -> Self {
        Self { db }
    }
}

fn to_record(entry: &UsageEntry) -> UsageRecord {
    UsageRecord {
        at: entry.at,
        member: entry.member,
        provider: entry.provider.clone(),
        model: entry.model.clone(),
        input_tokens: entry.input_tokens,
        output_tokens: entry.output_tokens,
        ok: entry.ok,
    }
}

impl UsageLedger for StoreLedger {
    fn usage_since(&self, member: MemberId, since: UnixMillis) -> Result<WindowUsage, LedgerError> {
        let usage = self.db.usage_since(member, since)?;
        Ok(WindowUsage {
            questions: usage.questions,
            tokens: usage.tokens,
        })
    }

    fn record(&self, entry: &UsageEntry) -> Result<(), LedgerError> {
        Ok(self.db.record(&to_record(entry))?)
    }

    fn reserve(
        &self,
        member: MemberId,
        now: UnixMillis,
        budget: &Budget,
        tokens: u64,
    ) -> Result<ReservationId, ReserveError> {
        let held = self.db.reserve(member, now, tokens, |usage_since| {
            budget.admit(now, tokens, |since| {
                let usage = usage_since(since)?;
                Ok(WindowUsage {
                    questions: usage.questions,
                    tokens: usage.tokens,
                })
            })
        })?;
        Ok(ReservationId::new(held.get()))
    }

    fn settle(&self, reservation: ReservationId, entry: &UsageEntry) -> Result<(), LedgerError> {
        Ok(self.db.settle(
            StoreReservation::from_raw(reservation.get()),
            &to_record(entry),
        )?)
    }
}

/// One outstanding reservation in [`InMemoryLedger`].
#[derive(Debug)]
struct Held {
    id: ReservationId,
    member: MemberId,
    at: UnixMillis,
    tokens: u64,
}

#[derive(Debug, Default)]
struct LedgerState {
    rows: Vec<UsageEntry>,
    held: Vec<Held>,
    next_id: u64,
}

impl LedgerState {
    fn usage_since(&self, member: MemberId, since: UnixMillis) -> WindowUsage {
        let rows = self
            .rows
            .iter()
            .filter(|r| r.member == member && r.at >= since)
            .map(|r| r.input_tokens.saturating_add(r.output_tokens));
        let held = self
            .held
            .iter()
            .filter(|h| h.member == member && h.at >= since)
            .map(|h| h.tokens);
        let mut usage = WindowUsage::default();
        for tokens in rows.chain(held) {
            usage.questions = usage.questions.saturating_add(1);
            usage.tokens = usage.tokens.saturating_add(tokens);
        }
        usage
    }
}

/// A ledger that keeps rows and reservations in memory, behind one lock.
#[derive(Debug, Default)]
pub struct InMemoryLedger {
    state: Mutex<LedgerState>,
}

impl InMemoryLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> std::sync::MutexGuard<'_, LedgerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// All rows recorded so far, oldest first. Outstanding reservations are not rows.
    #[must_use]
    pub fn entries(&self) -> Vec<UsageEntry> {
        self.state().rows.clone()
    }

    /// How many reservations are held and not yet settled.
    #[must_use]
    pub fn outstanding(&self) -> usize {
        self.state().held.len()
    }
}

impl UsageLedger for InMemoryLedger {
    fn usage_since(&self, member: MemberId, since: UnixMillis) -> Result<WindowUsage, LedgerError> {
        Ok(self.state().usage_since(member, since))
    }

    fn record(&self, entry: &UsageEntry) -> Result<(), LedgerError> {
        self.state().rows.push(entry.clone());
        Ok(())
    }

    fn reserve(
        &self,
        member: MemberId,
        now: UnixMillis,
        budget: &Budget,
        tokens: u64,
    ) -> Result<ReservationId, ReserveError> {
        // One lock covers the check and the hold, so reservations cannot interleave.
        let mut state = self.state();
        budget.admit(now, tokens, |since| Ok(state.usage_since(member, since)))?;
        let id = ReservationId::new(state.next_id);
        state.next_id = state.next_id.wrapping_add(1);
        state.held.push(Held {
            id,
            member,
            at: now,
            tokens,
        });
        Ok(id)
    }

    fn settle(&self, reservation: ReservationId, entry: &UsageEntry) -> Result<(), LedgerError> {
        let mut state = self.state();
        state.held.retain(|h| h.id != reservation);
        state.rows.push(entry.clone());
        Ok(())
    }
}

/// Refuses the question if it would exceed a limit, without holding anything: an early,
/// cheap refusal before retrieval. `tokens` is the most the call could use (prompt
/// estimate plus output allowance). [`Reservation::take`] repeats the check atomically.
pub(crate) fn check<L: UsageLedger + ?Sized>(
    ledger: &L,
    budget: &Budget,
    member: MemberId,
    now: UnixMillis,
    tokens: u64,
) -> Result<(), AskError> {
    budget
        .admit(now, tokens, |since| ledger.usage_since(member, since))
        .map_err(AskError::from)
}

/// A reservation held for the length of one provider call.
///
/// Settle it with the real usage via [`settle`](Self::settle). If it is dropped instead
/// (the future running the call was cancelled), it is settled with the row given to
/// [`take`](Self::take): a failed call charged at the full reserved size.
pub(crate) struct Reservation<'l, L: UsageLedger + ?Sized> {
    ledger: &'l L,
    id: Option<ReservationId>,
    if_dropped: UsageEntry,
}

impl<'l, L: UsageLedger + ?Sized> Reservation<'l, L> {
    /// Reserves one question and `if_dropped`'s tokens for `if_dropped.member` at
    /// `if_dropped.at`.
    pub(crate) fn take(
        ledger: &'l L,
        budget: &Budget,
        if_dropped: UsageEntry,
    ) -> Result<Self, AskError> {
        let tokens = if_dropped
            .input_tokens
            .saturating_add(if_dropped.output_tokens);
        let id = ledger.reserve(if_dropped.member, if_dropped.at, budget, tokens)?;
        Ok(Self {
            ledger,
            id: Some(id),
            if_dropped,
        })
    }

    /// Replaces the reservation with `entry`.
    pub(crate) fn settle(mut self, entry: &UsageEntry) -> Result<(), LedgerError> {
        match self.id.take() {
            Some(id) => self.ledger.settle(id, entry),
            None => Ok(()),
        }
    }
}

impl<L: UsageLedger + ?Sized> Drop for Reservation<'_, L> {
    fn drop(&mut self) {
        if let Some(id) = self.id.take() {
            // Nowhere to report an error from `drop`. If settling fails the reservation
            // stays held and keeps counting at its full size, so the failure still
            // refuses rather than gives a free call.
            let _ = self.ledger.settle(id, &self.if_dropped);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(id: i64) -> MemberId {
        MemberId::new(id).unwrap()
    }

    fn row(member_id: i64, at: i64, tokens: u64) -> UsageEntry {
        UsageEntry {
            at: UnixMillis::from_millis(at),
            member: member(member_id),
            provider: "fake".to_owned(),
            model: "m".to_owned(),
            input_tokens: tokens,
            output_tokens: 0,
            ok: true,
        }
    }

    fn budget() -> Budget {
        Budget {
            max_questions_per_hour: 2,
            max_tokens_per_hour: 1_000,
            max_tokens_per_day: 3_000,
        }
    }

    const NOW: i64 = 10 * DAY_MS;

    fn now() -> UnixMillis {
        UnixMillis::from_millis(NOW)
    }

    #[test]
    fn windows_are_per_member_and_inclusive_of_their_start() {
        let ledger = InMemoryLedger::new();
        ledger.record(&row(1, NOW - HOUR_MS, 10)).unwrap();
        ledger.record(&row(1, NOW - HOUR_MS - 1, 10)).unwrap();
        ledger.record(&row(2, NOW, 99)).unwrap();
        let hour = ledger
            .usage_since(member(1), UnixMillis::from_millis(NOW - HOUR_MS))
            .unwrap();
        assert_eq!(
            hour,
            WindowUsage {
                questions: 1,
                tokens: 10
            }
        );
    }

    #[test]
    fn hourly_question_limit() {
        let ledger = InMemoryLedger::new();
        ledger.record(&row(1, NOW - 60_000, 1)).unwrap();
        assert!(check(&ledger, &budget(), member(1), now(), 1).is_ok());
        ledger.record(&row(1, NOW - 30_000, 1)).unwrap();
        assert!(matches!(
            check(&ledger, &budget(), member(1), now(), 1),
            Err(AskError::BudgetExceeded(BudgetLimit::QuestionsPerHour))
        ));
        // Another member is unaffected.
        assert!(check(&ledger, &budget(), member(2), now(), 1).is_ok());
    }

    #[test]
    fn hourly_and_daily_token_limits_include_the_pending_prompt() {
        let ledger = InMemoryLedger::new();
        ledger.record(&row(1, NOW - 10_000, 900)).unwrap();
        assert!(check(&ledger, &budget(), member(1), now(), 100).is_ok());
        assert!(matches!(
            check(&ledger, &budget(), member(1), now(), 101),
            Err(AskError::BudgetExceeded(BudgetLimit::TokensPerHour))
        ));
        let ledger = InMemoryLedger::new();
        // Old enough to leave the hour window, still inside the day.
        ledger.record(&row(1, NOW - 2 * HOUR_MS, 2_900)).unwrap();
        assert!(check(&ledger, &budget(), member(1), now(), 100).is_ok());
        assert!(matches!(
            check(&ledger, &budget(), member(1), now(), 101),
            Err(AskError::BudgetExceeded(BudgetLimit::TokensPerDay))
        ));
        // Past the day window the budget is free again.
        let ledger = InMemoryLedger::new();
        ledger.record(&row(1, NOW - DAY_MS - 1, 2_900)).unwrap();
        assert!(check(&ledger, &budget(), member(1), now(), 100).is_ok());
    }

    #[test]
    fn a_zero_budget_refuses_everything() {
        let zero = Budget {
            max_questions_per_hour: 0,
            ..budget()
        };
        assert!(matches!(
            check(&InMemoryLedger::new(), &zero, member(1), now(), 0),
            Err(AskError::BudgetExceeded(BudgetLimit::QuestionsPerHour))
        ));
    }

    #[test]
    fn racing_threads_get_exactly_the_budget() {
        let ledger = InMemoryLedger::new();
        let one = Budget {
            max_questions_per_hour: 1,
            ..budget()
        };
        let granted = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..16)
                .map(|_| scope.spawn(|| ledger.reserve(member(1), now(), &one, 10).is_ok()))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|&ok| ok)
                .count()
        });
        assert_eq!(granted, 1);
        assert_eq!(ledger.outstanding(), 1);
    }

    #[test]
    fn reservations_count_until_settled_with_the_real_usage() {
        let ledger = InMemoryLedger::new();
        let id = ledger.reserve(member(1), now(), &budget(), 900).unwrap();
        // Held: one question and 900 tokens, though no row exists yet.
        let hour_ago = UnixMillis::from_millis(NOW - HOUR_MS);
        assert!(ledger.entries().is_empty());
        assert_eq!(
            ledger.usage_since(member(1), hour_ago).unwrap(),
            WindowUsage {
                questions: 1,
                tokens: 900
            }
        );
        assert!(matches!(
            ledger.reserve(member(1), now(), &budget(), 101),
            Err(ReserveError::Refused(BudgetLimit::TokensPerHour))
        ));
        // A refusal holds nothing.
        assert_eq!(ledger.outstanding(), 1);
        ledger.settle(id, &row(1, NOW, 40)).unwrap();
        assert_eq!(ledger.outstanding(), 0);
        assert_eq!(
            ledger.usage_since(member(1), hour_ago).unwrap(),
            WindowUsage {
                questions: 1,
                tokens: 40
            }
        );
    }

    #[test]
    fn a_dropped_reservation_is_charged_in_full() {
        let ledger = InMemoryLedger::new();
        let if_dropped = UsageEntry {
            output_tokens: 800,
            ok: false,
            ..row(1, NOW, 200)
        };
        drop(Reservation::take(&ledger, &budget(), if_dropped.clone()).unwrap());
        assert_eq!(ledger.outstanding(), 0);
        assert_eq!(ledger.entries(), [if_dropped]);
        // Settling explicitly records the real row instead, once.
        let ledger = InMemoryLedger::new();
        let held = Reservation::take(&ledger, &budget(), row(1, NOW, 1_000)).unwrap();
        held.settle(&row(1, NOW, 7)).unwrap();
        assert_eq!(ledger.entries(), [row(1, NOW, 7)]);
    }

    /// A temp-file database holding one fictional member (usage rows reference members).
    fn store_ledger(path: &Path) -> (StoreLedger, MemberId) {
        use teamctx_core::id::Snowflake;
        use teamctx_core::repo::{DirectoryRead, IngestWrite};
        use teamctx_core::source::{Member, SourceEvent, SourceKind};
        use teamctx_store::Store;

        let mut store = Store::open(path).unwrap();
        let source = store
            .upsert_source(SourceKind::Discord, Snowflake::new(1).unwrap(), "Fiction")
            .unwrap();
        let external = Snowflake::new(100).unwrap();
        let person = Member {
            id: external,
            display_name: "Piper Fiction".to_owned(),
            is_bot: false,
        };
        store
            .apply_event(
                source,
                &SourceEvent::MemberUpsert(person),
                UnixMillis::from_millis(1),
            )
            .unwrap();
        let id = store
            .member_by_external(source, external)
            .unwrap()
            .unwrap()
            .id;
        drop(store);
        (StoreLedger::open(path).unwrap(), id)
    }

    #[test]
    fn store_ledger_refuses_an_exhausted_budget_and_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let (ledger, who) = store_ledger(&path);
        let id = ledger.reserve(who, now(), &budget(), 900).unwrap();
        assert!(matches!(
            ledger.reserve(who, now(), &budget(), 101),
            Err(ReserveError::Refused(BudgetLimit::TokensPerHour))
        ));
        drop(ledger);

        // After a restart the unsettled reservation still counts in full.
        let ledger = StoreLedger::open(&path).unwrap();
        let hour_ago = UnixMillis::from_millis(NOW - HOUR_MS);
        assert_eq!(
            ledger.usage_since(who, hour_ago).unwrap(),
            WindowUsage {
                questions: 1,
                tokens: 900
            }
        );
        assert!(ledger.reserve(who, now(), &budget(), 101).is_err());

        // Settling swaps it for the real usage, which frees the rest of the budget.
        ledger.settle(id, &row(who.get(), NOW, 40)).unwrap();
        assert_eq!(
            ledger.usage_since(who, hour_ago).unwrap(),
            WindowUsage {
                questions: 1,
                tokens: 40
            }
        );
        assert!(ledger.reserve(who, now(), &budget(), 900).is_ok());
        // Window rollover: two hours later only the day window still holds them.
        let later = UnixMillis::from_millis(NOW + 2 * HOUR_MS);
        assert!(ledger.reserve(who, later, &budget(), 900).is_ok());
    }

    #[test]
    fn store_ledger_racing_threads_get_exactly_the_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let (ledger, who) = store_ledger(&path);
        let one = Budget {
            max_questions_per_hour: 1,
            ..budget()
        };
        let granted = std::thread::scope(|scope| {
            let ledger = &ledger;
            let one = &one;
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(move || ledger.reserve(who, now(), one, 10).is_ok()))
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap())
                .filter(|&ok| ok)
                .count()
        });
        assert_eq!(granted, 1);
    }

    #[test]
    fn a_dropped_store_reservation_is_charged_in_full() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.db");
        let (ledger, who) = store_ledger(&path);
        let if_dropped = UsageEntry {
            output_tokens: 800,
            ok: false,
            ..row(who.get(), NOW, 200)
        };
        drop(Reservation::take(&ledger, &budget(), if_dropped).unwrap());
        assert_eq!(
            ledger
                .usage_since(who, UnixMillis::from_millis(NOW - HOUR_MS))
                .unwrap(),
            WindowUsage {
                questions: 1,
                tokens: 1_000
            }
        );
    }

    #[test]
    fn refusals_read_politely() {
        for limit in [
            BudgetLimit::QuestionsPerHour,
            BudgetLimit::TokensPerHour,
            BudgetLimit::TokensPerDay,
        ] {
            assert!(limit.to_string().contains("try again"));
        }
    }
}
