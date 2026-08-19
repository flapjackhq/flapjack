//! Stub summary for engine/src/analytics/collector.rs.
use dashmap::DashMap;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::sync::Notify;

use super::aggregation::QueryAggregator;
use super::config::AnalyticsConfig;
use super::mutation;
use super::schema::{InsightEvent, SearchEvent};
use super::writer;

/// Maximum number of debug events retained in the ring buffer.
const DEBUG_BUFFER_CAP: usize = 3000;
const FLUSH_LATENCY_SAMPLE_CAP: usize = 2048;

/// A debug entry recorded for every event received via POST /1/events,
/// regardless of whether it passed validation. Used by the event debugger UI.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DebugEvent {
    pub timestamp_ms: i64,
    pub index: String,
    pub event_type: String,
    pub event_subtype: Option<String>,
    pub event_name: String,
    pub user_token: String,
    pub object_ids: Vec<String>,
    pub http_code: u16,
    pub validation_errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AnalyticsMetricsSnapshot {
    pub accepted_events_total: u64,
    pub dropped_events_total: u64,
    pub flush_latency_p99_ms: f64,
    pub rollup_windows_generated_total: u64,
    pub rollup_events_generated_total: u64,
    pub rollup_latest_nonempty_window_end_ms: i64,
    pub soak_marker_first_event_timestamp_ms: i64,
    pub rollup_generation_latency_p99_ms: f64,
}

/// Central analytics event collector.
///
/// Buffers events in memory and flushes to Parquet files either on a timer
/// or when the buffer reaches a threshold size. Uses `std::mem::take` to
/// swap the buffer without holding the lock during I/O.
pub struct AnalyticsCollector {
    config: AnalyticsConfig,
    search_buffer: Mutex<Vec<SearchEvent>>,
    /// Linearizes insight ingestion, flush publication, and user deletion.
    insight_mutation: Mutex<()>,
    insight_buffer: Mutex<Vec<InsightEvent>>,
    #[cfg(test)]
    insight_flush_after_take_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    insight_purge_before_lock_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    debug_buffer: Mutex<VecDeque<DebugEvent>>,
    aggregator: QueryAggregator,
    /// queryID -> (query, index_name, timestamp_ms) for correlating clicks with searches
    query_id_cache: DashMap<String, QueryIdEntry>,
    shutdown: Notify,
    accepted_events_total: AtomicU64,
    dropped_events_total: AtomicU64,
    flush_latency_ms_samples: Mutex<VecDeque<f64>>,
    rollup_windows_generated_total: AtomicU64,
    rollup_events_generated_total: AtomicU64,
    rollup_latest_nonempty_window_end_ms: AtomicI64,
    soak_marker_user_token: Option<String>,
    soak_marker_first_event_timestamp_ms: AtomicI64,
    rollup_latency_ms_samples: Mutex<VecDeque<f64>>,
}

#[derive(Clone)]
pub struct QueryIdEntry {
    pub query: String,
    pub index_name: String,
    pub timestamp_ms: i64,
}

impl AnalyticsCollector {
    /// TODO: Document AnalyticsCollector.new.
    pub fn new(config: AnalyticsConfig) -> Arc<Self> {
        let soak_marker_user_token = std::env::var("FLAPJACK_LOADTEST_SOAK_MARKER_USER_TOKEN")
            .ok()
            .filter(|value| !value.is_empty());
        Arc::new(Self {
            config,
            search_buffer: Mutex::new(Vec::with_capacity(1024)),
            insight_mutation: Mutex::new(()),
            insight_buffer: Mutex::new(Vec::with_capacity(256)),
            #[cfg(test)]
            insight_flush_after_take_hook: Mutex::new(None),
            #[cfg(test)]
            insight_purge_before_lock_hook: Mutex::new(None),
            debug_buffer: Mutex::new(VecDeque::with_capacity(DEBUG_BUFFER_CAP)),
            aggregator: QueryAggregator::new(30),
            query_id_cache: DashMap::new(),
            shutdown: Notify::new(),
            accepted_events_total: AtomicU64::new(0),
            dropped_events_total: AtomicU64::new(0),
            flush_latency_ms_samples: Mutex::new(VecDeque::with_capacity(FLUSH_LATENCY_SAMPLE_CAP)),
            rollup_windows_generated_total: AtomicU64::new(0),
            rollup_events_generated_total: AtomicU64::new(0),
            rollup_latest_nonempty_window_end_ms: AtomicI64::new(0),
            soak_marker_user_token,
            soak_marker_first_event_timestamp_ms: AtomicI64::new(0),
            rollup_latency_ms_samples: Mutex::new(VecDeque::with_capacity(
                FLUSH_LATENCY_SAMPLE_CAP,
            )),
        })
    }

