use super::{
    register_index_labeled_gauge_values, register_live_index_state_gauges, IndexGaugeSnapshot,
    IndexGaugeValues,
};
use prometheus::Registry;
use tempfile::TempDir;

/// Verify the shared live-index gauge registrar emits all three index-state families.
#[tokio::test]
async fn register_live_index_state_gauges_emits_storage_documents_and_oplog() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("live_idx").unwrap();
    let docs = vec![flapjack::types::Document {
        id: "d1".to_string(),
        fields: std::collections::HashMap::from([(
            "name".to_string(),
            flapjack::types::FieldValue::Text("Alice".to_string()),
        )]),
    }];
    state
        .manager
        .add_documents_sync("live_idx", docs)
        .await
        .unwrap();
    crate::background_tasks::refresh_metrics_snapshot(
        &state.manager,
        state.metrics_state.as_ref().unwrap(),
    )
    .unwrap();

    let registry = Registry::new();
    register_live_index_state_gauges(&registry, &state);

    let family_names: Vec<String> = registry
        .gather()
        .into_iter()
        .map(|family| family.get_name().to_string())
        .collect();

    assert!(
        family_names
            .iter()
            .any(|name| name == "flapjack_storage_bytes"),
        "storage family should be registered"
    );
    assert!(
        family_names
            .iter()
            .any(|name| name == "flapjack_documents_count"),
        "documents family should be registered"
    );
    assert!(
        family_names
            .iter()
            .any(|name| name == "flapjack_oplog_current_seq"),
        "oplog family should be registered"
    );
}

/// Verify the shared index gauge utility registers and sets all labeled values.
#[test]
fn register_index_labeled_gauge_values_registers_and_sets_values() {
    let registry = Registry::new();
    register_index_labeled_gauge_values(
        &registry,
        "flapjack_test_index_metric",
        "Test helper metric",
        vec![("alpha".to_string(), 12.0), ("beta".to_string(), 99.0)],
    );

    let family = registry
        .gather()
        .into_iter()
        .find(|metric_family| metric_family.get_name() == "flapjack_test_index_metric")
        .expect("test metric family should be registered");

    let mut values_by_label = std::collections::HashMap::new();
    for metric in family.get_metric() {
        let label = metric
            .get_label()
            .iter()
            .find(|label_pair| label_pair.get_name() == "index")
            .expect("index label must exist")
            .get_value()
            .to_string();
        values_by_label.insert(label, metric.get_gauge().get_value());
    }

    assert_eq!(values_by_label.get("alpha"), Some(&12.0));
    assert_eq!(values_by_label.get("beta"), Some(&99.0));
}

/// Verify a request does not merge newly-created tenant files into a stale
/// cached generation.
#[tokio::test]
async fn register_live_index_state_gauges_uses_only_cached_snapshot() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("fresh_idx").unwrap();
    state
        .manager
        .add_documents_sync(
            "fresh_idx",
            vec![flapjack::types::Document {
                id: "d1".to_string(),
                fields: std::collections::HashMap::from([(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("Alice".to_string()),
                )]),
            }],
        )
        .await
        .unwrap();

    let metrics_state = state.metrics_state.as_ref().unwrap();
    metrics_state.replace_index_gauges(IndexGaugeSnapshot::from([(
        "stale_only_idx".to_string(),
        IndexGaugeValues {
            documents_count: Some(9),
            storage_bytes: Some(123),
        },
    )]));

    let registry = Registry::new();
    register_live_index_state_gauges(&registry, &state);

    let storage_family = registry
        .gather()
        .into_iter()
        .find(|metric_family| metric_family.get_name() == "flapjack_storage_bytes")
        .expect("storage gauge family should be registered");
    let labels: Vec<String> = storage_family
        .get_metric()
        .iter()
        .filter_map(|metric| {
            metric
                .get_label()
                .iter()
                .find(|pair| pair.get_name() == "index")
                .map(|pair| pair.get_value().to_string())
        })
        .collect();

    assert_eq!(labels, vec!["stale_only_idx".to_string()]);
}

fn oplog_metric(state: &crate::handlers::AppState, tenant: &str) -> Option<u64> {
    let registry = Registry::new();
    super::register_oplog_sequence_gauge(&registry, state);
    registry
        .gather()
        .into_iter()
        .find(|family| family.get_name() == "flapjack_oplog_current_seq")
        .and_then(|family| {
            family
                .get_metric()
                .iter()
                .find(|metric| {
                    metric
                        .get_label()
                        .iter()
                        .any(|label| label.get_name() == "index" && label.get_value() == tenant)
                })
                .map(|metric| metric.get_gauge().get_value() as u64)
        })
}

