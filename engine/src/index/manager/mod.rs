use crate::error::{FlapjackError, Result};
use crate::index::oplog::{write_committed_seq, OpLog};
use crate::index::relevance::RelevanceConfig;
use crate::index::rules::{RuleEffects, RuleStore};
use crate::index::settings::IndexSettings;
use crate::index::synonyms::{Synonym, SynonymStore};
use crate::index::task_queue::TaskQueue;
use crate::index::utils::copy_dir_recursive;
use crate::index::write_queue::{
    admission::{WriteAdmissionRecord, WriteAdmissionStore, WriteAdmissionTicket},
    create_write_queue, ReplicatedWriteOrigin, VectorWriteContext, WriteAction, WriteOp,
    WriteQueue, WriteQueueContext, WriteTaskHandle,
};
use crate::index::BulkBuildWriterConfig;
use crate::index::Index;
use crate::query::algolia_filters::{
    facet_filters_to_ast, numeric_filters_to_ast, parse_optional_filters_grouped,
    tag_filters_to_ast,
};
use crate::query::{QueryExecutor, QueryParser};
use crate::text_normalization::{
    is_camel_case_attr_path, normalize_for_search, split_camel_case_words,
};
use crate::types::{
    Document, FieldValue, Filter, ScoredDocument, SearchResult, Sort, TaskInfo, TaskStatus,
    TenantId,
};
use dashmap::DashMap;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, Ordering as AtomicOrdering};
use std::sync::{Arc, OnceLock};

pub(crate) const MAX_TASKS_PER_TENANT: usize = 1000;

fn current_epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Maximum index name length in bytes.
const MAX_INDEX_NAME_BYTES: usize = 256;
/// Plain top-level paths in the shared data root that belong to server state.
///
/// Dot- and underscore-prefixed names are reserved by convention below, so this
/// list owns only the remaining exact collisions. New server components that
/// allocate a plain top-level path must add it here before shipping.
const SERVER_OWNED_DATA_ROOT_NAMES: &[&str] = &[
    "analytics",
    "autoheal_decisions.jsonl",
    "dashboard_sessions.json",
    "key_material.json",
    "keys.json",
    "migration_exports",
    "node.json",
    "security_sources.json",
    "ssl",
];

use super::OptionalFilterSpecs;
use super::SearchOptions;
use crate::index::settings::strip_unordered_prefix;

fn is_synchronous_metadata_oplog_op(op_type: &str) -> bool {
    matches!(
        op_type,
        "settings"
            | "save_synonym"
            | "save_synonyms"
            | "delete_synonym"
            | "clear_synonyms"
            | "save_rule"
            | "save_rules"
            | "delete_rule"
            | "clear_rules"
    )
}

/// Validate that a tenant/index name is safe in the server's shared data root.
/// Rejects path traversal, unsafe characters, and names owned by server storage.
pub fn validate_index_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(FlapjackError::InvalidQuery(
            "Index name must not be empty".to_string(),
        ));
    }
    if name.len() > MAX_INDEX_NAME_BYTES {
        return Err(FlapjackError::InvalidQuery(format!(
            "Index name exceeds maximum length of {} bytes",
            MAX_INDEX_NAME_BYTES
        )));
    }
    // Reject path traversal components
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err(FlapjackError::InvalidQuery(
            "Index name contains invalid characters (path traversal not allowed)".to_string(),
        ));
    }
    // Reject null bytes
    if name.contains('\0') {
        return Err(FlapjackError::InvalidQuery(
            "Index name contains null bytes".to_string(),
        ));
    }
    if publication::is_reserved_publication_namespace(Path::new(name)) {
        return Err(FlapjackError::InvalidQuery(
            "Index name is reserved publication namespace".to_string(),
        ));
    }
    // Internal state consistently uses dot/underscore prefixes. Reserving the
    // conventions, rather than today's individual names, prevents a future
    // subsystem from silently colliding with an already accepted index.
    if name == "."
        || name.starts_with('.')
        || name.starts_with('_')
        || SERVER_OWNED_DATA_ROOT_NAMES.contains(&name)
    {
        return Err(FlapjackError::InvalidQuery(
            "Index name is reserved for server storage".to_string(),
        ));
    }
    Ok(())
}

