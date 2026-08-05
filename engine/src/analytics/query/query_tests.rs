use super::*;
use crate::analytics::collector::AnalyticsCollector;
use crate::analytics::schema::{InsightEvent, SearchEvent};
use crate::analytics::seed::clear_analytics;
use crate::analytics::writer::{flush_rollup_window, search_events_to_batch, write_parquet_file};
use std::collections::BTreeSet;
use tempfile::TempDir;

fn test_analytics_config(temp_dir: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: temp_dir.path().to_path_buf(),
        flush_interval_secs: 60,
        flush_size: 10_000,
        retention_days: 90,
    }
}

/// Construct a click `InsightEvent` test fixture for the given index and object ID, with no query ID, a default user token, and position of 1.
fn click_event(index: &str, object_id: &str) -> InsightEvent {
    InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Product Clicked".to_string(),
        index: index.to_string(),
        user_token: "user-1".to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![1]),
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Construct a click `InsightEvent` test fixture for the given index, object ID, and query ID, with a default user token and position of 1.
fn click_event_with_query_id(index: &str, object_id: &str, query_id: &str) -> InsightEvent {
    InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Product Clicked".to_string(),
        index: index.to_string(),
        user_token: "user-1".to_string(),
        authenticated_user_token: None,
        query_id: Some(query_id.to_string()),
        object_ids: vec![object_id.to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![1]),
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Construct a `SearchEvent` test fixture for the given index, query text, and optional query ID, with sensible defaults (1 hit, 5ms processing time, user-1 token).
fn search_event(
    index: &str,
    query: &str,
    query_id: Option<&str>,
) -> crate::analytics::schema::SearchEvent {
    crate::analytics::schema::SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: query_id.map(str::to_string),
        index_name: index.to_string(),
        nb_hits: 1,
        processing_time_ms: 5,
        user_token: Some("user-1".to_string()),
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
    }
}

// ── date_to_start_ms ──

#[test]
fn date_to_start_ms_valid() {
    let ms = date_to_start_ms("2024-01-15").unwrap();
    // 2024-01-15 00:00:00 UTC
    assert_eq!(ms, 1705276800000);
}

#[test]
fn date_to_start_ms_epoch() {
    let ms = date_to_start_ms("1970-01-01").unwrap();
    assert_eq!(ms, 0);
}

#[test]
fn date_to_start_ms_invalid() {
    assert!(date_to_start_ms("not-a-date").is_err());
}

#[test]
fn date_to_start_ms_wrong_format() {
    assert!(date_to_start_ms("01/15/2024").is_err());
}

// ── date_to_end_ms ──

#[test]
fn date_to_end_ms_valid() {
    let ms = date_to_end_ms("2024-01-15").unwrap();
    // 2024-01-15 23:59:59 UTC
    assert_eq!(ms, 1705363199000);
}

#[test]
fn date_to_end_ms_after_start() {
    let start = date_to_start_ms("2024-01-15").unwrap();
    let end = date_to_end_ms("2024-01-15").unwrap();
    assert!(end > start);
    // Difference should be 23h59m59s = 86399 seconds
    assert_eq!(end - start, 86399000);
}

#[test]
fn date_to_end_ms_invalid() {
    assert!(date_to_end_ms("garbage").is_err());
}

// ── ms_to_date_string ──

#[test]
fn ms_to_date_string_epoch() {
    assert_eq!(ms_to_date_string(0), "1970-01-01");
}

#[test]
fn ms_to_date_string_roundtrip() {
    let ms = date_to_start_ms("2024-06-15").unwrap();
    assert_eq!(ms_to_date_string(ms), "2024-06-15");
}

#[test]
fn ms_to_date_string_mid_day() {
    // 2024-01-15 12:00:00 UTC = start + 12h
    let ms = date_to_start_ms("2024-01-15").unwrap() + 12 * 3600 * 1000;
    assert_eq!(ms_to_date_string(ms), "2024-01-15");
}

