//! Syntax tree for the plan section 9 query syntax:
//! plain words, `"phrase"`, `from:name`, `in:#channel`, `before:/after:YYYY-MM-DD`.
//!
//! The tree records what the user wrote, in order. All clauses combine with AND,
//! including repeated filters. Date boundaries are midnight in `server.timezone`,
//! converted outside core (`after` inclusive, `before` exclusive).
//!
//! All strings are user input: untrusted, and never SQL or FTS syntax. The store escapes
//! them.

use crate::time::CalendarDate;

/// A parsed search query: its clauses in input order.
///
/// An empty `clauses` list is representable; whether it is a valid search is the
/// parser's decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Query {
    /// The clauses, in the order they appeared.
    pub clauses: Vec<Clause>,
}

/// One element of a query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clause {
    /// A plain word, matched as a full-text term.
    Word(String),
    /// A quoted phrase (`"…"`), without the quotes, matched as a full-text phrase.
    Phrase(String),
    /// `from:name`: messages by the member with this name (without the `from:` prefix).
    From(String),
    /// `in:#channel`: messages in this channel (without `in:` and `#`).
    In(String),
    /// `before:YYYY-MM-DD`: messages created before this date.
    Before(CalendarDate),
    /// `after:YYYY-MM-DD`: messages created at or after the start of this date.
    After(CalendarDate),
}

impl Query {
    /// The full-text parts (words and phrases), in input order.
    pub fn text_clauses(&self) -> impl Iterator<Item = &Clause> {
        self.clauses
            .iter()
            .filter(|c| matches!(c, Clause::Word(_) | Clause::Phrase(_)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_clauses_keep_order_and_skip_filters() {
        let query = Query {
            clauses: vec![
                Clause::Word("sqlite".to_owned()),
                Clause::From("Robin Example".to_owned()),
                Clause::Phrase("full text".to_owned()),
                Clause::After("2026-01-01".parse().unwrap()),
            ],
        };
        let text: Vec<_> = query.text_clauses().collect();
        assert_eq!(
            text,
            [
                &Clause::Word("sqlite".to_owned()),
                &Clause::Phrase("full text".to_owned())
            ]
        );
    }
}
