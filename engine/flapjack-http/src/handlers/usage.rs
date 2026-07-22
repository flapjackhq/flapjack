//! Algolia-compatible usage statistics endpoints that merge persisted counter
//! snapshots with current live counter and gauge values.
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use chrono::{NaiveDate, Utc};
use dashmap::DashMap;
use serde::Deserialize;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::AppState;
use crate::error_response::json_error_parts;
use crate::usage_middleware::TenantUsageCounters;
use crate::usage_persistence::{CapturedUsageGauges, DailyUsageSnapshot, IndexUsageSnapshot};

/// All supported Algolia usage statistic names.
const KNOWN_STATS: &[&str] = &[
    "search_operations",
    "total_write_operations",
    "total_read_operations",
    "records",
    "bytes_received",
    "search_results_total",
    "documents_deleted",
    "queries_operations",
    "multi_queries_operations",
    "documents_count",
    "storage_bytes",
];

fn is_gauge_stat(stat: &str) -> bool {
    matches!(stat, "documents_count" | "storage_bytes")
}

/// Query parameters accepted by all usage endpoints.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageParams {
    #[serde(default = "default_start_date")]
    pub start_date: String,
    #[serde(default = "default_end_date")]
    pub end_date: String,
    /// Granularity of returned data points (currently only "daily" is supported).
    #[serde(default = "default_granularity")]
    pub granularity: String,
}

fn default_start_date() -> String {
    (Utc::now() - chrono::Duration::days(8))
        .format("%Y-%m-%d")
        .to_string()
}

fn default_end_date() -> String {
    Utc::now().format("%Y-%m-%d").to_string()
}

fn default_granularity() -> String {
    "daily".to_string()
}

type HandlerError = (StatusCode, Json<serde_json::Value>);

/// Parse a comma-separated statistic string into validated canonical names.
/// Returns 400 if any name is unknown.
fn parse_statistics(raw: &str) -> Result<Vec<&'static str>, HandlerError> {
    raw.split(',')
        .map(str::trim)
        .map(|name| {
            KNOWN_STATS
                .iter()
                .copied()
                .find(|s| *s == name)
                .ok_or_else(|| {
                    json_error_parts(
                        StatusCode::BAD_REQUEST,
                        format!("Unknown statistic: {}", name),
                    )
                })
        })
        .collect()
}

/// Read one statistic value from a live counter entry.
fn stat_value(counters: &TenantUsageCounters, stat: &str) -> u64 {
    match stat {
        "search_operations" | "queries_operations" | "multi_queries_operations" => {
            counters.search_count.load(Ordering::Relaxed)
        }
        "total_write_operations" => counters.write_count.load(Ordering::Relaxed),
        "total_read_operations" => counters.read_count.load(Ordering::Relaxed),
        "records" => counters.documents_indexed_total.load(Ordering::Relaxed),
        "bytes_received" => counters.bytes_in.load(Ordering::Relaxed),
        "search_results_total" => counters.search_results_total.load(Ordering::Relaxed),
        "documents_deleted" => counters.documents_deleted_total.load(Ordering::Relaxed),
        _ => 0,
    }
}

/// Read one statistic value from a persisted index snapshot.
fn snapshot_stat(index_stat: &IndexUsageSnapshot, stat: &str) -> u64 {
    match stat {
        "search_operations" | "queries_operations" | "multi_queries_operations" => {
            index_stat.search_operations
        }
        "total_write_operations" => index_stat.total_write_operations,
        "total_read_operations" => index_stat.total_read_operations,
        "records" => index_stat.records,
        "bytes_received" => index_stat.bytes_received,
        "search_results_total" => index_stat.search_results_total,
        "documents_deleted" => index_stat.documents_deleted,
        _ => 0,
    }
}

/// Aggregate a counter stat from a daily snapshot, optionally filtered to one index.
/// Returns `None` when the index filter names an index not present in the snapshot.
fn snapshot_aggregate(
    snapshot: &DailyUsageSnapshot,
    stat: &str,
    index_filter: Option<&str>,
) -> Option<u64> {
    match index_filter {
        Some(idx) => snapshot.indexes.get(idx).map(|s| snapshot_stat(s, stat)),
        None => {
            if snapshot.indexes.is_empty() {
                None
            } else {
                Some(
                    snapshot
                        .indexes
                        .values()
                        .map(|s| snapshot_stat(s, stat))
                        .sum(),
                )
            }
        }
    }
}

