//! The five read operations behind the MCP tools (plan sections 9 and 10.1).
//!
//! Everything here is synchronous and runs on the blocking pool. It reads through the
//! `teamctx-core` repository traits, ranks with [`teamctx_core::rank`], and returns items
//! whose text is still untrusted; framing happens in [`super::view`].

use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
};

use teamctx_core::{
    citation::{CitationId, DiscordMessageLink},
    id::{ChannelId, MemberId, MessageId, Snowflake},
    query::{self, Clause, Query},
    rank,
    record::{EntryKind, EntryStatus, RecordEntry},
    repo::{
        DirectoryRead, EntryFilter, MessageRead, PageRequest, RecordRead, RepoError, StoredMessage,
    },
    time::UnixMillis,
};
use teamctx_store::Store;

use super::{
    input::{self, MAX_CURSOR_OFFSET},
    view::{Item, ItemKind, MAX_CONTEXT_CHARS, MAX_LINE_CHARS, Summary, clip},
};

/// Entries and messages shown per group by `context_for` unless `limit` says otherwise.
pub(crate) const DEFAULT_CONTEXT_LIMIT: u32 = 4;
/// The most `context_for` accepts for `limit`.
pub(crate) const MAX_CONTEXT_LIMIT: u32 = 8;
/// Results per page for `search` and `record`.
const PAGE: u32 = 5;
const RECORD_PAGE: u32 = 8;
/// Candidates fetched before ranking.
const ENTRY_POOL: u32 = 24;
const MESSAGE_POOL: u32 = 40;
const SEARCH_POOL: u32 = MAX_CURSOR_OFFSET + PAGE + 5;
/// Longest text of the message a `read` is about.
const READ_TARGET_CHARS: usize = 900;
const MAX_NEIGHBOURS: u32 = 10;
/// Member rows scanned when `record` filters by owner name.
const OWNER_SCAN: i64 = 500;

/// Why an operation produced no items.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The arguments were unusable; the text is written here, never chat-derived.
    Rejected(&'static str),
    /// Storage failed. Logged, never shown.
    Repo(RepoError),
}

impl From<RepoError> for Failure {
    fn from(error: RepoError) -> Self {
        Self::Repo(error)
    }
}

pub(crate) type Done = Result<(Vec<Item>, Summary), Failure>;

#[derive(Clone)]
struct ChannelFacts {
    name: String,
    external: Snowflake,
    guild: Option<Snowflake>,
}

/// One request's view of the store, with small caches for names and channels.
pub(crate) struct Ctx<'a> {
    store: &'a Store,
    now: UnixMillis,
    offset: i32,
    members: HashMap<MemberId, String>,
    channels: HashMap<ChannelId, Option<ChannelFacts>>,
}

impl<'a> Ctx<'a> {
    pub(crate) fn new(store: &'a Store, now: UnixMillis, offset: i32) -> Self {
        Self {
            store,
            now,
            offset,
            members: HashMap::new(),
            channels: HashMap::new(),
        }
    }

    fn date(&self, at: UnixMillis) -> String {
        input::format_date(at, self.offset)
    }

    fn member_name(&mut self, id: Option<MemberId>) -> Result<String, RepoError> {
        let Some(id) = id else {
            return Ok("unknown".to_owned());
        };
        if let Some(name) = self.members.get(&id) {
            return Ok(name.clone());
        }
        let name = self
            .store
            .member(id)?
            .map_or_else(|| "unknown".to_owned(), |m| m.display_name);
        self.members.insert(id, name.clone());
        Ok(name)
    }

    fn channel(&mut self, id: ChannelId) -> Result<Option<ChannelFacts>, RepoError> {
        if let Some(found) = self.channels.get(&id) {
            return Ok(found.clone());
        }
        let found = match self.store.channel(id)? {
            Some(channel) => {
                let guild = self.store.source(channel.source)?.map(|s| s.external_id);
                Some(ChannelFacts {
                    name: channel.name,
                    external: channel.external_id,
                    guild,
                })
            }
            None => None,
        };
        self.channels.insert(id, found.clone());
        Ok(found)
    }

