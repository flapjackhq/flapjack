//! Orchestration, destination-isolation, sanitization, and resume regressions
//! for the spool-backed Algolia source export.

use super::{
    export_algolia_source, resume_algolia_source, wait_for_live_drift_barrier, ExportError,
    LIVE_DRIFT_BARRIER_DIR_ENV, LIVE_DRIFT_OBSERVED_FILE, LIVE_DRIFT_RELEASE_FILE,
    LIVE_DRIFT_SOURCE_ENV,
};
use crate::handlers::migration::algolia_client::AlgoliaIndexRecord;
use crate::handlers::migration::source_reader::collect_quiescent_source_snapshot;
use crate::handlers::migration::source_test_support::ScriptedSourceReader;
use crate::handlers::migration::spool::{
    ResourceDenominators, SpoolErrorKind, SpoolLimits, SpoolStore,
};
use crate::test_helpers::{EnvVarRestoreGuard, TestStateBuilder, ENV_MUTEX};
use serde_json::{json, Value};
use std::fs;
use std::thread;
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

const APP_ID: &str = "APP-SECRET-ID";
const SOURCE: &str = "products-source";
const PII_CANARY: &str = "PII-CANARY-123";
const SETTINGS_CANARY: &str = "settings-canary";

fn record() -> AlgoliaIndexRecord {
    record_with_entries(3)
}

fn record_with_entries(entries: u64) -> AlgoliaIndexRecord {
    AlgoliaIndexRecord {
        name: SOURCE.to_string(),
        entries,
        updated_at: "2026-07-15T00:00:00Z".to_string(),
        pending_task: false,
    }
}

fn settings() -> Value {
    json!({"attributesForFaceting": ["category"], "note": SETTINGS_CANARY})
}

fn documents() -> Vec<Vec<Value>> {
    vec![
        vec![json!({"objectID": "doc-1", "email": "a@example.com", "ssn": PII_CANARY})],
        vec![
            json!({"objectID": "doc-2", "title": null, "in_stock": true}),
            json!({"objectID": "doc-3", "nested": {"b": 2, "a": 1}}),
        ],
    ]
}

fn documents_with_inserted() -> Vec<Vec<Value>> {
    vec![
        vec![
            json!({"objectID": "doc-1", "email": "a@example.com", "ssn": PII_CANARY}),
            json!({"objectID": "doc-2", "title": null, "in_stock": true}),
        ],
        vec![
            json!({"objectID": "doc-3", "nested": {"b": 2, "a": 1}}),
            json!({"objectID": "doc-4", "title": "Inserted during original run"}),
        ],
    ]
}

fn rules() -> Vec<Vec<Value>> {
    vec![vec![
        json!({"objectID": "rule-1", "condition": {"pattern": "sale"}}),
    ]]
}

fn synonyms() -> Vec<Vec<Value>> {
    vec![vec![
        json!({"objectID": "syn-1", "synonyms": ["tee", "shirt"]}),
    ]]
}

/// A reader supplying one pre-snapshot pass, one export pass, and both quiescence
/// probes — the shape a full fresh or resumed export consumes.
fn full_reader() -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new(APP_ID, SOURCE);
    reader.push_quiescent(record());
    reader.push_pass(settings(), documents(), rules(), synonyms());
    reader.push_pass(settings(), documents(), rules(), synonyms());
    reader.push_quiescent(record());
    reader
}

/// A reader supplying only the single pass a pre-snapshot digest needs.
fn snapshot_reader() -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new(APP_ID, SOURCE);
    reader.push_quiescent(record());
    reader.push_pass(settings(), documents(), rules(), synonyms());
    reader
}

async fn seed_digest() -> (String, ResourceDenominators) {
    seed_digest_for(record(), documents()).await
}

async fn seed_digest_for(
    record: AlgoliaIndexRecord,
    document_pages: Vec<Vec<Value>>,
) -> (String, ResourceDenominators) {
    let mut reader = snapshot_reader();
    reader.quiescent_records = [record].into();
    reader.document_reads = [document_pages].into();
    let identity = collect_quiescent_source_snapshot(&mut reader)
        .await
        .expect("stable source should snapshot");
    let snapshot = identity.snapshot();
    (
        identity.digest().to_string(),
        ResourceDenominators {
            settings: 1,
            documents: snapshot.documents.count as u64,
            rules: snapshot.rules.count as u64,
            synonyms: snapshot.synonyms.count as u64,
            config: 0,
        },
    )
}

