//! Stub summary for io.rs.
use std::collections::HashMap;
use std::path::Path;

use sha2::{Digest, Sha256};

use super::aggregation::{aggregate_experiment_metrics, compute_pre_experiment_covariates};
use super::interleaving::compute_interleaving_metrics;
use super::types::{EventRow, ExperimentMetrics, InterleavingMetrics, PreSearchRow, SearchRow};
use crate::experiments::config::PrimaryMetric;

/// Read experiment metrics from analytics parquet files.
///
/// `index_names` should include all indexes involved (control + variant for Mode B).
pub async fn get_experiment_metrics(
    experiment_id: &str,
    index_names: &[&str],
    analytics_data_dir: &Path,
    winsorization_cap: Option<f64>,
) -> Result<ExperimentMetrics, String> {
    use datafusion::prelude::*;

    let ctx = SessionContext::new();

    // Collect search rows from all relevant indexes
    let mut all_searches: Vec<SearchRow> = Vec::new();
    let mut all_events: Vec<EventRow> = Vec::new();

    for index_name in index_names {
        let searches_dir = analytics_data_dir.join(index_name).join("searches");
        let events_dir = analytics_data_dir.join(index_name).join("events");

        // Read search events
        if searches_dir.exists() && has_parquet_files(&searches_dir) {
            let rows = read_search_rows(&ctx, &searches_dir, experiment_id).await?;
            all_searches.extend(rows);
        }

        // Read insight events
        if events_dir.exists() && has_parquet_files(&events_dir) {
            let rows = read_event_rows(&ctx, &events_dir).await?;
            all_events.extend(rows);
        }
    }

    Ok(aggregate_experiment_metrics(
        &all_searches,
        &all_events,
        winsorization_cap,
    ))
}

/// Read interleaving preference metrics from analytics parquet files.
///
/// Returns `None` if no interleaving click events are found.
pub async fn get_interleaving_metrics(
    index_names: &[&str],
    analytics_data_dir: &Path,
    experiment_id: &str,
) -> Result<Option<InterleavingMetrics>, String> {
    use datafusion::prelude::*;

    let ctx = SessionContext::new();
    let mut all_events: Vec<EventRow> = Vec::new();

    for index_name in index_names {
        let events_dir = analytics_data_dir.join(index_name).join("events");
        if events_dir.exists() && has_parquet_files(&events_dir) {
            let rows = read_event_rows(&ctx, &events_dir).await?;
            all_events.extend(rows);
        }
    }

    let metrics = compute_interleaving_metrics(&all_events, experiment_id);
    if metrics.total_queries == 0 {
        Ok(None)
    } else {
        Ok(Some(metrics))
    }
}

/// Check whether a directory (recursively) contains at least one `.parquet` file.
///
/// # Arguments
///
/// * `dir` - Root directory to scan.
///
/// # Returns
///
/// `true` if any descendant file has the `.parquet` extension.
fn has_parquet_files(dir: &Path) -> bool {
    /// Recursively check whether a directory tree contains at least one `.parquet` file.
    ///
    /// # Arguments
    ///
    /// * `dir` - Root directory to scan.
    ///
    /// # Returns
    ///
    /// `true` if any descendant file has the `.parquet` extension.
    fn check_dir(dir: &Path) -> bool {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return false,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if check_dir(&path) {
                    return true;
                }
            } else if path.extension().map(|e| e == "parquet").unwrap_or(false) {
                return true;
            }
        }
        false
    }
    check_dir(dir)
}

/// Register a parquet listing table in the DataFusion context, returning the generated table name.
async fn register_parquet_table(
    ctx: &datafusion::prelude::SessionContext,
    prefix: &str,
    dir: &Path,
) -> Result<String, String> {
    use datafusion::datasource::listing::ListingOptions;

    let table_name = listing_table_name(prefix, dir);
    let opts = ListingOptions::new(std::sync::Arc::new(
        datafusion::datasource::file_format::parquet::ParquetFormat::default(),
    ))
    .with_file_extension(".parquet")
    .with_collect_stat(false);

    ctx.register_listing_table(&table_name, &dir.to_string_lossy(), opts, None, None)
        .await
        .map_err(|e| format!("Failed to register {} table: {}", prefix, e))?;

    Ok(table_name)
}

