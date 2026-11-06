//! Output validation (plan section 11, step 4): the model's text is untrusted too.
//!
//! - Citation markers (`[m:12]`, `[D-3]`) are kept only when the id is in the source
//!   bundle. Invalid ones are removed and counted. A marker used as a Markdown link
//!   (`[m:12](https://evil.example)`) loses its link target.
//! - Links and URLs that do not appear in the sources are removed, by a small
//!   hand-written scanner described below.
//!
//! # Links
//!
//! The answer is shown as Markdown (Discord, and possibly other renderers), so every
//! way Markdown can turn text into a link is handled, in this order:
//!
//! 1. **Inline links and images**, `[text](destination "title")` and `![alt](src)`,
//!    with nested brackets, `<...>` destinations, balanced parentheses and titles. The
//!    link becomes its visible text. If the destination is allowed it follows as plain
//!    text, `text (https://...)`, so no link text can disguise where it goes; images
//!    never load. Otherwise only the text is kept. A target holding `[` or `]`, or
//!    longer than 2 KiB, is not accepted and is left to step 4.
//! 2. **Autolinks** `<scheme:...>` of any scheme and `<user@host>` become the allowed
//!    URL or a placeholder. **Reference definitions** `[label]: destination` at the
//!    start of a line (also after quote or list markers) lose their brackets and their
//!    destination becomes the allowed URL or a placeholder, so `[text][label]` and
//!    `[label]` never resolve.
//! 3. **Bare URLs**: `http://`, `https://` and `ftp://` anywhere (even glued to a word),
//!    `www.` at a word start, and `javascript:`, `vbscript:`, `data:` and `file:`
//!    tokens. They are found in a decoded view of the text: backslash escapes and HTML
//!    character references (numeric, and the named ones for ASCII punctuation) are
//!    decoded, and the scheme separator may be percent-encoded (`%3A`, `%2F`) or use
//!    backslashes. Each is kept only if allowed, otherwise it becomes a placeholder.
//! 4. Whatever link syntax is left, such as a link the scanner did not accept, is
//!    broken up: `](` becomes `] (` and `]:` becomes `] :`.
//!
//! **Allowed** means: written plainly (no escapes or character references), an
//! `http`, `https` or `www.` URL, and equal to a URL found in the sources after
//! normalisation. Normalisation lowercases only the scheme and host, drops a default
//! port and treats an empty path as `/`; the path, query, fragment, user info and any
//! trailing slash on a path keep their exact case and form. Every other destination,
//! including relative, protocol-relative (`//host`) and non-web schemes, is removed
//! even if it appears in the sources.
//!
//! Out of scope: raw HTML (a renderer must escape it), bare e-mail addresses, and
//! look-alike hosts that appear verbatim in the sources (the sources are allowed by
//! design, plan section 11). Code spans are scanned like any other text.

use std::collections::HashSet;

use teamctx_core::citation::CitationId;

/// What replaces a URL that is not in the sources.
pub(crate) const LINK_REMOVED: &str = "[link removed]";

/// The longest bracket content treated as a possible citation marker.
const MAX_MARKER_BYTES: usize = 128;
/// The longest link target (or autolink, or definition label) the scanner reads.
const MAX_TARGET_BYTES: usize = 2048;
/// The most `[` kept open at once while matching link text; more are plain text.
const MAX_OPEN_BRACKETS: usize = 64;