// ── find_parquet_files ──

#[test]
fn find_parquet_files_nonexistent_dir() {
    let files = find_parquet_files(std::path::Path::new("/nonexistent/path")).unwrap();
    assert!(files.is_empty());
}

#[test]
fn find_parquet_files_empty_dir() {
    let dir = tempfile::tempdir().unwrap();
    let files = find_parquet_files(dir.path()).unwrap();
    assert!(files.is_empty());
}

#[test]
fn find_parquet_files_finds_parquet() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("data.parquet"), b"fake").unwrap();
    std::fs::write(dir.path().join("other.txt"), b"text").unwrap();
    let files = find_parquet_files(dir.path()).unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].extension().unwrap() == "parquet");
}

#[test]
fn find_parquet_files_nested() {
    let dir = tempfile::tempdir().unwrap();
    let sub = dir.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("nested.parquet"), b"fake").unwrap();
    std::fs::write(dir.path().join("top.parquet"), b"fake").unwrap();
    let files = find_parquet_files(dir.path()).unwrap();
    assert_eq!(files.len(), 2);
}

#[test]
fn parquet_presence_check_stops_after_first_matching_partition() {
    let dir = tempfile::tempdir().unwrap();

    // Every partition holds a parquet, so whichever entry the iterator yields
    // first is a match. Descending into it consumes one root entry plus one leaf
    // entry, so a lazy walk must find the match well within a small entry budget.
    for partition in 0..100 {
        let path = dir.path().join(format!("{partition:02}_partition"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("data.parquet"), b"fake").unwrap();
    }

    // A budget of 4 entries is far below the 100 root partitions. This passes
    // only if the walk short-circuits after the first match; an implementation
    // that enumerates every sibling before inspecting the first would exhaust
    // the budget and return an error.
    assert!(contains_parquet_file_with_entry_budget(dir.path(), 4).unwrap());
}

#[test]
fn parquet_presence_check_errors_when_it_enumerates_past_the_first_match() {
    let dir = tempfile::tempdir().unwrap();

    // A budget of 1 cannot cover the root entry plus the leaf parquet needed for
    // even a single descent, so any real walk trips the guard. This proves the
    // budget is load-bearing and the passing case above is not vacuous.
    for partition in 0..8 {
        let path = dir.path().join(format!("{partition:02}_partition"));
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("data.parquet"), b"fake").unwrap();
    }

    assert!(contains_parquet_file_with_entry_budget(dir.path(), 1).is_err());
}

// ── arrow_value_at ──

#[test]
fn arrow_value_at_int64() {
    use arrow::array::Int64Array;
    let arr = Int64Array::from(vec![42, 99]);
    let val = arrow_value_at(&arr, 0);
    assert_eq!(val, serde_json::json!(42));
}

#[test]
fn arrow_value_at_float64() {
    use arrow::array::Float64Array;
    let arr = Float64Array::from(vec![2.5]);
    let val = arrow_value_at(&arr, 0);
    assert_eq!(val, serde_json::json!(2.5));
}

#[test]
fn arrow_value_at_string() {
    use arrow::array::StringArray;
    let arr = StringArray::from(vec!["hello"]);
    let val = arrow_value_at(&arr, 0);
    assert_eq!(val, serde_json::json!("hello"));
}

#[test]
fn arrow_value_at_bool() {
    use arrow::array::BooleanArray;
    let arr = BooleanArray::from(vec![true, false]);
    assert_eq!(arrow_value_at(&arr, 0), serde_json::json!(true));
    assert_eq!(arrow_value_at(&arr, 1), serde_json::json!(false));
}

#[test]
fn arrow_value_at_null() {
    use arrow::array::Int64Array;
    let arr = Int64Array::from(vec![Some(1), None]);
    assert_eq!(arrow_value_at(&arr, 1), serde_json::Value::Null);
}

