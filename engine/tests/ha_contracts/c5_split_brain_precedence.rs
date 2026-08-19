//! Stub summary for c5_split_brain_precedence.rs.
// Contract C5: Split-Brain Precedence (LWW)
//
// Decision 0003 §C5 verdict: already upheld at HEAD.
// Primary owner seam: engine/flapjack-http/src/handlers/internal.rs::apply_ops_to_manager
// Supporting seam: engine/flapjack-http/src/handlers/internal_ops/document_ops.rs

use crate::common;
use flapjack::IndexManager;

/// TODO: Document make_upsert_op.
fn make_upsert_op(
    seq: u64,
    timestamp_ms: u64,
    node_id: &str,
    object_id: &str,
    name: &str,
) -> flapjack::index::oplog::OpLogEntry {
    flapjack::index::oplog::OpLogEntry {
        seq,
        timestamp_ms,
        node_id: node_id.to_string(),
        tenant_id: "ha-c5".to_string(),
        op_type: "upsert".to_string(),
        payload: serde_json::json!({
            "objectID": object_id,
            "body": {"_id": object_id, "name": name}
        }),
    }
}

fn make_delete_op(
    seq: u64,
    timestamp_ms: u64,
    node_id: &str,
    object_id: &str,
) -> flapjack::index::oplog::OpLogEntry {
    flapjack::index::oplog::OpLogEntry {
        seq,
        timestamp_ms,
        node_id: node_id.to_string(),
        tenant_id: "ha-c5".to_string(),
        op_type: "delete".to_string(),
        payload: serde_json::json!({"objectID": object_id}),
    }
}

/// TODO: Document lww_same_timestamp_higher_node_id_wins.
#[tokio::test]
async fn lww_same_timestamp_higher_node_id_wins() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let op_z = vec![make_upsert_op(1, 1000, "z-node", "doc1", "ZNode")];
    flapjack_http::handlers::internal::apply_ops_to_manager(&manager, "c5_t1", &op_z)
        .await
        .unwrap();
    common::wait_for_document_text_field(&manager, "c5_t1", "doc1", "name", "ZNode").await;

    let op_a = vec![make_upsert_op(2, 1000, "a-node", "doc1", "ANode")];
    flapjack_http::handlers::internal::apply_ops_to_manager(&manager, "c5_t1", &op_a)
        .await
        .unwrap();

    let doc = manager.get_document("c5_t1", "doc1").unwrap().unwrap();
    // Hand-calc: equal timestamps => lexicographically higher node_id wins ("z-node" > "a-node").
    assert!(
        matches!(doc.fields.get("name"), Some(flapjack::types::FieldValue::Text(v)) if v == "ZNode")
    );
}

/// TODO: Document lww_stale_delete_does_not_remove_newer_upsert.
#[tokio::test]
async fn lww_stale_delete_does_not_remove_newer_upsert() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    let upsert = vec![make_upsert_op(1, 2000, "node-a", "doc1", "Alive")];
    flapjack_http::handlers::internal::apply_ops_to_manager(&manager, "c5_t2", &upsert)
        .await
        .unwrap();
    common::wait_for_document_exists(&manager, "c5_t2", "doc1").await;

    let stale_delete = vec![make_delete_op(2, 1000, "node-b", "doc1")];
    flapjack_http::handlers::internal::apply_ops_to_manager(&manager, "c5_t2", &stale_delete)
        .await
        .unwrap();

    // Hand-calc: delete ts=1000 is stale against upsert ts=2000, so doc count stays 1.
    assert!(manager.get_document("c5_t2", "doc1").unwrap().is_some());
}

/// TODO: Document lww_map_rebuilt_from_oplog_blocks_stale_op_after_restart.
#[tokio::test]
async fn lww_map_rebuilt_from_oplog_blocks_stale_op_after_restart() {
    let tmp = tempfile::TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let primary_ts;
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("c5_restart").unwrap();
        let doc = flapjack::types::Document::from_json(
            &serde_json::json!({"_id": "doc1", "name": "Original"}),
        )
        .unwrap();
        manager
            .add_documents_sync("c5_restart", vec![doc])
            .await
            .unwrap();

        let oplog = manager.get_or_create_oplog("c5_restart").unwrap();
        let ops = oplog.read_since(0).unwrap();
        primary_ts = ops
            .iter()
            .find(|op| op.op_type == "upsert")
            .unwrap()
            .timestamp_ms;
        manager.graceful_shutdown().await;
    }

    {
        let manager = IndexManager::new(&base);
        let stale = vec![make_upsert_op(
            99,
            primary_ts.saturating_sub(1),
            "remote",
            "doc1",
            "StaleOverwrite",
        )];
        flapjack_http::handlers::internal::apply_ops_to_manager(&manager, "c5_restart", &stale)
            .await
            .unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
        let doc = manager.get_document("c5_restart", "doc1").unwrap().unwrap();
        // Hand-calc: stale timestamp primary_ts-1 must lose to oplog-rebuilt LWW state, so name remains Original.
        assert!(
            matches!(doc.fields.get("name"), Some(flapjack::types::FieldValue::Text(v)) if v == "Original")
        );
        manager.graceful_shutdown().await;
    }
}
