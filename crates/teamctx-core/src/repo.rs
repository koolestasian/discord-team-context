//! Repository traits that `teamctx-store` implements (plan sections 5.2, 6, 6.1, 8, 9
//! and 10.1).
//!
//! Shape:
//! - **Synchronous.** Core has no async. Callers run these on the single writer thread
//!   or on read connections via `spawn_blocking` (plan section 5.2).
//! - **Split by role.** Write traits (`&mut self`) are implemented by the one writer
//!   connection; read traits (`&self`) by read-only connections. Each write method is
//!   one transaction: it either applies completely or not at all.
//! - **Times passed in.** Every method that stamps a time takes it as a parameter.
//!
//! Visibility rules every read method follows:
//! - messages in channels with `included = false` are never returned;
//! - the store never interprets message text: it is untrusted data.

use std::error::Error as StdError;

use thiserror::Error;

use crate::id::{ChannelId, EntryId, MemberId, MessageId, Snowflake, SourceId, TokenId};
use crate::query::Query;
use crate::record::{
    EntryKind, EntryStatus, NewEntry, RecordEntry, RecordEvent, RecordRuleError, ShortId,
    StatusChange,
};
use crate::source::{ChannelKind, MessageRef, SourceEvent, SourceKind};
use crate::time::UnixMillis;

/// Errors from every repository method.
#[derive(Debug, Error)]
pub enum RepoError {
    /// A referenced row does not exist (or is not visible, see the module rules).
    #[error("{what} not found")]
    NotFound {
        /// What was looked up, for example `"entry"`.
        what: &'static str,
    },
    /// A uniqueness rule was hit, for example a token name already in use.
    #[error("{what} already exists")]
    Conflict {
        /// What collided, for example `"token name"`.
        what: &'static str,
    },
    /// A record rule rejected the change (lifecycle, title, field rules).
    #[error(transparent)]
    Rule(#[from] RecordRuleError),
    /// The storage backend failed. The message is generic on purpose: the source error
    /// may contain SQL or row data and is only for logs at debug level.
    #[error("storage backend error")]
    Backend(#[source] Box<dyn StdError + Send + Sync>),
}

/// Result alias for repository methods.
pub type RepoResult<T> = Result<T, RepoError>;

/// A page window for list methods. Opaque client cursors (plan section 10.1) are
/// encoded by the interface layer on top of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageRequest {
    /// Rows to skip.
    pub offset: u32,
    /// Most rows to return. `0` returns nothing.
    pub limit: u32,
}

/// What [`IngestWrite::apply_event`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    /// Stored state changed.
    Applied,
    /// Nothing changed: a replay, an identical update, or a delete of an unknown row.
    Unchanged,
    /// A [`SourceEvent::MessageUpdate`] for a message that is not stored. The caller
    /// must fetch the full message and send a [`SourceEvent::MessageUpsert`] (plan
    /// section 7.2).
    NeedsFetch(MessageRef),
    /// The event refers to a channel that is not stored. The caller must send a
    /// [`SourceEvent::ChannelUpsert`] for it first, then retry.
    UnknownChannel(Snowflake),
    /// The event is about a channel with `included = false` and was dropped.
    Excluded,
}

/// Sync progress for one channel or thread (`sync_cursors`, plan sections 6 and 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncCursor {
    /// Newest message seen, for catch-up (`GET messages?after=`).
    pub newest_seen: Option<Snowflake>,
    /// Oldest message backfilled, for backfill (`GET messages?before=`).
    pub oldest_backfilled: Option<Snowflake>,
    /// Whether backfill reached its limit or the channel start.
    pub backfill_done: bool,
    /// When the cursor was last written.
    pub updated_at: UnixMillis,
}

/// A stored source (`sources` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    /// Row id.
    pub id: SourceId,
    /// Platform.
    pub kind: SourceKind,
    /// The guild id.
    pub external_id: Snowflake,
    /// Guild name (untrusted text).
    pub name: String,
}

/// A stored channel, thread or forum post (`channels` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    /// Row id.
    pub id: ChannelId,
    /// The source it belongs to.
    pub source: SourceId,
    /// External id.
    pub external_id: Snowflake,
    /// Parent channel for threads and forum posts.
    pub parent: Option<ChannelId>,
    /// Container kind.
    pub kind: ChannelKind,
    /// Name (untrusted text).
    pub name: String,
    /// Topic (untrusted text).
    pub topic: Option<String>,
    /// Allowlist/denylist result.
    pub included: bool,
    /// Archived thread.
    pub archived: bool,
}

