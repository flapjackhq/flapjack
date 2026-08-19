//! Stub summary for c1_ownership.rs.
// Contract C1: Push Replication Ownership
//
// Decision 0003 §C1 verdict: already upheld at HEAD.
// regression-locked at HEAD: keep failed-delivery cursor tracking visible.
// Primary owner seam: engine/flapjack-replication/src/manager.rs::replicate_ops

use crate::common;

/// TODO: Document c1_ownership_requires_trackable_delivery_failure.
#[tokio::test]
async fn c1_ownership_requires_trackable_delivery_failure() {
    let (live_peer_addr, _live_peer_tmp) = common::spawn_server_with_internal("c1-live-peer").await;
    let tmp_b = tempfile::TempDir::new().unwrap();
    let state_b = common::build_replication_state_for_existing_dir_with_peers(
        tmp_b.path(),
        "c1-node-b",
        vec![
            (
                "c1-live-peer".to_string(),
                format!("http://{}", live_peer_addr),
            ),
            (
                "c1-unreachable-peer".to_string(),
                "http://127.0.0.1:1".to_string(),
            ),
        ],
    );
    let manager = state_b.replication_manager.clone().unwrap();

    let op = flapjack::index::oplog::OpLogEntry {
        seq: 1,
        timestamp_ms: 1,
        node_id: "c1-node-a".to_string(),
        tenant_id: "tenant-c1".to_string(),
        op_type: "upsert".to_string(),
        payload: serde_json::json!({"objectID": "doc-1", "body": {"_id": "doc-1", "name": "Alpha"}}),
    };

    manager.replicate_ops("tenant-c1", vec![op]).await;
    for _ in 0..220 {
        let acked = manager
            .get_peer_cursors("tenant-c1")
            .map(|map| map.len())
            .unwrap_or(0);
        if acked >= 1 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    tokio::time::sleep(tokio::time::Duration::from_millis(2200)).await;

    let tracked_peers = manager
        .get_peer_cursors("tenant-c1")
        .expect("peer cursors must exist once replication attempts are issued");
    // Hand-calc: 2 configured peers -> 1 live ack record + 1 failed-delivery record.
    assert_eq!(
        tracked_peers.len(),
        2,
        "C1 regression gate: replicate_ops must track both successful and failed peer deliveries"
    );

    let live_cursor = tracked_peers
        .get("c1-live-peer")
        .expect("live peer must have a tracked delivery cursor");
    assert_eq!(
        live_cursor.last_acked_seq,
        Some(1),
        "live peer should acknowledge the replicated op sequence"
    );
    assert!(
        live_cursor.last_delivery_error.is_none(),
        "live peer should not keep a delivery error cursor"
    );
    drop(live_cursor);

    let failed_cursor = tracked_peers
        .get("c1-unreachable-peer")
        .expect("unreachable peer must have a tracked failed-delivery cursor");
    assert!(
        failed_cursor.last_acked_seq.is_none(),
        "unreachable peer should not report an acked sequence in this scenario"
    );
    assert!(
        failed_cursor.last_delivery_error.is_some(),
        "unreachable peer must retain the delivery failure detail for ownership tracking"
    );
}
