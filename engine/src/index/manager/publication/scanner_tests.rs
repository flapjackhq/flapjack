use super::*;
use crate::analytics::AnalyticsConfig;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn write_tree(path: &Path, value: &str) {
    std::fs::create_dir_all(path).unwrap();
    std::fs::write(path.join("settings.json"), value).unwrap();
}

fn tree_digest(path: &Path) -> ContentDigest {
    canonical_tenant_tree_digest(path, &TantivyManagedInventory::new([]).unwrap()).unwrap()
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
    let new_digest = if paths.staging.exists() {
        tree_digest(&paths.staging)
    } else if paths.target.exists() {
        tree_digest(&paths.target)
    } else {
        ContentDigest::new(format!("sha256:{}", "0".repeat(64))).unwrap()
    };
    let mut journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new("scanner_generation").unwrap(),
        new_digest,
        paths.clone(),
    );
    let prior_tree = if paths.backup.exists() {
        Some(&paths.backup)
    } else if paths.target.exists() && paths.staging.exists() {
        Some(&paths.target)
    } else {
        None
    };
    journal.prior_digest = prior_tree.map(|path| tree_digest(path));
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

fn scan(base: &Path) -> Vec<PublicationRepairReport> {
    scan_and_repair_publications(base, &AnalyticsConfig::for_data_dir(base)).unwrap()
}

#[test]
fn scanner_returns_no_actions_for_clean_storage() {
    let temp = TempDir::new().unwrap();
    write_tree(&temp.path().join("clean"), "live");

    assert!(scan(temp.path()).is_empty());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("clean/settings.json")).unwrap(),
        "live"
    );
}

#[test]
fn target_scanner_reports_clean_without_creating_namespace() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();

    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        target,
    )
    .unwrap();

    assert_eq!(report.status, PublicationRepairStatus::Clean);
    assert_eq!(report.action, PublicationScanAction::Clean);
    assert_eq!(report.transaction_id, None);
    assert_eq!(report.phase, None);
    assert_eq!(report.evidence, None);
    assert!(!temp.path().join(".publication/products").exists());
}

#[test]
fn target_scanner_reports_existing_empty_namespace_as_unresolved_evidence() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    std::fs::create_dir_all(temp.path().join(".publication/products")).unwrap();

    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        target,
    )
    .unwrap();

    assert_eq!(report.status, PublicationRepairStatus::Unresolved);
    assert_eq!(report.action, PublicationScanAction::Unresolved);
    assert_eq!(report.transaction_id, None);
    assert_eq!(report.phase, None);
    assert_eq!(
        report.evidence,
        Some(PathBuf::from(".publication/products"))
    );
    assert_eq!(
        report.disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert!(temp.path().join(".publication/products").exists());
}

#[cfg(unix)]
#[test]
fn target_scanner_reports_symlinked_live_tenant_without_namespace_as_unresolved() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    write_tree(outside.path(), "escaped");
    std::os::unix::fs::symlink(outside.path(), temp.path().join("products")).unwrap();

    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        PublicationTarget::new("products").unwrap(),
    )
    .unwrap();

    assert_eq!(report.status, PublicationRepairStatus::Unresolved);
    assert_eq!(report.action, PublicationScanAction::Unresolved);
    assert_eq!(report.transaction_id, None);
    assert_eq!(report.phase, None);
    assert_eq!(report.evidence, None);
    assert_eq!(
        report.disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert_eq!(
        std::fs::read_to_string(outside.path().join("settings.json")).unwrap(),
        "escaped"
    );
}

#[cfg(unix)]
#[test]
fn target_scanner_reports_symlinked_publication_namespace_as_unresolved_evidence() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::create_dir_all(temp.path().join(".publication")).unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join(".publication/products")).unwrap();

    let report = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        PublicationTarget::new("products").unwrap(),
    )
    .unwrap();

    assert_eq!(report.status, PublicationRepairStatus::Unresolved);
    assert_eq!(report.action, PublicationScanAction::Unresolved);
    assert_eq!(report.transaction_id, None);
    assert_eq!(report.phase, None);
    assert_eq!(
        report.evidence,
        Some(PathBuf::from(".publication/products"))
    );
    assert_eq!(
        report.disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert!(
        std::fs::symlink_metadata(temp.path().join(".publication/products"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn scanner_rejects_symlinked_publication_root_before_enumerating_targets() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::fs::create_dir_all(outside.path().join("products/txn_external")).unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join(".publication")).unwrap();

    let error = publication_scan_targets(temp.path())
        .expect_err("symlinked publication root must fail closed")
        .to_string();

    assert!(
        error.contains("publication root") && error.contains("symlink"),
        "publication root proof should reject the symlink before target enumeration, got: {error}"
    );
}

#[cfg(unix)]
#[test]
fn target_scanner_rejects_symlinked_publication_root_before_external_journal_read() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let external_target = PublicationTarget::new("products").unwrap();
    let external_transaction = PublicationTransactionId::new("txn_external").unwrap();
    let external_paths =
        PublicationPaths::new(outside.path(), &external_target, &external_transaction);
    write_tree(&external_paths.staging, "external");
    write_journal(
        outside.path(),
        "products",
        "txn_external",
        PublicationPhase::Prepared,
        PublicationArtifactManifest::default(),
    );
    std::os::unix::fs::symlink(
        outside.path().join(".publication"),
        temp.path().join(".publication"),
    )
    .unwrap();

    let error = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        PublicationTarget::new("products").unwrap(),
    )
    .expect_err("symlinked publication root must fail before reading external evidence")
    .to_string();

    assert!(
        error.contains("publication root") && error.contains("symlink"),
        "publication root proof should own the rejection, got: {error}"
    );
    assert!(
        external_paths.journal.exists(),
        "external journal must not be consumed through a symlinked publication root"
    );
}

