//! The team record: decisions, tasks and questions people marked (plan sections 6 and 8).
//!
//! Rules that live here, so every surface applies them the same way:
//! - the status lifecycle per kind ([`check_transition`]);
//! - short ids such as `D-12` ([`ShortId`]);
//! - title and tag limits ([`EntryTitle`], [`Tag`]);
//! - field rules for new entries ([`NewEntry::validate`]);
//! - the audit-log action vocabulary ([`RecordAction`]).

use std::fmt;
use std::str::FromStr;

use thiserror::Error;

use crate::id::{EntryId, MemberId, MessageId};
use crate::text_enum::text_enum;
use crate::time::UnixMillis;

text_enum! {
    /// What an entry records. Text forms match `record_entries.kind` (plan section 6).
    pub enum EntryKind {
        /// Something the team decided.
        Decision => "decision",
        /// Something someone should do.
        Task => "task",
        /// Something the team needs to answer.
        Question => "question",
    }
}

impl EntryKind {
    /// The short-id prefix: `D`, `T` or `Q` (plan section 8).
    #[must_use]
    pub const fn prefix(self) -> char {
        match self {
            Self::Decision => 'D',
            Self::Task => 'T',
            Self::Question => 'Q',
        }
    }

    /// The kind for a short-id prefix. Exact match: uppercase only.
    #[must_use]
    pub const fn from_prefix(prefix: char) -> Option<Self> {
        match prefix {
            'D' => Some(Self::Decision),
            'T' => Some(Self::Task),
            'Q' => Some(Self::Question),
            _ => None,
        }
    }

    /// The status a new entry of this kind starts in: decisions start `active`, tasks
    /// and questions start `open` (plan section 8).
    #[must_use]
    pub const fn initial_status(self) -> EntryStatus {
        match self {
            Self::Decision => EntryStatus::Active,
            Self::Task | Self::Question => EntryStatus::Open,
        }
    }

    /// Statuses an entry of this kind may ever have.
    #[must_use]
    pub const fn statuses(self) -> &'static [EntryStatus] {
        match self {
            Self::Decision => &[EntryStatus::Active, EntryStatus::Superseded],
            Self::Task => &[EntryStatus::Open, EntryStatus::Done, EntryStatus::Dropped],
            Self::Question => &[
                EntryStatus::Open,
                EntryStatus::Answered,
                EntryStatus::Dropped,
            ],
        }
    }
}

text_enum! {
    /// Where an entry is in its lifecycle. Text forms match `record_entries.status`
    /// (plan section 6).
    pub enum EntryStatus {
        /// A decision in force.
        Active => "active",
        /// A decision replaced by a newer one.
        Superseded => "superseded",
        /// A task or question not finished yet.
        Open => "open",
        /// A finished task.
        Done => "done",
        /// An answered question.
        Answered => "answered",
        /// A task or question abandoned.
        Dropped => "dropped",
    }
}

/// A status change that the lifecycle forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum TransitionError {
    /// The status does not exist for this kind (for example a `done` decision).
    #[error("a {kind} cannot have status {status}")]
    StatusNotForKind {
        /// The entry kind.
        kind: EntryKind,
        /// The status that does not belong to it.
        status: EntryStatus,
    },
    /// The entry already has the target status. Callers may treat this as a no-op.
    #[error("already {status}")]
    AlreadyInStatus {
        /// The current and requested status.
        status: EntryStatus,
    },
    /// Both statuses belong to the kind, but the lifecycle has no such step.
    #[error("a {kind} cannot go from {from} to {to}")]
    NotAllowed {
        /// The entry kind.
        kind: EntryKind,
        /// The current status.
        from: EntryStatus,
        /// The requested status.
        to: EntryStatus,
    },
}

