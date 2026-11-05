use super::{Clause, Query};
use crate::time::DateError;
use std::str::FromStr;
use thiserror::Error;

/// Invalid query syntax. Messages never interpolate untrusted query text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseQueryError {
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
/// # Errors
/// Returns [`ParseQueryError`] for empty input, malformed quotes, empty values or dates.
pub fn parse(input: &str) -> Result<Query, ParseQueryError> {
    let mut remaining = input.trim_start();
    let mut clauses = Vec::new();
    while !remaining.is_empty() {
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
}
