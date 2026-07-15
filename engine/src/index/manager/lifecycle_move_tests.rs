use super::*;
use crate::analytics::AnalyticsConfig;
use crate::index::manager::publication::PublicationFaultPoint;
use crate::query_suggestions::QsConfigStore;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn artifact_tree_bytes(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn collect(root: &Path, current: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        if !current.exists() {
            return;
        }
        let mut entries: Vec<_> = std::fs::read_dir(current)
            .unwrap()
            .map(|entry| entry.unwrap())
            .collect();
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(path).unwrap(),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    collect(root, root, &mut files);
    files
}

async fn create_tenant(manager: &IndexManager, tenant: &str, marker: &str) {
    manager.create_tenant(tenant).unwrap();
    manager
        .add_documents_sync(
            tenant,
            vec![Document {
                id: format!("{marker}_document"),
                fields: HashMap::from([(
                    "title".to_string(),
                    FieldValue::Text(format!("{marker} searchable")),
                )]),
            }],
        )
        .await
        .unwrap();
    manager.unload(&tenant.to_string()).unwrap();
}

fn write_sidecars(base: &Path, tenant: &str, marker: &str) {
    let query_suggestions = QsConfigStore::new(base)
        .target_artifact_paths(tenant)
        .unwrap();
    std::fs::create_dir_all(&query_suggestions.root_dir).unwrap();
    std::fs::write(query_suggestions.config_path, format!("{marker}-config")).unwrap();
    std::fs::write(query_suggestions.status_path, format!("{marker}-status")).unwrap();
    std::fs::write(query_suggestions.log_path, format!("{marker}-log")).unwrap();

    let analytics = test_analytics_config(base);
    let analytics = analytics.target_artifact_paths(tenant);
    std::fs::create_dir_all(&analytics.searches_dir).unwrap();
    std::fs::create_dir_all(&analytics.events_dir).unwrap();
    std::fs::create_dir_all(&analytics.rollups_dir).unwrap();
    std::fs::write(
        analytics.searches_dir.join("search.parquet"),
        format!("{marker}-search"),
    )
    .unwrap();
    std::fs::write(
        analytics.events_dir.join("event.parquet"),
        format!("{marker}-event"),
    )
    .unwrap();
    std::fs::write(analytics.rollup_manifest_path, format!("{marker}-rollup")).unwrap();
}

fn test_analytics_config(base: &Path) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: base.join("analytics"),
        flush_interval_secs: 1,
        flush_size: 1,
        retention_days: 1,
    }
}

fn sidecar_bytes(
    base: &Path,
    tenant: &str,
) -> (BTreeMap<PathBuf, Vec<u8>>, BTreeMap<PathBuf, Vec<u8>>) {
    let query_suggestions = QsConfigStore::new(base)
        .target_artifact_paths(tenant)
        .unwrap();
    let analytics = test_analytics_config(base).target_artifact_paths(tenant);
    (
        artifact_tree_bytes(&query_suggestions.root_dir)
            .into_iter()
            .filter(|(path, _)| path.to_string_lossy().starts_with(tenant))
            .collect(),
        artifact_tree_bytes(&analytics.index_root),
    )
}

fn all_sidecar_bytes(base: &Path) -> (BTreeMap<PathBuf, Vec<u8>>, BTreeMap<PathBuf, Vec<u8>>) {
    (
        artifact_tree_bytes(&base.join(".query_suggestions")),
        artifact_tree_bytes(&base.join("analytics")),
    )
}

#[tokio::test]
async fn move_index_promotes_target_keyed_sidecars_and_removes_source_artifacts() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    create_tenant(&manager, "source", "source").await;
    create_tenant(&manager, "destination", "destination").await;
    write_sidecars(temp_dir.path(), "source", "source");
    write_sidecars(temp_dir.path(), "destination", "destination");
    let expected_source = sidecar_bytes(temp_dir.path(), "source");

    manager.move_index("source", "destination").await.unwrap();

    assert_eq!(sidecar_bytes(temp_dir.path(), "source"), Default::default());
    let destination = sidecar_bytes(temp_dir.path(), "destination");
    assert_eq!(destination.1, expected_source.1);
    assert_eq!(
        destination.0.values().cloned().collect::<Vec<_>>(),
        expected_source.0.values().cloned().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn move_index_removes_destination_sidecars_absent_from_source() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    create_tenant(&manager, "source", "source").await;
    create_tenant(&manager, "destination", "destination").await;
    write_sidecars(temp_dir.path(), "destination", "destination");

    manager.move_index("source", "destination").await.unwrap();

    assert_eq!(
        sidecar_bytes(temp_dir.path(), "destination"),
        Default::default()
    );
}

#[tokio::test]
async fn move_index_fault_preserves_source_and_destination_sidecars_byte_for_byte() {
    for fault in [
        PublicationFaultPoint::BeforeStagingDigest,
        PublicationFaultPoint::DuringStagingSync,
        PublicationFaultPoint::AfterPrepareJournal,
        PublicationFaultPoint::AfterTargetBackup,
        PublicationFaultPoint::AfterStagingPromote,
        PublicationFaultPoint::BeforeCommitJournal,
    ] {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        create_tenant(&manager, "source", "source").await;
        create_tenant(&manager, "destination", "destination").await;
        write_sidecars(temp_dir.path(), "source", "source");
        write_sidecars(temp_dir.path(), "destination", "destination");
        let before = all_sidecar_bytes(temp_dir.path());

        let result = manager
            .move_index_with_fault_for_test("source", "destination", fault)
            .await;

        assert!(result.is_err(), "fault {fault:?} must fail move_index");
        assert_eq!(
            all_sidecar_bytes(temp_dir.path()),
            before,
            "fault {fault:?} must not change or leak sidecars"
        );
    }
}
