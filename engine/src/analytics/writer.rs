//! Stub summary for /Users/stuart/parallel_development/flapjack_dev/may31_eve_2_ha_snapshot_flake_verify/flapjack_dev/engine/src/analytics/writer.rs.
use arrow::array::{
    Array, ArrayRef, BinaryArray, BinaryBuilder, BooleanArray, BooleanBuilder, Float64Builder,
    Int64Array, Int64Builder, StringArray, StringBuilder, UInt32Array, UInt32Builder,
};
use arrow::compute::filter_record_batch;
use arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use super::config::AnalyticsConfig;
use super::hll::HllSketch;
use super::manifest::{RollupManifest, WindowEntry, WindowStatus};
use super::schema::{
    insight_event_schema, search_event_schema, search_rollup_schema, InsightEvent, SearchEvent,
};

static PARQUET_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
const DAY_MS: i64 = 86_400_000;

/// Write search events to a Parquet file with ZSTD compression.
pub fn flush_search_events(events: &[SearchEvent], dir: &Path) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }

    let path = partitioned_parquet_path(dir, "searches", chrono::Utc::now())?;

    let schema = search_event_schema();
    let batch = search_events_to_batch(events, &schema)?;

    write_parquet_file(&path, batch)?;
    Ok(())
}

/// Write insight events to a Parquet file with ZSTD compression.
pub fn flush_insight_events(events: &[InsightEvent], dir: &Path) -> Result<(), String> {
    if events.is_empty() {
        return Ok(());
    }

    let path = partitioned_parquet_path(dir, "events", chrono::Utc::now())?;

    let schema = insight_event_schema();
    let batch = insight_events_to_batch(events, &schema)?;

    write_parquet_file(&path, batch)?;
    Ok(())
}

/// Aggregate the events in a single closed time window into a pre-computed rollup
/// Parquet file (one row per query) for the given index/tier and record the window
/// in the rollup manifest.
///
/// The window is `[window_start_ms, window_end_ms)` and `tier` selects the rollup
/// granularity:
/// - `"1hour"` aggregates raw search events from `AnalyticsConfig::searches_dir`.
/// - `"1day"` compacts already-written `"1hour"` rollup rows whose
///   `window_start_ms` falls in `[window_start_ms, window_end_ms)`, merging HLL
///   sketches register-wise via `HllSketch::merge`.
///
/// The destination Parquet file is named deterministically by `(tier, start_ms)`
/// so reruns overwrite the same artifact. The matching manifest entry is also
/// replaced in place via `RollupManifest::record_window`.
///
/// Returns the path of the written rollup Parquet file on success.
pub fn flush_rollup_window(
    config: &AnalyticsConfig,
    index_name: &str,
    tier: &str,
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<PathBuf, String> {
    flush_rollup_window_with_event_count(config, index_name, tier, window_start_ms, window_end_ms)
        .map(|(path, _event_count)| path)
}

pub fn flush_rollup_window_with_event_count(
    config: &AnalyticsConfig,
    index_name: &str,
    tier: &str,
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<(PathBuf, i64), String> {
    validate_rollup_window(tier, window_start_ms, window_end_ms)?;

    let daily_hourly_sources = if tier == "1day" {
        Some(require_canonical_hourly_source_coverage(
            config,
            index_name,
            window_start_ms,
        )?)
    } else {
        None
    };

    let (aggregates, event_count) = match (tier, daily_hourly_sources.as_ref()) {
        ("1hour", _) => {
            collect_hourly_aggregates(config, index_name, window_start_ms, window_end_ms)?
        }
        ("1day", Some(hourly_sources)) => {
            collect_daily_aggregates(hourly_sources, window_start_ms, window_end_ms)?
        }
        (other, _) => return Err(format!("unsupported rollup tier: {}", other)),
    };

    let batch = aggregates_to_rollup_batch(window_start_ms, window_end_ms, aggregates)?;

    let tier_dir = config.rollups_dir(index_name, tier);
    fs::create_dir_all(&tier_dir)
        .map_err(|e| format!("Failed to create rollup dir {}: {}", tier_dir.display(), e))?;
    let filename = rollup_window_filename(tier, window_start_ms);
    let parquet_path = tier_dir.join(&filename);
    let backup_path = stage_existing_rollup_backup(&parquet_path)?;
    if let Err(error) = write_parquet_file_atomic(&parquet_path, batch) {
        if let Err(rollback_error) =
            rollback_rollup_file_replacement(&parquet_path, backup_path.as_deref())
        {
            return Err(format!("{error}; rollback failed: {rollback_error}"));
        }
        return Err(error);
    }

    let manifest_path = config.rollup_manifest_path(index_name);
    let mut manifest = match RollupManifest::load(&manifest_path) {
        Ok(manifest) => manifest,
        Err(error) => {
            rollback_rollup_file_replacement(&parquet_path, backup_path.as_deref()).map_err(
                |rollback_error| {
                    format!(
                        "Failed to load rollup manifest: {}; rollback failed: {}",
                        error, rollback_error
                    )
                },
            )?;
            return Err(format!("Failed to load rollup manifest: {}", error));
        }
    };
    let date = window_start_utc_date(window_start_ms);
    if let Err(error) = manifest.record_window(
        tier,
        &date,
        WindowEntry {
            start_ms: window_start_ms,
            end_ms: window_end_ms,
            status: WindowStatus::Closed,
            event_count,
            file: filename,
        },
        &tier_dir,
    ) {
        rollback_rollup_file_replacement(&parquet_path, backup_path.as_deref()).map_err(
            |rollback_error| {
                format!(
                    "Failed to record manifest window: {}; rollback failed: {}",
                    error, rollback_error
                )
            },
        )?;
        return Err(format!("Failed to record manifest window: {}", error));
    }
    if let Err(error) = manifest.save(&manifest_path) {
        rollback_rollup_file_replacement(&parquet_path, backup_path.as_deref()).map_err(
            |rollback_error| {
                format!(
                    "Failed to save rollup manifest: {}; rollback failed: {}",
                    error, rollback_error
                )
            },
        )?;
        return Err(format!("Failed to save rollup manifest: {}", error));
    }
    drop_rollup_backup(backup_path.as_deref())?;

    Ok((parquet_path, event_count))
}

fn require_canonical_hourly_source_coverage(
    config: &AnalyticsConfig,
    index_name: &str,
    day_start_ms: i64,
) -> Result<Vec<CertifiedHourlySource>, String> {
    let manifest_path = config.rollup_manifest_path(index_name);
    let manifest = RollupManifest::load(&manifest_path)
        .map_err(|e| format!("Failed to load rollup manifest: {}", e))?;
    let date = window_start_utc_date(day_start_ms);
    if !manifest.has_certified_coverage(&date, "1hour") {
        return Err(format!(
            "missing canonical hourly coverage for {} (tier=1hour) before 1day compaction",
            date
        ));
    }

    let hourly_state = manifest
        .tiers
        .get("1hour")
        .and_then(|tier| tier.dates.get(&date))
        .ok_or_else(|| format!("hourly date state missing after certification for {}", date))?;
    let hourly_dir = config.rollups_dir(index_name, "1hour");
    let mut hourly_paths = Vec::with_capacity(hourly_state.windows.len());
    for window in &hourly_state.windows {
        let path = manifest_hourly_source_path(&hourly_dir, &window.file)?;
        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            format!(
                "certified hourly source file missing for {} window {}: {} ({})",
                date,
                window.start_ms,
                path.display(),
                e
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "certified hourly source file must not be a symlink for {} window {}: {}",
                date,
                window.start_ms,
                path.display()
            ));
        }
        if !metadata.is_file() {
            return Err(format!(
                "certified hourly source file missing for {} window {}: {}",
                date,
                window.start_ms,
                path.display()
            ));
        }
        let file = fs::File::open(&path).map_err(|e| {
            format!(
                "failed to open certified hourly source file {}: {}",
                path.display(),
                e
            )
        })?;
        ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| {
            format!(
                "failed to read certified hourly parquet metadata {}: {}",
                path.display(),
                e
            )
        })?;
        hourly_paths.push(CertifiedHourlySource {
            path,
            start_ms: window.start_ms,
            end_ms: window.end_ms,
            event_count: window.event_count,
        });
    }
    Ok(hourly_paths)
}

/// Refuse manifest-provided filenames that would escape the canonical hourly
/// rollup directory. Daily compaction should only trust plain basenames that
/// the writer itself emitted, never traversal segments or absolute paths.
fn manifest_hourly_source_path(hourly_dir: &Path, file_name: &str) -> Result<PathBuf, String> {
    let candidate = Path::new(file_name);
    let mut components = candidate.components();
    match (components.next(), components.next()) {
        (Some(std::path::Component::Normal(_)), None) => Ok(hourly_dir.join(candidate)),
        _ => Err(format!(
            "invalid certified hourly source filename '{}': must stay inside {}",
            file_name,
            hourly_dir.display()
        )),
    }
}

#[derive(Debug)]
struct CertifiedHourlySource {
    path: PathBuf,
    start_ms: i64,
    end_ms: i64,
    event_count: i64,
}

