//! Canonical source snapshot hash and membership contracts.
use super::algolia_client::AlgoliaErrorKind;
use super::source_identity_partitions::{
    SourceIdentityConfig, SourceIdentityError, SourceIdentityVersion,
};
use super::source_snapshot::{
    canonical_json_bytes, source_item_hash, SourceSnapshot, SourceSnapshotBuilder,
};
use super::source_test_support::expected_document_v2_digest;
use serde_json::json;
use std::collections::BTreeSet;
use std::io;
use tempfile::TempDir;

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

fn rule_two() -> serde_json::Value {
    json!({"objectID": "rule-2", "consequence": {"filterPromotes": true}})
}

fn synonym_one() -> serde_json::Value {
    json!({"type": "synonym", "synonyms": ["tee", "shirt"], "objectID": "syn-1"})
}

fn synonym_two() -> serde_json::Value {
    json!({"type": "oneWaySynonym", "input": "tv", "synonyms": ["television"], "objectID": "syn-2"})
}

fn btree_set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

fn snapshot_from_raw(
    settings: serde_json::Value,
    documents: Vec<serde_json::Value>,
    rules: Vec<serde_json::Value>,
    synonyms: Vec<serde_json::Value>,
) -> SourceSnapshot {
    let spool_root = TempDir::new().expect("identity spool root should be created");
    SourceSnapshot::from_raw_with_identity_config(
        settings,
        documents,
        rules,
        synonyms,
        SourceIdentityConfig::for_test(spool_root.path(), 4096, 8),
    )
    .expect("valid snapshot should build")
}