#[cfg(unix)]
#[test]
fn target_scanner_rejects_symlinked_transaction_root_before_inventory_contents() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let nested = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_root").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    std::fs::create_dir_all(paths.staging.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(nested.path(), outside.path().join("escaped_link")).unwrap();
    std::os::unix::fs::symlink(outside.path(), &paths.staging).unwrap();

    let error = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        target,
    )
    .expect_err("symlinked transaction root must fail closed")
    .to_string();

    assert!(
        error.contains("publication repair managed"),
        "root proof should own the rejection, got: {error}"
    );
    assert!(
        !error.contains("tenant artifact"),
        "inventory must not inspect content beyond a symlinked root: {error}"
    );
}

#[cfg(unix)]
#[test]
fn target_scanner_rejects_symlinked_journal_before_reading_external_evidence() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let external_paths = write_journal(
        outside.path(),
        "products",
        "txn_journal",
        PublicationPhase::Prepared,
        PublicationArtifactManifest::default(),
    );
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_journal").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&external_paths.journal, &paths.journal).unwrap();

    let error = scan_and_repair_publication_target(
        temp.path(),
        &AnalyticsConfig::for_data_dir(temp.path()),
        target,
    )
    .expect_err("symlinked journal must fail before reading external evidence")
    .to_string();

    assert!(
        error.contains("publication scan evidence") && error.contains("symlink"),
        "the scanner's pre-read proof should own the rejection, got: {error}"
    );
    assert!(external_paths.journal.exists());
}

#[test]
fn prestaged_publication_allocates_activates_and_aborts_its_own_namespace() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    write_tree(&temp.path().join("products"), "old");

    let publication = PreStagedPublication::prepare(temp.path(), target.clone()).unwrap();
    assert!(publication.paths().staging.parent().unwrap().exists());
    write_tree(&publication.paths().staging, "new");
    publication.activate().unwrap();
    assert_eq!(
        std::fs::read_to_string(temp.path().join("products/settings.json")).unwrap(),
        "new"
    );

    let abandoned = PreStagedPublication::prepare(temp.path(), target).unwrap();
    write_tree(&abandoned.paths().staging, "abandoned");
    let namespace = abandoned.paths().staging.parent().unwrap().to_path_buf();
    abandoned.abort().unwrap();
    assert!(!namespace.exists());
    assert_eq!(
        std::fs::read_to_string(temp.path().join("products/settings.json")).unwrap(),
        "new"
    );
}

#[cfg(unix)]
#[test]
fn prestaged_publication_rejects_symlinked_namespace_parent() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let target_root = temp.path().join(".publication/products");
    std::fs::create_dir_all(target_root.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(outside.path(), &target_root).unwrap();

    let result =
        PreStagedPublication::prepare(temp.path(), PublicationTarget::new("products").unwrap());

    assert!(result.is_err());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[cfg(unix)]
#[test]
fn prestaged_publication_rejects_symlinked_publication_parent() {
    let temp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), temp.path().join(".publication")).unwrap();

    let result =
        PreStagedPublication::prepare(temp.path(), PublicationTarget::new("products").unwrap());

    assert!(result.is_err());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn scanner_repairs_prepared_replacement_and_reports_loadable_target() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_repair").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    write_tree(&paths.target, "old");
    write_tree(&paths.staging, "new");
    write_journal(
        temp.path(),
        "products",
        "txn_repair",
        PublicationPhase::Prepared,
        PublicationArtifactManifest::default(),
    );

    let reports = scan(temp.path());

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].target.as_str(), "products");
    assert_eq!(
        reports[0].action,
        PublicationScanAction::Repaired(RepairDecision::Complete)
    );
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Loadable
    );
    assert_eq!(
        std::fs::read_to_string(paths.target.join("settings.json")).unwrap(),
        "new"
    );
}

