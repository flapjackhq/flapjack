use base64::Engine;
use datafusion::datasource::listing::ListingOptions;
use datafusion::prelude::*;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::config::AnalyticsConfig;
use super::manifest::RollupManifest;

mod click_analytics;
mod conversion_analytics;
mod filters_analytics;
mod search_analytics;
mod user_analytics;

#[cfg(test)]
#[path = "query_concurrency_tests.rs"]
mod concurrency_tests;

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

/// Maximum limit for analytics query results to prevent DoS.
const MAX_ANALYTICS_LIMIT: usize = 10_000;

/// Common parameters shared by analytics date-range query endpoints.
pub struct AnalyticsQueryParams<'a> {
    pub index_name: &'a str,
    pub start_date: &'a str,
    pub end_date: &'a str,
    pub limit: usize,
    pub tags: Option<&'a str>,
}

/// Sanitize a string for safe interpolation into a SQL LIKE pattern.
/// Escapes single quotes for SQL and LIKE wildcards (%, _) so user input cannot
/// alter query semantics. Use with `ESCAPE '\'` in the LIKE clause.
fn sanitize_sql_like(s: &str) -> String {
    s.replace('\'', "''")
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// Sanitize for SQL equality comparisons (only needs quote escaping).
fn sanitize_sql_eq(s: &str) -> String {
    s.replace('\'', "''")
}

/// Quote a SQL string literal after escaping single quotes.
fn sql_string_literal(s: &str) -> String {
    format!("'{}'", sanitize_sql_eq(s))
}

/// Clamp a user-supplied limit to the allowed maximum.
fn clamp_limit(limit: usize) -> usize {
    limit.min(MAX_ANALYTICS_LIMIT)
}

/// DataFusion-based analytics query engine.
///
/// Reads Parquet files from the analytics data directory and executes SQL queries.
/// Supports Hive-style date partitioning for efficient range queries.
pub struct AnalyticsQueryEngine {
    config: AnalyticsConfig,
}

/// A registered raw analytics session and the stable per-index filesystem
/// snapshot it reads. Keeping ownership together prevents callers from dropping
/// the snapshot between parquet discovery and DataFusion execution.
struct AnalyticsSession {
    context: SessionContext,
    _snapshot: super::mutation::IndexReadGuard,
}

/// Windows inside a multi-step analytics read where a same-index seed or clear
/// may queue. Each variant names a handoff that must stay inside one snapshot:
/// a mutation arriving there must neither deadlock the read nor split it across
/// two different filesystem states. Only tests observe these stages; production
/// builds compile the hook body away.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AnalyticsReadStage {
    /// After a searches-only session holds its snapshot and has registered the
    /// raw table, before the endpoint begins executing its query.
    AfterSearchesRegistration,
    /// Between `searches` registration and `events` registration.
    BeforeEventsRegistration,
    /// After an endpoint collected its initial `searches` rows and before it
    /// runs any follow-up query, such as click enrichment.
    AfterInitialSearchCollection,
}

/// Stage hooks keyed by index name so concurrently running tests, which share
/// this process, never observe or cancel each other's hook.
#[cfg(test)]
type AnalyticsReadStageHook = Arc<dyn Fn(AnalyticsReadStage) + Send + Sync>;

#[cfg(test)]
type AnalyticsReadStageHookRegistry = std::sync::Mutex<HashMap<String, AnalyticsReadStageHook>>;

#[cfg(test)]
static ANALYTICS_READ_STAGE_HOOKS: once_cell::sync::Lazy<AnalyticsReadStageHookRegistry> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(HashMap::new()));

#[cfg(test)]
struct AnalyticsReadStageHookGuard {
    index_name: String,
}

#[cfg(test)]
impl Drop for AnalyticsReadStageHookGuard {
    fn drop(&mut self) {
        ANALYTICS_READ_STAGE_HOOKS
            .lock()
            .unwrap()
            .remove(&self.index_name);
    }
}

#[cfg(test)]
fn set_analytics_read_stage_hook(
    index_name: &str,
    hook: AnalyticsReadStageHook,
) -> AnalyticsReadStageHookGuard {
    ANALYTICS_READ_STAGE_HOOKS
        .lock()
        .unwrap()
        .insert(index_name.to_string(), hook);
    AnalyticsReadStageHookGuard {
        index_name: index_name.to_string(),
    }
}

#[cfg(test)]
fn run_analytics_read_stage_hook(stage: AnalyticsReadStage, index_name: &str) {
    let hook = ANALYTICS_READ_STAGE_HOOKS
        .lock()
        .unwrap()
        .get(index_name)
        .cloned();
    if let Some(hook) = hook {
        hook(stage);
    }
}

