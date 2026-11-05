//! Loads the fictional demo dataset (`fixtures/demo/dataset.json`) into a store.
//!
//! One importer serves `teamctx demo` and the evaluation runner, so both load the demo
//! the same way. The order follows `fixtures/demo/README.md` "Loading": source, members,
//! channels (parents first), messages in array order, then entries in array order through
//! [`NewEntry`], with `replaces` applied by the store and the final `done`, `dropped` and
//! `answered` statuses applied afterwards through [`StatusChange`].
//!
//! Everything in the dataset is untrusted data, including the two planted injection
//! messages: the importer stores it and never interprets it.
//!
//! Signature: [`import`] is generic over the repository traits of `teamctx-core` instead
//! of taking a [`Store`](crate::Store), so it works on any implementation. It needs both
//! writes and reads (the store assigns the ids that [`DemoMap`] reports, and re-importing
//! looks up what is already stored). The runtime's `WriterClient` implements writes only,
//! so import with the `Store` directly before moving it into a `Writer`.
//!
//! The whole dataset is parsed and cross-checked before the first write, so malformed
//! input never leaves a half-loaded database behind. Errors name the section and the
//! record's array index, never its content.

use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
};

use serde_json::Value;
use teamctx_core::{
    id::{EntryId, MemberId, MessageId, Snowflake, SourceId},
    record::{
        CreatedVia, EntryKind, EntryStatus, EntryTitle, NewEntry, ShortId, StatusChange, Tag,
    },
    repo::{
        ApplyOutcome, DirectoryRead, EntryFilter, IngestWrite, MessageRead, PageRequest,
        RecordRead, RecordWrite, RepoError,
    },
    source::{Channel, Member, Message, MessageRef, SourceEvent, SourceKind},
    time::UnixMillis,
};

/// The dataset schema this importer reads.
const SCHEMA_VERSION: i64 = 1;
/// Page size when listing the entries already stored.
const PAGE: u32 = 500;

/// Why the demo could not be imported.
#[derive(Debug)]
pub enum DemoError {
    /// The text is not JSON of the expected top-level shape.
    NotJson,
    /// The dataset is not schema version 1, or is not marked fictional.
    UnsupportedDataset,
    /// A field is missing, has the wrong type or is not valid for its type.
    Malformed {
        /// Dataset section, such as `"entries"`.
        section: &'static str,
        /// Zero-based position in that section's array.
        index: usize,
        /// The field name.
        field: &'static str,
    },
    /// A reference points at a record that is not in the dataset (or comes later where
    /// the loading order needs it earlier).
    Dangling {
        /// Dataset section holding the reference.
        section: &'static str,
        /// Zero-based position in that section's array.
        index: usize,
        /// The field holding the reference.
        field: &'static str,
    },
    /// The store rejected a record.
    Rejected {
        /// Dataset section, such as `"messages"`.
        section: &'static str,
        /// Zero-based position in that section's array.
        index: usize,
        /// The store's error.
        source: RepoError,
    },
    /// The store accepted a record but did not keep it: an unknown channel or an
    /// excluded one, or no row found after the write.
    NotStored {
        /// Dataset section, such as `"messages"`.
        section: &'static str,
        /// Zero-based position in that section's array.
        index: usize,
    },
    /// After loading, an entry's stored status differs from the dataset's snapshot.
    StatusMismatch {
        /// Zero-based position in the `entries` array.
        index: usize,
    },
}

impl fmt::Display for DemoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotJson => f.write_str("demo dataset is not valid JSON of the expected shape"),
            Self::UnsupportedDataset => {
                f.write_str("demo dataset is not a fictional schema version 1 dataset")
            }
            Self::Malformed {
                section,
                index,
                field,
            } => write!(
                f,
                "demo {section} record {index}: field `{field}` is missing or invalid"
            ),
            Self::Dangling {
                section,
                index,
                field,
            } => write!(
                f,
                "demo {section} record {index}: `{field}` refers to a record that is not available"
            ),
            Self::Rejected {
                section,
                index,
                source,
            } => write!(
                f,
                "demo {section} record {index} was rejected by the store: {source}"
            ),
            Self::NotStored { section, index } => {
                write!(f, "demo {section} record {index} was not stored")
            }
            Self::StatusMismatch { index } => write!(
                f,
                "demo entries record {index}: stored status differs from the dataset"
            ),
        }
    }
}

