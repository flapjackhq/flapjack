use super::*;
use crate::index::manager::publication::{
    PublicationScanAction, PublicationTarget, PublicationTargetDisposition,
    PublicationTransactionId,
};
use tempfile::TempDir;

#[tokio::test]
async fn startup_repair_unloads_only_publication_targets_and_reports_fail_closed_disposition() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("products").unwrap();
    manager.create_tenant("unrelated").unwrap();
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_stale").unwrap();
    let paths = publication::PublicationPaths::new(temp.path(), &target, &transaction);
    std::fs::create_dir_all(&paths.staging).unwrap();
    std::fs::write(paths.staging.join("settings.json"), b"unproven").unwrap();

    assert!(manager.loaded.contains_key("products"));
    assert!(manager.loaded.contains_key("unrelated"));

    let reports = manager.repair_publications_before_serve().unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].action, PublicationScanAction::Quarantined);
    assert_eq!(
        reports[0].disposition,
        PublicationTargetDisposition::Unavailable
    );
    assert!(!manager.loaded.contains_key("products"));
    assert!(manager.loaded.contains_key("unrelated"));
}

#[tokio::test]
async fn clean_target_repair_preserves_runtime_state_without_publication_evidence() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("products").unwrap();
    assert!(manager.get_or_create_oplog("products").is_some());
    manager.facet_cache.insert(
        "products:facets".to_string(),
        std::sync::Arc::new((
            std::time::Instant::now(),
            1,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            true,
        )),
    );

    let report = manager.repair_publication_target("products").unwrap();

    assert_eq!(report.disposition, PublicationTargetDisposition::Loadable);
    assert!(manager.loaded.contains_key("products"));
    assert!(manager.oplogs.contains_key("products"));
    assert!(manager.facet_cache.contains_key("products:facets"));
}

#[tokio::test]
async fn quarantined_repair_without_live_byte_mutation_preserves_runtime_state() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("products").unwrap();
    assert!(manager.get_or_create_oplog("products").is_some());
    manager.facet_cache.insert(
        "products:facets".to_string(),
        std::sync::Arc::new((
            std::time::Instant::now(),
            1,
            std::collections::HashMap::new(),
            std::collections::HashMap::new(),
            true,
        )),
    );
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_stale").unwrap();
    let paths = publication::PublicationPaths::new(temp.path(), &target, &transaction);
    std::fs::write(paths.target.join("live_marker.txt"), b"live").unwrap();
    std::fs::create_dir_all(&paths.staging).unwrap();
    std::fs::write(paths.staging.join("settings.json"), b"unproven").unwrap();

    let report = manager.repair_publication_target("products").unwrap();

    assert_eq!(report.action, PublicationScanAction::Quarantined);
    assert!(manager.loaded.contains_key("products"));
    assert!(manager.oplogs.contains_key("products"));
    assert!(manager.facet_cache.contains_key("products:facets"));
    assert_eq!(
        std::fs::read_to_string(paths.target.join("live_marker.txt")).unwrap(),
        "live"
    );
    assert!(paths.quarantine.join("staging/settings.json").exists());
}
