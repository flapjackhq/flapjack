use super::*;
use flapjack::index::oplog::OpLogEntry;
use flapjack::IndexManager;
use std::path::Path;
use tempfile::TempDir;

/// Build an `OpLogEntry` with op_type `"upsert"` for use in tests.
///
/// # Arguments
///
/// * `seq` - Sequence number.
/// * `ts` - Timestamp in milliseconds (used for LWW conflict resolution).
/// * `node` - Originating node ID.
/// * `tenant` - Tenant/index name.
/// * `id` - Document object ID.
/// * `name` - Value for the `name` field in the document body.
fn make_upsert_op(seq: u64, ts: u64, node: &str, tenant: &str, id: &str, name: &str) -> OpLogEntry {
    OpLogEntry {
        seq,
        timestamp_ms: ts,
        node_id: node.to_string(),
        tenant_id: tenant.to_string(),
        op_type: "upsert".to_string(),
        payload: serde_json::json!({
            "objectID": id,
            "body": {"_id": id, "name": name}
        }),
    }
}

fn make_delete_op(seq: u64, ts: u64, node: &str, tenant: &str, id: &str) -> OpLogEntry {
    OpLogEntry {
        seq,
        timestamp_ms: ts,
        node_id: node.to_string(),
        tenant_id: tenant.to_string(),
        op_type: "delete".to_string(),
        payload: serde_json::json!({"objectID": id}),
    }
}

/// Build an `OpLogEntry` with an arbitrary op_type and payload for use in tests.
///
/// # Arguments
///
/// * `seq` - Sequence number.
/// * `ts` - Timestamp in milliseconds.
/// * `node` - Originating node ID.
/// * `tenant` - Tenant/index name.
/// * `op_type` - Operation type string (e.g. `"save_synonym"`, `"clear_index"`).
/// * `payload` - JSON payload for the operation.
fn make_index_op(
    seq: u64,
    ts: u64,
    node: &str,
    tenant: &str,
    op_type: &str,
    payload: serde_json::Value,
) -> OpLogEntry {
    OpLogEntry {
        seq,
        timestamp_ms: ts,
        node_id: node.to_string(),
        tenant_id: tenant.to_string(),
        op_type: op_type.to_string(),
        payload,
    }
}

async fn apply_single_index_op(
    manager: &IndexManager,
    seq: u64,
    op_type: &str,
    payload: serde_json::Value,
) {
    apply_ops_to_manager(
        manager,
        "t1",
        &[make_index_op(
            seq,
            seq * 1000,
            "node-a",
            "t1",
            op_type,
            payload,
        )],
    )
    .await
    .unwrap();
}

fn make_replication_batch_payload(
    flag_field: &str,
    flag_value: bool,
    entries_field: &str,
    entry: serde_json::Value,
) -> serde_json::Value {
    let mut payload = serde_json::Map::new();
    payload.insert(flag_field.to_string(), serde_json::Value::Bool(flag_value));
    payload.insert(
        entries_field.to_string(),
        serde_json::Value::Array(vec![entry]),
    );
    serde_json::Value::Object(payload)
}

#[tokio::test]
async fn rotate_admin_key_error_response_hides_storage_details() {
    let response = rotate_admin_key_error_response(&"keys.json: permission denied");
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let body_text = String::from_utf8(body.to_vec()).unwrap();

    assert_eq!(status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(json["message"], "Failed to rotate admin key");
    assert!(
        !body_text.contains("keys.json"),
        "raw persistence path must not be exposed in the response body: {body_text}"
    );
    assert!(
        !body_text.contains("permission denied"),
        "raw I/O details must not be exposed in the response body: {body_text}"
    );
}

struct BatchWrapperFlowSpec<'a> {
    manager: &'a IndexManager,
    store_path: &'a Path,
    batch_op_type: &'a str,
    delete_op_type: &'a str,
    clear_op_type: &'a str,
    replacement_flag_field: &'a str,
    entries_field: &'a str,
    initial_entry: serde_json::Value,
    replacement_entry: serde_json::Value,
    deleted_object_id: &'a str,
    restored_entry: serde_json::Value,
}

async fn assert_batch_wrapper_flow<AfterReplace, AfterDelete, AfterRestore>(
    spec: BatchWrapperFlowSpec<'_>,
    assert_after_replace: AfterReplace,
    assert_after_delete: AfterDelete,
    assert_after_restore: AfterRestore,
) where
    AfterReplace: Fn(&Path),
    AfterDelete: Fn(&Path),
    AfterRestore: Fn(&Path),
{
    let BatchWrapperFlowSpec {
        manager,
        store_path,
        batch_op_type,
        delete_op_type,
        clear_op_type,
        replacement_flag_field,
        entries_field,
        initial_entry,
        replacement_entry,
        deleted_object_id,
        restored_entry,
    } = spec;

    apply_single_index_op(
        manager,
        1,
        batch_op_type,
        make_replication_batch_payload(replacement_flag_field, false, entries_field, initial_entry),
    )
    .await;

    apply_single_index_op(
        manager,
        2,
        batch_op_type,
        make_replication_batch_payload(
            replacement_flag_field,
            true,
            entries_field,
            replacement_entry,
        ),
    )
    .await;
    assert_after_replace(store_path);

    apply_single_index_op(
        manager,
        3,
        delete_op_type,
        serde_json::json!({ "objectID": deleted_object_id }),
    )
    .await;
    assert_after_delete(store_path);

    apply_single_index_op(
        manager,
        4,
        batch_op_type,
        make_replication_batch_payload(
            replacement_flag_field,
            false,
            entries_field,
            restored_entry,
        ),
    )
    .await;
    assert_after_restore(store_path);

    apply_single_index_op(manager, 5, clear_op_type, serde_json::json!({})).await;
    assert!(
        !store_path.exists(),
        "{clear_op_type} should remove the replicated store file"
    );
}

/// Poll until a document exists in the index (up to ~2s).
/// Panics with a clear message if it never appears.
async fn wait_for_doc_exists(manager: &IndexManager, tenant: &str, doc_id: &str) {
    for _ in 0..200 {
        if let Ok(Some(_)) = manager.get_document(tenant, doc_id) {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("{}[{}] never appeared in index after 2s", tenant, doc_id);
}

/// Poll until a document's text field equals the expected value (up to ~2s).
/// Panics with a clear diff message if it never matches.
async fn wait_for_field(
    manager: &IndexManager,
    tenant: &str,
    doc_id: &str,
    field: &str,
    expected: &str,
) {
    for _ in 0..200 {
        if let Ok(Some(doc)) = manager.get_document(tenant, doc_id) {
            if matches!(doc.fields.get(field), Some(flapjack::types::FieldValue::Text(s)) if s == expected)
            {
                return;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    let got = manager
        .get_document(tenant, doc_id)
        .ok()
        .flatten()
        .and_then(|d| d.fields.get(field).cloned());
    panic!(
        "{}[{}].{} never became {:?}; last value: {:?}",
        tenant, doc_id, field, expected, got
    );
}

// ── Basic apply ──

#[tokio::test]
async fn apply_ops_upsert_creates_document() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![make_upsert_op(1, 1000, "node-a", "t1", "doc1", "Alice")];
    let result = apply_ops_to_manager(&manager, "t1", &ops).await;
    assert_eq!(result.unwrap(), 1);
    // Write queue is async — poll until committed
    wait_for_doc_exists(&manager, "t1", "doc1").await;
}

#[tokio::test]
async fn apply_ops_delete_removes_document() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    // Insert first and confirm it's visible before testing deletion
    let upsert = vec![make_upsert_op(1, 1000, "node-a", "t1", "doc1", "Alice")];
    apply_ops_to_manager(&manager, "t1", &upsert).await.unwrap();
    wait_for_doc_exists(&manager, "t1", "doc1").await;
    // Now delete — delete_documents_sync_for_replication is synchronous
    let del = vec![make_delete_op(2, 2000, "node-a", "t1", "doc1")];
    apply_ops_to_manager(&manager, "t1", &del).await.unwrap();
    let doc = manager.get_document("t1", "doc1").unwrap();
    assert!(doc.is_none(), "doc1 should be gone after delete");
}

#[tokio::test]
async fn apply_ops_returns_max_seq() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(3, 1000, "node-a", "t1", "d1", "Alice"),
        make_upsert_op(7, 2000, "node-a", "t1", "d2", "Bob"),
        make_upsert_op(5, 1500, "node-a", "t1", "d3", "Carol"),
    ];
    let result = apply_ops_to_manager(&manager, "t1", &ops).await.unwrap();
    assert_eq!(result, 7, "should return max seq across all ops");
}

/// Verify that a `clear_index` op with a path-traversal `index_name` like `"../victim"` is rejected by validation and does not delete external directories.
#[tokio::test]
async fn apply_ops_clear_index_rejects_path_traversal_name() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let victim_name = format!("replication-victim-{}", uuid::Uuid::new_v4());
    let victim_dir = tmp.path().parent().unwrap().join(&victim_name);
    std::fs::create_dir_all(&victim_dir).unwrap();
    std::fs::write(victim_dir.join("marker.txt"), "keep").unwrap();

    let op = OpLogEntry {
        seq: 1,
        timestamp_ms: 1,
        node_id: "node-a".to_string(),
        tenant_id: "t1".to_string(),
        op_type: "clear_index".to_string(),
        payload: serde_json::json!({
            "index_name": format!("../{}", victim_name)
        }),
    };

    let max_seq = apply_ops_to_manager(&manager, "t1", &[op]).await.unwrap();
    assert_eq!(max_seq, 1);
    assert!(
        victim_dir.exists(),
        "clear_index with traversal name must not touch external directory"
    );
}

// ── LWW: newer timestamp wins ──

