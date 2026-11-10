//! Scoped questions (a Discord member sees only the channels they can read), catch-up
//! summaries and token askers, against a real in-memory store and a fake provider. All
//! people, channels and messages are fictional. No model is called.

#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use std::collections::HashSet;

use teamctx_core::citation::CitationId;
use teamctx_core::id::{ChannelId, MemberId, MessageId, Snowflake, SourceId, TokenId};
use teamctx_core::record::{CreatedVia, EntryKind, EntryTitle, NewEntry, ShortId};
use teamctx_core::repo::{DirectoryRead, IngestWrite, MessageRead, RecordWrite};
use teamctx_core::source::{
    Channel, ChannelKind, Member, Message, MessageRef, SourceEvent, SourceKind,
};
use teamctx_core::time::UnixMillis;
use teamctx_llm::{
    Answer, AskConfig, AskError, AskPipeline, AskRequest, Asker, Budget, BudgetLimit, FakeProvider,
    InMemoryLedger, Items, NotFoundReason, Scope, UsageLedger, without_urls,
};
use teamctx_store::Store;

const NOW: i64 = 1_777_000_000_000;
const MINUTE: i64 = 60_000;
const OPEN: u64 = 301;
const PRIVATE: u64 = 302;

fn sf(raw: u64) -> Snowflake {
    Snowflake::new(raw).unwrap()
}

fn at(millis: i64) -> UnixMillis {
    UnixMillis::from_millis(millis)
}

struct Fixture {
    store: Store,
    source: SourceId,
    open: ChannelId,
    private: ChannelId,
    member: MemberId,
}

impl Fixture {
    fn message(&self, channel: u64, id: u64) -> MessageId {
        self.store
            .message_by_ref(
                self.source,
                MessageRef {
                    channel: sf(channel),
                    message: sf(id),
                },
            )
            .unwrap()
            .unwrap()
            .id
    }

    fn open_only(&self) -> HashSet<ChannelId> {
        HashSet::from([self.open])
    }
}

fn setup() -> Fixture {
    let mut store = Store::in_memory().unwrap();
    let source = store
        .upsert_source(SourceKind::Discord, sf(1), "Quillmoor (fictional)")
        .unwrap();
    let ines = Member {
        id: sf(101),
        display_name: "Ines Varrow".to_owned(),
        is_bot: false,
    };
    let channel = |id, name: &str| Channel {
        id: sf(id),
        parent: None,
        kind: ChannelKind::Text,
        name: name.to_owned(),
        topic: None,
        included: true,
        archived: false,
    };
    let message = |channel, id, text: &str| Message {
        id: MessageRef {
            channel: sf(channel),
            message: sf(id),
        },
        author: Some(ines.clone()),
        content: text.to_owned(),
        mentions: Vec::new(),
        reply_to: None,
        attachment_names: Vec::new(),
        created_at: at(NOW - 100 * MINUTE + i64::try_from(id % 100).unwrap() * MINUTE),
        edited_at: None,
    };
    for event in [
        SourceEvent::ChannelUpsert(channel(OPEN, "general")),
        SourceEvent::ChannelUpsert(channel(PRIVATE, "hiring")),
        SourceEvent::MemberUpsert(ines.clone()),
        SourceEvent::MessageUpsert(message(OPEN, 1, "the kiln schedule moves to thursday")),
        SourceEvent::MessageUpsert(message(
            PRIVATE,
            2,
            "kiln budget: the confidential salary band is forty",
        )),
    ] {
        store.apply_event(source, &event, at(NOW)).unwrap();
    }
    let channels = store.channels(source).unwrap();
    let id_of = |external| {
        channels
            .iter()
            .find(|c| c.external_id == sf(external))
            .unwrap()
            .id
    };
    let member = store
        .member_by_external(source, sf(101))
        .unwrap()
        .unwrap()
        .id;
    let fixture = Fixture {
        open: id_of(OPEN),
        private: id_of(PRIVATE),
        store,
        source,
        member,
    };
    let mut fixture = fixture;
    // D-1 cites the private message with a typed title; D-2 is typed without a source.
    let private_message = fixture.message(PRIVATE, 2);
    for (title, source_message) in [
        ("Kiln hiring plan approved", Some(private_message)),
        ("Kiln firing moves to thursday", None),
    ] {
        fixture
            .store
            .create_entry(&NewEntry {
                kind: EntryKind::Decision,
                title: EntryTitle::new(title).unwrap(),
                title_is_copied: false,
                owner: None,
                due_at: None,
                source_message,
                created_by: member,
                created_via: CreatedVia::Slash,
                tags: Vec::new(),
                replaces: None,
                created_at: at(NOW - 50 * MINUTE),
            })
            .unwrap();
    }
    fixture
}