#[tokio::test]
async fn durable_watermark_survives_native_snapshot_without_cache_warming() {
    let source_tmp = TempDir::new().unwrap();
    let target_tmp = TempDir::new().unwrap();
    let source = crate::test_helpers::TestStateBuilder::new(&source_tmp).build_shared();
    let target = crate::test_helpers::TestStateBuilder::new(&target_tmp).build_shared();
    source.manager.create_tenant("products").unwrap();
    source
        .manager
        .add_documents_sync(
            "products",
            vec![flapjack::types::Document {
                id: "alpha".into(),
                fields: std::collections::HashMap::from([(
                    "title".into(),
                    flapjack::types::FieldValue::Text("native snapshot".into()),
                )]),
            }],
        )
        .await
        .unwrap();
    let expected = source
        .manager
        .get_oplog("products")
        .unwrap()
        .committed_seq()
        .unwrap()
        .unwrap();
    assert!(expected > 0);
    let bytes = crate::test_helpers::quiesced_snapshot_bytes(&source.manager, "products").await;
    crate::startup_catchup::restore_snapshot_bytes(&target.manager, "products", bytes)
        .await
        .unwrap();
    let mut observed = Vec::new();
    for state in [&source, &target] {
        assert!(state.manager.loaded_tenant_ids().is_empty());
        assert!(state.manager.get_oplog("products").is_none());
        let before = crate::test_helpers::snapshot_tree(&state.manager.base_path);
        observed.push(oplog_metric(state, "products"));
        assert_eq!(
            crate::test_helpers::snapshot_tree(&state.manager.base_path),
            before,
            "metric collection must not create/recover oplogs or write sidecars"
        );
        assert!(state.manager.loaded_tenant_ids().is_empty());
        assert!(state.manager.get_oplog("products").is_none());
    }
    assert_eq!(
        observed,
        vec![Some(expected), Some(expected)],
        "both exported and imported cold indexes must expose durable watermarks"
    );
}

#[tokio::test]
async fn durable_watermark_distinguishes_valid_zero_missing_and_invalid_state() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("products").unwrap();
    let path = state.manager.base_path.join("products/committed_seq");
    // A loaded tenant must not supply a guessed zero for missing or invalid
    // durable evidence, nor create files for the missing case.
    for (bytes, expected) in [
        (Some("0\n"), Some(0)),
        (None, None),
        (Some("not-a-watermark"), None),
        (Some("18446744073709551616"), None),
    ] {
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        if let Some(bytes) = bytes {
            std::fs::write(&path, bytes).unwrap();
        }
        let before = crate::test_helpers::snapshot_tree(&state.manager.base_path);
        let durable = state.manager.tenant_durable_oplog_seq("products");
        if bytes.is_some() && expected.is_none() {
            assert!(durable.is_err(), "invalid evidence must remain an error");
        } else {
            assert_eq!(durable.unwrap(), expected);
        }
        assert_eq!(oplog_metric(&state, "products"), expected, "{bytes:?}");
        assert_eq!(
            crate::test_helpers::snapshot_tree(&state.manager.base_path),
            before
        );
    }
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert_eq!(oplog_metric(&state, "products"), None);
}

#[tokio::test]
async fn durable_watermark_is_withheld_during_publication_fence() {
    use flapjack::index::manager::publication::{fence_publication_admission, PublicationTarget};
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("products").unwrap();
    state
        .manager
        .add_documents_sync(
            "products",
            vec![flapjack::types::Document {
                id: "alpha".into(),
                fields: std::collections::HashMap::new(),
            }],
        )
        .await
        .unwrap();
    let expected = oplog_metric(&state, "products").unwrap();
    let fence = fence_publication_admission(
        &state.manager.base_path,
        &PublicationTarget::new("products").unwrap(),
    )
    .unwrap();
    assert_eq!(
        oplog_metric(&state, "products"),
        None,
        "a cached runtime must not bypass the publication fence"
    );
    drop(fence);
    assert_eq!(oplog_metric(&state, "products"), Some(expected));
}

#[tokio::test]
async fn durable_watermark_respects_file_lock_without_process_registry() {
    use flapjack::index::manager::publication::publication_epoch_paths_for_target_path;
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("products").unwrap();
    std::fs::write(state.manager.base_path.join("products/committed_seq"), "0").unwrap();
    let paths = publication_epoch_paths_for_target_path(&state.manager.base_path.join("products"));
    // An independently opened descriptor exercises the same exclusive lock as
    // a publisher in another process, without registering an in-process fence.
    let exclusive = std::fs::File::open(&paths.lock).unwrap();
    exclusive.lock().unwrap();
    assert_eq!(oplog_metric(&state, "products"), None);
    drop(exclusive);
    assert_eq!(oplog_metric(&state, "products"), Some(0));
    std::fs::remove_file(&paths.lock).unwrap();
    let before = crate::test_helpers::snapshot_tree(&state.manager.base_path);
    assert_eq!(oplog_metric(&state, "products"), None);
    assert_eq!(
        crate::test_helpers::snapshot_tree(&state.manager.base_path),
        before
    );
}

