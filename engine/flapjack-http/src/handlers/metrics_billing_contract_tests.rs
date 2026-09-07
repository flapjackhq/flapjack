use super::{
    metrics_handler, register_billing_usage_gauges, register_live_index_state_gauges,
    register_live_index_state_gauges_with_snapshot_hook,
};
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use prometheus::Registry;
use tempfile::TempDir;
use tower::ServiceExt;

/// The 7 billing usage metric names that form the fjcloud metering contract.
const BILLING_METRIC_NAMES: [&str; 7] = [
    "flapjack_search_requests_total",
    "flapjack_write_operations_total",
    "flapjack_read_requests_total",
    "flapjack_bytes_in_total",
    "flapjack_search_results_total",
    "flapjack_documents_indexed_total",
    "flapjack_documents_deleted_total",
];

/// Extract the numeric value for a metric+index from Prometheus text output.
fn find_metric_value(text: &str, metric_name: &str, index: &str) -> f64 {
    let exact_sample = format!("{metric_name}{{index=\"{index}\"}}");
    text.lines()
        .find(|line| {
            let sample = line.split_whitespace().next().unwrap_or_default();
            sample == exact_sample
        })
        .unwrap_or_else(|| panic!("{}{{index={}}} not found in:\n{}", metric_name, index, text))
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap()
}

#[test]
fn metric_text_lookup_requires_exact_metric_and_index() {
    let text = concat!(
        "flapjack_search_requests_total_suffix{index=\"target\"} 91\n",
        "flapjack_search_requests_total{index=\"target_shadow\"} 73\n",
        "flapjack_search_requests_total{index=\"target\"} 7\n",
    );

    assert_eq!(
        find_metric_value(text, "flapjack_search_requests_total", "target"),
        7.0,
        "decoy metric and index substrings must not satisfy the assertion"
    );
}