/// Verify that a newer upsert (higher timestamp) wins over an older upsert for the same document via LWW conflict resolution.
#[tokio::test]
async fn lww_newer_timestamp_overwrites_older() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Apply op at ts=2000 first — poll until it's visible
    let op_newer = vec![make_upsert_op(
        1,
        2000,
        "node-a",
        "t1",
        "doc1",
        "NewerAlice",
    )];
    apply_ops_to_manager(&manager, "t1", &op_newer)
        .await
        .unwrap();
    wait_for_field(&manager, "t1", "doc1", "name", "NewerAlice").await;

    // Apply op at ts=1000 (older) — REJECTED by LWW immediately, no async work
    let op_older = vec![make_upsert_op(
        2,
        1000,
        "node-b",
        "t1",
        "doc1",
        "OlderAlice",
    )];
    apply_ops_to_manager(&manager, "t1", &op_older)
        .await
        .unwrap();

    let doc = manager.get_document("t1", "doc1").unwrap().unwrap();
    let name = doc.fields.get("name");
    assert!(
        matches!(name, Some(flapjack::types::FieldValue::Text(s)) if s == "NewerAlice"),
        "newer write should win; got: {:?}",
        doc.fields.get("name")
    );
}

/// Verify that when a batch contains both a newer and an older upsert for the same document, only the newer version is persisted.
#[tokio::test]
async fn lww_older_upsert_does_not_overwrite_newer() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Apply newer first, then try to apply older — both in one batch.
    // ts=5000 "Final" wins; ts=1000 "Stale" is deduped away before queuing.
    let ops = vec![
        make_upsert_op(1, 5000, "node-a", "t1", "doc1", "Final"),
        make_upsert_op(2, 1000, "node-b", "t1", "doc1", "Stale"),
    ];
    apply_ops_to_manager(&manager, "t1", &ops).await.unwrap();
    wait_for_field(&manager, "t1", "doc1", "name", "Final").await;

    let doc = manager.get_document("t1", "doc1").unwrap().unwrap();
    let name = doc.fields.get("name");
    assert!(
        matches!(name, Some(flapjack::types::FieldValue::Text(s)) if s == "Final"),
        "stale op should not overwrite newer; got: {:?}",
        doc.fields.get("name")
    );
}

// ── LWW: tie-break by node_id ──

/// Verify that when two upserts share the same timestamp, the one with the lexicographically higher node ID wins the LWW tie-break.
#[tokio::test]
async fn lww_same_timestamp_higher_node_id_wins() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Apply from "z-node" — poll until visible
    let op_z = vec![make_upsert_op(1, 1000, "z-node", "t1", "doc1", "ZNode")];
    apply_ops_to_manager(&manager, "t1", &op_z).await.unwrap();
    wait_for_field(&manager, "t1", "doc1", "name", "ZNode").await;

    // "a-node" at same ts=1000 — REJECTED (z > a lexicographically), no async work
    let op_a = vec![make_upsert_op(2, 1000, "a-node", "t1", "doc1", "ANode")];
    apply_ops_to_manager(&manager, "t1", &op_a).await.unwrap();

    let doc = manager.get_document("t1", "doc1").unwrap().unwrap();
    let name = doc.fields.get("name");
    assert!(
        matches!(name, Some(flapjack::types::FieldValue::Text(s)) if s == "ZNode"),
        "z-node (higher lexicographic) should win tie-break; got: {:?}",
        doc.fields.get("name")
    );
}

// ── LWW: stale delete is rejected ──

/// Verify that a delete with an older timestamp is rejected by LWW and does not remove a document written with a newer timestamp.
#[tokio::test]
async fn lww_stale_delete_does_not_remove_newer_upsert() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Write doc at ts=2000 — poll until visible
    let upsert = vec![make_upsert_op(1, 2000, "node-a", "t1", "doc1", "Alice")];
    apply_ops_to_manager(&manager, "t1", &upsert).await.unwrap();
    wait_for_doc_exists(&manager, "t1", "doc1").await;

    // Try to delete with stale ts=1000 — REJECTED immediately by LWW, no async work
    let del = vec![make_delete_op(2, 1000, "node-b", "t1", "doc1")];
    apply_ops_to_manager(&manager, "t1", &del).await.unwrap();

    let doc = manager.get_document("t1", "doc1").unwrap();
    assert!(doc.is_some(), "stale delete should not remove a newer doc");
}

// ── LWW: same-node ops always apply in sequence ──

/// Verify that sequential upserts from the same node with increasing timestamps are all applied in order.
#[tokio::test]
async fn lww_same_node_sequential_ops_always_apply() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // V1 first — poll until visible
    let op1 = vec![make_upsert_op(1, 1000, "node-a", "t1", "doc1", "V1")];
    apply_ops_to_manager(&manager, "t1", &op1).await.unwrap();
    wait_for_field(&manager, "t1", "doc1", "name", "V1").await;

    // V2 newer timestamp — accepted, poll until visible
    let op2 = vec![make_upsert_op(2, 2000, "node-a", "t1", "doc1", "V2")];
    apply_ops_to_manager(&manager, "t1", &op2).await.unwrap();
    wait_for_field(&manager, "t1", "doc1", "name", "V2").await;

    let doc = manager.get_document("t1", "doc1").unwrap().unwrap();
    let name = doc.fields.get("name");
    assert!(
        matches!(name, Some(flapjack::types::FieldValue::Text(s)) if s == "V2"),
        "sequential ops from same node should apply in order; got: {:?}",
        doc.fields.get("name")
    );
}

// ── LWW: primary write blocks stale replicated op ──
// This test validates the fix for the "known limitation" from session 23:
// primary-written docs must populate lww_map so stale replicated ops are rejected.

/// Verify that a document written via the primary path populates the LWW map, causing a stale replicated upsert with an older timestamp to be rejected.
#[tokio::test]
async fn lww_primary_write_blocks_stale_replicated_op() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Write a doc via the primary path (add_documents_sync — goes through write_queue)
    let doc = flapjack::types::Document {
        id: "doc1".to_string(),
        fields: {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "name".to_string(),
                flapjack::types::FieldValue::Text("Primary".to_string()),
            );
            m
        },
    };
    manager.create_tenant("t1").unwrap();
    manager.add_documents_sync("t1", vec![doc]).await.unwrap();

    // Confirm lww_map was populated by the write_queue
    let lww = manager.get_lww("t1", "doc1");
    assert!(
        lww.is_some(),
        "primary write must populate lww_map; got None"
    );
    let (primary_ts, _) = lww.unwrap();
    assert!(
        primary_ts > 0,
        "primary_ts should be a real system timestamp"
    );

    // Now try to replicate a stale op with ts=1 (much older than primary write).
    // LWW rejects this before queuing — no async work, result is immediately visible.
    let stale_op = vec![make_upsert_op(99, 1, "remote-node", "t1", "doc1", "Stale")];
    apply_ops_to_manager(&manager, "t1", &stale_op)
        .await
        .unwrap();

    // The stale replicated op must NOT overwrite the primary write
    let fetched = manager.get_document("t1", "doc1").unwrap().unwrap();
    let name = fetched.fields.get("name");
    assert!(
        matches!(name, Some(flapjack::types::FieldValue::Text(s)) if s == "Primary"),
        "stale replicated op must not overwrite primary write; got: {:?}",
        name
    );
}

// ── LWW: primary delete blocks stale replicated upsert ──

/// Verify that a primary-path delete populates the LWW map, preventing a stale replicated upsert from reviving the deleted document.
#[tokio::test]
async fn lww_primary_delete_blocks_stale_replicated_upsert() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // First write the doc via primary path
    let doc = flapjack::types::Document {
        id: "doc1".to_string(),
        fields: {
            let mut m = std::collections::HashMap::new();
            m.insert(
                "name".to_string(),
                flapjack::types::FieldValue::Text("Primary".to_string()),
            );
            m
        },
    };
    manager.create_tenant("t1").unwrap();
    manager.add_documents_sync("t1", vec![doc]).await.unwrap();

    // Delete via primary path
    manager
        .delete_documents_sync("t1", vec!["doc1".to_string()])
        .await
        .unwrap();

    // Confirm lww_map records the delete timestamp
    let lww = manager.get_lww("t1", "doc1");
    assert!(lww.is_some(), "primary delete must populate lww_map");

    // Now try to replicate a stale upsert with ts=1 — REJECTED by LWW immediately.
    // No async work queued; result is visible without waiting.
    let stale_upsert = vec![make_upsert_op(
        99,
        1,
        "remote-node",
        "t1",
        "doc1",
        "StaleRevive",
    )];
    apply_ops_to_manager(&manager, "t1", &stale_upsert)
        .await
        .unwrap();

    let doc = manager.get_document("t1", "doc1").unwrap();
    assert!(
        doc.is_none(),
        "stale replicated upsert must not revive a primary-deleted doc"
    );
}

// ── LWW: lww_map rebuilt from oplog on restart (P3) ──
// Without P3: after restart lww_map is empty → stale replicated ops bypass LWW
// With P3:    recover_from_oplog rebuilds lww_map → stale ops correctly rejected

