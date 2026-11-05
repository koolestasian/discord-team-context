//! All data below is synthetic. No Discord or model service is used.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use std::sync::Arc;
use teamctx_core::{
    id::*,
    query::{Clause, Query},
    record::*,
    repo::*,
    source::*,
    time::UnixMillis,
};
use teamctx_store::{Store, Writer};

fn sf(id: u64) -> Snowflake {
    Snowflake::new(id).unwrap()
}
fn at(time: i64) -> UnixMillis {
    UnixMillis::from_millis(time)
}
fn channel(id: u64, parent: Option<u64>) -> Channel {
    Channel {
        id: sf(id),
        parent: parent.map(sf),
        kind: if parent.is_some() {
            ChannelKind::Thread
        } else {
            ChannelKind::Text
        },
        name: format!("room-{id}"),
        topic: None,
        included: true,
        archived: false,
    }
}
fn author() -> Member {
    Member {
        id: sf(100),
        display_name: "Piper Fiction".to_owned(),
        is_bot: false,
    }
}
fn message(id: u64, room: u64, text: &str) -> Message {
    Message {
        id: MessageRef {
            channel: sf(room),
            message: sf(id),
        },
        author: Some(author()),
        content: text.to_owned(),
        reply_to: None,
        attachment_names: vec!["fiction.txt".to_owned()],
        created_at: at(id as i64),
        edited_at: None,
        mentions: vec![sf(100)],
    }
}
fn setup() -> (Store, SourceId) {
    let mut db = Store::in_memory().unwrap();
    let source = db
        .upsert_source(SourceKind::Discord, sf(1), "Fiction Team")
        .unwrap();
    db.apply_event(
        source,
        &SourceEvent::ChannelUpsert(channel(10, None)),
        at(1),
    )
    .unwrap();
    db.apply_event(source, &SourceEvent::MemberUpsert(author()), at(1))
        .unwrap();
    (db, source)
}
fn search(text: &str) -> Query {
    Query {
        clauses: vec![Clause::Word(text.to_owned())],
    }
}
fn captured(
    db: &mut Store,
    source: SourceId,
    msg: &Message,
    kind: EntryKind,
    copied: bool,
) -> RecordEntry {
    let message = db.message_by_ref(source, msg.id).unwrap().unwrap();
    let actor = db.member_by_external(source, author().id).unwrap().unwrap();
    db.create_entry(&NewEntry {
        kind,
        title: if copied {
            EntryTitle::copy_from_message(&msg.content).unwrap()
        } else {
            EntryTitle::new("Human retained summary").unwrap()
        },
        title_is_copied: copied,
        source_message: Some(message.id),
        owner: None,
        due_at: None,
        created_by: actor.id,
        created_via: CreatedVia::Slash,
        tags: vec![Tag::new("fiction").unwrap()],
        replaces: None,
        created_at: at(300),
    })
    .unwrap()
}

#[test]
fn migrations_reopen_readonly_and_reject_newer_schema() {
    let file = tempfile::NamedTempFile::new().unwrap();
    {
        let mut db = Store::open(file.path()).unwrap();
        assert_eq!(
            db.upsert_source(SourceKind::Discord, sf(1), "Fiction")
                .unwrap()
                .get(),
            1
        );
    }
    {
        let db = Store::open(file.path()).unwrap();
        assert_eq!(
            db.source(SourceId::new(1).unwrap()).unwrap().unwrap().name,
            "Fiction"
        );
    }
    let mut reader = Store::open_read_only(file.path()).unwrap();
    assert!(
        reader
            .upsert_source(SourceKind::Discord, sf(2), "Must fail")
            .is_err()
    );
    drop(reader);
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        conn.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    conn.pragma_update(None, "user_version", 3).unwrap();
    drop(conn);
    assert!(Store::open(file.path()).is_err());
    assert!(Store::open_read_only(file.path()).is_err());
}

