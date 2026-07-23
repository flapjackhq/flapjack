use super::*;
use chrono::{TimeZone, Utc};
use flapjack::index::manager::publication::{
    PublicationPaths, PublicationTarget, PublicationTransactionId,
};
use flapjack::index::manager::IndexManager;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::{Arc, Barrier, Mutex};
use tempfile::TempDir;
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::{
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
    Layer,
};

type ArtifactCorruption = Box<dyn Fn(&SpoolStore, uuid::Uuid)>;

const GC_EXPECTED_RECLAIMED_BYTES_PER_JOB: u64 = 106;

#[derive(Clone, Debug, PartialEq, Eq)]
struct CapturedSpoolWarning {
    job_uuid: uuid::Uuid,
    error_kind: String,
}

#[derive(Clone, Default)]
struct CapturedSpoolWarnings {
    events: Arc<Mutex<Vec<CapturedSpoolWarning>>>,
}

impl CapturedSpoolWarnings {
    fn events(&self) -> Vec<CapturedSpoolWarning> {
        self.events.lock().unwrap().clone()
    }
}

impl<S> Layer<S> for CapturedSpoolWarnings
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if *event.metadata().level() != Level::WARN {
            return;
        }
        let mut visitor = SpoolWarningVisitor::default();
        event.record(&mut visitor);
        if let (Some(job_uuid), Some(error_kind)) = (visitor.job_uuid, visitor.error_kind) {
            self.events.lock().unwrap().push(CapturedSpoolWarning {
                job_uuid,
                error_kind,
            });
        }
    }
}

#[derive(Default)]
struct SpoolWarningVisitor {
    job_uuid: Option<uuid::Uuid>,
    error_kind: Option<String>,
}

impl tracing::field::Visit for SpoolWarningVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.record_value(field.name(), value);
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        let rendered = format!("{value:?}");
        self.record_value(field.name(), rendered.trim_matches('"'));
    }
}

impl SpoolWarningVisitor {
    fn record_value(&mut self, name: &str, value: &str) {
        match name {
            "job_uuid" => self.job_uuid = uuid::Uuid::parse_str(value).ok(),
            "error_kind" => self.error_kind = Some(value.to_string()),
            _ => {}
        }
    }
}

fn spool_error_kind_name(kind: SpoolErrorKind) -> String {
    format!("{kind:?}")
}

fn test_limits() -> SpoolLimits {
    SpoolLimits {
        max_compressed_page_bytes: 64,
        max_decompressed_page_bytes: 128,
        max_items_per_resource: 10,
        max_bytes_per_job: 512,
        max_global_bytes: 1024,
        minimum_free_bytes: 32,
        max_staged_artifacts: 2,
        max_staged_artifact_bytes: 128,
        retention_seconds: 60,
    }
}

fn fixed_now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap()
}

fn fixed_store(tmp: &TempDir) -> SpoolStore {
    SpoolStore::new_for_tests(tmp.path(), test_limits(), fixed_now(), 10_000)
        .expect("test store should initialize")
}

fn default_limit_export() -> (TempDir, SpoolStore, SpoolManifest) {
    let tmp = TempDir::new().unwrap();
    let limits = SpoolLimits::default();
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        fixed_now(),
        limits.minimum_free_bytes + 1,
    )
    .expect("default-limit test store should initialize");
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .expect("default-limit export should be created");
    let manifest = store
        .read_manifest(view.job_uuid)
        .expect("default-limit manifest should be readable");
    (tmp, store, manifest)
}

fn validate_default_limit_items(
    items: u64,
    mutate_manifest: impl FnOnce(&mut SpoolManifest),
) -> SpoolResult<()> {
    let (_tmp, store, mut manifest) = default_limit_export();
    mutate_manifest(&mut manifest);
    store.validate_artifact_limits(&manifest, ArtifactKind::RulesPage, 0, 0, items)
}

fn denominators() -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: 3,
        rules: 2,
        synonyms: 1,
        config: 1,
    }
}

fn accepted_reader_denominators() -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: 2,
        rules: 1,
        synonyms: 1,
        config: 0,
    }
}

fn source_digest() -> String {
    hex_digest(b"source-identity")
}

fn fixed_job_uuid() -> uuid::Uuid {
    uuid::Uuid::from_u128(0x12345678123456781234567812345678)
}

fn create_export_for_test(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    source_identity_digest: &str,
    denominators: ResourceDenominators,
) -> SpoolResult<PublicExportView> {
    store.create_migration_phase(job_uuid)?;
    store.create_export(job_uuid, source_identity_digest, denominators)
}

fn object_payload(ids: &[String]) -> Vec<u8> {
    serde_json::to_vec(
        &ids.iter()
            .map(|id| json!({ "objectID": id }))
            .collect::<Vec<_>>(),
    )
    .unwrap()
}

fn fixed_width_document_pages(page_count: usize, page_size: usize) -> Vec<Vec<String>> {
    (0..page_count)
        .map(|page| {
            (0..page_size)
                .map(|offset| format!("doc-{:08}", page * page_size + offset))
                .collect()
        })
        .collect()
}

#[test]
fn completed_id_checkpoints_write_only_delta() {
    let tmp = TempDir::new().unwrap();
    let limits = SpoolLimits {
        max_compressed_page_bytes: 512,
        max_decompressed_page_bytes: 512,
        max_items_per_resource: 12,
        max_bytes_per_job: 4096,
        max_global_bytes: 4096,
        minimum_free_bytes: 32,
        max_staged_artifacts: 4,
        max_staged_artifact_bytes: 512,
        retention_seconds: 60,
    };
    let store = SpoolStore::new_for_tests(tmp.path(), limits, fixed_now(), 10_000).unwrap();
    let view = create_export_for_test(
        &store,
        fixed_job_uuid(),
        &source_digest(),
        ResourceDenominators {
            settings: 0,
            documents: 12,
            rules: 0,
            synonyms: 0,
            config: 0,
        },
    )
    .unwrap();
    let pages = fixed_width_document_pages(3, 4);
    let expected_delta_id_count = pages[0].len();
    let expected_delta_bytes = pages[0].iter().map(|id| id.len() + 1).sum::<usize>();

    reset_completed_id_checkpoint_writes_for_tests();
    for page in &pages {
        let object_ids = page.iter().map(String::as_str).collect::<Vec<_>>();
        let payload = object_payload(page);
        store
            .commit_document_page_with_ids(view.job_uuid, &payload, &object_ids)
            .unwrap();
    }

    let writes = completed_id_checkpoint_writes_for_tests();
    let observed_counts = writes
        .iter()
        .map(|write| write.serialized_id_count)
        .collect::<Vec<_>>();
    let observed_bytes = writes
        .iter()
        .map(|write| write.byte_len)
        .collect::<Vec<_>>();
    let observed_read_bytes = writes
        .iter()
        .map(|write| write.sidecar_read_bytes)
        .collect::<Vec<_>>();
    let observed_digest_bytes = writes
        .iter()
        .map(|write| write.digest_input_bytes)
        .collect::<Vec<_>>();
    let observed_counted_ids = writes
        .iter()
        .map(|write| write.counted_id_count)
        .collect::<Vec<_>>();

    assert_eq!(
        observed_counts,
        vec![expected_delta_id_count; pages.len()],
        "completed-ID checkpoints must serialize only the new page of IDs; observed counts: {observed_counts:?}, observed bytes: {observed_bytes:?}, expected max bytes per checkpoint: {expected_delta_bytes}"
    );
    assert!(
        observed_bytes
            .iter()
            .all(|byte_len| *byte_len <= expected_delta_bytes),
        "completed-ID checkpoints must write at most one page of fixed-width ID lines; observed bytes: {observed_bytes:?}, expected max: {expected_delta_bytes}"
    );
    assert!(
        observed_read_bytes
            .iter()
            .all(|byte_len| *byte_len <= expected_delta_bytes),
        "completed-ID checkpoints must not read the full prior sidecar; observed read bytes: {observed_read_bytes:?}, expected max: {expected_delta_bytes}"
    );
    assert!(
        observed_digest_bytes
            .iter()
            .all(|byte_len| *byte_len <= expected_delta_bytes),
        "completed-ID checkpoints must not hash the full sidecar; observed hash bytes: {observed_digest_bytes:?}, expected max: {expected_delta_bytes}"
    );
    assert_eq!(
        observed_counted_ids,
        vec![expected_delta_id_count; pages.len()],
        "completed-ID checkpoints must count only the new page; observed counted IDs: {observed_counted_ids:?}"
    );

    let retained_sidecar = std::fs::read_to_string(store.completed_sidecar_path(view.job_uuid))
        .expect("completed-ID sidecar should be retained for resume");
    let retained_ids = retained_sidecar.lines().collect::<Vec<_>>();
    let expected_all_ids = pages
        .iter()
        .flatten()
        .map(String::as_str)
        .collect::<Vec<_>>();
    assert_eq!(retained_ids, expected_all_ids);
}

