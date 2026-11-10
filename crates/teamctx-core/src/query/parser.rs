use super::{Clause, Query};
use crate::time::DateError;
use std::str::FromStr;
use thiserror::Error;

/// Longest query [`parse`] accepts, in bytes (UTF-8), counted before any trimming.
///
/// Why 2,000: a typed search or a pasted question is a few hundred bytes (the `/ask`
/// question limit is 500 characters), and a Discord message is at most 2,000 characters,
/// so a pasted message usually fits. Counting bytes rather than characters bounds the
/// work the parser does, whatever the script.
pub const MAX_QUERY_BYTES: usize = 2_000;

/// Most clauses (words, phrases and filters together) [`parse`] accepts. It is also the
/// most terms [`fts_expression`](super::fts_expression),
/// [`fts_expression_all`](super::fts_expression_all) and
/// [`context_query`](super::context_query) use, so a hand-built [`Query`] stays bounded.
pub const MAX_CLAUSES: usize = 32;

/// Invalid query syntax. Messages never interpolate untrusted query text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseQueryError {
    /// More than [`MAX_QUERY_BYTES`] bytes.
    #[error("query too long: at most {MAX_QUERY_BYTES} bytes")]
    TooLong,
    /// More than [`MAX_CLAUSES`] clauses.
    #[error("too many terms: at most {MAX_CLAUSES} words, phrases and filters")]
    TooManyClauses,
    /// No clauses supplied.
    #[error("query must contain a word, phrase or filter")]
    Empty,
    /// Missing closing quote.
    #[error("quoted value needs a closing double quote")]
    UnclosedQuote,
    /// Quotes must wrap a complete value.
    #[error("separate clauses with whitespace; quote a complete value")]
    UnexpectedQuote,
    /// Missing filter value or empty phrase.
    #[error("phrases and filters must not be empty")]
    EmptyValue,
    /// Invalid calendar date in a filter.
    #[error("invalid query date: {0}")]
    Date(#[from] DateError),
}

/// Parses plan section 9 syntax. Double an embedded quote inside a quoted value.
/// Unknown `name:value` prefixes remain plain words. Repeated filters remain AND
/// clauses in order, never silently overwritten.
///
/// Size limits come first, so hostile input costs little: more than
/// [`MAX_QUERY_BYTES`] bytes is rejected before anything is read, and parsing stops
/// before clause number [`MAX_CLAUSES`] + 1, without reading it.
///
/// # Errors
/// Returns [`ParseQueryError`] for oversized input, too many clauses, empty input,
/// malformed quotes, empty values or dates.
pub fn parse(input: &str) -> Result<Query, ParseQueryError> {
    if input.len() > MAX_QUERY_BYTES {
        return Err(ParseQueryError::TooLong);
    }
    let mut remaining = input.trim_start();
    let mut clauses = Vec::new();
    while !remaining.is_empty() {
        if clauses.len() == MAX_CLAUSES {
            return Err(ParseQueryError::TooManyClauses);
        }
        let prefix = ["from:", "in:", "before:", "after:"]
            .into_iter()
            .find(|prefix| remaining.starts_with(prefix));
        if let Some(prefix) = prefix {
            remaining = &remaining[prefix.len()..];
        }
        let quoted = remaining.starts_with('"');
        let (mut value, rest) = value(remaining)?;
        remaining = rest.trim_start();
        let clause = match prefix {
            Some("from:") => Clause::From(value),
            Some("in:") => {
                if value.starts_with('#') {
                    value.remove(0);
                }
                if value.is_empty() {
                    return Err(ParseQueryError::EmptyValue);
                }
                Clause::In(value)
            }
            Some("before:") => Clause::Before(value.parse()?),
            Some("after:") => Clause::After(value.parse()?),
            _ if quoted => Clause::Phrase(value),
            _ => Clause::Word(value),
        };
        clauses.push(clause);
    }
    if clauses.is_empty() {
        return Err(ParseQueryError::Empty);
    }
    Ok(Query { clauses })
}

fn value(input: &str) -> Result<(String, &str), ParseQueryError> {
    if let Some(mut rest) = input.strip_prefix('"') {
        let mut text = String::new();
        loop {
            let Some(index) = rest.find('"') else {
                return Err(ParseQueryError::UnclosedQuote);
            };
            text.push_str(&rest[..index]);
            rest = &rest[index + 1..];
            if let Some(after) = rest.strip_prefix('"') {
                text.push('"');
                rest = after;
                continue;
            }
            if rest.chars().next().is_some_and(|c| !c.is_whitespace()) {
                return Err(ParseQueryError::UnexpectedQuote);
            }
            if text.trim().is_empty() {
                return Err(ParseQueryError::EmptyValue);
            }
            return Ok((text, rest));
        }
    }
    let index = input.find(char::is_whitespace).unwrap_or(input.len());
    let text = &input[..index];
    if text.is_empty() {
        return Err(ParseQueryError::EmptyValue);
    }
    if text.contains('"') {
        return Err(ParseQueryError::UnexpectedQuote);
    }
    Ok((text.to_owned(), &input[index..]))
}

impl FromStr for Query {
    type Err = ParseQueryError;
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        parse(input)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_clause_types_and_repeated_filters() {
        let query = parse(" sqlite \"full text\" from:\"Robin Example\" in:#planning before:2026-12-01 after:2026-01-01 from:sage in:dev ").unwrap();
        assert_eq!(
            query.clauses,
            vec![
                Clause::Word("sqlite".to_owned()),
                Clause::Phrase("full text".to_owned()),
                Clause::From("Robin Example".to_owned()),
                Clause::In("planning".to_owned()),
                Clause::Before("2026-12-01".parse().unwrap()),
                Clause::After("2026-01-01".parse().unwrap()),
                Clause::From("sage".to_owned()),
                Clause::In("dev".to_owned())
            ]
        );
        assert_eq!(
            parse("in:\"#release plans\"").unwrap().clauses,
            vec![Clause::In("release plans".to_owned())]
        );
        assert_eq!(
            parse("\"say \"\"hello\"\"\"").unwrap().clauses,
            vec![Clause::Phrase("say \"hello\"".to_owned())]
        );
        assert_eq!("sqlite".parse::<Query>(), parse("sqlite"));
    }

