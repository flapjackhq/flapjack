//! Stub summary for c3_replica_freshness.rs.
// Contract C3: Startup/Periodic Freshness Gates
//
// Decision 0003 §C3 verdict: already upheld at HEAD.
// regression-locked at HEAD: keep all-peer bootstrap coverage enforced.
// Primary owner seam: engine/flapjack-http/src/startup_catchup.rs::{run_pre_serve_catchup, run_periodic_catchup}
// Supporting owner seam: engine/flapjack-http/src/handlers/readiness.rs::ready

use crate::common;

/// TODO: Document c3_replica_freshness_requires_all_peer_coverage_per_tenant_round.
#[tokio::test]
async fn c3_replica_freshness_requires_all_peer_coverage_per_tenant_round() {
    let (addr_a, _tmp_a) = common::spawn_server_with_internal("c3-node-a").await;
    let (addr_c, _tmp_c) = common::spawn_server_with_internal("c3-node-c").await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/1/indexes/c3-peer-only/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [
                {"action": "addObject", "body": {"_id": "a-dummy", "title": "OnlyOnPeerA"}}
            ]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, resp).await;
    let resp = client
        .delete(format!("http://{}/1/indexes/c3-peer-only/a-dummy", addr_a))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, resp).await;

    let resp = client
        .post(format!("http://{}/1/indexes/c3-peer-only/batch", addr_c))
        .json(&serde_json::json!({
            "requests": [
                {"action": "addObject", "body": {"_id": "c-dummy", "title": "CSeqAdvance"}},
                {"action": "deleteObject", "body": {"objectID": "c-dummy"}},
                {"action": "addObject", "body": {"_id": "c3-doc", "title": "OnlyOnPeerC"}}
            ]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_c, resp).await;

    let tmp_b = tempfile::TempDir::new().unwrap();
    let node_b = common::try_spawn_replication_node_on_existing_dir_with_peers(
        tmp_b.path(),
        "c3-node-b",
        vec![
            ("c3-node-a".to_string(), format!("http://{}", addr_a)),
            ("c3-node-c".to_string(), format!("http://{}", addr_c)),
        ],
    )
    .await
    .expect("multi-peer pre-serve catch-up bootstrap should finish");

    let query_result: serde_json::Value = client
        .post(format!(
            "http://{}/1/indexes/c3-peer-only/query",
            node_b.addr
        ))
        .json(&serde_json::json!({"query": "OnlyOnPeerC"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let found = query_result["nbHits"].as_u64().unwrap_or(0);
    node_b.stop().await;

    // Hand-calc: node A has 0 live docs in tenant (dummy add+delete), node C has 1 live doc.
    assert_eq!(
        found, 1,
        "C3 regression gate: pre-serve catch-up must evaluate all peers per tenant before readiness"
    );
    let first_hit_id = query_result["hits"][0]["_id"]
        .as_str()
        .or_else(|| query_result["hits"][0]["objectID"].as_str());
    assert_eq!(
        first_hit_id,
        Some("c3-doc"),
        "node should serve the document that exists only on the second peer after bootstrap catch-up"
    );
}
