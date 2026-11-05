//! Connector-neutral source data and the [`SourceEvent`] stream (plan sections 5.3, 6,
//! 6.1 and 7.2).
//!
//! A connector (Discord in v0.1) maps platform events into these types; the store's
//! single writer applies them (plan section 5.2). Connectors only know external ids
//! ([`Snowflake`]); the store maps them to row ids.
//!
//! All text in these types (message content, names, topics, attachment names) is
//! written by chat participants. It is untrusted data, never instructions.

use thiserror::Error;

use crate::id::Snowflake;
use crate::record::EntryKind;
use crate::text_enum::text_enum;
use crate::time::UnixMillis;

text_enum! {
    /// Which platform a source is. Text forms match `sources.kind` (plan section 6).
    pub enum SourceKind {
        /// A Discord guild.
        Discord => "discord",
    }
}

text_enum! {
    /// What kind of conversation container a channel row is. Text forms match
    /// `channels.kind` (plan section 6).
    ///
    /// Threads and forum posts are channels with a parent (plan section 5.3: "includes
    /// threads (kind = Thread)").
    pub enum ChannelKind {
        /// A text channel.
        Text => "text",
        /// A thread under a text or announcement channel.
        Thread => "thread",
        /// A top-level forum containing forum posts.
        Forum => "forum",
        /// A post (thread) in a forum channel.
        ForumPost => "forum_post",
        /// An announcement channel.
        Announcement => "announcement",
    }
}

impl ChannelKind {
    /// Whether this kind lives under a parent channel. Threads and forum posts do;
    /// text, forum and announcement channels do not.
    #[must_use]
    pub const fn has_parent(self) -> bool {
        matches!(self, Self::Thread | Self::ForumPost)
    }
}

/// A channel, thread or forum post as the source reports it.
///
/// Invariant (checked by [`Channel::validate`]): `parent` is `Some` exactly when
/// `kind.has_parent()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    /// The channel's external id.
    pub id: Snowflake,
    /// The parent channel for threads and forum posts.
    pub parent: Option<Snowflake>,
    /// The container kind.
    pub kind: ChannelKind,
    /// Display name (untrusted text).
    pub name: String,
    /// Channel topic, if any (untrusted text).
    pub topic: Option<String>,
    /// The connector's allowlist/denylist result from configuration (plan sections 7.2
    /// step 1 and 12). The connector decides it; the store only records it.
    pub included: bool,
    /// Whether the thread is archived. Always `false` for non-thread channels.
    pub archived: bool,
}

/// A channel that breaks the parent invariant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ChannelError {
    /// A thread or forum post without a parent.
    #[error("a {0} must have a parent channel")]
    MissingParent(ChannelKind),
    /// A top-level channel with a parent.
    #[error("a {0} channel must not have a parent")]
    UnexpectedParent(ChannelKind),
    /// A channel that names itself as its parent.
    #[error("a channel cannot be its own parent")]
    SelfParent,
}

impl Channel {
    /// Checks the parent invariant.
    ///
    /// # Errors
    ///
    /// [`ChannelError`] when `parent` does not match `kind`, or names the channel itself.
    pub fn validate(&self) -> Result<(), ChannelError> {
        match (self.kind.has_parent(), self.parent) {
            (true, None) => Err(ChannelError::MissingParent(self.kind)),
            (false, Some(_)) => Err(ChannelError::UnexpectedParent(self.kind)),
            (true, Some(parent)) if parent == self.id => Err(ChannelError::SelfParent),
            _ => Ok(()),
        }
    }
}

/// A person or bot in a source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    /// The user's external id.
    pub id: Snowflake,
    /// Name to show (untrusted text). For Discord: the server nickname if set, else the
    /// global display name, else the username; decided by the mapping (T15).
    pub display_name: String,
    /// Whether the account is a bot. Bot messages are stored but excluded from `/ask`
    /// context by default (plan section 6.1).
    pub is_bot: bool,
}

/// Identifies one message. Messages are unique per `(channel, message)` (plan section
/// 6.1: upserts are keyed on `(channel, external_id)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MessageRef {
    /// The channel, thread or forum post that holds the message.
    pub channel: Snowflake,
    /// The message's external id.
    pub message: Snowflake,
}

