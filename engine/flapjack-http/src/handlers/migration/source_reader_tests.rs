use super::algolia_client::{
    install_test_algolia_validation_resolver, AlgoliaClientError, AlgoliaErrorKind,
    AlgoliaIndexRecord, TEST_VETTED_ALGOLIA_IP,
};
use super::export::export_algolia_source;
use super::meilisearch_client::{MeilisearchClientError, MeilisearchErrorKind};
use super::source_identity_partitions::SourceIdentityVersion;
use super::source_reader::{
    accept_source_export, collect_quiescent_source_snapshot, collect_replica_settings,
    AcceptedSourceExport, AlgoliaSourceReader, MeilisearchSourceReader, MigrationSourceReader,
    SourceConfigurationArtifact, SourceConfigurationConsumer, SourceDocumentPageConsumer,
    SourceExportError, SourceExportErrorKind, SourceExportRecord, SourceExportSink, SourceFuture,
    SourceObservation, TypesenseSourceReader,
};
use super::source_snapshot::{
    canonical_json_bytes, source_item_hash, update_source_item_hash_digest, SourceResourceSnapshot,
};
use super::source_test_support::{
    expected_document_v2_digest, meilisearch_observation, typesense_observation, RecordingSink,
    ScriptedMeilisearchSource, ScriptedSourceReader, ScriptedTypesenseSource,
};
use super::spool::{SpoolLimits, SpoolStore};
use super::translation::{translate_settings_for_provider, ReportCode, SettingsSourceProvider};
use super::typesense_client::{TypesenseClientError, TypesenseErrorKind};
use super::AsyncMigrationSourceProvider;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use tempfile::TempDir;
use uuid::Uuid;

/// Drive one reader through the shared capture seam and return exactly what a
/// production sink observed. Tests assert against captured artifacts rather than
/// re-deriving them.
async fn capture_through_sink<R>(reader: &mut R) -> Result<RecordingSink, SourceExportError>
where
    R: MigrationSourceReader + Send,
{
    let mut sink = RecordingSink::default();
    super::source_reader::read_source_snapshot(reader, &mut sink).await?;
    Ok(sink)
}

/// The identity payloads a capture produced, flattened in page order.
fn captured_documents(sink: &RecordingSink) -> Vec<Value> {
    sink.raw_document_pages
        .iter()
        .flatten()
        .map(|document| document["payload"].clone())
        .collect()
}

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

fn meilisearch_settings_with_source_owned_urls() -> Value {
    json!({
        "searchableAttributes": ["title"],
        "displayedAttributes": ["url", "apiKey", "title"],
        "stopWords": ["https://search.example/settings", "apiKey"],
        "synonyms": {
            "saw": ["cutter"]
        }
    })
}

fn meilisearch_settings_after_synonym_split_with_source_owned_urls() -> Value {
    json!({
        "searchableAttributes": ["title"],
        "displayedAttributes": ["url", "apiKey", "title"],
        "stopWords": ["https://search.example/settings", "apiKey"]
    })
}

