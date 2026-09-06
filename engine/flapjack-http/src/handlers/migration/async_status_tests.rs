//! Stub summary for engine/flapjack-http/src/handlers/migration/async_status_tests.rs.
use super::*;
use crate::auth::AuthenticatedAppId;
use crate::handlers::indices::list_indices;
use crate::handlers::migration::algolia_client::AlgoliaIndexRecord;
use crate::handlers::migration::source_reader::{
    MeilisearchSourceReader, MigrationSourceReader, SourceConfigurationConsumer,
    SourceDocumentPageConsumer, SourceFuture, SourceObservation, TypesenseSourceReader,
};
use crate::handlers::migration::source_test_support::{
    meilisearch_observation, sorted_exact_hits_by_object_id, typesense_observation,
    ScriptedMeilisearchSource, ScriptedSourceReader, ScriptedTypesenseSource,
};
use crate::handlers::migration::spool::{
    AsyncMigrationPublicationSemantic, MigrationDisposition, MigrationExportProgress,
    MigrationImportOutcome, MigrationImportWarning, MigrationPhase, MigrationPhaseRecord,
};
use crate::test_helpers::{body_json, SharedLogBuffer, TestStateBuilder};
use axum::body::Body;
use axum::extract::{Path as AxumPath, Query};
use axum::http::{Method, Request, Response};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use chrono::{TimeZone, Utc};
use flapjack::index::manager::publication::{
    ContentDigest, PublicationEvent, PublicationGenerationEvidence, PublicationJournal,
    PublicationPaths, PublicationTarget, PublicationTransactionId,
};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;
use uuid::Uuid;

const ASYNC_STATUS_TERMINAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const ASYNC_LIFECYCLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

fn assert_import_outcome_fields_absent(body: &serde_json::Value) {
    assert!(body.get("settingsApplied").is_none());
    assert!(body.get("objectsImported").is_none());
    assert!(body.get("synonymsImported").is_none());
    assert!(body.get("rulesImported").is_none());
    assert!(body.get("warnings").is_none());
}

/// TODO: Document async_migration_status_response_wire_contract_has_no_overall_progress.
#[test]
fn async_migration_status_response_wire_contract_has_no_overall_progress() {
    let job_uuid = Uuid::parse_str("01890f8e-8b28-78e8-b542-8cfdcb2d4f24").unwrap();
    let record = MigrationPhaseRecord {
        job_uuid,
        phase: MigrationPhase::Exporting,
        disposition: MigrationDisposition::Running,
        cancel_requested: true,
        export_progress: Some(MigrationExportProgress {
            completed: 7,
            total: 11,
        }),
        created_at: Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 7, 15, 12, 1, 0).unwrap(),
        terminal_at: None,
        import_outcome: None,
    };

    let body = serde_json::to_value(AsyncMigrationStatusResponse::from(record)).unwrap();

    assert_eq!(
        body,
        json!({
            "jobId": "01890f8e-8b28-78e8-b542-8cfdcb2d4f24",
            "phase": "exporting",
            "disposition": "running",
            "exportProgress": {
                "completed": 7,
                "total": 11
            },
            "createdAt": "2026-07-15T12:00:00Z",
            "updatedAt": "2026-07-15T12:01:00Z"
        })
    );
    assert!(body.get("terminalAt").is_none());
    assert!(body.get("progress").is_none());
    assert!(body.get("overallProgress").is_none());
    assert!(body.get("cancelRequested").is_none());
    assert!(body.get("cancel_requested").is_none());
    assert!(body["exportProgress"].get("ratio").is_none());
    assert_import_outcome_fields_absent(&body);
}

#[test]
fn async_running_status_hides_pre_recorded_import_outcome() {
    let job_uuid = Uuid::parse_str("01890f8e-8b28-78e8-b542-8cfdcb2d4f24").unwrap();
    let record = MigrationPhaseRecord {
        job_uuid,
        phase: MigrationPhase::Activating,
        disposition: MigrationDisposition::Running,
        cancel_requested: false,
        export_progress: None,
        created_at: Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        updated_at: Utc.with_ymd_and_hms(2026, 7, 15, 12, 1, 0).unwrap(),
        terminal_at: None,
        import_outcome: Some(MigrationImportOutcome {
            settings_applied: true,
            objects_imported: 0,
            synonyms_imported: 1,
            rules_imported: 2,
            warnings: vec![MigrationImportWarning {
                code: "PersistedNoBehaviorSetting".to_string(),
                message:
                    "Source setting is preserved for compatibility but has no Flapjack behavior."
                        .to_string(),
                resource: "Settings".to_string(),
                page_index: None,
                item_index: None,
                json_path: "$.hitsPerPage".to_string(),
            }],
        }),
    };

    let body = serde_json::to_value(AsyncMigrationStatusResponse::from(record)).unwrap();

    assert_eq!(body["phase"], "activating");
    assert_eq!(body["disposition"], "running");
    assert_import_outcome_fields_absent(&body);
}

/// TODO: Document async_migration_status_response_serializes_cancelled_terminal_disposition.
#[test]
fn async_migration_status_response_serializes_cancelled_terminal_disposition() {
    let job_uuid = Uuid::parse_str("01890f8e-8b28-78e8-b542-8cfdcb2d4f24").unwrap();
    let terminal_at = Utc.with_ymd_and_hms(2026, 7, 15, 12, 2, 0).unwrap();
    let record = MigrationPhaseRecord {
        job_uuid,
        phase: MigrationPhase::Staging,
        disposition: MigrationDisposition::Cancelled,
        cancel_requested: true,
        export_progress: None,
        created_at: Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        updated_at: terminal_at,
        terminal_at: Some(terminal_at),
        import_outcome: None,
    };

    let body = serde_json::to_value(AsyncMigrationStatusResponse::from(record)).unwrap();

    assert_eq!(body["disposition"], "cancelled");
    assert_eq!(body["terminalAt"], "2026-07-15T12:02:00Z");
    assert!(body.get("cancelRequested").is_none());
    assert!(body.get("cancel_requested").is_none());
    assert_import_outcome_fields_absent(&body);
}

/// TODO: Document async_submit_returns_admission_snapshot_and_status_reads_durable_phase.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_submit_returns_admission_snapshot_and_status_reads_durable_phase() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let reached_documents = Arc::new(Notify::new());
    let release_documents = Arc::new(Notify::new());

    let (status, Json(submitted)) = submit_algolia_migration_with_test_source_factory(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        Json(valid_async_request()),
        {
            let reached_documents = Arc::clone(&reached_documents);
            let release_documents = Arc::clone(&release_documents);
            move |_| {
                Ok(BlockingDocumentReadSourceReader::new(
                    async_hermetic_source_reader(),
                    reached_documents,
                    release_documents,
                ))
            }
        },
    )
    .await
    .expect("async submission should be admitted");

    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(submitted.phase, AsyncMigrationPhase::Submitted);
    assert_eq!(submitted.disposition, AsyncMigrationDisposition::Running);
    assert!(submitted.export_progress.is_none());

    tokio::time::timeout(ASYNC_LIFECYCLE_TIMEOUT, reached_documents.notified())
        .await
        .expect("background import should reach document export");
    let Json(current) = get_algolia_migration_status(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        AxumPath(submitted.job_id.to_string()),
    )
    .await
    .expect("status should read the durable phase record");
    assert_eq!(current.job_id, submitted.job_id);
    assert_eq!(current.phase, AsyncMigrationPhase::Exporting);
    assert_eq!(current.disposition, AsyncMigrationDisposition::Running);
    assert!(current.terminal_at.is_none());

    release_documents.notify_waiters();
    // This test owns the spawned job, so join that exact task instead of using
    // a time-bound status poll. The durable read below still proves the result.
    tokio::time::timeout(
        ASYNC_STATUS_TERMINAL_TIMEOUT,
        state.migration_runner.drain_active_imports(),
    )
    .await
    .expect("owned async import should finish after release");
    let Json(terminal) = get_algolia_migration_status(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        AxumPath(submitted.job_id.to_string()),
    )
    .await
    .expect("status should expose the terminal durable phase after the owned task joins");
    assert_eq!(terminal.phase, AsyncMigrationPhase::Activating);
    assert_eq!(terminal.disposition, AsyncMigrationDisposition::Succeeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_terminal_status_reports_import_outcome_counts_and_warnings() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let (status, Json(submitted)) = submit_algolia_migration_with_test_source_factory(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        Json(valid_async_request()),
        |_| Ok(async_source_reader_with_import_outcome()),
    )
    .await
    .expect("async submission should be admitted");

    assert_eq!(status, StatusCode::ACCEPTED);
    let terminal = wait_for_async_terminal(&state, submitted.job_id, "async-owner-app", None).await;
    assert_eq!(terminal.phase, AsyncMigrationPhase::Activating);
    assert_eq!(terminal.disposition, AsyncMigrationDisposition::Succeeded);
    assert_eq!(terminal.settings_applied, Some(true));
    assert_eq!(
        terminal.synonyms_imported,
        Some(MigrateCount { imported: 1 })
    );
    assert_eq!(terminal.rules_imported, Some(MigrateCount { imported: 2 }));

    let warning_codes = terminal
        .warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        warning_codes,
        BTreeSet::from([
            "ReplicaExhaustiveSortApproximated",
            "ReplicaMatchingCriticalFieldDiverges",
            "ReplicaRelevancyStrictnessSemanticMismatch",
        ])
    );
    assert_eq!(
        terminal.warnings.len(),
        3,
        "async status must not expose extra translation warnings"
    );
    let exhaustive_warning = terminal
        .warnings
        .iter()
        .find(|warning| warning.code == "ReplicaExhaustiveSortApproximated")
        .expect("status should expose the exhaustive-sort approximation warning");
    assert_eq!(
        exhaustive_warning.message,
        "Algolia standard replica exhaustive sorting is approximated as a Flapjack virtual replica."
    );
    assert_eq!(exhaustive_warning.json_path, "$.replicas[0]");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn async_terminal_status_reports_zero_rule_and_synonym_counts() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let (status, Json(submitted)) = submit_algolia_migration_with_test_source_factory(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        Json(valid_async_request()),
        |_| Ok(async_hermetic_source_reader()),
    )
    .await
    .expect("async submission should be admitted");

    assert_eq!(status, StatusCode::ACCEPTED);
    let terminal = wait_for_async_terminal(&state, submitted.job_id, "async-owner-app", None).await;
    assert_eq!(terminal.phase, AsyncMigrationPhase::Activating);
    assert_eq!(terminal.disposition, AsyncMigrationDisposition::Succeeded);
    assert_eq!(terminal.rules_imported, Some(MigrateCount { imported: 0 }));
    assert_eq!(
        terminal.synonyms_imported,
        Some(MigrateCount { imported: 0 })
    );
}

/// TODO: Document async_status_unknown_uuid_returns_stable_not_found_code.
#[tokio::test]
async fn async_status_unknown_uuid_returns_stable_not_found_code() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let missing_uuid = "01890f8e-8b28-78e8-b542-8cfdcb2d4f25";

    let error = get_algolia_migration_status(
        State(state),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        AxumPath(missing_uuid.to_string()),
    )
    .await
    .expect_err("unknown durable job should be a stable typed 404");

    assert_eq!(error.0, StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        })
    );
}

/// TODO: Document async_cancel_invalid_uuid_returns_bad_request.
#[tokio::test]
async fn async_cancel_invalid_uuid_returns_bad_request() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let error = cancel_algolia_migration(
        State(state),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        AxumPath("not-a-uuid".to_string()),
    )
    .await
    .expect_err("invalid cancel UUID should be rejected before spool access");

    assert_eq!(error.0, StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "job_id must be a valid UUID",
            "status": 400
        })
    );
}

/// TODO: Document async_cancel_unknown_uuid_returns_stable_not_found_code.
#[tokio::test]
async fn async_cancel_unknown_uuid_returns_stable_not_found_code() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let missing_uuid = "01890f8e-8b28-78e8-b542-8cfdcb2d4f25";

    let error = cancel_algolia_migration(
        State(state),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        AxumPath(missing_uuid.to_string()),
    )
    .await
    .expect_err("unknown durable job should be a stable typed 404");

    assert_eq!(error.0, StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        })
    );
}

