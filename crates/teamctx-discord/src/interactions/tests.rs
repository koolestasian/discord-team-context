//! Handler tests: payload JSON in, response JSON out, against an in-memory store.
//!
//! Covers plan section 7.3 (every command, ephemeral by default, `public:true`, embeds
//! of at most 10 items with jump links), section 8 (lifecycle, short ids, dates in the
//! host's time zone, audit log with the actor), US-3 to US-6, retries on the same
//! interaction id, hostile chat text and per-member channel access. Every id, name and
//! message here is fictional.

#![expect(
    clippy::needless_pass_by_value,
    reason = "test helpers take json! literals"
)]

use std::sync::Arc;

use serde_json::{Value, json};
use teamctx_core::id::{DISCORD_EPOCH_MS, Snowflake, SourceId};
use teamctx_core::record::{CreatedVia, EntryKind, EntryStatus, RecordAction, ShortId};
use teamctx_core::repo::{
    DirectoryRead, EntryFilter, IngestWrite, MessageRead, PageRequest, RecordRead,
};
use teamctx_core::source::{
    Channel, ChannelKind, Member, Message, MessageRef, SourceEvent, SourceKind,
};
use teamctx_core::time::UnixMillis;
use teamctx_core::when::FixedOffset;
use teamctx_store::Store;

use super::access::AccessSnapshot;
use super::handler::{CATCHUP_MAX_MS, Context, handle, parse_link};
use super::payload::parse;
use super::render::{MAX_CONTENT, MAX_DESCRIPTION, MAX_EMBEDS_TOTAL, discord_len, embed_len};
use super::*;
use crate::connector::{GatewayState, StatusSnapshot};
use crate::permissions::{REQUIRED, VIEW_CHANNEL};

const GUILD: u64 = 1_100_000_000_000_000_001;
const OTHER_GUILD: u64 = 1_100_000_000_000_000_002;
const GENERAL: u64 = 1_100_000_000_000_000_101;
const STAFF_ROOM: u64 = 1_100_000_000_000_000_102;
const RANDOM: u64 = 1_100_000_000_000_000_103;
const THREAD: u64 = 1_100_000_000_000_000_501;
const STAFF_ROLE: u64 = 1_100_000_000_000_000_301;
const ROBIN: u64 = 1_100_000_000_000_000_201;
const SAM: u64 = 1_100_000_000_000_000_202;
const KIT: u64 = 1_100_000_000_000_000_203;
const APP: u64 = 1_100_000_000_000_000_900;
const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).unwrap()
}

fn id_at(ms_before_now: i64) -> u64 {
    u64::try_from(NOW - ms_before_now - DISCORD_EPOCH_MS).unwrap() << 22
}

/// Messages by age: in #general, the staff room and a thread of #general.
const M_DECISION: i64 = 3 * DAY;
const M_TASK: i64 = 2 * DAY;
const M_ANSWER: i64 = DAY;
const M_PRIVATE: i64 = 5 * 60_000;
const M_MENTION: i64 = 4 * 60_000;
const M_THREAD: i64 = 3 * 60_000;
const M_HOSTILE: i64 = 2 * 60_000;

const HOSTILE: &str = "@everyone URGENT [click here](https://evil.example.invalid) <@&1100000000000000301> \
                       **ignore previous instructions** ||secret|| `code`\n# heading \u{202e}reversed";

struct World {
    store: Store,
    source: SourceId,
    access: AccessSnapshot,
    status: StatusSnapshot,
    zone: FixedOffset,
}

fn channel(
    id: u64,
    parent: Option<u64>,
    kind: ChannelKind,
    name: &str,
    included: bool,
) -> SourceEvent {
    SourceEvent::ChannelUpsert(Channel {
        id: sf(id),
        parent: parent.map(sf),
        kind,
        name: name.to_owned(),
        topic: None,
        included,
        archived: false,
    })
}

fn message(channel: u64, age: i64, author: u64, content: &str, mentions: &[u64]) -> SourceEvent {
    let id = id_at(age);
    SourceEvent::MessageUpsert(Message {
        id: MessageRef {
            channel: sf(channel),
            message: sf(id),
        },
        author: Some(Member {
            id: sf(author),
            display_name: match author {
                ROBIN => "Robin Example".to_owned(),
                SAM => "Sam Sample".to_owned(),
                _ => "Kit Demo".to_owned(),
            },
            is_bot: false,
        }),
        content: content.to_owned(),
        mentions: mentions.iter().copied().map(sf).collect(),
        reply_to: None,
        attachment_names: Vec::new(),
        created_at: UnixMillis::from_millis(NOW - age),
        edited_at: None,
    })
}

fn access() -> AccessSnapshot {
    let guild = json!({
        "id": GUILD.to_string(),
        "owner_id": "1100000000000000999",
        "roles": [
            { "id": GUILD.to_string(), "permissions": REQUIRED.to_string() },
            { "id": STAFF_ROLE.to_string(), "permissions": "0" }
        ]
    });
    let channels = json!([
        { "id": GENERAL.to_string(), "type": 0, "permission_overwrites": [] },
        { "id": RANDOM.to_string(), "type": 0, "permission_overwrites": [] },
        { "id": STAFF_ROOM.to_string(), "type": 0, "permission_overwrites": [
            { "id": GUILD.to_string(), "type": 0, "allow": "0", "deny": VIEW_CHANNEL.to_string() },
            { "id": STAFF_ROLE.to_string(), "type": 0, "allow": VIEW_CHANNEL.to_string(), "deny": "0" }
        ] }
    ]);
    AccessSnapshot::from_payloads(sf(GUILD), &guild, &channels).unwrap()
}

