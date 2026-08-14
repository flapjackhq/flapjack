use super::*;
#[cfg(test)]
use crate::index::oplog::read_committed_seq;
use crate::index::oplog::{
    read_checked_committed_seq, write_committed_seq, OpLogEntry, OpLogReceipt,
};

#[derive(Clone, Copy)]
pub(super) struct RecoverySeqWindow {
    pub(super) committed_seq: u64,
    pub(super) final_seq: u64,
}

pub(super) struct RecoveryDocumentContext<'a> {
    pub(super) tenant_id: &'a str,
    pub(super) index: &'a Arc<Index>,
    pub(super) tenant_path: &'a Path,
    pub(super) seq_window: RecoverySeqWindow,
    pub(super) settings: Option<&'a IndexSettings>,
}

struct RecoveryWriterContext<'a> {
    tenant_id: &'a str,
    index: &'a Arc<Index>,
    settings: Option<&'a IndexSettings>,
    writer: &'a mut crate::index::ManagedIndexWriter,
    id_field: tantivy::schema::Field,
}

impl IndexManager {
    /// Recover the uncommitted oplog tail for a tenant after startup.
    ///
    /// Config changes are restored first. Document changes then commit to Tantivy,
    /// update the durable object-version store in one transaction, and finally
    /// advance `committed_seq`. Any error before the final step leaves the tail
    /// replayable on the next startup.
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
        let committed_seq = read_checked_committed_seq(tenant_path)?.unwrap_or(0);

        let node_id = crate::index::configured_node_id();
        let oplog = OpLog::open(&oplog_dir, tenant_id, &node_id)?;

        let ops = oplog.read_since(committed_seq)?;
        if ops.is_empty() {
            return Ok(());
        }
        Self::validate_recovery_sequence(tenant_id, committed_seq, &ops)?;

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
        self.recover_document_ops(
            RecoveryDocumentContext {
                tenant_id,
                index,
                tenant_path,
                seq_window,
                settings: settings.as_ref(),
            },
            &document_ops,
        )?;

        #[cfg(feature = "vector-search")]
        self.rebuild_vector_index(tenant_id, tenant_path, &ops);

