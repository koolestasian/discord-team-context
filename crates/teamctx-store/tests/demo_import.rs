//! The demo importer against the real fictional dataset. All data is synthetic.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use serde_json::Value;
use teamctx_core::{
    id::{MemberId, Snowflake},
    record::{CreatedVia, EntryKind, EntryStatus, EntryTitle, NewEntry, ShortId},
    repo::{
        DirectoryRead, EntryFilter, IngestWrite, MessageRead, PageRequest, RecordRead, RecordWrite,
    },
    source::{Channel, ChannelKind, Member, Message, MessageRef, SourceEvent, SourceKind},
    time::UnixMillis,
};
use teamctx_store::{
    Store,
    demo::{DemoError, DemoMap, import},
};

const DATASET: &str = include_str!("../../../fixtures/demo/dataset.json");

fn dataset() -> Value {
    serde_json::from_str(DATASET).unwrap()
}

fn items<'a>(data: &'a Value, key: &str) -> &'a [Value] {
    data[key].as_array().unwrap()
}

fn sf(text: &str) -> Snowflake {
    text.parse().unwrap()
}

fn all_entries(store: &Store) -> Vec<teamctx_core::record::RecordEntry> {
    store
        .list_entries(
            &EntryFilter::default(),
            PageRequest {
                offset: 0,
                limit: 10_000,
            },
        )
        .unwrap()
}

fn message_total(store: &Store) -> u64 {
    store
        .channel_activity_since(UnixMillis::from_millis(0), 1000)
        .unwrap()
        .iter()
        .map(|c| c.messages)
        .sum()
}

fn imported() -> (Store, DemoMap) {
    let mut store = Store::in_memory().unwrap();
    let map = import(&mut store, DATASET).unwrap();
    (store, map)
}

fn fixture_id(entry: &Value) -> ShortId {
    format!(
        "{}-{}",
        entry["kind"]
            .as_str()
            .unwrap()
            .parse::<EntryKind>()
            .unwrap()
            .prefix(),
        entry["id"]
    )
    .parse()
    .unwrap()
}

#[test]
fn imports_the_whole_dataset_with_matching_counts() {
    let data = dataset();
    let (store, map) = imported();

    assert_eq!(map.message_count(), items(&data, "messages").len());
    assert_eq!(message_total(&store), items(&data, "messages").len() as u64);
    assert_eq!(map.entry_count(), items(&data, "entries").len());
    let stored = all_entries(&store);
    assert_eq!(stored.len(), items(&data, "entries").len());

    for kind in EntryKind::ALL {
        for status in kind.statuses() {
            let expected = items(&data, "entries")
                .iter()
                .filter(|e| e["kind"] == kind.as_str() && e["status"] == status.as_str())
                .count();
            let actual = stored
                .iter()
                .filter(|e| e.kind == *kind && e.status == *status)
                .count();
            assert_eq!(actual, expected, "{kind:?} {status:?}");
        }
    }
    // The record is not empty in every bucket, so the loop above is meaningful.
    assert!(stored.iter().any(|e| e.status == EntryStatus::Superseded));
    assert!(stored.iter().any(|e| e.status == EntryStatus::Answered));
    assert!(stored.iter().any(|e| e.status == EntryStatus::Dropped));
    assert!(stored.iter().any(|e| e.status == EntryStatus::Done));

    for member in items(&data, "members") {
        let id = map.member(sf(member["id"].as_str().unwrap())).unwrap();
        let row = store.member(id).unwrap().unwrap();
        assert_eq!(row.display_name, member["display_name"].as_str().unwrap());
        assert_eq!(row.is_bot, member["is_bot"].as_bool().unwrap());
    }
    assert_eq!(
        store.channels(map.source()).unwrap().len(),
        items(&data, "channels").len()
    );
}

