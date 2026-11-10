//! The pure command handler: a parsed [`Interaction`] and a store in, an [`Outcome`] out
//! (plan sections 7.3, 8 and 9). No network, no clock, no async; the service runs it on
//! a blocking thread and sends the result.
//!
//! Rules every command follows:
//!
//! - **Who:** the person is `member.user` of the interaction. Their member row is looked
//!   up by user id and created (or renamed) from the interaction, as the sync path does
//!   for message authors. Every write passes them as the actor, so the store's audit log
//!   records who did it.
//! - **Retries:** commands that create an entry use the interaction id as the
//!   [`RequestKey`]; marking a message is unique per message and kind; status changes
//!   treat a repeat as success. A retried or double-submitted interaction creates nothing
//!   twice.
//! - **Who may change what:** plan section 8 limits changes by lifecycle only (no
//!   reopening; owner and due date only while live). It names no per-person rule, so any
//!   member who can run a command may close or edit any entry; the audit log shows who.
//! - **What a reply shows:** chat-derived text only from channels the person can read
//!   (and, with `public:true`, that `@everyone` can read), see [`super::access`]. Entries
//!   typed with a slash command have no source channel and are shown to everyone.
//! - **Text:** everything chat-derived goes through [`render::inline`]; errors are fixed
//!   text or our own ids, never echoed input.

use std::collections::HashMap;

use teamctx_core::citation::DiscordMessageLink;
use teamctx_core::id::{ChannelId, MemberId, Snowflake, SourceId};
use teamctx_core::query::{self, Clause, ParseQueryError};
use teamctx_core::rank;
use teamctx_core::record::{
    CreatedVia, EntryKind, EntryStatus, EntryTitle, NewEntry, RecordEntry, RecordRuleError,
    RequestKey, ShortId, StatusChange,
};
use teamctx_core::repo::{
    ApplyOutcome, DirectoryRead, EntryFilter, IngestWrite, MessageRead, PageRequest, RecordEdit,
    RecordRead, RecordWrite, RepoError, StoredMessage,
};
use teamctx_core::source::{Member, MessageRef, SourceEvent};
use teamctx_core::time::{CalendarDate, UnixMillis};
use teamctx_core::when::{DueDate, FixedOffset, Zone, parse_due, parse_since};
use twilight_model::channel::message::embed::Embed;

use super::access::{AccessSnapshot, Viewer};
use super::payload::{Interaction, Person, Request};
use super::render::{self, EXCERPT_CHARS, MAX_ITEMS, NAME_CHARS, TITLE_CHARS};
use crate::connector::StatusSnapshot;
use crate::mapping::{Mapped, map_message};

/// Everything a command may need from a store: the connector's [`crate::store::HostStore`]
/// in production, an in-memory `Store` in tests.
pub trait InteractionStore:
    IngestWrite + DirectoryRead + MessageRead + RecordRead + RecordEdit + RecordWrite
{
}

impl<T> InteractionStore for T where
    T: IngestWrite + DirectoryRead + MessageRead + RecordRead + RecordEdit + RecordWrite + ?Sized
{
}

/// The longest `/catchup` window (plan section 7.3).
pub const CATCHUP_MAX_MS: i64 = 14 * 86_400_000;

/// Candidates read before filtering by access.
const LIST_POOL: u32 = 60;
const SEARCH_POOL: u32 = 60;
const CHANNELS_SHOWN: usize = 5;

/// What the handler needs besides the store.
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// The configured guild.
    pub guild: Snowflake,
    /// The stored source row of the guild, once the connector created it.
    pub source: Option<SourceId>,
    /// Now.
    pub now: UnixMillis,
    /// `server.timezone` (fixed offsets only, plan section 12).
    pub zone: FixedOffset,
    /// Roles and channel overwrites; `None` when they could not be fetched (fail closed).
    pub access: Option<&'a AccessSnapshot>,
    /// The connector's state, for `/teamctx status`.
    pub status: &'a StatusSnapshot,
}

/// What to answer.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// A message.
    Reply(Reply),
    /// The task modal for this message.
    Modal(MessageRef),
}

/// A message reply. `content` and the embeds are fixed text plus escaped chat text.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// The message text.
    pub content: String,
    /// Embeds (lists).
    pub embeds: Vec<Embed>,
    /// Visible to everyone in the channel (`public:true`); otherwise ephemeral.
    pub public: bool,
}

impl Reply {
    /// An ephemeral reply with fixed text.
    #[must_use]
    pub fn note(text: &str) -> Self {
        Self {
            content: text.to_owned(),
            embeds: Vec::new(),
            public: false,
        }
    }
}

