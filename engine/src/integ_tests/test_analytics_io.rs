//! Integration tests for the analytics I/O stack covering the collector's buffering and flushing behavior, Parquet writer roundtrips through the query engine, and orphaned analytics directory cleanup.

use crate::analytics::collector::AnalyticsCollector;
use crate::analytics::config::AnalyticsConfig;
use crate::analytics::query::AnalyticsQueryEngine;
use crate::analytics::schema::{InsightEvent, SearchEvent};
use crate::analytics::writer;
use std::collections::HashSet;
use tempfile::TempDir;

// ─── Config helpers ───────────────────────────────────────────────────────────

fn collector_config(dir: &std::path::Path, flush_size: usize) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: dir.to_path_buf(),
        flush_interval_secs: 3600,
        flush_size,
        retention_days: 7,
    }
}

fn writer_config(dir: &std::path::Path) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: dir.to_path_buf(),
        flush_interval_secs: 1,
        flush_size: 100,
        retention_days: 7,
    }
}

/// Write a pre-minimization search fixture whose `user_ip` values remain exact.
///
/// This deliberately bypasses `writer::search_events_to_batch` only for the
/// final Parquet write. It is the sole test path permitted to create legacy
/// full-IP analytics files: all canonical columns come from the production
/// schema and `SearchEvent` conversion, then only `user_ip` is replaced.
pub(super) fn write_legacy_search_parquet(
    config: &AnalyticsConfig,
    index: &str,
    date: &str,
    user_ips: &[&str],
) -> std::path::PathBuf {
    use arrow::array::StringArray;
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let timestamp_ms = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let events: Vec<SearchEvent> = user_ips
        .iter()
        .enumerate()
        .map(|(row, user_ip)| SearchEvent {
            timestamp_ms,
            query: format!("legacy-{row}"),
            query_id: None,
            index_name: index.to_string(),
            nb_hits: 1,
            processing_time_ms: 1,
            user_token: None,
            user_ip: Some((*user_ip).to_string()),
            filters: None,
            facets: None,
            analytics_tags: None,
            page: 0,
            hits_per_page: 20,
            has_results: true,
            country: None,
            region: None,
            experiment_id: None,
            variant_id: None,
            assignment_method: None,
        })
        .collect();

    let schema = crate::analytics::schema::search_event_schema();
    let canonical_batch = writer::search_events_to_batch(&events, &schema).unwrap();
    let user_ip_index = schema.index_of("user_ip").unwrap();
    let mut columns = canonical_batch.columns().to_vec();
    columns[user_ip_index] = Arc::new(StringArray::from(user_ips.to_vec()));
    let legacy_batch = RecordBatch::try_new(schema.clone(), columns).unwrap();

    let partition_dir = config.searches_dir(index).join(format!("date={date}"));
    std::fs::create_dir_all(&partition_dir).unwrap();
    let parquet_path = partition_dir.join("searches_legacy_fixture.parquet");
    let file = std::fs::File::create(&parquet_path).unwrap();
    let mut parquet_writer = ArrowWriter::try_new(file, schema, None).unwrap();
    parquet_writer.write(&legacy_batch).unwrap();
    parquet_writer.close().unwrap();
    parquet_path
}

fn cleanup_config(dir: &std::path::Path) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: dir.to_path_buf(),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    }
}

// ─── Event builders ───────────────────────────────────────────────────────────