/// Checks one lifecycle step (plan section 8).
///
/// The complete set of allowed steps:
///
/// | Kind | Steps |
/// |---|---|
/// | decision | `active → superseded` |
/// | task | `open → done`, `open → dropped` |
/// | question | `open → answered`, `open → dropped` |
///
/// Every other pair is rejected, including reopening (the plan defines no way back).
///
/// # Errors
///
/// - [`TransitionError::StatusNotForKind`] if `from` or `to` is not a status of `kind`
///   (`from` is checked first);
/// - [`TransitionError::AlreadyInStatus`] if `from == to`;
/// - [`TransitionError::NotAllowed`] for any other step not in the table.
pub fn check_transition(
    kind: EntryKind,
    from: EntryStatus,
    to: EntryStatus,
) -> Result<(), TransitionError> {
    for status in [from, to] {
        if !kind.statuses().contains(&status) {
            return Err(TransitionError::StatusNotForKind { kind, status });
        }
    }
    if from == to {
        return Err(TransitionError::AlreadyInStatus { status: to });
    }
    let allowed = matches!(
        (kind, from, to),
        (
            EntryKind::Decision,
            EntryStatus::Active,
            EntryStatus::Superseded
        ) | (
            EntryKind::Task,
            EntryStatus::Open,
            EntryStatus::Done | EntryStatus::Dropped
        ) | (
            EntryKind::Question,
            EntryStatus::Open,
            EntryStatus::Answered | EntryStatus::Dropped
        )
    );
    if allowed {
        Ok(())
    } else {
        Err(TransitionError::NotAllowed { kind, from, to })
    }
}

/// A requested status change, with the data the target status needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusChange {
    /// `active → superseded`: the decision is replaced by decision `by` (plan section 8,
    /// "via `replaces`"). The store sets `superseded_by`.
    Supersede {
        /// The replacing decision. Must be a different, existing decision.
        by: EntryId,
    },
    /// `open → done` for a task.
    Done,
    /// `open → dropped` for a task or question.
    Drop,
    /// `open → answered` for a question. `/answer` takes a message link (plan section
    /// 7.3); the store sets `answered_by_message_id`.
    Answer {
        /// The stored message that answers the question.
        message: MessageId,
    },
}

impl StatusChange {
    /// The status this change leads to.
    #[must_use]
    pub const fn target(self) -> EntryStatus {
        match self {
            Self::Supersede { .. } => EntryStatus::Superseded,
            Self::Done => EntryStatus::Done,
            Self::Drop => EntryStatus::Dropped,
            Self::Answer { .. } => EntryStatus::Answered,
        }
    }

    /// Checks this change against an entry's kind, current status and id.
    ///
    /// # Errors
    ///
    /// - [`RecordRuleError::Transition`] when [`check_transition`] rejects the step;
    /// - [`RecordRuleError::SelfSupersede`] when a decision would supersede itself.
    ///
    /// Whether `by` exists and is a decision is checked by the store, which can look it
    /// up.
    pub fn check(
        self,
        entry: EntryId,
        kind: EntryKind,
        from: EntryStatus,
    ) -> Result<(), RecordRuleError> {
        check_transition(kind, from, self.target())?;
        if let Self::Supersede { by } = self
            && by == entry
        {
            return Err(RecordRuleError::SelfSupersede);
        }
        Ok(())
    }
}

/// An entry's public id: kind prefix, `-`, row id (plan section 8: `D-12`, `T-7`, `Q-3`).
///
/// Invariants:
/// - the text form is exactly `[DTQ]-[1-9][0-9]*` and the number fits in `i64`;
/// - `Display` and `FromStr` are inverse: `s.parse::<ShortId>()?.to_string() == s`.
///
/// The number is the `record_entries` row id, so ids are shared across kinds (`D-3` and
/// `T-3` cannot both exist). A short id whose prefix does not match the stored entry's
/// kind does not identify that entry; the store treats it as not found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShortId {
    /// The kind, shown as the prefix.
    pub kind: EntryKind,
    /// The row id, shown as the number.
    pub id: EntryId,
}