#[test]
fn completed_id_sidecar_corruption_after_committed_prefix_fails_closed() {
    for resource in [
        ObjectResource::Documents,
        ObjectResource::Rules,
        ObjectResource::Synonyms,
    ] {
        let tmp = TempDir::new().unwrap();
        let mut limits = test_limits();
        limits.max_staged_artifacts = 8;
        let store = SpoolStore::new_for_tests(tmp.path(), limits, fixed_now(), 10_000).unwrap();
        let view = create_export_for_test(
            &store,
            uuid::Uuid::new_v4(),
            &source_digest(),
            ResourceDenominators {
                settings: 0,
                documents: 4,
                rules: 4,
                synonyms: 4,
                config: 0,
            },
        )
        .unwrap();
        let first_page = vec![format!("{resource:?}-id-01"), format!("{resource:?}-id-02")];
        let second_page = vec![format!("{resource:?}-id-03"), format!("{resource:?}-id-04")];
        commit_resource_page_with_ids(&store, view.job_uuid, resource, &first_page).unwrap();
        commit_resource_page_with_ids(&store, view.job_uuid, resource, &second_page).unwrap();

        let sidecar_path = store.resource_sidecar_path(view.job_uuid, resource);
        let mut sidecar = std::fs::read(&sidecar_path).unwrap();
        let prior_prefix_len = first_page.iter().map(|id| id.len() + 1).sum::<usize>();
        assert!(prior_prefix_len < sidecar.len());
        let corrupt_offset = prior_prefix_len + 1;
        sidecar[corrupt_offset] = b'X';
        std::fs::write(&sidecar_path, sidecar).unwrap();

        let reopened = SpoolStore::new_for_tests(tmp.path(), limits, fixed_now(), 10_000).unwrap();
        reopened.recover().unwrap();
        assert_eq!(
            completed_ids_for_resource(&reopened, view.job_uuid, resource)
                .unwrap_err()
                .kind(),
            SpoolErrorKind::ManifestCorrupt
        );
    }
}

fn commit_resource_page_with_ids(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    resource: ObjectResource,
    ids: &[String],
) -> SpoolResult<()> {
    let object_ids = ids.iter().map(String::as_str).collect::<Vec<_>>();
    let payload = object_payload(ids);
    match resource {
        ObjectResource::Documents => {
            store.commit_document_page_with_ids(job_uuid, &payload, &object_ids)
        }
        ObjectResource::Rules => store.commit_rule_page_with_ids(job_uuid, &payload, &object_ids),
        ObjectResource::Synonyms => {
            store.commit_synonym_page_with_ids(job_uuid, &payload, &object_ids)
        }
    }
}

fn completed_ids_for_resource(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    resource: ObjectResource,
) -> SpoolResult<Vec<String>> {
    match resource {
        ObjectResource::Documents => store.completed_document_ids(job_uuid),
        ObjectResource::Rules => store.completed_rule_ids(job_uuid),
        ObjectResource::Synonyms => store.completed_synonym_ids(job_uuid),
    }
}

fn accepted_store_with_artifacts() -> (TempDir, SpoolStore, uuid::Uuid) {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        accepted_reader_denominators(),
    )
    .expect("job should be created");
    store
        .commit_settings_once(view.job_uuid, br#"{"ranking":["typo"]}"#, &source_digest())
        .unwrap();
    store
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"doc-1"},{"objectID":"doc-2"}]"#,
            &["doc-1", "doc-2"],
        )
        .unwrap();
    store
        .commit_rule_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"rule-1","condition":{"pattern":"sale"}}]"#,
            &["rule-1"],
        )
        .unwrap();
    store
        .commit_synonym_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"syn-1","synonyms":["tee","shirt"]}]"#,
            &["syn-1"],
        )
        .unwrap();
    store
        .complete_documents(view.job_uuid, 2, &source_digest())
        .unwrap();
    store
        .complete_rules(view.job_uuid, 1, &source_digest())
        .unwrap();
    store
        .complete_synonyms(view.job_uuid, 1, &source_digest())
        .unwrap();
    store.accept_export(view.job_uuid).unwrap();
    (tmp, store, view.job_uuid)
}

#[test]
fn migration_phase_record_uses_caller_uuid_and_survives_reopen() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);

    let view = create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();

    assert_eq!(view.job_uuid, job_uuid);
    assert!(store.job_dir(job_uuid).exists());
    let manifest = store.read_manifest(job_uuid).unwrap();
    assert_eq!(manifest.job_uuid, job_uuid);

    let reopened = fixed_store(&tmp);
    let phase = reopened.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.job_uuid, job_uuid);
    assert_eq!(phase.phase, MigrationPhase::Exporting);
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert!(!phase.cancel_requested);
    assert_eq!(phase.created_at, fixed_now());
    assert_eq!(phase.updated_at, fixed_now());
    assert_eq!(
        phase.export_progress,
        Some(MigrationExportProgress {
            completed: 0,
            total: 8
        })
    );

    let cancelled = reopened.request_migration_cancel(job_uuid).unwrap();
    assert!(cancelled.cancel_requested);
    assert_eq!(cancelled.disposition, MigrationDisposition::Running);

    let reopened_after_cancel = fixed_store(&tmp);
    let phase = reopened_after_cancel
        .read_migration_phase(job_uuid)
        .unwrap();
    assert!(phase.cancel_requested);
}

#[test]
fn migration_phase_record_defaults_missing_cancel_requested_to_false() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);
    create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();

    let legacy_phase = json!({
        "job_uuid": job_uuid,
        "phase": "Exporting",
        "disposition": "Running",
        "export_progress": {
            "completed": 0,
            "total": 8
        },
        "created_at": fixed_now(),
        "updated_at": fixed_now(),
        "terminal_at": null
    });
    std::fs::write(
        store.migration_phase_path(job_uuid),
        serde_json::to_vec_pretty(&legacy_phase).unwrap(),
    )
    .unwrap();

    let reopened = fixed_store(&tmp);
    let phase = reopened.read_migration_phase(job_uuid).unwrap();
    assert!(!phase.cancel_requested);
}

#[test]
fn request_async_migration_cancel_is_idempotent_for_running_jobs() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let job_uuid = fixed_job_uuid();
    store
        .create_async_migration_admission(job_uuid, "cancel_idempotent")
        .unwrap();

    let first = requested_cancel_record(store.request_async_migration_cancel(job_uuid).unwrap());
    let second = requested_cancel_record(store.request_async_migration_cancel(job_uuid).unwrap());

    assert_eq!(second, first);
    assert!(second.cancel_requested);
    assert_eq!(second.disposition, MigrationDisposition::Running);
}

#[test]
fn request_async_migration_cancel_is_noop_for_terminal_jobs() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let cancelled = uuid::Uuid::new_v4();
    let failed = uuid::Uuid::new_v4();
    let succeeded = uuid::Uuid::new_v4();

    store
        .create_async_migration_admission(cancelled, "terminal_cancelled")
        .unwrap();
    let cancelled_before = store.cancel_migration(cancelled).unwrap();
    store
        .create_async_migration_admission(failed, "terminal_failed")
        .unwrap();
    let failed_before = store.fail_migration(failed).unwrap();
    store
        .create_async_migration_admission(succeeded, "terminal_succeeded")
        .unwrap();
    for phase in [
        MigrationPhase::Exporting,
        MigrationPhase::Preparing,
        MigrationPhase::Staging,
        MigrationPhase::Activating,
    ] {
        store.transition_migration_phase(succeeded, phase).unwrap();
    }
    let succeeded_before = store.succeed_migration(succeeded).unwrap();

    for (job_uuid, expected) in [
        (cancelled, cancelled_before),
        (failed, failed_before),
        (succeeded, succeeded_before),
    ] {
        let returned =
            requested_cancel_record(store.request_async_migration_cancel(job_uuid).unwrap());

        assert_eq!(returned, expected);
        assert_eq!(store.read_migration_phase(job_uuid).unwrap(), expected);
    }
}

#[test]
fn request_async_migration_cancel_detects_post_commit_boundary() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let job_uuid = fixed_job_uuid();
    let target_index = "post_commit_target";
    let transaction_id = PublicationTransactionId::new("post_commit_tx").unwrap();
    store
        .create_async_migration_admission(job_uuid, target_index)
        .unwrap();
    for phase in [
        MigrationPhase::Exporting,
        MigrationPhase::Preparing,
        MigrationPhase::Staging,
        MigrationPhase::Activating,
    ] {
        store.transition_migration_phase(job_uuid, phase).unwrap();
    }
    store
        .record_async_publication_transaction_if_present(job_uuid, transaction_id.clone())
        .unwrap();
    let paths = PublicationPaths::new(
        tmp.path(),
        &PublicationTarget::new(target_index.to_string()).unwrap(),
        &transaction_id,
    );
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(&paths.journal, "{}").unwrap();

    let record = match store.request_async_migration_cancel(job_uuid).unwrap() {
        MigrationCancelRequest::TooLate(record) => record,
        MigrationCancelRequest::Requested(record) => {
            panic!("journaled publication evidence should be too late: {record:?}")
        }
    };

    assert_eq!(record.phase, MigrationPhase::Activating);
    assert!(!record.cancel_requested);
    assert!(
        !store
            .read_migration_phase(job_uuid)
            .unwrap()
            .cancel_requested
    );
}