#[cfg(not(test))]
fn run_analytics_read_stage_hook(_stage: AnalyticsReadStage, _index_name: &str) {}

impl Deref for AnalyticsSession {
    type Target = SessionContext;

    fn deref(&self) -> &Self::Target {
        &self.context
    }
}

impl AnalyticsQueryEngine {
    pub fn new(config: AnalyticsConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &AnalyticsConfig {
        &self.config
    }

    /// Execute a SQL query over search events for a given index.
    /// Returns results as a Vec of serde_json::Value rows.
    pub async fn query_searches(
        &self,
        index_name: &str,
        sql: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let _snapshot = super::mutation::begin_index_read(&self.config, index_name).await?;
        let dir = self.config.searches_dir(index_name);
        self.query_parquet_dir(&dir, "searches", sql).await
    }

    /// Execute a SQL query over insight events for a given index.
    pub async fn query_events(
        &self,
        index_name: &str,
        sql: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let _snapshot = super::mutation::begin_index_read(&self.config, index_name).await?;
        let dir = self.config.events_dir(index_name);
        self.query_parquet_dir(&dir, "events", sql).await
    }

    /// Execute a SQL query over pre-aggregated rollup Parquet files for a given
    /// index/tier. Internal engine seam: Stage 3 routes `top_searches` /
    /// `search_count` / `users_count_hll` through this method when the rollup
    /// manifest certifies coverage for the requested window. Stage 1 only
    /// establishes the seam so the crate compiles with the rollup-aware surface.
    pub async fn query_rollup_parquet(
        &self,
        index_name: &str,
        tier: &str,
        sql: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        let dir = self.config.rollups_dir(index_name, tier);
        self.query_parquet_dir(&dir, "rollups", sql).await
    }

    /// Return a single certified non-raw tier only when every UTC date in the
    /// requested range resolves to the same best tier in the rollup manifest.
    /// Mixed tiers, uncertified dates, parse failures, or missing manifests all
    /// return `None`, which the caller treats as a raw-path fallback.
    fn select_uniform_certified_tier(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Option<String> {
        let manifest_path = self.config.rollup_manifest_path(index_name);
        let manifest = RollupManifest::load(&manifest_path).ok()?;
        let dates = enumerate_utc_dates_inclusive(start_date, end_date).ok()?;
        if dates.is_empty() {
            return None;
        }

        let mut selected_tier: Option<String> = None;
        for date in dates {
            let best_tier = manifest.best_tier_for_date(&date);
            if best_tier == "raw" {
                return None;
            }
            match selected_tier.as_deref() {
                None => selected_tier = Some(best_tier.to_string()),
                Some(existing) if existing == best_tier => {}
                Some(_) => return None,
            }
        }

        selected_tier
    }

    /// Execute an arbitrary SQL query against all Parquet files in the given directory, registered under the specified table name. Returns results as a `Vec<serde_json::Value>`. Returns an empty `Vec` if the directory does not exist or contains no Parquet files.
    async fn query_parquet_dir(
        &self,
        dir: &Path,
        table_name: &str,
        sql: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let ctx = SessionContext::new();

        // Find all parquet files recursively (Hive-partitioned)
        let parquet_files = find_parquet_files(dir)?;
        if parquet_files.is_empty() {
            return Ok(Vec::new());
        }

        // Register parquet files as a table using listing options
        let opts = ListingOptions::new(Arc::new(
            datafusion::datasource::file_format::parquet::ParquetFormat::default(),
        ))
        .with_file_extension(".parquet")
        .with_collect_stat(false);

        let table_path = dir.to_string_lossy().to_string();
        ctx.register_listing_table(table_name, &table_path, opts, None, None)
            .await
            .map_err(|e| format!("Failed to register table: {}", e))?;

        let df = ctx
            .sql(sql)
            .await
            .map_err(|e| format!("SQL error: {}", e))?;
        let batches = df
            .collect()
            .await
            .map_err(|e| format!("Query execution error: {}", e))?;

        batches_to_json(&batches)
    }

    // ── High-level analytics query helpers ──

    /// Analytics status (last updated timestamp).
    pub async fn status(&self, index_name: &str) -> Result<serde_json::Value, String> {
        let _snapshot = super::mutation::begin_index_read(&self.config, index_name).await?;
        let dir = self.config.searches_dir(index_name);
        let has_data = contains_parquet_file(&dir)?;

        Ok(serde_json::json!({
            "enabled": self.config.enabled,
            "hasData": has_data,
            "retentionDays": self.config.retention_days,
        }))
    }

    /// Overview analytics across all indices (server-wide).
    /// Returns aggregated totals: search count, user count, no-result rate, CTR.
    pub async fn overview(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;
        let indices = self.list_analytics_indices()?;
        self.overview_for_indices(&indices, start_ms, end_ms).await
    }

    /// Overview analytics for one index.
    pub async fn overview_for_index(
        &self,
        index_name: &str,
        start_date: &str,
        end_date: &str,
    ) -> Result<serde_json::Value, String> {
        let start_ms = date_to_start_ms(start_date)?;
        let end_ms = date_to_end_ms(end_date)?;
        let indices = vec![index_name.to_string()];
        self.overview_for_indices(&indices, start_ms, end_ms).await
    }

    async fn overview_for_indices(
        &self,
        indices: &[String],
        start_ms: i64,
        end_ms: i64,
    ) -> Result<serde_json::Value, String> {
        if indices.is_empty() {
            return Ok(serde_json::json!({
                "totalSearches": 0,
                "uniqueUsers": 0,
                "noResultRate": null,
                "clickThroughRate": null,
                "indices": [],
                "dates": []
            }));
        }

        let mut totals = OverviewTotals::default();
        let mut all_users: HashSet<String> = HashSet::new();
        let mut daily_searches: BTreeMap<i64, i64> = BTreeMap::new();
        let mut per_index: Vec<serde_json::Value> = Vec::new();

        for index_name in indices {
            let metrics = self
                .collect_overview_metrics_for_index(index_name, start_ms, end_ms)
                .await?;
            totals.add_search_totals(&metrics.search_totals);
            totals.total_clicks += metrics.click_count;

            if metrics.search_totals.total_searches > 0 {
                per_index.push(serde_json::json!({
                    "index": index_name,
                    "searches": metrics.search_totals.total_searches,
                    "noResults": metrics.search_totals.no_results
                }));
            }
            merge_daily_counts(&mut daily_searches, &metrics.daily_rows);
            collect_user_ids(&mut all_users, &metrics.user_rows);
        }

        let nrr = rounded_ratio(totals.total_no_results, totals.total_searches);
        let ctr = rounded_ratio(totals.total_clicks, totals.total_tracked);

        let dates: Vec<serde_json::Value> = daily_searches
            .iter()
            .map(|(&ms, &count)| serde_json::json!({"date": ms_to_date_string(ms), "count": count}))
            .collect();

        per_index.sort_by(|a, b| {
            let sa = a.get("searches").and_then(|v| v.as_i64()).unwrap_or(0);
            let sb = b.get("searches").and_then(|v| v.as_i64()).unwrap_or(0);
            sb.cmp(&sa)
        });

        Ok(serde_json::json!({
            "totalSearches": totals.total_searches,
            "uniqueUsers": all_users.len(),
            "noResultRate": nrr,
            "clickThroughRate": ctr,
            "indices": per_index,
            "dates": dates
        }))
    }

    /// List all index names that have analytics data.
    pub fn list_analytics_indices(&self) -> Result<Vec<String>, String> {
        let dir = &self.config.data_dir;
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut indices = Vec::new();
        let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir error: {}", e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("entry error: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    indices.push(name.to_string());
                }
            }
        }
        Ok(indices)
    }

    /// Gather search totals, daily breakdown, user counts, and click counts for a single
    /// index within the given time range by querying both search and event parquet tables.
    async fn collect_overview_metrics_for_index(
        &self,
        index_name: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> Result<IndexOverviewMetrics, String> {
        let ctx = self
            .create_session_with_searches_and_events(index_name)
            .await?;
        let totals_rows = self
            .query_rows_or_empty(&ctx, &overview_totals_sql(start_ms, end_ms))
            .await?;
        let daily_rows = self
            .query_rows_or_empty(&ctx, &overview_daily_sql(start_ms, end_ms))
            .await?;
        let user_rows = self
            .query_rows_or_empty(&ctx, &overview_users_sql(start_ms, end_ms))
            .await?;
        let click_rows = self
            .query_rows_or_empty(&ctx, &overview_clicks_sql(start_ms, end_ms))
            .await?;

        Ok(IndexOverviewMetrics {
            search_totals: parse_search_totals(totals_rows.first()),
            daily_rows,
            user_rows,
            click_count: row_i64(click_rows.first(), "count"),
        })
    }

    async fn query_rows_or_empty(
        &self,
        ctx: &SessionContext,
        sql: &str,
    ) -> Result<Vec<serde_json::Value>, String> {
        match ctx.sql(sql).await {
            Ok(dataframe) => match dataframe.collect().await {
                Ok(batches) => batches_to_json(&batches),
                Err(_) => Ok(Vec::new()),
            },
            Err(_) => Ok(Vec::new()),
        }
    }

    /// Execute SQL against the session context, returning `None` on SQL compilation
    /// errors (missing table) and propagating execution errors.
    async fn query_rows_or_skip_sql(
        &self,
        ctx: &SessionContext,
        sql: &str,
    ) -> Result<Option<Vec<serde_json::Value>>, String> {
        match ctx.sql(sql).await {
            Ok(dataframe) => {
                let batches = dataframe
                    .collect()
                    .await
                    .map_err(|e| format!("Exec error: {}", e))?;
                Ok(Some(batches_to_json(&batches)?))
            }
            Err(_) => Ok(None),
        }
    }

    // ── Internal helpers ──

    /// Create a DataFusion `SessionContext` with the `searches` table registered from Parquet files for the given index. If the searches directory does not exist, registers an empty in-memory table so queries return zero rows instead of failing.
    async fn create_session_with_searches(
        &self,
        index_name: &str,
    ) -> Result<AnalyticsSession, String> {
        let snapshot = super::mutation::begin_index_read(&self.config, index_name).await?;
        let ctx = SessionContext::new();
        self.register_searches_table(&ctx, index_name).await?;
        run_analytics_read_stage_hook(AnalyticsReadStage::AfterSearchesRegistration, index_name);
        Ok(AnalyticsSession {
            context: ctx,
            _snapshot: snapshot,
        })
    }

    /// Create a DataFusion `SessionContext` with the `events` table registered from Parquet files for the given index. If the events directory does not exist, registers an empty in-memory table so queries return zero rows instead of failing.
    async fn create_session_with_events(
        &self,
        index_name: &str,
    ) -> Result<AnalyticsSession, String> {
        let snapshot = super::mutation::begin_index_read(&self.config, index_name).await?;
        let ctx = SessionContext::new();
        self.register_events_table(&ctx, index_name).await?;
        Ok(AnalyticsSession {
            context: ctx,
            _snapshot: snapshot,
        })
    }

    async fn create_session_with_searches_and_events(
        &self,
        index_name: &str,
    ) -> Result<AnalyticsSession, String> {
        let snapshot = super::mutation::begin_index_read(&self.config, index_name).await?;
        let ctx = SessionContext::new();
        self.register_searches_table(&ctx, index_name).await?;
        self.register_events_table(&ctx, index_name).await?;
        Ok(AnalyticsSession {
            context: ctx,
            _snapshot: snapshot,
        })
    }

    async fn register_searches_table(
        &self,
        ctx: &SessionContext,
        index_name: &str,
    ) -> Result<(), String> {
        let dir = self.config.searches_dir(index_name);
        if !contains_parquet_file(&dir)? {
            // Register an empty table so SQL queries return 0 rows instead of erroring
            let batch =
                arrow::record_batch::RecordBatch::new_empty(super::schema::search_event_schema());
            let mem_table = datafusion::datasource::MemTable::try_new(
                super::schema::search_event_schema(),
                vec![vec![batch]],
            )
            .map_err(|e| format!("Failed to create empty searches table: {}", e))?;
            ctx.register_table("searches", Arc::new(mem_table))
                .map_err(|e| format!("Failed to register empty searches: {}", e))?;
            return Ok(());
        }
        let opts = ListingOptions::new(Arc::new(
            datafusion::datasource::file_format::parquet::ParquetFormat::default(),
        ))
        .with_file_extension(".parquet")
        .with_collect_stat(false);
        let table_path = dir.to_string_lossy().to_string();
        ctx.register_listing_table("searches", &table_path, opts, None, None)
            .await
            .map_err(|e| format!("Failed to register searches: {}", e))?;
        Ok(())
    }

    async fn register_events_table(
        &self,
        ctx: &SessionContext,
        index_name: &str,
    ) -> Result<(), String> {
        run_analytics_read_stage_hook(AnalyticsReadStage::BeforeEventsRegistration, index_name);
        let dir = self.config.events_dir(index_name);
        if !contains_parquet_file(&dir)? {
            let batch =
                arrow::record_batch::RecordBatch::new_empty(super::schema::insight_event_schema());
            let mem_table = datafusion::datasource::MemTable::try_new(
                super::schema::insight_event_schema(),
                vec![vec![batch]],
            )
            .map_err(|e| format!("Failed to create empty events table: {}", e))?;
            ctx.register_table("events", Arc::new(mem_table))
                .map_err(|e| format!("Failed to register empty events: {}", e))?;
            return Ok(());
        }
        let opts = ListingOptions::new(Arc::new(
            datafusion::datasource::file_format::parquet::ParquetFormat::default(),
        ))
        .with_file_extension(".parquet")
        .with_collect_stat(false);
        let table_path = dir.to_string_lossy().to_string();
        ctx.register_listing_table("events", &table_path, opts, None, None)
            .await
            .map_err(|e| format!("Failed to register events: {}", e))?;
        Ok(())
    }

    /// Augment search result rows with click analytics fields (`clickThroughRate`, `conversionRate`, `clickCount`, `trackedSearchCount`, `conversionCount`, `averageClickPosition`) by cross-referencing the events table through query ID correlation.
    ///
    /// Runs inside the caller's already-admitted searches+events session: taking
    /// a second same-index read here would deadlock against any seed or clear
    /// that queued while the caller's snapshot was still live.
    async fn enrich_with_click_data(
        &self,
        ctx: &SessionContext,
        start_ms: i64,
        end_ms: i64,
        rows: Vec<serde_json::Value>,
    ) -> Result<Vec<serde_json::Value>, String> {
        let tracked_sql = tracked_counts_sql(start_ms, end_ms);
        let Some(tracked_rows) = self.query_rows_or_skip_sql(ctx, &tracked_sql).await? else {
            return Ok(rows);
        };
        let tracked_by_query = map_query_counts(&tracked_rows, "query", "tracked_count");

        let qid_sql = query_id_map_sql(start_ms, end_ms);
        let Some(qid_rows) = self.query_rows_or_skip_sql(ctx, &qid_sql).await? else {
            return Ok(rows);
        };
        let qid_to_query = map_query_ids_to_queries(&qid_rows);

        let clicks_rows = self
            .query_rows_or_empty(ctx, &click_counts_sql(start_ms, end_ms))
            .await?;
        let clicks_by_query =
            aggregate_counts_by_query_id(&clicks_rows, &qid_to_query, "click_count");

        let conversion_rows = self
            .query_rows_or_empty(ctx, &conversion_counts_sql(start_ms, end_ms))
            .await?;
        let conversions_by_query =
            aggregate_counts_by_query_id(&conversion_rows, &qid_to_query, "conv_count");

        let position_rows = self
            .query_rows_or_empty(ctx, &click_positions_sql(start_ms, end_ms))
            .await?;
        let (position_sums, position_counts) =
            aggregate_click_positions_by_query(&position_rows, &qid_to_query);

        Ok(enrich_rows_with_click_metrics(
            rows,
            &tracked_by_query,
            &clicks_by_query,
            &conversions_by_query,
            &position_sums,
            &position_counts,
        ))
    }
}