fn world() -> World {
    let mut store = Store::in_memory().unwrap();
    store.set_date_resolver(Arc::new(|date| Ok(date.start_of_day_utc())));
    let source = store
        .upsert_source(SourceKind::Discord, sf(GUILD), "Example Team")
        .unwrap();
    let now = UnixMillis::from_millis(NOW);
    for event in [
        channel(GENERAL, None, ChannelKind::Text, "general", true),
        channel(STAFF_ROOM, None, ChannelKind::Text, "staff-room", true),
        channel(RANDOM, None, ChannelKind::Text, "random", false),
        channel(
            THREAD,
            Some(GENERAL),
            ChannelKind::Thread,
            "beta-checklist",
            true,
        ),
        message(
            GENERAL,
            M_DECISION,
            ROBIN,
            "We ship the beta on Friday.",
            &[],
        ),
        message(
            GENERAL,
            M_TASK,
            SAM,
            "Someone should write the release notes.",
            &[],
        ),
        message(GENERAL, M_ANSWER, KIT, "Yes, the beta includes sync.", &[]),
        message(
            STAFF_ROOM,
            M_PRIVATE,
            KIT,
            "Salary bands for the beta testers (staff only).",
            &[ROBIN],
        ),
        message(
            GENERAL,
            M_MENTION,
            SAM,
            "Robin, can you review the beta notes?",
            &[ROBIN],
        ),
        message(
            THREAD,
            M_THREAD,
            SAM,
            "Beta checklist item: signing keys.",
            &[],
        ),
        message(GENERAL, M_HOSTILE, KIT, HOSTILE, &[ROBIN]),
    ] {
        store.apply_event(source, &event, now).unwrap();
    }
    World {
        store,
        source,
        access: access(),
        status: StatusSnapshot {
            gateway: GatewayState::Connected,
            source: Some(source),
            ..StatusSnapshot::default()
        },
        zone: FixedOffset::from_seconds(2 * 3600).unwrap(),
    }
}

/// An interaction payload as Discord sends it.
fn interaction(id: u64, user: u64, roles: &[u64], kind: u64, data: Value) -> Value {
    json!({
        "id": id.to_string(),
        "application_id": APP.to_string(),
        "type": kind,
        "token": "fake-interaction-token.for_tests-only",
        "guild_id": GUILD.to_string(),
        "channel_id": GENERAL.to_string(),
        "member": {
            "user": { "id": user.to_string(), "username": format!("user{}", user % 1000), "global_name": null },
            "nick": match user { ROBIN => json!("Robin Example"), SAM => json!("Sam Sample"), _ => Value::Null },
            "roles": roles.iter().map(ToString::to_string).collect::<Vec<_>>()
        },
        "data": data
    })
}

fn slash(name: &str, options: Value) -> Value {
    json!({ "id": "1100000000000000950", "name": name, "type": 1, "options": options })
}

fn sub(name: &str, sub: &str, options: Value) -> Value {
    slash(
        name,
        json!([{ "name": sub, "type": 1, "options": options }]),
    )
}

fn user_option(name: &str, user: u64) -> (Value, Value) {
    (
        json!({ "name": name, "type": 6, "value": user.to_string() }),
        json!({ "users": { user.to_string(): { "id": user.to_string(), "username": format!("user{}", user % 1000), "global_name": "Sam Sample" } } }),
    )
}

fn mark(kind: &str, channel: u64, age: i64, content: &str) -> Value {
    let id = id_at(age).to_string();
    json!({
        "id": "1100000000000000951", "name": kind, "type": 3, "target_id": id,
        "resolved": { "messages": { id.clone(): {
            "id": id, "channel_id": channel.to_string(), "type": 0, "content": content,
            "author": { "id": ROBIN.to_string(), "username": "robin_example", "global_name": "Robin Example" },
            "mentions": [], "attachments": [], "timestamp": "2027-01-15T08:00:00.000000+00:00"
        }}}
    })
}

impl World {
    fn run(&mut self, payload: &Value) -> Outcome {
        let interaction = parse(payload).unwrap();
        let ctx = Context {
            guild: sf(GUILD),
            source: Some(self.source),
            now: UnixMillis::from_millis(NOW),
            zone: self.zone,
            access: Some(&self.access),
            status: &self.status,
        };
        handle(&mut self.store, &ctx, &interaction)
    }

    /// The response JSON Discord would receive.
    fn respond(&mut self, payload: &Value) -> Value {
        serde_json::to_value(self.run(payload).response()).unwrap()
    }

    fn entries(&self, kind: EntryKind) -> Vec<teamctx_core::record::RecordEntry> {
        self.store
            .list_entries(
                &EntryFilter {
                    kind: Some(kind),
                    ..EntryFilter::default()
                },
                PageRequest {
                    offset: 0,
                    limit: 100,
                },
            )
            .unwrap()
    }

    fn member(&self, user: u64) -> teamctx_core::id::MemberId {
        self.store
            .member_by_external(self.source, sf(user))
            .unwrap()
            .unwrap()
            .id
    }
}

/// All text of a response: content, embed titles, descriptions and footers.
fn text(response: &Value) -> String {
    let data = &response["data"];
    let mut out = data["content"].as_str().unwrap_or_default().to_owned();
    for embed in data["embeds"].as_array().into_iter().flatten() {
        for key in ["title", "description"] {
            out.push('\n');
            out.push_str(embed[key].as_str().unwrap_or_default());
        }
        out.push('\n');
        out.push_str(embed["footer"]["text"].as_str().unwrap_or_default());
    }
    out
}

fn assert_ephemeral_and_silent(response: &Value) {
    assert_eq!(response["type"], 4);
    assert_eq!(response["data"]["flags"], 64, "{response}");
    assert_eq!(response["data"]["allowed_mentions"], json!({ "parse": [] }));
}

fn link(channel: u64, age: i64) -> String {
    format!(
        "https://discord.com/channels/{GUILD}/{channel}/{}",
        id_at(age)
    )
}

// ---- US-3: mark a message ----

#[test]
fn us3_mark_as_decision_cites_the_message_and_answers_only_me() {
    let mut w = world();
    let payload = interaction(
        7001,
        ROBIN,
        &[],
        2,
        mark("Mark as decision", GENERAL, M_DECISION, "ignored"),
    );
    let response = w.respond(&payload);
    assert_ephemeral_and_silent(&response);
    let shown = text(&response);
    assert!(shown.starts_with("Recorded as a decision: **D-"), "{shown}");
    assert!(shown.contains("We ship the beta on Friday."), "{shown}");
    assert!(
        shown.contains(&format!("[jump]({})", link(GENERAL, M_DECISION))),
        "{shown}"
    );

    let decisions = w.entries(EntryKind::Decision);
    assert_eq!(decisions.len(), 1);
    let entry = &decisions[0];
    assert_eq!(entry.created_via, CreatedVia::ContextMenu);
    assert_eq!(entry.created_by, w.member(ROBIN));
    assert!(entry.title_is_copied);
    let source = w
        .store
        .message(entry.source_message.unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(source.external_id.get(), id_at(M_DECISION));
    let events = w.store.entry_events(entry.id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].action, RecordAction::Created);
    assert_eq!(events[0].actor, Some(w.member(ROBIN)));

    // A second click (or a retried interaction) creates nothing new.
    let again = text(&w.respond(&interaction(
        7002,
        SAM,
        &[],
        2,
        mark("Mark as decision", GENERAL, M_DECISION, "x"),
    )));
    assert!(again.starts_with("Already recorded"), "{again}");
    assert_eq!(w.entries(EntryKind::Decision).len(), 1);
}