/// Verify that after a crash-style restart, the LWW map is rebuilt from the oplog so a stale replicated upsert with an older timestamp is correctly rejected.
#[tokio::test]
async fn lww_map_rebuilt_from_oplog_blocks_stale_op_after_restart() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    // PHASE 1: Primary write — establishes LWW state in oplog
    let primary_ts;
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("t_restart").unwrap();
        let doc = flapjack::types::Document {
            id: "doc1".to_string(),
            fields: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("Original".to_string()),
                );
                m
            },
        };
        manager
            .add_documents_sync("t_restart", vec![doc])
            .await
            .unwrap();

        // Capture oplog timestamp — this is what LWW must be rebuilt from
        let oplog = manager.get_or_create_oplog("t_restart").unwrap();
        let ops = oplog.read_since(0).unwrap();
        let upsert_op = ops
            .iter()
            .find(|o| o.op_type == "upsert")
            .expect("should have upsert in oplog after primary write");
        primary_ts = upsert_op.timestamp_ms;
        assert!(primary_ts > 0, "oplog should record a real timestamp");

        manager.graceful_shutdown().await;
    }

    // PHASE 2: Restart (new IndexManager = fresh empty lww_map until P3 fix)
    {
        let manager = IndexManager::new(&base);

        // Try to apply a stale replicated op (1ms before the primary write).
        // With P3: lww_map rebuilt from oplog → REJECTED immediately.
        // Without P3: would be accepted (queued async) — we poll briefly to detect that case.
        let stale_op = vec![make_upsert_op(
            99,
            primary_ts.saturating_sub(1),
            "remote-node",
            "t_restart",
            "doc1",
            "StaleOverwrite",
        )];
        apply_ops_to_manager(&manager, "t_restart", &stale_op)
            .await
            .unwrap();

        // Poll briefly — if P3 is broken the write queue would commit "StaleOverwrite"
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // P3: lww_map rebuilt from oplog → stale op rejected → "Original" survives
        let fetched = manager.get_document("t_restart", "doc1").unwrap();
        assert!(
            fetched.is_some(),
            "doc1 must exist (was written by primary)"
        );
        let name = fetched.unwrap().fields.get("name").cloned();
        assert!(
            matches!(&name, Some(flapjack::types::FieldValue::Text(s)) if s == "Original"),
            "stale replicated op must not overwrite after restart; lww_map must be rebuilt from oplog. got: {:?}",
            name
        );

        manager.graceful_shutdown().await;
    }
}

// ── LWW: lww_map rebuilt for normal restart (no uncommitted ops) ──
// Covers the case where committed_seq is current (normal shutdown, not crash).
// recover_from_oplog must still rebuild lww_map even when there's nothing to replay.

/// Verify that after a clean shutdown (no uncommitted ops), restarting rebuilds the LWW map from the oplog so stale replicated ops are still rejected.
#[tokio::test]
async fn lww_map_rebuilt_on_normal_restart_no_uncommitted_ops() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let primary_ts;
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("t_normal_restart").unwrap();
        let doc = flapjack::types::Document {
            id: "docA".to_string(),
            fields: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("Persisted".to_string()),
                );
                m
            },
        };
        manager
            .add_documents_sync("t_normal_restart", vec![doc])
            .await
            .unwrap();

        let oplog = manager.get_or_create_oplog("t_normal_restart").unwrap();
        let ops = oplog.read_since(0).unwrap();
        primary_ts = ops
            .iter()
            .find(|o| o.op_type == "upsert")
            .map(|o| o.timestamp_ms)
            .unwrap_or(0);
        assert!(primary_ts > 0);

        // Normal clean shutdown: committed_seq is updated, no uncommitted ops
        manager.graceful_shutdown().await;
    }

    // Restart: committed_seq is current → no document replay needed.
    // But lww_map must still be rebuilt so stale ops are rejected.
    {
        let manager = IndexManager::new(&base);

        let stale_op = vec![make_upsert_op(
            99,
            primary_ts.saturating_sub(1),
            "remote-node",
            "t_normal_restart",
            "docA",
            "ShouldBeRejected",
        )];
        apply_ops_to_manager(&manager, "t_normal_restart", &stale_op)
            .await
            .unwrap();

        // Stale upsert is rejected by LWW (P3 correct). If P3 were broken the write
        // queue would commit "ShouldBeRejected" asynchronously — wait briefly to detect that.
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let fetched = manager.get_document("t_normal_restart", "docA").unwrap();
        assert!(fetched.is_some());
        let name = fetched.unwrap().fields.get("name").cloned();
        assert!(
            matches!(&name, Some(flapjack::types::FieldValue::Text(s)) if s == "Persisted"),
            "stale op must be rejected even after clean shutdown restart; got: {:?}",
            name
        );

        manager.graceful_shutdown().await;
    }
}

/// Verify that restart rebuilds LWW state before any replay early-return path,
/// so a tenant with current committed_seq still has in-memory LWW entries.
#[tokio::test]
async fn lww_map_rebuilt_populates_state_when_committed_seq_is_current() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let (primary_ts, primary_node_id);
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("t_lww_populate").unwrap();
        let doc = flapjack::types::Document {
            id: "doc1".to_string(),
            fields: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("Persisted".to_string()),
                );
                m
            },
        };
        manager
            .add_documents_sync("t_lww_populate", vec![doc])
            .await
            .unwrap();

        let oplog = manager.get_or_create_oplog("t_lww_populate").unwrap();
        let upsert = oplog
            .read_since(0)
            .unwrap()
            .into_iter()
            .find(|entry| entry.op_type == "upsert")
            .expect("expected upsert in oplog");
        primary_ts = upsert.timestamp_ms;
        primary_node_id = upsert.node_id;

        manager.graceful_shutdown().await;
    }

    {
        let manager = IndexManager::new(&base);
        let _ = manager.get_document("t_lww_populate", "doc1").unwrap();

        let rebuilt = manager
            .get_lww("t_lww_populate", "doc1")
            .expect("lww_map should be rebuilt on restart");
        assert_eq!(rebuilt.0, primary_ts);
        assert_eq!(rebuilt.1, primary_node_id);

        manager.graceful_shutdown().await;
    }
}

// ── LWW: lww_map rebuild blocks stale DELETE after restart (P3) ──
// Variant of the P3 crash test but with a stale DELETE instead of a stale UPSERT.
// A stale replicated delete arriving after restart must NOT remove a newer primary write.

/// Verify that after restart the rebuilt LWW map blocks a stale replicated delete from removing a document that was written with a newer timestamp before shutdown.
#[tokio::test]
async fn lww_map_rebuilt_from_oplog_blocks_stale_delete_after_restart() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let primary_ts;
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("t_del_restart").unwrap();
        let doc = flapjack::types::Document {
            id: "doc1".to_string(),
            fields: {
                let mut m = std::collections::HashMap::new();
                m.insert(
                    "name".to_string(),
                    flapjack::types::FieldValue::Text("ShouldSurvive".to_string()),
                );
                m
            },
        };
        manager
            .add_documents_sync("t_del_restart", vec![doc])
            .await
            .unwrap();

        let oplog = manager.get_or_create_oplog("t_del_restart").unwrap();
        let ops = oplog.read_since(0).unwrap();
        primary_ts = ops
            .iter()
            .find(|o| o.op_type == "upsert")
            .map(|o| o.timestamp_ms)
            .expect("should have upsert in oplog");
        assert!(primary_ts > 0);

        manager.graceful_shutdown().await;
    }

    // Restart: lww_map is rebuilt from oplog → stale delete ts=primary_ts-1 must be rejected
    {
        let manager = IndexManager::new(&base);

        let stale_delete = vec![make_delete_op(
            99,
            primary_ts.saturating_sub(1),
            "remote-node",
            "t_del_restart",
            "doc1",
        )];
        apply_ops_to_manager(&manager, "t_del_restart", &stale_delete)
            .await
            .unwrap();

        // Stale delete is rejected by LWW (P3 correct). If P3 were broken, the delete
        // runs synchronously via delete_documents_sync_for_replication (also .awaited),
        // so the outcome is committed before apply_ops_to_manager returns — no sleep needed.
        let fetched = manager.get_document("t_del_restart", "doc1").unwrap();
        assert!(
            fetched.is_some(),
            "stale delete must not remove doc after restart; lww_map must be rebuilt from oplog"
        );

        manager.graceful_shutdown().await;
    }
}

// ── Batch ordering: upsert→delete→re-upsert in a single batch ──
// Regression test: apply_ops_to_manager used to split ops into separate
// upserts and deletes lists, applying all upserts first then all deletes.
// This caused a later re-upsert (ts=3000) to be overridden by an earlier
// delete (ts=2000) because the delete was applied after the upsert.

/// Verify that when a single batch contains upsert, delete, then re-upsert for the same document, the final upsert (highest timestamp) wins and the document is kept.
#[tokio::test]
async fn batch_upsert_delete_reupsert_same_doc_keeps_final_upsert() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Single batch: create → delete → re-create the SAME doc
    let ops = vec![
        make_upsert_op(1, 1000, "node-a", "t1", "doc1", "Version1"),
        make_delete_op(2, 2000, "node-a", "t1", "doc1"),
        make_upsert_op(3, 3000, "node-a", "t1", "doc1", "Version3"),
    ];
    let result = apply_ops_to_manager(&manager, "t1", &ops).await;
    assert_eq!(result.unwrap(), 3);

    // Wait for write queue to commit — the ts=3000 re-upsert must win over the ts=2000 delete
    wait_for_field(&manager, "t1", "doc1", "name", "Version3").await;
}

/// Verify that when a single batch contains an upsert followed by a delete for the same document, the delete wins and the document is removed.
#[tokio::test]
async fn batch_upsert_then_delete_same_doc_deletes() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Single batch: create → delete the SAME doc (delete is final)
    let ops = vec![
        make_upsert_op(1, 1000, "node-a", "t1", "doc1", "ToDelete"),
        make_delete_op(2, 2000, "node-a", "t1", "doc1"),
    ];
    apply_ops_to_manager(&manager, "t1", &ops).await.unwrap();

    // The ts=2000 delete wins: the upsert is filtered from the batch, and the delete
    // runs synchronously via delete_documents_sync_for_replication. No sleep needed.
    let doc = manager.get_document("t1", "doc1").unwrap();
    assert!(
        doc.is_none(),
        "doc1 should be deleted — the ts=2000 delete is the final op"
    );
}