/// Aggregate a gauge stat from a daily snapshot using `Option` semantics:
/// filtered queries return the index's option directly; global queries sum
/// only present values and return `None` when every index has `None`.
fn snapshot_aggregate_gauge(
    snapshot: &DailyUsageSnapshot,
    stat: &str,
    index_filter: Option<&str>,
) -> Option<u64> {
    match index_filter {
        Some(idx) => snapshot
            .indexes
            .get(idx)
            .and_then(|s| s.captured_gauges().get(stat)),
        None => {
            let values: Vec<u64> = snapshot
                .indexes
                .values()
                .filter_map(|s| s.captured_gauges().get(stat))
                .collect();
            if values.is_empty() {
                None
            } else {
                Some(values.into_iter().sum())
            }
        }
    }
}

/// Aggregate a stat value across all live counters, or for a single index.
/// Returns `None` when there are no counter entries matching the filter.
fn aggregate_stat(
    counters: &Arc<DashMap<String, TenantUsageCounters>>,
    stat: &str,
    index_filter: Option<&str>,
) -> Option<u64> {
    match index_filter {
        Some(idx) => counters.get(idx).map(|c| stat_value(&c, stat)),
        None => {
            if counters.is_empty() {
                None
            } else {
                Some(counters.iter().map(|c| stat_value(&c, stat)).sum())
            }
        }
    }
}

#[derive(Clone, Copy)]
struct LiveUsageValues<'a> {
    counters: &'a Arc<DashMap<String, TenantUsageCounters>>,
    gauges: Option<&'a CapturedUsageGauges>,
}

impl LiveUsageValues<'_> {
    fn get(&self, stat: &str, index_filter: Option<&str>) -> Option<u64> {
        if is_gauge_stat(stat) {
            self.gauges.and_then(|gauges| gauges.get(stat))
        } else {
            aggregate_stat(self.counters, stat, index_filter)
        }
    }
}

/// Query a single gauge stat from captured per-index gauge values.
fn gauge_from_captured(
    captured: &std::collections::HashMap<String, CapturedUsageGauges>,
    stat: &str,
    index_filter: Option<&str>,
) -> Option<u64> {
    match index_filter {
        Some(idx) => captured.get(idx).and_then(|g| g.get(stat)),
        None => {
            if captured.is_empty() {
                return None;
            }
            let values: Vec<u64> = captured.values().filter_map(|g| g.get(stat)).collect();
            if values.is_empty() {
                None
            } else {
                Some(values.into_iter().sum())
            }
        }
    }
}

/// Compute live gauge values by capturing from IndexManager + MetricsState.
fn live_gauge_values(
    state: &Arc<AppState>,
    stats: &[&str],
    index_filter: Option<&str>,
) -> CapturedUsageGauges {
    let storage_gauges = state
        .metrics_state
        .as_ref()
        .map(|ms| ms.storage_gauges.as_ref());
    let selection = crate::usage_capture::UsageGaugeSelection::from_statistics(stats);
    let captured = crate::usage_capture::capture_requested_live_gauges(
        &state.manager,
        storage_gauges,
        selection,
        index_filter,
    );

    CapturedUsageGauges {
        documents_count: gauge_from_captured(&captured, "documents_count", index_filter),
        storage_bytes: gauge_from_captured(&captured, "storage_bytes", index_filter),
    }
}

/// Build the Algolia-shaped usage response, merging historical snapshots with
/// live in-memory counters.
///
/// Historical days become one `{"t": midnight_epoch_ms, "v": value}` entry
/// each.  When `include_live` is true the current live counters are appended
/// as a final data point using the current timestamp.
fn build_response_merged(
    stats: &[&str],
    historical: &[(NaiveDate, DailyUsageSnapshot)],
    live_values: LiveUsageValues<'_>,
    include_live: bool,
    index_filter: Option<&str>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    let live_timestamp_ms = include_live.then(|| Utc::now().timestamp_millis());

    for stat in stats {
        let mut data_points: Vec<serde_json::Value> = Vec::new();
        let gauge = is_gauge_stat(stat);

        for (date, snapshot) in historical {
            let value = if gauge {
                snapshot_aggregate_gauge(snapshot, stat, index_filter)
            } else {
                snapshot_aggregate(snapshot, stat, index_filter)
            };
            if let Some(v) = value {
                let t = date
                    .and_hms_opt(0, 0, 0)
                    .expect("midnight is always valid")
                    .and_utc()
                    .timestamp_millis();
                data_points.push(serde_json::json!({"t": t, "v": v}));
            }
        }

        if include_live {
            if let Some(v) = live_values.get(stat, index_filter) {
                data_points.push(serde_json::json!({
                    "t": live_timestamp_ms.expect("live timestamp exists when include_live=true"),
                    "v": v
                }));
            }
        }

        map.insert((*stat).to_string(), serde_json::Value::Array(data_points));
    }

    serde_json::Value::Object(map)
}