#[test]
fn marking_stores_a_message_the_host_has_not_seen_yet() {
    let mut w = world();
    let age = 30_000;
    let shown = text(&w.respond(&interaction(
        7003,
        ROBIN,
        &[],
        2,
        mark(
            "Mark as question",
            GENERAL,
            age,
            "Who owns the store listing?",
        ),
    )));
    assert!(shown.starts_with("Recorded as a question: **Q-"), "{shown}");
    let stored = w
        .store
        .message_by_ref(
            w.source,
            MessageRef {
                channel: sf(GENERAL),
                message: sf(id_at(age)),
            },
        )
        .unwrap();
    assert!(stored.is_some());
}

#[test]
fn excluded_unknown_and_textless_messages_are_refused() {
    let mut w = world();
    for (channel, content, expected) in [
        (RANDOM, "Lunch?", "excluded from the team record"),
        (
            1_100_000_000_000_000_199,
            "New channel",
            "not in the record yet",
        ),
        (GENERAL, "", "no text to copy"),
    ] {
        let shown = text(&w.respond(&interaction(
            7004,
            ROBIN,
            &[],
            2,
            mark("Mark as decision", channel, 10_000, content),
        )));
        assert!(shown.contains(expected), "{shown}");
    }
    assert!(w.entries(EntryKind::Decision).is_empty());
}

#[test]
fn mark_as_task_opens_a_modal_and_its_submission_sets_owner_and_due() {
    let mut w = world();
    let response = w.respond(&interaction(
        7010,
        ROBIN,
        &[],
        2,
        mark("Mark as task", GENERAL, M_TASK, "x"),
    ));
    assert_eq!(response["type"], 9);
    let custom_id = response["data"]["custom_id"].as_str().unwrap().to_owned();
    assert_eq!(custom_id, format!("task:{GENERAL}:{}", id_at(M_TASK)));
    assert!(discord_len(response["data"]["title"].as_str().unwrap()) <= 45);
    let components = response["data"]["components"].as_array().unwrap();
    assert_eq!(components.len(), 2);
    assert_eq!(components[0]["type"], 18, "Label");
    assert_eq!(components[0]["component"]["type"], 5, "user select");
    assert_eq!(components[1]["component"]["type"], 4, "text input");
    assert!(components[1]["component"].get("label").is_none());
    assert!(
        w.entries(EntryKind::Task).is_empty(),
        "nothing is created before submit"
    );

    let (_, resolved) = user_option("owner", SAM);
    let submit = |due: &str| {
        json!({
            "custom_id": custom_id,
            "components": [
                { "type": 18, "id": 1, "component": { "type": 5, "id": 2, "custom_id": "owner", "values": [SAM.to_string()] } },
                { "type": 18, "id": 3, "component": { "type": 4, "id": 4, "custom_id": "due", "value": due } }
            ],
            "resolved": resolved
        })
    };
    // An ambiguous date is refused before anything is written.
    let shown = text(&w.respond(&interaction(7011, ROBIN, &[], 5, submit("11/20"))));
    assert!(shown.contains("day first or month first"), "{shown}");
    assert!(!shown.contains("11/20"), "input is not echoed: {shown}");
    assert!(w.entries(EntryKind::Task).is_empty());

    let shown = text(&w.respond(&interaction(7012, ROBIN, &[], 5, submit("2027-02-01"))));
    assert!(shown.starts_with("Recorded as a task: **T-"), "{shown}");
    assert!(shown.contains("Due date: 2027-02-01"), "{shown}");
    let task = &w.entries(EntryKind::Task)[0];
    assert_eq!(task.owner, Some(w.member(SAM)));
    // Midnight at +02:00.
    let expected = "2027-02-01"
        .parse::<teamctx_core::time::CalendarDate>()
        .unwrap();
    assert_eq!(
        task.due_at.unwrap().as_millis(),
        expected.start_of_day_utc().as_millis() - 2 * 3_600_000
    );
    let actions: Vec<&str> = w
        .store
        .entry_events(task.id)
        .unwrap()
        .iter()
        .map(|e| e.action.action())
        .collect();
    assert_eq!(actions, ["created", "owner", "due"]);

    // Submitting the same values again changes nothing and logs nothing.
    let shown = text(&w.respond(&interaction(7013, ROBIN, &[], 5, submit("2027-02-01"))));
    assert!(shown.starts_with("Updated the task"), "{shown}");
    assert_eq!(w.store.entry_events(task.id).unwrap().len(), 3);
}

#[test]
fn a_task_modal_for_a_channel_i_cannot_read_is_refused() {
    let mut w = world();
    let data = json!({
        "custom_id": format!("task:{STAFF_ROOM}:{}", id_at(M_PRIVATE)),
        "components": []
    });
    let shown = text(&w.respond(&interaction(7014, SAM, &[], 5, data.clone())));
    assert!(shown.contains("cannot find that message"), "{shown}");
    assert!(w.entries(EntryKind::Task).is_empty());
    let shown = text(&w.respond(&interaction(7015, SAM, &[STAFF_ROLE], 5, data)));
    assert!(shown.starts_with("Recorded as a task"), "{shown}");
}

// ---- typed entries and retries ----