fn ask(question: &str, asker: Asker) -> AskRequest<'_> {
    AskRequest {
        asker,
        question,
        now: at(NOW),
    }
}

fn sent(pipe: &AskPipeline<FakeProvider>) -> String {
    pipe.provider()
        .requests()
        .iter()
        .map(|r| format!("{}\n{}", r.system, r.user))
        .collect()
}

#[tokio::test]
async fn a_scoped_question_never_sends_or_cites_a_hidden_channel() {
    let fx = setup();
    let ledger = InMemoryLedger::new();
    let hidden = CitationId::Message(fx.message(PRIVATE, 2));
    // The model tries to cite the private message and the entry that points at it.
    let pipe = AskPipeline::new(
        FakeProvider::fixed(format!(
            "Thursday [m:{}] [{hidden}] [D-1]",
            fx.message(OPEN, 1)
        )),
        AskConfig::default(),
    );
    let scope = fx.open_only();
    let prepared = pipe
        .prepare_in(
            &fx.store,
            &ledger,
            &ask("kiln", fx.member.into()),
            Scope::Channels(&scope),
        )
        .unwrap();
    let ids = prepared.bundle().ids();
    assert!(ids.contains(&CitationId::Message(fx.message(OPEN, 1))));
    assert!(!ids.contains(&hidden), "private message in the bundle");
    assert!(
        !ids.contains(&"D-1".parse().unwrap()),
        "an entry citing a hidden message is hidden whole"
    );
    assert!(ids.contains(&"D-2".parse().unwrap()), "typed entries stay");
    let result = pipe.run(&ledger, prepared).await.unwrap();
    let text = sent(&pipe);
    assert!(!text.contains("salary"), "hidden text reached the provider");
    assert!(!text.contains("hiring plan"));
    let Answer::Found(found) = result.answer else {
        panic!("expected an answer");
    };
    assert!(!found.text.contains(&hidden.marker()));
    assert!(!found.text.contains("[D-1]"));
    assert_eq!(found.dropped_citations, 2);
}

#[tokio::test]
async fn an_unscoped_question_reads_every_included_channel() {
    let fx = setup();
    let ledger = InMemoryLedger::new();
    let pipe = AskPipeline::new(FakeProvider::fixed("unused"), AskConfig::default());
    let prepared = pipe
        .prepare(&fx.store, &ledger, &ask("kiln", fx.member.into()))
        .unwrap();
    let ids = prepared.bundle().ids();
    assert!(ids.contains(&CitationId::Message(fx.message(PRIVATE, 2))));
    assert!(ids.contains(&"D-1".parse().unwrap()));
    // An empty scope (access unknown: fail closed) keeps only typed entries.
    let none = HashSet::new();
    let prepared = pipe
        .prepare_in(
            &fx.store,
            &ledger,
            &ask("kiln", fx.member.into()),
            Scope::Channels(&none),
        )
        .unwrap();
    let ids: Vec<String> = prepared
        .bundle()
        .ids()
        .iter()
        .map(|id| id.marker())
        .collect();
    assert_eq!(ids, ["[D-2]"]);
    assert!(!fx.open_only().contains(&fx.private));
}