#[derive(Default)]
struct OverviewTotals {
    total_searches: i64,
    total_no_results: i64,
    total_tracked: i64,
    total_clicks: i64,
}

impl OverviewTotals {
    fn add_search_totals(&mut self, totals: &SearchTotals) {
        self.total_searches += totals.total_searches;
        self.total_no_results += totals.no_results;
        self.total_tracked += totals.tracked;
    }
}

#[derive(Default)]
struct SearchTotals {
    total_searches: i64,
    no_results: i64,
    tracked: i64,
}

struct IndexOverviewMetrics {
    search_totals: SearchTotals,
    daily_rows: Vec<serde_json::Value>,
    user_rows: Vec<serde_json::Value>,
    click_count: i64,
}

// ── Utility functions ──

fn rounded_ratio(numerator: i64, denominator: i64) -> Option<f64> {
    if denominator > 0 {
        Some((numerator as f64 / denominator as f64 * 1000.0).round() / 1000.0)
    } else {
        None
    }
}

fn row_i64(row: Option<&serde_json::Value>, field: &str) -> i64 {
    row.and_then(|value| value.get(field))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
}

fn parse_search_totals(row: Option<&serde_json::Value>) -> SearchTotals {
    SearchTotals {
        total_searches: row_i64(row, "total"),
        no_results: row_i64(row, "no_results"),
        tracked: row_i64(row, "tracked"),
    }
}

