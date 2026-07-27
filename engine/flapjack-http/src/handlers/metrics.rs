use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use prometheus::{Encoder, GaugeVec, Opts, Registry, TextEncoder};
use std::sync::Arc;

use super::AppState;

/// GET /metrics — returns Prometheus text exposition format.
///
/// Gauges are populated on each request from live AppState / IndexManager /
/// MemoryObserver values. Per-tenant storage gauges are updated by a background
/// poller (see `server.rs`) and stored in `MetricsState`.
pub async fn metrics_handler(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let registry = Registry::new();
    let state = state.as_ref();

    register_system_runtime_gauges(&registry, state);
    register_replication_gauges(&registry, state);
    register_live_index_state_gauges(&registry, state);
    register_analytics_gauges(&registry);

    // --- Per-index billing usage gauges (daily-reset, from usage_counters) ---
    register_billing_usage_gauges(&registry, &state.usage_counters);

    encode_registry_response(&registry)
}

/// Registers Prometheus gauges for active writers, memory budget, and concurrent readers.
fn register_system_runtime_gauges(registry: &Registry, state: &AppState) {
    let budget = flapjack::get_global_budget();
    register_gauge(
        registry,
        "flapjack_active_writers",
        "Number of active index writers",
        budget.active_writers() as f64,
    );
    register_gauge(
        registry,
        "flapjack_max_concurrent_writers",
        "Maximum concurrent writers allowed",
        budget.max_concurrent_writers() as f64,
    );

    let observer = flapjack::MemoryObserver::global();
    let mem_stats = observer.stats();
    register_gauge(
        registry,
        "flapjack_memory_heap_bytes",
        "Heap allocated bytes",
        mem_stats.heap_allocated_bytes as f64,
    );
    register_gauge(
        registry,
        "flapjack_memory_limit_bytes",
        "System memory limit bytes",
        mem_stats.system_limit_bytes as f64,
    );
    let pressure_level: f64 = match mem_stats.pressure_level {
        flapjack::PressureLevel::Normal => 0.0,
        flapjack::PressureLevel::Elevated => 1.0,
        flapjack::PressureLevel::Critical => 2.0,
    };
    register_gauge(
        registry,
        "flapjack_memory_pressure_level",
        "Memory pressure level (0=normal, 1=elevated, 2=critical)",
        pressure_level,
    );

    register_gauge(
        registry,
        "flapjack_facet_cache_entries",
        "Number of entries in the facet cache",
        state.manager.facet_cache.len() as f64,
    );
    register_gauge(
        registry,
        "flapjack_tenants_loaded",
        "Number of loaded tenant indexes",
        state.manager.loaded_count() as f64,
    );
}

/// Registers Prometheus gauges for replication state (enabled, peer count, circuit breaker).
fn register_replication_gauges(registry: &Registry, state: &AppState) {
    register_gauge(
        registry,
        "flapjack_replication_enabled",
        "Whether replication is enabled (1=yes, 0=no)",
        bool_as_gauge_value(state.replication_manager.is_some()),
    );

    let Some(repl_mgr) = &state.replication_manager else {
        return;
    };

    let peer_gauge = GaugeVec::new(
        Opts::new(
            "flapjack_peer_status",
            "Peer health status (1=healthy, 0=unhealthy)",
        ),
        &["peer_id"],
    )
    .unwrap();
    registry.register(Box::new(peer_gauge.clone())).unwrap();
    for peer_status in repl_mgr.peer_statuses() {
        peer_gauge
            .with_label_values(&[&peer_status.peer_id])
            .set(bool_as_gauge_value(peer_status.status == "healthy"));
    }
}

fn register_live_index_state_gauges(registry: &Registry, state: &AppState) {
    // `/metrics` is an authenticated admin endpoint; keep one stable per-index
    // contract across test and runtime builds so dashboard parsing behavior
    // matches production.
    register_storage_bytes_gauge(registry, state);
    register_documents_count_gauge(registry, state);
    register_oplog_sequence_gauge(registry, state);
}

