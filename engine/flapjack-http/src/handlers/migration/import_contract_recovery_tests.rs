use super::*;
use flapjack::index::manager::publication::{
    PreStagedPublication, PublicationTarget, PublicationTargetDisposition, PublicationTransactionId,
};
use flapjack::index::settings::IndexSettings;
use flapjack::types::Document;

#[tokio::test]
async fn async_recovery_leaves_terminal_jobs_untouched() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let failed = uuid::Uuid::new_v4();
    let succeeded = uuid::Uuid::new_v4();

    spool
        .create_async_migration_admission(failed, "terminal_failed")
        .unwrap();
    spool.fail_migration(failed).unwrap();
    spool
        .create_async_migration_admission(succeeded, "terminal_succeeded")
        .unwrap();
    advance_to_activating(&spool, succeeded);
    spool.succeed_migration(succeeded).unwrap();
    let failed_before = spool.read_migration_phase(failed).unwrap();
    let succeeded_before = spool.read_migration_phase(succeeded).unwrap();

    let reports = state.manager.repair_publications_before_serve().unwrap();
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    assert_eq!(spool.read_migration_phase(failed).unwrap(), failed_before);
    assert_eq!(
        spool.read_migration_phase(succeeded).unwrap(),
        succeeded_before
    );
}

#[tokio::test]
async fn async_recovery_settles_safe_nonterminal_jobs_failed() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let jobs = [
        admitted_async_job(&spool, "submitted_only", None),
        admitted_async_job(&spool, "exporting_only", Some(MigrationPhase::Exporting)),
        admitted_async_job(&spool, "preparing_only", Some(MigrationPhase::Preparing)),
        admitted_async_job(&spool, "staging_only", Some(MigrationPhase::Staging)),
        admitted_async_job(
            &spool,
            "activating_without_tx",
            Some(MigrationPhase::Activating),
        ),
    ];

    let reports = state.manager.repair_publications_before_serve().unwrap();
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    for job_uuid in jobs {
        let phase = spool.read_migration_phase(job_uuid).unwrap();
        assert_eq!(phase.disposition, MigrationDisposition::Failed);
        assert!(
            phase.terminal_at.is_some(),
            "recovery must persist a terminal failed phase for {job_uuid}"
        );
    }
}

#[tokio::test]
async fn async_recovery_preserves_preexisting_target_before_publication_prepare() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;
    let before = directory_snapshot(&state.manager.base_path.join(TARGET_INDEX));
    let spool = spool_for_state(&state);
    let job_uuid = admitted_async_job(&spool, TARGET_INDEX, Some(MigrationPhase::Preparing));

    let reports = state.manager.repair_publications_before_serve().unwrap();
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Failed);
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join(TARGET_INDEX)),
        before,
        "a target that predates publication preparation must survive byte-for-byte"
    );
    assert_preexisting_target_resources(&state, TARGET_INDEX).await;
}

#[tokio::test]
async fn async_recovery_settles_cancel_requested_without_publication_cancelled() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let job_uuid = admitted_async_job(&spool, "cancel_without_tx", Some(MigrationPhase::Preparing));
    spool.request_migration_cancel(job_uuid).unwrap();

    let reports = state.manager.repair_publications_before_serve().unwrap();
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Cancelled);
    assert!(phase.terminal_at.is_some());
    assert!(!state.manager.base_path.join("cancel_without_tx").exists());
}

#[tokio::test]
async fn async_recovery_aborts_unjournaled_cancelled_publication_and_preserves_preexisting_target()
{
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_preexisting_target_resources(&state, TARGET_INDEX).await;
    let before = directory_snapshot(&state.manager.base_path.join(TARGET_INDEX));
    let (job_uuid, transaction_namespace) =
        create_unjournaled_async_publication(&state, TARGET_INDEX).await;
    let spool = spool_for_state(&state);
    spool.request_migration_cancel(job_uuid).unwrap();

    let reports = state.manager.repair_publications_before_serve().unwrap();
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Cancelled);
    assert!(phase.terminal_at.is_some());
    assert!(
        !transaction_namespace.exists(),
        "cancel recovery must remove only the unjournaled publication transaction"
    );
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join(TARGET_INDEX)),
        before,
        "cancel recovery must preserve a pre-existing destination byte-for-byte"
    );
    assert_preexisting_target_resources(&state, TARGET_INDEX).await;
}

#[tokio::test]
async fn async_recovery_treats_cancel_requested_committed_publication_as_succeeded() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let job_uuid = create_committed_async_job(&state, "cancel_committed_primary", Vec::new()).await;
    spool.request_migration_cancel(job_uuid).unwrap();
    let before = directory_snapshot(&state.manager.base_path.join("cancel_committed_primary"));

    let reports = state.manager.repair_publications_before_serve().unwrap();
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Succeeded);
    assert!(phase.terminal_at.is_some());
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join("cancel_committed_primary")),
        before,
        "post-commit cancel recovery must preserve the committed target"
    );
    assert_eq!(
        query_hit_count(&state, "cancel_committed_primary", "Recovery document").await,
        1
    );
}

