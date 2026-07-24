use super::*;
use crate::index::oplog::{read_committed_seq, write_committed_seq, OpLogEntry};

#[derive(Default)]
struct ReplayDocumentStats {
    replayed: usize,
    failed: usize,
}

impl ReplayDocumentStats {
    fn record(&mut self, outcome: ReplayDocumentOutcome) {
        self.replayed += outcome.replayed;
        self.failed += outcome.failed;
    }
}

#[derive(Clone, Copy)]
struct ReplayDocumentOutcome {
    replayed: usize,
    failed: usize,
}

impl ReplayDocumentOutcome {
    const SKIPPED: Self = Self {
        replayed: 0,
        failed: 0,
    };
    const REPLAYED: Self = Self {
        replayed: 1,
        failed: 0,
    };
    const FAILED: Self = Self {
        replayed: 0,
        failed: 1,
    };
}

#[derive(Clone, Copy)]
pub(super) struct RecoverySeqWindow {
    pub(super) committed_seq: u64,
    pub(super) final_seq: u64,
}

struct ReplayCommitContext<'a> {
    tenant_id: &'a str,
    index: &'a Arc<Index>,
    tenant_path: &'a Path,
    seq_window: RecoverySeqWindow,
}

impl IndexManager {
    /// Replay uncommitted oplog entries for a tenant after startup. Rebuilds the LWW map
    /// from all retained entries, replays config ops (settings), then replays document ops
    /// (upsert/delete/clear) with a fresh Tantivy writer. Rebuilds the vector index when
    /// the `vector-search` feature is enabled.
    pub(super) fn recover_from_oplog(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
        tenant_path: &Path,
    ) -> Result<()> {
        let oplog_dir = tenant_path.join("oplog");
        if !oplog_dir.exists() {
            return Ok(());
        }
        let committed_seq = read_committed_seq(tenant_path);

        let node_id = crate::index::configured_node_id();
        let oplog = OpLog::open(&oplog_dir, tenant_id, &node_id)?;

        self.rebuild_lww_map(tenant_id, &oplog)?;

        let ops = oplog.read_since(committed_seq)?;
        if ops.is_empty() {
            return Ok(());
        }

        tracing::info!(
            "[RECOVERY {}] replaying {} ops from seq {} (committed_seq={})",
            tenant_id,
            ops.len(),
            ops[0].seq,
            committed_seq
        );

        self.replay_config_ops(tenant_id, tenant_path, &ops)?;
        let settings = self.load_settings_after_config(tenant_id, tenant_path)?;
        let document_ops: Vec<OpLogEntry> = ops
            .iter()
            .filter(|entry| Self::is_document_recovery_op(entry.op_type.as_str()))
            .cloned()
            .collect();
        let seq_window = RecoverySeqWindow {
            committed_seq,
            final_seq: ops.last().map(|op| op.seq).unwrap_or(committed_seq),
        };
        if document_ops.is_empty() {
            self.finish_config_only_recovery(tenant_id, tenant_path, seq_window)?;
            return Ok(());
        }
        self.replay_document_ops(
            tenant_id,
            index,
            tenant_path,
            &document_ops,
            seq_window,
            settings.as_ref(),
        )?;

        #[cfg(feature = "vector-search")]
        self.rebuild_vector_index(tenant_id, tenant_path, &ops);

        Ok(())
    }

    pub(super) fn is_document_recovery_op(op_type: &str) -> bool {
        matches!(op_type, "upsert" | "delete" | "clear")
    }

    /// Advance the committed sequence number when only config ops were replayed (no
    /// document ops). No-ops if the final sequence has not advanced past the committed mark.
    fn finish_config_only_recovery(
        &self,
        tenant_id: &str,
        tenant_path: &Path,
        seq_window: RecoverySeqWindow,
    ) -> Result<()> {
        if seq_window.final_seq <= seq_window.committed_seq {
            return Ok(());
        }

        write_committed_seq(tenant_path, seq_window.final_seq)?;
        tracing::info!(
            "[RECOVERY {}] applied config-only ops, new committed_seq={}",
            tenant_id,
            seq_window.final_seq
        );
        Ok(())
    }