    pub fn config(&self) -> &AnalyticsConfig {
        &self.config
    }

    /// Record a search event. Called from the search path after results are computed.
    pub fn record_search(&self, event: SearchEvent) {
        if !self.config.enabled {
            return;
        }

        // Store queryID mapping for click correlation
        if let Some(ref qid) = event.query_id {
            self.query_id_cache.insert(
                qid.clone(),
                QueryIdEntry {
                    query: event.query.clone(),
                    index_name: event.index_name.clone(),
                    timestamp_ms: event.timestamp_ms,
                },
            );
        }

        // Check aggregation: should this count as a distinct search?
        let user_id = event
            .user_token
            .as_deref()
            .or(event.user_ip.as_deref())
            .unwrap_or("anonymous");
        let _is_new_search = self
            .aggregator
            .should_count(user_id, &event.index_name, &event.query);
        // We always store the raw event; aggregation is applied at query time.
        // The aggregator is kept for future use (e.g. deduped search count queries).
        self.accepted_events_total.fetch_add(1, Ordering::Relaxed);

        let should_flush = {
            let mut buf = self.search_buffer.lock().unwrap();
            buf.push(event);
            buf.len() >= self.config.flush_size
        };

        if should_flush {
            self.flush_searches();
        }
    }

    /// Record an insight event (click, conversion, view).
    pub fn record_insight(&self, event: InsightEvent) {
        if !self.config.enabled {
            return;
        }
        self.record_soak_marker_event_if_match(&event.user_token);
        self.accepted_events_total.fetch_add(1, Ordering::Relaxed);

        let should_flush = {
            let _mutation = self.insight_mutation.lock().unwrap();
            let mut buf = self.insight_buffer.lock().unwrap();
            buf.push(event);
            buf.len() >= self.config.flush_size
        };

        if should_flush {
            self.flush_insights();
        }
    }

    /// Record a debug event entry for the event debugger UI.
    pub fn record_debug_event(&self, event: DebugEvent) {
        let mut buf = self.debug_buffer.lock().unwrap();
        if buf.len() >= DEBUG_BUFFER_CAP {
            buf.pop_front();
        }
        buf.push_back(event);
    }