#[test]
fn the_map_resolves_fixture_entries_and_messages_to_stored_rows() {
    let data = dataset();
    let (store, map) = imported();

    for value in items(&data, "entries") {
        let fixture = fixture_id(value);
        let stored = store.entry(map.entry(fixture).unwrap()).unwrap().unwrap();
        assert_eq!(
            map.entry_label(&fixture.to_string()),
            Some(stored_id(&stored))
        );
        assert_eq!(map.fixture_entry(stored_id(&stored)), Some(fixture));
        assert_eq!(stored.title.as_str(), value["title"].as_str().unwrap());
        assert_eq!(stored.status.as_str(), value["status"].as_str().unwrap());
        assert_eq!(stored.kind.as_str(), value["kind"].as_str().unwrap());
        assert_eq!(
            stored.due_at.map(UnixMillis::as_millis),
            value["due_at"].as_i64()
        );

        let cited = map.message(sf(value["source_message_id"].as_str().unwrap()));
        assert_eq!(stored.source_message, cited);
        assert_eq!(
            stored.owner,
            value["owner_id"]
                .as_str()
                .map(|id| map.member(sf(id)).unwrap())
        );
        assert_eq!(
            stored.answered_by_message,
            value["answered_by_message_id"]
                .as_str()
                .map(|id| map.message(sf(id)).unwrap())
        );
        // `superseded_by` names the stored row of the replacing fixture entry.
        let replacer = value["superseded_by"]
            .as_i64()
            .map(|raw| map.entry_label(&format!("D-{raw}")).unwrap().id);
        assert_eq!(stored.superseded_by, replacer);
    }

    // Messages round-trip through the map in both directions.
    for value in items(&data, "messages") {
        let external = sf(value["id"].as_str().unwrap());
        let row = map.message(external).unwrap();
        assert_eq!(map.message_external(row), Some(external));
        let stored = store.message(row).unwrap().unwrap();
        assert_eq!(stored.content, value["content"].as_str().unwrap());
        assert_eq!(stored.external_id, external);
    }

    // The two examples named in the dataset README resolve to decisions and tasks.
    assert_eq!(map.entry_label("D-7").unwrap().kind, EntryKind::Decision);
    assert_eq!(map.entry_label("T-2").unwrap().kind, EntryKind::Task);
    assert_eq!(map.entry_label("D-9999"), None);
    assert_eq!(map.entry_label("not an id"), None);
}

fn stored_id(entry: &teamctx_core::record::RecordEntry) -> ShortId {
    ShortId {
        kind: entry.kind,
        id: entry.id,
    }
}

#[test]
fn replacing_decisions_supersede_and_answers_cite_the_indicated_message() {
    let data = dataset();
    let (store, map) = imported();

    let mut replacements = 0;
    for value in items(&data, "entries") {
        if let Some(old) = value["replaces"].as_i64() {
            replacements += 1;
            let old = store
                .entry(map.entry_label(&format!("D-{old}")).unwrap())
                .unwrap()
                .unwrap();
            let new = map.entry(fixture_id(value)).unwrap();
            assert_eq!(old.status, EntryStatus::Superseded);
            assert_eq!(old.superseded_by, Some(new.id));
        }
    }
    assert!(replacements >= 8);

    let answered = items(&data, "entries")
        .iter()
        .filter(|e| e["answered_by_message_id"].is_string())
        .count();
    assert!(answered > 0);
}

#[test]
fn no_stored_entry_follows_or_cites_the_injection_messages() {
    let data = dataset();
    let (store, map) = imported();
    let injected: Vec<_> = items(&data, "messages")
        .iter()
        .filter(|m| {
            let text = m["content"].as_str().unwrap().to_lowercase();
            text.contains("ignore all previous instructions")
                || text.contains("disregard all prior instructions")
        })
        .map(|m| map.message(sf(m["id"].as_str().unwrap())).unwrap())
        .collect();
    assert_eq!(injected.len(), 2);
    // The injection messages are stored as ordinary data.
    for id in &injected {
        assert!(store.message(*id).unwrap().is_some());
    }
    for entry in all_entries(&store) {
        assert!(!injected.contains(&entry.source_message.unwrap()));
        let title = entry.title.as_str().to_lowercase();
        assert!(!title.contains("free forever") && !title.contains("admin token"));
    }
}