fn store_at(path: &std::path::Path) -> SpoolStore {
    SpoolStore::new(path, SpoolLimits::default()).expect("spool store should open")
}

#[test]
fn live_drift_barrier_records_job_and_waits_for_release() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex poisoned");
    let tmp = TempDir::new().unwrap();
    let _source = EnvVarRestoreGuard::set(LIVE_DRIFT_SOURCE_ENV, SOURCE);
    let _dir = EnvVarRestoreGuard::set(
        LIVE_DRIFT_BARRIER_DIR_ENV,
        tmp.path().to_str().expect("temp path should be UTF-8"),
    );

    let job_uuid = Uuid::new_v4();
    let observed = tmp.path().join(LIVE_DRIFT_OBSERVED_FILE);
    let observed_for_thread = observed.clone();
    let release = tmp.path().join(LIVE_DRIFT_RELEASE_FILE);
    let release_thread = thread::spawn(move || {
        for _ in 0..100 {
            if observed_for_thread.exists() {
                fs::write(release, b"").unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("barrier observation file was not created");
    });

    wait_for_live_drift_barrier(SOURCE, job_uuid).expect("release should unblock barrier");
    release_thread.join().unwrap();
    assert_eq!(fs::read_to_string(observed).unwrap(), job_uuid.to_string());
}

#[tokio::test]
async fn export_destination_isolation_and_sanitization_writes_only_spool_job() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let base_path = state.manager.base_path.clone();
    let store = store_at(&base_path);
    let mut reader = full_reader();

    let accepted = export_algolia_source(&store, &mut reader)
        .await
        .expect("stable source should export");

    assert_eq!(accepted.documents, 3);
    assert_eq!(accepted.rules, 1);
    assert_eq!(accepted.synonyms, 1);

    // No destination tenant is created and nothing is written under a target
    // index directory: only the spool root appears beneath the data root.
    assert!(!base_path.join(SOURCE).exists());
    let job_dir = store.job_dir(accepted.job_uuid);
    assert!(job_dir.exists());
    assert!(base_path.join("migration_exports").join("jobs").exists());

    // The raw document artifact preserves the source PII canary verbatim.
    let mut raw = String::new();
    for entry in std::fs::read_dir(&job_dir).unwrap() {
        let path = entry.unwrap().path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("documents-") && name.ends_with(".bin"))
        {
            raw.push_str(&std::fs::read_to_string(&path).unwrap());
        }
    }
    assert!(
        raw.contains(PII_CANARY),
        "raw artifact must retain source PII"
    );

    // Durable acceptance is reachable only through the opaque checkpoint handle.
    let (digest, _) = seed_digest().await;
    let checkpoint = store
        .checkpoint(&accepted.checkpoint_handle, &digest)
        .unwrap();
    assert_eq!(checkpoint.state, "Accepted");
    assert_eq!(checkpoint.job_uuid, accepted.job_uuid);
}

#[tokio::test]
async fn export_destination_isolation_and_sanitization_scrubs_public_material() {
    let tmp = TempDir::new().unwrap();
    let store = store_at(tmp.path());
    let mut reader = full_reader();

    let accepted = export_algolia_source(&store, &mut reader)
        .await
        .expect("stable source should export");
    let (digest, _) = seed_digest().await;

    let public = store.public_view(&accepted.public_handle).unwrap();
    let checkpoint = store
        .checkpoint(&accepted.checkpoint_handle, &digest)
        .unwrap();

    let rendered = format!("{accepted:?}{public:?}{checkpoint:?}");
    for secret in [
        APP_ID,
        SOURCE,
        PII_CANARY,
        SETTINGS_CANARY,
        "doc-1",
        "rule-1",
        "syn-1",
    ] {
        assert!(
            !rendered.contains(secret),
            "public export material must not expose `{secret}`"
        );
    }
}

