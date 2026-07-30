#[cfg(feature = "vector-search")]
use std::collections::HashMap;

#[cfg(feature = "vector-search")]
use std::sync::Arc;

#[cfg(feature = "vector-search")]
use super::{PreparedWriteDocument, PreparedWriteOperation, WriteFinalizationContext};

/// Process all vector embeddings for a write batch: collect user-supplied vectors,
/// generate auto-embeddings via configured embedders, store results in the
/// VectorIndex, apply deletes, and inject computed vectors back into document JSON.
#[cfg(feature = "vector-search")]
pub(super) async fn process_vectors_for_write_op(
    context: &WriteFinalizationContext<'_>,
    prepared: &mut PreparedWriteOperation,
) {
    let mut computed_vectors = ComputedVectors::new();
    for (embedder_name, config) in context.embedder_configs {
        let (mut vectors_to_store, docs_needing_embed) =
            collect_embedder_workload(prepared, embedder_name, config);
        if let Some(generated_vectors) = generate_embedder_vectors(
            context.tenant_id,
            embedder_name,
            config,
            &docs_needing_embed,
        )
        .await
        {
            vectors_to_store.extend(generated_vectors);
        }

        // Keep any user-supplied vectors for this embedder even if auto-embedding
        // failed for other documents in the same batch.
        if vectors_to_store.is_empty() {
            continue;
        }

        record_computed_vectors(&mut computed_vectors, embedder_name, &vectors_to_store);
        if apply_vectors_to_index(
            &context.vector_ctx.vector_indices,
            context.tenant_id,
            embedder_name,
            &vectors_to_store,
        ) {
            prepared.vectors_modified = true;
        }
    }

    if apply_vector_deletes(
        &context.vector_ctx.vector_indices,
        context.tenant_id,
        &prepared.deleted_ids,
    ) {
        prepared.vectors_modified = true;
    }
    inject_computed_vectors(
        &mut prepared.valid_docs,
        &mut prepared.oplog_ops,
        &computed_vectors,
    );
}

#[cfg(feature = "vector-search")]
type StoredVectors = Vec<(String, Vec<f32>)>;

#[cfg(feature = "vector-search")]
type PendingEmbedDocuments = Vec<(String, String)>;

#[cfg(feature = "vector-search")]
type ComputedVectors = HashMap<String, HashMap<String, Vec<f32>>>;

/// Partition documents into user-supplied vectors ready for storage and documents
/// needing auto-embedding via the configured embedder template.
#[cfg(feature = "vector-search")]
fn collect_embedder_workload(
    prepared: &PreparedWriteOperation,
    embedder_name: &str,
    config: &crate::vector::config::EmbedderConfig,
) -> (StoredVectors, PendingEmbedDocuments) {
    use crate::vector::config::EmbedderSource;

    let template = config.document_template();
    let mut vectors_to_store = Vec::new();
    let mut docs_needing_embed = Vec::new();
    for ((doc_id, doc_json, _), user_vectors) in
        prepared.valid_docs.iter().zip(prepared.doc_vectors.iter())
    {
        if let Some(user_vectors) = user_vectors {
            if let Some(vector) = user_vectors.get(embedder_name) {
                vectors_to_store.push((doc_id.clone(), vector.clone()));
                continue;
            }
        }
        if config.source == EmbedderSource::UserProvided {
            continue;
        }
        docs_needing_embed.push((doc_id.clone(), template.render(doc_json)));
    }
    (vectors_to_store, docs_needing_embed)
}

/// Create an embedder from config and generate vector embeddings for documents
/// lacking user-supplied vectors. Returns `None` on embedder creation or
/// embedding failure (logged as warnings, does not block the Tantivy commit).
#[cfg(feature = "vector-search")]
async fn generate_embedder_vectors(
    tenant_id: &str,
    embedder_name: &str,
    config: &crate::vector::config::EmbedderConfig,
    docs_needing_embed: &[(String, String)],
) -> Option<StoredVectors> {
    use crate::vector::config::EmbedderSource;

    if docs_needing_embed.is_empty() || config.source == EmbedderSource::UserProvided {
        return Some(Vec::new());
    }

    let embedder = create_embedder_with_logging(tenant_id, embedder_name, config)?;
    let texts: Vec<&str> = docs_needing_embed
        .iter()
        .map(|(_, text)| text.as_str())
        .collect();
    let embeddings =
        embed_documents_with_logging(tenant_id, embedder_name, &embedder, &texts).await?;

    Some(
        docs_needing_embed
            .iter()
            .zip(embeddings)
            .map(|((doc_id, _), vector)| (doc_id.clone(), vector))
            .collect(),
    )
}

/// Instantiate an embedder from config, logging a warning and returning `None`
/// on failure instead of propagating the error.
#[cfg(feature = "vector-search")]
fn create_embedder_with_logging(
    tenant_id: &str,
    embedder_name: &str,
    config: &crate::vector::config::EmbedderConfig,
) -> Option<crate::vector::embedder::Embedder> {
    match crate::vector::embedder::create_embedder(config) {
        Ok(embedder) => Some(embedder),
        Err(error) => {
            tracing::warn!(
                "[WQ {}] failed to create embedder '{}': {}",
                tenant_id,
                embedder_name,
                error
            );
            None
        }
    }
}