#[test]
fn arrow_value_at_int32() {
    use arrow::array::Int32Array;
    let arr = Int32Array::from(vec![7]);
    assert_eq!(arrow_value_at(&arr, 0), serde_json::json!(7));
}

#[test]
fn arrow_value_at_uint64() {
    use arrow::array::UInt64Array;
    let arr = UInt64Array::from(vec![u64::MAX]);
    assert_eq!(arrow_value_at(&arr, 0), serde_json::json!(u64::MAX));
}

// ── batches_to_json ──

#[test]
fn batches_to_json_empty() {
    let rows = batches_to_json(&[]).unwrap();
    assert!(rows.is_empty());
}

/// Verify that `batches_to_json` correctly converts a single `RecordBatch` with string and integer columns into the expected JSON row objects.
#[test]
fn batches_to_json_single_batch() {
    use arrow::array::{Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use arrow::record_batch::RecordBatch;

    let schema = std::sync::Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("count", DataType::Int64, false),
    ]));

    let batch = RecordBatch::try_new(
        schema,
        vec![
            std::sync::Arc::new(StringArray::from(vec!["alice", "bob"])),
            std::sync::Arc::new(Int64Array::from(vec![10, 20])),
        ],
    )
    .unwrap();

    let rows = batches_to_json(&[batch]).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["name"], "alice");
    assert_eq!(rows[0]["count"], 10);
    assert_eq!(rows[1]["name"], "bob");
    assert_eq!(rows[1]["count"], 20);
}

/// Verify that `get_click_counts_for_objects` returns correct per-object click counts after flushing insight events, and omits object IDs with no recorded clicks.
#[tokio::test]
async fn get_click_counts_for_objects_returns_expected_counts() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let collector = AnalyticsCollector::new(config.clone());

    collector.record_insight(click_event("products", "nike-1"));
    collector.record_insight(click_event("products", "nike-1"));
    collector.record_insight(click_event("products", "nike-1"));
    collector.record_insight(click_event("products", "adidas-1"));
    collector.record_insight(click_event("products", "other-1"));
    collector.flush_insights();

    let engine = AnalyticsQueryEngine::new(config);
    let object_ids = vec![
        "nike-1".to_string(),
        "adidas-1".to_string(),
        "missing-1".to_string(),
    ];
    let counts = engine
        .get_click_counts_for_objects("products", &object_ids)
        .await
        .unwrap();

    assert_eq!(counts.get("nike-1"), Some(&3));
    assert_eq!(counts.get("adidas-1"), Some(&1));
    assert!(!counts.contains_key("missing-1"));
}

#[tokio::test]
async fn get_click_counts_for_objects_empty_data_returns_empty_map() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    let object_ids = vec!["nike-1".to_string(), "adidas-1".to_string()];
    let counts = engine
        .get_click_counts_for_objects("products", &object_ids)
        .await
        .unwrap();

    assert!(counts.is_empty());
}