    fn link(&mut self, message: &StoredMessage) -> Result<Option<String>, RepoError> {
        let channel = self.channel(message.channel)?;
        Ok(channel.and_then(|c| {
            c.guild.map(|guild| {
                DiscordMessageLink {
                    guild,
                    channel: c.external,
                    message: message.external_id,
                }
                .to_string()
            })
        }))
    }

    fn message_item(&mut self, message: &StoredMessage, context: bool) -> Result<Item, RepoError> {
        let author = self.member_name(message.author)?;
        let channel = self
            .channel(message.channel)?
            .map_or_else(|| "unknown".to_owned(), |c| c.name);
        let link = self.link(message)?;
        let id = CitationId::Message(message.id);
        let max = if context {
            MAX_CONTEXT_CHARS
        } else {
            MAX_LINE_CHARS
        };
        let line = format!(
            "{}{id} | {author} in #{channel} | {} | {}",
            if context { "  (context) " } else { "" },
            self.date(message.created_at),
            clip(&message.content, max),
        );
        Ok(Item::cited(
            id,
            ItemKind::Message,
            None,
            message.created_at,
            link,
            context,
            line,
        ))
    }

    fn entry_item(&mut self, entry: &RecordEntry) -> Result<Item, RepoError> {
        let short = entry.short_id();
        let link = match entry.source_message {
            Some(id) => match self.store.message(id)? {
                Some(message) => self.link(&message)?,
                None => None,
            },
            None => None,
        };
        let mut line = format!(
            "{short} {} {} | {}",
            entry.kind,
            entry.status,
            self.date(entry.created_at)
        );
        if entry.owner.is_some() {
            let owner = self.member_name(entry.owner)?;
            line.push_str(&format!(" | owner {owner}"));
        }
        if let Some(due) = entry.due_at {
            line.push_str(&format!(" | due {}", self.date(due)));
        }
        line.push_str(&format!(
            " | {}",
            clip(entry.title.as_str(), MAX_LINE_CHARS)
        ));
        Ok(Item::cited(
            CitationId::Entry(short),
            entry.kind.into(),
            Some(entry.status.to_string()),
            entry.created_at,
            link,
            false,
            line,
        ))
    }
}

/// Best score first, then newest, then the lower id: a total, stable order.
fn by_rank(a: &(f64, UnixMillis, i64), b: &(f64, UnixMillis, i64)) -> Ordering {
    b.0.total_cmp(&a.0)
        .then_with(|| b.1.cmp(&a.1))
        .then_with(|| a.2.cmp(&b.2))
}

fn ranked_messages(
    hits: Vec<teamctx_core::repo::MessageHit>,
    now: UnixMillis,
) -> Vec<StoredMessage> {
    let mut scored: Vec<_> = hits
        .into_iter()
        .map(|hit| {
            let key = (
                rank::score(hit.bm25, hit.message.created_at, now, false),
                hit.message.created_at,
                hit.message.id.get(),
            );
            (key, hit.message)
        })
        .collect();
    scored.sort_by(|a, b| by_rank(&a.0, &b.0));
    scored.into_iter().map(|(_, message)| message).collect()
}

fn ranked_entries(hits: Vec<teamctx_core::repo::EntryHit>, now: UnixMillis) -> Vec<RecordEntry> {
    let mut scored: Vec<_> = hits
        .into_iter()
        .map(|hit| {
            let key = (
                rank::score(hit.bm25, hit.entry.created_at, now, true),
                hit.entry.created_at,
                hit.entry.id.get(),
            );
            (key, hit.entry)
        })
        .collect();
    scored.sort_by(|a, b| by_rank(&a.0, &b.0));
    scored.into_iter().map(|(_, entry)| entry).collect()
}

