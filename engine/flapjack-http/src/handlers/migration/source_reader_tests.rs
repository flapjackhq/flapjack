use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord};
use super::meilisearch_client::{MeilisearchClientError, MeilisearchErrorKind};
use super::source_identity_partitions::SourceIdentityVersion;
use super::source_reader::{
    accept_source_export, collect_quiescent_source_snapshot, collect_replica_settings,
    AlgoliaSourceReader, MeilisearchSourceReader, MigrationSourceReader, TypesenseSourceReader,
};
use super::source_snapshot::{
    canonical_json_bytes, source_item_hash, update_source_item_hash_digest, SourceResourceSnapshot,
};
use super::source_test_support::{
    expected_document_v2_digest, meilisearch_observation, typesense_observation, RecordingSink,
    ScriptedMeilisearchSource, ScriptedSourceReader, ScriptedTypesenseSource,
};
use super::typesense_client::{TypesenseClientError, TypesenseErrorKind};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

fn meilisearch_settings() -> Value {
    json!({
        "searchableAttributes": ["title", "category"],
        "pagination": {"maxTotalHits": 50},
        "synonyms": {
            "saw": ["cutter"],
            "wrench": ["spanner"]
        }
    })
}

fn typesense_settings() -> Value {
    json!({
        "default_sorting_field": "price",
        "enable_nested_fields": true,
        "token_separators": ["-"],
        "symbols_to_index": ["#"]
    })
}

