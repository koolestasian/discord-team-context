//! The runtime's sole writable connection, with bounded typed command delivery.

use std::sync::{
    Arc, Mutex,
    mpsc::{self, Sender, SyncSender},
};
use std::thread::{self, JoinHandle};

use crate::{Store, backend, invalid};
use teamctx_core::{id::*, record::*, repo::*, source::*, time::UnixMillis};

type Reply<T> = Sender<RepoResult<T>>;
enum Command {
    Source(SourceKind, Snowflake, String, Reply<SourceId>),
    Event(SourceId, SourceEvent, UnixMillis, Reply<ApplyOutcome>),
    Cursor(SourceId, Snowflake, SyncCursor, Reply<()>),
    Purge(UnixMillis, UnixMillis, Reply<u64>),
    CreateEntry(NewEntry, Reply<RecordEntry>),
    Status(
        ShortId,
        StatusChange,
        MemberId,
        UnixMillis,
        Reply<RecordEntry>,
    ),
    Catchup(MemberId, UnixMillis, Reply<()>),
    Capture(
        EntryKind,
        MessageId,
        MemberId,
        CreatedVia,
        UnixMillis,
        Reply<EntryOutcome>,
    ),
    CreateOnce(RequestKey, NewEntry, Reply<EntryOutcome>),
    StatusOnce(
        ShortId,
        StatusChange,
        MemberId,
        UnixMillis,
        Reply<EntryOutcome>,
    ),
    Owner(
        ShortId,
        Option<MemberId>,
        MemberId,
        UnixMillis,
        Reply<EntryOutcome>,
    ),
    Due(
        ShortId,
        Option<UnixMillis>,
        MemberId,
        UnixMillis,
        Reply<EntryOutcome>,
    ),
    CreateToken(String, TokenHash, String, UnixMillis, Reply<AccessToken>),
    TouchToken(TokenId, UnixMillis, Reply<()>),
    RevokeToken(String, UnixMillis, Reply<AccessToken>),
    Shutdown,
}

/// Cloneable synchronous command client. Call from a blocking worker, never directly
/// on an async executor. The 128-command queue applies backpressure to ingestion.
#[derive(Debug, Clone)]
pub struct WriterClient {
    sender: Arc<Mutex<Option<SyncSender<Command>>>>,
}

impl WriterClient {
    fn request<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> RepoResult<T> {
        let (reply, receive) = mpsc::channel();
        {
            let sender = self
                .sender
                .lock()
                .map_err(|_| invalid("writer command lock poisoned"))?;
            sender
                .as_ref()
                .ok_or_else(|| invalid("writer has shut down"))?
                .send(command(reply))
                .map_err(|_| invalid("writer thread disconnected"))?;
        }
        receive
            .recv()
            .map_err(|_| invalid("writer response disconnected"))?
    }
}

/// Owns the writer lifecycle. Shutdown rejects new commands, drains accepted work,
/// checkpoints WAL and joins the thread. Drop also shuts down but cannot report errors.
#[derive(Debug)]
pub struct Writer {
    client: WriterClient,
    thread: Option<JoinHandle<RepoResult<()>>>,
}