#[test]
fn scanner_rolls_back_backup_without_target() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_backup").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    write_tree(&paths.backup, "old");
    write_journal(
        temp.path(),
        "products",
        "txn_backup",
        PublicationPhase::Prepared,
        PublicationArtifactManifest::default(),
    );

    let reports = scan(temp.path());

    assert_eq!(
        reports[0].action,
        PublicationScanAction::Repaired(RepairDecision::Rollback)
    );
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Loadable
    );
    assert_eq!(
        std::fs::read_to_string(paths.target.join("settings.json")).unwrap(),
        "old"
    );
}

#[test]
fn scanner_cleans_committed_target_with_backup() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_cleanup").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    write_tree(&paths.target, "new");
    write_tree(&paths.backup, "old");
    write_journal(
        temp.path(),
        "products",
        "txn_cleanup",
        PublicationPhase::Committed,
        PublicationArtifactManifest::default(),
    );

    let reports = scan(temp.path());

    assert_eq!(
        reports[0].action,
        PublicationScanAction::Repaired(RepairDecision::Cleanup)
    );
    assert!(!paths.backup.exists());
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Loadable
    );
}

#[test]
fn scanner_quarantines_stale_staging_without_loading_an_unproven_create() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("created").unwrap();
    let transaction = PublicationTransactionId::new("txn_stale").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    write_tree(&paths.staging, "unproven");

    let reports = scan(temp.path());

    assert_eq!(reports[0].action, PublicationScanAction::Quarantined);
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert!(paths.quarantine.join("staging/settings.json").exists());
    assert!(!paths.target.exists());
}

#[test]
fn scanner_quarantines_corrupt_journal_without_mutating_live_target() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_corrupt").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    write_tree(&paths.target, "live");
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(&paths.journal, b"not-json").unwrap();

    let reports = scan(temp.path());

    assert_eq!(reports[0].action, PublicationScanAction::Quarantined);
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert_eq!(
        std::fs::read_to_string(paths.target.join("settings.json")).unwrap(),
        "live"
    );
    assert_eq!(
        std::fs::read_to_string(paths.quarantine.join("journal.json")).unwrap(),
        "not-json"
    );
}

#[test]
fn scanner_quarantines_sidecar_boundary_violation_but_keeps_proven_old_target_loadable() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_sidecar").unwrap();
    let paths = PublicationPaths::new(temp.path(), &target, &transaction);
    write_tree(&paths.target, "old");
    write_tree(&paths.staging, "new");
    let manifest = PublicationArtifactManifest::new([PublicationArtifactManifestEntry::journaled(
        "query_suggestions",
        PublicationArtifactRoot::QuerySuggestions,
        PathBuf::from("wrong.json"),
        PathBuf::from("also_wrong.json"),
        temp.path().join(".query_suggestions"),
    )])
    .unwrap();
    write_journal(
        temp.path(),
        "products",
        "txn_sidecar",
        PublicationPhase::Prepared,
        manifest,
    );

    let reports = scan(temp.path());

    assert_eq!(reports[0].action, PublicationScanAction::Quarantined);
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Loadable
    );
    assert_eq!(
        std::fs::read_to_string(paths.target.join("settings.json")).unwrap(),
        "old"
    );
    assert!(paths.quarantine.join("journal.json").exists());
}

#[test]
fn scanner_reports_duplicate_transactions_once_without_choosing_by_iteration_order() {
    let temp = TempDir::new().unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let first = PublicationPaths::new(
        temp.path(),
        &target,
        &PublicationTransactionId::new("txn_b").unwrap(),
    );
    let second = PublicationPaths::new(
        temp.path(),
        &target,
        &PublicationTransactionId::new("txn_a").unwrap(),
    );
    write_tree(&first.staging, "first");
    write_tree(&second.staging, "second");

    let reports = scan(temp.path());

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].action, PublicationScanAction::Unresolved);
    assert_eq!(
        reports[0]
            .transactions
            .iter()
            .map(PublicationTransactionId::as_str)
            .collect::<Vec<_>>(),
        vec!["txn_a", "txn_b"]
    );
    assert!(first.staging.exists());
    assert!(second.staging.exists());
}

#[test]
fn scanner_orders_targets_and_transactions_stably_after_startup_crash() {
    let temp = TempDir::new().unwrap();
    for (name, transaction) in [("zeta", "txn_z"), ("alpha", "txn_a")] {
        let target = PublicationTarget::new(name).unwrap();
        let transaction_id = PublicationTransactionId::new(transaction).unwrap();
        let paths = PublicationPaths::new(temp.path(), &target, &transaction_id);
        write_tree(&paths.target, "old");
        write_tree(&paths.staging, "new");
        write_journal(
            temp.path(),
            name,
            transaction,
            PublicationPhase::Prepared,
            PublicationArtifactManifest::default(),
        );
    }

    let reports = scan(temp.path());

    assert_eq!(
        reports
            .iter()
            .map(|report| report.target.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha", "zeta"]
    );
    assert!(reports.iter().all(|report| {
        report.action == PublicationScanAction::Repaired(RepairDecision::Complete)
            && report.disposition == PublicationTargetDisposition::Loadable
    }));
}