const NOT_IN_SERVER: &str = "teamctx answers only in the server it is set up for.";
const NO_MEMBER: &str = "I could not tell who ran this command.";
const STARTING: &str = "teamctx is still connecting to Discord; try again in a minute.";
const UNSUPPORTED: &str = "This command is not available in this version of teamctx.";
const STORAGE: &str =
    "Something went wrong on the teamctx host; nothing was changed. The host's log has details.";
const NOT_SYNCED: &str = "That message is not in the record yet; its channel may still be syncing. Try again in a minute.";
const EXCLUDED: &str = "That channel is excluded from the team record by the host's configuration.";
const NOT_CAPTURABLE: &str = "That kind of message cannot be recorded.";
const NO_TEXT: &str = "That message has no text to copy as a title. Use /decide, /todo add or /question with a title instead.";
const MESSAGE_NOT_FOUND: &str = "I cannot find that message in the record. It may be deleted, in a channel you cannot read, or not synced yet.";
const ACCESS_UNKNOWN: &str = "I could not check which channels you can read, so nothing from chat is shown. Try again in a minute.";
const HIDDEN_NOTE: &str = "Some items are hidden: they come from channels you (or, for a public reply, everyone) cannot read.";

/// Runs one interaction. Never fails: every error becomes an ephemeral reply with fixed
/// text, and storage failures are logged without their details.
pub fn handle<S>(store: &mut S, ctx: &Context<'_>, interaction: &Interaction) -> Outcome
where
    S: InteractionStore + ?Sized,
{
    if interaction.guild != Some(ctx.guild) {
        return Outcome::Reply(Reply::note(NOT_IN_SERVER));
    }
    let Some(person) = interaction.invoker.as_ref() else {
        return Outcome::Reply(Reply::note(NO_MEMBER));
    };
    match &interaction.request {
        Request::Unsupported => return Outcome::Reply(Reply::note(UNSUPPORTED)),
        Request::Status => return Outcome::Reply(status(ctx.status, interaction.public)),
        _ => {}
    }
    let Some(source) = ctx.source else {
        return Outcome::Reply(Reply::note(STARTING));
    };
    let mut run = Run {
        store,
        ctx,
        source,
        interaction,
        person,
        public: interaction.public,
        viewer: Viewer::Member {
            user: person.id,
            roles: interaction.roles.clone(),
        },
        channels: HashMap::new(),
        hidden: false,
    };
    match run.dispatch() {
        Ok(outcome) => outcome,
        Err(Fail::User(text)) => Outcome::Reply(Reply::note(&text)),
        Err(Fail::Repo(error)) => {
            tracing::warn!(
                command = interaction.request.name(),
                %error,
                "an interaction could not be completed"
            );
            Outcome::Reply(Reply::note(STORAGE))
        }
    }
}

/// Why a command stopped. `User` text is fixed or built from our own ids.
#[derive(Debug)]
enum Fail {
    User(String),
    Repo(RepoError),
}

impl From<RepoError> for Fail {
    fn from(error: RepoError) -> Self {
        match error {
            // Rule texts are fixed (lifecycle, title limits, edit rules).
            RepoError::Rule(rule) => Self::User(sentence(&rule.to_string())),
            other => Self::Repo(other),
        }
    }
}

impl From<RecordRuleError> for Fail {
    fn from(rule: RecordRuleError) -> Self {
        Self::User(sentence(&rule.to_string()))
    }
}

fn user(text: &str) -> Fail {
    Fail::User(text.to_owned())
}

/// Capitalises fixed error text and ends it with a full stop.
fn sentence(text: &str) -> String {
    let mut chars = text.chars();
    let mut out: String = chars
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    out.push_str(chars.as_str());
    if !out.ends_with('.') {
        out.push('.');
    }
    out
}

type Done = Result<Outcome, Fail>;

/// One command's state: the store, the person, and a cache of channel visibility.
struct Run<'s, 'c, S: ?Sized> {
    store: &'s mut S,
    ctx: &'c Context<'c>,
    source: SourceId,
    interaction: &'c Interaction,
    person: &'c Person,
    public: bool,
    viewer: Viewer,
    channels: HashMap<ChannelId, bool>,
    /// Something was left out because of access.
    hidden: bool,
}

impl<S: InteractionStore + ?Sized> Run<'_, '_, S> {
    fn dispatch(&mut self) -> Done {
        match &self.interaction.request {
            Request::Mark {
                kind,
                message,
                resolved,
            } => self.mark(*kind, *message, resolved),
            Request::TaskModal {
                message,
                owner,
                due,
            } => self.task_modal(*message, owner.as_ref(), due.as_deref()),
            Request::Decide { text, replaces } => self.decide(text, replaces.as_deref()),
            Request::Decisions {
                topic,
                include_superseded,
            } => self.decisions(topic.as_deref(), *include_superseded),
            Request::TodoAdd { text, owner, due } => {
                self.todo_add(text, owner.as_ref(), due.as_deref())
            }
            Request::TodoList { owner, status } => {
                self.todo_list(owner.as_ref(), status.as_deref())
            }
            Request::TodoDone { id } => self.todo_done(id),
            Request::Question { text } => self.question(text),
            Request::Answer { id, link } => self.answer(id, link),
            Request::Catchup { since } => self.catchup(since.as_deref()),
            Request::Search {
                query,
                channel,
                author,
            } => self.search(query, *channel, author.as_ref()),
            Request::Status | Request::Unsupported => Ok(Outcome::Reply(Reply::note(UNSUPPORTED))),
        }
    }

    // ---- people, channels, access ----

    /// The member row for a person, created or renamed from the interaction.
    fn member(&mut self, person: &Person) -> Result<MemberId, Fail> {
        if let Some(found) = self.store.member_by_external(self.source, person.id)?
            && found.display_name == person.display_name
            && found.is_bot == person.is_bot
        {
            return Ok(found.id);
        }
        self.store.apply_event(
            self.source,
            &SourceEvent::MemberUpsert(Member {
                id: person.id,
                display_name: person.display_name.clone(),
                is_bot: person.is_bot,
            }),
            self.ctx.now,
        )?;
        self.store
            .member_by_external(self.source, person.id)?
            .map(|member| member.id)
            .ok_or(Fail::Repo(RepoError::NotFound { what: "member" }))
    }

    fn actor(&mut self) -> Result<MemberId, Fail> {
        self.member(self.person)
    }

    /// Whether a channel row is shown to this person: included, and readable (through its
    /// parent for threads and forum posts).
    fn channel_visible(&mut self, channel: ChannelId) -> Result<bool, Fail> {
        if let Some(known) = self.channels.get(&channel) {
            return Ok(*known);
        }
        let visible = match self.store.channel(channel)? {
            Some(info) if info.included => {
                let top = match info.parent {
                    Some(parent) => self.store.channel(parent)?.map(|p| p.external_id),
                    None => Some(info.external_id),
                };
                top.is_some_and(|top| self.readable(top))
            }
            _ => false,
        };
        self.channels.insert(channel, visible);
        Ok(visible)
    }

    /// Discord-side access to a top-level channel. Without a snapshot: no.
    fn readable(&self, top: Snowflake) -> bool {
        let Some(access) = self.ctx.access else {
            return false;
        };
        access.readable(&self.viewer, top)
            && (!self.public || access.readable(&Viewer::Everyone, top))
    }

    fn message_visible(&mut self, message: &StoredMessage) -> Result<bool, Fail> {
        self.channel_visible(message.channel)
    }

    /// Entries without a source message were typed in a slash command: shown to all.
    fn entry_visible(&mut self, entry: &RecordEntry) -> Result<bool, Fail> {
        match entry.source_message {
            None => Ok(true),
            Some(id) => match self.store.message(id)? {
                Some(message) => self.message_visible(&message),
                None => Ok(false),
            },
        }
    }

    fn link(&self, message: &StoredMessage) -> Result<Option<String>, Fail> {
        Ok(self.store.channel(message.channel)?.map(|channel| {
            render::jump(DiscordMessageLink {
                guild: self.ctx.guild,
                channel: channel.external_id,
                message: message.external_id,
            })
        }))
    }

    fn name(&self, member: Option<MemberId>) -> Result<String, Fail> {
        Ok(match member {
            Some(id) => self.store.member(id)?.map_or_else(
                || "someone".to_owned(),
                |m| render::inline(&m.display_name, NAME_CHARS),
            ),
            None => "someone".to_owned(),
        })
    }

    fn channel_name(&self, channel: ChannelId) -> Result<String, Fail> {
        Ok(self.store.channel(channel)?.map_or_else(
            || "unknown".to_owned(),
            |c| render::inline(&c.name, NAME_CHARS),
        ))
    }

    /// One list line for an entry the person may see.
    fn entry_line(&mut self, entry: &RecordEntry) -> Result<String, Fail> {
        let short = entry.short_id();
        let source = match entry.source_message {
            Some(id) => self.store.message(id)?,
            None => None,
        };
        let mut line = format!(
            "**{short}** \u{b7} {}",
            render::inline(entry.title.as_str(), TITLE_CHARS)
        );
        match entry.kind {
            EntryKind::Task => {
                if entry.owner.is_some() {
                    line.push_str(&format!(" \u{2014} owner {}", self.name(entry.owner)?));
                }
                if let Some(due) = entry.due_at {
                    line.push_str(&format!(", due {}", render::date(due)));
                }
            }
            EntryKind::Decision | EntryKind::Question => {
                let author = match &source {
                    Some(message) => message.author,
                    None => Some(entry.created_by),
                };
                line.push_str(&format!(
                    " \u{2014} {}, {}",
                    self.name(author)?,
                    render::date(entry.created_at)
                ));
            }
        }
        if entry.status != entry.kind.initial_status() {
            line.push_str(&format!(" ({}", entry.status));
            if let Some(by) = entry.superseded_by {
                line.push_str(&format!(
                    " by {}",
                    ShortId {
                        kind: EntryKind::Decision,
                        id: by
                    }
                ));
            }
            line.push(')');
        }
        if let Some(message) = &source
            && let Some(link) = self.link(message)?
        {
            line.push_str(&format!(" \u{b7} {link}"));
        }
        Ok(line)
    }

    /// A one-line confirmation about one entry. The title is shown only if visible.
    fn about(&mut self, entry: &RecordEntry) -> Result<String, Fail> {
        if self.entry_visible(entry)? {
            self.entry_line(entry)
        } else {
            Ok(format!("**{}**", entry.short_id()))
        }
    }

    fn reply(&self, content: String, embeds: Vec<Embed>) -> Done {
        Ok(Outcome::Reply(Reply {
            content,
            embeds,
            public: self.public,
        }))
    }

    fn footer(&self) -> Option<String> {
        if self.ctx.access.is_none() {
            Some(ACCESS_UNKNOWN.to_owned())
        } else if self.hidden {
            Some(HIDDEN_NOTE.to_owned())
        } else {
            None
        }
    }

    /// Visible entries of `filter`, at most [`MAX_ITEMS`], and how many more there are.
    fn visible_entries(&mut self, filter: &EntryFilter) -> Result<(Vec<RecordEntry>, usize), Fail> {
        let candidates = self.store.list_entries(
            filter,
            PageRequest {
                offset: 0,
                limit: LIST_POOL,
            },
        )?;
        let mut shown = Vec::new();
        let mut more = 0;
        for entry in candidates {
            if !self.entry_visible(&entry)? {
                self.hidden = true;
                continue;
            }
            if shown.len() < MAX_ITEMS {
                shown.push(entry);
            } else {
                more += 1;
            }
        }
        Ok((shown, more))
    }

    fn entry_list(
        &mut self,
        title: &str,
        entries: &[RecordEntry],
        more: usize,
        empty: &str,
    ) -> Result<Embed, Fail> {
        let mut lines = Vec::new();
        for entry in entries {
            lines.push(self.entry_line(entry)?);
        }
        let description = if lines.is_empty() {
            empty.to_owned()
        } else {
            render::clip_lines(&lines, render::MAX_DESCRIPTION, more)
        };
        Ok(render::embed(title, &description, self.footer()))
    }

    fn request_key(&self) -> Result<RequestKey, Fail> {
        RequestKey::new(&format!("discord:{}", self.interaction.envelope.id)).map_err(|_| {
            Fail::Repo(RepoError::NotFound {
                what: "request key",
            })
        })
    }

    fn due(&self, phrase: Option<&str>) -> Result<Option<DueDate>, Fail> {
        phrase
            .map(|text| {
                parse_due(text, self.ctx.now, &self.ctx.zone)
                    .map_err(|e| Fail::User(sentence(&e.to_string())))
            })
            .transpose()
    }

    // ---- capture ----

    /// Finds the target message, storing the resolved copy when the host lacks it.
    fn stored_message(
        &mut self,
        message: MessageRef,
        resolved: &serde_json::Value,
    ) -> Result<StoredMessage, Fail> {
        if let Some(found) = self.store.message_by_ref(self.source, message)? {
            return Ok(found);
        }
        let events = match map_message(resolved) {
            Ok(Mapped::Events(events)) => events,
            Ok(Mapped::Ignored(_)) => return Err(user(NOT_CAPTURABLE)),
            Err(_) => return Err(user(NOT_CAPTURABLE)),
        };
        for event in &events {
            if matches!(event, SourceEvent::MessageUpsert(m) if m.id == message) {
                match self.store.apply_event(self.source, event, self.ctx.now)? {
                    ApplyOutcome::Excluded => return Err(user(EXCLUDED)),
                    ApplyOutcome::UnknownChannel(_) | ApplyOutcome::NeedsFetch(_) => {
                        return Err(user(NOT_SYNCED));
                    }
                    ApplyOutcome::Applied | ApplyOutcome::Unchanged => {}
                }
            }
        }
        self.store
            .message_by_ref(self.source, message)?
            .ok_or_else(|| user(NOT_SYNCED))
    }

    fn capture(
        &mut self,
        kind: EntryKind,
        message: &StoredMessage,
    ) -> Result<(RecordEntry, bool), Fail> {
        let actor = self.actor()?;
        match self.store.capture_message(
            kind,
            message.id,
            actor,
            CreatedVia::ContextMenu,
            self.ctx.now,
        ) {
            Ok(outcome) => Ok((outcome.entry, outcome.changed)),
            Err(RepoError::Rule(RecordRuleError::EmptyTitle)) => Err(user(NO_TEXT)),
            Err(error) => Err(error.into()),
        }
    }

    fn mark(&mut self, kind: EntryKind, message: MessageRef, resolved: &serde_json::Value) -> Done {
        let stored = self.stored_message(message, resolved)?;
        if kind == EntryKind::Task {
            // The modal comes first; the task is created when it is submitted.
            return Ok(Outcome::Modal(message));
        }
        let (entry, created) = self.capture(kind, &stored)?;
        let line = self.entry_line(&entry)?;
        let head = if created {
            "Recorded"
        } else {
            "Already recorded"
        };
        self.reply(format!("{head} as a {kind}: {line}"), Vec::new())
    }

    fn task_modal(
        &mut self,
        message: MessageRef,
        owner: Option<&Person>,
        due: Option<&str>,
    ) -> Done {
        let due = self.due(due)?;
        let stored = self
            .store
            .message_by_ref(self.source, message)?
            .ok_or_else(|| user(MESSAGE_NOT_FOUND))?;
        if !self.message_visible(&stored)? {
            return Err(user(MESSAGE_NOT_FOUND));
        }
        let (mut entry, created) = self.capture(EntryKind::Task, &stored)?;
        let actor = self.actor()?;
        if let Some(owner) = owner {
            let owner = self.member(owner)?;
            entry = self
                .store
                .set_owner(entry.short_id(), Some(owner), actor, self.ctx.now)?
                .entry;
        }
        if let Some(due) = due {
            entry = self
                .store
                .set_due(entry.short_id(), Some(due.at), actor, self.ctx.now)?
                .entry;
        }
        let line = self.entry_line(&entry)?;
        let head = if created {
            "Recorded as a task"
        } else {
            "Updated the task"
        };
        self.reply(format!("{head}: {line}{}", due_echo(due)), Vec::new())
    }

    // ---- typed entries ----

    fn create(&mut self, entry: &NewEntry) -> Done {
        let key = self.request_key()?;
        let replaces = entry.replaces;
        let outcome = match self.store.create_entry_once(&key, entry) {
            Ok(outcome) => outcome,
            Err(RepoError::NotFound { .. }) if replaces.is_some() => {
                return Err(user("The decision it replaces was not found."));
            }
            Err(error) => return Err(error.into()),
        };
        let head = if outcome.changed {
            "Recorded"
        } else {
            "Already recorded"
        };
        let mut line = self.entry_line(&outcome.entry)?;
        if let Some(old) = replaces {
            line.push_str(&format!(
                "; it replaces {}",
                ShortId {
                    kind: EntryKind::Decision,
                    id: old
                }
            ));
        }
        let due = outcome
            .entry
            .due_at
            .and_then(|at| self.ctx.zone_date(at).map(|date| DueDate { date, at }));
        self.reply(format!("{head}: {line}{}", due_echo(due)), Vec::new())
    }

    fn new_entry(&mut self, kind: EntryKind, text: &str) -> Result<NewEntry, Fail> {
        let title = EntryTitle::new(text.split_whitespace().collect::<Vec<_>>().join(" "))?;
        Ok(NewEntry {
            kind,
            title,
            title_is_copied: false,
            owner: None,
            due_at: None,
            source_message: None,
            created_by: self.actor()?,
            created_via: CreatedVia::Slash,
            tags: Vec::new(),
            replaces: None,
            created_at: self.ctx.now,
        })
    }

    fn decide(&mut self, text: &str, replaces: Option<&str>) -> Done {
        let replaces = replaces
            .map(|id| parse_id(id, EntryKind::Decision))
            .transpose()?;
        let mut entry = self.new_entry(EntryKind::Decision, text)?;
        entry.replaces = replaces.map(|short| short.id);
        self.create(&entry)
    }

    fn question(&mut self, text: &str) -> Done {
        let entry = self.new_entry(EntryKind::Question, text)?;
        self.create(&entry)
    }

    fn todo_add(&mut self, text: &str, owner: Option<&Person>, due: Option<&str>) -> Done {
        let due = self.due(due)?;
        let mut entry = self.new_entry(EntryKind::Task, text)?;
        if let Some(owner) = owner {
            entry.owner = Some(self.member(owner)?);
        }
        entry.due_at = due.map(|d| d.at);
        self.create(&entry)
    }

    // ---- lifecycle ----

    fn todo_done(&mut self, id: &str) -> Done {
        let short = parse_id(id, EntryKind::Task)?;
        let actor = self.actor()?;
        let outcome =
            match self
                .store
                .change_status_once(short, StatusChange::Done, actor, self.ctx.now)
            {
                Ok(outcome) => outcome,
                Err(RepoError::NotFound { .. }) => {
                    return Err(Fail::User(format!("There is no task {short}.")));
                }
                Err(error) => return Err(error.into()),
            };
        let line = self.about(&outcome.entry)?;
        let head = if outcome.changed {
            "Done"
        } else {
            "Already done"
        };
        self.reply(format!("{head}: {line}"), Vec::new())
    }

    fn answer(&mut self, id: &str, link: &str) -> Done {
        let short = parse_id(id, EntryKind::Question)?;
        let target = parse_link(link).ok_or_else(|| {
            user("message_link must be a Discord message link (right-click the message, Copy Message Link).")
        })?;
        if target.guild != self.ctx.guild {
            return Err(user("That link points to another server."));
        }
        let message = self
            .store
            .message_by_ref(
                self.source,
                MessageRef {
                    channel: target.channel,
                    message: target.message,
                },
            )?
            .ok_or_else(|| user(MESSAGE_NOT_FOUND))?;
        if !self.message_visible(&message)? {
            return Err(user(MESSAGE_NOT_FOUND));
        }
        let actor = self.actor()?;
        let change = StatusChange::Answer {
            message: message.id,
        };
        let outcome = match self
            .store
            .change_status_once(short, change, actor, self.ctx.now)
        {
            Ok(outcome) => outcome,
            Err(RepoError::NotFound { .. }) => {
                return Err(Fail::User(format!("There is no question {short}.")));
            }
            Err(error) => return Err(error.into()),
        };
        let line = self.about(&outcome.entry)?;
        let answer = self.link(&message)?.unwrap_or_default();
        let head = if outcome.changed {
            "Answered"
        } else {
            "Already answered"
        };
        self.reply(format!("{head}: {line}; answer {answer}"), Vec::new())
    }

    // ---- reads ----

    fn decisions(&mut self, topic: Option<&str>, include_superseded: bool) -> Done {
        let filter = EntryFilter {
            kind: Some(EntryKind::Decision),
            status: (!include_superseded).then_some(EntryStatus::Active),
            topic: topic.map(topic_words),
            ..EntryFilter::default()
        };
        let (entries, more) = self.visible_entries(&filter)?;
        let title = if include_superseded {
            "Decisions (including replaced)"
        } else {
            "Current decisions"
        };
        let embed = self.entry_list(title, &entries, more, "No decisions match.")?;
        self.reply(String::new(), vec![embed])
    }

    fn todo_list(&mut self, owner: Option<&Person>, status: Option<&str>) -> Done {
        let status = match status.unwrap_or("open") {
            "open" => EntryStatus::Open,
            "done" => EntryStatus::Done,
            "dropped" => EntryStatus::Dropped,
            _ => return Err(user("status must be open, done or dropped.")),
        };
        let owner = match owner {
            None => None,
            Some(person) => match self.store.member_by_external(self.source, person.id)? {
                Some(member) => Some(member.id),
                None => {
                    let embed = render::embed("Tasks", "No tasks match.", None);
                    return self.reply(String::new(), vec![embed]);
                }
            },
        };
        let filter = EntryFilter {
            kind: Some(EntryKind::Task),
            status: Some(status),
            owner,
            ..EntryFilter::default()
        };
        let (entries, more) = self.visible_entries(&filter)?;
        let embed = self.entry_list(
            &format!("Tasks ({status})"),
            &entries,
            more,
            "No tasks match.",
        )?;
        self.reply(String::new(), vec![embed])
    }

    fn catchup(&mut self, since: Option<&str>) -> Done {
        let now = self.ctx.now;
        let floor = UnixMillis::from_millis(now.as_millis().saturating_sub(CATCHUP_MAX_MS));
        let actor = self.actor()?;
        let asked = match since {
            Some(text) => parse_since(text, now, &self.ctx.zone)
                .map_err(|e| Fail::User(sentence(&e.to_string())))?,
            None => self.store.last_catchup(actor)?.unwrap_or(floor),
        };
        let capped = asked.as_millis() < floor.as_millis();
        let since = if capped { floor } else { asked };

        let (entries, more) = self.visible_entries(&EntryFilter {
            updated_since: Some(since),
            ..EntryFilter::default()
        })?;
        let mut record = self.entry_list("Record changes", &entries, more, "No record changes.")?;
        // The access note goes once, under the last embed.
        record.footer = None;

        let mut busy = Vec::new();
        for activity in self.store.channel_activity_since(since, 20)? {
            if !self.channel_visible(activity.channel)? {
                self.hidden = true;
                continue;
            }
            if busy.len() == CHANNELS_SHOWN {
                break;
            }
            let external = self.store.channel(activity.channel)?.map(|c| c.external_id);
            if let Some(external) = external {
                busy.push(format!(
                    "<#{external}> \u{2014} {} messages",
                    activity.messages
                ));
            }
        }
        let channels_text = if busy.is_empty() {
            "No messages.".to_owned()
        } else {
            busy.join("\n")
        };

        let mut mentions = Vec::new();
        let mut extra = 0;
        for message in self.store.mentions_since(actor, since, 40)? {
            if !self.message_visible(&message)? {
                self.hidden = true;
                continue;
            }
            if mentions.len() == MAX_ITEMS {
                extra += 1;
                continue;
            }
            mentions.push(self.message_line(&message)?);
        }
        let mentions_text = if mentions.is_empty() {
            "Nobody mentioned you.".to_owned()
        } else {
            render::clip_lines(&mentions, render::MAX_DESCRIPTION, extra)
        };

        self.store.set_last_catchup(actor, now)?;
        let mut content = format!("Since {}", render::long_date(since));
        if capped {
            content.push_str(" (catch-up covers at most 14 days)");
        }
        let footer = self.footer();
        let embeds = vec![
            record,
            render::embed("Most active channels", &channels_text, None),
            render::embed("Mentions of you", &mentions_text, footer),
        ];
        self.reply(content, embeds)
    }

    fn message_line(&mut self, message: &StoredMessage) -> Result<String, Fail> {
        let author = self.name(message.author)?;
        let channel = self.channel_name(message.channel)?;
        let link = self.link(message)?.unwrap_or_default();
        Ok(format!(
            "**#{channel}** \u{b7} {author}, {}: {} \u{b7} {link}",
            render::date(message.created_at),
            render::inline(&message.content, EXCERPT_CHARS)
        ))
    }

    fn search(&mut self, text: &str, channel: Option<Snowflake>, author: Option<&Person>) -> Done {
        let mut query = query::parse(text).map_err(|error| user(query_error(error)))?;
        if !query
            .clauses
            .iter()
            .any(|c| matches!(c, Clause::Word(_) | Clause::Phrase(_)))
        {
            return Err(user(
                "Add at least one word or phrase to search for; filters alone do not search.",
            ));
        }
        if let Some(channel) = channel {
            let found = self
                .store
                .channels(self.source)?
                .into_iter()
                .find(|info| info.external_id == channel);
            let Some(info) = found else {
                return Err(user("That channel is not in the record."));
            };
            if !self.channel_visible(info.id)? {
                return Err(user("That channel is not in the record."));
            }
            query.clauses.push(Clause::In(info.name));
        }
        if let Some(author) = author {
            let Some(member) = self.store.member_by_external(self.source, author.id)? else {
                let embed = render::embed("Search", "No messages match.", None);
                return self.reply(String::new(), vec![embed]);
            };
            query.clauses.push(Clause::From(member.display_name));
        }
        let mut hits = self.store.search_messages(&query, SEARCH_POOL)?;
        let now = self.ctx.now;
        hits.sort_by(|a, b| {
            rank::score(b.bm25, b.message.created_at, now, false)
                .total_cmp(&rank::score(a.bm25, a.message.created_at, now, false))
                .then_with(|| b.message.created_at.cmp(&a.message.created_at))
                .then_with(|| a.message.id.get().cmp(&b.message.id.get()))
        });
        let mut lines = Vec::new();
        let mut more = 0;
        for hit in hits {
            if !self.message_visible(&hit.message)? {
                self.hidden = true;
                continue;
            }
            if lines.len() == MAX_ITEMS {
                more += 1;
                continue;
            }
            lines.push(self.message_line(&hit.message)?);
        }
        let description = if lines.is_empty() {
            "No messages match.".to_owned()
        } else {
            render::clip_lines(&lines, render::MAX_DESCRIPTION, more)
        };
        let embed = render::embed("Search results", &description, self.footer());
        self.reply(String::new(), vec![embed])
    }
}

impl Context<'_> {
    /// The local date of an instant, for echoing a stored due date.
    fn zone_date(&self, at: UnixMillis) -> Option<CalendarDate> {
        self.zone.local_date(at).ok()
    }
}