#[test]
fn request_async_migration_cancel_allows_preexisting_target_before_journal() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let job_uuid = fixed_job_uuid();
    let target_index = "preexisting_target";
    let transaction_id = PublicationTransactionId::new("prepared_tx").unwrap();
    store
        .create_async_migration_admission(job_uuid, target_index)
        .unwrap();
    for phase in [
        MigrationPhase::Exporting,
        MigrationPhase::Preparing,
        MigrationPhase::Staging,
        MigrationPhase::Activating,
    ] {
        store.transition_migration_phase(job_uuid, phase).unwrap();
    }
    store
        .record_async_publication_transaction_if_present(job_uuid, transaction_id.clone())
        .unwrap();
    let paths = PublicationPaths::new(
        tmp.path(),
        &PublicationTarget::new(target_index.to_string()).unwrap(),
        &transaction_id,
    );
    std::fs::create_dir_all(&paths.target).unwrap();

    let record = requested_cancel_record(store.request_async_migration_cancel(job_uuid).unwrap());

    assert_eq!(record.phase, MigrationPhase::Activating);
    assert!(record.cancel_requested);
    assert!(
        store
            .read_migration_phase(job_uuid)
            .unwrap()
            .cancel_requested
    );
}

fn requested_cancel_record(decision: MigrationCancelRequest) -> MigrationPhaseRecord {
    match decision {
        MigrationCancelRequest::Requested(record) => record,
        MigrationCancelRequest::TooLate(record) => {
            panic!("cancel request should not be too late: {record:?}")
        }
    }
}

#[test]
fn migration_phase_progress_is_labeled_export_progress_not_completion() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);

    let view = create_export_for_test(
        &store,
        job_uuid,
        &source_digest(),
        accepted_reader_denominators(),
    )
    .unwrap();
    store
        .commit_settings_once(view.job_uuid, br#"{"ranking":["typo"]}"#, &source_digest())
        .unwrap();
    store
        .commit_document_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"doc-1"},{"objectID":"doc-2"}]"#,
            &["doc-1", "doc-2"],
        )
        .unwrap();
    store
        .commit_rule_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"rule-1","condition":{"pattern":"sale"}}]"#,
            &["rule-1"],
        )
        .unwrap();
    store
        .commit_synonym_page_with_ids(
            view.job_uuid,
            br#"[{"objectID":"syn-1","synonyms":["tee"]}]"#,
            &["syn-1"],
        )
        .unwrap();
    store
        .complete_documents(view.job_uuid, 2, &source_digest())
        .unwrap();
    store
        .complete_rules(view.job_uuid, 1, &source_digest())
        .unwrap();
    store
        .complete_synonyms(view.job_uuid, 1, &source_digest())
        .unwrap();
    store.accept_export(view.job_uuid).unwrap();

    let phase = store.read_migration_phase(job_uuid).unwrap();
    assert_eq!(
        phase.export_progress,
        Some(MigrationExportProgress {
            completed: 5,
            total: 5
        })
    );
    assert_eq!(phase.phase, MigrationPhase::Exporting);
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert_eq!(phase.terminal_at, None);
    let raw_phase = std::fs::read_to_string(store.job_dir(job_uuid).join("migration_phase.json"))
        .expect("phase record should be persisted");
    assert!(raw_phase.contains("export_progress"));
    assert!(!raw_phase.contains("ratio"));
    assert!(!raw_phase.contains("percent"));
}

#[test]
fn migration_phase_read_rejects_missing_and_corrupt_records_by_uuid() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let job_uuid = fixed_job_uuid();

    assert_eq!(
        store.read_migration_phase(job_uuid).unwrap_err().kind(),
        SpoolErrorKind::JobNotFound
    );

    store.create_migration_phase(job_uuid).unwrap();
    std::fs::write(
        store.job_dir(job_uuid).join("migration_phase.json"),
        b"not-json",
    )
    .unwrap();

    assert_eq!(
        store.read_migration_phase(job_uuid).unwrap_err().kind(),
        SpoolErrorKind::ManifestCorrupt
    );
}

#[test]
fn migration_phase_record_survives_export_artifact_deletion() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);

    let view = create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();
    store
        .commit_settings(view.job_uuid, b"settings", 1)
        .unwrap();
    // Walk the adjacent forward edges the pipeline uses to reach staging.
    for phase in [MigrationPhase::Preparing, MigrationPhase::Staging] {
        store.transition_migration_phase(job_uuid, phase).unwrap();
    }

    store
        .delete_export_artifacts(job_uuid, &source_digest())
        .unwrap();

    let phase = fixed_store(&tmp).read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.phase, MigrationPhase::Staging);
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert_eq!(
        phase.export_progress,
        Some(MigrationExportProgress {
            completed: 1,
            total: 8
        })
    );
}

#[test]
fn create_export_phase_write_failure_does_not_publish_manifest() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);
    store.create_migration_phase(job_uuid).unwrap();
    store
        .fail_next_migration_phase_commit_for_test(job_uuid)
        .unwrap();

    let error = store
        .create_export(job_uuid, &source_digest(), denominators())
        .expect_err("phase persistence failure should reject export creation");

    assert_eq!(error.kind(), SpoolErrorKind::Io);
    assert!(
        !store.manifest_path(job_uuid).exists(),
        "a failed phase refresh must not leave a live export manifest"
    );
    let phase = store.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.phase, MigrationPhase::Submitted);
    assert_eq!(phase.disposition, MigrationDisposition::Running);
    assert_eq!(phase.export_progress, None);
}

#[test]
fn create_export_recovers_after_crash_between_phase_and_manifest() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);

    let first = create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();

    // Simulate a crash that advanced the durable phase record to Exporting but
    // lost the manifest before it became durable.
    std::fs::remove_file(store.manifest_path(job_uuid)).unwrap();
    let interrupted = store.read_migration_phase(job_uuid).unwrap();
    assert_eq!(interrupted.phase, MigrationPhase::Exporting);
    assert_eq!(interrupted.disposition, MigrationDisposition::Running);

    // Restarting the same UUID must complete admission rather than fail closed.
    let recovered = fixed_store(&tmp)
        .create_export(job_uuid, &source_digest(), denominators())
        .expect("re-admission after a lost manifest must recover the same job");
    assert_eq!(recovered.job_uuid, job_uuid);
    assert!(store.manifest_path(job_uuid).exists());

    // The recovered manifest is a live export the pipeline can keep writing to.
    let reopened = fixed_store(&tmp);
    reopened
        .commit_document_page(job_uuid, br#"[{"objectID":"doc-1"}]"#, 1)
        .expect("recovered export must accept further artifacts");
    let phase = reopened.read_migration_phase(job_uuid).unwrap();
    assert_eq!(phase.phase, MigrationPhase::Exporting);
    assert_eq!(phase.disposition, MigrationDisposition::Running);

    // A durable manifest is adopted idempotently, never minted a third time: the
    // recovered identity is stable across repeated admission of the same job.
    let _ = first;
    let second = reopened
        .create_export(job_uuid, &source_digest(), denominators())
        .expect("a fully admitted export must be adopted idempotently");
    assert_eq!(second.public_handle, recovered.public_handle);
}

#[test]
fn create_export_rejects_mismatched_source_identity_on_readmission() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);
    create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();

    assert_eq!(
        store
            .create_export(job_uuid, &hex_digest(b"a-different-source"), denominators())
            .expect_err("re-admitting a different source must be refused")
            .kind(),
        SpoolErrorKind::SourceIdentityMismatch
    );
}

#[test]
fn migration_phase_read_rejects_foreign_uuid_record() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let job_a = uuid::Uuid::from_u128(0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa);
    let job_b = uuid::Uuid::from_u128(0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb);
    store.create_migration_phase(job_a).unwrap();
    store.create_migration_phase(job_b).unwrap();

    // A structurally valid record naming job A copied into job B's directory must
    // be rejected, never returned as B or used to redirect B's mutations.
    let foreign = std::fs::read(store.migration_phase_path(job_a)).unwrap();
    std::fs::write(store.migration_phase_path(job_b), &foreign).unwrap();

    assert_eq!(
        store.read_migration_phase(job_b).unwrap_err().kind(),
        SpoolErrorKind::ManifestCorrupt
    );
    assert_eq!(
        store
            .transition_migration_phase(job_b, MigrationPhase::Exporting)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ManifestCorrupt
    );
    // Job A's own record is untouched by the rejected read of B.
    assert_eq!(
        store.read_migration_phase(job_a).unwrap().phase,
        MigrationPhase::Submitted
    );
}