impl Error for DemoError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Rejected { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Maps the dataset's ids to the ids the store assigned, and back.
///
/// Entry ids in `eval.toml` (`D-3`, `T-4`) are fixture-local; on an empty database they
/// equal the real ids, but not on a database that already holds entries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DemoMap {
    source: SourceId,
    members: BTreeMap<Snowflake, MemberId>,
    messages: BTreeMap<Snowflake, MessageId>,
    message_externals: BTreeMap<MessageId, Snowflake>,
    entries: BTreeMap<ShortId, ShortId>,
    fixture_entries: BTreeMap<ShortId, ShortId>,
}

impl DemoMap {
    /// The stored source.
    #[must_use]
    pub fn source(&self) -> SourceId {
        self.source
    }

    /// The row id of a member by external id.
    #[must_use]
    pub fn member(&self, external: Snowflake) -> Option<MemberId> {
        self.members.get(&external).copied()
    }

    /// The row id of a message by its external (snowflake) id.
    #[must_use]
    pub fn message(&self, external: Snowflake) -> Option<MessageId> {
        self.messages.get(&external).copied()
    }

    /// The dataset's external id of a stored message.
    #[must_use]
    pub fn message_external(&self, id: MessageId) -> Option<Snowflake> {
        self.message_externals.get(&id).copied()
    }

    /// The stored short id of a fixture entry (`D-3` in `eval.toml`).
    #[must_use]
    pub fn entry(&self, fixture: ShortId) -> Option<ShortId> {
        self.entries.get(&fixture).copied()
    }

    /// Like [`entry`](Self::entry) for the text form, such as `"D-3"`. `None` for text
    /// that is not a short id or names no fixture entry.
    #[must_use]
    pub fn entry_label(&self, label: &str) -> Option<ShortId> {
        self.entry(label.parse().ok()?)
    }

    /// The fixture short id of a stored entry, for scoring search hits.
    #[must_use]
    pub fn fixture_entry(&self, stored: ShortId) -> Option<ShortId> {
        self.fixture_entries.get(&stored).copied()
    }

    /// How many messages the dataset holds.
    #[must_use]
    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// How many entries the dataset holds.
    #[must_use]
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }
}

/// A record of one dataset section, for typed field access with located errors.
#[derive(Clone, Copy)]
struct Rec<'a> {
    value: &'a Value,
    section: &'static str,
    index: usize,
}

impl<'a> Rec<'a> {
    fn bad(self, field: &'static str) -> DemoError {
        DemoError::Malformed {
            section: self.section,
            index: self.index,
            field,
        }
    }

    fn dangling(self, field: &'static str) -> DemoError {
        DemoError::Dangling {
            section: self.section,
            index: self.index,
            field,
        }
    }

    fn str(self, field: &'static str) -> Result<&'a str, DemoError> {
        self.value
            .get(field)
            .and_then(Value::as_str)
            .ok_or_else(|| self.bad(field))
    }

    fn opt_str(self, field: &'static str) -> Result<Option<&'a str>, DemoError> {
        match self.value.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(text)) => Ok(Some(text)),
            Some(_) => Err(self.bad(field)),
        }
    }

    fn snowflake(self, field: &'static str) -> Result<Snowflake, DemoError> {
        self.str(field)?.parse().map_err(|_| self.bad(field))
    }

    fn opt_snowflake(self, field: &'static str) -> Result<Option<Snowflake>, DemoError> {
        self.opt_str(field)?
            .map(|text| text.parse().map_err(|_| self.bad(field)))
            .transpose()
    }

    fn bool(self, field: &'static str) -> Result<bool, DemoError> {
        self.value
            .get(field)
            .and_then(Value::as_bool)
            .ok_or_else(|| self.bad(field))
    }

    fn int(self, field: &'static str) -> Result<i64, DemoError> {
        self.value
            .get(field)
            .and_then(Value::as_i64)
            .ok_or_else(|| self.bad(field))
    }

