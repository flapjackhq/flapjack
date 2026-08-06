use super::algolia_client::AlgoliaIndexRecord;
use super::meilisearch_client::MEILISEARCH_PREVIEW_LOOPBACK_ENV;
use super::source_reader::{
    SourceConfigurationArtifact, SourceConfigurationRecord, SourceExportSink,
};
use super::source_test_support::ScriptedSourceReader;
use super::translation::tests::spool_payload;
use super::translation::{translate_spool_payload, TranslationOutcome, TranslationReportEntry};
use super::{AsyncMigrationSourceProvider, PreviewSourceExport, TestMigrationSourceReaderFactory};
use crate::auth::KeyStore;
use crate::test_helpers::{body_json, build_test_router, with_env_var};
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

mod meilisearch;
mod typesense;

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

fn live_preview_receipt(status: StatusCode, body: &Value, report_codes: Vec<String>) -> Value {
    json!({
        "previewProof": "PASS",
        "previewStatus": status.as_u16(),
        "previewBody": body,
        "sourceCounts": body["sourceCounts"],
        "reportCodes": report_codes,
    })
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

async fn seed_durable_state_specimens(root: &Path) -> BTreeMap<String, DurableStateSpecimen> {
    seed_destination_index(root, "shop", "target-before-preview").await;
    seed_destination_index(root, "bystander", "bystander-before-preview").await;
    fs::create_dir_all(root.join("migration_exports/jobs")).unwrap();
    fs::write(
        root.join("migration_exports/jobs/existing_job.json"),
        b"existing job sentinel",
    )
    .unwrap();
    fs::write(
        root.join("migration_exports/owner_metadata.json"),
        b"migration export root metadata sentinel",
    )
    .unwrap();
    fs::create_dir_all(root.join(".publication/existing_transaction")).unwrap();
    fs::write(
        root.join(".publication/existing_transaction/manifest.json"),
        b"publication sentinel",
    )
    .unwrap();

    let before = snapshot_durable_state(root);
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
    before
}

async fn assert_preview_preserves_durable_state(
    root: &Path,
    before: BTreeMap<String, DurableStateSpecimen>,
    response: axum::response::Response,
) {
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(
        body.get("jobId").is_none(),
        "preview must return an advisory report, not an admitted migration receipt"
    );
    let after = snapshot_durable_state(root);
    assert_eq!(after.len(), before.len(), "durable specimen count changed");
    assert_eq!(
        after, before,
        "preview crossed a durable job, spool, staging, publication, target, or bystander owner"
    );
}

async fn post_migration_route(
    app: &axum::Router,
    path: String,
    body: Value,
) -> axum::response::Response {
    app.clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("x-algolia-api-key", "admin-key")
                .header("x-algolia-application-id", "preview-contract-app")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn post_provider_preview(
    app: &axum::Router,
    provider: &str,
    body: Value,
) -> axum::response::Response {
    post_migration_route(app, format!("/1/migrations/{provider}/preview"), body).await
}

fn algolia_preview_app(tmp: &TempDir) -> axum::Router {
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let source_factory = TestMigrationSourceReaderFactory::new(|source_provider| {
        assert_eq!(source_provider, AsyncMigrationSourceProvider::Algolia);
        Ok(Box::new(preview_source_reader()))
    });
    build_test_router(tmp, Some(key_store)).layer(Extension(source_factory))
}

fn sorted_object_keys(value: &Value) -> Vec<&str> {
    let mut keys: Vec<&str> = value
        .as_object()
        .unwrap_or_else(|| panic!("expected a JSON object, got {value}"))
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    keys
}

async fn post_provider_submit(
    app: &axum::Router,
    provider: &str,
    body: Value,
) -> axum::response::Response {
    post_migration_route(app, format!("/1/migrations/{provider}"), body).await
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
    let app = algolia_preview_app(&tmp);

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

#[test]
fn meilisearch_preview_translation_preserves_native_synonym_stable_id() {
    let stable_id = "meilisearch:synonym:url";
    let native_payload = json!({
        "url": ["https://search.example/synonym", "address"]
    });
    let artifact =
        SourceConfigurationArtifact::synonym_records(vec![SourceConfigurationRecord::new(
            stable_id.to_string(),
            native_payload.clone(),
        )
        .unwrap()]);
    let mut export = PreviewSourceExport::default();
    export
        .commit_configuration(&SourceConfigurationArtifact::settings(&json!({})))
        .unwrap();
    export.commit_configuration(&artifact).unwrap();
    assert_eq!(
        export.synonym_pages,
        vec![vec![native_payload]],
        "preview capture must preserve provider-native synonym vocabulary byte-for-value"
    );

    let outcome = translate_spool_payload(export.into_translation_input(
        "products".to_string(),
        "shop".to_string(),
        AsyncMigrationSourceProvider::Meilisearch,
    ));
    let translated = match outcome {
        TranslationOutcome::Translated(translated) => translated,
        TranslationOutcome::Rejected(report) => {
            panic!("native Meilisearch synonym must translate, got {report:#?}")
        }
    };

    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(translated.bundle.synonyms.len(), 1);
    assert_eq!(translated.bundle.synonyms[0].object_id(), stable_id);
    assert_eq!(
        serde_json::to_value(&translated.bundle.synonyms[0]).unwrap(),
        json!({
            "type": "synonym",
            "objectID": stable_id,
            "synonyms": ["url", "https://search.example/synonym", "address"]
        })
    );
}

#[test]
fn meilisearch_preview_preserves_provider_native_settings_payload() {
    let native_settings = json!({
        "searchableAttributes": ["title"],
        "displayedAttributes": ["url", "apiKey", "title"],
        "stopWords": ["https://search.example/settings", "apiKey"]
    });
    let mut export = PreviewSourceExport::default();
    export
        .commit_configuration(&SourceConfigurationArtifact::settings(&native_settings))
        .unwrap();
    assert_eq!(
        export.settings, native_settings,
        "preview capture must not redact source-owned provider settings"
    );

    let outcome = translate_spool_payload(export.into_translation_input(
        "products".to_string(),
        "shop".to_string(),
        AsyncMigrationSourceProvider::Meilisearch,
    ));
    let translated = match outcome {
        TranslationOutcome::Translated(translated) => translated,
        TranslationOutcome::Rejected(report) => {
            panic!("native Meilisearch settings must not hard-reject, got {report:#?}")
        }
    };

    assert_eq!(translated.report.summary.hard_rejections, 0);
    assert_eq!(
        translated.bundle.settings.searchable_attributes,
        Some(vec!["title".to_string()])
    );
}

/// `flapjack-server/src/migrate.rs` declares its own deserialize-only
/// `MigrationPreviewResponse` because this owner is serialize-only with private
/// fields, so the two shapes have no compiler-enforced link. This test is that
/// link: renaming or dropping a key here fails now, instead of leaving
/// `flapjack migrate preview` to fail at runtime against a real server with
/// "migration preview returned an incompatible response". Exact key sets are
/// pinned only where the CLI model declares every key; entries carry
/// server-only keys, so they are pinned per CLI-consumed key.
#[tokio::test]
async fn preview_response_pins_the_json_keys_the_migrate_cli_deserializes() {
    let tmp = TempDir::new().unwrap();
    let app = algolia_preview_app(&tmp);

    let response = post_preview(&app).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;

    assert_eq!(sorted_object_keys(&body), ["report", "sourceCounts"]);
    assert_eq!(
        sorted_object_keys(&body["report"]),
        ["entries", "reportDigest", "summary"]
    );
    assert_eq!(
        sorted_object_keys(&body["sourceCounts"]),
        ["indexes", "records"]
    );
    for count in ["indexes", "records"] {
        assert!(
            body["sourceCounts"][count].is_u64(),
            "the CLI model deserializes sourceCounts.{count} as usize"
        );
    }

    let summary = &body["report"]["summary"];
    assert_eq!(
        sorted_object_keys(summary),
        ["hardRejections", "scopeGaps", "totalEntries", "warnings"]
    );
    for count in ["hardRejections", "scopeGaps", "totalEntries", "warnings"] {
        assert!(
            summary[count].is_u64(),
            "the CLI model deserializes summary.{count} as usize, and gates its exit code on \
             hardRejections"
        );
    }
    assert!(
        body["report"]["reportDigest"].is_string() || body["report"]["reportDigest"].is_null(),
        "the CLI model deserializes reportDigest as Option<String>"
    );

    let entry = body["report"]["entries"]
        .get(0)
        .expect("the preview fixture must report at least one entry");
    // Entries carry keys the CLI's model does not declare (`pageIndex`,
    // `itemIndex`), and serde ignores them, so pin presence and type of the
    // four keys the CLI actually deserializes instead of the whole key set.
    for rendered in ["code", "jsonPath", "resource", "severity"] {
        assert!(
            entry.get(rendered).is_some_and(Value::is_string),
            "the CLI deserializes and renders entry.{rendered} as a String, got {:?}",
            entry.get(rendered)
        );
    }
}

#[tokio::test]
async fn preview_does_not_write_durable_state_byte_identity() {
    let tmp = TempDir::new().unwrap();
    let app = algolia_preview_app(&tmp);
    let before = seed_durable_state_specimens(tmp.path()).await;
    let response = post_preview(&app).await;
    assert_preview_preserves_durable_state(tmp.path(), before, response).await;
}

#[tokio::test]
async fn preview_leaves_absent_target_and_unopened_publication_namespace_absent() {
    let tmp = TempDir::new().unwrap();
    let app = algolia_preview_app(&tmp);
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
#[ignore = "invoked by tests/meilisearch_source_contract_kat.sh --preview-live"]
async fn meilisearch_live_preview_reports_exact_seeded_counts_and_codes() {
    let _env = with_env_var(MEILISEARCH_PREVIEW_LOOPBACK_ENV, "1");
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

    let entries = body["report"]["entries"]
        .as_array()
        .expect("preview report entries must be an array")
        .clone();
    let report_codes = entries
        .iter()
        .map(|entry| {
            entry["code"]
                .as_str()
                .expect("preview report code must be a string")
                .to_string()
        })
        .collect::<Vec<_>>();
    // The report is sorted by (severity, resource, jsonPath, code), so the settings
    // warnings interleave by path rather than by code. The seeded fixture PATCHes
    // `typoTolerance.disableOnWords: ["SKU"]`, which the live server stores
    // lowercased, so the GET response carries no value-normalization warning.
    let report_contract = entries
        .iter()
        .map(|entry| {
            (
                entry["code"]
                    .as_str()
                    .expect("preview report code must be a string"),
                entry["jsonPath"]
                    .as_str()
                    .expect("preview report jsonPath must be a string"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        report_contract,
        vec![
            ("ProductNotMigrated", "$"),
            ("ProductNotMigrated", "$"),
            ("ProductNotMigrated", "$"),
            ("ProductNotMigrated", "$"),
            ("ProductNotMigrated", "$"),
            ("MeilisearchSettingNotMigrated", "$.dictionary"),
            ("MeilisearchDocumentOrderNotContractual", "$.documents"),
            ("MeilisearchSettingNotMigrated", "$.facetSearch"),
            ("MeilisearchSettingNotMigrated", "$.nonSeparatorTokens"),
            ("MeilisearchSearchPaginationNotExportBound", "$.pagination"),
            ("MeilisearchSettingNotMigrated", "$.prefixSearch"),
            ("MeilisearchSettingNotMigrated", "$.proximityPrecision"),
            ("MeilisearchSettingNotMigrated", "$.rankingRules[5]"),
            ("MeilisearchSettingNotMigrated", "$.sortableAttributes"),
            ("MeilisearchSettingNotMigrated", "$.stopWords"),
        ],
        "live preview codes must remain owned by the existing Meilisearch translation matrix and live settings warning contract"
    );
    println!("{}", live_preview_receipt(status, &body, report_codes));
}

#[test]
fn live_preview_receipt_includes_status_body_counts_and_codes() {
    let body = json!({
        "sourceCounts": {"indexes": 1, "records": 3},
        "report": {"entries": []}
    });

    assert_eq!(
        live_preview_receipt(
            StatusCode::OK,
            &body,
            vec!["ProductNotMigrated".to_string()]
        ),
        json!({
            "previewProof": "PASS",
            "previewStatus": 200,
            "previewBody": body,
            "sourceCounts": {"indexes": 1, "records": 3},
            "reportCodes": ["ProductNotMigrated"],
        })
    );
}
