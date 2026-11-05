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
    assert_eq!(list(&data, "messages").len(), 300);
    assert_eq!(list(&data, "entries").len(), 120);

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
    assert_eq!(channels.values().filter(|c| c.parent.is_some()).count(), 6);

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
    assert_eq!(events.len(), 316);
    assert!(
        messages
            .values()
            .any(|m| m.content.starts_with("Meeting notes\n"))
    );

    let mut entries = BTreeMap::new();
    let mut reversals = 0;
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
    assert_eq!(reversals, 30);
}