    fn opt_int(self, field: &'static str) -> Result<Option<i64>, DemoError> {
        match self.value.get(field) {
            None | Some(Value::Null) => Ok(None),
            Some(number) => number.as_i64().map(Some).ok_or_else(|| self.bad(field)),
        }
    }

    fn time(self, field: &'static str) -> Result<UnixMillis, DemoError> {
        self.int(field).map(UnixMillis::from_millis)
    }

    fn strings(self, field: &'static str) -> Result<Vec<&'a str>, DemoError> {
        self.value
            .get(field)
            .and_then(Value::as_array)
            .ok_or_else(|| self.bad(field))?
            .iter()
            .map(|item| item.as_str().ok_or_else(|| self.bad(field)))
            .collect()
    }
}

/// Iterates one top-level array as located records.
fn section<'a>(
    data: &'a Value,
    name: &'static str,
) -> Result<impl Iterator<Item = Rec<'a>>, DemoError> {
    let items = data
        .get(name)
        .and_then(Value::as_array)
        .ok_or(DemoError::NotJson)?;
    Ok(items.iter().enumerate().map(move |(index, value)| Rec {
        value,
        section: name,
        index,
    }))
}

struct ParsedSource {
    kind: SourceKind,
    id: Snowflake,
    name: String,
}

struct ParsedMessage {
    index: usize,
    message: Message,
}

struct ParsedEntry {
    index: usize,
    fixture: ShortId,
    status: EntryStatus,
    title: EntryTitle,
    title_is_copied: bool,
    kind: EntryKind,
    owner: Option<Snowflake>,
    due_at: Option<UnixMillis>,
    source_message: Snowflake,
    created_by: Snowflake,
    created_via: CreatedVia,
    tags: Vec<Tag>,
    replaces: Option<ShortId>,
    created_at: UnixMillis,
    updated_at: UnixMillis,
    answered_by: Option<Snowflake>,
}

/// The dataset after parsing and cross-checking, before any write.
struct Parsed {
    source: ParsedSource,
    members: Vec<Member>,
    channels: Vec<(usize, Channel)>,
    messages: Vec<ParsedMessage>,
    entries: Vec<ParsedEntry>,
}