/// `context_for`: record entries first, then messages with up to three neighbours (plan
/// section 9).
pub(crate) fn context_for(ctx: &mut Ctx<'_>, text: &str, limit: Option<u32>) -> Done {
    let query = query::context_query(text);
    if query.clauses.is_empty() {
        return Err(Failure::Rejected(
            "`text` has no searchable words; pass a branch name or a task description.",
        ));
    }
    let limit = limit
        .unwrap_or(DEFAULT_CONTEXT_LIMIT)
        .clamp(1, MAX_CONTEXT_LIMIT) as usize;
    let entries = ranked_entries(ctx.store.search_entries(&query, ENTRY_POOL)?, ctx.now);
    let messages = ranked_messages(ctx.store.search_messages(&query, MESSAGE_POOL)?, ctx.now);
    let more = entries.len() > limit || messages.len() > limit;

    let mut items = Vec::new();
    let mut entry_count = 0;
    for entry in entries.iter().take(limit) {
        items.push(ctx.entry_item(entry)?);
        entry_count += 1;
    }
    let hits: Vec<&StoredMessage> = messages.iter().take(limit).collect();
    let hit_ids: HashSet<MessageId> = hits.iter().map(|m| m.id).collect();
    let mut shown: HashSet<MessageId> = HashSet::new();
    for hit in &hits {
        items.push(ctx.message_item(hit, false)?);
        shown.insert(hit.id);
        for neighbour in ctx.store.neighbours(hit.id, 2, 1)? {
            if hit_ids.contains(&neighbour.id) || !shown.insert(neighbour.id) {
                continue;
            }
            items.push(ctx.message_item(&neighbour, true)?);
        }
    }
    let message_count = hits.len();
    let next_step = match items.iter().find_map(|i| i.reference.as_ref()) {
        Some(first) => format!("Next: use `read` with id {} for the full thread.", first.id),
        None => "Next: nothing matched; try `search` with other words.".to_owned(),
    };
    Ok((
        items,
        Summary {
            headline: format!(
                "context_for found {entry_count} record entries and {message_count} messages."
            ),
            next_step,
            next_cursor: None,
            more,
        },
    ))
}

/// `whats_new`: changed record entries and the busiest channels since a time.
pub(crate) fn whats_new(ctx: &mut Ctx<'_>, since: UnixMillis) -> Done {
    const ENTRIES: u32 = 6;
    const CHANNELS: u32 = 3;
    let filter = EntryFilter {
        updated_since: Some(since),
        ..EntryFilter::default()
    };
    let entries = ctx.store.list_entries(
        &filter,
        PageRequest {
            offset: 0,
            limit: ENTRIES + 1,
        },
    )?;
    let more = entries.len() > ENTRIES as usize;
    let mut items = Vec::new();
    for entry in entries.iter().take(ENTRIES as usize) {
        items.push(ctx.entry_item(entry)?);
    }
    let changes = items.len();
    let activity = ctx.store.channel_activity_since(since, CHANNELS)?;
    let mut active = 0;
    for busy in activity {
        let name = ctx
            .channel(busy.channel)?
            .map_or_else(|| "unknown".to_owned(), |c| c.name);
        items.push(Item::plain(format!(
            "#{name}: {} messages since {}",
            busy.messages,
            ctx.date(since)
        )));
        active += 1;
    }
    let next_step = match items.iter().find_map(|i| i.reference.as_ref()) {
        Some(first) => format!(
            "Next: use `read` with id {} for details, or `context_for` with your branch name.",
            first.id
        ),
        None => "Next: nothing changed in that window; try a longer `since`.".to_owned(),
    };
    Ok((
        items,
        Summary {
            headline: format!(
                "whats_new since {}: {changes} record changes, {active} busiest channels.",
                ctx.date(since)
            ),
            next_step,
            next_cursor: None,
            more,
        },
    ))
}

/// Arguments of `record`, already decoded.
pub(crate) struct RecordArgs<'a> {
    pub(crate) kind: Option<&'a str>,
    pub(crate) status: Option<&'a str>,
    pub(crate) topic: Option<&'a str>,
    pub(crate) owner: Option<&'a str>,
    pub(crate) since: Option<UnixMillis>,
    pub(crate) cursor: Option<&'a str>,
}

