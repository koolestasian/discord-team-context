//! Offline Discord permission calculations following the official permission hierarchy.
//! Thread reads use the parent channel's overwrites. Private-thread membership must
//! additionally be checked by the live connector; permission bits alone cannot prove it.

use std::num::NonZeroU64;

/// View Channels.
pub const VIEW_CHANNEL: u64 = 1 << 10;
/// Read Message History.
pub const READ_MESSAGE_HISTORY: u64 = 1 << 16;
/// Administrator bypasses all overwrites and must be warned about by doctor.
pub const ADMINISTRATOR: u64 = 1 << 3;
/// Exactly the two permissions requested by the v0.1 bot invite.
pub const REQUIRED: u64 = VIEW_CHANNEL | READ_MESSAGE_HISTORY;

/// One role held by the bot. The everyone role is passed separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Role {
    /// Discord role id.
    pub id: u64,
    /// Guild-level permission bits.
    pub permissions: u64,
}

/// The target of a channel overwrite (role and user ids are distinct namespaces).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Role, including the everyone role whose id equals the guild id.
    Role(u64),
    /// Individual user.
    Member(u64),
}

/// A channel's allowed and denied permission bits for one target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Overwrite {
    /// The role or user affected.
    pub target: Target,
    /// Bits explicitly granted.
    pub allow: u64,
    /// Bits explicitly denied.
    pub deny: u64,
}

/// Computes the effective bitset: everyone base, role union, everyone overwrite,
/// combined role overwrites, then the member overwrite. Role order never matters.
/// Owner/admin bypasses overwrites; `u64::MAX` represents unrestricted permission.
#[must_use]
pub fn effective_permissions(
    guild_id: u64,
    member_id: u64,
    is_owner: bool,
    everyone: u64,
    roles: &[Role],
    overwrites: &[Overwrite],
) -> u64 {
    let mut permissions = roles
        .iter()
        .fold(everyone, |bits, role| bits | role.permissions);
    if is_owner || permissions & ADMINISTRATOR != 0 {
        return u64::MAX;
    }
    let apply = |bits: u64, allow: u64, deny: u64| (bits & !deny) | allow;
    for overwrite in overwrites
        .iter()
        .filter(|o| o.target == Target::Role(guild_id))
    {
        permissions = apply(permissions, overwrite.allow, overwrite.deny);
    }
    let (mut allow, mut deny) = (0, 0);
    for overwrite in overwrites {
        if let Target::Role(id) = overwrite.target
            && id != guild_id
            && roles.iter().any(|role| role.id == id)
        {
            allow |= overwrite.allow;
            deny |= overwrite.deny;
        }
    }
    permissions = apply(permissions, allow, deny);
    for overwrite in overwrites
        .iter()
        .filter(|o| o.target == Target::Member(member_id))
    {
        permissions = apply(permissions, overwrite.allow, overwrite.deny);
    }
    permissions
}

/// Whether ordinary channel history can be read. View denial implicitly prevents
/// history reads even when the history bit remains set in the effective bitset.
#[must_use]
pub const fn can_read_history(permissions: u64) -> bool {
    permissions & REQUIRED == REQUIRED
}

/// Bits beyond the requested minimum; doctor should warn, including administrator.
#[must_use]
pub const fn extra_permissions(permissions: u64) -> u64 {
    permissions & !REQUIRED
}

/// Builds a guild-locked bot install link with exactly 66560 permissions and the bot
/// and applications.commands scopes. Ids are numeric, so no input can inject URL fields.
#[must_use]
pub fn invite_url(application_id: NonZeroU64, guild_id: NonZeroU64) -> String {
    format!(
        "https://discord.com/oauth2/authorize?client_id={application_id}&scope=bot%20applications.commands&permissions={REQUIRED}&guild_id={guild_id}&disable_guild_select=true"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overwrite(target: Target, allow: u64, deny: u64) -> Overwrite {
        Overwrite {
            target,
            allow,
            deny,
        }
    }

    #[test]
    fn invite_requests_only_read_permissions() {
        assert_eq!(REQUIRED, 66560);
        let url = invite_url(NonZeroU64::new(123).unwrap(), NonZeroU64::new(456).unwrap());
        assert_eq!(
            url,
            "https://discord.com/oauth2/authorize?client_id=123&scope=bot%20applications.commands&permissions=66560&guild_id=456&disable_guild_select=true"
        );
    }

    #[test]
    fn overwrite_precedence_and_role_order() {
        let roles = [
            Role {
                id: 2,
                permissions: REQUIRED,
            },
            Role {
                id: 3,
                permissions: 0,
            },
        ];
        let overwrites = [
            overwrite(Target::Role(1), 0, VIEW_CHANNEL),
            overwrite(Target::Role(2), 0, VIEW_CHANNEL),
            overwrite(Target::Role(3), VIEW_CHANNEL, 0),
        ];
        assert!(can_read_history(effective_permissions(
            1,
            9,
            false,
            0,
            &roles,
            &overwrites
        )));
        assert!(can_read_history(effective_permissions(
            1,
            9,
            false,
            0,
            &[roles[1], roles[0]],
            &overwrites
        )));
        let mut member_denies = overwrites.to_vec();
        member_denies.push(overwrite(Target::Member(9), 0, READ_MESSAGE_HISTORY));
        assert!(!can_read_history(effective_permissions(
            1,
            9,
            false,
            0,
            &roles,
            &member_denies
        )));
        member_denies.pop();
        member_denies.push(overwrite(Target::Member(9), REQUIRED, 0));
        assert!(can_read_history(effective_permissions(
            1,
            9,
            false,
            0,
            &[],
            &member_denies
        )));
        let only_everyone = [overwrite(Target::Role(1), VIEW_CHANNEL, 0)];
        assert!(can_read_history(effective_permissions(
            1,
            9,
            false,
            READ_MESSAGE_HISTORY,
            &[],
            &only_everyone
        )));
    }

    #[test]
    fn unrelated_targets_and_admin_bypass() {
        let denied = [
            overwrite(Target::Role(1), 0, REQUIRED),
            overwrite(Target::Member(8), REQUIRED, 0),
        ];
        assert!(!can_read_history(effective_permissions(
            1,
            9,
            false,
            REQUIRED,
            &[],
            &denied
        )));
        for (owner, base) in [(true, 0), (false, ADMINISTRATOR)] {
            let permissions = effective_permissions(1, 9, owner, base, &[], &denied);
            assert!(can_read_history(permissions));
            assert_ne!(extra_permissions(permissions), 0);
        }
        assert!(!can_read_history(READ_MESSAGE_HISTORY));
        assert_eq!(extra_permissions(REQUIRED), 0);
    }
}