#[test]
fn idempotent_ingestion_partial_patch_mentions_and_safe_fts() {
    let (mut db, source) = setup();
    let msg = message(20, 10, "sqlite oldterm OR punctuation");
    assert_eq!(
        db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(2))
            .unwrap(),
        ApplyOutcome::Applied
    );
    assert_eq!(
        db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(2))
            .unwrap(),
        ApplyOutcome::Unchanged
    );
    let stored = db.message_by_ref(source, msg.id).unwrap().unwrap();
    assert_eq!(stored.attachment_names, ["fiction.txt"]);
    let actor = db.member_by_external(source, sf(100)).unwrap().unwrap().id;
    assert_eq!(db.mentions_since(actor, at(0), 10).unwrap().len(), 1);
    let patch = MessagePatch {
        id: msg.id,
        content: Some("sqlite newterm".to_owned()),
        mentions: Some(vec![]),
        attachment_names: None,
        edited_at: Some(at(50)),
    };
    assert_eq!(
        db.apply_event(source, &SourceEvent::MessageUpdate(patch.clone()), at(50))
            .unwrap(),
        ApplyOutcome::Applied
    );
    assert_eq!(
        db.apply_event(source, &SourceEvent::MessageUpdate(patch), at(50))
            .unwrap(),
        ApplyOutcome::Unchanged
    );
    assert!(
        db.search_messages(&search("oldterm"), 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(db.search_messages(&search("newterm"), 10).unwrap().len(), 1);
    assert!(db.mentions_since(actor, at(0), 10).unwrap().is_empty());
    for (term, expected) in [
        ("!!!", 0),
        ("\0", 0),
        ("\" OR \"", 0),
        ("NEAR(sqlite)", 0),
        ("sqlite*", 1),
        ("sqlite\0OR", 0),
        ("sqlite\" OR \"absent", 0),
        ("\"; DROP TABLE messages;--", 0),
    ] {
        assert_eq!(
            db.search_messages(&search(term), 10).unwrap().len(),
            expected,
            "literal {term:?}"
        );
    }
    assert_eq!(db.search_messages(&search("sqlite"), 10).unwrap().len(), 1);
    let missing = MessagePatch {
        id: MessageRef {
            channel: sf(10),
            message: sf(999),
        },
        content: None,
        mentions: None,
        attachment_names: None,
        edited_at: None,
    };
    assert_eq!(
        db.apply_event(source, &SourceEvent::MessageUpdate(missing.clone()), at(5))
            .unwrap(),
        ApplyOutcome::NeedsFetch(missing.id)
    );
}

#[test]
fn deletion_removes_fts_copied_title_and_answer_but_keeps_human_title() {
    let (mut db, source) = setup();
    let msg = message(20, 10, "secret_deleted_term");
    db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
        .unwrap();
    let copied = captured(&mut db, source, &msg, EntryKind::Decision, true);
    let human = captured(&mut db, source, &msg, EntryKind::Task, false);
    let question = captured(&mut db, source, &msg, EntryKind::Question, false);
    db.change_status(
        question.short_id(),
        StatusChange::Answer {
            message: copied.source_message.unwrap(),
        },
        question.created_by,
        at(400),
    )
    .unwrap();
    assert!(
        !db.search_entries(&search("secret_deleted_term"), 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db.apply_event(
            source,
            &SourceEvent::MessageDelete { message: msg.id },
            at(500)
        )
        .unwrap(),
        ApplyOutcome::Applied
    );
    assert_eq!(
        db.apply_event(
            source,
            &SourceEvent::MessageDelete { message: msg.id },
            at(500)
        )
        .unwrap(),
        ApplyOutcome::Unchanged
    );
    assert!(
        db.message(copied.source_message.unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        db.search_messages(&search("secret_deleted_term"), 10)
            .unwrap()
            .is_empty()
    );
    assert!(
        db.search_entries(&search("secret_deleted_term"), 10)
            .unwrap()
            .is_empty()
    );
    let deleted = db.entry(copied.short_id()).unwrap().unwrap();
    assert!(deleted.source_deleted);
    assert_eq!(deleted.title.as_str(), SOURCE_DELETED_TITLE);
    assert_eq!(deleted.source_message, None);
    assert_eq!(
        db.entry(human.short_id()).unwrap().unwrap().title.as_str(),
        "Human retained summary"
    );
    assert_eq!(
        db.entry(question.short_id())
            .unwrap()
            .unwrap()
            .answered_by_message,
        None
    );
    let audit = db.entry_events(copied.id).unwrap();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[1].action, RecordAction::SourceDeleted);
    assert_eq!(audit[1].actor, None);
    // Deleted SQLite ids must never be recycled into stale citations.
    db.apply_event(
        source,
        &SourceEvent::MessageUpsert(message(21, 10, "replacement")),
        at(501),
    )
    .unwrap();
    assert_ne!(
        db.message_by_ref(
            source,
            MessageRef {
                channel: sf(10),
                message: sf(21)
            }
        )
        .unwrap()
        .unwrap()
        .id,
        copied.source_message.unwrap()
    );
}

#[test]
fn channel_delete_and_exclusion_purge_descendants_and_retention_audits() {
    for exclude in [false, true] {
        let (mut db, source) = setup();
        db.apply_event(
            source,
            &SourceEvent::ChannelUpsert(channel(11, Some(10))),
            at(1),
        )
        .unwrap();
        let msg = message(20, 11, "private cascade");
        db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
            .unwrap();
        let entry = captured(&mut db, source, &msg, EntryKind::Decision, true);
        let event = if exclude {
            let mut c = channel(10, None);
            c.included = false;
            SourceEvent::ChannelUpsert(c)
        } else {
            SourceEvent::ChannelDelete { channel: sf(10) }
        };
        db.apply_event(source, &event, at(500)).unwrap();
        assert!(db.message(entry.source_message.unwrap()).unwrap().is_none());
        assert_eq!(
            db.entry(entry.short_id()).unwrap().unwrap().title.as_str(),
            SOURCE_DELETED_TITLE
        );
        assert!(
            db.search_messages(&search("private"), 10)
                .unwrap()
                .is_empty()
        );
        if exclude {
            assert!(db.channels(source).unwrap().iter().all(|c| !c.included));
            assert_eq!(
                db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(501))
                    .unwrap(),
                ApplyOutcome::Excluded
            );
            db.apply_event(
                source,
                &SourceEvent::MessageDelete { message: msg.id },
                at(502),
            )
            .unwrap();
        } else {
            assert!(db.channels(source).unwrap().is_empty());
        }
    }
    let (mut db, source) = setup();
    let msg = message(20, 10, "retained title");
    db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
        .unwrap();
    let entry = captured(&mut db, source, &msg, EntryKind::Decision, true);
    assert_eq!(db.purge_messages_before(at(20), at(500)).unwrap(), 0);
    assert_eq!(db.purge_messages_before(at(21), at(500)).unwrap(), 1);
    assert!(db.entry(entry.short_id()).unwrap().unwrap().source_deleted);
    assert_eq!(db.entry_events(entry.id).unwrap().len(), 2);
}

#[test]
fn reaction_capture_is_unique_and_removal_keeps_record() {
    let (mut db, source) = setup();
    let msg = message(20, 10, "  one\n  decision ");
    db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
        .unwrap();
    let marker = Marker {
        message: msg.id,
        actor: sf(100),
        kind: EntryKind::Decision,
    };
    assert_eq!(
        db.apply_event(source, &SourceEvent::MarkerAdd(marker), at(300))
            .unwrap(),
        ApplyOutcome::Applied
    );
    assert_eq!(
        db.apply_event(source, &SourceEvent::MarkerAdd(marker), at(301))
            .unwrap(),
        ApplyOutcome::Unchanged
    );
    assert_eq!(
        db.apply_event(source, &SourceEvent::MarkerRemove(marker), at(302))
            .unwrap(),
        ApplyOutcome::Unchanged
    );
    let entries = db
        .list_entries(
            &EntryFilter::default(),
            PageRequest {
                offset: 0,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].title.as_str(), "one decision");
    assert_eq!(db.entry_events(entries[0].id).unwrap().len(), 1);
}

#[test]
fn record_replacement_is_atomic_and_status_lifecycle_is_enforced() {
    let (mut db, source) = setup();
    let msg = message(20, 10, "initial decision");
    db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
        .unwrap();
    let old = captured(&mut db, source, &msg, EntryKind::Decision, true);
    let base = NewEntry {
        kind: EntryKind::Decision,
        title: EntryTitle::new("replacing decision").unwrap(),
        title_is_copied: false,
        source_message: None,
        owner: None,
        due_at: None,
        created_by: old.created_by,
        created_via: CreatedVia::Slash,
        tags: vec![],
        replaces: Some(old.id),
        created_at: at(400),
    };
    let new = db.create_entry(&base).unwrap();
    let superseded = db.entry(old.short_id()).unwrap().unwrap();
    assert_eq!(superseded.superseded_by, Some(new.id));
    assert_eq!(superseded.status, EntryStatus::Superseded);
    assert!(db.create_entry(&base).is_err());
    assert_eq!(
        db.list_entries(
            &EntryFilter::default(),
            PageRequest {
                offset: 0,
                limit: 20
            }
        )
        .unwrap()
        .len(),
        2
    );
    assert_eq!(db.entry_events(old.id).unwrap().len(), 2);
    let mut bad = base;
    bad.replaces = None;
    bad.owner = MemberId::new(999);
    assert!(db.create_entry(&bad).is_err());
    assert_eq!(
        db.list_entries(
            &EntryFilter::default(),
            PageRequest {
                offset: 0,
                limit: 20
            }
        )
        .unwrap()
        .len(),
        2
    );
    assert!(
        db.change_status(
            new.short_id(),
            StatusChange::Supersede { by: new.id },
            old.created_by,
            at(500)
        )
        .is_err()
    );
}

#[test]
fn date_filters_require_explicit_timezone_and_repeated_filters_use_and() {
    let (mut db, source) = setup();
    let mut msg = message(20, 10, "query sample");
    msg.created_at = "2026-03-10"
        .parse::<teamctx_core::time::CalendarDate>()
        .unwrap()
        .start_of_day_utc();
    db.apply_event(source, &SourceEvent::MessageUpsert(msg), at(20))
        .unwrap();
    let query = Query {
        clauses: vec![
            Clause::After("2026-03-10".parse().unwrap()),
            Clause::Before("2026-03-11".parse().unwrap()),
        ],
    };
    assert!(db.search_messages(&query, 10).is_err());
    db.set_date_resolver(Arc::new(|date| Ok(date.start_of_day_utc())));
    assert_eq!(db.search_messages(&query, 10).unwrap().len(), 1);
    let query = Query {
        clauses: vec![
            Clause::In("room-10".to_owned()),
            Clause::In("other".to_owned()),
        ],
    };
    assert!(db.search_messages(&query, 10).unwrap().is_empty());
    let query = Query {
        clauses: vec![Clause::Before("2026-03-10".parse().unwrap())],
    };
    assert!(db.search_messages(&query, 10).unwrap().is_empty());
}

#[test]
fn writer_serializes_concurrent_commands_and_revocation_is_visible_to_reader() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let writer = Writer::spawn(Store::open(file.path()).unwrap()).unwrap();
    let mut client = writer.client();
    let source = client
        .upsert_source(SourceKind::Discord, sf(1), "Fiction")
        .unwrap();
    client
        .apply_event(
            source,
            &SourceEvent::ChannelUpsert(channel(10, None)),
            at(1),
        )
        .unwrap();
    let mut threads = vec![];
    for n in 0..4 {
        let mut client = writer.client();
        threads.push(std::thread::spawn(move || {
            for i in 0..10 {
                let msg = message(1000 + n * 10 + i, 10, "concurrent fictional message");
                client
                    .apply_event(source, &SourceEvent::MessageUpsert(msg), at(1000))
                    .unwrap();
            }
        }));
    }
    for thread in threads {
        thread.join().unwrap();
    }
    let reader = Store::open_read_only(file.path()).unwrap();
    assert_eq!(
        reader
            .search_messages(&search("fictional"), 100)
            .unwrap()
            .len(),
        40
    );
    let hash = TokenHash([7; 32]);
    let token = client
        .create_token("fiction-laptop", &hash, "read", at(2000))
        .unwrap();
    assert!(reader.active_token(&hash).unwrap().is_some());
    client.touch_token(token.id, at(2001)).unwrap();
    client.revoke_token("fiction-laptop", at(2002)).unwrap();
    assert!(reader.active_token(&hash).unwrap().is_none());
    assert_eq!(
        client
            .revoke_token("fiction-laptop", at(3000))
            .unwrap()
            .revoked_at,
        Some(at(2002))
    );
    assert!(
        client
            .create_token("other", &hash, "read", at(3001))
            .is_err()
    );
    writer.shutdown().unwrap();
    assert!(
        client
            .upsert_source(SourceKind::Discord, sf(2), "after shutdown")
            .is_err()
    );
    drop(reader);
    let reopened = Store::open(file.path()).unwrap();
    assert_eq!(
        reopened
            .search_messages(&search("fictional"), 100)
            .unwrap()
            .len(),
        40
    );
}

