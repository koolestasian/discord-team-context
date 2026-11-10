//! The human-captured write path: capture from a message, owner and due changes, status
//! changes that tolerate a retry, request keys, audit events and the schema that backs
//! them. All data is synthetic.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use teamctx_core::{id::*, record::*, repo::*, source::*, time::UnixMillis};
use teamctx_store::{Store, Writer};

fn sf(id: u64) -> Snowflake {
    Snowflake::new(id).unwrap()
}

fn at(time: i64) -> UnixMillis {
    UnixMillis::from_millis(time)
}

fn person(id: u64, name: &str) -> Member {
    Member {
        id: sf(id),
        display_name: name.to_owned(),
        is_bot: false,
    }
}

fn room(id: u64, included: bool) -> Channel {
    Channel {
        id: sf(id),
        parent: None,
        kind: ChannelKind::Text,
        name: format!("room-{id}"),
        topic: None,
        included,
        archived: false,
    }
}

fn message(id: u64, channel: u64, text: &str) -> Message {
    Message {
        id: MessageRef {
            channel: sf(channel),
            message: sf(id),
        },
        author: Some(person(100, "Piper Fiction")),
        content: text.to_owned(),
        reply_to: None,
        attachment_names: vec![],
        created_at: at(i64::try_from(id).unwrap()),
        edited_at: None,
        mentions: vec![],
    }
}

struct World {
    db: Store,
    source: SourceId,
    piper: MemberId,
    quinn: MemberId,
}

impl World {
    fn new() -> Self {
        Self::on(Store::in_memory().unwrap())
    }

    fn on(mut db: Store) -> Self {
        let source = db
            .upsert_source(SourceKind::Discord, sf(1), "Fiction Team")
            .unwrap();
        for event in [
            SourceEvent::ChannelUpsert(room(10, true)),
            SourceEvent::MemberUpsert(person(100, "Piper Fiction")),
            SourceEvent::MemberUpsert(person(101, "Quinn Fiction")),
            SourceEvent::MessageUpsert(message(20, 10, "We will use SQLite for storage")),
            SourceEvent::MessageUpsert(message(21, 10, "Who owns the beta checklist?")),
            SourceEvent::MessageUpsert(message(22, 10, "Quinn: use the shared sheet")),
            SourceEvent::MessageUpsert(message(23, 10, "   \n  ")),
        ] {
            db.apply_event(source, &event, at(1)).unwrap();
        }
        let piper = db.member_by_external(source, sf(100)).unwrap().unwrap().id;
        let quinn = db.member_by_external(source, sf(101)).unwrap().unwrap().id;
        Self {
            db,
            source,
            piper,
            quinn,
        }
    }

    fn row(&self, message: u64, channel: u64) -> MessageId {
        self.db
            .message_by_ref(
                self.source,
                MessageRef {
                    channel: sf(channel),
                    message: sf(message),
                },
            )
            .unwrap()
            .unwrap()
            .id
    }

    fn events(&self, entry: &RecordEntry) -> Vec<RecordEvent> {
        self.db.entry_events(entry.id).unwrap()
    }

    fn entry_count(&self) -> usize {
        self.db
            .list_entries(
                &EntryFilter::default(),
                PageRequest {
                    offset: 0,
                    limit: 100,
                },
            )
            .unwrap()
            .len()
    }
}

fn new_entry(kind: EntryKind, title: &str, by: MemberId) -> NewEntry {
    NewEntry {
        kind,
        title: EntryTitle::new(title).unwrap(),
        title_is_copied: false,
        owner: None,
        due_at: None,
        source_message: None,
        created_by: by,
        created_via: CreatedVia::Slash,
        tags: vec![],
        replaces: None,
        created_at: at(100),
    }
}

fn key(text: &str) -> RequestKey {
    RequestKey::new(text).unwrap()
}

fn rule(error: RepoError) -> RecordRuleError {
    match error {
        RepoError::Rule(rule) => rule,
        other => panic!("expected a rule error, got {other:?}"),
    }
}