/// Verify that `no_click_searches` safely handles query IDs containing SQL-special characters (e.g. single quotes) when building the `IN` clause, preventing SQL injection.
#[tokio::test]
async fn no_click_searches_escapes_query_ids_from_events_before_in_clause() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let collector = AnalyticsCollector::new(config.clone());

    let malicious_qid = "qid'withquote";
    collector.record_search(search_event("products", "shoes", Some(malicious_qid)));
    collector.record_insight(click_event_with_query_id(
        "products",
        "shoe-1",
        malicious_qid,
    ));
    collector.flush_searches();
    collector.flush_insights();

    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .no_click_searches("products", &today, &today, 10)
        .await
        .unwrap();

    let searches = result
        .get("searches")
        .and_then(serde_json::Value::as_array)
        .expect("searches array");
    assert!(
        searches.is_empty(),
        "clicked query should not be returned as no-click even with quote in query_id"
    );
}
#[test]
fn aggregate_counts_by_query_id_merges_multiple_query_ids_into_same_query() {
    let rows = vec![
        serde_json::json!({"query_id": "qid-1", "click_count": 2}),
        serde_json::json!({"query_id": "qid-2", "click_count": 3}),
        serde_json::json!({"query_id": "qid-3", "click_count": 1}),
    ];
    let qid_to_query = HashMap::from([
        ("qid-1".to_string(), "boots".to_string()),
        ("qid-2".to_string(), "boots".to_string()),
        ("qid-3".to_string(), "hats".to_string()),
    ]);

    let aggregated = aggregate_counts_by_query_id(&rows, &qid_to_query, "click_count");

    assert_eq!(aggregated.get("boots"), Some(&5));
    assert_eq!(aggregated.get("hats"), Some(&1));
}
#[test]
fn enrich_rows_with_click_metrics_adds_expected_fields() {
    let rows = vec![serde_json::json!({"search": "boots"})];
    let tracked = HashMap::from([("boots".to_string(), 4_i64)]);
    let clicks = HashMap::from([("boots".to_string(), 3_i64)]);
    let conversions = HashMap::from([("boots".to_string(), 1_i64)]);
    let position_sums = HashMap::from([("boots".to_string(), 13.0_f64)]);
    let position_counts = HashMap::from([("boots".to_string(), 4_i64)]);

    let enriched = enrich_rows_with_click_metrics(
        rows,
        &tracked,
        &clicks,
        &conversions,
        &position_sums,
        &position_counts,
    );

    let row = enriched.first().expect("enriched row");
    assert_eq!(row.get("clickCount"), Some(&serde_json::json!(3)));
    assert_eq!(row.get("trackedSearchCount"), Some(&serde_json::json!(4)));
    assert_eq!(row.get("conversionCount"), Some(&serde_json::json!(1)));
    assert_eq!(row.get("clickThroughRate"), Some(&serde_json::json!(0.75)));
    assert_eq!(row.get("conversionRate"), Some(&serde_json::json!(0.25)));
    assert_eq!(
        row.get("averageClickPosition"),
        Some(&serde_json::json!(3.3))
    );
}

// ── Known-answer query contract tests ──
//
// These tests lock the raw-query contract that rollup queries must reproduce.
// They write hand-calculated Parquet datasets via `write_parquet_file` and
// `search_events_to_batch`, then assert exact numeric results through the
// public `AnalyticsQueryEngine` API.

fn search_event_with_hits(
    index: &str,
    query: &str,
    nb_hits: u32,
    has_results: bool,
    timestamp_ms: i64,
    user_token: &str,
) -> SearchEvent {
    SearchEvent {
        timestamp_ms,
        query: query.to_string(),
        query_id: None,
        index_name: index.to_string(),
        nb_hits,
        processing_time_ms: 5,
        user_token: Some(user_token.to_string()),
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    }
}

fn seed_known_answer_dataset(temp_dir: &TempDir) -> AnalyticsConfig {
    let config = test_analytics_config(temp_dir);
    let index = "products";
    let schema = crate::analytics::schema::search_event_schema();

    // 2025-06-15 12:00:00 UTC
    let base_date = chrono::NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
    let base_ts = base_date
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();

    // Dataset (all on 2025-06-15):
    //   3x "laptop"  nb_hits=[10, 50, 60]  has_results=true   → count=3, AVG(nb_hits)=40
    //   2x "phone"   nb_hits=[100, 200]    has_results=true   → count=2, AVG(nb_hits)=150
    //   1x "xyzzy"   nb_hits=0             has_results=false  → count=1, no-result
    //   1x "zzznothing" nb_hits=0          has_results=false  → count=1, no-result
    // Total: 7 searches, 2 no-result searches
    let events = vec![
        search_event_with_hits(index, "laptop", 10, true, base_ts, "user-a"),
        search_event_with_hits(index, "laptop", 50, true, base_ts + 1000, "user-b"),
        search_event_with_hits(index, "laptop", 60, true, base_ts + 2000, "user-c"),
        search_event_with_hits(index, "phone", 100, true, base_ts + 3000, "user-a"),
        search_event_with_hits(index, "phone", 200, true, base_ts + 4000, "user-b"),
        search_event_with_hits(index, "xyzzy", 0, false, base_ts + 5000, "user-a"),
        search_event_with_hits(index, "zzznothing", 0, false, base_ts + 6000, "user-d"),
    ];

    let batch = search_events_to_batch(&events, &schema).unwrap();

    let partition_dir = config.searches_dir(index).join("date=2025-06-15");
    std::fs::create_dir_all(&partition_dir).unwrap();
    let parquet_path = partition_dir.join("test_data.parquet");
    write_parquet_file(&parquet_path, batch).unwrap();

    config
}

