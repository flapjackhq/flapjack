//! Integration tests for the analytics pipeline covering partition retention cleanup, InsightEvent validation, and end-to-end recording through query verification for searches, clicks, conversions, HLL user counting, revenue aggregation, and rate endpoint edge cases.

// QueryAggregator tests removed — fully covered by inline tests in analytics/aggregation.rs

// ─── Retention cleanup (retention.rs) ─────────────────────────────────────────

use crate::analytics::manifest::{DateState, RollupManifest, TierState, WindowEntry, WindowStatus};
use crate::analytics::retention::{cleanup_old_partitions, cleanup_old_partitions_at};
use std::collections::HashMap;
use tempfile::TempDir;

fn create_partition(base: &std::path::Path, index: &str, table: &str, date: &str) {
    let dir = base.join(index).join(table).join(format!("date={}", date));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("test.parquet"), b"dummy").unwrap();
}

fn retention_gate_now() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
        chrono::NaiveDate::from_ymd_opt(2024, 4, 10)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap(),
        chrono::Utc,
    )
}

fn certified_hourly_windows(date: &str) -> Vec<WindowEntry> {
    let day_start = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis();
    (0..24)
        .map(|hour| {
            let start_ms = day_start + (hour as i64) * 3_600_000;
            WindowEntry {
                start_ms,
                end_ms: start_ms + 3_600_000,
                status: WindowStatus::Closed,
                event_count: 10,
                file: format!("rollup_1hour_{}_0.parquet", start_ms),
            }
        })
        .collect()
}

fn save_manifest(base: &std::path::Path, index: &str, manifest: &RollupManifest) {
    let manifest_path = base.join(index).join("rollups").join("manifest.json");
    manifest.save(&manifest_path).unwrap();
}

fn write_certified_hourly_artifacts(base: &std::path::Path, index: &str, windows: &[WindowEntry]) {
    let tier_dir = base.join(index).join("rollups").join("1hour");
    std::fs::create_dir_all(&tier_dir).unwrap();
    for window in windows {
        std::fs::write(tier_dir.join(&window.file), b"certified rollup").unwrap();
    }
}

/// Verify that partitions older than the retention window are evicted while recent ones are preserved.
#[test]
fn removes_old_partitions() {
    let tmp = TempDir::new().unwrap();
    let old_date = (chrono::Utc::now() - chrono::Duration::days(200))
        .format("%Y-%m-%d")
        .to_string();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    create_partition(tmp.path(), "products", "events", &old_date);
    create_partition(tmp.path(), "products", "events", &today);
    let removed = cleanup_old_partitions(tmp.path(), 90).unwrap();
    assert_eq!(removed, 1);
    assert!(!tmp
        .path()
        .join("products")
        .join("events")
        .join(format!("date={}", old_date))
        .exists());
    assert!(tmp
        .path()
        .join("products")
        .join("events")
        .join(format!("date={}", today))
        .exists());
}

#[test]
fn keeps_recent_partitions() {
    let tmp = TempDir::new().unwrap();
    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    create_partition(tmp.path(), "products", "searches", &yesterday);
    let removed = cleanup_old_partitions(tmp.path(), 90).unwrap();
    assert_eq!(removed, 0);
}

#[test]
fn retention_removes_only_expired_legacy_full_ip_partition() {
    let tmp = TempDir::new().unwrap();
    let config = crate::analytics::config::AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().to_path_buf(),
        flush_interval_secs: 1,
        flush_size: 100,
        retention_days: 30,
    };
    let old_path = super::test_analytics_io::write_legacy_search_parquet(
        &config,
        "products",
        "2024-03-01",
        &["203.0.113.47"],
    );
    let recent_path = super::test_analytics_io::write_legacy_search_parquet(
        &config,
        "products",
        "2024-04-01",
        &["198.51.100.23"],
    );
    let date = "2024-03-01";
    let windows = certified_hourly_windows(date);
    write_certified_hourly_artifacts(tmp.path(), "products", &windows);
    let mut manifest = RollupManifest::new("products");
    let mut tier = TierState {
        dates: HashMap::new(),
    };
    tier.dates.insert(
        date.to_string(),
        DateState {
            total_event_count: windows.iter().map(|window| window.event_count).sum(),
            windows,
            complete: true,
        },
    );
    manifest.tiers.insert("1hour".to_string(), tier);
    save_manifest(tmp.path(), "products", &manifest);

    let removed = cleanup_old_partitions_at(
        tmp.path(),
        30,
        chrono::DateTime::parse_from_rfc3339("2024-04-10T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc),
    )
    .unwrap();

    assert_eq!(removed, 1);
    assert!(!old_path.parent().unwrap().exists());
    assert!(recent_path.parent().unwrap().exists());
}

