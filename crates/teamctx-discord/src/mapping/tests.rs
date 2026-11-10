//! Mapping tests against the recorded-shape fixtures in `fixtures/discord/`.
//!
//! One test per event type, plus a test that every fixture file is covered, so adding a
//! fixture without a test fails.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use teamctx_core::id::Snowflake;
use teamctx_core::source::{
    Channel, ChannelError, ChannelKind, Member, Message, MessagePatch, MessageRef, SourceEvent,
};
use teamctx_core::time::UnixMillis;

use super::{
    Ignored, MapError, Mapped, map_dispatch, map_gateway_payload, map_rest_channels,
    map_rest_messages, map_thread_list,
};

const GENERAL: u64 = 1_100_000_000_000_000_101;
const FORUM: u64 = 1_100_000_000_000_000_103;
const ROBIN: u64 = 1_100_000_000_000_000_201;
const SAM: u64 = 1_100_000_000_000_000_202;
const KIT: u64 = 1_100_000_000_000_000_203;
const THREAD: u64 = 1_100_000_000_000_000_501;
const FORUM_POST: u64 = 1_100_000_000_000_000_502;

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).unwrap()
}

fn message_ref(message: u64) -> MessageRef {
    MessageRef {
        channel: sf(GENERAL),
        message: sf(message),
    }
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/discord")
}

fn load(relative: &str) -> Value {
    let text = fs::read_to_string(fixtures_dir().join(relative)).unwrap();
    serde_json::from_str(&text).unwrap()
}

/// Maps a gateway fixture and expects events.
fn gateway_events(name: &str) -> Vec<SourceEvent> {
    match map_gateway_payload(&load(&format!("gateway/{name}.json"))).unwrap() {
        Mapped::Events(events) => events,
        Mapped::Ignored(reason) => panic!("{name} was ignored: {reason:?}"),
    }
}

/// Maps a gateway fixture and expects exactly one event.
fn gateway_event(name: &str) -> SourceEvent {
    let mut events = gateway_events(name);
    assert_eq!(events.len(), 1, "{name} should map to one event");
    events.remove(0)
}

fn gateway_ignored(name: &str) -> Ignored {
    match map_gateway_payload(&load(&format!("gateway/{name}.json"))).unwrap() {
        Mapped::Ignored(reason) => reason,
        Mapped::Events(events) => panic!("{name} should be ignored, got {events:?}"),
    }
}

fn upserted_message(name: &str) -> Message {
    match gateway_event(name) {
        SourceEvent::MessageUpsert(message) => message,
        other => panic!("expected MessageUpsert, got {other:?}"),
    }
}

fn upserted_channel(name: &str) -> Channel {
    match gateway_event(name) {
        SourceEvent::ChannelUpsert(channel) => channel,
        other => panic!("expected ChannelUpsert, got {other:?}"),
    }
}

fn channel_upserts(events: Vec<SourceEvent>) -> Vec<Channel> {
    events
        .into_iter()
        .map(|event| match event {
            SourceEvent::ChannelUpsert(channel) => channel,
            other => panic!("expected ChannelUpsert, got {other:?}"),
        })
        .collect()
}

#[test]
fn message_create_maps_text_mentions_attachments_and_author() {
    let message = upserted_message("message_create");
    assert_eq!(message.id, message_ref(1_100_000_000_000_001_001));
    assert_eq!(
        message.author,
        Some(Member {
            id: sf(ROBIN),
            // The server nickname wins over the global display name.
            display_name: "Robin (release lead)".to_owned(),
            is_bot: false,
        })
    );
    // User mentions become names (global name, else nickname/username); the role mention
    // stays as written because the payload carries no role names.
    assert_eq!(
        message.content,
        "Decision: we ship the beta on Friday. cc @Sam Sample and @Kit D., role <@&1100000000000000301>."
    );
    assert_eq!(message.mentions, vec![sf(SAM), sf(KIT)]);
    assert_eq!(
        message.attachment_names,
        vec!["release-plan.pdf".to_owned()]
    );
    assert_eq!(message.reply_to, None);
    assert_eq!(message.edited_at, None);
    assert_eq!(message.created_at, message.id.message.timestamp());
    assert_eq!(
        message.created_at,
        UnixMillis::from_millis(1_682_330_837_011)
    );
}