/// A complete message as the source reports it (create, or a full fetch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Where the message lives and its id.
    pub id: MessageRef,
    /// The author, if the source reports one. Carried in full so the store can upsert
    /// the member in the same transaction.
    pub author: Option<Member>,
    /// Normalised text with mentions resolved to names (plan section 6). Untrusted.
    pub content: String,
    /// The external id of the message this one replies to, if any.
    pub reply_to: Option<Snowflake>,
    /// Attachment file names only, never contents (plan section 6.1). An empty list
    /// means no attachments; the store derives `has_attachments` from it.
    pub attachment_names: Vec<String>,
    /// Creation time. For Discord this is `id.message.timestamp()`.
    pub created_at: UnixMillis,
    /// Last edit time, if edited.
    pub edited_at: Option<UnixMillis>,
}

/// A partial message update (plan section 7.2: "MESSAGE_UPDATE may be partial: merge it
/// with the stored row, or fetch if missing").
///
/// Semantics, per field: `None` keeps the stored value, `Some(v)` replaces it with `v`.
/// The author, reply target and creation time never change after creation, so they are
/// not patchable. Applying the same patch twice gives the same result as applying it
/// once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessagePatch {
    /// The message to update.
    pub id: MessageRef,
    /// New normalised content.
    pub content: Option<String>,
    /// New attachment names (the full list, not a delta).
    pub attachment_names: Option<Vec<String>>,
    /// New edit time.
    pub edited_at: Option<UnixMillis>,
}

/// A patch was applied to a message with a different id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("patch targets a different message")]
pub struct PatchTargetMismatch;

impl MessagePatch {
    /// Merges this patch into `message`.
    ///
    /// Returns whether any field changed, so the store can report a replay as
    /// unchanged.
    ///
    /// # Errors
    ///
    /// [`PatchTargetMismatch`] if `message.id != self.id`; `message` is left untouched.
    pub fn apply(&self, message: &mut Message) -> Result<bool, PatchTargetMismatch> {
        if message.id != self.id {
            return Err(PatchTargetMismatch);
        }
        let mut changed = false;
        if let Some(content) = &self.content
            && *content != message.content
        {
            message.content.clone_from(content);
            changed = true;
        }
        if let Some(names) = &self.attachment_names
            && *names != message.attachment_names
        {
            message.attachment_names.clone_from(names);
            changed = true;
        }
        if let Some(edited_at) = self.edited_at
            && Some(edited_at) != message.edited_at
        {
            message.edited_at = Some(edited_at);
            changed = true;
        }
        Ok(changed)
    }
}

/// A record-capture marker on a message, such as the configured reaction (plan sections
/// 5.3 and 8; off unless enabled in `reaction_capture`, plan section 12).
///
/// The connector maps the configured emoji to an [`EntryKind`] and drops every other
/// reaction, so unrelated reactions never reach the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Marker {
    /// The marked message.
    pub message: MessageRef,
    /// Who added or removed the marker (capture records who did it, plan section 8).
    pub actor: Snowflake,
    /// The record kind the marker captures.
    pub kind: EntryKind,
}

