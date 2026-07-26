use super::*;
use crate::handlers::migration::spool::{
    AsyncMigrationPublicationSemantic, MigrationImportOutcome, MigrationImportWarning,
};
use flapjack::index::manager::publication::{
    PreStagedPublication, PublicationPhase, PublicationTarget, PublicationTargetDisposition,
};
use flapjack::index::settings::IndexSettings;
use flapjack::types::Document;

type SharedAppState = Arc<crate::handlers::AppState>;
type ReplacementDocument = (&'static str, &'static str, &'static str, &'static str);
type ReplacementDocuments = [ReplacementDocument; 3];

struct InterruptedPrivacyScrub {
    tmp: TempDir,
    private_key: String,
    owner_identity: String,
    job_uuid: uuid::Uuid,
    durable_phase: MigrationPhaseRecord,
}

async fn interrupt_privacy_scrub_request(
    expected_generation: &str,
    boundary: PrivacyScrubBoundary,
    hooks: Arc<PrivacyScrubTestHooks>,
    expected_disposition: MigrationDisposition,
    expect_terminal: bool,
) -> InterruptedPrivacyScrub {
    let tmp = TempDir::new().unwrap();
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, expected_generation);
    let state = TestStateBuilder::new(&tmp).with_analytics().build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(tmp.path(), "admin-key"));
    let private_key = privacy_scrub_private_key(&key_store);
    let owner_identity = privacy_scrub_owner_identity(&private_key);
    seed_preexisting_target_resources(&state, PRIVACY_SCRUB_TARGET).await;
    write_current_generation_evidence(
        &state.manager.base_path,
        PRIVACY_SCRUB_TARGET,
        expected_generation,
    );
    let app = privacy_scrub_test_router_with_hooks(
        &tmp,
        Arc::clone(&state),
        key_store,
        Arc::clone(&hooks),
    );
    let request_app = app.clone();
    let request_key = private_key.clone();
    let mut request =
        tokio::spawn(async move { post_privacy_scrub(&request_app, &request_key, payload).await });

    observe_privacy_scrub_boundary(&mut request, &hooks, boundary).await;
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let jobs = spool.job_uuids().unwrap();
    assert_eq!(
        jobs.len(),
        1,
        "interrupted scrub must retain one durable job"
    );
    let durable_phase = spool.read_migration_phase(jobs[0]).unwrap();
    assert_eq!(durable_phase.disposition, expected_disposition);
    assert_eq!(
        durable_phase.terminal_at.is_some(),
        expect_terminal,
        "interrupted scrub fixture must preserve the expected terminality"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&spool, jobs[0]),
        &owner_identity,
        PRIVACY_SCRUB_ID,
        expected_generation,
    );
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
    request.abort();
    release_privacy_scrub_boundary(&hooks, boundary);
    let cancelled = tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, request)
        .await
        .expect("interrupted scrub request must settle after cancellation")
        .expect_err("interrupted scrub request must not deliver its ACK");
    assert!(cancelled.is_cancelled());
    drop(app);
    drop(state);

    InterruptedPrivacyScrub {
        tmp,
        private_key,
        owner_identity,
        job_uuid: jobs[0],
        durable_phase,
    }
}

fn repair_publications(
    state: &SharedAppState,
) -> Vec<flapjack::index::manager::publication::PublicationRepairReport> {
    state.manager.repair_publications_before_serve().unwrap()
}

async fn recover_jobs(
    state: &SharedAppState,
    reports: &[flapjack::index::manager::publication::PublicationRepairReport],
) {
    state
        .migration_runner
        .recover_async_jobs_before_serve(reports)
        .await
        .unwrap();
}

fn assert_terminal_phase(
    spool: &SpoolStore,
    job_uuid: uuid::Uuid,
    disposition: MigrationDisposition,
) -> MigrationPhaseRecord {
    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, disposition);
    assert!(phase.terminal_at.is_some());
    phase
}