/// `record`: entries by kind, status, topic, owner and update time, newest first.
pub(crate) fn record(ctx: &mut Ctx<'_>, args: &RecordArgs<'_>) -> Done {
    let kind = match args.kind {
        None => None,
        Some(text) => Some(
            text.parse::<EntryKind>()
                .map_err(|_| Failure::Rejected("`kind` must be decision, task or question."))?,
        ),
    };
    let status = match args.status {
        None => None,
        Some(text) => Some(text.parse::<EntryStatus>().map_err(|_| {
            Failure::Rejected(
                "`status` must be active, superseded, open, done, answered or dropped.",
            )
        })?),
    };
    let offset = match args.cursor {
        None => 0,
        Some(text) => input::decode_cursor(text).ok_or(Failure::Rejected(
            "`cursor` is not valid; omit it to start over.",
        ))?,
    };
    let owner = match args.owner {
        None => None,
        Some(name) => Some(find_member(ctx.store, name)?.ok_or(Failure::Rejected(
            "No team member has that name; owners are matched by display name.",
        ))?),
    };
    let filter = EntryFilter {
        kind,
        status,
        owner,
        updated_since: args.since,
        topic: args.topic.map(str::to_owned),
    };
    let mut entries = ctx.store.list_entries(
        &filter,
        PageRequest {
            offset,
            limit: RECORD_PAGE + 1,
        },
    )?;
    let more = entries.len() > RECORD_PAGE as usize;
    entries.truncate(RECORD_PAGE as usize);
    let mut items = Vec::new();
    for entry in &entries {
        items.push(ctx.entry_item(entry)?);
    }
    let next_cursor = (more && offset + RECORD_PAGE <= MAX_CURSOR_OFFSET)
        .then(|| input::encode_cursor(offset + RECORD_PAGE));
    let next_step = match items.iter().find_map(|i| i.reference.as_ref()) {
        Some(first) => format!(
            "Next: use `read` with id {} for its source thread.",
            first.id
        ),
        None => "Next: no entries match; loosen the filters.".to_owned(),
    };
    Ok((
        items,
        Summary {
            headline: format!("record returned {} entries.", entries.len()),
            next_step,
            next_cursor,
            more: false,
        },
    ))
}

/// Finds a member by display name (case-insensitive) among the first rows. The store has
/// no lookup by name, so this scans a bounded number of member ids.
fn find_member(store: &Store, name: &str) -> Result<Option<MemberId>, RepoError> {
    let mut misses = 0;
    for raw in 1..=OWNER_SCAN {
        let Some(id) = MemberId::new(raw) else {
            continue;
        };
        match store.member(id)? {
            Some(member) => {
                misses = 0;
                if member.display_name.eq_ignore_ascii_case(name.trim()) {
                    return Ok(Some(id));
                }
            }
            None => {
                misses += 1;
                if misses >= 50 {
                    break;
                }
            }
        }
    }
    Ok(None)
}