fn register_analytics_gauges(registry: &Registry) {
    let snapshot = flapjack::analytics::get_global_collector()
        .map(|collector| collector.analytics_metrics_snapshot())
        .unwrap_or(flapjack::analytics::collector::AnalyticsMetricsSnapshot {
            accepted_events_total: 0,
            dropped_events_total: 0,
            flush_latency_p99_ms: 0.0,
            rollup_windows_generated_total: 0,
            rollup_events_generated_total: 0,
            rollup_latest_nonempty_window_end_ms: 0,
            soak_marker_first_event_timestamp_ms: 0,
            rollup_generation_latency_p99_ms: 0.0,
        });

    register_gauge(
        registry,
        "flapjack_analytics_events_accepted_total",
        "Total analytics events accepted by the collector",
        snapshot.accepted_events_total as f64,
    );
    register_gauge(
        registry,
        "flapjack_analytics_events_dropped_total",
        "Total analytics events dropped due to flush errors",
        snapshot.dropped_events_total as f64,
    );
    register_gauge(
        registry,
        "flapjack_analytics_flush_latency_ms_p99",
        "P99 analytics flush latency in milliseconds",
        snapshot.flush_latency_p99_ms,
    );
    register_gauge(
        registry,
        "flapjack_analytics_rollup_windows_generated_total",
        "Total rollup windows generated by the running server",
        snapshot.rollup_windows_generated_total as f64,
    );
    register_gauge(
        registry,
        "flapjack_analytics_rollup_events_generated_total",
        "Total events included in generated rollup windows by the running server",
        snapshot.rollup_events_generated_total as f64,
    );
    register_gauge(
        registry,
        "flapjack_analytics_rollup_generation_latency_ms_p99",
        "P99 analytics rollup generation latency in milliseconds",
        snapshot.rollup_generation_latency_p99_ms,
    );
    register_gauge(
        registry,
        "flapjack_analytics_rollup_latest_nonempty_window_end_ms",
        "Latest non-empty rollup window end timestamp in epoch milliseconds",
        snapshot.rollup_latest_nonempty_window_end_ms as f64,
    );
    register_gauge(
        registry,
        "flapjack_analytics_soak_marker_first_event_timestamp_ms",
        "First server-observed soak marker event timestamp in epoch milliseconds",
        snapshot.soak_marker_first_event_timestamp_ms as f64,
    );
}

fn register_storage_bytes_gauge(registry: &Registry, state: &AppState) {
    let values = collect_storage_gauge_values(state);
    register_index_labeled_gauge_values(
        registry,
        "flapjack_storage_bytes",
        "Per-tenant disk storage in bytes",
        values,
    );
}

#[cfg(test)]
fn register_public_storage_bytes_gauge(registry: &Registry, state: &AppState) {
    register_gauge(
        registry,
        "flapjack_storage_bytes",
        "Aggregate disk storage in bytes across all tenant indexes",
        collect_storage_gauge_values(state)
            .into_iter()
            .map(|(_, value)| value)
            .sum(),
    );
}

/// Registers per-tenant Prometheus gauges for document counts.
fn register_documents_count_gauge(registry: &Registry, state: &AppState) {
    let values = state
        .manager
        .loaded_tenant_ids()
        .into_iter()
        .filter_map(|tenant_id| {
            state
                .manager
                .tenant_doc_count(&tenant_id)
                .map(|document_count| (tenant_id, document_count as f64))
        });
    register_index_labeled_gauge_values(
        registry,
        "flapjack_documents_count",
        "Number of documents per tenant index",
        values,
    );
}

#[cfg(test)]
fn register_public_documents_count_gauge(registry: &Registry, state: &AppState) {
    let total_documents = state
        .manager
        .loaded_tenant_ids()
        .into_iter()
        .filter_map(|tenant_id| state.manager.tenant_doc_count(&tenant_id))
        .sum::<u64>() as f64;
    register_gauge(
        registry,
        "flapjack_documents_count",
        "Aggregate document count across all tenant indexes",
        total_documents,
    );
}

