//! Initial deterministic retrieval baseline (plan section 9), pending the M2 retrieval evaluation.
//!
//! Score = `max(-bm25, 0) / (1 + age_days / 30) * entry_boost`, where entry_boost is
//! 2 for record entries and 1 for messages. Sort descending, then by a stable item id.
//! SQLite bm25 is negative: smaller is more relevant. Recency halves the score after
//! 30 days; future timestamps have zero age. `context_for` still selects record entries
//! before messages, independently of this within-group score (plan section 9).

use crate::time::UnixMillis;

/// Returns a finite, nonnegative rank with the clock supplied by the caller.
/// Invalid/nonnegative bm25 values score zero rather than contaminating sort order.
#[must_use]
pub fn score(bm25: f64, created_at: UnixMillis, now: UnixMillis, is_record: bool) -> f64 {
    if !bm25.is_finite() || bm25 >= 0.0 {
        return 0.0;
    }
    let age = now
        .as_millis()
        .saturating_sub(created_at.as_millis())
        .max(0) as f64;
    let relevance = -bm25 / (1.0 + age / (30.0 * 86_400_000.0));
    (relevance * if is_record { 2.0 } else { 1.0 }).min(f64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_formula_table() {
        let now = UnixMillis::from_millis(100 * 86_400_000);
        for (bm25, age_days, record, expected) in [
            (-2.0, 0, false, 2.0),
            (-4.0, 0, false, 4.0),
            (-2.0, 30, false, 1.0),
            (-2.0, 90, false, 0.5),
            (-2.0, 30, true, 2.0),
            (-2.0, -1, false, 2.0),
            (1.0, 0, false, 0.0),
        ] {
            let at = UnixMillis::from_millis(now.as_millis() - age_days * 86_400_000);
            assert_eq!(score(bm25, at, now, record), expected);
        }
    }

    #[test]
    fn extreme_scores_and_times_stay_finite() {
        for bm25 in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            -f64::MAX,
            -1.0,
            0.0,
        ] {
            for at in [i64::MIN, 0, i64::MAX] {
                let result = score(
                    bm25,
                    UnixMillis::from_millis(at),
                    UnixMillis::from_millis(i64::MAX),
                    true,
                );
                assert!(result.is_finite() && result >= 0.0);
            }
        }
    }
}