    /// Rebuild the LWW (last-writer-wins) map from all retained oplog entries, tracking
    /// the highest `(timestamp_ms, node_id)` pair per object ID. Runs on every startup
    /// so stale replicated ops arriving after restart are correctly rejected.
    pub(super) fn rebuild_lww_map(&self, tenant_id: &str, oplog: &OpLog) -> Result<()> {
        // P3: Rebuild lww_map from ALL retained oplog entries (read from seq=0).
        // This runs on every startup — crash or normal — so that stale replicated ops
        // arriving after any restart are correctly rejected by the LWW check in
        // apply_ops_to_manager. We track the highest (timestamp_ms, node_id) per
        // object so out-of-order oplog entries (clock skew / replication) are handled.
        let all_ops = oplog.read_since(0)?;
        for entry in &all_ops {
            let obj_id = match entry.op_type.as_str() {
                "upsert" | "delete" => entry.payload.get("objectID").and_then(|v| v.as_str()),
                _ => None,
            };
            if let Some(obj_id) = obj_id {
                let incoming = (entry.timestamp_ms, entry.node_id.clone());
                if self
                    .get_lww(tenant_id, obj_id)
                    .is_none_or(|existing| incoming > existing)
                {
                    self.record_lww(tenant_id, obj_id, entry.timestamp_ms, entry.node_id.clone());
                }
            }
        }
        if !all_ops.is_empty() {
            tracing::info!(
                "[RECOVERY {}] rebuilt lww_map from {} oplog entries",
                tenant_id,
                all_ops.len()
            );
        }
        Ok(())
    }

    /// Replay configuration operations (settings, synonyms, rules) from oplog entries.
    /// Restores `settings.json` from the serialized payload; synonym and rule ops are
    /// currently skipped pending aggregation support.
    pub(super) fn replay_config_ops(
        &self,
        tenant_id: &str,
        tenant_path: &Path,
        ops: &[OpLogEntry],
    ) -> Result<()> {
        for entry in ops {
            match entry.op_type.as_str() {
                "settings" => {
                    let settings_path = tenant_path.join("settings.json");
                    let settings_json =
                        serde_json::to_string_pretty(&entry.payload).map_err(|error| {
                            FlapjackError::Tantivy(format!(
                                "[RECOVERY {}] failed to serialize settings payload: {}",
                                tenant_id, error
                            ))
                        })?;
                    std::fs::write(&settings_path, settings_json).map_err(|error| {
                        FlapjackError::Tantivy(format!(
                            "[RECOVERY {}] failed to write restored settings.json: {}",
                            tenant_id, error
                        ))
                    })?;
                    tracing::info!("[RECOVERY {}] restored settings.json from oplog", tenant_id);
                }
                op if op.starts_with("save_synonym") || op == "clear_synonyms" => {
                    // Synonyms handled by dedicated endpoints, reconstruct from current state
                    // For now, skip - proper implementation needs synonym aggregation
                }
                op if op.starts_with("save_rule") || op == "clear_rules" => {
                    // Rules handled by dedicated endpoints, reconstruct from current state
                    // For now, skip - proper implementation needs rules aggregation
                }
                _ => {}
            }
        }
        Ok(())
    }

    /// Load `IndexSettings` from the tenant's `settings.json` after config replay.
    /// Returns `None` with a warning if the file is missing.
    pub(super) fn load_settings_after_config(
        &self,
        tenant_id: &str,
        tenant_path: &Path,
    ) -> Result<Option<IndexSettings>> {
        let settings_path = tenant_path.join("settings.json");
        if settings_path.exists() {
            Ok(Some(IndexSettings::load(&settings_path)?))
        } else {
            tracing::warn!(
                "[RECOVERY {}] no settings.json after config phase - using defaults",
                tenant_id
            );
            Ok(None)
        }
    }