#[test]
fn capture_copies_the_message_and_records_who_did_it() {
    let mut w = World::new();
    let message = w.row(20, 10);
    let out =
        w.db.capture_message(
            EntryKind::Decision,
            message,
            w.quinn,
            CreatedVia::ContextMenu,
            at(500),
        )
        .unwrap();
    assert!(out.changed);
    let entry = out.entry;
    assert_eq!(entry.title.as_str(), "We will use SQLite for storage");
    assert!(entry.title_is_copied);
    assert_eq!(entry.source_message, Some(message));
    assert_eq!(entry.created_by, w.quinn);
    assert_eq!(entry.created_via, CreatedVia::ContextMenu);
    assert_eq!(entry.status, EntryStatus::Active);
    assert_eq!((entry.created_at, entry.updated_at), (at(500), at(500)));
    assert_eq!(
        w.events(&entry),
        [RecordEvent {
            entry: entry.id,
            actor: Some(w.quinn),
            action: RecordAction::Created,
            at: at(500),
        }]
    );
}

#[test]
fn capturing_twice_returns_the_same_entry_and_writes_nothing() {
    let mut w = World::new();
    let message = w.row(21, 10);
    let first =
        w.db.capture_message(
            EntryKind::Question,
            message,
            w.piper,
            CreatedVia::Slash,
            at(10),
        )
        .unwrap();
    let again =
        w.db.capture_message(
            EntryKind::Question,
            message,
            w.quinn,
            CreatedVia::ContextMenu,
            at(99),
        )
        .unwrap();
    assert!(first.changed);
    assert!(!again.changed);
    assert_eq!(again.entry, first.entry);
    assert_eq!(w.events(&first.entry).len(), 1);
    assert_eq!(w.entry_count(), 1);
    // Another kind on the same message is a different entry.
    let task =
        w.db.capture_message(EntryKind::Task, message, w.piper, CreatedVia::Slash, at(11))
            .unwrap();
    assert!(task.changed);
    assert_ne!(task.entry.id, first.entry.id);
}

#[test]
fn capture_rejects_unknown_empty_and_unattributed_requests() {
    let mut w = World::new();
    let unknown = MessageId::new(9_999).unwrap();
    let error =
        w.db.capture_message(EntryKind::Task, unknown, w.piper, CreatedVia::Slash, at(5))
            .unwrap_err();
    assert!(matches!(error, RepoError::NotFound { what: "message" }));
    // Empty text: the caller must supply a title through create_entry instead.
    let blank = w.row(23, 10);
    let error =
        w.db.capture_message(EntryKind::Task, blank, w.piper, CreatedVia::Slash, at(5))
            .unwrap_err();
    assert_eq!(rule(error), RecordRuleError::EmptyTitle);
    let ok = w.row(20, 10);
    let error =
        w.db.capture_message(
            EntryKind::Task,
            ok,
            MemberId::new(999).unwrap(),
            CreatedVia::Slash,
            at(5),
        )
        .unwrap_err();
    assert!(matches!(error, RepoError::NotFound { what: "actor" }));
    assert_eq!(w.entry_count(), 0, "no failed attempt created anything");
}

#[test]
fn a_message_in_an_excluded_channel_cannot_be_captured() {
    let mut w = World::new();
    w.db.apply_event(w.source, &SourceEvent::ChannelUpsert(room(11, true)), at(2))
        .unwrap();
    w.db.apply_event(
        w.source,
        &SourceEvent::MessageUpsert(message(30, 11, "private plans")),
        at(3),
    )
    .unwrap();
    let row = w.row(30, 11);
    w.db.apply_event(
        w.source,
        &SourceEvent::ChannelUpsert(room(11, false)),
        at(4),
    )
    .unwrap();
    // Excluding the channel purged its messages; the old row id no longer resolves.
    let error =
        w.db.capture_message(EntryKind::Decision, row, w.piper, CreatedVia::Slash, at(5))
            .unwrap_err();
    assert!(matches!(error, RepoError::NotFound { what: "message" }));
}

#[test]
fn deleting_the_source_of_a_captured_entry_applies_the_deletion_rule() {
    let mut w = World::new();
    let message = w.row(20, 10);
    let entry =
        w.db.capture_message(
            EntryKind::Decision,
            message,
            w.piper,
            CreatedVia::Slash,
            at(5),
        )
        .unwrap()
        .entry;
    w.db.apply_event(
        w.source,
        &SourceEvent::MessageDelete {
            message: MessageRef {
                channel: sf(10),
                message: sf(20),
            },
        },
        at(9),
    )
    .unwrap();
    let after = w.db.entry(entry.short_id()).unwrap().unwrap();
    assert!(after.source_deleted);
    assert_eq!(after.title.as_str(), SOURCE_DELETED_TITLE);
    assert_eq!(
        w.events(&after).last().unwrap().action,
        RecordAction::SourceDeleted
    );
}

