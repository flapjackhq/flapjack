use super::algolia_client::AlgoliaIndexRecord;
use super::source_test_support::ScriptedSourceReader;
use super::translation::tests::spool_payload;
use super::translation::{translate_spool_payload, TranslationOutcome, TranslationReportEntry};
use super::{
    AsyncMigrationSourceProvider, TestMigrationSourceReaderFactory,
    SOURCE_PROVIDER_UNSUPPORTED_MESSAGE,
};
use crate::auth::KeyStore;
use crate::test_helpers::{body_json, build_test_router};
use axum::body::Body;
use axum::extract::Extension;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

const SINGLE_REQUESTED_SOURCE_INDEX_COUNT: usize = 1;
const SOURCE_METADATA_RECORD_COUNT: u64 = 97;
const SERVED_SOURCE_RECORD_COUNT: usize = 3;
const MEILISEARCH_LIVE_ENDPOINT_ENV: &str = "FJ_MEILISEARCH_PREVIEW_ENDPOINT";
const MEILISEARCH_LIVE_API_KEY_ENV: &str = "FJ_MEILISEARCH_PREVIEW_API_KEY";
const MEILISEARCH_LIVE_EXPECTED_RECORDS_ENV: &str = "FJ_MEILISEARCH_PREVIEW_EXPECTED_RECORDS";
// 51 = two 21-entry IndexManager index trees + three publication-namespace
// entries + two KeyStore files + four migration-export entries.
const DURABLE_STATE_SPECIMEN_COUNT: usize = 51;

#[derive(Debug, Clone, PartialEq, Eq)]
struct DurableStateSpecimen {
    kind: &'static str,
    sha256: Option<[u8; 32]>,
}

fn preview_settings() -> Value {
    json!({
        "searchableAttributes": ["title"],
        "allowCompressionOfIntegerArray": false
    })
}

fn preview_document_pages() -> Vec<Vec<Value>> {
    vec![
        vec![
            json!({"objectID": "record-1", "title": "First"}),
            json!({"objectID": "record-2", "title": "Second"}),
        ],
        vec![json!({"objectID": "record-3", "title": "Third"})],
    ]
}

fn preview_source_reader() -> ScriptedSourceReader {
    let settings = preview_settings();
    let document_pages = preview_document_pages();
    let source_record = AlgoliaIndexRecord {
        name: "products".to_string(),
        // Deliberately differs from the three documents served below. Preview
        // sourceCounts.records owns observed export output, not source metadata.
        entries: SOURCE_METADATA_RECORD_COUNT,
        updated_at: "2026-07-30T00:00:00Z".to_string(),
        pending_task: false,
    };
    let mut reader = ScriptedSourceReader::new("PREVIEW_FIXTURE", "products");
    reader.push_quiescent(source_record.clone());
    reader.push_pass(settings.clone(), document_pages.clone(), vec![], vec![]);
    reader.push_pass(settings, document_pages, vec![], vec![]);
    reader.push_quiescent(source_record);
    reader
}

fn preview_entry_json(entry: &TranslationReportEntry) -> Value {
    json!({
        "severity": entry.severity,
        "code": entry.code,
        "resource": entry.resource,
        "pageIndex": entry.page_index,
        "itemIndex": entry.item_index,
        "jsonPath": entry.json_path,
    })
}

fn expected_preview_entries() -> Vec<Value> {
    let outcome = translate_spool_payload(spool_payload(
        preview_settings(),
        preview_document_pages(),
        vec![],
        vec![],
    ));
    let report = match outcome {
        TranslationOutcome::Translated(translated) => translated.report,
        TranslationOutcome::Rejected(report) => {
            panic!("warning-only preview fixture must translate, got {report:#?}")
        }
    };
    report.entries.iter().map(preview_entry_json).collect()
}

async fn post_preview(app: &axum::Router) -> axum::response::Response {
    post_provider_preview(
        app,
        "algolia",
        json!({
            "appId": "PREVIEW_FIXTURE",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "shop"
        }),
    )
    .await
}

fn snapshot_durable_state(root: &Path) -> BTreeMap<String, DurableStateSpecimen> {
    fn visit(root: &Path, current: &Path, snapshot: &mut BTreeMap<String, DurableStateSpecimen>) {
        let mut entries = fs::read_dir(current)
            .unwrap_or_else(|error| panic!("{} must remain readable: {error}", current.display()))
            .map(|entry| entry.expect("durable-state directory entry must remain readable"))
            .collect::<Vec<_>>();
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .expect("snapshot specimen must stay under the temp data root")
                .to_string_lossy()
                .into_owned();
            let file_type = entry
                .file_type()
                .expect("durable-state specimen kind must remain readable");
            if file_type.is_dir() {
                snapshot.insert(
                    relative,
                    DurableStateSpecimen {
                        kind: "directory",
                        sha256: None,
                    },
                );
                visit(root, &path, snapshot);
            } else if file_type.is_file() {
                let bytes = fs::read(&path).unwrap_or_else(|error| {
                    panic!("{} must remain readable: {error}", path.display())
                });
                snapshot.insert(
                    relative,
                    DurableStateSpecimen {
                        kind: "file",
                        sha256: Some(Sha256::digest(bytes).into()),
                    },
                );
            } else {
                panic!("unexpected durable-state specimen kind at {relative}");
            }
        }
    }

    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}

