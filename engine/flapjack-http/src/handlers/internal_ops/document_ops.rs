use super::wait_for_durable_replication_task;
use flapjack::index::oplog::OpLogEntry;
use flapjack::index::version_store::{VersionProofComparison, VersionRecord};
use flapjack::index::write_queue::ReplicatedWriteOrigin;
use flapjack::types::Document;
use flapjack::IndexManager;
use std::collections::{HashMap, HashSet};

#[cfg(test)]
type AfterDocumentProofAcceptedHook = std::sync::Arc<dyn Fn(u64) + Send + Sync>;

#[cfg(test)]
static AFTER_DOCUMENT_PROOF_ACCEPTED_HOOK: std::sync::OnceLock<
    std::sync::Mutex<Option<AfterDocumentProofAcceptedHook>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
pub(crate) struct AfterDocumentProofAcceptedHookGuard {
    previous: Option<AfterDocumentProofAcceptedHook>,
}

#[cfg(test)]
impl Drop for AfterDocumentProofAcceptedHookGuard {
    fn drop(&mut self) {
        *after_document_proof_accepted_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
fn after_document_proof_accepted_hook(
) -> &'static std::sync::Mutex<Option<AfterDocumentProofAcceptedHook>> {
    AFTER_DOCUMENT_PROOF_ACCEPTED_HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

#[cfg(test)]
pub(crate) fn set_after_document_proof_accepted_hook_for_test(
    hook: impl Fn(u64) + Send + Sync + 'static,
) -> AfterDocumentProofAcceptedHookGuard {
    let mut slot = after_document_proof_accepted_hook().lock().unwrap();
    AfterDocumentProofAcceptedHookGuard {
        previous: slot.replace(std::sync::Arc::new(hook)),
    }
}

#[cfg(test)]
pub(crate) fn run_after_document_proof_accepted_hook_for_test(source_seq: u64) {
    let hook = after_document_proof_accepted_hook().lock().unwrap().clone();
    if let Some(hook) = hook {
        hook(source_seq);
    }
}

#[derive(Default)]
pub(crate) struct ReplicatedDocumentBatch {
    upserts: Vec<(Document, ReplicatedWriteOrigin)>,
    deletes: Vec<(String, ReplicatedWriteOrigin)>,
    final_op_type: HashMap<String, &'static str>,
    pending_versions: HashMap<String, VersionRecord>,
}

enum IncomingDocumentEffect<'a> {
    Upsert(&'a Document),
    Delete,
}

impl IncomingDocumentEffect<'_> {
    fn is_tombstone(&self) -> bool {
        matches!(self, Self::Delete)
    }

    fn digest(&self, object_id: &str) -> [u8; 32] {
        match self {
            Self::Upsert(document) => flapjack::index::oplog::upsert_effect_digest(document),
            Self::Delete => flapjack::index::oplog::delete_effect_digest(object_id),
        }
    }

    fn op_type(&self) -> &'static str {
        match self {
            Self::Upsert(_) => "upsert",
            Self::Delete => "delete",
        }
    }
}

impl ReplicatedDocumentBatch {
    fn accept_version(
        &mut self,
        manager: &IndexManager,
        tenant_id: &str,
        object_id: &str,
        source_seq: u64,
        incoming: &(u64, String),
        effect: IncomingDocumentEffect<'_>,
    ) -> Result<bool, String> {
        let candidate = VersionRecord::new(incoming.0, &incoming.1, effect.is_tombstone(), 0)
            .with_origin_proof(source_seq, effect.digest(object_id));
        if let Some(existing) = self.pending_versions.get(object_id) {
            match candidate.compare_replication_proof(existing) {
                VersionProofComparison::Newer => {}
                VersionProofComparison::Older | VersionProofComparison::Exact => return Ok(false),
                VersionProofComparison::Ambiguous => {
                    return Err(ambiguous_equal_origin_error(
                        tenant_id,
                        object_id,
                        effect.op_type(),
                        "conflicting effects share one source sequence",
                    ));
                }
            }
        } else if let Some(durable) = manager
            .get_object_version(tenant_id, object_id)
            .map_err(|error| format!("failed to read durable object version: {error}"))?
        {
            match candidate.compare_replication_proof(&durable) {
                VersionProofComparison::Newer => {}
                VersionProofComparison::Older | VersionProofComparison::Exact => return Ok(false),
                VersionProofComparison::Ambiguous => {
                    if durable.origin_seq.is_none()
                        && durable.effect_digest.is_none()
                        && retained_legacy_oplog_proves_retry(
                            manager, tenant_id, object_id, source_seq, &durable, &effect,
                        )?
                    {
                        return Ok(false);
                    }
                    return Err(ambiguous_equal_origin_error(
                        tenant_id,
                        object_id,
                        effect.op_type(),
                        "durable proof is missing or conflicts with the incoming effect",
                    ));
                }
            }
        }

        self.pending_versions
            .insert(object_id.to_string(), candidate);
        Ok(true)
    }
}