/// Verify that a replicated `save_synonym` op creates `synonyms.json` on disk with the expected synonym entry.
#[tokio::test]
async fn apply_ops_save_synonym_creates_synonyms_file() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let op = make_index_op(
        1,
        1000,
        "node-a",
        "t1",
        "save_synonym",
        serde_json::json!({
            "objectID": "syn-copy",
            "type": "synonym",
            "synonyms": ["tv", "television"]
        }),
    );
    let result = apply_ops_to_manager(&manager, "t1", &[op]).await;
    assert_eq!(result.unwrap(), 1);

    let synonyms_path = tmp.path().join("t1").join("synonyms.json");
    assert!(synonyms_path.exists(), "synonyms.json should be created");
    let store = flapjack::index::synonyms::SynonymStore::load(&synonyms_path).unwrap();
    assert!(
        store.get("syn-copy").is_some(),
        "replicated save_synonym should persist synonym entry"
    );
}

/// Verify that replicated synonym batch, delete, and clear ops preserve the expected store contents on disk.
#[tokio::test]
async fn apply_ops_save_delete_and_clear_synonym_batches() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let synonyms_path = tmp.path().join("t1").join("synonyms.json");

    assert_batch_wrapper_flow(
        BatchWrapperFlowSpec {
            manager: &manager,
            store_path: &synonyms_path,
            batch_op_type: "save_synonyms",
            delete_op_type: "delete_synonym",
            clear_op_type: "clear_synonyms",
            replacement_flag_field: "replace",
            entries_field: "synonyms",
            initial_entry: serde_json::json!({
                "objectID": "syn-old",
                "type": "synonym",
                "synonyms": ["tv", "television"]
            }),
            replacement_entry: serde_json::json!({
                "objectID": "syn-new",
                "type": "synonym",
                "synonyms": ["phone", "telephone"]
            }),
            deleted_object_id: "syn-new",
            restored_entry: serde_json::json!({
                "objectID": "syn-clear",
                "type": "synonym",
                "synonyms": ["notebook", "laptop"]
            }),
        },
        |path| {
            let store = flapjack::index::synonyms::SynonymStore::load(path).unwrap();
            assert!(
                store.get("syn-old").is_none(),
                "`replace: true` should discard previously replicated synonyms"
            );
            assert!(
                store.get("syn-new").is_some(),
                "batch save should persist the replacement synonym"
            );
        },
        |path| {
            let store = flapjack::index::synonyms::SynonymStore::load(path).unwrap();
            assert!(
                store.get("syn-new").is_none(),
                "delete_synonym should remove the targeted synonym from disk"
            );
        },
        |path| {
            let store = flapjack::index::synonyms::SynonymStore::load(path).unwrap();
            assert!(
                store.get("syn-clear").is_some(),
                "restore batch should recreate the synonym before clear_synonyms runs"
            );
        },
    )
    .await;
}

/// Verify that a replicated `save_rule` op creates `rules.json` on disk with the expected rule entry.
#[tokio::test]
async fn apply_ops_save_rule_creates_rules_file() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let op = make_index_op(
        1,
        1000,
        "node-a",
        "t1",
        "save_rule",
        serde_json::json!({
            "objectID": "rule-copy",
            "conditions": [{"anchoring": "contains", "pattern": "laptop"}],
            "consequence": {"params": {"query": "laptop computer"}}
        }),
    );
    let result = apply_ops_to_manager(&manager, "t1", &[op]).await;
    assert_eq!(result.unwrap(), 1);

    let rules_path = tmp.path().join("t1").join("rules.json");
    assert!(rules_path.exists(), "rules.json should be created");
    let store = flapjack::index::rules::RuleStore::load(&rules_path).unwrap();
    assert!(
        store.get("rule-copy").is_some(),
        "replicated save_rule should persist rule entry"
    );
}

/// Verify that replicated rule batch, delete, and clear ops preserve the expected store contents on disk.
#[tokio::test]
async fn apply_ops_save_delete_and_clear_rule_batches() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let rules_path = tmp.path().join("t1").join("rules.json");

    assert_batch_wrapper_flow(
        BatchWrapperFlowSpec {
            manager: &manager,
            store_path: &rules_path,
            batch_op_type: "save_rules",
            delete_op_type: "delete_rule",
            clear_op_type: "clear_rules",
            replacement_flag_field: "clearExisting",
            entries_field: "rules",
            initial_entry: serde_json::json!({
                "objectID": "rule-old",
                "conditions": [{"anchoring": "contains", "pattern": "tv"}],
                "consequence": {"params": {"query": "television"}}
            }),
            replacement_entry: serde_json::json!({
                "objectID": "rule-new",
                "conditions": [{"anchoring": "contains", "pattern": "phone"}],
                "consequence": {"params": {"query": "telephone"}}
            }),
            deleted_object_id: "rule-new",
            restored_entry: serde_json::json!({
                "objectID": "rule-clear",
                "conditions": [{"anchoring": "contains", "pattern": "notebook"}],
                "consequence": {"params": {"query": "laptop"}}
            }),
        },
        |path| {
            let store = flapjack::index::rules::RuleStore::load(path).unwrap();
            assert!(
                store.get("rule-old").is_none(),
                "`clearExisting: true` should discard previously replicated rules"
            );
            assert!(
                store.get("rule-new").is_some(),
                "batch save should persist the replacement rule"
            );
        },
        |path| {
            let store = flapjack::index::rules::RuleStore::load(path).unwrap();
            assert!(
                store.get("rule-new").is_none(),
                "delete_rule should remove the targeted rule from disk"
            );
        },
        |path| {
            let store = flapjack::index::rules::RuleStore::load(path).unwrap();
            assert!(
                store.get("rule-clear").is_some(),
                "restore batch should recreate the rule before clear_rules runs"
            );
        },
    )
    .await;
}

/// Verify that `apply_ops_to_manager` returns an error when the tenant ID contains path traversal characters like `"../evil"`.
#[tokio::test]
async fn apply_ops_rejects_invalid_tenant_id() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let op = make_index_op(
        1,
        1000,
        "node-a",
        "../evil",
        "clear_synonyms",
        serde_json::json!({}),
    );
    let result = apply_ops_to_manager(&manager, "../evil", &[op]).await;
    assert!(
        result.is_err(),
        "invalid tenant_id should be rejected before applying ops"
    );
}

// ── Unknown op type skipped gracefully ──

#[tokio::test]
async fn apply_ops_unknown_type_skipped() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let op = OpLogEntry {
        seq: 1,
        timestamp_ms: 1000,
        node_id: "node-a".to_string(),
        tenant_id: "t1".to_string(),
        op_type: "noop_unknown".to_string(),
        payload: serde_json::json!({}),
    };
    // Should not panic, just skip
    let result = apply_ops_to_manager(&manager, "t1", &[op]).await;
    assert_eq!(result.unwrap(), 1);
}

// ── /internal/storage endpoint tests ──

use crate::test_helpers::{EnvVarRestoreGuard, TestStateBuilder, ENV_MUTEX};
use axum::body::Body;
use axum::http::Request;
use axum::routing::{delete, get, post};
use axum::Router;
use tower::ServiceExt;

fn internal_replication_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/internal/replicate", post(super::replicate_ops))
        .route("/internal/ops", get(super::get_ops))
        .route("/internal/tenants", get(super::list_tenants))
        .with_state(state)
}

/// Verify that POST `/internal/replicate` keeps the HTTP 200 JSON success envelope
/// after switching from an explicit `(StatusCode, Json(...))` response to `Ok(Json(...))`.
#[tokio::test]
async fn replicate_ops_returns_json_success_payload() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                        tenant_id: "products".to_string(),
                        ops: vec![make_upsert_op(
                            7, 1_000, "node-a", "products", "doc1", "Alpha",
                        )],
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["tenant_id"], "products");
    assert_eq!(json["acked_seq"], 7);

    wait_for_doc_exists(&state.manager, "products", "doc1").await;
}

/// Verify that malformed tenant IDs in POST `/internal/replicate` stay client-visible
/// 400s instead of being collapsed into sanitized 500s.
#[tokio::test]
async fn replicate_ops_invalid_tenant_returns_standard_400_json() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                        tenant_id: "../evil".to_string(),
                        ops: Vec::new(),
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], 400);
    assert_eq!(
        json["message"],
        "Index name contains invalid characters (path traversal not allowed)"
    );
}

/// Verify that GET `/internal/ops` keeps the standard `{message,status}` 404 body
/// when the tenant oplog does not exist.
#[tokio::test]
async fn get_ops_missing_tenant_returns_standard_404_json() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=missing&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], 404);
    assert_eq!(json["message"], "Tenant not found");
}

#[tokio::test]
async fn list_tenants_excludes_publication_roots() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("products").unwrap();
    std::fs::create_dir_all(tmp.path().join(".publication")).unwrap();
    std::fs::create_dir_all(tmp.path().join(".publication_quarantine")).unwrap();
    let app = internal_replication_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/tenants")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = crate::test_helpers::body_json(resp).await;
    assert_eq!(body["tenants"], serde_json::json!(["products"]));
}

#[tokio::test]
async fn get_ops_does_not_open_publication_roots_as_moved_source_candidates() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let node_id = std::env::var("FLAPJACK_NODE_ID").unwrap_or_else(|_| "unknown".to_string());
    let publication_oplog_dir = tmp.path().join(".publication").join("oplog");
    let publication_oplog =
        flapjack::index::oplog::OpLog::open(&publication_oplog_dir, ".publication", &node_id)
            .unwrap();
    publication_oplog
        .append(
            "move_index",
            serde_json::json!({"source": "missing_source", "destination": "products"}),
        )
        .unwrap();
    let app = internal_replication_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=missing_source&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = crate::test_helpers::body_json(resp).await;
    assert_eq!(body["message"], "Tenant not found");
    assert_eq!(body["status"], 404);
}