impl ShortId {
    /// Accepts a person's case-insensitive prefix; storage and display stay canonical.
    ///
    /// # Errors
    /// Returns the same validation errors as [`ShortId::from_str`].
    pub fn parse_user(text: &str) -> Result<Self, ParseShortIdError> {
        text.to_ascii_uppercase().parse()
    }
}

impl fmt::Display for ShortId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}", self.kind.prefix(), self.id)
    }
}

/// Why text is not a short id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ParseShortIdError {
    /// The first character is not `D`, `T` or `Q`.
    #[error("short id must start with D, T or Q")]
    Prefix,
    /// No `-` after the prefix.
    #[error("short id needs a '-' after the letter, like D-12")]
    Separator,
    /// The number is missing, has a sign, a leading zero or other characters, or does
    /// not fit.
    #[error("short id needs a positive number without leading zeros, like D-12")]
    Number,
}

impl FromStr for ShortId {
    type Err = ParseShortIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut chars = s.chars();
        let kind = chars
            .next()
            .and_then(EntryKind::from_prefix)
            .ok_or(ParseShortIdError::Prefix)?;
        let rest = chars.as_str();
        let digits = rest.strip_prefix('-').ok_or(ParseShortIdError::Separator)?;
        let canonical = digits.bytes().all(|b| b.is_ascii_digit())
            && digits.bytes().next().is_some_and(|b| b != b'0');
        if !canonical {
            return Err(ParseShortIdError::Number);
        }
        let raw: i64 = digits.parse().map_err(|_| ParseShortIdError::Number)?;
        let id = EntryId::new(raw).ok_or(ParseShortIdError::Number)?;
        Ok(Self { kind, id })
    }
}

/// The most characters (Unicode scalar values) an entry title may have (plan section 6).
pub const MAX_TITLE_CHARS: usize = 200;

/// The title that replaces a copied title when its source message is gone (plan
/// section 6.1).
pub const SOURCE_DELETED_TITLE: &str = "(source message deleted)";

/// An entry title.
///
/// Invariants: not empty or whitespace-only, at most [`MAX_TITLE_CHARS`] characters.
/// The text is kept as given (no trimming). It is untrusted when copied from a message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntryTitle(String);

impl EntryTitle {
    /// Copies message text into a single-line title, limited to 200 Unicode scalars.
    ///
    /// # Errors
    /// Returns [`RecordRuleError::EmptyTitle`] for blank source text.
    pub fn copy_from_message(text: &str) -> Result<Self, RecordRuleError> {
        let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        Self::new(line.chars().take(MAX_TITLE_CHARS).collect::<String>())
    }

    /// Checks and wraps a title.
    ///
    /// # Errors
    ///
    /// [`RecordRuleError::EmptyTitle`] or [`RecordRuleError::TitleTooLong`].
    pub fn new(text: impl Into<String>) -> Result<Self, RecordRuleError> {
        let text = text.into();
        if text.trim().is_empty() {
            return Err(RecordRuleError::EmptyTitle);
        }
        let chars = text.chars().count();
        if chars > MAX_TITLE_CHARS {
            return Err(RecordRuleError::TitleTooLong { chars });
        }
        Ok(Self(text))
    }

    /// The placeholder title [`SOURCE_DELETED_TITLE`].
    #[must_use]
    pub fn source_deleted() -> Self {
        Self(SOURCE_DELETED_TITLE.to_owned())
    }

    /// The title text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A topic tag. Stored space-separated and lowercase in `record_entries.tags` (plan
/// section 6).
///
/// Invariants: not empty, no whitespace, no uppercase characters ([`Tag::new`]
/// lowercases its input).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Tag(String);

impl Tag {
    /// Lowercases and wraps a tag.
    ///
    /// # Errors
    ///
    /// [`RecordRuleError::InvalidTag`] if the tag is empty or contains whitespace.
    pub fn new(text: &str) -> Result<Self, RecordRuleError> {
        if text.is_empty() || text.chars().any(char::is_whitespace) {
            return Err(RecordRuleError::InvalidTag);
        }
        Ok(Self(text.to_lowercase()))
    }