fn register_oplog_sequence_gauge(registry: &Registry, state: &AppState) {
    let values = state
        .manager
        .all_tenant_oplog_seqs()
        .into_iter()
        .map(|(tenant_id, sequence)| (tenant_id, sequence as f64));
    register_index_labeled_gauge_values(
        registry,
        "flapjack_oplog_current_seq",
        "Current oplog sequence number per tenant",
        values,
    );
}

#[cfg(test)]
fn register_public_oplog_sequence_gauge(registry: &Registry, state: &AppState) {
    let max_sequence = state
        .manager
        .all_tenant_oplog_seqs()
        .into_iter()
        .map(|(_, sequence)| sequence)
        .max()
        .unwrap_or(0) as f64;
    register_gauge(
        registry,
        "flapjack_oplog_current_seq",
        "Highest current oplog sequence number across all tenant indexes",
        max_sequence,
    );
}

/// Encodes the Prometheus registry into a text-format HTTP response.
fn encode_registry_response(registry: &Registry) -> Response {
    let encoder = TextEncoder::new();
    let mut metric_families = registry.gather();
    metric_families.extend(crate::latency_middleware::gather_latency_metric_families());
    metric_families
        .extend(flapjack::index::write_queue::gather_write_queue_phase_metric_families());
    metric_families.extend(flapjack::query::executor::gather_query_phase_metric_families());
    let mut buffer = Vec::new();
    if let Err(e) = encoder.encode(&metric_families, &mut buffer) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("metrics encode error: {}", e),
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        buffer,
    )
        .into_response()
}

fn register_gauge(registry: &Registry, name: &str, help: &str, value: f64) {
    let gauge = prometheus::Gauge::new(name, help).unwrap();
    registry.register(Box::new(gauge.clone())).unwrap();
    gauge.set(value);
}

fn bool_as_gauge_value(value: bool) -> f64 {
    if value {
        1.0
    } else {
        0.0
    }
}

/// Collects per-index storage-size gauge values from the metrics poller.
fn collect_storage_gauge_values(state: &AppState) -> Vec<(String, f64)> {
    let mut merged_values: std::collections::BTreeMap<String, f64> = state
        .manager
        .all_tenant_storage()
        .into_iter()
        .map(|(tenant_id, bytes)| (tenant_id, bytes as f64))
        .collect();

    if let Some(metrics_state) = state.metrics_state.as_ref() {
        // Prefer poller-owned values when present, but keep manager-derived entries
        // so newly-created indexes are never omitted between poller cycles.
        for entry in metrics_state.storage_gauges.iter() {
            merged_values.insert(entry.key().clone(), *entry.value() as f64);
        }
    }

    merged_values.into_iter().collect()
}

fn register_index_labeled_gauge_values<I>(registry: &Registry, name: &str, help: &str, values: I)
where
    I: IntoIterator<Item = (String, f64)>,
{
    let gauge = GaugeVec::new(Opts::new(name, help), &["index"]).unwrap();
    registry.register(Box::new(gauge.clone())).unwrap();
    for (index, value) in values {
        gauge.with_label_values(&[index.as_str()]).set(value);
    }
}

