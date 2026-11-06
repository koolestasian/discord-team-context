//! `MessageRead::channel_messages_since`: oldest-first keyset paging of one channel.
//! All data below is synthetic.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use teamctx_core::{
    id::{ChannelId, Snowflake, SourceId},
    repo::{DirectoryRead, IngestWrite, MessageCursor, MessageRead, StoredMessage},
    source::{Channel, ChannelKind, Member, Message, MessageRef, SourceEvent, SourceKind},
    time::UnixMillis,
};
use teamctx_store::Store;

fn sf(id: u64) -> Snowflake {
    Snowflake::new(id).unwrap()
}
fn at(time: i64) -> UnixMillis {
    UnixMillis::from_millis(time)
}
fn channel(id: u64, parent: Option<u64>, included: bool) -> Channel {
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
        included,
        archived: false,
    }
}
fn message(id: u64, room: u64, created: i64) -> Message {
    Message {
        id: MessageRef {
            channel: sf(room),
            message: sf(id),
        },
        author: Some(Member {
            id: sf(100),
            display_name: "Piper Fiction".to_owned(),
            is_bot: false,
        }),
        content: format!("message {id}"),
        reply_to: None,
        attachment_names: Vec::new(),
        created_at: at(created),
        edited_at: None,
        mentions: Vec::new(),
    }
}
fn setup() -> (Store, SourceId) {
    let mut db = Store::in_memory().unwrap();
    let source = db
        .upsert_source(SourceKind::Discord, sf(1), "Fiction Team")
        .unwrap();
    for event in [
        SourceEvent::ChannelUpsert(channel(10, None, true)),
        SourceEvent::ChannelUpsert(channel(11, Some(10), true)),
        SourceEvent::ChannelUpsert(channel(20, None, false)),
    ] {
        db.apply_event(source, &event, at(1)).unwrap();
    }
    (db, source)
}
fn add(db: &mut Store, source: SourceId, id: u64, room: u64, created: i64) {
    db.apply_event(
        source,
        &SourceEvent::MessageUpsert(message(id, room, created)),
        at(created),
    )
    .unwrap();
}
fn row(db: &Store, source: SourceId, room: u64) -> ChannelId {
    db.channels(source)
        .unwrap()
        .into_iter()
        .find(|info| info.external_id == sf(room))
        .unwrap()
        .id
}
fn ids(messages: &[StoredMessage]) -> Vec<u64> {
    messages.iter().map(|m| m.external_id.get()).collect()
}

#[test]
fn returns_oldest_first_with_numeric_id_ties_and_an_inclusive_since() {
    let (mut db, source) = setup();
    // Same creation time: 2 < 3 < 10 numerically (not as text).
    for (id, created) in [(10, 100), (3, 100), (2, 100), (7, 50), (8, 99), (9, 200)] {
        add(&mut db, source, id, 10, created);
    }
    let room = row(&db, source, 10);
    let all = db.channel_messages_since(room, at(0), 100, None).unwrap();
    assert_eq!(ids(&all), [7, 8, 2, 3, 10, 9]);
    let since = db.channel_messages_since(room, at(100), 100, None).unwrap();
    assert_eq!(ids(&since), [2, 3, 10, 9]);
    let limited = db.channel_messages_since(room, at(0), 2, None).unwrap();
    assert_eq!(ids(&limited), [7, 8]);
}

#[test]
fn a_cursor_continues_strictly_after_it_and_survives_deleting_the_cursor_message() {
    let (mut db, source) = setup();
    for id in 1..=7 {
        add(&mut db, source, id, 10, 100);
    }
    let room = row(&db, source, 10);
    let first = db.channel_messages_since(room, at(0), 3, None).unwrap();
    assert_eq!(ids(&first), [1, 2, 3]);
    let cursor = MessageCursor::from(first.last().unwrap());

    // The message the cursor points at disappears between pages (a deletion found by the
    // sync): the next page still starts right after its position.
    db.apply_event(
        source,
        &SourceEvent::MessageDelete {
            message: MessageRef {
                channel: sf(10),
                message: sf(3),
            },
        },
        at(200),
    )
    .unwrap();
    let second = db
        .channel_messages_since(room, at(0), 3, Some(cursor))
        .unwrap();
    assert_eq!(ids(&second), [4, 5, 6]);
    let third = db
        .channel_messages_since(
            room,
            at(0),
            3,
            Some(MessageCursor::from(second.last().unwrap())),
        )
        .unwrap();
    assert_eq!(ids(&third), [7]);
    let end = db
        .channel_messages_since(
            room,
            at(0),
            3,
            Some(MessageCursor::from(third.last().unwrap())),
        )
        .unwrap();
    assert!(end.is_empty());
}

#[test]
fn pages_through_more_than_five_thousand_messages_without_skips_or_repeats() {
    let (mut db, source) = setup();
    let total: u64 = 5_300;
    for id in 1..=total {
        add(&mut db, source, id, 10, i64::try_from(id).unwrap());
    }
    let room = row(&db, source, 10);
    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = db
            .channel_messages_since(room, at(0), 1_000, cursor)
            .unwrap();
        seen.extend(ids(&page));
        if page.len() < 1_000 {
            break;
        }
        cursor = page.last().map(MessageCursor::from);
    }
    assert_eq!(seen, (1..=total).collect::<Vec<_>>());
}

#[test]
fn a_thread_is_its_own_channel_and_excluded_or_unknown_channels_are_empty() {
    let (mut db, source) = setup();
    add(&mut db, source, 1, 10, 100);
    add(&mut db, source, 2, 11, 100);
    // The store drops messages of an excluded channel on ingest (and purges older ones on
    // exclusion), so the read has nothing to return for it.
    add(&mut db, source, 3, 20, 100);
    let parent = row(&db, source, 10);
    let thread = row(&db, source, 11);
    let excluded = row(&db, source, 20);
    assert_eq!(
        ids(&db.channel_messages_since(parent, at(0), 10, None).unwrap()),
        [1]
    );
    assert_eq!(
        ids(&db.channel_messages_since(thread, at(0), 10, None).unwrap()),
        [2]
    );
    assert!(
        db.channel_messages_since(excluded, at(0), 10, None)
            .unwrap()
            .is_empty()
    );
    let unknown = ChannelId::new(9_999).unwrap();
    assert!(
        db.channel_messages_since(unknown, at(0), 10, None)
            .unwrap()
            .is_empty()
    );
    assert!(
        db.channel_messages_since(parent, at(0), 0, None)
            .unwrap()
            .is_empty()
    );
}