fn merge_daily_counts(daily_searches: &mut BTreeMap<i64, i64>, rows: &[serde_json::Value]) {
    for row in rows {
        if let (Some(day_ms), Some(count)) = (
            row.get("day_ms").and_then(serde_json::Value::as_i64),
            row.get("count").and_then(serde_json::Value::as_i64),
        ) {
            *daily_searches.entry(day_ms).or_insert(0) += count;
        }
    }
}

fn collect_user_ids(all_users: &mut HashSet<String>, rows: &[serde_json::Value]) {
    for row in rows {
        if let Some(user_id) = row.get("user_id").and_then(serde_json::Value::as_str) {
            all_users.insert(user_id.to_string());
        }
    }
}

fn overview_totals_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT COUNT(*) as total, \
         SUM(CASE WHEN has_results = false THEN 1 ELSE 0 END) as no_results, \
         SUM(CASE WHEN query_id IS NOT NULL THEN 1 ELSE 0 END) as tracked \
         FROM searches WHERE timestamp_ms >= {} AND timestamp_ms <= {}",
        start_ms, end_ms
    )
}

fn overview_daily_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT CAST(timestamp_ms / 86400000 * 86400000 AS BIGINT) as day_ms, \
         COUNT(*) as count FROM searches \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
         GROUP BY day_ms",
        start_ms, end_ms
    )
}