    /// Replay document operations (upsert, delete, clear) through a fresh Tantivy writer.
    /// Acquires a writer, replays all entries, commits, reloads the reader, and advances
    /// the committed sequence number.
    pub(super) fn replay_document_ops(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
        tenant_path: &Path,
        ops: &[OpLogEntry],
        seq_window: RecoverySeqWindow,
        settings: Option<&IndexSettings>,
    ) -> Result<()> {
        let mut writer = index.writer()?;
        let schema = index.inner().schema();
        let id_field = schema.get_field("_id").unwrap();
        let stats =
            self.replay_document_entries(tenant_id, index, ops, settings, &mut writer, id_field)?;
        self.finish_replay_document_ops(
            ReplayCommitContext {
                tenant_id,
                index,
                tenant_path,
                seq_window,
            },
            &mut writer,
            stats,
        )
    }

    /// Iterate over document oplog entries and dispatch each to `replay_document_entry`,
    /// accumulating replay and failure counts.
    fn replay_document_entries(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
        ops: &[OpLogEntry],
        settings: Option<&IndexSettings>,
        writer: &mut crate::index::ManagedIndexWriter,
        id_field: tantivy::schema::Field,
    ) -> Result<ReplayDocumentStats> {
        let mut stats = ReplayDocumentStats::default();
        for entry in ops {
            let outcome =
                self.replay_document_entry(tenant_id, index, entry, settings, writer, id_field)?;
            stats.record(outcome);
        }
        Ok(stats)
    }

    /// Dispatch a single oplog entry by op type: upsert converts JSON to a Tantivy
    /// document, delete removes by object ID term, clear deletes all documents.
    /// Unknown op types are skipped with a warning.
    fn replay_document_entry(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
        entry: &OpLogEntry,
        settings: Option<&IndexSettings>,
        writer: &mut crate::index::ManagedIndexWriter,
        id_field: tantivy::schema::Field,
    ) -> Result<ReplayDocumentOutcome> {
        match entry.op_type.as_str() {
            "upsert" => {
                self.replay_upsert_entry(tenant_id, index, entry, settings, writer, id_field)
            }
            "delete" => Ok(Self::replay_delete_entry(entry, writer, id_field)),
            "settings" | "synonyms" | "rules" => Ok(ReplayDocumentOutcome::REPLAYED),
            "clear" => {
                writer.delete_all_documents()?;
                Ok(ReplayDocumentOutcome::REPLAYED)
            }
            _ => {
                tracing::warn!(
                    "[RECOVERY {}] unknown op_type '{}' at seq {}, skipping",
                    tenant_id,
                    entry.op_type,
                    entry.seq
                );
                Ok(ReplayDocumentOutcome::SKIPPED)
            }
        }
    }

    /// Replay a single upsert: delete the existing term for the object ID, parse the
    /// JSON body into a `Document`, convert to a Tantivy document, and add to the writer.
    fn replay_upsert_entry(
        &self,
        tenant_id: &str,
        index: &Arc<Index>,
        entry: &OpLogEntry,
        settings: Option<&IndexSettings>,
        writer: &mut crate::index::ManagedIndexWriter,
        id_field: tantivy::schema::Field,
    ) -> Result<ReplayDocumentOutcome> {
        let Some(obj_id) = entry
            .payload
            .get("objectID")
            .and_then(|value| value.as_str())
        else {
            return Ok(ReplayDocumentOutcome::SKIPPED);
        };

        writer.delete_term(tantivy::Term::from_field_text(id_field, obj_id));

        let Some(body) = entry.payload.get("body") else {
            return Ok(ReplayDocumentOutcome::SKIPPED);
        };

        let doc = match crate::types::Document::from_json(body) {
            Ok(doc) => doc,
            Err(error) => {
                tracing::warn!(
                    "[RECOVERY {}] failed to parse doc {}: {}",
                    tenant_id,
                    obj_id,
                    error
                );
                return Ok(ReplayDocumentOutcome::FAILED);
            }
        };

        match index.converter().to_tantivy(&doc, settings) {
            Ok(tantivy_doc) => {
                writer.add_document(tantivy_doc)?;
                Ok(ReplayDocumentOutcome::REPLAYED)
            }
            Err(error) => {
                tracing::warn!(
                    "[RECOVERY {}] failed to_tantivy for {}: {}",
                    tenant_id,
                    obj_id,
                    error
                );
                Ok(ReplayDocumentOutcome::FAILED)
            }
        }
    }