#[tokio::test]
async fn status_reports_no_data_after_clear_preserves_empty_roots() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config.clone());

    assert_eq!(engine.status("products").await.unwrap()["hasData"], true);
    assert_eq!(clear_analytics(&config, "products").unwrap(), 1);
    assert_eq!(engine.status("products").await.unwrap()["hasData"], false);
}

fn seed_hourly_spread_dataset(temp_dir: &TempDir) -> AnalyticsConfig {
    let config = test_analytics_config(temp_dir);
    let index = "products";
    let schema = crate::analytics::schema::search_event_schema();

    let base_date = chrono::NaiveDate::from_ymd_opt(2025, 6, 15).unwrap();
    let hour_12 = base_date
        .and_hms_opt(12, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    let hour_13 = base_date
        .and_hms_opt(13, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();

    // 7 total events split across two UTC hours to verify 1hour rollup ranges
    // still collapse into day-level output shape.
    let events = vec![
        search_event_with_hits(index, "laptop", 10, true, hour_12, "user-a"),
        search_event_with_hits(index, "laptop", 50, true, hour_12 + 1000, "user-b"),
        search_event_with_hits(index, "laptop", 60, true, hour_12 + 2000, "user-c"),
        search_event_with_hits(index, "phone", 100, true, hour_13, "user-a"),
        search_event_with_hits(index, "phone", 200, true, hour_13 + 1000, "user-b"),
        search_event_with_hits(index, "xyzzy", 0, false, hour_13 + 2000, "user-a"),
        search_event_with_hits(index, "zzznothing", 0, false, hour_13 + 3000, "user-d"),
    ];

    let batch = search_events_to_batch(&events, &schema).unwrap();
    let partition_dir = config.searches_dir(index).join("date=2025-06-15");
    std::fs::create_dir_all(&partition_dir).unwrap();
    write_parquet_file(&partition_dir.join("test_data.parquet"), batch).unwrap();

    config
}

#[tokio::test]
async fn known_answer_top_searches() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    let params = AnalyticsQueryParams {
        index_name: "products",
        start_date: "2025-06-15",
        end_date: "2025-06-15",
        limit: 10,
        tags: None,
    };
    let result = engine.top_searches(&params, false, None).await.unwrap();
    let searches = result["searches"].as_array().unwrap();

    assert_eq!(searches.len(), 4, "should return all 4 distinct queries");

    // Ordered by count DESC: laptop(3), phone(2), xyzzy(1), zzznothing(1)
    assert_eq!(searches[0]["search"], "laptop");
    assert_eq!(searches[0]["count"], 3);
    // AVG(10, 50, 60) = 40.0, CAST to INTEGER = 40
    assert_eq!(searches[0]["nbHits"], 40);

    assert_eq!(searches[1]["search"], "phone");
    assert_eq!(searches[1]["count"], 2);
    // AVG(100, 200) = 150.0, CAST to INTEGER = 150
    assert_eq!(searches[1]["nbHits"], 150);

    // xyzzy and zzznothing both have count=1; order between them is stable by query text
    let tail: Vec<&str> = searches[2..]
        .iter()
        .map(|s| s["search"].as_str().unwrap())
        .collect();
    assert!(tail.contains(&"xyzzy"));
    assert!(tail.contains(&"zzznothing"));

    for s in &searches[2..] {
        assert_eq!(s["count"], 1);
        assert_eq!(s["nbHits"], 0);
    }
}

#[tokio::test]
async fn known_answer_search_count() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    let result = engine
        .search_count("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();

    assert_eq!(result["count"], 7, "total search count");

    let dates = result["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1, "single day in range");
    assert_eq!(dates[0]["date"], "2025-06-15");
    assert_eq!(dates[0]["count"], 7);
}

#[tokio::test]
async fn known_answer_no_results_searches() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    let result = engine
        .no_results_searches("products", "2025-06-15", "2025-06-15", 10)
        .await
        .unwrap();
    let searches = result["searches"].as_array().unwrap();

    assert_eq!(searches.len(), 2, "exactly 2 no-result queries");

    let queries: Vec<&str> = searches
        .iter()
        .map(|s| s["search"].as_str().unwrap())
        .collect();
    assert!(queries.contains(&"xyzzy"));
    assert!(queries.contains(&"zzznothing"));

    for s in searches {
        assert_eq!(s["count"], 1);
        assert_eq!(s["nbHits"], 0);
    }
}

#[tokio::test]
async fn known_answer_no_results_rate() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    let result = engine
        .no_results_rate("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();

    assert_eq!(result["count"], 7, "total searches");
    assert_eq!(result["noResults"], 2, "no-result searches");
    // 2/7 = 0.2857… → rounded to 3 decimals = 0.286
    assert_eq!(result["rate"], serde_json::json!(0.286));

    let dates = result["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    assert_eq!(dates[0]["date"], "2025-06-15");
    assert_eq!(dates[0]["rate"], serde_json::json!(0.286));
    assert_eq!(dates[0]["count"], 7);
    assert_eq!(dates[0]["noResults"], 2);
}

#[tokio::test]
async fn known_answer_date_range_excludes_out_of_range() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    // Query a different date range that doesn't overlap
    let result = engine
        .search_count("products", "2025-07-01", "2025-07-31")
        .await
        .unwrap();

    assert_eq!(result["count"], 0, "no data in July");
    let dates = result["dates"].as_array().unwrap();
    assert!(dates.is_empty());
}

#[tokio::test]
async fn known_answer_empty_index_returns_zero() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    // Query an index with no data
    let result = engine
        .search_count("nonexistent_index", "2025-06-15", "2025-06-15")
        .await
        .unwrap();

    assert_eq!(result["count"], 0);
}