#[tokio::test]
async fn privacy_scrub_recovery_replays_ack_after_response_loss_and_restart() {
    let hooks = Arc::new(PrivacyScrubTestHooks::default().with_boundaries([
        PrivacyScrubBoundary::ResponseLoss,
        PrivacyScrubBoundary::Restart,
        PrivacyScrubBoundary::AckReplay,
    ]));
    let interrupted = interrupt_privacy_scrub_request(
        "generation-response-loss",
        PrivacyScrubBoundary::ResponseLoss,
        Arc::clone(&hooks),
        MigrationDisposition::Succeeded,
        true,
    )
    .await;

    let state = TestStateBuilder::new(&interrupted.tmp)
        .with_analytics()
        .build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(
        interrupted.tmp.path(),
        "admin-key",
    ));
    let restart_state = Arc::clone(&state);
    let restart_hooks = Arc::clone(&hooks);
    let mut restart = tokio::spawn(async move {
        restart_hooks.wait_at(PrivacyScrubBoundary::Restart).await;
        let reports = repair_publications(&restart_state);
        restart_state
            .migration_runner
            .recover_async_jobs_before_serve(&reports)
            .await
    });
    observe_privacy_scrub_boundary(&mut restart, &hooks, PrivacyScrubBoundary::Restart).await;
    let restart_spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    assert_eq!(
        restart_spool
            .read_migration_phase(interrupted.job_uuid)
            .unwrap(),
        interrupted.durable_phase,
        "restart must expose the earned terminal outcome before recovery runs"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&restart_spool, interrupted.job_uuid),
        &interrupted.owner_identity,
        PRIVACY_SCRUB_ID,
        "generation-response-loss",
    );
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::Restart);
    tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, restart)
        .await
        .expect("restart recovery must finish after release")
        .expect("restart recovery task must not panic")
        .expect("restart recovery must reconcile the privacy scrub");

    let app = privacy_scrub_test_router_with_hooks(
        &interrupted.tmp,
        Arc::clone(&state),
        key_store,
        Arc::clone(&hooks),
    );
    let request_app = app.clone();
    let private_key = interrupted.private_key.clone();
    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-response-loss");
    let mut replay =
        tokio::spawn(async move { post_privacy_scrub(&request_app, &private_key, payload).await });
    observe_privacy_scrub_boundary(&mut replay, &hooks, PrivacyScrubBoundary::AckReplay).await;
    let replay_spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    assert_eq!(
        replay_spool
            .read_migration_phase(interrupted.job_uuid)
            .unwrap(),
        interrupted.durable_phase,
        "ACK replay must leave the durable terminal phase unchanged"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&replay_spool, interrupted.job_uuid),
        &interrupted.owner_identity,
        PRIVACY_SCRUB_ID,
        "generation-response-loss",
    );
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
    release_privacy_scrub_boundary(&hooks, PrivacyScrubBoundary::AckReplay);
    let replay = tokio::time::timeout(PRIVACY_SCRUB_BOUNDARY_TIMEOUT, replay)
        .await
        .expect("ACK replay must finish after release")
        .expect("ACK replay request task must not panic");
    assert_eq!(
        replay.status(),
        StatusCode::ACCEPTED,
        "restart recovery must preserve the earned ACK for the stable scrub ID"
    );
    let replay_ack = body_json(replay).await;
    assert_eq!(replay_ack["scrubId"], PRIVACY_SCRUB_ID);
    assert_eq!(replay_ack["disposition"], "acknowledged");
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
}

