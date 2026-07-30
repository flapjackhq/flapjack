use flapjack::index::oplog::OpLogEntry;
use flapjack::index::version_store::tuple_is_strictly_newer;
use flapjack::index::write_queue::ReplicatedWriteOrigin;
use flapjack::types::Document;
use flapjack::IndexManager;
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(crate) struct ReplicatedDocumentBatch {
    upserts: Vec<(Document, ReplicatedWriteOrigin)>,
    deletes: Vec<(String, ReplicatedWriteOrigin)>,
    final_op_type: HashMap<String, &'static str>,
    pending_versions: HashMap<String, (u64, String)>,
}

impl ReplicatedDocumentBatch {
    fn accept_version(
        &mut self,
        manager: &IndexManager,
        tenant_id: &str,
        object_id: &str,
        incoming: &(u64, String),
    ) -> Result<bool, String> {
        if !self.pending_versions.contains_key(object_id) {
            let durable = manager
                .get_object_version(tenant_id, object_id)
                .map_err(|error| format!("failed to read durable object version: {error}"))?;
            if let Some(durable) = durable {
                self.pending_versions.insert(
                    object_id.to_string(),
                    (durable.timestamp_ms, durable.node_id),
                );
            }
        }

        if let Some(existing) = self.pending_versions.get(object_id) {
            if !tuple_is_strictly_newer(
                (incoming.0, incoming.1.as_str()),
                (existing.0, existing.1.as_str()),
            ) {
                return Ok(false);
            }
        }

        self.pending_versions
            .insert(object_id.to_string(), incoming.clone());
        Ok(true)
    }
}

/// Apply an upsert replication op to invocation-scoped batch state.
pub(crate) fn apply_upsert_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
    incoming: (u64, String),
    batch: &mut ReplicatedDocumentBatch,
) -> Result<(), String> {
    let Some(body) = op_entry.payload.get("body") else {
        tracing::warn!(
            "[REPL {}] upsert seq {} missing body field",
            tenant_id,
            op_entry.seq
        );
        return Ok(());
    };

    match Document::from_json(body) {
        Ok(doc) => {
            if !batch.accept_version(manager, tenant_id, &doc.id, &incoming)? {
                tracing::debug!(
                    "[REPL {}] skipping stale upsert for {}/{}",
                    tenant_id,
                    tenant_id,
                    doc.id
                );
                return Ok(());
            }
            batch.final_op_type.insert(doc.id.to_string(), "upsert");
            batch
                .upserts
                .push((doc, ReplicatedWriteOrigin::new(incoming.0, incoming.1)));
        }
        Err(e) => tracing::warn!(
            "[REPL {}] failed to parse upsert seq {}: {}",
            tenant_id,
            op_entry.seq,
            e
        ),
    }
    Ok(())
}

/// Apply a delete replication op to invocation-scoped batch state.
pub(crate) fn apply_delete_op(
    manager: &IndexManager,
    tenant_id: &str,
    op_entry: &OpLogEntry,
    incoming: (u64, String),
    batch: &mut ReplicatedDocumentBatch,
) -> Result<(), String> {
    let Some(id) = op_entry.payload.get("objectID").and_then(|v| v.as_str()) else {
        tracing::warn!(
            "[REPL {}] delete seq {} missing objectID field",
            tenant_id,
            op_entry.seq
        );
        return Ok(());
    };

    if !batch.accept_version(manager, tenant_id, id, &incoming)? {
        tracing::debug!(
            "[REPL {}] skipping stale delete for {}/{}",
            tenant_id,
            tenant_id,
            id
        );
        return Ok(());
    }

    batch.final_op_type.insert(id.to_string(), "delete");
    batch.deletes.push((
        id.to_string(),
        ReplicatedWriteOrigin::new(incoming.0, incoming.1),
    ));
    Ok(())
}

/// Resolve batch ordering, deduplicate upserts, and flush documents to the index.
///
/// When the same doc ID appears in both upserts and deletes within one batch,
/// only the operation with the newest origin tuple is applied. Upserts are further
/// deduplicated so only the last version per doc ID is indexed.
pub(crate) async fn flush_document_batch(
    manager: &IndexManager,
    tenant_id: &str,
    mut batch: ReplicatedDocumentBatch,
) -> Result<(), String> {
    // Resolve batch ordering: when the same doc ID appears in both upserts and
    // deletes, only the operation with the newest origin tuple should be applied.
    batch.upserts.retain(|(doc, _)| {
        batch
            .final_op_type
            .get(&doc.id)
            .copied()
            .unwrap_or("upsert")
            == "upsert"
    });
    batch.deletes.retain(|(id, _)| {
        batch
            .final_op_type
            .get(id.as_str())
            .copied()
            .unwrap_or("delete")
            == "delete"
    });

    // Deduplicate upserts: keep only the last version for each doc ID.
    // tantivy's delete_term only affects pre-existing docs, so adding two
    // docs with the same ID in one batch leaves both in the index.
    {
        let mut seen = HashSet::new();
        let mut deduped = Vec::with_capacity(batch.upserts.len());
        for (doc, origin) in batch.upserts.into_iter().rev() {
            if seen.insert(doc.id.clone()) {
                deduped.push((doc, origin));
            }
        }
        deduped.reverse();
        batch.upserts = deduped;
    }

    if !batch.upserts.is_empty() {
        manager
            .add_documents_sync_for_replication_with_origins(tenant_id, batch.upserts)
            .await
            .map_err(|e| format!("add_documents failed: {}", e))?;
    }

    if !batch.deletes.is_empty() {
        manager
            .delete_documents_sync_for_replication_with_origins(tenant_id, batch.deletes)
            .await
            .map_err(|e| format!("delete_documents failed: {}", e))?;
    }

    Ok(())
}
