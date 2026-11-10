//! The live `doctor` checks (plan section 7.1): can the bot see the guild, which channels
//! can it read (effective permissions with channel overwrites, via
//! [`crate::permissions`]), does it hold more than it needs, and is the Message Content
//! intent enabled for the application.
//!
//! [`diagnose`] is pure and runs on JSON payloads, so it is tested on fixtures.
//! [`check`] fetches those payloads with five `GET` requests through
//! [`TwilightRest::read`]. Nothing is changed on Discord.
//!
//! What this cannot see: whether the bot is a member of each private thread (that needs
//! a per-thread request), and whether the gateway actually delivers message content
//! (only a live connection shows that; `teamctx run` reports close code 4014).

use serde_json::Value;
use teamctx_core::id::Snowflake;
use thiserror::Error;

use crate::http::{Read, TwilightRest};
use crate::permissions::{
    ADMINISTRATOR, Overwrite, Role, Target, can_read_history, effective_permissions,
    extra_permissions,
};
use crate::sync::RestError;

/// Application flag: Message Content intent enabled (verified apps, or approved).
const GATEWAY_MESSAGE_CONTENT: u64 = 1 << 18;
/// Application flag: Message Content intent enabled for an unverified app (under 100
/// servers), which is the normal case for one team's self-hosted bot.
const GATEWAY_MESSAGE_CONTENT_LIMITED: u64 = 1 << 19;

/// Channel types whose history teamctx reads: text, announcement and forum.
const READ_KINDS: [u64; 3] = [0, 5, 15];

/// Whether the application may receive message content over the gateway.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageContentIntent {
    /// Enabled in the Developer Portal.
    Enabled,
    /// Not enabled: the gateway would close with 4014.
    Disabled,
}

/// The bot's access to one channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelAccess {
    /// The channel id.
    pub id: Snowflake,
    /// The channel name. Untrusted text: escape it before printing.
    pub name: String,
    /// View Channels and Read Message History are both effective.
    pub readable: bool,
    /// Effective bits beyond the two the bot needs. `u64::MAX` means administrator or
    /// owner (everything).
    pub extra: u64,
}

/// What the live checks found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorReport {
    /// The bot's user id.
    pub bot_user: Snowflake,
    /// The Message Content intent setting.
    pub message_content: MessageContentIntent,
    /// The bot holds Administrator or owns the guild.
    pub administrator: bool,
    /// Text, announcement and forum channels, in the order Discord listed them.
    pub channels: Vec<ChannelAccess>,
}

impl DoctorReport {
    /// Everything `teamctx run` needs: the intent is on and at least one channel is
    /// readable. Extra permissions and unreadable channels are warnings.
    #[must_use]
    pub fn ready(&self) -> bool {
        self.message_content == MessageContentIntent::Enabled
            && self.channels.iter().any(|channel| channel.readable)
    }
}