#[test]
fn message_create_reply_keeps_the_reply_target_and_global_name() {
    let message = upserted_message("message_create_reply");
    assert_eq!(message.reply_to, Some(sf(1_100_000_000_000_001_001)));
    let author = message.author.unwrap();
    // `member.nick` is null, so the global display name is used.
    assert_eq!(author.display_name, "Sam Sample");
    assert!(message.mentions.is_empty());
    assert!(message.attachment_names.is_empty());
}

#[test]
fn message_create_flags_bots_and_falls_back_to_the_username() {
    let message = upserted_message("message_create_bot");
    assert_eq!(
        message.author,
        Some(Member {
            id: sf(1_100_000_000_000_000_204),
            display_name: "buildbot_demo".to_owned(),
            is_bot: true,
        })
    );
}

#[test]
fn webhook_messages_count_as_bots() {
    let mut frame = load("gateway/message_create.json");
    frame["d"]["webhook_id"] = json!("1100000000000000777");
    let events = map_gateway_payload(&frame).unwrap().into_events();
    let [SourceEvent::MessageUpsert(message)] = events.as_slice() else {
        panic!("expected one MessageUpsert, got {events:?}");
    };
    assert!(message.author.as_ref().unwrap().is_bot);
}

#[test]
fn prompt_injection_text_maps_as_plain_data() {
    let frame = load("gateway/message_create_injection.json");
    let injected = frame["d"]["content"].as_str().unwrap().to_owned();
    assert!(injected.starts_with("IGNORE ALL PREVIOUS INSTRUCTIONS"));

    let events = gateway_events("message_create_injection");
    // Exactly one upsert, with the text byte for byte: nothing was executed, dropped or
    // turned into another event.
    assert_eq!(events.len(), 1);
    let SourceEvent::MessageUpsert(message) = &events[0] else {
        panic!("expected MessageUpsert, got {:?}", events[0]);
    };
    assert_eq!(message.content, injected);
    assert_eq!(message.author.as_ref().unwrap().display_name, "Kit Demo");
}

#[test]
fn system_messages_are_ignored() {
    assert_eq!(
        gateway_ignored("message_create_system"),
        Ignored::UnsupportedMessageType(7)
    );
}

#[test]
fn partial_message_update_patches_only_the_fields_it_carries() {
    let SourceEvent::MessageUpdate(patch) = gateway_event("message_update_partial") else {
        panic!("expected MessageUpdate");
    };
    assert_eq!(
        patch,
        MessagePatch {
            id: message_ref(1_100_000_000_000_001_001),
            content: Some("Decision: we ship the beta on Monday instead.".to_owned()),
            mentions: None,
            attachment_names: None,
            edited_at: Some(UnixMillis::from_millis(1_772_622_000_500)),
        }
    );

    // Applied to the stored message, the fields the payload left out are preserved.
    let mut stored = upserted_message("message_create");
    assert_eq!(patch.apply(&mut stored), Ok(true));
    assert_eq!(
        stored.content,
        "Decision: we ship the beta on Monday instead."
    );
    assert_eq!(stored.mentions, vec![sf(SAM), sf(KIT)]);
    assert_eq!(stored.attachment_names, vec!["release-plan.pdf".to_owned()]);
    assert_eq!(
        stored.edited_at,
        Some(UnixMillis::from_millis(1_772_622_000_500))
    );
}

#[test]
fn full_message_update_replaces_content_mentions_and_attachments() {
    let SourceEvent::MessageUpdate(patch) = gateway_event("message_update_full") else {
        panic!("expected MessageUpdate");
    };
    assert_eq!(
        patch.content.as_deref(),
        Some("Decision: we ship the beta on Monday. cc @Sam Sample.")
    );
    assert_eq!(patch.mentions, Some(vec![sf(SAM)]));
    // The update lists no attachments, which clears them: an empty list is not "absent".
    assert_eq!(patch.attachment_names, Some(Vec::new()));
    assert_eq!(
        patch.edited_at,
        Some(UnixMillis::from_millis(1_772_623_800_000))
    );
}

#[test]
fn update_that_changes_nothing_we_store_is_ignored() {
    assert_eq!(
        gateway_ignored("message_update_embed_only"),
        Ignored::EmptyUpdate
    );
}