#[tokio::test]
async fn export_resume_skips_completed_ids_through_checkpoint_handle() {
    let tmp = TempDir::new().unwrap();
    let store = store_at(tmp.path());
    let (digest, denominators) = seed_digest().await;
    let view = store.create_export(&digest, denominators).unwrap();

    // Simulate a crash that already published one document page.
    store
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"doc-1","ssn":"PII-CANARY-123"}]"#,
            &["doc-1"],
        )
        .unwrap();

    let reopened = store_at(tmp.path());
    reopened.recover().unwrap();
    let mut reader = full_reader();
    let accepted = resume_algolia_source(&reopened, &mut reader, &view.checkpoint_handle)
        .await
        .expect("resume should complete the export");

    assert_eq!(accepted.job_uuid, view.job_uuid);
    assert_eq!(accepted.documents, 3);
    let mut completed = reopened.completed_document_ids(view.job_uuid).unwrap();
    completed.sort();
    assert_eq!(completed, vec!["doc-1", "doc-2", "doc-3"]);
    // The already-completed `[doc-1]` page is a true no-op on resume: only the
    // seeded page and the single fresh `[doc-2, doc-3]` page remain visible.
    let document_artifacts = reopened
        .visible_artifacts(view.job_uuid)
        .unwrap()
        .into_iter()
        .filter(|name| name.starts_with("documents-"))
        .count();
    assert_eq!(document_artifacts, 2);
    let checkpoint = reopened
        .checkpoint(&view.checkpoint_handle, &digest)
        .unwrap();
    assert_eq!(checkpoint.state, "Accepted");
}

