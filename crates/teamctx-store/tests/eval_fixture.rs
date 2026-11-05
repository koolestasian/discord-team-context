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
    assert!(cases.len() >= 40);
    let mut ids = BTreeSet::new();
    let mut questions = BTreeSet::new();
    let mut categories = BTreeSet::new();
    for case in cases {
        let case = case.as_table().expect("case table");
        let label = case["id"].as_str().expect("case ID");
        assert!(ids.insert(label), "duplicate case {label}");
        let question = case["question"].as_str().expect("question");
        assert!(!question.trim().is_empty());
        assert!(questions.insert(question), "duplicate question {label}");
        assert!(!case["query"].as_str().expect("query").trim().is_empty());
        categories.insert(case["category"].as_str().expect("category"));
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
    for required in ["paraphrase", "reversed", "name", "date"] {
        assert!(
            categories.contains(required),
            "missing eval category {required}"
        );
    }
}