/// Build a SQL-safe table name from a directory path by lowercasing alphanumeric chars
/// and replacing all other characters with underscores.
fn listing_table_name(prefix: &str, dir: &Path) -> String {
    let raw = dir.to_string_lossy();
    let readable = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    let readable = readable.trim_matches('_');
    let readable = if readable.is_empty() {
        "path".to_string()
    } else {
        readable.chars().take(48).collect()
    };
    let digest = Sha256::digest(raw.as_bytes());
    let suffix = hex::encode(&digest[..8]);

    format!("{prefix}_{readable}_{suffix}")
}

/// Read experiment search rows from parquet files, filtered by `experiment_id`.
///
/// # Arguments
///
/// * `ctx` - DataFusion session context for query execution.
/// * `searches_dir` - Directory containing search event parquet files.
/// * `experiment_id` - Only rows matching this experiment are returned.
///
/// # Returns
///
/// Parsed `SearchRow` vectors or an error string.
async fn read_search_rows(
    ctx: &datafusion::prelude::SessionContext,
    searches_dir: &Path,
    experiment_id: &str,
) -> Result<Vec<SearchRow>, String> {
    let table_name = register_parquet_table(ctx, "searches", searches_dir).await?;

    // Escape single quotes in experiment_id for safety
    let safe_id = experiment_id.replace('\'', "''");
    let sql = format!(
        "SELECT user_token, variant_id, query_id, nb_hits, has_results, assignment_method \
         FROM {} WHERE experiment_id = '{}'",
        table_name, safe_id
    );

    let df = ctx
        .sql(&sql)
        .await
        .map_err(|e| format!("SQL error: {}", e))?;
    let batches = df
        .collect()
        .await
        .map_err(|e| format!("Query execution error: {}", e))?;

    let mut rows = Vec::new();
    for batch in &batches {
        let user_token_col = batch.column_by_name("user_token").unwrap().clone();
        let variant_id_col = batch.column_by_name("variant_id").unwrap().clone();
        let query_id_col = batch.column_by_name("query_id").unwrap().clone();
        let nb_hits_col = batch.column_by_name("nb_hits").unwrap().clone();
        let has_results_col = batch.column_by_name("has_results").unwrap().clone();
        let assignment_method_col = batch.column_by_name("assignment_method").unwrap().clone();

        for i in 0..batch.num_rows() {
            let user_token = match arrow_helpers::get_string(&user_token_col, i) {
                Some(v) => v,
                None => continue,
            };
            let variant_id = match arrow_helpers::get_string(&variant_id_col, i) {
                Some(v) => v,
                None => continue,
            };
            let assignment_method = match arrow_helpers::get_string(&assignment_method_col, i) {
                Some(v) => v,
                None => continue,
            };
            rows.push(SearchRow {
                user_token,
                variant_id,
                query_id: arrow_helpers::get_string(&query_id_col, i),
                nb_hits: arrow_helpers::get_u32(&nb_hits_col, i),
                has_results: arrow_helpers::get_bool(&has_results_col, i),
                assignment_method,
            });
        }
    }

    Ok(rows)
}