/// Schemes that renderers link when written bare, followed by `:` and slashes.
const WEB_SCHEMES: [&str; 3] = ["https", "http", "ftp"];
/// Schemes that run code or carry content. Removed wherever they start a token.
const SCRIPT_SCHEMES: [&str; 4] = ["javascript", "vbscript", "data", "file"];
/// Characters that end a bare URL.
const URL_END: [char; 5] = ['<', '>', '"', '`', '|'];
/// Characters trimmed from the end of a bare URL (`)` only when unbalanced).
const URL_TRAILING: [char; 12] = ['.', ',', ';', ':', '!', '?', ']', '}', '\'', '*', '_', '~'];
/// Named character references decoded when looking for URLs: the ones for ASCII
/// punctuation and the whitespace a URL could be split with.
const NAMED_REFERENCES: [(&str, char); 30] = [
    ("amp", '&'),
    ("apos", '\''),
    ("ast", '*'),
    ("bsol", '\\'),
    ("colon", ':'),
    ("comma", ','),
    ("commat", '@'),
    ("dollar", '$'),
    ("equals", '='),
    ("excl", '!'),
    ("grave", '`'),
    ("gt", '>'),
    ("lbrack", '['),
    ("lowbar", '_'),
    ("lpar", '('),
    ("lsqb", '['),
    ("lt", '<'),
    ("nbsp", '\u{a0}'),
    ("NewLine", '\n'),
    ("num", '#'),
    ("percnt", '%'),
    ("period", '.'),
    ("plus", '+'),
    ("quest", '?'),
    ("quot", '"'),
    ("rbrack", ']'),
    ("rpar", ')'),
    ("rsqb", ']'),
    ("sol", '/'),
    ("Tab", '\t'),
];

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