/// ", due 2026-11-20 (<t:..:D>)" for a confirmation, or nothing.
fn due_echo(due: Option<DueDate>) -> String {
    due.map_or_else(String::new, |due| {
        format!(". Due date: {} ({})", due.date, render::long_date(due.at))
    })
}

/// A short id of the expected kind; errors are fixed text.
fn parse_id(text: &str, kind: EntryKind) -> Result<ShortId, Fail> {
    let short =
        ShortId::parse_user(text.trim()).map_err(|e| Fail::User(sentence(&e.to_string())))?;
    if short.kind != kind {
        let example = match kind {
            EntryKind::Decision => "D-12",
            EntryKind::Task => "T-7",
            EntryKind::Question => "Q-3",
        };
        return Err(Fail::User(format!("Use a {kind} id like {example}.")));
    }
    Ok(short)
}

/// Parses `https://discord.com/channels/{guild}/{channel}/{message}` (also the `ptb.`,
/// `canary.` and `discordapp.com` hosts). Anything else is rejected.
#[must_use]
pub fn parse_link(text: &str) -> Option<DiscordMessageLink> {
    let text = text.trim();
    let rest = text.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    if !matches!(
        host,
        "discord.com" | "ptb.discord.com" | "canary.discord.com" | "discordapp.com"
    ) {
        return None;
    }
    let mut parts = path.strip_prefix("channels/")?.split('/');
    let guild = parts.next()?.parse().ok()?;
    let channel = parts.next()?.parse().ok()?;
    let message = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(DiscordMessageLink {
        guild,
        channel,
        message,
    })
}

