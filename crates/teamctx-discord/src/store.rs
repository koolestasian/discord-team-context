//! The store as the connector sees it: writes through the single writer thread, reads
//! through the connector's own read-only connection (plan section 5.2).
//!
//! The interaction layer uses the same type: record writes (`RecordWrite`, `RecordEdit`)
//! go to the writer, which appends their audit events, and record reads to the reader.

use teamctx_core::id::{ChannelId, EntryId, MemberId, MessageId, Snowflake, SourceId};
use teamctx_core::query::Query;
use teamctx_core::record::{
    CreatedVia, EntryKind, NewEntry, RecordEntry, RecordEvent, RequestKey, ShortId, StatusChange,
};
use teamctx_core::repo::{
    ApplyOutcome, ChannelActivity, ChannelInfo, DirectoryRead, EntryFilter, EntryHit, EntryOutcome,
    IngestWrite, MemberInfo, MessageCursor, MessageHit, MessageRead, PageRequest, RecordEdit,
    RecordRead, RecordWrite, RepoResult, SourceInfo, StoredMessage, SyncCursor,
};
use teamctx_core::source::{MessageRef, SourceEvent, SourceKind};
use teamctx_core::time::UnixMillis;
use teamctx_store::{Store, WriterClient};

/// A writer client plus a read-only connection. Each connector thread owns one.
///
/// The writer commits before it answers, so a read made after a write on this value
/// sees that write (WAL readers start a new snapshot per statement).
#[derive(Debug)]
pub struct HostStore {
    writer: WriterClient,
    reader: Store,
}

impl HostStore {
    /// Combines `writer` with `reader` (open it with [`Store::open_read_only`]).
    #[must_use]
    pub fn new(writer: WriterClient, reader: Store) -> Self {
        Self { writer, reader }
    }
}

impl IngestWrite for HostStore {
    fn upsert_source(
        &mut self,
        kind: SourceKind,
        external_id: Snowflake,
        name: &str,
    ) -> RepoResult<SourceId> {
        self.writer.upsert_source(kind, external_id, name)
    }

    fn apply_event(
        &mut self,
        source: SourceId,
        event: &SourceEvent,
        now: UnixMillis,
    ) -> RepoResult<ApplyOutcome> {
        self.writer.apply_event(source, event, now)
    }

    fn set_cursor(
        &mut self,
        source: SourceId,
        channel: Snowflake,
        cursor: &SyncCursor,
    ) -> RepoResult<()> {
        self.writer.set_cursor(source, channel, cursor)
    }

    fn purge_messages_before(&mut self, cutoff: UnixMillis, now: UnixMillis) -> RepoResult<u64> {
        self.writer.purge_messages_before(cutoff, now)
    }
}

impl RecordWrite for HostStore {
    fn create_entry(&mut self, entry: &NewEntry) -> RepoResult<RecordEntry> {
        self.writer.create_entry(entry)
    }

    fn change_status(
        &mut self,
        id: ShortId,
        change: StatusChange,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<RecordEntry> {
        self.writer.change_status(id, change, actor, at)
    }

    fn set_last_catchup(&mut self, member: MemberId, at: UnixMillis) -> RepoResult<()> {
        self.writer.set_last_catchup(member, at)
    }
}

impl RecordEdit for HostStore {
    fn capture_message(
        &mut self,
        kind: EntryKind,
        message: MessageId,
        actor: MemberId,
        via: CreatedVia,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.writer.capture_message(kind, message, actor, via, at)
    }

    fn create_entry_once(
        &mut self,
        key: &RequestKey,
        entry: &NewEntry,
    ) -> RepoResult<EntryOutcome> {
        self.writer.create_entry_once(key, entry)
    }

    fn change_status_once(
        &mut self,
        id: ShortId,
        change: StatusChange,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.writer.change_status_once(id, change, actor, at)
    }

    fn set_owner(
        &mut self,
        id: ShortId,
        owner: Option<MemberId>,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.writer.set_owner(id, owner, actor, at)
    }

    fn set_due(
        &mut self,
        id: ShortId,
        due: Option<UnixMillis>,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.writer.set_due(id, due, actor, at)
    }
}

impl RecordRead for HostStore {
    fn entry(&self, id: ShortId) -> RepoResult<Option<RecordEntry>> {
        self.reader.entry(id)
    }

    fn list_entries(
        &self,
        filter: &EntryFilter,
        page: PageRequest,
    ) -> RepoResult<Vec<RecordEntry>> {
        self.reader.list_entries(filter, page)
    }

    fn search_entries(&self, query: &Query, limit: u32) -> RepoResult<Vec<EntryHit>> {
        self.reader.search_entries(query, limit)
    }

    fn entry_events(&self, id: EntryId) -> RepoResult<Vec<RecordEvent>> {
        self.reader.entry_events(id)
    }
}

impl DirectoryRead for HostStore {
    fn source(&self, id: SourceId) -> RepoResult<Option<SourceInfo>> {
        self.reader.source(id)
    }

    fn channel(&self, id: ChannelId) -> RepoResult<Option<ChannelInfo>> {
        self.reader.channel(id)
    }

    fn channels(&self, source: SourceId) -> RepoResult<Vec<ChannelInfo>> {
        self.reader.channels(source)
    }

    fn member(&self, id: MemberId) -> RepoResult<Option<MemberInfo>> {
        self.reader.member(id)
    }

    fn member_by_external(
        &self,
        source: SourceId,
        external_id: Snowflake,
    ) -> RepoResult<Option<MemberInfo>> {
        self.reader.member_by_external(source, external_id)
    }

    fn cursor(&self, source: SourceId, channel: Snowflake) -> RepoResult<Option<SyncCursor>> {
        self.reader.cursor(source, channel)
    }

    fn last_catchup(&self, member: MemberId) -> RepoResult<Option<UnixMillis>> {
        self.reader.last_catchup(member)
    }
}

impl MessageRead for HostStore {
    fn mentions_since(
        &self,
        member: MemberId,
        since: UnixMillis,
        limit: u32,
    ) -> RepoResult<Vec<StoredMessage>> {
        self.reader.mentions_since(member, since, limit)
    }

    fn message(&self, id: MessageId) -> RepoResult<Option<StoredMessage>> {
        self.reader.message(id)
    }

    fn message_by_ref(
        &self,
        source: SourceId,
        id: MessageRef,
    ) -> RepoResult<Option<StoredMessage>> {
        self.reader.message_by_ref(source, id)
    }

    fn neighbours(&self, id: MessageId, before: u32, after: u32) -> RepoResult<Vec<StoredMessage>> {
        self.reader.neighbours(id, before, after)
    }

    fn search_messages(&self, query: &Query, limit: u32) -> RepoResult<Vec<MessageHit>> {
        self.reader.search_messages(query, limit)
    }

    fn channel_messages_since(
        &self,
        channel: ChannelId,
        since: UnixMillis,
        limit: u32,
        cursor: Option<MessageCursor>,
    ) -> RepoResult<Vec<StoredMessage>> {
        self.reader
            .channel_messages_since(channel, since, limit, cursor)
    }

    fn channel_activity_since(
        &self,
        since: UnixMillis,
        limit: u32,
    ) -> RepoResult<Vec<ChannelActivity>> {
        self.reader.channel_activity_since(since, limit)
    }
}