fn validate_rollup_window(
    tier: &str,
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<(), String> {
    validate_rollup_window_with_hour_window_ms(
        tier,
        window_start_ms,
        window_end_ms,
        super::resolved_hourly_rollup_window_ms(),
    )
}

fn validate_rollup_window_with_hour_window_ms(
    tier: &str,
    window_start_ms: i64,
    window_end_ms: i64,
    expected_hour_window_ms: i64,
) -> Result<(), String> {
    if window_end_ms <= window_start_ms {
        return Err(format!(
            "invalid rollup window: end_ms ({}) must be > start_ms ({})",
            window_end_ms, window_start_ms
        ));
    }
    let duration = window_end_ms - window_start_ms;
    match tier {
        "1hour" => {
            // PL-9: background_tasks.rs can drive shorter test-only hourly rollup
            // windows via FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS. Validation must
            // honor the same width or the writer rejects the HTTP layer's
            // minute-granularity override before any rollup parquet is emitted.
            if duration != expected_hour_window_ms {
                return Err(format!(
                    "invalid 1hour rollup window duration: expected {} ms, got {} ms",
                    expected_hour_window_ms, duration
                ));
            }
            if window_start_ms.rem_euclid(expected_hour_window_ms) != 0 {
                return Err(
                    "invalid 1hour rollup window: start_ms must align to the configured rollup boundary"
                        .to_string(),
                );
            }
        }
        "1day" => {
            if duration != DAY_MS {
                return Err(format!(
                    "invalid 1day rollup window duration: expected {} ms, got {} ms",
                    DAY_MS, duration
                ));
            }
            if window_start_ms.rem_euclid(DAY_MS) != 0 {
                return Err(
                    "invalid 1day rollup window: start_ms must align to UTC day boundary"
                        .to_string(),
                );
            }
        }
        _ => {}
    }
    Ok(())
}

/// Per-query rollup aggregate (intermediate, in-memory).
///
/// Mirrors the columns of `search_rollup_schema` plus an optional HLL sketch
/// that collects the distinct user tokens observed in the window for the query.
#[derive(Default)]
struct QueryAggregate {
    count: i64,
    nb_hits_sum: i64,
    nb_hits_count: i64,
    no_results_count: i64,
    has_results_count: i64,
    hll: Option<HllSketch>,
}

impl QueryAggregate {
    fn add_user(&mut self, token: &str) {
        self.hll.get_or_insert_with(HllSketch::new).add(token);
    }

    fn merge_user_hll_bytes(&mut self, bytes: &[u8], query: &str) -> Result<(), String> {
        let other = HllSketch::from_bytes(bytes).ok_or_else(|| {
            format!(
                "invalid unique_users_hll payload for query '{}' in certified hourly rollup",
                query
            )
        })?;
        self.hll.get_or_insert_with(HllSketch::new).merge(&other);
        Ok(())
    }
}

/// Build per-query hourly aggregates by scanning raw search Parquet files under
/// `AnalyticsConfig::searches_dir(index_name)` for events whose `timestamp_ms`
/// falls in `[window_start_ms, window_end_ms)`. Returns the aggregate map plus
/// the total event count observed in the window.
fn collect_hourly_aggregates(
    config: &AnalyticsConfig,
    index_name: &str,
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<(HashMap<String, QueryAggregate>, i64), String> {
    let searches_dir = config.searches_dir(index_name);
    let mut aggregates: HashMap<String, QueryAggregate> = HashMap::new();
    let mut event_count: i64 = 0;

    if !searches_dir.exists() {
        return Ok((aggregates, event_count));
    }

    for file_path in collect_parquet_files(&searches_dir)? {
        accumulate_search_file(
            &file_path,
            window_start_ms,
            window_end_ms,
            &mut aggregates,
            &mut event_count,
        )?;
    }
    Ok((aggregates, event_count))
}

/// Read a single raw-search Parquet file and fold its in-window rows into
/// `aggregates` / `event_count`.
fn accumulate_search_file(
    path: &Path,
    window_start_ms: i64,
    window_end_ms: i64,
    aggregates: &mut HashMap<String, QueryAggregate>,
    event_count: &mut i64,
) -> Result<(), String> {
    let file = fs::File::open(path)
        .map_err(|e| format!("Failed to open search parquet {}: {}", path.display(), e))?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| {
            format!(
                "Failed to read search parquet metadata {}: {}",
                path.display(),
                e
            )
        })?
        .build()
        .map_err(|e| {
            format!(
                "Failed to build search parquet reader {}: {}",
                path.display(),
                e
            )
        })?;

    for batch_result in reader {
        let batch = batch_result.map_err(|e| {
            format!(
                "Failed to read search parquet batch {}: {}",
                path.display(),
                e
            )
        })?;
        accumulate_search_batch(
            &batch,
            window_start_ms,
            window_end_ms,
            aggregates,
            event_count,
        )?;
    }
    Ok(())
}

/// Apply one raw-search `RecordBatch` to the in-progress aggregate map.
fn accumulate_search_batch(
    batch: &RecordBatch,
    window_start_ms: i64,
    window_end_ms: i64,
    aggregates: &mut HashMap<String, QueryAggregate>,
    event_count: &mut i64,
) -> Result<(), String> {
    let timestamp = downcast_col::<Int64Array>(batch, "timestamp_ms")?;
    let query = downcast_col::<StringArray>(batch, "query")?;
    let nb_hits = downcast_col::<UInt32Array>(batch, "nb_hits")?;
    let has_results = downcast_col::<BooleanArray>(batch, "has_results")?;
    let user_token = downcast_col::<StringArray>(batch, "user_token")?;

    for row in 0..batch.num_rows() {
        let ts = timestamp.value(row);
        if ts < window_start_ms || ts >= window_end_ms {
            continue;
        }
        *event_count += 1;
        let q = query.value(row);
        let agg = aggregates.entry(q.to_string()).or_default();
        agg.count += 1;
        agg.nb_hits_sum += nb_hits.value(row) as i64;
        agg.nb_hits_count += 1;
        if has_results.value(row) {
            agg.has_results_count += 1;
        } else {
            agg.no_results_count += 1;
        }
        if !user_token.is_null(row) {
            agg.add_user(user_token.value(row));
        }
    }
    Ok(())
}

/// Build per-query daily aggregates by reading every `"1hour"` rollup Parquet
/// file under the index's hourly rollup dir and merging rows whose
/// `window_start_ms` falls in the daily window. Distinct-user sketches are
/// unioned register-wise via `HllSketch::merge` rather than recomputed from raw
/// events — daily compaction must be reproducible from already-written hourly
/// rollups alone.
fn collect_daily_aggregates(
    hourly_parquet_paths: &[CertifiedHourlySource],
    window_start_ms: i64,
    window_end_ms: i64,
) -> Result<(HashMap<String, QueryAggregate>, i64), String> {
    let mut aggregates: HashMap<String, QueryAggregate> = HashMap::new();
    let mut event_count: i64 = 0;

    for source in hourly_parquet_paths {
        accumulate_hourly_rollup_file(
            source,
            window_start_ms,
            window_end_ms,
            &mut aggregates,
            &mut event_count,
        )?;
    }
    Ok((aggregates, event_count))
}

/// Read a single hourly-rollup Parquet file and fold its in-window rows into
/// `aggregates` / `event_count` for daily compaction.
fn accumulate_hourly_rollup_file(
    source: &CertifiedHourlySource,
    window_start_ms: i64,
    window_end_ms: i64,
    aggregates: &mut HashMap<String, QueryAggregate>,
    event_count: &mut i64,
) -> Result<(), String> {
    let file = fs::File::open(&source.path).map_err(|e| {
        format!(
            "Failed to open rollup parquet {}: {}",
            source.path.display(),
            e
        )
    })?;
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| {
            format!(
                "Failed to read rollup parquet metadata {}: {}",
                source.path.display(),
                e
            )
        })?
        .build()
        .map_err(|e| {
            format!(
                "Failed to build rollup parquet reader {}: {}",
                source.path.display(),
                e
            )
        })?;

    let mut source_event_count: i64 = 0;
    for batch_result in reader {
        let batch = batch_result.map_err(|e| {
            format!(
                "Failed to read rollup parquet batch {}: {}",
                source.path.display(),
                e
            )
        })?;
        accumulate_hourly_rollup_batch(
            &batch,
            source,
            window_start_ms,
            window_end_ms,
            aggregates,
            event_count,
            &mut source_event_count,
        )?;
    }
    if source_event_count != source.event_count {
        return Err(format!(
            "certified hourly source {} event_count mismatch: manifest={}, parquet={}",
            source.path.display(),
            source.event_count,
            source_event_count
        ));
    }
    Ok(())
}