#[test]
fn short_ids_are_numbered_by_the_shared_row_id_in_creation_order() {
    let mut w = World::new();
    let mut ids = Vec::new();
    for (kind, title) in [
        (EntryKind::Decision, "first"),
        (EntryKind::Task, "second"),
        (EntryKind::Question, "third"),
        (EntryKind::Decision, "fourth"),
    ] {
        let entry = w.db.create_entry(&new_entry(kind, title, w.piper)).unwrap();
        ids.push(entry.short_id().to_string());
    }
    assert_eq!(ids, ["D-1", "T-2", "Q-3", "D-4"]);
}

#[test]
fn a_request_key_makes_creation_safe_to_retry() {
    let mut w = World::new();
    let request = new_entry(EntryKind::Decision, "Use SQLite", w.piper);
    let first = w.db.create_entry_once(&key("slash:1"), &request).unwrap();
    let retry = w.db.create_entry_once(&key("slash:1"), &request).unwrap();
    assert!(first.changed);
    assert!(!retry.changed);
    assert_eq!(retry.entry, first.entry);
    assert_eq!(w.events(&first.entry).len(), 1);
    // A different key is a different request.
    let other = w.db.create_entry_once(&key("slash:2"), &request).unwrap();
    assert!(other.changed);
    assert_ne!(other.entry.id, first.entry.id);
    // The key belongs to the first kind.
    let task = new_entry(EntryKind::Task, "Use SQLite", w.piper);
    let error = w.db.create_entry_once(&key("slash:1"), &task).unwrap_err();
    assert!(matches!(
        error,
        RepoError::Conflict {
            what: "request key"
        }
    ));
}

#[test]
fn a_retried_decide_with_replaces_supersedes_once() {
    let mut w = World::new();
    let old =
        w.db.create_entry(&new_entry(EntryKind::Decision, "Use JSON files", w.piper))
            .unwrap();
    let mut request = new_entry(EntryKind::Decision, "Use SQLite", w.quinn);
    request.replaces = Some(old.id);
    request.created_at = at(200);
    let first = w.db.create_entry_once(&key("slash:7"), &request).unwrap();
    let retry = w.db.create_entry_once(&key("slash:7"), &request).unwrap();
    assert!(first.changed && !retry.changed);
    let old_now = w.db.entry(old.short_id()).unwrap().unwrap();
    assert_eq!(old_now.status, EntryStatus::Superseded);
    assert_eq!(old_now.superseded_by, Some(first.entry.id));
    let events = w.events(&old_now);
    assert_eq!(events.len(), 2, "created, then superseded once");
    assert_eq!(
        events[1],
        RecordEvent {
            entry: old.id,
            actor: Some(w.quinn),
            action: RecordAction::StatusChanged {
                from: EntryStatus::Active,
                to: EntryStatus::Superseded
            },
            at: at(200),
        }
    );
    // Without the key a second decide fails: the old decision is not active any more.
    let error = w.db.create_entry(&request).unwrap_err();
    assert!(matches!(rule(error), RecordRuleError::Transition(_)));
}

#[test]
fn creating_with_a_replaced_decision_is_all_or_nothing() {
    let mut w = World::new();
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Not a decision", w.piper))
            .unwrap();
    let mut request = new_entry(EntryKind::Decision, "Replaces a task", w.piper);
    request.replaces = Some(task.id);
    assert!(w.db.create_entry_once(&key("slash:9"), &request).is_err());
    assert_eq!(w.entry_count(), 1, "the failed attempt left no entry");
    // And no key: the same key still works once the request is valid.
    request.replaces = None;
    let ok = w.db.create_entry_once(&key("slash:9"), &request).unwrap();
    assert!(ok.changed);
}