fn snapshot_with_resource_order(reverse: bool) -> SourceSnapshot {
    let spool_root = TempDir::new().expect("identity spool root should be created");
    let mut builder =
        SourceSnapshotBuilder::new(SourceIdentityConfig::for_test(spool_root.path(), 4096, 8))
            .expect("snapshot builder should be created");
    builder.record_settings(&settings_fixture());
    builder
        .record_documents_page(0, &[document_one(), document_two()])
        .expect("documents should record");

    if reverse {
        builder
            .record_rules_page(0, &[rule_two()])
            .expect("rule two should record");
        builder
            .record_rules_page(1, &[rule_one()])
            .expect("rule one should record");
        builder
            .record_synonyms_page(0, &[synonym_two()])
            .expect("synonym two should record");
        builder
            .record_synonyms_page(1, &[synonym_one()])
            .expect("synonym one should record");
        builder
            .record_replica_settings("replica-b", &json!({"ranking": ["words"]}))
            .expect("replica b settings should record");
        builder
            .record_replica_settings("replica-a", &json!({"ranking": ["typo"]}))
            .expect("replica a settings should record");
    } else {
        builder
            .record_rules_page(0, &[rule_one()])
            .expect("rule one should record");
        builder
            .record_rules_page(1, &[rule_two()])
            .expect("rule two should record");
        builder
            .record_synonyms_page(0, &[synonym_one()])
            .expect("synonym one should record");
        builder
            .record_synonyms_page(1, &[synonym_two()])
            .expect("synonym two should record");
        builder
            .record_replica_settings("replica-a", &json!({"ranking": ["typo"]}))
            .expect("replica a settings should record");
        builder
            .record_replica_settings("replica-b", &json!({"ranking": ["words"]}))
            .expect("replica b settings should record");
    }

    builder.finish().expect("snapshot should finish")
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

/// TODO: Document source_snapshot_canonical_hashes_counts_and_membership_independent_of_item_order.
#[test]
fn source_snapshot_canonical_hashes_counts_and_membership_independent_of_item_order() {
    let first = snapshot_from_raw(
        settings_fixture(),
        vec![document_one(), document_two()],
        vec![rule_one()],
        vec![synonym_one()],
    );
    let reordered = snapshot_from_raw(
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
    );

    assert_eq!(first, reordered);
    assert_eq!(first.settings.count, 1);
    assert_eq!(first.settings.ids, btree_set(&["settings"]));
    assert_eq!(
        first.settings.hash,
        "e650339378b616bfa703025ec0a57325d958a2d227b1abed63091dbc4d8157d1"
    );
    assert_eq!(first.documents.count, 2);
    assert_eq!(first.documents.version, SourceIdentityVersion::V2);
    assert!(
        first.documents.ids.is_empty(),
        "document snapshots must not retain exact IDs in resident memory"
    );
    assert_eq!(
        first.documents.hash,
        expected_document_v2_digest(vec![document_one(), document_two()], 1)
    );
    assert_eq!(first.rules.count, 1);
    assert_eq!(first.rules.version, SourceIdentityVersion::V1);
    assert_eq!(first.rules.ids, btree_set(&["rule-1"]));
    assert_eq!(
        first.rules.hash,
        "6b1f5a494454d147f67a81b6cf25b38bf425aa29fe86ba5240509340f75b5967"
    );
    assert_eq!(first.synonyms.count, 1);
    assert_eq!(first.synonyms.version, SourceIdentityVersion::V1);
    assert_eq!(first.synonyms.ids, btree_set(&["syn-1"]));
    assert_eq!(
        first.synonyms.hash,
        "21565b89eacd4b569d043e4377a801cda328d1faf9756ebe223f06551ae59fe8"
    );
    assert_eq!(first.replica_settings.count, 0);
    assert_eq!(first.replica_settings.version, SourceIdentityVersion::V1);
    assert!(first.replica_settings.ids.is_empty());
    assert_eq!(
        first.replica_settings.hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn source_snapshot_resource_hashes_are_independent_of_rule_synonym_and_replica_order() {
    let first = snapshot_with_resource_order(false);
    let reordered = snapshot_with_resource_order(true);

    assert_eq!(first.rules, reordered.rules);
    assert_eq!(first.synonyms, reordered.synonyms);
    assert_eq!(first.replica_settings, reordered.replica_settings);
    assert_eq!(first.rules.count, 2);
    assert_eq!(first.rules.ids, btree_set(&["rule-1", "rule-2"]));
    assert_eq!(first.synonyms.count, 2);
    assert_eq!(first.synonyms.ids, btree_set(&["syn-1", "syn-2"]));
    assert_eq!(first.replica_settings.count, 2);
    assert_eq!(
        first.replica_settings.ids,
        btree_set(&["replica-a", "replica-b"])
    );
}

/// TODO: Document source_snapshot_canonical_changes_for_value_insertions_and_deletions.
#[test]
fn source_snapshot_canonical_changes_for_value_insertions_and_deletions() {
    let baseline = snapshot_from_raw(
        settings_fixture(),
        vec![document_one(), document_two()],
        vec![rule_one()],
        vec![synonym_one()],
    );
    let changed_value = snapshot_from_raw(
        settings_fixture(),
        vec![
            json!({"objectID": "doc-1", "title": "Keyboard Pro", "nested": {"a": 1, "b": 2}, "tags": ["z", "a"], "flag": true}),
            document_two(),
        ],
        vec![rule_one()],
        vec![synonym_one()],
    );
    let inserted = snapshot_from_raw(
        settings_fixture(),
        vec![
            document_one(),
            document_two(),
            json!({"objectID": "doc-3", "title": "Mouse"}),
        ],
        vec![rule_one()],
        vec![synonym_one()],
    );
    let deleted = snapshot_from_raw(
        settings_fixture(),
        vec![document_one()],
        vec![rule_one()],
        vec![synonym_one()],
    );

    for changed_hash in [
        &changed_value.documents.hash,
        &inserted.documents.hash,
        &deleted.documents.hash,
    ] {
        assert_ne!(&baseline.documents.hash, changed_hash);
    }
    assert!(baseline.documents.ids.is_empty());
    assert!(inserted.documents.ids.is_empty());
    assert!(deleted.documents.ids.is_empty());
    assert_ne!(baseline.documents.count, inserted.documents.count);
    assert_ne!(baseline.documents.count, deleted.documents.count);
}

/// TODO: Document source_snapshot_canonical_rejects_missing_and_duplicate_object_ids.
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
        let spool_root = TempDir::new().expect("identity spool root should be created");
        let error = SourceSnapshot::from_raw_with_identity_config(
            settings_fixture(),
            invalid_documents,
            vec![rule_one()],
            vec![synonym_one()],
            SourceIdentityConfig::for_test(spool_root.path(), 4096, 8),
        )
        .expect_err("invalid objectID membership must fail");

        assert_eq!(error.kind(), AlgoliaErrorKind::Schema);
        assert!(!error.safe_message().contains("doc-1"));
    }
}

#[test]
fn source_identity_infrastructure_errors_map_to_scrubbed_client_errors() {
    let cases = [
        (
            SourceIdentityError::PartitionBudgetExceeded {
                partition: 7,
                bytes: 8192,
                budget_bytes: 4096,
            },
            AlgoliaErrorKind::Limit,
            "source identity partition exceeded memory budget",
        ),
        (
            SourceIdentityError::InvalidConfig {
                name: "secret-config-name",
            },
            AlgoliaErrorKind::Validation,
            "source identity configuration was invalid",
        ),
        (
            SourceIdentityError::Io(io::Error::other("secret-spool-path")),
            AlgoliaErrorKind::Transport,
            "source identity partition I/O failed",
        ),
    ];

    for (identity_error, expected_kind, expected_message) in cases {
        let client_error = super::algolia_client::AlgoliaClientError::from(identity_error);
        assert_eq!(client_error.kind(), expected_kind);
        assert_eq!(client_error.safe_message(), expected_message);
        assert!(!format!("{client_error:?}").contains("secret"));
    }
}