#[test]
fn decide_with_replaces_supersedes_and_retries_are_safe() {
    let mut w = world();
    let first = text(&w.respond(&interaction(
        7020,
        ROBIN,
        &[],
        2,
        slash(
            "decide",
            json!([{ "name": "text", "type": 3, "value": "  Use   SQLite\nfor storage " }]),
        ),
    )));
    assert!(first.starts_with("Recorded: **D-"), "{first}");
    let old = w.entries(EntryKind::Decision)[0].short_id();
    assert_eq!(
        w.entries(EntryKind::Decision)[0].title.as_str(),
        "Use SQLite for storage"
    );

    let replace = interaction(
        7021,
        SAM,
        &[],
        2,
        slash(
            "decide",
            json!([
                { "name": "text", "type": 3, "value": "Use SQLite with WAL" },
                { "name": "replaces", "type": 3, "value": old.to_string().to_lowercase() }
            ]),
        ),
    );
    let shown = text(&w.respond(&replace));
    assert!(shown.contains(&format!("it replaces {old}")), "{shown}");
    let decisions = w.entries(EntryKind::Decision);
    assert_eq!(decisions.len(), 2);
    let old_entry = w.store.entry(old).unwrap().unwrap();
    assert_eq!(old_entry.status, EntryStatus::Superseded);

    // Discord retrying the same interaction id: same entry, nothing new.
    let retry = text(&w.respond(&replace));
    assert!(retry.starts_with("Already recorded"), "{retry}");
    assert_eq!(w.entries(EntryKind::Decision).len(), 2);

    // Wrong kind, unknown id, malformed id.
    for (value, expected) in [
        ("T-1", "Use a decision id like D-12."),
        ("D-999", "not found"),
        ("D-01", "positive number"),
    ] {
        let payload = interaction(
            7022,
            ROBIN,
            &[],
            2,
            slash(
                "decide",
                json!([
                    { "name": "text", "type": 3, "value": "x" },
                    { "name": "replaces", "type": 3, "value": value }
                ]),
            ),
        );
        let shown = text(&w.respond(&payload));
        assert!(shown.contains(expected), "{value}: {shown}");
    }
    // A title over 200 characters is refused with the limit.
    let long = "x".repeat(201);
    let shown = text(&w.respond(&interaction(
        7023,
        ROBIN,
        &[],
        2,
        slash(
            "decide",
            json!([{ "name": "text", "type": 3, "value": long }]),
        ),
    )));
    assert!(shown.contains("the limit is 200"), "{shown}");
}

// ---- US-4: decisions on a topic ----

#[test]
fn us4_decisions_list_current_ones_with_author_date_and_link() {
    let mut w = world();
    w.respond(&interaction(
        7030,
        ROBIN,
        &[],
        2,
        mark("Mark as decision", GENERAL, M_DECISION, "x"),
    ));
    w.respond(&interaction(
        7031,
        KIT,
        &[STAFF_ROLE],
        2,
        mark("Mark as decision", STAFF_ROOM, M_PRIVATE, "x"),
    ));
    let old = text(&w.respond(&interaction(
        7032,
        ROBIN,
        &[],
        2,
        slash(
            "decide",
            json!([{ "name": "text", "type": 3, "value": "Beta ships Thursday" }]),
        ),
    )));
    assert!(old.contains("D-"));
    let thursday = w
        .entries(EntryKind::Decision)
        .into_iter()
        .find(|e| e.title.as_str() == "Beta ships Thursday")
        .unwrap();
    w.respond(&interaction(
        7033,
        ROBIN,
        &[],
        2,
        slash(
            "decide",
            json!([
                { "name": "text", "type": 3, "value": "Beta ships Monday" },
                { "name": "replaces", "type": 3, "value": thursday.short_id().to_string() }
            ]),
        ),
    ));

    let list = |w: &mut World, user, roles: &[u64], options: Value| {
        let response = w.respond(&interaction(
            7034,
            user,
            roles,
            2,
            slash("decisions", options),
        ));
        (response.clone(), text(&response))
    };
    let (response, shown) = list(
        &mut w,
        SAM,
        &[],
        json!([{ "name": "topic", "type": 3, "value": "beta" }]),
    );
    assert_ephemeral_and_silent(&response);
    assert!(shown.contains("Current decisions"));
    assert!(shown.contains("We ship the beta on Friday."), "{shown}");
    assert!(
        shown.contains("Robin Example, <t:"),
        "author and date: {shown}"
    );
    assert!(shown.contains(&link(GENERAL, M_DECISION)), "{shown}");
    assert!(shown.contains("Beta ships Monday"));
    assert!(
        !shown.contains("Beta ships Thursday"),
        "superseded hidden: {shown}"
    );
    assert!(
        !shown.contains("Salary"),
        "private channel hidden from Sam: {shown}"
    );
    assert!(shown.contains("Some items are hidden"), "{shown}");

    let (_, shown) = list(
        &mut w,
        SAM,
        &[],
        json!([{ "name": "include_superseded", "type": 5, "value": true }]),
    );
    let thursday_line = shown
        .lines()
        .find(|l| l.contains("Beta ships Thursday"))
        .unwrap();
    assert!(thursday_line.ends_with("(superseded by D-4)"), "{shown}");

    let (_, shown) = list(&mut w, KIT, &[STAFF_ROLE], json!([]));
    assert!(
        shown.contains("Salary bands"),
        "staff sees the staff room: {shown}"
    );

    // A public reply shows only what everyone can read, even when staff asks.
    let (response, shown) = list(
        &mut w,
        KIT,
        &[STAFF_ROLE],
        json!([{ "name": "public", "type": 5, "value": true }]),
    );
    assert!(response["data"].get("flags").is_none(), "{response}");
    assert!(!shown.contains("Salary"), "{shown}");
}

// ---- US-6: tasks ----

