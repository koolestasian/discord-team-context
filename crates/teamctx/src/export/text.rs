//! Text helpers for the export. Everything a person wrote in chat (titles, names, channel
//! names) is untrusted: it is escaped before it reaches a file name, a YAML value or
//! Markdown, so it can neither leave the output directory nor forge structure.

/// The longest slug in a file name, in characters (all ASCII).
const MAX_SLUG: usize = 48;

/// Characters that are invisible or change how text is laid out: control characters, line
/// and paragraph separators, zero-width and bidirectional formatting characters, and the
/// byte-order mark. They are escaped in YAML and dropped from Markdown.
fn is_unsafe(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}'
        )
}

/// A lowercase ASCII slug of `text` for a file name: letters and digits, single hyphens
/// between words, no leading or trailing hyphen, at most 48 characters. Anything else,
/// including non-ASCII letters, ends a word. The result can be empty.
///
/// The output alphabet is `[a-z0-9-]`, so a slug can never contain a path separator, a
/// dot, a drive letter, or a reserved device name.
pub(super) fn slug(text: &str) -> String {
    let mut out = String::new();
    let mut pending_hyphen = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() {
            if pending_hyphen && !out.is_empty() {
                out.push('-');
            }
            pending_hyphen = false;
            out.push(c.to_ascii_lowercase());
        } else {
            pending_hyphen = true;
        }
    }
    out.truncate(MAX_SLUG);
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// `text` as a YAML double-quoted scalar. Quotes, backslashes and every unsafe character
/// are escaped, so the value stays on one line and cannot end the string early.
pub(super) fn yaml_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if is_unsafe(c) => {
                // All unsafe characters are in the Basic Multilingual Plane.
                out.push_str(&format!("\\u{:04X}", u32::from(c)));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `text` as one line of Markdown that renders as the same plain text: whitespace runs
/// (including newlines) become one space, invisible characters are dropped, and the
/// characters Markdown treats specially are backslash-escaped. A URL scheme separator is
/// broken up too, so chat text cannot turn into a clickable link.
pub(super) fn md_text(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if is_unsafe(c) { ' ' } else { c })
        .collect();
    let line = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = String::with_capacity(line.len());
    for c in line.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '#' | '|' | '~' | '&' | '!'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out.replace("://", ":\\/\\/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_use_only_lowercase_ascii_and_single_hyphens() {
        let cases = [
            ("Use SQLite for storage", "use-sqlite-for-storage"),
            ("  --Already--hyphenated--  ", "already-hyphenated"),
            ("Ship v0.1 by Friday!", "ship-v0-1-by-friday"),
            ("Café menu", "caf-menu"),
            ("日本語", ""),
            ("", ""),
            ("../../etc/passwd", "etc-passwd"),
            ("..\\..\\windows\\system32", "windows-system32"),
            ("C:\\Users\\x", "c-users-x"),
            ("CON", "con"),
            ("a\0b\nc", "a-b-c"),
            ("%2e%2e%2f", "2e-2e-2f"),
        ];
        for (input, expected) in cases {
            assert_eq!(slug(input), expected, "{input:?}");
        }
    }

    #[test]
    fn slugs_are_bounded_and_never_end_in_a_hyphen() {
        let long = "word ".repeat(40);
        let slugged = slug(&long);
        assert!(slugged.len() <= MAX_SLUG, "{slugged}");
        assert!(!slugged.ends_with('-') && !slugged.starts_with('-'));
        // Cutting at the limit lands on a hyphen: it is removed again.
        let edge = format!("{}-tail", "a".repeat(MAX_SLUG - 1));
        assert_eq!(slug(&edge), "a".repeat(MAX_SLUG - 1));
        for input in ["x/../y", "a b", "ÀÉÎ", "9", "\u{202E}evil"] {
            assert!(
                slug(input)
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{input:?}"
            );
        }
    }

    #[test]
    fn yaml_strings_stay_on_one_line_and_cannot_close_early() {
        assert_eq!(yaml_string("Use SQLite"), "\"Use SQLite\"");
        assert_eq!(yaml_string("say \"hi\" \\ bye"), r#""say \"hi\" \\ bye""#);
        assert_eq!(yaml_string("a\nb\r\tc"), r#""a\nb\r\tc""#);
        assert_eq!(yaml_string("nul\0bell\u{7}"), "\"nul\\u0000bell\\u0007\"");
        assert_eq!(
            yaml_string("a\u{2028}b\u{202E}c\u{FEFF}"),
            "\"a\\u2028b\\u202Ec\\uFEFF\""
        );
        let hostile = "x\"\nsources:\n  - id: forged\n---\n";
        let value = yaml_string(hostile);
        assert_eq!(value.lines().count(), 1, "{value}");
        assert!(value.starts_with('"') && value.ends_with('"'));
        assert_eq!(yaml_string("café 日本語 🙂"), "\"café 日本語 🙂\"");
    }

    #[test]
    fn markdown_text_is_one_line_of_plain_text() {
        assert_eq!(md_text("Use SQLite (again)"), "Use SQLite (again)");
        assert_eq!(md_text("  two\n\nlines\tand   gaps "), "two lines and gaps");
        assert_eq!(md_text("a\u{202E}b\u{0}c"), "a b c");
        assert_eq!(
            md_text("[click](https://evil.example)"),
            "\\[click\\](https:\\/\\/evil.example)"
        );
        assert_eq!(md_text("# heading"), "\\# heading");
        assert_eq!(
            md_text("*bold* _it_ `code` ~~s~~"),
            "\\*bold\\* \\_it\\_ \\`code\\` \\~\\~s\\~\\~"
        );
        assert_eq!(
            md_text("<script>x</script> & ![i]"),
            "\\<script\\>x\\</script\\> \\& \\!\\[i\\]"
        );
        assert_eq!(md_text("a | b"), "a \\| b");
        assert_eq!(md_text("back\\slash"), "back\\\\slash");
        assert!(!md_text("line1\nline2").contains('\n'));
        assert_eq!(md_text(""), "");
    }
}