/// A stored member (`members` row).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberInfo {
    /// Row id.
    pub id: MemberId,
    /// The source it belongs to.
    pub source: SourceId,
    /// External user id.
    pub external_id: Snowflake,
    /// Name to show (untrusted text).
    pub display_name: String,
    /// Bot account.
    pub is_bot: bool,
}

/// A stored message (`messages` row). `content` is untrusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMessage {
    /// Row id.
    pub id: MessageId,
    /// The channel, thread or forum post holding it.
    pub channel: ChannelId,
    /// External message id.
    pub external_id: Snowflake,
    /// Author, if known.
    pub author: Option<MemberId>,
    /// Normalised content (untrusted).
    pub content: String,
    /// External id of the message it replies to.
    pub reply_to: Option<Snowflake>,
    /// Attachment names only (untrusted); empty means none.
    pub attachment_names: Vec<String>,
    /// Creation time.
    pub created_at: UnixMillis,
    /// Last edit time.
    pub edited_at: Option<UnixMillis>,
}

/// A full-text match on a message, before ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageHit {
    /// The matched message.
    pub message: StoredMessage,
    /// SQLite's raw `bm25()` value: smaller (more negative) is more relevant.
    pub bm25: f64,
}

/// A full-text match on a record entry, before ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct EntryHit {
    /// The matched entry.
    pub entry: RecordEntry,
    /// SQLite's raw `bm25()` value: smaller (more negative) is more relevant.
    pub bm25: f64,
}

/// Message count for one channel or thread over a time window (`whats_new`, plan
/// section 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChannelActivity {
    /// The channel or thread.
    pub channel: ChannelId,
    /// Messages created in the window.
    pub messages: u64,
}

/// Filters for [`RecordRead::list_entries`]. `None` means "any". All filters combine
/// with AND.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EntryFilter {
    /// Only this kind.
    pub kind: Option<EntryKind>,
    /// Only this status.
    pub status: Option<EntryStatus>,
    /// Only entries owned by this member.
    pub owner: Option<MemberId>,
    /// Only entries with `updated_at >= updated_since`.
    pub updated_since: Option<UnixMillis>,
    /// Free text matched against title and tags with full-text search. Untrusted: the
    /// store escapes it as plain terms, never as FTS syntax.
    pub topic: Option<String>,
}

/// SHA-256 of an access token (plan section 13: "SHA-256 stored"). The token itself is
/// never stored; hashing happens in the server crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TokenHash(pub [u8; 32]);

/// A stored access token (`access_tokens` row), without any secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    /// Row id.
    pub id: TokenId,
    /// Unique human name, for example `"robin-laptop"`.
    pub name: String,
    /// Scope text as stored; `"read"` by default (plan section 6).
    pub scopes: String,
    /// Creation time.
    pub created_at: UnixMillis,
    /// Last successful use.
    pub last_used_at: Option<UnixMillis>,
    /// Revocation time; a revoked token never authenticates again.
    pub revoked_at: Option<UnixMillis>,
}

/// Writes from connectors: the ingestion path of plan section 5.2.
pub trait IngestWrite {
    /// Creates or renames the source with this `(kind, external_id)`; returns its id.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn upsert_source(
        &mut self,
        kind: SourceKind,
        external_id: Snowflake,
        name: &str,
    ) -> RepoResult<SourceId>;

    /// Applies one event in one transaction (plan sections 5.2, 6.1).
    ///
    /// Contract per variant:
    /// - `ChannelUpsert`: upsert keyed on `(source, id)`; the parent must already be
    ///   stored, else [`ApplyOutcome::UnknownChannel`] with the parent's id.
    /// - `MemberUpsert`: upsert keyed on `(source, id)`.
    /// - `MessageUpsert`: upsert keyed on `(channel, id)`, upserting the author first;
    ///   updates the FTS index in the same transaction.
    /// - `MessageUpdate`: merge with the stored row using
    ///   [`MessagePatch::apply`](crate::source::MessagePatch::apply);
    ///   [`ApplyOutcome::NeedsFetch`] if the row is missing.
    /// - `MessageDelete` / `MessageBulkDelete`: remove the rows (and so their FTS
    ///   entries); every entry citing one gets `source_deleted = true`, its
    ///   `source_message` cleared, a copied title replaced by
    ///   [`SOURCE_DELETED_TITLE`](crate::record::SOURCE_DELETED_TITLE), and a
    ///   [`RecordAction::SourceDeleted`](crate::record::RecordAction::SourceDeleted)
    ///   event stamped `now` with no actor. An answering message that is deleted clears
    ///   `answered_by_message`.
    /// - `ChannelDelete`, `MarkerAdd`, `MarkerRemove`: semantics pending the open
    ///   questions in `docs/design/contracts.md`.
    ///
    /// Message events for a channel with `included = false` return
    /// [`ApplyOutcome::Excluded`] and store nothing. Replaying any event returns
    /// [`ApplyOutcome::Unchanged`].
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if `source` does not exist; [`RepoError::Backend`].
    fn apply_event(
        &mut self,
        source: SourceId,
        event: &SourceEvent,
        now: UnixMillis,
    ) -> RepoResult<ApplyOutcome>;

    /// Stores the sync cursor for a channel or thread.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if the channel is not stored; [`RepoError::Backend`].
    fn set_cursor(
        &mut self,
        source: SourceId,
        channel: Snowflake,
        cursor: &SyncCursor,
    ) -> RepoResult<()>;

    /// Retention (plan section 6.1): deletes every message with `created_at < cutoff`.
    /// Record entries are kept; a copied title citing a purged message is replaced by
    /// [`SOURCE_DELETED_TITLE`](crate::record::SOURCE_DELETED_TITLE). Whether
    /// `source_deleted` is also set is an open question (`docs/design/contracts.md`).
    /// Returns how many messages were deleted.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn purge_messages_before(&mut self, cutoff: UnixMillis, now: UnixMillis) -> RepoResult<u64>;
}