/// `search`: messages matching the section 9 syntax, ranked, one page at a time.
pub(crate) fn search(ctx: &mut Ctx<'_>, text: &str, cursor: Option<&str>) -> Done {
    let query: Query = query::parse(text).map_err(|error| {
        // The parser's messages never echo the query, so they are safe to pass on.
        Failure::Rejected(match error {
            query::ParseQueryError::TooLong => "The query is too long.",
            query::ParseQueryError::TooManyClauses => "The query has too many terms.",
            query::ParseQueryError::Empty => "The query is empty.",
            query::ParseQueryError::UnclosedQuote => "A quoted phrase needs a closing quote.",
            query::ParseQueryError::UnexpectedQuote => {
                "Separate clauses with spaces and quote a whole value."
            }
            query::ParseQueryError::EmptyValue => "Phrases and filters must not be empty.",
            query::ParseQueryError::Date(_) => "A before:/after: date must look like 2026-04-01.",
        })
    })?;
    let offset = match cursor {
        None => 0,
        Some(text) => input::decode_cursor(text).ok_or(Failure::Rejected(
            "`cursor` is not valid; omit it to start over.",
        ))?,
    } as usize;
    if !query
        .clauses
        .iter()
        .any(|c| matches!(c, Clause::Word(_) | Clause::Phrase(_)))
    {
        return Err(Failure::Rejected(
            "Add at least one word or phrase to search for; filters alone do not search.",
        ));
    }
    let ranked = ranked_messages(ctx.store.search_messages(&query, SEARCH_POOL)?, ctx.now);
    let page: Vec<&StoredMessage> = ranked.iter().skip(offset).take(PAGE as usize).collect();
    let more = ranked.len() > offset + PAGE as usize;
    let mut items = Vec::new();
    for message in &page {
        items.push(ctx.message_item(message, false)?);
    }
    let next_cursor = more.then(|| input::encode_cursor(offset as u32 + PAGE));
    let next_step = match items.iter().find_map(|i| i.reference.as_ref()) {
        Some(first) => format!(
            "Next: use `read` with id {} for the surrounding thread.",
            first.id
        ),
        None => "Next: no messages matched; try other words.".to_owned(),
    };
    Ok((
        items,
        Summary {
            headline: format!("search returned {} messages.", page.len()),
            next_step,
            next_cursor,
            more: false,
        },
    ))
}

/// `read`: one message with its thread neighbours, or one entry with its source thread.
pub(crate) fn read(ctx: &mut Ctx<'_>, id: &str, before: Option<u32>, after: Option<u32>) -> Done {
    let id = input::parse_id(id).ok_or(Failure::Rejected(
        "`id` must look like m:123, D-12, T-7 or Q-3.",
    ))?;
    let before = before.unwrap_or(2).min(MAX_NEIGHBOURS);
    let after = after.unwrap_or(2).min(MAX_NEIGHBOURS);
    let not_found = Failure::Rejected("No item with that id.");
    let mut items = Vec::new();
    match id {
        CitationId::Message(message_id) => {
            let thread = match ctx.store.neighbours(message_id, before, after) {
                Ok(thread) => thread,
                Err(RepoError::NotFound { .. }) => return Err(not_found),
                Err(other) => return Err(other.into()),
            };
            push_thread(ctx, &mut items, &thread, message_id)?;
        }
        CitationId::Entry(short) => {
            let entry = ctx.store.entry(short)?.ok_or(not_found)?;
            items.push(ctx.entry_item(&entry)?);
            if let Some(source) = entry.source_message
                && let Ok(thread) = ctx.store.neighbours(source, 2, 1)
            {
                push_thread(ctx, &mut items, &thread, source)?;
            }
            if let Some(answer) = entry.answered_by_message
                && let Some(message) = ctx.store.message(answer)?
            {
                items.push(ctx.message_item(&message, true)?);
            }
        }
    }
    Ok((
        items,
        Summary {
            headline: format!("read {id}."),
            next_step: "Next: use `search` or `context_for` to find related items.".to_owned(),
            next_cursor: None,
            more: false,
        },
    ))
}

fn push_thread(
    ctx: &mut Ctx<'_>,
    items: &mut Vec<Item>,
    thread: &[StoredMessage],
    target: MessageId,
) -> Result<(), RepoError> {
    for message in thread {
        // The message the caller asked for gets the long budget.
        items.push(if message.id == target {
            ctx.message_item_with(message, READ_TARGET_CHARS)?
        } else {
            ctx.message_item(message, true)?
        });
    }
    Ok(())
}

impl Ctx<'_> {
    /// A non-context message item with a custom text budget.
    fn message_item_with(
        &mut self,
        message: &StoredMessage,
        max: usize,
    ) -> Result<Item, RepoError> {
        let mut item = self.message_item(message, false)?;
        let author = self.member_name(message.author)?;
        let channel = self
            .channel(message.channel)?
            .map_or_else(|| "unknown".to_owned(), |c| c.name);
        item.line = format!(
            "{} | {author} in #{channel} | {} | {}",
            CitationId::Message(message.id),
            self.date(message.created_at),
            clip(&message.content, max),
        );
        Ok(item)
    }
}