#[tokio::test]
async fn a_summary_cites_only_the_given_visible_items() {
    let fx = setup();
    let ledger = InMemoryLedger::new();
    let open = fx.message(OPEN, 1);
    let private = fx.message(PRIVATE, 2);
    let pipe = AskPipeline::new(
        FakeProvider::fixed(format!(
            "- Kiln firing moved [D-2] [m:{open}]\n- Hiring [m:{private}] [m:999999] see https://evil.example.invalid/x"
        )),
        AskConfig::default(),
    );
    let items = Items {
        entries: vec![
            ShortId::parse_user("D-1").unwrap(),
            ShortId::parse_user("D-2").unwrap(),
            ShortId::parse_user("D-77").unwrap(),
        ],
        messages: vec![open, private],
    };
    let scope = fx.open_only();
    let prepared = pipe
        .prepare_summary(
            &fx.store,
            &ledger,
            fx.member.into(),
            at(NOW),
            &items,
            Scope::Channels(&scope),
        )
        .unwrap();
    assert_eq!(
        prepared.bundle().sources().len(),
        2,
        "D-2 and the open message"
    );
    assert!(prepared.request().system.contains("summarise"));
    let result = pipe.run(&ledger, prepared).await.unwrap();
    assert!(!sent(&pipe).contains("salary"));
    let Answer::Found(found) = result.answer else {
        panic!("expected a summary");
    };
    assert_eq!(found.citations.len(), 2);
    assert_eq!(found.dropped_citations, 2);
    assert_eq!(found.removed_urls, 1);
    assert!(!found.text.contains("evil.example"));
    // The call was charged to the member.
    assert_eq!(ledger.entries()[0].asker, Asker::Member(fx.member));
}

#[tokio::test]
async fn an_empty_or_declined_summary_is_not_found() {
    let fx = setup();
    let ledger = InMemoryLedger::new();
    let pipe = AskPipeline::new(FakeProvider::fixed("NOT_FOUND"), AskConfig::default());
    let empty = pipe
        .prepare_summary(
            &fx.store,
            &ledger,
            fx.member.into(),
            at(NOW),
            &Items::default(),
            Scope::All,
        )
        .unwrap();
    let result = pipe.run(&ledger, empty).await.unwrap();
    assert!(matches!(
        result.answer,
        Answer::NotFound(ref n) if n.reason == NotFoundReason::NoSources
    ));
    assert_eq!(
        pipe.provider().call_count(),
        0,
        "nothing to summarise: no call"
    );
    let items = Items {
        entries: vec![ShortId::parse_user("D-2").unwrap()],
        messages: Vec::new(),
    };
    let prepared = pipe
        .prepare_summary(
            &fx.store,
            &ledger,
            fx.member.into(),
            at(NOW),
            &items,
            Scope::All,
        )
        .unwrap();
    let result = pipe.run(&ledger, prepared).await.unwrap();
    assert!(matches!(
        result.answer,
        Answer::NotFound(ref n) if n.reason == NotFoundReason::ModelDeclined
    ));
}

#[tokio::test]
async fn tokens_have_their_own_budget() {
    let fx = setup();
    let ledger = InMemoryLedger::new();
    let one = AskConfig {
        budget: Budget {
            max_questions_per_hour: 1,
            ..Budget::default()
        },
        ..AskConfig::default()
    };
    let pipe = AskPipeline::new(FakeProvider::fixed("Thursday [D-2]"), one);
    let token = Asker::Token(TokenId::new(fx.member.get()).unwrap());
    let prepared = pipe
        .prepare(&fx.store, &ledger, &ask("kiln", token))
        .unwrap();
    pipe.run(&ledger, prepared).await.unwrap();
    assert!(matches!(
        pipe.prepare(&fx.store, &ledger, &ask("kiln", token)),
        Err(AskError::BudgetExceeded(BudgetLimit::QuestionsPerHour))
    ));
    // The member whose row id happens to match is a different asker.
    assert!(
        pipe.prepare(&fx.store, &ledger, &ask("kiln", fx.member.into()))
            .is_ok()
    );
    assert_eq!(
        ledger
            .usage_since(token, at(NOW - 60 * MINUTE))
            .unwrap()
            .questions,
        1
    );
}

#[test]
fn without_urls_removes_every_link() {
    let (text, removed) = without_urls(
        "See [the doc](https://example.org/a) and https://example.org/b or <https://x.example>",
    );
    assert_eq!(removed, 3);
    assert!(!text.contains("example.org"), "{text}");
    assert!(!text.contains("x.example"), "{text}");
    assert!(text.contains("the doc"));
}