#[tokio::test]
async fn get_ops_moved_source_fallback_scans_valid_nonpublication_destinations() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("_shadow").unwrap();
    let destination_oplog = state.manager.get_or_create_oplog("_shadow").unwrap();
    destination_oplog
        .append(
            "move_index",
            serde_json::json!({"source": "missing_source", "destination": "_shadow"}),
        )
        .unwrap();
    destination_oplog
        .append(
            "upsert",
            serde_json::json!({"objectID": "after-move", "body": {"_id": "after-move"}}),
        )
        .unwrap();
    let app = internal_replication_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=missing_source&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["tenant_id"], "missing_source");
    assert_eq!(json["current_seq"], 1);
    let ops = json["ops"].as_array().unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0]["op_type"], "move_index");
    assert_eq!(ops[0]["payload"]["destination"], "_shadow");
}

/// Verify that malformed tenant IDs in GET `/internal/ops` are rejected as
/// client-visible 400s instead of falling through to a missing-tenant 404.
#[tokio::test]
async fn get_ops_invalid_tenant_returns_standard_400_json() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=../evil&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], 400);
    assert_eq!(
        json["message"],
        "Index name contains invalid characters (path traversal not allowed)"
    );
}

/// Verify that successful GET `/internal/ops` responses include retention metadata
/// (`oldest_retained_seq`) used by startup catch-up gap detection.
#[tokio::test]
async fn get_ops_success_includes_oldest_retained_seq_metadata() {
    let tmp = TempDir::new().unwrap();
    let mut app_state = TestStateBuilder::new(&tmp).build();
    app_state.replication_manager = Some(flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-local".to_string(),
            bind_addr: "127.0.0.1:0".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![],
        },
        None,
        tmp.path().to_path_buf(),
    ));
    let state = Arc::new(app_state);
    state.manager.create_tenant("products").unwrap();

    let oplog = state.manager.get_or_create_oplog("products").unwrap();
    oplog
        .append(
            "upsert",
            serde_json::json!({"objectID": "p1", "body": {"_id": "p1", "name": "Alpha"}}),
        )
        .unwrap();
    oplog
        .append("delete", serde_json::json!({"objectID": "p1"}))
        .unwrap();

    let app = internal_replication_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=products&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(
        json.get("node_current_seqs")
            .and_then(|value| value.as_object())
            .is_some_and(|map| map.contains_key("test-node-local")),
        "GET /internal/ops must include node-local sequence metadata in node_current_seqs"
    );
    let payload: flapjack_replication::types::GetOpsResponse =
        serde_json::from_slice(&body).unwrap();
    assert_eq!(payload.tenant_id, "products");
    assert_eq!(payload.current_seq, 2);
    assert_eq!(payload.oldest_retained_seq, Some(1));
    assert_eq!(
        payload.node_current_seqs.get("test-node-local"),
        Some(&2),
        "typed response should carry node-local current seq metadata"
    );
    assert_eq!(payload.ops.len(), 2);
    assert_eq!(payload.ops[0].seq, 1);
    assert_eq!(payload.ops[1].seq, 2);
}

/// Rolling deploy safety: missing node_current_seqs in old payloads must still deserialize.
#[test]
fn get_ops_response_deserializes_when_node_current_seqs_absent() {
    let payload = serde_json::json!({
        "tenant_id": "products",
        "ops": [],
        "current_seq": 17,
        "oldest_retained_seq": 1
    });
    let parsed: flapjack_replication::types::GetOpsResponse =
        serde_json::from_value(payload).unwrap();
    assert_eq!(parsed.tenant_id, "products");
    assert_eq!(parsed.current_seq, 17);
    assert_eq!(parsed.oldest_retained_seq, Some(1));
    assert!(
        parsed.node_current_seqs.is_empty(),
        "missing node_current_seqs must default to empty map for mixed-version peers"
    );
}

/// Verify that GET `/internal/ops` sanitizes oplog I/O failures after the handler
/// switched to the shared `HandlerError` path.
#[tokio::test]
async fn get_ops_read_failure_is_sanitized() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("broken").unwrap();
    state.manager.get_or_create_oplog("broken").unwrap();

    let oplog_dir = tmp.path().join("broken").join("oplog");
    std::fs::remove_dir_all(&oplog_dir).unwrap();
    std::fs::write(&oplog_dir, "not a directory").unwrap();

    let app = internal_replication_router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=broken&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], 500);
    assert_eq!(json["message"], "Internal server error");
    assert!(
        !json.to_string().contains("not a directory"),
        "500 payload must stay sanitized"
    );
}

/// Verify that GET `/internal/storage` returns a tenant list with IDs and non-zero byte counts for each created tenant.
#[tokio::test]
async fn storage_all_returns_tenant_list() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("tenant_a").unwrap();
    state.manager.create_tenant("tenant_b").unwrap();

    let app = Router::new()
        .route("/internal/storage", get(super::storage_all))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/storage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let tenants = json["tenants"].as_array().unwrap();
    assert_eq!(tenants.len(), 2, "should have 2 tenants");

    let ids: Vec<&str> = tenants.iter().map(|t| t["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"tenant_a"), "should contain tenant_a");
    assert!(ids.contains(&"tenant_b"), "should contain tenant_b");

    // Each tenant should have bytes field > 0 (tantivy creates meta files)
    for t in tenants {
        assert!(
            t["bytes"].as_u64().unwrap() > 0,
            "tenant {} should have non-zero bytes",
            t["id"]
        );
    }
}

/// Verify that GET `/internal/storage/:indexName` returns the index name and non-zero byte count for an existing tenant.
#[tokio::test]
async fn storage_index_returns_bytes_for_specific_tenant() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("my_index").unwrap();

    let app = Router::new()
        .route("/internal/storage/:indexName", get(super::storage_index))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/storage/my_index")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["index"].as_str().unwrap(), "my_index");
    assert!(
        json["bytes"].as_u64().unwrap() > 0,
        "existing tenant should have non-zero bytes"
    );
}

/// Verify that GET `/internal/storage/:indexName` returns `bytes: 0` for a tenant that does not exist.
#[tokio::test]
async fn storage_index_returns_zero_for_nonexistent() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let app = Router::new()
        .route("/internal/storage/:indexName", get(super::storage_index))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/storage/no_such_index")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["index"].as_str().unwrap(), "no_such_index");
    assert_eq!(
        json["bytes"].as_u64().unwrap(),
        0,
        "nonexistent tenant should have 0 bytes"
    );
}

/// Verify that GET `/internal/storage/:indexName` returns 400 for path-traversal names like `".."`.
#[tokio::test]
async fn storage_index_rejects_invalid_index_name() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    let app = Router::new()
        .route("/internal/storage/:indexName", get(super::storage_index))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/storage/..")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], 400);
}

// ── doc_count in /internal/storage ──

/// Verify that GET `/internal/storage/:indexName` includes a `doc_count` field reflecting the number of indexed documents.
#[tokio::test]
async fn storage_index_includes_doc_count() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("dc_test").unwrap();
    let docs = vec![
        flapjack::types::Document {
            id: "d1".to_string(),
            fields: std::collections::HashMap::from([(
                "name".to_string(),
                flapjack::types::FieldValue::Text("Alice".to_string()),
            )]),
        },
        flapjack::types::Document {
            id: "d2".to_string(),
            fields: std::collections::HashMap::from([(
                "name".to_string(),
                flapjack::types::FieldValue::Text("Bob".to_string()),
            )]),
        },
    ];
    state
        .manager
        .add_documents_sync("dc_test", docs)
        .await
        .unwrap();

    let app = Router::new()
        .route("/internal/storage/:indexName", get(super::storage_index))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/storage/dc_test")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(json["doc_count"].as_u64().unwrap(), 2, "should have 2 docs");
}

/// Verify that GET `/internal/storage` includes a `doc_count` field for each tenant in the response.
#[tokio::test]
async fn storage_all_includes_doc_count() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("t_dc").unwrap();
    let docs = vec![flapjack::types::Document {
        id: "d1".to_string(),
        fields: std::collections::HashMap::from([(
            "name".to_string(),
            flapjack::types::FieldValue::Text("Alice".to_string()),
        )]),
    }];
    state
        .manager
        .add_documents_sync("t_dc", docs)
        .await
        .unwrap();

    let app = Router::new()
        .route("/internal/storage", get(super::storage_all))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/storage")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let tenants = json["tenants"].as_array().unwrap();
    let tenant = tenants.iter().find(|t| t["id"] == "t_dc").unwrap();
    assert_eq!(
        tenant["doc_count"].as_u64().unwrap(),
        1,
        "should have 1 doc"
    );
}

// ── /internal/status enhancements ──

/// Pin the standalone `/internal/status` response consumed by the System screen.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Process-global env guard must span the request.
async fn ops_contract_internal_status_standalone_exact_fields() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _node_id = EnvVarRestoreGuard::remove("FLAPJACK_NODE_ID");
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    state.manager.create_tenant("s1").unwrap();
    state.manager.create_tenant("s2").unwrap();

    let app = Router::new()
        .route("/internal/status", get(super::replication_status))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    let storage_total_bytes = json["storage_total_bytes"]
        .as_u64()
        .expect("storage_total_bytes must be an unsigned integer");
    assert!(
        storage_total_bytes > 0,
        "total bytes should be > 0 with 2 tenants"
    );
    assert_eq!(
        json,
        serde_json::json!({
            "node_id": "unknown",
            "replication_enabled": false,
            "peer_count": 0,
            "ssl_renewal": null,
            "storage_total_bytes": storage_total_bytes,
            "tenant_count": 2,
            "vector_memory_bytes": 0,
        }),
        "standalone status must keep the exact seven-field wire contract"
    );
}