fn parse(text: &str) -> Result<Parsed, DemoError> {
    let data: Value = serde_json::from_str(text).map_err(|_| DemoError::NotJson)?;
    if data.get("schema_version").and_then(Value::as_i64) != Some(SCHEMA_VERSION)
        || data.get("fictional").and_then(Value::as_bool) != Some(true)
    {
        return Err(DemoError::UnsupportedDataset);
    }
    let source = {
        let rec = Rec {
            value: data.get("source").ok_or(DemoError::NotJson)?,
            section: "source",
            index: 0,
        };
        ParsedSource {
            kind: rec.str("kind")?.parse().map_err(|_| rec.bad("kind"))?,
            id: rec.snowflake("external_id")?,
            name: rec.str("name")?.to_owned(),
        }
    };

    let mut members = Vec::new();
    let mut known_members = BTreeSet::new();
    for rec in section(&data, "members")? {
        let member = Member {
            id: rec.snowflake("id")?,
            display_name: rec.str("display_name")?.to_owned(),
            is_bot: rec.bool("is_bot")?,
        };
        if !known_members.insert(member.id) {
            return Err(rec.bad("id"));
        }
        members.push(member);
    }

    let mut channels = Vec::new();
    let mut known_channels = BTreeSet::new();
    for rec in section(&data, "channels")? {
        let channel = Channel {
            id: rec.snowflake("id")?,
            parent: rec.opt_snowflake("parent_id")?,
            kind: rec.str("kind")?.parse().map_err(|_| rec.bad("kind"))?,
            name: rec.str("name")?.to_owned(),
            topic: rec.opt_str("topic")?.map(str::to_owned),
            included: rec.bool("included")?,
            archived: rec.bool("archived")?,
        };
        channel.validate().map_err(|_| rec.bad("parent_id"))?;
        if channel
            .parent
            .is_some_and(|parent| !known_channels.contains(&parent))
        {
            return Err(rec.dangling("parent_id"));
        }
        if !known_channels.insert(channel.id) {
            return Err(rec.bad("id"));
        }
        channels.push((rec.index, channel));
    }

    let mut messages = Vec::new();
    // Message id -> its channel, for reply and entry references.
    let mut known_messages: BTreeMap<Snowflake, Snowflake> = BTreeMap::new();
    let mut authors: BTreeMap<Snowflake, &Member> = BTreeMap::new();
    for member in &members {
        authors.insert(member.id, member);
    }
    for rec in section(&data, "messages")? {
        let channel = rec.snowflake("channel_id")?;
        if !known_channels.contains(&channel) {
            return Err(rec.dangling("channel_id"));
        }
        let author = authors
            .get(&rec.snowflake("author_id")?)
            .ok_or_else(|| rec.dangling("author_id"))?;
        let id = rec.snowflake("id")?;
        let reply_to = rec.opt_snowflake("reply_to")?;
        if let Some(reply) = reply_to
            && known_messages.get(&reply) != Some(&channel)
        {
            return Err(rec.dangling("reply_to"));
        }
        let message = Message {
            id: MessageRef {
                channel,
                message: id,
            },
            author: Some((*author).clone()),
            content: rec.str("content")?.to_owned(),
            mentions: Vec::new(),
            reply_to,
            attachment_names: rec
                .strings("attachment_names")?
                .into_iter()
                .map(str::to_owned)
                .collect(),
            created_at: rec.time("created_at")?,
            edited_at: rec.opt_int("edited_at")?.map(UnixMillis::from_millis),
        };
        if known_messages.insert(id, channel).is_some() {
            return Err(rec.bad("id"));
        }
        messages.push(ParsedMessage {
            index: rec.index,
            message,
        });
    }

    let mut entries: Vec<ParsedEntry> = Vec::new();
    let mut by_fixture: BTreeMap<i64, EntryKind> = BTreeMap::new();
    for rec in section(&data, "entries")? {
        let raw = rec.int("id")?;
        let kind: EntryKind = rec.str("kind")?.parse().map_err(|_| rec.bad("kind"))?;
        let fixture = EntryId::new(raw)
            .map(|id| ShortId { kind, id })
            .ok_or_else(|| rec.bad("id"))?;
        if by_fixture.insert(raw, kind).is_some() {
            return Err(rec.bad("id"));
        }
        let status: EntryStatus = rec.str("status")?.parse().map_err(|_| rec.bad("status"))?;
        if !kind.statuses().contains(&status) {
            return Err(rec.bad("status"));
        }
        let source_message = rec.snowflake("source_message_id")?;
        let created_by = rec.snowflake("created_by")?;
        let owner = rec.opt_snowflake("owner_id")?;
        let answered_by = rec.opt_snowflake("answered_by_message_id")?;
        let replaces = match rec.opt_int("replaces")? {
            None => None,
            Some(id) => match by_fixture.get(&id) {
                Some(&EntryKind::Decision) => Some(ShortId {
                    kind: EntryKind::Decision,
                    id: EntryId::new(id).ok_or_else(|| rec.bad("replaces"))?,
                }),
                _ => return Err(rec.dangling("replaces")),
            },
        };
        if !known_messages.contains_key(&source_message) {
            return Err(rec.dangling("source_message_id"));
        }
        if !known_members.contains(&created_by) {
            return Err(rec.dangling("created_by"));
        }
        if owner.is_some_and(|member| !known_members.contains(&member)) {
            return Err(rec.dangling("owner_id"));
        }
        match (status, answered_by) {
            (EntryStatus::Answered, None) => return Err(rec.bad("answered_by_message_id")),
            (_, Some(message)) if !known_messages.contains_key(&message) => {
                return Err(rec.dangling("answered_by_message_id"));
            }
            _ => {}
        }
        entries.push(ParsedEntry {
            index: rec.index,
            fixture,
            status,
            title: EntryTitle::new(rec.str("title")?).map_err(|_| rec.bad("title"))?,
            title_is_copied: rec.bool("title_is_copied")?,
            kind,
            owner,
            due_at: rec.opt_int("due_at")?.map(UnixMillis::from_millis),
            source_message,
            created_by,
            created_via: rec
                .str("created_via")?
                .parse()
                .map_err(|_| rec.bad("created_via"))?,
            tags: rec
                .strings("tags")?
                .into_iter()
                .map(|tag| Tag::new(tag).map_err(|_| rec.bad("tags")))
                .collect::<Result<_, _>>()?,
            replaces,
            created_at: rec.time("created_at")?,
            updated_at: rec.time("updated_at")?,
            answered_by,
        });
    }

    Ok(Parsed {
        source,
        members,
        channels,
        messages,
        entries,
    })
}