fn overview_users_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT DISTINCT COALESCE(user_token, user_ip, 'anonymous') as user_id \
         FROM searches WHERE timestamp_ms >= {} AND timestamp_ms <= {}",
        start_ms, end_ms
    )
}

fn overview_clicks_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT COUNT(*) as count FROM events \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND event_type = 'click'",
        start_ms, end_ms
    )
}

fn tracked_counts_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT query, COUNT(*) as tracked_count \
         FROM searches \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND query_id IS NOT NULL \
         GROUP BY query",
        start_ms, end_ms
    )
}

fn query_id_map_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT query_id, query FROM searches \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} AND query_id IS NOT NULL",
        start_ms, end_ms
    )
}

fn click_counts_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT query_id, COUNT(*) as click_count \
         FROM events \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
           AND event_type = 'click' AND query_id IS NOT NULL \
         GROUP BY query_id",
        start_ms, end_ms
    )
}

fn conversion_counts_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT query_id, COUNT(*) as conv_count \
         FROM events \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
           AND event_type = 'conversion' AND query_id IS NOT NULL \
         GROUP BY query_id",
        start_ms, end_ms
    )
}

fn click_positions_sql(start_ms: i64, end_ms: i64) -> String {
    format!(
        "SELECT query_id, positions \
         FROM events \
         WHERE timestamp_ms >= {} AND timestamp_ms <= {} \
           AND event_type = 'click' AND query_id IS NOT NULL AND positions IS NOT NULL",
        start_ms, end_ms
    )
}