#[tokio::test]
async fn privacy_scrub_restart_replays_ack_after_post_commit_crash() {
    let hooks = Arc::new(
        PrivacyScrubTestHooks::default().with_boundaries([PrivacyScrubBoundary::EngineCommit]),
    );
    let interrupted = interrupt_privacy_scrub_request(
        "generation-post-commit-crash",
        PrivacyScrubBoundary::EngineCommit,
        Arc::clone(&hooks),
        MigrationDisposition::Running,
        false,
    )
    .await;

    let state = TestStateBuilder::new(&interrupted.tmp)
        .with_analytics()
        .build_shared();
    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;
    let key_store = Arc::new(KeyStore::load_or_create(
        interrupted.tmp.path(),
        "admin-key",
    ));
    let app = privacy_scrub_test_router_with_hooks(
        &interrupted.tmp,
        Arc::clone(&state),
        key_store,
        Arc::clone(&hooks),
    );

    let payload = privacy_scrub_payload(PRIVACY_SCRUB_ID, "generation-post-commit-crash");
    let replay = post_privacy_scrub(&app, &interrupted.private_key, payload).await;
    assert_eq!(
        replay.status(),
        StatusCode::ACCEPTED,
        "a restarted delivery must finish the durable transport once exact absence is already true"
    );
    let replay_ack = body_json(replay).await;
    assert_eq!(replay_ack["scrubId"], PRIVACY_SCRUB_ID);
    assert_eq!(replay_ack["disposition"], "acknowledged");
    let replay_spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let durable_phase = replay_spool
        .read_migration_phase(interrupted.job_uuid)
        .unwrap();
    assert_eq!(durable_phase.disposition, MigrationDisposition::Succeeded);
    assert!(
        durable_phase.terminal_at.is_some(),
        "replayed post-commit crash must settle the durable terminal phase"
    );
    assert_privacy_scrub_intent(
        &read_privacy_scrub_intent(&replay_spool, interrupted.job_uuid),
        &interrupted.owner_identity,
        PRIVACY_SCRUB_ID,
        "generation-post-commit-crash",
    );
    assert_preexisting_target_resources_exactly_absent(&state, PRIVACY_SCRUB_TARGET).await;
}