/// At most eight words of a topic; the store requires every one.
fn topic_words(topic: &str) -> String {
    topic
        .split_whitespace()
        .take(8)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The parser's errors, worded for Discord. They never echo the query.
const fn query_error(error: ParseQueryError) -> &'static str {
    match error {
        ParseQueryError::TooLong => "The query is too long.",
        ParseQueryError::TooManyClauses => "The query has too many terms.",
        ParseQueryError::Empty => "The query is empty.",
        ParseQueryError::UnclosedQuote => "A quoted phrase needs a closing quote.",
        ParseQueryError::UnexpectedQuote => "Separate clauses with spaces and quote a whole value.",
        ParseQueryError::EmptyValue => "Phrases and filters must not be empty.",
        ParseQueryError::Date(_) => "A before:/after: date must look like 2026-04-01.",
    }
}

/// `/teamctx status`: connector state as fixed text, ids, counts and times.
fn status(status: &StatusSnapshot, public: bool) -> Reply {
    let mut lines = vec![format!("Gateway: {}", status.gateway.as_str())];
    match (status.last_sync_finished_at, status.last_sync_ok) {
        (Some(at), Some(ok)) => lines.push(format!(
            "Last sync: {} ({})",
            render::date(at),
            if ok { "ok" } else { "failed" }
        )),
        _ => lines.push("Last sync: not finished yet".to_owned()),
    }
    lines.push(format!("Sync runs: {}", status.sync_runs));
    lines.push(format!("Live events: {}", status.events));
    lines.push(format!("Disconnects: {}", status.disconnects));
    if let Some(problem) = status.problem {
        lines.push(format!("Problem: {problem}"));
    }
    Reply {
        content: String::new(),
        embeds: vec![render::embed("teamctx status", &lines.join("\n"), None)],
        public,
    }
}
