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

struct RecoveryDocumentPlan {
    receipt: OpLogReceipt,
    apply_effect: bool,
}

struct RecoveryOriginProofs {
    actions_by_task: HashMap<String, Vec<WriteAction>>,
}

impl RecoveryOriginProofs {
    fn load(tenant_id: &str, tenant_path: &Path) -> Result<Self> {
        let base_path = tenant_path.parent().ok_or_else(|| {
            FlapjackError::Tantivy(format!(
                "[RECOVERY {tenant_id}] tenant path has no data-root parent"
            ))
        })?;
        let store = WriteAdmissionStore::open(base_path, tenant_id)?;
        let actions_by_task = store
            .load_records()?
            .into_iter()
            .map(|record| (record.task_id, record.actions))
            .collect();
        Ok(Self { actions_by_task })
    }

    fn take_origin_seq(
        &mut self,
        entry: &OpLogEntry,
        object_id: Option<&str>,
        effect_digest: Option<[u8; 32]>,
    ) -> Option<u64> {
        let task_id = crate::index::oplog::payload_task_id(&entry.payload)?;
        let actions = self.actions_by_task.get_mut(task_id)?;
        let position = actions.iter().position(|action| {
            recovery_action_matches(action, entry.op_type.as_str(), object_id, effect_digest)
        })?;
        match actions.remove(position) {
            WriteAction::UpsertWithOrigin { origin, .. }
            | WriteAction::DeleteWithOrigin { origin, .. } => origin.origin_seq,
            WriteAction::Add(_) | WriteAction::Upsert(_) | WriteAction::Delete(_) => {
                Some(entry.seq)
            }
            WriteAction::UpsertNoLwwUpdate(_)
            | WriteAction::DeleteNoLwwUpdate(_)
            | WriteAction::Compact => None,
        }
    }
}