/// Read insight event rows (clicks, conversions) from parquet files in the given directory.
///
/// Handles backward compatibility: older parquet files may lack `positions` and `interleaving_team` columns.
///
/// # Arguments
///
/// * `ctx` - DataFusion session context for query execution.
/// * `events_dir` - Directory containing insight event parquet files.
///
/// # Returns
///
/// Parsed `EventRow` vectors with optional position and team fields, or an error string.
async fn read_event_rows(
    ctx: &datafusion::prelude::SessionContext,
    events_dir: &Path,
) -> Result<Vec<EventRow>, String> {
    let table_name = register_parquet_table(ctx, "events", events_dir).await?;

    // Backward compatibility: older analytics parquet files may predate optional columns.
    let schema_fields: std::collections::HashSet<String> = ctx
        .table(&table_name)
        .await
        .map_err(|e| format!("Failed to inspect events table schema: {}", e))?
        .schema()
        .fields()
        .iter()
        .map(|field| field.name().clone())
        .collect();

    let has_positions = schema_fields.contains("positions");
    let has_interleaving_team = schema_fields.contains("interleaving_team");

    let mut columns = vec!["query_id", "event_type", "value"];
    if has_positions {
        columns.push("positions");
    }
    if has_interleaving_team {
        columns.push("interleaving_team");
    }
    let sql = format!(
        "SELECT {} FROM {} WHERE query_id IS NOT NULL",
        columns.join(", "),
        table_name
    );

    let df = ctx
        .sql(&sql)
        .await
        .map_err(|e| format!("SQL error: {}", e))?;
    let batches = df
        .collect()
        .await
        .map_err(|e| format!("Query execution error: {}", e))?;

    let mut rows = Vec::new();
    for batch in &batches {
        let query_id_col = batch.column_by_name("query_id").unwrap().clone();
        let event_type_col = batch.column_by_name("event_type").unwrap().clone();
        let value_col = batch.column_by_name("value").unwrap().clone();
        let positions_col = batch.column_by_name("positions").cloned();
        let interleaving_team_col = batch.column_by_name("interleaving_team").cloned();

        for i in 0..batch.num_rows() {
            let query_id = match arrow_helpers::get_string(&query_id_col, i) {
                Some(v) => v,
                None => continue,
            };
            rows.push(EventRow {
                query_id,
                event_type: arrow_helpers::get_string(&event_type_col, i).unwrap_or_default(),
                value: arrow_helpers::get_f64_opt(&value_col, i),
                positions: positions_col
                    .as_ref()
                    .and_then(|col| arrow_helpers::get_string(col, i)),
                interleaving_team: interleaving_team_col
                    .as_ref()
                    .and_then(|col| arrow_helpers::get_string(col, i)),
            });
        }
    }

    Ok(rows)
}

/// Read pre-experiment covariate data for CUPED variance reduction.
///
/// Queries analytics parquet files for the time window `[started_at - lookback_days, started_at)`
/// and returns per-user metric values for the specified primary metric.
///
/// Only the control index is queried (pre-experiment traffic on the same index).
pub async fn get_pre_experiment_covariates(
    index_name: &str,
    analytics_data_dir: &Path,
    metric: &PrimaryMetric,
    started_at_ms: i64,
    lookback_days: u32,
) -> Result<HashMap<String, f64>, String> {
    use datafusion::prelude::*;

    let lookback_ms = (lookback_days as i64) * 24 * 60 * 60 * 1000;
    let window_start = started_at_ms - lookback_ms;

    let ctx = SessionContext::new();

    let searches_dir = analytics_data_dir.join(index_name).join("searches");
    let events_dir = analytics_data_dir.join(index_name).join("events");

    let pre_searches = if searches_dir.exists() && has_parquet_files(&searches_dir) {
        read_pre_search_rows(&ctx, &searches_dir, window_start, started_at_ms).await?
    } else {
        Vec::new()
    };

    let pre_events = if events_dir.exists() && has_parquet_files(&events_dir) {
        read_event_rows(&ctx, &events_dir).await?
    } else {
        Vec::new()
    };

    Ok(compute_pre_experiment_covariates(
        &pre_searches,
        &pre_events,
        metric,
    ))
}