/// TODO: Document async_cancel_running_job_returns_status_without_exposing_internal_flag.
#[tokio::test]
async fn async_cancel_running_job_returns_status_without_exposing_internal_flag() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let job_uuid = Uuid::parse_str("01890f8e-8b28-78e8-b542-8cfdcb2d4f24").unwrap();
    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "cancel_running",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();

    let Json(status) = cancel_algolia_migration(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        AxumPath(job_uuid.to_string()),
    )
    .await
    .expect("running cancel should return the updated status");

    assert_eq!(status.job_id, job_uuid);
    assert_eq!(status.disposition, AsyncMigrationDisposition::Running);
    assert!(status.terminal_at.is_none());
    assert!(
        spool
            .read_migration_phase(job_uuid)
            .unwrap()
            .cancel_requested
    );
    let body = serde_json::to_value(status).unwrap();
    assert!(body.get("cancelRequested").is_none());
    assert!(body.get("cancel_requested").is_none());
    assert_import_outcome_fields_absent(&body);
}

/// TODO: Document async_cancel_terminal_jobs_returns_existing_terminal_status.
#[tokio::test]
async fn async_cancel_terminal_jobs_returns_existing_terminal_status() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let cancelled = Uuid::new_v4();
    let failed = Uuid::new_v4();
    let succeeded = Uuid::new_v4();

    spool
        .create_async_migration_admission_for_owner(
            cancelled,
            "already_cancelled",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    let cancelled_before = spool.cancel_migration(cancelled).unwrap();
    spool
        .create_async_migration_admission_for_owner(
            failed,
            "already_failed",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    let failed_before = spool.fail_migration(failed).unwrap();
    spool
        .create_async_migration_admission_for_owner(
            succeeded,
            "already_succeeded",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    spool
        .transition_migration_phase(succeeded, MigrationPhase::Exporting)
        .unwrap();
    spool
        .transition_migration_phase(succeeded, MigrationPhase::Preparing)
        .unwrap();
    spool
        .transition_migration_phase(succeeded, MigrationPhase::Staging)
        .unwrap();
    spool
        .transition_migration_phase(succeeded, MigrationPhase::Activating)
        .unwrap();
    let succeeded_before = spool.succeed_migration(succeeded, None).unwrap();

    for (job_uuid, expected, target_index) in [
        (cancelled, cancelled_before, "already_cancelled"),
        (failed, failed_before, "already_failed"),
        (succeeded, succeeded_before, "already_succeeded"),
    ] {
        let Json(status) = cancel_algolia_migration(
            State(Arc::clone(&state)),
            axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
            AxumPath(job_uuid.to_string()),
        )
        .await
        .expect("terminal cancel should be a no-op status read");

        let mut expected_status = AsyncMigrationStatusResponse::from(expected.clone());
        expected_status.target_index = Some(target_index.to_string());
        assert_eq!(status, expected_status);
        assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), expected);
    }
}

/// TODO: Document async_acknowledge_terminal_job_returns_no_content_and_preserves_phase.
#[tokio::test]
async fn async_acknowledge_terminal_job_returns_no_content_and_preserves_phase() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let job_uuid = Uuid::new_v4();

    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "acknowledged_terminal",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    let terminal = spool.succeed_migration(job_uuid, None).unwrap();

    let status = acknowledge_algolia_migration(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        HeaderMap::new(),
        AxumPath(job_uuid.to_string()),
    )
    .await
    .expect("terminal ACK should be accepted");

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), terminal);
}

#[tokio::test]
async fn async_acknowledge_uses_authenticated_header_owner_identity() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let app = migration_job_route(Arc::clone(&state));
    let job_uuid = Uuid::new_v4();
    let not_found_body = json!({
        "message": "Migration job not found",
        "status": 404,
        "code": "migration_job_not_found"
    });

    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "acknowledged_terminal_header_owner",
            Some(
                "async-owner-app:85dbe15d75ef9308c7ae0f33c7a324cc6f4bf519a2ed2f3027bd33c140a4f9aa",
            ),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    let terminal_before = spool.succeed_migration(job_uuid, None).unwrap();

    let first = send_acknowledge_request(&app, job_uuid, "async-owner-app", "secret-key").await;
    assert_eq!(first.status(), StatusCode::NO_CONTENT);
    let terminal_between = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(terminal_between, terminal_before);

    let second = send_acknowledge_request(&app, job_uuid, "async-owner-app", "secret-key").await;
    assert_eq!(second.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        spool.read_migration_phase(job_uuid).unwrap(),
        terminal_before
    );

    let wrong_key =
        send_acknowledge_request(&app, job_uuid, "async-owner-app", "wrong-secret-key").await;
    assert_eq!(wrong_key.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(wrong_key).await, not_found_body);
    assert_eq!(
        spool.read_migration_phase(job_uuid).unwrap(),
        terminal_before
    );

    let wrong_app = send_acknowledge_request(&app, job_uuid, "other-owner-app", "secret-key").await;
    assert_eq!(wrong_app.status(), StatusCode::NOT_FOUND);
    assert_eq!(body_json(wrong_app).await, not_found_body);
    assert_eq!(
        spool.read_migration_phase(job_uuid).unwrap(),
        terminal_before
    );
}

#[tokio::test]
async fn async_acknowledge_running_job_fails_closed_without_mutating_phase() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let app = migration_job_route(Arc::clone(&state));
    let job_uuid = Uuid::new_v4();

    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "acknowledge_too_early",
            Some(
                "async-owner-app:85dbe15d75ef9308c7ae0f33c7a324cc6f4bf519a2ed2f3027bd33c140a4f9aa",
            ),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    let running = spool.read_migration_phase(job_uuid).unwrap();

    let response = send_acknowledge_request(&app, job_uuid, "async-owner-app", "secret-key").await;

    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert_eq!(
        body_json(response).await,
        json!({
            "message": "Migration job must be terminal before it can be acknowledged",
            "status": 409,
            "code": "migration_ack_too_early"
        })
    );
    assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), running);
}

#[tokio::test]
async fn async_acknowledge_failed_and_cancelled_jobs_preserve_terminal_compatibility() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let cancelled_job = Uuid::new_v4();
    let failed_job = Uuid::new_v4();

    spool
        .create_async_migration_admission_for_owner(
            cancelled_job,
            "ack_cancelled_terminal",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    let cancelled_terminal = spool.cancel_migration(cancelled_job).unwrap();

    spool
        .create_async_migration_admission_for_owner(
            failed_job,
            "ack_failed_terminal",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    let failed_terminal = spool.fail_migration(failed_job).unwrap();

    for (job_uuid, terminal) in [
        (cancelled_job, cancelled_terminal),
        (failed_job, failed_terminal),
    ] {
        let status = acknowledge_algolia_migration(
            State(Arc::clone(&state)),
            axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
            HeaderMap::new(),
            AxumPath(job_uuid.to_string()),
        )
        .await
        .expect("failed and cancelled ACKs should remain idempotent no-ops");

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), terminal);
    }
}

#[tokio::test]
async fn async_acknowledge_published_terminal_job_requires_generation_evidence() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let job_uuid = Uuid::new_v4();

    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "ack_terminal_missing_generation",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    spool
        .record_async_publication_transaction_if_present(
            job_uuid,
            PublicationTransactionId::new("ack_terminal_missing_generation_txn").unwrap(),
        )
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    let terminal = spool.succeed_migration(job_uuid, None).unwrap();

    let response = acknowledge_algolia_migration(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        HeaderMap::new(),
        AxumPath(job_uuid.to_string()),
    )
    .await
    .expect_err("published successes must fail closed when generation evidence is missing");

    assert_eq!(response.0, StatusCode::CONFLICT);
    assert_eq!(
        body_json(response.1.into_response()).await,
        json!({
            "message": "Migration publication generation evidence is stale or unavailable",
            "status": 409,
            "code": "migration_ack_stale_generation"
        })
    );
    assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), terminal);
}

#[tokio::test]
async fn async_acknowledge_published_terminal_job_accepts_current_generation() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let job_uuid = Uuid::new_v4();

    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "ack_published_terminal",
            Some("async-owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    seed_ack_generation_evidence(&state, &spool, job_uuid, "ack_published_terminal");
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    let terminal = spool.succeed_migration(job_uuid, None).unwrap();

    let status = acknowledge_algolia_migration(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        HeaderMap::new(),
        AxumPath(job_uuid.to_string()),
    )
    .await
    .expect("published terminal ACK should accept matching current generation evidence");

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), terminal);
}

/// Migration job lifecycle wire surface owned by one source provider.
///
/// The provider is part of the job-lifecycle URL, so status, cancel, and ACK
/// requests are addressed through the provider that owns the route instead of
/// through a hard-coded Algolia path.
struct MigrationLifecycleRoutes {
    provider: &'static str,
    router: Router,
}

impl MigrationLifecycleRoutes {
    /// Mount one provider's lifecycle handlers under its own migration prefix.
    fn mounted(provider: &'static str, provider_routes: Router) -> Self {
        Self {
            provider,
            router: Router::new().nest(&format!("/1/migrations/{provider}"), provider_routes),
        }
    }

    fn job_uri(&self, job_uuid: Uuid, suffix: &str) -> String {
        format!("/1/migrations/{}/{job_uuid}{suffix}", self.provider)
    }
}

fn migration_job_route(state: Arc<AppState>) -> MigrationLifecycleRoutes {
    migration_job_route_for_provider(
        AsyncMigrationSourceProvider::Algolia.as_str().unwrap(),
        state,
    )
}

fn migration_job_route_for_provider(
    provider: &'static str,
    state: Arc<AppState>,
) -> MigrationLifecycleRoutes {
    migration_job_route_for_provider_with_test_source_factory(provider, state, None)
}

fn migration_job_route_for_provider_with_test_source_factory(
    provider: &'static str,
    state: Arc<AppState>,
    test_source_factory: Option<TestMigrationSourceReaderFactory>,
) -> MigrationLifecycleRoutes {
    let mutation_permit = state.global_mutation_fence.try_admit_mutation().unwrap();
    let provider_routes = Router::new()
        .route("/", post(submit_algolia_migration_http))
        .route("/:job_id", get(get_algolia_migration_status_http))
        .route("/:job_id/resume", post(resume_algolia_migration_http))
        .route(
            "/:job_id/acknowledge",
            post(acknowledge_algolia_migration_http),
        )
        .route("/:job_id/cancel", post(cancel_algolia_migration_http))
        .with_state(state)
        .layer(axum::extract::Extension(mutation_permit));
    let provider_routes = match test_source_factory {
        Some(factory) => provider_routes.layer(axum::extract::Extension(factory)),
        None => provider_routes,
    };
    let provider_routes = match AsyncMigrationSourceProvider::parse(provider) {
        Some(provider) => provider_routes.layer(axum::extract::Extension(provider)),
        None => provider_routes,
    };
    MigrationLifecycleRoutes::mounted(provider, provider_routes)
}

fn public_provider_migration_routes(state: &Arc<AppState>) -> Vec<MigrationLifecycleRoutes> {
    AsyncMigrationSourceProvider::PUBLIC
        .into_iter()
        .map(|provider| {
            migration_job_route_for_provider(provider.as_str().unwrap(), Arc::clone(state))
        })
        .collect()
}