/// Register the 7 per-index billing usage gauges from daily-reset `usage_counters`.
///
/// These series form the fjcloud metering contract: daily-scoped gauges that reset to
/// zero on `UsagePersistence::rollup()` and may decrease across process restarts.
/// They must NOT be relabeled as Prometheus counters without first introducing a
/// separate cumulative persistence source (see stage 3 research contract).
fn register_billing_usage_gauges(
    registry: &Registry,
    usage_counters: &dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters>,
) {
    use std::sync::atomic::Ordering;

    type CounterAccessor = fn(&crate::usage_middleware::TenantUsageCounters) -> u64;
    let gauge_defs: &[(&str, &str, CounterAccessor)] = &[
        (
            "flapjack_search_requests_total",
            "Total search requests per index",
            |c| c.search_count.load(Ordering::Relaxed),
        ),
        (
            "flapjack_write_operations_total",
            "Total write operations per index",
            |c| c.write_count.load(Ordering::Relaxed),
        ),
        (
            "flapjack_read_requests_total",
            "Total read requests per index",
            |c| c.read_count.load(Ordering::Relaxed),
        ),
        (
            "flapjack_bytes_in_total",
            "Total bytes ingested per index",
            |c| c.bytes_in.load(Ordering::Relaxed),
        ),
        (
            "flapjack_search_results_total",
            "Total search results returned per index",
            |c| c.search_results_total.load(Ordering::Relaxed),
        ),
        (
            "flapjack_documents_indexed_total",
            "Total documents indexed per index",
            |c| c.documents_indexed_total.load(Ordering::Relaxed),
        ),
        (
            "flapjack_documents_deleted_total",
            "Total documents deleted per index",
            |c| c.documents_deleted_total.load(Ordering::Relaxed),
        ),
    ];

    for &(name, help, accessor) in gauge_defs {
        let values: Vec<(String, f64)> = usage_counters
            .iter()
            .map(|entry| (entry.key().clone(), accessor(entry.value()) as f64))
            .collect();
        register_index_labeled_gauge_values(registry, name, help, values);
    }
}

/// Shared state for metrics updated by background tasks.
///
/// The storage background poller writes per-tenant byte counts here;
/// the `/metrics` handler reads them.
#[derive(Clone)]
pub struct MetricsState {
    pub storage_gauges: Arc<dashmap::DashMap<String, u64>>,
}

impl MetricsState {
    pub fn new() -> Self {
        MetricsState {
            storage_gauges: Arc::new(dashmap::DashMap::new()),
        }
    }
}

