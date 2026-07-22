use super::*;
use crate::auth::AuthenticatedAppId;
use crate::handlers::migration::algolia_client::AlgoliaIndexRecord;
use crate::handlers::migration::source_reader::{
    MigrationSourceReader, PageConsumer, SourceFuture,
};
use crate::handlers::migration::source_test_support::ScriptedSourceReader;
use crate::handlers::migration::spool::{
    MigrationDisposition, MigrationExportProgress, MigrationPhase, MigrationPhaseRecord,
};
use crate::test_helpers::{body_json, TestStateBuilder};
use axum::extract::Path as AxumPath;
use axum::response::IntoResponse;
use chrono::{TimeZone, Utc};
use serde_json::json;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tempfile::TempDir;
use tokio::sync::Notify;
use uuid::Uuid;

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
    };

    let body = serde_json::to_value(AsyncMigrationStatusResponse::from(record)).unwrap();

    assert_eq!(body["disposition"], "cancelled");
    assert_eq!(body["terminalAt"], "2026-07-15T12:02:00Z");
    assert!(body.get("cancelRequested").is_none());
    assert!(body.get("cancel_requested").is_none());
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
    wait_for_async_terminal(&state, submitted.job_id).await;
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
        )
        .unwrap();
    let cancelled_before = spool.cancel_migration(cancelled).unwrap();
    spool
        .create_async_migration_admission_for_owner(
            failed,
            "already_failed",
            Some("async-owner-app"),
        )
        .unwrap();
    let failed_before = spool.fail_migration(failed).unwrap();
    spool
        .create_async_migration_admission_for_owner(
            succeeded,
            "already_succeeded",
            Some("async-owner-app"),
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
    let succeeded_before = spool.succeed_migration(succeeded).unwrap();

    for (job_uuid, expected) in [
        (cancelled, cancelled_before),
        (failed, failed_before),
        (succeeded, succeeded_before),
    ] {
        let Json(status) = cancel_algolia_migration(
            State(Arc::clone(&state)),
            axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
            AxumPath(job_uuid.to_string()),
        )
        .await
        .expect("terminal cancel should be a no-op status read");

        assert_eq!(status, AsyncMigrationStatusResponse::from(expected.clone()));
        assert_eq!(spool.read_migration_phase(job_uuid).unwrap(), expected);
    }
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
        .create_async_migration_admission_for_owner(job_uuid, "owned_target", Some("owner-app"))
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
    reader.push_pass(
        json!({
            "searchableAttributes": ["title"],
            "attributesForFaceting": ["category"]
        }),
        vec![vec![
            json!({"objectID": "doc-1", "title": "Quartz adapter", "category": "hardware"}),
            json!({"objectID": "doc-2", "title": "Velvet compass", "category": "navigation"}),
        ]],
        vec![],
        vec![],
    );
    reader.push_quiescent(source_record);
    reader
}

async fn wait_for_async_terminal(state: &Arc<AppState>, job_uuid: Uuid) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let Json(current) = get_algolia_migration_status(
                State(Arc::clone(state)),
                axum::extract::Extension(AuthenticatedAppId("async-owner-app".to_string())),
                AxumPath(job_uuid.to_string()),
            )
            .await
            .expect("status should remain readable");
            if current.disposition != AsyncMigrationDisposition::Running {
                assert!(current.terminal_at.is_some());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("async import should finish after release");
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