async fn send_submit_request(
    app: &MigrationLifecycleRoutes,
    authenticated_app_id: &str,
    api_key: &str,
    payload: serde_json::Value,
) -> Response<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/migrations/{}", app.provider))
        .header("content-type", "application/json")
        .header("x-algolia-api-key", api_key)
        .body(Body::from(payload.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(AuthenticatedAppId(authenticated_app_id.to_string()));
    app.router.clone().oneshot(request).await.unwrap()
}

fn algolia_submit_payload(target_index: &str) -> serde_json::Value {
    json!({
        "appId": "LOCALMIGRATIONTEST",
        "apiKey": "hermetic-source-key-not-used",
        "sourceIndex": "source_products",
        "targetIndex": target_index,
        "overwrite": false
    })
}

fn meilisearch_submit_payload(target_index: &str) -> serde_json::Value {
    json!({
        "endpoint": "https://your-instance.meilisearch.io",
        "apiKey": "hermetic-meilisearch-key-not-used",
        "sourceIndex": "source_products",
        "targetIndex": target_index,
        "overwrite": false
    })
}

fn typesense_submit_payload(target_index: &str) -> serde_json::Value {
    typesense_submit_payload_with_key(target_index, "hermetic-typesense-key-not-used")
}

fn typesense_submit_payload_with_key(target_index: &str, api_key: &str) -> serde_json::Value {
    json!({
        "node": "https://tenant.typesense.net",
        "apiKey": api_key,
        "sourceIndex": "source_products",
        "targetIndex": target_index,
        "overwrite": false,
        "sourceWriteFrozen": true
    })
}

fn typesense_resume_payload(source_write_frozen: Option<bool>) -> serde_json::Value {
    let mut body = typesense_submit_payload("resume_target");
    match source_write_frozen {
        Some(attestation) => body["sourceWriteFrozen"] = json!(attestation),
        None => {
            body.as_object_mut().unwrap().remove("sourceWriteFrozen");
        }
    }
    body
}

async fn job_uuid_from_submit_response(response: Response<Body>) -> Uuid {
    let body = body_json(response).await;
    Uuid::parse_str(
        body["jobId"]
            .as_str()
            .expect("submit response must include jobId"),
    )
    .unwrap()
}

async fn send_raw_submit_request(
    app: &MigrationLifecycleRoutes,
    authenticated_app_id: &str,
    api_key: &str,
    body: &'static str,
) -> Response<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(format!("/1/migrations/{}", app.provider))
        .header("content-type", "application/json")
        .header("x-algolia-api-key", api_key)
        .body(Body::from(body))
        .unwrap();
    request
        .extensions_mut()
        .insert(AuthenticatedAppId(authenticated_app_id.to_string()));
    app.router.clone().oneshot(request).await.unwrap()
}

async fn send_acknowledge_request(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
) -> Response<Body> {
    send_migration_job_request(
        app,
        job_uuid,
        authenticated_app_id,
        api_key,
        Method::POST,
        "/acknowledge",
    )
    .await
}

async fn send_status_request(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
) -> Response<Body> {
    send_migration_job_request(
        app,
        job_uuid,
        authenticated_app_id,
        api_key,
        Method::GET,
        "",
    )
    .await
}

async fn send_cancel_request(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
) -> Response<Body> {
    send_migration_job_request(
        app,
        job_uuid,
        authenticated_app_id,
        api_key,
        Method::POST,
        "/cancel",
    )
    .await
}

async fn send_resume_request(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
) -> Response<Body> {
    send_resume_request_with_body(
        app,
        job_uuid,
        authenticated_app_id,
        api_key,
        algolia_submit_payload("resume_target"),
    )
    .await
}

async fn send_resume_request_with_body(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
    body: serde_json::Value,
) -> Response<Body> {
    let mut request = Request::builder()
        .method(Method::POST)
        .uri(app.job_uri(job_uuid, "/resume"))
        .header("content-type", "application/json")
        .header("x-algolia-api-key", api_key)
        .body(Body::from(body.to_string()))
        .unwrap();
    request
        .extensions_mut()
        .insert(AuthenticatedAppId(authenticated_app_id.to_string()));
    app.router.clone().oneshot(request).await.unwrap()
}

async fn send_migration_job_request(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
    method: Method,
    suffix: &str,
) -> Response<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(app.job_uri(job_uuid, suffix))
        .header("x-algolia-api-key", api_key)
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(AuthenticatedAppId(authenticated_app_id.to_string()));
    app.router.clone().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn algolia_malformed_submit_body_keeps_json_error_contract() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = migration_job_route_for_provider("algolia", Arc::clone(&state));

    let response = send_raw_submit_request(
        &app,
        "malformed-body-owner",
        "malformed-body-key",
        "{\"appId\":\"LOCALMIGRATIONTEST\"",
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({
            "message": "Invalid migration request body",
            "status": 400
        }),
        "Algolia malformed submit bodies must keep the existing JSON error envelope"
    );
    assert_eq!(state.migration_runner.active_count_for_test(), 0);
}

/// TODO: Document async_submit_spool_failure_returns_sanitized_500_without_spawning_import.
#[tokio::test]
async fn async_submit_spool_failure_returns_sanitized_500_without_spawning_import() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("migration_exports"), b"not a directory").unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let source_factory_invoked = Arc::new(AtomicBool::new(false));
    let invoked = Arc::clone(&source_factory_invoked);

    let error = submit_algolia_migration_with_test_source_factory(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
        Json(valid_async_request()),
        move |_| {
            invoked.store(true, Ordering::SeqCst);
            Ok(async_hermetic_source_reader())
        },
    )
    .await
    .expect_err("spool admission failure should be sanitized");

    assert_eq!(error.0, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(error.1.into_response()).await,
        json!({
            "message": "Internal server error",
            "status": 500
        })
    );
    assert!(
        source_factory_invoked.load(Ordering::SeqCst),
        "reader construction must precede spool initialization so construction failure leaves no spool residue"
    );
    assert_eq!(state.migration_runner.active_count_for_test(), 0);
}

/// TODO: Document async_status_and_cancel_hide_foreign_job_uuids.
#[tokio::test]
async fn async_status_and_cancel_hide_foreign_job_uuids() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let job_uuid = Uuid::new_v4();
    spool
        .create_async_migration_admission_for_owner(
            job_uuid,
            "owned_target",
            Some("owner-app"),
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();

    let status_error = get_algolia_migration_status(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("other-app".to_string())),
        AxumPath(job_uuid.to_string()),
    )
    .await
    .expect_err("foreign callers must not read another app's async job");
    assert_eq!(status_error.0, StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(status_error.1.into_response()).await,
        json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        })
    );

    let cancel_error = cancel_algolia_migration(
        State(Arc::clone(&state)),
        axum::extract::Extension(AuthenticatedAppId("other-app".to_string())),
        AxumPath(job_uuid.to_string()),
    )
    .await
    .expect_err("foreign callers must not cancel another app's async job");
    assert_eq!(cancel_error.0, StatusCode::NOT_FOUND);
    assert_eq!(
        body_json(cancel_error.1.into_response()).await,
        json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        })
    );

    assert!(
        !spool
            .read_migration_phase(job_uuid)
            .unwrap()
            .cancel_requested,
        "foreign cancel attempts must not mutate the durable job state"
    );
}

#[tokio::test]
async fn async_runner_created_job_is_isolated_by_authenticated_app_and_key() {
    const OWNER_APP: &str = "runner-owner-app";
    const OWNER_KEY: &str = "runner-owner-key";
    const TARGET: &str = "runner_owned_target";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let app = migration_job_route(Arc::clone(&state));
    let owner_headers = migration_owner_headers(Some(OWNER_KEY));
    let owner_identity = authenticated_owner_identity(OWNER_APP.to_string(), &owner_headers);
    let request = MigrateFromAlgoliaRequest {
        target_index: Some(TARGET.to_string()),
        ..valid_async_request()
    };

    let (job_uuid, _) = state
        .migration_runner
        .submit_algolia_import_for_owner(request, Some(owner_identity), |_| {
            Ok(async_hermetic_source_reader())
        })
        .await
        .expect("runner-created owned async job should be admitted");
    let rightful_status =
        wait_for_async_terminal(&state, job_uuid, OWNER_APP, Some(OWNER_KEY)).await;
    assert_eq!(rightful_status.phase, AsyncMigrationPhase::Activating);
    assert_eq!(
        rightful_status.disposition,
        AsyncMigrationDisposition::Succeeded
    );
    assert!(rightful_status.terminal_at.is_some());

    let terminal_before = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(terminal_before.phase, MigrationPhase::Activating);
    assert_eq!(terminal_before.disposition, MigrationDisposition::Succeeded);
    assert!(terminal_before.terminal_at.is_some());
    let expected_hits = vec![
        (
            "doc-1".to_string(),
            "Quartz adapter".to_string(),
            "hardware".to_string(),
        ),
        (
            "doc-2".to_string(),
            "Velvet compass".to_string(),
            "navigation".to_string(),
        ),
    ];
    assert_eq!(
        sorted_async_target_hits(&state, TARGET).await,
        expected_hits
    );

    for (case, foreign_app, foreign_key) in [
        ("different app", "other-runner-app", OWNER_KEY),
        ("same app with different key", OWNER_APP, "wrong-runner-key"),
    ] {
        let status = send_status_request(&app, job_uuid, foreign_app, foreign_key).await;
        assert_migration_job_not_found(status, &format!("{case} status")).await;

        let acknowledge = send_acknowledge_request(&app, job_uuid, foreign_app, foreign_key).await;
        assert_migration_job_not_found(acknowledge, &format!("{case} ACK")).await;

        let cancel = send_cancel_request(&app, job_uuid, foreign_app, foreign_key).await;
        assert_migration_job_not_found(cancel, &format!("{case} cancel")).await;

        assert_eq!(
            spool.read_migration_phase(job_uuid).unwrap(),
            terminal_before,
            "{case} must not advance or mutate the durable phase"
        );
        assert_eq!(
            sorted_async_target_hits(&state, TARGET).await,
            expected_hits,
            "{case} must not mutate the exact target contents"
        );
    }

    let status = send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(status.status(), StatusCode::OK);
    let mut expected_status = AsyncMigrationStatusResponse::from(terminal_before.clone());
    expected_status.target_index = Some(TARGET.to_string());
    assert_eq!(
        body_json(status).await,
        serde_json::to_value(expected_status).unwrap()
    );

    for attempt in 1..=2 {
        let acknowledge = send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        assert_eq!(
            acknowledge.status(),
            StatusCode::NO_CONTENT,
            "rightful authenticated ACK {attempt} should be idempotent"
        );
        assert_eq!(
            spool.read_migration_phase(job_uuid).unwrap(),
            terminal_before,
            "rightful authenticated ACK {attempt} must preserve the terminal phase"
        );
    }
}

#[test]
fn async_metadata_legacy_algolia_omission_decodes_as_source_import() {
    let job_uuid = Uuid::new_v4();
    let legacy: spool::AsyncMigrationMetadata = serde_json::from_value(json!({
        "job_uuid": job_uuid,
        "target_index": "legacy-provider-target"
    }))
    .unwrap();

    assert_eq!(legacy.job_uuid, job_uuid);
    assert_eq!(legacy.target_index, "legacy-provider-target");
    assert_eq!(
        legacy.source_provider,
        AsyncMigrationSourceProvider::Algolia
    );
    assert_eq!(
        legacy.publication_semantic,
        AsyncMigrationPublicationSemantic::CreateOnly
    );
    assert_eq!(legacy.topology, None);
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        json!({
            "job_uuid": job_uuid,
            "target_index": "legacy-provider-target"
        })
    );
}

#[test]
fn async_metadata_legacy_bulk_replace_source_provider_decodes_as_internal_operation() {
    let job_uuid = Uuid::new_v4();
    let legacy: spool::AsyncMigrationMetadata = serde_json::from_value(json!({
        "job_uuid": job_uuid,
        "source_provider": "bulk_replace",
        "target_index": "legacy-bulk-replace-target",
        "topology": "single_node_only",
        "publication_semantic": "replaceExisting",
        "authenticated_app_id": "legacy-owner"
    }))
    .unwrap();

    assert_eq!(legacy.job_uuid, job_uuid);
    assert_eq!(legacy.target_index, "legacy-bulk-replace-target");
    assert_eq!(
        legacy.source_provider,
        AsyncMigrationSourceProvider::Algolia,
        "legacy bulk_replace rows must normalize public source_provider to Algolia"
    );
    assert_eq!(
        AsyncMigrationSourceProvider::parse("bulk_replace"),
        None,
        "bulk_replace compatibility decoding must not widen the public parser"
    );
    assert_eq!(
        legacy.publication_semantic,
        AsyncMigrationPublicationSemantic::ReplaceExisting
    );
    assert_eq!(legacy.topology, Some(MigrationTopology::SingleNodeOnly));
    assert_eq!(legacy.authenticated_app_id.as_deref(), Some("legacy-owner"));
    assert_eq!(
        serde_json::to_value(&legacy).unwrap(),
        json!({
            "job_uuid": job_uuid,
            "operation_kind": "bulk_replace",
            "target_index": "legacy-bulk-replace-target",
            "topology": "single_node_only",
            "publication_semantic": "replaceExisting",
            "authenticated_app_id": "legacy-owner"
        })
    );
}