/// Verify that GET `/internal/status` includes a non-zero `vector_memory_bytes` field when vector indexes contain data. Requires the `vector-search` feature.
#[cfg(feature = "vector-search")]
#[tokio::test]
async fn test_internal_status_includes_vector_memory() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    // Add some vectors so memory > 0
    let mut vi =
        flapjack::vector::index::VectorIndex::new(3, flapjack::vector::MetricKind::Cos).unwrap();
    vi.add("doc1", &[1.0, 0.0, 0.0]).unwrap();
    vi.add("doc2", &[0.0, 1.0, 0.0]).unwrap();
    state.manager.set_vector_index("vec_tenant", vi);

    let app = Router::new()
        .route("/internal/status", get(super::replication_status))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

    assert!(
        json["vector_memory_bytes"].is_number(),
        "status response should include vector_memory_bytes field, got: {:?}",
        json
    );
    assert!(
        json["vector_memory_bytes"].as_u64().unwrap() > 0,
        "vector_memory_bytes should be > 0 when vectors exist"
    );
}

// ── Pause endpoint tests ──

fn make_pause_app(state: Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/internal/pause/:indexName",
            axum::routing::post(super::pause_index),
        )
        .route(
            "/internal/resume/:indexName",
            axum::routing::post(super::resume_index),
        )
        .with_state(state)
}

/// Verify that POST `/internal/pause/:indexName` returns 200 and a JSON body with `paused: true`.
#[tokio::test]
async fn test_pause_endpoint_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = make_pause_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["index"], "foo");
    assert_eq!(json["paused"], true);
}

/// Verify that pausing a nonexistent index still returns 200 (pre-emptive pause before the index is created).
#[tokio::test]
async fn test_pause_endpoint_unknown_index_still_200() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = make_pause_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/nonexistent_index")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

/// Verify that POST `/internal/pause/:indexName` returns 400 for path-traversal names and does not add them to the pause registry.
#[tokio::test]
async fn test_pause_endpoint_rejects_invalid_index_name() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = make_pause_app(state.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/..")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    assert!(
        !state.paused_indexes.is_paused(".."),
        "invalid index name must not be added to pause registry"
    );
}

/// Verify that calling the pause endpoint adds the index to the pause registry so `is_paused` returns true.
#[tokio::test]
async fn test_pause_endpoint_marks_index_in_registry() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = make_pause_app(state.clone());

    let _resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        state.paused_indexes.is_paused("foo"),
        "registry should show foo as paused after endpoint call"
    );
}

/// Verify that calling pause twice on the same index returns 200 both times (idempotent).
#[tokio::test]
async fn test_pause_endpoint_double_call_idempotent() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    // First call
    let app1 = make_pause_app(state.clone());
    let resp1 = app1
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second call (same index)
    let app2 = make_pause_app(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

// ── Resume endpoint tests ──

/// Verify that POST `/internal/resume/:indexName` returns 200 and a JSON body with `paused: false`.
#[tokio::test]
async fn test_resume_endpoint_returns_200() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    // Pause first so there's something to resume
    state.paused_indexes.pause("foo");
    let app = make_pause_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["index"], "foo");
    assert_eq!(json["paused"], false);
}

/// Verify that resuming an index that was never paused still returns 200 (idempotent no-op).
#[tokio::test]
async fn test_resume_endpoint_unknown_index_still_200() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = make_pause_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/never_paused")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

/// Verify that POST `/internal/resume/:indexName` returns 400 for path-traversal index names like `".."`.
#[tokio::test]
async fn test_resume_endpoint_rejects_invalid_index_name() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = make_pause_app(state);

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/..")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// Verify that calling the resume endpoint removes the index from the pause registry so `is_paused` returns false.
#[tokio::test]
async fn test_resume_endpoint_clears_pause_in_registry() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    state.paused_indexes.pause("foo");
    let app = make_pause_app(state.clone());

    let _resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        !state.paused_indexes.is_paused("foo"),
        "foo should no longer be paused after resume endpoint"
    );
}

/// Verify that calling resume twice on the same index returns 200 both times (idempotent).
#[tokio::test]
async fn test_resume_endpoint_double_call_idempotent() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    // First resume (not paused — should still be 200)
    let app1 = make_pause_app(state.clone());
    let resp1 = app1
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp1.status(), StatusCode::OK);

    // Second resume
    let app2 = make_pause_app(state);
    let resp2 = app2
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/foo")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp2.status(), StatusCode::OK);
}

// ── Full cycle integration test (2I) ────────────────────────────────

/// Integration test exercising the full pause/resume lifecycle: write before pause succeeds, pause blocks writes with 503 and Retry-After header, reads remain unblocked, resume restores write access.
#[tokio::test]
async fn test_full_pause_write_resume_cycle() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();

    // Build a combined router with pause/resume + write + search endpoints
    /// Build an Axum router with pause, resume, batch write, and search endpoints for the full pause/resume integration test.
    fn make_cycle_app(state: Arc<AppState>) -> Router {
        crate::router::app_id_layer(
            Router::new()
                .route(
                    "/internal/pause/:indexName",
                    axum::routing::post(super::pause_index),
                )
                .route(
                    "/internal/resume/:indexName",
                    axum::routing::post(super::resume_index),
                )
                .route(
                    "/1/indexes/:indexName/batch",
                    axum::routing::post(crate::handlers::objects::add_documents),
                )
                .route(
                    "/1/indexes/:indexName/query",
                    axum::routing::post(crate::handlers::search::search),
                )
                .with_state(state),
        )
    }

    // Step 1: Write before pause — should NOT be 503
    let app = make_cycle_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/batch")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "step 1: write before pause should NOT return 503"
    );

    // Step 2: Pause "products"
    let app = make_cycle_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/pause/products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "step 2: pause should return 200"
    );

    // Step 3: Write while paused — should be 503
    let app = make_cycle_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/batch")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "step 3: write while paused should return 503"
    );
    // Verify Retry-After header is present (required by 2B checklist)
    assert_eq!(
        resp.headers()
            .get("Retry-After")
            .and_then(|v| v.to_str().ok()),
        Some("1"),
        "step 3: 503 response should include Retry-After: 1 header"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["status"], 503,
        "step 3: error payload should include HTTP status"
    );
    assert!(
        json["message"]
            .as_str()
            .is_some_and(|msg| msg.contains("temporarily unavailable")),
        "step 3: error payload should include index paused message, got: {json}"
    );

    // Step 4: Search/read while paused — reads must NOT be blocked
    let app = make_cycle_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/query")
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"query":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "step 4: search while paused must NOT return 503 — reads are never blocked"
    );

    // Step 5: Resume "products"
    let app = make_cycle_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/resume/products")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "step 5: resume should return 200"
    );

    // Step 6: Write after resume — should NOT be 503
    let app = make_cycle_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/batch")
                .header("Content-Type", "application/json")
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(
        resp.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "step 6: write after resume should NOT return 503"
    );
}
#[test]
fn contains_document_replication_ops_detects_upsert_and_delete() {
    let upsert = make_upsert_op(1, 1000, "node-a", "tenant", "doc1", "alpha");
    let delete = make_delete_op(2, 1001, "node-a", "tenant", "doc1");
    let save_rule = make_index_op(
        3,
        1002,
        "node-a",
        "tenant",
        "save_rule",
        serde_json::json!({
            "objectID": "rule-1",
            "conditions": [{"anchoring": "contains", "pattern": "phone"}],
            "consequence": {"params": {"query": "telephone"}}
        }),
    );

    assert!(contains_document_replication_ops(&[
        save_rule.clone(),
        upsert.clone(),
    ]));
    assert!(contains_document_replication_ops(&[delete]));
    assert!(!contains_document_replication_ops(&[save_rule]));
}

// ── Cluster status endpoint tests ──

