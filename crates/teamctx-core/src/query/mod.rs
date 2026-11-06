//! Search query syntax. Filters (`from:`, `in:`, `before:`, `after:`) and quoted phrases
//! are required (AND), including repeated filters; bare words are alternatives (OR) that
//! rank higher the more of them match. Date boundaries are resolved outside core at
//! midnight in `server.timezone`: `after` inclusive, `before` exclusive. Bind filters as
//! SQL parameters separately.

pub mod ast;
mod parser;
mod stopwords;
pub use ast::{Clause, Query};
pub use parser::{ParseQueryError, parse};
pub use stopwords::is_stopword;

/// Quotes one text literal for an FTS5 MATCH expression. NUL becomes a space because
/// SQLite terminates FTS expressions at NUL.
fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\0', " ").replace('"', "\"\""))
}

/// Compiles the text clauses to one FTS5 MATCH parameter, never SQL. `None` means no
/// text clauses.
///
/// Matching rule: every quoted phrase is required (AND). Bare words are alternatives
/// (OR) and at least one must match; SQLite's `bm25` then scores an item higher the more
/// of them it matches. Stopwords ([`is_stopword`]) and repeated words (ignoring case)
/// are dropped from the bare words so a question like "What database did we pick?"
/// searches for "database" and "pick". If every bare word is a stopword they are all
/// kept, so the query still searches; next to a phrase they are dropped instead, since
/// the phrase already constrains the result.
///
/// Escaping: every term is a double-quoted literal with embedded quotes doubled, so
/// operators, column filters (`title:x`), `*`, `-` and parentheses in user text stay
/// literals. The only operators in the output are the `AND`, `OR` and parentheses
/// added here.
#[must_use]
pub fn fts_expression(query: &Query) -> Option<String> {
    let mut all_words: Vec<&str> = Vec::new();
    let mut phrases: Vec<String> = Vec::new();
    for clause in &query.clauses {
        match clause {
            Clause::Word(text) => all_words.push(text),
            Clause::Phrase(text) => phrases.push(quote(text)),
            _ => {}
        }
    }
    let mut words: Vec<&str> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for word in all_words.iter().copied().filter(|word| !is_stopword(word)) {
        let key = word.to_lowercase();
        if !seen.contains(&key) {
            seen.push(key);
            words.push(word);
        }
    }
    if words.is_empty() && phrases.is_empty() {
        words = all_words;
    }
    let alternatives: Vec<String> = words.into_iter().map(quote).collect();
    let mut parts = Vec::new();
    if !alternatives.is_empty() {
        let joined = alternatives.join(" OR ");
        let grouped = alternatives.len() > 1 && !phrases.is_empty();
        parts.push(if grouped {
            format!("({joined})")
        } else {
            joined
        });
    }
    parts.extend(phrases);
    (!parts.is_empty()).then(|| parts.join(" AND "))
}