    /// Query recent debug events from the ring buffer, applying optional filters.
    /// Returns events in reverse-chronological order (newest first), capped at `limit`.
    pub fn get_debug_events(
        &self,
        limit: usize,
        index: Option<&str>,
        event_type: Option<&str>,
        status: Option<&str>,
        from_timestamp_ms: Option<i64>,
        until_timestamp_ms: Option<i64>,
    ) -> Vec<DebugEvent> {
        let buf = self.debug_buffer.lock().unwrap();
        buf.iter()
            .rev()
            .filter(|e| {
                if let Some(idx) = index {
                    if e.index != idx {
                        return false;
                    }
                }
                if let Some(et) = event_type {
                    if e.event_type != et {
                        return false;
                    }
                }
                if let Some(st) = status {
                    match st {
                        "ok" if e.http_code != 200 => {
                            return false;
                        }
                        "error" if e.http_code == 200 => {
                            return false;
                        }
                        _ => {}
                    }
                }
                if let Some(from_ms) = from_timestamp_ms {
                    if e.timestamp_ms < from_ms {
                        return false;
                    }
                }
                if let Some(until_ms) = until_timestamp_ms {
                    if e.timestamp_ms > until_ms {
                        return false;
                    }
                }
                true
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// Look up a queryID to correlate with the original search.
    pub fn lookup_query_id(&self, query_id: &str) -> Option<QueryIdEntry> {
        self.query_id_cache.get(query_id).map(|e| e.clone())
    }

    /// Flush search events to Parquet. Swaps buffer to avoid holding lock during I/O.
    pub fn flush_searches(&self) {
        let flush_started_at = Instant::now();
        let events = {
            let mut buf = self.search_buffer.lock().unwrap();
            std::mem::take(&mut *buf)
        };
        if events.is_empty() {
            return;
        }

        // Group events by index_name for per-index Parquet files
        let mut by_index: std::collections::HashMap<String, Vec<SearchEvent>> =
            std::collections::HashMap::new();
        for event in events {
            by_index
                .entry(event.index_name.clone())
                .or_default()
                .push(event);
        }

        let mut dropped_events = 0_u64;
        for (index_name, index_events) in by_index {
            let dir = self.config.searches_dir(&index_name);
            if let Err(e) = writer::flush_search_events(&index_events, &dir) {
                dropped_events += index_events.len() as u64;
                tracing::error!(
                    "[analytics] Failed to flush {} search events for {}: {}",
                    index_events.len(),
                    index_name,
                    e
                );
            } else {
                tracing::debug!(
                    "[analytics] Flushed {} search events for {}",
                    index_events.len(),
                    index_name
                );
            }
        }
        if dropped_events > 0 {
            self.dropped_events_total
                .fetch_add(dropped_events, Ordering::Relaxed);
        }
        self.record_flush_latency_sample(flush_started_at.elapsed().as_secs_f64() * 1000.0);
    }

    /// Flush insight events to Parquet.
    pub fn flush_insights(&self) {
        let flush_started_at = Instant::now();
        let _mutation = self.insight_mutation.lock().unwrap();
        let events = {
            let mut buf = self.insight_buffer.lock().unwrap();
            std::mem::take(&mut *buf)
        };
        if events.is_empty() {
            return;
        }
        #[cfg(test)]
        if let Some(hook) = self.insight_flush_after_take_hook.lock().unwrap().clone() {
            hook();
        }

        let mut by_index: std::collections::HashMap<String, Vec<InsightEvent>> =
            std::collections::HashMap::new();
        for event in events {
            by_index.entry(event.index.clone()).or_default().push(event);
        }

        let mut dropped_events = 0_u64;
        for (index_name, index_events) in by_index {
            let dir = self.config.events_dir(&index_name);
            if let Err(e) = mutation::with_index_mutation(&self.config, &index_name, || {
                writer::flush_insight_events(&index_events, &dir)
            }) {
                dropped_events += index_events.len() as u64;
                tracing::error!(
                    "[analytics] Failed to flush {} insight events for {}: {}",
                    index_events.len(),
                    index_name,
                    e
                );
            } else {
                tracing::debug!(
                    "[analytics] Flushed {} insight events for {}",
                    index_events.len(),
                    index_name
                );
            }
        }
        if dropped_events > 0 {
            self.dropped_events_total
                .fetch_add(dropped_events, Ordering::Relaxed);
        }
        self.record_flush_latency_sample(flush_started_at.elapsed().as_secs_f64() * 1000.0);
    }

    /// Flush all buffers (called at shutdown or periodically).
    pub fn flush_all(&self) {
        self.flush_searches();
        self.flush_insights();
    }

    /// Purge all insight events for a user token from memory and on-disk analytics data.
    /// Returns number of removed events.
    pub fn purge_user_token(&self, user_token: &str) -> Result<u64, String> {
        self.purge_user_token_matching_index(user_token, None)
    }

    /// Purge insight events for a user token only where the event's index matches.
    /// Returns number of removed events.
    pub fn purge_user_token_where_index(
        &self,
        user_token: &str,
        index_matches: &dyn Fn(&str) -> bool,
    ) -> Result<u64, String> {
        self.purge_user_token_matching_index(user_token, Some(index_matches))
    }

    fn purge_user_token_matching_index(
        &self,
        user_token: &str,
        index_matches: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<u64, String> {
        #[cfg(test)]
        if let Some(hook) = self.insight_purge_before_lock_hook.lock().unwrap().clone() {
            hook();
        }
        let _mutation = self
            .insight_mutation
            .lock()
            .map_err(|error| format!("analytics insight mutation lock poisoned: {error}"))?;
        let events_dirs = self.events_dirs_for_user_token_purge(index_matches)?;

        let removed_from_buffer = {
            let mut buf = self.insight_buffer.lock().unwrap();
            let before = buf.len();
            buf.retain(|event| {
                event.user_token != user_token
                    || !index_matches.is_none_or(|matches| matches(&event.index))
            });
            (before - buf.len()) as u64
        };

        let removed_from_debug = {
            let mut buf = self.debug_buffer.lock().unwrap();
            let before = buf.len();
            buf.retain(|event| {
                event.user_token != user_token
                    || !index_matches.is_none_or(|matches| matches(&event.index))
            });
            (before - buf.len()) as u64
        };

        let mut removed_from_disk = 0_u64;
        for (index_root, events_dir) in events_dirs {
            removed_from_disk += mutation::with_index_root_mutation(index_root, || {
                writer::purge_insight_events_for_user_token(&events_dir, user_token)
            })?;
        }

        Ok(removed_from_buffer + removed_from_debug + removed_from_disk)
    }

    /// Resolve every selected on-disk events root before deleting from any
    /// in-memory or persisted store. `symlink_metadata` deliberately does not
    /// follow the configured root or events leaves outside the analytics tree.
    fn events_dirs_for_user_token_purge(
        &self,
        index_matches: Option<&dyn Fn(&str) -> bool>,
    ) -> Result<Vec<(std::path::PathBuf, std::path::PathBuf)>, String> {
        match std::fs::symlink_metadata(&self.config.data_dir) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing to traverse symlinked analytics path {}",
                    self.config.data_dir.display()
                ));
            }
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                return Err(format!(
                    "analytics data root is not a directory: {}",
                    self.config.data_dir.display()
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => {
                return Err(format!(
                    "Failed to stat analytics data dir {}: {}",
                    self.config.data_dir.display(),
                    error
                ));
            }
        }

        let entries = std::fs::read_dir(&self.config.data_dir)
            .map_err(|e| format!("Failed to read analytics data dir: {}", e))?;
        let mut events_dirs = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("Failed to read analytics entry: {}", e))?;
            let file_type = entry.file_type().map_err(|e| {
                format!(
                    "Failed to stat analytics entry {}: {}",
                    entry.path().display(),
                    e
                )
            })?;
            if file_type.is_symlink() {
                return Err(format!(
                    "refusing to traverse symlinked analytics path {}",
                    entry.path().display()
                ));
            }
            let index_dir = entry.path();
            if !file_type.is_dir() {
                continue;
            }
            if index_matches.is_some_and(|matches| {
                index_dir
                    .file_name()
                    .and_then(AnalyticsConfig::value_from_path_component)
                    .is_none_or(|index| !matches(&index))
            }) {
                continue;
            }

            let events_dir = index_dir.join("events");
            events_dirs.push((index_dir, events_dir));
        }

        events_dirs.sort_by(|left, right| left.0.cmp(&right.0));
        for (_, events_dir) in &events_dirs {
            writer::preflight_insight_events_purge(events_dir)?;
        }
        Ok(events_dirs)
    }