fn retained_legacy_oplog_proves_retry(
    manager: &IndexManager,
    tenant_id: &str,
    object_id: &str,
    source_seq: u64,
    durable: &flapjack::index::version_store::VersionRecord,
    effect: &IncomingDocumentEffect<'_>,
) -> Result<bool, String> {
    let Some(oplog) = manager.get_oplog(tenant_id) else {
        return Ok(false);
    };
    let retained = oplog
        .read_since(durable.oplog_seq.saturating_sub(1))
        .map_err(|error| format!("failed to read retained oplog evidence: {error}"))?
        .into_iter()
        .find(|entry| entry.seq == durable.oplog_seq);
    let Some(retained) = retained else {
        return Ok(false);
    };
    let retained_origin_seq = flapjack::index::oplog::replication_origin_seq(&retained.payload)
        .map_err(|error| format!("invalid retained origin metadata: {error}"))?
        .unwrap_or(retained.seq);
    let retained_effect = match retained.op_type.as_str() {
        "upsert" => retained
            .payload
            .get("body")
            .and_then(|body| Document::from_json(body).ok())
            .map(|document| {
                let digest = flapjack::index::oplog::upsert_effect_digest(&document);
                (document.id, false, digest)
            }),
        "delete" => retained
            .payload
            .get("objectID")
            .and_then(serde_json::Value::as_str)
            .map(|retained_object_id| {
                (
                    retained_object_id.to_string(),
                    true,
                    flapjack::index::oplog::delete_effect_digest(retained_object_id),
                )
            }),
        _ => None,
    };

    Ok(retained_effect.is_some_and(
        |(retained_object_id, retained_tombstone, retained_digest)| {
            retained.tenant_id == tenant_id
                && retained.timestamp_ms == durable.timestamp_ms
                && retained.node_id == durable.node_id
                && retained_object_id == object_id
                && retained_tombstone == effect.is_tombstone()
                && retained.op_type == effect.op_type()
                && retained_origin_seq == source_seq
                && retained_digest == effect.digest(object_id)
        },
    ))
}

fn ambiguous_equal_origin_error(
    tenant_id: &str,
    object_id: &str,
    op_type: &str,
    detail: &str,
) -> String {
    format!(
        "[REPL {}] ambiguous equal-tuple {} for {}: {}",
        tenant_id, op_type, object_id, detail
    )
}

pub(crate) fn preflight_document_op(tenant_id: &str, op_entry: &OpLogEntry) -> Result<(), String> {
    source_sequence(op_entry)?;
    match op_entry.op_type.as_str() {
        "upsert" => {
            let body = op_entry.payload.get("body").ok_or_else(|| {
                format!(
                    "[REPL {}] upsert seq {} missing body field",
                    tenant_id, op_entry.seq
                )
            })?;
            Document::from_json(body).map_err(|error| {
                format!(
                    "[REPL {}] failed to parse upsert seq {}: {}",
                    tenant_id, op_entry.seq, error
                )
            })?;
            Ok(())
        }
        "delete" => op_entry
            .payload
            .get("objectID")
            .and_then(|value| value.as_str())
            .map(|_| ())
            .ok_or_else(|| {
                format!(
                    "[REPL {}] delete seq {} missing objectID field",
                    tenant_id, op_entry.seq
                )
            }),
        _ => unreachable!("document preflight only receives document operations"),
    }
}

fn source_sequence(op_entry: &OpLogEntry) -> Result<u64, String> {
    flapjack::index::oplog::replication_origin_seq(&op_entry.payload)
        .map_err(|error| {
            format!(
                "[REPL {}] invalid source sequence metadata at seq {}: {}",
                op_entry.tenant_id, op_entry.seq, error
            )
        })
        .map(|origin_seq| origin_seq.unwrap_or(op_entry.seq))
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
        return Err(format!(
            "[REPL {}] upsert seq {} missing body field",
            tenant_id, op_entry.seq
        ));
    };

    let doc = Document::from_json(body).map_err(|error| {
        format!(
            "[REPL {}] failed to parse upsert seq {}: {}",
            tenant_id, op_entry.seq, error
        )
    })?;
    let source_seq = source_sequence(op_entry)?;
    if !batch.accept_version(
        manager,
        tenant_id,
        &doc.id,
        source_seq,
        &incoming,
        IncomingDocumentEffect::Upsert(&doc),
    )? {
        tracing::debug!(
            "[REPL {}] skipping stale upsert for {}/{}",
            tenant_id,
            tenant_id,
            doc.id
        );
        return Ok(());
    }
    batch.final_op_type.insert(doc.id.to_string(), "upsert");
    batch.upserts.push((
        doc,
        ReplicatedWriteOrigin::new(incoming.0, incoming.1).with_origin_seq(source_seq),
    ));
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
        return Err(format!(
            "[REPL {}] delete seq {} missing objectID field",
            tenant_id, op_entry.seq
        ));
    };
    let source_seq = source_sequence(op_entry)?;

    if !batch.accept_version(
        manager,
        tenant_id,
        id,
        source_seq,
        &incoming,
        IncomingDocumentEffect::Delete,
    )? {
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
        ReplicatedWriteOrigin::new(incoming.0, incoming.1).with_origin_seq(source_seq),
    ));
    Ok(())
}

/// Resolve batch ordering, deduplicate upserts, and flush documents to the index.
///
/// When the same doc ID appears in both upserts and deletes within one batch,
/// only the operation with the newest origin tuple is applied; equal tuples use
/// the higher source sequence from this invocation. Upserts are further
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
        let task = manager
            .add_documents_for_replication_with_origins(tenant_id, batch.upserts)
            .map_err(|error| format!("add_documents failed: {error}"))?;
        wait_for_durable_replication_task(manager, tenant_id, "add_documents", &task.id).await?;
    }

    if !batch.deletes.is_empty() {
        let task = manager
            .delete_documents_for_replication_with_origins(tenant_id, batch.deletes)
            .map_err(|error| format!("delete_documents failed: {error}"))?;
        wait_for_durable_replication_task(manager, tenant_id, "delete_documents", &task.id).await?;
    }

    Ok(())
}