/// The set of URLs that appear in `texts`, normalised for comparison. Only plainly
/// written web URLs are collected (see the module docs).
pub(crate) fn urls_in<'a>(texts: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    texts
        .into_iter()
        .flat_map(|text| {
            url_ranges(text)
                .into_iter()
                .filter_map(|(start, end)| allowed_key(&text[start..end]))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Removes every link and URL in `text` that is not in `allowed` (see the module docs).
/// Returns the new text and how many link targets or URLs were removed.
pub(crate) fn strip_unknown_urls(text: &str, allowed: &HashSet<String>) -> (String, usize) {
    let (text, inline) = flatten_inline_links(text, allowed);
    let (text, other) = replace_autolinks_and_definitions(&text, allowed);
    let (text, bare) = replace_bare_urls(&text, allowed);
    (break_link_syntax(&text), inline + other + bare)
}

// ---- allow-list ----

/// The comparison key for a URL that may be allowed: `None` unless it is a plainly
/// written web URL.
fn allowed_key(raw: &str) -> Option<String> {
    if decode(raw).iter().all(|d| d.is_literal()) {
        normalise_url(raw)
    } else {
        None
    }
}

fn is_allowed(raw: &str, allowed: &HashSet<String>) -> bool {
    allowed_key(raw).is_some_and(|key| allowed.contains(&key))
}

/// Writes `destination` if it is allowed, else the placeholder (and counts it).
fn push_link(out: &mut String, removed: &mut usize, destination: &str, allowed: &HashSet<String>) {
    if is_allowed(destination, allowed) {
        out.push_str(destination);
    } else {
        out.push_str(LINK_REMOVED);
        *removed += 1;
    }
}

/// Lowercases the scheme and host, drops a default port and writes an empty path as
/// `/`. Everything after the host keeps its case. `None` for anything that is not an
/// `http`, `https` or `www.` URL, or that holds whitespace, control characters or
/// backslashes (which browsers read as slashes).
fn normalise_url(url: &str) -> Option<String> {
    if url
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let (scheme, rest) = if let Some(rest) = strip_prefix_ignore_case(url, "https://") {
        ("https", rest)
    } else if let Some(rest) = strip_prefix_ignore_case(url, "http://") {
        ("http", rest)
    } else if strip_prefix_ignore_case(url, "www.").is_some() {
        ("http", url)
    } else {
        return None;
    };
    let (authority, tail) = rest.split_at(rest.find(['/', '?', '#']).unwrap_or(rest.len()));
    let (user_info, host) = authority
        .rfind('@')
        .map_or(("", authority), |at| authority.split_at(at + 1));
    let host = host.to_ascii_lowercase();
    let default_port = if scheme == "https" { ":443" } else { ":80" };
    let host = host.strip_suffix(default_port).unwrap_or(&host);
    if host.is_empty() {
        return None;
    }
    let slash = if tail.starts_with('/') { "" } else { "/" };
    Some(format!("{scheme}://{user_info}{host}{slash}{tail}"))
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

// ---- step 1: inline links and images ----

/// Replaces `[text](destination)` and `![alt](src)` with their text, followed by the
/// destination in parentheses when it is allowed.
fn flatten_inline_links(text: &str, allowed: &HashSet<String>) -> (String, usize) {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    // Where each open `[` (or `![`) starts in `out`, and its length.
    let mut open: Vec<(usize, usize)> = Vec::new();
    let mut removed = 0;
    let mut at = 0;
    while at < text.len() {
        match bytes[at] {
            b'\\' if bytes.get(at + 1).is_some_and(u8::is_ascii_punctuation) => {
                out.push_str(&text[at..at + 2]);
                at += 2;
            }
            b'!' if bytes.get(at + 1) == Some(&b'[') => {
                if open.len() < MAX_OPEN_BRACKETS {
                    open.push((out.len(), 2));
                }
                out.push_str("![");
                at += 2;
            }
            b'[' => {
                if open.len() < MAX_OPEN_BRACKETS {
                    open.push((out.len(), 1));
                }
                out.push('[');
                at += 1;
            }
            b']' => {
                at += 1;
                let link = open
                    .pop()
                    .and_then(|opener| inline_target(&text[at..]).map(|target| (opener, target)));
                if let Some(((start, syntax), (len, destination))) = link {
                    let label = out.split_off(start);
                    out.push_str(&label[syntax..]);
                    if is_allowed(destination, allowed) {
                        out.push_str(" (");
                        out.push_str(destination);
                        out.push(')');
                    } else {
                        removed += 1;
                    }
                    at += len;
                } else {
                    out.push(']');
                }
            }
            _ => {
                let end = text[at..]
                    .char_indices()
                    .skip(1)
                    .find(|&(_, c)| matches!(c, '\\' | '!' | '[' | ']'))
                    .map_or(text.len(), |(offset, _)| at + offset);
                out.push_str(&text[at..end]);
                at = end;
            }
        }
    }
    (out, removed)
}

/// Parses `(destination "title")` at the start of `rest`. Returns how many bytes it
/// spans and the destination without angle brackets.
fn inline_target(rest: &str) -> Option<(usize, &str)> {
    let bytes = rest.as_bytes();
    if bytes.first() != Some(&b'(') {
        return None;
    }
    // A target is never read past a bracket, so each `]` scans only up to the next one
    // and the whole pass stays linear. A target holding a bracket is not accepted; step
    // 4 then breaks its syntax.
    let limit = bytes.len().min(MAX_TARGET_BYTES);
    let byte_at = |at: usize| {
        bytes
            .get(at)
            .copied()
            .filter(|&b| at < limit && b != b'[' && b != b']')
    };
    let mut at = skip_spaces(bytes, 1);
    let destination = if byte_at(at) == Some(b'<') {
        let start = at + 1;
        at = start;
        loop {
            match byte_at(at)? {
                b'>' => break,
                b'<' | b'\n' | b'\r' => return None,
                b'\\' => at += 2,
                _ => at += 1,
            }
        }
        let destination = rest.get(start..at)?;
        at += 1;
        destination
    } else {
        let start = at;
        let mut depth = 0_usize;
        while let Some(byte) = byte_at(at) {
            match byte {
                b'\\' => at += 2,
                b'(' => {
                    depth += 1;
                    at += 1;
                }
                b')' if depth == 0 => break,
                b')' => {
                    depth -= 1;
                    at += 1;
                }
                _ if byte.is_ascii_whitespace() || byte.is_ascii_control() => break,
                _ => at += 1,
            }
        }
        rest.get(start..at)?
    };
    at = skip_spaces(bytes, at);
    let close = match byte_at(at)? {
        b')' => return Some((at + 1, destination)),
        b'"' => b'"',
        b'\'' => b'\'',
        b'(' => b')',
        _ => return None,
    };
    at += 1;
    loop {
        match byte_at(at)? {
            byte if byte == close => break,
            b'\\' => at += 2,
            _ => at += 1,
        }
    }
    at = skip_spaces(bytes, at + 1);
    (byte_at(at) == Some(b')')).then_some((at + 1, destination))
}

/// Skips spaces and tabs, and at most one line ending, from `at`.
fn skip_spaces(bytes: &[u8], mut at: usize) -> usize {
    let mut seen_newline = false;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b' ' | b'\t' => at += 1,
            b'\r' | b'\n' if !seen_newline => {
                seen_newline = true;
                at += if bytes[at..].starts_with(b"\r\n") {
                    2
                } else {
                    1
                };
            }
            _ => break,
        }
    }
    at
}

// ---- step 2: autolinks and reference definitions ----

fn replace_autolinks_and_definitions(text: &str, allowed: &HashSet<String>) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    let mut at = 0;
    let mut line_start = true;
    while let Some(ch) = text[at..].chars().next() {
        let rest = &text[at..];
        if line_start && let Some(found) = definition(rest) {
            out.push_str(found.prefix);
            out.push_str(found.label);
            out.push_str(": ");
            push_link(&mut out, &mut removed, found.destination, allowed);
            at += found.len;
            line_start = false;
            continue;
        }
        line_start = false;
        if ch == '\\' && rest.as_bytes().get(1).is_some_and(u8::is_ascii_punctuation) {
            out.push_str(&rest[..2]);
            at += 2;
        } else if ch == '<'
            && let Some(len) = autolink_len(rest)
        {
            push_link(&mut out, &mut removed, &rest[1..len - 1], allowed);
            at += len;
        } else {
            out.push(ch);
            at += ch.len_utf8();
            line_start = ch == '\n';
        }
    }
    (out, removed)
}

