//! Validate the fictional demo against the core contracts before any DB import exists.

#![expect(
    clippy::expect_used,
    reason = "fixture loader helpers fail the test on malformed data"
)]

use std::collections::BTreeMap;

use serde_json::Value;
use teamctx_core::{
    id::{EntryId, MemberId, MessageId, Snowflake},
    record::{EntryKind, EntryStatus, EntryTitle, NewEntry, Tag, check_transition},
    source::{Channel, Member, Message, MessageRef, SourceEvent},
    time::UnixMillis,
};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().expect("string field")
}

fn id(value: &Value, key: &str) -> Snowflake {
    text(value, key)
        .parse()
        .expect("canonical synthetic snowflake")
}

fn time(value: &Value, key: &str) -> UnixMillis {
    UnixMillis::from_millis(value[key].as_i64().expect("UTC milliseconds"))
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().expect("array field")
}

#[test]
fn demo_loads_into_core_types_with_valid_cross_references() {
    let data: Value = serde_json::from_str(include_str!("../../../fixtures/demo/dataset.json"))
        .expect("valid JSON demo");
    assert_eq!(data["schema_version"], 1);
    assert_eq!(data["fictional"], true);
    assert!((280..=320).contains(&list(&data, "messages").len()));
    assert!(list(&data, "entries").len() >= 60);

    let mut members = BTreeMap::new();
    let mut member_rows = BTreeMap::new();
    let mut events = Vec::new();
    for (index, value) in list(&data, "members").iter().enumerate() {
        let member = Member {
            id: id(value, "id"),
            display_name: text(value, "display_name").to_owned(),
            is_bot: value["is_bot"].as_bool().expect("bot flag"),
        };
        assert!(
            member_rows
                .insert(member.id, MemberId::new(index as i64 + 1).expect("row id"))
                .is_none()
        );
        assert!(members.insert(member.id, member.clone()).is_none());
        events.push(SourceEvent::MemberUpsert(member));
    }

    let mut channels = BTreeMap::new();
    for value in list(&data, "channels") {
        let channel = Channel {
            id: id(value, "id"),
            parent: value["parent_id"]
                .as_str()
                .map(|s| s.parse().expect("parent ID")),
            kind: text(value, "kind").parse().expect("channel kind"),
            name: text(value, "name").to_owned(),
            topic: value["topic"].as_str().map(str::to_owned),
            included: value["included"].as_bool().expect("included flag"),
            archived: value["archived"].as_bool().expect("archived flag"),
        };
        channel.validate().expect("valid channel");
        if let Some(parent) = channel.parent {
            assert!(channels.contains_key(&parent), "parent loaded first");
        }
        assert!(channels.insert(channel.id, channel.clone()).is_none());
        events.push(SourceEvent::ChannelUpsert(channel));
    }
    assert_eq!(channels.values().filter(|c| c.parent.is_none()).count(), 4);
    assert!(channels.values().filter(|c| c.parent.is_some()).count() >= 5);
    assert!(channels.values().any(|c| c.archived), "one archived thread");

    let mut messages = BTreeMap::new();
    let mut message_rows = BTreeMap::new();
    for (index, value) in list(&data, "messages").iter().enumerate() {
        let message = Message {
            id: MessageRef {
                channel: id(value, "channel_id"),
                message: id(value, "id"),
            },
            author: Some(members[&id(value, "author_id")].clone()),
            content: text(value, "content").to_owned(),
            mentions: Vec::new(),
            reply_to: value["reply_to"]
                .as_str()
                .map(|s| s.parse().expect("reply ID")),
            attachment_names: list(value, "attachment_names")
                .iter()
                .map(|v| v.as_str().expect("filename").to_owned())
                .collect(),
            created_at: time(value, "created_at"),
            edited_at: value["edited_at"].as_i64().map(UnixMillis::from_millis),
        };
        assert!(channels.contains_key(&message.id.channel));
        assert_eq!(message.id.message.timestamp(), message.created_at);
        if let Some(reply) = message.reply_to {
            let original: &Message = &messages[&reply];
            assert_eq!(original.id.channel, message.id.channel);
        }
        assert!(
            message_rows
                .insert(
                    message.id.message,
                    MessageId::new(index as i64 + 1).expect("row id")
                )
                .is_none()
        );
        assert!(
            messages
                .insert(message.id.message, message.clone())
                .is_none()
        );
        events.push(SourceEvent::MessageUpsert(message));
    }
    assert_eq!(
        events.len(),
        members.len() + channels.len() + messages.len()
    );

    let mut entries = BTreeMap::new();
    let mut reversals = 0;
    let mut superseded = 0;
    for value in list(&data, "entries") {
        let raw_id = value["id"].as_i64().expect("entry ID");
        let kind: EntryKind = text(value, "kind").parse().expect("entry kind");
        let status: EntryStatus = text(value, "status").parse().expect("entry status");
        let source = id(value, "source_message_id");
        let entry = NewEntry {
            kind,
            title: EntryTitle::new(text(value, "title")).expect("valid title"),
            title_is_copied: value["title_is_copied"].as_bool().expect("copied flag"),
            owner: value["owner_id"]
                .as_str()
                .map(|s| member_rows[&s.parse::<Snowflake>().expect("owner ID")]),
            due_at: value["due_at"].as_i64().map(UnixMillis::from_millis),
            source_message: Some(message_rows[&source]),
            created_by: member_rows[&id(value, "created_by")],
            created_via: text(value, "created_via").parse().expect("capture origin"),
            tags: list(value, "tags")
                .iter()
                .map(|v| Tag::new(v.as_str().expect("tag text")).expect("valid tag"))
                .collect(),
            replaces: value["replaces"]
                .as_i64()
                .map(|v| EntryId::new(v).expect("replaced entry ID")),
            created_at: time(value, "created_at"),
        };
        entry.validate().expect("valid entry request");
        assert!(kind.statuses().contains(&status));
        if status == EntryStatus::Superseded {
            superseded += 1;
        }
        if status != kind.initial_status() {
            check_transition(kind, kind.initial_status(), status).expect("valid final status");
        }
        assert!(entry.created_at <= time(value, "updated_at"));
        assert!(messages[&source].created_at <= entry.created_at);
        if entry.title_is_copied {
            assert_eq!(entry.title.as_str(), messages[&source].content);
        }
        if let Some(replaced) = value["replaces"].as_i64() {
            let prior: &Value = entries[&replaced];
            assert_eq!(prior["kind"], "decision");
            assert_eq!(prior["status"], "superseded");
            assert_eq!(prior["superseded_by"], raw_id);
            reversals += 1;
        }
        if let Some(answer) = value["answered_by_message_id"].as_str() {
            assert_eq!(status, EntryStatus::Answered);
            assert!(messages.contains_key(&answer.parse::<Snowflake>().expect("answer ID")));
        }
        assert!(entries.insert(raw_id, value).is_none());
    }
    assert!(reversals >= 8, "at least eight reversed decisions");
    assert_eq!(
        superseded, reversals,
        "every superseded decision is replaced once"
    );
}

