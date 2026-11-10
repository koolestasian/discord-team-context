//! What the export renders: a read-only snapshot of the record, with every name and link
//! already resolved, so the renderers are pure functions of this value.

use std::collections::BTreeMap;

use teamctx_core::{
    citation::DiscordMessageLink,
    id::{EntryId, MemberId},
    record::{EntryKind, RecordAction, RecordEntry, RecordEvent, ShortId},
    repo::{DirectoryRead, EntryFilter, MessageRead, PageRequest, RecordRead, RepoError},
    time::UnixMillis,
};

use super::text::{md_text, slug};

/// A person as the export names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Person {
    /// The actor id in OKF form, `human:discord-<user id>` (OKF 0.2 section 7).
    pub(crate) actor: String,
    /// The display name. Untrusted text.
    pub(crate) name: String,
}

/// A Discord message an entry cites, as a footnote source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Cited {
    /// The footnote and `sources[].id` label: `m-<message row id>`.
    pub(crate) id: String,
    /// The jump link. Built from numeric ids only.
    pub(crate) link: String,
    /// The channel or thread name. Untrusted text.
    pub(crate) channel: String,
    /// The message author's display name, if known. Untrusted text.
    pub(crate) author: Option<String>,
}

/// One record entry with what the renderers need around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Item {
    pub(crate) entry: RecordEntry,
    /// The message the entry was captured from, if it still exists.
    pub(crate) source: Option<Cited>,
    /// The message that answered a question.
    pub(crate) answer: Option<Cited>,
    /// Decisions this one replaced.
    pub(crate) supersedes: Vec<ShortId>,
    /// The audit log, oldest first.
    pub(crate) events: Vec<RecordEvent>,
}

/// The whole record, ordered by entry id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub(crate) items: Vec<Item>,
    pub(crate) people: BTreeMap<MemberId, Person>,
}

impl Snapshot {
    /// The display name of a member, or a neutral placeholder.
    pub(crate) fn name(&self, id: MemberId) -> &str {
        self.people
            .get(&id)
            .map_or("unknown member", |person| person.name.as_str())
    }

    /// The OKF actor of a member.
    pub(crate) fn actor(&self, id: MemberId) -> &str {
        self.people
            .get(&id)
            .map_or("human:discord-unknown", |person| person.actor.as_str())
    }

    /// The item for a short id, if it is part of the snapshot.
    pub(crate) fn item(&self, id: EntryId) -> Option<&Item> {
        self.items
            .binary_search_by_key(&id, |item| item.entry.id)
            .ok()
            .map(|index| &self.items[index])
    }

    /// Items of one kind, in id order.
    pub(crate) fn of_kind(&self, kind: EntryKind) -> impl Iterator<Item = &Item> {
        self.items
            .iter()
            .filter(move |item| item.entry.kind == kind)
    }
}

impl Item {
    /// The short id, for example `D-12`.
    pub(crate) fn short(&self) -> ShortId {
        self.entry.short_id()
    }

    /// The path of the OKF file for this entry, relative to the bundle root:
    /// `decisions/D-12-use-sqlite.md`. The title slug uses `[a-z0-9-]` only, so the
    /// path cannot leave its directory whatever the title says.
    pub(crate) fn okf_path(&self) -> String {
        let slug = slug(self.entry.title.as_str());
        let short = self.short();
        if slug.is_empty() {
            format!("{}/{short}.md", kind_dir(self.entry.kind))
        } else {
            format!("{}/{short}-{slug}.md", kind_dir(self.entry.kind))
        }
    }
}

/// The directory of an entry kind in the OKF bundle.
pub(crate) const fn kind_dir(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Decision => "decisions",
        EntryKind::Task => "tasks",
        EntryKind::Question => "questions",
    }
}

/// Why the record could not be read.
#[derive(Debug)]
pub(crate) enum GatherError {
    /// The store failed.
    Store(RepoError),
}

impl std::fmt::Display for GatherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Store(error) => write!(f, "cannot read the record: {error}"),
        }
    }
}

impl From<RepoError> for GatherError {
    fn from(error: RepoError) -> Self {
        Self::Store(error)
    }
}

fn person(store: &impl DirectoryRead, id: MemberId) -> Result<Person, GatherError> {
    Ok(match store.member(id)? {
        Some(member) => Person {
            actor: format!("human:discord-{}", member.external_id),
            name: member.display_name,
        },
        None => Person {
            actor: "human:discord-unknown".to_owned(),
            name: "unknown member".to_owned(),
        },
    })
}