/// Build a `SearchEvent` with sensible defaults for collector tests.
///
/// # Arguments
///
/// * `query` - The search query string.
/// * `index` - The index name to associate the event with.
/// * `query_id` - Optional query ID; when `Some`, the collector caches a lookup entry.
fn make_search(query: &str, index: &str, query_id: Option<&str>) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: query_id.map(|s| s.to_string()),
        index_name: index.to_string(),
        nb_hits: 10,
        processing_time_ms: 5,
        user_token: Some("user1".to_string()),
        user_ip: Some("127.0.0.1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

/// Build a `SearchEvent` with a fixed 32-character query ID and configurable hit count.
///
/// Unlike `make_search`, this builder always populates `query_id` and derives
/// `has_results` from `nb_hits > 0`, making it suited for writer and query-engine
/// tests.
///
/// # Arguments
///
/// * `query` - The search query string.
/// * `index` - The index name.
/// * `nb_hits` - Number of hits; also determines `has_results`.
fn make_search_ev(query: &str, index: &str, nb_hits: u32) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: Some("a".repeat(32)),
        index_name: index.to_string(),
        nb_hits,
        processing_time_ms: 5,
        user_token: Some("user1".to_string()),
        user_ip: Some("192.168.1.1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: nb_hits > 0,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

/// Build a minimal `InsightEvent` with defaults suitable for collector buffer tests.
///
/// # Arguments
///
/// * `event_type` - The event type (e.g. `"click"`, `"conversion"`).
/// * `index` - The index name to associate the event with.
fn make_insight(event_type: &str, index: &str) -> InsightEvent {
    InsightEvent {
        event_type: event_type.to_string(),
        event_subtype: None,
        event_name: "Test".to_string(),
        index: index.to_string(),
        user_token: "user1".to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Build an `InsightEvent` with configurable event type, index, and query ID.
///
/// Automatically sets `positions` to `[3]` for click events that have a query ID,
/// mimicking a realistic click-after-search scenario.
///
/// # Arguments
///
/// * `event_type` - The event type (e.g. `"click"`, `"conversion"`, `"view"`).
/// * `index` - The index name to associate the event with.
/// * `query_id` - Optional query ID linking the insight to a prior search.
fn make_insight_ev(event_type: &str, index: &str, query_id: Option<&str>) -> InsightEvent {
    InsightEvent {
        event_type: event_type.to_string(),
        event_subtype: None,
        event_name: "Test Event".to_string(),
        index: index.to_string(),
        user_token: "user1".to_string(),
        authenticated_user_token: None,
        query_id: query_id.map(|s| s.to_string()),
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: if event_type == "click" && query_id.is_some() {
            Some(vec![3])
        } else {
            None
        },
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

// ─── Cleanup helper ───────────────────────────────────────────────────────────

/// Identify orphaned analytics directories and remove them.
///
/// Compares the set of analytics indices reported by the query engine against
/// subdirectories present in `active_index_dir`. Any analytics index without a
/// corresponding active directory is deleted from disk.
///
/// # Returns
///
/// A list of index names that were identified as orphaned and removed.
fn run_cleanup(engine: &AnalyticsQueryEngine, active_index_dir: &std::path::Path) -> Vec<String> {
    let analytics_indices = engine.list_analytics_indices().unwrap();
    let mut active_indices: HashSet<String> = HashSet::new();
    if active_index_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(active_index_dir) {
            for entry in entries.flatten() {
                if entry.path().is_dir() {
                    if let Some(name) = entry.file_name().to_str() {
                        active_indices.insert(name.to_string());
                    }
                }
            }
        }
    }
    let orphaned: Vec<String> = analytics_indices
        .into_iter()
        .filter(|name| !active_indices.contains(name))
        .collect();
    let config = engine.config();
    for index_name in &orphaned {
        let index_dir = config.data_dir.join(index_name);
        if index_dir.exists() {
            let _ = std::fs::remove_dir_all(&index_dir);
        }
    }
    orphaned
}

// ─── AnalyticsCollector tests ──────────────────────────────────────────────────

#[test]
fn record_search_stores_in_buffer() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 1000));
    collector.record_search(make_search("laptop", "products", None));
    collector.record_search(make_search("phone", "products", None));
    collector.flush_all();
    assert!(tmp.path().join("products").join("searches").exists());
}

#[test]
fn auto_flush_at_threshold() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 3));
    collector.record_search(make_search("a", "products", None));
    collector.record_search(make_search("b", "products", None));
    collector.record_search(make_search("c", "products", None));
    assert!(tmp.path().join("products").join("searches").exists());
}

/// Verify that an `AnalyticsCollector` with `enabled: false` silently discards all recorded events and writes nothing to disk.
#[test]
fn disabled_collector_does_not_write() {
    let tmp = TempDir::new().unwrap();
    let config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().to_path_buf(),
        flush_interval_secs: 3600,
        flush_size: 1,
        retention_days: 7,
    };
    let collector = AnalyticsCollector::new(config);
    collector.record_search(make_search("laptop", "products", None));
    collector.record_insight(make_insight("click", "products"));
    collector.flush_all();
    let entries: Vec<_> = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert_eq!(entries.len(), 0);
}

#[test]
fn query_id_cache_stores_and_retrieves() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 1000));
    let qid = "a".repeat(32);
    collector.record_search(make_search("laptop", "products", Some(&qid)));
    let entry = collector.lookup_query_id(&qid).unwrap();
    assert_eq!(entry.query, "laptop");
    assert_eq!(entry.index_name, "products");
}