#[tokio::test]
async fn export_resume_accepts_reordered_inserted_source_and_refuses_mutation() {
    let tmp = TempDir::new().unwrap();
    let store = store_at(tmp.path());
    let source_documents = documents_with_inserted();
    let (digest, denominators) =
        seed_digest_for(record_with_entries(4), source_documents.clone()).await;
    let view = store.create_export(&digest, denominators).unwrap();

    for completed_id in ["doc-1", "doc-2"] {
        let payload = format!(r#"[{{"objectID":"{completed_id}"}}]"#);
        store
            .commit_document_page_with_ids(view.job_uuid, payload.as_bytes(), &[completed_id])
            .unwrap();
    }

    let mut reader = ScriptedSourceReader::new(APP_ID, SOURCE);
    reader.push_quiescent(record_with_entries(4));
    reader.push_pass(settings(), source_documents, rules(), synonyms());
    reader.push_pass(
        settings(),
        vec![
            vec![
                json!({"objectID": "doc-3", "nested": {"a": 1, "b": 2}}),
                json!({"objectID": "doc-2", "title": null, "in_stock": true}),
            ],
            vec![
                json!({"objectID": "doc-4", "title": "Inserted during original run"}),
                json!({"objectID": "doc-1", "email": "a@example.com", "ssn": PII_CANARY}),
            ],
        ],
        rules(),
        synonyms(),
    );
    reader.push_quiescent(record_with_entries(4));

    let accepted = resume_algolia_source(&store, &mut reader, &view.checkpoint_handle)
        .await
        .expect("resume should accept unchanged source identity with shifted pages");

    assert_eq!(accepted.documents, 4);
    assert_eq!(accepted.job_uuid, view.job_uuid);
    let mut completed = store.completed_document_ids(view.job_uuid).unwrap();
    completed.sort();
    assert_eq!(completed, vec!["doc-1", "doc-2", "doc-3", "doc-4"]);
    assert_eq!(
        completed.windows(2).filter(|ids| ids[0] == ids[1]).count(),
        0
    );
    let document_artifacts = store
        .visible_artifacts(view.job_uuid)
        .unwrap()
        .into_iter()
        .filter(|name| name.starts_with("documents-"))
        .count();
    assert_eq!(document_artifacts, 4);
    assert_eq!(
        store
            .checkpoint(&view.checkpoint_handle, &digest)
            .unwrap()
            .state,
        "Accepted"
    );

    let mutation_view = store.create_export(&digest, denominators).unwrap();
    store
        .commit_document_page_with_ids(
            mutation_view.job_uuid,
            br#"[{"objectID":"doc-1"}]"#,
            &["doc-1"],
        )
        .unwrap();
    let artifacts_before = store
        .visible_artifacts(mutation_view.job_uuid)
        .unwrap()
        .len();
    let mut mutated_reader = ScriptedSourceReader::new(APP_ID, SOURCE);
    mutated_reader.push_quiescent(record_with_entries(4));
    mutated_reader.push_pass(
        settings(),
        vec![
            vec![
                json!({"objectID": "doc-1", "email": "a@example.com", "ssn": PII_CANARY}),
                json!({"objectID": "doc-2", "title": null, "in_stock": true}),
            ],
            vec![
                json!({"objectID": "doc-3", "nested": {"b": 2, "a": 1}}),
                json!({"objectID": "doc-4", "title": "Mutated after checkpoint"}),
            ],
        ],
        rules(),
        synonyms(),
    );

    let error = resume_algolia_source(
        &store,
        &mut mutated_reader,
        &mutation_view.checkpoint_handle,
    )
    .await
    .expect_err("changed source identity must refuse resume before streaming");
    assert!(matches!(
        error,
        ExportError::Spool(ref inner) if inner.kind() == SpoolErrorKind::SourceIdentityMismatch
    ));
    assert_eq!(
        store
            .visible_artifacts(mutation_view.job_uuid)
            .unwrap()
            .len(),
        artifacts_before
    );
    assert_eq!(
        store
            .checkpoint(&mutation_view.checkpoint_handle, &digest)
            .unwrap()
            .state,
        "Running"
    );
}

#[tokio::test]
async fn export_resume_refuses_mutated_source_without_new_artifacts() {
    let tmp = TempDir::new().unwrap();
    let store = store_at(tmp.path());
    let (digest, denominators) = seed_digest().await;
    let view = store.create_export(&digest, denominators).unwrap();
    store
        .commit_document_page_with_ids(view.job_uuid, br#"[{"objectID":"doc-1"}]"#, &["doc-1"])
        .unwrap();
    let artifacts_before = store.visible_artifacts(view.job_uuid).unwrap().len();

    // A reader whose documents mutate produces a different source identity.
    let mut reader = ScriptedSourceReader::new(APP_ID, SOURCE);
    reader.push_quiescent(record());
    let mutated = vec![vec![
        json!({"objectID": "doc-1", "ssn": "DIFFERENT-VALUE"}),
        json!({"objectID": "doc-2"}),
        json!({"objectID": "doc-3"}),
    ]];
    reader.push_pass(settings(), mutated, rules(), synonyms());

    let error = resume_algolia_source(&store, &mut reader, &view.checkpoint_handle)
        .await
        .expect_err("a changed source identity must be refused");
    assert!(matches!(
        error,
        ExportError::Spool(ref inner) if inner.kind() == SpoolErrorKind::SourceIdentityMismatch
    ));

    // The refusal advanced no counters, sidecars, or artifacts, and left the job
    // resumable under its original identity.
    assert_eq!(
        store.visible_artifacts(view.job_uuid).unwrap().len(),
        artifacts_before
    );
    let checkpoint = store.checkpoint(&view.checkpoint_handle, &digest).unwrap();
    assert_eq!(checkpoint.state, "Running");
}

#[tokio::test]
async fn export_drift_during_streaming_fences_the_job() {
    let tmp = TempDir::new().unwrap();
    let store = store_at(tmp.path());

    // Same membership on both passes, but the final quiescence reports a changed
    // updatedAt, which the two-pass contract treats as source drift.
    let mut reader = ScriptedSourceReader::new(APP_ID, SOURCE);
    reader.push_quiescent(record());
    reader.push_pass(settings(), documents(), rules(), synonyms());
    reader.push_pass(settings(), documents(), rules(), synonyms());
    let mut drifted = record();
    drifted.updated_at = "2026-07-15T01:00:00Z".to_string();
    reader.push_quiescent(drifted);

    let error = export_algolia_source(&store, &mut reader)
        .await
        .expect_err("final metadata drift must be rejected");
    assert!(matches!(error, ExportError::Source(_)));

    // The fenced job is durably failed, never left apparently complete.
    let uuids = store.job_uuids().unwrap();
    assert_eq!(uuids.len(), 1);
    let (public_handle, _) = store.handles(uuids[0]).unwrap();
    assert_eq!(store.public_view(&public_handle).unwrap().state, "Failed");
}
