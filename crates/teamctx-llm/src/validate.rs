//! Output validation (plan section 11, step 4): the model's text is untrusted too.
//!
//! - Citation markers (`[m:12]`, `[D-3]`) are kept only when the id is in the source
//!   bundle. Invalid ones are removed and counted. A marker used as a Markdown link
//!   (`[m:12](https://evil.example)`) loses its link target.
//! - URLs that do not appear in the sources are replaced with a placeholder.

use std::collections::HashSet;

use teamctx_core::citation::CitationId;

/// What replaces a URL that is not in the sources.
pub(crate) const LINK_REMOVED: &str = "[link removed]";

/// The longest bracket content treated as a possible citation marker.
const MAX_MARKER_BYTES: usize = 128;
/// The longest parenthesised link target removed after a marker.
const MAX_TARGET_BYTES: usize = 2048;

/// The text after citation validation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct CitationCheck {
    /// The text with invalid markers removed.
    pub(crate) text: String,
    /// Valid cited ids in order of first appearance, without duplicates.
    pub(crate) cited: Vec<CitationId>,
    /// How many markers named an id that is not in the bundle.
    pub(crate) dropped: usize,
}

/// Validates every citation marker in `text` against `allowed`.
///
/// A bracket whose content is one or more ids separated by `,` or `;` is a marker.
/// Any other bracket is plain text and is left alone.
pub(crate) fn check_citations(text: &str, allowed: &HashSet<CitationId>) -> CitationCheck {
    let mut out = String::with_capacity(text.len());
    let mut cited = Vec::new();
    let mut dropped = 0;
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let marker = after
            .find([']', '['])
            .filter(|&close| after.as_bytes()[close] == b']' && close <= MAX_MARKER_BYTES)
            .and_then(|close| parse_marker(&after[..close]).map(|ids| (close, ids)));
        let Some((close, ids)) = marker else {
            out.push('[');
            rest = after;
            continue;
        };
        for id in ids {
            if allowed.contains(&id) {
                if !cited.contains(&id) {
                    cited.push(id);
                }
                out.push_str(&id.marker());
            } else {
                dropped += 1;
            }
        }
        rest = skip_link_target(&after[close + 1..]);
    }
    out.push_str(rest);
    CitationCheck {
        text: out,
        cited,
        dropped,
    }
}

/// Parses `m:1`, `D-2` or a list such as `m:1, D-2`. `None` if any part is not an id.
fn parse_marker(inner: &str) -> Option<Vec<CitationId>> {
    let ids: Option<Vec<_>> = inner
        .split([',', ';'])
        .map(|part| part.trim().parse::<CitationId>().ok())
        .collect();
    ids.filter(|ids| !ids.is_empty())
}

/// Drops a Markdown link target such as `(https://...)` directly after a marker.
fn skip_link_target(rest: &str) -> &str {
    if let Some(target) = rest.strip_prefix('(')
        && let Some(close) = target.find([')', '(', '\n'])
        && target.as_bytes()[close] == b')'
        && close <= MAX_TARGET_BYTES
    {
        return &target[close + 1..];
    }
    rest
}

/// Byte ranges of the URLs in `text`: `http://`, `https://` (any case) and `www.`.
fn url_ranges(text: &str) -> Vec<(usize, usize)> {
    let lower = text.to_ascii_lowercase();
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for prefix in ["https://", "http://", "www."] {
        for (start, _) in lower.match_indices(prefix) {
            if ranges.iter().any(|&(s, e)| start >= s && start < e) {
                continue;
            }
            let boundary_ok = text[..start]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '.' && c != '/');
            if !boundary_ok {
                continue;
            }
            let end = start
                + text[start..]
                    .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '`' | '|'))
                    .unwrap_or(text.len() - start);
            let trimmed = text[start..end].trim_end_matches([
                '.', ',', ';', ':', '!', '?', ')', ']', '}', '\'', '*', '_', '~',
            ]);
            if trimmed.len() > prefix.len() {
                ranges.push((start, start + trimmed.len()));
            }
        }
    }
    ranges.sort_unstable();
    ranges
}

fn normalise_url(url: &str) -> String {
    url.trim_end_matches('/').to_ascii_lowercase()
}

