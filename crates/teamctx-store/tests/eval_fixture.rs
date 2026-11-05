//! Validate gold references and evidence, without claiming retrieval scores.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use teamctx_core::record::ShortId;

#[test]
fn eval_questions_cite_existing_evidence_with_matching_status() {
    let data: Value = serde_json::from_str(include_str!("../../../fixtures/demo/dataset.json"))
        .expect("valid demo JSON");
    let eval: toml::Table = include_str!("../../../fixtures/demo/eval.toml")
        .parse()
        .expect("valid eval TOML");
    assert_eq!(eval["schema_version"].as_integer(), Some(1));
    assert_eq!(eval["dataset"].as_str(), Some("dataset.json"));
    assert_eq!(eval["cutoff"].as_integer(), Some(10));
    let messages: BTreeMap<_, _> = data["messages"]
        .as_array()
        .expect("messages")
        .iter()
        .map(|m| (m["id"].as_str().expect("message ID"), m))
        .collect();
    let entries: BTreeMap<_, _> = data["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .map(|e| (e["id"].as_i64().expect("entry ID"), e))
        .collect();
    let cases = eval["cases"].as_array().expect("cases");
    assert!(cases.len() >= 50);
    let mut ids = BTreeSet::new();
    let mut questions = BTreeSet::new();
    let mut categories: BTreeMap<&str, usize> = BTreeMap::new();
    for case in cases {
        let case = case.as_table().expect("case table");
        let label = case["id"].as_str().expect("case ID");
        assert!(ids.insert(label), "duplicate case {label}");
        let question = case["question"].as_str().expect("question");
        assert!(!question.trim().is_empty());
        assert!(questions.insert(question), "duplicate question {label}");
        assert!(!case["query"].as_str().expect("query").trim().is_empty());
        let category = case["category"].as_str().expect("category");
        *categories.entry(category).or_default() += 1;
        let expected_messages = case["expected_messages"].as_array().expect("message gold");
        let expected_entries = case["expected_entries"].as_array().expect("entry gold");
        assert!(
            !expected_messages.is_empty(),
            "no message evidence for {label}"
        );
        assert!(
            !expected_entries.is_empty(),
            "no entry evidence for {label}"
        );
        let mut evidence = String::new();
        let mut cited_messages = BTreeSet::new();
        for id in expected_messages {
            let id = id.as_str().expect("message ID string");
            assert!(cited_messages.insert(id), "duplicate gold in {label}");
            let message = messages.get(id).expect("gold message exists");
            evidence.push_str(message["content"].as_str().expect("message text"));
            evidence.push('\n');
        }
        let mut cited_entries = BTreeSet::new();
        for id in expected_entries {
            let id: ShortId = id
                .as_str()
                .expect("short ID string")
                .parse()
                .expect("short ID");
            assert!(cited_entries.insert(id), "duplicate gold in {label}");
            let entry = entries.get(&id.id.get()).expect("gold entry exists");
            assert_eq!(entry["kind"].as_str(), Some(id.kind.as_str()));
            assert_eq!(entry["status"].as_str(), case["expected_status"].as_str());
            assert!(
                cited_messages.contains(entry["source_message_id"].as_str().expect("source")),
                "gold entry source is cited for {label}"
            );
            evidence.push_str(entry["title"].as_str().expect("entry title"));
            evidence.push('\n');
        }
        let answer = case["answer_contains"].as_str().expect("answer fragment");
        assert!(!answer.trim().is_empty());
        assert!(
            evidence.to_lowercase().contains(&answer.to_lowercase()),
            "unsupported answer for {label}"
        );
    }
    for (required, minimum) in [
        ("paraphrase", 20),
        ("reversed", 8),
        ("name", 6),
        ("date", 6),
        ("thread-context", 4),
    ] {
        let found = categories.get(required).copied().unwrap_or(0);
        assert!(
            found >= minimum,
            "eval category {required} has {found} cases, needs {minimum}"
        );
    }
}

/// English function words that do not count as content when comparing a query with its
/// answer fragment.
const STOPWORDS: &[&str] = &[
    "a", "about", "above", "after", "again", "all", "also", "am", "an", "and", "any", "are", "as",
    "at", "be", "because", "been", "before", "being", "below", "between", "both", "but", "by",
    "can", "could", "did", "do", "does", "doing", "down", "during", "each", "few", "for", "from",
    "further", "had", "has", "have", "having", "he", "her", "here", "hers", "him", "his", "how",
    "i", "if", "in", "into", "is", "it", "its", "itself", "just", "me", "more", "most", "my",
    "myself", "no", "nor", "not", "now", "of", "off", "on", "once", "only", "or", "other", "our",
    "ours", "out", "over", "own", "same", "she", "should", "so", "some", "such", "than", "that",
    "the", "their", "theirs", "them", "then", "there", "these", "they", "this", "those", "through",
    "to", "too", "under", "until", "up", "very", "was", "we", "were", "what", "when", "where",
    "which", "while", "who", "whom", "why", "will", "with", "would", "you", "your", "yours",
];

/// Lowercased alphanumeric words, with a trailing plural `s` removed from longer words so
/// that "cover" and "covers" count as the same word.
fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| match w.strip_suffix('s') {
            Some(stem) if w.len() > 3 => stem.to_owned(),
            _ => w.to_owned(),
        })
        .collect()
}

#[test]
fn paraphrase_queries_share_no_content_word_with_their_answer() {
    let eval: toml::Table = include_str!("../../../fixtures/demo/eval.toml")
        .parse()
        .expect("valid eval TOML");
    let stopwords: BTreeSet<String> = words(&STOPWORDS.join(" ")).into_iter().collect();
    let mut checked = 0;
    for case in eval["cases"].as_array().expect("cases") {
        let case = case.as_table().expect("case table");
        if case["category"].as_str() != Some("paraphrase") {
            continue;
        }
        let label = case["id"].as_str().expect("case ID");
        let query = case["query"].as_str().expect("query");
        let answer = case["answer_contains"].as_str().expect("answer fragment");
        let answer_words: BTreeSet<String> = words(answer).into_iter().collect();
        let shared: Vec<String> = words(query)
            .into_iter()
            .filter(|w| !stopwords.contains(w) && answer_words.contains(w))
            .collect();
        assert!(
            shared.is_empty(),
            "paraphrase {label} shares {shared:?} between query and answer"
        );
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} paraphrase cases");
}