#[tokio::test]
async fn async_recovery_removes_committed_job_owned_primary_and_replica_sidecars() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let job_uuid = create_committed_async_job(
        &state,
        "recovery_primary",
        vec![
            "virtual(recovery_replica_sidecar)".to_string(),
            "virtual(recovery_replica_empty)".to_string(),
        ],
    )
    .await;
    write_replica_sidecar(&state, "recovery_replica_sidecar", "recovery_primary");
    std::fs::create_dir(state.manager.base_path.join("recovery_replica_empty")).unwrap();

    let reports = state.manager.repair_publications_before_serve().unwrap();
    assert!(reports.iter().any(|report| {
        report.target.as_str() == "recovery_primary"
            && report.disposition == PublicationTargetDisposition::Loadable
    }));
    state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .unwrap();

    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Failed);
    assert!(!state.manager.base_path.join("recovery_primary").exists());
    assert!(!state
        .manager
        .base_path
        .join("recovery_replica_sidecar")
        .exists());
    assert!(!state
        .manager
        .base_path
        .join("recovery_replica_empty")
        .exists());
}

#[tokio::test]
async fn async_recovery_fails_closed_on_mismatched_publication_transaction() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let job_uuid = create_committed_async_job(&state, "mismatch_primary", Vec::new()).await;
    let mut reports = state.manager.repair_publications_before_serve().unwrap();
    let report = reports
        .iter_mut()
        .find(|report| report.target.as_str() == "mismatch_primary")
        .expect("publication repair should report the committed target");
    report.transaction_id = Some(PublicationTransactionId::new("different_tx").unwrap());

    let error = state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .expect_err("mismatched ownership evidence must stop startup recovery");

    assert!(error.contains("mismatch_primary"));
    assert!(state.manager.base_path.join("mismatch_primary").exists());
    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Running);
}

async fn create_committed_async_job(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
    replicas: Vec<String>,
) -> uuid::Uuid {
    let spool = spool_for_state(state);
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(job_uuid, target_index)
        .unwrap();
    advance_to_preparing(&spool, job_uuid);
    let publication = PreStagedPublication::prepare(
        &state.manager.base_path,
        PublicationTarget::new(target_index).unwrap(),
    )
    .unwrap();
    spool
        .record_async_publication_transaction_if_present(
            job_uuid,
            publication.transaction_id().clone(),
        )
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    populate_staging_index(&publication, target_index, replicas).await;
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    publication.activate_create_only().unwrap();
    state.manager.unload(&target_index.to_string()).unwrap();
    job_uuid
}

async fn create_unjournaled_async_publication(
    state: &Arc<crate::handlers::AppState>,
    target_index: &str,
) -> (uuid::Uuid, std::path::PathBuf) {
    let spool = spool_for_state(state);
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(job_uuid, target_index)
        .unwrap();
    advance_to_preparing(&spool, job_uuid);
    let publication = PreStagedPublication::prepare(
        &state.manager.base_path,
        PublicationTarget::new(target_index).unwrap(),
    )
    .unwrap();
    let transaction_namespace = publication.paths().staging.parent().unwrap().to_path_buf();
    spool
        .record_async_publication_transaction_if_present(
            job_uuid,
            publication.transaction_id().clone(),
        )
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    populate_staging_index(&publication, target_index, Vec::new()).await;
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    (job_uuid, transaction_namespace)
}

async fn populate_staging_index(
    publication: &PreStagedPublication,
    target_index: &str,
    replicas: Vec<String>,
) {
    let staging_parent = publication.paths().staging.parent().unwrap();
    let staging_tenant = publication
        .paths()
        .staging
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    let manager = flapjack::IndexManager::new(staging_parent);
    manager.create_tenant(staging_tenant).unwrap();
    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        replicas: if replicas.is_empty() {
            None
        } else {
            Some(replicas)
        },
        ..Default::default()
    };
    settings
        .save(publication.paths().staging.join("settings.json"))
        .unwrap();
    manager
        .add_documents_durable(
            staging_tenant,
            vec![Document::from_json(&json!({
                "objectID": "recovery-doc",
                "title": format!("Recovery document for {target_index}"),
            }))
            .unwrap()],
        )
        .await
        .unwrap();
    manager.unload(&staging_tenant.to_string()).unwrap();
}

fn write_replica_sidecar(
    state: &Arc<crate::handlers::AppState>,
    replica_name: &str,
    primary_name: &str,
) {
    let replica_dir = state.manager.base_path.join(replica_name);
    std::fs::create_dir(&replica_dir).unwrap();
    let settings = IndexSettings {
        primary: Some(primary_name.to_string()),
        ..Default::default()
    };
    settings.save(replica_dir.join("settings.json")).unwrap();
}

fn admitted_async_job(
    spool: &SpoolStore,
    target_index: &str,
    phase: Option<MigrationPhase>,
) -> uuid::Uuid {
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(job_uuid, target_index)
        .unwrap();
    match phase {
        Some(MigrationPhase::Exporting) => {
            spool
                .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
                .unwrap();
        }
        Some(MigrationPhase::Preparing) => advance_to_preparing(spool, job_uuid),
        Some(MigrationPhase::Staging) => {
            advance_to_preparing(spool, job_uuid);
            spool
                .transition_migration_phase(job_uuid, MigrationPhase::Staging)
                .unwrap();
        }
        Some(MigrationPhase::Activating) => advance_to_activating(spool, job_uuid),
        Some(MigrationPhase::Submitted) | None => {}
    }
    job_uuid
}

fn advance_to_preparing(spool: &SpoolStore, job_uuid: uuid::Uuid) {
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
}

fn advance_to_activating(spool: &SpoolStore, job_uuid: uuid::Uuid) {
    advance_to_preparing(spool, job_uuid);
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
}

fn spool_for_state(state: &Arc<crate::handlers::AppState>) -> SpoolStore {
    SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap()
}