/// The length of an autolink, `<scheme:...>` or `<user@host>`, at the start of `rest`.
fn autolink_len(rest: &str) -> Option<usize> {
    // Stop at the first byte an autolink cannot hold, so runs of `<` stay linear.
    let inner_len =
        rest.bytes().skip(1).take(MAX_TARGET_BYTES).position(|b| {
            b == b'>' || b == b'<' || b.is_ascii_whitespace() || b.is_ascii_control()
        })?;
    if rest.as_bytes()[1 + inner_len] != b'>' {
        return None;
    }
    let inner = &rest[1..=inner_len];
    if inner.is_empty() || inner.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    let scheme = &inner[..inner.find(':').unwrap_or(0)];
    let is_uri = (2..=32).contains(&scheme.len())
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
    (is_uri || inner.contains('@')).then_some(inner_len + 2)
}

/// A reference definition, `[label]: destination`, at the start of a line.
struct Definition<'a> {
    /// Indentation and quote or list markers before the label.
    prefix: &'a str,
    label: &'a str,
    destination: &'a str,
    /// Bytes from the line start to the end of the destination.
    len: usize,
}

fn definition(line: &str) -> Option<Definition<'_>> {
    let bytes = line.as_bytes();
    let prefix_len = bytes.iter().take(32).position(|b| {
        !matches!(
            b,
            b' ' | b'\t' | b'>' | b'-' | b'+' | b'*' | b'.' | b')' | b'0'..=b'9'
        )
    })?;
    let after = &line[prefix_len..];
    let label_len = after
        .strip_prefix('[')?
        .bytes()
        .take(MAX_TARGET_BYTES)
        .position(|b| matches!(b, b']' | b'['))?;
    let label = &after[1..=label_len];
    if !after[1 + label_len..].starts_with("]:") || label.trim().is_empty() {
        return None;
    }
    let at = skip_spaces(bytes, prefix_len + label_len + 3);
    let (destination, end) = if bytes.get(at) == Some(&b'<') {
        let close = line[at + 1..]
            .bytes()
            .take(MAX_TARGET_BYTES)
            .position(|b| matches!(b, b'>' | b'\n'))?;
        if bytes[at + 1 + close] != b'>' {
            return None;
        }
        (&line[at + 1..at + 1 + close], at + close + 2)
    } else {
        let len = line[at..]
            .find(|c: char| c.is_whitespace() || c.is_control())
            .unwrap_or(line.len() - at);
        if len == 0 {
            return None;
        }
        (&line[at..at + len], at + len)
    };
    // The line must end after the destination, or go on with a title.
    let tail = &bytes[end..];
    let gap = tail
        .iter()
        .position(|b| !matches!(b, b' ' | b'\t'))
        .unwrap_or(tail.len());
    match tail.get(gap) {
        None | Some(b'\n' | b'\r') => {}
        Some(b'"' | b'\'' | b'(') if gap > 0 => {}
        Some(_) => return None,
    }
    Some(Definition {
        prefix: &line[..prefix_len],
        label,
        destination,
        len: end,
    })
}