#[test]
fn us6_todo_add_list_and_done_with_absolute_due_dates() {
    let mut w = world();
    let (owner, resolved) = user_option("owner", SAM);
    let mut add = sub(
        "todo",
        "add",
        json!([
            { "name": "text", "type": 3, "value": "Write the release notes" },
            owner,
            { "name": "due", "type": 3, "value": "tomorrow" }
        ]),
    );
    add["resolved"] = resolved.clone();
    let shown = text(&w.respond(&interaction(7040, ROBIN, &[], 2, add.clone())));
    assert!(shown.starts_with("Recorded: **T-"), "{shown}");
    // NOW is 2027-01-15T08:00Z, so 10:00 at +02:00; tomorrow is the 16th.
    assert!(shown.contains("Due date: 2027-01-16"), "{shown}");
    let task = w.entries(EntryKind::Task)[0].clone();
    assert_eq!(task.owner, Some(w.member(SAM)));
    assert_eq!(task.created_via, CreatedVia::Slash);
    assert_eq!(
        task.due_at.unwrap().as_millis(),
        (NOW / DAY + 1) * DAY - 2 * 3_600_000
    );

    // Same interaction id again: no second task.
    w.respond(&interaction(7040, ROBIN, &[], 2, add));
    assert_eq!(w.entries(EntryKind::Task).len(), 1);

    let (owner, resolved) = user_option("owner", SAM);
    let mut list = sub("todo", "list", json!([owner]));
    list["resolved"] = resolved;
    let shown = text(&w.respond(&interaction(7041, KIT, &[], 2, list)));
    assert!(shown.contains("Tasks (open)"));
    assert!(
        shown.contains("Write the release notes \u{2014} owner Sam Sample, due <t:"),
        "{shown}"
    );

    let short = task.short_id().to_string();
    let done = |id: &str| {
        interaction(
            7042,
            KIT,
            &[],
            2,
            sub(
                "todo",
                "done",
                json!([{ "name": "id", "type": 3, "value": id }]),
            ),
        )
    };
    let shown = text(&w.respond(&done(&short.to_lowercase())));
    assert!(shown.starts_with(&format!("Done: **{short}**")), "{shown}");
    let stored = w.store.entry(task.short_id()).unwrap().unwrap();
    assert_eq!(stored.status, EntryStatus::Done);
    let last = w.store.entry_events(stored.id).unwrap().pop().unwrap();
    assert_eq!(last.actor, Some(w.member(KIT)), "the closer is recorded");

    let shown = text(&w.respond(&done(&short)));
    assert!(shown.starts_with("Already done"), "{shown}");
    for (id, expected) in [
        ("D-1", "Use a task id like T-7."),
        ("T-999", "There is no task T-999."),
        ("x", "must start with D, T or Q"),
    ] {
        let shown = text(&w.respond(&done(id)));
        assert!(shown.contains(expected), "{id}: {shown}");
    }

    let open = text(&w.respond(&interaction(
        7043,
        KIT,
        &[],
        2,
        sub("todo", "list", json!([])),
    )));
    assert!(open.contains("No tasks match."), "{open}");
    let finished = text(&w.respond(&interaction(
        7044,
        KIT,
        &[],
        2,
        sub(
            "todo",
            "list",
            json!([{ "name": "status", "type": 3, "value": "done" }]),
        ),
    )));
    assert!(finished.contains("Write the release notes"), "{finished}");

    // Due dates in the past and unknown phrases are refused with examples.
    for (phrase, expected) in [
        ("2020-01-01", "in the past"),
        ("someday", "try tomorrow, friday"),
    ] {
        let payload = interaction(
            7045,
            ROBIN,
            &[],
            2,
            sub(
                "todo",
                "add",
                json!([
                    { "name": "text", "type": 3, "value": "x" },
                    { "name": "due", "type": 3, "value": phrase }
                ]),
            ),
        );
        let shown = text(&w.respond(&payload));
        assert!(shown.contains(expected), "{phrase}: {shown}");
    }
    assert_eq!(w.entries(EntryKind::Task).len(), 1);
}

// ---- questions ----

#[test]
fn question_and_answer_by_message_link() {
    let mut w = world();
    let shown = text(&w.respond(&interaction(
        7050,
        SAM,
        &[],
        2,
        slash(
            "question",
            json!([{ "name": "text", "type": 3, "value": "Does the beta include sync?" }]),
        ),
    )));
    assert!(shown.starts_with("Recorded: **Q-"), "{shown}");
    let question = w.entries(EntryKind::Question)[0].short_id().to_string();
    let answer = |w: &mut World, user, roles: &[u64], link: &str| {
        text(&w.respond(&interaction(
            7051,
            user,
            roles,
            2,
            slash(
                "answer",
                json!([
                    { "name": "id", "type": 3, "value": question },
                    { "name": "message_link", "type": 3, "value": link }
                ]),
            ),
        )))
    };
    for (bad, expected) in [
        ("not a link", "must be a Discord message link"),
        (
            "https://evil.example.invalid/channels/1/2/3",
            "must be a Discord message link",
        ),
        (
            &format!(
                "https://discord.com/channels/{OTHER_GUILD}/{GENERAL}/{}",
                id_at(M_ANSWER)
            ) as &str,
            "another server",
        ),
        (&link(GENERAL, 1) as &str, "cannot find that message"),
        (
            &link(STAFF_ROOM, M_PRIVATE) as &str,
            "cannot find that message",
        ),
        (&link(RANDOM, M_ANSWER) as &str, "cannot find that message"),
    ] {
        let shown = answer(&mut w, SAM, &[], bad);
        assert!(shown.contains(expected), "{bad}: {shown}");
    }
    let shown = answer(&mut w, SAM, &[], &link(GENERAL, M_ANSWER));
    assert!(shown.starts_with("Answered: **Q-"), "{shown}");
    assert!(shown.contains(&link(GENERAL, M_ANSWER)), "{shown}");
    let stored = w.entries(EntryKind::Question)[0].clone();
    assert_eq!(stored.status, EntryStatus::Answered);
    let shown = answer(&mut w, SAM, &[], &link(GENERAL, M_ANSWER));
    assert!(shown.starts_with("Already answered"), "{shown}");
    // Another message is refused, not a silent change of the answer.
    let shown = answer(&mut w, SAM, &[], &link(GENERAL, M_MENTION));
    assert!(!shown.starts_with("Answered"), "{shown}");
    let after = w.entries(EntryKind::Question)[0].clone();
    assert_eq!(after.answered_by_message, stored.answered_by_message);
}

#[test]
fn jump_links_parse_strictly() {
    for good in [
        "https://discord.com/channels/1/2/3",
        " https://ptb.discord.com/channels/1/2/3 ",
        "https://canary.discord.com/channels/1/2/3",
        "https://discordapp.com/channels/1/2/3",
    ] {
        assert!(parse_link(good).is_some(), "{good}");
    }
    for bad in [
        "http://discord.com/channels/1/2/3",
        "https://discord.com.evil.example/channels/1/2/3",
        "https://discord.com/channels/1/2",
        "https://discord.com/channels/1/2/3/4",
        "https://discord.com/channels/@me/2/3",
        "https://discord.com/channels/01/2/3",
        "https://user@discord.com/channels/1/2/3",
    ] {
        assert!(parse_link(bad).is_none(), "{bad}");
    }
}