/// Apply one hourly-rollup `RecordBatch` to the in-progress daily aggregate map.
fn accumulate_hourly_rollup_batch(
    batch: &RecordBatch,
    source: &CertifiedHourlySource,
    window_start_ms: i64,
    window_end_ms: i64,
    aggregates: &mut HashMap<String, QueryAggregate>,
    event_count: &mut i64,
    source_event_count: &mut i64,
) -> Result<(), String> {
    let starts = downcast_col::<Int64Array>(batch, "window_start_ms")?;
    let ends = downcast_col::<Int64Array>(batch, "window_end_ms")?;
    let query = downcast_col::<StringArray>(batch, "query")?;
    let count = downcast_col::<Int64Array>(batch, "count")?;
    let nb_hits_sum = downcast_col::<Int64Array>(batch, "nb_hits_sum")?;
    let nb_hits_count = downcast_col::<Int64Array>(batch, "nb_hits_count")?;
    let no_results_count = downcast_col::<Int64Array>(batch, "no_results_count")?;
    let has_results_count = downcast_col::<Int64Array>(batch, "has_results_count")?;
    let hll = downcast_col::<BinaryArray>(batch, "unique_users_hll")?;

    for row in 0..batch.num_rows() {
        let ws = starts.value(row);
        let we = ends.value(row);
        if ws != source.start_ms || we != source.end_ms {
            return Err(format!(
                "certified hourly source {} contains row window [{}, {}) but manifest requires [{}, {})",
                source.path.display(),
                ws,
                we,
                source.start_ms,
                source.end_ms
            ));
        }
        if ws < window_start_ms || ws >= window_end_ms || we > window_end_ms {
            return Err(format!(
                "certified hourly source {} row window [{}, {}) is outside daily window [{}, {})",
                source.path.display(),
                ws,
                we,
                window_start_ms,
                window_end_ms
            ));
        }
        let q = query.value(row);
        let row_count = count.value(row);
        *event_count += row_count;
        *source_event_count += row_count;
        let agg = aggregates.entry(q.to_string()).or_default();
        agg.count += row_count;
        agg.nb_hits_sum += nb_hits_sum.value(row);
        agg.nb_hits_count += nb_hits_count.value(row);
        agg.no_results_count += no_results_count.value(row);
        agg.has_results_count += has_results_count.value(row);
        if !hll.is_null(row) {
            agg.merge_user_hll_bytes(hll.value(row), q)?;
        }
    }
    Ok(())
}

/// Materialize the per-query aggregate map as a `search_rollup_schema()`
/// `RecordBatch`. Rows are sorted by query text for deterministic on-disk order.
fn aggregates_to_rollup_batch(
    window_start_ms: i64,
    window_end_ms: i64,
    aggregates: HashMap<String, QueryAggregate>,
) -> Result<RecordBatch, String> {
    let mut rows: Vec<(String, QueryAggregate)> = aggregates.into_iter().collect();
    rows.sort_by(|a, b| a.0.cmp(&b.0));
    let n = rows.len();

    let mut window_start_b = Int64Builder::with_capacity(n);
    let mut window_end_b = Int64Builder::with_capacity(n);
    let mut query_b = StringBuilder::with_capacity(n, n * 16);
    let mut count_b = Int64Builder::with_capacity(n);
    let mut nb_hits_sum_b = Int64Builder::with_capacity(n);
    let mut nb_hits_count_b = Int64Builder::with_capacity(n);
    let mut no_results_count_b = Int64Builder::with_capacity(n);
    let mut has_results_count_b = Int64Builder::with_capacity(n);
    let mut hll_b = BinaryBuilder::with_capacity(n, n.saturating_mul(16_384));

    for (query, agg) in &rows {
        window_start_b.append_value(window_start_ms);
        window_end_b.append_value(window_end_ms);
        query_b.append_value(query);
        count_b.append_value(agg.count);
        nb_hits_sum_b.append_value(agg.nb_hits_sum);
        nb_hits_count_b.append_value(agg.nb_hits_count);
        no_results_count_b.append_value(agg.no_results_count);
        has_results_count_b.append_value(agg.has_results_count);
        match &agg.hll {
            Some(sketch) => hll_b.append_value(sketch.to_bytes()),
            None => hll_b.append_null(),
        }
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(window_start_b.finish()),
        Arc::new(window_end_b.finish()),
        Arc::new(query_b.finish()),
        Arc::new(count_b.finish()),
        Arc::new(nb_hits_sum_b.finish()),
        Arc::new(nb_hits_count_b.finish()),
        Arc::new(no_results_count_b.finish()),
        Arc::new(has_results_count_b.finish()),
        Arc::new(hll_b.finish()),
    ];

    RecordBatch::try_new(search_rollup_schema(), columns)
        .map_err(|e| format!("Rollup RecordBatch error: {}", e))
}

/// Downcast a named column to the requested Arrow array type, returning a
/// descriptive error if the column is missing or has the wrong type.
fn downcast_col<'a, T: Array + 'static>(
    batch: &'a RecordBatch,
    name: &str,
) -> Result<&'a T, String> {
    let idx = batch
        .schema()
        .index_of(name)
        .map_err(|_| format!("column '{}' missing from batch", name))?;
    batch
        .column(idx)
        .as_any()
        .downcast_ref::<T>()
        .ok_or_else(|| format!("column '{}' has unexpected type", name))
}

/// Deterministic single-file name for a rollup window. Keyed by `(tier, start)`
/// so reruns of the same window overwrite the prior artifact instead of
/// accumulating duplicates.
fn rollup_window_filename(tier: &str, window_start_ms: i64) -> String {
    format!("rollup_{}_{}.parquet", tier, window_start_ms)
}

fn unique_rollup_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.with_extension(format!("parquet.{}.{}", now_ns, suffix))
}

fn stage_existing_rollup_backup(path: &Path) -> Result<Option<PathBuf>, String> {
    if !path.exists() {
        return Ok(None);
    }

    let backup_path = unique_rollup_sidecar_path(path, "bak");
    fs::rename(path, &backup_path).map_err(|error| {
        format!(
            "Failed to stage existing rollup parquet {} at {}: {}",
            path.display(),
            backup_path.display(),
            error
        )
    })?;
    Ok(Some(backup_path))
}

fn rollback_rollup_file_replacement(path: &Path, backup_path: Option<&Path>) -> Result<(), String> {
    remove_file_if_exists(path)?;
    if let Some(backup_path) = backup_path {
        fs::rename(backup_path, path).map_err(|error| {
            format!(
                "Failed to restore rollup parquet {} from backup {}: {}",
                path.display(),
                backup_path.display(),
                error
            )
        })?;
    }
    Ok(())
}

fn drop_rollup_backup(backup_path: Option<&Path>) -> Result<(), String> {
    if let Some(backup_path) = backup_path {
        remove_file_if_exists(backup_path)?;
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "Failed to remove file {}: {}",
            path.display(),
            error
        )),
    }
}

/// UTC date string (YYYY-MM-DD) for the start of a rollup window, used as the
/// per-date manifest bucket key.
fn window_start_utc_date(window_start_ms: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp_millis(window_start_ms)
        .expect("valid timestamp ms")
        .format("%Y-%m-%d")
        .to_string()
}

/// Build a date-partitioned parquet file path (`<dir>/date=YYYY-MM-DD/<prefix>_<ts>_<pid>_<seq>.parquet`),
/// creating the partition directory if needed.
fn partitioned_parquet_path(
    dir: &Path,
    prefix: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<PathBuf, String> {
    let partition_dir = dir.join(format!("date={}", now.format("%Y-%m-%d")));
    fs::create_dir_all(&partition_dir).map_err(|e| format!("Failed to create dir: {}", e))?;

    let timestamp_ms = now.timestamp_millis();
    let sequence = PARQUET_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let filename = format!(
        "{}_{}_{}_{}.parquet",
        prefix,
        timestamp_ms,
        std::process::id(),
        sequence
    );

    Ok(partition_dir.join(filename))
}

/// Write a single `RecordBatch` to a Parquet file using ZSTD compression and a 100 000-row group size.
///
/// # Arguments
///
/// * `path` - Destination file path; created or truncated.
/// * `batch` - The Arrow record batch to write.
pub(crate) fn write_parquet_file(path: &Path, batch: RecordBatch) -> Result<(), String> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_max_row_group_size(100_000)
        .build();

    let file =
        fs::File::create(path).map_err(|e| format!("Failed to create parquet file: {}", e))?;
    let mut writer = ArrowWriter::try_new(file, batch.schema(), Some(props))
        .map_err(|e| format!("Failed to create arrow writer: {}", e))?;

    writer
        .write(&batch)
        .map_err(|e| format!("Failed to write batch: {}", e))?;
    writer
        .close()
        .map_err(|e| format!("Failed to close writer: {}", e))?;

    Ok(())
}

fn write_parquet_file_atomic(path: &Path, batch: RecordBatch) -> Result<(), String> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = path.with_extension(format!("parquet.{}.tmp", now_ns));
    write_parquet_file(&tmp_path, batch)?;
    fs::File::open(&tmp_path)
        .and_then(|file| file.sync_all())
        .map_err(|e| {
            format!(
                "Failed to sync temp parquet file {} before rename: {}",
                tmp_path.display(),
                e
            )
        })?;
    fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "Failed to atomically replace parquet file {} with {}: {}",
            path.display(),
            tmp_path.display(),
            e
        )
    })?;
    if let Some(parent) = path.parent() {
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(|e| {
                format!(
                    "Failed to sync parquet directory {} after rename: {}",
                    parent.display(),
                    e
                )
            })?;
    }
    Ok(())
}

