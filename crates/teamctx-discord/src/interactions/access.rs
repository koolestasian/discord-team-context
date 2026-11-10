//! Who may see what in a reply: per-member channel access (plan sections 7.3 and 13).
//!
//! Slash commands are open to every member of the server, but the bot reads channels
//! that some members cannot. So every chat-derived item in a reply (a message, or a
//! record entry whose title was copied from a message) is shown only when the person who
//! ran the command can read its channel, using Discord's own permission rules (the
//! [`crate::permissions`] calculation the doctor uses): View Channel and Read Message
//! History after role and member overwrites, with the owner and Administrator seeing
//! everything. Threads and forum posts use their parent channel.
//!
//! A reply with `public:true` is seen by everyone in the channel, so its items must also
//! be readable by the `@everyone` role alone.
//!
//! The snapshot is built from two read-only `GET`s the doctor already uses (the guild with
//! its roles, and the channel list with overwrites) and cached briefly by the service.
//! When it cannot be fetched, nothing chat-derived is shown (fail closed).
//!
//! Not covered: private-thread membership (a private thread is treated like its parent
//! channel) and changes made within the cache lifetime.

use std::collections::HashMap;

use serde_json::Value;
use teamctx_core::id::Snowflake;
use thiserror::Error;

use crate::permissions::{Overwrite, Role, Target, can_read_history, effective_permissions};

/// A payload lacked a field the access check needs. Fixed text only.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("the {0} payload is malformed")]
pub struct AccessError(&'static str);

/// Roles and channel overwrites of the guild at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessSnapshot {
    guild: u64,
    owner: Option<u64>,
    everyone: u64,
    roles: HashMap<u64, u64>,
    channels: HashMap<u64, Vec<Overwrite>>,
}

/// Who is looking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Viewer {
    /// A member with these role ids.
    Member {
        /// The user id.
        user: Snowflake,
        /// Role ids from the interaction's member object.
        roles: Vec<Snowflake>,
    },
    /// The `@everyone` role alone, for public replies.
    Everyone,
}

impl AccessSnapshot {
    /// Builds a snapshot from `GET /guilds/{id}` and `GET /guilds/{id}/channels`.
    ///
    /// # Errors
    ///
    /// [`AccessError`] when a payload lacks the roles, ids or overwrites.
    pub fn from_payloads(
        guild: Snowflake,
        guild_json: &Value,
        channels_json: &Value,
    ) -> Result<Self, AccessError> {
        let owner = id_field(guild_json, "owner_id");
        let mut everyone = None;
        let mut roles = HashMap::new();
        for role in guild_json
            .get("roles")
            .and_then(Value::as_array)
            .ok_or(AccessError("guild"))?
        {
            let id = id_field(role, "id").ok_or(AccessError("role"))?;
            let bits = bits_field(role, "permissions").ok_or(AccessError("role"))?;
            if id == guild.get() {
                everyone = Some(bits);
            } else {
                roles.insert(id, bits);
            }
        }
        let mut channels = HashMap::new();
        for channel in channels_json
            .as_array()
            .ok_or(AccessError("channel list"))?
        {
            let id = id_field(channel, "id").ok_or(AccessError("channel"))?;
            let overwrites = overwrites(channel).ok_or(AccessError("channel"))?;
            channels.insert(id, overwrites);
        }
        Ok(Self {
            guild: guild.get(),
            owner,
            // Without the everyone role nobody is known to read anything.
            everyone: everyone.ok_or(AccessError("guild"))?,
            roles,
            channels,
        })
    }

    /// Whether `viewer` can read the history of `channel` (a top-level channel id; pass
    /// a thread's parent). Unknown channels are not readable.
    #[must_use]
    pub fn readable(&self, viewer: &Viewer, channel: Snowflake) -> bool {
        let Some(overwrites) = self.channels.get(&channel.get()) else {
            return false;
        };
        let (user, held, is_owner) = match viewer {
            Viewer::Member { user, roles } => {
                let held: Vec<Role> = roles
                    .iter()
                    .filter_map(|id| {
                        self.roles.get(&id.get()).map(|bits| Role {
                            id: id.get(),
                            permissions: *bits,
                        })
                    })
                    .collect();
                (user.get(), held, self.owner == Some(user.get()))
            }
            // No user id matches 0, so no member overwrite applies.
            Viewer::Everyone => (0, Vec::new(), false),
        };
        can_read_history(effective_permissions(
            self.guild,
            user,
            is_owner,
            self.everyone,
            &held,
            overwrites,
        ))
    }
}