    /// Replay a single delete: remove the document matching the object ID from the
    /// Tantivy writer via term deletion.
    fn replay_delete_entry(
        entry: &OpLogEntry,
        writer: &mut crate::index::ManagedIndexWriter,
        id_field: tantivy::schema::Field,
    ) -> ReplayDocumentOutcome {
        let Some(obj_id) = entry
            .payload
            .get("objectID")
            .and_then(|value| value.as_str())
        else {
            return ReplayDocumentOutcome::SKIPPED;
        };

        writer.delete_term(tantivy::Term::from_field_text(id_field, obj_id));
        ReplayDocumentOutcome::REPLAYED
    }

    /// Commit the Tantivy writer after document replay, reload the reader, invalidate
    /// the searchable-paths cache, and advance the committed sequence number on disk.
    /// Logs a warning if any entries failed conversion.
    fn finish_replay_document_ops(
        &self,
        commit_context: ReplayCommitContext<'_>,
        writer: &mut crate::index::ManagedIndexWriter,
        stats: ReplayDocumentStats,
    ) -> Result<()> {
        if stats.replayed == 0 {
            return Ok(());
        }

        writer.commit()?;
        commit_context.index.reader().reload()?;
        commit_context.index.invalidate_searchable_paths_cache();

        if let Err(error) = write_committed_seq(
            commit_context.tenant_path,
            commit_context.seq_window.final_seq,
        ) {
            tracing::warn!(
                "[RECOVERY {}] failed to write committed_seq: {}",
                commit_context.tenant_id,
                error
            );
        }

        if stats.failed > 0 {
            tracing::warn!(
                "[RECOVERY {}] replayed {}/{} ops successfully ({} failed), new committed_seq={}",
                commit_context.tenant_id,
                stats.replayed,
                commit_context
                    .seq_window
                    .final_seq
                    .saturating_sub(commit_context.seq_window.committed_seq)
                    as usize,
                stats.failed,
                commit_context.seq_window.final_seq
            );
        } else {
            tracing::info!(
                "[RECOVERY {}] replayed {} ops, new committed_seq={}",
                commit_context.tenant_id,
                stats.replayed,
                commit_context.seq_window.final_seq
            );
        }

        Ok(())
    }

    /// Rebuild the in-memory VectorIndex by replaying all oplog entries (upsert, delete,
    /// clear). Persists the rebuilt index to disk only if any vectors were modified.
    #[cfg(feature = "vector-search")]
    pub(super) fn rebuild_vector_index(
        &self,
        tenant_id: &str,
        tenant_path: &Path,
        ops: &[OpLogEntry],
    ) {
        let mut vector_index: Option<crate::vector::index::VectorIndex> = None;
        let mut vectors_modified = false;

        for entry in ops {
            vectors_modified |=
                Self::apply_vector_recovery_entry(tenant_id, entry, &mut vector_index);
        }

        if vectors_modified {
            self.persist_rebuilt_vector_index(tenant_id, tenant_path, vector_index);
        }
    }

    #[cfg(feature = "vector-search")]
    fn apply_vector_recovery_entry(
        tenant_id: &str,
        entry: &OpLogEntry,
        vector_index: &mut Option<crate::vector::index::VectorIndex>,
    ) -> bool {
        match entry.op_type.as_str() {
            "upsert" => Self::recover_vectors_from_upsert(tenant_id, entry, vector_index),
            "delete" => Self::recover_vector_delete(entry, vector_index),
            "clear" => Self::recover_vector_clear(vector_index),
            _ => false,
        }
    }