#[tokio::test]
async fn meilisearch_reader_normalizes_configured_primary_key_without_rewriting_source_fields() {
    let pages = vec![
        vec![
            json!({"sku": "SKU-001", "title": "Alpha Wrench"}),
            json!({"sku": "SKU-002", "title": "Beta Hammer"}),
        ],
        vec![json!({"sku": "SKU-003", "title": "Gamma Saw"})],
    ];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("configured_pk", "sku", 3),
        meilisearch_settings(),
        vec![pages],
    );
    let mut reader = MeilisearchSourceReader::from_source("configured_pk", source);
    reader.wait_for_quiescent_source().await.unwrap();
    let settings = reader.read_settings().await.unwrap();
    reader
        .require_unretrievable_access(&settings)
        .await
        .unwrap();

    let mut observed_pages = Vec::new();
    reader
        .read_documents(&mut |page| {
            observed_pages.push(page);
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(
        observed_pages.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(observed_pages[0][0]["objectID"], "SKU-001");
    assert_eq!(observed_pages[0][0]["sku"], "SKU-001");
    assert_eq!(observed_pages[0][0]["title"], "Alpha Wrench");
    assert_eq!(observed_pages[1][0]["objectID"], "SKU-003");
}

#[tokio::test]
async fn meilisearch_reader_normalizes_inferred_primary_key_and_synonyms_deterministically() {
    let documents = vec![vec![
        json!({"book_id": "B-001", "title": "First Book"}),
        json!({"book_id": "B-002", "title": "Second Book"}),
    ]];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("inferred_pk", "book_id", 2),
        meilisearch_settings(),
        vec![documents],
    );
    let mut reader = MeilisearchSourceReader::from_source("inferred_pk", source);
    reader.wait_for_quiescent_source().await.unwrap();
    let _settings = reader.read_settings().await.unwrap();

    let mut normalized = Vec::new();
    reader
        .read_documents(&mut |page| {
            normalized.extend(page);
            Ok(())
        })
        .await
        .unwrap();
    let mut synonyms = Vec::new();
    reader
        .read_synonyms(&mut |page| {
            synonyms.extend(page);
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(normalized[0]["objectID"], "B-001");
    assert_eq!(normalized[0]["book_id"], "B-001");
    assert_eq!(normalized[1]["objectID"], "B-002");
    assert_eq!(
        synonyms,
        vec![
            json!({
                "objectID": "meilisearch:saw",
                "type": "synonym",
                "synonyms": ["saw", "cutter"]
            }),
            json!({
                "objectID": "meilisearch:wrench",
                "type": "synonym",
                "synonyms": ["wrench", "spanner"]
            })
        ]
    );
}

#[tokio::test]
async fn meilisearch_reader_rejects_invalid_or_duplicate_stable_ids_with_sanitized_errors() {
    for (documents, expected_message) in [
        (
            vec![json!({"title": "missing"})],
            "Meilisearch document primary key is invalid",
        ),
        (
            vec![json!({"sku": 123, "title": "non-string"})],
            "Meilisearch document primary key is invalid",
        ),
        (
            vec![json!({"sku": "SKU-001"}), json!({"sku": "SKU-001"})],
            "duplicate source objectID",
        ),
    ] {
        let source = ScriptedMeilisearchSource::with_passes(
            meilisearch_observation("configured_pk", "sku", documents.len() as u64),
            meilisearch_settings(),
            vec![vec![documents]],
        );
        let mut reader = MeilisearchSourceReader::from_source("configured_pk", source);
        let error = collect_quiescent_source_snapshot(&mut reader)
            .await
            .unwrap_err();
        assert_eq!(error.safe_message(), expected_message);
        let debug = format!("{error:?}");
        assert!(!debug.contains("http://127.0.0.1:17747"));
        assert!(!debug.contains("meili-source-key-canary"));
    }
}

#[tokio::test]
async fn meilisearch_reader_rejects_restricted_credentials_before_accepting_snapshot() {
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("configured_pk", "sku", 1),
        meilisearch_settings(),
        vec![vec![vec![json!({"sku": "SKU-001"})]]],
    )
    .with_access_error(MeilisearchClientError::new(
        MeilisearchErrorKind::Upstream,
        "Meilisearch source credentials lack required read access",
    ));
    let mut reader = MeilisearchSourceReader::from_source("configured_pk", source);
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("restricted credentials must fail before accepting any artifact");

    assert_eq!(error.kind(), AlgoliaErrorKind::Upstream);
    assert_eq!(
        error.safe_message(),
        "Meilisearch source credentials lack required read access"
    );
    assert!(sink.settings.is_empty());
    assert!(sink.raw_document_pages.is_empty());
    let debug = format!("{error:?}");
    assert!(!debug.contains("meili-source-key-canary"));
    assert!(!debug.contains("http://127.0.0.1:17747"));
}

#[tokio::test]
async fn meilisearch_reader_rejects_drift_with_provider_neutral_diagnostic() {
    let initial = meilisearch_observation("configured_pk", "sku", 1);
    let mut changed = initial.clone();
    changed.updated_at = "2026-07-26T19:21:26Z".to_string();
    let source = ScriptedMeilisearchSource::with_passes(
        initial.clone(),
        meilisearch_settings(),
        vec![vec![vec![json!({"sku": "SKU-001"})]]],
    )
    .with_observations(vec![initial, changed]);
    let mut reader = MeilisearchSourceReader::from_source("configured_pk", source);
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("changed Meilisearch metadata must fail closed");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert_eq!(error.safe_message(), "Source changed during export");
    assert!(!format!("{error:?}").contains("Algolia"));
}

#[tokio::test]
async fn typesense_reader_normalizes_document_id_without_rewriting_source_fields() {
    let pages = vec![
        vec![
            json!({"id": "prod_001", "title": "Alpha Wrench"}),
            json!({"id": "prod_002", "title": "Beta Hammer"}),
        ],
        vec![json!({"id": "prod_003", "title": "Gamma Saw"})],
    ];
    let source = ScriptedTypesenseSource::with_passes(
        typesense_observation("products", 3),
        typesense_settings(),
        vec![pages],
    );
    let mut reader = TypesenseSourceReader::from_source("products", source);
    reader.wait_for_quiescent_source().await.unwrap();
    let settings = reader.read_settings().await.unwrap();
    reader
        .require_unretrievable_access(&settings)
        .await
        .unwrap();

    let mut observed_pages = Vec::new();
    reader
        .read_documents(&mut |page| {
            observed_pages.push(page);
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(
        observed_pages.iter().map(Vec::len).collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(observed_pages[0][0]["objectID"], "prod_001");
    assert_eq!(observed_pages[0][0]["id"], "prod_001");
    assert_eq!(observed_pages[0][0]["title"], "Alpha Wrench");
    assert_eq!(observed_pages[1][0]["objectID"], "prod_003");
}

#[tokio::test]
async fn typesense_reader_rejects_invalid_ids_with_sanitized_errors() {
    for documents in [
        vec![json!({"title": "missing"})],
        vec![json!({"id": 123, "title": "non-string"})],
        vec![json!({"id": "prod_001"}), json!({"id": "prod_001"})],
    ] {
        let source = ScriptedTypesenseSource::with_passes(
            typesense_observation("products", documents.len() as u64),
            typesense_settings(),
            vec![vec![documents]],
        );
        let mut reader = TypesenseSourceReader::from_source("products", source);
        let error = collect_quiescent_source_snapshot(&mut reader)
            .await
            .unwrap_err();
        assert_eq!(error.safe_message(), "Typesense document id is invalid");
        let debug = format!("{error:?}");
        assert!(!debug.contains("typesense-source-key-canary"));
        assert!(!debug.contains("http://127.0.0.1:18108"));
    }
}

#[tokio::test]
async fn typesense_reader_rejects_restricted_credentials_before_accepting_snapshot() {
    let source = ScriptedTypesenseSource::with_passes(
        typesense_observation("products", 1),
        typesense_settings(),
        vec![vec![vec![json!({"id": "prod_001"})]]],
    )
    .with_access_error(TypesenseClientError::new(
        TypesenseErrorKind::Upstream,
        "Typesense source credentials lack required read access",
    ));
    let mut reader = TypesenseSourceReader::from_source("products", source);
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("restricted credentials must fail before accepting any artifact");

    assert_eq!(error.kind(), AlgoliaErrorKind::Upstream);
    assert_eq!(
        error.safe_message(),
        "Typesense source credentials lack required read access"
    );
    assert!(sink.settings.is_empty());
    assert!(sink.raw_document_pages.is_empty());
    assert!(!format!("{error:?}").contains("typesense-source-key-canary"));
}

#[tokio::test]
async fn typesense_reader_rejects_observation_name_mismatch() {
    let source = ScriptedTypesenseSource::with_passes(
        typesense_observation("products_v2", 1),
        typesense_settings(),
        vec![vec![vec![json!({"id": "prod_001"})]]],
    );
    let mut reader = TypesenseSourceReader::from_source("products", source);

    let error = reader
        .wait_for_quiescent_source()
        .await
        .expect_err("an observed collection name must match the requested collection");

    assert_eq!(error.kind(), AlgoliaErrorKind::Schema);
    assert_eq!(
        error.safe_message(),
        "Typesense source response schema is invalid"
    );
}

#[tokio::test]
async fn typesense_reader_rejects_drift_with_provider_neutral_diagnostic() {
    let initial = typesense_observation("products", 1);
    let mut changed = initial.clone();
    changed.updated_at = "1785020401".to_string();
    let source = ScriptedTypesenseSource::with_passes(
        initial.clone(),
        typesense_settings(),
        vec![vec![vec![json!({"id": "prod_001"})]]],
    )
    .with_observations(vec![initial, changed]);
    let mut reader = TypesenseSourceReader::from_source("products", source);
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("changed Typesense metadata must fail closed");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert_eq!(error.safe_message(), "Source changed during export");
    assert!(!format!("{error:?}").contains("Typesense"));
}

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
        expected_source_identity_digest(
            "APPID",
            "products",
            &stable_record(),
            &first_identity.snapshot().settings,
            &first_identity.snapshot().documents,
            &first_identity.snapshot().rules,
            &first_identity.snapshot().synonyms,
        )
    );
    assert_eq!(first_identity.updated_at(), "2026-07-15T00:00:00Z");
    assert_eq!(first_identity.document_metadata_count(), 2);
    assert_eq!(first_identity.snapshot().documents.count, 2);
    assert_eq!(
        first_identity.snapshot().documents.version,
        SourceIdentityVersion::V2
    );
    assert!(first_identity.snapshot().documents.ids.is_empty());
    assert_eq!(first.acl_checks, 1);
    assert_eq!(reordered.acl_checks, 1);
}

#[tokio::test]
async fn source_reader_document_identity_uses_v2_digest_and_versioned_top_level_preimage() {
    let mut reader = stable_reader();

    let identity = collect_quiescent_source_snapshot(&mut reader)
        .await
        .expect("stable source should snapshot");

    let snapshot = identity.snapshot();
    assert_eq!(snapshot.documents.version, SourceIdentityVersion::V2);
    assert_eq!(
        snapshot.documents.hash,
        expected_document_v2_digest(vec![document_one(), document_two()], 2048)
    );
    assert_eq!(
        identity.digest(),
        expected_source_identity_digest(
            "APPID",
            "products",
            &stable_record(),
            &snapshot.settings,
            &snapshot.documents,
            &snapshot.rules,
            &snapshot.synonyms,
        )
    );

    let v1_document_resource = SourceResourceSnapshot {
        count: 2,
        hash: expected_v1_aggregate_digest(vec![document_one(), document_two()]),
        ids: ["doc-1".to_string(), "doc-2".to_string()].into(),
        version: SourceIdentityVersion::V1,
    };
    assert_ne!(
        identity.digest(),
        expected_source_identity_digest(
            "APPID",
            "products",
            &stable_record(),
            &snapshot.settings,
            &v1_document_resource,
            &snapshot.rules,
            &snapshot.synonyms,
        ),
        "top-level identity must include resource identity version in the preimage"
    );
}

#[tokio::test]
async fn source_reader_document_identity_changes_for_insert_delete_and_in_place_update() {
    let baseline = source_identity_for_documents(document_pages_in_order()).await;
    let inserted = source_identity_for_documents(vec![vec![
        document_one(),
        document_two(),
        json!({"objectID": "doc-3", "title": "Mouse"}),
    ]])
    .await;
    let deleted = source_identity_for_documents(vec![vec![document_one()]]).await;
    let changed = source_identity_for_documents(vec![vec![
        document_one(),
        json!({"objectID": "doc-2", "title": "Monitor"}),
    ]])
    .await;

    assert_ne!(baseline.digest(), inserted.digest());
    assert_ne!(baseline.digest(), deleted.digest());
    assert_ne!(baseline.digest(), changed.digest());
    assert_ne!(
        baseline.snapshot().documents.hash,
        inserted.snapshot().documents.hash
    );
    assert_ne!(
        baseline.snapshot().documents.hash,
        deleted.snapshot().documents.hash
    );
    assert_ne!(
        baseline.snapshot().documents.hash,
        changed.snapshot().documents.hash
    );
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
        expected_source_identity_digest(
            "APPID",
            "products",
            &stable_record(),
            &accepted.identity().snapshot().settings,
            &accepted.identity().snapshot().documents,
            &accepted.identity().snapshot().rules,
            &accepted.identity().snapshot().synonyms,
        )
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
    assert_eq!(error.safe_message(), "Source changed during export");
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
    assert_eq!(error.safe_message(), "Source changed during export");
}

#[tokio::test]
async fn source_reader_final_drift_is_detected_after_sink_capture() {
    let mut changed_record = stable_record();
    changed_record.updated_at = "2026-07-26T19:21:26Z".to_string();
    let mut reader = stable_reader();
    add_export_pass(&mut reader, document_pages_in_order());
    reader.push_quiescent(changed_record);
    let mut sink = RecordingSink::default();

    let error = accept_source_export(&mut reader, &mut sink)
        .await
        .expect_err("changed source must be rejected after capture");

    assert_eq!(error.kind(), AlgoliaErrorKind::Progress);
    assert_eq!(error.safe_message(), "Source changed during export");
    assert_eq!(sink.settings, vec![settings_fixture()]);
    assert_eq!(sink.document_pages, vec![vec!["doc-1"], vec!["doc-2"]]);
    assert_eq!(sink.rule_pages, vec![vec!["rule-1"]]);
    assert_eq!(sink.synonym_pages, vec![vec!["syn-1"]]);
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

async fn source_identity_for_documents(
    document_pages: Vec<Vec<Value>>,
) -> super::source_reader::SourceIdentity {
    let document_count = document_pages.iter().map(Vec::len).sum::<usize>() as u64;
    let mut record = stable_record();
    record.entries = document_count;
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    reader.push_quiescent(record);
    reader.push_pass(
        settings_fixture(),
        document_pages,
        vec![vec![rule_one()]],
        vec![vec![synonym_one()]],
    );
    collect_quiescent_source_snapshot(&mut reader)
        .await
        .expect("stable source should snapshot")
}

fn expected_v1_aggregate_digest(items: Vec<Value>) -> String {
    let mut tuples = items
        .into_iter()
        .map(|item| {
            (
                item["objectID"].as_str().unwrap().to_string(),
                source_item_hash(&item),
            )
        })
        .collect::<Vec<_>>();
    tuples.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (object_id, item_hash) in tuples {
        update_source_item_hash_digest(&mut hasher, &object_id, &item_hash);
    }
    hex::encode(hasher.finalize())
}

fn expected_source_identity_digest(
    app_id: &str,
    source_name: &str,
    metadata: &AlgoliaIndexRecord,
    settings: &SourceResourceSnapshot,
    documents: &SourceResourceSnapshot,
    rules: &SourceResourceSnapshot,
    synonyms: &SourceResourceSnapshot,
) -> String {
    let identity = json!({
        "appID": app_id,
        "sourceIndex": source_name,
        "updatedAt": metadata.updated_at,
        "documentMetadataCount": metadata.entries,
        "resources": {
            "settings": expected_resource_identity(settings),
            "documents": expected_resource_identity(documents),
            "rules": expected_resource_identity(rules),
            "synonyms": expected_resource_identity(synonyms),
        }
    });
    hex::encode(Sha256::digest(canonical_json_bytes(&identity)))
}

fn expected_resource_identity(resource: &SourceResourceSnapshot) -> Value {
    json!({
        "count": resource.count,
        "hash": resource.hash,
        "version": expected_version_name(resource.version),
    })
}

fn expected_version_name(version: SourceIdentityVersion) -> &'static str {
    match version {
        SourceIdentityVersion::V1 => "v1",
        SourceIdentityVersion::V2 => "v2",
    }
}