/// Compiles the text clauses with every term required (AND): words and phrases alike,
/// no stopword removal. Used where text narrows a list, such as the `topic` filter of
/// `list_entries`, rather than ranking a question. Escaping is as for [`fts_expression`].
#[must_use]
pub fn fts_expression_all(query: &Query) -> Option<String> {
    let terms: Vec<_> = query
        .text_clauses()
        .filter_map(|clause| match clause {
            Clause::Word(text) | Clause::Phrase(text) => Some(quote(text)),
            _ => None,
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// Splits a branch or task description into lowercase literal terms, removes initial
/// git prefixes and the stopword list ([`is_stopword`]), and deduplicates while
/// preserving order. No filter syntax is interpreted. An input with no useful terms
/// yields an empty AST.
#[must_use]
pub fn context_query(text: &str) -> Query {
    let mut words = Vec::new();
    for (index, word) in text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .enumerate()
    {
        let word = word.to_lowercase();
        if (index == 0
            && matches!(
                word.as_str(),
                "feat"
                    | "feature"
                    | "fix"
                    | "bugfix"
                    | "hotfix"
                    | "chore"
                    | "docs"
                    | "refactor"
                    | "test"
                    | "build"
                    | "release"
            ))
            || is_stopword(&word)
            || words.contains(&word)
        {
            continue;
        }
        words.push(word);
    }
    Query {
        clauses: words.into_iter().map(Clause::Word).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expression(input: &str) -> Option<String> {
        fts_expression(&parse(input).unwrap())
    }

    #[test]
    fn bare_words_are_alternatives_and_phrases_are_required() {
        for (input, expected) in [
            ("sqlite", "\"sqlite\""),
            ("sqlite postgres", "\"sqlite\" OR \"postgres\""),
            ("\"full text\"", "\"full text\""),
            (
                "\"full text\" \"exact order\"",
                "\"full text\" AND \"exact order\"",
            ),
            ("sqlite \"full text\"", "\"sqlite\" AND \"full text\""),
            (
                "sqlite postgres \"full text\"",
                "(\"sqlite\" OR \"postgres\") AND \"full text\"",
            ),
        ] {
            assert_eq!(expression(input).as_deref(), Some(expected), "{input:?}");
        }
        // Filters never reach the FTS expression.
        assert_eq!(expression("from:robin in:dev after:2026-01-01"), None);
        assert_eq!(
            expression("from:robin sqlite postgres").as_deref(),
            Some("\"sqlite\" OR \"postgres\"")
        );
    }

    #[test]
    fn natural_questions_drop_stopwords_and_repeats() {
        assert_eq!(
            expression("What database did we finally pick?").as_deref(),
            Some("\"database\" OR \"finally\" OR \"pick?\"")
        );
        assert_eq!(
            expression("SQLite sqlite SQLITE is the answer").as_deref(),
            Some("\"SQLite\" OR \"answer\"")
        );
    }

    #[test]
    fn stopword_only_queries_fall_back_to_the_original_words() {
        assert_eq!(
            expression("what did we").as_deref(),
            Some("\"what\" OR \"did\" OR \"we\"")
        );
        assert_eq!(expression("the").as_deref(), Some("\"the\""));
        // Next to a phrase the phrase constrains the result, so the filler is dropped.
        assert_eq!(
            expression("the \"full text\"").as_deref(),
            Some("\"full text\"")
        );
    }

    #[test]
    fn fts_literals_cannot_inject_operators_or_columns() {
        for (input, expected) in [
            // `OR` and `AND` are stopwords here; the operators in the output are ours.
            ("sqlite OR postgres", "\"sqlite\" OR \"postgres\""),
            ("sqlite AND postgres", "\"sqlite\" OR \"postgres\""),
            (
                "sqlite NOT postgres",
                "\"sqlite\" OR \"NOT\" OR \"postgres\"",
            ),
            ("a NEAR b", "\"NEAR\" OR \"b\""),
            (
                "title:secret - * ( )",
                "\"title:secret\" OR \"-\" OR \"*\" OR \"(\" OR \")\"",
            ),
            ("{title tags}:secret", "\"{title\" OR \"tags}:secret\""),
            ("^secret sqlite*", "\"^secret\" OR \"sqlite*\""),
            (
                "\"a \"\" OR content:secret\"",
                "\"a \"\" OR content:secret\"",
            ),
            ("\"x\" OR \"y\"", "\"x\" AND \"y\""),
            ("epub-reader.toc", "\"epub-reader.toc\""),
            ("hello\0world", "\"hello world\""),
        ] {
            assert_eq!(expression(input).as_deref(), Some(expected), "{input:?}");
        }
        assert_eq!(
            fts_expression(&Query {
                clauses: vec![Clause::Word("\" OR *".to_owned())]
            })
            .as_deref(),
            Some("\"\"\" OR *\"")
        );
    }

    #[test]
    fn every_term_is_a_closed_quoted_literal() {
        // Outside the quoted literals only AND, OR and parentheses may remain.
        for input in [
            "\"a\"\"b\" c\"\"d",
            "a OR b AND c NOT d",
            "col:val (a OR b) *",
            "x \"y z\" \"\"\" w",
            "\"q\" NEAR(a b)",
        ] {
            let Ok(query) = parse(input) else { continue };
            let expression = fts_expression(&query).unwrap();
            let mut inside = false;
            let mut outside = String::new();
            let mut chars = expression.chars().peekable();
            while let Some(c) = chars.next() {
                match (c, inside) {
                    ('"', false) => inside = true,
                    ('"', true) if chars.peek() == Some(&'"') => {
                        chars.next();
                    }
                    ('"', true) => inside = false,
                    (_, false) => outside.push(c),
                    _ => {}
                }
            }
            assert!(!inside, "unterminated literal in {expression}");
            let leftover: Vec<_> = outside
                .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
                .filter(|token| !token.is_empty() && *token != "AND" && *token != "OR")
                .collect();
            assert!(leftover.is_empty(), "{expression}: {leftover:?}");
        }
    }

    #[test]
    fn all_terms_expression_keeps_every_term_required() {
        let query = parse("the sqlite \"full text\"").unwrap();
        assert_eq!(
            fts_expression_all(&query).as_deref(),
            Some("\"the\" AND \"sqlite\" AND \"full text\"")
        );
        assert_eq!(fts_expression_all(&parse("from:robin").unwrap()), None);
    }

    #[test]
    fn branch_and_description_terms_are_literal_and_ordered() {
        for (input, expected) in [
            ("feat/epub-reader-toc", vec!["epub", "reader", "toc"]),
            ("fix/SQLite_and-the-SQLite", vec!["sqlite"]),
            (
                "How to search from:Robin in:#planning",
                vec!["search", "robin", "planning"],
            ),
            (
                "What database did we finally pick?",
                vec!["database", "finally", "pick"],
            ),
            ("---", vec![]),
        ] {
            assert_eq!(
                context_query(input).clauses,
                expected
                    .into_iter()
                    .map(|s| Clause::Word(s.to_owned()))
                    .collect::<Vec<_>>()
            );
        }
    }
}
