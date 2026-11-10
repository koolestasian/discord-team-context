//! Display names and mention resolution for message text.

use std::collections::HashMap;

/// The name to show for a person: the server nickname if set, else the global display
/// name, else the username (the rule documented on `teamctx_core::source::Member`).
///
/// Empty or whitespace-only values count as unset, so a blank nickname never hides the
/// real name.
pub(crate) fn display_name(
    nick: Option<&str>,
    global_name: Option<&str>,
    username: &str,
) -> String {
    [nick, global_name]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|name| !name.is_empty())
        .unwrap_or(username)
        .to_owned()
}

/// Replaces user mentions (`<@id>` and `<@!id>`) with `@name` using `names`.
///
/// Mentions whose id is not in `names` stay as written, so no information is lost. Role
/// (`<@&id>`) and channel (`<#id>`) mentions are left as written too: the payload does not
/// carry their names. The scan is a single pass, so text inserted for one mention is never
/// re-scanned, even if a display name itself looks like a mention.
pub(super) fn resolve_mentions(content: &str, names: &HashMap<u64, String>) -> String {
    let mut out = String::with_capacity(content.len());
    let mut rest = content;
    while let Some(start) = rest.find("<@") {
        out.push_str(&rest[..start]);
        let candidate = &rest[start..];
        let resolved = parse_user_mention(candidate)
            .and_then(|(id, len)| names.get(&id).map(|name| (name, len)));
        if let Some((name, len)) = resolved {
            out.push('@');
            out.push_str(name);
            rest = &candidate[len..];
        } else {
            out.push_str("<@");
            rest = &candidate[2..];
        }
    }
    out.push_str(rest);
    out
}

/// Parses `<@id>` or `<@!id>` at the start of `text`; returns the id and the byte length.
fn parse_user_mention(text: &str) -> Option<(u64, usize)> {
    let body = text.strip_prefix("<@")?;
    let (body, prefix_len) = match body.strip_prefix('!') {
        Some(after_bang) => (after_bang, 3),
        None => (body, 2),
    };
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 || body.as_bytes().get(digits) != Some(&b'>') {
        return None;
    }
    let id = body[..digits].parse().ok()?;
    Some((id, prefix_len + digits + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> HashMap<u64, String> {
        HashMap::from([(1, "Robin".to_owned()), (2, "<@1>".to_owned())])
    }

    #[test]
    fn display_name_prefers_nick_then_global_then_username() {
        assert_eq!(display_name(Some("Nick"), Some("Global"), "user"), "Nick");
        assert_eq!(display_name(None, Some("Global"), "user"), "Global");
        assert_eq!(display_name(None, None, "user"), "user");
        assert_eq!(display_name(Some("  "), Some(""), "user"), "user");
        assert_eq!(display_name(Some(""), Some("Global"), "user"), "Global");
    }

    #[test]
    fn resolves_plain_and_nickname_mentions() {
        assert_eq!(
            resolve_mentions("hi <@1> and <@!1>!", &names()),
            "hi @Robin and @Robin!"
        );
    }

    #[test]
    fn keeps_unknown_role_channel_and_malformed_mentions() {
        let text = "<@9> <@&1> <#1> <@> <@1 <@x1> <@!>";
        assert_eq!(resolve_mentions(text, &names()), text);
    }

    #[test]
    fn single_pass_does_not_rescan_inserted_names() {
        assert_eq!(resolve_mentions("<@2>", &names()), "@<@1>");
    }

    #[test]
    fn handles_multibyte_text_and_trailing_marker() {
        assert_eq!(
            resolve_mentions("héllo <@1> ✅ <@", &names()),
            "héllo @Robin ✅ <@"
        );
    }
}