/// Writes from people: record capture and status changes (plan section 8), and the
/// caller's catch-up marker (`user_state`).
pub trait RecordWrite {
    /// Creates an entry; see [`NewEntry`] for what the store fills in.
    ///
    /// Checks [`NewEntry::validate`]. With `replaces`, also supersedes that decision in
    /// the same transaction (status change, `superseded_by`, and its own
    /// [`RecordAction::StatusChanged`](crate::record::RecordAction::StatusChanged)
    /// event).
    ///
    /// # Errors
    ///
    /// - [`RepoError::Rule`] if a rule fails, or the replaced decision cannot be
    ///   superseded;
    /// - [`RepoError::NotFound`] if `replaces`, `owner`, `created_by` or
    ///   `source_message` does not exist, or `replaces` is not a decision;
    /// - [`RepoError::Backend`].
    fn create_entry(&mut self, entry: &NewEntry) -> RepoResult<RecordEntry>;

    /// Changes an entry's status after [`StatusChange::check`] passes, sets the field
    /// the change carries (`superseded_by` or `answered_by_message`), sets
    /// `updated_at = at`, and appends a `StatusChanged` event with `actor`.
    ///
    /// # Errors
    ///
    /// - [`RepoError::NotFound`] if `id` matches no entry of that kind, or the
    ///   `Supersede { by }` target is not an existing decision, or the `Answer` message
    ///   does not exist;
    /// - [`RepoError::Rule`] if the lifecycle rejects the change;
    /// - [`RepoError::Backend`].
    fn change_status(
        &mut self,
        id: ShortId,
        change: StatusChange,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<RecordEntry>;

    /// Records when `member` last ran `/catchup`.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if the member does not exist; [`RepoError::Backend`].
    fn set_last_catchup(&mut self, member: MemberId, at: UnixMillis) -> RepoResult<()>;
}

/// Writes for access tokens (plan sections 6, 10.1, 13).
pub trait TokenWrite {
    /// Stores a new token hash under a unique name.
    ///
    /// # Errors
    ///
    /// [`RepoError::Conflict`] if the name or hash exists; [`RepoError::Backend`].
    fn create_token(
        &mut self,
        name: &str,
        hash: &TokenHash,
        scopes: &str,
        at: UnixMillis,
    ) -> RepoResult<AccessToken>;

    /// Sets `last_used_at = at`.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if the token does not exist; [`RepoError::Backend`].
    fn touch_token(&mut self, id: TokenId, at: UnixMillis) -> RepoResult<()>;

    /// Sets `revoked_at = at` on the named token. Revoking twice keeps the first time.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if no token has that name; [`RepoError::Backend`].
    fn revoke_token(&mut self, name: &str, at: UnixMillis) -> RepoResult<AccessToken>;
}

/// Lookups of sources, channels, members, cursors and user state.
pub trait DirectoryRead {
    /// The source with this id, if any.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only (missing rows are `Ok(None)`, here and below).
    fn source(&self, id: SourceId) -> RepoResult<Option<SourceInfo>>;

    /// The channel with this id, if any (included or not).
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn channel(&self, id: ChannelId) -> RepoResult<Option<ChannelInfo>>;