#[test]
fn handles_missing_directory() {
    let tmp = TempDir::new().unwrap();
    let removed = cleanup_old_partitions(&tmp.path().join("nonexistent"), 90).unwrap();
    assert_eq!(removed, 0);
}

#[test]
fn cleans_multiple_indices() {
    let tmp = TempDir::new().unwrap();
    let old_date = (chrono::Utc::now() - chrono::Duration::days(200))
        .format("%Y-%m-%d")
        .to_string();
    create_partition(tmp.path(), "products", "searches", &old_date);
    create_partition(tmp.path(), "products", "events", &old_date);
    create_partition(tmp.path(), "articles", "events", &old_date);
    let removed = cleanup_old_partitions(tmp.path(), 90).unwrap();
    assert_eq!(removed, 2);
}

#[test]
fn ignores_non_date_directories() {
    let tmp = TempDir::new().unwrap();
    let weird_dir = tmp
        .path()
        .join("products")
        .join("searches")
        .join("not-a-date");
    std::fs::create_dir_all(&weird_dir).unwrap();
    let removed = cleanup_old_partitions(tmp.path(), 90).unwrap();
    assert_eq!(removed, 0);
    assert!(weird_dir.exists());
}

#[test]
fn retention_gate_retains_uncertified_partitions() {
    let tmp = TempDir::new().unwrap();
    let index = "products";
    let table = "searches";
    let date = "2024-01-15";

    create_partition(tmp.path(), index, table, date);

    let mut manifest = RollupManifest::new(index);
    let mut tier = TierState {
        dates: HashMap::new(),
    };
    let mut partial_windows = certified_hourly_windows(date);
    partial_windows.pop();
    let total_event_count = partial_windows
        .iter()
        .map(|window| window.event_count)
        .sum();
    tier.dates.insert(
        date.to_string(),
        DateState {
            windows: partial_windows,
            complete: false,
            total_event_count,
        },
    );
    manifest.tiers.insert("1hour".to_string(), tier);
    save_manifest(tmp.path(), index, &manifest);

    let removed = cleanup_old_partitions_at(tmp.path(), 30, retention_gate_now()).unwrap();

    assert_eq!(removed, 0);
    assert!(tmp
        .path()
        .join(index)
        .join(table)
        .join(format!("date={date}"))
        .exists());
}

#[test]
fn retention_gate_deletes_certified_partitions() {
    let tmp = TempDir::new().unwrap();
    let index = "products";
    let table = "searches";
    let date = "2024-01-15";

    create_partition(tmp.path(), index, table, date);

    let mut manifest = RollupManifest::new(index);
    let windows = certified_hourly_windows(date);
    write_certified_hourly_artifacts(tmp.path(), index, &windows);
    let total_event_count = windows.iter().map(|window| window.event_count).sum();
    let mut tier = TierState {
        dates: HashMap::new(),
    };
    tier.dates.insert(
        date.to_string(),
        DateState {
            windows,
            complete: true,
            total_event_count,
        },
    );
    manifest.tiers.insert("1hour".to_string(), tier);
    save_manifest(tmp.path(), index, &manifest);

    let removed = cleanup_old_partitions_at(tmp.path(), 30, retention_gate_now()).unwrap();

    assert_eq!(removed, 1);
    assert!(!tmp
        .path()
        .join(index)
        .join(table)
        .join(format!("date={date}"))
        .exists());
}

// ─── InsightEvent validation (schema.rs) ──────────────────────────────────────

use crate::analytics::schema::InsightEvent;

const VALID_SELECTED_USER_TOKEN: &str = "00000000-0000-4000-8000-000000000000";

/// Build a valid click `InsightEvent` with a correlated query ID, one object, and position 1.
fn valid_click() -> InsightEvent {
    InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Product Click".to_string(),
        index: "products".to_string(),
        user_token: VALID_SELECTED_USER_TOKEN.to_string(),
        authenticated_user_token: None,
        query_id: Some("a".repeat(32)),
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: Some(vec![1]),
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Build a valid conversion `InsightEvent` with a correlated query ID and a $99.99 USD value.
fn valid_conversion() -> InsightEvent {
    InsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: None,
        event_name: "Purchase".to_string(),
        index: "products".to_string(),
        user_token: VALID_SELECTED_USER_TOKEN.to_string(),
        authenticated_user_token: None,
        query_id: Some("b".repeat(32)),
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: Some(99.99),
        currency: Some("USD".to_string()),
        interleaving_team: None,
    }
}