fn recovery_action_matches(
    action: &WriteAction,
    op_type: &str,
    object_id: Option<&str>,
    effect_digest: Option<[u8; 32]>,
) -> bool {
    match action {
        WriteAction::Add(document)
        | WriteAction::Upsert(document)
        | WriteAction::UpsertNoLwwUpdate(document)
        | WriteAction::UpsertWithOrigin { doc: document, .. } => {
            op_type == "upsert"
                && object_id == Some(document.id.as_str())
                && effect_digest == Some(crate::index::oplog::upsert_effect_digest(document))
        }
        WriteAction::Delete(action_object_id)
        | WriteAction::DeleteNoLwwUpdate(action_object_id)
        | WriteAction::DeleteWithOrigin {
            object_id: action_object_id,
            ..
        } => {
            op_type == "delete"
                && object_id == Some(action_object_id.as_str())
                && effect_digest
                    == Some(crate::index::oplog::delete_effect_digest(action_object_id))
        }
        WriteAction::Compact => false,
    }
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
        let mut origin_proofs = RecoveryOriginProofs::load(context.tenant_id, context.tenant_path)?;
        let version_store = crate::index::version_store::VersionStore::open(context.tenant_path)?;
        let plans = self.prepare_recovery_document_plans(
            context.tenant_id,
            ops,
            &mut origin_proofs,
            &version_store,
        )?;
        #[cfg(feature = "vector-search")]
        let selected_vector_ops = ops
            .iter()
            .zip(&plans)
            .filter_map(|(entry, plan)| plan.apply_effect.then_some(entry.clone()))
            .collect::<Vec<_>>();
        let mut writer = context.index.writer()?;
        let schema = context.index.inner().schema();
        let id_field = schema.get_field("_id").unwrap();
        {
            let mut writer_context = RecoveryWriterContext {
                tenant_id: context.tenant_id,
                index: context.index,
                settings: context.settings,
                writer: &mut writer,
                id_field,
            };
            for (entry, plan) in ops.iter().zip(&plans) {
                if plan.apply_effect {
                    self.recover_document_effect(&mut writer_context, entry)?;
                }
            }
        }

        writer.commit()?;
        context.index.reader().reload()?;
        context.index.invalidate_searchable_paths_cache();

        #[cfg(feature = "vector-search")]
        self.rebuild_vector_index(context.tenant_id, context.tenant_path, &selected_vector_ops);

        let receipts = plans
            .into_iter()
            .map(|plan| plan.receipt)
            .collect::<Vec<_>>();
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

    fn prepare_recovery_document_plans(
        &self,
        tenant_id: &str,
        ops: &[OpLogEntry],
        origin_proofs: &mut RecoveryOriginProofs,
        version_store: &crate::index::version_store::VersionStore,
    ) -> Result<Vec<RecoveryDocumentPlan>> {
        use crate::index::version_store::{VersionProofComparison, VersionRecord};

        let mut plans = Vec::with_capacity(ops.len());
        let mut planned_versions: HashMap<String, VersionRecord> = HashMap::new();
        for entry in ops {
            if entry.op_type == "clear" {
                plans.push(RecoveryDocumentPlan {
                    receipt: OpLogReceipt {
                        seq: entry.seq,
                        object_id: None,
                        timestamp_ms: entry.timestamp_ms,
                        node_id: entry.node_id.clone(),
                        is_tombstone: false,
                        origin_seq: None,
                        effect_digest: None,
                    },
                    apply_effect: true,
                });
                continue;
            }

            let (object_id, effect_digest, is_tombstone) =
                Self::recovery_document_identity(tenant_id, entry)?;
            let stable_origin_seq = crate::index::oplog::replication_origin_seq(&entry.payload)
                .map_err(|error| {
                    FlapjackError::Tantivy(format!(
                        "[RECOVERY {tenant_id}] invalid origin proof at seq {}: {error}",
                        entry.seq
                    ))
                })?;
            let admission_origin_seq =
                origin_proofs.take_origin_seq(entry, Some(&object_id), Some(effect_digest));
            let durable = match planned_versions.get(&object_id) {
                Some(version) => Some((version.clone(), true)),
                None => version_store
                    .get(&object_id)?
                    .map(|version| (version, false)),
            };
            let origin_seq = match (stable_origin_seq, admission_origin_seq) {
                (Some(stable), Some(admission)) if stable != admission => {
                    return Err(FlapjackError::Tantivy(format!(
                        "[RECOVERY {tenant_id}] conflicting stable/admission origin proof for {object_id} at seq {}",
                        entry.seq
                    )));
                }
                (Some(stable), _) => stable,
                (None, Some(admission)) => admission,
                (None, None) => durable
                    .as_ref()
                    .and_then(|(version, _)| {
                        if version.timestamp_ms == entry.timestamp_ms
                            && version.node_id == entry.node_id
                            && version.tombstone == is_tombstone
                            && version.effect_digest == Some(effect_digest)
                        {
                            version.origin_seq
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        FlapjackError::Tantivy(format!(
                            "[RECOVERY {tenant_id}] missing stable origin proof for {object_id} at seq {}",
                            entry.seq
                        ))
                    })?,
            };
            let candidate =
                VersionRecord::new(entry.timestamp_ms, &entry.node_id, is_tombstone, entry.seq)
                    .with_origin_proof(origin_seq, effect_digest);
            let apply_effect = match durable {
                Some((existing, from_planned_tail)) => {
                    match candidate.compare_replication_proof(&existing) {
                        VersionProofComparison::Newer => {
                            planned_versions.insert(object_id.clone(), candidate.clone());
                            true
                        }
                        VersionProofComparison::Older => false,
                        VersionProofComparison::Exact => !from_planned_tail,
                        VersionProofComparison::Ambiguous => {
                            return Err(FlapjackError::Tantivy(format!(
                                "[RECOVERY {tenant_id}] ambiguous origin proof for {object_id} at seq {}",
                                entry.seq
                            )));
                        }
                    }
                }
                None => {
                    planned_versions.insert(object_id.clone(), candidate.clone());
                    true
                }
            };
            plans.push(RecoveryDocumentPlan {
                receipt: OpLogReceipt {
                    seq: entry.seq,
                    object_id: Some(object_id),
                    timestamp_ms: entry.timestamp_ms,
                    node_id: entry.node_id.clone(),
                    is_tombstone,
                    origin_seq: Some(origin_seq),
                    effect_digest: Some(effect_digest),
                },
                apply_effect,
            });
        }
        Ok(plans)
    }

    fn recover_document_effect(
        &self,
        context: &mut RecoveryWriterContext<'_>,
        entry: &OpLogEntry,
    ) -> Result<()> {
        match entry.op_type.as_str() {
            "upsert" => {
                self.recover_upsert_entry(context, entry)?;
            }
            "delete" => {
                Self::recover_delete_entry(context, entry)?;
            }
            "clear" => {
                context.writer.delete_all_documents()?;
            }
            _ => {
                return Err(FlapjackError::Tantivy(format!(
                    "[RECOVERY {}] non-document op '{}' reached document recovery at seq {}",
                    context.tenant_id, entry.op_type, entry.seq
                )));
            }
        }
        Ok(())
    }

    fn recovery_document_identity(
        tenant_id: &str,
        entry: &OpLogEntry,
    ) -> Result<(String, [u8; 32], bool)> {
        match entry.op_type.as_str() {
            "upsert" => {
                let body = entry
                    .payload
                    .get("body")
                    .ok_or_else(|| Self::invalid_recovery_entry(tenant_id, entry, "body"))?;
                let document = crate::types::Document::from_json(body).map_err(|error| {
                    FlapjackError::Tantivy(format!(
                        "[RECOVERY {tenant_id}] failed to parse document at seq {}: {error}",
                        entry.seq
                    ))
                })?;
                Ok((
                    document.id.clone(),
                    crate::index::oplog::upsert_effect_digest(&document),
                    false,
                ))
            }
            "delete" => {
                let object_id = entry
                    .payload
                    .get("objectID")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| Self::invalid_recovery_entry(tenant_id, entry, "objectID"))?;
                Ok((
                    object_id.to_string(),
                    crate::index::oplog::delete_effect_digest(object_id),
                    true,
                ))
            }
            _ => Err(FlapjackError::Tantivy(format!(
                "[RECOVERY {tenant_id}] unsupported proof operation '{}' at seq {}",
                entry.op_type, entry.seq
            ))),
        }
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

    #[test]
    fn replication_origin_proof_recovery_resolves_source_seq_from_admission() {
        let document = crate::types::Document::from_json(&serde_json::json!({
            "objectID": "recovered",
            "title": "accepted body"
        }))
        .unwrap();
        let mut proofs = RecoveryOriginProofs {
            actions_by_task: HashMap::from([(
                "task-1".to_string(),
                vec![WriteAction::UpsertWithOrigin {
                    doc: document.clone(),
                    origin: ReplicatedWriteOrigin::new(5_000, "source-node".to_string())
                        .with_origin_seq(91),
                }],
            )]),
        };
        let entry = OpLogEntry {
            seq: 7,
            timestamp_ms: 5_000,
            node_id: "source-node".to_string(),
            tenant_id: "tenant".to_string(),
            op_type: "upsert".to_string(),
            payload: serde_json::json!({
                "objectID": "recovered",
                "body": document.to_json(),
                "_flapjack_task_id": "task-1"
            }),
        };
        let digest = crate::index::oplog::operation_effect_digest("upsert", &entry.payload);

        assert_eq!(
            proofs.take_origin_seq(&entry, Some("recovered"), digest),
            Some(91),
            "recovery must preserve source seq rather than destination oplog seq"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replication_origin_proof_recovery_refuses_unproven_or_contradictory_tail() {
        for (case, seed_contradictory_version) in
            [("missing-proof", false), ("contradictory-proof", true)]
        {
            let temp_dir = TempDir::new().unwrap();
            let tenant_id = format!("recovery-{case}");
            let tenant_path = temp_dir.path().join(&tenant_id);
            std::fs::create_dir_all(&tenant_path).unwrap();
            let schema = crate::index::schema::Schema::builder().build();
            let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
            IndexSettings::default()
                .save(tenant_path.join("settings.json"))
                .unwrap();
            let document = crate::types::Document::from_json(&serde_json::json!({
                "objectID": "unproven-tail",
                "title": "must not recover without exact origin proof"
            }))
            .unwrap();
            let oplog = OpLog::open(&tenant_path.join("oplog"), &tenant_id, "local-node").unwrap();
            oplog
                .append_operations_for_task(
                    "unproven-recovery-task",
                    vec![OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({
                            "objectID": document.id.clone(),
                            "body": document.to_json()
                        }),
                        OpLogOrigin::new(5_000, "source-node"),
                    )],
                )
                .unwrap();
            drop(oplog);

            let expected_existing = seed_contradictory_version.then(|| {
                VersionRecord::new(5_000, "source-node", false, 1).with_origin_proof(91, [0xaa; 32])
            });
            if let Some(version) = expected_existing.as_ref() {
                assert!(VersionStore::open(&tenant_path)
                    .unwrap()
                    .upsert("unproven-tail", version)
                    .unwrap());
            }

            let manager = IndexManager::new_with_node_id(temp_dir.path(), "local-node");
            let error = manager
                .recover_from_oplog(&tenant_id, &index, &tenant_path)
                .expect_err("an unproven replicated tail must fail before mutation");

            assert!(
                error.to_string().contains("origin") || error.to_string().contains("proof"),
                "{case} refusal must identify missing or contradictory origin proof: {error}"
            );
            assert_eq!(read_committed_seq(&tenant_path), 0, "{case}");
            assert_eq!(index.reader().searcher().num_docs(), 0, "{case}");
            assert_eq!(
                VersionStore::open(&tenant_path)
                    .unwrap()
                    .get("unproven-tail")
                    .unwrap(),
                expected_existing,
                "{case} must leave durable version evidence unchanged"
            );
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replication_origin_proof_recovery_accepts_already_durable_exact_complete_version() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_id = "recovery-exact-durable-proof";
        let tenant_path = temp_dir.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let schema = crate::index::schema::Schema::builder().build();
        let index = Arc::new(crate::index::Index::create(&tenant_path, schema).unwrap());
        IndexSettings::default()
            .save(tenant_path.join("settings.json"))
            .unwrap();
        let document = crate::types::Document::from_json(&serde_json::json!({
            "objectID": "exact-tail",
            "title": "durable exact effect"
        }))
        .unwrap();
        let oplog = OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node").unwrap();
        oplog
            .append_operations_for_task(
                "exact-recovery-task",
                vec![OpLogOperation::replicated(
                    "upsert",
                    serde_json::json!({
                        "objectID": document.id.clone(),
                        "body": document.to_json()
                    }),
                    OpLogOrigin::new(6_000, "source-node"),
                )],
            )
            .unwrap();
        drop(oplog);
        let exact_version = VersionRecord::new(6_000, "source-node", false, 1)
            .with_origin_proof(92, crate::index::oplog::upsert_effect_digest(&document));
        assert!(VersionStore::open(&tenant_path)
            .unwrap()
            .upsert("exact-tail", &exact_version)
            .unwrap());

        let manager = IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        manager
            .recover_from_oplog(tenant_id, &index, &tenant_path)
            .expect("an already-durable exact complete version may finish recovery safely");

        assert_eq!(read_committed_seq(&tenant_path), 1);
        assert_eq!(index.reader().searcher().num_docs(), 1);
        assert_eq!(
            VersionStore::open(&tenant_path)
                .unwrap()
                .get("exact-tail")
                .unwrap(),
            Some(exact_version)
        );
    }

    #[cfg(feature = "vector-search")]
    #[tokio::test(flavor = "current_thread")]
    async fn olr_vector_recovery_applies_only_selected_document_plans() {
        let temp_dir = TempDir::new().unwrap();
        let tenant_id = "olr-selected-vector-recovery";
        let tenant_path = temp_dir.path().join(tenant_id);
        std::fs::create_dir_all(&tenant_path).unwrap();
        let schema = crate::index::schema::Schema::builder().build();
        let _index = crate::index::Index::create(&tenant_path, schema).unwrap();
        IndexSettings::default()
            .save(tenant_path.join("settings.json"))
            .unwrap();
        let newer_document = crate::types::Document::from_json(&serde_json::json!({
            "objectID": "same-doc",
            "title": "newer-a",
            "_vectors": {"default": [1.0, 0.0, 0.0]}
        }))
        .unwrap();
        let older_document = crate::types::Document::from_json(&serde_json::json!({
            "objectID": "same-doc",
            "title": "older-b",
            "_vectors": {"default": [0.0, 1.0, 0.0]}
        }))
        .unwrap();
        let oplog = OpLog::open(&tenant_path.join("oplog"), tenant_id, "local-node").unwrap();
        oplog
            .append_operations_for_task(
                "olr-vector-recovery",
                vec![
                    OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({
                            "objectID": "same-doc",
                            "body": newer_document.to_json()
                        }),
                        OpLogOrigin::new(2_000, "source-node").with_origin_seq(20),
                    ),
                    OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({
                            "objectID": "same-doc",
                            "body": older_document.to_json()
                        }),
                        OpLogOrigin::new(1_000, "source-node").with_origin_seq(10),
                    ),
                ],
            )
            .unwrap();
        write_committed_seq(&tenant_path, 0).unwrap();
        drop(oplog);
        drop(_index);

        let manager = IndexManager::new_with_node_id(temp_dir.path(), "local-node");
        manager.get_or_load(tenant_id).unwrap();

        let recovered = manager
            .get_document(tenant_id, "same-doc")
            .unwrap()
            .expect("the selected newer document must be searchable");
        assert!(matches!(
            recovered.fields.get("title"),
            Some(crate::types::FieldValue::Text(value)) if value == "newer-a"
        ));
        assert_eq!(
            manager.get_object_version(tenant_id, "same-doc").unwrap(),
            Some(
                VersionRecord::new(2_000, "source-node", false, 1).with_origin_proof(
                    20,
                    crate::index::oplog::upsert_effect_digest(&newer_document),
                ),
            )
        );
        let vector_index = manager
            .get_vector_index(tenant_id)
            .expect("the selected document vector must be recovered");
        assert_eq!(
            vector_index.read().unwrap().get("same-doc").unwrap(),
            Some(vec![1.0, 0.0, 0.0]),
            "an older skipped document plan must not overwrite the selected vector effect"
        );
        assert_eq!(read_committed_seq(&tenant_path), 2);
    }

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
                        OpLogOrigin::new(1000, "node-a").with_origin_seq(1),
                    ),
                    OpLogOperation::replicated(
                        "upsert",
                        serde_json::json!({
                            "objectID": "recovered-upsert",
                            "body": {"objectID": "recovered-upsert", "title": "Recovered"}
                        }),
                        OpLogOrigin::new(5000, "node-b").with_origin_seq(2),
                    ),
                    OpLogOperation::replicated(
                        "delete",
                        serde_json::json!({"objectID": "recovered-delete"}),
                        OpLogOrigin::new(6000, "node-c").with_origin_seq(3),
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
            Some(VersionRecord {
                timestamp_ms: 5000,
                node_id: "node-b".to_string(),
                tombstone: false,
                oplog_seq: 2,
                origin_seq: Some(2),
                effect_digest: Some(crate::index::oplog::upsert_effect_digest(
                    &crate::types::Document::from_json(&serde_json::json!({
                        "objectID": "recovered-upsert",
                        "title": "Recovered"
                    }))
                    .unwrap(),
                )),
            })
        );
        assert_eq!(
            recovered_store.get("recovered-delete").unwrap(),
            Some(VersionRecord {
                timestamp_ms: 6000,
                node_id: "node-c".to_string(),
                tombstone: true,
                oplog_seq: 3,
                origin_seq: Some(3),
                effect_digest: Some(crate::index::oplog::delete_effect_digest(
                    "recovered-delete",
                )),
            })
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