// ── Rollup-vs-raw parity tests (RED baseline for RF-2 Wave 2) ──
//
// Each test reuses `seed_known_answer_dataset`, first proves the raw-path known
// answer through the public `AnalyticsQueryEngine` method, then writes the day
// rollup via `flush_rollup_window`, removes the raw search Parquet so the query
// engine can no longer fall back to raw data, and re-queries the same public
// method — asserting the rollup-served result is identical to the raw result.
//
// Removing raw data after rollup generation is the key guard: it forces the
// query path to read from rollup Parquet to produce any non-empty result,
// preventing false-green if the query planner routing (Stage 3) is missing.
//
// The fixture is seeded on the historical date 2025-06-15, so the wall clock is
// already well past the day window end — the window is therefore rollup-eligible
// without any synthetic-clock injection. The "1day" tier string matches
// `manifest::TIER_PREFERENCE`.
//
// These stay RED in Stage 1: `flush_rollup_window` is an `unimplemented!()` seam
// (RF-2 Stage 2) and panics before the parity assertion can run. The raw-path
// assertions execute and pass first, locking the contract the rollup path must
// reproduce.

/// Day window covering the entire 2025-06-15 fixture: `[start, end)` aligned to
/// UTC midnight boundaries (start = 2025-06-15 00:00:00, end = 2025-06-16 00:00:00).
fn fixture_day_window_ms() -> (i64, i64) {
    let window_start_ms = date_to_start_ms("2025-06-15").unwrap();
    let window_end_ms = date_to_start_ms("2025-06-16").unwrap();
    (window_start_ms, window_end_ms)
}