fn map_query_counts(
    rows: &[serde_json::Value],
    query_field: &str,
    count_field: &str,
) -> HashMap<String, i64> {
    rows.iter()
        .filter_map(|row| {
            let query = row.get(query_field)?.as_str()?.to_string();
            let count = row.get(count_field)?.as_i64()?;
            Some((query, count))
        })
        .collect()
}

fn map_query_ids_to_queries(rows: &[serde_json::Value]) -> HashMap<String, String> {
    rows.iter()
        .filter_map(|row| {
            let query_id = row.get("query_id")?.as_str()?.to_string();
            let query = row.get("query")?.as_str()?.to_string();
            Some((query_id, query))
        })
        .collect()
}

/// Sum a numeric field (e.g. click_count) across rows, grouping by the resolved query
/// text via the query-ID-to-query mapping.
fn aggregate_counts_by_query_id(
    rows: &[serde_json::Value],
    qid_to_query: &HashMap<String, String>,
    count_field: &str,
) -> HashMap<String, i64> {
    let mut counts_by_query = HashMap::new();
    for row in rows {
        let Some(query_id) = row.get("query_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(count) = row.get(count_field).and_then(serde_json::Value::as_i64) else {
            continue;
        };
        if let Some(query) = qid_to_query.get(query_id) {
            *counts_by_query.entry(query.clone()).or_insert(0) += count;
        }
    }
    counts_by_query
}

/// Parse the JSON `positions` array from each row and accumulate per-query position
/// sums and counts for computing average click position.
fn aggregate_click_positions_by_query(
    rows: &[serde_json::Value],
    qid_to_query: &HashMap<String, String>,
) -> (HashMap<String, f64>, HashMap<String, i64>) {
    let mut position_sums = HashMap::new();
    let mut position_counts = HashMap::new();

    for row in rows {
        let Some(query_id) = row.get("query_id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(query) = qid_to_query.get(query_id) else {
            continue;
        };
        let positions_raw = row
            .get("positions")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("[]");
        let positions: Vec<f64> = serde_json::from_str(positions_raw).unwrap_or_default();
        for position in positions {
            *position_sums.entry(query.clone()).or_insert(0.0) += position;
            *position_counts.entry(query.clone()).or_insert(0) += 1;
        }
    }

    (position_sums, position_counts)
}