#[test]
fn migration_phase_transitions_reject_backward_and_skipped_edges() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);
    create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();

    // Skipping a phase forward is refused.
    assert_eq!(
        store
            .transition_migration_phase(job_uuid, MigrationPhase::Activating)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::InvalidPhaseTransition
    );

    // The legal adjacent forward edge is accepted, and repeating it is idempotent.
    store
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    store
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    store
        .transition_migration_phase(job_uuid, MigrationPhase::Staging)
        .unwrap();

    // Regressing to an earlier phase is refused.
    assert_eq!(
        store
            .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::InvalidPhaseTransition
    );

    let cancelled = store.cancel_migration(job_uuid).unwrap();
    assert_eq!(cancelled.disposition, MigrationDisposition::Cancelled);
    assert!(cancelled.terminal_at.is_some());
    assert_eq!(
        store
            .transition_migration_phase(job_uuid, MigrationPhase::Activating)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::JobTerminal
    );
    let cancelled_again = store.cancel_migration(job_uuid).unwrap();
    assert_eq!(cancelled_again, cancelled);
    assert_eq!(
        store.fail_migration(job_uuid).unwrap_err().kind(),
        SpoolErrorKind::JobTerminal
    );
}

#[test]
fn succeed_migration_requires_activating_phase() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);
    create_export_for_test(&store, job_uuid, &source_digest(), denominators()).unwrap();

    // Success may not be recorded before the destination is being activated.
    assert_eq!(
        store.succeed_migration(job_uuid).unwrap_err().kind(),
        SpoolErrorKind::InvalidPhaseTransition
    );

    for phase in [
        MigrationPhase::Preparing,
        MigrationPhase::Staging,
        MigrationPhase::Activating,
    ] {
        store.transition_migration_phase(job_uuid, phase).unwrap();
    }
    let settled = store.succeed_migration(job_uuid).unwrap();
    assert_eq!(settled.disposition, MigrationDisposition::Succeeded);
    assert_eq!(settled.phase, MigrationPhase::Activating);
    assert!(settled.terminal_at.is_some());

    assert_eq!(
        store.cancel_migration(job_uuid).unwrap_err().kind(),
        SpoolErrorKind::JobTerminal
    );
}

#[test]
fn read_migration_phase_reconciles_stale_progress_from_manifest() {
    let tmp = TempDir::new().unwrap();
    let job_uuid = fixed_job_uuid();
    let store = fixed_store(&tmp);
    create_export_for_test(
        &store,
        job_uuid,
        &source_digest(),
        accepted_reader_denominators(),
    )
    .unwrap();
    store
        .commit_document_page_with_ids(
            job_uuid,
            br#"[{"objectID":"doc-1"},{"objectID":"doc-2"}]"#,
            &["doc-1", "doc-2"],
        )
        .unwrap();

    // Simulate a crash between the manifest counter write and the phase progress
    // refresh: force the durable phase record back to an under-reporting snapshot.
    let stale = MigrationPhaseRecord {
        job_uuid,
        phase: MigrationPhase::Exporting,
        disposition: MigrationDisposition::Running,
        cancel_requested: false,
        export_progress: Some(MigrationExportProgress {
            completed: 0,
            total: accepted_reader_denominators().total(),
        }),
        created_at: fixed_now(),
        updated_at: fixed_now(),
        terminal_at: None,
    };
    store.commit_migration_phase(&stale).unwrap();

    // A direct restart read reconciles from the manifest counters, so status can
    // never permanently under-report an accepted export.
    let reopened = fixed_store(&tmp);
    let reconciled = reopened.read_migration_phase(job_uuid).unwrap();
    assert_eq!(
        reconciled.export_progress,
        Some(MigrationExportProgress {
            completed: 2,
            total: accepted_reader_denominators().total()
        })
    );

    // A forward transition persists the reconciled truth into the phase file.
    reopened
        .transition_migration_phase(job_uuid, MigrationPhase::Preparing)
        .unwrap();
    let persisted: MigrationPhaseRecord =
        serde_json::from_slice(&std::fs::read(reopened.migration_phase_path(job_uuid)).unwrap())
            .unwrap();
    assert_eq!(
        persisted.export_progress,
        Some(MigrationExportProgress {
            completed: 2,
            total: accepted_reader_denominators().total()
        })
    );
}

fn mutate_manifest(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    mutate: impl FnOnce(&mut SpoolManifest),
) {
    let mut manifest = store.read_manifest(job_uuid).unwrap();
    mutate(&mut manifest);
    store.commit_manifest(&manifest).unwrap();
}

fn artifact_final_path(store: &SpoolStore, job_uuid: uuid::Uuid, kind: ArtifactKind) -> String {
    store
        .read_manifest(job_uuid)
        .unwrap()
        .artifacts
        .into_iter()
        .find(|artifact| artifact.kind == kind && artifact.state == ArtifactState::Visible)
        .expect("artifact should exist")
        .final_path
}

fn reader_error_kind(store: &SpoolStore, job_uuid: uuid::Uuid) -> SpoolErrorKind {
    store.accepted_artifacts(job_uuid).unwrap_err().kind()
}

fn first_page_error_kind(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    kind: ArtifactKind,
) -> SpoolErrorKind {
    let reader = store.accepted_artifacts(job_uuid).unwrap();
    let page = match kind {
        ArtifactKind::DocumentPage => reader.document_pages().next().unwrap(),
        ArtifactKind::RulesPage => reader.rule_pages().next().unwrap(),
        ArtifactKind::SynonymsPage => reader.synonym_pages().next().unwrap(),
        ArtifactKind::Settings | ArtifactKind::Config => unreachable!("page kind required"),
    };
    page.unwrap_err().kind()
}

#[test]
fn default_limits_accept_algolia_free_build_plan_item_counts() {
    for items in [23_407, 999_999, 1_000_000] {
        let result = validate_default_limit_items(items, |_| {});
        assert!(
            result.is_ok(),
            "default limit should accept {items} items, got {:?}",
            result.err().map(|error| error.kind())
        );
    }
}

#[test]
fn default_limits_refuse_items_above_resource_boundary() {
    let direct_items = 1_000_001;
    assert_eq!(direct_items, 1_000_000 + 1);
    assert_eq!(
        validate_default_limit_items(direct_items, |_| {})
            .expect_err("one item above the default cap should be refused")
            .kind(),
        SpoolErrorKind::ResourceItemCountExceeded
    );

    let existing_items = 1_000_000;
    let incoming_items = 1;
    assert_eq!(existing_items + incoming_items, 1_000_000 + 1);
    assert_eq!(
        validate_default_limit_items(incoming_items, |manifest| {
            manifest.counters.rules = existing_items;
        })
        .expect_err("accumulated items above the default cap should be refused")
        .kind(),
        SpoolErrorKind::ResourceItemCountExceeded
    );
}

#[test]
fn manifest_freezes_default_item_limit_at_export_creation() {
    let (tmp, _store, manifest) = default_limit_export();
    assert_eq!(manifest.limits.max_items_per_resource, 1_000_000);

    let live_limits = SpoolLimits {
        max_items_per_resource: 2_000_000,
        ..SpoolLimits::default()
    };
    let reopened = SpoolStore::new_for_tests(
        tmp.path(),
        live_limits,
        fixed_now(),
        live_limits.minimum_free_bytes + 1,
    )
    .expect("reopened store should initialize");
    let frozen_manifest = reopened
        .read_manifest(manifest.job_uuid)
        .expect("frozen manifest should be readable");

    assert_eq!(frozen_manifest.limits.max_items_per_resource, 1_000_000);
    assert_eq!(
        reopened
            .validate_artifact_limits(&frozen_manifest, ArtifactKind::RulesPage, 0, 0, 1_000_001)
            .expect_err("frozen manifest should keep the original default cap")
            .kind(),
        SpoolErrorKind::ResourceItemCountExceeded
    );
}

#[tokio::test]
async fn creates_jobs_under_migration_export_root_with_private_mode() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("products").unwrap();

    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .expect("job should be created");

    let expected = tmp
        .path()
        .join("migration_exports")
        .join("jobs")
        .join(view.job_uuid.to_string());
    assert_eq!(store.job_dir(view.job_uuid), expected);
    assert!(expected.is_dir());
    assert!(!tmp
        .path()
        .join("products")
        .join(view.job_uuid.to_string())
        .exists());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(expected).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }
}

