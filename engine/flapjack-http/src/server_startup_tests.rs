use super::*;
use crate::handlers::migration::spool::{
    MigrationDisposition, MigrationPhase, SpoolLimits, SpoolStore,
};
use flapjack::index::manager::publication::{
    canonical_tenant_tree_digest, PublicationArtifactManifest, PublicationArtifactManifestEntry,
    PublicationArtifactRoot, PublicationEvent, PublicationGenerationEvidence, PublicationJournal,
    PublicationPaths, PublicationPhase, PublicationScanAction, PublicationTarget,
    PublicationTargetDisposition, PublicationTransactionId, RepairDecision,
    TantivyManagedInventory,
};
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn copy_tree(source: &Path, destination: &Path) {
    std::fs::create_dir_all(destination).unwrap();
    for entry in std::fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &destination_path);
        } else {
            std::fs::copy(entry.path(), destination_path).unwrap();
        }
    }
}

fn write_journal(
    base: &Path,
    target_name: &str,
    transaction_name: &str,
    phase: PublicationPhase,
    manifest: PublicationArtifactManifest,
) -> PublicationPaths {
    let target = PublicationTarget::new(target_name).unwrap();
    let transaction = PublicationTransactionId::new(transaction_name).unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    let inventory = TantivyManagedInventory::from_existing_trees([
        paths.target.as_path(),
        paths.staging.as_path(),
        paths.backup.as_path(),
    ])
    .unwrap();
    let new_tree = if paths.staging.exists() {
        &paths.staging
    } else {
        &paths.target
    };
    let mut journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new("http_startup_generation").unwrap(),
        canonical_tenant_tree_digest(new_tree, &inventory).unwrap(),
        paths.clone(),
    );
    let prior_tree = if paths.backup.exists() {
        Some(&paths.backup)
    } else if paths.staging.exists() && paths.target.exists() {
        Some(&paths.target)
    } else {
        None
    };
    journal.prior_digest =
        prior_tree.map(|path| canonical_tenant_tree_digest(path, &inventory).unwrap());
    journal.artifact_manifest = manifest;
    if phase == PublicationPhase::Committed {
        journal = journal.apply(PublicationEvent::Commit).unwrap();
    }
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(
        &paths.journal,
        serde_json::to_vec_pretty(&journal.to_json_value()).unwrap(),
    )
    .unwrap();
    paths
}

fn prepare_committed_cleanup(base: &Path, manager: &flapjack::IndexManager) -> PublicationPaths {
    manager.create_tenant("products").unwrap();
    manager.create_tenant("products_old_seed").unwrap();
    manager.unload(&"products".to_string()).unwrap();
    manager.unload(&"products_old_seed".to_string()).unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_cleanup").unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    copy_tree(&base.join("products_old_seed"), &paths.backup);
    write_journal(
        base,
        "products",
        "txn_cleanup",
        PublicationPhase::Committed,
        PublicationArtifactManifest::default(),
    )
}

fn prepare_quarantined_replacement(
    base: &Path,
    manager: &flapjack::IndexManager,
) -> PublicationPaths {
    manager.create_tenant("retained").unwrap();
    manager.create_tenant("retained_new_seed").unwrap();
    manager.unload(&"retained".to_string()).unwrap();
    manager.unload(&"retained_new_seed".to_string()).unwrap();
    let target = PublicationTarget::new("retained").unwrap();
    let transaction = PublicationTransactionId::new("txn_quarantine").unwrap();
    let paths = PublicationPaths::new(base, &target, &transaction);
    copy_tree(&base.join("retained_new_seed"), &paths.staging);
    let manifest = PublicationArtifactManifest::new([PublicationArtifactManifestEntry::journaled(
        "query_suggestions",
        PublicationArtifactRoot::QuerySuggestions,
        PathBuf::from("wrong.json"),
        PathBuf::from("also_wrong.json"),
        base.join(".query_suggestions"),
    )])
    .unwrap();
    write_journal(
        base,
        "retained",
        "txn_quarantine",
        PublicationPhase::Prepared,
        manifest,
    )
}

#[tokio::test]
async fn pre_serve_barrier_repairs_without_peers_and_preserves_report_dispositions() {
    let temp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&temp).build();
    state.manager.create_tenant("unrelated").unwrap();
    let cleanup_paths = prepare_committed_cleanup(temp.path(), &state.manager);
    let quarantine_paths = prepare_quarantined_replacement(temp.path(), &state.manager);
    let create_paths = PublicationPaths::new(
        temp.path(),
        &PublicationTarget::new("unproven_create").unwrap(),
        &PublicationTransactionId::new("txn_stale").unwrap(),
    );
    std::fs::create_dir_all(&create_paths.staging).unwrap();
    std::fs::write(create_paths.staging.join("settings.json"), b"unproven").unwrap();

    let reports = run_pre_serve_barrier(&state).await.unwrap();

    assert!(state.replication_manager.is_none());
    assert!(reports.iter().any(|report| {
        report.target.as_str() == "products"
            && report.action == PublicationScanAction::Repaired(RepairDecision::Cleanup)
            && report.disposition == PublicationTargetDisposition::Loadable
    }));
    assert!(reports.iter().any(|report| {
        report.target.as_str() == "retained"
            && report.action == PublicationScanAction::Quarantined
            && report.disposition == PublicationTargetDisposition::Loadable
    }));
    assert!(reports.iter().any(|report| {
        report.target.as_str() == "unproven_create"
            && report.disposition == PublicationTargetDisposition::Unavailable
    }));
    assert!(!cleanup_paths.backup.exists());
    assert!(quarantine_paths.quarantine.join("journal.json").exists());
    assert!(!create_paths.target.exists());
    state
        .manager
        .search("products", "", None, None, 10)
        .unwrap();
    state
        .manager
        .search("retained", "", None, None, 10)
        .unwrap();
    state
        .manager
        .search("unrelated", "", None, None, 10)
        .unwrap();
}

#[tokio::test]
async fn pre_serve_barrier_completes_publication_repair_before_catchup_future() {
    let temp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&temp).build();
    let cleanup_paths = prepare_committed_cleanup(temp.path(), &state.manager);
    let backup = cleanup_paths.backup.clone();

    let reports = run_pre_serve_barrier_with_catchup(&state, async move {
        assert!(!backup.exists(), "catch-up ran before publication repair");
        Ok(())
    })
    .await
    .unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].action,
        PublicationScanAction::Repaired(RepairDecision::Cleanup)
    );
}

#[tokio::test]
async fn pre_serve_barrier_recovers_async_migrations_before_catchup_future() {
    let temp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&temp).build();
    let spool = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
    let job_uuid = uuid::Uuid::new_v4();
    spool
        .create_async_migration_admission(job_uuid, "async_before_catchup")
        .unwrap();
    spool
        .transition_migration_phase(job_uuid, MigrationPhase::Exporting)
        .unwrap();

    run_pre_serve_barrier_with_catchup(&state, async {
        let phase = spool.read_migration_phase(job_uuid).unwrap();
        assert_eq!(
            phase.disposition,
            MigrationDisposition::Failed,
            "catch-up must not run until async recovery settles known-safe jobs"
        );
        Ok(())
    })
    .await
    .unwrap();
}
