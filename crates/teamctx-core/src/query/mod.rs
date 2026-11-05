//! Search query syntax. Clauses combine with AND, including repeated filters.
//! Date boundaries are resolved outside core at midnight in `server.timezone`:
//! `after` inclusive, `before` exclusive. Bind filters as SQL parameters separately.

pub mod ast;
mod parser;
pub use ast::{Clause, Query};
pub use parser::{ParseQueryError, parse};

/// Quotes every text literal for binding as one FTS5 MATCH parameter, never SQL.
/// `None` means no text clauses. Operators and punctuation always remain literals.
/// NUL is replaced with a space because SQLite terminates FTS expressions at NUL.
#[must_use]
pub fn fts_expression(query: &Query) -> Option<String> {
    let terms: Vec<_> = query
        .text_clauses()
        .filter_map(|clause| match clause {
            Clause::Word(text) | Clause::Phrase(text) => Some(format!(
                "\"{}\"",
                text.replace('\0', " ").replace('"', "\"\"")
            )),
            _ => None,
        })
        .collect();
    (!terms.is_empty()).then(|| terms.join(" AND "))
}

/// Splits a branch or task description into lowercase literal terms, removes initial
/// git prefixes and a fixed stopword list, and deduplicates while preserving order.
/// No filter syntax is interpreted. An input with no useful terms yields an empty AST.
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
            || matches!(
                word.as_str(),
                "a" | "an"
                    | "and"
                    | "are"
                    | "as"
                    | "at"
                    | "be"
                    | "by"
                    | "for"
                    | "from"
                    | "in"
                    | "is"
                    | "it"
                    | "of"
                    | "on"
                    | "or"
                    | "the"
                    | "to"
                    | "with"
            )
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

    #[test]
    fn fts_literals_cannot_inject_operators_or_columns() {
        for (input, expected) in [
            (
                "sqlite OR postgres",
                "\"sqlite\" AND \"OR\" AND \"postgres\"",
            ),
            (
                "title:secret - * ( )",
                "\"title:secret\" AND \"-\" AND \"*\" AND \"(\" AND \")\"",
            ),
            (
                "\"a \"\" OR content:secret\"",
                "\"a \"\" OR content:secret\"",
            ),
            ("epub-reader.toc", "\"epub-reader.toc\""),
            ("hello\0world", "\"hello world\""),
        ] {
            assert_eq!(
                fts_expression(&parse(input).unwrap()).as_deref(),
                Some(expected)
            );
        }
        assert_eq!(fts_expression(&parse("from:robin").unwrap()), None);
        assert_eq!(
            fts_expression(&Query {
                clauses: vec![Clause::Word("\" OR *".to_owned())]
            })
            .as_deref(),
            Some("\"\"\" OR *\"")
        );
    }

    #[test]
    fn branch_and_description_terms_are_literal_and_ordered() {
        for (input, expected) in [
            ("feat/epub-reader-toc", vec!["epub", "reader", "toc"]),
            ("fix/SQLite_and-the-SQLite", vec!["sqlite"]),
            (
                "How to search from:Robin in:#planning",
                vec!["how", "search", "robin", "planning"],
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