    /// Start the background flush loop. Should be spawned as a tokio task.
    pub async fn run_flush_loop(self: Arc<Self>) {
        let interval = tokio::time::Duration::from_secs(self.config.flush_interval_secs);
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip the first immediate tick

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.flush_all();
                    self.aggregator.evict_expired();
                    self.evict_old_query_ids();
                }
                _ = self.shutdown.notified() => {
                    self.flush_all();
                    tracing::info!("[analytics] Flush loop shutting down");
                    break;
                }
            }
        }
    }

    /// Signal the flush loop to stop.
    pub fn shutdown(&self) {
        self.shutdown.notify_one();
    }

    /// TODO: Document AnalyticsCollector.analytics_metrics_snapshot.
    pub fn analytics_metrics_snapshot(&self) -> AnalyticsMetricsSnapshot {
        let flush_samples: Vec<f64> = {
            let samples = self.flush_latency_ms_samples.lock().unwrap();
            samples.iter().copied().collect()
        };
        let rollup_samples: Vec<f64> = {
            let samples = self.rollup_latency_ms_samples.lock().unwrap();
            samples.iter().copied().collect()
        };
        AnalyticsMetricsSnapshot {
            accepted_events_total: self.accepted_events_total.load(Ordering::Relaxed),
            dropped_events_total: self.dropped_events_total.load(Ordering::Relaxed),
            flush_latency_p99_ms: percentile_99(&flush_samples),
            rollup_windows_generated_total: self
                .rollup_windows_generated_total
                .load(Ordering::Relaxed),
            rollup_events_generated_total: self
                .rollup_events_generated_total
                .load(Ordering::Relaxed),
            rollup_latest_nonempty_window_end_ms: self
                .rollup_latest_nonempty_window_end_ms
                .load(Ordering::Relaxed),
            soak_marker_first_event_timestamp_ms: self
                .soak_marker_first_event_timestamp_ms
                .load(Ordering::Relaxed),
            rollup_generation_latency_p99_ms: percentile_99(&rollup_samples),
        }
    }

    /// Records latency for one rollup window generated by the running server.
    pub fn record_rollup_generation_sample(
        &self,
        sample_ms: f64,
        event_count: i64,
        window_end_ms: i64,
    ) {
        self.rollup_windows_generated_total
            .fetch_add(1, Ordering::Relaxed);
        if event_count > 0 {
            self.rollup_events_generated_total
                .fetch_add(event_count as u64, Ordering::Relaxed);
            self.rollup_latest_nonempty_window_end_ms
                .store(window_end_ms, Ordering::Relaxed);
        }
        let mut samples = self.rollup_latency_ms_samples.lock().unwrap();
        if samples.len() >= FLUSH_LATENCY_SAMPLE_CAP {
            samples.pop_front();
        }
        samples.push_back(sample_ms);
    }

    /// Evict queryID entries older than 1 hour.
    fn evict_old_query_ids(&self) {
        let cutoff = chrono::Utc::now().timestamp_millis() - 3_600_000;
        self.query_id_cache.retain(|_, v| v.timestamp_ms > cutoff);
    }

    fn record_flush_latency_sample(&self, sample_ms: f64) {
        let mut samples = self.flush_latency_ms_samples.lock().unwrap();
        if samples.len() >= FLUSH_LATENCY_SAMPLE_CAP {
            samples.pop_front();
        }
        samples.push_back(sample_ms);
    }

    fn record_soak_marker_event_if_match(&self, user_token: &str) {
        let Some(marker_user_token) = self.soak_marker_user_token.as_deref() else {
            return;
        };
        if user_token != marker_user_token {
            return;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        let _ = self.soak_marker_first_event_timestamp_ms.compare_exchange(
            0,
            now_ms,
            Ordering::Relaxed,
            Ordering::Relaxed,
        );
    }
}