/// Normalised events every source produces (plan section 5.3).
///
/// Every variant is idempotent when applied by the store: replaying an event changes
/// nothing. The enum is deliberately exhaustive so that adding a variant forces the
/// store and every mapping to handle it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceEvent {
    /// A channel, thread or forum post was created, changed or discovered.
    ChannelUpsert(Channel),
    /// A channel, thread or forum post was deleted. What happens to its stored messages
    /// is an open question (`docs/design/contracts.md`).
    ChannelDelete {
        /// The deleted channel.
        channel: Snowflake,
    },
    /// A member's name or bot flag is known or changed.
    MemberUpsert(Member),
    /// A complete message: created, or fully re-fetched. Keyed on `(channel, id)`.
    MessageUpsert(Message),
    /// A partial update; see [`MessagePatch`] for merge semantics.
    MessageUpdate(MessagePatch),
    /// A message was deleted. The store removes it and applies the deletion rule of
    /// plan section 6.1 to record entries citing it.
    MessageDelete {
        /// The deleted message.
        message: MessageRef,
    },
    /// Several messages in one channel were deleted at once. Same rule as
    /// [`SourceEvent::MessageDelete`] for each id.
    MessageBulkDelete {
        /// The channel holding the messages.
        channel: Snowflake,
        /// External ids of the deleted messages.
        ids: Vec<Snowflake>,
    },
    /// A capture marker was added.
    MarkerAdd(Marker),
    /// A capture marker was removed. Its effect on an existing entry is an open
    /// question (`docs/design/contracts.md`).
    MarkerRemove(Marker),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sf(raw: u64) -> Snowflake {
        Snowflake::new(raw).unwrap()
    }

    fn message() -> Message {
        Message {
            id: MessageRef {
                channel: sf(10),
                message: sf(20),
            },
            author: Some(Member {
                id: sf(30),
                display_name: "Robin Example".to_owned(),
                is_bot: false,
            }),
            content: "We will ship the beta on Friday.".to_owned(),
            reply_to: None,
            attachment_names: vec!["plan.txt".to_owned()],
            created_at: UnixMillis::from_millis(1_000),
            edited_at: None,
        }
    }

    fn empty_patch() -> MessagePatch {
        MessagePatch {
            id: message().id,
            content: None,
            attachment_names: None,
            edited_at: None,
        }
    }

    #[test]
    fn text_enums_round_trip_with_schema_values() {
        let kinds: Vec<_> = ChannelKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(
            kinds,
            ["text", "thread", "forum", "forum_post", "announcement"]
        );
        for kind in ChannelKind::ALL {
            assert_eq!(kind.as_str().parse::<ChannelKind>().unwrap(), *kind);
        }
        assert_eq!(
            "discord".parse::<SourceKind>().unwrap(),
            SourceKind::Discord
        );
        assert!("Discord".parse::<SourceKind>().is_err());
        assert_eq!("forum".parse::<ChannelKind>(), Ok(ChannelKind::Forum));
    }

    #[test]
    fn channel_parent_invariant() {
        let mut channel = Channel {
            id: sf(1),
            parent: None,
            kind: ChannelKind::Text,
            name: "general".to_owned(),
            topic: None,
            included: true,
            archived: false,
        };
        assert_eq!(channel.validate(), Ok(()));

        channel.parent = Some(sf(2));
        assert_eq!(
            channel.validate(),
            Err(ChannelError::UnexpectedParent(ChannelKind::Text))
        );

        channel.kind = ChannelKind::Thread;
        assert_eq!(channel.validate(), Ok(()));

        channel.kind = ChannelKind::ForumPost;
        assert_eq!(channel.validate(), Ok(()));

        channel.parent = Some(sf(1));
        assert_eq!(channel.validate(), Err(ChannelError::SelfParent));

        channel.parent = None;
        assert_eq!(
            channel.validate(),
            Err(ChannelError::MissingParent(ChannelKind::ForumPost))
        );
    }

    #[test]
    fn empty_patch_changes_nothing() {
        let mut stored = message();
        assert_eq!(empty_patch().apply(&mut stored), Ok(false));
        assert_eq!(stored, message());
    }

    #[test]
    fn patch_replaces_only_present_fields() {
        let mut stored = message();
        let patch = MessagePatch {
            content: Some("We will ship the beta on Monday.".to_owned()),
            edited_at: Some(UnixMillis::from_millis(2_000)),
            ..empty_patch()
        };
        assert_eq!(patch.apply(&mut stored), Ok(true));

        let expected = Message {
            content: "We will ship the beta on Monday.".to_owned(),
            edited_at: Some(UnixMillis::from_millis(2_000)),
            ..message()
        };
        assert_eq!(stored, expected);
    }

    #[test]
    fn patch_can_clear_attachments() {
        let mut stored = message();
        let patch = MessagePatch {
            attachment_names: Some(Vec::new()),
            ..empty_patch()
        };
        assert_eq!(patch.apply(&mut stored), Ok(true));
        assert!(stored.attachment_names.is_empty());
    }

    #[test]
    fn patch_is_idempotent() {
        let patch = MessagePatch {
            content: Some("edited".to_owned()),
            attachment_names: Some(vec!["a.png".to_owned()]),
            edited_at: Some(UnixMillis::from_millis(5_000)),
            ..empty_patch()
        };
        let mut once = message();
        assert_eq!(patch.apply(&mut once), Ok(true));
        let mut twice = once.clone();
        assert_eq!(patch.apply(&mut twice), Ok(false));
        assert_eq!(once, twice);
    }

    #[test]
    fn patch_with_same_values_reports_unchanged() {
        let mut stored = message();
        let patch = MessagePatch {
            content: Some(stored.content.clone()),
            attachment_names: Some(stored.attachment_names.clone()),
            ..empty_patch()
        };
        assert_eq!(patch.apply(&mut stored), Ok(false));
    }

    #[test]
    fn patch_for_another_message_is_rejected_without_changes() {
        let mut stored = message();
        let patch = MessagePatch {
            id: MessageRef {
                channel: sf(10),
                message: sf(21),
            },
            content: Some("other".to_owned()),
            ..empty_patch()
        };
        assert_eq!(patch.apply(&mut stored), Err(PatchTargetMismatch));
        assert_eq!(stored, message());

        let other_channel = MessagePatch {
            id: MessageRef {
                channel: sf(11),
                message: sf(20),
            },
            ..empty_patch()
        };
        assert_eq!(other_channel.apply(&mut stored), Err(PatchTargetMismatch));
    }
}