/// Build canonical hourly coverage for the fixture day, then compact to the
/// 1day tier so parity tests exercise a certified daily rollup.
fn flush_fixture_day_rollup(config: &AnalyticsConfig, index_name: &str) {
    let (window_start_ms, window_end_ms) = fixture_day_window_ms();
    let hour_ms = 3_600_000;
    for offset in 0..24 {
        let hour_start = window_start_ms + offset * hour_ms;
        let hour_end = hour_start + hour_ms;
        flush_rollup_window(config, index_name, "1hour", hour_start, hour_end)
            .expect("hourly rollup window flush");
    }
    flush_rollup_window(config, index_name, "1day", window_start_ms, window_end_ms)
        .expect("daily rollup window flush");
}

/// Build only hourly coverage for the fixture day so the planner must choose the
/// certified "1hour" tier instead of "1day".
fn flush_fixture_day_hourly_rollup_only(config: &AnalyticsConfig, index_name: &str) {
    let (window_start_ms, _) = fixture_day_window_ms();
    let hour_ms = 3_600_000;
    for offset in 0..24 {
        let hour_start = window_start_ms + offset * hour_ms;
        let hour_end = hour_start + hour_ms;
        flush_rollup_window(config, index_name, "1hour", hour_start, hour_end)
            .expect("hourly rollup window flush");
    }
}

/// Remove raw search Parquet for an index so re-queries must read from rollups.
fn remove_raw_searches(config: &AnalyticsConfig, index_name: &str) {
    let searches_dir = config.searches_dir(index_name);
    if searches_dir.exists() {
        std::fs::remove_dir_all(&searches_dir).unwrap();
    }
}

#[tokio::test]
async fn rollup_matches_raw_top_searches() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config.clone());

    let params = AnalyticsQueryParams {
        index_name: "products",
        start_date: "2025-06-15",
        end_date: "2025-06-15",
        limit: 10,
        tags: None,
    };

    // 1. Raw path must produce the hand-calculated known answer.
    let raw = engine.top_searches(&params, false, None).await.unwrap();
    let raw_searches = raw["searches"].as_array().unwrap();
    assert_eq!(raw_searches[0]["search"], "laptop");
    assert_eq!(raw_searches[0]["count"], 3);
    assert_eq!(raw_searches[0]["nbHits"], 40);
    assert_eq!(raw_searches[1]["search"], "phone");
    assert_eq!(raw_searches[1]["count"], 2);
    assert_eq!(raw_searches[1]["nbHits"], 150);

    // 2. Write the rollup, then remove raw search data so the query engine cannot
    //    fall back to raw Parquet. RED in Stage 1 — the flush panics on the seam.
    flush_fixture_day_rollup(&config, "products");
    remove_raw_searches(&config, "products");

    // 3. Re-query: must produce the same result from rollup data alone.
    let rollup = engine.top_searches(&params, false, None).await.unwrap();
    let rollup_searches = rollup["searches"].as_array().unwrap();
    assert_eq!(
        rollup_searches.len(),
        raw_searches.len(),
        "rollup-served top_searches must preserve result length"
    );
    // Rows with count=1 are ties, so compare the deterministic head positionally
    // and compare the tail as an unordered set.
    assert_eq!(
        rollup_searches[0], raw_searches[0],
        "top ranked query must match raw result"
    );
    assert_eq!(
        rollup_searches[1], raw_searches[1],
        "second ranked query must match raw result"
    );
    let extract_row = |row: &serde_json::Value| -> (String, i64, i64) {
        (
            row["search"].as_str().unwrap().to_string(),
            row["count"].as_i64().unwrap(),
            row["nbHits"].as_i64().unwrap(),
        )
    };
    let raw_tied_tail: BTreeSet<(String, i64, i64)> =
        raw_searches[2..].iter().map(extract_row).collect();
    let rollup_tied_tail: BTreeSet<(String, i64, i64)> =
        rollup_searches[2..].iter().map(extract_row).collect();
    assert_eq!(
        rollup_tied_tail, raw_tied_tail,
        "rollup-served tie rows must match raw result as a set"
    );
}