    /// The tag text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

text_enum! {
    /// How an entry was captured. Text forms match `record_entries.created_via` (plan
    /// section 6).
    pub enum CreatedVia {
        /// Message context menu ("Mark as decision / task / question").
        ContextMenu => "context_menu",
        /// A slash command such as `/decide`.
        Slash => "slash",
        /// The configured capture reaction.
        Reaction => "reaction",
        /// The web view.
        Web => "web",
        /// The CLI.
        Cli => "cli",
    }
}

/// A rule broken by an entry or a status change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum RecordRuleError {
    /// Copied text must retain a source reference so deletion can redact it.
    #[error("a copied title requires a source message")]
    CopiedTitleWithoutSource,
    /// The lifecycle forbids the status change.
    #[error(transparent)]
    Transition(#[from] TransitionError),
    /// A decision cannot supersede itself.
    #[error("a decision cannot replace itself")]
    SelfSupersede,
    /// The title is empty or whitespace-only.
    #[error("title must not be empty")]
    EmptyTitle,
    /// The title is longer than [`MAX_TITLE_CHARS`].
    #[error("title has {chars} characters; the limit is 200")]
    TitleTooLong {
        /// The title's length in characters.
        chars: usize,
    },
    /// A tag is empty or contains whitespace.
    #[error("tags must be single words")]
    InvalidTag,
    /// `due_at` was set on a decision or question (plan section 6: tasks only).
    #[error("only tasks have a due date")]
    DueOnNonTask,
    /// `replaces` was set on a task or question (plan section 7.3: `/decide` only).
    #[error("only a decision can replace a decision")]
    ReplacesOnNonDecision,
}

/// A request to create an entry. The store assigns the id, sets the status to
/// `kind.initial_status()`, sets `updated_at = created_at`, and appends a
/// [`RecordAction::Created`] event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    /// What the entry records.
    pub kind: EntryKind,
    /// The title.
    pub title: EntryTitle,
    /// Whether the title was copied automatically from the source message text. Such
    /// titles are replaced when the message is deleted or purged (plan section 6.1).
    pub title_is_copied: bool,
    /// Who owns it, if anyone.
    pub owner: Option<MemberId>,
    /// Absolute due time; tasks only.
    pub due_at: Option<UnixMillis>,
    /// The cited message, if the entry was captured from one.
    pub source_message: Option<MessageId>,
    /// Who captured it (plan section 8: "Capture records who did it").
    pub created_by: MemberId,
    /// How it was captured.
    pub created_via: CreatedVia,
    /// Topic tags.
    pub tags: Vec<Tag>,
    /// An active decision this new decision supersedes (`/decide replaces:<D-id>`).
    /// The store creates the entry and supersedes the old one in one transaction.
    pub replaces: Option<EntryId>,
    /// Capture time.
    pub created_at: UnixMillis,
}

impl NewEntry {
    /// Checks the field rules that depend on the kind.
    ///
    /// # Errors
    ///
    /// [`RecordRuleError::DueOnNonTask`], [`RecordRuleError::ReplacesOnNonDecision`],
    /// or [`RecordRuleError::CopiedTitleWithoutSource`].
    pub fn validate(&self) -> Result<(), RecordRuleError> {
        if self.title_is_copied && self.source_message.is_none() {
            return Err(RecordRuleError::CopiedTitleWithoutSource);
        }
        if self.due_at.is_some() && self.kind != EntryKind::Task {
            return Err(RecordRuleError::DueOnNonTask);
        }
        if self.replaces.is_some() && self.kind != EntryKind::Decision {
            return Err(RecordRuleError::ReplacesOnNonDecision);
        }
        Ok(())
    }
}

/// A stored entry (one `record_entries` row).
///
/// Invariants the store maintains:
/// - `status` is in `kind.statuses()` and only changes by [`check_transition`] steps;
/// - `superseded_by.is_some()` exactly when `status == Superseded`;
/// - `answered_by_message` is `None` unless `status == Answered` (it becomes `None`
///   again if that message is deleted);
/// - `due_at` is `None` unless `kind == Task`;
/// - when `source_deleted`, `source_message` is `None`, and if `title_is_copied` the
///   title is [`SOURCE_DELETED_TITLE`];
/// - `created_at <= updated_at`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEntry {
    /// Row id; with `kind` it forms the [`ShortId`].
    pub id: EntryId,
    /// What the entry records.
    pub kind: EntryKind,
    /// Current status.
    pub status: EntryStatus,
    /// The title.
    pub title: EntryTitle,
    /// Whether the title was copied from the source message.
    pub title_is_copied: bool,
    /// Owner, if any.
    pub owner: Option<MemberId>,
    /// Due time; tasks only.
    pub due_at: Option<UnixMillis>,
    /// The cited message, while it exists.
    pub source_message: Option<MessageId>,
    /// Whether the cited message was deleted.
    pub source_deleted: bool,
    /// The decision that replaced this one.
    pub superseded_by: Option<EntryId>,
    /// The message that answered this question.
    pub answered_by_message: Option<MessageId>,
    /// Who captured it.
    pub created_by: MemberId,
    /// How it was captured.
    pub created_via: CreatedVia,
    /// Topic tags.
    pub tags: Vec<Tag>,
    /// Capture time.
    pub created_at: UnixMillis,
    /// Time of the last change.
    pub updated_at: UnixMillis,
}