// ---- step 3: bare URLs ----

fn replace_bare_urls(text: &str, allowed: &HashSet<String>) -> (String, usize) {
    let mut out = String::with_capacity(text.len());
    let mut removed = 0;
    let mut last = 0;
    for (start, end) in url_ranges(text) {
        out.push_str(&text[last..start]);
        push_link(&mut out, &mut removed, &text[start..end], allowed);
        last = end;
    }
    out.push_str(&text[last..]);
    (out, removed)
}

/// Byte ranges of the bare URLs in `text`, found in its decoded view. Sorted and
/// disjoint.
fn url_ranges(text: &str) -> Vec<(usize, usize)> {
    let view = decode(text);
    let mut ranges = Vec::new();
    let mut at = 0;
    while at < view.len() {
        let after_word = at
            .checked_sub(1)
            .is_some_and(|p| view[p].ch.is_alphanumeric() || matches!(view[p].ch, '.' | '/'));
        if let Some(prefix) = url_prefix_len(&view[at..], after_word) {
            let mut end = at + prefix;
            while view.get(end).is_some_and(|d| {
                !(d.ch.is_whitespace() || d.ch.is_control() || URL_END.contains(&d.ch))
            }) {
                end += 1;
            }
            end = trim_url_end(&view[at..end], prefix) + at;
            if end > at + prefix {
                ranges.push((view[at].start, view[end - 1].end));
                at = end;
                continue;
            }
        }
        at += 1;
    }
    ranges
}

/// The length of `url` without trailing punctuation, never shorter than `prefix`.
fn trim_url_end(url: &[Decoded], prefix: usize) -> usize {
    let opens = url.iter().filter(|d| d.ch == '(').count();
    let mut closes = url.iter().filter(|d| d.ch == ')').count();
    let mut end = url.len();
    while end > prefix {
        let last = url[end - 1].ch;
        if URL_TRAILING.contains(&last) {
            end -= 1;
        } else if last == ')' && closes > opens {
            closes -= 1;
            end -= 1;
        } else {
            break;
        }
    }
    end
}

/// If a URL starts at `view[0]`, the length of its scheme part (`https://`, `www.`,
/// `javascript:`). Web schemes count even right after a word, because some renderers
/// link them there; the others need a word boundary.
fn url_prefix_len(view: &[Decoded], after_word: bool) -> Option<usize> {
    for scheme in WEB_SCHEMES {
        if starts_with_ignore_case(view, scheme)
            && let Some(colon) = colon_len(&view[scheme.len()..])
        {
            let slashes = slashes_len(&view[scheme.len() + colon..]);
            if slashes > 0 {
                return Some(scheme.len() + colon + slashes);
            }
        }
    }
    if after_word {
        return None;
    }
    if starts_with_ignore_case(view, "www.") {
        return Some(4);
    }
    for scheme in SCRIPT_SCHEMES {
        if starts_with_ignore_case(view, scheme)
            && let Some(colon) = colon_len(&view[scheme.len()..])
            && view
                .get(scheme.len() + colon)
                .is_some_and(|d| !d.ch.is_whitespace())
        {
            return Some(scheme.len() + colon);
        }
    }
    None
}

