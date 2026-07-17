//! Canonical source snapshot hash and membership contracts.
use super::algolia_client::AlgoliaErrorKind;
use super::source_snapshot::{canonical_json_bytes, source_item_hash, SourceSnapshot};
use serde_json::json;
use std::collections::BTreeSet;

fn settings_fixture() -> serde_json::Value {
    json!({
        "b": [{"z": 2, "y": null}, 1],
        "a": true,
        "c": {"b": false, "a": "x"}
    })
}

fn document_one() -> serde_json::Value {
    json!({
        "objectID": "doc-1",
        "title": "Keyboard",
        "nested": {"b": 2, "a": 1},
        "tags": ["z", "a"],
        "flag": true
    })
}

fn document_two() -> serde_json::Value {
    json!({"title": null, "objectID": "doc-2"})
}

fn rule_one() -> serde_json::Value {
    json!({"objectID": "rule-1", "condition": {"pattern": "sale", "anchoring": "contains"}})
}

fn synonym_one() -> serde_json::Value {
    json!({"type": "synonym", "synonyms": ["tee", "shirt"], "objectID": "syn-1"})
}

fn btree_set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn source_snapshot_canonical_orders_object_keys_recursively_without_reordering_arrays() {
    let value = settings_fixture();

    assert_eq!(
        canonical_json_bytes(&value),
        br#"{"a":true,"b":[{"y":null,"z":2},1],"c":{"a":"x","b":false}}"#
    );
    assert_eq!(
        source_item_hash(&value),
        "f4f3850f967e1ee7bb269a32e75754209ba4394ee1c8f8fed5067910e4dfa31e"
    );
}

#[test]
fn source_snapshot_canonical_hashes_counts_and_membership_independent_of_item_order() {
    let first = SourceSnapshot::from_raw(
        settings_fixture(),
        vec![document_one(), document_two()],
        vec![rule_one()],
        vec![synonym_one()],
    )
    .expect("valid snapshot should build");
    let reordered = SourceSnapshot::from_raw(
        json!({
            "c": {"a": "x", "b": false},
            "b": [{"y": null, "z": 2}, 1],
            "a": true
        }),
        vec![
            json!({"objectID": "doc-2", "title": null}),
            json!({
                "tags": ["z", "a"],
                "nested": {"a": 1, "b": 2},
                "title": "Keyboard",
                "flag": true,
                "objectID": "doc-1"
            }),
        ],
        vec![json!({
            "condition": {"anchoring": "contains", "pattern": "sale"},
            "objectID": "rule-1"
        })],
        vec![json!({
            "objectID": "syn-1",
            "type": "synonym",
            "synonyms": ["tee", "shirt"]
        })],
    )
    .expect("reordered snapshot should build");

    assert_eq!(first, reordered);
    assert_eq!(first.settings.count, 1);
    assert_eq!(first.settings.ids, btree_set(&["settings"]));
    assert_eq!(
        first.settings.hash,
        "e650339378b616bfa703025ec0a57325d958a2d227b1abed63091dbc4d8157d1"
    );
    assert_eq!(first.documents.count, 2);
    assert_eq!(first.documents.ids, btree_set(&["doc-1", "doc-2"]));
    assert_eq!(
        first.documents.hash,
        "c29e809a377d2ebc3671961603f66843489049c485899c70ee9b1ccc9283ff9c"
    );
    assert_eq!(first.rules.count, 1);
    assert_eq!(first.rules.ids, btree_set(&["rule-1"]));
    assert_eq!(
        first.rules.hash,
        "6b1f5a494454d147f67a81b6cf25b38bf425aa29fe86ba5240509340f75b5967"
    );
    assert_eq!(first.synonyms.count, 1);
    assert_eq!(first.synonyms.ids, btree_set(&["syn-1"]));
    assert_eq!(
        first.synonyms.hash,
        "21565b89eacd4b569d043e4377a801cda328d1faf9756ebe223f06551ae59fe8"
    );
}

#[test]
fn source_snapshot_canonical_changes_for_value_insertions_and_deletions() {
    let baseline = SourceSnapshot::from_raw(
        settings_fixture(),
        vec![document_one(), document_two()],
        vec![rule_one()],
        vec![synonym_one()],
    )
    .expect("valid snapshot should build");
    let changed_value = SourceSnapshot::from_raw(
        settings_fixture(),
        vec![
            json!({"objectID": "doc-1", "title": "Keyboard Pro", "nested": {"a": 1, "b": 2}, "tags": ["z", "a"], "flag": true}),
            document_two(),
        ],
        vec![rule_one()],
        vec![synonym_one()],
    )
    .expect("changed value should still build");
    let inserted = SourceSnapshot::from_raw(
        settings_fixture(),
        vec![
            document_one(),
            document_two(),
            json!({"objectID": "doc-3", "title": "Mouse"}),
        ],
        vec![rule_one()],
        vec![synonym_one()],
    )
    .expect("inserted document should still build");
    let deleted = SourceSnapshot::from_raw(
        settings_fixture(),
        vec![document_one()],
        vec![rule_one()],
        vec![synonym_one()],
    )
    .expect("deleted document snapshot should still build");

    for changed_hash in [
        &changed_value.documents.hash,
        &inserted.documents.hash,
        &deleted.documents.hash,
    ] {
        assert_ne!(&baseline.documents.hash, changed_hash);
    }
    assert_ne!(baseline.documents.ids, inserted.documents.ids);
    assert_ne!(baseline.documents.ids, deleted.documents.ids);
    assert_ne!(baseline.documents.count, inserted.documents.count);
    assert_ne!(baseline.documents.count, deleted.documents.count);
}

#[test]
fn source_snapshot_canonical_rejects_missing_and_duplicate_object_ids() {
    for invalid_documents in [
        vec![json!({"title": "missing"})],
        vec![
            json!({"objectID": "doc-1", "title": "first"}),
            json!({"objectID": "doc-1", "title": "duplicate"}),
        ],
        vec![json!({"objectID": 7, "title": "wrong type"})],
    ] {
        let error = SourceSnapshot::from_raw(
            settings_fixture(),
            invalid_documents,
            vec![rule_one()],
            vec![synonym_one()],
        )
        .expect_err("invalid objectID membership must fail");

        assert_eq!(error.kind(), AlgoliaErrorKind::Schema);
        assert!(!error.safe_message().contains("doc-1"));
    }
}