impl RecordEntry {
    /// The entry's public id, for example `D-12`.
    #[must_use]
    pub const fn short_id(&self) -> ShortId {
        ShortId {
            kind: self.kind,
            id: self.id,
        }
    }
}

/// What happened to an entry; one `record_events.action` value (plan section 8: "Every
/// change appends a `record_events` row").
///
/// The text encoding is `(action, detail)`: [`RecordAction::action`] and
/// [`RecordAction::detail`] produce it, [`RecordAction::decode`] reads it back, and the
/// two are inverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordAction {
    /// The entry was captured. Encoded `("created", None)`.
    Created,
    /// The status changed. Encoded `("status", Some("<from> <to>"))`, for example
    /// `("status", Some("open done"))`.
    StatusChanged {
        /// The previous status.
        from: EntryStatus,
        /// The new status.
        to: EntryStatus,
    },
    /// The cited message was deleted and the deletion rule applied. Encoded
    /// `("source_deleted", None)`.
    SourceDeleted,
}

/// Text that is not a [`RecordAction`] encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("unknown record event encoding")]
pub struct ParseRecordActionError;

impl RecordAction {
    /// The `record_events.action` text.
    #[must_use]
    pub const fn action(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::StatusChanged { .. } => "status",
            Self::SourceDeleted => "source_deleted",
        }
    }

    /// The `record_events.detail` text.
    #[must_use]
    pub fn detail(self) -> Option<String> {
        match self {
            Self::StatusChanged { from, to } => Some(format!("{from} {to}")),
            Self::Created | Self::SourceDeleted => None,
        }
    }

    /// Reads an `(action, detail)` pair written by [`action`](Self::action) and
    /// [`detail`](Self::detail).
    ///
    /// # Errors
    ///
    /// [`ParseRecordActionError`] for any other pair.
    pub fn decode(action: &str, detail: Option<&str>) -> Result<Self, ParseRecordActionError> {
        match (action, detail) {
            ("created", None) => Ok(Self::Created),
            ("source_deleted", None) => Ok(Self::SourceDeleted),
            ("status", Some(detail)) => {
                let (from, to) = detail.split_once(' ').ok_or(ParseRecordActionError)?;
                Ok(Self::StatusChanged {
                    from: from.parse().map_err(|_| ParseRecordActionError)?,
                    to: to.parse().map_err(|_| ParseRecordActionError)?,
                })
            }
            _ => Err(ParseRecordActionError),
        }
    }
}