/// Multi-tenant index manager.
///
/// `IndexManager` owns a collection of [`Index`] instances (one per tenant),
/// handles lazy loading from disk, background write queues, facet caching,
/// oplog recovery, and query execution with synonyms/rules.
///
/// Create one with [`IndexManager::new`], which returns `Arc<IndexManager>`
/// (it is `Send + Sync` and designed to be shared).
///
/// # Examples
///
/// ```rust,no_run
/// use flapjack::IndexManager;
///
/// # fn main() -> flapjack::Result<()> {
/// let manager = IndexManager::new("./data");
/// manager.create_tenant("products")?;
/// let results = manager.search("products", "laptop", None, None, 10)?;
/// # Ok(())
/// # }
/// ```
pub struct IndexManager {
    pub base_path: PathBuf,
    node_id: String,
    pub(crate) loaded: DashMap<TenantId, Arc<Index>>,
    tenant_load_locks: DashMap<TenantId, Arc<std::sync::Mutex<()>>>,
    admission_stores: DashMap<TenantId, Arc<WriteAdmissionStore>>,
    pub(crate) write_queues: DashMap<TenantId, WriteQueue>,
    pub(crate) write_task_handles: DashMap<TenantId, WriteTaskHandle>,
    pub(crate) oplogs: DashMap<TenantId, Arc<OpLog>>,
    tasks: Arc<DashMap<String, TaskInfo>>,
    task_retention: Arc<TaskRetention>,
    next_task_numeric_id: AtomicI64,
    task_queue: TaskQueue,
    settings_cache: DashMap<TenantId, Arc<IndexSettings>>,
    rules_cache: DashMap<TenantId, Arc<RuleStore>>,
    synonyms_cache: DashMap<TenantId, Arc<SynonymStore>>,
    pub facet_cache: super::FacetCacheMap,
    pub facet_cache_cap: std::sync::atomic::AtomicUsize,
    /// Vector indices per tenant. Uses std::sync::RwLock (not tokio) because
    /// vector search is called from spawn_blocking. Read lock for search,
    /// write lock for add/remove (stage 7). Wrapped in Arc for sharing with
    /// the write queue (commit_batch needs access for auto-embedding).
    #[cfg(feature = "vector-search")]
    vector_indices:
        Arc<DashMap<TenantId, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>>,
    /// Optional dictionary manager for custom stopwords/plurals/compounds in the query pipeline.
    dictionary_manager: OnceLock<Arc<crate::dictionaries::manager::DictionaryManager>>,
    analytics_config: OnceLock<crate::analytics::AnalyticsConfig>,
    bulk_build_writer_config: BulkBuildWriterConfig,
}

const DEFAULT_FACET_CACHE_CAP: usize = 500;

mod config;
mod lifecycle;
pub use lifecycle::TenantQuiesce;
#[cfg(test)]
mod lifecycle_move_tests;
pub mod publication;
mod publication_startup;
#[cfg(test)]
mod publication_startup_tests;
mod query;
mod ranking;
mod recovery;
mod search;
mod search_phases;
pub(crate) mod task_retention;
pub(crate) mod tokenization;
#[cfg(feature = "vector-search")]
mod vector;
mod write;

// Re-export sub-module items for use within the manager module
use query::*;
use ranking::*;
use task_retention::TaskRetention;
use tokenization::*;

impl IndexManager {
    fn is_task_terminal(status: &TaskStatus) -> bool {
        !matches!(status, TaskStatus::Enqueued | TaskStatus::Processing)
    }

    /// Create a new IndexManager with the given base directory.
    ///
    /// Each tenant's index will be stored in `{base_path}/{tenant_id}/`.
    pub fn new<P: AsRef<Path>>(base_path: P) -> Arc<Self> {
        Self::new_with_node_id(base_path, crate::index::configured_node_id())
    }

    /// Create a new IndexManager with an explicit node identifier for local oplog entries.
    pub fn new_with_node_id<P: AsRef<Path>, S: Into<String>>(
        base_path: P,
        node_id: S,
    ) -> Arc<Self> {
        Self::new_with_node_id_and_bulk_build_config(
            base_path,
            node_id,
            BulkBuildWriterConfig::default(),
        )
    }