#[test]
fn migration_failure_and_write_failure_roll_back_schema_rows_and_fts() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch("CREATE TABLE record_entries (sentinel TEXT);")
        .unwrap();
    drop(conn);
    assert!(Store::open(file.path()).is_err());
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE name='sources'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.pragma_query_value(None, "user_version", |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(conn);

    let file = tempfile::NamedTempFile::new().unwrap();
    let mut db = Store::open(file.path()).unwrap();
    let source = db
        .upsert_source(SourceKind::Discord, sf(1), "Fiction")
        .unwrap();
    db.apply_event(
        source,
        &SourceEvent::ChannelUpsert(channel(10, None)),
        at(1),
    )
    .unwrap();
    let msg = message(20, 10, "rollback confidentialword");
    db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
        .unwrap();
    let actor = db.member_by_external(source, sf(100)).unwrap().unwrap().id;
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch("CREATE TRIGGER reject_audit BEFORE INSERT ON record_events BEGIN SELECT RAISE(ABORT,'synthetic failure'); END;").unwrap();
    let entry = NewEntry {
        kind: EntryKind::Decision,
        title: EntryTitle::new("rolledbacktitle").unwrap(),
        title_is_copied: false,
        source_message: None,
        owner: None,
        due_at: None,
        created_by: actor,
        created_via: CreatedVia::Slash,
        tags: vec![],
        replaces: None,
        created_at: at(300),
    };
    assert!(db.create_entry(&entry).is_err());
    assert!(
        db.list_entries(
            &EntryFilter::default(),
            PageRequest {
                offset: 0,
                limit: 20
            }
        )
        .unwrap()
        .is_empty()
    );
    assert!(
        db.search_entries(&search("rolledbacktitle"), 10)
            .unwrap()
            .is_empty()
    );
    conn.execute_batch("DROP TRIGGER reject_audit;").unwrap();
    let copied = captured(&mut db, source, &msg, EntryKind::Decision, true);
    conn.execute_batch("CREATE TRIGGER reject_deletion BEFORE INSERT ON record_events WHEN new.action='source_deleted' BEGIN SELECT RAISE(ABORT,'synthetic deletion failure'); END;").unwrap();
    assert!(
        db.apply_event(
            source,
            &SourceEvent::MessageDelete { message: msg.id },
            at(500)
        )
        .is_err()
    );
    assert!(
        db.message(copied.source_message.unwrap())
            .unwrap()
            .is_some()
    );
    assert_eq!(
        db.search_messages(&search("confidentialword"), 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.entry(copied.short_id()).unwrap().unwrap().title.as_str(),
        "rollback confidentialword"
    );
}

#[test]
fn cursor_and_neighbour_windows_use_numeric_external_ids_and_stay_in_thread() {
    let (mut db, source) = setup();
    db.apply_event(
        source,
        &SourceEvent::ChannelUpsert(channel(11, Some(10))),
        at(1),
    )
    .unwrap();
    for id in [2, 10, 3] {
        let mut msg = message(id, 10, "ordered");
        msg.created_at = at(100);
        db.apply_event(source, &SourceEvent::MessageUpsert(msg), at(100))
            .unwrap();
    }
    db.apply_event(
        source,
        &SourceEvent::MessageUpsert(message(5, 11, "other thread")),
        at(100),
    )
    .unwrap();
    let anchor = db
        .message_by_ref(
            source,
            MessageRef {
                channel: sf(10),
                message: sf(3),
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        db.neighbours(anchor.id, 1, 1)
            .unwrap()
            .iter()
            .map(|m| m.external_id.get())
            .collect::<Vec<_>>(),
        [2, 3, 10]
    );
    assert_eq!(
        db.neighbours(anchor.id, 0, 0).unwrap(),
        std::slice::from_ref(&anchor)
    );
    let cursor = SyncCursor {
        newest_seen: Some(sf(10)),
        oldest_backfilled: Some(sf(2)),
        backfill_done: true,
        updated_at: at(200),
    };
    db.set_cursor(source, sf(10), &cursor).unwrap();
    assert_eq!(db.cursor(source, sf(10)).unwrap(), Some(cursor));
    assert_eq!(db.cursor(source, sf(11)).unwrap(), None);
    db.set_last_catchup(anchor.author.unwrap(), at(300))
        .unwrap();
    assert_eq!(
        db.last_catchup(anchor.author.unwrap()).unwrap(),
        Some(at(300))
    );
}

#[test]
fn parent_reinclusion_restores_only_requested_descendants() {
    let (mut db, source) = setup();
    for (id, parent) in [(11, 10), (12, 11), (13, 10), (14, 13)] {
        let mut c = channel(id, Some(parent));
        c.included = id != 13;
        db.apply_event(source, &SourceEvent::ChannelUpsert(c), at(1))
            .unwrap();
    }
    let mut root = channel(10, None);
    root.included = false;
    db.apply_event(source, &SourceEvent::ChannelUpsert(root.clone()), at(2))
        .unwrap();
    assert!(db.channels(source).unwrap().iter().all(|c| !c.included));
    // A child may be discovered while an ancestor is excluded. Its requested
    // inclusion must survive even though its effective visibility remains false.
    db.apply_event(
        source,
        &SourceEvent::ChannelUpsert(channel(15, Some(11))),
        at(3),
    )
    .unwrap();
    root.included = true;
    db.apply_event(source, &SourceEvent::ChannelUpsert(root), at(4))
        .unwrap();
    for c in db.channels(source).unwrap() {
        assert_eq!(
            c.included,
            !matches!(c.external_id.get(), 13 | 14),
            "channel {}",
            c.external_id
        );
    }
    assert_eq!(
        db.apply_event(
            source,
            &SourceEvent::MessageUpsert(message(20, 12, "included again")),
            at(5)
        )
        .unwrap(),
        ApplyOutcome::Applied
    );
    assert_eq!(
        db.apply_event(
            source,
            &SourceEvent::MessageUpsert(message(21, 14, "still excluded")),
            at(5)
        )
        .unwrap(),
        ApplyOutcome::Excluded
    );
}

#[test]
fn version_one_migration_preserves_exclusions_until_connector_rediscovery() {
    let file = tempfile::NamedTempFile::new().unwrap();
    let conn = rusqlite::Connection::open(file.path()).unwrap();
    conn.execute_batch(include_str!("../migrations/0001_init.sql"))
        .unwrap();
    conn.execute_batch("INSERT INTO sources(id,kind,external_id,name) VALUES(1,'discord','1','Fiction'); INSERT INTO channels(id,source_id,external_id,kind,name,is_included,archived) VALUES(1,1,'10','text','room-10',0,0); INSERT INTO channels(id,source_id,external_id,parent_id,kind,name,is_included,archived) VALUES(2,1,'11',1,'thread','room-11',0,0);").unwrap();
    conn.pragma_update(None, "user_version", 1).unwrap();
    drop(conn);
    assert!(Store::open_read_only(file.path()).is_err());
    let mut db = Store::open(file.path()).unwrap();
    let source = SourceId::new(1).unwrap();
    db.apply_event(
        source,
        &SourceEvent::ChannelUpsert(channel(10, None)),
        at(2),
    )
    .unwrap();
    let child = db
        .channels(source)
        .unwrap()
        .into_iter()
        .find(|c| c.external_id == sf(11))
        .unwrap();
    assert!(!child.included);
    assert_eq!(child.parent, ChannelId::new(1));
    assert_eq!(child.name, "room-11");
    // v1's lossy exclusion cannot be inferred safely. Connector intent restores it.
    db.apply_event(
        source,
        &SourceEvent::ChannelUpsert(channel(11, Some(10))),
        at(3),
    )
    .unwrap();
    assert!(db.channels(source).unwrap().iter().all(|c| c.included));
    drop(db);
    let reader = Store::open_read_only(file.path()).unwrap();
    assert_eq!(reader.channels(source).unwrap().len(), 2);
}

#[test]
fn deleting_a_distinct_answer_updates_since_without_marking_the_source_deleted() {
    let (mut db, source) = setup();
    let question_message = message(20, 10, "Question retained source");
    let answer_message = message(21, 10, "Answer to remove");
    for msg in [&question_message, &answer_message] {
        db.apply_event(source, &SourceEvent::MessageUpsert(msg.clone()), at(20))
            .unwrap();
    }
    let question = captured(
        &mut db,
        source,
        &question_message,
        EntryKind::Question,
        true,
    );
    let answer = db
        .message_by_ref(source, answer_message.id)
        .unwrap()
        .unwrap();
    db.change_status(
        question.short_id(),
        StatusChange::Answer { message: answer.id },
        question.created_by,
        at(400),
    )
    .unwrap();
    db.apply_event(
        source,
        &SourceEvent::MessageDelete {
            message: answer_message.id,
        },
        at(500),
    )
    .unwrap();
    let updated = db.entry(question.short_id()).unwrap().unwrap();
    assert_eq!(updated.answered_by_message, None);
    assert_eq!(updated.updated_at, at(500));
    assert!(!updated.source_deleted);
    assert_eq!(updated.source_message, question.source_message);
    assert_eq!(updated.title, question.title);
    let changes = db
        .list_entries(
            &EntryFilter {
                updated_since: Some(at(450)),
                ..EntryFilter::default()
            },
            PageRequest {
                offset: 0,
                limit: 10,
            },
        )
        .unwrap();
    assert_eq!(changes, [updated]);
    assert_eq!(db.entry_events(question.id).unwrap().len(), 2);
}