// ---- US-5: catch-up ----

#[test]
fn us5_catchup_shows_changes_channels_and_mentions_since_last_time() {
    let mut w = world();
    w.respond(&interaction(
        7060,
        SAM,
        &[],
        2,
        mark("Mark as decision", GENERAL, M_DECISION, "x"),
    ));
    let response = w.respond(&interaction(
        7061,
        ROBIN,
        &[],
        2,
        slash("catchup", json!([])),
    ));
    assert_ephemeral_and_silent(&response);
    let shown = text(&response);
    let floor = (NOW - CATCHUP_MAX_MS) / 1000;
    assert!(
        shown.starts_with(&format!("Since <t:{floor}:D>")),
        "first catch-up covers 14 days: {shown}"
    );
    assert!(
        shown.contains("We ship the beta on Friday."),
        "record change: {shown}"
    );
    assert!(
        shown.contains(&format!("<#{GENERAL}> \u{2014} ")),
        "busiest channel: {shown}"
    );
    assert!(
        shown.contains("Robin, can you review the beta notes?"),
        "mention: {shown}"
    );
    assert!(!shown.contains("Salary"), "private mention hidden: {shown}");
    assert!(!shown.contains(&format!("<#{STAFF_ROOM}>")), "{shown}");
    assert!(!shown.contains("@everyone"), "{shown}");
    let robin = w.member(ROBIN);
    assert_eq!(
        w.store.last_catchup(robin).unwrap(),
        Some(UnixMillis::from_millis(NOW))
    );

    // The next catch-up starts at the last one: nothing new.
    let shown = text(&w.respond(&interaction(
        7062,
        ROBIN,
        &[],
        2,
        slash("catchup", json!([])),
    )));
    assert!(
        shown.starts_with(&format!("Since <t:{}:D>", NOW / 1000)),
        "{shown}"
    );
    assert!(shown.contains("Nobody mentioned you."), "{shown}");

    // An explicit window longer than 14 days is capped and says so; bad input is refused.
    let shown = text(&w.respond(&interaction(
        7063,
        ROBIN,
        &[],
        2,
        slash(
            "catchup",
            json!([{ "name": "since", "type": 3, "value": "30d" }]),
        ),
    )));
    assert!(shown.contains("at most 14 days"), "{shown}");
    let shown = text(&w.respond(&interaction(
        7064,
        ROBIN,
        &[],
        2,
        slash(
            "catchup",
            json!([{ "name": "since", "type": 3, "value": "next week" }]),
        ),
    )));
    assert!(shown.contains("Not a time I understand"), "{shown}");
}

// ---- search ----

#[test]
fn search_is_ranked_filtered_and_escaped() {
    let mut w = world();
    let search = |w: &mut World, user, roles: &[u64], options: Value| {
        let response = w.respond(&interaction(7070, user, roles, 2, slash("search", options)));
        (response.clone(), text(&response))
    };
    let (response, shown) = search(
        &mut w,
        SAM,
        &[],
        json!([{ "name": "query", "type": 3, "value": "beta" }]),
    );
    assert_ephemeral_and_silent(&response);
    assert!(shown.contains("We ship the beta on Friday."), "{shown}");
    assert!(
        shown.contains("**#beta\\-checklist**"),
        "thread result through its parent: {shown}"
    );
    assert!(!shown.contains("Salary"), "{shown}");
    let (_, shown) = search(
        &mut w,
        KIT,
        &[STAFF_ROLE],
        json!([{ "name": "query", "type": 3, "value": "beta" }]),
    );
    assert!(shown.contains("Salary bands"), "{shown}");

    let (_, shown) = search(
        &mut w,
        SAM,
        &[],
        json!([
            { "name": "query", "type": 3, "value": "beta" },
            { "name": "channel", "type": 7, "value": THREAD.to_string() }
        ]),
    );
    assert!(
        shown.contains("signing keys") && !shown.contains("Friday"),
        "{shown}"
    );
    let (_, shown) = search(
        &mut w,
        SAM,
        &[],
        json!([
            { "name": "query", "type": 3, "value": "beta" },
            { "name": "channel", "type": 7, "value": STAFF_ROOM.to_string() }
        ]),
    );
    assert!(
        shown.contains("not in the record"),
        "a channel I cannot read: {shown}"
    );

    let (author, resolved) = user_option("author", SAM);
    let mut data = slash(
        "search",
        json!([{ "name": "query", "type": 3, "value": "beta" }, author]),
    );
    data["resolved"] = resolved;
    let shown = text(&w.respond(&interaction(7071, KIT, &[], 2, data)));
    assert!(
        shown.contains("review the beta notes") && !shown.contains("Friday"),
        "{shown}"
    );

    for (query, expected) in [
        ("from:robin", "filters alone do not search"),
        ("\"unclosed", "closing quote"),
        ("after:2027-13-01 beta", "must look like 2026-04-01"),
    ] {
        let (_, shown) = search(
            &mut w,
            SAM,
            &[],
            json!([{ "name": "query", "type": 3, "value": query }]),
        );
        assert!(shown.contains(expected), "{query}: {shown}");
    }
}

#[test]
fn hostile_chat_text_cannot_ping_format_or_link() {
    let mut w = world();
    w.respond(&interaction(
        7080,
        ROBIN,
        &[],
        2,
        mark("Mark as decision", GENERAL, M_HOSTILE, "x"),
    ));
    for payload in [
        interaction(
            7081,
            ROBIN,
            &[],
            2,
            slash(
                "search",
                json!([{ "name": "query", "type": 3, "value": "urgent" }]),
            ),
        ),
        interaction(7082, ROBIN, &[], 2, slash("decisions", json!([]))),
        interaction(7083, ROBIN, &[], 2, slash("catchup", json!([]))),
    ] {
        let response = w.respond(&payload);
        assert_ephemeral_and_silent(&response);
        let shown = text(&response);
        assert!(shown.contains("URGENT"), "{shown}");
        assert!(!shown.contains("@everyone"), "{shown}");
        assert!(!shown.contains("<@&"), "{shown}");
        assert!(!shown.contains("](https://evil"), "masked link: {shown}");
        assert!(
            shown.contains("\\[click here\\]\\(https://evil.example.invalid\\)"),
            "{shown}"
        );
        assert!(!shown.contains("**ignore"), "{shown}");
        assert!(!shown.contains("||secret"), "{shown}");
        assert!(!shown.contains('\u{202e}'), "{shown}");
        assert!(!shown.contains("\n# heading"), "{shown}");
    }
}