    fn new_with_node_id_and_bulk_build_config<P: AsRef<Path>, S: Into<String>>(
        base_path: P,
        node_id: S,
        bulk_build_writer_config: BulkBuildWriterConfig,
    ) -> Arc<Self> {
        Arc::new_cyclic(|weak| {
            let tasks = Arc::new(DashMap::new());
            let task_retention = Arc::new(TaskRetention::new());
            IndexManager {
                base_path: base_path.as_ref().to_path_buf(),
                node_id: node_id.into(),
                loaded: DashMap::new(),
                tenant_load_locks: DashMap::new(),
                admission_stores: DashMap::new(),
                write_queues: DashMap::new(),
                write_task_handles: DashMap::new(),
                oplogs: DashMap::new(),
                tasks: tasks.clone(),
                task_retention: Arc::clone(&task_retention),
                next_task_numeric_id: AtomicI64::new(current_epoch_millis()),
                task_queue: TaskQueue::new(weak.clone(), tasks, task_retention),
                settings_cache: DashMap::new(),
                rules_cache: DashMap::new(),
                synonyms_cache: DashMap::new(),
                facet_cache: Arc::new(DashMap::new()),
                facet_cache_cap: std::sync::atomic::AtomicUsize::new(DEFAULT_FACET_CACHE_CAP),
                #[cfg(feature = "vector-search")]
                vector_indices: Arc::new(DashMap::new()),
                dictionary_manager: OnceLock::new(),
                analytics_config: OnceLock::new(),
                bulk_build_writer_config,
            }
        })
    }

    /// Create an IndexManager for bulk-build staging with bulk-only writer sizing.
    pub fn new_for_bulk_build<P: AsRef<Path>>(
        base_path: P,
        config: BulkBuildWriterConfig,
    ) -> Arc<Self> {
        Self::new_with_node_id_and_bulk_build_config(
            base_path,
            crate::index::configured_node_id(),
            config,
        )
    }

    pub(crate) fn write_queue_writer_buffer_size(&self) -> usize {
        self.bulk_build_writer_config.writer_buffer_size
    }

    /// Set the dictionary manager for custom stopwords/plurals/compounds support.
    /// Must be called after construction; can only be set once.
    pub fn set_dictionary_manager(&self, dm: Arc<crate::dictionaries::manager::DictionaryManager>) {
        let _ = self.dictionary_manager.set(dm);
    }

    /// Get the dictionary manager, if one has been set.
    pub fn dictionary_manager(
        &self,
    ) -> Option<&Arc<crate::dictionaries::manager::DictionaryManager>> {
        self.dictionary_manager.get()
    }

    /// Configure the canonical analytics artifact root used by publication operations.
    pub fn set_analytics_config(&self, config: crate::analytics::AnalyticsConfig) {
        let _ = self.analytics_config.set(config);
    }

    pub(super) fn publication_analytics_config(&self) -> crate::analytics::AnalyticsConfig {
        self.analytics_config
            .get()
            .cloned()
            .unwrap_or_else(|| crate::analytics::AnalyticsConfig::for_data_dir(&self.base_path))
    }

    /// Get the oplog for a tenant (for external access)
    pub fn get_oplog(&self, tenant_id: &str) -> Option<Arc<OpLog>> {
        self.oplogs.get(tenant_id).map(|r| Arc::clone(&r))
    }

    /// Read the committed conflict version for one tenant object.
    pub fn get_object_version(
        &self,
        tenant_id: &str,
        object_id: &str,
    ) -> Result<Option<crate::index::version_store::VersionRecord>> {
        validate_index_name(tenant_id)?;
        crate::index::version_store::VersionStore::open(&self.base_path.join(tenant_id))?
            .get(object_id)
            .map_err(Into::into)
    }

    /// Arm one tenant's next write-queue commit to fail.
    ///
    /// This control exists only in explicit fault-injection builds.
    #[cfg(feature = "fault-injection")]
    pub fn fail_next_commit_for_test(&self, tenant_id: &str) -> impl Drop {
        crate::index::write_queue::fail_next_commit_for_test(tenant_id)
    }

    /// Arm one tenant's next write-queue finalization boundary to fail.
    ///
    /// This control exists only in explicit fault-injection builds.
    #[cfg(feature = "fault-injection")]
    pub fn fail_next_finalization_for_test(
        &self,
        tenant_id: &str,
        fault_point: crate::index::write_queue::FinalizationFaultPoint,
    ) -> impl Drop {
        crate::index::write_queue::fail_next_finalization_for_test(tenant_id, fault_point)
    }

