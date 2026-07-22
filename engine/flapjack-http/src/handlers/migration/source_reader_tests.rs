use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord};
use super::source_reader::{
    accept_source_export, collect_quiescent_source_snapshot, collect_replica_settings,
    AlgoliaSourceReader, MigrationSourceReader,
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

// --- Replica settings collector ---------------------------------------------

fn primary_with_replicas() -> Value {
    json!({
        "ranking": ["typo"],
        "replicas": ["price_asc", "virtual(relevance)"]
    })
}

fn replica_price_settings() -> Value {
    json!({
        "ranking": ["desc(price)"],
        "customRanking": ["asc(name)"],
        "relevancyStrictness": 80,
        "searchableAttributes": ["title", "brand"],
        "primary": "products"
    })
}

fn replica_relevance_settings() -> Value {
    json!({
        "ranking": ["asc(popularity)"],
        "relevancyStrictness": 50,
        "searchableAttributes": ["title"],
        "primary": "products"
    })
}

#[tokio::test]
async fn collect_replica_settings_fetches_bare_and_virtual_names_in_order_with_full_json() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    // Queued in the exact order the collector must request them: the bare name
    // first, then the virtual replica's inner name.
    reader.push_index_settings("price_asc", Ok(replica_price_settings()));
    reader.push_index_settings("relevance", Ok(replica_relevance_settings()));

    let collected = collect_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect("all queued replica settings should collect");

    // Exact parsed names (virtual(...) unwrapped) become the map keys.
    assert_eq!(
        collected.keys().collect::<Vec<_>>(),
        vec!["price_asc", "relevance"]
    );
    // The complete per-replica JSON is preserved, including searchableAttributes.
    assert_eq!(collected["price_asc"], replica_price_settings());
    assert_eq!(collected["relevance"], replica_relevance_settings());
    assert_eq!(
        collected["price_asc"]["searchableAttributes"],
        json!(["title", "brand"])
    );
    // Every queued read was consumed exactly once, in order.
    assert!(reader.index_settings_reads.is_empty());
}

#[tokio::test]
async fn collect_replica_settings_absent_replicas_performs_zero_reads() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    // A queued read is present but must never be consulted when replicas is absent.
    reader.push_index_settings("price_asc", Ok(replica_price_settings()));

    let collected = collect_replica_settings(&mut reader, &json!({"ranking": ["typo"]}))
        .await
        .expect("absent replicas must succeed with no reads");

    assert!(collected.is_empty());
    assert_eq!(reader.index_settings_reads.len(), 1);
}

#[tokio::test]
async fn collect_replica_settings_fails_closed_on_missing_script() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    // replicas names a replica, but no settings read was queued.

    let error = collect_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect_err("a missing scripted read must fail closed");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
}

#[tokio::test]
async fn collect_replica_settings_maps_parser_failure_to_scrubbed_validation() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    let malformed = json!({"replicas": ["virtual(no-close"]});

    let error = collect_replica_settings(&mut reader, &malformed)
        .await
        .expect_err("an unparseable replica entry must be rejected");

    assert_eq!(error.kind(), AlgoliaErrorKind::Validation);
    assert_eq!(
        error.safe_message(),
        "Algolia replica entry could not be parsed for migration"
    );
    assert!(
        !format!("{error:?}").contains("no-close"),
        "parser failures must not echo the raw replica entry"
    );
}

#[tokio::test]
async fn collect_replica_settings_propagates_typed_fetch_error() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    reader.push_index_settings(
        "price_asc",
        Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Upstream,
            "Algolia upstream rejected the request",
        )),
    );

    let error = collect_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect_err("a replica fetch error must surface");

    assert_eq!(error.kind(), AlgoliaErrorKind::Upstream);
    assert_eq!(
        error.safe_message(),
        "Algolia upstream rejected the request"
    );
}

#[tokio::test]
async fn collect_replica_settings_fails_closed_on_requested_name_mismatch() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    // The queued read expects a different name than the collector will request.
    reader.push_index_settings("wrong_name", Ok(replica_price_settings()));

    let error = collect_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect_err("an out-of-order replica request must fail closed");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
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