fn rejected(section: &'static str, index: usize) -> impl FnOnce(RepoError) -> DemoError {
    move |source| DemoError::Rejected {
        section,
        index,
        source,
    }
}

/// Applies one source event and requires that the store kept it.
fn apply<S: IngestWrite + ?Sized>(
    store: &mut S,
    source: SourceId,
    event: &SourceEvent,
    now: UnixMillis,
    section: &'static str,
    index: usize,
) -> Result<(), DemoError> {
    match store
        .apply_event(source, event, now)
        .map_err(rejected(section, index))?
    {
        ApplyOutcome::Applied | ApplyOutcome::Unchanged => Ok(()),
        _ => Err(DemoError::NotStored { section, index }),
    }
}

/// What identifies an entry already stored, so re-importing finds it again.
type EntryKey = (&'static str, String, Option<MessageId>, i64);

fn entry_key(
    kind: EntryKind,
    title: &EntryTitle,
    source_message: Option<MessageId>,
    created_at: UnixMillis,
) -> EntryKey {
    (
        kind.as_str(),
        title.as_str().to_owned(),
        source_message,
        created_at.as_millis(),
    )
}

/// Loads the demo dataset into `store` and returns the id mapping.
///
/// Importing the same dataset again changes nothing: members, channels and messages are
/// upserts, and an entry that is already stored (same kind, title, source message and
/// capture time) is reused instead of created again.
///
/// # Errors
///
/// [`DemoError`] when the dataset is malformed (nothing is written in that case) or the
/// store rejects a record.
pub fn import<S>(store: &mut S, dataset: &str) -> Result<DemoMap, DemoError>
where
    S: IngestWrite + RecordWrite + DirectoryRead + MessageRead + RecordRead + ?Sized,
{
    let parsed = parse(dataset)?;
    // Only deletion events use this stamp, and the import sends none.
    let now = parsed
        .entries
        .iter()
        .map(|entry| entry.updated_at)
        .max_by_key(|time| time.as_millis())
        .unwrap_or(UnixMillis::from_millis(0));

    let source = store
        .upsert_source(parsed.source.kind, parsed.source.id, &parsed.source.name)
        .map_err(rejected("source", 0))?;

    let mut members = BTreeMap::new();
    for (index, member) in parsed.members.iter().enumerate() {
        apply(
            store,
            source,
            &SourceEvent::MemberUpsert(member.clone()),
            now,
            "members",
            index,
        )?;
        let stored = store
            .member_by_external(source, member.id)
            .map_err(rejected("members", index))?
            .ok_or(DemoError::NotStored {
                section: "members",
                index,
            })?;
        members.insert(member.id, stored.id);
    }

    for (index, channel) in &parsed.channels {
        apply(
            store,
            source,
            &SourceEvent::ChannelUpsert(channel.clone()),
            now,
            "channels",
            *index,
        )?;
    }

    let mut messages = BTreeMap::new();
    let mut message_externals = BTreeMap::new();
    for parsed_message in &parsed.messages {
        let index = parsed_message.index;
        let message = &parsed_message.message;
        apply(
            store,
            source,
            &SourceEvent::MessageUpsert(message.clone()),
            now,
            "messages",
            index,
        )?;
        let stored = store
            .message_by_ref(source, message.id)
            .map_err(rejected("messages", index))?
            .ok_or(DemoError::NotStored {
                section: "messages",
                index,
            })?;
        messages.insert(message.id.message, stored.id);
        message_externals.insert(stored.id, message.id.message);
    }

    let mut stored_entries = BTreeMap::new();
    let mut offset = 0;
    loop {
        let page = store
            .list_entries(
                &EntryFilter::default(),
                PageRequest {
                    offset,
                    limit: PAGE,
                },
            )
            .map_err(rejected("entries", 0))?;
        stored_entries.extend(page.iter().map(|entry| {
            let key = entry_key(
                entry.kind,
                &entry.title,
                entry.source_message,
                entry.created_at,
            );
            (key, entry.id)
        }));
        if page.len() < PAGE as usize {
            break;
        }
        offset += PAGE;
    }

    let mut entries = BTreeMap::new();
    let mut fixture_entries = BTreeMap::new();
    for entry in &parsed.entries {
        let index = entry.index;
        // Every reference was checked against the dataset in `parse`, so these lookups
        // succeed unless the store lost a row it just wrote.
        let member = |id: Snowflake| {
            members.get(&id).copied().ok_or(DemoError::NotStored {
                section: "members",
                index,
            })
        };
        let message = |id: Snowflake| {
            messages.get(&id).copied().ok_or(DemoError::NotStored {
                section: "messages",
                index,
            })
        };
        let actor = member(entry.created_by)?;
        let source_message = message(entry.source_message)?;
        let replaces = entry
            .replaces
            .and_then(|fixture| entries.get(&fixture))
            .map(|stored: &ShortId| stored.id);
        if entry.replaces.is_some() && replaces.is_none() {
            return Err(DemoError::Dangling {
                section: "entries",
                index,
                field: "replaces",
            });
        }
        let request = NewEntry {
            kind: entry.kind,
            title: entry.title.clone(),
            title_is_copied: entry.title_is_copied,
            owner: entry.owner.map(member).transpose()?,
            due_at: entry.due_at,
            source_message: Some(source_message),
            created_by: actor,
            created_via: entry.created_via,
            tags: entry.tags.clone(),
            replaces,
            created_at: entry.created_at,
        };
        let key = entry_key(
            entry.kind,
            &entry.title,
            Some(source_message),
            entry.created_at,
        );
        let id = match stored_entries.get(&key) {
            Some(&id) => id,
            None => {
                let created = store
                    .create_entry(&request)
                    .map_err(rejected("entries", index))?;
                stored_entries.insert(key, created.id);
                created.id
            }
        };
        let stored = ShortId {
            kind: entry.kind,
            id,
        };
        // Two dataset entries resolving to one stored row would make the map ambiguous.
        if entries.insert(entry.fixture, stored).is_some()
            || fixture_entries.insert(stored, entry.fixture).is_some()
        {
            return Err(DemoError::Malformed {
                section: "entries",
                index,
                field: "id",
            });
        }

        // Statuses that need a separate change. `superseded` was applied by the
        // replacing decision's `replaces`; it is verified below.
        let current = store
            .entry(stored)
            .map_err(rejected("entries", index))?
            .ok_or(DemoError::NotStored {
                section: "entries",
                index,
            })?
            .status;
        if current == entry.kind.initial_status() {
            let change = match entry.status {
                EntryStatus::Done => Some(StatusChange::Done),
                EntryStatus::Dropped => Some(StatusChange::Drop),
                EntryStatus::Answered => match entry.answered_by {
                    Some(answer) => Some(StatusChange::Answer {
                        message: message(answer)?,
                    }),
                    None => None,
                },
                _ => None,
            };
            if let Some(change) = change {
                store
                    .change_status(stored, change, actor, entry.updated_at)
                    .map_err(rejected("entries", index))?;
            }
        }
    }

    // Every entry, including decisions superseded by a later entry, must now match the
    // dataset's snapshot.
    for entry in &parsed.entries {
        let status = match entries.get(&entry.fixture) {
            Some(&stored) => store
                .entry(stored)
                .map_err(rejected("entries", entry.index))?
                .map(|found| found.status),
            None => None,
        };
        if status != Some(entry.status) {
            return Err(DemoError::StatusMismatch { index: entry.index });
        }
    }

    Ok(DemoMap {
        source,
        members,
        messages,
        message_externals,
        entries,
        fixture_entries,
    })
}