/// Why the live checks could not finish. Fixed text only.
#[derive(Debug, Error)]
pub enum DoctorError {
    /// Discord rejected the token.
    #[error("Discord rejected the bot token")]
    TokenRejected,
    /// The bot is not in the guild, or cannot see it.
    #[error("the bot cannot see the configured Discord server (not invited, or removed)")]
    GuildNotVisible,
    /// Discord could not be reached or failed.
    #[error("Discord could not be reached")]
    Unreachable(#[source] RestError),
    /// A payload did not have the expected shape.
    #[error("Discord sent an unexpected {0} payload")]
    Malformed(&'static str),
}

/// Runs the live checks for `guild`.
///
/// # Errors
///
/// [`DoctorError`] when a request fails or a payload is malformed.
pub async fn check(rest: &TwilightRest, guild: Snowflake) -> Result<DoctorReport, DoctorError> {
    let user = rest.read(Read::CurrentUser).await.map_err(general)?;
    let bot = id_field(&user, "id").ok_or(DoctorError::Malformed("user"))?;
    let application = rest.read(Read::Application).await.map_err(general)?;
    let guild_json = rest.read(Read::Guild(guild)).await.map_err(in_guild)?;
    let member = rest
        .read(Read::Member(guild, bot))
        .await
        .map_err(in_guild)?;
    let channels = rest.read(Read::Channels(guild)).await.map_err(in_guild)?;
    diagnose(guild, &user, &application, &guild_json, &member, &channels)
}

fn general(error: RestError) -> DoctorError {
    match error {
        RestError::Unauthorized => DoctorError::TokenRejected,
        other => DoctorError::Unreachable(other),
    }
}

fn in_guild(error: RestError) -> DoctorError {
    match error {
        RestError::NotFound | RestError::Forbidden => DoctorError::GuildNotVisible,
        other => general(other),
    }
}

/// Computes the report from the five payloads.
///
/// # Errors
///
/// [`DoctorError::Malformed`] if a payload lacks a field the checks need.
pub fn diagnose(
    guild: Snowflake,
    user: &Value,
    application: &Value,
    guild_json: &Value,
    member: &Value,
    channels: &Value,
) -> Result<DoctorReport, DoctorError> {
    let bot = id_field(user, "id").ok_or(DoctorError::Malformed("user"))?;
    let flags = application
        .get("flags")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let message_content =
        if flags & (GATEWAY_MESSAGE_CONTENT | GATEWAY_MESSAGE_CONTENT_LIMITED) != 0 {
            MessageContentIntent::Enabled
        } else {
            MessageContentIntent::Disabled
        };

    let is_owner = id_field(guild_json, "owner_id") == Some(bot);
    let all_roles = guild_json
        .get("roles")
        .and_then(Value::as_array)
        .ok_or(DoctorError::Malformed("guild"))?;
    let mut everyone = 0;
    let mut role_bits = Vec::new();
    for role in all_roles {
        let id = id_field(role, "id").ok_or(DoctorError::Malformed("role"))?;
        let bits = bits_field(role, "permissions").ok_or(DoctorError::Malformed("role"))?;
        if id == guild {
            everyone = bits;
        } else {
            role_bits.push(Role {
                id: id.get(),
                permissions: bits,
            });
        }
    }
    let held: Vec<u64> = member
        .get("roles")
        .and_then(Value::as_array)
        .ok_or(DoctorError::Malformed("member"))?
        .iter()
        .filter_map(|role| role.as_str()?.parse::<u64>().ok())
        .collect();
    let roles: Vec<Role> = role_bits
        .into_iter()
        .filter(|role| held.contains(&role.id))
        .collect();
    let guild_level =
        effective_permissions(guild.get(), bot.get(), is_owner, everyone, &roles, &[]);

    let mut access = Vec::new();
    for channel in channels
        .as_array()
        .ok_or(DoctorError::Malformed("channel list"))?
    {
        let kind = channel.get("type").and_then(Value::as_u64);
        if !kind.is_some_and(|kind| READ_KINDS.contains(&kind)) {
            continue;
        }
        let id = id_field(channel, "id").ok_or(DoctorError::Malformed("channel"))?;
        let overwrites = overwrites(channel).ok_or(DoctorError::Malformed("channel"))?;
        let bits = effective_permissions(
            guild.get(),
            bot.get(),
            is_owner,
            everyone,
            &roles,
            &overwrites,
        );
        access.push(ChannelAccess {
            id,
            name: channel
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            readable: can_read_history(bits),
            extra: if bits == u64::MAX {
                u64::MAX
            } else {
                extra_permissions(bits)
            },
        });
    }
    Ok(DoctorReport {
        bot_user: bot,
        message_content,
        administrator: guild_level == u64::MAX || guild_level & ADMINISTRATOR != 0,
        channels: access,
    })
}

fn id_field(value: &Value, key: &str) -> Option<Snowflake> {
    value.get(key)?.as_str()?.parse().ok()
}

/// Permission bitsets are decimal strings in Discord's API.
fn bits_field(value: &Value, key: &str) -> Option<u64> {
    value.get(key)?.as_str()?.parse().ok()
}

fn overwrites(channel: &Value) -> Option<Vec<Overwrite>> {
    let Some(list) = channel.get("permission_overwrites") else {
        return Some(Vec::new());
    };
    list.as_array()?
        .iter()
        .map(|entry| {
            let id: u64 = entry.get("id")?.as_str()?.parse().ok()?;
            let target = match entry.get("type")?.as_u64()? {
                0 => Target::Role(id),
                1 => Target::Member(id),
                _ => return None,
            };
            Some(Overwrite {
                target,
                allow: bits_field(entry, "allow")?,
                deny: bits_field(entry, "deny")?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::permissions::{READ_MESSAGE_HISTORY, REQUIRED, VIEW_CHANNEL};

    const GUILD: u64 = 1_100_000_000_000_000_001;
    const BOT: &str = "1100000000000000500";
    const BOT_ROLE: &str = "1100000000000000600";

    fn guild() -> Snowflake {
        Snowflake::new(GUILD).unwrap()
    }

    fn guild_json(everyone: u64, bot_role: u64) -> Value {
        json!({
            "id": GUILD.to_string(),
            "owner_id": "1100000000000000201",
            "roles": [
                { "id": GUILD.to_string(), "permissions": everyone.to_string() },
                { "id": BOT_ROLE, "permissions": bot_role.to_string() },
                { "id": "1100000000000000601", "permissions": "8" }
            ]
        })
    }

    fn channel(id: &str, kind: u64, overwrites: &Value) -> Value {
        json!({ "id": id, "type": kind, "name": "general", "permission_overwrites": overwrites })
    }

    fn run(application_flags: u64, guild: &Value, channels: &Value) -> DoctorReport {
        diagnose(
            self::guild(),
            &json!({ "id": BOT, "username": "teamctx-test" }),
            &json!({ "flags": application_flags }),
            guild,
            &json!({ "roles": [BOT_ROLE] }),
            channels,
        )
        .unwrap()
    }

    #[test]
    fn minimal_invite_reads_open_channels_and_skips_denied_ones() {
        let channels = json!([
            channel("1100000000000000101", 0, &json!([])),
            channel(
                "1100000000000000102",
                0,
                &json!([{ "id": GUILD.to_string(), "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string() }]),
            ),
            channel(
                "1100000000000000103",
                5,
                &json!([{ "id": BOT, "type": 1, "allow": "0", "deny": READ_MESSAGE_HISTORY.to_string() }]),
            ),
            channel("1100000000000000104", 2, &json!([])),
            channel("1100000000000000105", 4, &json!([])),
        ]);
        let report = run(1 << 19, &guild_json(0, REQUIRED), &channels);
        assert_eq!(report.message_content, MessageContentIntent::Enabled);
        assert!(!report.administrator);
        let readable: Vec<(u64, bool)> = report
            .channels
            .iter()
            .map(|c| (c.id.get(), c.readable))
            .collect();
        assert_eq!(
            readable,
            [
                (1_100_000_000_000_000_101, true),
                (1_100_000_000_000_000_102, false),
                (1_100_000_000_000_000_103, false),
            ],
            "voice and category channels are not listed"
        );
        assert!(report.channels.iter().all(|c| c.extra == 0));
        assert!(report.ready());
    }

    #[test]
    fn extra_permissions_and_administrator_are_reported() {
        let send_messages = 1 << 11;
        let report = run(
            1 << 18,
            &guild_json(0, REQUIRED | send_messages),
            &json!([channel("1100000000000000101", 0, &json!([]))]),
        );
        assert_eq!(report.channels[0].extra, send_messages);
        let admin = run(
            1 << 18,
            &guild_json(0, REQUIRED | 8),
            &json!([channel("1100000000000000101", 0, &json!([]))]),
        );
        assert!(admin.administrator);
        assert_eq!(admin.channels[0].extra, u64::MAX);
    }

    #[test]
    fn a_disabled_intent_or_no_readable_channel_is_not_ready() {
        let channels = json!([channel("1100000000000000101", 0, &json!([]))]);
        let report = run(0, &guild_json(0, REQUIRED), &channels);
        assert_eq!(report.message_content, MessageContentIntent::Disabled);
        assert!(!report.ready());
        let blind = run(1 << 19, &guild_json(0, 0), &channels);
        assert!(!blind.channels[0].readable);
        assert!(!blind.ready());
    }

    #[test]
    fn roles_the_bot_does_not_hold_do_not_count() {
        // Role ...601 has Administrator but the bot does not hold it.
        let report = run(
            1 << 19,
            &guild_json(REQUIRED, 0),
            &json!([channel("1100000000000000101", 0, &json!([]))]),
        );
        assert!(!report.administrator);
        assert!(report.channels[0].readable);
    }

    #[test]
    fn malformed_payloads_are_errors_without_their_text() {
        let error = diagnose(
            guild(),
            &json!({ "id": "FAKE-secret-in-payload" }),
            &json!({}),
            &json!({}),
            &json!({}),
            &json!([]),
        )
        .unwrap_err();
        assert!(!format!("{error} {error:?}").contains("FAKE-secret"));
        let error = diagnose(
            guild(),
            &json!({ "id": BOT }),
            &json!({}),
            &json!({ "roles": [{ "id": "1", "permissions": "FAKE-secret" }] }),
            &json!({ "roles": [] }),
            &json!([]),
        )
        .unwrap_err();
        assert!(!format!("{error} {error:?}").contains("FAKE-secret"));
    }
}