fn cite(
    store: &(impl DirectoryRead + MessageRead),
    message: teamctx_core::id::MessageId,
) -> Result<Option<Cited>, GatherError> {
    let Some(stored) = store.message(message)? else {
        return Ok(None);
    };
    let Some(channel) = store.channel(stored.channel)? else {
        return Ok(None);
    };
    let Some(source) = store.source(channel.source)? else {
        return Ok(None);
    };
    let author = match stored.author {
        Some(id) => Some(person(store, id)?.name),
        None => None,
    };
    let link = DiscordMessageLink {
        guild: source.external_id,
        channel: channel.external_id,
        message: stored.external_id,
    };
    Ok(Some(Cited {
        id: format!("m-{message}"),
        link: link.to_string(),
        channel: channel.name,
        author,
    }))
}

/// Reads the whole record. Messages in excluded channels are not visible to the store's
/// readers, so an entry whose source is hidden exports without a source.
///
/// # Errors
///
/// [`GatherError::Store`] when a read fails.
pub(crate) fn gather(
    store: &(impl DirectoryRead + MessageRead + RecordRead),
) -> Result<Snapshot, GatherError> {
    // One query for every entry: a single consistent read, not pages that could shift
    // while the host writes.
    let mut entries = store.list_entries(
        &EntryFilter::default(),
        PageRequest {
            offset: 0,
            limit: u32::MAX,
        },
    )?;
    entries.sort_by_key(|entry| entry.id);
    let mut snapshot = Snapshot::default();
    let mut replaced_by: BTreeMap<EntryId, Vec<ShortId>> = BTreeMap::new();
    for entry in &entries {
        if let Some(by) = entry.superseded_by {
            replaced_by.entry(by).or_default().push(entry.short_id());
        }
    }
    for entry in entries {
        let events = store.entry_events(entry.id)?;
        let mut members = vec![entry.created_by];
        members.extend(entry.owner);
        for event in &events {
            members.extend(event.actor);
            if let teamctx_core::record::RecordAction::OwnerChanged { from, to } = event.action {
                members.extend(from);
                members.extend(to);
            }
        }
        for member in members {
            if let std::collections::btree_map::Entry::Vacant(slot) = snapshot.people.entry(member)
            {
                slot.insert(person(store, member)?);
            }
        }
        let source = match entry.source_message {
            Some(message) => cite(store, message)?,
            None => None,
        };
        let answer = match entry.answered_by_message {
            Some(message) => cite(store, message)?,
            None => None,
        };
        let supersedes = replaced_by.remove(&entry.id).unwrap_or_default();
        snapshot.items.push(Item {
            entry,
            source,
            answer,
            supersedes,
            events,
        });
    }
    Ok(snapshot)
}

/// One audit event as a sentence for the change logs: a label (`Creation` or `Update`)
/// and the text after it. `subject` is how the entry is written in the sentence (a link
/// in the OKF log, the plain short id in the single-file export). Names are escaped.
pub(crate) fn describe(
    snapshot: &Snapshot,
    item: &Item,
    event: &RecordEvent,
    subject: &str,
) -> (&'static str, String) {
    let by = event
        .actor
        .map_or_else(|| "the system".to_owned(), |id| md_text(snapshot.name(id)));
    let name = |member: Option<MemberId>| {
        member.map_or_else(|| "nobody".to_owned(), |id| md_text(snapshot.name(id)))
    };
    let when = |at: Option<UnixMillis>| at.map_or_else(|| "none".to_owned(), iso);
    match event.action {
        RecordAction::Created => (
            "Creation",
            format!("{subject} captured by {by} via {}.", item.entry.created_via),
        ),
        RecordAction::StatusChanged { from, to } => (
            "Update",
            format!("{subject} went from {from} to {to} (by {by})."),
        ),
        RecordAction::SourceDeleted => (
            "Update",
            format!("{subject} lost its source message to a deletion."),
        ),
        RecordAction::OwnerChanged { from, to } => (
            "Update",
            format!(
                "{subject} owner changed from {} to {} (by {by}).",
                name(from),
                name(to)
            ),
        ),
        RecordAction::DueChanged { from, to } => (
            "Update",
            format!(
                "{subject} due date changed from {} to {} (by {by}).",
                when(from),
                when(to)
            ),
        ),
    }
}

/// An ISO 8601 UTC instant, or the raw millisecond count for the (impossible in practice)
/// instants outside years 1 to 9999.
pub(crate) fn iso(at: UnixMillis) -> String {
    at.to_iso8601_utc()
        .unwrap_or_else(|| format!("{}ms", at.as_millis()))
}