/// Merge click analytics (tracked count, clicks, conversions, average position,
/// click-through rate, conversion rate) into each search result row.
fn enrich_rows_with_click_metrics(
    rows: Vec<serde_json::Value>,
    tracked_by_query: &HashMap<String, i64>,
    clicks_by_query: &HashMap<String, i64>,
    conversions_by_query: &HashMap<String, i64>,
    position_sums: &HashMap<String, f64>,
    position_counts: &HashMap<String, i64>,
) -> Vec<serde_json::Value> {
    rows.into_iter()
        .map(|mut row| {
            let Some(query) = row
                .get("search")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
            else {
                return row;
            };

            let tracked = tracked_by_query.get(&query).copied().unwrap_or(0);
            let clicks = clicks_by_query.get(&query).copied().unwrap_or(0);
            let conversions = conversions_by_query.get(&query).copied().unwrap_or(0);
            let click_through_rate = rounded_ratio(clicks, tracked).unwrap_or(0.0);
            let conversion_rate = rounded_ratio(conversions, tracked).unwrap_or(0.0);
            let average_click_position =
                match (position_sums.get(&query), position_counts.get(&query)) {
                    (Some(sum), Some(count)) if *count > 0 => {
                        (sum / *count as f64 * 10.0).round() / 10.0
                    }
                    _ => 0.0,
                };

            if let Some(obj) = row.as_object_mut() {
                obj.insert(
                    "clickThroughRate".to_string(),
                    serde_json::json!(click_through_rate),
                );
                obj.insert(
                    "conversionRate".to_string(),
                    serde_json::json!(conversion_rate),
                );
                obj.insert("clickCount".to_string(), serde_json::json!(clicks));
                obj.insert("trackedSearchCount".to_string(), serde_json::json!(tracked));
                obj.insert(
                    "conversionCount".to_string(),
                    serde_json::json!(conversions),
                );
                obj.insert(
                    "averageClickPosition".to_string(),
                    serde_json::json!(average_click_position),
                );
            }
            row
        })
        .collect()
}

/// Recursively walk a directory and return paths to all `.parquet` files found. Returns an empty `Vec` if the directory does not exist. Follows Hive-style partitioned layouts where parquet files may be nested in date-keyed subdirectories.
fn find_parquet_files(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    if !dir.exists() {
        return Ok(files);
    }
    fn walk(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir error: {}", e))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("entry error: {}", e))?;
            let path = entry.path();
            if path.is_dir() {
                walk(&path, files)?;
            } else if path.extension().map(|e| e == "parquet").unwrap_or(false) {
                files.push(path);
            }
        }
        Ok(())
    }
    walk(dir, &mut files)?;
    Ok(files)
}

fn contains_parquet_file(dir: &Path) -> Result<bool, String> {
    contains_parquet_file_inner(dir, &mut None)
}

#[cfg(test)]
fn contains_parquet_file_with_entry_budget(dir: &Path, max_entries: usize) -> Result<bool, String> {
    contains_parquet_file_inner(dir, &mut Some(max_entries))
}