/// One audit-log row (`record_events`), append-only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordEvent {
    /// The entry it belongs to.
    pub entry: EntryId,
    /// Who did it; `None` for changes made by the system (for example a source
    /// deletion).
    pub actor: Option<MemberId>,
    /// What happened.
    pub action: RecordAction,
    /// When.
    pub at: UnixMillis,
}

#[cfg(test)]
mod tests {
    use super::*;

    use EntryKind::{Decision, Question, Task};
    use EntryStatus::{Active, Answered, Done, Dropped, Open, Superseded};

    fn entry_id(raw: i64) -> EntryId {
        EntryId::new(raw).unwrap()
    }

    /// The complete set of allowed steps from plan section 8. Everything else must fail.
    const ALLOWED: &[(EntryKind, EntryStatus, EntryStatus)] = &[
        (Decision, Active, Superseded),
        (Task, Open, Done),
        (Task, Open, Dropped),
        (Question, Open, Answered),
        (Question, Open, Dropped),
    ];

    #[test]
    fn lifecycle_table_is_exact() {
        for &kind in EntryKind::ALL {
            for &from in EntryStatus::ALL {
                for &to in EntryStatus::ALL {
                    let result = check_transition(kind, from, to);
                    if ALLOWED.contains(&(kind, from, to)) {
                        assert_eq!(result, Ok(()), "{kind}: {from} -> {to}");
                        continue;
                    }
                    let expected = if !kind.statuses().contains(&from) {
                        TransitionError::StatusNotForKind { kind, status: from }
                    } else if !kind.statuses().contains(&to) {
                        TransitionError::StatusNotForKind { kind, status: to }
                    } else if from == to {
                        TransitionError::AlreadyInStatus { status: to }
                    } else {
                        TransitionError::NotAllowed { kind, from, to }
                    };
                    assert_eq!(result, Err(expected), "{kind}: {from} -> {to}");
                }
            }
        }
    }

    #[test]
    fn terminal_statuses_cannot_reopen() {
        assert_eq!(
            check_transition(Task, Done, Open),
            Err(TransitionError::NotAllowed {
                kind: Task,
                from: Done,
                to: Open
            })
        );
        assert!(check_transition(Decision, Superseded, Active).is_err());
        assert!(check_transition(Question, Answered, Dropped).is_err());
    }

    #[test]
    fn initial_status_belongs_to_kind() {
        assert_eq!(Decision.initial_status(), Active);
        assert_eq!(Task.initial_status(), Open);
        assert_eq!(Question.initial_status(), Open);
        for &kind in EntryKind::ALL {
            assert!(kind.statuses().contains(&kind.initial_status()));
        }
    }

    #[test]
    fn every_status_belongs_to_some_kind() {
        for status in EntryStatus::ALL {
            assert!(EntryKind::ALL.iter().any(|k| k.statuses().contains(status)));
        }
    }

    #[test]
    fn status_change_targets_and_checks() {
        let id = entry_id(5);
        let change = StatusChange::Supersede { by: entry_id(6) };
        assert_eq!(change.target(), Superseded);
        assert_eq!(change.check(id, Decision, Active), Ok(()));
        assert_eq!(
            StatusChange::Supersede { by: id }.check(id, Decision, Active),
            Err(RecordRuleError::SelfSupersede)
        );
        assert_eq!(
            StatusChange::Done.check(id, Question, Open),
            Err(RecordRuleError::Transition(
                TransitionError::StatusNotForKind {
                    kind: Question,
                    status: Done
                }
            ))
        );
        let answer = StatusChange::Answer {
            message: MessageId::new(9).unwrap(),
        };
        assert_eq!(answer.check(id, Question, Open), Ok(()));
        assert_eq!(StatusChange::Drop.check(id, Task, Open), Ok(()));
        assert_eq!(
            StatusChange::Drop.check(id, Task, Dropped),
            Err(RecordRuleError::Transition(
                TransitionError::AlreadyInStatus { status: Dropped }
            ))
        );
    }