    /// Every stored channel, thread and forum post of a source, ordered by row id.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn channels(&self, source: SourceId) -> RepoResult<Vec<ChannelInfo>>;

    /// The member with this id, if any.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn member(&self, id: MemberId) -> RepoResult<Option<MemberInfo>>;

    /// The member with this external user id in a source, if any.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn member_by_external(
        &self,
        source: SourceId,
        external_id: Snowflake,
    ) -> RepoResult<Option<MemberInfo>>;

    /// The sync cursor of a channel or thread, if one was stored.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn cursor(&self, source: SourceId, channel: Snowflake) -> RepoResult<Option<SyncCursor>>;

    /// When `member` last ran `/catchup`, if ever.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn last_catchup(&self, member: MemberId) -> RepoResult<Option<UnixMillis>>;
}

/// Message reads for search, `context_for`, `read` and `whats_new` (plan sections 9,
/// 10.1). Never returns messages from excluded channels.
pub trait MessageRead {
    /// The message with this id, if any.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn message(&self, id: MessageId) -> RepoResult<Option<StoredMessage>>;

    /// The message with this external reference in a source, if any (resolves jump
    /// links and markers).
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn message_by_ref(&self, source: SourceId, id: MessageRef)
    -> RepoResult<Option<StoredMessage>>;

    /// Up to `before` messages before and `after` messages after `id` in the same
    /// channel or thread, plus the message itself, in chronological order (by
    /// `created_at`, then external id).
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if `id` is not a visible message; [`RepoError::Backend`].
    fn neighbours(&self, id: MessageId, before: u32, after: u32) -> RepoResult<Vec<StoredMessage>>;

    /// Full-text candidates for `query`, best `bm25` first, at most `limit`. Ranking
    /// happens afterwards in [`crate::rank`]. Filters in the query (`from:`, `in:`,
    /// dates) restrict the candidates.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only. Query validity is the parser's concern.
    fn search_messages(&self, query: &Query, limit: u32) -> RepoResult<Vec<MessageHit>>;

    /// Channels and threads with messages created at or after `since`, most messages
    /// first (ties by channel row id), at most `limit`.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn channel_activity_since(
        &self,
        since: UnixMillis,
        limit: u32,
    ) -> RepoResult<Vec<ChannelActivity>>;
}

/// Record reads (plan sections 8, 9, 10.1).
pub trait RecordRead {
    /// The entry with this short id. `None` also when the row exists with another kind.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn entry(&self, id: ShortId) -> RepoResult<Option<RecordEntry>>;

    /// Entries matching `filter`, newest `updated_at` first (ties by id, highest first),
    /// windowed by `page`.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn list_entries(&self, filter: &EntryFilter, page: PageRequest)
    -> RepoResult<Vec<RecordEntry>>;

    /// Full-text candidates among entries (title and tags), best `bm25` first, at most
    /// `limit`. `context_for` searches these before messages (plan section 9).
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn search_entries(&self, query: &Query, limit: u32) -> RepoResult<Vec<EntryHit>>;

    /// The audit log of one entry, oldest first.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] if the entry does not exist; [`RepoError::Backend`].
    fn entry_events(&self, id: EntryId) -> RepoResult<Vec<RecordEvent>>;
}

/// Token reads for authentication (plan section 10.1).
pub trait TokenRead {
    /// The token with this hash if it exists and is not revoked.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn active_token(&self, hash: &TokenHash) -> RepoResult<Option<AccessToken>>;

    /// All tokens, revoked included, ordered by name.
    ///
    /// # Errors
    ///
    /// [`RepoError::Backend`] only.
    fn tokens(&self) -> RepoResult<Vec<AccessToken>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_errors_do_not_leak_their_source_in_display() {
        let source: Box<dyn StdError + Send + Sync> =
            "UNIQUE constraint failed: messages.content = 'private text'".into();
        let error = RepoError::Backend(source);
        assert_eq!(error.to_string(), "storage backend error");
        assert!(error.source().is_some());
    }

    #[test]
    fn rule_errors_convert_and_display() {
        let error: RepoError = RecordRuleError::SelfSupersede.into();
        assert_eq!(error.to_string(), "a decision cannot replace itself");
        assert_eq!(
            RepoError::NotFound { what: "entry" }.to_string(),
            "entry not found"
        );
    }

    #[test]
    fn traits_are_object_safe() {
        fn _assert(
            _: &dyn IngestWrite,
            _: &dyn RecordWrite,
            _: &dyn TokenWrite,
            _: &dyn DirectoryRead,
            _: &dyn MessageRead,
            _: &dyn RecordRead,
            _: &dyn TokenRead,
        ) {
        }
    }
}