/// Return `true` as soon as any `.parquet` file is found anywhere under `dir`.
///
/// Directory entries are pulled lazily from the `read_dir` iterator and each is
/// inspected before the next is read, so a match short-circuits without
/// enumerating (or allocating) the remaining sibling partitions. This keeps the
/// common analytics presence check cheap on large Hive-partitioned trees.
///
/// `remaining_entry_budget`, when set, bounds how many directory entries may be
/// consumed before giving up. Tests use it to prove the walk stops after the
/// first match instead of eagerly enumerating every partition.
fn contains_parquet_file_inner(
    dir: &Path,
    remaining_entry_budget: &mut Option<usize>,
) -> Result<bool, String> {
    if !dir.exists() {
        return Ok(false);
    }

    let entries = std::fs::read_dir(dir).map_err(|e| format!("read_dir error: {}", e))?;
    for entry in entries {
        if let Some(remaining) = remaining_entry_budget.as_mut() {
            if *remaining == 0 {
                return Err("parquet presence check enumerated too many entries".to_string());
            }
            *remaining -= 1;
        }
        let entry = entry.map_err(|e| format!("entry error: {}", e))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type error for {}: {}", path.display(), e))?;
        if file_type.is_dir() {
            if contains_parquet_file_inner(&path, remaining_entry_budget)? {
                return Ok(true);
            }
        } else if path.extension().map(|e| e == "parquet").unwrap_or(false) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn rounded_rate_or_null(numerator: i64, denominator: i64) -> serde_json::Value {
    if denominator > 0 {
        serde_json::json!(((numerator as f64 / denominator as f64) * 1000.0).round() / 1000.0)
    } else {
        serde_json::Value::Null
    }
}

fn date_to_start_ms(date: &str) -> Result<i64, String> {
    let dt = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| format!("Invalid date '{}': {}", date, e))?;
    Ok(dt
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_millis())
}

fn date_to_end_ms(date: &str) -> Result<i64, String> {
    let dt = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .map_err(|e| format!("Invalid date '{}': {}", date, e))?;
    Ok(dt
        .and_hms_opt(23, 59, 59)
        .unwrap()
        .and_utc()
        .timestamp_millis())
}

/// Enumerate UTC calendar dates in `[start_date, end_date]` (inclusive).
fn enumerate_utc_dates_inclusive(start_date: &str, end_date: &str) -> Result<Vec<String>, String> {
    const DAY_MS: i64 = 86_400_000;

    let mut current_ms = date_to_start_ms(start_date)?;
    let end_ms = date_to_start_ms(end_date)?;
    if current_ms > end_ms {
        return Ok(Vec::new());
    }

    let mut dates = Vec::new();
    while current_ms <= end_ms {
        dates.push(ms_to_date_string(current_ms));
        current_ms += DAY_MS;
    }
    Ok(dates)
}

fn ms_to_date_string(ms: i64) -> String {
    let dt = chrono::DateTime::from_timestamp_millis(ms).unwrap_or_default();
    dt.format("%Y-%m-%d").to_string()
}

/// Convert Arrow RecordBatches to JSON rows.
fn batches_to_json(
    batches: &[arrow::record_batch::RecordBatch],
) -> Result<Vec<serde_json::Value>, String> {
    let mut rows = Vec::new();
    for batch in batches {
        let schema = batch.schema();
        for row_idx in 0..batch.num_rows() {
            let mut obj = serde_json::Map::new();
            for (col_idx, field) in schema.fields().iter().enumerate() {
                let col = batch.column(col_idx);
                let value = arrow_value_at(col, row_idx);
                obj.insert(field.name().clone(), value);
            }
            rows.push(serde_json::Value::Object(obj));
        }
    }
    Ok(rows)
}

/// Extract a single cell value from an Arrow array at the given row index, converting it to a `serde_json::Value`. Returns `Null` for unsupported data types or null cells. Supports integer, float, boolean, and string (including `Utf8View` and `LargeUtf8`) arrays.
fn arrow_value_at(col: &dyn arrow::array::Array, idx: usize) -> serde_json::Value {
    use arrow::array::*;
    use arrow::datatypes::DataType;

    if col.is_null(idx) {
        return serde_json::Value::Null;
    }

    match col.data_type() {
        DataType::Int8 => {
            let arr = col.as_any().downcast_ref::<Int8Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Int16 => {
            let arr = col.as_any().downcast_ref::<Int16Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Int32 => {
            let arr = col.as_any().downcast_ref::<Int32Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Int64 => {
            let arr = col.as_any().downcast_ref::<Int64Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::UInt32 => {
            let arr = col.as_any().downcast_ref::<UInt32Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::UInt64 => {
            let arr = col.as_any().downcast_ref::<UInt64Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Float32 => {
            let arr = col.as_any().downcast_ref::<Float32Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Float64 => {
            let arr = col.as_any().downcast_ref::<Float64Array>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Boolean => {
            let arr = col.as_any().downcast_ref::<BooleanArray>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Utf8 => {
            let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::LargeUtf8 => {
            let arr = col.as_any().downcast_ref::<LargeStringArray>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Utf8View => {
            let arr = col.as_any().downcast_ref::<StringViewArray>().unwrap();
            serde_json::json!(arr.value(idx))
        }
        DataType::Binary => {
            let arr = col.as_any().downcast_ref::<BinaryArray>().unwrap();
            let bytes = arr.value(idx);
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        DataType::LargeBinary => {
            let arr = col.as_any().downcast_ref::<LargeBinaryArray>().unwrap();
            let bytes = arr.value(idx);
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        DataType::BinaryView => {
            let arr = col.as_any().downcast_ref::<BinaryViewArray>().unwrap();
            let bytes = arr.value(idx);
            serde_json::json!(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        _ => serde_json::Value::Null,
    }
}
