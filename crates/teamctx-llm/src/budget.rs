//! Per-member budgets (plan section 11, step 6), backed by the `llm_usage` table.
//!
//! The pipeline talks to the table through [`UsageLedger`]. The store crate implements
//! the trait over `llm_usage` when the host is wired; this crate ships an in-memory
//! ledger for tests and demos.

use std::error::Error as StdError;
use std::fmt;
use std::sync::{Mutex, PoisonError};

use thiserror::Error;

use teamctx_core::id::MemberId;
use teamctx_core::time::UnixMillis;

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

/// Reads and writes `llm_usage`. Both methods are synchronous like the other store
/// traits; a host runs them off the async threads.
pub trait UsageLedger {
    /// Calls and tokens by `member` with `at >= since`.
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
}

/// A ledger that keeps rows in memory.
#[derive(Debug, Default)]
pub struct InMemoryLedger {
    rows: Mutex<Vec<UsageEntry>>,
}

impl InMemoryLedger {
    /// An empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// All rows recorded so far, oldest first.
    #[must_use]
    pub fn entries(&self) -> Vec<UsageEntry> {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl UsageLedger for InMemoryLedger {
    fn usage_since(&self, member: MemberId, since: UnixMillis) -> Result<WindowUsage, LedgerError> {
        let rows = self.rows.lock().unwrap_or_else(PoisonError::into_inner);
        let mut usage = WindowUsage::default();
        for row in rows.iter().filter(|r| r.member == member && r.at >= since) {
            usage.questions = usage.questions.saturating_add(1);
            usage.tokens = usage
                .tokens
                .saturating_add(row.input_tokens)
                .saturating_add(row.output_tokens);
        }
        Ok(usage)
    }

    fn record(&self, entry: &UsageEntry) -> Result<(), LedgerError> {
        self.rows
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(entry.clone());
        Ok(())
    }
}

/// Refuses the question if it would exceed a limit. `estimated_input` is the size of the
/// prompt that would be sent, so one huge prompt cannot slip under a nearly full budget.
pub(crate) fn check<L: UsageLedger + ?Sized>(
    ledger: &L,
    budget: &Budget,
    member: MemberId,
    now: UnixMillis,
    estimated_input: u64,
) -> Result<(), AskError> {
    let hour = ledger.usage_since(
        member,
        UnixMillis::from_millis(now.as_millis().saturating_sub(HOUR_MS)),
    )?;
    if hour.questions >= budget.max_questions_per_hour {
        return Err(AskError::BudgetExceeded(BudgetLimit::QuestionsPerHour));
    }
    if hour.tokens.saturating_add(estimated_input) > budget.max_tokens_per_hour {
        return Err(AskError::BudgetExceeded(BudgetLimit::TokensPerHour));
    }
    let day = ledger.usage_since(
        member,
        UnixMillis::from_millis(now.as_millis().saturating_sub(DAY_MS)),
    )?;
    if day.tokens.saturating_add(estimated_input) > budget.max_tokens_per_day {
        return Err(AskError::BudgetExceeded(BudgetLimit::TokensPerDay));
    }
    Ok(())
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