async fn seed_destination_index(root: &Path, index_name: &str, object_id: &str) {
    let manager = flapjack::IndexManager::new(root);
    manager
        .create_tenant(index_name)
        .expect("durable-state fixture index must be created by IndexManager");
    manager
        .add_documents_sync(
            index_name,
            vec![flapjack::types::Document {
                id: object_id.to_string(),
                fields: HashMap::from([(
                    "title".to_string(),
                    flapjack::types::FieldValue::Text(format!("fixture {object_id}")),
                )]),
            }],
        )
        .await
        .expect("durable-state fixture document must be committed");
    let quiesce = manager
        .quiesce_tenant(&index_name.to_string())
        .await
        .expect("durable-state fixture index must reach merge quiescence");
    drop(quiesce);
    drop(manager);
}

async fn post_provider_preview(
    app: &axum::Router,
    provider: &str,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/migrations/{provider}/preview"))
                .header("content-type", "application/json")
                .header("x-algolia-api-key", "admin-key")
                .header("x-algolia-application-id", "preview-contract-app")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn preview_http_report_matches_translation_owner_and_exact_source_counts() {
    let expected_entries = expected_preview_entries();
    assert!(
        expected_entries.contains(&json!({
            "severity": "Warning",
            "code": "PersistedNoBehaviorSetting",
            "resource": "Settings",
            "pageIndex": null,
            "itemIndex": null,
            "jsonPath": "$.allowCompressionOfIntegerArray"
        })),
        "the fixture must exercise the persisted-without-behavior warning contract"
    );

    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let source_factory = TestMigrationSourceReaderFactory::new(|source_provider| {
        assert_eq!(source_provider, AsyncMigrationSourceProvider::Algolia);
        Ok(Box::new(preview_source_reader()))
    });
    let app = build_test_router(&tmp, Some(key_store)).layer(Extension(source_factory));

    let response = post_preview(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    // mod.rs::preview_source_migration stops at
    // translation_session.rs::translate_spool_report. It must not call
    // mod.rs::submit_source_migration_impl,
    // MigrationJobRunner::submit_source_import_for_owner,
    // import.rs::spool_for_manager, or import.rs::import_accepted_export_inner.
    assert_eq!(body["report"]["entries"], Value::Array(expected_entries));
    assert_eq!(
        body["sourceCounts"],
        json!({
            // A preview request accepts exactly one sourceIndex, so the index
            // count is fixed by the request contract rather than discovery.
            "indexes": SINGLE_REQUESTED_SOURCE_INDEX_COUNT,
            "records": SERVED_SOURCE_RECORD_COUNT
        })
    );
}

#[tokio::test]
async fn preview_does_not_write_durable_state_byte_identity() {
    let tmp = TempDir::new().unwrap();
    seed_destination_index(tmp.path(), "shop", "target-before-preview").await;
    seed_destination_index(tmp.path(), "bystander", "bystander-before-preview").await;

    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let source_factory = TestMigrationSourceReaderFactory::new(|source_provider| {
        assert_eq!(source_provider, AsyncMigrationSourceProvider::Algolia);
        Ok(Box::new(preview_source_reader()))
    });
    let app = build_test_router(&tmp, Some(key_store)).layer(Extension(source_factory));

    fs::create_dir_all(tmp.path().join("migration_exports/jobs")).unwrap();
    fs::write(
        tmp.path().join("migration_exports/jobs/existing_job.json"),
        b"existing job sentinel",
    )
    .unwrap();
    fs::write(
        tmp.path().join("migration_exports/owner_metadata.json"),
        b"migration export root metadata sentinel",
    )
    .unwrap();
    fs::create_dir_all(tmp.path().join(".publication/existing_transaction")).unwrap();
    fs::write(
        tmp.path()
            .join(".publication/existing_transaction/manifest.json"),
        b"publication sentinel",
    )
    .unwrap();

    let before = snapshot_durable_state(tmp.path());
    assert_eq!(
        before.len(),
        DURABLE_STATE_SPECIMEN_COUNT,
        "durable specimen inventory drifted"
    );
    for required in [
        "migration_exports/jobs",
        "migration_exports/jobs/existing_job.json",
        "migration_exports/owner_metadata.json",
        ".publication",
        ".publication/existing_transaction/manifest.json",
        "shop",
        "bystander",
    ] {
        assert!(
            before.contains_key(required),
            "required nonzero durable-state surface missing from specimen: {required}"
        );
    }
    println!("durable_state_specimens={}", before.len());

    let response = post_preview(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert!(
        body.get("jobId").is_none(),
        "preview must return an advisory report, not an admitted migration receipt"
    );
    let after = snapshot_durable_state(tmp.path());
    assert_eq!(after.len(), before.len(), "durable specimen count changed");
    assert_eq!(
        after, before,
        "preview crossed a durable job, spool, staging, publication, target, or bystander owner"
    );
}

#[tokio::test]
async fn preview_leaves_absent_target_and_unopened_publication_namespace_absent() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let source_factory = TestMigrationSourceReaderFactory::new(|source_provider| {
        assert_eq!(source_provider, AsyncMigrationSourceProvider::Algolia);
        Ok(Box::new(preview_source_reader()))
    });
    let app = build_test_router(&tmp, Some(key_store)).layer(Extension(source_factory));
    let target_index = tmp.path().join("shop");
    let publication_namespace = tmp.path().join(".publication");

    assert!(!target_index.exists(), "target must start absent");
    assert!(
        !publication_namespace.exists(),
        "publication namespace must start unopened"
    );

    let response = post_preview(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert!(
        body.get("jobId").is_none(),
        "preview must return an advisory report, not an admitted migration receipt"
    );
    assert!(
        !target_index.exists(),
        "preview must not create, stage, or publish an absent target index"
    );
    assert!(
        !publication_namespace.exists(),
        "preview must not prepare an unopened publication namespace"
    );
}

#[tokio::test]
async fn typesense_preview_returns_exact_unsupported_provider_error() {
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_preview(
        &app,
        "typesense",
        json!({
            "appId": "typesense-fixture",
            "apiKey": "source-key",
            "sourceIndex": "products",
            "targetIndex": "shop"
        }),
    )
    .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = body_json(response).await;
    assert_eq!(body["code"], "source_provider_unsupported");
    assert_eq!(body["message"], SOURCE_PROVIDER_UNSUPPORTED_MESSAGE);
}

#[tokio::test]
#[ignore = "invoked by tests/meilisearch_source_contract_kat.sh --preview-live"]
async fn meilisearch_live_preview_reports_exact_seeded_counts_and_codes() {
    let endpoint = env::var(MEILISEARCH_LIVE_ENDPOINT_ENV)
        .expect("Meilisearch preview endpoint must be supplied by the live KAT owner");
    let source_api_key = env::var(MEILISEARCH_LIVE_API_KEY_ENV)
        .expect("Meilisearch preview API key must be supplied by the live KAT owner");
    let expected_record_count = env::var(MEILISEARCH_LIVE_EXPECTED_RECORDS_ENV)
        .expect("Meilisearch preview record count must be supplied by the live KAT fixture owner")
        .parse::<u64>()
        .expect("Meilisearch preview record count must be an unsigned integer");
    let tmp = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let app = build_test_router(&tmp, Some(key_store));

    let response = post_provider_preview(
        &app,
        "meilisearch",
        json!({
            "endpoint": endpoint,
            "apiKey": source_api_key,
            "sourceIndex": "configured_pk",
            "targetIndex": "meilisearch_preview_target"
        }),
    )
    .await;
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "preview error body: {body}");
    assert_eq!(
        body["sourceCounts"],
        json!({"indexes": 1, "records": expected_record_count})
    );

    let report_codes = body["report"]["entries"]
        .as_array()
        .expect("preview report entries must be an array")
        .iter()
        .map(|entry| {
            entry["code"]
                .as_str()
                .expect("preview report code must be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        report_codes,
        vec![
            "ProductNotMigrated",
            "ProductNotMigrated",
            "ProductNotMigrated",
            "ProductNotMigrated",
            "ProductNotMigrated",
            "MeilisearchDocumentOrderNotContractual",
            "MeilisearchSearchPaginationNotExportBound",
            "MeilisearchSettingNotMigrated",
            "MeilisearchSettingNotMigrated",
            "MeilisearchSettingNotMigrated",
            "MeilisearchSettingNotMigrated",
            "MeilisearchSettingNotMigrated",
            "MeilisearchSettingValueNormalized",
        ],
        "live preview codes must remain owned by the existing Meilisearch translation matrix"
    );
    println!(
        "{}",
        json!({
            "previewProof": "PASS",
            "sourceCounts": body["sourceCounts"],
            "reportCodes": report_codes,
        })
    );
}