#[test]
fn manifest_scrubs_source_data_and_public_progress_is_derived() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();
    store
        .commit_settings(view.job_uuid, br#"{"not":"secret payload"}"#, 1)
        .unwrap();
    store
        .commit_document_page(view.job_uuid, br#"[{"objectID":"obj-1"}]"#, 2)
        .unwrap();

    let manifest = store.manifest_json(view.job_uuid).unwrap();
    for forbidden in [
        "ALGOLIA_API_KEY",
        "APPID123",
        "products_source",
        "obj-1",
        "secret payload",
        "ranking",
        "synonym-body",
        "canary-config",
    ] {
        assert!(
            !manifest.contains(forbidden),
            "manifest leaked forbidden value {forbidden}: {manifest}"
        );
    }

    let decoded: Value = serde_json::from_str(&manifest).unwrap();
    assert!(decoded.get("progress").is_none());
    assert_ne!(view.public_handle, view.checkpoint_handle);
    assert_ne!(view.public_handle, view.job_uuid.to_string());
    assert_ne!(view.checkpoint_handle, view.job_uuid.to_string());

    let status = store.public_view(&view.public_handle).unwrap();
    assert_eq!(status.progress.completed, 3);
    assert_eq!(status.progress.total, 8);
    assert!((status.progress.ratio - 0.375).abs() < f64::EPSILON);
}

#[test]
fn limits_reject_writes_without_advancing_manifest_or_exposing_artifacts() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_compressed_page_bytes = 8;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    let err = store
        .commit_document_page(view.job_uuid, b"this page is too large", 1)
        .expect_err("compressed byte limit should reject write");
    assert_eq!(err.kind(), SpoolErrorKind::CompressedPageBytesExceeded);
    assert!(store.visible_artifacts(view.job_uuid).unwrap().is_empty());
    assert_eq!(
        store
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        0
    );
}

#[test]
fn staged_artifacts_recover_without_unregistered_visible_files() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();
    let staged = store
        .pre_register_artifact_for_test(view.job_uuid, ArtifactKind::DocumentPage, "leaked bytes")
        .unwrap();
    std::fs::write(
        store.job_dir(view.job_uuid).join(&staged.final_path),
        b"leaked bytes",
    )
    .unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert!(reopened
        .visible_artifacts(view.job_uuid)
        .unwrap()
        .is_empty());
    assert!(!reopened
        .job_dir(view.job_uuid)
        .join(&staged.final_path)
        .exists());
}

#[test]
fn completed_object_sidecar_uses_only_committed_prefix_after_reopen() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1", "obj-2"])
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.completed_sidecar_path(view.job_uuid))
        .unwrap()
        .write_all(b"obj-3\n")
        .unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert_eq!(
        reopened.completed_object_ids(view.job_uuid).unwrap(),
        vec!["obj-1".to_string(), "obj-2".to_string()]
    );
    assert!(reopened
        .is_object_completed(view.job_uuid, "obj-1")
        .unwrap());
    assert!(!reopened
        .is_object_completed(view.job_uuid, "obj-3")
        .unwrap());
}

#[test]
fn deletion_fence_is_digest_checked_and_blocks_future_commits() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();
    store
        .commit_document_page(view.job_uuid, b"page", 1)
        .unwrap();

    assert!(!store
        .delete_export_artifacts(view.job_uuid, "wrong-digest")
        .unwrap());
    assert_eq!(store.visible_artifacts(view.job_uuid).unwrap().len(), 1);

    assert!(store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap());
    assert!(store.visible_artifacts(view.job_uuid).unwrap().is_empty());
    assert_eq!(
        store
            .commit_document_page(view.job_uuid, b"late", 1)
            .expect_err("fenced job should reject writes")
            .kind(),
        SpoolErrorKind::JobDeleting
    );

    let second_handle = fixed_store(&tmp);
    assert_eq!(
        second_handle
            .mark_completed_object_ids(view.job_uuid, &["obj-9"])
            .expect_err("fence must be durable across handles")
            .kind(),
        SpoolErrorKind::JobDeleting
    );
}

#[test]
fn garbage_collection_keeps_tombstone_and_does_not_scan_unregistered_paths() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let job = create_gc_job(&store, uuid::Uuid::new_v4());
    let unrelated = tmp
        .path()
        .join("migration_exports")
        .join("unregistered-secret");
    std::fs::write(&unrelated, b"do not touch").unwrap();
    let outside = tmp.path().join("outside-secret");
    std::fs::write(&outside, b"do not touch").unwrap();

    store
        .delete_export_artifacts(job.job_uuid, &source_digest())
        .unwrap();
    let later = SpoolStore::new_for_tests(
        tmp.path(),
        test_limits(),
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 2, 0).unwrap(),
        10_000,
    )
    .unwrap();

    later.collect_garbage().unwrap();

    assert!(later
        .tombstone_json(job.job_uuid)
        .unwrap()
        .contains("deleted"));
    assert_eq!(std::fs::read(&job.phase_path).unwrap(), job.phase_bytes);
    assert_eq!(
        std::fs::read(&job.async_metadata_path).unwrap(),
        job.async_metadata_bytes
    );
    assert_eq!(
        later.read_migration_phase(job.job_uuid).unwrap().job_uuid,
        job.job_uuid
    );
    assert_eq!(
        later
            .read_async_migration_metadata(job.job_uuid)
            .unwrap()
            .job_uuid,
        job.job_uuid
    );
    assert!(unrelated.exists());
    assert!(outside.exists());
}

#[test]
fn garbage_collection_reclaims_only_terminal_payloads_after_retention() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_staged_artifacts = 8;
    let now = fixed_now();
    let store = SpoolStore::new_for_tests(tmp.path(), limits, now, 10_000).unwrap();
    let eligible = [
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000001),
            MigrationDisposition::Succeeded,
            now - chrono::Duration::seconds(limits.retention_seconds),
        ),
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000002),
            MigrationDisposition::Failed,
            now - chrono::Duration::seconds(limits.retention_seconds + 1),
        ),
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000003),
            MigrationDisposition::Cancelled,
            now - chrono::Duration::seconds(limits.retention_seconds + 2),
        ),
    ];
    let ineligible = [
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000004),
            MigrationDisposition::Failed,
            Some(now - chrono::Duration::seconds(limits.retention_seconds - 1)),
        ),
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000005),
            MigrationDisposition::Running,
            None,
        ),
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000006),
            MigrationDisposition::Running,
            Some(now - chrono::Duration::seconds(limits.retention_seconds + 30)),
        ),
        (
            uuid::Uuid::from_u128(0x10000000000000000000000000000007),
            MigrationDisposition::Succeeded,
            None,
        ),
    ];

    let mut eligible_jobs = Vec::new();
    for (job_uuid, disposition, terminal_at) in eligible {
        let mut job = create_gc_job(&store, job_uuid);
        write_gc_phase(&store, job_uuid, disposition, Some(terminal_at));
        refresh_gc_control_snapshot(&store, &mut job);
        eligible_jobs.push(job);
    }
    let mut ineligible_snapshots = BTreeMap::new();
    for (job_uuid, disposition, terminal_at) in ineligible {
        create_gc_job(&store, job_uuid);
        write_gc_phase(&store, job_uuid, disposition, terminal_at);
        ineligible_snapshots.insert(job_uuid, snapshot_job_files(&store, job_uuid));
    }

    let expected_reclaimed = GC_EXPECTED_RECLAIMED_BYTES_PER_JOB * eligible_jobs.len() as u64;

    store.collect_garbage().unwrap();

    let mut actual_reclaimed = 0;
    for job in eligible_jobs {
        actual_reclaimed += job.reclaimable_bytes;
        assert_eq!(
            job.reclaimable_bytes, GC_EXPECTED_RECLAIMED_BYTES_PER_JOB,
            "fixture should prove the hand-calculated payload byte total"
        );
        for path in job.deleted_paths {
            assert!(
                !path.exists(),
                "eligible payload path should be deleted: {}",
                path.display()
            );
        }
        assert_eq!(
            std::fs::read(&job.phase_path).unwrap(),
            job.phase_bytes,
            "phase control file must survive byte-for-byte"
        );
        assert_eq!(
            std::fs::read(&job.async_metadata_path).unwrap(),
            job.async_metadata_bytes,
            "async owner control file must survive byte-for-byte"
        );
        assert_eq!(
            store.read_migration_phase(job.job_uuid).unwrap().job_uuid,
            job.job_uuid
        );
        assert_eq!(
            store
                .read_async_migration_metadata(job.job_uuid)
                .unwrap()
                .job_uuid,
            job.job_uuid
        );
        assert_manifest_payloads_reclaimed(&store, job.job_uuid);
    }
    assert_eq!(actual_reclaimed, expected_reclaimed);

    for (job_uuid, before) in ineligible_snapshots {
        assert_eq!(
            snapshot_job_files(&store, job_uuid),
            before,
            "ineligible job {job_uuid} must be byte-for-byte unchanged"
        );
    }
}