#[test]
fn importing_again_changes_nothing() {
    let (mut store, first) = imported();
    let entries_before = all_entries(&store);
    let events_before: Vec<_> = entries_before
        .iter()
        .map(|e| store.entry_events(e.id).unwrap())
        .collect();
    let messages_before = message_total(&store);

    let second = import(&mut store, DATASET).unwrap();

    assert_eq!(second, first);
    assert_eq!(all_entries(&store), entries_before);
    let events_after: Vec<_> = entries_before
        .iter()
        .map(|e| store.entry_events(e.id).unwrap())
        .collect();
    assert_eq!(events_after, events_before);
    assert_eq!(message_total(&store), messages_before);
}

#[test]
fn an_interrupted_import_is_completed_by_the_next_one() {
    // A dataset cut to its first 10 entries stands in for an import that stopped early.
    // D-1, D-3 and D-8 are superseded by later entries that the cut removed, so they are
    // active
    // here; a cut copy that still said "superseded" is rejected before any write (see
    // `lifecycle_violations_found_late_write_nothing`).
    let mut data = dataset();
    data["entries"].as_array_mut().unwrap().truncate(10);
    for index in [0, 2, 7] {
        data["entries"][index]["status"] = Value::from("active");
    }
    let mut store = Store::in_memory().unwrap();
    import(&mut store, &data.to_string()).unwrap();
    assert_eq!(all_entries(&store).len(), 10);

    let map = import(&mut store, DATASET).unwrap();
    assert_eq!(all_entries(&store).len(), map.entry_count());
    assert_eq!(
        map.entry_label("D-1").unwrap().id.get(),
        1,
        "the first 10 entries were reused, not duplicated"
    );
}

#[test]
fn entries_already_in_the_database_shift_the_real_ids_but_not_the_map() {
    let mut store = Store::in_memory().unwrap();
    // One unrelated entry in another source takes entry id 1 before the demo loads.
    let other = store
        .upsert_source(SourceKind::Discord, sf("999"), "Other (fictional)")
        .unwrap();
    let member = Member {
        id: sf("900"),
        display_name: "Test Person".to_owned(),
        is_bot: false,
    };
    let channel = Channel {
        id: sf("901"),
        parent: None,
        kind: ChannelKind::Text,
        name: "misc".to_owned(),
        topic: None,
        included: true,
        archived: false,
    };
    let message = Message {
        id: MessageRef {
            channel: sf("901"),
            message: sf("902"),
        },
        author: Some(member.clone()),
        content: "a lone note".to_owned(),
        mentions: Vec::new(),
        reply_to: None,
        attachment_names: Vec::new(),
        created_at: UnixMillis::from_millis(1),
        edited_at: None,
    };
    let now = UnixMillis::from_millis(2);
    for event in [
        SourceEvent::MemberUpsert(member),
        SourceEvent::ChannelUpsert(channel),
        SourceEvent::MessageUpsert(message.clone()),
    ] {
        store.apply_event(other, &event, now).unwrap();
    }
    let author: MemberId = store
        .member_by_external(other, sf("900"))
        .unwrap()
        .unwrap()
        .id;
    let stored_message = store.message_by_ref(other, message.id).unwrap().unwrap();
    let lone = store
        .create_entry(&NewEntry {
            kind: EntryKind::Decision,
            title: EntryTitle::new("A lone decision").unwrap(),
            title_is_copied: false,
            owner: None,
            due_at: None,
            source_message: Some(stored_message.id),
            created_by: author,
            created_via: CreatedVia::Cli,
            tags: Vec::new(),
            replaces: None,
            created_at: now,
        })
        .unwrap();
    assert_eq!(lone.id.get(), 1);

    let map = import(&mut store, DATASET).unwrap();

    let fixture: ShortId = "D-1".parse().unwrap();
    let real = map.entry(fixture).unwrap();
    assert_eq!(real.id.get(), 2, "the demo's D-1 is the second entry");
    assert_eq!(map.fixture_entry(real), Some(fixture));
    assert_eq!(
        store.entry(real).unwrap().unwrap().title.as_str(),
        dataset()["entries"][0]["title"].as_str().unwrap()
    );
    assert_eq!(all_entries(&store).len(), map.entry_count() + 1);
    // The unrelated entry is untouched.
    assert_eq!(
        store.entry(stored_id(&lone)).unwrap().unwrap().status,
        EntryStatus::Active
    );
}