    #[test]
    fn rejects_malformed_queries() {
        for (input, expected) in [
            (" \n", ParseQueryError::Empty),
            ("\"", ParseQueryError::UnclosedQuote),
            ("\"a", ParseQueryError::UnclosedQuote),
            ("from:", ParseQueryError::EmptyValue),
            ("from: robin", ParseQueryError::EmptyValue),
            ("in:#", ParseQueryError::EmptyValue),
            ("\"\"", ParseQueryError::EmptyValue),
            ("\" \"", ParseQueryError::EmptyValue),
            ("a\"b", ParseQueryError::UnexpectedQuote),
            ("\"a\"b", ParseQueryError::UnexpectedQuote),
            ("after:2025-02-29", ParseQueryError::Date(DateError::Day)),
            (
                "before:2026/01/01",
                ParseQueryError::Date(DateError::Format),
            ),
        ] {
            assert_eq!(parse(input), Err(expected), "{input:?}");
        }
    }

    #[test]
    fn length_limit_counts_bytes_and_is_inclusive() {
        let at_limit = "a".repeat(MAX_QUERY_BYTES);
        assert_eq!(parse(&at_limit).map(|q| q.clauses.len()), Ok(1));
        let over = "a".repeat(MAX_QUERY_BYTES + 1);
        assert_eq!(parse(&over), Err(ParseQueryError::TooLong));

        // `é` is two bytes: half as many characters reach the same byte limit.
        let wide = "é".repeat(MAX_QUERY_BYTES / 2);
        assert_eq!(wide.len(), MAX_QUERY_BYTES);
        assert!(parse(&wide).is_ok());
        let wide_over = format!("{wide}a");
        assert!(wide_over.chars().count() < MAX_QUERY_BYTES);
        assert_eq!(parse(&wide_over), Err(ParseQueryError::TooLong));

        // Whitespace counts too: the check runs before any trimming or splitting.
        let padded = format!("{}sqlite", " ".repeat(MAX_QUERY_BYTES));
        assert_eq!(parse(&padded), Err(ParseQueryError::TooLong));
        assert_eq!(
            parse(&" ".repeat(MAX_QUERY_BYTES + 1)),
            Err(ParseQueryError::TooLong)
        );
        // A huge input is rejected without being parsed, even when it is malformed.
        let hostile = format!("\"{}", "x ".repeat(500_000));
        assert_eq!(parse(&hostile), Err(ParseQueryError::TooLong));
    }

    #[test]
    fn clause_limit_counts_words_phrases_and_filters() {
        let words = |n: usize| {
            (0..n)
                .map(|i| format!("w{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            parse(&words(MAX_CLAUSES)).map(|q| q.clauses.len()),
            Ok(MAX_CLAUSES)
        );
        assert_eq!(
            parse(&words(MAX_CLAUSES + 1)),
            Err(ParseQueryError::TooManyClauses)
        );

        // A mix: 8 of each kind of clause is exactly the limit.
        let mut mixed = Vec::new();
        for i in 0..8 {
            mixed.push(format!("word{i}"));
            mixed.push(format!("\"phrase {i}\""));
            mixed.push(format!("from:member{i}"));
            mixed.push(format!("after:2026-01-{:02}", i + 1));
        }
        let mixed = mixed.join(" ");
        assert_eq!(parse(&mixed).map(|q| q.clauses.len()), Ok(MAX_CLAUSES));
        for extra in ["\"one more\"", "in:#dev", "before:2026-02-01", "more"] {
            assert_eq!(
                parse(&format!("{mixed} {extra}")),
                Err(ParseQueryError::TooManyClauses),
                "{extra:?}"
            );
        }
        // Trailing whitespace is not a clause.
        assert!(parse(&format!("{mixed}   ")).is_ok());
        // The clause past the limit is never parsed: a malformed one still reports the
        // limit, so no per-clause work happens beyond it.
        assert_eq!(
            parse(&format!("{} \"unclosed", words(MAX_CLAUSES))),
            Err(ParseQueryError::TooManyClauses)
        );
        // Single-letter words: the most clauses that fit in the byte limit.
        assert_eq!(
            parse(&"a ".repeat(MAX_QUERY_BYTES / 2)),
            Err(ParseQueryError::TooManyClauses)
        );
    }

    #[test]
    fn limit_errors_never_echo_the_query() {
        let marker = "not-a-real-secret-marker";
        for input in [
            format!("{marker} {}", "x".repeat(MAX_QUERY_BYTES)),
            format!("{marker} {}", "y ".repeat(MAX_CLAUSES)),
        ] {
            let error = parse(&input).unwrap_err();
            for message in [error.to_string(), format!("{error:?}")] {
                assert!(!message.contains(marker), "{message}");
                assert!(!message.contains("xx"), "{message}");
                assert!(!message.contains("y y"), "{message}");
            }
        }
        assert_eq!(
            ParseQueryError::TooLong.to_string(),
            "query too long: at most 2000 bytes"
        );
        assert_eq!(
            ParseQueryError::TooManyClauses.to_string(),
            "too many terms: at most 32 words, phrases and filters"
        );
    }
}