fn assert_meilisearch_capture_still_translates(settings: &Value) {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let translated = translate_settings_for_provider(
        settings,
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .expect("captured raw Meilisearch settings should translate downstream");

    assert!(failures.is_empty());
    assert_eq!(
        translated.searchable_attributes,
        Some(vec!["title".to_string(), "category".to_string()])
    );
    assert_eq!(translated.pagination_limited_to, 50);
}

fn typesense_settings() -> Value {
    json!({
        "default_sorting_field": "price",
        "enable_nested_fields": true,
        "token_separators": ["-"],
        "symbols_to_index": ["#"]
    })
}

fn typesense_settings_with_source_owned_urls() -> Value {
    json!({
        "default_sorting_field": "price",
        "enable_nested_fields": true,
        "metadata": {
            "url": "https://typesense.example/schema",
            "apiKey": "source-owned-field-name",
            "nested": {
                "endpoint": "https://typesense.example/nested"
            }
        }
    })
}

#[tokio::test]
async fn meilisearch_reader_normalizes_configured_primary_key_without_rewriting_source_fields() {
    // Each document carries a source-owned `objectID` that is NOT the configured primary
    // key, so a reader that reverts to Algolia-shaped identity fails this test loudly.
    let pages = vec![
        vec![
            json!({"sku": "SKU-001", "objectID": "source-owned-field-1", "title": "Alpha Wrench"}),
            json!({"sku": "SKU-002", "objectID": "source-owned-field-2", "title": "Beta Hammer"}),
        ],
        vec![json!({"sku": "SKU-003", "objectID": "source-owned-field-3", "title": "Gamma Saw"})],
    ];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("configured_pk", "sku", 3),
        meilisearch_settings(),
        vec![pages],
    );
    let mut reader = MeilisearchSourceReader::from_source("configured_pk", source);
    reader.observe_quiescent_source().await.unwrap();
    let sink = capture_through_sink(&mut reader).await.unwrap();

    assert_eq!(
        sink.document_pages,
        vec![vec!["SKU-001", "SKU-002"], vec!["SKU-003"]]
    );
    let documents = captured_documents(&sink);
    assert_eq!(
        documents[0],
        json!({"sku": "SKU-001", "objectID": "source-owned-field-1", "title": "Alpha Wrench"})
    );
    assert_eq!(
        documents[2],
        json!({"sku": "SKU-003", "objectID": "source-owned-field-3", "title": "Gamma Saw"})
    );
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
    reader.observe_quiescent_source().await.unwrap();
    let sink = capture_through_sink(&mut reader).await.unwrap();

    assert_eq!(sink.document_pages, vec![vec!["B-001", "B-002"]]);
    let documents = captured_documents(&sink);
    assert_eq!(
        documents[0],
        json!({"book_id": "B-001", "title": "First Book"})
    );
    assert_eq!(
        documents[1],
        json!({"book_id": "B-002", "title": "Second Book"})
    );
    assert_eq!(
        sink.raw_synonym_pages.concat(),
        vec![
            json!({
                "stableId": "meilisearch:synonym:saw",
                "payload": {"saw": ["cutter"]}
            }),
            json!({
                "stableId": "meilisearch:synonym:wrench",
                "payload": {"wrench": ["spanner"]}
            })
        ]
    );
    assert_meilisearch_capture_still_translates(&sink.settings[0]);
}

#[tokio::test]
async fn meilisearch_reader_rejects_empty_synonym_alternative_at_source_boundary() {
    let settings = json!({
        "searchableAttributes": ["title"],
        "synonyms": {"saw": ["cutter", ""]}
    });
    let documents = vec![vec![json!({"sku": "SKU-001", "title": "Alpha"})]];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("invalid_synonym", "sku", 1),
        settings,
        vec![documents],
    );
    let mut reader = MeilisearchSourceReader::from_source("invalid_synonym", source);
    reader
        .observe_quiescent_source()
        .await
        .expect("scripted source observation must succeed before testing synonym validation");

    let error = match capture_through_sink(&mut reader).await {
        Err(error) => error,
        Ok(_) => panic!("invalid synonyms must be rejected before source capture is accepted"),
    };

    assert_eq!(error.kind(), SourceExportErrorKind::Schema);
    assert_eq!(
        error.safe_message(),
        "Meilisearch source response schema is invalid"
    );
}

#[tokio::test]
async fn meilisearch_reader_captures_synonym_with_no_alternatives() {
    // Meilisearch allows an input mapped to an empty alternatives list. Capture
    // keeps that payload native so replay can translate it to the input word
    // alone; only malformed alternatives are rejected at the source boundary.
    let settings = json!({
        "searchableAttributes": ["title"],
        "synonyms": {"saw": []}
    });
    let documents = vec![vec![json!({"sku": "SKU-001", "title": "Alpha"})]];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("empty_synonym", "sku", 1),
        settings,
        vec![documents],
    );
    let mut reader = MeilisearchSourceReader::from_source("empty_synonym", source);
    reader
        .observe_quiescent_source()
        .await
        .expect("scripted source observation must succeed before testing synonym capture");

    let sink = capture_through_sink(&mut reader)
        .await
        .expect("a synonym with no alternatives must survive source capture");

    assert_eq!(
        sink.raw_synonym_pages.concat(),
        vec![json!({
            "stableId": "meilisearch:synonym:saw",
            "payload": {"saw": []}
        })]
    );
}