    pub fn get_task(&self, task_id: &str) -> Result<TaskInfo> {
        self.tasks
            .get(task_id)
            .map(|task| task.clone())
            .ok_or_else(|| FlapjackError::TaskNotFound(task_id.to_string()))
    }

    /// Allocate a process-unique numeric task ID seeded from current epoch millis.
    /// The atomic allocator prevents simultaneous requests from reserving the
    /// same alias before either one publishes its task record.
    fn next_numeric_task_id(&self) -> i64 {
        loop {
            let numeric_id = self
                .next_task_numeric_id
                .fetch_add(1, AtomicOrdering::Relaxed);
            if !self.tasks.contains_key(&numeric_id.to_string()) {
                return numeric_id;
            }
        }
    }

    /// Count tasks in Enqueued or Processing state for a given tenant.
    pub fn pending_task_count(&self, tenant_id: &str) -> usize {
        let prefix = format!("task_{}_", tenant_id);
        self.tasks
            .iter()
            .filter(|entry| {
                entry.key().starts_with(&prefix) && !Self::is_task_terminal(&entry.value().status)
            })
            .count()
    }

    /// Wait until all pending write tasks have completed, up to `timeout`.
    /// Returns `true` if all tasks finished, `false` if the timeout was reached.
    pub async fn wait_for_pending_tasks(&self, timeout: std::time::Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let has_pending = self
                .tasks
                .iter()
                .any(|entry| !Self::is_task_terminal(&entry.value().status));
            if !has_pending {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }

    /// Remove the oldest tasks for a tenant when the count exceeds `max_tasks`. Tasks
    /// are sorted by creation time; both the string task ID and numeric ID alias are removed.
    pub fn evict_old_tasks(&self, tenant_id: &str, max_tasks: usize) {
        self.task_retention.trim(&self.tasks, tenant_id, max_tasks);
    }

    /// Return a tenant's `Arc<Index>`, loading from disk if not cached. Acquires a
    /// per-tenant mutex to serialize recovery (oplog replay, vector index load) so
    /// concurrent requests do not double-replay.
    pub fn get_or_load(&self, tenant_id: &str) -> Result<Arc<Index>> {
        validate_index_name(tenant_id)?;
        if let Some(index) = self.loaded.get(tenant_id) {
            return Ok(Arc::clone(&index));
        }

        // A missing tenant has no recovery state to serialize. Reject it before
        // allocating a per-name mutex so arbitrary lookup names cannot become
        // permanent registry entries. A concurrent create may make this one
        // request observe NotFound; its retry will take the normal locked path.
        let path = self.base_path.join(tenant_id);
        if !path.exists() {
            return Err(FlapjackError::TenantNotFound(tenant_id.to_string()));
        }

        // Recovery mutates on-disk state and acquires a writer; only one thread
        // may initialize a tenant at a time or concurrent startup/search requests
        // can replay the same oplog twice and trip Tantivy's writer lock.
        let load_lock = self
            .tenant_load_locks
            .entry(tenant_id.to_string())
            .or_insert_with(|| Arc::new(std::sync::Mutex::new(())))
            .clone();
        let _guard = load_lock.lock().map_err(|_| {
            FlapjackError::Tantivy(format!("tenant load lock poisoned for '{}'", tenant_id))
        })?;

        if let Some(index) = self.loaded.get(tenant_id) {
            return Ok(Arc::clone(&index));
        }

        let index_languages = Self::read_index_languages(&path);
        let indexed_separators = Self::read_indexed_separators(&path);
        let keep_diacritics_on_characters = Self::read_keep_diacritics_on_characters(&path);
        let custom_normalization = Self::read_custom_normalization(&path);
        let index = match Index::open_with_languages_indexed_separators_and_keep_diacritics(
            &path,
            crate::index::get_global_budget(),
            &index_languages,
            &indexed_separators,
            &keep_diacritics_on_characters,
            &custom_normalization,
        ) {
            Ok(idx) => Arc::new(idx),
            Err(e) => {
                let oplog_dir = path.join("oplog");
                if oplog_dir.exists() {
                    tracing::warn!("[RECOVERY {}] Index::open failed ({}), but oplog exists — creating fresh index for replay", tenant_id, e);
                    let cs_path = path.join("committed_seq");
                    if cs_path.exists() {
                        tracing::info!(
                            "[RECOVERY {}] Resetting committed_seq to 0 for full replay",
                            tenant_id
                        );
                        let _ = write_committed_seq(&path, 0);
                    }
                    let schema = crate::index::schema::Schema::builder().build();
                    Arc::new(
                        Index::create_with_languages_indexed_separators_and_keep_diacritics(
                            &path,
                            schema,
                            crate::index::get_global_budget(),
                            &index_languages,
                            &indexed_separators,
                            &keep_diacritics_on_characters,
                            &custom_normalization,
                        )?,
                    )
                } else {
                    return Err(e);
                }
            }
        };
        self.recover_from_oplog(tenant_id, &index, &path)?;
        self.publish_loaded_runtime_state_if_unfenced(tenant_id, index)
    }

    /// Get the number of loaded indexes.
    ///
    /// Useful for monitoring and debugging.
    pub fn loaded_count(&self) -> usize {
        self.loaded.len()
    }

    /// Return the total disk usage in bytes for a single tenant's data directory.
    ///
    /// Returns 0 if the tenant directory does not exist.
    pub fn tenant_storage_bytes(&self, tenant_id: &str) -> u64 {
        if validate_index_name(tenant_id).is_err() {
            return 0;
        }
        let path = self.base_path.join(tenant_id);
        crate::index::storage_size::dir_size_bytes(&path).unwrap_or(0)
    }

    /// Remove writer-local control artifacts that must not enter publication manifests.
    pub fn scrub_transient_runtime_artifacts(&self, tenant_id: &str) -> Result<()> {
        validate_index_name(tenant_id)?;
        let pause_artifact = crate::index::write_queue::backpressure::pause_artifact_path(
            &self.base_path,
            tenant_id,
        );
        match std::fs::remove_file(&pause_artifact) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    /// Return the document count for a loaded tenant's index.
    ///
    /// Reads Tantivy segment metadata (in-memory, fast). Returns `None` if
    /// the tenant is not currently loaded.
    pub fn tenant_doc_count(&self, tenant_id: &str) -> Option<u64> {
        let index = self.loaded.get(tenant_id)?;
        let reader = index.reader();
        let searcher = reader.searcher();
        let count: u64 = searcher
            .segment_readers()
            .iter()
            .map(|r| r.num_docs() as u64)
            .sum();
        Some(count)
    }

    /// Load durable index metadata for a tenant without requiring the full index to be loaded.
    /// Returns `None` if metadata file doesn't exist (pre-metadata indexes).
    pub fn tenant_metadata(
        &self,
        tenant_id: &str,
    ) -> Option<crate::index::index_metadata::IndexMetadata> {
        validate_index_name(tenant_id).ok()?;
        let path = self.base_path.join(tenant_id);
        crate::index::index_metadata::IndexMetadata::load(&path)
            .ok()
            .flatten()
    }

    /// Return the IDs of all currently loaded tenants.
    ///
    /// Needed by the metrics handler since `loaded` is `pub(crate)`.
    pub fn loaded_tenant_ids(&self) -> Vec<String> {
        self.loaded
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Return (tenant_id, current_oplog_seq) pairs for all tenants with a loaded oplog.
    ///
    /// Uses `get_oplog()` (not `get_or_create_oplog()`) to avoid side effects.
    pub fn all_tenant_oplog_seqs(&self) -> Vec<(String, u64)> {
        self.loaded
            .iter()
            .filter_map(|entry| {
                let tid = entry.key().clone();
                self.get_oplog(&tid).map(|oplog| (tid, oplog.current_seq()))
            })
            .collect()
    }

    /// Return disk usage in bytes for every loaded tenant.
    pub fn all_tenant_storage(&self) -> Vec<(String, u64)> {
        self.loaded
            .iter()
            .map(|entry| {
                let tid = entry.key().clone();
                let bytes = self.tenant_storage_bytes(&tid);
                (tid, bytes)
            })
            .collect()
    }

    pub fn make_noop_task(&self, index_name: &str) -> Result<TaskInfo> {
        // Synchronous metadata operations still publish ordinary task IDs, so
        // they must pass through the same retention owner as queued writes.
        let numeric_id = self.next_numeric_task_id();
        let task_id = format!("task_{}_{}", index_name, uuid::Uuid::new_v4());
        let mut task = TaskInfo::new(task_id.clone(), numeric_id, 0);
        task.status = TaskStatus::Succeeded;
        self.task_retention
            .insert(&self.tasks, index_name, task.clone(), MAX_TASKS_PER_TENANT);
        Ok(task)
    }

    /// Return the tenant's `OpLog`, creating and caching it on first access. Opens the
    /// oplog directory under the tenant path with the configured node ID.
    pub fn get_or_create_oplog(&self, tenant_id: &str) -> Option<Arc<OpLog>> {
        match self.get_or_create_oplog_result(tenant_id) {
            Ok(oplog) => Some(oplog),
            Err(error) => {
                tracing::error!("[OPLOG {}] open failed: {}", tenant_id, error);
                None
            }
        }
    }

    pub(crate) fn get_or_create_oplog_result(&self, tenant_id: &str) -> Result<Arc<OpLog>> {
        if let Err(error) = validate_index_name(tenant_id) {
            tracing::warn!("[OPLOG {}] invalid tenant id: {}", tenant_id, error);
            return Err(error);
        }
        let entry = self
            .oplogs
            .entry(tenant_id.to_string())
            .or_try_insert_with(|| {
                let oplog_dir = self.base_path.join(tenant_id).join("oplog");
                OpLog::open(&oplog_dir, tenant_id, &self.node_id)
                    .map(Arc::new)
                    .map_err(|e| {
                        tracing::error!("[OPLOG {}] open failed: {}", tenant_id, e);
                        e
                    })
            });
        match entry {
            Ok(e) => Ok(Arc::clone(&e)),
            Err(error) => Err(error.clone()),
        }
    }

    pub fn append_oplog(&self, tenant_id: &str, op_type: &str, payload: serde_json::Value) {
        if let Some(ol) = self.get_or_create_oplog(tenant_id) {
            match ol.append(op_type, payload) {
                Ok(seq) if is_synchronous_metadata_oplog_op(op_type) => {
                    if let Err(error) = write_committed_seq(&self.base_path.join(tenant_id), seq) {
                        tracing::error!(
                            "[OPLOG {}] committed_seq advance failed after {} append: {}",
                            tenant_id,
                            op_type,
                            error
                        );
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!("[OPLOG {}] append failed: {}", tenant_id, e);
                }
            }
        }
    }

    /// Retrieve a single document by object ID via a Tantivy term query. Returns
    /// `None` if no document matches.
    pub fn get_document(&self, tenant_id: &str, object_id: &str) -> Result<Option<Document>> {
        let index = self.get_or_load(tenant_id)?;
        let reader = index.reader();
        let searcher = reader.searcher();
        let schema = index.inner().schema();

        let id_field = schema
            .get_field("_id")
            .map_err(|_| FlapjackError::FieldNotFound("_id".to_string()))?;

        let term = tantivy::Term::from_field_text(id_field, object_id);
        let term_query =
            tantivy::query::TermQuery::new(term, tantivy::schema::IndexRecordOption::Basic);

        let top_docs = searcher.search(
            &term_query,
            &tantivy::collector::TopDocs::with_limit(1).order_by_score(),
        )?;

        if top_docs.is_empty() {
            return Ok(None);
        }

        let doc_address = top_docs[0].1;
        let retrieved_doc = searcher.doc(doc_address)?;

        let document =
            index
                .converter()
                .from_tantivy(retrieved_doc, &schema, object_id.to_string())?;
        Ok(Some(document))
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

impl Drop for IndexManager {
    /// Abort all background write tasks when the manager is dropped.
    ///
    /// Without this, dropping a JoinHandle in tokio detaches the task (does not
    /// cancel it). Detached tasks continue running in the tokio runtime even after
    /// the IndexManager is gone, holding file handles briefly. Under parallel
    /// test loads this causes races with other tests that access the same runtime.
    ///
    /// In production the server always calls `graceful_shutdown()` before dropping,
    /// which drains writes cleanly. This abort-on-drop is a safety net for tests
    /// and unexpected drops.
    fn drop(&mut self) {
        let handles: Vec<_> = self
            .write_task_handles
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        for (tenant_id, _) in &handles {
            drop(self.write_queues.remove(tenant_id));
        }
        for (_, handle) in &handles {
            handle.abort();
        }
        for (tenant_id, handle) in handles {
            handle.wait_for_shutdown_after_cancellation(tenant_id);
        }
    }
}
