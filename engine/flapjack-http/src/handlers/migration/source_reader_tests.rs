//! Source-reader source identity, drift, and sanitization contracts.
use super::algolia_client::{AlgoliaErrorKind, AlgoliaIndexRecord};
use super::source_reader::{
    accept_source_export, collect_quiescent_source_snapshot, AlgoliaSourceReader,
    MigrationSourceReader,
};
use super::source_test_support::{RecordingSink, ScriptedSourceReader};
use serde_json::{json, Value};
use std::collections::VecDeque;

fn stable_reader() -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    reader.push_quiescent(stable_record());
    reader.push_pass(
        settings_fixture(),
        document_pages_in_order(),
        vec![vec![rule_one()]],
        vec![vec![synonym_one()]],
    );
    reader
}

fn add_export_pass(reader: &mut ScriptedSourceReader, document_pages: Vec<Vec<Value>>) {
    reader.push_pass(
        settings_fixture(),
        document_pages,
        vec![vec![rule_one()]],
        vec![vec![synonym_one()]],
    );
}

#[tokio::test]
async fn source_reader_identity_is_order_independent_and_uses_canonical_source_inputs() {
    let mut first = stable_reader();
    let mut reordered = stable_reader();
    reordered.document_reads = VecDeque::from([vec![vec![document_two()], vec![document_one()]]]);

    let first_identity = collect_quiescent_source_snapshot(&mut first)
        .await
        .expect("stable source should snapshot");
    let reordered_identity = collect_quiescent_source_snapshot(&mut reordered)
        .await
        .expect("reordered source should snapshot");

    assert_eq!(first_identity.digest(), reordered_identity.digest());
    assert_eq!(
        first_identity.digest(),
        "a11a1b23f8e9ca312bad89867fedf170a9a807df34bbaa9a30f8e60d9ef163e9"
    );
    assert_eq!(first_identity.updated_at(), "2026-07-15T00:00:00Z");
    assert_eq!(first_identity.document_metadata_count(), 2);
    assert_eq!(first_identity.snapshot().documents.count, 2);
    assert_eq!(first.acl_checks, 1);
    assert_eq!(reordered.acl_checks, 1);
}

#[test]
fn source_reader_algolia_backend_is_constructed_only_through_algolia_client_validation() {
    let reader = AlgoliaSourceReader::new("APPID", "source-key", "products")
        .expect("valid Algolia source reader should construct");
    assert_eq!(reader.app_id(), "APPID");
    assert_eq!(reader.source_name(), "products");

    let error = AlgoliaSourceReader::new("APPID", "source-key", "")
        .expect_err("empty source index should be rejected by AlgoliaClient");
    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
}

#[tokio::test]
async fn source_reader_two_pass_accepts_same_membership_with_page_order_changes() {
    let mut reader = stable_reader();
    add_export_pass(
        &mut reader,
        vec![vec![document_two()], vec![document_one()]],
    );
    reader.push_quiescent(stable_record());
    let mut sink = RecordingSink::default();

    let accepted = accept_source_export(&mut reader, &mut sink)
        .await
        .expect("same source identity should be accepted");

    assert_eq!(
        accepted.identity().digest(),
        "a11a1b23f8e9ca312bad89867fedf170a9a807df34bbaa9a30f8e60d9ef163e9"
    );
    assert_eq!(sink.settings, vec![settings_fixture()]);
    assert_eq!(sink.document_pages, vec![vec!["doc-2"], vec!["doc-1"]]);
    assert_eq!(sink.rule_pages, vec![vec!["rule-1"]]);
    assert_eq!(sink.synonym_pages, vec![vec!["syn-1"]]);
}

#[tokio::test]
async fn source_reader_two_pass_rejects_drift_with_scrubbed_error() {
    let mut reader = stable_reader();
    add_export_pass(
        &mut reader,
        vec![vec![
            document_one(),
            json!({"objectID": "doc-2", "title": "PII changed", "secret": "source-object-id"}),
        ]],
    );
    reader.push_quiescent(stable_record());
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("changed source hash must be rejected");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert_eq!(error.safe_message(), "Algolia source changed during export");
    let rendered = format!("{:?}", error);
    for secret in [
        "APPID",
        "products",
        "source-key",
        "source-object-id",
        "PII changed",
    ] {
        assert!(
            !rendered.contains(secret),
            "drift errors must not expose source material"
        );
    }
}

#[tokio::test]
async fn source_reader_two_pass_rejects_final_metadata_drift() {
    let mut changed_record = stable_record();
    changed_record.updated_at = "2026-07-15T00:01:00Z".to_string();
    let mut reader = stable_reader();
    add_export_pass(&mut reader, document_pages_in_order());
    reader.push_quiescent(changed_record);
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("changed final metadata must be rejected");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert_eq!(error.safe_message(), "Algolia source changed during export");
}

fn stable_record() -> AlgoliaIndexRecord {
    AlgoliaIndexRecord {
        name: "products".to_string(),
        entries: 2,
        updated_at: "2026-07-15T00:00:00Z".to_string(),
        pending_task: false,
    }
}

fn settings_fixture() -> Value {
    json!({"ranking": ["typo"], "nested": {"b": 2, "a": 1}})
}

fn document_pages_in_order() -> Vec<Vec<Value>> {
    vec![vec![document_one()], vec![document_two()]]
}

fn document_one() -> Value {
    json!({"objectID": "doc-1", "title": "Keyboard", "available": true})
}

fn document_two() -> Value {
    json!({"objectID": "doc-2", "title": null, "nested": {"b": 2, "a": 1}})
}

fn rule_one() -> Value {
    json!({"objectID": "rule-1", "condition": {"pattern": "sale"}})
}

fn synonym_one() -> Value {
    json!({"objectID": "syn-1", "type": "synonym", "synonyms": ["tee", "shirt"]})
}