/// Build a valid view `InsightEvent` with two object IDs and no query correlation.
fn valid_view() -> InsightEvent {
    InsightEvent {
        event_type: "view".to_string(),
        event_subtype: None,
        event_name: "Product Viewed".to_string(),
        index: "products".to_string(),
        user_token: "user456".to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec!["obj1".to_string(), "obj2".to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: None,
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

#[test]
fn valid_click_passes() {
    assert!(valid_click().validate().is_ok());
}

#[test]
fn valid_conversion_passes() {
    assert!(valid_conversion().validate().is_ok());
}

#[test]
fn valid_view_passes() {
    assert!(valid_view().validate().is_ok());
}

#[test]
fn invalid_event_type() {
    let mut e = valid_click();
    e.event_type = "hover".to_string();
    assert!(e.validate().unwrap_err().contains("Invalid eventType"));
}

#[test]
fn empty_event_name() {
    let mut e = valid_click();
    e.event_name = "".to_string();
    assert!(e.validate().unwrap_err().contains("eventName"));
}

#[test]
fn event_name_too_long() {
    let mut e = valid_click();
    e.event_name = "a".repeat(65);
    assert!(e.validate().unwrap_err().contains("eventName"));
}

#[test]
fn max_boundary_event_name_64_chars() {
    let mut e = valid_click();
    e.event_name = "a".repeat(64);
    assert!(e.validate().is_ok());
}

#[test]
fn empty_user_token() {
    let mut e = valid_click();
    e.user_token = "".to_string();
    assert!(e.validate().unwrap_err().contains("userToken"));
}

#[test]
fn user_token_too_long() {
    let mut e = valid_click();
    e.user_token = "x".repeat(130);
    assert!(e.validate().unwrap_err().contains("userToken"));
}

#[test]
fn max_boundary_user_token_129_chars() {
    let mut e = valid_view();
    e.user_token = "x".repeat(129);
    assert!(e.validate().is_ok());
}

#[test]
fn user_token_with_spaces_rejected() {
    let mut e = valid_click();
    e.user_token = "user token".to_string();
    assert!(e.validate().unwrap_err().contains("userToken"));
}

#[test]
fn user_token_with_special_chars_rejected() {
    for token in ["user@token", "user!token", "user.token"] {
        let mut e = valid_click();
        e.user_token = token.to_string();
        assert!(e.validate().unwrap_err().contains("userToken"));
    }
}

#[test]
fn user_token_with_valid_charset_accepted() {
    let mut e = valid_view();
    e.user_token = "abc-123_XYZ".to_string();
    assert!(e.validate().is_ok());
}

#[test]
fn empty_object_ids() {
    let mut e = valid_click();
    e.object_ids = vec![];
    assert!(e.validate().unwrap_err().contains("objectIDs"));
}

#[test]
fn too_many_object_ids() {
    let mut e = valid_click();
    e.object_ids = (0..21).map(|i| format!("obj{}", i)).collect();
    e.positions = Some((0..21).map(|i| i as u32).collect());
    assert!(e.validate().unwrap_err().contains("objectIDs"));
}

#[test]
fn max_boundary_20_object_ids() {
    let mut e = valid_view();
    e.object_ids = (0..20).map(|i| format!("obj{}", i)).collect();
    assert!(e.validate().is_ok());
}

#[test]
fn click_after_search_missing_positions() {
    let mut e = valid_click();
    e.positions = None;
    assert!(e.validate().unwrap_err().contains("positions required"));
}

#[test]
fn click_after_search_positions_length_mismatch() {
    let mut e = valid_click();
    e.object_ids = vec!["obj1".to_string(), "obj2".to_string()];
    e.positions = Some(vec![1]);
    assert!(e.validate().unwrap_err().contains("positions length"));
}

#[test]
fn click_without_query_id_no_positions_rejected() {
    let mut e = valid_click();
    e.query_id = None;
    e.positions = None;
    assert!(e.validate().unwrap_err().contains("positions required"));
}

#[test]
fn click_without_query_id_with_positions_ok() {
    let mut e = valid_click();
    e.query_id = None;
    e.positions = Some(vec![1]);
    assert!(e.validate().is_ok());
}

#[test]
fn event_subtype_add_to_cart_on_conversion_accepted() {
    let mut e = valid_conversion();
    e.event_subtype = Some("addToCart".to_string());
    assert!(e.validate().is_ok());
}

#[test]
fn event_subtype_purchase_on_conversion_accepted() {
    let mut e = valid_conversion();
    e.event_subtype = Some("purchase".to_string());
    assert!(e.validate().is_ok());
}

#[test]
fn event_subtype_add_to_cart_on_click_rejected() {
    let mut e = valid_click();
    e.event_subtype = Some("addToCart".to_string());
    assert!(e.validate().unwrap_err().contains("eventSubtype"));
}

#[test]
fn event_subtype_purchase_on_view_rejected() {
    let mut e = valid_view();
    e.event_subtype = Some("purchase".to_string());
    assert!(e.validate().unwrap_err().contains("eventSubtype"));
}

#[test]
fn event_subtype_invalid_on_conversion_rejected() {
    let mut e = valid_conversion();
    e.event_subtype = Some("invalid".to_string());
    assert!(e.validate().unwrap_err().contains("eventSubtype"));
}

#[test]
fn invalid_query_id_too_short() {
    let mut e = valid_click();
    e.query_id = Some("abc123".to_string());
    assert!(e.validate().unwrap_err().contains("queryID"));
}

#[test]
fn invalid_query_id_non_hex() {
    let mut e = valid_click();
    e.query_id = Some("g".repeat(32));
    assert!(e.validate().unwrap_err().contains("queryID"));
}

#[test]
fn timestamp_too_old_rejected() {
    let mut e = valid_click();
    let five_days_ago = chrono::Utc::now().timestamp_millis() - (5 * 24 * 60 * 60 * 1000);
    e.timestamp = Some(five_days_ago);
    assert!(e.validate().unwrap_err().contains("4 days"));
}

#[test]
fn timestamp_within_4_days_accepted() {
    let mut e = valid_click();
    let three_days_ago = chrono::Utc::now().timestamp_millis() - (3 * 24 * 60 * 60 * 1000);
    e.timestamp = Some(three_days_ago);
    assert!(e.validate().is_ok());
}

#[test]
fn no_timestamp_accepted() {
    let mut e = valid_click();
    e.timestamp = None;
    assert!(e.validate().is_ok());
}

#[test]
fn effective_object_ids_prefers_primary() {
    let mut e = valid_view();
    e.object_ids = vec!["primary".to_string()];
    e.object_ids_alt = vec!["alt".to_string()];
    assert_eq!(e.effective_object_ids(), &["primary".to_string()]);
}

#[test]
fn effective_object_ids_falls_back_to_alt() {
    let mut e = valid_view();
    e.object_ids = vec![];
    e.object_ids_alt = vec!["alt1".to_string(), "alt2".to_string()];
    assert_eq!(
        e.effective_object_ids(),
        &["alt1".to_string(), "alt2".to_string()]
    );
}

// ─── E2E analytics pipeline tests ───────────────────────────────────────────

use crate::analytics::collector::AnalyticsCollector;
use crate::analytics::config::AnalyticsConfig;
use crate::analytics::query::AnalyticsQueryEngine;
use crate::analytics::schema::SearchEvent;

fn test_config(dir: &std::path::Path) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: dir.to_path_buf(),
        flush_interval_secs: 3600,
        flush_size: 10_000, // won't auto-flush in tests
        retention_days: 90,
    }
}

/// Build a `SearchEvent` with sensible defaults for integration tests.
///
/// # Arguments
///
/// * `query` - Search query string.
/// * `index` - Target index name.
/// * `nb_hits` - Number of hits returned.
/// * `query_id` - Optional hex query ID for click/conversion correlation.
/// * `user_token` - User token for attribution.
/// * `filters` - Optional filter string (e.g. `"brand:Apple"`).
fn search_event(
    query: &str,
    index: &str,
    nb_hits: u32,
    query_id: Option<&str>,
    user_token: &str,
    filters: Option<&str>,
) -> SearchEvent {
    SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: query_id.map(|s| s.to_string()),
        index_name: index.to_string(),
        nb_hits,
        processing_time_ms: 5,
        user_token: Some(user_token.to_string()),
        user_ip: Some("10.0.0.1".to_string()),
        filters: filters.map(|s| s.to_string()),
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

/// Build a click `InsightEvent` correlated to a search via `query_id`.
///
/// # Arguments
///
/// * `query_id` - Hex query ID linking this click to a prior search.
/// * `index` - Target index name.
/// * `user` - User token for attribution.
/// * `positions` - One-indexed click positions; also determines synthetic object IDs.
fn click_event(query_id: &str, index: &str, user: &str, positions: Vec<u32>) -> InsightEvent {
    InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Result Click".to_string(),
        index: index.to_string(),
        user_token: user.to_string(),
        authenticated_user_token: None,
        query_id: Some(query_id.to_string()),
        object_ids: positions.iter().map(|p| format!("obj{}", p)).collect(),
        object_ids_alt: vec![],
        positions: Some(positions),
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    }
}

/// Build a purchase-type conversion `InsightEvent` with a fixed $49.99 USD value.
///
/// # Arguments
///
/// * `query_id` - Hex query ID linking this conversion to a prior search.
/// * `index` - Target index name.
/// * `user` - User token for attribution.
fn conversion_event(query_id: &str, index: &str, user: &str) -> InsightEvent {
    InsightEvent {
        event_type: "conversion".to_string(),
        event_subtype: Some("purchase".to_string()),
        event_name: "Purchase".to_string(),
        index: index.to_string(),
        user_token: user.to_string(),
        authenticated_user_token: None,
        query_id: Some(query_id.to_string()),
        object_ids: vec!["obj1".to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: Some(49.99),
        currency: Some("USD".to_string()),
        interleaving_team: None,
    }
}

/// Full pipeline: searches -> clicks -> conversions -> query all analytics endpoints.
#[tokio::test]
async fn full_analytics_pipeline() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid1 = "a".repeat(32);
    let qid2 = "b".repeat(32);
    let qid3 = "c".repeat(32);
    let qid4 = "d".repeat(32);

    // --- Record search events ---
    collector.record_search(search_event(
        "laptop",
        "products",
        42,
        Some(&qid1),
        "alice",
        None,
    ));
    collector.record_search(search_event(
        "laptop",
        "products",
        38,
        Some(&qid2),
        "bob",
        None,
    ));
    collector.record_search(search_event(
        "laptop",
        "products",
        42,
        Some(&qid3),
        "charlie",
        None,
    ));
    collector.record_search(search_event(
        "phone",
        "products",
        15,
        Some(&qid4),
        "alice",
        None,
    ));
    collector.record_search(search_event(
        "nonexistent",
        "products",
        0,
        None,
        "alice",
        None,
    ));
    collector.record_search(search_event(
        "nonexistent",
        "products",
        0,
        None,
        "bob",
        None,
    ));
    collector.record_search(search_event(
        "laptop",
        "products",
        10,
        None,
        "alice",
        Some("brand:Apple"),
    ));
    collector.record_search(search_event(
        "laptop",
        "products",
        0,
        None,
        "bob",
        Some("brand:Nonexistent"),
    ));

    // --- Record click events ---
    collector.record_insight(click_event(&qid1, "products", "alice", vec![1]));
    collector.record_insight(click_event(&qid2, "products", "bob", vec![3]));

    // --- Record conversion event ---
    collector.record_insight(conversion_event(&qid1, "products", "alice"));

    // --- Flush everything to Parquet ---
    collector.flush_all();

    // === Query and verify all analytics endpoints ===

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
    assert!(
        searches.len() >= 2,
        "Should have at least 2 distinct queries"
    );
    assert_eq!(searches[0]["search"], "laptop");

    let result = engine
        .search_count("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["count"], 8, "Total 8 searches recorded");

    let result = engine
        .no_results_rate("products", &today, &today)
        .await
        .unwrap();
    let rate = result["rate"].as_f64().unwrap();
    assert!(
        (rate - 0.375).abs() < 0.01,
        "No-results rate should be ~0.375, got {}",
        rate
    );
    assert_eq!(result["noResults"], 3);

    let result = engine
        .no_results_searches("products", &today, &today, 10)
        .await
        .unwrap();
    let searches = result["searches"].as_array().unwrap();
    assert!(!searches.is_empty());
    let nonexistent = searches
        .iter()
        .find(|s| s["search"] == "nonexistent")
        .expect("'nonexistent' should appear");
    assert_eq!(nonexistent["count"], 2);

    let result = engine
        .users_count_hll("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["count"], 3, "3 unique users: alice, bob, charlie");
    assert!(result["hll_sketch"].is_string());
    assert!(result["dates"].is_array());

    let result = engine
        .click_through_rate("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["trackedSearchCount"], 4);
    assert_eq!(result["clickCount"], 2);
    let ctr = result["rate"].as_f64().unwrap();
    assert!((ctr - 0.5).abs() < 0.01, "CTR should be 0.5, got {}", ctr);

    let result = engine
        .average_click_position("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["clickCount"], 2);
    let avg = result["average"].as_f64().unwrap();
    assert!(
        (avg - 2.0).abs() < 0.5,
        "Avg click position should be ~2.0, got {}",
        avg
    );

    let result = engine
        .conversion_rate("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["trackedSearchCount"], 4);
    assert_eq!(result["conversionCount"], 1);
    let conv_rate = result["rate"].as_f64().unwrap();
    assert!(
        (conv_rate - 0.25).abs() < 0.01,
        "Conversion rate should be 0.25, got {}",
        conv_rate
    );

    // Verify event_subtype is persisted and queryable.
    let subtype_rows = engine
        .query_events(
            "products",
            "SELECT COUNT(*) as count FROM events WHERE event_type = 'conversion' AND event_subtype = 'purchase'",
        )
        .await
        .unwrap();
    assert_eq!(subtype_rows[0]["count"], 1);

    // Stage 2 depends on subtype-specific conversion metrics.
    let add_to_cart = engine
        .conversion_rate_for_subtype("products", &today, &today, "addToCart")
        .await
        .unwrap();
    assert_eq!(add_to_cart["addToCartCount"], 0);
    assert!(add_to_cart.get("conversionCount").is_none());
    assert_eq!(add_to_cart["rate"], 0.0);

    let purchase = engine
        .conversion_rate_for_subtype("products", &today, &today, "purchase")
        .await
        .unwrap();
    assert_eq!(purchase["purchaseCount"], 1);
    assert!(purchase.get("conversionCount").is_none());
    assert_eq!(purchase["trackedSearchCount"], 4);
    assert_eq!(purchase["rate"], 0.25);

    let result = engine
        .top_filters("products", &today, &today, 10)
        .await
        .unwrap();
    let filters = result["filters"].as_array().unwrap();
    assert_eq!(filters.len(), 2, "2 distinct filter strings");

    let result = engine
        .filter_values("products", "brand", &today, &today, 10)
        .await
        .unwrap();
    assert_eq!(result["attribute"], "brand");
    let values = result["values"].as_array().unwrap();
    assert!(values.len() >= 2);

    let result = engine
        .filters_no_results("products", &today, &today, 10)
        .await
        .unwrap();
    let filters = result["filters"].as_array().unwrap();
    assert_eq!(filters.len(), 1);
    assert_eq!(filters[0]["attribute"], "brand:Nonexistent");

    let result = engine
        .top_hits("products", &today, &today, 10)
        .await
        .unwrap();
    let hits = result["hits"].as_array().unwrap();
    assert!(!hits.is_empty());

    let result = engine.status("products").await.unwrap();
    assert_eq!(result["enabled"], true);
    assert_eq!(result["hasData"], true);
}

/// Verify that `lookup_query_id` resolves a recorded search by its hex query ID and returns `None` for unknown IDs.
#[tokio::test]
async fn query_id_correlation() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config);

    let qid = "abcdef0123456789abcdef0123456789".to_string();
    collector.record_search(search_event(
        "laptop",
        "products",
        42,
        Some(&qid),
        "alice",
        None,
    ));

    let entry = collector.lookup_query_id(&qid).unwrap();
    assert_eq!(entry.query, "laptop");
    assert_eq!(entry.index_name, "products");

    assert!(collector
        .lookup_query_id("0000000000000000000000000000000")
        .is_none());
}

/// Verify that setting `enabled: false` in `AnalyticsConfig` causes the collector to silently discard all recorded events, producing a zero search count after flush.
#[tokio::test]
async fn analytics_disabled_suppresses_recording() {
    let tmp = TempDir::new().unwrap();
    let config = AnalyticsConfig {
        enabled: false,
        data_dir: tmp.path().to_path_buf(),
        flush_interval_secs: 3600,
        flush_size: 1,
        retention_days: 90,
    };
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    collector.record_search(search_event("laptop", "products", 42, None, "alice", None));
    collector.flush_all();

    let result = engine
        .search_count("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(
        result["count"], 0,
        "Disabled analytics should record nothing"
    );
}

/// Verify that click events without a `query_id` are persisted and appear in `top_hits`, but do not inflate `trackedSearchCount` used as the CTR denominator.
#[tokio::test]
async fn non_correlated_events_recorded_but_dont_inflate_ctr() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid = "a".repeat(32);
    collector.record_search(search_event(
        "laptop",
        "products",
        42,
        Some(&qid),
        "alice",
        None,
    ));
    collector.record_insight(click_event(&qid, "products", "alice", vec![1]));

    let uncorrelated_click = InsightEvent {
        event_type: "click".to_string(),
        event_subtype: None,
        event_name: "Non-correlated Click".to_string(),
        index: "products".to_string(),
        user_token: "bob".to_string(),
        authenticated_user_token: None,
        query_id: None,
        object_ids: vec!["obj5".to_string()],
        object_ids_alt: vec![],
        positions: None,
        timestamp: Some(chrono::Utc::now().timestamp_millis()),
        value: None,
        currency: None,
        interleaving_team: None,
    };
    collector.record_insight(uncorrelated_click);

    collector.flush_all();

    let result = engine
        .click_through_rate("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["trackedSearchCount"], 1);
    assert_eq!(result["clickCount"], 2);

    let result = engine
        .top_hits("products", &today, &today, 10)
        .await
        .unwrap();
    let hits = result["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
}

/// Verify that `users_count_hll` returns a valid base64-encoded HLL sketch, deduplicates users across searches, and that merging independent sketches preserves approximate cardinality.
#[tokio::test]
async fn users_count_hll_returns_sketch_and_deduplicates() {
    use crate::analytics::hll::HllSketch;

    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    for user in &["alice", "bob", "charlie"] {
        collector.record_search(search_event("laptop", "products", 10, None, user, None));
    }
    collector.flush_all();

    let result = engine
        .users_count_hll("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["count"], 3);
    assert!(result["hll_sketch"].is_string());
    assert!(result["dates"].is_array());
    assert!(result["daily_sketches"].is_object());

    let sketch_b64 = result["hll_sketch"].as_str().unwrap();
    let sketch = HllSketch::from_base64(sketch_b64).expect("valid base64 sketch");
    assert_eq!(sketch.cardinality(), 3);

    let items_a: Vec<String> = vec!["alice".to_string(), "bob".to_string()];
    let items_b: Vec<String> = vec!["bob".to_string(), "charlie".to_string()];
    let s_a = HllSketch::from_items(items_a.iter().map(|s| s.as_str()));
    let s_b = HllSketch::from_items(items_b.iter().map(|s| s.as_str()));
    let merged = HllSketch::merge_all(&[s_a, s_b]);
    let card = merged.cardinality();
    assert!(
        (2..=4).contains(&card),
        "merged HLL should be ~3 (got {})",
        card
    );
}

/// Verify that `search_count` returns events only within the requested date range, excluding events written on other days.
#[tokio::test]
async fn date_range_filtering() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let engine = AnalyticsQueryEngine::new(config.clone());

    let events = vec![
        search_event("laptop", "products", 42, None, "alice", None),
        search_event("phone", "products", 18, None, "bob", None),
    ];
    let searches_dir = config.searches_dir("products");
    crate::analytics::writer::flush_search_events(&events, &searches_dir).unwrap();

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let result = engine
        .search_count("products", &today, &today)
        .await
        .unwrap();
    assert_eq!(result["count"], 2);

    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let result = engine
        .search_count("products", &yesterday, &yesterday)
        .await
        .unwrap();
    assert_eq!(result["count"], 0, "Yesterday should have no events");
}

/// Verify that internal fields (`hll_sketch`, `daily_sketches`) are present in the raw engine result but can be stripped before returning a public API response, leaving only `count` and `dates`.
#[tokio::test]
async fn users_count_hll_internal_fields_stripped_in_response() {
    use crate::analytics::hll::HllSketch;

    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    for i in 0..100 {
        collector.record_search(search_event(
            "laptop",
            "products",
            10,
            None,
            &format!("user_{:03}", i),
            None,
        ));
    }
    collector.flush_all();

    let engine_result = engine
        .users_count_hll("products", &today, &today)
        .await
        .unwrap();
    assert!(engine_result["hll_sketch"].is_string());
    assert!(engine_result["daily_sketches"].is_object());
    assert_eq!(engine_result["count"], 100);

    let b64 = engine_result["hll_sketch"].as_str().unwrap();
    let sketch = HllSketch::from_base64(b64).expect("valid sketch");
    assert_eq!(sketch.cardinality(), 100);

    let mut public_response = engine_result.clone();
    if let Some(obj) = public_response.as_object_mut() {
        obj.remove("hll_sketch");
        obj.remove("daily_sketches");
    }

    assert!(public_response.get("hll_sketch").is_none());
    assert!(public_response.get("daily_sketches").is_none());
    assert!(public_response["count"].is_number());
    assert!(public_response["dates"].is_array());
    assert_eq!(public_response["count"], 100);
}

/// Verify that all rate endpoints (`conversion_rate`, `click_through_rate`, `no_results_rate`, `no_click_rate`, and subtype variants) return `null` for the rate field when the denominator is zero, rather than dividing by zero or returning `0.0`.
#[tokio::test]
async fn rate_endpoints_return_null_when_denominator_is_zero() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let conversion = engine
        .conversion_rate("products", &today, &today)
        .await
        .unwrap();
    assert!(
        conversion["rate"].is_null(),
        "conversionRate should return null when trackedSearchCount=0"
    );
    assert_eq!(conversion["trackedSearchCount"], 0);

    let add_to_cart = engine
        .conversion_rate_for_subtype("products", &today, &today, "addToCart")
        .await
        .unwrap();
    assert!(
        add_to_cart["rate"].is_null(),
        "addToCartRate should return null when trackedSearchCount=0"
    );
    assert_eq!(add_to_cart["trackedSearchCount"], 0);

    let purchase = engine
        .conversion_rate_for_subtype("products", &today, &today, "purchase")
        .await
        .unwrap();
    assert!(
        purchase["rate"].is_null(),
        "purchaseRate should return null when trackedSearchCount=0"
    );
    assert_eq!(purchase["trackedSearchCount"], 0);

    let ctr = engine
        .click_through_rate("products", &today, &today)
        .await
        .unwrap();
    assert!(
        ctr["rate"].is_null(),
        "clickThroughRate should return null when trackedSearchCount=0"
    );
    assert_eq!(ctr["trackedSearchCount"], 0);

    let no_result = engine
        .no_results_rate("products", &today, &today)
        .await
        .unwrap();
    assert!(
        no_result["rate"].is_null(),
        "noResultRate should return null when count=0"
    );
    assert_eq!(no_result["count"], 0);

    let no_click = engine
        .no_click_rate("products", &today, &today)
        .await
        .unwrap();
    assert!(
        no_click["rate"].is_null(),
        "noClickRate should return null when trackedSearchCount=0"
    );
    assert_eq!(no_click["trackedSearchCount"], 0);
}

/// Verify that the revenue endpoint correctly sums purchase values, returns a per-currency breakdown, and includes a daily time-series with matching totals.
#[tokio::test]
async fn revenue_positive_case() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid1 = "a".repeat(32);
    let qid2 = "b".repeat(32);

    collector.record_search(search_event(
        "laptop",
        "products",
        42,
        Some(&qid1),
        "alice",
        None,
    ));
    collector.record_search(search_event(
        "laptop",
        "products",
        38,
        Some(&qid2),
        "bob",
        None,
    ));

    // Two USD purchase events with known values
    let mut conv1 = conversion_event(&qid1, "products", "alice");
    conv1.value = Some(49.99);
    conv1.currency = Some("USD".to_string());
    collector.record_insight(conv1);

    let mut conv2 = conversion_event(&qid2, "products", "bob");
    conv2.value = Some(25.01);
    conv2.currency = Some("USD".to_string());
    collector.record_insight(conv2);

    collector.flush_all();

    let result = engine.revenue("products", &today, &today).await.unwrap();

    // Top-level currencies map
    let currencies = result["currencies"].as_object().unwrap();
    assert!(currencies.contains_key("USD"));
    let usd = &currencies["USD"];
    assert_eq!(usd["currency"], "USD");
    let rev = usd["revenue"].as_f64().unwrap();
    assert!((rev - 75.0).abs() < 0.01, "Expected ~75.0, got {}", rev);

    // Daily breakdown
    let dates = result["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    let day = &dates[0];
    assert_eq!(day["date"], today);
    let day_currencies = day["currencies"].as_object().unwrap();
    let day_usd = &day_currencies["USD"];
    assert_eq!(day_usd["currency"], "USD");
    let day_rev = day_usd["revenue"].as_f64().unwrap();
    assert!((day_rev - 75.0).abs() < 0.01);
}

/// Verify that the revenue endpoint returns empty `currencies` and `dates` when no purchase events exist.
#[tokio::test]
async fn revenue_zero_data() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let result = engine.revenue("products", &today, &today).await.unwrap();

    let currencies = result["currencies"].as_object().unwrap();
    assert!(
        currencies.is_empty(),
        "No purchase events → empty currencies map"
    );
    let dates = result["dates"].as_array().unwrap();
    assert!(dates.is_empty(), "No purchase events → empty dates array");
}

/// Verify that the revenue endpoint aggregates purchase values separately per currency, returning independent totals for USD and EUR in both the top-level summary and daily breakdown.
#[tokio::test]
async fn revenue_multi_currency() {
    let tmp = TempDir::new().unwrap();
    let config = test_config(tmp.path());
    let collector = AnalyticsCollector::new(config.clone());
    let engine = AnalyticsQueryEngine::new(config);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let qid1 = "a".repeat(32);
    let qid2 = "b".repeat(32);
    let qid3 = "c".repeat(32);

    collector.record_search(search_event(
        "laptop",
        "products",
        42,
        Some(&qid1),
        "alice",
        None,
    ));
    collector.record_search(search_event(
        "laptop",
        "products",
        38,
        Some(&qid2),
        "bob",
        None,
    ));
    collector.record_search(search_event(
        "phone",
        "products",
        15,
        Some(&qid3),
        "charlie",
        None,
    ));

    let mut conv_usd = conversion_event(&qid1, "products", "alice");
    conv_usd.value = Some(99.99);
    conv_usd.currency = Some("USD".to_string());
    collector.record_insight(conv_usd);

    let mut conv_eur = conversion_event(&qid2, "products", "bob");
    conv_eur.value = Some(49.50);
    conv_eur.currency = Some("EUR".to_string());
    collector.record_insight(conv_eur);

    let mut conv_eur2 = conversion_event(&qid3, "products", "charlie");
    conv_eur2.value = Some(30.50);
    conv_eur2.currency = Some("EUR".to_string());
    collector.record_insight(conv_eur2);

    collector.flush_all();

    let result = engine.revenue("products", &today, &today).await.unwrap();

    let currencies = result["currencies"].as_object().unwrap();
    assert_eq!(currencies.len(), 2, "Should have USD and EUR");

    let usd = &currencies["USD"];
    assert_eq!(usd["currency"], "USD");
    assert!((usd["revenue"].as_f64().unwrap() - 99.99).abs() < 0.01);

    let eur = &currencies["EUR"];
    assert_eq!(eur["currency"], "EUR");
    assert!((eur["revenue"].as_f64().unwrap() - 80.0).abs() < 0.01);

    // Daily breakdown should also have both currencies
    let dates = result["dates"].as_array().unwrap();
    assert_eq!(dates.len(), 1);
    let day_currencies = dates[0]["currencies"].as_object().unwrap();
    assert_eq!(day_currencies.len(), 2);
}