/// Read pre-experiment search rows within a timestamp window.
async fn read_pre_search_rows(
    ctx: &datafusion::prelude::SessionContext,
    searches_dir: &Path,
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<Vec<PreSearchRow>, String> {
    let table_name = register_parquet_table(ctx, "pre_searches", searches_dir).await?;

    let sql = format!(
        "SELECT user_token, query_id, nb_hits, has_results \
         FROM {} WHERE timestamp_ms >= {} AND timestamp_ms < {} \
         AND user_token IS NOT NULL",
        table_name, window_start_ms, window_end_ms
    );

    let df = ctx
        .sql(&sql)
        .await
        .map_err(|e| format!("SQL error: {}", e))?;
    let batches = df
        .collect()
        .await
        .map_err(|e| format!("Query execution error: {}", e))?;

    let mut rows = Vec::new();
    for batch in &batches {
        let user_token_col = batch.column_by_name("user_token").unwrap().clone();
        let query_id_col = batch.column_by_name("query_id").unwrap().clone();
        let nb_hits_col = batch.column_by_name("nb_hits").unwrap().clone();
        let has_results_col = batch.column_by_name("has_results").unwrap().clone();

        for i in 0..batch.num_rows() {
            let user_token = match arrow_helpers::get_string(&user_token_col, i) {
                Some(v) => v,
                None => continue,
            };
            rows.push(PreSearchRow {
                user_token,
                query_id: arrow_helpers::get_string(&query_id_col, i),
                nb_hits: arrow_helpers::get_u32(&nb_hits_col, i),
                has_results: arrow_helpers::get_bool(&has_results_col, i),
            });
        }
    }

    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::listing_table_name;
    use std::path::Path;
    /// TODO: Document listing_table_name_remains_sql_safe_for_hostile_paths.
    #[test]
    fn listing_table_name_remains_sql_safe_for_hostile_paths() {
        let suspicious =
            Path::new("/tmp/analytics/idx; drop table metrics --/searches with spaces");
        let other = Path::new("/tmp/analytics/idx:drop=table/searches");

        let suspicious_name = listing_table_name("searches", suspicious);
        let other_name = listing_table_name("searches", other);

        assert!(suspicious_name.starts_with("searches_"));
        assert!(suspicious_name
            .chars()
            .all(|ch: char| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'));
        assert_ne!(
            suspicious_name, other_name,
            "distinct paths must not collapse to the same DataFusion table name"
        );
    }
}

mod arrow_helpers {
    use arrow::array::Array;
    use arrow::datatypes::DataType;
    use std::sync::Arc;

    /// Extract a string value from any arrow string column type (Utf8, LargeUtf8, Utf8View).
    /// Returns None if the value is null.
    pub fn get_string(col: &Arc<dyn Array>, idx: usize) -> Option<String> {
        if col.is_null(idx) {
            return None;
        }
        match col.data_type() {
            DataType::Utf8 => {
                let arr = col
                    .as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap();
                Some(arr.value(idx).to_string())
            }
            DataType::LargeUtf8 => {
                let arr = col
                    .as_any()
                    .downcast_ref::<arrow::array::LargeStringArray>()
                    .unwrap();
                Some(arr.value(idx).to_string())
            }
            DataType::Utf8View => {
                let arr = col
                    .as_any()
                    .downcast_ref::<arrow::array::StringViewArray>()
                    .unwrap();
                Some(arr.value(idx).to_string())
            }
            _ => None,
        }
    }

    /// Extract a u32 value from a UInt32 column.
    pub fn get_u32(col: &Arc<dyn Array>, idx: usize) -> u32 {
        col.as_any()
            .downcast_ref::<arrow::array::UInt32Array>()
            .unwrap()
            .value(idx)
    }

    /// Extract a bool value from a Boolean column.
    pub fn get_bool(col: &Arc<dyn Array>, idx: usize) -> bool {
        col.as_any()
            .downcast_ref::<arrow::array::BooleanArray>()
            .unwrap()
            .value(idx)
    }

    /// Extract an optional f64 value from a Float64 column.
    pub fn get_f64_opt(col: &Arc<dyn Array>, idx: usize) -> Option<f64> {
        if col.is_null(idx) {
            return None;
        }
        Some(
            col.as_any()
                .downcast_ref::<arrow::array::Float64Array>()
                .unwrap()
                .value(idx),
        )
    }

    #[cfg(test)]
    mod tests {
        use super::get_string;
        use arrow::array::{ArrayRef, LargeStringArray, StringArray};
        use std::sync::Arc;

        #[test]
        fn get_string_supports_utf8_and_large_utf8() {
            let utf8: ArrayRef = Arc::new(StringArray::from(vec![Some("alpha"), None]));
            assert_eq!(get_string(&utf8, 0), Some("alpha".to_string()));
            assert_eq!(get_string(&utf8, 1), None);

            let large_utf8: ArrayRef = Arc::new(LargeStringArray::from(vec![Some("beta"), None]));
            assert_eq!(get_string(&large_utf8, 0), Some("beta".to_string()));
            assert_eq!(get_string(&large_utf8, 1), None);
        }
    }
}