#[test]
fn long_text_stays_within_discord_limits() {
    let mut w = world();
    let now = UnixMillis::from_millis(NOW);
    for i in 0..30 {
        let content = format!("beta {} {}", "*_`|".repeat(400), "\u{1f600}".repeat(300));
        w.store
            .apply_event(
                w.source,
                &message(GENERAL, 90_000 + i * 1000, SAM, &content, &[ROBIN]),
                now,
            )
            .unwrap();
        w.respond(&interaction(
            7090 + u64::try_from(i).unwrap(),
            ROBIN,
            &[],
            2,
            mark("Mark as decision", GENERAL, 90_000 + i * 1000, "x"),
        ));
    }
    for payload in [
        interaction(
            7200,
            ROBIN,
            &[],
            2,
            slash(
                "search",
                json!([{ "name": "query", "type": 3, "value": "beta" }]),
            ),
        ),
        interaction(7201, ROBIN, &[], 2, slash("decisions", json!([]))),
        interaction(7202, ROBIN, &[], 2, slash("catchup", json!([]))),
    ] {
        let Outcome::Reply(reply) = w.run(&payload) else {
            panic!("reply")
        };
        let data = reply.data();
        let content = data.content.unwrap_or_default();
        assert!(discord_len(&content) <= MAX_CONTENT);
        let embeds = data.embeds.unwrap_or_default();
        assert!(embeds.len() <= render::MAX_EMBEDS);
        let total: usize = embeds.iter().map(embed_len).sum();
        assert!(total <= MAX_EMBEDS_TOTAL, "{total}");
        for embed in &embeds {
            let description = embed.description.as_deref().unwrap_or_default();
            assert!(discord_len(description) <= MAX_DESCRIPTION);
            let items = description.lines().filter(|l| l.starts_with("**")).count();
            assert!(items <= render::MAX_ITEMS, "{items} items");
        }
    }
}

// ---- who, where, and failure modes ----

#[test]
fn unknown_members_are_created_from_the_interaction() {
    let mut w = world();
    let newcomer = 1_100_000_000_000_000_260;
    let mut payload = interaction(
        7300,
        newcomer,
        &[],
        2,
        slash(
            "question",
            json!([{ "name": "text", "type": 3, "value": "Where are the docs?" }]),
        ),
    );
    payload["member"]["nick"] = json!("New \u{202e}Person");
    w.respond(&payload);
    let member = w
        .store
        .member_by_external(w.source, sf(newcomer))
        .unwrap()
        .unwrap();
    assert_eq!(member.display_name, "New \u{202e}Person", "stored as given");
    let shown = text(&w.respond(&interaction(
        7301,
        SAM,
        &[],
        2,
        slash(
            "decisions",
            json!([{ "name": "include_superseded", "type": 5, "value": true }]),
        ),
    )));
    assert!(!shown.contains('\u{202e}'));
    // A renamed member is updated, not duplicated.
    payload["id"] = json!("1100000000000007302");
    payload["member"]["nick"] = json!("Renamed");
    w.respond(&payload);
    let member = w
        .store
        .member_by_external(w.source, sf(newcomer))
        .unwrap()
        .unwrap();
    assert_eq!(member.display_name, "Renamed");
}

#[test]
fn other_servers_direct_messages_and_startup_get_fixed_notes() {
    let mut w = world();
    let mut elsewhere = interaction(7310, ROBIN, &[], 2, slash("decisions", json!([])));
    elsewhere["guild_id"] = json!(OTHER_GUILD.to_string());
    assert!(text(&w.respond(&elsewhere)).contains("only in the server"));

    let mut dm = interaction(7311, ROBIN, &[], 2, slash("decisions", json!([])));
    dm.as_object_mut().unwrap().remove("guild_id");
    let member = dm.as_object_mut().unwrap().remove("member").unwrap();
    dm["user"] = member["user"].clone();
    assert!(text(&w.respond(&dm)).contains("only in the server"));

    let unsupported = interaction(
        7312,
        ROBIN,
        &[],
        2,
        slash(
            "ask",
            json!([{ "name": "question", "type": 3, "value": "?" }]),
        ),
    );
    assert!(text(&w.respond(&unsupported)).contains("not available"));

    let parsed = parse(&interaction(
        7313,
        ROBIN,
        &[],
        2,
        slash("decisions", json!([])),
    ))
    .unwrap();
    let (access, status) = (w.access.clone(), w.status.clone());
    let ctx = Context {
        guild: sf(GUILD),
        source: None,
        now: UnixMillis::from_millis(NOW),
        zone: w.zone,
        access: Some(&access),
        status: &status,
    };
    let Outcome::Reply(reply) = handle(&mut w.store, &ctx, &parsed) else {
        panic!()
    };
    assert!(reply.content.contains("still connecting"));
}

#[test]
fn without_channel_access_nothing_chat_derived_is_shown() {
    let mut w = world();
    w.respond(&interaction(
        7320,
        ROBIN,
        &[],
        2,
        mark("Mark as decision", GENERAL, M_DECISION, "x"),
    ));
    w.respond(&interaction(
        7321,
        ROBIN,
        &[],
        2,
        slash(
            "decide",
            json!([{ "name": "text", "type": 3, "value": "Typed decision" }]),
        ),
    ));
    let status = w.status.clone();
    let ctx = Context {
        guild: sf(GUILD),
        source: Some(w.source),
        now: UnixMillis::from_millis(NOW),
        zone: w.zone,
        access: None,
        status: &status,
    };
    for (data, absent) in [
        (slash("decisions", json!([])), "We ship the beta"),
        (
            slash(
                "search",
                json!([{ "name": "query", "type": 3, "value": "beta" }]),
            ),
            "We ship the beta",
        ),
    ] {
        let parsed = parse(&interaction(7322, ROBIN, &[], 2, data)).unwrap();
        let response =
            serde_json::to_value(handle(&mut w.store, &ctx, &parsed).response()).unwrap();
        let shown = text(&response);
        assert!(!shown.contains(absent), "{shown}");
        assert!(shown.contains("could not check which channels"), "{shown}");
    }
    let parsed = parse(&interaction(
        7323,
        ROBIN,
        &[],
        2,
        slash("decisions", json!([])),
    ))
    .unwrap();
    let shown =
        text(&serde_json::to_value(handle(&mut w.store, &ctx, &parsed).response()).unwrap());
    assert!(
        shown.contains("Typed decision"),
        "slash-typed entries have no source channel: {shown}"
    );
}