#[test]
fn garbage_collection_isolates_malformed_phase_and_continues_in_uuid_order() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_staged_artifacts = 8;
    let now = fixed_now();
    let store = SpoolStore::new_for_tests(tmp.path(), limits, now, 10_000).unwrap();
    let missing_phase = uuid::Uuid::from_u128(0x20000000000000000000000000000001);
    let invalid_json = uuid::Uuid::from_u128(0x20000000000000000000000000000002);
    let mismatched_uuid = uuid::Uuid::from_u128(0x20000000000000000000000000000003);
    let eligible = uuid::Uuid::from_u128(0x20000000000000000000000000000004);

    for job_uuid in [eligible, mismatched_uuid, invalid_json, missing_phase] {
        create_gc_job(&store, job_uuid);
        write_gc_phase(
            &store,
            job_uuid,
            MigrationDisposition::Failed,
            Some(now - chrono::Duration::seconds(limits.retention_seconds)),
        );
    }
    std::fs::remove_file(store.migration_phase_path(missing_phase)).unwrap();
    std::fs::write(store.migration_phase_path(invalid_json), b"not-json").unwrap();
    let mut wrong_record = store.read_migration_phase(mismatched_uuid).unwrap();
    wrong_record.job_uuid = uuid::Uuid::from_u128(0x29999999999999999999999999999999);
    let wrong_bytes = serde_json::to_vec_pretty(&wrong_record).unwrap();
    std::fs::write(store.migration_phase_path(mismatched_uuid), wrong_bytes).unwrap();

    let bad_before = [missing_phase, invalid_json, mismatched_uuid]
        .into_iter()
        .map(|job_uuid| (job_uuid, snapshot_job_files(&store, job_uuid)))
        .collect::<BTreeMap<_, _>>();
    let expected_eligible_deleted = gc_deleted_paths(&store, eligible);
    let captured = CapturedSpoolWarnings::default();
    let subscriber = tracing_subscriber::registry().with(captured.clone());

    tracing::subscriber::with_default(subscriber, || {
        store.collect_garbage().unwrap();
    });

    assert_eq!(
        captured.events(),
        vec![
            CapturedSpoolWarning {
                job_uuid: missing_phase,
                error_kind: spool_error_kind_name(SpoolErrorKind::JobNotFound),
            },
            CapturedSpoolWarning {
                job_uuid: invalid_json,
                error_kind: spool_error_kind_name(SpoolErrorKind::ManifestCorrupt),
            },
            CapturedSpoolWarning {
                job_uuid: mismatched_uuid,
                error_kind: spool_error_kind_name(SpoolErrorKind::ManifestCorrupt),
            },
        ],
        "malformed phase warnings must follow lexical UUID order"
    );
    for (job_uuid, before) in bad_before {
        assert_eq!(
            snapshot_job_files(&store, job_uuid),
            before,
            "bad phase job {job_uuid} must fail closed"
        );
    }
    for path in expected_eligible_deleted {
        assert!(
            !path.exists(),
            "later eligible job should still be reclaimed: {}",
            path.display()
        );
    }
    assert_manifest_payloads_reclaimed(&store, eligible);

    let post_first_pass = [missing_phase, invalid_json, mismatched_uuid, eligible]
        .into_iter()
        .map(|job_uuid| (job_uuid, snapshot_job_files(&store, job_uuid)))
        .collect::<BTreeMap<_, _>>();
    let post_first_accounting = [missing_phase, invalid_json, mismatched_uuid, eligible]
        .into_iter()
        .map(|job_uuid| (job_uuid, manifest_payload_accounting(&store, job_uuid)))
        .collect::<BTreeMap<_, _>>();

    store.collect_garbage().unwrap();

    for (job_uuid, before) in post_first_pass {
        assert_eq!(
            snapshot_job_files(&store, job_uuid),
            before,
            "second pass should leave job files unchanged for {job_uuid}"
        );
    }
    for (job_uuid, before) in post_first_accounting {
        assert_eq!(
            manifest_payload_accounting(&store, job_uuid),
            before,
            "second pass should leave manifest accounting unchanged for {job_uuid}"
        );
    }
}

#[derive(Debug)]
struct GcJobFixture {
    job_uuid: uuid::Uuid,
    deleted_paths: Vec<std::path::PathBuf>,
    reclaimable_bytes: u64,
    phase_path: std::path::PathBuf,
    phase_bytes: Vec<u8>,
    async_metadata_path: std::path::PathBuf,
    async_metadata_bytes: Vec<u8>,
}

fn create_gc_job(store: &SpoolStore, job_uuid: uuid::Uuid) -> GcJobFixture {
    store
        .create_async_migration_admission(job_uuid, "target-index")
        .unwrap();
    store
        .create_export(
            job_uuid,
            &source_digest(),
            ResourceDenominators {
                settings: 1,
                documents: 1,
                rules: 1,
                synonyms: 1,
                config: 0,
            },
        )
        .unwrap();
    store
        .commit_settings(job_uuid, br#"{"ranking":["typo"]}"#, 1)
        .unwrap();
    store
        .commit_document_page_with_ids(job_uuid, br#"[{"objectID":"doc-1"}]"#, &["doc-1"])
        .unwrap();
    store
        .commit_rule_page_with_ids(job_uuid, br#"[{"objectID":"rule-1"}]"#, &["rule-1"])
        .unwrap();
    store
        .commit_synonym_page_with_ids(job_uuid, br#"[{"objectID":"syn-1"}]"#, &["syn-1"])
        .unwrap();
    GcJobFixture {
        job_uuid,
        deleted_paths: gc_deleted_paths(store, job_uuid),
        reclaimable_bytes: gc_reclaimable_bytes(store, job_uuid),
        phase_path: store.migration_phase_path(job_uuid),
        phase_bytes: std::fs::read(store.migration_phase_path(job_uuid)).unwrap(),
        async_metadata_path: store.async_migration_metadata_path(job_uuid),
        async_metadata_bytes: std::fs::read(store.async_migration_metadata_path(job_uuid)).unwrap(),
    }
}

fn refresh_gc_control_snapshot(store: &SpoolStore, job: &mut GcJobFixture) {
    job.phase_bytes = std::fs::read(store.migration_phase_path(job.job_uuid)).unwrap();
    job.async_metadata_bytes =
        std::fs::read(store.async_migration_metadata_path(job.job_uuid)).unwrap();
}

fn write_gc_phase(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
    disposition: MigrationDisposition,
    terminal_at: Option<chrono::DateTime<Utc>>,
) {
    let mut record = store.read_migration_phase(job_uuid).unwrap();
    record.disposition = disposition;
    record.terminal_at = terminal_at;
    record.updated_at = terminal_at.unwrap_or_else(fixed_now);
    if disposition == MigrationDisposition::Succeeded {
        record.phase = MigrationPhase::Activating;
    }
    if disposition == MigrationDisposition::Cancelled {
        record.cancel_requested = true;
    }
    store.commit_migration_phase(&record).unwrap();
}

fn gc_deleted_paths(store: &SpoolStore, job_uuid: uuid::Uuid) -> Vec<std::path::PathBuf> {
    let manifest = store.read_manifest(job_uuid).unwrap();
    let mut paths = visible_artifacts(&manifest)
        .map(|artifact| store.job_dir(job_uuid).join(&artifact.final_path))
        .collect::<Vec<_>>();
    paths.extend([
        store.completed_sidecar_path(job_uuid),
        store.resource_sidecar_path(job_uuid, ObjectResource::Rules),
        store.resource_sidecar_path(job_uuid, ObjectResource::Synonyms),
    ]);
    paths
}

fn gc_reclaimable_bytes(store: &SpoolStore, job_uuid: uuid::Uuid) -> u64 {
    gc_deleted_paths(store, job_uuid)
        .into_iter()
        .map(|path| std::fs::metadata(path).unwrap().len())
        .sum()
}

fn snapshot_job_files(store: &SpoolStore, job_uuid: uuid::Uuid) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    for entry in std::fs::read_dir(store.job_dir(job_uuid)).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            files.insert(
                entry.file_name().to_string_lossy().to_string(),
                std::fs::read(entry.path()).unwrap(),
            );
        }
    }
    files
}

fn assert_manifest_payloads_reclaimed(store: &SpoolStore, job_uuid: uuid::Uuid) {
    let manifest = store.read_manifest(job_uuid).unwrap();
    assert_eq!(manifest.bytes_committed, 0);
    assert_eq!(manifest.counters.total(), 0);
    assert!(manifest.artifacts.is_empty());
    assert_eq!(manifest.completed_objects, SidecarManifest::default());
    assert_eq!(manifest.completed_rules, SidecarManifest::default());
    assert_eq!(manifest.completed_synonyms, SidecarManifest::default());
}

fn manifest_payload_accounting(
    store: &SpoolStore,
    job_uuid: uuid::Uuid,
) -> (
    u64,
    ResourceCounters,
    usize,
    SidecarManifest,
    SidecarManifest,
    SidecarManifest,
) {
    let manifest = store.read_manifest(job_uuid).unwrap();
    (
        manifest.bytes_committed,
        manifest.counters,
        manifest.artifacts.len(),
        manifest.completed_objects,
        manifest.completed_rules,
        manifest.completed_synonyms,
    )
}

#[test]
fn recovery_handles_each_artifact_transaction_boundary() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_staged_artifacts = 8;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    let only_manifest = store
        .pre_register_artifact_for_test(view.job_uuid, ArtifactKind::RulesPage, "rules-secret")
        .unwrap();
    let with_temp = store
        .pre_register_artifact_for_test(
            view.job_uuid,
            ArtifactKind::SynonymsPage,
            "synonyms-secret",
        )
        .unwrap();
    std::fs::write(
        store.job_dir(view.job_uuid).join(&with_temp.temp_path),
        b"synonyms-secret",
    )
    .unwrap();
    let with_final = store
        .pre_register_artifact_for_test(view.job_uuid, ArtifactKind::Config, "config-secret")
        .unwrap();
    std::fs::write(
        store.job_dir(view.job_uuid).join(&with_final.final_path),
        b"config-secret",
    )
    .unwrap();
    store
        .commit_settings(view.job_uuid, b"registered-visible", 1)
        .unwrap();
    let registered_visible = store.visible_artifacts(view.job_uuid).unwrap()[0].clone();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert_eq!(
        reopened.visible_artifacts(view.job_uuid).unwrap(),
        vec![registered_visible]
    );
    for path in [
        only_manifest.temp_path,
        only_manifest.final_path,
        with_temp.temp_path,
        with_temp.final_path,
        with_final.temp_path,
        with_final.final_path,
    ] {
        assert!(
            !reopened.job_dir(view.job_uuid).join(path).exists(),
            "staged transaction residue should be removed"
        );
    }
}