    #[test]
    fn short_id_round_trips() {
        for text in ["D-1", "T-7", "Q-3", "D-12", "T-9223372036854775807"] {
            let id: ShortId = text.parse().unwrap();
            assert_eq!(id.to_string(), text);
        }
        let id: ShortId = "Q-42".parse().unwrap();
        assert_eq!(id.kind, Question);
        assert_eq!(id.id.get(), 42);
        for &kind in EntryKind::ALL {
            let id = ShortId {
                kind,
                id: entry_id(30),
            };
            assert_eq!(id.to_string().parse::<ShortId>(), Ok(id));
        }
    }

    #[test]
    fn short_id_rejects_other_text() {
        let cases = [
            ("", ParseShortIdError::Prefix),
            ("d-12", ParseShortIdError::Prefix),
            ("X-12", ParseShortIdError::Prefix),
            (" D-12", ParseShortIdError::Prefix),
            ("D12", ParseShortIdError::Separator),
            ("D", ParseShortIdError::Separator),
            ("D_12", ParseShortIdError::Separator),
            ("D-", ParseShortIdError::Number),
            ("D-0", ParseShortIdError::Number),
            ("D-012", ParseShortIdError::Number),
            ("D--1", ParseShortIdError::Number),
            ("D-+1", ParseShortIdError::Number),
            ("D-1 ", ParseShortIdError::Number),
            ("D-1a", ParseShortIdError::Number),
            ("D-9223372036854775808", ParseShortIdError::Number), // i64::MAX + 1
        ];
        for (input, expected) in cases {
            assert_eq!(input.parse::<ShortId>(), Err(expected), "input {input:?}");
        }
    }

    #[test]
    fn entry_and_status_text_matches_schema() {
        let kinds: Vec<_> = EntryKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(kinds, ["decision", "task", "question"]);
        let statuses: Vec<_> = EntryStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(
            statuses,
            [
                "active",
                "superseded",
                "open",
                "done",
                "answered",
                "dropped"
            ]
        );
        let via: Vec<_> = CreatedVia::ALL.iter().map(|v| v.as_str()).collect();
        assert_eq!(via, ["context_menu", "slash", "reaction", "web", "cli"]);
        for status in EntryStatus::ALL {
            assert_eq!(status.as_str().parse::<EntryStatus>().unwrap(), *status);
        }
        for via in CreatedVia::ALL {
            assert_eq!(via.as_str().parse::<CreatedVia>().unwrap(), *via);
        }
        assert!("Done".parse::<EntryStatus>().is_err());
    }

    #[test]
    fn title_limits() {
        assert_eq!(EntryTitle::new(""), Err(RecordRuleError::EmptyTitle));
        assert_eq!(EntryTitle::new(" \n\t"), Err(RecordRuleError::EmptyTitle));
        let max = "é".repeat(MAX_TITLE_CHARS);
        assert_eq!(EntryTitle::new(max.clone()).unwrap().as_str(), max);
        assert_eq!(
            EntryTitle::new(format!("{max}x")),
            Err(RecordRuleError::TitleTooLong { chars: 201 })
        );
        assert_eq!(
            EntryTitle::new(" Use SQLite ").unwrap().as_str(),
            " Use SQLite "
        );
        assert_eq!(EntryTitle::source_deleted().as_str(), SOURCE_DELETED_TITLE);
    }

    #[test]
    fn tags_are_single_lowercase_words() {
        assert_eq!(Tag::new("Storage").unwrap().as_str(), "storage");
        assert_eq!(Tag::new("ÉPUB").unwrap().as_str(), "épub");
        assert_eq!(Tag::new(""), Err(RecordRuleError::InvalidTag));
        assert_eq!(Tag::new("two words"), Err(RecordRuleError::InvalidTag));
        assert_eq!(Tag::new("tab\there"), Err(RecordRuleError::InvalidTag));
    }