#[tokio::test]
async fn async_metadata_new_bulk_replace_write_uses_internal_operation_kind() {
    let job_uuid = Uuid::new_v4();

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    spool
        .create_bulk_replace_admission_for_owner(
            job_uuid,
            "provider-bulk-replace-target",
            "provider-owner",
            AsyncMigrationPublicationSemantic::ReplaceExisting,
        )
        .unwrap();
    let admitted = spool.read_async_migration_metadata(job_uuid).unwrap();

    assert_eq!(admitted.job_uuid, job_uuid);
    assert_eq!(admitted.target_index, "provider-bulk-replace-target");
    assert_eq!(
        admitted.source_provider,
        AsyncMigrationSourceProvider::Algolia,
        "new bulk-replace metadata must keep the public source_provider normalized"
    );
    assert_eq!(
        admitted.publication_semantic,
        AsyncMigrationPublicationSemantic::ReplaceExisting
    );
    assert_eq!(admitted.topology, Some(MigrationTopology::SingleNodeOnly));
    assert_eq!(
        admitted.authenticated_app_id.as_deref(),
        Some("provider-owner")
    );

    let wire: serde_json::Value = serde_json::from_slice(
        &std::fs::read(spool.async_migration_metadata_path(job_uuid)).unwrap(),
    )
    .unwrap();
    assert_eq!(
        wire,
        json!({
            "job_uuid": job_uuid,
            "operation_kind": "bulk_replace",
            "target_index": "provider-bulk-replace-target",
            "topology": "single_node_only",
            "publication_semantic": "replaceExisting",
            "authenticated_app_id": "provider-owner"
        })
    );
    assert_ne!(wire.get("source_provider"), Some(&json!("bulk_replace")));
}

#[test]
fn async_migration_source_provider_parser_rejects_non_members() {
    assert_eq!(AsyncMigrationSourceProvider::parse("bulk_replace"), None);
    assert_eq!(AsyncMigrationSourceProvider::parse("not_a_provider"), None);
}

#[test]
fn async_migration_source_provider_parser_pins_closed_public_union() {
    let parsed_public_providers: BTreeSet<&'static str> = ["algolia", "meilisearch", "typesense"]
        .into_iter()
        .filter_map(AsyncMigrationSourceProvider::parse)
        .filter_map(AsyncMigrationSourceProvider::as_str)
        .collect();

    let public_provider_routes: BTreeSet<&'static str> = AsyncMigrationSourceProvider::PUBLIC
        .into_iter()
        .map(|provider| provider.as_str().unwrap())
        .collect();

    assert_eq!(
        parsed_public_providers,
        BTreeSet::from(["algolia", "meilisearch", "typesense"]),
        "source_provider parser must expose exactly the closed public migration provider union"
    );
    assert_eq!(
        public_provider_routes, parsed_public_providers,
        "public route providers must stay in lockstep with parser-accepted providers"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_typesense_submission_is_admitted_through_shared_lifecycle() {
    const OWNER_APP: &str = "typesense-admission-owner";
    const OWNER_KEY: &str = "typesense-admission-key";
    const TARGET_INDEX: &str = "typesense_admission_target";
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let app = migration_job_route_for_provider_with_test_source_factory(
        "typesense",
        Arc::clone(&state),
        Some(TestMigrationSourceReaderFactory::new(|source_provider| {
            assert_eq!(source_provider, AsyncMigrationSourceProvider::Typesense);
            Ok(Box::new(async_hermetic_typesense_source_reader()))
        })),
    );

    let response = send_submit_request(
        &app,
        OWNER_APP,
        OWNER_KEY,
        typesense_submit_payload(TARGET_INDEX),
    )
    .await;

    assert_eq!(
        response.status(),
        StatusCode::ACCEPTED,
        "valid Typesense submissions must enter the shared async lifecycle"
    );
    let job_uuid = job_uuid_from_submit_response(response).await;
    let metadata = spool.read_async_migration_metadata(job_uuid).unwrap();
    assert_eq!(
        metadata.source_provider,
        AsyncMigrationSourceProvider::Typesense
    );
    assert_eq!(metadata.target_index, TARGET_INDEX);
    let terminal = wait_for_route_terminal(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(
        terminal["disposition"], "succeeded",
        "terminal status: {terminal:#}"
    );

    for wrong_provider in ["algolia", "meilisearch"] {
        let wrong_provider_route =
            migration_job_route_for_provider(wrong_provider, Arc::clone(&state));
        assert_migration_job_not_found(
            send_status_request(&wrong_provider_route, job_uuid, OWNER_APP, OWNER_KEY).await,
            "wrong-provider status",
        )
        .await;
        assert_migration_job_not_found(
            send_cancel_request(&wrong_provider_route, job_uuid, OWNER_APP, OWNER_KEY).await,
            "wrong-provider cancel",
        )
        .await;
        assert_migration_job_not_found(
            send_acknowledge_request(&wrong_provider_route, job_uuid, OWNER_APP, OWNER_KEY).await,
            "wrong-provider acknowledge",
        )
        .await;
    }

    assert_eq!(
        send_cancel_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::NO_CONTENT
    );
}

#[tokio::test]
async fn malformed_meilisearch_payloads_reject_before_persistence_or_source_use() {
    const SPOOL_POISON: &[u8] = b"invalid payload must not access the migration spool";

    let tmp = TempDir::new().unwrap();
    let spool_root = tmp.path().join("migration_exports");
    std::fs::write(&spool_root, SPOOL_POISON).unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = migration_job_route_for_provider("meilisearch", Arc::clone(&state));

    for (target_index, payload) in [
        (
            "meilisearch_wrong_shape_target",
            json!({
                "appId": "algolia-shaped-app",
                "apiKey": "unused-key-canary",
                "sourceIndex": "source",
                "targetIndex": "meilisearch_wrong_shape_target"
            }),
        ),
        (
            "meilisearch_mixed_shape_target",
            json!({
                "endpoint": "https://your-instance.meilisearch.io",
                "apiKey": "meilisearch-secret-canary",
                "sourceIndex": "source_products",
                "targetIndex": "meilisearch_mixed_shape_target",
                "overwrite": false,
                "appId": "algolia-app-canary"
            }),
        ),
    ] {
        let residue_before = rejected_admission_residue_snapshot(&state, target_index);
        let response = send_submit_request(
            &app,
            "malformed-provider-owner",
            "malformed-provider-key",
            payload,
        )
        .await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.migration_runner.active_count_for_test(), 0);
        assert_eq!(
            rejected_admission_residue_snapshot(&state, target_index),
            residue_before
        );
        let body = body_json(response).await.to_string();
        assert!(!body.contains("unused-key-canary"));
        assert!(!body.contains("meilisearch-secret-canary"));
        assert!(!body.contains("algolia-app-canary"));
    }

    assert_eq!(std::fs::read(spool_root).unwrap(), SPOOL_POISON);
}

async fn assert_target_not_listed(state: &Arc<AppState>, target_index: &str, reason: &str) {
    assert!(
        !state.manager.base_path.join(target_index).exists(),
        "{reason}: target directory {target_index:?} exists"
    );
    let Json(indices) = list_indices(State(Arc::clone(state)), Query(HashMap::new()))
        .await
        .expect("index list should remain readable while checking rejected migration residue");
    assert!(
        indices.items.iter().all(|item| item.name != target_index),
        "{reason}: target index {target_index:?} is listable"
    );
}

fn rejected_admission_residue_snapshot(
    state: &Arc<AppState>,
    target_index: &str,
) -> BTreeMap<&'static str, PathSnapshot> {
    let mut snapshot = target_publication_residue_snapshot(state, target_index);
    snapshot.insert(
        "publication_receipt_spool",
        path_snapshot(&state.manager.base_path.join("migration_exports")),
    );
    snapshot
}

fn target_publication_residue_snapshot(
    state: &Arc<AppState>,
    target_index: &str,
) -> BTreeMap<&'static str, PathSnapshot> {
    BTreeMap::from([
        (
            "target_index",
            path_snapshot(&state.manager.base_path.join(target_index)),
        ),
        (
            "publication_target_namespace",
            path_snapshot(
                &state
                    .manager
                    .base_path
                    .join(".publication")
                    .join(target_index),
            ),
        ),
        (
            "publication_quarantine_namespace",
            path_snapshot(
                &state
                    .manager
                    .base_path
                    .join(".publication_quarantine")
                    .join(target_index),
            ),
        ),
    ])
}

#[derive(Debug, PartialEq, Eq)]
enum PathSnapshot {
    Missing,
    File(Vec<u8>),
    Directory(BTreeMap<PathBuf, Vec<u8>>),
}

fn path_snapshot(path: &Path) -> PathSnapshot {
    if !path.exists() {
        return PathSnapshot::Missing;
    }
    if path.is_file() {
        return PathSnapshot::File(std::fs::read(path).unwrap());
    }
    let mut files = BTreeMap::new();
    collect_path_snapshot(path, path, &mut files);
    PathSnapshot::Directory(files)
}

fn collect_path_snapshot(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let mut entries = std::fs::read_dir(current)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            collect_path_snapshot(root, &path, files);
        } else {
            files.insert(
                path.strip_prefix(root).unwrap().to_path_buf(),
                std::fs::read(path).unwrap(),
            );
        }
    }
}

fn capture_logs(logs: &SharedLogBuffer) -> tracing::subscriber::DefaultGuard {
    let subscriber = tracing_subscriber::registry().with(
        tracing_subscriber::fmt::layer()
            .without_time()
            .with_writer(logs.clone()),
    );
    tracing::subscriber::set_default(subscriber)
}

fn assert_secret_absent(secret: &str, surfaces: &[(&str, &str)]) {
    for (surface_name, surface) in surfaces {
        assert!(
            !surface.contains(secret),
            "Typesense credential leaked through {surface_name}: {surface}"
        );
    }
}