#[test]
fn status_reports_fixed_text_only() {
    let mut w = world();
    w.status.problem = Some("Discord rejected the bot token; sync stopped");
    w.status.last_sync_finished_at = Some(UnixMillis::from_millis(NOW - 1000));
    w.status.last_sync_ok = Some(false);
    let response = w.respond(&interaction(
        7330,
        ROBIN,
        &[],
        2,
        sub("teamctx", "status", json!([])),
    ));
    assert_ephemeral_and_silent(&response);
    let shown = text(&response);
    assert!(shown.contains("Gateway: connected"), "{shown}");
    assert!(
        shown.contains("(failed)") && shown.contains("Problem: Discord rejected the bot token"),
        "{shown}"
    );
}

#[test]
fn deferred_and_public_responses_have_the_right_flags() {
    let deferred = serde_json::to_value(deferred(false)).unwrap();
    assert_eq!(deferred, json!({ "type": 5, "data": { "flags": 64 } }));
    let public = serde_json::to_value(super::deferred(true)).unwrap();
    assert_eq!(public, json!({ "type": 5, "data": {} }));
}

// ---- the demo dataset ----

#[test]
fn demo_record_answers_us4_us5_and_us6() {
    let dataset = include_str!("../../../../fixtures/demo/dataset.json");
    let mut store = Store::in_memory().unwrap();
    store.set_date_resolver(Arc::new(|date| Ok(date.start_of_day_utc())));
    let map = teamctx_store::demo::import(&mut store, dataset).unwrap();
    let guild = sf(1);
    let channels = json!(
        ["201", "202", "203", "204"]
            .map(|id| json!({ "id": id, "type": 0, "permission_overwrites": [] }))
    );
    let access = AccessSnapshot::from_payloads(
        guild,
        &json!({ "id": "1", "owner_id": "101", "roles": [{ "id": "1", "permissions": REQUIRED.to_string() }] }),
        &channels,
    )
    .unwrap();
    let status = StatusSnapshot {
        source: Some(map.source()),
        ..StatusSnapshot::default()
    };
    // A day after the last message of the demo.
    let now = UnixMillis::from_millis(1_777_046_588_140 + DAY);
    let ctx = Context {
        guild,
        source: Some(map.source()),
        now,
        zone: FixedOffset::UTC,
        access: Some(&access),
        status: &status,
    };
    let run = |store: &mut Store, data: Value| {
        let mut payload = interaction(7400, 102, &[], 2, data);
        payload["guild_id"] = json!("1");
        payload["member"]["user"]["global_name"] = json!("Dov Kestrel");
        let parsed = parse(&payload).unwrap();
        text(&serde_json::to_value(handle(store, &ctx, &parsed).response()).unwrap())
    };

    // US-4: onboarding decisions, the reversed one hidden unless asked for.
    let current = map
        .entry(ShortId {
            kind: EntryKind::Decision,
            id: teamctx_core::id::EntryId::new(16).unwrap(),
        })
        .unwrap();
    let shown = run(
        &mut store,
        slash(
            "decisions",
            json!([{ "name": "topic", "type": 3, "value": "onboarding" }]),
        ),
    );
    assert!(shown.contains(&format!("**{current}**")), "{shown}");
    assert!(shown.contains("Onboarding needs no account"), "{shown}");
    assert!(
        !shown.contains("create an account at first launch"),
        "{shown}"
    );
    assert!(shown.contains("https://discord.com/channels/1/"), "{shown}");
    let shown = run(
        &mut store,
        slash(
            "decisions",
            json!([
                { "name": "topic", "type": 3, "value": "onboarding" },
                { "name": "include_superseded", "type": 5, "value": true }
            ]),
        ),
    );
    assert!(
        shown.contains("create an account at first launch") && shown.contains("(superseded by D-"),
        "{shown}"
    );

    // US-6: finished tasks are listed with their owner.
    let shown = run(
        &mut store,
        sub(
            "todo",
            "list",
            json!([{ "name": "status", "type": 3, "value": "done" }]),
        ),
    );
    assert!(
        shown.contains("Tasks (done)") && shown.contains("**T-"),
        "{shown}"
    );

    // US-5: catch-up over the last two weeks shows activity, as Dov.
    let shown = run(
        &mut store,
        slash(
            "catchup",
            json!([{ "name": "since", "type": 3, "value": "14d" }]),
        ),
    );
    assert!(
        shown.contains("Most active channels") && shown.contains("<#20"),
        "{shown}"
    );

    // Search finds the font decision's discussion.
    let shown = run(
        &mut store,
        slash(
            "search",
            json!([{ "name": "query", "type": 3, "value": "Fenwick" }]),
        ),
    );
    assert!(
        shown.contains("Search results") && shown.contains("Fenwick"),
        "{shown}"
    );
}

// ---- fixtures ----

#[test]
fn interaction_fixtures_parse_and_hold_only_fictional_ids() {
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
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/discord/interactions");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "catchup.json",
            "decide_public.json",
            "mark_as_decision.json",
            "ready.json"
        ]
    );
    for name in names {
        let text = std::fs::read_to_string(dir.join(&name)).unwrap();
        let frame: Value = serde_json::from_str(&text).unwrap();
        let mut ids = Vec::new();
        walk(&frame, &mut ids);
        assert!(!ids.is_empty(), "{name}");
        for id in ids {
            assert!(id.starts_with("11000000000000"), "{name}: {id}");
        }
        if frame["t"] == "INTERACTION_CREATE" {
            let parsed = parse(&frame["d"]).unwrap();
            assert_ne!(parsed.request, payload::Request::Unsupported, "{name}");
            assert!(
                parsed.envelope.token.expose().starts_with("FAKE-"),
                "{name}"
            );
        }
    }
}