/// Convert a slice of search events into an Arrow `RecordBatch`.
///
/// Maps each `SearchEvent` field to a typed Arrow array column, preserving nullability for optional fields.
///
/// # Arguments
///
/// * `events` - Search events to convert.
/// * `schema` - Arrow schema that defines column order and types (must match `search_event_schema()`).
///
/// # Returns
///
/// A `RecordBatch` ready for Parquet serialization, or a descriptive error string.
pub(crate) fn search_events_to_batch(
    events: &[SearchEvent],
    schema: &Arc<arrow::datatypes::Schema>,
) -> Result<RecordBatch, String> {
    let len = events.len();
    let mut timestamp_ms = Int64Builder::with_capacity(len);
    let mut query = StringBuilder::with_capacity(len, len * 20);
    let mut query_id = StringBuilder::with_capacity(len, len * 32);
    let mut index_name = StringBuilder::with_capacity(len, len * 20);
    let mut nb_hits = UInt32Builder::with_capacity(len);
    let mut processing_time_ms = UInt32Builder::with_capacity(len);
    let mut user_token = StringBuilder::with_capacity(len, len * 20);
    let mut user_ip = StringBuilder::with_capacity(len, len * 15);
    let mut filters = StringBuilder::with_capacity(len, len * 30);
    let mut facets = StringBuilder::with_capacity(len, len * 30);
    let mut analytics_tags = StringBuilder::with_capacity(len, len * 20);
    let mut page = UInt32Builder::with_capacity(len);
    let mut hits_per_page = UInt32Builder::with_capacity(len);
    let mut has_results = BooleanBuilder::with_capacity(len);
    let mut country = StringBuilder::with_capacity(len, len * 2);
    let mut region = StringBuilder::with_capacity(len, len * 10);
    let mut experiment_id = StringBuilder::with_capacity(len, len * 36);
    let mut variant_id = StringBuilder::with_capacity(len, len * 10);
    let mut assignment_method = StringBuilder::with_capacity(len, len * 12);

    for e in events {
        timestamp_ms.append_value(e.timestamp_ms);
        query.append_value(&e.query);
        match &e.query_id {
            Some(qid) => query_id.append_value(qid),
            None => query_id.append_null(),
        }
        index_name.append_value(&e.index_name);
        nb_hits.append_value(e.nb_hits);
        processing_time_ms.append_value(e.processing_time_ms);
        match &e.user_token {
            Some(t) => user_token.append_value(t),
            None => user_token.append_null(),
        }
        match &e.user_ip {
            Some(ip) => user_ip.append_value(ip),
            None => user_ip.append_null(),
        }
        match &e.filters {
            Some(f) => filters.append_value(f),
            None => filters.append_null(),
        }
        match &e.facets {
            Some(f) => facets.append_value(f),
            None => facets.append_null(),
        }
        match &e.analytics_tags {
            Some(t) => analytics_tags.append_value(t),
            None => analytics_tags.append_null(),
        }
        page.append_value(e.page);
        hits_per_page.append_value(e.hits_per_page);
        has_results.append_value(e.has_results);
        match &e.country {
            Some(c) => country.append_value(c),
            None => country.append_null(),
        }
        match &e.region {
            Some(r) => region.append_value(r),
            None => region.append_null(),
        }
        match &e.experiment_id {
            Some(v) => experiment_id.append_value(v),
            None => experiment_id.append_null(),
        }
        match &e.variant_id {
            Some(v) => variant_id.append_value(v),
            None => variant_id.append_null(),
        }
        match &e.assignment_method {
            Some(v) => assignment_method.append_value(v),
            None => assignment_method.append_null(),
        }
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(timestamp_ms.finish()),
        Arc::new(query.finish()),
        Arc::new(query_id.finish()),
        Arc::new(index_name.finish()),
        Arc::new(nb_hits.finish()),
        Arc::new(processing_time_ms.finish()),
        Arc::new(user_token.finish()),
        Arc::new(user_ip.finish()),
        Arc::new(filters.finish()),
        Arc::new(facets.finish()),
        Arc::new(analytics_tags.finish()),
        Arc::new(page.finish()),
        Arc::new(hits_per_page.finish()),
        Arc::new(has_results.finish()),
        Arc::new(country.finish()),
        Arc::new(region.finish()),
        Arc::new(experiment_id.finish()),
        Arc::new(variant_id.finish()),
        Arc::new(assignment_method.finish()),
    ];

    RecordBatch::try_new(schema.clone(), columns).map_err(|e| format!("RecordBatch error: {}", e))
}

/// Convert a slice of insight events into an Arrow `RecordBatch`.
///
/// Each event field is mapped to a typed Arrow array column. Optional fields produce nullable columns.
/// Timestamps default to `Utc::now()` when `InsightEvent::timestamp` is `None`.
/// Object IDs and positions are serialized as JSON strings.
///
/// # Arguments
///
/// * `events` - Insight events to convert.
/// * `schema` - Arrow schema that defines column order and types (must match `insight_event_schema()`).
///
/// # Returns
///
/// A `RecordBatch` ready for Parquet serialization, or a descriptive error string.
pub(crate) fn insight_events_to_batch(
    events: &[InsightEvent],
    schema: &Arc<arrow::datatypes::Schema>,
) -> Result<RecordBatch, String> {
    let len = events.len();
    let mut timestamp_ms = Int64Builder::with_capacity(len);
    let mut event_type = StringBuilder::with_capacity(len, len * 10);
    let mut event_subtype = StringBuilder::with_capacity(len, len * 10);
    let mut event_name = StringBuilder::with_capacity(len, len * 30);
    let mut index_name = StringBuilder::with_capacity(len, len * 20);
    let mut user_token = StringBuilder::with_capacity(len, len * 20);
    let mut auth_user_token = StringBuilder::with_capacity(len, len * 20);
    let mut query_id = StringBuilder::with_capacity(len, len * 32);
    let mut object_ids = StringBuilder::with_capacity(len, len * 50);
    let mut positions = StringBuilder::with_capacity(len, len * 20);
    let mut value = Float64Builder::with_capacity(len);
    let mut currency = StringBuilder::with_capacity(len, len * 3);
    let mut interleaving_team = StringBuilder::with_capacity(len, len * 2);

    for e in events {
        let ts = e
            .timestamp
            .unwrap_or_else(|| chrono::Utc::now().timestamp_millis());
        timestamp_ms.append_value(ts);
        event_type.append_value(&e.event_type);
        match &e.event_subtype {
            Some(s) => event_subtype.append_value(s),
            None => event_subtype.append_null(),
        }
        event_name.append_value(&e.event_name);
        index_name.append_value(&e.index);
        user_token.append_value(&e.user_token);
        match &e.authenticated_user_token {
            Some(t) => auth_user_token.append_value(t),
            None => auth_user_token.append_null(),
        }
        match &e.query_id {
            Some(qid) => query_id.append_value(qid),
            None => query_id.append_null(),
        }
        let oids_json = serde_json::to_string(e.effective_object_ids()).unwrap_or_default();
        object_ids.append_value(&oids_json);
        match &e.positions {
            Some(p) => {
                let pos_json = serde_json::to_string(p).unwrap_or_default();
                positions.append_value(&pos_json);
            }
            None => positions.append_null(),
        }
        match e.value {
            Some(v) => value.append_value(v),
            None => value.append_null(),
        }
        match &e.currency {
            Some(c) => currency.append_value(c),
            None => currency.append_null(),
        }
        match &e.interleaving_team {
            Some(t) => interleaving_team.append_value(t),
            None => interleaving_team.append_null(),
        }
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(timestamp_ms.finish()),
        Arc::new(event_type.finish()),
        Arc::new(event_subtype.finish()),
        Arc::new(event_name.finish()),
        Arc::new(index_name.finish()),
        Arc::new(user_token.finish()),
        Arc::new(auth_user_token.finish()),
        Arc::new(query_id.finish()),
        Arc::new(object_ids.finish()),
        Arc::new(positions.finish()),
        Arc::new(value.finish()),
        Arc::new(currency.finish()),
        Arc::new(interleaving_team.finish()),
    ];

    RecordBatch::try_new(schema.clone(), columns).map_err(|e| format!("RecordBatch error: {}", e))
}

/// Purge all insight events with a given user_token from a tenant's events directory.
/// Returns the number of removed rows.
pub fn purge_insight_events_for_user_token(
    events_dir: &Path,
    user_token: &str,
) -> Result<u64, String> {
    if !events_dir.exists() {
        return Ok(0);
    }

    let parquet_files = collect_parquet_files(events_dir)?;
    let mut removed_rows = 0_u64;
    for file_path in parquet_files {
        removed_rows += purge_user_token_from_file(&file_path, user_token)?;
    }
    Ok(removed_rows)
}

/// Recursively collect all `.parquet` file paths under `dir`.
///
/// Returns an empty vec if the directory does not exist.
fn collect_parquet_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    for entry in
        fs::read_dir(dir).map_err(|e| format!("Failed to read dir {}: {}", dir.display(), e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read entry: {}", e))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to stat entry {}: {}", path.display(), e))?;
        if file_type.is_symlink() {
            return Err(format!(
                "refusing to traverse symlinked analytics path {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            files.extend(collect_parquet_files(&path)?);
        } else if file_type.is_file()
            && path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("parquet"))
                .unwrap_or(false)
        {
            files.push(path);
        }
    }
    Ok(files)
}