fn corrupt(edit: impl FnOnce(&mut Value)) -> String {
    let mut data = dataset();
    edit(&mut data);
    data.to_string()
}

fn expect_error(text: &str) -> DemoError {
    let mut store = Store::in_memory().unwrap();
    let error = import(&mut store, text).unwrap_err();
    // A malformed dataset is rejected before the first write.
    assert!(all_entries(&store).is_empty());
    assert_eq!(message_total(&store), 0);
    error
}

#[test]
fn a_dangling_source_message_is_an_error_naming_the_index() {
    let text = corrupt(|data| {
        data["entries"][3]["source_message_id"] = Value::from("424242");
    });
    let error = expect_error(&text);
    assert!(matches!(
        error,
        DemoError::Dangling {
            section: "entries",
            index: 3,
            field: "source_message_id"
        }
    ));
    let shown = error.to_string();
    assert!(shown.contains("entries record 3"), "{shown}");
    let title = dataset()["entries"][3]["title"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(!shown.contains(&title), "errors never quote record content");
}

#[test]
fn other_broken_references_are_errors_not_panics() {
    type Case = (&'static str, fn(&mut Value), &'static str, usize);
    let cases: [Case; 5] = [
        (
            "message in an unknown channel",
            |d| d["messages"][5]["channel_id"] = Value::from("987654"),
            "messages",
            5,
        ),
        (
            "message by an unknown author",
            |d| d["messages"][7]["author_id"] = Value::from("987654"),
            "messages",
            7,
        ),
        (
            "reply to a later message",
            |d| {
                let later = d["messages"][20]["id"].clone();
                d["messages"][2]["reply_to"] = later;
            },
            "messages",
            2,
        ),
        (
            "replaces an entry that does not exist",
            |d| d["entries"][4]["replaces"] = Value::from(9999),
            "entries",
            4,
        ),
        (
            "owner that is not a member",
            |d| d["entries"][1]["owner_id"] = Value::from("987654"),
            "entries",
            1,
        ),
    ];
    for (name, edit, expected_section, expected_index) in cases {
        match expect_error(&corrupt(edit)) {
            DemoError::Dangling { section, index, .. }
            | DemoError::Malformed { section, index, .. } => {
                assert_eq!(
                    (section, index),
                    (expected_section, expected_index),
                    "{name}"
                );
            }
            other => panic!("{name}: unexpected error {other}"),
        }
    }
}

#[test]
fn malformed_records_and_datasets_are_typed_errors() {
    assert!(matches!(expect_error("not json"), DemoError::NotJson));
    assert!(matches!(
        expect_error(&corrupt(|d| d["schema_version"] = Value::from(2))),
        DemoError::UnsupportedDataset
    ));
    assert!(matches!(
        expect_error(&corrupt(|d| d["fictional"] = Value::from(false))),
        DemoError::UnsupportedDataset
    ));
    assert!(matches!(
        expect_error(&corrupt(|d| d["entries"][6]["title"] = Value::from(7))),
        DemoError::Malformed {
            section: "entries",
            index: 6,
            field: "title"
        }
    ));
    assert!(matches!(
        expect_error(&corrupt(|d| {
            d["entries"][0]["status"] = Value::from("done");
        })),
        DemoError::Malformed {
            section: "entries",
            index: 0,
            field: "status"
        }
    ));
    assert!(matches!(
        expect_error(&corrupt(|d| {
            let first = d["members"][0].clone();
            d["members"][1] = first;
        })),
        DemoError::Malformed {
            section: "members",
            index: 1,
            field: "id"
        }
    ));
    // A superseded decision that nothing replaces is reported before anything is written.
    let mut store = Store::in_memory().unwrap();
    let text = corrupt(|d| {
        let entries = d["entries"].as_array_mut().unwrap();
        let replacer = entries.iter().position(|e| e["replaces"] == 1).unwrap();
        entries[replacer]["replaces"] = Value::Null;
    });
    assert!(matches!(
        import(&mut store, &text),
        Err(DemoError::StatusMismatch { index: 0 })
    ));
}

/// Row counts of every table, plus the autoincrement counters, read straight from the
/// file. Equal snapshots mean an import wrote nothing at all.
fn snapshot(path: &std::path::Path) -> Vec<(String, i64)> {
    let conn = rusqlite::Connection::open(path).unwrap();
    let names: Vec<String> = {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master
                 WHERE type = 'table' AND name <> 'sqlite_sequence'
                 ORDER BY name",
            )
            .unwrap();
        stmt.query_map([], |row| row.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    let mut rows: Vec<(String, i64)> = names
        .iter()
        .map(|name| {
            let count = conn
                .query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (name.clone(), count)
        })
        .collect();
    let mut stmt = conn
        .prepare("SELECT name, seq FROM sqlite_sequence ORDER BY name")
        .unwrap();
    rows.extend(
        stmt.query_map([], |row| {
            Ok((format!("seq:{}", row.get::<_, String>(0)?), row.get(1)?))
        })
        .unwrap()
        .map(Result::unwrap),
    );
    rows
}

/// Imports `text` into a fresh file database and requires that it fails without having
/// written a single row. Returns the error.
fn rejected_without_writing(text: &str) -> DemoError {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo.db");
    let mut store = Store::open(&path).unwrap();
    let before = snapshot(&path);
    let error = import(&mut store, text).unwrap_err();
    assert_eq!(
        snapshot(&path),
        before,
        "a rejected dataset wrote rows: {error}"
    );
    error
}

#[test]
fn a_record_rule_violation_found_late_writes_nothing() {
    // A due date on a decision parses fine, but the record rules reject it. Before the
    // fix this was only noticed while writing entry 0, after every member, channel and
    // message had been committed.
    let error = rejected_without_writing(&corrupt(|d| {
        d["entries"][0]["due_at"] = Value::from(1_774_976_400_000_i64);
    }));
    assert!(
        matches!(
            error,
            DemoError::Malformed {
                section: "entries",
                index: 0,
                field: "due_at"
            }
        ),
        "{error}"
    );

    // Entry 1 is a task; only decisions may replace another entry.
    let error = rejected_without_writing(&corrupt(|d| {
        d["entries"][1]["replaces"] = Value::from(1);
    }));
    assert!(
        matches!(
            error,
            DemoError::Malformed {
                section: "entries",
                index: 1,
                field: "replaces"
            }
        ),
        "{error}"
    );
}

#[test]
fn lifecycle_violations_found_late_write_nothing() {
    // D-1 is already replaced by entry 49; replacing it a second time is not allowed.
    let error = rejected_without_writing(&corrupt(|d| {
        d["entries"][75]["replaces"] = Value::from(1);
    }));
    assert!(
        matches!(
            error,
            DemoError::Malformed {
                section: "entries",
                index: 75,
                field: "replaces"
            }
        ),
        "{error}"
    );

    // A superseded decision that nothing replaces.
    let error = rejected_without_writing(&corrupt(|d| {
        let entries = d["entries"].as_array_mut().unwrap();
        let replacer = entries.iter().position(|e| e["replaces"] == 1).unwrap();
        entries[replacer]["replaces"] = Value::Null;
    }));
    assert!(
        matches!(error, DemoError::StatusMismatch { index: 0 }),
        "{error}"
    );

    // A message in a channel the store would not keep.
    let mut data = dataset();
    let channel = data["channels"][0]["id"].clone();
    let index = items(&data, "messages")
        .iter()
        .position(|m| m["channel_id"] == channel)
        .unwrap();
    data["channels"][0]["included"] = Value::from(false);
    let error = rejected_without_writing(&data.to_string());
    assert!(
        matches!(error, DemoError::NotStored { section: "messages", index: i } if i == index),
        "{error}"
    );
}
