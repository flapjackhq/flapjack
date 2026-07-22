use super::*;
use crate::usage_persistence::{CapturedUsageGauges, UsagePersistence};
use std::collections::HashMap;

fn counters_with_search_count(
    index_name: &str,
    count: u64,
) -> dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> {
    let counters: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
        dashmap::DashMap::new();
    counters
        .entry(index_name.to_string())
        .or_default()
        .search_count
        .fetch_add(count, Ordering::Relaxed);
    counters
}

fn midnight_utc_ms(date: &str) -> i64 {
    chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis()
}

#[tokio::test]
async fn usage_historical_gauge_points_preserve_options_and_union_indexes() {
    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());
    let counters = counters_with_search_count("legacy", 9);
    let gauges = HashMap::from([
        (
            "products".to_string(),
            CapturedUsageGauges {
                documents_count: Some(12),
                storage_bytes: Some(100),
            },
        ),
        (
            "archive".to_string(),
            CapturedUsageGauges {
                documents_count: Some(0),
                storage_bytes: Some(5),
            },
        ),
    ]);
    persistence
        .save_snapshot_with_gauges("2026-02-24", &counters, &gauges)
        .unwrap();

    let state = make_state_with_persistence(&tmp, persistence);
    let app = usage_router(state);

    let resp = get_usage(
        app.clone(),
        "/1/usage/documents_count,storage_bytes?startDate=2026-02-24&endDate=2026-02-24",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["documents_count"][0]["v"].as_u64().unwrap(), 12);
    assert_eq!(json["storage_bytes"][0]["v"].as_u64().unwrap(), 105);

    let resp = get_usage(
        app.clone(),
        "/1/usage/documents_count/archive?startDate=2026-02-24&endDate=2026-02-24",
    )
    .await;
    let json = body_json(resp).await;
    let archive_docs = json["documents_count"].as_array().unwrap();
    assert_eq!(archive_docs.len(), 1);
    assert_eq!(archive_docs[0]["v"].as_u64().unwrap(), 0);

    let resp = get_usage(
        app,
        "/1/usage/documents_count/legacy?startDate=2026-02-24&endDate=2026-02-24",
    )
    .await;
    let json = body_json(resp).await;
    assert!(json["documents_count"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn usage_historical_gauge_range_appends_todays_live_point_once() {
    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());
    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let counters: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
        dashmap::DashMap::new();
    let gauges = HashMap::from([(
        "products".to_string(),
        CapturedUsageGauges {
            documents_count: Some(8),
            storage_bytes: None,
        },
    )]);
    persistence
        .save_snapshot_with_gauges(&yesterday, &counters, &gauges)
        .unwrap();

    let state = make_state_with_persistence(&tmp, persistence);
    seed_loaded_documents(&state, "products", 3).await;

    let app = usage_router(state);
    let path = format!("/1/usage/documents_count?startDate={yesterday}&endDate={today}");
    let resp = get_usage(app, &path).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["documents_count"].as_array().unwrap();
    assert_eq!(arr.len(), 2);
    assert_eq!(arr[0]["t"].as_i64().unwrap(), midnight_utc_ms(&yesterday));
    assert_eq!(arr[0]["v"].as_u64().unwrap(), 8);
    assert_eq!(arr[1]["v"].as_u64().unwrap(), 3);
}

/// Verify that a date range ending on today merges yesterday's persisted snapshot with today's live in-memory counters into two data points.
#[tokio::test]
async fn usage_date_range_includes_current_day_inmemory() {
    let tmp = TempDir::new().unwrap();
    let persistence = Arc::new(UsagePersistence::new(tmp.path()).unwrap());

    let yesterday = (chrono::Utc::now() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    let hist: dashmap::DashMap<String, crate::usage_middleware::TenantUsageCounters> =
        dashmap::DashMap::new();
    hist.entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(5, Ordering::Relaxed);
    persistence.save_snapshot(&yesterday, &hist).unwrap();

    let state = make_state_with_persistence(&tmp, persistence);
    state
        .usage_counters
        .entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(99, Ordering::Relaxed);

    let app = usage_router(state);
    let path = format!(
        "/1/usage/search_operations?startDate={}&endDate={}",
        yesterday, today
    );
    let resp = get_usage(app, &path).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert_eq!(
        arr.len(),
        2,
        "should return 1 historical + 1 live data point"
    );

    let values: Vec<u64> = arr.iter().map(|p| p["v"].as_u64().unwrap()).collect();
    assert!(values.contains(&5), "historical value must be present");
    assert!(values.contains(&99), "live value must be present");
}

/// Verify completed-day gauge ranges exclude today's live value.
#[tokio::test]
async fn usage_documents_count_completed_day_range_excludes_live_value() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);

    seed_loaded_documents(&state, "idx", 4).await;

    let app = usage_router(state);
    let resp = get_usage(
        app,
        "/1/usage/documents_count?startDate=2026-02-24&endDate=2026-02-24",
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["documents_count"].as_array().unwrap();
    assert!(
        arr.is_empty(),
        "endDate before today must not append today's live document count"
    );
}

/// Verify an invalid reversed date range does not leak live usage data.
#[tokio::test]
async fn usage_reversed_date_range_returns_no_points() {
    let tmp = TempDir::new().unwrap();
    let state = make_state(&tmp);
    state
        .usage_counters
        .entry("idx".to_string())
        .or_default()
        .search_count
        .fetch_add(9, Ordering::Relaxed);

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let tomorrow = (chrono::Utc::now() + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let app = usage_router(state);
    let path = format!("/1/usage/search_operations?startDate={tomorrow}&endDate={today}");
    let resp = get_usage(app, &path).await;
    assert_eq!(resp.status(), StatusCode::OK);

    let json = body_json(resp).await;
    let arr = json["search_operations"].as_array().unwrap();
    assert!(
        arr.is_empty(),
        "startDate after endDate must not append today's live counters"
    );
}