/// Embed documents via the embedder, chunking into batches of 50 when the input
/// exceeds 100 documents. Returns `None` on failure.
#[cfg(feature = "vector-search")]
async fn embed_documents_with_logging(
    tenant_id: &str,
    embedder_name: &str,
    embedder: &crate::vector::embedder::Embedder,
    texts: &[&str],
) -> Option<Vec<Vec<f32>>> {
    if texts.len() > 100 {
        return embed_documents_in_chunks(tenant_id, embedder_name, embedder, texts).await;
    }

    match embedder.embed_documents(texts).await {
        Ok(vectors) => Some(vectors),
        Err(error) => {
            tracing::warn!(
                "[WQ {}] embedding failed for '{}': {}",
                tenant_id,
                embedder_name,
                error
            );
            None
        }
    }
}

/// Split a large text batch into chunks of 50 and embed each sequentially,
/// aborting on first failure.
#[cfg(feature = "vector-search")]
async fn embed_documents_in_chunks(
    tenant_id: &str,
    embedder_name: &str,
    embedder: &crate::vector::embedder::Embedder,
    texts: &[&str],
) -> Option<Vec<Vec<f32>>> {
    let mut all_vectors = Vec::new();
    for chunk in texts.chunks(50) {
        match embedder.embed_documents(chunk).await {
            Ok(batch_vectors) => all_vectors.extend(batch_vectors),
            Err(error) => {
                tracing::warn!(
                    "[WQ {}] embedding sub-batch failed for '{}': {}",
                    tenant_id,
                    embedder_name,
                    error
                );
                return None;
            }
        }
    }
    Some(all_vectors)
}

#[cfg(feature = "vector-search")]
fn record_computed_vectors(
    computed_vectors: &mut ComputedVectors,
    embedder_name: &str,
    vectors_to_store: &[(String, Vec<f32>)],
) {
    for (doc_id, vector) in vectors_to_store {
        computed_vectors
            .entry(doc_id.clone())
            .or_default()
            .insert(embedder_name.to_string(), vector.clone());
    }
}

/// Write vector embeddings into the tenant's VectorIndex, creating the index
/// if it does not exist. Returns `true` if any vectors were successfully added.
#[cfg(feature = "vector-search")]
fn apply_vectors_to_index(
    vector_indices: &Arc<
        dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>,
    >,
    tenant_id: &str,
    embedder_name: &str,
    vectors_to_store: &[(String, Vec<f32>)],
) -> bool {
    let first_dim = vectors_to_store[0].1.len();
    let vector_index =
        super::finalization::get_or_create_vector_index(vector_indices, tenant_id, first_dim);
    let write_result = vector_index.write();
    let Ok(mut guard) = write_result else {
        tracing::error!(
            "[WQ {}] VectorIndex write lock poisoned for embedder '{}'",
            tenant_id,
            embedder_name
        );
        return false;
    };

    let mut vectors_modified = false;
    for (doc_id, vector) in vectors_to_store {
        match guard.add(doc_id, vector) {
            Ok(()) => vectors_modified = true,
            Err(error) => tracing::warn!(
                "[WQ {}] failed to add vector for '{}': {}",
                tenant_id,
                doc_id,
                error
            ),
        }
    }
    vectors_modified
}

/// Remove vector entries for deleted document IDs from the tenant's VectorIndex.
/// Returns `true` if any vectors were removed.
#[cfg(feature = "vector-search")]
fn apply_vector_deletes(
    vector_indices: &Arc<
        dashmap::DashMap<String, Arc<std::sync::RwLock<crate::vector::index::VectorIndex>>>,
    >,
    tenant_id: &str,
    deleted_ids: &[String],
) -> bool {
    let Some(vector_index) = vector_indices.get(tenant_id) else {
        return false;
    };
    let write_result = vector_index.write();
    let Ok(mut guard) = write_result else {
        tracing::error!(
            "[WQ {}] VectorIndex write lock poisoned for delete",
            tenant_id
        );
        return false;
    };

    let mut vectors_modified = false;
    for deleted_id in deleted_ids {
        if guard.remove(deleted_id).is_ok() {
            vectors_modified = true;
        }
    }
    vectors_modified
}

/// Merge computed embedding vectors back into document JSON under the `_vectors`
/// key so they persist alongside the indexed document.
#[cfg(feature = "vector-search")]
fn inject_computed_vectors(
    valid_docs: &mut [PreparedWriteDocument],
    oplog_ops: &mut [crate::index::oplog::OpLogOperation],
    computed_vectors: &ComputedVectors,
) {
    if computed_vectors.is_empty() {
        return;
    }

    for (doc_id, doc_json, _) in valid_docs {
        let Some(embedder_vectors) = computed_vectors.get(doc_id.as_str()) else {
            continue;
        };
        inject_embedder_vectors(doc_json, embedder_vectors);
    }

    for operation in oplog_ops {
        if operation.op_type != "upsert" {
            continue;
        }
        let Some(doc_id) = operation
            .payload
            .get("objectID")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let Some(embedder_vectors) = computed_vectors.get(doc_id) else {
            continue;
        };
        if let Some(body) = operation.payload.get_mut("body") {
            inject_embedder_vectors(body, embedder_vectors);
        }
    }
}

#[cfg(feature = "vector-search")]
fn inject_embedder_vectors(
    doc_json: &mut serde_json::Value,
    embedder_vectors: &HashMap<String, Vec<f32>>,
) {
    let vectors_obj = doc_json
        .as_object_mut()
        .expect("prepared document JSON must be an object")
        .entry("_vectors")
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = vectors_obj.as_object_mut() {
        for (embedder_name, vector) in embedder_vectors {
            let json_vec: Vec<serde_json::Value> = vector
                .iter()
                .map(|&value| serde_json::Value::from(value as f64))
                .collect();
            obj.insert(embedder_name.clone(), serde_json::Value::Array(json_vec));
        }
    }
}