impl Default for MetricsState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use tempfile::TempDir;
    use tower::ServiceExt;

    /// Parse a labeled metric value from Prometheus text output.
    fn find_metric_value(text: &str, metric_name: &str, label_value: &str) -> f64 {
        text.lines()
            .find(|l| l.contains(metric_name) && l.contains(label_value) && !l.starts_with('#'))
            .unwrap_or_else(|| {
                panic!(
                    "metric {}{{..={}}} not found in:\n{}",
                    metric_name, label_value, text
                )
            })
            .split_whitespace()
            .last()
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Extract the value of an unlabeled gauge by matching the same contract
    /// the analytics-soak probe uses: a non-comment line whose first
    /// whitespace-separated token equals `metric_name`. The shell probe at
    /// engine/loadtest/soak_proof.sh::metric_value runs the equivalent
    /// `awk '$1 == metric_name { print $2 }'`, so this helper deliberately
    /// rejects anything that only appears in the `# HELP` / `# TYPE` comment
    /// lines — that asymmetry is exactly the smoke-test gap the L2 RF-2
    /// follow-up flagged. Panics if no value line exists or the value is
    /// not parseable as f64.
    fn extract_unlabeled_metric_value(text: &str, metric_name: &str) -> f64 {
        let line = text
            .lines()
            .find(|l| !l.starts_with('#') && l.split_whitespace().next() == Some(metric_name))
            .unwrap_or_else(|| {
                panic!("no unlabeled value line for {metric_name} in /metrics output:\n{text}")
            });
        line.split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("metric line missing value field: {line}"))
            .parse()
            .unwrap_or_else(|e| {
                panic!("metric value for {metric_name} not parseable as f64: {line} ({e})")
            })
    }

    /// Send a GET /metrics request and return the response body text.
    async fn fetch_metrics_text(state: std::sync::Arc<crate::handlers::AppState>) -> String {
        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .with_state(state);
        let response = app
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

    /// Verify the `/metrics` endpoint returns HTTP 200 with `text/plain` content type and all expected system-wide gauge names present in the body.
    #[tokio::test]
    async fn metrics_returns_200_with_prometheus_format() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        let app = Router::new()
            .route("/metrics", get(metrics_handler))
            .with_state(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let content_type = response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap();
        assert!(
            content_type.contains("text/plain"),
            "should be text/plain, got: {}",
            content_type
        );

        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();

        // Check key gauges are present
        assert!(
            text.contains("flapjack_active_writers"),
            "missing flapjack_active_writers"
        );
        assert!(
            text.contains("flapjack_max_concurrent_writers"),
            "missing flapjack_max_concurrent_writers"
        );
        assert!(
            text.contains("flapjack_memory_heap_bytes"),
            "missing flapjack_memory_heap_bytes"
        );
        assert!(
            text.contains("flapjack_memory_limit_bytes"),
            "missing flapjack_memory_limit_bytes"
        );
        assert!(
            text.contains("flapjack_memory_pressure_level"),
            "missing flapjack_memory_pressure_level"
        );
        assert!(
            text.contains("flapjack_facet_cache_entries"),
            "missing flapjack_facet_cache_entries"
        );
        assert!(
            text.contains("flapjack_tenants_loaded"),
            "missing flapjack_tenants_loaded"
        );
        assert!(
            text.contains("flapjack_replication_enabled"),
            "missing flapjack_replication_enabled"
        );
        // Analytics gauges: assert value-line presence the same way the
        // analytics-soak probe does (awk '$1 == name { print $2 }'). The
        // previous text.contains() assertions matched the `# HELP` / `# TYPE`
        // comments and would silently pass a registration regression that
        // dropped the value line, leaving the probe to fail in production.
        // Expected value is 0.0 because no events/rollups have been observed
        // by this freshly-built AppState.
        for analytics_metric in [
            "flapjack_analytics_events_accepted_total",
            "flapjack_analytics_events_dropped_total",
            "flapjack_analytics_flush_latency_ms_p99",
            "flapjack_analytics_rollup_windows_generated_total",
            "flapjack_analytics_rollup_events_generated_total",
            "flapjack_analytics_rollup_generation_latency_ms_p99",
            "flapjack_analytics_rollup_latest_nonempty_window_end_ms",
            "flapjack_analytics_soak_marker_first_event_timestamp_ms",
        ] {
            assert_eq!(
                extract_unlabeled_metric_value(&text, analytics_metric),
                0.0,
                "analytics gauge {analytics_metric} should be present as an unlabeled value line equal to 0.0 on a fresh server"
            );
        }

        for phase in [
            "process_writes",
            "flush_pending_batch",
            "commit_batch",
            "commit_writer_with_panic_guard",
            "finalize_committed_batch",
        ] {
            assert!(
                text.lines().any(|line| {
                    line.starts_with("flapjack_write_queue_phase_seconds_count")
                        && line.contains(&format!("phase=\"{phase}\""))
                }),
                "missing flapjack_write_queue_phase_seconds_count phase series for {phase}\n{text}"
            );
        }

        for phase in [
            "prepare",
            "collect",
            "rank",
            "fetch",
            "facet_extract",
            "unattributed",
        ] {
            for execution_path in [
                "relevance",
                "relevance_facets",
                "sort_fast",
                "sort_fallback",
                "count_only",
            ] {
                assert!(
                    text.lines().any(|line| {
                        line.starts_with("flapjack_query_phase_seconds_count")
                            && line.contains(&format!("phase=\"{phase}\""))
                            && line.contains(&format!("execution_path=\"{execution_path}\""))
                    }),
                    "missing query phase series for {phase}/{execution_path}\n{text}"
                );
            }
        }
    }

    #[tokio::test]
    async fn metrics_reflects_actual_tenant_count() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        // Create two tenants
        state.manager.create_tenant("idx1").unwrap();
        state.manager.create_tenant("idx2").unwrap();

        let text = fetch_metrics_text(state).await;

        // Find the flapjack_tenants_loaded line and verify value is 2
        let line = text
            .lines()
            .find(|l| l.starts_with("flapjack_tenants_loaded "))
            .unwrap();
        assert!(
            line.ends_with(" 2"),
            "tenants_loaded should be 2, got: {}",
            line
        );
    }

    #[tokio::test]
    async fn metrics_shows_storage_gauges_after_poller_update() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        // Create a tenant so it has some storage
        state.manager.create_tenant("store1").unwrap();

        // Simulate the background poller by publishing a known storage snapshot.
        let ms = state.metrics_state.as_ref().unwrap();
        ms.storage_gauges.clear();
        ms.storage_gauges.insert("store1".to_string(), 1234);

        let text = fetch_metrics_text(state).await;

        // The storage gauge should appear with the tenant label
        assert!(
            text.contains("flapjack_storage_bytes"),
            "should contain flapjack_storage_bytes gauge"
        );
        assert!(
            text.contains("store1"),
            "should contain tenant label 'store1'"
        );

        // Verify /metrics reads the poller-owned storage snapshot.
        let line = text
            .lines()
            .find(|l| l.contains("store1") && l.contains("flapjack_storage_bytes"))
            .unwrap();
        let value: f64 = line.split_whitespace().last().unwrap().parse().unwrap();
        assert_eq!(value, 1234.0, "storage bytes should come from MetricsState");
    }

    #[tokio::test]
    async fn metrics_includes_per_index_usage_counters() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        // Simulate some usage counter data
        {
            let counters = crate::usage_middleware::TenantUsageCounters::new();
            counters
                .search_count
                .store(5, std::sync::atomic::Ordering::Relaxed);
            counters
                .write_count
                .store(3, std::sync::atomic::Ordering::Relaxed);
            counters
                .read_count
                .store(2, std::sync::atomic::Ordering::Relaxed);
            counters
                .bytes_in
                .store(1024, std::sync::atomic::Ordering::Relaxed);
            counters
                .search_results_total
                .store(42, std::sync::atomic::Ordering::Relaxed);
            counters
                .documents_indexed_total
                .store(10, std::sync::atomic::Ordering::Relaxed);
            counters
                .documents_deleted_total
                .store(1, std::sync::atomic::Ordering::Relaxed);
            state
                .usage_counters
                .insert("test_index".to_string(), counters);
        }

        let text = fetch_metrics_text(state).await;

        // Verify all 7 per-index counter gauges appear with correct values
        assert_eq!(
            find_metric_value(&text, "flapjack_search_requests_total", "test_index"),
            5.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_write_operations_total", "test_index"),
            3.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_read_requests_total", "test_index"),
            2.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_bytes_in_total", "test_index"),
            1024.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_search_results_total", "test_index"),
            42.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_documents_indexed_total", "test_index"),
            10.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_documents_deleted_total", "test_index"),
            1.0
        );
    }

    #[tokio::test]
    async fn metrics_counter_values_match_known_operations() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        // Simulate two indexes with different counter values
        {
            let c1 = crate::usage_middleware::TenantUsageCounters::new();
            c1.search_count
                .store(10, std::sync::atomic::Ordering::Relaxed);
            c1.documents_indexed_total
                .store(100, std::sync::atomic::Ordering::Relaxed);
            state.usage_counters.insert("idx_a".to_string(), c1);

            let c2 = crate::usage_middleware::TenantUsageCounters::new();
            c2.write_count
                .store(7, std::sync::atomic::Ordering::Relaxed);
            c2.documents_deleted_total
                .store(3, std::sync::atomic::Ordering::Relaxed);
            state.usage_counters.insert("idx_b".to_string(), c2);
        }

        let text = fetch_metrics_text(state).await;

        // idx_a counters
        assert!(text.contains("idx_a"), "should contain idx_a label");
        assert!(text.contains("idx_b"), "should contain idx_b label");

        // Verify specific values per index
        assert_eq!(
            find_metric_value(&text, "flapjack_search_requests_total", "idx_a"),
            10.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_documents_indexed_total", "idx_a"),
            100.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_write_operations_total", "idx_b"),
            7.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_documents_deleted_total", "idx_b"),
            3.0
        );
        // idx_a should have 0 writes, idx_b should have 0 searches
        assert_eq!(
            find_metric_value(&text, "flapjack_write_operations_total", "idx_a"),
            0.0
        );
        assert_eq!(
            find_metric_value(&text, "flapjack_search_requests_total", "idx_b"),
            0.0
        );
    }

    #[tokio::test]
    async fn metrics_includes_documents_count_gauge() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        state.manager.create_tenant("docs_idx").unwrap();
        let docs = vec![
            flapjack::types::Document {
                id: "d1".to_string(),
                fields: std::collections::HashMap::from([(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("Alice".to_string()),
                )]),
            },
            flapjack::types::Document {
                id: "d2".to_string(),
                fields: std::collections::HashMap::from([(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("Bob".to_string()),
                )]),
            },
        ];
        state
            .manager
            .add_documents_sync("docs_idx", docs)
            .await
            .unwrap();

        let text = fetch_metrics_text(state).await;

        let line = text
            .lines()
            .find(|l| {
                l.contains("flapjack_documents_count")
                    && l.contains("docs_idx")
                    && !l.starts_with('#')
            })
            .unwrap_or_else(|| {
                panic!(
                    "flapjack_documents_count for docs_idx not found in:\n{}",
                    text
                )
            });
        let value: f64 = line.split_whitespace().last().unwrap().parse().unwrap();
        assert_eq!(value, 2.0, "should have 2 docs in the gauge");
    }

    #[tokio::test]
    async fn metrics_includes_oplog_current_seq_gauge() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        state.manager.create_tenant("oplog_idx").unwrap();
        let docs = vec![flapjack::types::Document {
            id: "d1".to_string(),
            fields: std::collections::HashMap::from([(
                "name".to_string(),
                flapjack::types::FieldValue::Text("Alice".to_string()),
            )]),
        }];
        state
            .manager
            .add_documents_sync("oplog_idx", docs)
            .await
            .unwrap();

        let text = fetch_metrics_text(state).await;

        let line = text
            .lines()
            .find(|l| {
                l.contains("flapjack_oplog_current_seq")
                    && l.contains("oplog_idx")
                    && !l.starts_with('#')
            })
            .unwrap_or_else(|| {
                panic!(
                    "flapjack_oplog_current_seq for oplog_idx not found in:\n{}",
                    text
                )
            });
        let value: f64 = line.split_whitespace().last().unwrap().parse().unwrap();
        assert!(
            value > 0.0,
            "oplog seq should be > 0 after a write, got: {}",
            value
        );
    }

    #[test]
    fn public_billing_usage_gauges_aggregate_without_index_labels() {
        let registry = Registry::new();
        let usage_counters = dashmap::DashMap::new();

        let idx_a = crate::usage_middleware::TenantUsageCounters::new();
        idx_a
            .search_count
            .store(7, std::sync::atomic::Ordering::Relaxed);
        usage_counters.insert("idx_a".to_string(), idx_a);

        let idx_b = crate::usage_middleware::TenantUsageCounters::new();
        idx_b
            .search_count
            .store(3, std::sync::atomic::Ordering::Relaxed);
        usage_counters.insert("idx_b".to_string(), idx_b);

        for family in register_public_billing_usage_metric_families(&usage_counters) {
            registry.register(Box::new(family)).unwrap();
        }

        let text = encode_registry_as_text(&registry);
        assert_eq!(
            extract_unlabeled_metric_value(&text, "flapjack_search_requests_total"),
            10.0
        );
        assert!(
            !text.contains("idx_a") && !text.contains("idx_b"),
            "public billing gauges must not expose tenant labels:\n{text}"
        );
    }

    #[tokio::test]
    async fn public_live_index_gauges_aggregate_without_tenant_labels() {
        let tmp = TempDir::new().unwrap();
        let state = crate::test_helpers::TestStateBuilder::new(&tmp).build_shared();

        state.manager.create_tenant("public_a").unwrap();
        state.manager.create_tenant("public_b").unwrap();
        state.metrics_state.as_ref().unwrap().storage_gauges.clear();
        state
            .metrics_state
            .as_ref()
            .unwrap()
            .storage_gauges
            .insert("public_a".to_string(), 100);
        state
            .metrics_state
            .as_ref()
            .unwrap()
            .storage_gauges
            .insert("public_b".to_string(), 50);
        state
            .manager
            .add_documents_sync(
                "public_a",
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

        let registry = Registry::new();
        register_public_storage_bytes_gauge(&registry, &state);
        register_public_documents_count_gauge(&registry, &state);
        register_public_oplog_sequence_gauge(&registry, &state);

        let text = encode_registry_as_text(&registry);
        assert_eq!(
            extract_unlabeled_metric_value(&text, "flapjack_storage_bytes"),
            150.0
        );
        assert_eq!(
            extract_unlabeled_metric_value(&text, "flapjack_documents_count"),
            1.0
        );
        assert!(
            !text.contains("public_a") && !text.contains("public_b"),
            "public live-index gauges must not expose tenant labels:\n{text}"
        );
    }

    fn register_public_billing_usage_metric_families(
        usage_counters: &dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters>,
    ) -> Vec<prometheus::Gauge> {
        use std::sync::atomic::Ordering;

        let metric_defs = [
            (
                "flapjack_search_requests_total",
                "Total search requests across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| entry.value().search_count.load(Ordering::Relaxed) as f64)
                    .sum(),
            ),
            (
                "flapjack_write_operations_total",
                "Total write operations across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| entry.value().write_count.load(Ordering::Relaxed) as f64)
                    .sum(),
            ),
            (
                "flapjack_read_requests_total",
                "Total read requests across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| entry.value().read_count.load(Ordering::Relaxed) as f64)
                    .sum(),
            ),
            (
                "flapjack_bytes_in_total",
                "Total bytes ingested across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| entry.value().bytes_in.load(Ordering::Relaxed) as f64)
                    .sum(),
            ),
            (
                "flapjack_search_results_total",
                "Total search results returned across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| entry.value().search_results_total.load(Ordering::Relaxed) as f64)
                    .sum(),
            ),
            (
                "flapjack_documents_indexed_total",
                "Total documents indexed across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| {
                        entry
                            .value()
                            .documents_indexed_total
                            .load(Ordering::Relaxed) as f64
                    })
                    .sum(),
            ),
            (
                "flapjack_documents_deleted_total",
                "Total documents deleted across all indexes",
                usage_counters
                    .iter()
                    .map(|entry| {
                        entry
                            .value()
                            .documents_deleted_total
                            .load(Ordering::Relaxed) as f64
                    })
                    .sum(),
            ),
        ];

        metric_defs
            .into_iter()
            .map(|(name, help, value)| {
                let gauge = prometheus::Gauge::new(name, help).unwrap();
                gauge.set(value);
                gauge
            })
            .collect()
    }

    fn encode_registry_as_text(registry: &Registry) -> String {
        let mut buffer = Vec::new();
        TextEncoder::new()
            .encode(&registry.gather(), &mut buffer)
            .unwrap();
        String::from_utf8(buffer).unwrap()
    }
}

#[cfg(test)]
#[path = "metrics_billing_contract_tests.rs"]
mod metrics_billing_contract_tests;

#[cfg(test)]
#[path = "metrics_live_index_state_tests.rs"]
mod metrics_live_index_state_tests;

#[cfg(test)]
#[path = "metrics_latency_tests.rs"]
mod metrics_latency_tests;