#[test]
fn query_id_cache_returns_none_for_unknown() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 1000));
    assert!(collector.lookup_query_id("nonexistent").is_none());
}

#[test]
fn insight_events_flush_to_parquet() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 1000));
    collector.record_insight(make_insight("click", "products"));
    collector.record_insight(make_insight("conversion", "products"));
    collector.flush_all();
    assert!(tmp.path().join("products").join("events").exists());
}

#[test]
fn events_grouped_by_index() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 1000));
    collector.record_search(make_search("a", "index1", None));
    collector.record_search(make_search("b", "index2", None));
    collector.flush_all();
    assert!(tmp.path().join("index1").join("searches").exists());
    assert!(tmp.path().join("index2").join("searches").exists());
}

#[test]
fn flush_all_empties_both_buffers() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(collector_config(tmp.path(), 1000));
    collector.record_search(make_search("a", "products", None));
    collector.record_insight(make_insight("click", "products"));
    collector.flush_all();
    collector.flush_all(); // should not panic
}

// ─── Writer / AnalyticsQueryEngine tests ──────────────────────────────────────

/// Verify that `flush_search_events` produces exactly one Parquet file inside a date-partitioned directory.
#[test]
fn write_search_events_creates_parquet_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("searches");
    let events = vec![
        make_search_ev("laptop", "products", 42),
        make_search_ev("phone", "products", 18),
    ];
    writer::flush_search_events(&events, &dir).unwrap();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let partition_dir = dir.join(format!("date={}", today));
    assert!(partition_dir.exists());
    let parquet_count = std::fs::read_dir(&partition_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|x| x == "parquet")
                .unwrap_or(false)
        })
        .count();
    assert_eq!(parquet_count, 1);
}

#[test]
fn write_insight_events_creates_parquet_file() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("events");
    let events = vec![
        make_insight_ev("click", "products", Some(&"a".repeat(32))),
        make_insight_ev("conversion", "products", Some(&"b".repeat(32))),
        make_insight_ev("view", "products", None),
    ];
    writer::flush_insight_events(&events, &dir).unwrap();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    assert!(dir.join(format!("date={}", today)).exists());
}

#[test]
fn empty_events_is_noop() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("searches");
    writer::flush_search_events(&[], &dir).unwrap();
    assert!(!dir.exists());
}

/// Flush search events to Parquet, then read them back through `AnalyticsQueryEngine::top_searches` and verify aggregation results.
#[tokio::test]
async fn search_events_roundtrip_via_query_engine() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let events = vec![
        make_search_ev("laptop", "products", 42),
        make_search_ev("laptop", "products", 35),
        make_search_ev("phone", "products", 18),
        make_search_ev("nonexistent", "products", 0),
    ];
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&events, &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .top_searches(
            &crate::analytics::AnalyticsQueryParams {
                index_name: "products",
                start_date: &today,
                end_date: &today,
                limit: 10,
                tags: None,
            },
            false,
            None,
        )
        .await
        .unwrap();
    let searches = result["searches"].as_array().unwrap();
    assert_eq!(searches.len(), 3);
    assert_eq!(searches[0]["search"], "laptop");
    assert_eq!(searches[0]["count"], 2);
}

/// Verify that `search_count` returns the total count and a per-day breakdown array.
#[tokio::test]
async fn search_count_with_daily_breakdown() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let events = vec![
        make_search_ev("a", "products", 1),
        make_search_ev("b", "products", 2),
        make_search_ev("c", "products", 3),
    ];
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&events, &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .search_count("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["count"], 3);
    let dates = result["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert_eq!(dates[0]["count"], 3);
}

/// Verify that `no_results_rate` correctly computes the ratio of zero-hit searches to total searches.
#[tokio::test]
async fn no_results_rate_calculation() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let events = vec![
        make_search_ev("found", "products", 10),
        make_search_ev("found2", "products", 5),
        make_search_ev("notfound1", "products", 0),
        make_search_ev("notfound2", "products", 0),
    ];
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&events, &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .no_results_rate("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["rate"], 0.5);
    assert_eq!(result["count"], 4);
    assert_eq!(result["noResults"], 2);
}

/// Verify that `no_results_searches` returns only queries with zero hits, aggregated and ranked by frequency.
#[tokio::test]
async fn no_results_searches_returns_zero_hit_queries() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let events = vec![
        make_search_ev("found", "products", 10),
        make_search_ev("missing", "products", 0),
        make_search_ev("missing", "products", 0),
        make_search_ev("also_missing", "products", 0),
    ];
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&events, &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .no_results_searches("products", &today, &today, 10)
        .await
        .unwrap();
    let searches = result["searches"].as_array().unwrap();
    assert_eq!(searches.len(), 2);
    assert_eq!(searches[0]["search"], "missing");
    assert_eq!(searches[0]["count"], 2);
}