#[tokio::test]
async fn async_recovery_leaves_terminal_jobs_untouched() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let failed = uuid::Uuid::new_v4();
    let succeeded = uuid::Uuid::new_v4();

    spool
        .create_async_migration_admission(
            failed,
            "terminal_failed",
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    spool.fail_migration(failed).unwrap();
    spool
        .create_async_migration_admission(
            succeeded,
            "terminal_succeeded",
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
        .unwrap();
    advance_to_activating(&spool, succeeded);
    spool.succeed_migration(succeeded, None).unwrap();
    let failed_before = spool.read_migration_phase(failed).unwrap();
    let succeeded_before = spool.read_migration_phase(succeeded).unwrap();

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

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

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

    for job_uuid in jobs {
        let phase = assert_terminal_phase(&spool, job_uuid, MigrationDisposition::Failed);
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

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

    assert_terminal_phase(&spool, job_uuid, MigrationDisposition::Failed);
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

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

    assert_terminal_phase(&spool, job_uuid, MigrationDisposition::Cancelled);
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

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

    assert_terminal_phase(&spool, job_uuid, MigrationDisposition::Cancelled);
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
async fn async_recovery_fails_uncommitted_replacement_without_deleting_existing_target() {
    assert_uncommitted_replacement_recovery(
        false,
        MigrationDisposition::Failed,
        "replacement recovery must abort only the matching uncommitted staging transaction",
        "replacement recovery must not classify the committed target as disposable create-owned state",
    )
    .await;
}

#[tokio::test]
async fn async_recovery_cancels_uncommitted_replacement_without_deleting_existing_target() {
    assert_uncommitted_replacement_recovery(
        true,
        MigrationDisposition::Cancelled,
        "replacement cancel recovery must abort only the matching uncommitted staging transaction",
        "replacement cancel recovery must preserve the committed destination byte-for-byte",
    )
    .await;
}

#[tokio::test]
async fn async_recovery_settles_committed_replacement_failed_without_doubled_union() {
    assert_committed_replacement_recovery(
        false,
        MigrationDisposition::Failed,
        "existing replacement recovery contract settles committed, non-cancelled jobs as Failed",
        "committed replacement recovery must not rewrite the committed target",
    )
    .await;
}

#[tokio::test]
async fn async_recovery_treats_cancel_requested_committed_publication_as_succeeded() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let spool = spool_for_state(&state);
    let job_uuid = create_committed_async_job(&state, "cancel_committed_primary", Vec::new()).await;
    spool.request_migration_cancel(job_uuid).unwrap();
    let before = directory_snapshot(&state.manager.base_path.join("cancel_committed_primary"));

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

    let phase = assert_terminal_phase(&spool, job_uuid, MigrationDisposition::Succeeded);
    assert_eq!(phase.import_outcome, Some(recovery_import_outcome()));
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
async fn async_recovery_treats_cancel_requested_committed_replacement_as_succeeded() {
    assert_committed_replacement_recovery(
        true,
        MigrationDisposition::Succeeded,
        "committed publication evidence must win over a late replacement cancel request",
        "post-commit replacement cancel recovery must preserve the committed target",
    )
    .await;
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

    let reports = repair_publications(&state);
    assert!(reports.iter().any(|report| {
        report.target.as_str() == "recovery_primary"
            && report.disposition == PublicationTargetDisposition::Loadable
    }));
    recover_jobs(&state, &reports).await;

    assert_terminal_phase(&spool, job_uuid, MigrationDisposition::Failed);
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
    create_committed_target_publication(
        &state,
        "mismatch_primary",
        ASYNC_REPLACE_INITIAL_DOCUMENTS,
    )
    .await;
    let reports = repair_publications(&state);
    assert_committed_replacement_report(&reports, "mismatch_primary");
    let spool = spool_for_state(&state);
    let (job_uuid, transaction_namespace) = create_unjournaled_async_publication_with_semantic(
        &state,
        "mismatch_primary",
        AsyncMigrationPublicationSemantic::ReplaceExisting,
    )
    .await;
    let before = directory_snapshot(&state.manager.base_path.join("mismatch_primary"));

    let error = state
        .migration_runner
        .recover_async_jobs_before_serve(&reports)
        .await
        .expect_err("mismatched ownership evidence must stop startup recovery");

    assert!(error.contains("mismatch_primary"));
    assert!(error.contains("publication transaction mismatch"));
    assert!(state.manager.base_path.join("mismatch_primary").exists());
    let phase = spool.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert!(phase.terminal_at.is_none());
    assert!(
        transaction_namespace.exists(),
        "mismatched replacement recovery must not delete the job-owned staging transaction"
    );
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join("mismatch_primary")),
        before,
        "mismatched replacement recovery must leave the committed target unchanged"
    );
    assert_async_replacement_exact_state(
        &state,
        "mismatch_primary",
        ASYNC_REPLACE_INITIAL_DOCUMENTS,
    )
    .await;
}

async fn assert_uncommitted_replacement_recovery(
    cancel_requested: bool,
    expected_disposition: MigrationDisposition,
    transaction_message: &str,
    snapshot_message: &str,
) {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    create_committed_target_publication(&state, TARGET_INDEX, ASYNC_REPLACE_INITIAL_DOCUMENTS)
        .await;
    let before = directory_snapshot(&state.manager.base_path.join(TARGET_INDEX));
    let (job_uuid, transaction_namespace) = create_unjournaled_async_publication_with_semantic(
        &state,
        TARGET_INDEX,
        AsyncMigrationPublicationSemantic::ReplaceExisting,
    )
    .await;
    let spool = spool_for_state(&state);
    if cancel_requested {
        spool.request_migration_cancel(job_uuid).unwrap();
    }

    let reports = repair_publications(&state);
    recover_jobs(&state, &reports).await;

    assert_terminal_phase(&spool, job_uuid, expected_disposition);
    assert!(!transaction_namespace.exists(), "{transaction_message}");
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join(TARGET_INDEX)),
        before,
        "{snapshot_message}"
    );
    assert_async_replacement_exact_state(&state, TARGET_INDEX, ASYNC_REPLACE_INITIAL_DOCUMENTS)
        .await;
}