#[test]
fn typed_artifact_methods_account_exact_limits_and_leave_no_partial_state() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_compressed_page_bytes = 6;
    limits.max_decompressed_page_bytes = 9;
    limits.max_items_per_resource = 2;
    limits.max_bytes_per_job = 12;
    limits.max_global_bytes = 18;
    limits.minimum_free_bytes = 4;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10,
    )
    .unwrap();
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    store.commit_rules_page(view.job_uuid, b"rules", 1).unwrap();
    assert_eq!(
        store
            .commit_synonyms_page(view.job_uuid, b"synonyms", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::CompressedPageBytesExceeded
    );
    assert_eq!(
        store
            .commit_config_file(view.job_uuid, b"123456", b"1234567890", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::DecompressedPageBytesExceeded
    );
    assert_eq!(
        store
            .commit_rules_page(view.job_uuid, b"abc", 2)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ResourceItemCountExceeded
    );
    assert_eq!(
        store
            .commit_settings(view.job_uuid, b"123456", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::FreeSpaceFloor
    );

    assert_eq!(store.visible_artifacts(view.job_uuid).unwrap().len(), 1);
    let status = store.public_view(&view.public_handle).unwrap();
    assert_eq!(status.progress.completed, 1);
    assert_eq!(status.progress.total, 8);
}

#[test]
fn global_byte_limit_is_derived_across_jobs_under_root_lock() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_global_bytes = 7;
    let store = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let first = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &hex_digest(b"source-a"),
        denominators(),
    )
    .unwrap();
    let second = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &hex_digest(b"source-b"),
        denominators(),
    )
    .unwrap();

    store
        .commit_document_page(first.job_uuid, b"1234", 1)
        .unwrap();
    assert_eq!(
        store
            .commit_document_page(second.job_uuid, b"1234", 1)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::GlobalBytesExceeded
    );
    store.commit_settings(second.job_uuid, b"123", 1).unwrap();

    assert_eq!(
        store
            .public_view(&first.public_handle)
            .unwrap()
            .progress
            .completed,
        1
    );
    assert_eq!(
        store
            .public_view(&second.public_handle)
            .unwrap()
            .progress
            .completed,
        1
    );
}

#[test]
fn completed_object_sidecar_commits_exact_membership_and_truncates_tail() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1", "obj-2"])
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-2", "obj-3"])
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.completed_sidecar_path(view.job_uuid))
        .unwrap()
        .write_all(b"obj-4\n")
        .unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert_eq!(
        reopened.completed_object_ids(view.job_uuid).unwrap(),
        vec![
            "obj-1".to_string(),
            "obj-2".to_string(),
            "obj-3".to_string()
        ]
    );
    assert!(reopened
        .is_object_completed(view.job_uuid, "obj-3")
        .unwrap());
    assert!(!reopened
        .is_object_completed(view.job_uuid, "obj-4")
        .unwrap());
}

#[test]
fn completed_object_sidecar_ignores_uncommitted_tail_before_next_commit() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1", "obj-2"])
        .unwrap();
    std::fs::OpenOptions::new()
        .append(true)
        .open(store.completed_sidecar_path(view.job_uuid))
        .unwrap()
        .write_all(b"obj-tail\n")
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-3"])
        .unwrap();

    assert_eq!(
        store.completed_object_ids(view.job_uuid).unwrap(),
        vec![
            "obj-1".to_string(),
            "obj-2".to_string(),
            "obj-3".to_string()
        ]
    );
    assert!(!store
        .is_object_completed(view.job_uuid, "obj-tail")
        .unwrap());
}

#[test]
fn completed_object_sidecar_corruption_is_rejected() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    store
        .mark_completed_object_ids(view.job_uuid, &["obj-1"])
        .unwrap();
    std::fs::write(store.completed_sidecar_path(view.job_uuid), b"tampered\n").unwrap();

    assert_eq!(
        store
            .completed_object_ids(view.job_uuid)
            .unwrap_err()
            .kind(),
        SpoolErrorKind::ManifestCorrupt
    );
}

#[test]
fn deletion_removes_completed_object_sidecar_and_resets_public_progress() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    store
        .commit_document_page(view.job_uuid, b"page", 2)
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-secret"])
        .unwrap();
    assert!(store.completed_sidecar_path(view.job_uuid).exists());

    store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap();

    assert!(!store.completed_sidecar_path(view.job_uuid).exists());
    assert_eq!(
        store.completed_object_ids(view.job_uuid).unwrap(),
        Vec::<String>::new()
    );
    assert_eq!(
        store
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        0
    );
}

#[test]
fn recovery_recalculates_progress_when_visible_artifact_is_missing() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();

    store
        .commit_document_page(view.job_uuid, b"page", 2)
        .unwrap();
    let artifact = store.visible_artifacts(view.job_uuid).unwrap()[0].clone();
    std::fs::remove_file(store.job_dir(view.job_uuid).join(artifact)).unwrap();

    let reopened = fixed_store(&tmp);
    reopened.recover().unwrap();

    assert!(reopened
        .visible_artifacts(view.job_uuid)
        .unwrap()
        .is_empty());
    assert_eq!(
        reopened
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        0
    );
}

#[test]
fn same_job_two_handles_preserve_artifact_and_sidecar_progress() {
    let tmp = TempDir::new().unwrap();
    let first = fixed_store(&tmp);
    let view = create_export_for_test(
        &first,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();
    let second = fixed_store(&tmp);
    let barrier = Arc::new(Barrier::new(2));

    let first_barrier = Arc::clone(&barrier);
    let first_job = view.job_uuid;
    let first_thread = std::thread::spawn(move || {
        first_barrier.wait();
        first.commit_document_page(first_job, b"doc-page", 1)
    });

    let second_barrier = Arc::clone(&barrier);
    let second_job = view.job_uuid;
    let second_thread = std::thread::spawn(move || {
        second_barrier.wait();
        second.mark_completed_object_ids(second_job, &["obj-1"])
    });

    first_thread.join().unwrap().unwrap();
    second_thread.join().unwrap().unwrap();

    let reopened = fixed_store(&tmp);
    assert_eq!(
        reopened
            .public_view(&view.public_handle)
            .unwrap()
            .progress
            .completed,
        1
    );
    assert_eq!(
        reopened.completed_object_ids(view.job_uuid).unwrap(),
        vec!["obj-1".to_string()]
    );
}

#[test]
fn cross_job_two_handles_cannot_exceed_global_cap_or_leave_orphans() {
    let tmp = TempDir::new().unwrap();
    let mut limits = test_limits();
    limits.max_global_bytes = 4;
    let first = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let first_job = create_export_for_test(
        &first,
        uuid::Uuid::new_v4(),
        &hex_digest(b"source-a"),
        denominators(),
    )
    .unwrap();
    let first_job_uuid = first_job.job_uuid;
    let second = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    let second_job = create_export_for_test(
        &second,
        uuid::Uuid::new_v4(),
        &hex_digest(b"source-b"),
        denominators(),
    )
    .unwrap();
    let second_job_uuid = second_job.job_uuid;
    let barrier = Arc::new(Barrier::new(2));

    let first_barrier = Arc::clone(&barrier);
    let first_thread = std::thread::spawn(move || {
        first_barrier.wait();
        first.commit_document_page(first_job_uuid, b"1234", 1)
    });

    let second_barrier = Arc::clone(&barrier);
    let second_thread = std::thread::spawn(move || {
        second_barrier.wait();
        second.commit_document_page(second_job_uuid, b"5678", 1)
    });

    let results = [first_thread.join().unwrap(), second_thread.join().unwrap()];
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|result| result.as_ref().err())
            .map(SpoolError::kind)
            .collect::<Vec<_>>(),
        vec![SpoolErrorKind::GlobalBytesExceeded]
    );

    let reopened = SpoolStore::new_for_tests(
        tmp.path(),
        limits,
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0).unwrap(),
        10_000,
    )
    .unwrap();
    reopened.recover().unwrap();
    let visible_count = reopened
        .visible_artifacts(first_job.job_uuid)
        .unwrap()
        .len()
        + reopened
            .visible_artifacts(second_job.job_uuid)
            .unwrap()
            .len();
    assert_eq!(visible_count, 1);
}