#[tokio::test]
async fn meilisearch_reader_preserves_native_settings_payloads_and_identity_preimage() {
    let source_settings = meilisearch_settings_with_source_owned_urls();
    let captured_settings = meilisearch_settings_after_synonym_split_with_source_owned_urls();
    let documents = vec![vec![json!({"sku": "SKU-001", "title": "Alpha"})]];
    let mut reader = MeilisearchSourceReader::from_source(
        "settings_canary",
        ScriptedMeilisearchSource::with_passes(
            meilisearch_observation("settings_canary", "sku", 1),
            source_settings,
            vec![documents.clone(), documents],
        ),
    );
    let mut sink = RecordingSink::default();

    let accepted = accept_source_export(
        AsyncMigrationSourceProvider::Meilisearch,
        &mut reader,
        &mut sink,
    )
    .await
    .expect("source-owned URL settings must survive Meilisearch capture");

    assert_eq!(sink.settings, vec![captured_settings.clone()]);
    assert_eq!(
        accepted.identity().snapshot().settings.hash,
        expected_settings_resource_hash(&captured_settings),
        "source identity must hash the raw settings payload, not the scrubbed payload"
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Meilisearch,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("restricted credentials must fail before accepting any artifact");

    assert_eq!(error.kind(), SourceExportErrorKind::Upstream);
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Meilisearch,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("changed Meilisearch metadata must fail closed");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
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
    reader.observe_quiescent_source().await.unwrap();
    let sink = capture_through_sink(&mut reader).await.unwrap();

    assert_eq!(
        sink.document_pages,
        vec![vec!["prod_001", "prod_002"], vec!["prod_003"]]
    );
    let documents = captured_documents(&sink);
    assert_eq!(
        documents[0],
        json!({"id": "prod_001", "title": "Alpha Wrench"})
    );
    assert_eq!(
        documents[2],
        json!({"id": "prod_003", "title": "Gamma Saw"})
    );
}

#[tokio::test]
async fn typesense_reader_preserves_native_settings_payloads_and_identity_preimage() {
    let settings = typesense_settings_with_source_owned_urls();
    let documents = vec![vec![json!({"id": "prod_001", "title": "Alpha"})]];
    let mut reader = TypesenseSourceReader::from_source(
        "settings_canary",
        ScriptedTypesenseSource::with_passes(
            typesense_observation("settings_canary", 1),
            settings.clone(),
            vec![documents.clone(), documents],
        ),
    );
    let mut sink = RecordingSink::default();

    let accepted = accept_source_export(
        AsyncMigrationSourceProvider::Typesense,
        &mut reader,
        &mut sink,
    )
    .await
    .expect("source-owned URL settings must survive Typesense capture");

    assert_eq!(sink.settings, vec![settings.clone()]);
    assert_eq!(
        accepted.identity().snapshot().settings.hash,
        expected_settings_resource_hash(&settings),
        "source identity must hash the raw settings payload, not the scrubbed payload"
    );
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Typesense,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("restricted credentials must fail before accepting any artifact");

    assert_eq!(error.kind(), SourceExportErrorKind::Upstream);
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
        .observe_quiescent_source()
        .await
        .expect_err("an observed collection name must match the requested collection");

    assert_eq!(error.kind(), SourceExportErrorKind::Schema);
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Typesense,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("changed Typesense metadata must fail closed");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
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

/// Project an accepted export and the artifacts its sink captured onto the
/// neutral bundle shape. Every field is read through a typed production
/// accessor, so no debug rendering can stand in for a real contract field.
fn accepted_source_export_projection(
    accepted: &AcceptedSourceExport,
    sink: &RecordingSink,
) -> Value {
    json!({
        "provider": accepted.provider().as_str(),
        "identity": {
            "namespace": accepted.source_namespace(),
            "source": accepted.source_name(),
        },
        "documents": sink.raw_document_pages.iter().flatten().collect::<Vec<_>>(),
        "configuration": [
            json!({"kind": "settings", "items": sink.settings}),
            json!({"kind": "rules", "items": sink.raw_rule_pages.concat()}),
            json!({"kind": "synonyms", "items": sink.raw_synonym_pages.concat()}),
        ],
        "manifest": {
            "acceptedRevision": accepted.identity().accepted_revision(),
            "documentCount": accepted.identity().document_metadata_count(),
            "digest": accepted.identity().digest(),
        },
        "warnings": accepted.warning_codes(),
    })
}

struct NeutralExportOracle<'a> {
    provider: &'a str,
    namespace: Option<&'a str>,
    source_name: &'a str,
    documents: Vec<Value>,
    configuration: Vec<Value>,
    accepted_revision: &'a str,
    document_count: u64,
    digest: &'a str,
    warning_codes: Vec<ReportCode>,
}

fn neutral_export_bundle_oracle(spec: NeutralExportOracle<'_>) -> Value {
    json!({
        "provider": spec.provider,
        "identity": {
            "namespace": spec.namespace,
            "source": spec.source_name,
        },
        "documents": spec.documents,
        "configuration": spec.configuration,
        "manifest": {
            "acceptedRevision": spec.accepted_revision,
            "documentCount": spec.document_count,
            "digest": spec.digest,
        },
        "warnings": spec.warning_codes,
    })
}

#[tokio::test]
async fn all_source_readers_emit_the_exact_neutral_export_bundle_oracle() {
    let mut algolia = stable_reader();
    add_export_pass(&mut algolia, document_pages_in_order());
    algolia.push_quiescent(stable_record());
    let mut algolia_sink = RecordingSink::default();
    let algolia_accepted = accept_source_export(
        AsyncMigrationSourceProvider::Algolia,
        &mut algolia,
        &mut algolia_sink,
    )
    .await
    .expect("stable Algolia fixture should reach the shared export seam");

    let meilisearch_documents = vec![
        json!({"sku": "SKU-001", "objectID": "source-owned-field", "title": "Alpha"}),
        json!({"sku": "SKU-002", "title": "Beta"}),
    ];
    let mut meilisearch = MeilisearchSourceReader::from_source(
        "products",
        ScriptedMeilisearchSource::with_passes(
            meilisearch_observation("products", "sku", 2),
            meilisearch_settings(),
            vec![
                vec![meilisearch_documents.clone()],
                vec![meilisearch_documents.clone()],
            ],
        ),
    );
    let mut meilisearch_sink = RecordingSink::default();
    let meilisearch_accepted = accept_source_export(
        AsyncMigrationSourceProvider::Meilisearch,
        &mut meilisearch,
        &mut meilisearch_sink,
    )
    .await
    .expect("stable Meilisearch fixture should reach the shared export seam");

    let typesense_documents = vec![
        json!({"id": "prod_001", "objectID": "source-owned-field", "title": "Alpha"}),
        json!({"id": "prod_002", "title": "Beta"}),
    ];
    let mut typesense = TypesenseSourceReader::from_source(
        "products",
        ScriptedTypesenseSource::with_passes(
            typesense_observation("products", 2),
            typesense_settings(),
            vec![
                vec![typesense_documents.clone()],
                vec![typesense_documents.clone()],
            ],
        ),
    );
    let mut typesense_sink = RecordingSink::default();
    let typesense_accepted = accept_source_export(
        AsyncMigrationSourceProvider::Typesense,
        &mut typesense,
        &mut typesense_sink,
    )
    .await
    .expect("stable Typesense fixture should reach the shared export seam");

    let observed = vec![
        accepted_source_export_projection(&algolia_accepted, &algolia_sink),
        accepted_source_export_projection(&meilisearch_accepted, &meilisearch_sink),
        accepted_source_export_projection(&typesense_accepted, &typesense_sink),
    ];
    let expected = vec![
        neutral_export_bundle_oracle(NeutralExportOracle {
            provider: "algolia",
            namespace: Some("APPID"),
            source_name: "products",
            documents: vec![
                json!({
                    "stableId": "doc-1",
                    "payload": {"objectID": "doc-1", "title": "Keyboard", "available": true},
                }),
                json!({
                    "stableId": "doc-2",
                    "payload": {"objectID": "doc-2", "title": null, "nested": {"b": 2, "a": 1}},
                }),
            ],
            configuration: vec![
                json!({"kind": "settings", "items": [{"ranking": ["typo"], "nested": {"b": 2, "a": 1}}]}),
                json!({"kind": "rules", "items": [{
                    "stableId": "rule-1",
                    "payload": {"objectID": "rule-1", "condition": {"pattern": "sale"}},
                }]}),
                json!({"kind": "synonyms", "items": [{
                    "stableId": "syn-1",
                    "payload": {"objectID": "syn-1", "type": "synonym", "synonyms": ["tee", "shirt"]},
                }]}),
            ],
            accepted_revision: "2026-07-15T00:00:00Z",
            document_count: 2,
            digest: "2d6001d6397c12882b1406e668c51a15d5a99f94a36c2cab6f5c96769ff3796c",
            warning_codes: vec![],
        }),
        neutral_export_bundle_oracle(NeutralExportOracle {
            provider: "meilisearch",
            namespace: None,
            source_name: "products",
            documents: vec![
                json!({
                    "stableId": "SKU-001",
                    "payload": {"sku": "SKU-001", "objectID": "source-owned-field", "title": "Alpha"},
                }),
                json!({
                    "stableId": "SKU-002",
                    "payload": {"sku": "SKU-002", "title": "Beta"},
                }),
            ],
            configuration: vec![
                json!({
                    "kind": "settings",
                    "items": [{
                        "searchableAttributes": ["title", "category"],
                        "pagination": {"maxTotalHits": 50},
                    }],
                }),
                json!({"kind": "rules", "items": []}),
                json!({
                    "kind": "synonyms",
                    "items": [
                        {"stableId": "meilisearch:synonym:saw", "payload": {"saw": ["cutter"]}},
                        {"stableId": "meilisearch:synonym:wrench", "payload": {"wrench": ["spanner"]}},
                    ],
                }),
            ],
            accepted_revision: "2026-07-26T19:20:26Z",
            document_count: 2,
            digest: "1af7139b22f4732130f9ebfd95d274bc6d88d53c73f51e2da90d730b01d717e3",
            warning_codes: vec![
                ReportCode::MeilisearchDocumentOrderNotContractual,
                ReportCode::MeilisearchSearchPaginationNotExportBound,
            ],
        }),
        neutral_export_bundle_oracle(NeutralExportOracle {
            provider: "typesense",
            namespace: None,
            source_name: "products",
            documents: vec![
                json!({
                    "stableId": "prod_001",
                    "payload": {"id": "prod_001", "objectID": "source-owned-field", "title": "Alpha"},
                }),
                json!({
                    "stableId": "prod_002",
                    "payload": {"id": "prod_002", "title": "Beta"},
                }),
            ],
            configuration: vec![
                json!({
                    "kind": "settings",
                    "items": [{
                        "default_sorting_field": "price",
                        "enable_nested_fields": true,
                        "token_separators": ["-"],
                        "symbols_to_index": ["#"],
                    }],
                }),
                json!({"kind": "rules", "items": []}),
                json!({"kind": "synonyms", "items": []}),
            ],
            accepted_revision: "1785020400",
            document_count: 2,
            digest: "6cfe40928589b12b4833860fb9b1e8f157d0c8f0def0e69aacd80e15fe4d3737",
            warning_codes: vec![ReportCode::TypesenseSettingNotMigrated],
        }),
    ];

    assert_eq!(
        observed, expected,
        "neutral source export invariant: all providers must emit the exact provider-tagged bundle oracle"
    );
}

#[tokio::test]
async fn neutral_export_rejects_wrong_provider_identity_before_artifact_capture() {
    /// A reader whose declared provider disagrees with the provider the caller
    /// admitted, wrapping an otherwise healthy Algolia fixture.
    struct ProviderTaggedReader {
        reported_provider: &'static str,
        inner: ScriptedSourceReader,
    }

    struct ProviderExpectationSink {
        expected_provider: &'static str,
        inner: RecordingSink,
    }

    impl MigrationSourceReader for ProviderTaggedReader {
        fn source_provider(&self) -> AsyncMigrationSourceProvider {
            AsyncMigrationSourceProvider::Typesense
        }

        fn source_namespace(&self) -> Option<&str> {
            Some(self.reported_provider)
        }

        fn source_name(&self) -> &str {
            self.inner.source_name()
        }

        fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation> {
            self.inner.observe_quiescent_source()
        }

        fn read_configuration<'a>(
            &'a mut self,
            consume: &'a mut SourceConfigurationConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            self.inner.read_configuration(consume)
        }

        fn read_document_records<'a>(
            &'a mut self,
            consume_page: &'a mut SourceDocumentPageConsumer<'a>,
        ) -> SourceFuture<'a, ()> {
            self.inner.read_document_records(consume_page)
        }
    }

    impl SourceExportSink for ProviderExpectationSink {
        fn commit_configuration(
            &mut self,
            artifact: &SourceConfigurationArtifact,
        ) -> Result<(), SourceExportError> {
            self.inner.commit_configuration(artifact)
        }

        fn commit_document_page(
            &mut self,
            page: &[SourceExportRecord],
        ) -> Result<(), SourceExportError> {
            self.inner.commit_document_page(page)
        }
    }

    let mut reader = stable_reader();
    add_export_pass(&mut reader, document_pages_in_order());
    reader.push_quiescent(stable_record());
    let mut reader = ProviderTaggedReader {
        reported_provider: "typesense",
        inner: reader,
    };
    let mut sink = ProviderExpectationSink {
        expected_provider: "algolia",
        inner: RecordingSink::default(),
    };

    let export_result = accept_source_export(
        AsyncMigrationSourceProvider::Algolia,
        &mut reader,
        &mut sink,
    )
    .await;
    let observed = json!({
        "expectedProvider": sink.expected_provider,
        "readerReportedProvider": reader.source_namespace(),
        "disposition": if export_result.is_ok() { "accepted" } else { "rejected" },
        "error": export_result.err().map(|error| error.safe_message().to_string()),
        "capturedArtifactCount": sink.inner.settings.len()
            + sink.inner.raw_document_pages.len()
            + sink.inner.raw_rule_pages.len()
            + sink.inner.raw_synonym_pages.len(),
    });

    assert_eq!(
        observed,
        json!({
            "expectedProvider": "algolia",
            "readerReportedProvider": "typesense",
            "disposition": "rejected",
            "error": "Source export provider identity mismatch",
            "capturedArtifactCount": 0,
        }),
        "neutral source export invariant: wrong-provider identity must be rejected before artifact capture"
    );
    assert!(sink.inner.settings.is_empty());
    assert!(sink.inner.raw_document_pages.is_empty());
}

