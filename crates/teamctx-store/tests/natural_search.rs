//! Natural-language questions against the fictional demo dataset (plan section 9).
//! Bare words are alternatives ranked by bm25; phrases and filters stay required.
#![expect(clippy::unwrap_used, reason = "test setup must fail loudly")]

use teamctx_core::{
    query::parse,
    repo::{MessageRead, RecordRead},
};
use teamctx_store::{Store, demo::import};

const DATASET: &str = include_str!("../../../fixtures/demo/dataset.json");

fn imported() -> (Store, teamctx_store::demo::DemoMap) {
    let mut store = Store::in_memory().unwrap();
    let map = import(&mut store, DATASET).unwrap();
    (store, map)
}

#[test]
fn a_natural_question_finds_the_expected_entry_first() {
    let (store, map) = imported();
    let d7 = map.entry_label("D-7").unwrap();
    // Not one of the eval questions. No entry contains every word of it, so the old
    // all-words rule found nothing.
    let query = parse("What is the default font for the interface?").unwrap();
    let hits = store.search_entries(&query, 10).unwrap();
    assert!(!hits.is_empty(), "a natural question must match");
    assert_eq!(map.fixture_entry(hits[0].entry.short_id()), Some(d7));
    assert!(hits.iter().all(|hit| hit.bm25 < 0.0));
    // More matched terms score better: bm25 is negative, and sorted best first.
    assert!(hits.windows(2).all(|pair| pair[0].bm25 <= pair[1].bm25));
}

#[test]
fn a_natural_question_finds_messages_too() {
    let (store, _) = imported();
    let query = parse("Is Fenwick licensed for apps?").unwrap();
    let hits = store.search_messages(&query, 10).unwrap();
    assert!(!hits.is_empty(), "a natural question must match");
    assert!(
        hits[0]
            .message
            .content
            .contains("Fenwick licensed for apps")
    );
}

#[test]
fn phrases_stay_required_and_words_stay_optional() {
    let (store, _) = imported();
    let loose = parse("Fenwick licence").unwrap();
    let both = store.search_messages(&loose, 100).unwrap();
    let fenwick = store
        .search_messages(&parse("Fenwick").unwrap(), 100)
        .unwrap();
    // OR: a word added to the query never removes a hit.
    assert!(both.len() >= fenwick.len());
    // A phrase no message contains removes everything, even next to a matching word.
    let required = parse("Fenwick \"no message says this\"").unwrap();
    assert!(store.search_messages(&required, 100).unwrap().is_empty());
    assert!(store.search_entries(&required, 100).unwrap().is_empty());
}

#[test]
fn a_query_of_only_stopwords_still_searches() {
    let (store, _) = imported();
    let query = parse("what did we").unwrap();
    assert!(!store.search_messages(&query, 5).unwrap().is_empty());
}

#[test]
fn hostile_question_text_is_matched_as_literals() {
    let (store, _) = imported();
    for text in [
        "Fenwick OR content:secret",
        "Fenwick NOT licensed",
        "title:Fenwick* ^licensed",
        "Fenwick; DROP TABLE messages;--",
        "NEAR(Fenwick licensed, 1)",
    ] {
        let query = parse(text).unwrap();
        // None of these may fail as an FTS syntax error; Fenwick still matches.
        let hits = store.search_messages(&query, 5);
        assert!(hits.is_ok(), "{text:?}: {hits:?}");
        assert!(store.search_entries(&query, 5).is_ok(), "{text:?}");
    }
    // The data is intact afterwards.
    let all = parse("Fenwick").unwrap();
    assert!(!store.search_messages(&all, 5).unwrap().is_empty());
}