#[test]
fn status_changes_are_idempotent_and_keep_the_first_event() {
    let mut w = World::new();
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Write intro", w.piper))
            .unwrap();
    let done =
        w.db.change_status_once(task.short_id(), StatusChange::Done, w.quinn, at(300))
            .unwrap();
    assert!(done.changed);
    assert_eq!(done.entry.status, EntryStatus::Done);
    let retry =
        w.db.change_status_once(task.short_id(), StatusChange::Done, w.quinn, at(400))
            .unwrap();
    assert!(!retry.changed);
    assert_eq!(retry.entry, done.entry);
    assert_eq!(retry.entry.updated_at, at(300));
    assert_eq!(w.events(&task).len(), 2);
    // A different final status is still refused: there is no way back.
    let error =
        w.db.change_status_once(task.short_id(), StatusChange::Drop, w.quinn, at(500))
            .unwrap_err();
    assert!(matches!(rule(error), RecordRuleError::Transition(_)));
    // A repeat with an unknown actor is still an error.
    let error =
        w.db.change_status_once(
            task.short_id(),
            StatusChange::Done,
            MemberId::new(999).unwrap(),
            at(600),
        )
        .unwrap_err();
    assert!(matches!(error, RepoError::NotFound { what: "actor" }));
    // The wrong prefix is not this entry.
    let wrong = ShortId {
        kind: EntryKind::Decision,
        id: task.id,
    };
    let error =
        w.db.change_status_once(wrong, StatusChange::Done, w.quinn, at(700))
            .unwrap_err();
    assert!(matches!(error, RepoError::NotFound { what: "entry" }));
}

#[test]
fn answering_links_a_message_and_a_repeat_must_name_the_same_one() {
    let mut w = World::new();
    let question =
        w.db.create_entry(&new_entry(EntryKind::Question, "Which sheet?", w.piper))
            .unwrap();
    let answer = w.row(22, 10);
    let other = w.row(20, 10);
    let first =
        w.db.change_status_once(
            question.short_id(),
            StatusChange::Answer { message: answer },
            w.quinn,
            at(300),
        )
        .unwrap();
    assert!(first.changed);
    assert_eq!(first.entry.status, EntryStatus::Answered);
    assert_eq!(first.entry.answered_by_message, Some(answer));
    let retry =
        w.db.change_status_once(
            question.short_id(),
            StatusChange::Answer { message: answer },
            w.quinn,
            at(301),
        )
        .unwrap();
    assert!(!retry.changed);
    let error =
        w.db.change_status_once(
            question.short_id(),
            StatusChange::Answer { message: other },
            w.quinn,
            at(302),
        )
        .unwrap_err();
    assert!(matches!(
        rule(error),
        RecordRuleError::Transition(TransitionError::AlreadyInStatus { .. })
    ));
    assert_eq!(w.events(&question).len(), 2);
    // A message that does not exist cannot answer.
    let open =
        w.db.create_entry(&new_entry(EntryKind::Question, "Another?", w.piper))
            .unwrap();
    let error =
        w.db.change_status_once(
            open.short_id(),
            StatusChange::Answer {
                message: MessageId::new(9_999).unwrap(),
            },
            w.quinn,
            at(303),
        )
        .unwrap_err();
    assert!(matches!(error, RepoError::NotFound { what: "message" }));
}

#[test]
fn superseding_twice_with_the_same_decision_is_a_no_op() {
    let mut w = World::new();
    let old =
        w.db.create_entry(&new_entry(EntryKind::Decision, "Old way", w.piper))
            .unwrap();
    let new =
        w.db.create_entry(&new_entry(EntryKind::Decision, "New way", w.piper))
            .unwrap();
    let change = StatusChange::Supersede { by: new.id };
    assert!(
        w.db.change_status_once(old.short_id(), change, w.piper, at(300))
            .unwrap()
            .changed
    );
    assert!(
        !w.db
            .change_status_once(old.short_id(), change, w.piper, at(301))
            .unwrap()
            .changed
    );
    let third =
        w.db.create_entry(&new_entry(EntryKind::Decision, "Newer way", w.piper))
            .unwrap();
    let error =
        w.db.change_status_once(
            old.short_id(),
            StatusChange::Supersede { by: third.id },
            w.piper,
            at(302),
        )
        .unwrap_err();
    assert!(matches!(rule(error), RecordRuleError::Transition(_)));
    let error =
        w.db.change_status_once(
            third.short_id(),
            StatusChange::Supersede { by: third.id },
            w.piper,
            at(303),
        )
        .unwrap_err();
    assert_eq!(rule(error), RecordRuleError::SelfSupersede);
}