#[tokio::test]
async fn neutral_export_preserves_primary_and_derived_settings_and_scrubs_diagnostics() {
    const SECRET: &str = "neutral-export-secret-canary";
    const RAW_URL: &str = "https://source-tenant.example.invalid/private";
    const REPLICA: &str = "products_price_asc";
    let settings = json!({
        "ranking": ["typo"],
        "apiKey": SECRET,
        "endpoint": RAW_URL,
        "replicas": [REPLICA],
    });
    let replica_settings = json!({
        "ranking": ["desc(price)"],
        "apiKey": SECRET,
        "endpoint": RAW_URL,
    });
    let documents = vec![vec![json!({"objectID": "doc-1", "title": "safe"})]];
    let record = AlgoliaIndexRecord {
        entries: 1,
        ..stable_record()
    };
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    reader.push_quiescent(record.clone());
    reader.push_pass(settings.clone(), documents.clone(), vec![], vec![]);
    reader.push_index_settings(REPLICA, Ok(replica_settings.clone()));
    reader.push_pass(settings.clone(), documents, vec![], vec![]);
    reader.push_index_settings(REPLICA, Ok(replica_settings.clone()));
    reader.push_quiescent(record);

    let spool_root = TempDir::new().unwrap();
    let spool = SpoolStore::new(spool_root.path(), SpoolLimits::default()).unwrap();
    let accepted = export_algolia_source(
        &spool,
        Uuid::new_v4(),
        &mut reader,
        AsyncMigrationSourceProvider::Algolia,
    )
    .await
    .expect("seeded canaries should reach the production spool capture seam");

    let accepted_artifacts = spool.accepted_artifacts(accepted.job_uuid).unwrap();
    assert_eq!(
        accepted_artifacts.settings().unwrap(),
        settings,
        "primary source settings are a raw neutral artifact, not a diagnostic surface"
    );

    let durable_replica_settings = accepted_artifacts.replica_settings().unwrap();
    assert_eq!(
        durable_replica_settings.keys().collect::<Vec<_>>(),
        vec![REPLICA],
        "the replica settings must have been captured for this scan to have teeth"
    );
    // Replica settings come from the same AlgoliaClient index-settings read as the
    // primary settings, so they are the same raw source artifact and are captured
    // verbatim. Source-owned fields (even ones named apiKey/endpoint) are the user's
    // own data, not our connection credentials, and must survive to translation.
    assert_eq!(
        durable_replica_settings.get(REPLICA).unwrap(),
        &replica_settings,
        "derived replica settings are a raw neutral artifact, not a diagnostic surface"
    );

    // Redaction stays at the diagnostic boundary: Debug must never echo raw source
    // content, while the durable neutral artifacts stay raw.
    let debug = format!("{accepted:?}");
    assert!(
        !debug.contains(SECRET) && !debug.contains(RAW_URL),
        "neutral source export invariant: credentials and raw source URLs must be absent from Debug"
    );
}