async fn assert_committed_replacement_recovery(
    cancel_requested: bool,
    expected_disposition: MigrationDisposition,
    disposition_message: &str,
    snapshot_message: &str,
) {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    seed_async_replacement_target(&state, TARGET_INDEX, ASYNC_REPLACE_INITIAL_DOCUMENTS).await;
    let spool = spool_for_state(&state);
    let job_uuid = create_committed_async_replacement_job(&state, TARGET_INDEX).await;
    if cancel_requested {
        spool.request_migration_cancel(job_uuid).unwrap();
    }
    let before = directory_snapshot(&state.manager.base_path.join(TARGET_INDEX));
    let reports = repair_publications(&state);
    assert_committed_replacement_report(&reports, TARGET_INDEX);
    recover_jobs(&state, &reports).await;

    let phase = assert_terminal_phase(&spool, job_uuid, expected_disposition);
    assert_eq!(
        phase.cancel_requested, cancel_requested,
        "{disposition_message}"
    );
    assert_eq!(
        phase.import_outcome,
        if expected_disposition == MigrationDisposition::Succeeded {
            Some(recovery_import_outcome())
        } else {
            None
        }
    );
    assert_eq!(
        directory_snapshot(&state.manager.base_path.join(TARGET_INDEX)),
        before,
        "{snapshot_message}"
    );
    assert_async_replacement_exact_state(&state, TARGET_INDEX, ASYNC_REPLACE_FINAL_DOCUMENTS).await;
}

async fn seed_async_replacement_target(
    state: &SharedAppState,
    target_index: &str,
    documents: ReplacementDocuments,
) {
    state.manager.create_tenant(target_index).unwrap();
    write_async_replacement_settings(&state.manager.base_path.join(target_index));
    state
        .manager
        .add_documents_durable(target_index, async_replacement_documents(documents))
        .await
        .unwrap();
    state.manager.unload(&target_index.to_string()).unwrap();
}

async fn assert_async_replacement_exact_state(
    state: &SharedAppState,
    target_index: &str,
    documents: ReplacementDocuments,
) {
    let hits = sorted_exact_hits_by_object_id(
        state,
        target_index,
        10,
        "async replacement recovery target should be queryable",
        |hit| {
            (
                hit["objectID"]
                    .as_str()
                    .expect("hit should contain objectID")
                    .to_string(),
                hit["title"]
                    .as_str()
                    .expect("hit should contain title")
                    .to_string(),
                hit["category"]
                    .as_str()
                    .expect("hit should contain category")
                    .to_string(),
                hit["revision"]
                    .as_str()
                    .expect("hit should contain revision")
                    .to_string(),
            )
        },
    )
    .await;
    assert_eq!(
        hits,
        documents
            .into_iter()
            .map(|(object_id, title, category, revision)| (
                object_id.to_string(),
                title.to_string(),
                category.to_string(),
                revision.to_string(),
            ))
            .collect::<Vec<_>>()
    );
}

fn async_replacement_documents(documents: ReplacementDocuments) -> Vec<Document> {
    documents
        .into_iter()
        .map(|(object_id, title, category, revision)| {
            Document::from_json(&json!({
                "objectID": object_id,
                "title": title,
                "category": category,
                "revision": revision,
            }))
            .unwrap()
        })
        .collect()
}

fn write_async_replacement_settings(index_path: &std::path::Path) {
    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        attributes_for_faceting: vec!["category".to_string()],
        ..Default::default()
    };
    settings.save(index_path.join("settings.json")).unwrap();
}

fn assert_committed_replacement_report(
    reports: &[flapjack::index::manager::publication::PublicationRepairReport],
    target_index: &str,
) {
    assert!(reports.iter().any(|report| {
        report.target.as_str() == target_index
            && report.disposition == PublicationTargetDisposition::Loadable
            && report.phase == Some(PublicationPhase::Committed)
            && report.transaction_id.is_some()
    }));
}

async fn create_committed_target_publication(
    state: &SharedAppState,
    target_index: &str,
    documents: ReplacementDocuments,
) {
    let publication = PreStagedPublication::prepare(
        &state.manager.base_path,
        PublicationTarget::new(target_index).unwrap(),
    )
    .unwrap();
    populate_staging_index_with_documents(
        &publication,
        Vec::new(),
        async_replacement_documents(documents),
    )
    .await;
    publication.activate_create_only().unwrap();
    state.manager.unload(&target_index.to_string()).unwrap();
}