/// Pin the standalone branch of GET `/internal/cluster/status`.
#[tokio::test]
#[allow(clippy::await_holding_lock)] // Process-global env guard must span the request.
async fn ops_contract_cluster_status_standalone_exact_branch() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _node_id = EnvVarRestoreGuard::remove("FLAPJACK_NODE_ID");
    let tmp = TempDir::new().unwrap();
    // Default TestStateBuilder has replication_manager: None → standalone mode.
    let state = TestStateBuilder::new(&tmp).build_shared();

    let app = Router::new()
        .route("/internal/cluster/status", get(super::cluster_status))
        .with_state(state);

    let resp = app
        .oneshot(
            Request::builder()
                .uri("/internal/cluster/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = crate::test_helpers::body_json(resp).await;

    assert_eq!(
        body,
        serde_json::json!({
            "node_id": "unknown",
            "replication_enabled": false,
            "peers": [],
            "autoheal_enabled": false,
            "autoheal_peers": [],
        }),
        "standalone cluster status must keep the exact auto-heal-aware branch"
    );
    assert_eq!(body["autoheal_enabled"], false);
    assert_eq!(
        body["autoheal_peers"],
        serde_json::json!([]),
        "standalone status should expose an empty auto-heal lifecycle array"
    );
}

/// Pin the HA branch of GET `/internal/cluster/status`.
#[tokio::test]
async fn ops_contract_cluster_status_ha_exact_branch() {
    let tmp = TempDir::new().unwrap();
    let (_replication_data_dir, repl_mgr) = test_replication_manager_with_two_peers();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let app = Router::new()
        .route("/internal/cluster/status", get(super::cluster_status))
        .with_state(state);

    let body = cluster_status_body(&app).await;
    assert_eq!(
        body,
        serde_json::json!({
            "node_id": "test-node-a",
            "replication_enabled": true,
            "peers_total": 2,
            "peers_healthy": 0,
            "autoheal_enabled": false,
            "autoheal_peers": [],
            "peers": [
                {
                    "peer_id": "test-node-b",
                    "addr": "http://test-node-b:7700",
                    "status": "never_contacted",
                    "last_success_secs_ago": null,
                },
                {
                    "peer_id": "test-node-c",
                    "addr": "http://test-node-c:7700",
                    "status": "never_contacted",
                    "last_success_secs_ago": null,
                },
            ],
        }),
        "HA cluster status must keep the exact auto-heal-aware branch and peer values"
    );

    let peers = body["peers"]
        .as_array()
        .expect("HA response must include peers array");
    for peer in peers {
        let object = peer.as_object().expect("each peer must be an object");
        assert_eq!(object.len(), 4, "each peer must have exactly four fields");
        for field in ["peer_id", "addr", "status", "last_success_secs_ago"] {
            assert!(object.contains_key(field), "peer must include {field}");
        }
    }

    let peers_total = body["peers_total"].as_u64().unwrap() as usize;
    let peers_healthy = body["peers_healthy"].as_u64().unwrap() as usize;
    assert_eq!(
        peers_total,
        peers.len(),
        "backend-owned peers_total must match the returned rows"
    );
    assert!(
        peers_healthy <= peers_total,
        "backend-owned healthy count must be within the total"
    );
    let peer_ids: Vec<&str> = peers.iter().filter_map(|p| p["peer_id"].as_str()).collect();
    assert!(peer_ids.contains(&"test-node-b"));
    assert!(peer_ids.contains(&"test-node-c"));
    assert_eq!(
        body["autoheal_peers"],
        serde_json::json!([]),
        "disabled auto-heal should not fabricate lifecycle observations"
    );
}

/// Pin every peer-health wire token and the exact serialized peer shape.
#[tokio::test]
async fn ops_contract_cluster_peer_status_wire_tokens() {
    let expected_tokens = [
        "healthy",
        "stale",
        "unhealthy",
        "circuit_open",
        "never_contacted",
    ];
    let peer =
        |index: usize, status: &str, last_success_secs_ago: Option<u64>| super::ClusterPeerStatus {
            peer_id: format!("peer-{index}"),
            addr: format!("http://peer-{index}:7700"),
            status: status.to_string(),
            last_success_secs_ago,
        };
    let response = super::ClusterStatusResponse::Ha(super::ClusterStatusHaResponse {
        node_id: "contract-node".to_string(),
        replication_enabled: true,
        peers_total: expected_tokens.len(),
        peers_healthy: 1,
        peers: vec![
            peer(0, "healthy", Some(0)),
            peer(1, "stale", Some(60)),
            peer(2, "unhealthy", Some(300)),
            peer(3, "circuit_open", Some(1)),
            peer(4, "never_contacted", None),
        ],
        autoheal_enabled: false,
        autoheal_peers: Vec::new(),
    });

    let serialized = serde_json::to_value(response).unwrap();
    let peers = serialized["peers"].as_array().unwrap();
    let emitted_tokens: Vec<&str> = peers
        .iter()
        .map(|peer| peer["status"].as_str().unwrap())
        .collect();
    assert_eq!(emitted_tokens, expected_tokens);
    for peer in peers {
        let object = peer.as_object().expect("each peer must be an object");
        assert_eq!(object.len(), 4, "each peer must have exactly four fields");
        for field in ["peer_id", "addr", "status", "last_success_secs_ago"] {
            assert!(object.contains_key(field), "peer must include {field}");
        }
    }

    let tmp = TempDir::new().unwrap();
    let (_replication_data_dir, repl_mgr) = test_replication_manager_with_two_peers();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr)
        .build_shared();
    let app = Router::new()
        .route("/internal/cluster/status", get(super::cluster_status))
        .with_state(state);
    let observed = cluster_status_body(&app).await;
    for status in observed["peers"].as_array().unwrap().iter().map(|peer| {
        peer["status"]
            .as_str()
            .expect("real HA endpoint status must be a string")
    }) {
        assert!(
            expected_tokens.contains(&status),
            "real HA endpoint emitted unknown peer status {status}"
        );
    }
}

#[test]
fn ops_contract_cluster_peer_status_schema_tokens_match_wire_tokens() {
    use utoipa::PartialSchema;

    let expected_tokens = super::ClusterPeerHealthStatus::WIRE_TOKENS;
    let schema = serde_json::to_value(super::ClusterPeerHealthStatus::schema()).unwrap();
    let schema_tokens = schema
        .pointer("/enum")
        .and_then(|value| value.as_array())
        .expect("ClusterPeerHealthStatus schema must declare enum values")
        .iter()
        .map(|value| {
            value
                .as_str()
                .expect("peer status enum values must be strings")
        })
        .collect::<Vec<_>>();

    assert_eq!(schema_tokens, expected_tokens);
}

#[test]
fn ssl_renewal_status_projection_preserves_canonical_fields() {
    let next_check = chrono::Utc::now();
    let canonical = flapjack_ssl::manager::RenewalStatus {
        enabled: false,
        status: "failed".to_string(),
        error: Some("certificate expired".to_string()),
        cert_expires_in_days: Some(-1),
        next_check: Some(next_check),
    };

    let projected = super::SslRenewalStatus::from(canonical.clone());

    assert_eq!(projected.enabled, canonical.enabled);
    assert_eq!(projected.status, canonical.status);
    assert_eq!(projected.error, canonical.error);
    assert_eq!(
        projected.cert_expires_in_days,
        canonical.cert_expires_in_days
    );
    assert_eq!(projected.next_check, canonical.next_check);
}

#[test]
fn cluster_status_deserialization_uses_replication_enabled_discriminator() {
    let ha_without_counts = serde_json::json!({
        "node_id": "bootstrap-a",
        "replication_enabled": true,
        "peers": [{
            "peer_id": "peer-a",
            "addr": "http://peer-a:7700",
            "status": "healthy",
            "last_success_secs_ago": 1
        }]
    });
    let decoded: super::ClusterStatusResponse = serde_json::from_value(ha_without_counts).unwrap();
    let super::ClusterStatusResponse::Ha(decoded) = decoded else {
        panic!("replication_enabled=true must deserialize to the HA branch");
    };
    assert_eq!(decoded.peers_total, 1);
    assert_eq!(decoded.peers_healthy, 1);

    let impossible_standalone = serde_json::json!({
        "node_id": "standalone",
        "replication_enabled": false,
        "peers_total": 1,
        "peers_healthy": 0,
        "peers": []
    });
    assert!(
        serde_json::from_value::<super::ClusterStatusResponse>(impossible_standalone).is_err(),
        "replication_enabled=false must reject HA count fields"
    );
}

fn cluster_status_test_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/internal/cluster/status", get(super::cluster_status))
        .with_state(state)
}

fn replication_manager_in(
    data_dir: &Path,
    peers: Vec<flapjack_replication::config::PeerConfig>,
) -> std::sync::Arc<flapjack_replication::manager::ReplicationManager> {
    flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers,
        },
        None,
        data_dir.to_path_buf(),
    )
}

fn cluster_peer<'a>(body: &'a serde_json::Value, peer_id: &str) -> &'a serde_json::Value {
    body["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["peer_id"] == peer_id)
        .unwrap_or_else(|| panic!("expected active peer {peer_id} in cluster status"))
}

fn autoheal_peer<'a>(body: &'a serde_json::Value, peer_id: &str) -> &'a serde_json::Value {
    body["autoheal_peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["peer_id"] == peer_id)
        .unwrap_or_else(|| panic!("expected auto-heal peer {peer_id} in lifecycle status"))
}

#[tokio::test]
async fn cluster_status_autoheal_enabled_before_observation_reports_zero_counts() {
    let tmp = TempDir::new().unwrap();
    let repl_mgr = replication_manager_in(
        tmp.path(),
        vec![
            flapjack_replication::config::PeerConfig {
                node_id: "test-node-b".to_string(),
                addr: "http://test-node-b:7700".to_string(),
            },
            flapjack_replication::config::PeerConfig {
                node_id: "test-node-c".to_string(),
                addr: "http://test-node-c:7700".to_string(),
            },
        ],
    );
    repl_mgr.start_health_probe(60, true);
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();

    let body = cluster_status_body(&cluster_status_test_router(state)).await;

    repl_mgr.stop_health_probe();
    assert_eq!(body["autoheal_enabled"], true);
    assert_eq!(body["peers_total"], 2);
    assert_eq!(body["peers"].as_array().unwrap().len(), 2);
    assert_eq!(body["autoheal_peers"].as_array().unwrap().len(), 2);
    for peer_id in ["test-node-b", "test-node-c"] {
        let peer = autoheal_peer(&body, peer_id);
        assert_eq!(peer["observation_count"], 0);
        assert!(peer.get("decision").is_none());
        assert!(peer.get("action").is_none());
    }
}