#[test]
fn owner_changes_are_audited_with_who_and_when() {
    let mut w = World::new();
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Write intro", w.piper))
            .unwrap();
    let set =
        w.db.set_owner(task.short_id(), Some(w.quinn), w.piper, at(300))
            .unwrap();
    assert!(set.changed);
    assert_eq!(set.entry.owner, Some(w.quinn));
    assert_eq!(set.entry.updated_at, at(300));
    let same =
        w.db.set_owner(task.short_id(), Some(w.quinn), w.piper, at(400))
            .unwrap();
    assert!(!same.changed);
    assert_eq!(same.entry.updated_at, at(300));
    let cleared =
        w.db.set_owner(task.short_id(), None, w.quinn, at(500))
            .unwrap();
    assert_eq!(cleared.entry.owner, None);
    let events = w.events(&task);
    assert_eq!(events.len(), 3);
    assert_eq!(
        events[1],
        RecordEvent {
            entry: task.id,
            actor: Some(w.piper),
            action: RecordAction::OwnerChanged {
                from: None,
                to: Some(w.quinn)
            },
            at: at(300),
        }
    );
    assert_eq!(
        events[2],
        RecordEvent {
            entry: task.id,
            actor: Some(w.quinn),
            action: RecordAction::OwnerChanged {
                from: Some(w.quinn),
                to: None
            },
            at: at(500),
        }
    );
}

#[test]
fn owner_rules_unknown_members_closed_entries_and_wrong_kind() {
    let mut w = World::new();
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Write intro", w.piper))
            .unwrap();
    let ghost = MemberId::new(999).unwrap();
    assert!(matches!(
        w.db.set_owner(task.short_id(), Some(ghost), w.piper, at(300))
            .unwrap_err(),
        RepoError::NotFound { what: "owner" }
    ));
    assert!(matches!(
        w.db.set_owner(task.short_id(), None, ghost, at(300))
            .unwrap_err(),
        RepoError::NotFound { what: "actor" }
    ));
    // Owners may be set on any live kind.
    let decision =
        w.db.create_entry(&new_entry(EntryKind::Decision, "Use SQLite", w.piper))
            .unwrap();
    assert!(
        w.db.set_owner(decision.short_id(), Some(w.quinn), w.piper, at(300))
            .unwrap()
            .changed
    );
    // A finished task is closed for changes, even a no-op one.
    w.db.change_status_once(task.short_id(), StatusChange::Done, w.piper, at(310))
        .unwrap();
    let error =
        w.db.set_owner(task.short_id(), Some(w.quinn), w.piper, at(320))
            .unwrap_err();
    assert_eq!(
        rule(error),
        RecordRuleError::NotEditable {
            status: EntryStatus::Done
        }
    );
    let wrong = ShortId {
        kind: EntryKind::Question,
        id: task.id,
    };
    assert!(matches!(
        w.db.set_owner(wrong, None, w.piper, at(330)).unwrap_err(),
        RepoError::NotFound { what: "entry" }
    ));
}

#[test]
fn due_dates_are_for_open_tasks_and_audited() {
    let mut w = World::new();
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Write intro", w.piper))
            .unwrap();
    let friday = at(1_794_000_000_000);
    let set =
        w.db.set_due(task.short_id(), Some(friday), w.quinn, at(300))
            .unwrap();
    assert!(set.changed);
    assert_eq!(set.entry.due_at, Some(friday));
    assert!(
        !w.db
            .set_due(task.short_id(), Some(friday), w.quinn, at(310))
            .unwrap()
            .changed
    );
    let moved = at(1_794_100_000_000);
    w.db.set_due(task.short_id(), Some(moved), w.piper, at(320))
        .unwrap();
    w.db.set_due(task.short_id(), None, w.piper, at(330))
        .unwrap();
    let actions: Vec<_> = w
        .events(&task)
        .into_iter()
        .map(|e| (e.actor, e.action))
        .collect();
    assert_eq!(
        actions,
        [
            (Some(w.piper), RecordAction::Created),
            (
                Some(w.quinn),
                RecordAction::DueChanged {
                    from: None,
                    to: Some(friday)
                }
            ),
            (
                Some(w.piper),
                RecordAction::DueChanged {
                    from: Some(friday),
                    to: Some(moved)
                }
            ),
            (
                Some(w.piper),
                RecordAction::DueChanged {
                    from: Some(moved),
                    to: None
                }
            ),
        ]
    );
    // Decisions and questions have no due date.
    for kind in [EntryKind::Decision, EntryKind::Question] {
        let entry =
            w.db.create_entry(&new_entry(kind, "No due here", w.piper))
                .unwrap();
        let error =
            w.db.set_due(entry.short_id(), Some(friday), w.piper, at(400))
                .unwrap_err();
        assert_eq!(rule(error), RecordRuleError::DueOnNonTask);
    }
    // Dropped tasks are closed.
    w.db.change_status_once(task.short_id(), StatusChange::Drop, w.piper, at(500))
        .unwrap();
    let error =
        w.db.set_due(task.short_id(), Some(friday), w.piper, at(510))
            .unwrap_err();
    assert_eq!(
        rule(error),
        RecordRuleError::NotEditable {
            status: EntryStatus::Dropped
        }
    );
}