/// Verify that `users_count` deduplicates user tokens, counting each unique user exactly once.
#[tokio::test]
async fn users_count_deduplicates() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let mut e1 = make_search_ev("a", "products", 1);
    e1.user_token = Some("alice".to_string());
    let mut e2 = make_search_ev("b", "products", 2);
    e2.user_token = Some("bob".to_string());
    let mut e3 = make_search_ev("c", "products", 3);
    e3.user_token = Some("alice".to_string());
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&[e1, e2, e3], &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .users_count("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["count"], 2);
}

#[tokio::test]
async fn users_count_reads_legacy_and_minimized_ip_partitions_together() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let legacy_date = "2025-06-15";
    let current_date = "2025-06-16";
    let current_timestamp_ms = chrono::NaiveDate::parse_from_str(current_date, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();

    write_legacy_search_parquet(
        &config,
        "products",
        legacy_date,
        &["203.0.113.47", "198.51.100.23"],
    );

    let engine = AnalyticsQueryEngine::new(config.clone());
    let legacy_only = engine
        .users_count("products", legacy_date, legacy_date)
        .await
        .unwrap();
    assert_eq!(legacy_only["count"], 2);
    assert_eq!(
        legacy_only["dates"],
        serde_json::json!([{"date": legacy_date, "count": 2}])
    );

    let current_events = ["192.0.2.8", "203.0.114.99"].map(|user_ip| {
        let mut event = make_search_ev("current", "products", 1);
        event.timestamp_ms = current_timestamp_ms;
        event.user_token = None;
        event.user_ip = Some(user_ip.to_string());
        event
    });
    writer::flush_search_events(&current_events, &config.searches_dir("products")).unwrap();

    let combined = engine
        .users_count("products", legacy_date, current_date)
        .await
        .unwrap();
    assert_eq!(combined["count"], 4);
    assert_eq!(
        combined["dates"],
        serde_json::json!([
            {"date": legacy_date, "count": 2},
            {"date": current_date, "count": 2}
        ])
    );
}

#[tokio::test]
async fn query_empty_index_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .top_searches(
            &crate::analytics::AnalyticsQueryParams {
                index_name: "nonexistent",
                start_date: &today,
                end_date: &today,
                limit: 10,
                tags: None,
            },
            false,
            None,
        )
        .await
        .unwrap();
    assert_eq!(result["searches"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn status_reflects_config() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let engine = AnalyticsQueryEngine::new(config);
    let result = engine.status("products").await.unwrap();
    assert_eq!(result["enabled"], true);
    assert_eq!(result["hasData"], false);
    assert_eq!(result["retentionDays"], 7);
}

/// Verify that `top_filters` aggregates filter strings and returns them ranked by frequency.
#[tokio::test]
async fn filters_query_returns_filter_data() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let mut e1 = make_search_ev("laptop", "products", 10);
    e1.filters = Some("brand:Apple".to_string());
    let mut e2 = make_search_ev("phone", "products", 5);
    e2.filters = Some("brand:Samsung".to_string());
    let mut e3 = make_search_ev("tablet", "products", 3);
    e3.filters = Some("brand:Apple".to_string());
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&[e1, e2, e3], &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .top_filters("products", &today, &today, 10)
        .await
        .unwrap();
    let filters = result["filters"].as_array().unwrap();
    assert_eq!(filters.len(), 2);
    assert_eq!(filters[0]["attribute"], "brand:Apple");
    assert_eq!(filters[0]["count"], 2);
}