fn percentile_99(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let last_idx = sorted.len() - 1;
    let idx = ((last_idx as f64) * 0.99).ceil() as usize;
    sorted[idx.min(last_idx)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(temp_dir: &TempDir) -> AnalyticsConfig {
        AnalyticsConfig {
            enabled: true,
            data_dir: temp_dir.path().to_path_buf(),
            flush_interval_secs: 60,
            flush_size: 100,
            retention_days: 90,
        }
    }

    /// TODO: Document test_event.
    fn test_event(
        timestamp_ms: i64,
        index: &str,
        event_type: &str,
        event_name: &str,
        http_code: u16,
    ) -> DebugEvent {
        DebugEvent {
            timestamp_ms,
            index: index.to_string(),
            event_type: event_type.to_string(),
            event_subtype: None,
            event_name: event_name.to_string(),
            user_token: "user-1".to_string(),
            object_ids: vec!["obj-1".to_string()],
            http_code,
            validation_errors: if http_code == 200 {
                vec![]
            } else {
                vec!["validation failed".to_string()]
            },
        }
    }

    fn insight_event(user_token: &str) -> InsightEvent {
        InsightEvent {
            event_type: "view".to_string(),
            event_subtype: None,
            event_name: "Viewed".to_string(),
            index: "products".to_string(),
            user_token: user_token.to_string(),
            authenticated_user_token: None,
            query_id: None,
            object_ids: vec![format!("object-{user_token}")],
            object_ids_alt: vec![],
            positions: None,
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            value: None,
            currency: None,
            interleaving_team: None,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn user_purge_waits_for_inflight_flush_and_prevents_resurrection() {
        use std::sync::{mpsc, Barrier};
        use std::time::Duration;

        let temp_dir = TempDir::new().expect("temp dir");
        let config = test_config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        collector.record_insight(insight_event("delete-me"));
        collector.record_insight(insight_event("safe-user"));

        let flush_reached_barrier = Arc::new(Barrier::new(2));
        let release_flush_barrier = Arc::new(Barrier::new(2));
        let hook_reached = Arc::clone(&flush_reached_barrier);
        let hook_release = Arc::clone(&release_flush_barrier);
        *collector.insight_flush_after_take_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_reached.wait();
            hook_release.wait();
        }));

        let flush_collector = Arc::clone(&collector);
        let flush_thread = std::thread::spawn(move || flush_collector.flush_insights());
        flush_reached_barrier.wait();

        let (purge_done_tx, purge_done_rx) = mpsc::channel();
        let purge_attempted_barrier = Arc::new(Barrier::new(2));
        let hook_attempted = Arc::clone(&purge_attempted_barrier);
        *collector.insight_purge_before_lock_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_attempted.wait();
        }));
        let purge_collector = Arc::clone(&collector);
        let purge_thread = std::thread::spawn(move || {
            let result = purge_collector.purge_user_token("delete-me");
            purge_done_tx.send(result).unwrap();
        });
        purge_attempted_barrier.wait();
        assert!(
            purge_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "purge must wait while the taken insight batch is still publishing"
        );

        release_flush_barrier.wait();
        flush_thread.join().unwrap();
        let removed = purge_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("purge should finish after flush publication")
            .unwrap();
        purge_thread.join().unwrap();
        assert_eq!(removed, 1);

        let rows = crate::analytics::AnalyticsQueryEngine::new(config)
            .query_events(
                "products",
                "SELECT user_token, COUNT(*) AS count FROM events GROUP BY user_token ORDER BY user_token",
            )
            .await
            .unwrap();
        assert!(
            !rows
                .iter()
                .any(|row| row.get("user_token") == Some(&serde_json::json!("delete-me"))),
            "deleted user resurrected after the in-flight flush: {rows:?}"
        );
        assert!(
            rows.iter()
                .any(|row| row.get("user_token") == Some(&serde_json::json!("safe-user"))),
            "non-target control event was lost: {rows:?}"
        );
    }

    #[test]
    fn get_debug_events_filters_by_index() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_debug_event(test_event(100, "products", "search", "prod-old", 200));
        collector.record_debug_event(test_event(200, "users", "search", "users", 200));
        collector.record_debug_event(test_event(300, "products", "click", "prod-new", 200));

        let events = collector.get_debug_events(10, Some("products"), None, None, None, None);

        let names: Vec<String> = events.into_iter().map(|e| e.event_name).collect();
        assert_eq!(names, vec!["prod-new".to_string(), "prod-old".to_string()]);
    }

    #[test]
    fn get_debug_events_filters_by_event_type() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_debug_event(test_event(100, "products", "search", "search-old", 200));
        collector.record_debug_event(test_event(200, "products", "click", "click", 200));
        collector.record_debug_event(test_event(300, "products", "search", "search-new", 200));

        let events = collector.get_debug_events(10, None, Some("search"), None, None, None);

        let names: Vec<String> = events.into_iter().map(|e| e.event_name).collect();
        assert_eq!(
            names,
            vec!["search-new".to_string(), "search-old".to_string()]
        );
    }

    /// TODO: Document get_debug_events_filters_by_status_ok_and_error.
    #[test]
    fn get_debug_events_filters_by_status_ok_and_error() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_debug_event(test_event(100, "products", "search", "ok", 200));
        collector.record_debug_event(test_event(200, "products", "search", "bad-request", 400));
        collector.record_debug_event(test_event(300, "products", "search", "server-error", 500));

        let ok_events = collector.get_debug_events(10, None, None, Some("ok"), None, None);
        let error_events = collector.get_debug_events(10, None, None, Some("error"), None, None);

        let ok_names: Vec<String> = ok_events.into_iter().map(|e| e.event_name).collect();
        let error_names: Vec<String> = error_events.into_iter().map(|e| e.event_name).collect();
        assert_eq!(ok_names, vec!["ok".to_string()]);
        assert_eq!(
            error_names,
            vec!["server-error".to_string(), "bad-request".to_string()]
        );
    }

    #[test]
    fn get_debug_events_filters_by_timestamp_window() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_debug_event(test_event(100, "products", "search", "old", 200));
        collector.record_debug_event(test_event(200, "products", "search", "inside", 200));
        collector.record_debug_event(test_event(300, "products", "search", "new", 200));

        let events = collector.get_debug_events(10, None, None, None, Some(150), Some(250));

        let names: Vec<String> = events.into_iter().map(|e| e.event_name).collect();
        assert_eq!(names, vec!["inside".to_string()]);
    }

    #[test]
    fn get_debug_events_applies_limit_after_filtering() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_debug_event(test_event(100, "products", "search", "one", 200));
        collector.record_debug_event(test_event(200, "products", "search", "two", 200));
        collector.record_debug_event(test_event(300, "products", "search", "three", 200));

        let events = collector.get_debug_events(2, Some("products"), None, None, None, None);

        let names: Vec<String> = events.into_iter().map(|e| e.event_name).collect();
        assert_eq!(names, vec!["three".to_string(), "two".to_string()]);
    }

    /// TODO: Document get_debug_events_returns_reverse_chronological_order.
    #[test]
    fn get_debug_events_returns_reverse_chronological_order() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_debug_event(test_event(100, "products", "search", "first", 200));
        collector.record_debug_event(test_event(200, "products", "search", "second", 200));
        collector.record_debug_event(test_event(300, "products", "search", "third", 200));

        let events = collector.get_debug_events(10, None, None, None, None, None);

        let names: Vec<String> = events.into_iter().map(|e| e.event_name).collect();
        assert_eq!(
            names,
            vec![
                "third".to_string(),
                "second".to_string(),
                "first".to_string()
            ]
        );
    }

    /// TODO: Document analytics_metrics_snapshot_tracks_accepted_events_and_flush_latency.
    #[test]
    fn analytics_metrics_snapshot_tracks_accepted_events_and_flush_latency() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_search(SearchEvent {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            query: "laptop".to_string(),
            query_id: None,
            index_name: "products".to_string(),
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
        });

        collector.flush_all();
        collector.record_rollup_generation_sample(123.0, 7, 3_600_000);
        let snapshot = collector.analytics_metrics_snapshot();
        assert_eq!(snapshot.accepted_events_total, 1);
        assert_eq!(snapshot.dropped_events_total, 0);
        assert!(
            snapshot.flush_latency_p99_ms >= 0.0,
            "flush latency p99 should be recorded"
        );
        assert_eq!(snapshot.rollup_windows_generated_total, 1);
        assert_eq!(snapshot.rollup_events_generated_total, 7);
        assert_eq!(snapshot.rollup_latest_nonempty_window_end_ms, 3_600_000);
        assert_eq!(snapshot.soak_marker_first_event_timestamp_ms, 0);
        assert!(
            snapshot.rollup_generation_latency_p99_ms >= 123.0,
            "rollup generation p99 should be recorded"
        );
    }

    /// TODO: Document rollup_boundary_metric_only_tracks_nonempty_windows.
    #[test]
    fn rollup_boundary_metric_only_tracks_nonempty_windows() {
        let temp_dir = TempDir::new().expect("temp dir");
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_rollup_generation_sample(10.0, 0, 7_200_000);
        assert_eq!(
            collector
                .analytics_metrics_snapshot()
                .rollup_latest_nonempty_window_end_ms,
            0
        );

        collector.record_rollup_generation_sample(12.0, 2, 10_800_000);
        assert_eq!(
            collector
                .analytics_metrics_snapshot()
                .rollup_latest_nonempty_window_end_ms,
            10_800_000
        );
    }

    /// TODO: Document soak_marker_metric_tracks_first_matching_insight_event.
    #[test]
    fn soak_marker_metric_tracks_first_matching_insight_event() {
        let temp_dir = TempDir::new().expect("temp dir");
        let marker_token = "soak-marker-test-token";
        unsafe {
            std::env::set_var("FLAPJACK_LOADTEST_SOAK_MARKER_USER_TOKEN", marker_token);
        }
        let collector = AnalyticsCollector::new(test_config(&temp_dir));
        collector.record_insight(InsightEvent {
            event_type: "click".to_string(),
            event_subtype: None,
            event_name: "control-event".to_string(),
            index: "products".to_string(),
            user_token: "someone-else".to_string(),
            authenticated_user_token: None,
            query_id: Some("0123456789abcdef0123456789abcdef".to_string()),
            object_ids: vec!["obj-1".to_string()],
            object_ids_alt: vec![],
            positions: Some(vec![1]),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            value: None,
            currency: None,
            interleaving_team: None,
        });
        let before = collector
            .analytics_metrics_snapshot()
            .soak_marker_first_event_timestamp_ms;
        assert_eq!(before, 0);

        collector.record_insight(InsightEvent {
            event_type: "click".to_string(),
            event_subtype: None,
            event_name: "marker".to_string(),
            index: "products".to_string(),
            user_token: marker_token.to_string(),
            authenticated_user_token: None,
            query_id: Some("fedcba9876543210fedcba9876543210".to_string()),
            object_ids: vec!["obj-1".to_string()],
            object_ids_alt: vec![],
            positions: Some(vec![1]),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            value: None,
            currency: None,
            interleaving_team: None,
        });
        let first = collector
            .analytics_metrics_snapshot()
            .soak_marker_first_event_timestamp_ms;
        assert!(
            first > 0,
            "expected first matching marker insight event timestamp"
        );

        collector.record_insight(InsightEvent {
            event_type: "click".to_string(),
            event_subtype: None,
            event_name: "marker-again".to_string(),
            index: "products".to_string(),
            user_token: marker_token.to_string(),
            authenticated_user_token: None,
            query_id: Some("00112233445566778899aabbccddeeff".to_string()),
            object_ids: vec!["obj-2".to_string()],
            object_ids_alt: vec![],
            positions: Some(vec![1]),
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            value: None,
            currency: None,
            interleaving_team: None,
        });
        let after = collector
            .analytics_metrics_snapshot()
            .soak_marker_first_event_timestamp_ms;
        assert_eq!(after, first);

        unsafe {
            std::env::remove_var("FLAPJACK_LOADTEST_SOAK_MARKER_USER_TOKEN");
        }
    }
}