#[test]
fn the_audit_log_is_append_only() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut w = World::on(Store::open(file.path()).unwrap());
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Write intro", w.piper))
            .unwrap();
    w.db.set_owner(task.short_id(), Some(w.quinn), w.piper, at(300))
        .unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    for sql in [
        "UPDATE record_events SET action = 'created'",
        "UPDATE record_events SET at = 0",
        "DELETE FROM record_events",
        "DELETE FROM record_events WHERE id = 1",
    ] {
        let error = conn.execute(sql, []).unwrap_err().to_string();
        assert!(error.contains("append-only"), "{sql}: {error}");
    }
    assert_eq!(w.events(&task).len(), 2);
}

#[test]
fn a_failed_audit_write_rolls_back_the_edit() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let mut w = World::on(Store::open(file.path()).unwrap());
    let task =
        w.db.create_entry(&new_entry(EntryKind::Task, "Write intro", w.piper))
            .unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch(
        "CREATE TRIGGER reject_edits BEFORE INSERT ON record_events
         WHEN new.action IN ('owner', 'due')
         BEGIN SELECT RAISE(ABORT, 'synthetic failure'); END;",
    )
    .unwrap();
    assert!(
        w.db.set_owner(task.short_id(), Some(w.quinn), w.piper, at(300))
            .is_err()
    );
    assert!(
        w.db.set_due(task.short_id(), Some(at(5)), w.piper, at(300))
            .is_err()
    );
    let after = w.db.entry(task.short_id()).unwrap().unwrap();
    assert_eq!(after, task);
    assert_eq!(w.events(&task).len(), 1);
}

#[test]
fn a_database_from_schema_version_one_upgrades_in_place() {
    let file = tempfile::NamedTempFile::new().unwrap();
    {
        let conn = rusqlite::Connection::open(file.path()).unwrap();
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(
            "INSERT INTO sources (kind, external_id, name) VALUES ('discord', '1', 'Kept');",
        )
        .unwrap();
    }
    let mut db = Store::open(file.path()).unwrap();
    assert_eq!(
        db.source(SourceId::new(1).unwrap()).unwrap().unwrap().name,
        "Kept"
    );
    let source = db
        .upsert_source(SourceKind::Discord, sf(1), "Kept")
        .unwrap();
    db.apply_event(
        source,
        &SourceEvent::MemberUpsert(person(100, "Piper")),
        at(1),
    )
    .unwrap();
    let piper = db.member_by_external(source, sf(100)).unwrap().unwrap().id;
    let entry = db
        .create_entry_once(
            &key("slash:1"),
            &new_entry(EntryKind::Task, "Upgraded", piper),
        )
        .unwrap();
    assert!(entry.changed);
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
}

#[test]
fn the_writer_thread_serves_the_edit_commands() {
    let w = World::new();
    let (piper, quinn) = (w.piper, w.quinn);
    let message = w.row(20, 10);
    let writer = Writer::spawn(w.db).unwrap();
    let mut client = writer.client();
    let captured = client
        .capture_message(
            EntryKind::Task,
            message,
            piper,
            CreatedVia::ContextMenu,
            at(10),
        )
        .unwrap();
    assert!(captured.changed);
    let id = captured.entry.short_id();
    assert!(
        client
            .set_owner(id, Some(quinn), piper, at(20))
            .unwrap()
            .changed
    );
    assert!(
        client
            .set_due(id, Some(at(1_000)), piper, at(30))
            .unwrap()
            .changed
    );
    assert!(
        client
            .change_status_once(id, StatusChange::Done, quinn, at(40))
            .unwrap()
            .changed
    );
    assert!(
        !client
            .change_status_once(id, StatusChange::Done, quinn, at(41))
            .unwrap()
            .changed
    );
    let request = new_entry(EntryKind::Decision, "Use SQLite", piper);
    let first = client.create_entry_once(&key("slash:5"), &request).unwrap();
    let retry = client.create_entry_once(&key("slash:5"), &request).unwrap();
    assert!(first.changed && !retry.changed);
    assert_eq!(first.entry, retry.entry);
    writer.shutdown().unwrap();
}