impl Writer {
    /// Moves a migrated connection onto a dedicated thread.
    pub fn spawn(mut store: Store) -> RepoResult<Self> {
        let (sender, receive) = mpsc::sync_channel(128);
        let thread = thread::Builder::new()
            .name("teamctx-db-writer".to_owned())
            .spawn(move || {
                while let Ok(command) = receive.recv() {
                    match command {
                        Command::Source(kind, id, name, reply) => {
                            let _ = reply.send(store.upsert_source(kind, id, &name));
                        }
                        Command::Event(source, event, now, reply) => {
                            let _ = reply.send(store.apply_event(source, &event, now));
                        }
                        Command::Cursor(source, channel, cursor, reply) => {
                            let _ = reply.send(store.set_cursor(source, channel, &cursor));
                        }
                        Command::Purge(cutoff, now, reply) => {
                            let _ = reply.send(store.purge_messages_before(cutoff, now));
                        }
                        Command::CreateEntry(entry, reply) => {
                            let _ = reply.send(store.create_entry(&entry));
                        }
                        Command::Status(id, change, actor, at, reply) => {
                            let _ = reply.send(store.change_status(id, change, actor, at));
                        }
                        Command::Catchup(member, at, reply) => {
                            let _ = reply.send(store.set_last_catchup(member, at));
                        }
                        Command::Capture(kind, message, actor, via, at, reply) => {
                            let _ =
                                reply.send(store.capture_message(kind, message, actor, via, at));
                        }
                        Command::CreateOnce(key, entry, reply) => {
                            let _ = reply.send(store.create_entry_once(&key, &entry));
                        }
                        Command::StatusOnce(id, change, actor, at, reply) => {
                            let _ = reply.send(store.change_status_once(id, change, actor, at));
                        }
                        Command::Owner(id, owner, actor, at, reply) => {
                            let _ = reply.send(store.set_owner(id, owner, actor, at));
                        }
                        Command::Due(id, due, actor, at, reply) => {
                            let _ = reply.send(store.set_due(id, due, actor, at));
                        }
                        Command::CreateToken(name, hash, scopes, at, reply) => {
                            let _ = reply.send(store.create_token(&name, &hash, &scopes, at));
                        }
                        Command::TouchToken(id, at, reply) => {
                            let _ = reply.send(store.touch_token(id, at));
                        }
                        Command::RevokeToken(name, at, reply) => {
                            let _ = reply.send(store.revoke_token(&name, at));
                        }
                        Command::Shutdown => break,
                    }
                }
                let busy: i64 = store
                    .conn
                    .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| r.get(0))
                    .map_err(backend)?;
                if busy != 0 {
                    return Err(invalid("WAL checkpoint blocked by an active reader"));
                }
                Ok(())
            })
            .map_err(backend)?;
        Ok(Self {
            client: WriterClient {
                sender: Arc::new(Mutex::new(Some(sender))),
            },
            thread: Some(thread),
        })
    }

    /// A handle for ingestion, record commands or token maintenance.
    #[must_use]
    pub fn client(&self) -> WriterClient {
        self.client.clone()
    }

    /// Drains work and reports checkpoint or thread failures. Existing clients reject
    /// further requests once shutdown begins.
    pub fn shutdown(mut self) -> RepoResult<()> {
        self.stop()
    }

    fn stop(&mut self) -> RepoResult<()> {
        let sender = self
            .client
            .sender
            .lock()
            .map_err(|_| invalid("writer command lock poisoned"))?
            .take();
        let send_failed = sender.is_some_and(|sender| sender.send(Command::Shutdown).is_err());
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| invalid("writer thread panicked"))??;
        }
        if send_failed {
            return Err(invalid("writer thread disconnected"));
        }
        Ok(())
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl IngestWrite for WriterClient {
    fn upsert_source(
        &mut self,
        kind: SourceKind,
        id: Snowflake,
        name: &str,
    ) -> RepoResult<SourceId> {
        self.request(|r| Command::Source(kind, id, name.to_owned(), r))
    }
    fn apply_event(
        &mut self,
        source: SourceId,
        event: &SourceEvent,
        now: UnixMillis,
    ) -> RepoResult<ApplyOutcome> {
        self.request(|r| Command::Event(source, event.clone(), now, r))
    }
    fn set_cursor(
        &mut self,
        source: SourceId,
        channel: Snowflake,
        cursor: &SyncCursor,
    ) -> RepoResult<()> {
        self.request(|r| Command::Cursor(source, channel, *cursor, r))
    }
    fn purge_messages_before(&mut self, cutoff: UnixMillis, now: UnixMillis) -> RepoResult<u64> {
        self.request(|r| Command::Purge(cutoff, now, r))
    }
}
impl RecordWrite for WriterClient {
    fn create_entry(&mut self, entry: &NewEntry) -> RepoResult<RecordEntry> {
        self.request(|r| Command::CreateEntry(entry.clone(), r))
    }
    fn change_status(
        &mut self,
        id: ShortId,
        change: StatusChange,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<RecordEntry> {
        self.request(|r| Command::Status(id, change, actor, at, r))
    }
    fn set_last_catchup(&mut self, member: MemberId, at: UnixMillis) -> RepoResult<()> {
        self.request(|r| Command::Catchup(member, at, r))
    }
}
impl RecordEdit for WriterClient {
    fn capture_message(
        &mut self,
        kind: EntryKind,
        message: MessageId,
        actor: MemberId,
        via: CreatedVia,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.request(|r| Command::Capture(kind, message, actor, via, at, r))
    }
    fn create_entry_once(
        &mut self,
        key: &RequestKey,
        entry: &NewEntry,
    ) -> RepoResult<EntryOutcome> {
        self.request(|r| Command::CreateOnce(key.clone(), entry.clone(), r))
    }
    fn change_status_once(
        &mut self,
        id: ShortId,
        change: StatusChange,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.request(|r| Command::StatusOnce(id, change, actor, at, r))
    }
    fn set_owner(
        &mut self,
        id: ShortId,
        owner: Option<MemberId>,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.request(|r| Command::Owner(id, owner, actor, at, r))
    }
    fn set_due(
        &mut self,
        id: ShortId,
        due: Option<UnixMillis>,
        actor: MemberId,
        at: UnixMillis,
    ) -> RepoResult<EntryOutcome> {
        self.request(|r| Command::Due(id, due, actor, at, r))
    }
}
impl TokenWrite for WriterClient {
    fn create_token(
        &mut self,
        name: &str,
        hash: &TokenHash,
        scopes: &str,
        at: UnixMillis,
    ) -> RepoResult<AccessToken> {
        self.request(|r| Command::CreateToken(name.to_owned(), *hash, scopes.to_owned(), at, r))
    }
    fn touch_token(&mut self, id: TokenId, at: UnixMillis) -> RepoResult<()> {
        self.request(|r| Command::TouchToken(id, at, r))
    }
    fn revoke_token(&mut self, name: &str, at: UnixMillis) -> RepoResult<AccessToken> {
        self.request(|r| Command::RevokeToken(name.to_owned(), at, r))
    }
}
