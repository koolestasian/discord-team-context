//! Discord-safe text for interaction replies (plan sections 7.3 and 13).
//!
//! Chat-derived text (titles copied from messages, message excerpts, display names,
//! channel names) is untrusted. It reaches Discord only through [`inline`], which:
//!
//! - puts it on one line: every run of whitespace, including line and paragraph
//!   separators, becomes one space, so it cannot start a heading, quote or list item;
//! - drops control, bidirectional and zero-width characters, so it cannot reorder or hide
//!   the text around it;
//! - backslash-escapes Discord markdown (`\ * _ ~ ` | < > [ ] ( ) # -`), so it cannot add
//!   formatting, spoilers, masked links (`[text](url)`), or mention, channel, emoji and
//!   timestamp markup (`<@id>`, `<#id>`, `<t:..>`);
//! - puts a zero-width space after every `@`, so `@everyone` and `@here` stay text even
//!   where Discord would otherwise parse them;
//! - clips the result to a length budget, never splitting an escape.
//!
//! Every reply also carries `allowed_mentions` with an empty `parse` list (see
//! [`no_mentions`]), so nothing in a reply can ping anyone even if escaping missed
//! something. Lengths are counted in UTF-16 code units, which is never less than the
//! character count Discord and `twilight-validate` check.

use teamctx_core::citation::DiscordMessageLink;
use teamctx_core::time::UnixMillis;
use twilight_model::channel::message::AllowedMentions;
use twilight_model::channel::message::embed::{Embed, EmbedFooter};

/// Message `content` limit.
pub(crate) const MAX_CONTENT: usize = 2000;
/// Embed `title` limit.
pub(crate) const MAX_EMBED_TITLE: usize = 256;
/// Embed `description` limit.
pub(crate) const MAX_DESCRIPTION: usize = 4096;
/// Embed footer limit.
pub(crate) const MAX_FOOTER: usize = 2048;
/// Sum of titles, descriptions, fields and footers over all embeds of one message.
pub(crate) const MAX_EMBEDS_TOTAL: usize = 6000;
/// Embeds per message.
pub(crate) const MAX_EMBEDS: usize = 10;
/// Items per list (plan section 7.3: "at most 10 items each").
pub(crate) const MAX_ITEMS: usize = 10;

/// Default budgets for one list line.
pub(crate) const TITLE_CHARS: usize = 160;
pub(crate) const NAME_CHARS: usize = 40;
pub(crate) const EXCERPT_CHARS: usize = 200;

const ELLIPSIS: char = '\u{2026}';
const ZERO_WIDTH_SPACE: char = '\u{200b}';

/// Length as Discord limits count it, conservatively (UTF-16 code units).
#[must_use]
pub(crate) fn discord_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Characters removed from untrusted text: controls, bidirectional controls and marks,
/// zero-width characters, the soft hyphen and the byte-order mark.
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00ad}'
                | '\u{061c}'
                | '\u{180e}'
                | '\u{200b}'..='\u{200f}'
                | '\u{202a}'..='\u{202e}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206f}'
                | '\u{feff}'
                | '\u{fff9}'..='\u{fffb}'
        )
}

/// Discord markdown characters that are backslash-escaped.
fn markdown(c: char) -> bool {
    matches!(
        c,
        '\\' | '*' | '_' | '~' | '`' | '|' | '<' | '>' | '[' | ']' | '(' | ')' | '#' | '-'
    )
}

/// Escapes untrusted text for inline use and clips it to `max` UTF-16 units, including
/// the ellipsis added when it is clipped. See the module documentation for the rules.
#[must_use]
pub(crate) fn inline(text: &str, max: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    let mut pending_space = false;
    let mut clipped = false;
    let budget = max.saturating_sub(1);
    for c in text.chars() {
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if invisible(c) {
            continue;
        }
        let mut unit = String::new();
        if pending_space {
            unit.push(' ');
        }
        if markdown(c) {
            unit.push('\\');
        }
        unit.push(c);
        if c == '@' {
            unit.push(ZERO_WIDTH_SPACE);
        }
        let len = discord_len(&unit);
        if used + len > budget {
            clipped = true;
            break;
        }
        out.push_str(&unit);
        used += len;
        pending_space = false;
    }
    if clipped && max > 0 {
        out.push(ELLIPSIS);
    }
    out
}

