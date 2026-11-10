//! The store as the connector sees it: writes through the single writer thread, reads
//! through the connector's own read-only connection (plan section 5.2).

use teamctx_core::id::{ChannelId, MemberId, MessageId, Snowflake, SourceId};
use teamctx_core::query::Query;
use teamctx_core::repo::{
    ApplyOutcome, ChannelActivity, ChannelInfo, DirectoryRead, IngestWrite, MemberInfo,
    MessageCursor, MessageHit, MessageRead, RepoResult, SourceInfo, StoredMessage, SyncCursor,
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