#[tokio::test]
async fn neutral_export_preserves_replica_user_data_urls_verbatim() {
    // A replica whose source-owned `userData` carries URL- and key-named content.
    // The scrub previously deleted these keys and rewrote the URL values, corrupting
    // a valid source artifact before translation and shifting source identity.
    const REPLICA: &str = "products_price_asc";
    let settings = json!({
        "ranking": ["typo"],
        "replicas": [REPLICA],
    });
    let replica_settings = json!({
        "ranking": ["desc(price)"],
        "userData": {
            "url": "https://tenant.example.invalid/catalog",
            "apiKey": "source-owned-not-a-credential",
            "nested": {"endpoint": "https://tenant.example.invalid/nested"}
        }
    });
    let documents = vec![vec![json!({"objectID": "doc-1", "title": "safe"})]];
    let record = AlgoliaIndexRecord {
        entries: 1,
        ..stable_record()
    };
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    reader.push_quiescent(record.clone());
    reader.push_pass(settings.clone(), documents.clone(), vec![], vec![]);
    reader.push_index_settings(REPLICA, Ok(replica_settings.clone()));
    reader.push_pass(settings.clone(), documents, vec![], vec![]);
    reader.push_index_settings(REPLICA, Ok(replica_settings.clone()));
    reader.push_quiescent(record);

    let spool_root = TempDir::new().unwrap();
    let spool = SpoolStore::new(spool_root.path(), SpoolLimits::default()).unwrap();
    let accepted = export_algolia_source(
        &spool,
        Uuid::new_v4(),
        &mut reader,
        AsyncMigrationSourceProvider::Algolia,
    )
    .await
    .expect("replica capture should reach the production spool seam");

    let accepted_artifacts = spool.accepted_artifacts(accepted.job_uuid).unwrap();
    let durable_replica_settings = accepted_artifacts.replica_settings().unwrap();
    assert_eq!(
        durable_replica_settings.get(REPLICA).unwrap(),
        &replica_settings,
        "replica userData must be captured verbatim, keys and URL values intact"
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
        expected_source_identity_digest(ExpectedSourceIdentityDigest {
            app_id: "APPID",
            source_name: "products",
            metadata: &stable_record(),
            settings: &first_identity.snapshot().settings,
            documents: &first_identity.snapshot().documents,
            rules: &first_identity.snapshot().rules,
            synonyms: &first_identity.snapshot().synonyms,
            replica_settings: &first_identity.snapshot().replica_settings,
        })
    );
    assert_eq!(first_identity.accepted_revision(), "2026-07-15T00:00:00Z");
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
        expected_source_identity_digest(ExpectedSourceIdentityDigest {
            app_id: "APPID",
            source_name: "products",
            metadata: &stable_record(),
            settings: &snapshot.settings,
            documents: &snapshot.documents,
            rules: &snapshot.rules,
            synonyms: &snapshot.synonyms,
            replica_settings: &snapshot.replica_settings,
        })
    );

    let v1_document_resource = SourceResourceSnapshot {
        count: 2,
        hash: expected_v1_aggregate_digest(vec![document_one(), document_two()]),
        ids: ["doc-1".to_string(), "doc-2".to_string()].into(),
        version: SourceIdentityVersion::V1,
    };
    assert_ne!(
        identity.digest(),
        expected_source_identity_digest(ExpectedSourceIdentityDigest {
            app_id: "APPID",
            source_name: "products",
            metadata: &stable_record(),
            settings: &snapshot.settings,
            documents: &v1_document_resource,
            rules: &snapshot.rules,
            synonyms: &snapshot.synonyms,
            replica_settings: &snapshot.replica_settings,
        }),
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
#[serial_test::serial(flapjack_outbound_url_policy)]
fn source_reader_algolia_backend_is_constructed_only_through_algolia_client_validation() {
    let _validation_resolver = install_test_algolia_validation_resolver(
        "APPID",
        Some(vec![TEST_VETTED_ALGOLIA_IP]),
        |_host, _port| {},
    );
    let unrelated_error = flapjack::security::vet_outbound_url_target("https://localhost./", false)
        .expect_err("the APPID fixture must preserve unrelated system DNS answers");
    assert!(unrelated_error.contains("private or local destination"));

    let reader = AlgoliaSourceReader::new("APPID", "source-key", "products")
        .expect("valid Algolia source reader should construct");
    assert_eq!(reader.source_namespace(), Some("APPID"));
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

    let accepted = accept_source_export(
        AsyncMigrationSourceProvider::Algolia,
        &mut reader,
        &mut sink,
    )
    .await
    .expect("same source identity should be accepted");

    assert_eq!(
        accepted.identity().digest(),
        expected_source_identity_digest(ExpectedSourceIdentityDigest {
            app_id: "APPID",
            source_name: "products",
            metadata: &stable_record(),
            settings: &accepted.identity().snapshot().settings,
            documents: &accepted.identity().snapshot().documents,
            rules: &accepted.identity().snapshot().rules,
            synonyms: &accepted.identity().snapshot().synonyms,
            replica_settings: &accepted.identity().snapshot().replica_settings,
        })
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Algolia,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("changed source hash must be rejected");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Algolia,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("changed final metadata must be rejected");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
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

    let error = accept_source_export(
        AsyncMigrationSourceProvider::Algolia,
        &mut reader,
        &mut sink,
    )
    .await
    .expect_err("changed source must be rejected after capture");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
    assert_eq!(error.safe_message(), "Source changed during export");
    assert_eq!(sink.settings, vec![settings_fixture()]);
    assert_eq!(sink.document_pages, vec![vec!["doc-1"], vec!["doc-2"]]);
    assert_eq!(sink.rule_pages, vec![vec!["rule-1"]]);
    assert_eq!(sink.synonym_pages, vec![vec!["syn-1"]]);
}

// --- Replica settings collector ---------------------------------------------

/// Drain the replica-settings collector's tagged configuration stream into the
/// name-keyed view these tests assert against.
async fn collected_replica_settings(
    reader: &mut ScriptedSourceReader,
    primary_settings: &Value,
) -> Result<std::collections::BTreeMap<String, Value>, SourceExportError> {
    let mut collected = std::collections::BTreeMap::new();
    {
        let mut consume = |artifact: SourceConfigurationArtifact| {
            if let SourceConfigurationArtifact::ReplicaSettings {
                source_name,
                payload,
            } = artifact
            {
                collected.insert(source_name, payload);
            }
            Ok(())
        };
        collect_replica_settings(reader, primary_settings, &mut consume).await?;
    }
    Ok(collected)
}

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

    let collected = collected_replica_settings(&mut reader, &primary_with_replicas())
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

    let collected = collected_replica_settings(&mut reader, &json!({"ranking": ["typo"]}))
        .await
        .expect("absent replicas must succeed with no reads");

    assert!(collected.is_empty());
    assert_eq!(reader.index_settings_reads.len(), 1);
}

#[tokio::test]
async fn collect_replica_settings_fails_closed_on_missing_script() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    // replicas names a replica, but no settings read was queued.

    let error = collected_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect_err("a missing scripted read must fail closed");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
}

