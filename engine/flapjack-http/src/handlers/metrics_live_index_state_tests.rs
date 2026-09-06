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