    /// Extract `_vectors` from an upsert oplog entry's body and add each named vector
    /// to the VectorIndex, creating the index on first use with cosine similarity.
    #[cfg(feature = "vector-search")]
    fn recover_vectors_from_upsert(
        tenant_id: &str,
        entry: &OpLogEntry,
        vector_index: &mut Option<crate::vector::index::VectorIndex>,
    ) -> bool {
        let Some(object_id) = Self::recovery_object_id(entry) else {
            return false;
        };

        let mut vectors_modified = false;
        for vector in Self::recovered_vectors(entry) {
            let vector_store = vector_index.get_or_insert_with(|| {
                crate::vector::index::VectorIndex::new(vector.len(), usearch::ffi::MetricKind::Cos)
                    .expect("failed to create VectorIndex during recovery")
            });
            match vector_store.add(object_id, &vector) {
                Ok(()) => vectors_modified = true,
                Err(error) => tracing::warn!(
                    "[RECOVERY {}] failed to add vector for '{}': {}",
                    tenant_id,
                    object_id,
                    error
                ),
            }
        }
        vectors_modified
    }

    #[cfg(feature = "vector-search")]
    fn recover_vector_delete(
        entry: &OpLogEntry,
        vector_index: &mut Option<crate::vector::index::VectorIndex>,
    ) -> bool {
        let Some(vector_store) = vector_index.as_mut() else {
            return false;
        };
        let Some(object_id) = Self::recovery_object_id(entry) else {
            return false;
        };
        vector_store.remove(object_id).is_ok()
    }

    #[cfg(feature = "vector-search")]
    fn recover_vector_clear(vector_index: &mut Option<crate::vector::index::VectorIndex>) -> bool {
        let Some(vector_store) = vector_index.as_ref() else {
            return false;
        };
        *vector_index = Some(
            crate::vector::index::VectorIndex::new(
                vector_store.dimensions(),
                usearch::ffi::MetricKind::Cos,
            )
            .expect("failed to create VectorIndex during recovery clear"),
        );
        true
    }

    #[cfg(feature = "vector-search")]
    fn recovery_object_id(entry: &OpLogEntry) -> Option<&str> {
        entry
            .payload
            .get("objectID")
            .and_then(|value| value.as_str())
    }

    #[cfg(feature = "vector-search")]
    fn recovered_vectors(entry: &OpLogEntry) -> Vec<Vec<f32>> {
        entry
            .payload
            .get("body")
            .and_then(|body| body.get("_vectors"))
            .and_then(|vectors| vectors.as_object())
            .into_iter()
            .flat_map(|vectors| vectors.values())
            .filter_map(Self::recovered_vector_values)
            .collect()
    }

    #[cfg(feature = "vector-search")]
    fn recovered_vector_values(vector_value: &serde_json::Value) -> Option<Vec<f32>> {
        let raw_values = vector_value.as_array()?;
        let vector: Vec<f32> = raw_values
            .iter()
            .filter_map(|value| value.as_f64().map(|float| float as f32))
            .collect();
        (vector.len() == raw_values.len() && !vector.is_empty()).then_some(vector)
    }

    /// Save the rebuilt VectorIndex to the tenant's `vectors/` directory and register
    /// it in the in-memory map. Logs a warning on save failure.
    #[cfg(feature = "vector-search")]
    fn persist_rebuilt_vector_index(
        &self,
        tenant_id: &str,
        tenant_path: &Path,
        vector_index: Option<crate::vector::index::VectorIndex>,
    ) {
        let Some(vector_store) = vector_index else {
            return;
        };

        let vectors_dir = tenant_path.join("vectors");
        if let Err(error) = vector_store.save(&vectors_dir) {
            tracing::warn!(
                "[RECOVERY {}] failed to save recovered vector index: {}",
                tenant_id,
                error
            );
        }
        let vector_count = vector_store.len();
        self.set_vector_index(tenant_id, vector_store);
        tracing::info!(
            "[RECOVERY {}] rebuilt vector index from oplog ({} vectors)",
            tenant_id,
            vector_count
        );
    }
}