#[tokio::test]
async fn collect_replica_settings_maps_parser_failure_to_scrubbed_validation() {
    let mut reader = ScriptedSourceReader::new("APPID", "products");
    let malformed = json!({"replicas": ["virtual(no-close"]});

    let error = collected_replica_settings(&mut reader, &malformed)
        .await
        .expect_err("an unparseable replica entry must be rejected");

    assert_eq!(error.kind(), SourceExportErrorKind::Validation);
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

    let error = collected_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect_err("a replica fetch error must surface");

    assert_eq!(error.kind(), SourceExportErrorKind::Upstream);
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

    let error = collected_replica_settings(&mut reader, &primary_with_replicas())
        .await
        .expect_err("an out-of-order replica request must fail closed");

    assert_eq!(error.kind(), SourceExportErrorKind::Progress);
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

fn expected_settings_resource_hash(settings: &Value) -> String {
    let mut hasher = Sha256::new();
    update_source_item_hash_digest(&mut hasher, "settings", &source_item_hash(settings));
    hex::encode(hasher.finalize())
}

struct ExpectedSourceIdentityDigest<'a> {
    app_id: &'a str,
    source_name: &'a str,
    metadata: &'a AlgoliaIndexRecord,
    settings: &'a SourceResourceSnapshot,
    documents: &'a SourceResourceSnapshot,
    rules: &'a SourceResourceSnapshot,
    synonyms: &'a SourceResourceSnapshot,
    replica_settings: &'a SourceResourceSnapshot,
}

fn expected_source_identity_digest(spec: ExpectedSourceIdentityDigest<'_>) -> String {
    let identity = json!({
        "provider": "algolia",
        "namespace": spec.app_id,
        "sourceName": spec.source_name,
        "updatedAt": spec.metadata.updated_at,
        "documentMetadataCount": spec.metadata.entries,
        "resources": {
            "settings": expected_resource_identity(spec.settings),
            "documents": expected_resource_identity(spec.documents),
            "rules": expected_resource_identity(spec.rules),
            "synonyms": expected_resource_identity(spec.synonyms),
            "replicaSettings": expected_resource_identity(spec.replica_settings),
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