/// Remove all rows whose `user_token` column matches the given token from a single Parquet file.
///
/// Reads the file batch-by-batch, filters out matching rows, then rewrites the file in place.
/// If every row is removed the file is deleted entirely.
///
/// # Returns
///
/// The number of rows removed.
fn purge_user_token_from_file(path: &Path, user_token: &str) -> Result<u64, String> {
    let file = fs::File::open(path)
        .map_err(|e| format!("Failed to open parquet file {}: {}", path.display(), e))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| format!("Failed to read parquet metadata {}: {}", path.display(), e))?;
    let schema = builder.schema().clone();
    let reader = builder
        .with_batch_size(8192)
        .build()
        .map_err(|e| format!("Failed to build parquet reader {}: {}", path.display(), e))?;

    let mut kept_batches = Vec::new();
    let mut removed_rows = 0_u64;
    let mut kept_rows = 0_usize;

    for batch_result in reader {
        let batch = batch_result
            .map_err(|e| format!("Failed to read parquet batch {}: {}", path.display(), e))?;
        let keep_mask = build_keep_mask(&batch, user_token)?;
        let filtered = filter_record_batch(&batch, &keep_mask)
            .map_err(|e| format!("Failed to filter parquet batch {}: {}", path.display(), e))?;
        removed_rows += (batch.num_rows() - filtered.num_rows()) as u64;
        if filtered.num_rows() > 0 {
            kept_rows += filtered.num_rows();
            kept_batches.push(filtered);
        }
    }

    if removed_rows == 0 {
        return Ok(0);
    }

    if kept_rows == 0 {
        fs::remove_file(path).map_err(|e| {
            format!(
                "Failed to remove emptied parquet file {}: {}",
                path.display(),
                e
            )
        })?;
        return Ok(removed_rows);
    }

    rewrite_parquet_file(path, schema, &kept_batches)?;
    Ok(removed_rows)
}

/// Build a boolean mask that is `false` for rows whose `user_token` equals the target token.
///
/// If the batch has no `user_token` column, returns an all-true mask so no rows are dropped.
/// Null user-token values are always retained.
///
/// # Arguments
///
/// * `batch` - The record batch to inspect.
/// * `user_token` - The token value to exclude.
fn build_keep_mask(batch: &RecordBatch, user_token: &str) -> Result<BooleanArray, String> {
    let idx = match batch.schema().index_of("user_token") {
        Ok(i) => i,
        Err(_) => return Ok(BooleanArray::from(vec![true; batch.num_rows()])),
    };
    let user_col = batch.column(idx);
    let user_col = user_col
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| "user_token column is not Utf8".to_string())?;

    let mut keep = BooleanBuilder::with_capacity(batch.num_rows());
    for row in 0..batch.num_rows() {
        let retain = user_col.is_null(row) || user_col.value(row) != user_token;
        keep.append_value(retain);
    }
    Ok(keep.finish())
}