#[test]
fn message_delete_maps_to_a_message_ref() {
    assert_eq!(
        gateway_event("message_delete"),
        SourceEvent::MessageDelete {
            message: message_ref(1_100_000_000_000_001_002)
        }
    );
}

#[test]
fn message_delete_bulk_keeps_every_id_in_order() {
    assert_eq!(
        gateway_event("message_delete_bulk"),
        SourceEvent::MessageBulkDelete {
            channel: sf(GENERAL),
            ids: vec![sf(1_100_000_000_000_001_003), sf(1_100_000_000_000_001_004)],
        }
    );
}

#[test]
fn channel_create_maps_a_text_channel_without_its_category_as_parent() {
    assert_eq!(
        upserted_channel("channel_create"),
        Channel {
            id: sf(GENERAL),
            parent: None,
            kind: ChannelKind::Text,
            name: "general".to_owned(),
            topic: Some("Team chat and decisions".to_owned()),
            included: true,
            archived: false,
        }
    );
}

#[test]
fn channel_create_maps_announcement_and_forum_parents() {
    let announcement = upserted_channel("channel_create_announcement");
    assert_eq!(announcement.kind, ChannelKind::Announcement);
    assert_eq!(announcement.topic, None);

    let forum = upserted_channel("channel_create_forum");
    assert_eq!(forum.id, sf(FORUM));
    assert_eq!(forum.kind, ChannelKind::Forum);
    assert_eq!(forum.parent, None);
}

#[test]
fn channel_create_ignores_voice_channels() {
    assert_eq!(
        gateway_ignored("channel_create_voice"),
        Ignored::UnsupportedChannelType(2)
    );
}

#[test]
fn channel_update_maps_the_new_name_and_topic() {
    let channel = upserted_channel("channel_update");
    assert_eq!(channel.name, "team-general");
    assert_eq!(
        channel.topic.as_deref(),
        Some("Team chat, decisions and questions")
    );
}

#[test]
fn channel_delete_maps_to_the_channel_id() {
    assert_eq!(
        gateway_event("channel_delete"),
        SourceEvent::ChannelDelete {
            channel: sf(1_100_000_000_000_000_102)
        }
    );
}

#[test]
fn thread_create_maps_a_thread_with_its_parent() {
    assert_eq!(
        upserted_channel("thread_create"),
        Channel {
            id: sf(THREAD),
            parent: Some(sf(GENERAL)),
            kind: ChannelKind::Thread,
            name: "Beta checklist".to_owned(),
            topic: None,
            included: true,
            archived: false,
        }
    );
}

#[test]
fn thread_create_in_a_forum_maps_a_forum_post() {
    let post = upserted_channel("thread_create_forum_post");
    assert_eq!(post.id, sf(FORUM_POST));
    assert_eq!(post.kind, ChannelKind::ForumPost);
    assert_eq!(post.parent, Some(sf(FORUM)));
    assert_eq!(post.name, "Idea: weekly digest");
}

#[test]
fn thread_update_carries_the_archived_flag() {
    let thread = upserted_channel("thread_update");
    assert_eq!(thread.name, "Beta checklist (done)");
    assert!(thread.archived);
}

#[test]
fn thread_delete_maps_to_the_thread_id() {
    assert_eq!(
        gateway_event("thread_delete"),
        SourceEvent::ChannelDelete {
            channel: sf(THREAD)
        }
    );
}

