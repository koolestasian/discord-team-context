//! Paging and owner lookup of the `search` and `record` tools, on small synthetic datasets
//! built for the case (not the demo record). All names and text here are fictional.

#![expect(
    clippy::expect_used,
    reason = "clippy.toml exempts #[test] functions only; this file's helpers panic on failure too"
)]

#[expect(
    dead_code,
    reason = "this binary does not use every harness helper; auth_http.rs does"
)]
mod common;

use common::{TestDb, TestServer};
use reqwest::Client;
use serde_json::{Value, json};
use teamctx_core::{
    id::{MemberId, Snowflake, SourceId},
    record::{CreatedVia, EntryKind, EntryTitle, NewEntry},
    repo::*,
    source::{Channel, ChannelKind, Member, Message, MessageRef, SourceEvent, SourceKind},
    time::UnixMillis,
};
use teamctx_store::Store;

fn sf(id: u64) -> Snowflake {
    Snowflake::new(id).expect("non-zero snowflake")
}

fn at(millis: i64) -> UnixMillis {
    UnixMillis::from_millis(millis)
}

fn member(id: u64, name: &str) -> Member {
    Member {
        id: sf(id),
        display_name: name.to_owned(),
        is_bot: false,
    }
}

/// A source with one included channel (external id 10) and one member (external id 100).
fn seed_base(store: &mut Store) -> (SourceId, MemberId) {
    let source = store
        .upsert_source(SourceKind::Discord, sf(1), "Fiction Team")
        .expect("source");
    let channel = Channel {
        id: sf(10),
        parent: None,
        kind: ChannelKind::Text,
        name: "general".to_owned(),
        topic: None,
        included: true,
        archived: false,
    };
    store
        .apply_event(source, &SourceEvent::ChannelUpsert(channel), at(1))
        .expect("channel");
    store
        .apply_event(
            source,
            &SourceEvent::MemberUpsert(member(100, "Piper Fiction")),
            at(1),
        )
        .expect("member");
    let author = store
        .member_by_external(source, sf(100))
        .expect("lookup")
        .expect("author row")
        .id;
    (source, author)
}

fn add_entry(
    store: &mut Store,
    created_by: MemberId,
    owner: Option<MemberId>,
    title: &str,
    n: i64,
) {
    store
        .create_entry(&NewEntry {
            kind: EntryKind::Decision,
            title: EntryTitle::new(title).expect("title"),
            title_is_copied: false,
            source_message: None,
            owner,
            due_at: None,
            created_by,
            created_via: CreatedVia::Slash,
            tags: Vec::new(),
            replaces: None,
            created_at: at(n),
        })
        .expect("entry");
}

struct Harness {
    _db: TestDb,
    server: TestServer,
    token: String,
    client: Client,
}

async fn start(seed: impl FnOnce(&mut Store)) -> Harness {
    let db = TestDb::with_seed(seed);
    let token = db.create_token("paging-test");
    let server = common::start(&db).await;
    Harness {
        _db: db,
        server,
        token,
        client: Client::new(),
    }
}

async fn call(h: &Harness, name: &str, arguments: Value) -> Value {
    let response = h
        .client
        .post(&h.server.url)
        .bearer_auth(&h.token)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2025-11-25")
        .json(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        }))
        .send()
        .await
        .expect("send");
    let text = response.text().await.expect("body");
    let json_text = match text.lines().find_map(|l| l.strip_prefix("data:")) {
        Some(data) => data.trim().to_owned(),
        None => text,
    };
    let message: Value = serde_json::from_str(&json_text).expect("JSON-RPC message");
    assert!(message["error"].is_null(), "{name}: {message}");
    message["result"].clone()
}