#[tokio::test]
async fn durable_watermark_rejects_invalid_publication_lock() {
    use flapjack::index::manager::publication::publication_epoch_paths_for_target_path;
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("products").unwrap();
    std::fs::write(state.manager.base_path.join("products/committed_seq"), "0").unwrap();
    let paths = publication_epoch_paths_for_target_path(&state.manager.base_path.join("products"));
    std::fs::remove_file(&paths.lock).unwrap();
    std::fs::create_dir(&paths.lock).unwrap();
    assert!(state.manager.tenant_durable_oplog_seq("products").is_err());
    assert_eq!(oplog_metric(&state, "products"), None);
    #[cfg(unix)]
    {
        std::fs::remove_dir(&paths.lock).unwrap();
        std::os::unix::fs::symlink(
            state.manager.base_path.join("products/committed_seq"),
            &paths.lock,
        )
        .unwrap();
        assert!(state.manager.tenant_durable_oplog_seq("products").is_err());
        assert_eq!(oplog_metric(&state, "products"), None);
    }
}

#[tokio::test]
async fn durable_watermark_legacy_index_upgrades_at_startup_without_writes() {
    use flapjack::types::Document;
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("products").unwrap();
    let alpha = Document {
        id: "alpha".into(),
        fields: std::collections::HashMap::new(),
    };
    state
        .manager
        .add_documents_sync("products", vec![alpha.clone()])
        .await
        .unwrap();
    state.manager.drain_all_write_queues().await.unwrap();
    let before = state
        .manager
        .tenant_durable_oplog_seq("products")
        .unwrap()
        .unwrap();
    drop(state);
    // Pre-publication legacy layout: durable index/oplog, no epoch sidecars.
    std::fs::remove_dir_all(tmp.path().join(".publication")).unwrap();
    let upgraded = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let legacy_data = crate::test_helpers::snapshot_tree(&tmp.path().join("products"));
    let legacy_tree = crate::test_helpers::snapshot_tree(tmp.path());
    assert_eq!(oplog_metric(&upgraded, "products"), None);
    assert_eq!(crate::test_helpers::snapshot_tree(tmp.path()), legacy_tree);
    upgraded
        .global_mutation_fence
        .acquire("legacy-upgrade")
        .await
        .unwrap();
    let fenced_tree = crate::test_helpers::snapshot_tree(tmp.path());
    crate::server::run_pre_serve_barrier(&upgraded)
        .await
        .unwrap();
    assert_eq!(crate::test_helpers::snapshot_tree(tmp.path()), fenced_tree);
    upgraded
        .global_mutation_fence
        .release("legacy-upgrade")
        .await
        .unwrap();
    // Startup owns compatibility admission; scrapes stay read-only and no
    // customer write or document read is needed to make the evidence available.
    crate::server::run_pre_serve_barrier(&upgraded)
        .await
        .unwrap();
    assert!(upgraded.manager.loaded_tenant_ids().is_empty());
    assert_eq!(oplog_metric(&upgraded, "products"), Some(before));
    assert_eq!(
        crate::test_helpers::snapshot_tree(&tmp.path().join("products")),
        legacy_data
    );
    assert_eq!(
        upgraded
            .manager
            .get_document("products", "alpha")
            .unwrap()
            .unwrap()
            .id,
        alpha.id
    );
    upgraded.manager.drain_all_write_queues().await.unwrap();
    drop(upgraded);
    let restarted = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    assert!(restarted.manager.loaded_tenant_ids().is_empty());
    assert_eq!(oplog_metric(&restarted, "products"), Some(before));
    assert_eq!(
        restarted
            .manager
            .tenant_durable_doc_count("products")
            .unwrap(),
        1
    );
}

#[tokio::test]
async fn legacy_startup_preserves_missing_and_corrupt_watermarks_and_valid_zero() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    for (tenant, bytes) in [
        ("missing", None),
        ("corrupt", Some("not-a-sequence")),
        ("zero", Some("0")),
    ] {
        state.manager.create_tenant(tenant).unwrap();
        if let Some(bytes) = bytes {
            std::fs::write(tmp.path().join(tenant).join("committed_seq"), bytes).unwrap();
        } else {
            std::fs::remove_file(tmp.path().join(tenant).join("committed_seq")).unwrap();
        }
    }
    drop(state);
    std::fs::remove_dir_all(tmp.path().join(".publication")).unwrap();
    for _ in 0..2 {
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
        let before: Vec<_> = ["missing", "corrupt", "zero"]
            .iter()
            .map(|tenant| crate::test_helpers::snapshot_tree(&tmp.path().join(tenant)))
            .collect();
        crate::server::run_pre_serve_barrier(&state).await.unwrap();
        assert!(state.manager.loaded_tenant_ids().is_empty());
        assert_eq!(oplog_metric(&state, "missing"), None);
        assert_eq!(oplog_metric(&state, "corrupt"), None);
        assert_eq!(oplog_metric(&state, "zero"), Some(0));
        for (tenant, expected) in ["missing", "corrupt", "zero"].iter().zip(before) {
            assert_eq!(
                crate::test_helpers::snapshot_tree(&tmp.path().join(tenant)),
                expected
            );
        }
        assert!(!tmp.path().join(".publication/missing").exists());
        assert!(!tmp.path().join(".publication/corrupt").exists());
    }
}
