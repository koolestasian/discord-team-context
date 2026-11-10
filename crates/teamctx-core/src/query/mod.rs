//! Search query syntax. Filters (`from:`, `in:`, `before:`, `after:`) and quoted phrases
//! are required (AND), including repeated filters; bare words are alternatives (OR) that
//! rank higher the more of them match. Date boundaries are resolved outside core at
//! midnight in `server.timezone`: `after` inclusive, `before` exclusive. Bind filters as
//! SQL parameters separately.
//!
//! Size limits (all query text is untrusted): [`parse`] rejects input over
//! [`MAX_QUERY_BYTES`] bytes and more than [`MAX_CLAUSES`] clauses. The compilers below
//! use at most [`MAX_CLAUSES`] terms even for a hand-built [`Query`], so no input can
//! make the FTS expression, or the work to build it, grow without bound.

use std::collections::HashSet;

pub mod ast;
mod parser;
mod stopwords;
pub use ast::{Clause, Query};
pub use parser::{MAX_CLAUSES, MAX_QUERY_BYTES, ParseQueryError, parse};
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
///
/// Size: at most [`MAX_CLAUSES`] terms are used, so a hand-built [`Query`] that never
/// went through [`parse`] still compiles to a bounded expression. Phrases are required,
/// so they are kept first, in order; the first distinct bare words fill the rest. A
/// parsed query has at most [`MAX_CLAUSES`] clauses, so nothing it contains is dropped.
#[must_use]
pub fn fts_expression(query: &Query) -> Option<String> {
    let phrases: Vec<String> = query
        .clauses
        .iter()
        .filter_map(|clause| match clause {
            Clause::Phrase(text) => Some(quote(text)),
            _ => None,
        })
        .take(MAX_CLAUSES)
        .collect();
    let all_words = || {
        query.clauses.iter().filter_map(|clause| match clause {
            Clause::Word(text) => Some(text.as_str()),
            _ => None,
        })
    };
    let budget = MAX_CLAUSES - phrases.len();
    let mut words: Vec<&str> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for word in all_words().filter(|word| !is_stopword(word)) {
        if words.len() == budget {
            break;
        }
        if seen.insert(word.to_lowercase()) {
            words.push(word);
        }
    }
    if words.is_empty() && phrases.is_empty() {
        words = all_words().take(MAX_CLAUSES).collect();
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
/// Only the first [`MAX_CLAUSES`] terms are used, so the expression stays bounded.
#[must_use]
pub fn fts_expression_all(query: &Query) -> Option<String> {
    let terms: Vec<_> = query
        .text_clauses()
        .filter_map(|clause| match clause {
            Clause::Word(text) | Clause::Phrase(text) => Some(quote(text)),
            _ => None,
        })
        .take(MAX_CLAUSES)
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// Splits a branch or task description into lowercase literal terms, removes initial
/// git prefixes and the stopword list ([`is_stopword`]), and deduplicates while
/// preserving order. No filter syntax is interpreted. An input with no useful terms
/// yields an empty AST. At most the first [`MAX_CLAUSES`] distinct terms are kept, and
/// reading stops there, so long text costs time linear in its length at most.
#[must_use]
pub fn context_query(text: &str) -> Query {
    let mut words = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
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
            || seen.contains(&word)
        {
            continue;
        }
        seen.insert(word.clone());
        words.push(word);
        if words.len() == MAX_CLAUSES {
            break;
        }
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

    /// Splits an expression into its quoted literals, unescaped.
    fn literals(expression: &str) -> Vec<String> {
        let mut found = Vec::new();
        let mut current: Option<String> = None;
        let mut chars = expression.chars().peekable();
        while let Some(c) = chars.next() {
            match (&mut current, c) {
                (None, '"') => current = Some(String::new()),
                (Some(text), '"') if chars.peek() == Some(&'"') => {
                    chars.next();
                    text.push('"');
                }
                (Some(_), '"') => found.extend(current.take()),
                (Some(text), c) => text.push(c),
                (None, _) => {}
            }
        }
        found
    }

    fn many(count: usize, make: impl Fn(usize) -> Clause) -> Query {
        Query {
            clauses: (0..count).map(make).collect(),
        }
    }

    #[test]
    fn hand_built_queries_compile_to_a_bounded_expression() {
        // 100,000 distinct words used to take seconds (linear de-duplication) and built
        // a 100,000-term expression. Now the first MAX_CLAUSES distinct words are kept.
        let query = many(100_000, |i| Clause::Word(format!("w{i}")));
        let expression = fts_expression(&query).unwrap();
        let terms = literals(&expression);
        assert_eq!(terms.len(), MAX_CLAUSES);
        assert_eq!(terms[0], "w0");
        assert_eq!(terms[MAX_CLAUSES - 1], format!("w{}", MAX_CLAUSES - 1));

        // Repeats (ignoring case) and stopwords do not use up the budget.
        let mut clauses = vec![Clause::Word("the".to_owned()); 50_000];
        clauses.extend((0..50_000).map(|i| Clause::Word(["W", "w"][i % 2].to_owned())));
        clauses.extend((0..100).map(|i| Clause::Word(format!("x{i}"))));
        let terms = literals(&fts_expression(&Query { clauses }).unwrap());
        assert_eq!(terms.len(), MAX_CLAUSES);
        assert_eq!(terms[0], "W");
        assert_eq!(terms[1], "x0");

        // Stopword-only fallback is bounded too.
        let query = many(100_000, |_| Clause::Word("the".to_owned()));
        assert_eq!(
            literals(&fts_expression(&query).unwrap()).len(),
            MAX_CLAUSES
        );

        // Phrases are required, so they take the budget first; words fill the rest.
        let query = many(100_000, |i| Clause::Phrase(format!("p {i}")));
        let terms = literals(&fts_expression(&query).unwrap());
        assert_eq!(terms.len(), MAX_CLAUSES);
        assert_eq!(terms[0], "p 0");
        let mut clauses = vec![Clause::Phrase("full text".to_owned())];
        clauses.extend((0..100_000).map(|i| Clause::Word(format!("w{i}"))));
        let expression = fts_expression(&Query { clauses }).unwrap();
        let terms = literals(&expression);
        assert_eq!(terms.len(), MAX_CLAUSES);
        assert!(expression.ends_with(" AND \"full text\""), "{expression}");

        // The all-terms form is bounded the same way.
        let query = many(100_000, |i| Clause::Word(format!("w{i}")));
        let terms = literals(&fts_expression_all(&query).unwrap());
        assert_eq!(terms.len(), MAX_CLAUSES);
        assert_eq!(terms[0], "w0");
    }

    #[test]
    fn parsed_queries_are_never_truncated() {
        // Anything `parse` accepts fits the term cap, so search results never change.
        let words: Vec<String> = (0..MAX_CLAUSES).map(|i| format!("w{i}")).collect();
        let query = parse(&words.join(" ")).unwrap();
        assert_eq!(literals(&fts_expression(&query).unwrap()), words);
        assert_eq!(literals(&fts_expression_all(&query).unwrap()), words);
    }

    #[test]
    fn context_query_keeps_at_most_the_term_cap() {
        let text: String = (0..100_000).map(|i| format!("w{i} ")).collect();
        let clauses = context_query(&text).clauses;
        assert_eq!(clauses.len(), MAX_CLAUSES);
        assert_eq!(clauses[0], Clause::Word("w0".to_owned()));
        assert_eq!(
            clauses[MAX_CLAUSES - 1],
            Clause::Word(format!("w{}", MAX_CLAUSES - 1))
        );
        // Repeats and stopwords are skipped without using up the cap.
        let text = format!("{}{}", "the Same same ".repeat(50_000), "unique");
        assert_eq!(
            context_query(&text).clauses,
            [
                Clause::Word("same".to_owned()),
                Clause::Word("unique".to_owned())
            ]
        );
    }
}