#[tokio::test]
async fn typesense_provider_payload_mismatch_rejects_before_persistence() {
    const TARGET_INDEX: &str = "typesense_payload_mismatch_target";
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let source_factory_observed = Arc::new(AtomicBool::new(false));
    let app = migration_job_route_for_provider_with_test_source_factory(
        "typesense",
        Arc::clone(&state),
        Some(TestMigrationSourceReaderFactory::new({
            let source_factory_observed = Arc::clone(&source_factory_observed);
            move |_| {
                source_factory_observed.store(true, Ordering::SeqCst);
                Ok(Box::new(async_hermetic_source_reader()))
            }
        })),
    );
    let residue_before = rejected_admission_residue_snapshot(&state, TARGET_INDEX);
    let response = send_submit_request(
        &app,
        "typesense-owner",
        "typesense-owner-key",
        meilisearch_submit_payload(TARGET_INDEX),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        body_json(response).await,
        json!({
            "message": "Typesense payload does not match source_provider",
            "status": 400,
            "code": "source_provider_payload_mismatch"
        })
    );
    assert_eq!(state.migration_runner.active_count_for_test(), 0);
    assert!(!source_factory_observed.load(Ordering::SeqCst));
    assert_target_not_listed(
        &state,
        TARGET_INDEX,
        "payload mismatch must reject before target creation",
    )
    .await;
    assert_eq!(
        rejected_admission_residue_snapshot(&state, TARGET_INDEX),
        residue_before,
        "payload mismatch must reject before runner, spool, target, publication, receipt, or quarantine persistence"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn typesense_credentials_never_reach_logs_errors_spool_or_public_json() {
    const TYPESENSE_KEY_CANARY: &str = "typesense-stage1-secret-key-canary";
    const TARGET_INDEX: &str = "typesense_secret_residue_target";
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let logs = SharedLogBuffer::default();
    let _subscriber_guard = capture_logs(&logs);
    let source_error = AlgoliaClientError::new(
        AlgoliaErrorKind::Transport,
        "Typesense endpoint is unreachable",
    );
    let error_debug = format!("{source_error:?}");
    let app = migration_job_route_for_provider_with_test_source_factory(
        "typesense",
        Arc::clone(&state),
        Some(TestMigrationSourceReaderFactory::new(move |_| {
            Err(source_error.clone())
        })),
    );
    let residue_before = rejected_admission_residue_snapshot(&state, TARGET_INDEX);
    let response = send_submit_request(
        &app,
        "typesense-owner",
        "typesense-owner-key",
        typesense_submit_payload_with_key(TARGET_INDEX, TYPESENSE_KEY_CANARY),
    )
    .await;
    let response_debug = format!("{response:?}");
    let response_status = response.status();
    let body = body_json(response).await;
    let public_json = serde_json::to_string(&body).unwrap();
    let residue_after = rejected_admission_residue_snapshot(&state, TARGET_INDEX);
    let durable_status_debug = format!("{residue_after:?}");
    let captured_logs = logs.contents();

    assert_secret_absent(
        TYPESENSE_KEY_CANARY,
        &[
            ("response debug", response_debug.as_str()),
            ("source error debug", error_debug.as_str()),
            ("public error JSON", public_json.as_str()),
            ("durable status/spool debug", durable_status_debug.as_str()),
            ("captured logs", captured_logs.as_str()),
        ],
    );
    assert_eq!(response_status, StatusCode::BAD_GATEWAY);
    assert_eq!(
        body,
        json!({
            "message": "Typesense endpoint is unreachable",
            "status": 502
        })
    );
    assert_eq!(state.migration_runner.active_count_for_test(), 0);
    assert_target_not_listed(
        &state,
        TARGET_INDEX,
        "source-construction failure must not create a target",
    )
    .await;
    assert_eq!(
        residue_after, residue_before,
        "source-construction failure must not persist job status, spool, target, publication, receipt, or quarantine residue"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn local_provider_changed_source_after_export_blocks_publication() {
    for (provider, provider_name, owner_app, owner_key, target_index, payload) in [
        (
            AsyncMigrationSourceProvider::Meilisearch,
            "meilisearch",
            "meilisearch-owner",
            "meilisearch-owner-key",
            "meilisearch_changed_source_target",
            meilisearch_submit_payload("meilisearch_changed_source_target"),
        ),
        (
            AsyncMigrationSourceProvider::Typesense,
            "typesense",
            "typesense-owner",
            "typesense-owner-key",
            "typesense_changed_source_target",
            typesense_submit_payload("typesense_changed_source_target"),
        ),
    ] {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let logs = SharedLogBuffer::default();
        let _subscriber_guard = capture_logs(&logs);
        let app = migration_job_route_for_provider_with_test_source_factory(
            provider_name,
            Arc::clone(&state),
            Some(TestMigrationSourceReaderFactory::new(move |observed| {
                assert_eq!(observed, provider);
                // The durable export admits the request context's provider before
                // observing the source, so the scripted reader must report the
                // provider the route admitted for the drift proof to be reached.
                Ok(Box::new(
                    async_source_reader_with_final_drift().reporting_provider(observed),
                ))
            })),
        );
        let publication_before = target_publication_residue_snapshot(&state, target_index);
        let response = send_submit_request(&app, owner_app, owner_key, payload).await;

        assert_eq!(
            response.status(),
            StatusCode::ACCEPTED,
            "{provider_name} drift must be exercised after shared lifecycle admission"
        );
        let job_uuid = job_uuid_from_submit_response(response).await;
        let terminal = wait_for_route_terminal(&app, job_uuid, owner_app, owner_key).await;
        state.migration_runner.drain_active_imports().await;

        assert_eq!(
            terminal["disposition"], "failed",
            "{provider_name} source drift must fail the admitted job"
        );
        assert_import_outcome_fields_absent(&terminal);
        assert_target_not_listed(
            &state,
            target_index,
            "source drift must block target publication",
        )
        .await;
        assert_eq!(
            target_publication_residue_snapshot(&state, target_index),
            publication_before,
            "{provider_name} source drift must leave target, publication, and quarantine namespaces absent"
        );
        assert!(
            logs.contents().contains("Source changed during export"),
            "{provider_name} source drift must retain the shared scrubbed error in captured logs"
        );
    }
}

#[tokio::test]
async fn unknown_source_provider_submit_route_remains_unmapped() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let router = public_provider_migration_routes(&state)
        .into_iter()
        .fold(Router::new(), |router, app| router.merge(app.router));
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/1/migrations/not-a-provider")
                .header("content-type", "application/json")
                .body(Body::from(json!({}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        matches!(
            response.status(),
            StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED
        ),
        "unknown providers must remain route misses"
    );
    let response_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let response_code = serde_json::from_slice::<serde_json::Value>(&response_bytes)
        .ok()
        .and_then(|body| body["code"].as_str().map(str::to_owned));
    assert_ne!(
        response_code.as_deref(),
        Some(SOURCE_PROVIDER_UNSUPPORTED_CODE),
        "an unknown route must not be classified as a recognized unsupported provider"
    );
}

#[tokio::test]
async fn source_migration_job_lifecycle_rejects_wrong_provider_route() {
    const OWNER_APP: &str = "provider-binding-owner";
    const OWNER_KEY: &str = "provider-binding-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let owner_identity = authenticated_owner_identity(
        OWNER_APP.to_string(),
        &migration_owner_headers(Some(OWNER_KEY)),
    );
    let (job_uuid, _) = state
        .migration_runner
        .submit_algolia_import_for_owner(valid_async_request(), Some(owner_identity), |_| {
            Ok(async_hermetic_source_reader())
        })
        .await
        .expect("Algolia admission should create the provider-bound job specimen");
    let wrong_provider_route = migration_job_route_for_provider(
        AsyncMigrationSourceProvider::Meilisearch.as_str().unwrap(),
        Arc::clone(&state),
    );

    assert_eq!(
        send_status_request(&wrong_provider_route, job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "status must not reveal jobs through a different provider alias"
    );
    assert_eq!(
        send_cancel_request(&wrong_provider_route, job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "cancel must not mutate jobs through a different provider alias"
    );
    assert!(
        !spool
            .read_migration_phase(job_uuid)
            .unwrap()
            .cancel_requested,
        "wrong-provider cancel must leave the owned job unchanged"
    );
    assert_eq!(
        send_acknowledge_request(&wrong_provider_route, job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::NOT_FOUND,
        "acknowledge must not observe jobs through a different provider alias"
    );
}

#[tokio::test]
async fn source_migration_job_lifecycle_rejects_bulk_replace_jobs() {
    const OWNER_APP: &str = "bulk-replace-route-owner";
    const OWNER_KEY: &str = "bulk-replace-route-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let owner_identity = authenticated_owner_identity(
        OWNER_APP.to_string(),
        &migration_owner_headers(Some(OWNER_KEY)),
    );
    let job_uuid = Uuid::new_v4();
    spool
        .create_bulk_replace_admission_for_owner(
            job_uuid,
            "public_alias_hidden_bulk_replace",
            &owner_identity,
            AsyncMigrationPublicationSemantic::ReplaceExisting,
        )
        .unwrap();
    let phase_before = spool.read_migration_phase(job_uuid).unwrap();
    let public_algolia_route = migration_job_route(Arc::clone(&state));

    assert_migration_job_not_found(
        send_status_request(&public_algolia_route, job_uuid, OWNER_APP, OWNER_KEY).await,
        "public Algolia status for bulk_replace",
    )
    .await;
    assert_migration_job_not_found(
        send_cancel_request(&public_algolia_route, job_uuid, OWNER_APP, OWNER_KEY).await,
        "public Algolia cancel for bulk_replace",
    )
    .await;
    assert_migration_job_not_found(
        send_acknowledge_request(&public_algolia_route, job_uuid, OWNER_APP, OWNER_KEY).await,
        "public Algolia ACK for bulk_replace",
    )
    .await;
    assert_eq!(
        spool.read_migration_phase(job_uuid).unwrap(),
        phase_before,
        "public source-provider aliases must not mutate internal bulk_replace lifecycle state"
    );
}

#[tokio::test]
async fn non_algolia_resume_routes_reject_without_mutating_shared_lifecycle_state() {
    const OWNER_APP: &str = "non-algolia-resume-owner";
    const OWNER_KEY: &str = "non-algolia-resume-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();

    for provider in [
        AsyncMigrationSourceProvider::Meilisearch,
        AsyncMigrationSourceProvider::Typesense,
    ] {
        let job_uuid = Uuid::new_v4();
        spool
            .create_async_migration_admission_for_provider_owner(
                job_uuid,
                &format!("{provider:?}_resume_target"),
                Some(OWNER_APP),
                provider,
                AsyncMigrationPublicationSemantic::CreateOnly,
            )
            .unwrap();
        let phase_before = spool.read_migration_phase(job_uuid).unwrap();
        let app = migration_job_route_for_provider(provider.as_str().unwrap(), Arc::clone(&state));

        let response = send_resume_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await,
            json!({
                "message": SOURCE_PROVIDER_UNSUPPORTED_MESSAGE,
                "status": 400,
                "code": SOURCE_PROVIDER_UNSUPPORTED_CODE,
            }),
            "recognized non-Algolia providers must return the stable resume refusal"
        );
        assert_eq!(
            spool.read_migration_phase(job_uuid).unwrap(),
            phase_before,
            "non-Algolia resume refusal must not fabricate a lifecycle transition"
        );
    }
}

#[tokio::test]
async fn typesense_resume_refuses_every_attestation_arm_before_source_io() {
    const OWNER_APP: &str = "typesense-resume-attestation-owner";
    const OWNER_KEY: &str = "typesense-resume-attestation-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let observed_factory_calls = Arc::clone(&factory_calls);
    let source_factory = TestMigrationSourceReaderFactory::new(move |_| {
        observed_factory_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(async_hermetic_typesense_source_reader()))
    });
    let app = migration_job_route_for_provider_with_test_source_factory(
        "typesense",
        Arc::clone(&state),
        Some(source_factory),
    );

    for (attestation, body) in [
        ("missing", typesense_resume_payload(None)),
        ("false", typesense_resume_payload(Some(false))),
        ("true", typesense_resume_payload(Some(true))),
    ] {
        let job_uuid = Uuid::new_v4();
        spool
            .create_async_migration_admission_for_provider_owner(
                job_uuid,
                "typesense_resume_target",
                Some(OWNER_APP),
                AsyncMigrationSourceProvider::Typesense,
                AsyncMigrationPublicationSemantic::CreateOnly,
            )
            .unwrap();
        let phase_before = spool.read_migration_phase(job_uuid).unwrap();
        let response =
            send_resume_request_with_body(&app, job_uuid, OWNER_APP, OWNER_KEY, body).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{attestation}");
        assert_eq!(
            body_json(response).await,
            json!({
                "message": SOURCE_PROVIDER_UNSUPPORTED_MESSAGE,
                "status": 400,
                "code": SOURCE_PROVIDER_UNSUPPORTED_CODE,
            }),
            "Typesense resume must stay provider-unsupported for {attestation} attestation"
        );
        assert_eq!(factory_calls.load(Ordering::SeqCst), 0, "{attestation}");
        assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), phase_before);
    }
}

#[tokio::test]
async fn fabricated_running_outcomes_stay_hidden_for_every_provider_route() {
    const OWNER_APP: &str = "fabricated-outcome-owner";
    const OWNER_KEY: &str = "fabricated-outcome-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let owner_identity = authenticated_owner_identity(
        OWNER_APP.to_string(),
        &migration_owner_headers(Some(OWNER_KEY)),
    );
    for provider in AsyncMigrationSourceProvider::PUBLIC {
        let job_uuid = Uuid::new_v4();
        let target = format!("{}_fabricated_outcome", provider.as_str().unwrap());
        spool
            .create_async_migration_admission_for_provider_owner(
                job_uuid,
                &target,
                Some(&owner_identity),
                provider,
                AsyncMigrationPublicationSemantic::CreateOnly,
            )
            .unwrap();
        for phase in [
            MigrationPhase::Exporting,
            MigrationPhase::Preparing,
            MigrationPhase::Staging,
            MigrationPhase::Activating,
        ] {
            spool.transition_migration_phase(job_uuid, phase).unwrap();
        }
        spool
            .record_import_outcome(
                job_uuid,
                MigrationImportOutcome {
                    settings_applied: true,
                    objects_imported: 999,
                    synonyms_imported: 998,
                    rules_imported: 997,
                    warnings: vec![MigrationImportWarning {
                        code: "fabricated-terminal-warning".to_string(),
                        message: "must remain hidden while running".to_string(),
                        resource: "Document".to_string(),
                        page_index: Some(9),
                        item_index: Some(8),
                        json_path: "$.fabricated".to_string(),
                    }],
                },
            )
            .unwrap();
        let app = migration_job_route_for_provider(provider.as_str().unwrap(), Arc::clone(&state));

        let response = send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["targetIndex"], target);
        assert_eq!(body["phase"], "activating");
        assert_eq!(body["disposition"], "running");
        assert_import_outcome_fields_absent(&body);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_source_providers_share_one_migration_job_runner_and_spool() {
    const OWNER_APP: &str = "shared-runner-owner";
    const OWNER_KEY: &str = "shared-runner-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp)
        .with_migration_capacity(1)
        .build_shared();
    let reached_documents = Arc::new(Notify::new());
    let release_documents = Arc::new(Notify::new());
    let factory_reached = Arc::clone(&reached_documents);
    let factory_release = Arc::clone(&release_documents);
    let source_factory =
        TestMigrationSourceReaderFactory::new(move |source_provider| match source_provider {
            AsyncMigrationSourceProvider::Algolia => {
                Ok(Box::new(BlockingDocumentReadSourceReader::new(
                    async_hermetic_source_reader(),
                    Arc::clone(&factory_reached),
                    Arc::clone(&factory_release),
                )))
            }
            AsyncMigrationSourceProvider::Meilisearch => {
                Ok(Box::new(async_hermetic_meilisearch_source_reader()))
            }
            AsyncMigrationSourceProvider::Typesense => {
                Ok(Box::new(async_hermetic_typesense_source_reader()))
            }
        });

    let algolia_app = migration_job_route_for_provider_with_test_source_factory(
        "algolia",
        Arc::clone(&state),
        Some(source_factory.clone()),
    );
    let algolia_submit = send_submit_request(
        &algolia_app,
        OWNER_APP,
        OWNER_KEY,
        algolia_submit_payload("async_migrated_products"),
    )
    .await;
    assert_eq!(algolia_submit.status(), StatusCode::ACCEPTED);
    let algolia_job_uuid = job_uuid_from_submit_response(algolia_submit).await;
    tokio::time::timeout(ASYNC_LIFECYCLE_TIMEOUT, reached_documents.notified())
        .await
        .expect("shared runner should reach document export");

    let shared_runner_active_jobs = state.migration_runner.active_count_for_test();
    for (provider, payload) in [
        (
            "meilisearch",
            meilisearch_submit_payload("meilisearch_capacity_probe"),
        ),
        (
            "typesense",
            typesense_submit_payload("typesense_capacity_probe"),
        ),
    ] {
        let competing_app = migration_job_route_for_provider_with_test_source_factory(
            provider,
            Arc::clone(&state),
            Some(source_factory.clone()),
        );
        let competing_admission =
            send_submit_request(&competing_app, OWNER_APP, OWNER_KEY, payload).await;
        assert_eq!(
            competing_admission.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "{provider} must consume the shared capacity-one runner"
        );
    }
    assert_eq!(
        send_status_request(&algolia_app, algolia_job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        send_acknowledge_request(&algolia_app, algolia_job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::CONFLICT
    );
    assert_eq!(
        send_cancel_request(&algolia_app, algolia_job_uuid, OWNER_APP, OWNER_KEY)
            .await
            .status(),
        StatusCode::OK
    );
    release_documents.notify_waiters();
    let terminal =
        wait_for_route_terminal(&algolia_app, algolia_job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(terminal["disposition"], "cancelled");
    state.migration_runner.drain_active_imports().await;

    assert_eq!(
        shared_runner_active_jobs, 1,
        "provider-private runner forked lifecycle ownership: the shared runner did not account for its admitted job"
    );

    for (provider, payload) in [
        (
            "meilisearch",
            meilisearch_submit_payload("meili_shared_runner_target"),
        ),
        (
            "typesense",
            typesense_submit_payload("typesense_shared_runner_target"),
        ),
    ] {
        let app = migration_job_route_for_provider_with_test_source_factory(
            provider,
            Arc::clone(&state),
            Some(source_factory.clone()),
        );
        let submit = send_submit_request(&app, OWNER_APP, OWNER_KEY, payload).await;
        assert_eq!(submit.status(), StatusCode::ACCEPTED);
        let job_uuid = job_uuid_from_submit_response(submit).await;
        assert_eq!(
            send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
                .await
                .status(),
            StatusCode::OK
        );
        let terminal = wait_for_route_terminal(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        assert_eq!(terminal["disposition"], "succeeded");
        state.migration_runner.drain_active_imports().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scripted_meilisearch_submission_persists_provider_through_shared_lifecycle() {
    const OWNER_APP: &str = "meili-shared-owner";
    const OWNER_KEY: &str = "meili-shared-key";
    const TARGET: &str = "meili_async_products";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let owner_identity = authenticated_owner_identity(
        OWNER_APP.to_string(),
        &migration_owner_headers(Some(OWNER_KEY)),
    );

    let (job_uuid, submitted) = state
        .migration_runner
        .submit_meilisearch_import_for_owner(
            valid_meilisearch_async_request(TARGET),
            Some(owner_identity),
            |_| Ok(async_hermetic_meilisearch_source_reader()),
        )
        .await
        .expect("scripted Meilisearch submission should be admitted by the shared runner");
    assert_eq!(submitted.phase, MigrationPhase::Submitted);
    assert_eq!(submitted.disposition, MigrationDisposition::Running);
    assert_eq!(state.migration_runner.active_count_for_test(), 1);

    let metadata = spool.read_async_migration_metadata(job_uuid).unwrap();
    assert_eq!(
        metadata.source_provider,
        AsyncMigrationSourceProvider::Meilisearch
    );
    assert_eq!(
        metadata.operation_kind,
        spool::AsyncMigrationOperationKind::SourceImport
    );
    assert_eq!(metadata.target_index, TARGET);

    let app = migration_job_route_for_provider("meilisearch", Arc::clone(&state));
    let status = send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(status.status(), StatusCode::OK);
    assert_eq!(
        body_json(status).await["targetIndex"],
        json!(TARGET),
        "status must read Meilisearch metadata from the shared spool"
    );

    let terminal = wait_for_route_terminal(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(
        terminal["disposition"], "succeeded",
        "terminal status: {terminal:#}"
    );
    assert!(terminal.get("terminalAt").is_some());
    assert_eq!(
        spool
            .read_async_migration_metadata(job_uuid)
            .unwrap()
            .source_provider,
        AsyncMigrationSourceProvider::Meilisearch
    );

    let wrong_alias = migration_job_route(Arc::clone(&state));
    assert_migration_job_not_found(
        send_status_request(&wrong_alias, job_uuid, OWNER_APP, OWNER_KEY).await,
        "Algolia status for Meilisearch job",
    )
    .await;

    let acknowledge = send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(acknowledge.status(), StatusCode::NO_CONTENT);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn meilisearch_http_submission_uses_shared_cancel_recovery_ack_and_gc_lifecycle() {
    const OWNER_APP: &str = "meili-http-owner";
    const OWNER_KEY: &str = "meili-http-key";
    const TARGET: &str = "meili_http_cancel_products";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let reached_documents = Arc::new(Notify::new());
    let release_documents = Arc::new(Notify::new());
    let factory_reached = Arc::clone(&reached_documents);
    let factory_release = Arc::clone(&release_documents);
    let source_factory = TestMigrationSourceReaderFactory::new(move |source_provider| {
        assert_eq!(source_provider, AsyncMigrationSourceProvider::Meilisearch);
        Ok(Box::new(BlockingDocumentReadSourceReader::new(
            async_hermetic_meilisearch_source_reader(),
            Arc::clone(&factory_reached),
            Arc::clone(&factory_release),
        )))
    });
    let app = migration_job_route_for_provider_with_test_source_factory(
        "meilisearch",
        Arc::clone(&state),
        Some(source_factory),
    );

    let submit = send_submit_request(
        &app,
        OWNER_APP,
        OWNER_KEY,
        json!({
            "endpoint": "https://your-instance.meilisearch.io",
            "apiKey": "meili-http-secret-not-used",
            "sourceIndex": "source_products",
            "targetIndex": TARGET,
            "overwrite": false
        }),
    )
    .await;
    assert_eq!(submit.status(), StatusCode::ACCEPTED);
    let submitted = body_json(submit).await;
    let job_uuid = Uuid::parse_str(
        submitted["jobId"]
            .as_str()
            .expect("submit response must include jobId"),
    )
    .unwrap();
    assert_eq!(submitted["disposition"], "running");

    tokio::time::timeout(ASYNC_LIFECYCLE_TIMEOUT, reached_documents.notified())
        .await
        .expect("HTTP-submitted Meilisearch job should reach shared document export");
    let spool = import::spool_for_manager(&state.manager).unwrap();
    assert_eq!(
        spool
            .read_async_migration_metadata(job_uuid)
            .unwrap()
            .source_provider,
        AsyncMigrationSourceProvider::Meilisearch
    );

    let cancel = send_cancel_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(cancel.status(), StatusCode::OK);
    assert_eq!(body_json(cancel).await["disposition"], "running");
    release_documents.notify_waiters();
    let terminal = wait_for_route_terminal(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(terminal["disposition"], "cancelled");
    assert!(terminal.get("terminalAt").is_some());
    state.migration_runner.drain_active_imports().await;

    let reopened = spool::SpoolStore::new(&state.manager.base_path, spool::SpoolLimits::default())
        .expect("shared spool must reopen after HTTP Meilisearch submit");
    assert_eq!(
        reopened
            .read_async_migration_metadata(job_uuid)
            .unwrap()
            .source_provider,
        AsyncMigrationSourceProvider::Meilisearch
    );
    let wrong_alias = migration_job_route(Arc::clone(&state));
    assert_migration_job_not_found(
        send_status_request(&wrong_alias, job_uuid, OWNER_APP, OWNER_KEY).await,
        "Algolia status for HTTP-submitted Meilisearch job",
    )
    .await;

    let acknowledge = send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
    assert_eq!(acknowledge.status(), StatusCode::NO_CONTENT);
    let limits = spool::SpoolLimits {
        retention_seconds: 0,
        ..Default::default()
    };
    let gc_store = spool::SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc::now() + chrono::Duration::seconds(1),
        10_000,
    )
    .unwrap();
    gc_store.collect_garbage().unwrap();
    assert_eq!(
        gc_store.read_migration_phase(job_uuid).unwrap().disposition,
        MigrationDisposition::Cancelled,
        "terminal GC must follow the shared retention owner and keep control-plane phase readable"
    );
}

#[tokio::test]
async fn shared_migration_lifecycle_has_no_duplicate_owner_seams() {
    let sources = production_rust_sources();

    assert_shared_migration_lifecycle_owners(&sources);
}

#[test]
fn shared_migration_lifecycle_rejects_provider_private_owner_mutations() {
    let mutations = [
        (
            "provider-private runner",
            "flapjack-http/src/handlers/migration/meilisearch_runner.rs",
            "MigrationJobRunner::new(",
        ),
        (
            "provider-private spool",
            "flapjack-http/src/handlers/migration/meilisearch_spool.rs",
            "SpoolStore::new(",
        ),
        (
            "provider-private status handler",
            "flapjack-http/src/handlers/migration/meilisearch_status.rs",
            "async fn get_source_migration_status(",
        ),
        (
            "provider-private cancel handler",
            "flapjack-http/src/handlers/migration/meilisearch_cancel.rs",
            "async fn cancel_source_migration(",
        ),
        (
            "provider-private ACK handler",
            "flapjack-http/src/handlers/migration/meilisearch_ack.rs",
            "async fn acknowledge_source_migration(",
        ),
        (
            "provider-private publication receipt",
            "flapjack-http/src/handlers/migration/meilisearch_publication_receipt.rs",
            "record_async_publication_receipt_if_present(",
        ),
        (
            "provider-private publication journal",
            "src/index/manager/publication/meilisearch_journal.rs",
            "persist_journal(",
        ),
        (
            "provider-private publication journal write",
            "src/index/manager/publication/meilisearch_journal.rs",
            "io.write_file(",
        ),
        (
            "provider-private source-provider discriminator",
            "flapjack-http/src/handlers/migration/meilisearch_provider.rs",
            "pub(crate) enum AsyncMigrationSourceProvider",
        ),
        (
            "provider-private Typesense runner",
            "flapjack-http/src/handlers/migration/typesense_runner.rs",
            "MigrationJobRunner::new(",
        ),
        (
            "provider-private Typesense spool",
            "flapjack-http/src/handlers/migration/typesense_spool.rs",
            "SpoolStore::new(",
        ),
        (
            "provider-private Typesense status handler",
            "flapjack-http/src/handlers/migration/typesense_status.rs",
            "async fn get_source_migration_status(",
        ),
        (
            "provider-private Typesense cancel handler",
            "flapjack-http/src/handlers/migration/typesense_cancel.rs",
            "async fn cancel_source_migration(",
        ),
        (
            "provider-private Typesense ACK handler",
            "flapjack-http/src/handlers/migration/typesense_ack.rs",
            "async fn acknowledge_source_migration(",
        ),
        (
            "provider-private Typesense publication receipt",
            "flapjack-http/src/handlers/migration/typesense_publication_receipt.rs",
            "record_async_publication_receipt_if_present(",
        ),
        (
            "provider-private Typesense publication journal",
            "src/index/manager/publication/typesense_journal.rs",
            "persist_journal(",
        ),
        (
            "provider-private Typesense publication journal write",
            "src/index/manager/publication/typesense_journal.rs",
            "io.write_file(",
        ),
        (
            "provider-private Typesense source-provider discriminator",
            "flapjack-http/src/handlers/migration/typesense_provider.rs",
            "pub(crate) enum AsyncMigrationSourceProvider",
        ),
    ];

    for (label, path, marker) in mutations {
        let mut sources = production_rust_sources();
        sources.push((path.to_string(), marker.to_string()));

        let rejected =
            std::panic::catch_unwind(|| assert_shared_migration_lifecycle_owners(&sources));

        let panic_message = panic_message(
            rejected
                .expect_err("provider-private mutation must be rejected by the exact-owner guard"),
        );
        assert!(
            panic_message.contains(path),
            "{label} mutation must name injected path {path}; panic was {panic_message}"
        );
        assert!(
            panic_message.contains(marker),
            "{label} mutation must name injected marker {marker}; panic was {panic_message}"
        );
    }
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else if let Some(message) = payload.downcast_ref::<&'static str>() {
        (*message).to_string()
    } else {
        "<non-string panic payload>".to_string()
    }
}

fn assert_shared_migration_lifecycle_owners(sources: &[(String, String)]) {
    let expected_owners: &[(&str, &[(&str, usize)])] = &[
        (
            "MigrationJobRunner::new(",
            &[
                ("flapjack-http/src/handlers/migration/job_runner.rs", 1),
                ("flapjack-http/src/handlers/replicas.rs", 1),
                ("flapjack-http/src/server_init.rs", 1),
            ],
        ),
        (
            "SpoolStore::new(",
            &[
                ("flapjack-http/src/background_tasks.rs", 1),
                ("flapjack-http/src/handlers/migration/import.rs", 1),
                ("flapjack-http/src/handlers/migration/job_runner.rs", 1),
            ],
        ),
        (
            "create_async_migration_admission_for_owner(",
            &[
                ("flapjack-http/src/handlers/migration/job_runner.rs", 1),
                ("flapjack-http/src/handlers/migration/spool.rs", 2),
            ],
        ),
        (
            "create_async_migration_admission_for_provider_owner(",
            &[
                ("flapjack-http/src/handlers/migration/job_runner.rs", 1),
                ("flapjack-http/src/handlers/migration/spool.rs", 2),
            ],
        ),
        (
            "record_async_publication_receipt_if_present(",
            &[
                ("flapjack-http/src/handlers/migration/bulk_build.rs", 1),
                ("flapjack-http/src/handlers/migration/spool.rs", 2),
            ],
        ),
        (
            "request_async_migration_cancel(",
            &[
                ("flapjack-http/src/handlers/migration/mod.rs", 1),
                ("flapjack-http/src/handlers/migration/spool.rs", 1),
            ],
        ),
        (
            "read_migration_phase(",
            &[
                ("flapjack-http/src/handlers/migration/job_runner.rs", 2),
                ("flapjack-http/src/handlers/migration/mod.rs", 3),
                ("flapjack-http/src/handlers/migration/privacy_scrub.rs", 2),
                ("flapjack-http/src/handlers/migration/spool.rs", 11),
                ("flapjack-http/src/handlers/migration/spool_lifecycle.rs", 2),
                ("flapjack-http/src/handlers/migration/spool_support.rs", 1),
            ],
        ),
        (
            "owned_async_migration_job(",
            &[("flapjack-http/src/handlers/migration/mod.rs", 4)],
        ),
        (
            "async fn get_source_migration_status(",
            &[("flapjack-http/src/handlers/migration/mod.rs", 1)],
        ),
        (
            "async fn cancel_source_migration(",
            &[("flapjack-http/src/handlers/migration/mod.rs", 1)],
        ),
        (
            "async fn acknowledge_source_migration(",
            &[("flapjack-http/src/handlers/migration/mod.rs", 1)],
        ),
        (
            "pub migration_runner: Arc<migration::MigrationJobRunner>",
            &[("flapjack-http/src/handlers/mod.rs", 1)],
        ),
        (
            "pub struct MigrationJobRunner",
            &[("flapjack-http/src/handlers/migration/job_runner.rs", 1)],
        ),
        (
            "pub(crate) struct SpoolStore",
            &[("flapjack-http/src/handlers/migration/spool.rs", 1)],
        ),
        (
            "pub(crate) enum AsyncMigrationSourceProvider",
            &[("flapjack-http/src/handlers/migration/mod.rs", 1)],
        ),
        (
            "persist_journal(",
            &[
                ("src/index/manager/publication/executor.rs", 4),
                ("src/index/manager/publication/repair.rs", 2),
            ],
        ),
        (
            "io.write_file(",
            &[
                ("src/index/manager/publication/executor.rs", 2),
                ("src/index/manager/publication/repair.rs", 1),
            ],
        ),
    ];

    assert_exact_source_owners(sources, expected_owners);
}

fn assert_exact_source_owners(
    sources: &[(String, String)],
    expected_owners: &[(&str, &[(&str, usize)])],
) {
    for (marker, expected) in expected_owners {
        let actual: Vec<(&str, usize)> = sources
            .iter()
            .filter_map(|(path, source)| {
                let count = source.matches(marker).count();
                (count > 0).then_some((path.as_str(), count))
            })
            .collect();
        assert_eq!(
            actual, *expected,
            "shared migration lifecycle owner drift for marker {marker}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_generation_cannot_mutate_terminal_or_ack_state_for_any_provider() {
    const OWNER_APP: &str = "stale-generation-owner";
    const OWNER_KEY: &str = "stale-generation-key";

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = import::spool_for_manager(&state.manager).unwrap();
    let mut stale_ack_statuses = Vec::new();
    let source_factory =
        TestMigrationSourceReaderFactory::new(|source_provider| match source_provider {
            AsyncMigrationSourceProvider::Algolia => Ok(Box::new(async_hermetic_source_reader())),
            AsyncMigrationSourceProvider::Meilisearch => {
                Ok(Box::new(async_hermetic_meilisearch_source_reader()))
            }
            AsyncMigrationSourceProvider::Typesense => {
                Ok(Box::new(async_hermetic_typesense_source_reader()))
            }
        });

    // Every public source specimen is admitted by the one shared runner
    // that `all_source_providers_share_one_migration_job_runner_and_spool`
    // pins, and then driven over its own provider lifecycle route.
    for app in [
        migration_job_route_for_provider_with_test_source_factory(
            "algolia",
            Arc::clone(&state),
            Some(source_factory.clone()),
        ),
        migration_job_route_for_provider_with_test_source_factory(
            "meilisearch",
            Arc::clone(&state),
            Some(source_factory.clone()),
        ),
        migration_job_route_for_provider_with_test_source_factory(
            "typesense",
            Arc::clone(&state),
            Some(source_factory.clone()),
        ),
    ] {
        let source_provider = app.provider;
        let target_index = format!("{source_provider}_stale_generation_target");
        let payload = match AsyncMigrationSourceProvider::parse(source_provider).unwrap() {
            AsyncMigrationSourceProvider::Algolia => algolia_submit_payload(&target_index),
            AsyncMigrationSourceProvider::Meilisearch => meilisearch_submit_payload(&target_index),
            AsyncMigrationSourceProvider::Typesense => typesense_submit_payload(&target_index),
        };
        let submit = send_submit_request(&app, OWNER_APP, OWNER_KEY, payload).await;
        assert_eq!(submit.status(), StatusCode::ACCEPTED);
        let job_uuid = job_uuid_from_submit_response(submit).await;
        wait_for_route_terminal(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        let terminal_before = spool.read_migration_phase(job_uuid).unwrap();
        let metadata = spool.read_async_migration_metadata(job_uuid).unwrap();
        let job_row_before = std::fs::read(spool.migration_phase_path(job_uuid)).unwrap();
        let metadata_row_before =
            std::fs::read(spool.async_migration_metadata_path(job_uuid)).unwrap();
        let expected_terminal_body = serde_json::to_value(
            AsyncMigrationStatusResponse::with_metadata(terminal_before.clone(), &metadata),
        )
        .unwrap();
        let transaction_id = metadata
            .publication_transaction_id
            .clone()
            .expect("terminal import should retain its publication transaction");
        let target = PublicationTarget::new(&target_index).unwrap();
        let journal_path =
            PublicationPaths::new(&state.manager.base_path, &target, &transaction_id).journal;
        let mut journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        journal["generation"] = json!(format!("stale-replacement-{source_provider}"));
        std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let reservation_before = target_publication_residue_snapshot(&state, &target_index);

        let status = send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        assert_eq!(status.status(), StatusCode::OK);
        assert_eq!(
            body_json(status).await,
            expected_terminal_body,
            "{source_provider} stale generation status must expose the exact terminal job"
        );
        let cancel = send_cancel_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        assert_eq!(cancel.status(), StatusCode::OK);
        assert_eq!(
            body_json(cancel).await,
            expected_terminal_body,
            "{source_provider} late cancel must expose the exact unchanged terminal job"
        );
        let acknowledge = send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        stale_ack_statuses.push((source_provider, acknowledge.status()));
        assert_eq!(
            body_json(acknowledge).await,
            json!({
                "message": "Migration publication generation evidence is stale or unavailable",
                "status": 409,
                "code": "migration_ack_stale_generation"
            }),
            "{source_provider} stale generation ACK must expose the stable refusal body"
        );
        assert_eq!(
            spool.read_migration_phase(job_uuid).unwrap(),
            terminal_before,
            "{source_provider} stale lifecycle calls mutated the parsed terminal job row"
        );
        assert_eq!(
            std::fs::read(spool.migration_phase_path(job_uuid)).unwrap(),
            job_row_before,
            "{source_provider} stale lifecycle calls rewrote the terminal job row"
        );
        assert_eq!(
            std::fs::read(spool.async_migration_metadata_path(job_uuid)).unwrap(),
            metadata_row_before,
            "{source_provider} stale lifecycle calls rewrote the migration metadata row"
        );
        assert_eq!(
            target_publication_residue_snapshot(&state, &target_index),
            reservation_before,
            "{source_provider} stale lifecycle calls changed target reservation or publication state"
        );
    }

    assert_eq!(
        stale_ack_statuses,
        vec![
            ("algolia", StatusCode::CONFLICT),
            ("meilisearch", StatusCode::CONFLICT),
            ("typesense", StatusCode::CONFLICT)
        ],
        "stale generation ACK mutated ACK-visible state: every provider must fail closed before acknowledging a superseded terminal generation"
    );
}

fn production_rust_sources() -> Vec<(String, String)> {
    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let engine_root = manifest_dir
        .parent()
        .expect("flapjack-http must live under engine");
    let mut sources = Vec::new();
    collect_production_rust_sources(&manifest_dir.join("src"), engine_root, &mut sources);
    collect_production_rust_sources(
        &engine_root.join("src/index/manager/publication"),
        engine_root,
        &mut sources,
    );
    sources.sort_by(|left, right| left.0.cmp(&right.0));
    sources
}

fn collect_production_rust_sources(
    directory: &std::path::Path,
    engine_root: &std::path::Path,
    sources: &mut Vec<(String, String)>,
) {
    for entry in std::fs::read_dir(directory).expect("production source directory must be readable")
    {
        let path = entry
            .expect("production source entry must be readable")
            .path();
        if path.is_dir() {
            collect_production_rust_sources(&path, engine_root, sources);
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.extension().and_then(|extension| extension.to_str()) != Some("rs")
            || file_name.contains("_test")
            || file_name.starts_with("test_")
        {
            continue;
        }
        let relative = path
            .strip_prefix(engine_root)
            .expect("source must stay under engine")
            .to_string_lossy()
            .replace('\\', "/");
        let source =
            std::fs::read_to_string(&path).expect("production Rust source must be readable");
        sources.push((relative, source));
    }
}

async fn assert_migration_job_not_found(response: Response<Body>, operation: &str) {
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "{operation} must hide the owned job UUID"
    );
    assert_eq!(
        body_json(response).await,
        json!({
            "message": "Migration job not found",
            "status": 404,
            "code": "migration_job_not_found"
        }),
        "{operation} must return the stable owner-isolation response"
    );
}

async fn sorted_async_target_hits(
    state: &Arc<AppState>,
    target: &str,
) -> Vec<(String, String, String)> {
    sorted_exact_hits_by_object_id(
        state,
        target,
        10,
        "runner-created target should remain queryable",
        |hit| {
            (
                hit["objectID"].as_str().unwrap().to_string(),
                hit["title"].as_str().unwrap().to_string(),
                hit["category"].as_str().unwrap().to_string(),
            )
        },
    )
    .await
}

fn valid_async_request() -> MigrateFromAlgoliaRequest {
    MigrateFromAlgoliaRequest {
        app_id: "LOCALMIGRATIONTEST".to_string(),
        api_key: "hermetic-source-key-not-used".to_string(),
        source_index: "source_products".to_string(),
        target_index: Some("async_migrated_products".to_string()),
        overwrite: false,
    }
}

fn valid_meilisearch_async_request(target_index: &str) -> MigrateFromMeilisearchRequest {
    MigrateFromMeilisearchRequest {
        endpoint: "https://your-instance.meilisearch.io".to_string(),
        api_key: "hermetic-meilisearch-key-not-used".to_string(),
        source_index: "source_products".to_string(),
        target_index: Some(target_index.to_string()),
        overwrite: false,
    }
}

fn async_hermetic_source_reader() -> ScriptedSourceReader {
    async_source_reader_with_final_updated_at("2026-07-16T00:00:00Z")
}

fn async_source_reader_with_final_drift() -> ScriptedSourceReader {
    async_source_reader_with_final_updated_at("2026-07-16T00:01:00Z")
}

fn async_source_reader_with_final_updated_at(final_updated_at: &str) -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new("LOCALMIGRATIONTEST", "source_products");
    let source_record = AlgoliaIndexRecord {
        name: "source_products".to_string(),
        entries: 2,
        updated_at: "2026-07-16T00:00:00Z".to_string(),
        pending_task: false,
    };
    reader.push_quiescent(source_record.clone());
    let settings = json!({
        "searchableAttributes": ["title"],
        "attributesForFaceting": ["category"]
    });
    let document_pages = vec![vec![
        json!({"objectID": "doc-1", "title": "Quartz adapter", "category": "hardware"}),
        json!({"objectID": "doc-2", "title": "Velvet compass", "category": "navigation"}),
    ]];
    reader.push_pass(settings.clone(), document_pages.clone(), vec![], vec![]);
    reader.push_pass(settings, document_pages, vec![], vec![]);
    reader.push_quiescent(AlgoliaIndexRecord {
        updated_at: final_updated_at.to_string(),
        ..source_record
    });
    reader
}

fn async_hermetic_meilisearch_source_reader() -> MeilisearchSourceReader<ScriptedMeilisearchSource>
{
    let bundle = read_meilisearch_fixture_json("expected_bundle.json");
    let settings = bundle["settings"].clone();
    let documents = bundle["documents"]["beforeMutation"]
        .as_array()
        .unwrap()
        .to_vec();
    let document_pages = vec![documents[..2].to_vec(), documents[2..].to_vec()];
    let source = ScriptedMeilisearchSource::with_passes(
        meilisearch_observation("source_products", "sku", 3),
        settings,
        vec![document_pages.clone(), document_pages],
    );
    MeilisearchSourceReader::from_source("source_products", source)
}

fn async_hermetic_typesense_source_reader() -> TypesenseSourceReader<ScriptedTypesenseSource> {
    let settings = json!({
        "default_sorting_field": "price",
        "enable_nested_fields": true,
        "token_separators": ["-"],
        "symbols_to_index": ["#"]
    });
    let document_pages = vec![vec![
        json!({"id": "doc-1", "title": "Quartz adapter", "category": "hardware"}),
        json!({"id": "doc-2", "title": "Velvet compass", "category": "navigation"}),
    ]];
    let source = ScriptedTypesenseSource::with_passes(
        typesense_observation("source_products", 2),
        settings,
        vec![document_pages.clone(), document_pages],
    );
    TypesenseSourceReader::from_source("source_products", source, true)
}

fn read_meilisearch_fixture_json(name: &str) -> serde_json::Value {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/2026_07_26_m0a_meilisearch_source_contract")
        .join(name);
    serde_json::from_slice(
        &std::fs::read(&path)
            .unwrap_or_else(|error| panic!("fixture {} must be readable: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("fixture {} must be JSON: {error}", path.display()))
}

fn async_source_reader_with_import_outcome() -> ScriptedSourceReader {
    let mut reader = ScriptedSourceReader::new("LOCALMIGRATIONTEST", "source_products");
    let source_record = AlgoliaIndexRecord {
        name: "source_products".to_string(),
        entries: 2,
        updated_at: "2026-07-16T00:00:00Z".to_string(),
        pending_task: false,
    };
    reader.push_quiescent(source_record.clone());
    let settings = json!({
        "searchableAttributes": ["title"],
        "attributesForFaceting": ["category"],
        "replicas": ["replica_idx"]
    });
    let document_pages = vec![vec![
        json!({"objectID": "doc-1", "title": "Quartz adapter", "category": "hardware"}),
        json!({"objectID": "doc-2", "title": "Velvet compass", "category": "navigation"}),
    ]];
    let rule_pages = vec![vec![
        json!({
            "objectID": "rule-promote",
            "conditions": [{"pattern": "sale", "anchoring": "contains"}],
            "consequence": {
                "promote": [{"objectID": "doc-1", "position": 1}],
                "params": {
                    "query": {"remove": ["cheap"], "edits": [{"type": "remove", "delete": "cheap"}]},
                    "automaticFacetFilters": [{"facet": "brand", "score": 4}]
                }
            },
            "enabled": true
        }),
        json!({
            "objectID": "rule-hide",
            "conditions": [{"pattern": "sale", "anchoring": "contains"}],
            "consequence": {
                "promote": [{"objectID": "doc-1", "position": 1}],
                "params": {
                    "query": {"remove": ["cheap"], "edits": [{"type": "remove", "delete": "cheap"}]},
                    "automaticFacetFilters": [{"facet": "brand", "score": 4}]
                }
            },
            "enabled": true
        }),
    ]];
    let synonym_pages = vec![vec![json!({
        "objectID": "synonym-shoes",
        "type": "synonym",
        "synonyms": ["sneaker", "trainer"]
    })]];
    reader.push_pass(
        settings.clone(),
        document_pages.clone(),
        rule_pages.clone(),
        synonym_pages.clone(),
    );
    reader.push_pass(settings, document_pages, rule_pages, synonym_pages);
    reader.push_index_settings_for_capture_passes([(
        "replica_idx",
        json!({
            "ranking": ["desc(price)"],
            "relevancyStrictness": 80,
            "searchableAttributes": ["title"]
        }),
    )]);
    reader.push_quiescent(source_record);
    reader
}

/// TODO: Document wait_for_async_terminal.
async fn wait_for_async_terminal(
    state: &Arc<AppState>,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: Option<&str>,
) -> AsyncMigrationStatusResponse {
    tokio::time::timeout(ASYNC_STATUS_TERMINAL_TIMEOUT, async {
        loop {
            let Json(current) = get_algolia_migration_status_http(
                State(Arc::clone(state)),
                axum::extract::Extension(AuthenticatedAppId(authenticated_app_id.to_string())),
                None,
                migration_owner_headers(api_key),
                AxumPath(job_uuid.to_string()),
            )
            .await
            .expect("status should remain readable");
            if current.disposition != AsyncMigrationDisposition::Running {
                assert!(current.terminal_at.is_some());
                break current;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("async import should finish after release")
}

async fn wait_for_route_terminal(
    app: &MigrationLifecycleRoutes,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: &str,
) -> serde_json::Value {
    tokio::time::timeout(ASYNC_STATUS_TERMINAL_TIMEOUT, async {
        loop {
            let response = send_status_request(app, job_uuid, authenticated_app_id, api_key).await;
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            if body["disposition"] != "running" {
                assert!(body.get("terminalAt").is_some());
                break body;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("async import should finish after release")
}

fn migration_owner_headers(api_key: Option<&str>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if let Some(api_key) = api_key {
        headers.insert("x-algolia-api-key", api_key.parse().unwrap());
    }
    headers
}

fn seed_ack_generation_evidence(
    state: &Arc<AppState>,
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    target_index: &str,
) {
    let target = PublicationTarget::new(target_index).unwrap();
    let transaction_id =
        PublicationTransactionId::new(format!("async_status_{job_uuid}_current_gen")).unwrap();
    let generation =
        PublicationGenerationEvidence::new(format!("async_status_{job_uuid}_generation")).unwrap();
    let paths = PublicationPaths::new(&state.manager.base_path, &target, &transaction_id);
    let journal = PublicationJournal::prepare(
        transaction_id.clone(),
        target,
        generation.clone(),
        ContentDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap(),
        paths.clone(),
    )
    .apply(PublicationEvent::Commit)
    .unwrap();

    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(paths.journal, journal.to_json_value().to_string()).unwrap();
    spool
        .record_async_publication_receipt_if_present(job_uuid, transaction_id, Some(generation))
        .unwrap();
}

struct BlockingDocumentReadSourceReader<R> {
    inner: R,
    reached_documents: Arc<Notify>,
    release_documents: Arc<Notify>,
    blocked_once: bool,
}

impl<R> BlockingDocumentReadSourceReader<R> {
    fn new(inner: R, reached_documents: Arc<Notify>, release_documents: Arc<Notify>) -> Self {
        Self {
            inner,
            reached_documents,
            release_documents,
            blocked_once: false,
        }
    }
}

impl<R> MigrationSourceReader for BlockingDocumentReadSourceReader<R>
where
    R: MigrationSourceReader + Send,
{
    fn source_provider(&self) -> AsyncMigrationSourceProvider {
        self.inner.source_provider()
    }

    fn source_namespace(&self) -> Option<&str> {
        self.inner.source_namespace()
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

    fn read_derived_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        self.inner.read_derived_configuration(consume)
    }

    fn read_document_records<'a>(
        &'a mut self,
        consume_page: &'a mut SourceDocumentPageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            if !self.blocked_once {
                self.blocked_once = true;
                self.reached_documents.notify_one();
                self.release_documents.notified().await;
            }
            self.inner.read_document_records(consume_page).await
        })
    }
}