async fn create_committed_async_replacement_job(
    state: &SharedAppState,
    target_index: &str,
) -> uuid::Uuid {
    let spool = spool_for_state(state);
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(
            job_uuid,
            target_index,
            AsyncMigrationPublicationSemantic::ReplaceExisting,
        )
        .unwrap();
    advance_to_preparing(&spool, job_uuid);
    let staging_baseline = state
        .manager
        .capture_replacement_staging_baseline(target_index)
        .unwrap();
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
    populate_replacement_staging_index(&publication).await;
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Activating)
        .unwrap();
    state
        .manager
        .replace_index_contents_from_pre_staged(publication, target_index, staging_baseline)
        .await
        .unwrap();
    spool
        .record_import_outcome(job_uuid, recovery_import_outcome())
        .unwrap();
    state.manager.unload(&target_index.to_string()).unwrap();
    job_uuid
}

async fn create_committed_async_job(
    state: &SharedAppState,
    target_index: &str,
    replicas: Vec<String>,
) -> uuid::Uuid {
    let spool = spool_for_state(state);
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(
            job_uuid,
            target_index,
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
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
    spool
        .record_import_outcome(job_uuid, recovery_import_outcome())
        .unwrap();
    state.manager.unload(&target_index.to_string()).unwrap();
    job_uuid
}

fn recovery_import_outcome() -> MigrationImportOutcome {
    MigrationImportOutcome {
        settings_applied: true,
        synonyms_imported: 1,
        rules_imported: 0,
        warnings: vec![MigrationImportWarning {
            code: "PersistedNoBehaviorSetting".to_string(),
            message: "Source setting is preserved for compatibility but has no Flapjack behavior."
                .to_string(),
            resource: "Settings".to_string(),
            page_index: None,
            item_index: None,
            json_path: "$.hitsPerPage".to_string(),
        }],
    }
}

async fn create_unjournaled_async_publication(
    state: &SharedAppState,
    target_index: &str,
) -> (uuid::Uuid, std::path::PathBuf) {
    create_unjournaled_async_publication_with_semantic(
        state,
        target_index,
        AsyncMigrationPublicationSemantic::CreateOnly,
    )
    .await
}

async fn create_unjournaled_async_publication_with_semantic(
    state: &SharedAppState,
    target_index: &str,
    publication_semantic: AsyncMigrationPublicationSemantic,
) -> (uuid::Uuid, std::path::PathBuf) {
    let spool = spool_for_state(state);
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(job_uuid, target_index, publication_semantic)
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
    match publication_semantic {
        AsyncMigrationPublicationSemantic::CreateOnly => {
            populate_staging_index(&publication, target_index, Vec::new()).await;
        }
        AsyncMigrationPublicationSemantic::ReplaceExisting => {
            populate_replacement_staging_index(&publication).await;
        }
    }
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
    populate_staging_index_with_documents(
        publication,
        replicas,
        vec![Document::from_json(&json!({
            "objectID": "recovery-doc",
            "title": format!("Recovery document for {target_index}"),
        }))
        .unwrap()],
    )
    .await;
}

async fn populate_replacement_staging_index(publication: &PreStagedPublication) {
    populate_staging_index_with_documents(
        publication,
        Vec::new(),
        async_replacement_documents(ASYNC_REPLACE_FINAL_DOCUMENTS),
    )
    .await;
}

async fn populate_staging_index_with_documents(
    publication: &PreStagedPublication,
    replicas: Vec<String>,
    documents: Vec<Document>,
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
        .add_documents_durable(staging_tenant, documents)
        .await
        .unwrap();
    manager.unload(&staging_tenant.to_string()).unwrap();
}

fn write_replica_sidecar(state: &SharedAppState, replica_name: &str, primary_name: &str) {
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
        .create_async_migration_admission(
            job_uuid,
            target_index,
            AsyncMigrationPublicationSemantic::CreateOnly,
        )
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