/// Verify that `filter_values` splits `attribute:value` filter strings and aggregates values for a given attribute.
#[tokio::test]
async fn filter_values_extracts_attribute_values() {
    let tmp = TempDir::new().unwrap();
    let config = writer_config(tmp.path());
    let mut e1 = make_search_ev("q1", "products", 10);
    e1.filters = Some("brand:Apple".to_string());
    let mut e2 = make_search_ev("q2", "products", 5);
    e2.filters = Some("brand:Samsung".to_string());
    let mut e3 = make_search_ev("q3", "products", 3);
    e3.filters = Some("brand:Apple".to_string());
    let searches_dir = config.searches_dir("products");
    writer::flush_search_events(&[e1, e2, e3], &searches_dir).unwrap();
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .filter_values("products", "brand", &today, &today, 10)
        .await
        .unwrap();
    assert_eq!(result["attribute"], "brand");
    let values = result["values"].as_array().unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["value"], "Apple");
    assert_eq!(values[0]["count"], 2);
}

// ─── Experiment fields writer roundtrip tests ──────────────────────────────────

/// Verify that experiment metadata (`experiment_id`, `variant_id`, `assignment_method`) survives a Parquet write-read roundtrip with non-null values.
#[test]
fn writer_roundtrip_with_experiment_fields() {
    use crate::analytics::schema::search_event_schema;
    use arrow::array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("searches");
    let event = SearchEvent {
        timestamp_ms: 1000,
        query: "laptop".to_string(),
        query_id: Some("a".repeat(32)),
        index_name: "products".to_string(),
        nb_hits: 42,
        processing_time_ms: 5,
        user_token: Some("user1".to_string()),
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: Some("exp-1".to_string()),
        variant_id: Some("variant".to_string()),
        assignment_method: Some("user_token".to_string()),
    };
    writer::flush_search_events(&[event], &dir).unwrap();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let partition_dir = dir.join(format!("date={}", today));
    let parquet_file = std::fs::read_dir(&partition_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .extension()
                .map(|x| x == "parquet")
                .unwrap_or(false)
        })
        .unwrap();
    let file = File::open(parquet_file.path()).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];

    let schema = search_event_schema();
    let exp_idx = schema.index_of("experiment_id").unwrap();
    let var_idx = schema.index_of("variant_id").unwrap();
    let method_idx = schema.index_of("assignment_method").unwrap();

    let exp_col = batch
        .column(exp_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert_eq!(exp_col.value(0), "exp-1");

    let var_col = batch
        .column(var_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert_eq!(var_col.value(0), "variant");

    let method_col = batch
        .column(method_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert_eq!(method_col.value(0), "user_token");
}

#[test]
fn analytics_persists_truncated_client_ip_only() {
    use crate::analytics::schema::search_event_schema;
    use arrow::array::{Array, StringArray};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;
    use std::path::Path;

    let read_search_fields = |dir: &Path| -> Vec<(String, String, String)> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        let partition_dir = dir.join(format!("date={}", today));
        let parquet_file = std::fs::read_dir(&partition_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| {
                e.path()
                    .extension()
                    .map(|x| x == "parquet")
                    .unwrap_or(false)
            })
            .unwrap();
        let file = File::open(parquet_file.path()).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
        assert_eq!(batches.len(), 1);

        let batch = &batches[0];
        let user_ip_idx = batch.schema().index_of("user_ip").unwrap();
        let country_idx = batch.schema().index_of("country").unwrap();
        let region_idx = batch.schema().index_of("region").unwrap();
        let user_ip_col = batch
            .column(user_ip_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let country_col = batch
            .column(country_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let region_col = batch
            .column(region_idx)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();

        assert_eq!(batch.num_rows(), 2);
        (0..batch.num_rows())
            .map(|row| {
                assert!(!user_ip_col.is_null(row));
                assert!(!country_col.is_null(row));
                assert!(!region_col.is_null(row));
                (
                    user_ip_col.value(row).to_string(),
                    country_col.value(row).to_string(),
                    region_col.value(row).to_string(),
                )
            })
            .collect()
    };

    let decode_batch_fields =
        |batch: &arrow::record_batch::RecordBatch| -> Vec<(String, String, String)> {
            let user_ip_idx = batch.schema().index_of("user_ip").unwrap();
            let country_idx = batch.schema().index_of("country").unwrap();
            let region_idx = batch.schema().index_of("region").unwrap();
            let user_ip_col = batch
                .column(user_ip_idx)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let country_col = batch
                .column(country_idx)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let region_col = batch
                .column(region_idx)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();

            assert_eq!(batch.num_rows(), 2);
            (0..batch.num_rows())
                .map(|row| {
                    assert!(!user_ip_col.is_null(row));
                    assert!(!country_col.is_null(row));
                    assert!(!region_col.is_null(row));
                    (
                        user_ip_col.value(row).to_string(),
                        country_col.value(row).to_string(),
                        region_col.value(row).to_string(),
                    )
                })
                .collect()
        };

    let expected = vec![
        (
            "203.0.113.0".to_string(),
            "US".to_string(),
            "CA".to_string(),
        ),
        (
            "2001:db8:1234::".to_string(),
            "DE".to_string(),
            "BE".to_string(),
        ),
    ];

    let ipv4_event = SearchEvent {
        timestamp_ms: 1000,
        query: "laptop".to_string(),
        query_id: Some("b".repeat(32)),
        index_name: "products".to_string(),
        nb_hits: 42,
        processing_time_ms: 5,
        user_token: None,
        user_ip: Some("203.0.113.47".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: Some("US".to_string()),
        region: Some("CA".to_string()),
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    };
    let ipv6_event = SearchEvent {
        timestamp_ms: 2000,
        query: "router".to_string(),
        query_id: Some("c".repeat(32)),
        index_name: "products".to_string(),
        nb_hits: 7,
        processing_time_ms: 6,
        user_token: None,
        user_ip: Some("2001:db8:1234:5678::1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: Some("DE".to_string()),
        region: Some("BE".to_string()),
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    };
    let events = vec![ipv4_event, ipv6_event];

    let writer_tmp = TempDir::new().unwrap();
    let writer_dir = writer_tmp.path().join("searches");
    writer::flush_search_events(&events, &writer_dir).unwrap();
    let direct_writer_values = read_search_fields(&writer_dir);

    let collector_tmp = TempDir::new().unwrap();
    let collector_config = collector_config(collector_tmp.path(), 100);
    let collector = AnalyticsCollector::new(collector_config.clone());
    for event in events.clone() {
        collector.record_search(event);
    }
    collector.flush_searches();
    let collector_values = read_search_fields(&collector_config.searches_dir("products"));

    let schema = search_event_schema();
    let batch = writer::search_events_to_batch(&events, &schema).unwrap();
    let chokepoint_values = decode_batch_fields(&batch);

    assert_eq!(direct_writer_values, expected);
    assert_eq!(collector_values, expected);
    assert_eq!(chokepoint_values, expected);
}

/// Verify that null experiment metadata columns are preserved as Parquet nulls through a write-read roundtrip.
#[test]
fn writer_roundtrip_with_null_experiment_fields() {
    use crate::analytics::schema::search_event_schema;
    use arrow::array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("searches");
    let event = SearchEvent {
        timestamp_ms: 1000,
        query: "laptop".to_string(),
        query_id: None,
        index_name: "products".to_string(),
        nb_hits: 10,
        processing_time_ms: 3,
        user_token: None,
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    };
    writer::flush_search_events(&[event], &dir).unwrap();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let partition_dir = dir.join(format!("date={}", today));
    let parquet_file = std::fs::read_dir(&partition_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .extension()
                .map(|x| x == "parquet")
                .unwrap_or(false)
        })
        .unwrap();
    let file = File::open(parquet_file.path()).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];

    let schema = search_event_schema();
    let exp_idx = schema.index_of("experiment_id").unwrap();
    let var_idx = schema.index_of("variant_id").unwrap();
    let method_idx = schema.index_of("assignment_method").unwrap();

    let exp_col = batch
        .column(exp_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert!(exp_col.is_null(0));

    let var_col = batch
        .column(var_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert!(var_col.is_null(0));

    let method_col = batch
        .column(method_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert!(method_col.is_null(0));
}

// ─── Cleanup tests ─────────────────────────────────────────────────────────────

/// Build a `SearchEvent` with a fixed user token and IP, used by cleanup tests.
///
/// # Arguments
///
/// * `query` - The search query string.
/// * `index` - The index name to associate the event with.
fn cleanup_search_event(query: &str, index: &str) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: None,
        index_name: index.to_string(),
        nb_hits: 10,
        processing_time_ms: 5,
        user_token: Some("testuser".to_string()),
        user_ip: Some("10.0.0.1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

#[tokio::test]
async fn cleanup_no_orphans() {
    let analytics_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = cleanup_config(analytics_dir.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    collector.record_search(cleanup_search_event("laptop", "products"));
    collector.record_search(cleanup_search_event("action", "movies"));
    collector.flush_all();
    std::fs::create_dir_all(index_dir.path().join("products")).unwrap();
    std::fs::create_dir_all(index_dir.path().join("movies")).unwrap();
    let removed = run_cleanup(&engine, index_dir.path());
    assert!(removed.is_empty());
}

#[tokio::test]
async fn cleanup_all_orphaned() {
    let analytics_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = cleanup_config(analytics_dir.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    collector.record_search(cleanup_search_event("laptop", "products"));
    collector.record_search(cleanup_search_event("action", "movies"));
    collector.flush_all();
    let removed = run_cleanup(&engine, index_dir.path());
    assert_eq!(removed.len(), 2);
    assert!(!analytics_dir.path().join("products").exists());
    assert!(!analytics_dir.path().join("movies").exists());
}

/// Verify that cleanup removes only the orphaned analytics directory while preserving directories that match active indices.
#[tokio::test]
async fn cleanup_mixed() {
    let analytics_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = cleanup_config(analytics_dir.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    collector.record_search(cleanup_search_event("laptop", "products"));
    collector.record_search(cleanup_search_event("action", "movies"));
    collector.record_search(cleanup_search_event("query", "old-deleted-index"));
    collector.flush_all();
    std::fs::create_dir_all(index_dir.path().join("products")).unwrap();
    std::fs::create_dir_all(index_dir.path().join("movies")).unwrap();
    let removed = run_cleanup(&engine, index_dir.path());
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0], "old-deleted-index");
    assert!(analytics_dir.path().join("products").exists());
    assert!(!analytics_dir.path().join("old-deleted-index").exists());
}

#[tokio::test]
async fn cleanup_no_analytics_data() {
    let analytics_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = cleanup_config(analytics_dir.path());
    let engine = AnalyticsQueryEngine::new(config);
    std::fs::create_dir_all(index_dir.path().join("products")).unwrap();
    let removed = run_cleanup(&engine, index_dir.path());
    assert!(removed.is_empty());
}

/// Verify that a non-null `interleaving_team` value on an `InsightEvent` survives a Parquet write-read roundtrip.
#[test]
fn insight_event_interleaving_team_roundtrip() {
    use crate::analytics::schema::insight_event_schema;
    use arrow::array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("events");
    let mut event = make_insight_ev("click", "products", Some(&"a".repeat(32)));
    event.interleaving_team = Some("control".to_string());
    writer::flush_insight_events(&[event], &dir).unwrap();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let partition_dir = dir.join(format!("date={}", today));
    let parquet_file = std::fs::read_dir(&partition_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .extension()
                .map(|x| x == "parquet")
                .unwrap_or(false)
        })
        .unwrap();
    let file = File::open(parquet_file.path()).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];

    let schema = insight_event_schema();
    let team_idx = schema.index_of("interleaving_team").unwrap();
    let team_col = batch
        .column(team_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert_eq!(team_col.value(0), "control");
}

/// Verify that a `None` interleaving_team serializes as a Parquet null and survives a write-read roundtrip.
#[test]
fn insight_event_null_interleaving_team_roundtrip() {
    use crate::analytics::schema::insight_event_schema;
    use arrow::array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use std::fs::File;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("events");
    let event = make_insight_ev("click", "products", Some(&"a".repeat(32)));
    // interleaving_team is None by default
    writer::flush_insight_events(&[event], &dir).unwrap();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let partition_dir = dir.join(format!("date={}", today));
    let parquet_file = std::fs::read_dir(&partition_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| {
            e.path()
                .extension()
                .map(|x| x == "parquet")
                .unwrap_or(false)
        })
        .unwrap();
    let file = File::open(parquet_file.path()).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .unwrap()
        .build()
        .unwrap();
    let batches: Vec<_> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1);
    let batch = &batches[0];

    let schema = insight_event_schema();
    let team_idx = schema.index_of("interleaving_team").unwrap();
    let team_col = batch
        .column(team_idx)
        .as_any()
        .downcast_ref::<arrow::array::StringArray>()
        .unwrap();
    assert!(
        team_col.is_null(0),
        "interleaving_team should be null when not set"
    );
}

#[tokio::test]
async fn cleanup_analytics_dir_missing() {
    let analytics_dir = TempDir::new().unwrap();
    let index_dir = TempDir::new().unwrap();
    let config = AnalyticsConfig {
        enabled: true,
        data_dir: analytics_dir.path().join("nonexistent"),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    };
    let engine = AnalyticsQueryEngine::new(config);
    let removed = run_cleanup(&engine, index_dir.path());
    assert!(removed.is_empty());
}