/// A scheme separator, `:` or `%3A`, at the start of `view`.
fn colon_len(view: &[Decoded]) -> Option<usize> {
    if view.first()?.ch == ':' {
        Some(1)
    } else if starts_with_ignore_case(view, "%3a") {
        Some(3)
    } else {
        None
    }
}

/// The length of the run of `/`, `\`, `%2F` and `%5C` at the start of `view`.
fn slashes_len(view: &[Decoded]) -> usize {
    let mut at = 0;
    loop {
        let rest = &view[at..];
        match rest.first().map(|d| d.ch) {
            Some('/' | '\\') => at += 1,
            Some('%')
                if starts_with_ignore_case(rest, "%2f") || starts_with_ignore_case(rest, "%5c") =>
            {
                at += 3;
            }
            _ => return at,
        }
    }
}

fn starts_with_ignore_case(view: &[Decoded], word: &str) -> bool {
    view.len() >= word.len()
        && view
            .iter()
            .zip(word.chars())
            .all(|(d, c)| d.ch.eq_ignore_ascii_case(&c))
}

// ---- the decoded view ----

/// One character as a Markdown renderer would read it, and the bytes it came from.
#[derive(Debug, Clone, Copy)]
struct Decoded {
    ch: char,
    start: usize,
    end: usize,
}

impl Decoded {
    /// Whether the character was written as itself, not escaped or referenced.
    fn is_literal(self) -> bool {
        self.end - self.start == self.ch.len_utf8()
    }
}

/// Decodes backslash escapes and character references, keeping each character's source
/// range.
fn decode(text: &str) -> Vec<Decoded> {
    let mut out = Vec::with_capacity(text.len());
    let mut start = 0;
    while let Some(ch) = text[start..].chars().next() {
        let rest = &text[start..];
        let (ch, len) = match ch {
            '\\' => match rest[1..].chars().next() {
                Some(next) if next.is_ascii_punctuation() => (next, 2),
                _ => ('\\', 1),
            },
            '&' => char_reference(rest).unwrap_or(('&', 1)),
            other => (other, other.len_utf8()),
        };
        out.push(Decoded {
            ch,
            start,
            end: start + len,
        });
        start += len;
    }
    out
}

/// Decodes a character reference such as `&#58;`, `&#x3a;` or `&colon;` at the start
/// of `rest`, returning the character and the reference's length.
fn char_reference(rest: &str) -> Option<(char, usize)> {
    let body_len = rest.bytes().skip(1).take(32).position(|b| b == b';')?;
    let body = &rest[1..=body_len];
    let ch = if let Some(number) = body.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) if (1..=6).contains(&hex.len()) => u32::from_str_radix(hex, 16).ok()?,
            None if (1..=7).contains(&number.len()) => number.parse::<u32>().ok()?,
            _ => return None,
        };
        char::from_u32(code)
            .filter(|&c| c != '\0')
            .unwrap_or('\u{FFFD}')
    } else {
        NAMED_REFERENCES
            .iter()
            .find(|(name, _)| *name == body)
            .map(|&(_, ch)| ch)?
    };
    Some((ch, body_len + 2))
}

// ---- step 4 ----