        Ok(())
    }

    pub(super) fn is_document_recovery_op(op_type: &str) -> bool {
        matches!(op_type, "upsert" | "delete" | "clear")
    }

    fn validate_recovery_sequence(
        tenant_id: &str,
        committed_seq: u64,
        ops: &[OpLogEntry],
    ) -> Result<()> {
        let Some(first_entry) = ops.first() else {
            return Ok(());
        };
        if first_entry.seq <= committed_seq {
            return Err(FlapjackError::Tantivy(format!(
                "[RECOVERY {tenant_id}] oplog tail starts at seq {} at or before committed seq {committed_seq}",
                first_entry.seq
            )));
        }
        for entries in ops.windows(2) {
            let expected_seq = entries[0].seq.checked_add(1).ok_or_else(|| {
                FlapjackError::Tantivy(format!(
                    "[RECOVERY {tenant_id}] oplog sequence overflow after {}",
                    entries[0].seq
                ))
            })?;
            let entry = &entries[1];
            if entry.seq != expected_seq {
                return Err(FlapjackError::Tantivy(format!(
                    "[RECOVERY {tenant_id}] non-contiguous oplog tail: expected seq {expected_seq}, found {}",
                    entry.seq
                )));
            }
        }
        Ok(())
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
                    crate::index::atomic_write_file(&settings_path, settings_json.as_bytes())
                        .map_err(|error| {
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

    /// Recover document operations through the durable Tantivy/version/watermark order.
    pub(super) fn recover_document_ops(
        &self,
        context: RecoveryDocumentContext<'_>,
        ops: &[OpLogEntry],
    ) -> Result<()> {
        let mut writer = context.index.writer()?;
        let schema = context.index.inner().schema();
        let id_field = schema.get_field("_id").unwrap();
        let receipts = self.recover_document_entries(
            RecoveryWriterContext {
                tenant_id: context.tenant_id,
                index: context.index,
                settings: context.settings,
                writer: &mut writer,
                id_field,
            },
            ops,
        )?;

        writer.commit()?;
        context.index.reader().reload()?;
        context.index.invalidate_searchable_paths_cache();

        let version_store = crate::index::version_store::VersionStore::open(context.tenant_path)?;
        version_store.apply_receipts(&receipts)?;
        write_committed_seq(context.tenant_path, context.seq_window.final_seq)?;
        tracing::info!(
            "[RECOVERY {}] recovered {} document ops, new committed_seq={}",
            context.tenant_id,
            receipts.len(),
            context.seq_window.final_seq
        );
        Ok(())
    }

    fn recover_document_entries(
        &self,
        mut context: RecoveryWriterContext<'_>,
        ops: &[OpLogEntry],
    ) -> Result<Vec<OpLogReceipt>> {
        let mut receipts = Vec::with_capacity(ops.len());
        for entry in ops {
            receipts.push(self.recover_document_entry(&mut context, entry)?);
        }
        Ok(receipts)
    }

    fn recover_document_entry(
        &self,
        context: &mut RecoveryWriterContext<'_>,
        entry: &OpLogEntry,
    ) -> Result<OpLogReceipt> {
        let object_id = match entry.op_type.as_str() {
            "upsert" => Some(self.recover_upsert_entry(context, entry)?),
            "delete" => Some(Self::recover_delete_entry(context, entry)?),
            "clear" => {
                context.writer.delete_all_documents()?;
                None
            }
            _ => {
                return Err(FlapjackError::Tantivy(format!(
                    "[RECOVERY {}] non-document op '{}' reached document recovery at seq {}",
                    context.tenant_id, entry.op_type, entry.seq
                )));
            }
        };
        Ok(OpLogReceipt {
            seq: entry.seq,
            object_id,
            timestamp_ms: entry.timestamp_ms,
            node_id: entry.node_id.clone(),
            is_tombstone: entry.op_type == "delete",
        })
    }

    fn recover_upsert_entry(
        &self,
        context: &mut RecoveryWriterContext<'_>,
        entry: &OpLogEntry,
    ) -> Result<String> {
        let body = entry
            .payload
            .get("body")
            .ok_or_else(|| Self::invalid_recovery_entry(context.tenant_id, entry, "body"))?;
        let document = crate::types::Document::from_json(body).map_err(|error| {
            FlapjackError::Tantivy(format!(
                "[RECOVERY {}] failed to parse document at seq {}: {}",
                context.tenant_id, entry.seq, error
            ))
        })?;
        let object_id = document.id.clone();
        let tantivy_document = context
            .index
            .converter()
            .to_tantivy(&document, context.settings)
            .map_err(|error| {
                FlapjackError::Tantivy(format!(
                    "[RECOVERY {}] failed to convert document '{}' at seq {}: {}",
                    context.tenant_id, object_id, entry.seq, error
                ))
            })?;
        context
            .writer
            .delete_term(tantivy::Term::from_field_text(context.id_field, &object_id));
        context.writer.add_document(tantivy_document)?;
        Ok(object_id)
    }

    fn recover_delete_entry(
        context: &mut RecoveryWriterContext<'_>,
        entry: &OpLogEntry,
    ) -> Result<String> {
        let object_id = entry
            .payload
            .get("objectID")
            .and_then(|value| value.as_str())
            .ok_or_else(|| Self::invalid_recovery_entry(context.tenant_id, entry, "objectID"))?;
        context
            .writer
            .delete_term(tantivy::Term::from_field_text(context.id_field, object_id));
        Ok(object_id.to_string())
    }

    fn invalid_recovery_entry(
        tenant_id: &str,
        entry: &OpLogEntry,
        missing_field: &str,
    ) -> FlapjackError {
        FlapjackError::Tantivy(format!(
            "[RECOVERY {tenant_id}] {} at seq {} is missing required {missing_field}",
            entry.op_type, entry.seq
        ))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::oplog::{OpLogOperation, OpLogOrigin};
    use crate::index::version_store::{VersionRecord, VersionStore};
    use tempfile::TempDir;

    #[tokio::test(flavor = "current_thread")]
    async fn recovery_replays_after_committed_seq_into_version_store() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_id = "durable_recovery";
        let tenant_path = temp_dir.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
        IndexSettings::default()
            .save(tenant_path.join("settings.json"))
            .unwrap();

        let oplog = OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node").unwrap();
        oplog
            .append_operations_for_task(
                "recovery-task",
                vec![
                    OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({
                            "objectID": "already-committed",
                            "body": {"objectID": "already-committed", "title": "Committed"}
                        }),
                        OpLogOrigin::new(1000, "node-a"),
                    ),
                    OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({
                            "objectID": "recovered-upsert",
                            "body": {"objectID": "recovered-upsert", "title": "Recovered"}
                        }),
                        OpLogOrigin::new(5000, "node-b"),
                    ),
                    OpLogOperation::replicated(
                        "delete",
                        serde_json::json!({"objectID": "recovered-delete"}),
                        OpLogOrigin::new(6000, "node-c"),
                    ),
                ],
            )
            .unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        let version_store = VersionStore::open(&tenant_path).unwrap();
        assert!(version_store
            .upsert(
                "already-committed",
                &VersionRecord::new(1000, "node-a", false, 1),
            )
            .unwrap());
        drop(version_store);
        drop(oplog);

        let manager = IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        manager
            .recover_from_oplog(tenant_id, &index, &tenant_path)
            .unwrap();

        let recovered_store = VersionStore::open(&tenant_path).unwrap();
        assert_eq!(
            recovered_store.get("already-committed").unwrap(),
            Some(VersionRecord::new(1000, "node-a", false, 1))
        );
        assert_eq!(
            recovered_store.get("recovered-upsert").unwrap(),
            Some(VersionRecord::new(5000, "node-b", false, 2))
        );
        assert_eq!(
            recovered_store.get("recovered-delete").unwrap(),
            Some(VersionRecord::new(6000, "node-c", true, 3))
        );
        assert_eq!(read_committed_seq(&tenant_path), 3);
        let retained = OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node")
            .unwrap()
            .read_since(0)
            .unwrap();
        assert_eq!(
            retained.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, 2, 3],
            "recovery must not discard retained oplog evidence"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_document_recovery_fails_without_advancing_durable_state() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_id = "malformed_recovery";
        let tenant_path = temp_dir.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
        IndexSettings::default()
            .save(tenant_path.join("settings.json"))
            .unwrap();
        let oplog = OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node").unwrap();
        oplog
            .append_operations_for_task(
                "malformed-task",
                vec![OpLogOperation::replicated(
                    "upsert",
                    serde_json::json!({"objectID": "missing-body"}),
                    OpLogOrigin::new(7000, "node-z"),
                )],
            )
            .unwrap();
        drop(oplog);

        let manager = IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        let result = manager.recover_from_oplog(tenant_id, &index, &tenant_path);

        assert!(
            result.is_err(),
            "malformed document replay must fail closed"
        );
        assert_eq!(read_committed_seq(&tenant_path), 0);
        assert_eq!(
            VersionStore::open(&tenant_path)
                .unwrap()
                .get("missing-body")
                .unwrap(),
            None,
            "failed decoding must not publish version rows"
        );
        assert_eq!(
            index.reader().searcher().num_docs(),
            0,
            "failed decoding must not commit a partial Tantivy batch"
        );
        assert_eq!(
            OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node")
                .unwrap()
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn malformed_committed_seq_refuses_recovery_without_mutating_state() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_id = "malformed_watermark";
        let tenant_path = temp_dir.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
        IndexSettings::default()
            .save(tenant_path.join("settings.json"))
            .unwrap();
        let oplog = OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node").unwrap();
        oplog
            .append_operations_for_task(
                "watermark-task",
                vec![OpLogOperation::replicated(
                    "upsert",
                    serde_json::json!({
                        "objectID": "must-not-replay",
                        "body": {"objectID": "must-not-replay", "title": "Uncommitted"}
                    }),
                    OpLogOrigin::new(7000, "node-z"),
                )],
            )
            .unwrap();
        std::fs::write(
            tenant_path.join(crate::index::oplog::COMMITTED_SEQ_FILE),
            "not-a-sequence",
        )
        .unwrap();
        drop(oplog);

        let manager = IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        let error = manager
            .recover_from_oplog(tenant_id, &index, &tenant_path)
            .expect_err("corrupt watermark evidence must fail recovery closed");

        assert!(
            error.to_string().contains("not a u64"),
            "recovery error must identify malformed sequence evidence: {error}"
        );
        assert_eq!(index.reader().searcher().num_docs(), 0);
        assert_eq!(
            VersionStore::open(&tenant_path)
                .unwrap()
                .get("must-not-replay")
                .unwrap(),
            None
        );
        assert_eq!(
            std::fs::read_to_string(tenant_path.join(crate::index::oplog::COMMITTED_SEQ_FILE))
                .unwrap(),
            "not-a-sequence",
            "failed recovery must not replace corrupt watermark evidence"
        );
    }

    #[test]
    fn recovery_accepts_retained_leading_gap_before_replay() {
        let retained_tail = OpLogEntry {
            seq: 3,
            timestamp_ms: 2000,
            node_id: "node-a".to_string(),
            tenant_id: "retained-tail".to_string(),
            op_type: "clear".to_string(),
            payload: serde_json::json!({}),
        };

        IndexManager::validate_recovery_sequence("retained-tail", 1, &[retained_tail])
            .expect("retention may remove committed history before the first surviving tail entry");
    }

    #[test]
    fn recovery_rejects_gap_inside_retained_tail() {
        let retained_tail = [
            OpLogEntry {
                seq: 3,
                timestamp_ms: 2000,
                node_id: "node-a".to_string(),
                tenant_id: "internal-gap".to_string(),
                op_type: "clear".to_string(),
                payload: serde_json::json!({}),
            },
            OpLogEntry {
                seq: 5,
                timestamp_ms: 3000,
                node_id: "node-a".to_string(),
                tenant_id: "internal-gap".to_string(),
                op_type: "clear".to_string(),
                payload: serde_json::json!({}),
            },
        ];

        let error = IndexManager::validate_recovery_sequence("internal-gap", 1, &retained_tail)
            .unwrap_err();

        assert!(
            error.to_string().contains("expected seq 4, found 5"),
            "sequence failure must identify the exact missing local sequence: {error}"
        );
    }
}
