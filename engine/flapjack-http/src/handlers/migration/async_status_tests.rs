use super::*;
use crate::auth::AuthenticatedAppId;
use crate::handlers::migration::algolia_client::AlgoliaIndexRecord;
use crate::handlers::migration::source_reader::{
    MigrationSourceReader, PageConsumer, SourceFuture,
};
use crate::handlers::migration::source_test_support::{
    sorted_exact_hits_by_object_id, ScriptedSourceReader,
};
use crate::handlers::migration::spool::{
    AsyncMigrationPublicationSemantic, MigrationDisposition, MigrationExportProgress,
    MigrationImportOutcome, MigrationImportWarning, MigrationPhase, MigrationPhaseRecord,
};
use crate::test_helpers::{body_json, TestStateBuilder};
use axum::body::Body;
use axum::extract::Path as AxumPath;
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
use std::collections::BTreeSet;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use tower::ServiceExt;
use uuid::Uuid;

fn assert_import_outcome_fields_absent(body: &serde_json::Value) {
    assert!(body.get("settingsApplied").is_none());
    assert!(body.get("synonymsImported").is_none());
    assert!(body.get("rulesImported").is_none());
    assert!(body.get("warnings").is_none());
}

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

    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reached_documents.notified(),
    )
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
    let terminal = wait_for_async_terminal(&state, submitted.job_id, "async-owner-app", None).await;
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
    let provider_routes = Router::new()
        .route("/", post(submit_algolia_migration_http))
        .route("/:job_id", get(get_algolia_migration_status_http))
        .route(
            "/:job_id/acknowledge",
            post(acknowledge_algolia_migration_http),
        )
        .route("/:job_id/cancel", post(cancel_algolia_migration_http))
        .with_state(state);
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
async fn async_submit_spool_failure_returns_sanitized_500_without_spawning_source() {
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
    assert!(!source_factory_invoked.load(Ordering::SeqCst));
    assert_eq!(state.migration_runner.active_count_for_test(), 0);
}

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

async fn assert_unsupported_provider_skips_source_factory(
    source_provider: AsyncMigrationSourceProvider,
    state: Arc<AppState>,
) {
    let source_factory_observed = Arc::new(AtomicBool::new(false));
    let factory_error = submit_source_migration_with_test_source_factory(
        source_provider,
        State(state),
        axum::extract::Extension(AuthenticatedAppId("unsupported-provider-owner".to_string())),
        Json(valid_async_request()),
        {
            let source_factory_observed = Arc::clone(&source_factory_observed);
            move |_| -> Result<ScriptedSourceReader, AlgoliaClientError> {
                source_factory_observed.store(true, Ordering::SeqCst);
                Err(AlgoliaClientError::new(
                    AlgoliaErrorKind::Validation,
                    "unsupported provider reached source construction",
                ))
            }
        },
    )
    .await
    .expect_err("recognized unsupported providers must fail before source construction");
    let (factory_error_status, Json(factory_error_body)) = factory_error;

    assert_eq!(
        (
            source_factory_observed.load(Ordering::SeqCst),
            factory_error_status,
            factory_error_body["code"].as_str(),
        ),
        (
            false,
            StatusCode::BAD_REQUEST,
            Some(SOURCE_PROVIDER_UNSUPPORTED_CODE),
        ),
        "{} must not construct an outbound source reader",
        source_provider.as_str().unwrap()
    );
}