/// Breaks any link syntax left over: an inline link needs `](` and a reference
/// definition needs `]:`, both with nothing in between.
fn break_link_syntax(text: &str) -> String {
    text.replace("](", "] (").replace("]:", "] :")
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
    fn words_that_merely_end_in_www_are_not_urls() {
        let (text, removed) =
            strip_unknown_urls("awww.bar, a.www.b and 'https://' alone", &HashSet::new());
        assert_eq!(removed, 0, "{text}");
        assert_eq!(text, "awww.bar, a.www.b and 'https://' alone");
    }

    #[test]
    fn a_url_glued_to_a_word_is_still_a_url() {
        // Some renderers (Discord among them) link `http://` wherever it starts.
        let (text, removed) = strip_unknown_urls(
            "nothttps://evil.example/a and x://https://evil.example/b",
            &HashSet::new(),
        );
        assert_eq!(removed, 2, "{text}");
        assert!(!text.contains("evil"), "{text}");
    }

    #[test]
    fn only_scheme_and_host_ignore_case() {
        let known = urls_in(["read https://Example.com/Docs?key=AbC#Part and https://example.com"]);
        for kept in [
            "https://example.com/Docs?key=AbC#Part",
            "HTTPS://EXAMPLE.COM/Docs?key=AbC#Part",
            "https://example.com:443/Docs?key=AbC#Part",
            // An empty path and `/` are the same resource.
            "https://example.com/",
        ] {
            let (text, removed) = strip_unknown_urls(kept, &known);
            assert_eq!((text.as_str(), removed), (kept, 0), "{kept}");
        }
        for foreign in [
            "https://example.com/docs?key=AbC#Part",
            "https://example.com/Docs?key=abc#Part",
            "https://example.com/Docs?key=AbC#part",
            "https://example.com/Docs/?key=AbC#Part",
            "http://example.com/Docs?key=AbC#Part",
            "https://example.com:8443/Docs?key=AbC#Part",
            "https://example.com@evil.example/Docs?key=AbC#Part",
        ] {
            let (text, removed) = strip_unknown_urls(foreign, &known);
            assert_eq!((text.as_str(), removed), (LINK_REMOVED, 1), "{foreign}");
        }
        // A trailing slash on a path is part of the path.
        let known = urls_in(["https://example.com/docs/"]);
        let (_, removed) = strip_unknown_urls("https://example.com/docs", &known);
        assert_eq!(removed, 1);
    }

    #[test]
    fn every_link_form_is_checked_after_decoding() {
        let known = urls_in(["https://example.com/a"]);
        for (answer, shown) in [
            // Inline links, with escapes, character references and odd schemes.
            ("[click](https\\://evil.example/steal) [D-1]", "click [D-1]"),
            ("[click](https&#58;//evil.example/steal)", "click"),
            ("[click](https&#x3A;//evil.example/steal)", "click"),
            ("[click](https&colon;&sol;&sol;evil.example)", "click"),
            ("[click](https%3A//evil.example/steal)", "click"),
            ("[click](https%3a%2f%2fevil.example/steal)", "click"),
            ("[click](<https://evil.example/a b>)", "click"),
            ("[click]( https://evil.example/t \"title\" )", "click"),
            ("[click](//evil.example/steal)", "click"),
            ("[click](evil.example/steal)", "click"),
            ("[click](JaVaScRiPt:alert(1))", "click"),
            ("[click](java&#115;cript:alert(1))", "click"),
            ("[click](data:text/html;base64,PHNjcmlwdD4=)", "click"),
            ("[click](vbscript:msgbox)", "click"),
            ("[click]()", "click"),
            // Images load without a click: their source is a link too.
            ("![pixel](https://evil.example/p?d=secret)", "pixel"),
            // Reference definitions, also inside a quote or list.
            ("[r]: //evil.example/steal", "r: [link removed]"),
            ("> [r]: evil.example \"t\"", "> r: [link removed] \"t\""),
            ("- [r]:\n  https&#58;//evil.example", "- r: [link removed]"),
            // Autolinks of any scheme.
            ("<https://evil.example/x>", "[link removed]"),
            ("<javascript:alert(1)>", "[link removed]"),
            ("<irc:evil.example>", "[link removed]"),
            ("<someone@evil.example>", "[link removed]"),
            // Bare URLs written so that only a decoder sees them.
            ("see https&#58;//evil.example/x", "see [link removed]"),
            ("see https\\://evil.example/x", "see [link removed]"),
            ("see https%3A%2F%2Fevil.example/x", "see [link removed]"),
            ("see HTTPS:\\\\evil.example/x", "see [link removed]"),
            ("see javascript:alert(1)", "see [link removed]"),
            ("see data:text/html,hi", "see [link removed]"),
        ] {
            let (text, removed) = strip_unknown_urls(answer, &known);
            assert_eq!(text, shown, "{answer}");
            assert_eq!(removed, 1, "{answer}");
        }
        // Nested links lose both targets.
        let (text, removed) = strip_unknown_urls(
            "[[a](https://evil.example/1)](https://evil.example/2)",
            &known,
        );
        assert_eq!((text.as_str(), removed), ("a", 2));
    }

    #[test]
    fn allowed_links_keep_their_url_in_plain_view() {
        let known = urls_in(["https://example.com/a"]);
        let (text, removed) = strip_unknown_urls(
            "[the docs](https://example.com/a) and <https://example.com/a> and ![logo](<https://example.com/a>)",
            &known,
        );
        assert_eq!(
            text,
            "the docs (https://example.com/a) and https://example.com/a and logo (https://example.com/a)"
        );
        assert_eq!(removed, 0);
        // A known URL written in an encoded form is not trusted.
        let (text, removed) = strip_unknown_urls("[x](https\\://example.com/a)", &known);
        assert_eq!((text.as_str(), removed), ("x", 1));
    }

    #[test]
    fn leftover_link_syntax_is_broken_up() {
        let known = urls_in(["https://example.com/a"]);
        for answer in [
            "[x](//evil.example/no-close",
            "[x](//evil.example \"open title)",
            "[x]: //evil.example trailing words",
            "[a](https://example.com/a)(//evil.example)",
            "<https://evil.example>(//evil.example)",
        ] {
            let (text, _) = strip_unknown_urls(answer, &known);
            assert!(!text.contains("]("), "{answer} -> {text}");
            assert!(!text.contains("]:"), "{answer} -> {text}");
        }
        // Plain prose with brackets is untouched apart from that.
        let (text, removed) = strip_unknown_urls("A list [1] and [D-1], [x] y.", &known);
        assert_eq!(
            (text.as_str(), removed),
            ("A list [1] and [D-1], [x] y.", 0)
        );
    }

    #[test]
    fn no_combination_of_fragments_leaves_a_live_foreign_link() {
        let fragments = [
            "[",
            "]",
            "(",
            ")",
            "<",
            ">",
            "!",
            "\\",
            "&#58;",
            ":",
            "//",
            "\n",
            " ",
            "https",
            "evil.example",
            "javascript",
            "x",
            "](",
            "]:",
            "%3A",
        ];
        let known = urls_in(["https://example.com/a"]);
        let mut checked = 0;
        for a in fragments {
            for b in fragments {
                for c in fragments {
                    for d in ["", "https://evil.example", "[y](", ")"] {
                        let answer = format!("{a}{b}{c}{d}{a}{c}");
                        let (text, _) = strip_unknown_urls(&answer, &known);
                        let lower = text.to_ascii_lowercase();
                        for live in ["](", "]:", "https://evil", "http://evil"] {
                            assert!(!lower.contains(live), "{answer:?} -> {text:?}");
                        }
                        // Nothing a second pass would flag is left.
                        let again = strip_unknown_urls(&text, &known);
                        assert_eq!(again, (text.clone(), 0), "{answer:?}");
                        checked += 1;
                    }
                }
            }
        }
        assert!(checked > 30_000);
    }

    #[test]
    fn large_hostile_answers_are_handled_in_one_pass() {
        // Each pattern costs quadratic time in a naive scanner; at this size that would
        // hang the test run.
        const N: usize = 40_000;
        let known = urls_in(["https://example.com/a"]);
        for answer in [
            "<".repeat(N * 4),
            "<a".repeat(N * 2),
            "](x (a".repeat(N),
            "](<a".repeat(N),
            "[".repeat(N) + &"](a)".repeat(N),
            format!("https://x{}", ")".repeat(N * 4)),
            "&#".repeat(N * 2),
            "[a]:".repeat(N),
            "\n[".repeat(N * 2),
            "https://".repeat(N),
        ] {
            let (text, _) = strip_unknown_urls(&answer, &known);
            assert!(!text.contains("]("));
        }
    }

    #[test]
    fn no_urls_means_no_changes() {
        let (text, removed) = strip_unknown_urls("plain text, with: punctuation.", &HashSet::new());
        assert_eq!(text, "plain text, with: punctuation.");
        assert_eq!(removed, 0);
    }
}