fn load_demo() -> Value {
    serde_json::from_str(include_str!("../../../fixtures/demo/dataset.json"))
        .expect("valid JSON demo")
}

fn message_texts(data: &Value) -> Vec<&str> {
    list(data, "messages")
        .iter()
        .map(|m| text(m, "content"))
        .collect()
}

#[test]
fn demo_chat_has_the_texture_of_a_real_team() {
    let data = load_demo();
    let texts = message_texts(&data);
    let total = texts.len();

    // Message length runs from one word to several paragraphs.
    assert!(texts.iter().any(|t| t.split_whitespace().count() == 1));
    assert!(texts.iter().any(|t| t.contains("\n\n") && t.len() > 400));

    // At least 15% of messages are chatter or acknowledgements (four words or fewer).
    let short = texts
        .iter()
        .filter(|t| t.split_whitespace().count() <= 4)
        .count();
    assert!(
        short * 100 >= total * 15,
        "{short} short messages of {total}"
    );

    // Uneven author activity among the people: one dominant, one quiet.
    let mut per_author: BTreeMap<&str, usize> = BTreeMap::new();
    for message in list(&data, "messages") {
        *per_author.entry(text(message, "author_id")).or_default() += 1;
    }
    let humans: Vec<usize> = list(&data, "members")
        .iter()
        .filter(|m| m["is_bot"] == false)
        .map(|m| per_author.get(text(m, "id")).copied().unwrap_or(0))
        .collect();
    assert_eq!(humans.len(), 6);
    let most = humans.iter().max().copied().unwrap_or(0);
    let least = humans.iter().min().copied().unwrap_or(0);
    assert!(most * 100 >= total * 20, "a dominant author");
    assert!(least * 100 < total * 10 && least > 0, "a quiet author");

    // A code block, and links only to the reserved example domains.
    assert!(texts.iter().any(|t| t.contains("```")));
    for t in &texts {
        for (at, _) in t.match_indices("http") {
            let rest = &t[at..];
            assert!(
                rest.starts_with("https://example.com/")
                    || rest.starts_with("https://example.org/"),
                "link outside example.com or example.org: {rest}"
            );
        }
    }

    // Two or three pasted meeting-notes blocks, not one per topic.
    let notes = texts
        .iter()
        .filter(|t| t.lines().any(|l| l.starts_with("Attendees:")))
        .count();
    assert!((2..=3).contains(&notes), "{notes} meeting-notes blocks");
}