fn ids_of(result: &Value) -> Vec<String> {
    result["structuredContent"]["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|item| item["id"].as_str().expect("id").to_owned())
        .collect()
}

fn next_cursor(result: &Value) -> Option<String> {
    result["structuredContent"]["next_cursor"]
        .as_str()
        .map(str::to_owned)
}

#[tokio::test]
async fn trimmed_record_pages_resume_after_the_last_returned_entry() {
    // Each title is 200 characters that the untrusted-data framing escapes to seven, so a
    // page of eight cannot fit the size budget and is trimmed.
    let title = "<".repeat(200);
    let h = start(|store| {
        let (_, author) = seed_base(store);
        for n in 1..=12 {
            add_entry(store, author, None, &title, n);
        }
    })
    .await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut trimmed_pages = 0;
    for _ in 0..30 {
        let args = match &cursor {
            Some(cursor) => json!({ "cursor": cursor }),
            None => json!({}),
        };
        let page = call(&h, "record", args).await;
        assert_ne!(page["isError"], true, "{page}");
        let ids = ids_of(&page);
        assert!(!ids.is_empty(), "a page must make progress");
        if ids.len() < 8 {
            trimmed_pages += 1;
        }
        seen.extend(ids);
        cursor = next_cursor(&page);
        if cursor.is_none() {
            break;
        }
    }
    assert!(trimmed_pages > 0, "the data must exercise size trimming");
    let mut expected: Vec<String> = (1..=12).map(|n| format!("D-{n}")).collect();
    let mut got = seen.clone();
    expected.sort();
    got.sort();
    assert_eq!(got, expected, "no entry skipped or repeated: {seen:?}");
}

#[tokio::test]
async fn search_never_offers_a_cursor_it_would_reject() {
    let h = start(|store| {
        let (source, _) = seed_base(store);
        for n in 1..=130_u64 {
            let message = Message {
                id: MessageRef {
                    channel: sf(10),
                    message: sf(1_000 + n),
                },
                author: Some(member(100, "Piper Fiction")),
                content: format!("quarterly roadmap sync note {n}"),
                mentions: Vec::new(),
                reply_to: None,
                attachment_names: Vec::new(),
                created_at: at(i64::try_from(n).expect("small") * 1_000),
                edited_at: None,
            };
            store
                .apply_event(source, &SourceEvent::MessageUpsert(message), at(1))
                .expect("message");
        }
    })
    .await;

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut last = Value::Null;
    for _ in 0..40 {
        let args = match &cursor {
            Some(cursor) => json!({ "query": "roadmap", "cursor": cursor }),
            None => json!({ "query": "roadmap" }),
        };
        let page = call(&h, "search", args).await;
        assert_ne!(
            page["isError"], true,
            "following the advertised cursor {cursor:?} failed: {page}"
        );
        seen.extend(ids_of(&page));
        cursor = next_cursor(&page);
        last = page;
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "paging must end");
    let mut unique = seen.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), seen.len(), "no repeats");
    assert!(seen.len() >= 100, "reached the cap: {}", seen.len());
    // More than the cap matched, so the last page says results were cut off.
    assert_eq!(last["structuredContent"]["truncated"], true);
    let text = last["content"][0]["text"].as_str().expect("text");
    assert!(text.contains("limit"), "{text}");
}

#[tokio::test]
async fn record_owner_lookup_finds_members_beyond_the_first_ids() {
    let h = start(|store| {
        let (source, author) = seed_base(store);
        for n in 0..600_u64 {
            store
                .apply_event(
                    source,
                    &SourceEvent::MemberUpsert(member(2_000 + n, &format!("Filler {n}"))),
                    at(1),
                )
                .expect("filler member");
        }
        store
            .apply_event(
                source,
                &SourceEvent::MemberUpsert(member(9_000, "Zed Lateman")),
                at(1),
            )
            .expect("late member");
        let late = store
            .member_by_external(source, sf(9_000))
            .expect("lookup")
            .expect("row")
            .id;
        assert!(late.get() > 500, "the owner must sit past the old scan");
        add_entry(store, author, Some(late), "Ship the fictional beta", 5);
    })
    .await;

    let owned = call(&h, "record", json!({ "owner": "zed LATEMAN" })).await;
    assert_ne!(owned["isError"], true, "{owned}");
    assert_eq!(ids_of(&owned), ["D-1"]);
    let nobody = call(&h, "record", json!({ "owner": "Nobody Atall" })).await;
    assert_eq!(nobody["isError"], true);
}
