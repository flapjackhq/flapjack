//! Stub summary for engine/src/analytics/collector.rs.
use dashmap::DashMap;
use std::collections::VecDeque;
#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tokio::sync::Notify;

use super::aggregation::QueryAggregator;
use super::config::AnalyticsConfig;
use super::mutation;
use super::schema::{
    is_recommendation_fallback_identity, InsightEvent, SearchEvent,
    RECOMMENDATION_FALLBACK_IDENTITY_MARKER, RECOMMENDATION_REQUEST_EVENT_TYPE,
};
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
    /// Exact indexes deleted by the lifecycle owner. This closes the narrow
    /// gap where a request that searched before deletion records afterward.
    deleted_indexes: DashMap<String, ()>,
    /// Canonical tenant root bound by IndexManager. Absence is the durable
    /// admission fence across restart; `deleted_indexes` covers only in-flight
    /// deletion before the tenant directory is removed.
    index_data_dir: OnceLock<std::path::PathBuf>,
    #[cfg(test)]
    fail_next_quarantine_remove: AtomicBool,
    /// Linearizes search ingestion, flush publication, and exact-index deletion.
    search_mutation: Mutex<()>,
    search_buffer: Mutex<Vec<SearchEvent>>,
    #[cfg(test)]
    search_flush_after_take_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// Linearizes insight ingestion, batch take, and deletion admission.
    insight_mutation: Mutex<()>,
    /// Prevents an exact-user purge from committing while a taken insight
    /// batch is still publishing. Exact-index deletion deliberately uses its
    /// existing admission and per-index fences so it need not wait for I/O.
    /// Paths needing both locks acquire this before `insight_mutation`.
    insight_publication: Mutex<()>,
    insight_buffer: Mutex<Vec<InsightEvent>>,
    #[cfg(test)]
    insight_flush_after_take_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    insight_purge_before_lock_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    index_purge_before_lock_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
            deleted_indexes: DashMap::new(),
            index_data_dir: OnceLock::new(),
            #[cfg(test)]
            fail_next_quarantine_remove: AtomicBool::new(false),
            search_mutation: Mutex::new(()),
            search_buffer: Mutex::new(Vec::with_capacity(1024)),
            #[cfg(test)]
            search_flush_after_take_hook: Mutex::new(None),
            insight_mutation: Mutex::new(()),
            insight_publication: Mutex::new(()),
            insight_buffer: Mutex::new(Vec::with_capacity(256)),
            #[cfg(test)]
            insight_flush_after_take_hook: Mutex::new(None),
            #[cfg(test)]
            insight_purge_before_lock_hook: Mutex::new(None),
            #[cfg(test)]
            index_purge_before_lock_hook: Mutex::new(None),
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

    pub(crate) fn bind_index_data_dir(&self, data_dir: std::path::PathBuf) {
        let _ = self.index_data_dir.set(data_dir);
    }

    fn index_admission_closed(&self, index_name: &str) -> bool {
        self.deleted_indexes.contains_key(index_name)
            || self.index_data_dir.get().is_some_and(|data_dir| {
                crate::index::manager::validate_index_name(index_name).is_err()
                    || !data_dir.join(index_name).is_dir()
            })
    }

    /// Record a search event. Called from the search path after results are computed.
    pub fn record_search(&self, event: SearchEvent) {
        if !self.config.enabled {
            return;
        }

        let should_flush = {
            let _mutation = self.search_mutation.lock().unwrap();
            // The search may have resolved immediately before deletion. Check
            // durable tenant presence here so its late analytics cannot create
            // queryID/buffer state after the physical generation is gone.
            if self.index_admission_closed(&event.index_name) {
                self.dropped_events_total.fetch_add(1, Ordering::Relaxed);
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
            let _is_new_search =
                self.aggregator
                    .should_count(user_id, &event.index_name, &event.query);
            // We always store the raw event; aggregation is applied at query time.
            // The aggregator is kept for future use (e.g. deduped search count queries).
            self.accepted_events_total.fetch_add(1, Ordering::Relaxed);

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
            if self.index_admission_closed(&event.index) {
                self.dropped_events_total.fetch_add(1, Ordering::Relaxed);
                return;
            }
            let mut buf = self.insight_buffer.lock().unwrap();
            buf.push(event);
            buf.len() >= self.config.flush_size
        };

        if should_flush {
            self.flush_insights();
        }
    }

    /// Record one successfully admitted Recommend request in the existing
    /// durable insight pipeline. The public Insights validator cannot create
    /// this internal discriminator.
    pub fn record_recommendation_request(
        &self,
        index_name: &str,
        model: &str,
        user_token: Option<&str>,
        user_ip: Option<&str>,
        query_id: Option<String>,
        timestamp_ms: i64,
    ) {
        let identity = writer::recommendation_user_identity(user_token, user_ip);
        self.record_insight(InsightEvent {
            event_type: RECOMMENDATION_REQUEST_EVENT_TYPE.to_string(),
            event_subtype: Some(model.to_string()),
            event_name: "Recommend request".to_string(),
            index: index_name.to_string(),
            user_token: identity,
            authenticated_user_token: user_token
                .is_none()
                .then(|| RECOMMENDATION_FALLBACK_IDENTITY_MARKER.to_string()),
            query_id,
            object_ids: Vec::new(),
            object_ids_alt: Vec::new(),
            positions: None,
            timestamp: Some(timestamp_ms),
            value: None,
            currency: None,
            interleaving_team: None,
        });
    }

    /// Record a debug event entry for the event debugger UI.
    pub fn record_debug_event(&self, event: DebugEvent) {
        let _mutation = self.search_mutation.lock().unwrap();
        if self.index_admission_closed(&event.index) {
            return;
        }
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
            let _mutation = self.search_mutation.lock().unwrap();
            let mut buf = self.search_buffer.lock().unwrap();
            std::mem::take(&mut *buf)
        };
        if events.is_empty() {
            return;
        }
        #[cfg(test)]
        if let Some(hook) = self.search_flush_after_take_hook.lock().unwrap().clone() {
            hook();
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
            match mutation::with_index_mutation(&self.config, &index_name, || {
                if self.index_admission_closed(&index_name) {
                    return Ok(false);
                }
                writer::flush_search_events(&index_events, &dir)?;
                Ok(true)
            }) {
                Ok(true) => tracing::debug!(
                    "[analytics] Flushed {} search events for {}",
                    index_events.len(),
                    index_name
                ),
                Ok(false) => dropped_events += index_events.len() as u64,
                Err(e) => {
                    dropped_events += index_events.len() as u64;
                    tracing::error!(
                        "[analytics] Failed to flush {} search events for {}: {}",
                        index_events.len(),
                        index_name,
                        e
                    );
                }
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
        let _publication = self.insight_publication.lock().unwrap();
        let events = {
            let _mutation = self.insight_mutation.lock().unwrap();
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
            match mutation::with_index_mutation(&self.config, &index_name, || {
                if self.index_admission_closed(&index_name) {
                    return Ok(false);
                }
                writer::flush_insight_events(&index_events, &dir)?;
                Ok(true)
            }) {
                Ok(true) => tracing::debug!(
                    "[analytics] Flushed {} insight events for {}",
                    index_events.len(),
                    index_name
                ),
                Ok(false) => dropped_events += index_events.len() as u64,
                Err(e) => {
                    dropped_events += index_events.len() as u64;
                    tracing::error!(
                        "[analytics] Failed to flush {} insight events for {}: {}",
                        index_events.len(),
                        index_name,
                        e
                    );
                }
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

    /// Run the complete periodic analytics mutation pass. HTTP-layer
    /// schedulers call this only while holding the process-wide release
    /// mutation permit, keeping persistence and cache eviction inside one
    /// admitted effect.
    pub fn run_periodic_flush_pass(&self) {
        self.flush_all();
        self.aggregator.evict_expired();
        self.evict_old_query_ids();
    }

    /// Generate one rollup through the same exact-index admission owner as
    /// deletion, so a stale scheduler snapshot cannot recreate deleted data.
    pub fn flush_rollup_window_with_event_count(
        &self,
        index_name: &str,
        tier: &str,
        window_start_ms: i64,
        window_end_ms: i64,
    ) -> Result<(std::path::PathBuf, i64), String> {
        mutation::with_index_mutation(&self.config, index_name, || {
            if self.index_admission_closed(index_name) {
                return Err("analytics index is pending deletion".to_string());
            }
            writer::flush_rollup_window_with_event_count(
                &self.config,
                index_name,
                tier,
                window_start_ms,
                window_end_ms,
            )
        })
    }

    /// Fence late ingress and atomically move persisted analytics out of the
    /// canonical namespace before the tenant tree is removed.
    pub fn stage_index_deletion(
        &self,
        index_name: &str,
        quarantine: &std::path::Path,
    ) -> Result<(), String> {
        #[cfg(test)]
        if let Some(hook) = self.index_purge_before_lock_hook.lock().unwrap().clone() {
            hook();
        }
        {
            let _search_mutation = self.search_mutation.lock().unwrap();
            let _insight_mutation = self.insight_mutation.lock().unwrap();
            self.deleted_indexes.insert(index_name.to_string(), ());
        }
        if let Err(error) = mutation::stage_index_root(&self.config, index_name, quarantine) {
            self.deleted_indexes.remove(index_name);
            return Err(error);
        }
        Ok(())
    }

    /// Restore staged analytics when physical tenant removal did not commit.
    pub fn rollback_index_deletion(
        &self,
        index_name: &str,
        quarantine: &std::path::Path,
    ) -> Result<(), String> {
        mutation::rollback_staged_index_root(&self.config, index_name, quarantine)?;
        {
            let _search_mutation = self.search_mutation.lock().unwrap();
            let _insight_mutation = self.insight_mutation.lock().unwrap();
            self.deleted_indexes.remove(index_name);
        }
        Ok(())
    }

    /// Commit exact-index in-memory deletion and erase the staged filesystem
    /// tree. A transient erase failure leaves the lifecycle fence in place for
    /// startup or an explicit absent-index retry.
    pub fn finish_index_deletion(
        &self,
        index_name: &str,
        quarantine: &std::path::Path,
    ) -> Result<(), String> {
        {
            let _search_mutation = self.search_mutation.lock().unwrap();
            let _insight_mutation = self.insight_mutation.lock().unwrap();
            self.deleted_indexes.insert(index_name.to_string(), ());
            self.purge_index_memory_locked(index_name)?;
        }
        #[cfg(test)]
        if self
            .fail_next_quarantine_remove
            .swap(false, Ordering::SeqCst)
        {
            return Err("injected analytics quarantine erase failure".to_string());
        }
        let result = mutation::remove_staged_index_root(&self.config, index_name, quarantine);
        self.deleted_indexes.remove(index_name);
        result
    }

    fn purge_index_memory_locked(&self, index_name: &str) -> Result<(), String> {
        self.search_buffer
            .lock()
            .map_err(|error| format!("analytics search buffer poisoned: {error}"))?
            .retain(|event| event.index_name != index_name);
        self.insight_buffer
            .lock()
            .map_err(|error| format!("analytics insight buffer poisoned: {error}"))?
            .retain(|event| event.index != index_name);
        self.debug_buffer
            .lock()
            .map_err(|error| format!("analytics debug buffer poisoned: {error}"))?
            .retain(|event| event.index != index_name);
        self.query_id_cache
            .retain(|_, entry| entry.index_name != index_name);
        self.aggregator.purge_index(index_name);
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn fail_next_quarantine_remove_for_test(&self) {
        self.fail_next_quarantine_remove
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn deleting_index_count(&self) -> usize {
        self.deleted_indexes.len()
    }

    /// Reactivate analytics admission only after the lifecycle owner has made
    /// the exact tenant visible again.
    pub fn activate_index(&self, index_name: &str) {
        let _search_mutation = self.search_mutation.lock().unwrap();
        let _insight_mutation = self.insight_mutation.lock().unwrap();
        self.deleted_indexes.remove(index_name);
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
        let _publication = self
            .insight_publication
            .lock()
            .map_err(|error| format!("analytics insight publication lock poisoned: {error}"))?;
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
                    || is_recommendation_fallback_identity(event)
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
                    self.run_periodic_flush_pass();
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
    use crate::analytics::schema::SearchEvent;
    use crate::analytics::AnalyticsQueryEngine;
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

    fn search_event(index_name: &str, query_id: &str) -> SearchEvent {
        SearchEvent {
            timestamp_ms: chrono::Utc::now().timestamp_millis(),
            query: "query".to_string(),
            query_id: Some(query_id.to_string()),
            index_name: index_name.to_string(),
            nb_hits: 1,
            processing_time_ms: 1,
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

    fn purge_index(collector: &AnalyticsCollector, config: &AnalyticsConfig, index_name: &str) {
        let quarantine = config.data_dir.join(format!(".{index_name}-delete-test"));
        collector
            .stage_index_deletion(index_name, &quarantine)
            .expect("stage index deletion");
        if let Some(index_data_dir) = collector.index_data_dir.get() {
            std::fs::remove_dir_all(index_data_dir.join(index_name))
                .expect("remove physical index before committed analytics cleanup");
        }
        collector
            .finish_index_deletion(index_name, &quarantine)
            .expect("finish index deletion");
    }

    #[tokio::test]
    async fn analytics_delete_removes_real_and_buffered_data_without_touching_other_indexes() {
        let temp_dir = TempDir::new().expect("temp dir");
        let config = test_config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());

        collector.record_search(search_event("delete-me", "delete-flushed"));
        collector.record_insight(InsightEvent {
            index: "delete-me".to_string(),
            ..insight_event("delete-user")
        });
        collector.record_search(search_event("keep-me", "keep-flushed"));
        collector.record_insight(InsightEvent {
            index: "keep-me".to_string(),
            ..insight_event("keep-user")
        });
        collector.flush_all();

        let deleted_rollup = config
            .rollups_dir("delete-me", "1day")
            .join("deleted.parquet");
        std::fs::create_dir_all(deleted_rollup.parent().unwrap()).unwrap();
        std::fs::write(&deleted_rollup, b"deleted rollup").unwrap();
        let retained_rollup = config
            .rollups_dir("keep-me", "1day")
            .join("retained.parquet");
        std::fs::create_dir_all(retained_rollup.parent().unwrap()).unwrap();
        std::fs::write(&retained_rollup, b"retained rollup").unwrap();
        collector.record_debug_event(test_event(1, "delete-me", "search", "deleted", 200));
        collector.record_debug_event(test_event(2, "keep-me", "search", "retained", 200));

        collector.record_search(search_event("delete-me", "delete-buffered"));
        collector.record_insight(InsightEvent {
            index: "delete-me".to_string(),
            ..insight_event("delete-buffered-user")
        });
        collector.record_search(search_event("keep-me", "keep-buffered"));

        let quarantine = config.data_dir.join(".delete-me-delete-test");
        collector
            .stage_index_deletion("delete-me", &quarantine)
            .expect("stage index deletion");
        collector.record_search(search_event("delete-me", "delete-late"));
        collector.record_insight(InsightEvent {
            index: "delete-me".to_string(),
            ..insight_event("delete-late-user")
        });
        collector.record_debug_event(test_event(3, "delete-me", "search", "late", 200));
        collector.flush_all();
        let hour_end = chrono::Utc::now().timestamp_millis().div_euclid(3_600_000) * 3_600_000;
        assert!(collector
            .flush_rollup_window_with_event_count(
                "delete-me",
                "1hour",
                hour_end - 3_600_000,
                hour_end,
            )
            .unwrap_err()
            .contains("pending deletion"));
        collector
            .finish_index_deletion("delete-me", &quarantine)
            .expect("finish index deletion");
        assert_eq!(collector.deleting_index_count(), 0);

        assert!(
            !config
                .target_artifact_paths("delete-me")
                .index_root
                .exists(),
            "deleted analytics must not be recreated by a buffered flush"
        );
        assert!(
            config.target_artifact_paths("keep-me").index_root.exists(),
            "exact-index purge must preserve the control index"
        );
        assert!(collector.lookup_query_id("delete-flushed").is_none());
        assert!(collector.lookup_query_id("delete-buffered").is_none());
        assert!(collector.lookup_query_id("delete-late").is_none());
        assert!(collector.lookup_query_id("keep-flushed").is_some());
        assert!(collector.lookup_query_id("keep-buffered").is_some());
        assert!(collector
            .get_debug_events(10, Some("delete-me"), None, None, None, None)
            .is_empty());
        assert_eq!(
            collector
                .get_debug_events(10, Some("keep-me"), None, None, None, None)
                .len(),
            1
        );
        assert!(retained_rollup.exists(), "control rollup must be preserved");

        let searches = AnalyticsQueryEngine::new(config.clone())
            .query_searches("keep-me", "SELECT COUNT(*) AS count FROM searches")
            .await
            .expect("query retained searches");
        assert_eq!(searches[0]["count"], serde_json::json!(2));
        let insights = AnalyticsQueryEngine::new(config)
            .query_events("keep-me", "SELECT COUNT(*) AS count FROM events")
            .await
            .expect("query retained insights");
        assert_eq!(insights[0]["count"], serde_json::json!(1));

        collector.activate_index("delete-me");
        collector.record_search(search_event("delete-me", "recreated"));
        collector.flush_searches();
        assert!(collector.lookup_query_id("recreated").is_some());
    }

    #[test]
    fn analytics_delete_restart_rejects_late_ingress_until_exact_recreate() {
        let temp_dir = TempDir::new().expect("temp dir");
        let config = test_config(&temp_dir);
        let index_data_dir = temp_dir.path().join("indexes");
        std::fs::create_dir_all(&index_data_dir).unwrap();
        let collector = AnalyticsCollector::new(config.clone());
        collector.bind_index_data_dir(index_data_dir.clone());

        collector.record_search(search_event("deleted", "late"));
        collector.record_insight(InsightEvent {
            index: "deleted".to_string(),
            ..insight_event("late")
        });
        collector.record_debug_event(test_event(1, "deleted", "search", "late", 200));
        collector.flush_all();
        assert!(!config.target_artifact_paths("deleted").index_root.exists());
        assert!(collector.lookup_query_id("late").is_none());
        assert!(collector
            .get_debug_events(10, Some("deleted"), None, None, None, None)
            .is_empty());

        std::fs::create_dir_all(index_data_dir.join("deleted")).unwrap();
        collector.activate_index("deleted");
        collector.record_search(search_event("deleted", "recreated"));
        collector.flush_searches();
        assert!(config.target_artifact_paths("deleted").index_root.exists());
    }

    #[test]
    fn analytics_delete_blocks_taken_search_batch_from_post_stage_write() {
        use std::sync::{mpsc, Barrier};
        use std::time::Duration;

        let temp_dir = TempDir::new().expect("temp dir");
        let config = test_config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        let index_data_dir = temp_dir.path().join("indexes");
        std::fs::create_dir_all(index_data_dir.join("delete-me")).unwrap();
        collector.bind_index_data_dir(index_data_dir);
        collector.record_search(search_event("delete-me", "inflight"));

        let flush_taken = Arc::new(Barrier::new(2));
        let release_flush = Arc::new(Barrier::new(2));
        let hook_taken = Arc::clone(&flush_taken);
        let hook_release = Arc::clone(&release_flush);
        *collector.search_flush_after_take_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_taken.wait();
            hook_release.wait();
        }));
        let flush_collector = Arc::clone(&collector);
        let flush_thread = std::thread::spawn(move || flush_collector.flush_searches());
        flush_taken.wait();

        let purge_attempted = Arc::new(Barrier::new(2));
        let hook_attempted = Arc::clone(&purge_attempted);
        *collector.index_purge_before_lock_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_attempted.wait();
        }));
        let (purge_tx, purge_rx) = mpsc::channel();
        let purge_collector = Arc::clone(&collector);
        let purge_config = config.clone();
        let purge_thread = std::thread::spawn(move || {
            purge_tx
                .send(purge_index(&purge_collector, &purge_config, "delete-me"))
                .unwrap();
        });
        purge_attempted.wait();
        purge_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("purge must commit while the pre-admitted flush is paused");

        release_flush.wait();
        flush_thread.join().unwrap();
        purge_thread.join().unwrap();
        collector.flush_all();
        assert!(!config
            .target_artifact_paths("delete-me")
            .index_root
            .exists());
    }

    #[test]
    fn analytics_delete_blocks_taken_insight_batch_from_post_stage_write() {
        use std::sync::{mpsc, Barrier};
        use std::time::Duration;

        let temp_dir = TempDir::new().expect("temp dir");
        let config = test_config(&temp_dir);
        let collector = AnalyticsCollector::new(config.clone());
        let index_data_dir = temp_dir.path().join("indexes");
        std::fs::create_dir_all(index_data_dir.join("delete-me")).unwrap();
        collector.bind_index_data_dir(index_data_dir);
        collector.record_insight(InsightEvent {
            index: "delete-me".to_string(),
            ..insight_event("inflight")
        });

        let flush_taken = Arc::new(Barrier::new(2));
        let release_flush = Arc::new(Barrier::new(2));
        let hook_taken = Arc::clone(&flush_taken);
        let hook_release = Arc::clone(&release_flush);
        *collector.insight_flush_after_take_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_taken.wait();
            hook_release.wait();
        }));
        let flush_collector = Arc::clone(&collector);
        let flush_thread = std::thread::spawn(move || flush_collector.flush_insights());
        flush_taken.wait();

        let purge_attempted = Arc::new(Barrier::new(2));
        let hook_attempted = Arc::clone(&purge_attempted);
        *collector.index_purge_before_lock_hook.lock().unwrap() = Some(Arc::new(move || {
            hook_attempted.wait();
        }));
        let (purge_tx, purge_rx) = mpsc::channel();
        let purge_collector = Arc::clone(&collector);
        let purge_config = config.clone();
        let purge_thread = std::thread::spawn(move || {
            purge_tx
                .send(purge_index(&purge_collector, &purge_config, "delete-me"))
                .unwrap();
        });
        purge_attempted.wait();
        purge_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("purge must commit while the pre-admitted flush is paused");

        release_flush.wait();
        flush_thread.join().unwrap();
        purge_thread.join().unwrap();
        collector.flush_all();
        assert!(!config
            .target_artifact_paths("delete-me")
            .index_root
            .exists());
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