#[test]
fn public_outputs_and_errors_are_scrubbed_and_source_identity_must_be_digest() {
    let tmp = TempDir::new().unwrap();
    let store = fixed_store(&tmp);
    let forbidden = [
        "ALGOLIA_API_KEY",
        "APPID123",
        "products_source",
        "obj-secret",
        "record-secret",
        "settings-secret",
        "rules-secret",
        "synonyms-secret",
        "config-secret",
    ];

    let raw_err = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        "APPID123-products_source",
        denominators(),
    )
    .unwrap_err();
    assert_eq!(raw_err.kind(), SpoolErrorKind::InvalidSourceIdentityDigest);

    let view = create_export_for_test(
        &store,
        uuid::Uuid::new_v4(),
        &source_digest(),
        denominators(),
    )
    .unwrap();
    store
        .commit_settings(view.job_uuid, b"settings-secret", 1)
        .unwrap();
    store
        .mark_completed_object_ids(view.job_uuid, &["obj-secret"])
        .unwrap();
    store
        .delete_export_artifacts(view.job_uuid, &source_digest())
        .unwrap();
    let later = SpoolStore::new_for_tests(
        tmp.path(),
        test_limits(),
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 2, 0).unwrap(),
        10_000,
    )
    .unwrap();
    later.collect_garbage().unwrap();

    let status = format!("{:?}", later.public_view(&view.public_handle).unwrap());
    let tombstone = later.tombstone_json(view.job_uuid).unwrap();
    let display = raw_err.to_string();
    let debug = format!("{raw_err:?}");
    for rendered in [status, tombstone, display, debug] {
        for secret in forbidden {
            assert!(
                !rendered.contains(secret),
                "scrubbed output leaked {secret}: {rendered}"
            );
        }
    }
}

#[test]
fn accepted_reader_decodes_only_accepted_jobs_and_typed_artifacts() {
    let (_tmp, store, accepted_job) = accepted_store_with_artifacts();
    let reader = store
        .accepted_artifacts(accepted_job)
        .expect("accepted job should be readable");

    assert_eq!(reader.settings().unwrap(), json!({"ranking": ["typo"]}));
    let document_pages = reader
        .document_pages()
        .collect::<SpoolResult<Vec<_>>>()
        .unwrap();
    assert_eq!(document_pages.len(), 1);
    assert_eq!(document_pages[0].page_index, 0);
    assert_eq!(document_pages[0].manifest_count, 2);
    assert_eq!(
        document_pages[0].items,
        vec![json!({"objectID": "doc-1"}), json!({"objectID": "doc-2"})]
    );
    assert_eq!(
        reader
            .rule_pages()
            .collect::<SpoolResult<Vec<_>>>()
            .unwrap()[0]
            .manifest_count,
        1
    );
    assert_eq!(
        reader
            .synonym_pages()
            .collect::<SpoolResult<Vec<_>>>()
            .unwrap()[0]
            .manifest_count,
        1
    );

    let tmp = TempDir::new().unwrap();
    for state in [
        LifecycleState::Running,
        LifecycleState::Failed,
        LifecycleState::Deleting,
        LifecycleState::Deleted,
    ] {
        let store = fixed_store(&tmp);
        let view = create_export_for_test(
            &store,
            uuid::Uuid::new_v4(),
            &source_digest(),
            denominators(),
        )
        .unwrap();
        mutate_manifest(&store, view.job_uuid, |manifest| {
            manifest.lifecycle = state;
        });
        assert_eq!(
            reader_error_kind(&store, view.job_uuid),
            SpoolErrorKind::JobNotAccepted
        );
    }
}

#[test]
fn accepted_reader_refuses_config_artifacts_before_exposing_paths() {
    let (_tmp, store, job_uuid) = accepted_store_with_artifacts();
    mutate_manifest(&store, job_uuid, |manifest| {
        manifest.artifacts.push(ArtifactManifest {
            kind: ArtifactKind::Config,
            state: ArtifactState::Visible,
            temp_path: ".fj-spool-tmp-config-test.tmp".to_string(),
            final_path: "config-test.bin".to_string(),
            compressed_bytes: 2,
            decompressed_bytes: 2,
            item_count: 1,
            digest: hex_digest(b"{}"),
        });
    });
    std::fs::write(store.job_dir(job_uuid).join("config-test.bin"), b"{}").unwrap();

    assert_eq!(
        reader_error_kind(&store, job_uuid),
        SpoolErrorKind::UnsupportedArtifactKind
    );
}

#[test]
fn accepted_reader_refuses_unsafe_manifest_artifact_paths() {
    for final_path in [
        "../documents.bin",
        "/tmp/documents.bin",
        "documents/../documents.bin",
    ] {
        let (_tmp, store, job_uuid) = accepted_store_with_artifacts();
        mutate_manifest(&store, job_uuid, |manifest| {
            manifest
                .artifacts
                .iter_mut()
                .find(|artifact| artifact.kind == ArtifactKind::DocumentPage)
                .unwrap()
                .final_path = final_path.to_string();
        });

        assert_eq!(
            reader_error_kind(&store, job_uuid),
            SpoolErrorKind::InvalidRelativePath
        );
    }
}

#[test]
fn accepted_reader_refuses_symlink_artifact_targets() {
    let (tmp, store, job_uuid) = accepted_store_with_artifacts();
    let final_path = artifact_final_path(&store, job_uuid, ArtifactKind::DocumentPage);
    let artifact_path = store.job_dir(job_uuid).join(&final_path);
    std::fs::remove_file(&artifact_path).unwrap();
    let outside = tmp.path().join("outside-documents.json");
    std::fs::write(&outside, br#"[{"objectID":"doc-1"},{"objectID":"doc-2"}]"#).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside, artifact_path).unwrap();

    assert_eq!(
        first_page_error_kind(&store, job_uuid, ArtifactKind::DocumentPage),
        SpoolErrorKind::ManifestCorrupt
    );
}

#[test]
fn accepted_reader_refuses_corrupt_artifact_payloads() {
    let cases: Vec<(&str, SpoolErrorKind, ArtifactCorruption)> = vec![
        (
            "short length",
            SpoolErrorKind::ManifestCorrupt,
            Box::new(|store, job_uuid| {
                let path = artifact_final_path(store, job_uuid, ArtifactKind::DocumentPage);
                std::fs::write(store.job_dir(job_uuid).join(path), b"[]").unwrap();
            }),
        ),
        (
            "digest mismatch",
            SpoolErrorKind::ResourceVerificationFailed,
            Box::new(|store, job_uuid| {
                let path = artifact_final_path(store, job_uuid, ArtifactKind::DocumentPage);
                std::fs::write(
                    store.job_dir(job_uuid).join(path),
                    br#"[{"objectID":"doc-1"},{"objectID":"doc-X"}]"#,
                )
                .unwrap();
            }),
        ),
        (
            "non json",
            SpoolErrorKind::ManifestCorrupt,
            Box::new(|store, job_uuid| {
                let path = artifact_final_path(store, job_uuid, ArtifactKind::DocumentPage);
                std::fs::write(store.job_dir(job_uuid).join(path), b"not json at all").unwrap();
            }),
        ),
        (
            "wrong shape",
            SpoolErrorKind::ManifestCorrupt,
            Box::new(|store, job_uuid| {
                let path = artifact_final_path(store, job_uuid, ArtifactKind::DocumentPage);
                let bytes = br#"{"objectID":"doc-1"}"#;
                std::fs::write(store.job_dir(job_uuid).join(&path), bytes).unwrap();
                mutate_manifest(store, job_uuid, |manifest| {
                    let artifact = manifest
                        .artifacts
                        .iter_mut()
                        .find(|artifact| artifact.final_path == path)
                        .unwrap();
                    artifact.compressed_bytes = bytes.len() as u64;
                    artifact.decompressed_bytes = bytes.len() as u64;
                    artifact.digest = hex_digest(bytes);
                });
            }),
        ),
        (
            "item count mismatch",
            SpoolErrorKind::ManifestCorrupt,
            Box::new(|store, job_uuid| {
                mutate_manifest(store, job_uuid, |manifest| {
                    manifest
                        .artifacts
                        .iter_mut()
                        .find(|artifact| artifact.kind == ArtifactKind::DocumentPage)
                        .unwrap()
                        .item_count = 3;
                });
            }),
        ),
    ];

    for (name, expected, tamper) in cases {
        let (_tmp, store, job_uuid) = accepted_store_with_artifacts();
        tamper(&store, job_uuid);
        assert_eq!(
            first_page_error_kind(&store, job_uuid, ArtifactKind::DocumentPage),
            expected,
            "{name}"
        );
    }
}