/// Atomically replace a Parquet file with new content by writing to a temporary file and renaming.
///
/// Uses ZSTD compression and a 100 000-row group size, matching `write_parquet_file` settings.
///
/// # Arguments
///
/// * `path` - The original file to replace.
/// * `schema` - Arrow schema for the output.
/// * `batches` - Record batches to write into the replacement file.
fn rewrite_parquet_file(
    path: &Path,
    schema: Arc<arrow::datatypes::Schema>,
    batches: &[RecordBatch],
) -> Result<(), String> {
    let now_ns = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_path = path.with_extension(format!("parquet.{}.tmp", now_ns));

    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(Default::default()))
        .set_max_row_group_size(100_000)
        .build();

    let tmp_file = fs::File::create(&tmp_path).map_err(|e| {
        format!(
            "Failed to create temp parquet file {}: {}",
            tmp_path.display(),
            e
        )
    })?;
    let mut writer = ArrowWriter::try_new(tmp_file, schema, Some(props)).map_err(|e| {
        format!(
            "Failed to create parquet writer {}: {}",
            tmp_path.display(),
            e
        )
    })?;
    for batch in batches {
        writer.write(batch).map_err(|e| {
            format!(
                "Failed to write temp parquet batch {}: {}",
                tmp_path.display(),
                e
            )
        })?;
    }
    writer.close().map_err(|e| {
        format!(
            "Failed to close temp parquet writer {}: {}",
            tmp_path.display(),
            e
        )
    })?;

    fs::rename(&tmp_path, path).map_err(|e| {
        format!(
            "Failed to replace parquet file {} with {}: {}",
            path.display(),
            tmp_path.display(),
            e
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, TimeZone};
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    #[test]
    fn partitioned_parquet_path_disambiguates_same_timestamp() {
        let tmp = TempDir::new().unwrap();
        let now = chrono::Utc
            .timestamp_millis_opt(1_700_000_000_000)
            .single()
            .unwrap();

        let first = partitioned_parquet_path(tmp.path(), "events", now).unwrap();
        let second = partitioned_parquet_path(tmp.path(), "events", now).unwrap();

        assert_ne!(first, second);
        assert_eq!(first.parent(), second.parent());
        assert!(first.parent().unwrap().exists());
    }

    #[cfg(unix)]
    #[test]
    fn collect_parquet_files_rejects_symlinked_directories() {
        let tmp = TempDir::new().unwrap();
        let events_dir = tmp.path().join("events");
        let outside_dir = tmp.path().join("outside");
        std::fs::create_dir_all(&events_dir).unwrap();
        std::fs::create_dir_all(&outside_dir).unwrap();
        symlink(&outside_dir, events_dir.join("linked")).unwrap();

        let err = collect_parquet_files(&events_dir)
            .expect_err("analytics purge must fail closed on symlinked directories");
        assert!(
            err.contains("refusing to traverse symlinked analytics path"),
            "unexpected error: {err}"
        );
    }

    // ── Rollup writer tests ──
    //
    // Reuses the same 7-event / 4-user fixture seeded in
    // `analytics::query::tests::seed_known_answer_dataset` so the writer-level
    // assertions stay byte-for-byte consistent with the Stage 3 parity tests
    // that exercise the public query API.

    fn rollup_test_config(temp_dir: &TempDir) -> AnalyticsConfig {
        AnalyticsConfig {
            enabled: true,
            data_dir: temp_dir.path().to_path_buf(),
            flush_interval_secs: 60,
            flush_size: 10_000,
            retention_days: 90,
        }
    }

    /// Build the 7-event fixture: 3x laptop, 2x phone, 1x xyzzy, 1x zzznothing,
    /// 4 distinct user tokens (user-a/b/c/d) all dated 2025-06-15 12:00:00 UTC.
    fn fixture_events(base_ts: i64) -> Vec<SearchEvent> {
        let make = |query: &str,
                    nb_hits: u32,
                    has_results: bool,
                    offset: i64,
                    user: &str|
         -> SearchEvent {
            SearchEvent {
                timestamp_ms: base_ts + offset,
                query: query.to_string(),
                query_id: None,
                index_name: "products".to_string(),
                nb_hits,
                processing_time_ms: 5,
                user_token: Some(user.to_string()),
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
        };
        vec![
            make("laptop", 10, true, 0, "user-a"),
            make("laptop", 50, true, 1_000, "user-b"),
            make("laptop", 60, true, 2_000, "user-c"),
            make("phone", 100, true, 3_000, "user-a"),
            make("phone", 200, true, 4_000, "user-b"),
            make("xyzzy", 0, false, 5_000, "user-a"),
            make("zzznothing", 0, false, 6_000, "user-d"),
        ]
    }

    fn seed_raw_events(config: &AnalyticsConfig, events: &[SearchEvent], date: &str) {
        let schema = search_event_schema();
        let batch = search_events_to_batch(events, &schema).unwrap();
        let partition = config
            .searches_dir("products")
            .join(format!("date={}", date));
        fs::create_dir_all(&partition).unwrap();
        write_parquet_file(&partition.join("seed.parquet"), batch).unwrap();
    }

    fn base_ts_2025_06_15_noon() -> i64 {
        NaiveDate::from_ymd_opt(2025, 6, 15)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis()
    }

    fn hour_window_ms(year: i32, month: u32, day: u32, hour: u32) -> (i64, i64) {
        let date = NaiveDate::from_ymd_opt(year, month, day).unwrap();
        let start = date
            .and_hms_opt(hour, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        (start, start + 3_600_000)
    }

    fn day_window_ms(year: i32, month: u32, day: u32) -> (i64, i64) {
        let start = NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        (start, start + 86_400_000)
    }

    /// One projected rollup Parquet row:
    /// `(query, count, nb_hits_sum, nb_hits_count, no_results_count,
    /// has_results_count, Option<hll_bytes>)`.
    type RollupRow = (String, i64, i64, i64, i64, i64, Option<Vec<u8>>);

    /// Read every batch from a rollup Parquet file and project each row into a
    /// [`RollupRow`].
    fn read_rollup_rows(path: &Path) -> Vec<RollupRow> {
        let file = fs::File::open(path).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        let mut out = Vec::new();
        for batch_result in reader {
            let batch = batch_result.unwrap();
            let query = downcast_col::<StringArray>(&batch, "query").unwrap();
            let count = downcast_col::<Int64Array>(&batch, "count").unwrap();
            let nb_hits_sum = downcast_col::<Int64Array>(&batch, "nb_hits_sum").unwrap();
            let nb_hits_count = downcast_col::<Int64Array>(&batch, "nb_hits_count").unwrap();
            let no_results_count = downcast_col::<Int64Array>(&batch, "no_results_count").unwrap();
            let has_results_count =
                downcast_col::<Int64Array>(&batch, "has_results_count").unwrap();
            let hll = downcast_col::<BinaryArray>(&batch, "unique_users_hll").unwrap();
            for row in 0..batch.num_rows() {
                let hll_bytes = if hll.is_null(row) {
                    None
                } else {
                    Some(hll.value(row).to_vec())
                };
                out.push((
                    query.value(row).to_string(),
                    count.value(row),
                    nb_hits_sum.value(row),
                    nb_hits_count.value(row),
                    no_results_count.value(row),
                    has_results_count.value(row),
                    hll_bytes,
                ));
            }
        }
        out
    }

    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    /// TODO: Document flush_rollup_window_hourly_aggregates_known_answer.
    #[test]
    #[allow(clippy::cognitive_complexity)] // Known-answer test keeps all assertions inline so expected aggregates remain explicit and auditable.
    fn flush_rollup_window_hourly_aggregates_known_answer() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        let (start, end) = hour_window_ms(2025, 6, 15, 12);
        let path = flush_rollup_window(&config, "products", "1hour", start, end).unwrap();

        assert_eq!(
            path,
            config
                .rollups_dir("products", "1hour")
                .join(format!("rollup_1hour_{}.parquet", start))
        );
        assert!(path.exists(), "rollup parquet file must exist");

        let rows = read_rollup_rows(&path);
        let by_query: HashMap<&str, &RollupRow> = rows.iter().map(|r| (r.0.as_str(), r)).collect();
        assert_eq!(rows.len(), 4, "one row per distinct query");

        // laptop: count=3, sum=120, nb_hits_count=3, no_results=0, has_results=3
        let laptop = by_query["laptop"];
        assert_eq!(laptop.1, 3);
        assert_eq!(laptop.2, 120);
        assert_eq!(laptop.3, 3);
        assert_eq!(laptop.4, 0);
        assert_eq!(laptop.5, 3);

        // phone: count=2, sum=300, nb_hits_count=2, no_results=0, has_results=2
        let phone = by_query["phone"];
        assert_eq!(phone.1, 2);
        assert_eq!(phone.2, 300);
        assert_eq!(phone.3, 2);
        assert_eq!(phone.4, 0);
        assert_eq!(phone.5, 2);

        // xyzzy: count=1, sum=0, nb_hits_count=1, no_results=1, has_results=0
        let xyzzy = by_query["xyzzy"];
        assert_eq!(xyzzy.1, 1);
        assert_eq!(xyzzy.2, 0);
        assert_eq!(xyzzy.3, 1);
        assert_eq!(xyzzy.4, 1);
        assert_eq!(xyzzy.5, 0);

        let zzz = by_query["zzznothing"];
        assert_eq!(zzz.1, 1);
        assert_eq!(zzz.2, 0);
        assert_eq!(zzz.3, 1);
        assert_eq!(zzz.4, 1);
        assert_eq!(zzz.5, 0);

        // HLL must be present and decodable on every row that had at least one
        // user-tokened event. The fixture sets a user_token on every event.
        for row in &rows {
            let bytes = row.6.as_ref().expect("HLL bytes present");
            let sketch = HllSketch::from_bytes(bytes).expect("HLL bytes decode");
            assert!(
                !sketch.is_empty(),
                "HLL for query '{}' must not be empty",
                row.0
            );
        }
        // laptop has 3 distinct users (user-a/b/c), so cardinality estimate >= 1.
        let laptop_sketch = HllSketch::from_bytes(laptop.6.as_ref().unwrap()).unwrap();
        assert!(laptop_sketch.cardinality() >= 1);

        // Manifest reflects the window with replace-on-rerun semantics ready.
        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        let date_state = &manifest.tiers["1hour"].dates["2025-06-15"];
        assert_eq!(date_state.windows.len(), 1);
        assert_eq!(date_state.windows[0].start_ms, start);
        assert_eq!(date_state.windows[0].end_ms, end);
        assert_eq!(date_state.windows[0].event_count, 7);
        assert_eq!(date_state.windows[0].status, WindowStatus::Closed);
        assert_eq!(date_state.total_event_count, 7);
        assert!(
            !date_state.complete,
            "single hourly window must not mark the UTC day complete"
        );
    }

    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    /// TODO: Document flush_rollup_window_daily_compacts_from_hourly_only.
    #[test]
    #[allow(clippy::cognitive_complexity)] // Known-answer compaction test intentionally keeps full hand-calculated checks in one place.
    fn flush_rollup_window_daily_compacts_from_hourly_only() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        let events = fixture_events(base_ts);
        seed_raw_events(&config, &events, "2025-06-15");

        // Build all 24 hourly windows for 2025-06-15 (only the 12:00 window has data).
        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        // Remove raw search data so daily compaction CANNOT recompute from raw events.
        let searches_dir = config.searches_dir("products");
        fs::remove_dir_all(&searches_dir).unwrap();
        assert!(!searches_dir.exists());

        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        let daily_path =
            flush_rollup_window(&config, "products", "1day", day_start, day_end).unwrap();
        assert!(daily_path.exists());

        let rows = read_rollup_rows(&daily_path);
        let by_query: HashMap<&str, &RollupRow> = rows.iter().map(|r| (r.0.as_str(), r)).collect();
        assert_eq!(rows.len(), 4);

        // Hand-calculated daily totals (must equal hourly-merged totals because
        // the entire fixture falls inside a single hour, but the daily code path
        // is what we are exercising here).
        let laptop = by_query["laptop"];
        assert_eq!(laptop.1, 3);
        assert_eq!(laptop.2, 120);
        assert_eq!(laptop.3, 3);
        assert_eq!(laptop.4, 0);
        assert_eq!(laptop.5, 3);

        let phone = by_query["phone"];
        assert_eq!(phone.1, 2);
        assert_eq!(phone.2, 300);
        assert_eq!(phone.3, 2);
        assert_eq!(phone.4, 0);
        assert_eq!(phone.5, 2);

        let xyzzy = by_query["xyzzy"];
        assert_eq!(xyzzy.1, 1);
        assert_eq!(xyzzy.2, 0);
        assert_eq!(xyzzy.3, 1);
        assert_eq!(xyzzy.4, 1);
        assert_eq!(xyzzy.5, 0);

        // HLL merge proof: the daily HLL for each query must equal the
        // register-wise max of the same query's HLL across all hourly rollups.
        // Build that expected sketch by reading hourly Parquet files and merging
        // their per-query HLL bytes via `HllSketch::merge`. Comparing the raw
        // 16384-byte register array proves register-wise-max merging rather than
        // re-derivation from raw events (which were deleted above).
        let hourly_dir = config.rollups_dir("products", "1hour");
        let mut expected_by_query: HashMap<String, HllSketch> = HashMap::new();
        for hourly_path in collect_parquet_files(&hourly_dir).unwrap() {
            for hr in read_rollup_rows(&hourly_path) {
                if let Some(bytes) = hr.6 {
                    let sketch = HllSketch::from_bytes(&bytes).unwrap();
                    expected_by_query.entry(hr.0).or_default().merge(&sketch);
                }
            }
        }
        for row in &rows {
            let actual = row.6.as_ref().expect("daily row HLL present");
            let expected = expected_by_query
                .get(&row.0)
                .expect("hourly HLL present for query")
                .to_bytes();
            assert_eq!(
                actual, &expected,
                "daily HLL for '{}' must equal register-wise merge of hourly HLLs",
                row.0
            );
            let sketch = HllSketch::from_bytes(actual).unwrap();
            assert!(
                !sketch.is_empty(),
                "merged HLL for '{}' must not be empty",
                row.0
            );
        }

        // Manifest: daily certification must be derived from canonical hourly
        // coverage, not merely from the existence of a closed daily row.
        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        assert!(manifest.has_certified_coverage("2025-06-15", "1day"));
        let day_state = &manifest.tiers["1day"].dates["2025-06-15"];
        assert_eq!(day_state.windows.len(), 1);
        assert_eq!(day_state.windows[0].event_count, 7);
        assert_eq!(day_state.total_event_count, 7);
    }

    #[test]
    fn flush_rollup_window_rejects_non_canonical_utc_windows() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);

        let misaligned_hourly_start = NaiveDate::from_ymd_opt(2025, 6, 15)
            .unwrap()
            .and_hms_opt(12, 13, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let hourly_err = flush_rollup_window(
            &config,
            "products",
            "1hour",
            misaligned_hourly_start,
            misaligned_hourly_start + 3_600_000,
        )
        .unwrap_err();
        assert!(hourly_err.contains("must align to the configured rollup boundary"));

        let noon_day_start = NaiveDate::from_ymd_opt(2025, 6, 15)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let daily_err = flush_rollup_window(
            &config,
            "products",
            "1day",
            noon_day_start,
            noon_day_start + 86_400_000,
        )
        .unwrap_err();
        assert!(daily_err.contains("must align to UTC day boundary"));
    }

    #[test]
    fn flush_rollup_window_accepts_override_aligned_minute_window() {
        let start = NaiveDate::from_ymd_opt(2025, 6, 15)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        let end = start + 60_000;
        validate_rollup_window_with_hour_window_ms("1hour", start, end, 60_000)
            .expect("minute override should be accepted when validation uses the same width");
    }

    #[test]
    fn flush_rollup_window_idempotent_on_rerun() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        let (start, end) = hour_window_ms(2025, 6, 15, 12);
        flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        // Rerun the same window — must overwrite, not append.
        flush_rollup_window(&config, "products", "1hour", start, end).unwrap();

        let hourly_dir = config.rollups_dir("products", "1hour");
        let hourly_files = collect_parquet_files(&hourly_dir).unwrap();
        // 24 windows are not all run here; we ran 1 window twice.
        let target_filename = format!("rollup_1hour_{}.parquet", start);
        let hourly_matches: Vec<_> = hourly_files
            .iter()
            .filter(|p| p.file_name().and_then(|n| n.to_str()) == Some(&target_filename))
            .collect();
        assert_eq!(
            hourly_matches.len(),
            1,
            "exactly one hourly Parquet for the window after rerun"
        );

        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        let date_state = &manifest.tiers["1hour"].dates["2025-06-15"];
        let manifest_matches: Vec<_> = date_state
            .windows
            .iter()
            .filter(|w| w.start_ms == start)
            .collect();
        assert_eq!(
            manifest_matches.len(),
            1,
            "exactly one manifest entry for the window after rerun"
        );
        assert_eq!(manifest_matches[0].event_count, 7);

        // Build canonical hourly coverage before exercising daily idempotency.
        for hour in 0..24 {
            if hour == 12 {
                continue;
            }
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        // Now exercise daily idempotency on top of canonical hourly coverage.
        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        flush_rollup_window(&config, "products", "1day", day_start, day_end).unwrap();
        flush_rollup_window(&config, "products", "1day", day_start, day_end).unwrap();
        let daily_files = collect_parquet_files(&config.rollups_dir("products", "1day")).unwrap();
        assert_eq!(
            daily_files.len(),
            1,
            "exactly one daily Parquet after rerun"
        );

        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        let day_state = &manifest.tiers["1day"].dates["2025-06-15"];
        assert_eq!(
            day_state.windows.len(),
            1,
            "exactly one manifest daily window after rerun"
        );
        assert_eq!(day_state.windows[0].event_count, 7);
        assert_eq!(day_state.total_event_count, 7);
        assert!(day_state.complete);
    }

    #[test]
    fn flush_rollup_window_daily_requires_canonical_hourly_coverage() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();

        // Raw-only input is insufficient: daily compaction must require a
        // certified canonical 24-window hourly source set.
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");
        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        let err = flush_rollup_window(&config, "products", "1day", day_start, day_end)
            .expect_err("daily flush must reject raw-only source");
        assert!(err.contains("canonical hourly coverage"));
        let daily_dir = config.rollups_dir("products", "1day");
        if daily_dir.exists() {
            assert!(
                collect_parquet_files(&daily_dir).unwrap().is_empty(),
                "daily parquet must not be written on rejected raw-only input"
            );
        }
        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        assert!(
            manifest
                .tiers
                .get("1day")
                .and_then(|tier| tier.dates.get("2025-06-15"))
                .is_none(),
            "daily manifest date must remain untouched on rejection"
        );

        // Partial-hourly input is also insufficient.
        let tmp_partial = TempDir::new().unwrap();
        let config_partial = rollup_test_config(&tmp_partial);
        seed_raw_events(&config_partial, &fixture_events(base_ts), "2025-06-15");
        let (hour_start, hour_end) = hour_window_ms(2025, 6, 15, 12);
        flush_rollup_window(&config_partial, "products", "1hour", hour_start, hour_end).unwrap();

        let err = flush_rollup_window(&config_partial, "products", "1day", day_start, day_end)
            .expect_err("daily flush must reject partial-hourly source");
        assert!(err.contains("canonical hourly coverage"));
        let daily_dir = config_partial.rollups_dir("products", "1day");
        if daily_dir.exists() {
            assert!(
                collect_parquet_files(&daily_dir).unwrap().is_empty(),
                "daily parquet must not be written on rejected partial-hourly input"
            );
        }
        let manifest =
            RollupManifest::load(&config_partial.rollup_manifest_path("products")).unwrap();
        assert!(
            manifest
                .tiers
                .get("1day")
                .and_then(|tier| tier.dates.get("2025-06-15"))
                .is_none(),
            "daily manifest date must remain untouched on partial-hourly rejection"
        );
    }

    #[test]
    fn flush_rollup_window_write_is_atomic_against_partial_failures() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        let (start, end) = hour_window_ms(2025, 6, 15, 12);
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        let rollup_path = flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        let original_bytes = fs::read(&rollup_path).unwrap();

        // Simulate rerun failure by removing write permission on the rollup dir:
        // atomic temp+rename must preserve the prior file bytes when the rewrite
        // cannot create a replacement artifact.
        let rollup_dir = rollup_path.parent().unwrap().to_path_buf();
        let mut perms = fs::metadata(&rollup_dir).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o555);
        }
        fs::set_permissions(&rollup_dir, perms).unwrap();
        let err = flush_rollup_window(&config, "products", "1hour", start, end).unwrap_err();
        assert!(
            err.contains("Failed to create parquet file")
                || err.contains("Permission denied")
                || !err.is_empty()
        );

        let mut restore_perms = fs::metadata(&rollup_dir).unwrap().permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            restore_perms.set_mode(0o755);
        }
        fs::set_permissions(&rollup_dir, restore_perms).unwrap();

        assert!(
            rollup_path.is_file(),
            "failed rerun must leave the previously certified parquet artifact intact"
        );
        assert_eq!(
            fs::read(&rollup_path).unwrap(),
            original_bytes,
            "failed rerun must not change the prior parquet bytes"
        );
    }

    #[test]
    fn flush_rollup_window_daily_fails_when_certified_hourly_file_missing() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        assert!(manifest.has_certified_coverage("2025-06-15", "1hour"));

        let missing_hour = hour_window_ms(2025, 6, 15, 12).0;
        let missing_file = config
            .rollups_dir("products", "1hour")
            .join(format!("rollup_1hour_{}.parquet", missing_hour));
        fs::remove_file(&missing_file).unwrap();
        assert!(!missing_file.exists());

        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        let err = flush_rollup_window(&config, "products", "1day", day_start, day_end).expect_err(
            "daily flush must fail closed if a certified hourly source file is missing",
        );
        assert!(
            err.contains("missing") || err.contains("readable") || err.contains("Failed to open"),
            "unexpected error: {err}"
        );

        let daily_dir = config.rollups_dir("products", "1day");
        if daily_dir.exists() {
            assert!(
                collect_parquet_files(&daily_dir).unwrap().is_empty(),
                "daily parquet must not be written if a certified hourly source file is missing"
            );
        }
    }

    #[test]
    fn flush_rollup_window_daily_rejects_manifest_path_traversal() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        let escaped_path = config
            .rollups_dir("products", "1hour")
            .parent()
            .unwrap()
            .join("escaped_hourly.parquet");
        let escaped_batch = RecordBatch::try_new(
            search_rollup_schema(),
            vec![
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![60_000])),
                Arc::new(StringArray::from(vec!["escaped"])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(BinaryArray::from(vec![None::<&[u8]>])),
            ],
        )
        .unwrap();
        write_parquet_file_atomic(&escaped_path, escaped_batch).unwrap();

        let manifest_path = config.rollup_manifest_path("products");
        let mut manifest = RollupManifest::load(&manifest_path).unwrap();
        let hourly_state = manifest
            .tiers
            .get_mut("1hour")
            .and_then(|tier| tier.dates.get_mut("2025-06-15"))
            .expect("hourly state exists after canonical flushes");
        hourly_state.windows[0].file = "../escaped_hourly.parquet".to_string();
        manifest.save(&manifest_path).unwrap();

        let (day_start, _day_end) = day_window_ms(2025, 6, 15);
        let err = match require_canonical_hourly_source_coverage(&config, "products", day_start) {
            Ok(_) => panic!("daily source discovery must reject manifest filename traversal"),
            Err(err) => err,
        };
        assert!(
            err.contains("invalid certified hourly source filename"),
            "unexpected error: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn flush_rollup_window_daily_rejects_symlinked_certified_hourly_source() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        let (certified_start, certified_end) = hour_window_ms(2025, 6, 15, 0);
        let escaped_path = config
            .rollups_dir("products", "1hour")
            .parent()
            .unwrap()
            .join("symlinked_hourly_escape.parquet");
        let escaped_batch = RecordBatch::try_new(
            search_rollup_schema(),
            vec![
                Arc::new(Int64Array::from(vec![certified_start])),
                Arc::new(Int64Array::from(vec![certified_end])),
                Arc::new(StringArray::from(vec!["escaped"])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(BinaryArray::from(vec![None::<&[u8]>])),
            ],
        )
        .unwrap();
        write_parquet_file_atomic(&escaped_path, escaped_batch).unwrap();

        let certified_path = config
            .rollups_dir("products", "1hour")
            .join(format!("rollup_1hour_{}.parquet", certified_start));
        fs::remove_file(&certified_path).unwrap();
        symlink(&escaped_path, &certified_path).unwrap();

        let (day_start, _day_end) = day_window_ms(2025, 6, 15);
        let err = require_canonical_hourly_source_coverage(&config, "products", day_start)
            .expect_err("daily source discovery must reject symlinked certified hourly files");
        assert!(
            err.contains("must not be a symlink"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn flush_rollup_window_daily_fails_when_certified_hourly_rows_violate_manifest_window() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        // Replace one certified hourly file with a valid parquet row whose
        // window bounds do not match the manifest-declared hourly window.
        let (certified_start, certified_end) = hour_window_ms(2025, 6, 15, 0);
        let (wrong_start, wrong_end) = hour_window_ms(2025, 6, 15, 1);
        let replaced_path = config
            .rollups_dir("products", "1hour")
            .join(format!("rollup_1hour_{}.parquet", certified_start));
        let swapped_batch = RecordBatch::try_new(
            search_rollup_schema(),
            vec![
                Arc::new(Int64Array::from(vec![wrong_start])),
                Arc::new(Int64Array::from(vec![wrong_end])),
                Arc::new(StringArray::from(vec!["swapped-window-row"])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![1])),
                Arc::new(BinaryArray::from(vec![None::<&[u8]>])),
            ],
        )
        .unwrap();
        write_parquet_file_atomic(&replaced_path, swapped_batch).unwrap();

        let manifest = RollupManifest::load(&config.rollup_manifest_path("products")).unwrap();
        let hourly_window = manifest.tiers["1hour"].dates["2025-06-15"]
            .windows
            .iter()
            .find(|w| w.start_ms == certified_start)
            .unwrap();
        assert_eq!(hourly_window.end_ms, certified_end);

        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        let err = flush_rollup_window(&config, "products", "1day", day_start, day_end)
            .expect_err("daily flush must fail closed on manifest-window mismatch");
        assert!(
            err.contains("manifest requires") || err.contains("outside daily window"),
            "unexpected error: {err}"
        );

        let daily_dir = config.rollups_dir("products", "1day");
        if daily_dir.exists() {
            assert!(
                collect_parquet_files(&daily_dir).unwrap().is_empty(),
                "daily parquet must not be written on certified hourly window mismatch"
            );
        }
    }

    #[test]
    fn flush_rollup_window_daily_fails_when_certified_hourly_count_mismatches_manifest() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        let (certified_start, certified_end) = hour_window_ms(2025, 6, 15, 12);
        let replaced_path = config
            .rollups_dir("products", "1hour")
            .join(format!("rollup_1hour_{}.parquet", certified_start));
        let replaced_batch = RecordBatch::try_new(
            search_rollup_schema(),
            vec![
                Arc::new(Int64Array::from(vec![certified_start])),
                Arc::new(Int64Array::from(vec![certified_end])),
                Arc::new(StringArray::from(vec!["laptop"])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(BinaryArray::from(vec![None::<&[u8]>])),
            ],
        )
        .unwrap();
        write_parquet_file_atomic(&replaced_path, replaced_batch).unwrap();

        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        let err = flush_rollup_window(&config, "products", "1day", day_start, day_end)
            .expect_err("daily flush must fail closed on certified hourly count mismatch");
        assert!(
            err.contains("event_count") || err.contains("count mismatch"),
            "unexpected error: {err}"
        );

        let daily_dir = config.rollups_dir("products", "1day");
        if daily_dir.exists() {
            assert!(
                collect_parquet_files(&daily_dir).unwrap().is_empty(),
                "daily parquet must not be written when certified hourly count mismatches manifest"
            );
        }
    }

    #[test]
    fn flush_rollup_window_daily_recomputes_state_from_hourly_windows_not_stale_bits() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        let manifest_path = config.rollup_manifest_path("products");
        let mut stale_manifest = RollupManifest::load(&manifest_path).unwrap();
        let hourly_state = stale_manifest
            .tiers
            .get_mut("1hour")
            .and_then(|tier| tier.dates.get_mut("2025-06-15"))
            .unwrap();
        hourly_state.complete = false;
        hourly_state.total_event_count = 0;
        stale_manifest.save(&manifest_path).unwrap();

        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        flush_rollup_window(&config, "products", "1day", day_start, day_end).unwrap();

        let repaired_manifest = RollupManifest::load(&manifest_path).unwrap();
        let day_state = repaired_manifest
            .tiers
            .get("1day")
            .and_then(|tier| tier.dates.get("2025-06-15"))
            .expect("daily date state exists after successful 1day flush");
        assert!(
            day_state.complete,
            "successful daily flush must persist complete=true when canonical hourly windows exist"
        );
        assert_eq!(
            day_state.total_event_count, 7,
            "successful daily flush must persist hourly-derived event total, not stale loaded bits"
        );
    }

    #[test]
    fn flush_rollup_window_daily_fails_on_corrupt_hourly_hll_payload() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        for hour in 0..24 {
            let (start, end) = hour_window_ms(2025, 6, 15, hour);
            flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        }

        let (corrupt_start, corrupt_end) = hour_window_ms(2025, 6, 15, 0);
        let corrupt_path = config
            .rollups_dir("products", "1hour")
            .join(format!("rollup_1hour_{}.parquet", corrupt_start));
        let corrupt_batch = RecordBatch::try_new(
            search_rollup_schema(),
            vec![
                Arc::new(Int64Array::from(vec![corrupt_start])),
                Arc::new(Int64Array::from(vec![corrupt_end])),
                Arc::new(StringArray::from(vec!["corrupt-hll-row"])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(Int64Array::from(vec![0])),
                Arc::new(BinaryArray::from(vec![Some(&[1_u8, 2, 3][..])])),
            ],
        )
        .unwrap();
        write_parquet_file_atomic(&corrupt_path, corrupt_batch).unwrap();

        let (day_start, day_end) = day_window_ms(2025, 6, 15);
        let err = flush_rollup_window(&config, "products", "1day", day_start, day_end)
            .expect_err("daily flush must fail closed on corrupt hourly HLL blob");
        assert!(
            err.contains("invalid unique_users_hll payload"),
            "unexpected error: {err}"
        );

        let daily_dir = config.rollups_dir("products", "1day");
        if daily_dir.exists() {
            assert!(
                collect_parquet_files(&daily_dir).unwrap().is_empty(),
                "daily parquet must not be written when hourly HLL decode fails"
            );
        }
    }

    #[test]
    fn flush_rollup_window_hourly_removes_new_parquet_when_manifest_load_fails() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        let manifest_path = config.rollup_manifest_path("products");
        fs::create_dir_all(&manifest_path).unwrap();

        let (start, end) = hour_window_ms(2025, 6, 15, 12);
        let err = flush_rollup_window(&config, "products", "1hour", start, end)
            .expect_err("manifest load failure must abort hourly parquet publication");
        assert!(
            err.contains("Failed to load rollup manifest"),
            "unexpected error: {err}"
        );

        let hourly_dir = config.rollups_dir("products", "1hour");
        if hourly_dir.exists() {
            assert!(
                collect_parquet_files(&hourly_dir).unwrap().is_empty(),
                "failed manifest load must not leave an orphaned hourly parquet behind"
            );
        }
    }

    #[test]
    fn flush_rollup_window_hourly_restores_previous_parquet_when_manifest_load_fails() {
        let tmp = TempDir::new().unwrap();
        let config = rollup_test_config(&tmp);
        let base_ts = base_ts_2025_06_15_noon();
        seed_raw_events(&config, &fixture_events(base_ts), "2025-06-15");

        let (start, end) = hour_window_ms(2025, 6, 15, 12);
        let hourly_path = flush_rollup_window(&config, "products", "1hour", start, end).unwrap();
        let original_bytes = fs::read(&hourly_path).unwrap();

        let extra_event = SearchEvent {
            timestamp_ms: start + 7_000,
            query: "laptop".to_string(),
            query_id: None,
            index_name: "products".to_string(),
            nb_hits: 9,
            processing_time_ms: 5,
            user_token: Some("user-extra".to_string()),
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
        let partition = config.searches_dir("products").join("date=2025-06-15");
        let extra_batch = search_events_to_batch(&[extra_event], &search_event_schema()).unwrap();
        write_parquet_file(&partition.join("seed_extra.parquet"), extra_batch).unwrap();

        let manifest_path = config.rollup_manifest_path("products");
        fs::remove_file(&manifest_path).unwrap();
        fs::create_dir_all(&manifest_path).unwrap();

        let err = flush_rollup_window(&config, "products", "1hour", start, end)
            .expect_err("manifest load failure must roll back in-place hourly replacement");
        assert!(
            err.contains("Failed to load rollup manifest"),
            "unexpected error: {err}"
        );
        assert_eq!(
            fs::read(&hourly_path).unwrap(),
            original_bytes,
            "failed manifest load must restore the previous hourly parquet bytes"
        );
    }
}