/// The set of URLs that appear in `texts`, normalised for comparison.
pub(crate) fn urls_in<'a>(texts: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    texts
        .into_iter()
        .flat_map(|text| {
            url_ranges(text)
                .into_iter()
                .map(|(start, end)| normalise_url(&text[start..end]))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Replaces every URL in `text` that is not in `allowed` with [`LINK_REMOVED`].
/// Returns the new text and how many URLs were removed.
pub(crate) fn strip_unknown_urls(text: &str, allowed: &HashSet<String>) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    let mut last = 0;
    for (start, end) in url_ranges(text) {
        if start < last {
            continue;
        }
        out.push_str(&text[last..start]);
        if allowed.contains(&normalise_url(&text[start..end])) {
            out.push_str(&text[start..end]);
        } else {
            out.push_str(LINK_REMOVED);
            removed += 1;
        }
        last = end;
    }
    out.push_str(&text[last..]);
    (out, removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> CitationId {
        text.parse().unwrap()
    }

    fn allowed(ids: &[&str]) -> HashSet<CitationId> {
        ids.iter().map(|t| id(t)).collect()
    }

    #[test]
    fn valid_markers_stay_and_are_collected_in_order() {
        let check = check_citations(
            "Serif [D-3] was chosen [m:12], again [D-3].",
            &allowed(&["D-3", "m:12"]),
        );
        assert_eq!(check.text, "Serif [D-3] was chosen [m:12], again [D-3].");
        assert_eq!(check.cited, [id("D-3"), id("m:12")]);
        assert_eq!(check.dropped, 0);
    }

    #[test]
    fn markers_naming_unknown_ids_are_removed_and_counted() {
        let check = check_citations("A [m:999] B [D-3] C [T-1]", &allowed(&["D-3"]));
        assert_eq!(check.text, "A  B [D-3] C ");
        assert_eq!(check.cited, [id("D-3")]);
        assert_eq!(check.dropped, 2);
    }

    #[test]
    fn lists_are_split_and_each_id_is_checked() {
        let check = check_citations("x [m:1, m:2; D-9] y", &allowed(&["m:1", "D-9"]));
        assert_eq!(check.text, "x [m:1][D-9] y");
        assert_eq!(check.cited, [id("m:1"), id("D-9")]);
        assert_eq!(check.dropped, 1);
    }

    #[test]
    fn other_brackets_and_malformed_ids_are_plain_text() {
        let text = "see [the notes] and [m:01] and [D-0] and [ m:1 x] and [[m:1]";
        let check = check_citations(text, &allowed(&["m:1"]));
        // `[m:1]` inside `[[m:1]` is a real marker; everything else is untouched.
        assert_eq!(
            check.text,
            "see [the notes] and [m:01] and [D-0] and [ m:1 x] and [[m:1]"
        );
        assert_eq!(check.cited, [id("m:1")]);
        assert_eq!(check.dropped, 0);
    }

    #[test]
    fn a_marker_cannot_be_a_markdown_link() {
        let check = check_citations(
            "ok [m:1](https://evil.example/x) and [m:7](javascript:alert(1)) end",
            &allowed(&["m:1"]),
        );
        // With the invalid marker gone no link syntax remains, so the leftover
        // parenthesis is plain text.
        assert_eq!(check.text, "ok [m:1] and (javascript:alert(1)) end");
        assert!(!check.text.contains("evil"));
        // A parenthesis that is not a link target stays when the marker is valid.
        let check = check_citations("[m:1] (aside)", &allowed(&["m:1"]));
        assert_eq!(check.text, "[m:1] (aside)");
    }

    #[test]
    fn unbalanced_input_never_panics_or_loops() {
        for text in [
            "[",
            "]",
            "[[[",
            "[m:1",
            "[m:1](",
            "([m:1])",
            "[\u{1F600}]",
            "",
        ] {
            let _ = check_citations(text, &allowed(&["m:1"]));
        }
        let long = format!("[{}]", "m:1,".repeat(200));
        let check = check_citations(&long, &allowed(&["m:1"]));
        assert_eq!(check.text, long);
    }

    #[test]
    fn urls_not_in_the_sources_are_removed() {
        let known = urls_in(["docs at https://example.com/docs/fonts, and www.example.org"]);
        let (text, removed) = strip_unknown_urls(
            "See https://example.com/docs/fonts. Also HTTP://EVIL.example/x?a=1, www.example.org and www.evil.example/y.",
            &known,
        );
        assert_eq!(
            text,
            format!(
                "See https://example.com/docs/fonts. Also {LINK_REMOVED}, www.example.org and {LINK_REMOVED}."
            )
        );
        assert_eq!(removed, 2);
    }

    #[test]
    fn markdown_links_and_angle_brackets_cannot_hide_a_url() {
        let known = urls_in(["https://example.com/a"]);
        let (text, removed) = strip_unknown_urls(
            "[click](https://evil.example/p) <https://evil.example/q> `https://evil.example/r` \"https://evil.example/s\" (https://example.com/a)",
            &known,
        );
        assert!(!text.contains("evil"), "{text}");
        assert!(text.contains("https://example.com/a"));
        assert_eq!(removed, 4);
    }

    #[test]
    fn urls_match_ignoring_case_and_trailing_slash_only() {
        let known = urls_in(["https://Example.com/Docs/"]);
        let (text, removed) = strip_unknown_urls(
            "https://example.com/docs and https://example.com/docs/x",
            &known,
        );
        assert_eq!(removed, 1);
        assert!(text.starts_with("https://example.com/docs and "));
    }

    #[test]
    fn words_that_merely_end_in_www_or_http_are_not_urls() {
        let (text, removed) = strip_unknown_urls(
            "awww.bar nothttp://x.y and 'https://' alone",
            &HashSet::new(),
        );
        assert_eq!(removed, 0, "{text}");
        assert!(text.contains("awww.bar") && text.contains("nothttp://x.y"));
    }

    #[test]
    fn no_urls_means_no_changes() {
        let (text, removed) = strip_unknown_urls("plain text, with: punctuation.", &HashSet::new());
        assert_eq!(text, "plain text, with: punctuation.");
        assert_eq!(removed, 0);
    }
}