#[tokio::test]
async fn cluster_status_autoheal_lifecycle_reports_hold_refusal_eviction_and_readmission() {
    let tmp = TempDir::new().unwrap();
    {
        let mut journal =
            flapjack_replication::autoheal::AutohealJournal::with_max_bytes(tmp.path(), 16 * 1024)
                .unwrap();
        journal
            .record_decision(
                &["test-node-b".to_string(), "test-node-c".to_string()],
                "test-node-b",
                flapjack_replication::autoheal::EvictionDecision::Hold {
                    observations_remaining: 1,
                },
            )
            .unwrap();
        journal
            .record_decision(
                &["test-node-b".to_string(), "test-node-c".to_string()],
                "test-node-c",
                flapjack_replication::autoheal::EvictionDecision::RefuseWouldBreakQuorum {
                    current: 1,
                    required: 2,
                },
            )
            .unwrap();
        journal
            .record_eviction(
                &["test-node-b".to_string(), "test-node-c".to_string()],
                "test-node-b",
                Some(flapjack_replication::config::PeerConfig {
                    node_id: "test-node-b".to_string(),
                    addr: "http://test-node-b:7700".to_string(),
                }),
                flapjack_replication::autoheal::EvictionDecision::Evict {
                    node_id: "test-node-b".to_string(),
                    reason: "sustained health probe failures reached threshold 3".to_string(),
                },
                || Ok(()),
            )
            .unwrap();
        journal
            .record_readmission::<(), flapjack_replication::manager::AddPeerError, _>(
                &["test-node-c".to_string()],
                &flapjack_replication::config::PeerConfig {
                    node_id: "test-node-b".to_string(),
                    addr: "http://test-node-b:7700".to_string(),
                },
                "autoheal-0000000000000003".to_string(),
                || Ok(()),
            )
            .unwrap();
    }
    let repl_mgr = replication_manager_in(
        tmp.path(),
        vec![
            flapjack_replication::config::PeerConfig {
                node_id: "test-node-b".to_string(),
                addr: "http://test-node-b:7700".to_string(),
            },
            flapjack_replication::config::PeerConfig {
                node_id: "test-node-c".to_string(),
                addr: "http://test-node-c:7700".to_string(),
            },
        ],
    );
    repl_mgr.start_health_probe(60, true);
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();

    let body = cluster_status_body(&cluster_status_test_router(state)).await;

    repl_mgr.stop_health_probe();
    assert_eq!(body["autoheal_enabled"], true);
    assert_eq!(body["peers_total"], body["peers"].as_array().unwrap().len());
    assert_eq!(
        cluster_peer(&body, "test-node-b")["addr"],
        "http://test-node-b:7700"
    );
    let readmitted = autoheal_peer(&body, "test-node-b");
    assert_eq!(readmitted["observation_count"], 0);
    assert_eq!(readmitted["decision"]["kind"], "evict");
    assert_eq!(readmitted["action"]["phase"], "readmission_outcome");
    assert_eq!(readmitted["action"]["outcome"], "success");
    let refused = autoheal_peer(&body, "test-node-c");
    assert_eq!(refused["decision"]["kind"], "refuse_would_break_quorum");
    assert_eq!(refused["action"]["phase"], "decision_recorded");
    assert_eq!(refused["action"]["outcome"], "not_required");
}

#[tokio::test]
async fn cluster_status_autoheal_keeps_evicted_candidates_out_of_active_membership() {
    let tmp = TempDir::new().unwrap();
    {
        let mut journal =
            flapjack_replication::autoheal::AutohealJournal::with_max_bytes(tmp.path(), 16 * 1024)
                .unwrap();
        journal
            .record_eviction(
                &["test-node-b".to_string(), "test-node-c".to_string()],
                "test-node-b",
                Some(flapjack_replication::config::PeerConfig {
                    node_id: "test-node-b".to_string(),
                    addr: "http://test-node-b:7700".to_string(),
                }),
                flapjack_replication::autoheal::EvictionDecision::Evict {
                    node_id: "test-node-b".to_string(),
                    reason: "sustained health probe failures reached threshold 3".to_string(),
                },
                || Ok(()),
            )
            .unwrap();
    }
    let repl_mgr = replication_manager_in(
        tmp.path(),
        vec![flapjack_replication::config::PeerConfig {
            node_id: "test-node-c".to_string(),
            addr: "http://test-node-c:7700".to_string(),
        }],
    );
    repl_mgr.start_health_probe(60, true);
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();

    let body = cluster_status_body(&cluster_status_test_router(state)).await;

    repl_mgr.stop_health_probe();
    assert_eq!(body["peers_total"], 1);
    assert_eq!(body["peers"].as_array().unwrap().len(), 1);
    assert_eq!(
        cluster_peer(&body, "test-node-c")["addr"],
        "http://test-node-c:7700"
    );
    assert!(body["peers"]
        .as_array()
        .unwrap()
        .iter()
        .all(|peer| peer["peer_id"] != "test-node-b"));
    let evicted = autoheal_peer(&body, "test-node-b");
    assert_eq!(evicted["addr"], "http://test-node-b:7700");
    assert_eq!(evicted["observation_count"], 0);
    assert_eq!(evicted["decision"]["kind"], "evict");
    assert_eq!(evicted["action"]["phase"], "eviction_outcome");
    assert_eq!(evicted["action"]["outcome"], "success");
}

fn test_replication_manager_with_two_peers() -> (
    TempDir,
    std::sync::Arc<flapjack_replication::manager::ReplicationManager>,
) {
    let data_dir = TempDir::new().unwrap();
    let node_config = flapjack_replication::config::NodeConfig {
        node_id: "test-node-a".to_string(),
        bind_addr: "127.0.0.1:7700".to_string(),
        advertise_addr: None,
        bootstrap_peer: None,
        peers: vec![
            flapjack_replication::config::PeerConfig {
                node_id: "test-node-b".to_string(),
                addr: "http://test-node-b:7700".to_string(),
            },
            flapjack_replication::config::PeerConfig {
                node_id: "test-node-c".to_string(),
                addr: "http://test-node-c:7700".to_string(),
            },
        ],
    };
    let manager = flapjack_replication::manager::ReplicationManager::new(
        node_config,
        None,
        data_dir.path().to_path_buf(),
    );
    (data_dir, manager)
}

fn remove_peer_test_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route(
            "/internal/cluster/peers/:node_id",
            delete(super::remove_cluster_peer),
        )
        .route("/internal/cluster/status", get(super::cluster_status))
        .with_state(state)
}

fn add_peer_test_router(state: std::sync::Arc<AppState>) -> Router {
    Router::new()
        .route("/internal/cluster/peers", post(super::add_cluster_peer))
        .with_state(state)
}

async fn add_peer_request(app: Router, node_id: &str, addr: &str) -> Response {
    app.oneshot(
        Request::builder()
            .method("POST")
            .uri("/internal/cluster/peers")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"node_id": node_id, "addr": addr}).to_string(),
            ))
            .unwrap(),
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn add_cluster_peer_retries_are_idempotent_but_address_changes_conflict() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = test_replication_manager_with_two_peers();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();
    let app = add_peer_test_router(state);

    let retry = add_peer_request(app.clone(), "test-node-b", "http://test-node-b:7700").await;
    assert_eq!(retry.status(), StatusCode::OK);
    let retry_body = crate::test_helpers::body_json(retry).await;
    assert_eq!(retry_body["peers_total"], 2);
    assert_eq!(repl_mgr.peer_count(), 2);

    let conflict = add_peer_request(app, "test-node-b", "http://different-node-b:7700").await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(repl_mgr.peer_count(), 2);
}

#[tokio::test]
async fn add_cluster_peer_persistence_failure_returns_non_leaking_500() {
    let tmp = TempDir::new().unwrap();
    let missing_data_dir = tmp.path().join("missing-data-dir");
    let repl_mgr = flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: Vec::new(),
        },
        None,
        missing_data_dir,
    );
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();

    let response = add_peer_request(
        add_peer_test_router(state),
        "test-node-b",
        "http://test-node-b:7700",
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = crate::test_helpers::body_json(response).await;
    assert_eq!(
        body["message"],
        "Failed to persist replication peer membership"
    );
    assert_eq!(repl_mgr.peer_count(), 0);
}

#[tokio::test]
async fn remove_cluster_peer_persistence_failure_returns_non_leaking_500() {
    let tmp = TempDir::new().unwrap();
    let missing_data_dir = tmp.path().join("missing-data-dir");
    let repl_mgr = flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![flapjack_replication::config::PeerConfig {
                node_id: "test-node-b".to_string(),
                addr: "http://test-node-b:7700".to_string(),
            }],
        },
        None,
        missing_data_dir,
    );
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();

    let response = remove_peer_test_router(state)
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/cluster/peers/test-node-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let body = crate::test_helpers::body_json(response).await;
    assert_eq!(
        body["message"],
        "Failed to persist replication peer membership"
    );
    assert_eq!(repl_mgr.peer_count(), 1);
}

async fn cluster_status_body(app: &Router) -> serde_json::Value {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/internal/cluster/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    crate::test_helpers::body_json(resp).await
}

#[tokio::test]
async fn remove_cluster_peer_known_peer_returns_200_and_removes_membership() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = test_replication_manager_with_two_peers();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();
    let app = remove_peer_test_router(state);

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/cluster/peers/test-node-b")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = crate::test_helpers::body_json(resp).await;
    assert_eq!(body["node_id"], "test-node-b");
    assert_eq!(body["peers_total"], 1);
    assert_eq!(repl_mgr.peer_count(), 1);

    let status = cluster_status_body(&app).await;
    assert_eq!(status["peers_total"], 1);
    let peers = status["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0]["peer_id"], "test-node-c");
}

#[tokio::test]
async fn remove_cluster_peer_unknown_peer_returns_404_without_mutation() {
    let tmp = TempDir::new().unwrap();
    let (_repl_data_dir, repl_mgr) = test_replication_manager_with_two_peers();
    let state = TestStateBuilder::new(&tmp)
        .with_replication_manager(repl_mgr.clone())
        .build_shared();
    let app = remove_peer_test_router(state);
    let before_status = cluster_status_body(&app).await;
    assert!(repl_mgr.get_peer_cursors("tenant-red").is_none());

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/internal/cluster/peers/missing-node")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    let body = crate::test_helpers::body_json(resp).await;
    assert_eq!(body["status"], 404);
    assert_eq!(body["message"], "Peer 'missing-node' not found");
    assert_eq!(repl_mgr.peer_count(), 2);
    assert_eq!(cluster_status_body(&app).await, before_status);
    assert!(repl_mgr.get_peer_cursors("tenant-red").is_none());
}