#[test]
fn thread_list_sync_upserts_every_thread() {
    let threads = channel_upserts(gateway_events("thread_list_sync"));
    let summary: Vec<_> = threads
        .iter()
        .map(|thread| {
            (
                thread.id.get(),
                thread.kind,
                thread.parent.map(Snowflake::get),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            (THREAD, ChannelKind::Thread, Some(GENERAL)),
            (FORUM_POST, ChannelKind::ForumPost, Some(FORUM)),
            (
                1_100_000_000_000_000_503,
                ChannelKind::Thread,
                Some(GENERAL)
            ),
        ]
    );
}

#[test]
fn guild_create_discovers_channels_forums_and_threads() {
    let channels = channel_upserts(gateway_events("guild_create"));
    let summary: Vec<_> = channels
        .iter()
        .map(|channel| (channel.id.get(), channel.kind))
        .collect();
    // The category and the voice channel are skipped.
    assert_eq!(
        summary,
        [
            (GENERAL, ChannelKind::Text),
            (FORUM, ChannelKind::Forum),
            (THREAD, ChannelKind::Thread),
        ]
    );
}

#[test]
fn unknown_event_types_are_ignored_with_their_name() {
    assert_eq!(
        gateway_ignored("unknown_event"),
        Ignored::UnknownEventType("TYPING_START".to_owned())
    );
    // Reactions are not mapped until reaction capture exists (plan section 12).
    assert_eq!(
        gateway_ignored("message_reaction_add"),
        Ignored::UnknownEventType("MESSAGE_REACTION_ADD".to_owned())
    );
    assert_eq!(
        map_dispatch("SOMETHING_NEW", &Value::Null),
        Ok(Mapped::Ignored(Ignored::UnknownEventType(
            "SOMETHING_NEW".to_owned()
        )))
    );
}

#[test]
fn rest_message_page_maps_in_order_and_skips_system_messages() {
    let events = map_rest_messages(&load("rest/messages_page.json"))
        .unwrap()
        .into_events();
    let ids: Vec<_> = events
        .iter()
        .map(|event| match event {
            SourceEvent::MessageUpsert(message) => message.id.message.get(),
            other => panic!("expected MessageUpsert, got {other:?}"),
        })
        .collect();
    assert_eq!(ids, [1_100_000_000_000_001_002, 1_100_000_000_000_001_001]);

    let SourceEvent::MessageUpsert(decision) = &events[1] else {
        panic!("expected MessageUpsert");
    };
    assert_eq!(
        decision.edited_at,
        Some(UnixMillis::from_millis(1_772_618_520_000))
    );
}

#[test]
fn rest_archived_threads_map_as_threads_and_forum_posts() {
    let threads = channel_upserts(
        map_thread_list(&load("rest/archived_threads.json"))
            .unwrap()
            .into_events(),
    );
    assert_eq!(threads.len(), 2);
    assert!(threads.iter().all(|thread| thread.archived));
    assert_eq!(threads[0].kind, ChannelKind::Thread);
    // An empty `applied_tags` list still marks a forum post.
    assert_eq!(threads[1].kind, ChannelKind::ForumPost);
}

#[test]
fn rest_channel_list_maps_text_and_forum_and_skips_the_rest() {
    let channels = channel_upserts(
        map_rest_channels(&load("rest/channels.json"))
            .unwrap()
            .into_events(),
    );
    let kinds: Vec<_> = channels.iter().map(|channel| channel.kind).collect();
    assert_eq!(kinds, [ChannelKind::Text, ChannelKind::Forum]);
}

#[test]
fn attachment_urls_and_contents_never_reach_the_events() {
    let mut all = Vec::new();
    all.extend(gateway_events("message_create"));
    all.extend(
        map_rest_messages(&load("rest/messages_page.json"))
            .unwrap()
            .into_events(),
    );
    let text = format!("{all:?}");
    assert!(text.contains("release-plan.pdf"));
    assert!(!text.contains("http"));
    assert!(!text.contains("example.invalid"));
}

#[test]
fn malformed_payloads_are_errors_not_panics() {
    let cases: [(&str, Value); 9] = [
        ("MESSAGE_CREATE", Value::Null),
        ("MESSAGE_CREATE", json!({})),
        (
            "MESSAGE_CREATE",
            json!({"id": "0", "channel_id": "1", "author": {}}),
        ),
        (
            "MESSAGE_CREATE",
            json!({"id": "1", "channel_id": "2", "author": 5}),
        ),
        ("MESSAGE_UPDATE", json!({"id": "x", "channel_id": "1"})),
        ("MESSAGE_DELETE", json!({"id": "1"})),
        (
            "MESSAGE_DELETE_BULK",
            json!({"channel_id": "1", "ids": [1, "a"]}),
        ),
        ("CHANNEL_CREATE", json!([1, 2, 3])),
        ("THREAD_LIST_SYNC", json!({"threads": 4})),
    ];
    for (event, data) in cases {
        let result = map_dispatch(event, &data);
        assert!(
            matches!(result, Err(MapError::Malformed { .. })),
            "{event} {data}: {result:?}"
        );
    }
}

#[test]
fn a_thread_without_a_parent_is_an_invalid_channel() {
    let mut frame = load("gateway/thread_create.json");
    frame["d"].as_object_mut().unwrap().remove("parent_id");
    assert_eq!(
        map_gateway_payload(&frame),
        Err(MapError::InvalidChannel(ChannelError::MissingParent(
            ChannelKind::Thread
        )))
    );
}

#[test]
fn gateway_frames_without_type_or_data_are_errors() {
    assert!(matches!(
        map_gateway_payload(&json!({"d": {}})),
        Err(MapError::Malformed { .. })
    ));
    assert!(matches!(
        map_gateway_payload(&json!({"t": "MESSAGE_CREATE"})),
        Err(MapError::Malformed { .. })
    ));
    assert!(map_gateway_payload(&Value::Null).is_err());
}

/// Every fixture file with the number of events it must map to (`None`: ignored).
const GATEWAY_FIXTURES: &[(&str, Option<usize>)] = &[
    ("channel_create", Some(1)),
    ("channel_create_announcement", Some(1)),
    ("channel_create_forum", Some(1)),
    ("channel_create_voice", None),
    ("channel_delete", Some(1)),
    ("channel_update", Some(1)),
    ("guild_create", Some(3)),
    ("message_create", Some(1)),
    ("message_create_bot", Some(1)),
    ("message_create_injection", Some(1)),
    ("message_create_reply", Some(1)),
    ("message_create_system", None),
    ("message_delete", Some(1)),
    ("message_delete_bulk", Some(1)),
    ("message_reaction_add", None),
    ("message_update_embed_only", None),
    ("message_update_full", Some(1)),
    ("message_update_partial", Some(1)),
    ("thread_create", Some(1)),
    ("thread_create_forum_post", Some(1)),
    ("thread_delete", Some(1)),
    ("thread_list_sync", Some(3)),
    ("thread_update", Some(1)),
    ("unknown_event", None),
];

/// REST fixtures, covered by the `rest_*` tests above.
const REST_FIXTURES: &[&str] = &["archived_threads", "channels", "messages_page"];

fn json_files(dir: &str) -> BTreeSet<String> {
    fs::read_dir(fixtures_dir().join(dir))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
        .collect()
}

#[test]
fn every_fixture_file_is_covered() {
    let gateway: BTreeSet<_> = GATEWAY_FIXTURES
        .iter()
        .map(|(n, _)| (*n).to_owned())
        .collect();
    assert_eq!(
        json_files("gateway"),
        gateway,
        "gateway fixtures and GATEWAY_FIXTURES differ"
    );
    let rest: BTreeSet<_> = REST_FIXTURES.iter().map(|n| (*n).to_owned()).collect();
    assert_eq!(
        json_files("rest"),
        rest,
        "REST fixtures and REST_FIXTURES differ"
    );

    for (name, expected) in GATEWAY_FIXTURES {
        let frame = load(&format!("gateway/{name}.json"));
        match (map_gateway_payload(&frame).unwrap(), expected) {
            (Mapped::Events(events), Some(count)) => {
                assert_eq!(events.len(), *count, "{name}");
            }
            (Mapped::Ignored(_), None) => {}
            (other, expected) => panic!("{name}: got {other:?}, expected {expected:?}"),
        }
    }
}

#[test]
fn fixtures_contain_only_fictional_ids() {
    // Every id in the fixtures is in the reserved fictional range used by this dataset.
    fn walk(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::String(text) if text.len() >= 17 && text.bytes().all(|b| b.is_ascii_digit()) => {
                found.push(text.clone());
            }
            Value::Array(items) => items.iter().for_each(|item| walk(item, found)),
            Value::Object(map) => map.values().for_each(|item| walk(item, found)),
            _ => {}
        }
    }
    for dir in ["gateway", "rest"] {
        for name in json_files(dir) {
            let mut ids = Vec::new();
            walk(&load(&format!("{dir}/{name}.json")), &mut ids);
            assert!(!ids.is_empty() || name == "unknown_event", "{dir}/{name}");
            for id in ids {
                assert!(id.starts_with("11000000000000"), "{dir}/{name}: {id}");
            }
        }
    }
}