    fn new_entry(kind: EntryKind) -> NewEntry {
        NewEntry {
            kind,
            title: EntryTitle::new("Use SQLite for storage").unwrap(),
            title_is_copied: false,
            owner: None,
            due_at: None,
            source_message: None,
            created_by: MemberId::new(1).unwrap(),
            created_via: CreatedVia::Slash,
            tags: Vec::new(),
            replaces: None,
            created_at: UnixMillis::from_millis(0),
        }
    }

    #[test]
    fn new_entry_field_rules() {
        assert_eq!(
            NewEntry {
                title_is_copied: true,
                ..new_entry(Decision)
            }
            .validate(),
            Err(RecordRuleError::CopiedTitleWithoutSource)
        );
        for &kind in EntryKind::ALL {
            assert_eq!(new_entry(kind).validate(), Ok(()));
        }
        let due = Some(UnixMillis::from_millis(10));
        assert_eq!(
            NewEntry {
                due_at: due,
                ..new_entry(Task)
            }
            .validate(),
            Ok(())
        );
        for kind in [Decision, Question] {
            assert_eq!(
                NewEntry {
                    due_at: due,
                    ..new_entry(kind)
                }
                .validate(),
                Err(RecordRuleError::DueOnNonTask)
            );
        }
        let replaces = Some(entry_id(3));
        assert_eq!(
            NewEntry {
                replaces,
                ..new_entry(Decision)
            }
            .validate(),
            Ok(())
        );
        for kind in [Task, Question] {
            assert_eq!(
                NewEntry {
                    replaces,
                    ..new_entry(kind)
                }
                .validate(),
                Err(RecordRuleError::ReplacesOnNonDecision)
            );
        }
    }

    #[test]
    fn human_ids_and_copied_titles_follow_maintainer_defaults() {
        assert_eq!(ShortId::parse_user("d-12").unwrap().to_string(), "D-12");
        assert!(ShortId::parse_user("d-012").is_err());
        assert_eq!(
            EntryTitle::copy_from_message(" first\n\tsecond ")
                .unwrap()
                .as_str(),
            "first second"
        );
        let long = "é".repeat(201);
        assert_eq!(
            EntryTitle::copy_from_message(&long)
                .unwrap()
                .as_str()
                .chars()
                .count(),
            200
        );
        assert_eq!(
            EntryTitle::copy_from_message("\n\t"),
            Err(RecordRuleError::EmptyTitle)
        );
    }

    #[test]
    fn record_action_encoding_round_trips() {
        let mut actions = vec![RecordAction::Created, RecordAction::SourceDeleted];
        for &kind in EntryKind::ALL {
            for &from in kind.statuses() {
                for &to in kind.statuses() {
                    if check_transition(kind, from, to).is_ok() {
                        actions.push(RecordAction::StatusChanged { from, to });
                    }
                }
            }
        }
        for action in actions {
            let detail = action.detail();
            assert_eq!(
                RecordAction::decode(action.action(), detail.as_deref()),
                Ok(action)
            );
        }
        assert_eq!(
            RecordAction::StatusChanged {
                from: Open,
                to: Done
            }
            .detail()
            .as_deref(),
            Some("open done")
        );
    }

    #[test]
    fn record_action_rejects_unknown_encodings() {
        let cases: [(&str, Option<&str>); 6] = [
            ("created", Some("x")),
            ("status", None),
            ("status", Some("open")),
            ("status", Some("open finished")),
            ("edited", None),
            ("", None),
        ];
        for (action, detail) in cases {
            assert_eq!(
                RecordAction::decode(action, detail),
                Err(ParseRecordActionError),
                "{action:?} {detail:?}"
            );
        }
    }
}