/// Resolve query params into historical snapshots + a live flag, then build
/// the response.  Historical days are loaded from persistence (when available);
/// the live flag is set when `endDate` is today or later.
fn build_response_with_range(
    state: &Arc<AppState>,
    stats: &[&str],
    params: &UsageParams,
    index_filter: Option<&str>,
) -> Result<serde_json::Value, HandlerError> {
    let today = Utc::now().date_naive();

    let start = NaiveDate::parse_from_str(&params.start_date, "%Y-%m-%d")
        .unwrap_or_else(|_| today - chrono::Duration::days(8));
    let end = NaiveDate::parse_from_str(&params.end_date, "%Y-%m-%d").unwrap_or(today);
    if start > end {
        return Ok(build_response_merged(
            stats,
            &[],
            LiveUsageValues {
                counters: &state.usage_counters,
                gauges: None,
            },
            false,
            index_filter,
        ));
    }

    // Load historical snapshots for all completed days in the range.
    let historical: Vec<(NaiveDate, DailyUsageSnapshot)> = if let Some(persistence) =
        &state.usage_persistence
    {
        // Completed days are strictly before today.
        let hist_end = if end < today {
            end
        } else {
            match today.pred_opt() {
                Some(d) => d,
                None => {
                    let live_gauges = live_gauge_values(state, stats, index_filter);
                    let live_values = LiveUsageValues {
                        counters: &state.usage_counters,
                        gauges: Some(&live_gauges),
                    };
                    return Ok(build_response_merged(
                        stats,
                        &[],
                        live_values,
                        true,
                        index_filter,
                    ));
                }
            }
        };
        if start <= hist_end {
            persistence
                    .load_date_range(start, hist_end)
                    .map_err(|error| {
                        tracing::error!(
                            "Failed to load usage history (start_date={}, end_date={}, index_filter={:?}): {}",
                            params.start_date,
                            params.end_date,
                            index_filter,
                            error
                        );
                        json_error_parts(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "Failed to load usage history".to_string(),
                        )
                    })?
        } else {
            Vec::new()
        }
    } else {
        Vec::new()
    };

    let include_live = end >= today;
    let live_gauges = include_live.then(|| live_gauge_values(state, stats, index_filter));
    let live_values = LiveUsageValues {
        counters: &state.usage_counters,
        gauges: live_gauges.as_ref(),
    };

    Ok(build_response_merged(
        stats,
        &historical,
        live_values,
        include_live,
        index_filter,
    ))
}

/// `GET /1/usage/:statistic`
///
/// Returns usage statistics aggregated across all indexes.
/// `:statistic` may be a comma-separated list of statistic names.
#[utoipa::path(get, path = "/1/usage/{statistic}", tag = "usage",
    params(("statistic" = String, Path, description = "Statistic name (comma-separated)")),
    security(("api_key" = [])))]
pub async fn usage_global(
    State(state): State<Arc<AppState>>,
    Path(statistic): Path<String>,
    Query(params): Query<UsageParams>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    let stats = parse_statistics(&statistic)?;
    Ok(Json(build_response_with_range(
        &state, &stats, &params, None,
    )?))
}

/// `GET /1/usage/:statistic/:indexName`
///
/// Returns usage statistics filtered to a single index.
/// `:statistic` may be a comma-separated list of statistic names.
#[utoipa::path(get, path = "/1/usage/{statistic}/{indexName}", tag = "usage",
    params(
        ("statistic" = String, Path, description = "Statistic name (comma-separated)"),
        ("indexName" = String, Path, description = "Index name"),
    ),
    security(("api_key" = [])))]
pub async fn usage_per_index(
    State(state): State<Arc<AppState>>,
    Path((statistic, index_name)): Path<(String, String)>,
    Query(params): Query<UsageParams>,
) -> Result<Json<serde_json::Value>, HandlerError> {
    let stats = parse_statistics(&statistic)?;
    Ok(Json(build_response_with_range(
        &state,
        &stats,
        &params,
        Some(&index_name),
    )?))
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