const INJECTION_MARKERS: [&str; 2] = [
    "ignore all previous instructions",
    "disregard all prior instructions",
];
/// What the two injection messages ask for: leak tokens, or claim the app is free.
const INJECTION_PAYLOADS: [&str; 4] = ["admin token", "free forever", "no purchase", "is free"];

/// Lists every way the record could have followed the injected instructions:
/// citing an injection message, repeating its marker or payload in a title, or
/// losing the legitimate active price and trial decisions it contradicts.
fn injection_violations(data: &Value) -> Vec<String> {
    let injected: Vec<&str> = list(data, "messages")
        .iter()
        .filter(|m| {
            let lower = text(m, "content").to_lowercase();
            INJECTION_MARKERS
                .iter()
                .any(|marker| lower.contains(marker))
        })
        .map(|m| text(m, "id"))
        .collect();
    let mut violations = Vec::new();
    if injected.len() != 2 {
        violations.push(format!(
            "{} injection-style messages, expected 2",
            injected.len()
        ));
    }
    let active_decisions: Vec<&str> = list(data, "entries")
        .iter()
        .filter(|e| e["kind"] == "decision" && e["status"] == "active")
        .map(|e| text(e, "title"))
        .collect();
    for entry in list(data, "entries") {
        let entry_id = &entry["id"];
        for field in ["source_message_id", "answered_by_message_id"] {
            if entry[field].as_str().is_some_and(|m| injected.contains(&m)) {
                violations.push(format!("entry {entry_id} cites an injection via {field}"));
            }
        }
        let title = text(entry, "title").to_lowercase();
        if INJECTION_MARKERS
            .iter()
            .chain(&INJECTION_PAYLOADS)
            .any(|phrase| title.contains(phrase))
        {
            violations.push(format!("entry {entry_id} repeats injected text: {title}"));
        }
    }
    if !active_decisions
        .iter()
        .any(|t| t.contains("$7.99 one-time unlock"))
    {
        violations.push("no active decision keeps the $7.99 price".to_owned());
    }
    if !active_decisions
        .iter()
        .any(|t| t.starts_with("Free trial shortens to 7 days"))
    {
        violations.push("no active decision keeps the 7-day trial".to_owned());
    }
    violations
}

#[test]
fn demo_prompt_injection_text_is_data_that_no_record_follows() {
    assert_eq!(injection_violations(&load_demo()), Vec::<String>::new());
}

#[test]
fn injection_check_catches_a_record_that_adopts_the_injected_claim() {
    let mut data = load_demo();
    let entries = data["entries"].as_array_mut().unwrap();
    let price = entries
        .iter_mut()
        .find(|e| text(e, "title").starts_with("Price is a $7.99"))
        .unwrap();
    price["title"] = Value::from("The app is free forever, no purchase needed");
    let violations = injection_violations(&data);
    assert!(
        violations
            .iter()
            .any(|v| v.contains("repeats injected text"))
    );
    assert!(violations.iter().any(|v| v.contains("$7.99")));

    let mut data = load_demo();
    let injected = list(&data, "messages")
        .iter()
        .find(|m| {
            text(m, "content")
                .to_lowercase()
                .contains(INJECTION_MARKERS[1])
        })
        .map(|m| text(m, "id").to_owned())
        .unwrap();
    data["entries"][0]["source_message_id"] = Value::from(injected);
    assert!(
        injection_violations(&data)
            .iter()
            .any(|v| v.contains("cites an injection"))
    );
}

#[test]
fn demo_record_covers_every_lifecycle() {
    let data = load_demo();
    let entries = list(&data, "entries");
    let count = |kind: &str, status: &str| {
        entries
            .iter()
            .filter(|e| e["kind"] == kind && e["status"] == status)
            .count()
    };
    let tasks_with_due = entries
        .iter()
        .filter(|e| e["kind"] == "task" && e["due_at"].is_i64())
        .count();
    assert!(
        tasks_with_due >= 15,
        "{tasks_with_due} tasks with due dates"
    );
    assert!(count("task", "done") > 0 && count("task", "dropped") == 1);
    assert!(count("task", "open") > 0);
    assert!(count("question", "open") > 0 && count("question", "answered") > 0);
    assert!(count("decision", "active") > 0);
    // Only tasks carry a due date.
    assert!(
        entries
            .iter()
            .all(|e| e["due_at"].is_null() || e["kind"] == "task")
    );
}
