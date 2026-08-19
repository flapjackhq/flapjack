//! Stub summary for c4_restart_recovery.rs.
// Contract C4: Restart Recovery & Committed Sequence Continuity
//
// Decision 0003 §C4 verdict: already upheld at HEAD.
// Primary owner seam: engine/src/index/manager/recovery.rs::recover_from_oplog

use crate::common;

/// TODO: Document restart_recovery_catches_up_before_serving.
#[tokio::test]
async fn restart_recovery_catches_up_before_serving() {
    let client = reqwest::Client::new();

    let (node_a, node_b, tmp_a, tmp_b) =
        common::spawn_stoppable_replication_pair("c4-a", "c4-b").await;

    let resp = client
        .post(format!("http://{}/1/indexes/c4_restart/batch", node_a.addr))
        .json(&serde_json::json!({
            "requests": (1..=10).map(|i| serde_json::json!({
                "action": "addObject",
                "body": {"objectID": format!("doc-{}", i), "num": i}
            })).collect::<Vec<_>>()
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &node_a.addr, resp).await;

    let mut initial_hits = 0;
    for _ in 0..200 {
        let result: serde_json::Value = client
            .post(format!("http://{}/1/indexes/c4_restart/query", node_b.addr))
            .json(&serde_json::json!({"query": ""}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        initial_hits = result["nbHits"].as_u64().unwrap_or(0);
        if initial_hits >= 10 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    // Hand-calc: first batch is 10 docs, all should replicate before stop.
    assert_eq!(
        initial_hits, 10,
        "node-b must have the first 10 docs before restart"
    );

    node_b.stop().await;

    let resp = client
        .post(format!("http://{}/1/indexes/c4_restart/batch", node_a.addr))
        .json(&serde_json::json!({
            "requests": (11..=20).map(|i| serde_json::json!({
                "action": "addObject",
                "body": {"objectID": format!("doc-{}", i), "num": i}
            })).collect::<Vec<_>>()
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &node_a.addr, resp).await;

    let node_b_restarted = common::spawn_replication_node_on_existing_dir(
        tmp_b.path(),
        "c4-b",
        &format!("http://{}", node_a.addr),
        "c4-a",
    )
    .await;

    let search_result: serde_json::Value = client
        .post(format!(
            "http://{}/1/indexes/c4_restart/query",
            node_b_restarted.addr
        ))
        .json(&serde_json::json!({"query": ""}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let final_hits = search_result["nbHits"].as_u64().unwrap_or(0);
    // Hand-calc: 10 pre-stop + 10 while stopped = 20 after pre-serve recovery.
    assert_eq!(
        final_hits, 20,
        "restarted node must serve all 20 docs after catch-up"
    );

    let committed_after_restart =
        flapjack::index::oplog::read_committed_seq(&tmp_b.path().join("c4_restart"));
    // Hand-calc: committed sequence should advance at least once per replayed op and never remain zero after 20 writes.
    assert!(
        committed_after_restart >= 20,
        "committed_seq continuity broken after restart: {committed_after_restart}"
    );

    node_b_restarted.stop().await;
    node_a.stop().await;
    drop(tmp_a);
    drop(tmp_b);
}