#[tokio::test]
async fn unsupported_source_provider_admission_refuses_before_persistence_or_source_use() {
    const SPOOL_POISON: &[u8] = b"unsupported provider must not access the migration spool";

    let tmp = TempDir::new().unwrap();
    let spool_root = tmp.path().join("migration_exports");
    std::fs::write(&spool_root, SPOOL_POISON).unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    for app in public_provider_migration_routes(&state)
        .into_iter()
        .filter(|app| app.provider != AsyncMigrationSourceProvider::Algolia.as_str().unwrap())
    {
        let source_provider = AsyncMigrationSourceProvider::parse(app.provider).unwrap();
        assert_unsupported_provider_skips_source_factory(source_provider, Arc::clone(&state)).await;

        let response = send_submit_request(
            &app,
            "unsupported-provider-owner",
            "unsupported-provider-key",
            json!({
                "appId": "unused_app",
                "apiKey": "unused_key",
                "sourceIndex": "source",
                "targetIndex": format!("{}_target", app.provider)
            }),
        )
        .await;

        assert_eq!(state.migration_runner.active_count_for_test(), 0);
        assert_eq!(
            std::fs::read(&spool_root).expect("poisoned spool root must remain readable"),
            SPOOL_POISON,
            "{} admission must not replace or mutate the poisoned spool root",
            app.provider
        );
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "{} must be refused before credential use or durable admission",
            app.provider
        );
        let body = body_json(response).await;
        assert_eq!(body["code"], SOURCE_PROVIDER_UNSUPPORTED_CODE);
        assert_eq!(body["message"], SOURCE_PROVIDER_UNSUPPORTED_MESSAGE);
        assert_eq!(body["status"], 400);
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

fn bind_job_specimen_to_provider(
    spool: &spool::SpoolStore,
    job_uuid: Uuid,
    source_provider: AsyncMigrationSourceProvider,
) {
    let path = spool.async_migration_metadata_path(job_uuid);
    let mut wire: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    match source_provider.as_str().unwrap() {
        "algolia" => {
            wire.as_object_mut().unwrap().remove("source_provider");
        }
        provider => {
            wire["source_provider"] = json!(provider);
        }
    }
    std::fs::write(&path, serde_json::to_vec(&wire).unwrap()).unwrap();
    assert_eq!(
        spool
            .read_async_migration_metadata(job_uuid)
            .unwrap()
            .source_provider,
        source_provider
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
    let owner_identity = authenticated_owner_identity(
        OWNER_APP.to_string(),
        &migration_owner_headers(Some(OWNER_KEY)),
    );

    let (job_uuid, _) = state
        .migration_runner
        .submit_algolia_import_for_owner(valid_async_request(), Some(owner_identity), {
            let reached_documents = Arc::clone(&reached_documents);
            let release_documents = Arc::clone(&release_documents);
            move |_| {
                Ok(BlockingDocumentReadSourceReader::new(
                    async_hermetic_source_reader(),
                    reached_documents,
                    release_documents,
                ))
            }
        })
        .await
        .expect("provider admission should flow through the shared lifecycle owner");
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        reached_documents.notified(),
    )
    .await
    .expect("shared runner should reach document export");

    let shared_runner_active_jobs = state.migration_runner.active_count_for_test();
    let second_admission = state
        .migration_runner
        .submit_algolia_import_for_owner(valid_async_request(), None, |_| {
            Ok(async_hermetic_source_reader())
        })
        .await
        .expect_err("shared provider admission must respect the one shared capacity limit");
    assert_eq!(second_admission.0, StatusCode::SERVICE_UNAVAILABLE);
    for app in public_provider_migration_routes(&state) {
        bind_job_specimen_to_provider(
            &import::spool_for_manager(&state.manager).unwrap(),
            job_uuid,
            AsyncMigrationSourceProvider::parse(app.provider).unwrap(),
        );
        assert_eq!(
            send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
                .await
                .status(),
            StatusCode::OK,
            "{} status must use the shared spool",
            app.provider
        );
        assert_eq!(
            send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
                .await
                .status(),
            StatusCode::CONFLICT,
            "{} ACK must observe the shared running state",
            app.provider
        );
        assert_eq!(
            send_cancel_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
                .await
                .status(),
            StatusCode::OK,
            "{} cancel must mutate the shared spool",
            app.provider
        );
    }
    bind_job_specimen_to_provider(
        &import::spool_for_manager(&state.manager).unwrap(),
        job_uuid,
        AsyncMigrationSourceProvider::Algolia,
    );
    release_documents.notify_waiters();
    let terminal = wait_for_async_terminal(&state, job_uuid, OWNER_APP, Some(OWNER_KEY)).await;
    assert_eq!(terminal.disposition, AsyncMigrationDisposition::Cancelled);

    assert_eq!(
        shared_runner_active_jobs, 1,
        "provider-private runner forked lifecycle ownership: the shared runner did not account for its admitted job"
    );
}

#[tokio::test]
async fn shared_migration_lifecycle_has_no_duplicate_owner_seams() {
    let sources = production_rust_sources();

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
                ("flapjack-http/src/handlers/migration/job_runner.rs", 2),
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
                ("flapjack-http/src/handlers/migration/job_runner.rs", 1),
                ("flapjack-http/src/handlers/migration/mod.rs", 4),
                ("flapjack-http/src/handlers/migration/spool.rs", 11),
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

    assert_exact_source_owners(&sources, expected_owners);
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
    let owner_identity = authenticated_owner_identity(
        OWNER_APP.to_string(),
        &migration_owner_headers(Some(OWNER_KEY)),
    );
    let mut stale_ack_statuses = Vec::new();

    // Every specimen is admitted by the one shared runner that
    // `all_source_providers_share_one_migration_job_runner_and_spool` pins, and
    // then driven over their own provider's lifecycle routes: the guard under
    // test has to hold for every provider wire entry, not just Algolia's.
    for app in public_provider_migration_routes(&state) {
        let source_provider = app.provider;
        let target_index = format!("{source_provider}_stale_generation_target");
        let request = MigrateFromAlgoliaRequest {
            target_index: Some(target_index.clone()),
            ..valid_async_request()
        };
        let (job_uuid, _) = state
            .migration_runner
            .submit_algolia_import_for_owner(request, Some(owner_identity.clone()), |_| {
                Ok(async_hermetic_source_reader())
            })
            .await
            .expect("provider specimen should reach the current shared lifecycle");
        wait_for_async_terminal(&state, job_uuid, OWNER_APP, Some(OWNER_KEY)).await;
        bind_job_specimen_to_provider(
            &spool,
            job_uuid,
            AsyncMigrationSourceProvider::parse(app.provider).unwrap(),
        );
        let terminal_before = spool.read_migration_phase(job_uuid).unwrap();
        let metadata = spool.read_async_migration_metadata(job_uuid).unwrap();
        let transaction_id = metadata
            .publication_transaction_id
            .expect("terminal import should retain its publication transaction");
        let target = PublicationTarget::new(target_index).unwrap();
        let journal_path =
            PublicationPaths::new(&state.manager.base_path, &target, &transaction_id).journal;
        let mut journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        journal["generation"] = json!(format!("stale-replacement-{source_provider}"));
        std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();

        assert_eq!(
            send_status_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
                .await
                .status(),
            StatusCode::OK
        );
        assert_eq!(
            send_cancel_request(&app, job_uuid, OWNER_APP, OWNER_KEY)
                .await
                .status(),
            StatusCode::OK
        );
        let acknowledge = send_acknowledge_request(&app, job_uuid, OWNER_APP, OWNER_KEY).await;
        stale_ack_statuses.push((source_provider, acknowledge.status()));
        assert_eq!(
            spool.read_migration_phase(job_uuid).unwrap(),
            terminal_before
        );
    }

    assert_eq!(
        stale_ack_statuses,
        AsyncMigrationSourceProvider::PUBLIC
            .into_iter()
            .map(|provider| (provider.as_str().unwrap(), StatusCode::CONFLICT))
            .collect::<Vec<_>>(),
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

fn async_hermetic_source_reader() -> ScriptedSourceReader {
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
    reader.push_quiescent(source_record);
    reader
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
    reader.push_index_settings(
        "replica_idx",
        Ok(json!({
            "ranking": ["desc(price)"],
            "relevancyStrictness": 80,
            "searchableAttributes": ["title"]
        })),
    );
    reader.push_quiescent(source_record);
    reader
}

async fn wait_for_async_terminal(
    state: &Arc<AppState>,
    job_uuid: Uuid,
    authenticated_app_id: &str,
    api_key: Option<&str>,
) -> AsyncMigrationStatusResponse {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
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

struct BlockingDocumentReadSourceReader {
    inner: ScriptedSourceReader,
    reached_documents: Arc<Notify>,
    release_documents: Arc<Notify>,
    blocked_once: bool,
}

impl BlockingDocumentReadSourceReader {
    fn new(
        inner: ScriptedSourceReader,
        reached_documents: Arc<Notify>,
        release_documents: Arc<Notify>,
    ) -> Self {
        Self {
            inner,
            reached_documents,
            release_documents,
            blocked_once: false,
        }
    }
}

impl MigrationSourceReader for BlockingDocumentReadSourceReader {
    fn app_id(&self) -> &str {
        self.inner.app_id()
    }

    fn source_name(&self) -> &str {
        self.inner.source_name()
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        self.inner.wait_for_quiescent_source()
    }

    fn read_settings(&mut self) -> SourceFuture<'_, serde_json::Value> {
        self.inner.read_settings()
    }

    fn read_index_settings<'a>(
        &'a mut self,
        index_name: &'a str,
    ) -> SourceFuture<'a, serde_json::Value> {
        self.inner.read_index_settings(index_name)
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        settings: &'a serde_json::Value,
    ) -> SourceFuture<'a, ()> {
        self.inner.require_unretrievable_access(settings)
    }

    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            if !self.blocked_once {
                self.blocked_once = true;
                self.reached_documents.notify_one();
                self.release_documents.notified().await;
            }
            self.inner.read_documents(consume_page).await
        })
    }

    fn read_rules<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        self.inner.read_rules(consume_page)
    }

    fn read_synonyms<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        self.inner.read_synonyms(consume_page)
    }
}