fn id_field(value: &Value, key: &str) -> Option<u64> {
    value
        .get(key)?
        .as_str()?
        .parse::<Snowflake>()
        .ok()
        .map(Snowflake::get)
}

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
    use crate::permissions::{ADMINISTRATOR, REQUIRED, VIEW_CHANNEL};

    const GUILD: u64 = 1_100_000_000_000_000_001;
    const OPEN: u64 = 1_100_000_000_000_000_101;
    const PRIVATE: u64 = 1_100_000_000_000_000_102;
    const STAFF_ROLE: u64 = 1_100_000_000_000_000_301;
    const ADMIN_ROLE: u64 = 1_100_000_000_000_000_302;
    const OWNER: u64 = 1_100_000_000_000_000_200;
    const ROBIN: u64 = 1_100_000_000_000_000_201;
    const SAM: u64 = 1_100_000_000_000_000_202;

    fn sf(raw: u64) -> Snowflake {
        Snowflake::new(raw).unwrap()
    }

    fn snapshot() -> AccessSnapshot {
        let guild = json!({
            "id": GUILD.to_string(),
            "owner_id": OWNER.to_string(),
            "roles": [
                { "id": GUILD.to_string(), "permissions": REQUIRED.to_string() },
                { "id": STAFF_ROLE.to_string(), "permissions": "0" },
                { "id": ADMIN_ROLE.to_string(), "permissions": ADMINISTRATOR.to_string() }
            ]
        });
        let channels = json!([
            { "id": OPEN.to_string(), "type": 0, "permission_overwrites": [] },
            { "id": PRIVATE.to_string(), "type": 0, "permission_overwrites": [
                { "id": GUILD.to_string(), "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string() },
                { "id": STAFF_ROLE.to_string(), "type": 0, "allow": VIEW_CHANNEL.to_string(), "deny": "0" },
                { "id": SAM.to_string(), "type": 1, "allow": "0", "deny": VIEW_CHANNEL.to_string() }
            ] }
        ]);
        AccessSnapshot::from_payloads(sf(GUILD), &guild, &channels).unwrap()
    }

    fn member(user: u64, roles: &[u64]) -> Viewer {
        Viewer::Member {
            user: sf(user),
            roles: roles.iter().copied().map(sf).collect(),
        }
    }

    #[test]
    fn private_channels_need_the_granting_role() {
        let access = snapshot();
        assert!(access.readable(&member(ROBIN, &[]), sf(OPEN)));
        assert!(!access.readable(&member(ROBIN, &[]), sf(PRIVATE)));
        assert!(access.readable(&member(ROBIN, &[STAFF_ROLE]), sf(PRIVATE)));
        // A member overwrite beats the role allow.
        assert!(!access.readable(&member(SAM, &[STAFF_ROLE]), sf(PRIVATE)));
        // A role the guild does not have grants nothing.
        assert!(!access.readable(&member(ROBIN, &[1_100_000_000_000_000_399]), sf(PRIVATE)));
    }

    #[test]
    fn owner_and_administrator_see_everything_known() {
        let access = snapshot();
        assert!(access.readable(&member(OWNER, &[]), sf(PRIVATE)));
        assert!(access.readable(&member(SAM, &[ADMIN_ROLE]), sf(PRIVATE)));
        // Unknown channels stay closed, even for the owner.
        assert!(!access.readable(&member(OWNER, &[]), sf(1_100_000_000_000_000_999)));
    }

    #[test]
    fn everyone_sees_only_open_channels() {
        let access = snapshot();
        assert!(access.readable(&Viewer::Everyone, sf(OPEN)));
        assert!(!access.readable(&Viewer::Everyone, sf(PRIVATE)));
    }

    #[test]
    fn malformed_payloads_are_errors_without_their_text() {
        let channels = json!([]);
        for guild in [
            json!({}),
            json!({ "roles": [{ "id": "x", "permissions": "0" }] }),
            json!({ "roles": [{ "id": "1100000000000000301", "permissions": 5 }] }),
            json!({ "roles": [] }),
        ] {
            let error = AccessSnapshot::from_payloads(sf(GUILD), &guild, &channels).unwrap_err();
            assert!(!error.to_string().contains("1100"), "{error}");
        }
        let guild = json!({ "roles": [{ "id": GUILD.to_string(), "permissions": "0" }] });
        let bad = json!([{ "id": OPEN.to_string(), "permission_overwrites": [{ "id": "1", "type": 7, "allow": "0", "deny": "0" }] }]);
        assert!(AccessSnapshot::from_payloads(sf(GUILD), &guild, &bad).is_err());
        assert!(AccessSnapshot::from_payloads(sf(GUILD), &guild, &json!({})).is_err());
    }
}