#[tokio::test]
async fn rollup_matches_raw_search_count() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config.clone());

    // 1. Raw path known answer: 7 total searches, all on 2025-06-15.
    let raw = engine
        .search_count("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();
    assert_eq!(raw["count"], 7, "total search count");
    let raw_dates = raw["dates"].as_array().unwrap();
    assert_eq!(raw_dates.len(), 1, "single day in range");
    assert_eq!(raw_dates[0]["date"], "2025-06-15");
    assert_eq!(raw_dates[0]["count"], 7);

    // 2. Write the rollup, then remove raw search data so the query engine cannot
    //    fall back to raw Parquet. RED in Stage 1 — the flush panics on the seam.
    flush_fixture_day_rollup(&config, "products");
    remove_raw_searches(&config, "products");

    // 3. Re-query: must produce the same result from rollup data alone.
    let rollup = engine
        .search_count("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();
    assert_eq!(
        rollup["count"], raw["count"],
        "rollup-served total count must match raw"
    );
    assert_eq!(
        rollup["dates"], raw["dates"],
        "rollup-served daily breakdown must match raw"
    );
}

#[tokio::test]
async fn rollup_matches_raw_search_count_hourly_tier_daily_dates() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_hourly_spread_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config.clone());

    // Raw path contract: a single day-level date bucket.
    let raw = engine
        .search_count("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();
    assert_eq!(raw["count"], 7);
    assert_eq!(
        raw["dates"],
        serde_json::json!([{ "date": "2025-06-15", "count": 7 }])
    );

    // Generate certified hourly rollups only, then force rollup-only reads.
    flush_fixture_day_hourly_rollup_only(&config, "products");
    remove_raw_searches(&config, "products");

    // The rollup path must still collapse hourly windows into daily output shape.
    let rollup = engine
        .search_count("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();
    assert_eq!(rollup["count"], raw["count"]);
    assert_eq!(
        rollup["dates"], raw["dates"],
        "certified 1hour ranges must preserve day-level dates shape"
    );
}

#[tokio::test]
async fn rollup_matches_raw_users_count_hll() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config.clone());

    // 1. Raw path: the fixture has 4 distinct user tokens (user-a, user-b, user-c,
    //    user-d), so the distinct-user count is exactly 4.
    let raw = engine
        .users_count_hll("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();
    assert_eq!(raw["count"], 4, "4 distinct users in the fixture");

    // 2. Write the rollup, then remove raw search data so the query engine cannot
    //    fall back to raw Parquet. RED in Stage 1 — the flush panics on the seam.
    flush_fixture_day_rollup(&config, "products");
    remove_raw_searches(&config, "products");

    // 3. Re-query: must produce the same result from rollup data alone.
    let rollup = engine
        .users_count_hll("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();
    assert_eq!(rollup, raw, "rollup-served HLL payload must match raw");
}

#[tokio::test]
async fn raw_fallback_uncertified_range_serves_raw() {
    let temp_dir = TempDir::new().unwrap();
    let config = seed_known_answer_dataset(&temp_dir);
    let engine = AnalyticsQueryEngine::new(config);

    // No rollup manifest or rollup parquet has been generated for this index/date.
    // The planner must fall back to raw search parquet and preserve the known answer.
    let result = engine
        .search_count("products", "2025-06-15", "2025-06-15")
        .await
        .unwrap();

    assert_eq!(
        result["count"], 7,
        "uncertified range must stay on raw path and keep known-answer total"
    );
    assert_eq!(
        result["dates"],
        serde_json::json!([{ "date": "2025-06-15", "count": 7 }]),
        "uncertified range must keep known-answer daily breakdown from raw path"
    );
}