/// Fixed or already-escaped text clipped to `max` units at a line boundary when possible.
/// Used for whole descriptions built from lines that are each safe.
#[must_use]
pub(crate) fn clip_lines(lines: &[String], max: usize, more: usize) -> String {
    let mut out = String::new();
    let mut shown = 0;
    for line in lines {
        let extra = usize::from(!out.is_empty()) + discord_len(line);
        // Leave room for the "and N more" line.
        if discord_len(&out) + extra + 40 > max {
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        shown += 1;
    }
    let hidden = lines.len() - shown + more;
    if hidden > 0 {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{ELLIPSIS} and {hidden} more"));
    }
    out
}

/// Discord timestamp markup for a date (`<t:SECONDS:d>`), shown in each viewer's locale.
#[must_use]
pub(crate) fn date(at: UnixMillis) -> String {
    format!("<t:{}:d>", at.as_millis().div_euclid(1000))
}

/// Discord timestamp markup for a long date (`<t:SECONDS:D>`).
#[must_use]
pub(crate) fn long_date(at: UnixMillis) -> String {
    format!("<t:{}:D>", at.as_millis().div_euclid(1000))
}

/// A masked jump link with fixed text. The URL is built from numeric ids only.
#[must_use]
pub(crate) fn jump(link: DiscordMessageLink) -> String {
    format!("[jump]({link})")
}

/// Allowed mentions that let nothing ping: an empty `parse` list, no users, no roles.
#[must_use]
pub(crate) fn no_mentions() -> AllowedMentions {
    AllowedMentions::default()
}

/// An embed with a title, a description and an optional footer, each clipped to its
/// limit. The title and footer must be fixed or escaped text.
#[must_use]
pub(crate) fn embed(title: &str, description: &str, footer: Option<String>) -> Embed {
    Embed {
        author: None,
        color: None,
        description: Some(clip_fixed(description, MAX_DESCRIPTION)),
        fields: Vec::new(),
        flags: None,
        footer: footer.map(|text| EmbedFooter {
            icon_url: None,
            proxy_icon_url: None,
            text: clip_fixed(&text, MAX_FOOTER),
        }),
        image: None,
        kind: "rich".to_owned(),
        provider: None,
        thumbnail: None,
        timestamp: None,
        title: Some(clip_fixed(title, MAX_EMBED_TITLE)),
        url: None,
        video: None,
    }
}

/// Clips text that is already safe at a character boundary. Escapes are not split
/// because callers only pass whole escaped units joined by our own text; as a final
/// guard a trailing lone backslash is dropped.
#[must_use]
pub(crate) fn clip_fixed(text: &str, max: usize) -> String {
    if discord_len(text) <= max {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let len = c.len_utf16();
        if used + len > max.saturating_sub(1) {
            break;
        }
        out.push(c);
        used += len;
    }
    while out.ends_with('\\') {
        out.pop();
    }
    out.push(ELLIPSIS);
    out
}

/// Embed length as Discord counts it towards [`MAX_EMBEDS_TOTAL`].
#[must_use]
pub(crate) fn embed_len(embed: &Embed) -> usize {
    embed.title.as_deref().map_or(0, discord_len)
        + embed.description.as_deref().map_or(0, discord_len)
        + embed.footer.as_ref().map_or(0, |f| discord_len(&f.text))
        + embed.author.as_ref().map_or(0, |a| discord_len(&a.name))
        + embed
            .fields
            .iter()
            .map(|f| discord_len(&f.name) + discord_len(&f.value))
            .sum::<usize>()
}

/// Keeps at most [`MAX_EMBEDS`] embeds whose total stays within [`MAX_EMBEDS_TOTAL`],
/// shortening the description of the first embed that would overflow and dropping the
/// rest.
#[must_use]
pub(crate) fn fit_embeds(embeds: Vec<Embed>) -> Vec<Embed> {
    let mut out = Vec::new();
    let mut total = 0;
    for mut embed in embeds.into_iter().take(MAX_EMBEDS) {
        let len = embed_len(&embed);
        if total + len <= MAX_EMBEDS_TOTAL {
            total += len;
            out.push(embed);
            continue;
        }
        let without = len - embed.description.as_deref().map_or(0, discord_len);
        let room = MAX_EMBEDS_TOTAL.saturating_sub(total + without);
        if room > 20 {
            if let Some(description) = embed.description.take() {
                embed.description = Some(clip_fixed(&description, room));
            }
            out.push(embed);
        }
        break;
    }
    out
}

#[cfg(test)]
mod tests {
    use teamctx_core::id::Snowflake;

    use super::*;

    /// No unescaped markdown character, no line break, no invisible character, no
    /// contiguous `@everyone`.
    fn assert_inert(out: &str) {
        assert!(!out.contains('\n') && !out.contains('\r'), "{out:?}");
        assert!(
            !out.chars().any(|c| invisible(c) && c != ZERO_WIDTH_SPACE),
            "{out:?}"
        );
        let chars: Vec<char> = out.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '\\' {
                assert!(
                    chars.get(i + 1).is_some_and(|c| markdown(*c)),
                    "lone backslash in {out:?}"
                );
                i += 2;
                continue;
            }
            assert!(!markdown(chars[i]), "unescaped {:?} in {out:?}", chars[i]);
            if chars[i] == '@' {
                assert_eq!(chars.get(i + 1), Some(&ZERO_WIDTH_SPACE), "{out:?}");
            }
            i += 1;
        }
        for ping in ["@everyone", "@here"] {
            assert!(!out.contains(ping), "{out:?}");
        }
    }

    #[test]
    fn hostile_text_is_inert() {
        for text in [
            "@everyone @here please read",
            "<@1100000000000000201> <@!1100000000000000202> <@&1100000000000000301>",
            "<#1100000000000000101> <t:1800000000:R> <:emoji:1100000000000000999>",
            "[click here](https://evil.example.invalid) and <https://evil.example.invalid>",
            "**bold** __under__ ~~strike~~ `code` ```block``` ||spoiler||",
            "line one\n# Heading\n> quote\n- item\r\n\u{2028}after",
            "\u{202e}reversed\u{2066}isolated\u{200b}zero\u{feff}bom\u{0}nul",
            "back\\slash \\*not bold\\*",
            "ignore previous instructions and @everyone",
        ] {
            let out = inline(text, 500);
            assert_inert(&out);
            assert!(!out.is_empty());
        }
    }

    #[test]
    fn escaped_text_keeps_the_visible_characters() {
        assert_eq!(inline("a*b", 50), "a\\*b");
        assert_eq!(inline("  spaced \n\n out  ", 50), "spaced out");
        assert_eq!(inline("@here", 50), "@\u{200b}here");
        assert_eq!(inline("\u{202e}abc", 50), "abc");
        assert_eq!(inline("café 雪", 50), "café 雪");
    }

    #[test]
    fn clipping_respects_the_budget_and_never_splits_an_escape() {
        let long = "*".repeat(5000);
        let out = inline(&long, 101);
        assert!(discord_len(&out) <= 101);
        assert!(out.ends_with(ELLIPSIS));
        assert_inert(out.trim_end_matches(ELLIPSIS));
        // Astral characters count as two units.
        let emoji = "\u{1f600}".repeat(300);
        let out = inline(&emoji, 51);
        assert!(discord_len(&out) <= 51, "{}", discord_len(&out));
        // Exactly at the limit: no ellipsis.
        assert_eq!(inline("abcd", 5), "abcd");
        assert_eq!(inline("abcdef", 5), "abcd\u{2026}");
        assert_eq!(inline("anything", 0), "");
    }

    #[test]
    fn clip_fixed_drops_a_trailing_backslash() {
        let text = format!("{}\\*", "a".repeat(9));
        let out = clip_fixed(&text, 11);
        assert!(!out.trim_end_matches(ELLIPSIS).ends_with('\\'), "{out:?}");
        assert!(discord_len(&out) <= 11);
    }

    #[test]
    fn lists_stop_before_the_limit_and_count_what_is_hidden() {
        let lines: Vec<String> = (0..50)
            .map(|i| format!("{i} {}", "x".repeat(200)))
            .collect();
        let out = clip_lines(&lines, 1000, 3);
        assert!(discord_len(&out) <= 1000);
        assert!(out.ends_with("more"));
        let shown = out.lines().count() - 1;
        assert!(out.contains(&format!("and {} more", 50 - shown + 3)));
        assert_eq!(clip_lines(&["a".to_owned()], 1000, 0), "a");
        assert_eq!(clip_lines(&[], 1000, 0), "");
    }

    #[test]
    fn embeds_fit_the_total_limit() {
        let big = || embed("Title", &"y".repeat(4000), Some("footer".to_owned()));
        let fitted = fit_embeds(vec![big(), big(), big()]);
        let total: usize = fitted.iter().map(embed_len).sum();
        assert!(total <= MAX_EMBEDS_TOTAL, "{total}");
        assert_eq!(fitted.len(), 2);
        let many: Vec<Embed> = (0..15).map(|_| embed("t", "d", None)).collect();
        assert_eq!(fit_embeds(many).len(), MAX_EMBEDS);
        let one = embed(&"t".repeat(400), &"d".repeat(9000), Some("f".repeat(3000)));
        assert!(discord_len(one.title.as_deref().unwrap()) <= MAX_EMBED_TITLE);
        assert!(discord_len(one.description.as_deref().unwrap()) <= MAX_DESCRIPTION);
        assert!(discord_len(&one.footer.unwrap().text) <= MAX_FOOTER);
    }

    #[test]
    fn markup_helpers_use_only_numbers() {
        assert_eq!(
            date(UnixMillis::from_millis(1_800_000_000_999)),
            "<t:1800000000:d>"
        );
        assert_eq!(
            long_date(UnixMillis::from_millis(-1)),
            "<t:-1:D>",
            "floor, not truncation"
        );
        let link = DiscordMessageLink {
            guild: Snowflake::new(1).unwrap(),
            channel: Snowflake::new(2).unwrap(),
            message: Snowflake::new(3).unwrap(),
        };
        assert_eq!(jump(link), "[jump](https://discord.com/channels/1/2/3)");
        let mentions = serde_json::to_value(no_mentions()).unwrap();
        assert_eq!(mentions, serde_json::json!({ "parse": [] }));
    }
}