fn index_metric_values(
    registry: &Registry,
    metric_name: &str,
) -> std::collections::BTreeMap<String, f64> {
    registry
        .gather()
        .into_iter()
        .find(|family| family.get_name() == metric_name)
        .map(|family| {
            family
                .get_metric()
                .iter()
                .map(|metric| {
                    let index = metric
                        .get_label()
                        .iter()
                        .find(|pair| pair.get_name() == "index")
                        .expect("per-index gauge must carry the index label")
                        .get_value()
                        .to_string();
                    (index, metric.get_gauge().get_value())
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RED 1: creating durable files must not make a gauge visible until the
/// background owner publishes a new cached generation.
#[tokio::test]
async fn index_gauge_scrape_does_not_scan_request_path() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let metrics_state = state.metrics_state.as_ref().unwrap();
    metrics_state.replace_index_gauges(std::collections::BTreeMap::from([(
        "cached_only".to_string(),
        super::IndexGaugeValues {
            documents_count: None,
            storage_bytes: Some(77),
        },
    )]));

    state.manager.create_tenant("not_refreshed").unwrap();
    state
        .manager
        .add_documents_sync(
            "not_refreshed",
            vec![flapjack::types::Document {
                id: "doc-1".to_string(),
                fields: std::collections::HashMap::new(),
            }],
        )
        .await
        .unwrap();

    let registry = Registry::new();
    register_live_index_state_gauges(&registry, &state);

    assert_eq!(
        index_metric_values(&registry, "flapjack_storage_bytes"),
        std::collections::BTreeMap::from([("cached_only".to_string(), 77.0)]),
        "request-time collection must expose only the last published generation"
    );
    assert!(
        index_metric_values(&registry, "flapjack_documents_count").is_empty(),
        "document files created after the cached generation must remain invisible until refresh"
    );
}

/// A scrape interleaved with publication must keep using the generation it
/// captured before the publication boundary.
#[tokio::test]
async fn index_gauge_publication_is_whole_generation_after_capture() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();
    let metrics_state = state.metrics_state.as_ref().unwrap().clone();
    metrics_state.replace_index_gauges(std::collections::BTreeMap::from([(
        "old".to_string(),
        super::IndexGaugeValues {
            documents_count: Some(1),
            storage_bytes: Some(10),
        },
    )]));

    let registry = Registry::new();
    register_live_index_state_gauges_with_snapshot_hook(&registry, &state, || {
        metrics_state.replace_index_gauges(std::collections::BTreeMap::from([(
            "new".to_string(),
            super::IndexGaugeValues {
                documents_count: Some(2),
                storage_bytes: Some(20),
            },
        )]));
    });
    let observed_storage = index_metric_values(&registry, "flapjack_storage_bytes");
    let observed_documents = index_metric_values(&registry, "flapjack_documents_count");

    let old_storage = std::collections::BTreeMap::from([("old".to_string(), 10.0)]);
    let old_documents = std::collections::BTreeMap::from([("old".to_string(), 1.0)]);
    assert_eq!(
        observed_storage, old_storage,
        "storage must stay on the generation captured before publication"
    );
    assert_eq!(
        observed_documents, old_documents,
        "documents must stay on the generation captured before publication"
    );
    assert_eq!(
        metrics_state
            .index_gauge_snapshot()
            .get("new")
            .and_then(|gauges| gauges.storage_bytes),
        Some(20),
        "the hook must really publish the newer generation during registration"
    );
}

/// Poll /metrics on a test app and return the body as a string.
async fn poll_metrics(app: &Router<()>) -> String {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    String::from_utf8(body.to_vec()).unwrap()
}

/// Verify all 7 billing usage series are annotated as `gauge` (not `counter`) in
/// Prometheus text format.
#[tokio::test]
async fn billing_series_use_gauge_type_not_counter() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

    let counters = crate::usage_middleware::TenantUsageCounters::new();
    counters
        .search_count
        .store(1, std::sync::atomic::Ordering::Relaxed);
    state
        .usage_counters
        .insert("type_check".to_string(), counters);

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let text = poll_metrics(&app).await;

    for name in BILLING_METRIC_NAMES {
        let type_line = text
            .lines()
            .find(|line| line.starts_with("# TYPE") && line.contains(name))
            .unwrap_or_else(|| panic!("missing # TYPE line for {}", name));
        assert!(
            type_line.ends_with(" gauge"),
            "{} must be typed as gauge per metering contract, got: {}",
            name,
            type_line
        );
    }
}

/// After daily rollup, all 7 billing usage series must report 0 in /metrics output.
#[tokio::test]
async fn billing_usage_gauges_reset_to_zero_after_rollup() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

    let counters = crate::usage_middleware::TenantUsageCounters::new();
    counters
        .search_count
        .store(10, std::sync::atomic::Ordering::Relaxed);
    counters
        .write_count
        .store(5, std::sync::atomic::Ordering::Relaxed);
    counters
        .read_count
        .store(3, std::sync::atomic::Ordering::Relaxed);
    counters
        .bytes_in
        .store(1024, std::sync::atomic::Ordering::Relaxed);
    counters
        .search_results_total
        .store(42, std::sync::atomic::Ordering::Relaxed);
    counters
        .documents_indexed_total
        .store(8, std::sync::atomic::Ordering::Relaxed);
    counters
        .documents_deleted_total
        .store(2, std::sync::atomic::Ordering::Relaxed);
    state
        .usage_counters
        .insert("rollup_idx".to_string(), counters);

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(state.clone());

    let text_before = poll_metrics(&app).await;
    assert_eq!(
        find_metric_value(&text_before, "flapjack_search_requests_total", "rollup_idx"),
        10.0
    );

    let persistence = crate::usage_persistence::UsagePersistence::new(tmp.path()).unwrap();
    persistence
        .rollup("2026-03-15", &state.usage_counters)
        .unwrap();

    let text_after = poll_metrics(&app).await;
    for name in BILLING_METRIC_NAMES {
        let value = find_metric_value(&text_after, name, "rollup_idx");
        assert_eq!(value, 0.0, "{} must be 0 after rollup, got {}", name, value);
    }
}

/// Two consecutive `/metrics` polls without activity must return identical billing values.
#[tokio::test]
async fn billing_usage_gauges_are_stable_across_consecutive_polls() {
    let tmp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

    let counters = crate::usage_middleware::TenantUsageCounters::new();
    counters
        .search_count
        .store(7, std::sync::atomic::Ordering::Relaxed);
    counters
        .write_count
        .store(3, std::sync::atomic::Ordering::Relaxed);
    state
        .usage_counters
        .insert("stable_idx".to_string(), counters);

    let app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(state);

    let text1 = poll_metrics(&app).await;
    let text2 = poll_metrics(&app).await;

    for name in BILLING_METRIC_NAMES {
        let value_first = find_metric_value(&text1, name, "stable_idx");
        let value_second = find_metric_value(&text2, name, "stable_idx");
        assert_eq!(
            value_first, value_second,
            "{} changed between polls: {} vs {}",
            name, value_first, value_second
        );
    }
}

/// Verify register_billing_usage_gauges registers exactly 7 metric families.
#[test]
fn register_billing_usage_gauges_populates_seven_series() {
    let registry = Registry::new();
    let usage_counters = dashmap::DashMap::new();
    let counters = crate::usage_middleware::TenantUsageCounters::new();
    counters
        .search_count
        .store(1, std::sync::atomic::Ordering::Relaxed);
    usage_counters.insert("extract_idx".to_string(), counters);

    register_billing_usage_gauges(&registry, &usage_counters);

    let families = registry.gather();
    let family_names: Vec<&str> = families.iter().map(|family| family.get_name()).collect();
    assert_eq!(
        family_names.len(),
        7,
        "billing usage should register exactly 7 metric families, got: {:?}",
        family_names
    );
    for name in BILLING_METRIC_NAMES {
        assert!(
            family_names.contains(&name),
            "missing billing metric family: {}",
            name
        );
    }
}
