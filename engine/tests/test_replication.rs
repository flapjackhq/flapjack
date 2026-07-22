//! Consolidated replication tests.
//!
//! Merged from (all deleted):
//!   - test_replication_internal.rs  (no-auth enforcement for internal endpoints)
//!   - test_replication_safety.rs    (oplog integrity: body, delete payload, sequences)
//!   - test_replication_phase5.rs    (apply_ops helper, cluster/status, startup catch-up, peer_statuses)
//!
//! Two-node E2E tests (closes known gap #6 from TESTING.md):
//!   test_two_node_write_replicates_to_peer
//!   test_two_node_delete_propagates_to_peer
//!   test_two_node_bidirectional_replication
//!   test_two_node_startup_catchup_via_get_ops
//!
//! Phase 4 analytics rollup exchange tests:
//!   test_analytics_rollup_exchange_endpoint_accepts_rollup
//!
//! Phase 4b rollup broadcaster tests:
//!   test_rollup_cache_status_endpoint_empty
//!   test_rollup_cache_status_reflects_stored_rollup
//!   test_run_rollup_broadcast_sends_to_peer
//!   test_rollup_broadcaster_integration_periodic

mod common;

use common::state::make_test_app_state;
use flapjack::types::Document;
use flapjack::IndexManager;
use std::sync::Arc;
use tempfile::TempDir;

#[test]
fn test_replication_uses_shared_app_state_constructor_only() {
    let source = include_str!("test_replication.rs");
    let pattern = ["Arc::new(", "flapjack_http::handlers::AppState", " {"].concat();
    let inline_literal_count = source.matches(&pattern).count();
    assert_eq!(
        inline_literal_count, 0,
        "test_replication.rs still contains {inline_literal_count} inline AppState literals; use make_test_app_state instead"
    );
}

// ============================================================
// From test_replication_internal.rs
// Internal endpoints must work WITHOUT authentication
// ============================================================

// test_internal_status_no_auth_required removed — redundant with smoke_internal_endpoint in test_smoke.rs

#[tokio::test]
async fn test_internal_replicate_no_auth_required() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    let state = make_test_app_state(
        temp_dir.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let internal = Router::new()
        .route(
            "/internal/replicate",
            axum::routing::post(flapjack_http::handlers::internal::replicate_ops),
        )
        .with_state(state);

    let body = serde_json::json!({"tenant_id": "test", "ops": []});

    let response = internal
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/internal/replicate should return 200 OK without authentication"
    );
}

#[tokio::test]
async fn test_internal_get_ops_no_auth_required() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    let doc = Document::from_json(&serde_json::json!({"_id": "1", "title": "Test"})).unwrap();
    manager.add_documents_sync("test", vec![doc]).await.unwrap();

    let state = make_test_app_state(
        temp_dir.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let internal = Router::new()
        .route(
            "/internal/ops",
            axum::routing::get(flapjack_http::handlers::internal::get_ops),
        )
        .with_state(state);

    let response = internal
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=test&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "/internal/ops should return 200 OK without authentication"
    );
}

#[tokio::test]
async fn test_internal_tenants_no_auth_required() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("tenant-z").unwrap();
    manager.create_tenant("tenant-a").unwrap();
    std::fs::create_dir_all(temp_dir.path().join(".hidden-tenant")).unwrap();
    std::fs::write(temp_dir.path().join("not-a-directory.txt"), "ignore-me").unwrap();

    let state = make_test_app_state(
        temp_dir.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let internal = Router::new()
        .route(
            "/internal/tenants",
            axum::routing::get(flapjack_http::handlers::internal::list_tenants),
        )
        .with_state(state);

    let response = internal
        .oneshot(
            Request::builder()
                .uri("/internal/tenants")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1_000_000)
        .await
        .unwrap();
    let response: flapjack_replication::types::ListTenantsResponse =
        serde_json::from_slice(&body).unwrap();

    assert_eq!(
        status,
        StatusCode::OK,
        "/internal/tenants should return 200 OK without authentication"
    );
    assert_eq!(
        response.tenants,
        vec!["tenant-a".to_string(), "tenant-z".to_string()],
        "internal tenant listing should return sorted visible tenant directories only"
    );
}

#[tokio::test]
async fn test_internal_helper_router_write_path_injects_app_id_extension() {
    let (addr, _tmp) = common::spawn_server_with_internal("node-a").await;
    let client = reqwest::Client::new();

    // Intentionally omit x-algolia-application-id to ensure the helper router
    // still injects AuthenticatedAppId like the production router middleware.
    let write_resp = client
        .post(format!("http://{}/1/indexes/repl_idx/batch", addr))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "doc1", "title": "Parity"}}]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr, write_resp).await;

    let response = client
        .post(format!("http://{}/1/indexes/repl_idx/query", addr))
        .json(&serde_json::json!({"query": "Parity"}))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json::<serde_json::Value>().await.unwrap();
    assert!(
        status.is_success(),
        "helper router write/query path should stay successful without explicit app-id header; got status={status}, body={body}"
    );
    assert_eq!(
        body["nbHits"].as_u64().unwrap_or(0),
        1,
        "write issued through helper router should be visible in subsequent query"
    );
}

#[tokio::test]
async fn test_internal_tenant_isolation() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("tenant-a").unwrap();
    let doc = Document::from_json(&serde_json::json!({"_id": "1", "title": "Secret A"})).unwrap();
    manager
        .add_documents_sync("tenant-a", vec![doc])
        .await
        .unwrap();

    let state = make_test_app_state(
        temp_dir.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let internal = Router::new()
        .route(
            "/internal/ops",
            axum::routing::get(flapjack_http::handlers::internal::get_ops),
        )
        .with_state(state);

    let response = internal
        .clone()
        .oneshot(
            Request::builder()
                .uri("/internal/ops?tenant_id=tenant-a&since_seq=0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["tenant_id"], "tenant-a");
    assert!(!json["ops"].as_array().unwrap().is_empty());
}

// ============================================================
// From test_replication_safety.rs
// Oplog integrity: full body, delete payload, monotonic seqs
// ============================================================

#[tokio::test]
async fn test_oplog_contains_full_document_body() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    let doc = Document::from_json(
        &serde_json::json!({"_id": "1", "title": "Test Document", "price": 99}),
    )
    .unwrap();
    manager.add_documents_sync("test", vec![doc]).await.unwrap();

    let oplog = manager
        .get_or_create_oplog("test")
        .expect("OpLog should exist");
    let ops = oplog.read_since(0).unwrap();

    assert!(!ops.is_empty(), "OpLog should have at least one entry");
    let first_op = &ops[0];
    assert_eq!(first_op.op_type, "upsert", "First op should be an upsert");

    let body = first_op
        .payload
        .get("body")
        .expect("Payload should have 'body' field");
    assert_eq!(
        body.get("title").and_then(|v| v.as_str()),
        Some("Test Document")
    );
    assert_eq!(body.get("price").and_then(|v| v.as_u64()), Some(99));
}

#[tokio::test]
async fn test_oplog_delete_includes_object_id() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    let doc =
        Document::from_json(&serde_json::json!({"_id": "delete-me", "title": "Temp"})).unwrap();
    manager.add_documents_sync("test", vec![doc]).await.unwrap();
    manager
        .delete_documents_sync("test", vec!["delete-me".to_string()])
        .await
        .unwrap();

    let oplog = manager
        .get_or_create_oplog("test")
        .expect("OpLog should exist");
    let ops = oplog.read_since(0).unwrap();

    let delete_op = ops
        .iter()
        .find(|op| op.op_type == "delete")
        .expect("Should have a delete operation in oplog");

    assert!(
        delete_op.payload.get("objectID").is_some(),
        "Delete operation MUST include objectID"
    );
    assert_eq!(
        delete_op.payload.get("objectID").and_then(|v| v.as_str()),
        Some("delete-me")
    );
}

#[tokio::test]
async fn test_concurrent_tenant_oplog_isolation() {
    let temp_dir = TempDir::new().unwrap();
    let manager = Arc::new(IndexManager::new(temp_dir.path()));

    manager.create_tenant("tenant-a").unwrap();
    manager.create_tenant("tenant-b").unwrap();

    let mgr_a = Arc::clone(&manager);
    let mgr_b = Arc::clone(&manager);

    let handle_a = tokio::spawn(async move {
        for i in 0..10 {
            let doc =
                Document::from_json(&serde_json::json!({"_id": format!("a-{}", i), "tenant": "A"}))
                    .unwrap();
            mgr_a
                .add_documents_sync("tenant-a", vec![doc])
                .await
                .unwrap();
        }
    });

    let handle_b = tokio::spawn(async move {
        for i in 0..10 {
            let doc =
                Document::from_json(&serde_json::json!({"_id": format!("b-{}", i), "tenant": "B"}))
                    .unwrap();
            mgr_b
                .add_documents_sync("tenant-b", vec![doc])
                .await
                .unwrap();
        }
    });

    handle_a.await.unwrap();
    handle_b.await.unwrap();

    let ops_a = manager
        .get_or_create_oplog("tenant-a")
        .unwrap()
        .read_since(0)
        .unwrap();
    let ops_b = manager
        .get_or_create_oplog("tenant-b")
        .unwrap()
        .read_since(0)
        .unwrap();

    assert_eq!(
        ops_a.len(),
        10,
        "Tenant A should have exactly 10 operations"
    );
    assert_eq!(
        ops_b.len(),
        10,
        "Tenant B should have exactly 10 operations"
    );

    for op in &ops_a {
        if let Some(body) = op.payload.get("body") {
            assert_eq!(
                body.get("tenant").and_then(|v| v.as_str()),
                Some("A"),
                "Tenant A oplog should only contain tenant A documents"
            );
        }
    }
    for op in &ops_b {
        if let Some(body) = op.payload.get("body") {
            assert_eq!(
                body.get("tenant").and_then(|v| v.as_str()),
                Some("B"),
                "Tenant B oplog should only contain tenant B documents"
            );
        }
    }
}

#[tokio::test]
async fn test_oplog_sequence_numbers_monotonic() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    for i in 1..=5 {
        let doc =
            Document::from_json(&serde_json::json!({"_id": i.to_string(), "value": i})).unwrap();
        manager.add_documents_sync("test", vec![doc]).await.unwrap();
    }

    let ops = manager
        .get_or_create_oplog("test")
        .unwrap()
        .read_since(0)
        .unwrap();
    assert_eq!(ops.len(), 5, "Should have 5 operations");

    let mut prev_seq = 0u64;
    for (idx, op) in ops.iter().enumerate() {
        assert!(
            op.seq > prev_seq,
            "Sequence {} (op {}) should be greater than previous {}",
            op.seq,
            idx,
            prev_seq
        );
        prev_seq = op.seq;
    }
}

#[tokio::test]
async fn test_oplog_read_since_boundary() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test").unwrap();
    for i in 1..=10 {
        let doc =
            Document::from_json(&serde_json::json!({"_id": i.to_string(), "value": i})).unwrap();
        manager.add_documents_sync("test", vec![doc]).await.unwrap();
    }

    let oplog = manager.get_or_create_oplog("test").unwrap();
    let all_ops = oplog.read_since(0).unwrap();
    assert_eq!(all_ops.len(), 10);

    let fifth_seq = all_ops[4].seq;
    let ops_after = oplog.read_since(fifth_seq).unwrap();

    assert_eq!(
        ops_after.len(),
        5,
        "read_since({}) should return 5 remaining ops, not {}",
        fifth_seq,
        ops_after.len()
    );
    for op in &ops_after {
        assert!(
            op.seq > fifth_seq,
            "read_since returned op with seq {} <= {}",
            op.seq,
            fifth_seq
        );
    }
}

#[tokio::test]
async fn test_replicate_ops_handler_applies_correctly() {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use tower::ServiceExt;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());

    let state = make_test_app_state(
        temp_dir.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let internal = Router::new()
        .route(
            "/internal/replicate",
            axum::routing::post(flapjack_http::handlers::internal::replicate_ops),
        )
        .with_state(state);

    let req_body = serde_json::json!({
        "tenant_id": "test",
        "ops": [
            {"seq": 1, "timestamp_ms": 1000, "node_id": "node-a", "tenant_id": "test", "op_type": "upsert", "payload": {"body": {"_id": "1", "title": "First"}}},
            {"seq": 2, "timestamp_ms": 2000, "node_id": "node-a", "tenant_id": "test", "op_type": "upsert", "payload": {"body": {"_id": "2", "title": "Second"}}},
            {"seq": 3, "timestamp_ms": 3000, "node_id": "node-a", "tenant_id": "test", "op_type": "delete", "payload": {"objectID": "1"}}
        ]
    });

    let response = internal
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&req_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "Replication should succeed"
    );

    assert_eq!(
        manager
            .search("test", "First", None, None, 10)
            .unwrap()
            .total,
        0,
        "Document 1 should be deleted"
    );
    assert_eq!(
        manager
            .search("test", "Second", None, None, 10)
            .unwrap()
            .total,
        1,
        "Document 2 should exist"
    );
}

// ============================================================
// From test_replication_phase5.rs
// apply_ops_to_manager helper, cluster/status, startup catch-up, peer_statuses
// ============================================================

#[tokio::test]
async fn test_apply_ops_to_manager_upsert() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    let state = make_test_app_state(
        temp.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let router = Router::new()
        .route(
            "/internal/replicate",
            axum::routing::post(flapjack_http::handlers::internal::replicate_ops),
        )
        .with_state(state);

    let body = serde_json::json!({
        "tenant_id": "test-apply",
        "ops": [
            {
                "seq": 1, "timestamp_ms": 1000, "node_id": "node-a",
                "tenant_id": "test-apply", "op_type": "upsert",
                "payload": {"body": {"_id": "doc1", "title": "Alpha"}}
            },
            {
                "seq": 2, "timestamp_ms": 2000, "node_id": "node-a",
                "tenant_id": "test-apply", "op_type": "upsert",
                "payload": {"body": {"_id": "doc2", "title": "Beta"}}
            }
        ]
    });

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    // Poll until the write queue processes and commits the documents.
    for _ in 0..500 {
        if manager
            .search("test-apply", "", None, None, 10)
            .map(|r| r.total)
            .unwrap_or(0)
            >= 2
        {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    let results = manager
        .search("test-apply", "Alpha", None, None, 10)
        .unwrap();
    assert_eq!(results.total, 1, "doc1 should be indexed");
    let results2 = manager
        .search("test-apply", "Beta", None, None, 10)
        .unwrap();
    assert_eq!(results2.total, 1, "doc2 should be indexed");
}

// test_apply_ops_to_manager_returns_max_seq removed — redundant with apply_ops_returns_max_seq in internal.rs
// test_apply_ops_empty_batch_is_ok removed — redundant with apply_ops_unknown_type_skipped + direct tests in internal.rs

#[tokio::test]
async fn test_cluster_status_no_replication() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    let state = make_test_app_state(
        temp.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let router = Router::new()
        .route(
            "/internal/cluster/status",
            axum::routing::get(flapjack_http::handlers::internal::cluster_status),
        )
        .with_state(state);

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/internal/cluster/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["replication_enabled"], false);
    assert!(json["peers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn test_cluster_status_with_peers() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use flapjack_replication::config::{NodeConfig, PeerConfig};
    use flapjack_replication::manager::ReplicationManager;
    use tower::ServiceExt;

    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    let repl_mgr = ReplicationManager::new(
        NodeConfig {
            node_id: "node-a".to_string(),
            bind_addr: "0.0.0.0:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![
                PeerConfig {
                    node_id: "node-b".to_string(),
                    addr: "http://node-b:7700".to_string(),
                },
                PeerConfig {
                    node_id: "node-c".to_string(),
                    addr: "http://node-c:7700".to_string(),
                },
            ],
        },
        None,
        temp.path().to_path_buf(),
    );

    let state = make_test_app_state(
        temp.path(),
        Some(manager.clone()),
        None,
        Some(repl_mgr),
        None,
        None,
        None,
    );

    let router = Router::new()
        .route(
            "/internal/cluster/status",
            axum::routing::get(flapjack_http::handlers::internal::cluster_status),
        )
        .with_state(state);

    let resp = router
        .oneshot(
            Request::builder()
                .uri("/internal/cluster/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(json["node_id"], "node-a");
    assert_eq!(json["replication_enabled"], true);
    assert_eq!(json["peers_total"], 2);

    let peers = json["peers"].as_array().unwrap();
    assert_eq!(peers.len(), 2);

    assert_eq!(peers[0]["peer_id"], "node-b");
    assert_eq!(peers[0]["status"], "never_contacted");
    assert!(peers[0]["last_success_secs_ago"].is_null());

    assert_eq!(peers[1]["peer_id"], "node-c");
    assert_eq!(peers[1]["status"], "never_contacted");
}

#[tokio::test]
async fn test_startup_catchup_noop_without_replication() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    // Write a doc so we can verify catch-up doesn't corrupt existing data
    manager.create_tenant("existing").unwrap();
    let doc =
        Document::from_json(&serde_json::json!({"_id": "1", "title": "Pre-existing"})).unwrap();
    manager
        .add_documents_sync("existing", vec![doc])
        .await
        .unwrap();

    let state = make_test_app_state(
        temp.path(),
        Some(manager.clone()),
        None,
        None, // No replication configured
        None,
        None,
        None,
    );

    // Both the legacy and pre-serve paths should complete without error
    // and not corrupt existing data when replication is not configured.
    flapjack_http::startup_catchup::run_pre_serve_catchup(&state)
        .await
        .unwrap();
    flapjack_http::startup_catchup::run_startup_catchup(state).await;

    // Verify existing data is untouched
    let result = manager
        .search("existing", "Pre-existing", None, None, 10)
        .unwrap();
    assert_eq!(
        result.total, 1,
        "Startup catchup with no replication should not corrupt existing data"
    );
}

// test_peer_statuses_never_contacted removed — exact duplicate of test_peer_statuses_initially_never_contacted in manager.rs
// test_peer_statuses_no_peers removed — exact duplicate of test_peer_statuses_no_peers_returns_empty in manager.rs

#[tokio::test]
async fn test_apply_ops_upsert_then_delete_ordering() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use axum::Router;
    use tower::ServiceExt;

    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    let state = make_test_app_state(
        temp.path(),
        Some(manager.clone()),
        None,
        None,
        None,
        None,
        None,
    );

    let router = Router::new()
        .route(
            "/internal/replicate",
            axum::routing::post(flapjack_http::handlers::internal::replicate_ops),
        )
        .with_state(state);

    let body = serde_json::json!({
        "tenant_id": "test-order",
        "ops": [
            {"seq": 1, "timestamp_ms": 1000, "node_id": "n", "tenant_id": "test-order",
             "op_type": "upsert", "payload": {"body": {"_id": "keep", "title": "Keep this"}}},
            {"seq": 2, "timestamp_ms": 2000, "node_id": "n", "tenant_id": "test-order",
             "op_type": "upsert", "payload": {"body": {"_id": "remove", "title": "Remove this"}}},
            {"seq": 3, "timestamp_ms": 3000, "node_id": "n", "tenant_id": "test-order",
             "op_type": "delete", "payload": {"objectID": "remove"}}
        ]
    });

    let resp = router
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/replicate")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let keep = manager
        .search("test-order", "Keep", None, None, 10)
        .unwrap();
    assert_eq!(keep.total, 1, "'keep' doc should be searchable");

    let remove = manager
        .search("test-order", "Remove", None, None, 10)
        .unwrap();
    assert_eq!(remove.total, 0, "'remove' doc should be deleted");
}

// ============================================================
// Two-node E2E integration tests (closes known gap #6 in TESTING.md).
//
// All tests above use oneshot requests or direct function calls.
// These exercise the complete replication stack:
//   HTTP write → trigger_replication() → peer POST /internal/replicate
//   → peer applies ops → peer search returns result.
//
// Phase 3 note: write forwarding (proxying writes from one node to another)
// is intentionally NOT implemented. In the current full-mesh architecture:
//   1. Every node accepts writes directly.
//   2. trigger_replication() propagates every write to all peers.
//   3. A load balancer (nginx round-robin) distributes writes across nodes.
// test_two_node_bidirectional_replication below demonstrates that writes on
// any node reach all other nodes, which is what write forwarding would achieve
// at the proxy layer. Explicit write forwarding is therefore unnecessary.
// ============================================================

async fn query_index(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
    query: &str,
) -> (reqwest::StatusCode, u64, serde_json::Value) {
    let resp = client
        .post(format!("http://{}/1/indexes/{}/query", addr, index_name))
        .json(&serde_json::json!({ "query": query }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body = resp.json::<serde_json::Value>().await.unwrap();
    let hits = body.get("nbHits").and_then(|v| v.as_u64()).unwrap_or(0);
    (status, hits, body)
}

fn authenticated_client(api_key: &str) -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "x-algolia-api-key",
        reqwest::header::HeaderValue::from_str(api_key).unwrap(),
    );
    headers.insert(
        "x-algolia-application-id",
        reqwest::header::HeaderValue::from_static("runtime-membership-test"),
    );
    reqwest::Client::builder()
        .default_headers(headers)
        .build()
        .unwrap()
}

async fn write_unique_document(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
    object_id: &str,
) -> reqwest::Response {
    client
        .post(format!("http://{addr}/1/indexes/{index_name}/batch"))
        .json(&serde_json::json!({
            "requests": [{
                "action": "addObject",
                "body": { "objectID": object_id, "title": object_id }
            }]
        }))
        .send()
        .await
        .unwrap()
}

async fn wait_for_exact_document(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
    object_id: &str,
) {
    for _ in 0..200 {
        let (status, hits, body) = query_index(client, addr, index_name, object_id).await;
        if status.is_success() && hits == 1 && body["hits"][0]["objectID"] == object_id {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for exactly {index_name}/{object_id} on {addr}");
}

#[tokio::test]
async fn runtime_manager_add_peer_replicates_new_writes_without_restart() {
    use flapjack_replication::config::PeerConfig;

    let harness = common::state::spawn_runtime_add_peer_pair().await;
    assert_eq!(harness.node_a_replication_manager.peer_count(), 0);
    let receipt = harness
        .node_a_replication_manager
        .add_peer(PeerConfig {
            node_id: "node-b".to_string(),
            addr: harness.node_b_peer_url.clone(),
        })
        .unwrap();
    assert_eq!(receipt.node_id, "node-b");
    assert_eq!(receipt.addr, harness.node_b_peer_url);
    assert_eq!(receipt.peers_total, 1);

    let client = reqwest::Client::new();
    let response = write_unique_document(
        &client,
        &harness.node_a_addr,
        "runtime-manager-receipt",
        "manager-added-peer-document",
    )
    .await;
    common::wait_for_response_task(&client, &harness.node_a_addr, response).await;
    wait_for_exact_document(
        &client,
        &harness.node_b_addr,
        "runtime-manager-receipt",
        "manager-added-peer-document",
    )
    .await;
}

async fn post_add_peer(
    client: &reqwest::Client,
    node_a_addr: &str,
    body: serde_json::Value,
) -> reqwest::Response {
    client
        .post(format!("http://{node_a_addr}/internal/cluster/peers"))
        .json(&body)
        .send()
        .await
        .unwrap()
}

async fn delete_peer(
    client: &reqwest::Client,
    node_a_addr: &str,
    node_id: &str,
) -> reqwest::Response {
    client
        .delete(format!(
            "http://{node_a_addr}/internal/cluster/peers/{node_id}"
        ))
        .send()
        .await
        .unwrap()
}

async fn add_runtime_peer(
    admin_client: &reqwest::Client,
    node_a_addr: &str,
    node_id: &str,
    peer_url: &str,
) -> serde_json::Value {
    let response = post_add_peer(
        admin_client,
        node_a_addr,
        serde_json::json!({
            "node_id": node_id,
            "addr": format!("{peer_url}/"),
        }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    response.json().await.unwrap()
}

async fn assert_unauthorized_and_invalid_adds_preserve_empty_membership(
    harness: &common::state::RuntimeAddPeerHarness,
    admin_client: &reqwest::Client,
    non_admin_client: &reqwest::Client,
) {
    let anonymous_client = reqwest::Client::new();
    let valid_body = serde_json::json!({
        "node_id": "node-b",
        "addr": format!("{}/", harness.node_b_peer_url),
    });
    for client in [&anonymous_client, non_admin_client] {
        let response = post_add_peer(client, &harness.node_a_addr, valid_body.clone()).await;
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(harness.node_a_replication_manager.peer_count(), 0);
    }

    for invalid_body in [
        serde_json::json!({ "node_id": "", "addr": harness.node_b_peer_url }),
        serde_json::json!({ "node_id": "   ", "addr": harness.node_b_peer_url }),
        serde_json::json!({ "node_id": "node-b", "addr": "not-a-url" }),
        serde_json::json!({ "node_id": "node-b", "addr": "http://127.0.0.1:7700" }),
        serde_json::json!({ "node_id": "node-b", "addr": format!("{}/internal/status", harness.node_b_peer_url) }),
        serde_json::json!({ "node_id": "node-b", "addr": format!("{}?debug=1", harness.node_b_peer_url) }),
    ] {
        let response = post_add_peer(admin_client, &harness.node_a_addr, invalid_body).await;
        assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);
        assert_eq!(harness.node_a_replication_manager.peer_count(), 0);
    }
}

async fn assert_missing_replication_manager_rejected(peer_url: &str) {
    let (standalone_addr, _standalone_temp_dir) =
        common::spawn_server_with_key(Some("standalone-admin-key")).await;
    let standalone_client = authenticated_client("standalone-admin-key");
    let response = post_add_peer(
        &standalone_client,
        &standalone_addr,
        serde_json::json!({ "node_id": "node-b", "addr": peer_url }),
    )
    .await;
    assert_eq!(response.status(), reqwest::StatusCode::BAD_REQUEST);

    let status: serde_json::Value = standalone_client
        .get(format!("http://{standalone_addr}/internal/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["peer_count"], 0);
}

#[tokio::test]
async fn authenticated_http_add_peer_validates_authorizes_and_replicates_without_restart() {
    let admin_key = "runtime-membership-admin-key";
    let harness = common::state::spawn_authenticated_runtime_add_peer_pair(admin_key).await;
    let admin_client = authenticated_client(harness.admin_key.as_deref().unwrap());
    let non_admin_client = authenticated_client(harness.non_admin_key.as_deref().unwrap());
    assert_unauthorized_and_invalid_adds_preserve_empty_membership(
        &harness,
        &admin_client,
        &non_admin_client,
    )
    .await;

    let valid_body = serde_json::json!({
        "node_id": "node-b",
        "addr": format!("{}/", harness.node_b_peer_url),
    });
    let response = post_add_peer(&admin_client, &harness.node_a_addr, valid_body.clone()).await;
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    let receipt: serde_json::Value = response.json().await.unwrap();
    let normalized_addr = flapjack_replication::config::NodeConfig::normalize_peer_addr(
        valid_body["addr"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(receipt["node_id"], "node-b");
    assert_eq!(receipt["addr"], normalized_addr);
    assert_eq!(receipt["peers_total"], 1);
    assert_eq!(harness.node_a_replication_manager.peer_count(), 1);

    let status: serde_json::Value = admin_client
        .get(format!(
            "http://{}/internal/cluster/status",
            harness.node_a_addr
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["peers_total"], 1);
    assert_eq!(
        status["peers_total"],
        status["peers"].as_array().unwrap().len()
    );
    assert_eq!(status["peers"][0]["peer_id"], "node-b");
    assert_eq!(status["peers"][0]["addr"], normalized_addr);

    let duplicate = post_add_peer(
        &admin_client,
        &harness.node_a_addr,
        serde_json::json!({
            "node_id": "node-b",
            "addr": "http://10.9.8.7:7700",
        }),
    )
    .await;
    assert_eq!(duplicate.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(harness.node_a_replication_manager.peer_count(), 1);

    let response = write_unique_document(
        &admin_client,
        &harness.node_a_addr,
        "runtime-http-receipt",
        "http-added-peer-document",
    )
    .await;
    common::wait_for_response_task_authed(
        &admin_client,
        &harness.node_a_addr,
        response,
        Some(admin_key),
    )
    .await;
    wait_for_exact_document(
        &admin_client,
        &harness.node_b_addr,
        "runtime-http-receipt",
        "http-added-peer-document",
    )
    .await;
    assert_missing_replication_manager_rejected(&harness.node_b_peer_url).await;
}

async fn wait_for_peer_cursors(
    manager: &flapjack_replication::manager::ReplicationManager,
    tenant: &str,
    expected_peer_ids: &[&str],
) {
    for _ in 0..200 {
        if let Some(cursors) = manager.get_peer_cursors(tenant) {
            let has_all_expected = expected_peer_ids.iter().all(|peer_id| {
                cursors
                    .get(*peer_id)
                    .is_some_and(|cursor| cursor.last_acked_seq.is_some())
            });
            if has_all_expected && cursors.len() == expected_peer_ids.len() {
                return;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    let got = manager
        .get_peer_cursors(tenant)
        .map(|cursors| {
            cursors
                .iter()
                .map(|entry| entry.key().clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    panic!("timed out waiting for {tenant} cursors {expected_peer_ids:?}; got {got:?}");
}

async fn cluster_status_peer_ids(
    client: &reqwest::Client,
    node_addr: &str,
) -> (u64, Vec<String>, serde_json::Value) {
    let status: serde_json::Value = client
        .get(format!("http://{node_addr}/internal/cluster/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let peers_total = status["peers_total"]
        .as_u64()
        .expect("cluster status must include peers_total");
    let peer_ids = status["peers"]
        .as_array()
        .expect("cluster status must include peers")
        .iter()
        .map(|peer| {
            peer["peer_id"]
                .as_str()
                .expect("peer status must include peer_id")
                .to_string()
        })
        .collect();
    (peers_total, peer_ids, status)
}

async fn assert_remove_peer_forbidden_preserves_membership(
    harness: &common::state::RuntimeAddPeerHarness,
    admin_client: &reqwest::Client,
    non_admin_client: &reqwest::Client,
) {
    let anonymous_client = reqwest::Client::new();
    for client in [&anonymous_client, non_admin_client] {
        let response = delete_peer(client, &harness.node_a_addr, "node-b").await;
        assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
        assert_eq!(harness.node_a_replication_manager.peer_count(), 2);
        let (peers_total, peer_ids, _status) =
            cluster_status_peer_ids(admin_client, &harness.node_a_addr).await;
        assert_eq!(peers_total, 2);
        assert!(peer_ids.contains(&"node-b".to_string()));
        assert!(peer_ids.contains(&"node-c".to_string()));
    }
}

#[tokio::test]
async fn authenticated_http_remove_peer_stops_future_replication_without_restart() {
    let admin_key = "runtime-membership-remove-admin-key";
    let harness = common::state::spawn_authenticated_runtime_add_peer_triplet(admin_key).await;
    let admin_client = authenticated_client(harness.admin_key.as_deref().unwrap());
    let non_admin_client = authenticated_client(harness.non_admin_key.as_deref().unwrap());

    let node_b_receipt = add_runtime_peer(
        &admin_client,
        &harness.node_a_addr,
        "node-b",
        &harness.node_b_peer_url,
    )
    .await;
    let node_c_receipt = add_runtime_peer(
        &admin_client,
        &harness.node_a_addr,
        "node-c",
        harness.node_c_peer_url(),
    )
    .await;
    assert_eq!(node_b_receipt["peers_total"], 1);
    assert_eq!(node_c_receipt["peers_total"], 2);
    assert_eq!(harness.node_a_replication_manager.peer_count(), 2);

    assert_remove_peer_forbidden_preserves_membership(&harness, &admin_client, &non_admin_client)
        .await;

    let seed_tenant = "runtime-http-remove-peer-seed";
    let seed_response = write_unique_document(
        &admin_client,
        &harness.node_a_addr,
        seed_tenant,
        "seed-before-remove",
    )
    .await;
    common::wait_for_response_task_authed(
        &admin_client,
        &harness.node_a_addr,
        seed_response,
        Some(admin_key),
    )
    .await;
    wait_for_exact_document(
        &admin_client,
        &harness.node_b_addr,
        seed_tenant,
        "seed-before-remove",
    )
    .await;
    wait_for_exact_document(
        &admin_client,
        harness.node_c_addr(),
        seed_tenant,
        "seed-before-remove",
    )
    .await;
    wait_for_peer_cursors(
        &harness.node_a_replication_manager,
        seed_tenant,
        &["node-b", "node-c"],
    )
    .await;
    assert!(
        harness.node_b_replicate_request_count() > 0,
        "seed write must hit removed peer before deletion"
    );
    assert!(
        harness.node_c_replicate_request_count() > 0,
        "seed write must hit retained peer before deletion"
    );

    let remove_response = delete_peer(&admin_client, &harness.node_a_addr, "node-b").await;
    assert_eq!(remove_response.status(), reqwest::StatusCode::OK);
    let remove_body: serde_json::Value = remove_response.json().await.unwrap();
    assert_eq!(remove_body["node_id"], "node-b");
    assert_eq!(remove_body["peers_total"], 1);
    assert_eq!(harness.node_a_replication_manager.peer_count(), 1);

    let (peers_total, peer_ids, _status) =
        cluster_status_peer_ids(&admin_client, &harness.node_a_addr).await;
    assert_eq!(peers_total, 1);
    assert_eq!(peer_ids, vec!["node-c".to_string()]);
    let cursors_after_remove = harness
        .node_a_replication_manager
        .get_peer_cursors(seed_tenant)
        .expect("seed cursor map should remain for retained peer");
    assert!(!cursors_after_remove.contains_key("node-b"));
    assert!(cursors_after_remove.contains_key("node-c"));

    let removed_peer_count_after_delete = harness.node_b_replicate_request_count();
    let post_remove_response = write_unique_document(
        &admin_client,
        &harness.node_a_addr,
        seed_tenant,
        "post-remove-retained-only",
    )
    .await;
    common::wait_for_response_task_authed(
        &admin_client,
        &harness.node_a_addr,
        post_remove_response,
        Some(admin_key),
    )
    .await;
    wait_for_exact_document(
        &admin_client,
        harness.node_c_addr(),
        seed_tenant,
        "post-remove-retained-only",
    )
    .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    assert_eq!(
        harness.node_b_replicate_request_count(),
        removed_peer_count_after_delete,
        "removed peer must not receive future /internal/replicate requests"
    );
    let (_status, removed_hits, removed_body) = query_index(
        &admin_client,
        &harness.node_b_addr,
        seed_tenant,
        "post-remove-retained-only",
    )
    .await;
    assert_eq!(
        removed_hits, 0,
        "removed peer should not receive post-remove document; body={removed_body}"
    );
}

async fn get_json(client: &reqwest::Client, url: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client.get(url).send().await.unwrap();
    let status = resp.status();
    let raw_body = resp.text().await.unwrap_or_default();
    let body = serde_json::from_str::<serde_json::Value>(&raw_body)
        .unwrap_or_else(|_| serde_json::json!({ "raw": raw_body }));
    (status, body)
}

async fn wait_for_json_success(
    client: &reqwest::Client,
    url: String,
    description: &str,
) -> serde_json::Value {
    let mut last_status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
    let mut last_body = serde_json::json!(null);
    for _ in 0..200 {
        let (status, body) = get_json(client, &url).await;
        if status.is_success() {
            return body;
        }
        last_status = status;
        last_body = body;
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!(
        "Timed out waiting for {} at {} (last status={}, last body={})",
        description, url, last_status, last_body
    );
}

async fn wait_for_pagination_limited_to(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
    expected: u64,
) -> bool {
    let url = format!("http://{}/1/indexes/{}/settings", addr, index_name);
    for _ in 0..200 {
        let (status, body) = get_json(client, &url).await;
        if status.is_success()
            && body.get("paginationLimitedTo").and_then(|v| v.as_u64()) == Some(expected)
        {
            return true;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    false
}

async fn wait_for_hits_at_least(
    client: &reqwest::Client,
    addr: &str,
    index_name: &str,
    expected_hits: u64,
) {
    for _ in 0..200 {
        let (_status, hits, _body) = query_index(client, addr, index_name, "").await;
        if hits >= expected_hits {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!(
        "Timed out waiting for index '{}' on {} to reach at least {} hits",
        index_name, addr, expected_hits
    );
}

/// Copy from node-a must create destination on node-b within 2 seconds.
#[tokio::test]
async fn test_two_node_copy_index_replicates() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let requests: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "action": "addObject",
                "body": {"_id": format!("src_{}", i), "title": format!("Copy Source {}", i)}
            })
        })
        .collect();
    let seed_resp = client
        .post(format!("http://{}/1/indexes/src_idx/batch", addr_a))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, seed_resp).await;

    wait_for_hits_at_least(&client, &addr_b, "src_idx", 5).await;

    let copy_resp = client
        .post(format!("http://{}/1/indexes/src_idx/operation", addr_a))
        .json(&serde_json::json!({
            "operation": "copy",
            "destination": "dst_idx"
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, copy_resp).await;

    wait_for_hits_at_least(&client, &addr_b, "dst_idx", 5).await;

    let (_src_status, src_hits, _src_body) = query_index(&client, &addr_b, "src_idx", "").await;
    let (_dst_status, dst_hits, _dst_body) = query_index(&client, &addr_b, "dst_idx", "").await;
    assert!(
        src_hits >= 5,
        "source index should still have docs on node-b after copy; got {}",
        src_hits
    );
    assert!(
        dst_hits >= 5,
        "destination index should have docs on node-b after copy; got {}",
        dst_hits
    );
}

/// Scoped copy (settings only) must replicate to node-b within 2 seconds.
#[tokio::test]
async fn test_two_node_copy_index_scoped_replicates() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let requests: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "action": "addObject",
                "body": {"_id": format!("scoped_{}", i), "title": format!("Scoped Copy Source {}", i)}
            })
        })
        .collect();
    let seed_resp = client
        .post(format!("http://{}/1/indexes/src_idx/batch", addr_a))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, seed_resp).await;
    wait_for_hits_at_least(&client, &addr_b, "src_idx", 5).await;

    let set_settings_resp = client
        .put(format!("http://{}/1/indexes/src_idx/settings", addr_a))
        .json(&serde_json::json!({ "paginationLimitedTo": 4242 }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, set_settings_resp).await;
    let source_settings_ready =
        wait_for_pagination_limited_to(&client, &addr_a, "src_idx", 4242).await;
    assert!(
        source_settings_ready,
        "source settings on node-a did not stabilize to paginationLimitedTo=4242 within 2s"
    );

    let copy_resp = client
        .post(format!("http://{}/1/indexes/src_idx/operation", addr_a))
        .json(&serde_json::json!({
            "operation": "copy",
            "destination": "dst_idx",
            "scope": ["settings"]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, copy_resp).await;

    let mut last_settings_status = reqwest::StatusCode::INTERNAL_SERVER_ERROR;
    let mut last_pagination_limited_to = None;
    let mut last_dst_hits = 0;
    for _ in 0..200 {
        let dst_settings_resp = client
            .get(format!("http://{}/1/indexes/dst_idx/settings", addr_b))
            .send()
            .await
            .unwrap();
        let dst_settings_status = dst_settings_resp.status();
        let dst_settings_body = dst_settings_resp.json::<serde_json::Value>().await.unwrap();

        let (_dst_query_status, dst_hits, _dst_query_body) =
            query_index(&client, &addr_b, "dst_idx", "").await;

        let pagination_limited_to = dst_settings_body
            .get("paginationLimitedTo")
            .and_then(|v| v.as_u64());
        last_settings_status = dst_settings_status;
        last_pagination_limited_to = pagination_limited_to;
        last_dst_hits = dst_hits;
        if dst_settings_status.is_success() && pagination_limited_to == Some(4242) && dst_hits == 0
        {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!(
        "Scoped copy did not replicate settings-only result to node-b within 2s (expected paginationLimitedTo=4242 and nbHits=0; last status={}, last paginationLimitedTo={:?}, last nbHits={})",
        last_settings_status,
        last_pagination_limited_to,
        last_dst_hits
    );
}

/// Scoped copy (synonyms only) must replicate synonym records to node-b.
#[tokio::test]
async fn test_two_node_copy_index_scoped_synonyms_replicates() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let requests: Vec<serde_json::Value> = (0..3)
        .map(|i| {
            serde_json::json!({
                "action": "addObject",
                "body": {"_id": format!("syn_scope_{}", i), "title": format!("Syn Scope {}", i)}
            })
        })
        .collect();
    let seed_resp = client
        .post(format!("http://{}/1/indexes/src_idx/batch", addr_a))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, seed_resp).await;
    wait_for_hits_at_least(&client, &addr_b, "src_idx", 3).await;

    let set_settings_resp = client
        .put(format!("http://{}/1/indexes/src_idx/settings", addr_a))
        .json(&serde_json::json!({ "paginationLimitedTo": 4242 }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, set_settings_resp).await;
    assert!(
        wait_for_pagination_limited_to(&client, &addr_a, "src_idx", 4242).await,
        "source settings on node-a did not stabilize to paginationLimitedTo=4242 within 2s"
    );

    let save_synonym_resp = client
        .put(format!(
            "http://{}/1/indexes/src_idx/synonyms/syn-copy",
            addr_a
        ))
        .json(&serde_json::json!({
            "objectID": "syn-copy",
            "type": "synonym",
            "synonyms": ["tv", "television"]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, save_synonym_resp).await;

    let copy_resp = client
        .post(format!("http://{}/1/indexes/src_idx/operation", addr_a))
        .json(&serde_json::json!({
            "operation": "copy",
            "destination": "dst_idx",
            "scope": ["synonyms"]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, copy_resp).await;

    let dst_synonym = wait_for_json_success(
        &client,
        format!("http://{}/1/indexes/dst_idx/synonyms/syn-copy", addr_b),
        "destination synonym copy",
    )
    .await;
    assert_eq!(dst_synonym["objectID"], serde_json::json!("syn-copy"));

    let (_dst_query_status, dst_hits, _dst_query_body) =
        query_index(&client, &addr_b, "dst_idx", "").await;
    assert_eq!(dst_hits, 0, "synonyms-only copy should not copy records");

    let (_dst_settings_status, dst_settings_body) = get_json(
        &client,
        &format!("http://{}/1/indexes/dst_idx/settings", addr_b),
    )
    .await;
    assert_ne!(
        dst_settings_body
            .get("paginationLimitedTo")
            .and_then(|v| v.as_u64()),
        Some(4242),
        "synonyms-only copy should not copy source settings"
    );
}

/// Scoped copy (rules only) must replicate rules records to node-b.
#[tokio::test]
async fn test_two_node_copy_index_scoped_rules_replicates() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let requests: Vec<serde_json::Value> = (0..3)
        .map(|i| {
            serde_json::json!({
                "action": "addObject",
                "body": {"_id": format!("rule_scope_{}", i), "title": format!("Rule Scope {}", i)}
            })
        })
        .collect();
    let seed_resp = client
        .post(format!("http://{}/1/indexes/src_idx/batch", addr_a))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, seed_resp).await;
    wait_for_hits_at_least(&client, &addr_b, "src_idx", 3).await;

    let set_settings_resp = client
        .put(format!("http://{}/1/indexes/src_idx/settings", addr_a))
        .json(&serde_json::json!({ "paginationLimitedTo": 4242 }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, set_settings_resp).await;
    assert!(
        wait_for_pagination_limited_to(&client, &addr_a, "src_idx", 4242).await,
        "source settings on node-a did not stabilize to paginationLimitedTo=4242 within 2s"
    );

    let save_rule_resp = client
        .put(format!(
            "http://{}/1/indexes/src_idx/rules/rule-copy",
            addr_a
        ))
        .json(&serde_json::json!({
            "objectID": "rule-copy",
            "conditions": [{"anchoring": "contains", "pattern": "laptop"}],
            "consequence": {"params": {"query": "laptop computer"}}
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, save_rule_resp).await;

    let copy_resp = client
        .post(format!("http://{}/1/indexes/src_idx/operation", addr_a))
        .json(&serde_json::json!({
            "operation": "copy",
            "destination": "dst_idx",
            "scope": ["rules"]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, copy_resp).await;

    let dst_rule = wait_for_json_success(
        &client,
        format!("http://{}/1/indexes/dst_idx/rules/rule-copy", addr_b),
        "destination rule copy",
    )
    .await;
    assert_eq!(dst_rule["objectID"], serde_json::json!("rule-copy"));

    let (_dst_query_status, dst_hits, _dst_query_body) =
        query_index(&client, &addr_b, "dst_idx", "").await;
    assert_eq!(dst_hits, 0, "rules-only copy should not copy records");

    let (_dst_settings_status, dst_settings_body) = get_json(
        &client,
        &format!("http://{}/1/indexes/dst_idx/settings", addr_b),
    )
    .await;
    assert_ne!(
        dst_settings_body
            .get("paginationLimitedTo")
            .and_then(|v| v.as_u64()),
        Some(4242),
        "rules-only copy should not copy source settings"
    );
}

/// Move from node-a must rename index on node-b within 2 seconds.
#[tokio::test]
async fn test_two_node_move_index_replicates() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    // Missing-source move should be a harmless no-op on the peer apply path.
    let missing_move_resp = client
        .post(format!("http://{}/1/indexes/missing_src/operation", addr_a))
        .json(&serde_json::json!({
            "operation": "move",
            "destination": "missing_dst"
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, missing_move_resp).await;
    for _ in 0..200 {
        let (status, hits, _body) = query_index(&client, &addr_b, "missing_dst", "").await;
        if status == reqwest::StatusCode::NOT_FOUND || hits == 0 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    let requests: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "action": "addObject",
                "body": {"_id": format!("move_{}", i), "title": format!("Move Source {}", i)}
            })
        })
        .collect();
    let seed_resp = client
        .post(format!("http://{}/1/indexes/src_idx/batch", addr_a))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, seed_resp).await;
    wait_for_hits_at_least(&client, &addr_b, "src_idx", 5).await;

    let move_resp = client
        .post(format!("http://{}/1/indexes/src_idx/operation", addr_a))
        .json(&serde_json::json!({
            "operation": "move",
            "destination": "dst_idx"
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, move_resp).await;

    for _ in 0..200 {
        let (_dst_status, dst_hits, _dst_body) = query_index(&client, &addr_b, "dst_idx", "").await;
        let (src_status, src_hits, _src_body) = query_index(&client, &addr_b, "src_idx", "").await;
        let src_removed = src_status == reqwest::StatusCode::NOT_FOUND || src_hits == 0;
        if dst_hits >= 5 && src_removed {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!(
        "Move did not replicate to node-b within 2s (expected dst_idx>=5 hits and src_idx removed)"
    );
}

/// Catch-up safety: after src_idx is moved to dst_idx, requesting
/// /internal/ops for src_idx should still return source history up to the move op.
/// It must not leak destination writes created after the move boundary.
#[tokio::test]
async fn test_internal_ops_moved_source_fallback_stops_at_move_boundary() {
    let (addr, _tmp) = common::spawn_server_with_internal("node-a").await;
    let client = reqwest::Client::new();

    let seed_resp = client
        .post(format!("http://{}/1/indexes/src_idx/batch", addr))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "before_move", "title": "Before Move"}}]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr, seed_resp).await;

    let move_resp = client
        .post(format!("http://{}/1/indexes/src_idx/operation", addr))
        .json(&serde_json::json!({
            "operation": "move",
            "destination": "dst_idx"
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr, move_resp).await;

    // Destination write after move; must NOT appear in src_idx catch-up stream.
    let dst_write_resp = client
        .post(format!("http://{}/1/indexes/dst_idx/batch", addr))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "after_move", "title": "After Move"}}]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr, dst_write_resp).await;

    let ops_resp = client
        .get(format!(
            "http://{}/internal/ops?tenant_id=src_idx&since_seq=0",
            addr
        ))
        .send()
        .await
        .unwrap();
    assert!(
        ops_resp.status().is_success(),
        "moved source stream should remain catch-up readable"
    );
    let body = ops_resp.json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        body["tenant_id"].as_str(),
        Some("src_idx"),
        "fallback response should keep the requested source tenant id"
    );
    assert!(
        body["current_seq"].as_u64().is_some(),
        "fallback response should include current_seq"
    );
    assert!(
        body.get("oldest_retained_seq").is_none()
            || body.get("oldest_retained_seq") == Some(&serde_json::Value::Null),
        "moved-source fallback should not report retained-bound metadata from destination stream"
    );
    let ops = body["ops"]
        .as_array()
        .expect("ops should be an array in get_ops response");
    assert!(
        ops.iter()
            .any(|op| op["op_type"].as_str() == Some("move_index")),
        "source catch-up stream should include move_index operation"
    );
    assert_eq!(
        ops.last().and_then(|op| op["op_type"].as_str()),
        Some("move_index"),
        "source catch-up stream must stop at move boundary"
    );
    assert!(
        !ops.iter().any(|op| {
            op["payload"]
                .get("objectID")
                .and_then(|v| v.as_str())
                .map(|id| id == "after_move")
                .unwrap_or(false)
        }),
        "post-move destination writes must not leak into source catch-up stream"
    );
}

/// Clear on node-a must clear docs on node-b while preserving index existence.
#[tokio::test]
async fn test_two_node_clear_index_replicates() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let requests: Vec<serde_json::Value> = (0..5)
        .map(|i| {
            serde_json::json!({
                "action": "addObject",
                "body": {"_id": format!("clear_{}", i), "title": format!("Clear Source {}", i)}
            })
        })
        .collect();
    let seed_resp = client
        .post(format!("http://{}/1/indexes/testidx/batch", addr_a))
        .json(&serde_json::json!({ "requests": requests }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, seed_resp).await;
    wait_for_hits_at_least(&client, &addr_b, "testidx", 5).await;

    let clear_resp = client
        .post(format!("http://{}/1/indexes/testidx/clear", addr_a))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, clear_resp).await;

    for _ in 0..200 {
        let (query_status, hits, _query_body) = query_index(&client, &addr_b, "testidx", "").await;
        let settings_status = client
            .get(format!("http://{}/1/indexes/testidx/settings", addr_b))
            .send()
            .await
            .unwrap()
            .status();
        if query_status.is_success() && hits == 0 && settings_status.is_success() {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!(
        "Clear did not replicate to node-b within 2s (expected testidx nbHits==0 and settings endpoint 200)"
    );
}

/// Write to node-a via HTTP; doc must appear on node-b within 2 seconds.
#[tokio::test]
async fn test_two_node_write_replicates_to_peer() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{}/1/indexes/repltest/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "doc1", "title": "Saffron Pancakes"}}]
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "batch write failed: {}",
        resp.status()
    );

    // Poll node-b for up to 2 seconds (replication is async).
    for _ in 0..200 {
        let r = client
            .post(format!("http://{}/1/indexes/repltest/query", addr_b))
            .json(&serde_json::json!({"query": "Saffron"}))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        if r["nbHits"].as_u64().unwrap_or(0) >= 1 {
            return; // replication confirmed
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("'Saffron Pancakes' did not replicate from node-a to node-b within 2s");
}

/// Delete on node-a must propagate to node-b within 2 seconds.
#[tokio::test]
async fn test_two_node_delete_propagates_to_peer() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    // First write the doc and wait for it to appear on B.
    let resp = client
        .post(format!("http://{}/1/indexes/repltest/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "to-delete", "title": "Lavender Coffee"}}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    for _ in 0..200 {
        let r = client
            .post(format!("http://{}/1/indexes/repltest/query", addr_b))
            .json(&serde_json::json!({"query": "Lavender"}))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        if r["nbHits"].as_u64().unwrap_or(0) >= 1 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Delete on node-a.
    let del = client
        .delete(format!("http://{}/1/indexes/repltest/to-delete", addr_a))
        .send()
        .await
        .unwrap();
    assert!(del.status().is_success());

    // Poll node-b until the document is gone.
    for _ in 0..200 {
        let r = client
            .post(format!("http://{}/1/indexes/repltest/query", addr_b))
            .json(&serde_json::json!({"query": "Lavender"}))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        if r["nbHits"].as_u64().unwrap_or(0) == 0 {
            return; // deletion propagated
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("Delete did not propagate from node-a to node-b within 2s");
}

/// Write to node-b; doc must appear on node-a (bidirectional replication).
/// Also demonstrates why Phase 3 write forwarding is unnecessary: writes
/// on any node already propagate to all peers via trigger_replication().
#[tokio::test]
async fn test_two_node_bidirectional_replication() {
    let (addr_a, addr_b, _tmp_a, _tmp_b) = common::spawn_replication_pair("node-a", "node-b").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{}/1/indexes/bidir/batch", addr_b))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "b1", "title": "Cardamom Croissant"}}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    for _ in 0..200 {
        let r = client
            .post(format!("http://{}/1/indexes/bidir/query", addr_a))
            .json(&serde_json::json!({"query": "Cardamom"}))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap();
        if r["nbHits"].as_u64().unwrap_or(0) >= 1 {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("Bidirectional: doc written on node-b did not appear on node-a within 2s");
}

/// Startup catch-up: node-b fetches missed ops from node-a on startup via GET /internal/ops.
///
/// Tests ReplicationManager::catch_up_from_peer directly (bypasses the 3s startup
/// delay in spawn_startup_catchup). Verifies the full HTTP round-trip:
///   node-b calls /internal/ops on node-a → receives oplog entries →
///   apply_ops_to_manager → docs become searchable on node-b.
#[tokio::test]
async fn test_two_node_startup_catchup_via_get_ops() {
    use flapjack_replication::{
        config::{NodeConfig, PeerConfig},
        manager::ReplicationManager,
    };
    use tempfile::TempDir;

    // Only node-a runs as a server (serves /internal/ops).
    let (addr_a, _tmp_a) = common::spawn_server_with_internal("node-a").await;
    let client = reqwest::Client::new();

    // Write two docs to node-a.
    let resp = client
        .post(format!("http://{}/1/indexes/catchup/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [
                {"action": "addObject", "body": {"_id": "c1", "title": "Matcha Waffles"}},
                {"action": "addObject", "body": {"_id": "c2", "title": "Turmeric Toast"}}
            ]
        }))
        .send()
        .await
        .unwrap();
    // Wait for write queue to commit so the oplog has entries (no blind sleep).
    common::wait_for_response_task(&client, &addr_a, resp).await;

    // Node-b: starts fresh. Catch up from node-a using the replication manager.
    let tmp_b = TempDir::new().unwrap();
    let manager_b = flapjack::IndexManager::new(tmp_b.path());
    let repl_mgr_b = ReplicationManager::new(
        NodeConfig {
            node_id: "node-b".to_string(),
            bind_addr: "0.0.0.0:0".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-a".to_string(),
                addr: format!("http://{}", addr_a),
            }],
        },
        None,
        tmp_b.path().to_path_buf(),
    );

    let ops = repl_mgr_b
        .catch_up_from_peer("catchup", 0)
        .await
        .expect("catch_up_from_peer should succeed: node-a is reachable");

    assert!(
        ops.len() >= 2,
        "expected at least two oplog entries from node-a, got {}",
        ops.len()
    );
    let unique_seq_count = ops
        .iter()
        .map(|op| op.seq)
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    assert_eq!(
        unique_seq_count,
        ops.len(),
        "startup catch-up oplog entries must have unique sequence ids"
    );
    assert!(
        ops.iter().all(|op| op.tenant_id == "catchup"),
        "startup catch-up should only return ops for tenant catchup"
    );

    flapjack_http::handlers::internal::apply_ops_to_manager(&manager_b, "catchup", &ops)
        .await
        .unwrap();

    // Poll until write queue commits (no blind sleep).
    let mut matcha_ok = false;
    let mut turmeric_ok = false;
    for _ in 0..200 {
        if !matcha_ok {
            matcha_ok = manager_b
                .search("catchup", "Matcha", None, None, 10)
                .map(|r| r.total >= 1)
                .unwrap_or(false);
        }
        if !turmeric_ok {
            turmeric_ok = manager_b
                .search("catchup", "Turmeric", None, None, 10)
                .map(|r| r.total >= 1)
                .unwrap_or(false);
        }
        if matcha_ok && turmeric_ok {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    assert!(
        matcha_ok,
        "node-b should have 'Matcha Waffles' after startup catch-up"
    );
    assert!(
        turmeric_ok,
        "node-b should have 'Turmeric Toast' after startup catch-up"
    );
}

/// Restart catch-up: stop a node, write more docs on its peer, restart on the
/// same data dir, and verify all docs (including those written during downtime)
/// are queryable as soon as the restarted node is healthy.
///
/// RED state without pre-serve barrier: only docs from before the stop are present.
/// GREEN with pre-serve barrier: all docs are present immediately after restart.
#[tokio::test]
async fn test_restart_catches_up_before_serving() {
    let client = reqwest::Client::new();

    // 1. Spawn a stoppable pair with bidirectional replication.
    let (node_a, node_b, tmp_a, tmp_b) =
        common::spawn_stoppable_replication_pair("rst-a", "rst-b").await;

    // 2. Write 10 docs on A and wait for replication to B.
    let resp = client
        .post(format!(
            "http://{}/1/indexes/restart_test/batch",
            node_a.addr
        ))
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

    // Wait for push replication to deliver the docs to B.
    let mut initial_hits = 0;
    for _ in 0..200 {
        let r: serde_json::Value = client
            .post(format!(
                "http://{}/1/indexes/restart_test/query",
                node_b.addr
            ))
            .json(&serde_json::json!({"query": ""}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        initial_hits = r["nbHits"].as_u64().unwrap_or(0);
        if initial_hits >= 10 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        initial_hits, 10,
        "Node B should have the first 10 docs before shutdown; got {}",
        initial_hits
    );

    // 3. Stop node B.
    let _ = &tmp_b; // keep alive
    node_b.stop().await;

    // 4. Write 10 more docs on A (B is down — these go to A's oplog only).
    let resp = client
        .post(format!(
            "http://{}/1/indexes/restart_test/batch",
            node_a.addr
        ))
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

    // 5. Restart B on the same data dir, with A as its peer.
    let node_b_restarted = common::spawn_replication_node_on_existing_dir(
        tmp_b.path(),
        "rst-b",
        &format!("http://{}", node_a.addr),
        "rst-a",
    )
    .await;

    // 6. Immediately query B for all 20 docs.
    //    Pre-serve catch-up should have already fetched the 10 missing docs.
    let search_result: serde_json::Value = client
        .post(format!(
            "http://{}/1/indexes/restart_test/query",
            node_b_restarted.addr
        ))
        .json(&serde_json::json!({"query": ""}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let hits = search_result["nbHits"].as_u64().unwrap_or(0);
    assert_eq!(
        hits, 20,
        "Restarted node B should have all 20 docs immediately after health check; got {}",
        hits
    );
    let object_ids: std::collections::BTreeSet<String> = search_result["hits"]
        .as_array()
        .expect("search response should include hits array")
        .iter()
        .filter_map(|hit| hit["objectID"].as_str().map(|value| value.to_string()))
        .collect();
    assert_eq!(
        object_ids.len(),
        20,
        "restarted node should expose all expected objectIDs after catch-up"
    );
    assert!(
        (1..=20).all(|i| object_ids.contains(&format!("doc-{i}"))),
        "restarted node should include full doc-1..doc-20 range after catch-up"
    );

    // Cleanup: stop both nodes.
    node_b_restarted.stop().await;
    node_a.stop().await;
    drop(tmp_a);
    drop(tmp_b);
}

/// If peers are unavailable during bootstrap, the restarted replica must refuse
/// to become healthy rather than serving stale local data.
#[tokio::test]
async fn test_restart_refuses_to_serve_when_peer_is_unreachable() {
    let client = reqwest::Client::new();
    let (node_a, node_b, tmp_a, tmp_b) =
        common::spawn_stoppable_replication_pair("bootfail-a", "bootfail-b").await;

    let resp = client
        .post(format!(
            "http://{}/1/indexes/restart_fail/batch",
            node_a.addr
        ))
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
        let r: serde_json::Value = client
            .post(format!(
                "http://{}/1/indexes/restart_fail/query",
                node_b.addr
            ))
            .json(&serde_json::json!({"query": ""}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        initial_hits = r["nbHits"].as_u64().unwrap_or(0);
        if initial_hits >= 10 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        initial_hits, 10,
        "node B should have the first 10 docs before shutdown"
    );

    let node_a_addr = node_a.addr.clone();
    node_b.stop().await;

    let resp = client
        .post(format!(
            "http://{}/1/indexes/restart_fail/batch",
            node_a_addr
        ))
        .json(&serde_json::json!({
            "requests": (11..=20).map(|i| serde_json::json!({
                "action": "addObject",
                "body": {"objectID": format!("doc-{}", i), "num": i}
            })).collect::<Vec<_>>()
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &node_a_addr, resp).await;

    node_a.stop().await;

    let restart = common::try_spawn_replication_node_on_existing_dir(
        tmp_b.path(),
        "bootfail-b",
        &format!("http://{}", node_a_addr),
        "bootfail-a",
    )
    .await;

    assert!(
        restart.is_err(),
        "bootstrap should fail when the peer is unreachable instead of serving stale data"
    );

    let error = restart.err().unwrap();
    assert!(
        error.contains("Failed to fetch tenants")
            || error.contains("Failed to fetch ops")
            || error.contains("tripped circuit breakers"),
        "bootstrap failure should report peer catch-up failure, got: {}",
        error
    );

    drop(tmp_a);
    drop(tmp_b);
}

/// Retention-gap restart: if node-b's durable seq falls behind node-a's retained
/// oplog range, startup recovery must restore a full snapshot before serving.
#[tokio::test]
async fn test_restart_restores_snapshot_when_peer_oplog_is_compacted() {
    let client = reqwest::Client::new();
    let tenant = "retention_gap_test";

    let (node_a, node_b, tmp_a, tmp_b) =
        common::spawn_stoppable_replication_pair("gap-a", "gap-b").await;

    let resp = client
        .post(format!("http://{}/1/indexes/{}/batch", node_a.addr, tenant))
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
        let r: serde_json::Value = client
            .post(format!("http://{}/1/indexes/{}/query", node_b.addr, tenant))
            .json(&serde_json::json!({"query": ""}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        initial_hits = r["nbHits"].as_u64().unwrap_or(0);
        if initial_hits >= 10 {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;
    }
    assert_eq!(
        initial_hits, 10,
        "Node B should replicate the first 10 docs before shutdown; got {}",
        initial_hits
    );

    let b_committed_before_stop =
        flapjack::index::oplog::read_committed_seq(&tmp_b.path().join(tenant));
    assert_eq!(
        b_committed_before_stop, 10,
        "Node B durable committed_seq should be 10 before shutdown; got {}",
        b_committed_before_stop
    );

    node_b.stop().await;

    let resp = client
        .post(format!("http://{}/1/indexes/{}/batch", node_a.addr, tenant))
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

    let oplog_dir = tmp_a.path().join(tenant).join("oplog");
    let oplog = flapjack::index::oplog::OpLog::open(&oplog_dir, tenant, "gap-a").unwrap();
    // Force segment rotation in a deterministic way so truncate_before can drop
    // an entire historical segment and expose a true retention gap.
    let oversized_payload = "x".repeat(11 * 1024 * 1024);
    oplog
        .append(
            "noop",
            serde_json::json!({ "marker": "segment-rotate", "blob": oversized_payload }),
        )
        .expect("forcing segment rotation should succeed");
    for seq_marker in 22..=30 {
        oplog
            .append("noop", serde_json::json!({ "marker": seq_marker }))
            .expect("appending retained noop entries should succeed");
    }

    let removed_segments = oplog
        .truncate_before(22)
        .expect("truncate_before should succeed");
    assert!(
        removed_segments > 0,
        "Expected at least one compacted segment to be removed"
    );
    assert_eq!(
        oplog.oldest_seq(),
        Some(22),
        "Node A should retain only seq>=22 after compaction for this test scenario"
    );

    let node_b_restarted = common::spawn_replication_node_on_existing_dir(
        tmp_b.path(),
        "gap-b",
        &format!("http://{}", node_a.addr),
        "gap-a",
    )
    .await;

    let search_result: serde_json::Value = client
        .post(format!(
            "http://{}/1/indexes/{}/query",
            node_b_restarted.addr, tenant
        ))
        .json(&serde_json::json!({"query": ""}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let hits = search_result["nbHits"].as_u64().unwrap_or(0);
    assert_eq!(
        hits, 20,
        "Restarted node B must restore a full snapshot when oplog retention gap is detected; got {}",
        hits
    );

    node_b_restarted.stop().await;
    node_a.stop().await;
    drop(tmp_a);
    drop(tmp_b);
}

/// Fresh-node bootstrap: a never-used replica has committed_seq=0, but if the
/// peer has already compacted away early document ops, startup recovery must
/// restore a snapshot instead of applying only retained partial history.
#[tokio::test]
async fn test_fresh_node_bootstrap_restores_snapshot_when_peer_oplog_is_compacted() {
    let client = reqwest::Client::new();
    let tenant = "fresh_gap_test";
    let doc_count = 6u64;

    let (addr_a, tmp_a) = common::spawn_server_with_internal("fresh-gap-a").await;

    let resp = client
        .post(format!("http://{addr_a}/1/indexes/{tenant}/batch"))
        .json(&serde_json::json!({
            "requests": (1..=doc_count).map(|i| serde_json::json!({
                "action": "addObject",
                "body": {"objectID": format!("fresh-doc-{i}"), "num": i}
            })).collect::<Vec<_>>()
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, resp).await;

    let oplog_dir = tmp_a.path().join(tenant).join("oplog");
    let oplog = flapjack::index::oplog::OpLog::open(&oplog_dir, tenant, "fresh-gap-a").unwrap();
    let oversized_payload = "x".repeat(11 * 1024 * 1024);
    oplog
        .append(
            "noop",
            serde_json::json!({ "marker": "segment-rotate", "blob": oversized_payload }),
        )
        .expect("forcing segment rotation should succeed");
    for doc_id in 4..=doc_count {
        oplog
            .append(
                "upsert",
                serde_json::json!({
                    "body": {
                        "objectID": format!("fresh-doc-{doc_id}"),
                        "num": doc_id
                    }
                }),
            )
            .expect("appending retained duplicate upsert should succeed");
    }

    let removed_segments = oplog
        .truncate_before(8)
        .expect("truncate_before should succeed");
    assert!(
        removed_segments > 0,
        "Expected at least one compacted segment to be removed"
    );
    assert_eq!(
        oplog.oldest_seq(),
        Some(8),
        "Node A should retain only seq>=8 after compaction for this test scenario"
    );

    let tmp_b = TempDir::new().unwrap();
    assert_eq!(
        flapjack::index::oplog::read_committed_seq(&tmp_b.path().join(tenant)),
        0,
        "fresh node B must start from committed_seq=0 for this bootstrap reproduction"
    );

    let node_b = common::spawn_replication_node_on_existing_dir(
        tmp_b.path(),
        "fresh-gap-b",
        &format!("http://{addr_a}"),
        "fresh-gap-a",
    )
    .await;

    let query_resp = client
        .post(format!("http://{}/1/indexes/{tenant}/query", node_b.addr))
        .json(&serde_json::json!({"query": "", "hitsPerPage": doc_count}))
        .send()
        .await
        .unwrap();
    let query_status = query_resp.status();
    let query_body = query_resp.text().await.unwrap();
    assert!(
        query_status.is_success(),
        "fresh-node bootstrap query failed with status {query_status}: {query_body}"
    );
    let search_result: serde_json::Value = serde_json::from_str(&query_body).unwrap();

    let actual = search_result["nbHits"].as_u64().unwrap_or(0);
    let empty_hits = Vec::new();
    let actual_ids = search_result["hits"]
        .as_array()
        .unwrap_or(&empty_hits)
        .iter()
        .filter_map(|hit| hit["objectID"].as_str().map(str::to_string))
        .collect::<std::collections::BTreeSet<_>>();
    let expected_ids = (1..=doc_count)
        .map(|i| format!("fresh-doc-{i}"))
        .collect::<std::collections::BTreeSet<_>>();
    let missing_ids = expected_ids
        .difference(&actual_ids)
        .cloned()
        .collect::<Vec<_>>();

    assert_eq!(
        actual,
        doc_count,
        "fresh-node bootstrap should restore snapshot after peer compaction; actual {actual} < {doc_count}, missing {} docs: {:?}",
        doc_count.saturating_sub(actual),
        missing_ids
    );
    assert!(
        missing_ids.is_empty(),
        "fresh-node bootstrap lost objectIDs after peer compaction: {:?}",
        missing_ids
    );

    node_b.stop().await;
    drop(tmp_a);
    drop(tmp_b);
}

/// Fresh-node bootstrap: when peer history still starts at seq 1, a never-used
/// replica must replay ops and must not require the snapshot endpoint.
#[tokio::test]
async fn test_fresh_node_bootstrap_uses_op_replay_when_peer_untruncated() {
    use axum::{routing::get, Router};

    let client = reqwest::Client::new();
    let tenant = "fresh_replay_test";
    let expected_ids = ["replay-doc-1", "replay-doc-2", "replay-doc-3"];

    let mut tmp_a = common::TempDir::new().unwrap();
    let manager_a = IndexManager::new_with_node_id(tmp_a.path(), "fresh-replay-a");
    manager_a.create_tenant(tenant).unwrap();
    manager_a
        .add_documents_sync(
            tenant,
            expected_ids
                .iter()
                .enumerate()
                .map(|(offset, object_id)| {
                    Document::from_json(&serde_json::json!({
                        "objectID": object_id,
                        "title": format!("Replay Seed {}", offset + 1),
                        "rank": offset + 1
                    }))
                    .unwrap()
                })
                .collect(),
        )
        .await
        .unwrap();

    for object_id in expected_ids {
        assert!(
            manager_a.get_document(tenant, object_id).unwrap().is_some(),
            "peer A manager should own seeded document {object_id} before serving"
        );
    }
    let oplog = flapjack::index::oplog::OpLog::open(
        &tmp_a.path().join(tenant).join("oplog"),
        tenant,
        "fresh-replay-a",
    )
    .unwrap();
    assert_eq!(
        oplog.oldest_seq(),
        Some(1),
        "peer A must retain history from the first oplog entry"
    );

    let state = make_test_app_state(
        tmp_a.path(),
        Some(Arc::clone(&manager_a)),
        None,
        None,
        None,
        None,
        None,
    );
    let app = common::apply_test_app_id_layer(
        Router::new()
            .merge(flapjack_http::router::build_public_health_routes().with_state(state.clone()))
            .merge(
                Router::new()
                    .route(
                        "/internal/ops",
                        get(flapjack_http::handlers::internal::get_ops),
                    )
                    .route(
                        "/internal/tenants",
                        get(flapjack_http::handlers::internal::list_tenants),
                    )
                    .with_state(state),
            ),
    );
    let addr_a = common::spawn_router(app, &mut tmp_a).await;

    let tmp_b = common::TempDir::new().unwrap();
    assert_eq!(
        flapjack::index::oplog::read_committed_seq(&tmp_b.path().join(tenant)),
        0,
        "fresh node B must start from committed_seq=0 before bootstrap"
    );
    let node_b = common::try_spawn_replication_node_on_existing_dir(
        tmp_b.path(),
        "fresh-replay-b",
        &format!("http://{addr_a}"),
        "fresh-replay-a",
    )
    .await
    .expect("fresh node bootstrap should use op replay without calling the missing snapshot route");

    let node_b_addr = node_b.addr.clone();
    let query_resp = client
        .post(format!("http://{node_b_addr}/1/indexes/{tenant}/query"))
        .json(&serde_json::json!({"query": "", "hitsPerPage": expected_ids.len()}))
        .send()
        .await
        .unwrap();
    let query_status = query_resp.status();
    let query_body = query_resp.text().await.unwrap();
    assert!(
        query_status.is_success(),
        "fresh replay bootstrap query failed with status {query_status}: {query_body}"
    );
    let search_result: serde_json::Value = serde_json::from_str(&query_body).unwrap();
    let empty_hits = Vec::new();
    let actual_ids = search_result["hits"]
        .as_array()
        .unwrap_or(&empty_hits)
        .iter()
        .filter_map(|hit| hit["objectID"].as_str().map(str::to_string))
        .collect::<std::collections::BTreeSet<_>>();
    let expected_ids = expected_ids
        .into_iter()
        .map(str::to_string)
        .collect::<std::collections::BTreeSet<_>>();

    assert_eq!(
        search_result["nbHits"].as_u64(),
        Some(expected_ids.len() as u64),
        "fresh replay bootstrap should expose exactly the seeded document count"
    );
    assert_eq!(
        actual_ids, expected_ids,
        "fresh replay bootstrap should preserve the exact objectID set"
    );

    node_b.stop().await;
    drop(tmp_a);
    drop(tmp_b);
}

// ============================================================
// Phase 4: Analytics Rollup Exchange integration test.
//
// Unit tests for AnalyticsRollup / RollupCache live inline in
// analytics_cluster.rs (the rollup_tests module). This test
// covers the HTTP exchange endpoint end-to-end.
// ============================================================

/// POST /internal/analytics-rollup is accessible and stores the rollup.
/// Verifies: endpoint accepts the payload, returns 200, no auth required.
#[tokio::test]
async fn test_analytics_rollup_exchange_endpoint_accepts_rollup() {
    use std::time::{SystemTime, UNIX_EPOCH};

    let (addr, _tmp) = common::spawn_server_with_internal("node-a").await;
    let client = reqwest::Client::new();

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let rollup = serde_json::json!({
        "node_id": "peer-node",
        "index": "my-index",
        "generated_at_secs": now_secs,
        "results": {
            "searches": {"searches": [], "total": 0}
        }
    });

    let resp = client
        .post(format!("http://{}/internal/analytics-rollup", addr))
        .json(&rollup)
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "Exchange endpoint should return 200 OK (no auth required)"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
}

// ============================================================
// Phase 4b: Rollup Broadcaster tests.
//
// Tests the background rollup push task: computes analytics
// locally and POSTs AnalyticsRollup to each peer's
// /internal/analytics-rollup endpoint every N seconds.
//
// NEW endpoints covered:
//   GET /internal/rollup-cache  → inspect cached rollups (diagnostic)
//
// NEW functions covered:
//   rollup_broadcaster::run_rollup_broadcast()
//   rollup_broadcaster::discover_indexes()
//   rollup_broadcaster::spawn_rollup_broadcaster()
//   AnalyticsClusterClient::push_rollup_to_peers()
// ============================================================

/// GET /internal/rollup-cache returns 200 with count=0 on a fresh node.
/// RED: Fails until /internal/rollup-cache route is registered.
#[tokio::test]
async fn test_rollup_cache_status_endpoint_empty() {
    // Clear global cache to prevent state leakage from other tests
    flapjack_http::analytics_cluster::get_global_rollup_cache().clear();

    let (addr, _tmp) = common::spawn_server_with_internal("node-cache-empty").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("http://{}/internal/rollup-cache", addr))
        .send()
        .await
        .unwrap();

    assert_eq!(
        resp.status(),
        200,
        "/internal/rollup-cache should return 200 on a fresh node"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(
        body["count"],
        serde_json::json!(0),
        "Fresh node should have 0 cached rollups; got: {}",
        body
    );
    assert!(
        body["entries"].is_array(),
        "Response should have an 'entries' array"
    );
}

/// POST to /internal/analytics-rollup stores the rollup; GET /internal/rollup-cache reflects it.
/// RED: Fails (404) until /internal/rollup-cache route is registered.
#[tokio::test]
async fn test_rollup_cache_status_reflects_stored_rollup() {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Clear global cache to prevent state leakage from other tests
    flapjack_http::analytics_cluster::get_global_rollup_cache().clear();

    let (addr, _tmp) = common::spawn_server_with_internal("node-cache-reflect").await;
    let client = reqwest::Client::new();

    // POST a rollup
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rollup = serde_json::json!({
        "node_id": "peer-broadcaster",
        "index": "my-index",
        "generated_at_secs": now,
        "results": {
            "searches": {"searches": [{"search": "hat", "count": 5, "nbHits": 10}], "total": 1}
        }
    });
    let post_resp = client
        .post(format!("http://{}/internal/analytics-rollup", addr))
        .json(&rollup)
        .send()
        .await
        .unwrap();
    assert_eq!(
        post_resp.status(),
        200,
        "POST /internal/analytics-rollup should return 200"
    );

    // GET rollup cache — must see the stored rollup
    let get_resp = client
        .get(format!("http://{}/internal/rollup-cache", addr))
        .send()
        .await
        .unwrap();
    assert_eq!(
        get_resp.status(),
        200,
        "/internal/rollup-cache should return 200"
    );
    let body: serde_json::Value = get_resp.json().await.unwrap();

    assert_eq!(
        body["count"],
        serde_json::json!(1),
        "Cache should contain 1 entry after POST; got: {}",
        body
    );
    let entries = body["entries"]
        .as_array()
        .expect("entries must be an array");
    assert_eq!(entries[0]["node_id"], "peer-broadcaster");
    assert_eq!(entries[0]["index"], "my-index");
}

/// run_rollup_broadcast() discovers a seeded index and pushes an AnalyticsRollup to the peer.
/// RED: Fails to compile until rollup_broadcaster module exists.
#[tokio::test]
async fn test_run_rollup_broadcast_sends_to_peer() {
    use flapjack_replication::config::{NodeConfig, PeerConfig};

    // Start a peer node that can receive rollups
    let (addr_b, _tmp_b) = common::spawn_server_with_internal("node-b-recv").await;

    // Set up node-a with a real analytics directory
    let tmp_a = TempDir::new().unwrap();
    let analytics_config = flapjack::analytics::AnalyticsConfig {
        enabled: true,
        data_dir: tmp_a.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    };

    // Seed analytics data so discover_indexes() finds "products"
    flapjack::analytics::seed::seed_analytics(&analytics_config, "products", 1)
        .expect("seed_analytics must succeed");

    let engine = Arc::new(flapjack::analytics::AnalyticsQueryEngine::new(
        analytics_config.clone(),
    ));

    // Build a cluster client pointing at node-b
    let node_cfg = NodeConfig {
        node_id: "node-a-send".to_string(),
        bind_addr: "127.0.0.1:0".to_string(),
        advertise_addr: None,
        bootstrap_peer: None,
        peers: vec![PeerConfig {
            node_id: "node-b-recv".to_string(),
            addr: format!("http://{}", addr_b),
        }],
    };
    let cluster = flapjack_http::analytics_cluster::AnalyticsClusterClient::new(&node_cfg)
        .expect("Should build cluster client with one peer");

    // Run one broadcast cycle (synchronous, no spawn needed for unit testing)
    flapjack_http::rollup_broadcaster::run_rollup_broadcast(
        &engine,
        &analytics_config,
        &cluster,
        "node-a-send",
    )
    .await;

    // Verify node-b has the rollup in its cache
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://{}/internal/rollup-cache", addr_b))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "/internal/rollup-cache should return 200"
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    let count = body["count"].as_u64().unwrap_or(0);
    assert!(
        count > 0,
        "node-b rollup cache should have ≥1 entry after broadcast; got: {}",
        body
    );

    // Verify the rollup is from node-a for index=products
    let entries = body["entries"].as_array().expect("entries must be array");
    assert!(
        entries
            .iter()
            .any(|e| e["node_id"] == "node-a-send" && e["index"] == "products"),
        "Expected a rollup from node-a-send for index 'products'; got: {}",
        body
    );
}

/// spawn_rollup_broadcaster periodically pushes rollups to peers.
/// RED: Fails to compile until rollup_broadcaster module exists.
#[tokio::test]
async fn test_rollup_broadcaster_integration_periodic() {
    use flapjack_replication::config::{NodeConfig, PeerConfig};

    // Start the receiving peer
    let (addr_b, _tmp_b) = common::spawn_server_with_internal("node-b-periodic").await;

    // Set up node-a with seeded analytics
    let tmp_a = TempDir::new().unwrap();
    let analytics_config = flapjack::analytics::AnalyticsConfig {
        enabled: true,
        data_dir: tmp_a.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 100_000,
        retention_days: 90,
    };
    flapjack::analytics::seed::seed_analytics(&analytics_config, "widgets", 1)
        .expect("seed_analytics must succeed");

    let engine = Arc::new(flapjack::analytics::AnalyticsQueryEngine::new(
        analytics_config.clone(),
    ));

    let node_cfg = NodeConfig {
        node_id: "node-a-periodic".to_string(),
        bind_addr: "127.0.0.1:0".to_string(),
        advertise_addr: None,
        bootstrap_peer: None,
        peers: vec![PeerConfig {
            node_id: "node-b-periodic".to_string(),
            addr: format!("http://{}", addr_b),
        }],
    };
    let cluster = flapjack_http::analytics_cluster::AnalyticsClusterClient::new(&node_cfg)
        .expect("Should build cluster client");

    // Spawn broadcaster with a 1-second interval
    flapjack_http::rollup_broadcaster::spawn_rollup_broadcaster(
        Arc::clone(&engine),
        analytics_config,
        cluster,
        "node-a-periodic".to_string(),
        1, // 1s interval for test speed
    );

    // Wait up to 4 seconds for the broadcaster to fire at least once
    let client = reqwest::Client::new();
    let mut count = 0u64;
    for _ in 0..40 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let resp = client
            .get(format!("http://{}/internal/rollup-cache", addr_b))
            .send()
            .await
            .unwrap();
        if resp.status() == 200 {
            let body: serde_json::Value = resp.json().await.unwrap();
            count = body["count"].as_u64().unwrap_or(0);
            if count > 0 {
                break;
            }
        }
    }
    assert!(
        count > 0,
        "Broadcaster should have fired within 4s and pushed rollup to node-b"
    );
}

// ============================================================
// P0: Periodic Anti-Entropy Sync tests
//
// Verifies that the periodic sync mechanism pulls missed ops from
// peers, closing the network partition gap identified in
// HA_HARDENING_HANDOFF.md.
//
// These tests exercise:
//   run_periodic_catchup()   — one-shot catch-up for all tenants
//   spawn_periodic_sync()    — background task that fires on a timer
//
// RED phase: tests fail because the stub functions are empty.
// GREEN phase: tests pass after implementing the real logic.
// ============================================================

/// Core periodic sync test: node-b pulls missed ops from node-a without restart.
/// Simulates a partition gap: node-a has docs that node-b missed.
/// run_periodic_catchup() must detect the gap and fill it.
///
/// RED: Fails because stub run_periodic_catchup() does nothing.
#[tokio::test]
async fn test_periodic_sync_pulls_missed_ops_from_peer() {
    use flapjack_replication::{
        config::{NodeConfig, PeerConfig},
        manager::ReplicationManager,
    };

    // 1. Start node-a as a server (serves /internal/ops)
    let (addr_a, _tmp_a) = common::spawn_server_with_internal("node-a-sync").await;
    let client = reqwest::Client::new();

    // 2. Write docs to node-a (simulating writes during a partition)
    let resp = client
        .post(format!("http://{}/1/indexes/sync-test/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [
                {"action": "addObject", "body": {"_id": "s1", "title": "Espresso Martini"}},
                {"action": "addObject", "body": {"_id": "s2", "title": "Cold Brew Float"}}
            ]
        }))
        .send()
        .await
        .unwrap();
    // Wait for writes to commit to oplog (no blind sleep).
    common::wait_for_response_task(&client, &addr_a, resp).await;

    // 3. Set up node-b: has the tenant dir but seq=0 (missed node-a's writes)
    let tmp_b = tempfile::TempDir::new().unwrap();
    let manager_b = flapjack::IndexManager::new(tmp_b.path());
    manager_b.create_tenant("sync-test").unwrap();

    let repl_mgr_b = ReplicationManager::new(
        NodeConfig {
            node_id: "node-b-sync".to_string(),
            bind_addr: "0.0.0.0:0".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-a-sync".to_string(),
                addr: format!("http://{}", addr_a),
            }],
        },
        None,
        tmp_b.path().to_path_buf(),
    );

    let state_b = make_test_app_state(
        tmp_b.path(),
        Some(manager_b.clone()),
        None,
        Some(repl_mgr_b),
        None,
        None,
        None,
    );

    // 4. Run periodic catchup — should pull missed ops from node-a
    flapjack_http::startup_catchup::run_periodic_catchup(Arc::clone(&state_b)).await;

    // 5. Poll until node-b has both docs (write queue is async)
    let mut doc1_ok = false;
    let mut doc2_ok = false;
    for _ in 0..200 {
        if !doc1_ok {
            doc1_ok = manager_b
                .search("sync-test", "Espresso", None, None, 10)
                .map(|r| r.total >= 1)
                .unwrap_or(false);
        }
        if !doc2_ok {
            doc2_ok = manager_b
                .search("sync-test", "Cold Brew", None, None, 10)
                .map(|r| r.total >= 1)
                .unwrap_or(false);
        }
        if doc1_ok && doc2_ok {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    assert!(
        doc1_ok,
        "node-b should have 'Espresso Martini' after periodic sync"
    );
    assert!(
        doc2_ok,
        "node-b should have 'Cold Brew Float' after periodic sync"
    );
}

/// Periodic sync works across multiple tenants — not just the first one found.
///
/// RED: Fails because stub run_periodic_catchup() does nothing.
#[tokio::test]
async fn test_periodic_sync_catches_up_multiple_tenants() {
    use flapjack_replication::{
        config::{NodeConfig, PeerConfig},
        manager::ReplicationManager,
    };

    let (addr_a, _tmp_a) = common::spawn_server_with_internal("node-a-multi").await;
    let client = reqwest::Client::new();

    // Write docs to two different tenants on node-a
    let resp1 = client
        .post(format!("http://{}/1/indexes/tenant-alpha/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "a1", "title": "Maple Syrup"}}]
        }))
        .send()
        .await
        .unwrap();
    // Wait for writes to commit to oplog (no blind sleep).
    common::wait_for_response_task(&client, &addr_a, resp1).await;

    let resp2 = client
        .post(format!("http://{}/1/indexes/tenant-beta/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "b1", "title": "Vanilla Extract"}}]
        }))
        .send()
        .await
        .unwrap();
    // Wait for writes to commit to oplog (no blind sleep).
    common::wait_for_response_task(&client, &addr_a, resp2).await;

    // Node-b has both tenant dirs but no docs
    let tmp_b = tempfile::TempDir::new().unwrap();
    let manager_b = flapjack::IndexManager::new(tmp_b.path());
    manager_b.create_tenant("tenant-alpha").unwrap();
    manager_b.create_tenant("tenant-beta").unwrap();

    let repl_mgr_b = ReplicationManager::new(
        NodeConfig {
            node_id: "node-b-multi".to_string(),
            bind_addr: "0.0.0.0:0".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-a-multi".to_string(),
                addr: format!("http://{}", addr_a),
            }],
        },
        None,
        tmp_b.path().to_path_buf(),
    );

    let state_b = make_test_app_state(
        tmp_b.path(),
        Some(manager_b.clone()),
        None,
        Some(repl_mgr_b),
        None,
        None,
        None,
    );

    flapjack_http::startup_catchup::run_periodic_catchup(Arc::clone(&state_b)).await;

    // Poll until both tenants have their docs (write queue is async)
    let mut alpha_ok = false;
    let mut beta_ok = false;
    for _ in 0..200 {
        if !alpha_ok {
            alpha_ok = manager_b
                .search("tenant-alpha", "Maple", None, None, 10)
                .map(|r| r.total >= 1)
                .unwrap_or(false);
        }
        if !beta_ok {
            beta_ok = manager_b
                .search("tenant-beta", "Vanilla", None, None, 10)
                .map(|r| r.total >= 1)
                .unwrap_or(false);
        }
        if alpha_ok && beta_ok {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    assert!(
        alpha_ok,
        "tenant-alpha should have 'Maple Syrup' after periodic sync"
    );
    assert!(
        beta_ok,
        "tenant-beta should have 'Vanilla Extract' after periodic sync"
    );
}

/// Periodic sync must discover and recover tenants that only exist on peers.
///
/// Regression target: startup/periodic catch-up used to scan only local tenant
/// directories, skipping peer-created tenants after downtime.
#[tokio::test]
async fn test_periodic_sync_discovers_peer_only_tenant() {
    use flapjack_replication::{
        config::{NodeConfig, PeerConfig},
        manager::ReplicationManager,
    };

    let (addr_a, _tmp_a) = common::spawn_server_with_internal("node-a-peer-only").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{}/1/indexes/peer-only/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "p1", "title": "Saffron Bun"}}]
        }))
        .send()
        .await
        .unwrap();
    common::wait_for_response_task(&client, &addr_a, resp).await;

    // Node-b starts with no local tenant directories.
    let tmp_b = tempfile::TempDir::new().unwrap();
    let manager_b = flapjack::IndexManager::new(tmp_b.path());

    let repl_mgr_b = ReplicationManager::new(
        NodeConfig {
            node_id: "node-b-peer-only".to_string(),
            bind_addr: "0.0.0.0:0".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-a-peer-only".to_string(),
                addr: format!("http://{}", addr_a),
            }],
        },
        None,
        tmp_b.path().to_path_buf(),
    );

    let state_b = make_test_app_state(
        tmp_b.path(),
        Some(manager_b.clone()),
        None,
        Some(repl_mgr_b),
        None,
        None,
        None,
    );

    flapjack_http::startup_catchup::run_periodic_catchup(Arc::clone(&state_b)).await;

    let mut found = false;
    for _ in 0..200 {
        if manager_b
            .search("peer-only", "Saffron", None, None, 10)
            .map(|r| r.total)
            .unwrap_or(0)
            >= 1
        {
            found = true;
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    assert!(
        found,
        "peer-only tenant should be discovered from peer and recovered during periodic sync"
    );
}

/// spawn_periodic_sync fires the sync task within the configured interval.
/// Uses a 1s interval and verifies data is pulled within 4s.
///
/// RED: Fails because stub spawn_periodic_sync() does nothing.
#[tokio::test]
async fn test_spawn_periodic_sync_fires_within_interval() {
    use flapjack_replication::{
        config::{NodeConfig, PeerConfig},
        manager::ReplicationManager,
    };

    // Start node-a with a doc
    let (addr_a, _tmp_a) = common::spawn_server_with_internal("node-a-spawn").await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("http://{}/1/indexes/spawn-test/batch", addr_a))
        .json(&serde_json::json!({
            "requests": [{"action": "addObject", "body": {"_id": "p1", "title": "Pistachio Latte"}}]
        }))
        .send()
        .await
        .unwrap();
    // Wait for writes to commit to oplog (no blind sleep).
    common::wait_for_response_task(&client, &addr_a, resp).await;

    // Node-b: tenant exists but no docs
    let tmp_b = tempfile::TempDir::new().unwrap();
    let manager_b = flapjack::IndexManager::new(tmp_b.path());
    manager_b.create_tenant("spawn-test").unwrap();

    let repl_mgr_b = ReplicationManager::new(
        NodeConfig {
            node_id: "node-b-spawn".to_string(),
            bind_addr: "0.0.0.0:0".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: vec![PeerConfig {
                node_id: "node-a-spawn".to_string(),
                addr: format!("http://{}", addr_a),
            }],
        },
        None,
        tmp_b.path().to_path_buf(),
    );

    let state_b = make_test_app_state(
        tmp_b.path(),
        Some(manager_b.clone()),
        None,
        Some(repl_mgr_b),
        None,
        None,
        None,
    );

    // Spawn periodic sync with 1s interval
    flapjack_http::startup_catchup::spawn_periodic_sync(Arc::clone(&state_b), 1);

    // Poll node-b for up to 5s — the spawned task should fire and pull docs
    let mut found = false;
    for _ in 0..50 {
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        if manager_b
            .search("spawn-test", "Pistachio", None, None, 10)
            .map(|r| r.total)
            .unwrap_or(0)
            >= 1
        {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "spawn_periodic_sync should have fired within 5s and pulled 'Pistachio Latte' to node-b"
    );
}

// ── Authenticated replication tests ────────────────────────────────────────

/// Verify that /internal/replicate rejects requests without auth headers
/// when the server has authentication enabled.
#[tokio::test]
async fn test_authenticated_replication_rejects_no_auth() {
    let admin_key = "test-repl-auth-key-001";
    let (addr, _tmp) = common::spawn_server_with_key(Some(admin_key)).await;
    let client = reqwest::Client::new();

    // No auth headers → 403
    let resp = client
        .post(format!("http://{}/internal/replicate", addr))
        .json(&serde_json::json!({
            "tenant_id": "test-tenant",
            "ops": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "Request without auth headers should be rejected"
    );

    // Wrong key → 403
    let resp = client
        .post(format!("http://{}/internal/replicate", addr))
        .header("x-algolia-api-key", "wrong-key")
        .header("x-algolia-application-id", "flapjack-replication")
        .json(&serde_json::json!({
            "tenant_id": "test-tenant",
            "ops": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "Request with wrong key should be rejected"
    );

    // Missing application-id → 403
    let resp = client
        .post(format!("http://{}/internal/replicate", addr))
        .header("x-algolia-api-key", admin_key)
        .json(&serde_json::json!({
            "tenant_id": "test-tenant",
            "ops": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        403,
        "Request without application-id should be rejected"
    );

    // Correct admin key + application-id → 200
    let resp = client
        .post(format!("http://{}/internal/replicate", addr))
        .header("x-algolia-api-key", admin_key)
        .header("x-algolia-application-id", "flapjack-replication")
        .json(&serde_json::json!({
            "tenant_id": "test-tenant",
            "ops": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "Request with correct admin key should succeed"
    );
}

#[tokio::test]
async fn test_peer_client_list_tenants_requires_auth_and_returns_visible_sorted_tenants() {
    let admin_key = "test-repl-list-tenants-key-004";
    let (addr, temp_dir) = common::spawn_server_with_key(Some(admin_key)).await;
    let client = reqwest::Client::new();

    for (index_name, object_id, title) in [
        ("tenant-z", "z1", "Zaatar Biscuit"),
        ("tenant-a", "a1", "Anise Cookie"),
    ] {
        let resp = client
            .post(format!("http://{}/1/indexes/{}/batch", addr, index_name))
            .header("x-algolia-api-key", admin_key)
            .header("x-algolia-application-id", "test")
            .json(&serde_json::json!({
                "requests": [{
                    "action": "addObject",
                    "body": { "objectID": object_id, "title": title }
                }]
            }))
            .send()
            .await
            .unwrap();
        common::wait_for_response_task_authed(&client, &addr, resp, Some(admin_key)).await;
    }

    std::fs::create_dir_all(temp_dir.path().join(".hidden-peer-dir")).unwrap();
    std::fs::write(temp_dir.path().join("not-a-directory.txt"), "ignore-me").unwrap();

    let wrong_key_peer = flapjack_replication::peer::PeerClient::new(
        "peer-auth-wrong".to_string(),
        format!("http://{}", addr),
        Some("wrong-admin-key".to_string()),
    );
    let wrong_key_result = wrong_key_peer.list_tenants().await;
    assert!(
        wrong_key_result.is_err(),
        "list_tenants with wrong key should fail"
    );
    let wrong_key_error = wrong_key_result.unwrap_err();
    assert!(
        wrong_key_error.contains("403"),
        "wrong-key list_tenants error should include HTTP status 403, got: {}",
        wrong_key_error
    );

    let good_key_peer = flapjack_replication::peer::PeerClient::new(
        "peer-auth-ok".to_string(),
        format!("http://{}", addr),
        Some(admin_key.to_string()),
    );
    let tenants = good_key_peer
        .list_tenants()
        .await
        .expect("list_tenants with valid key should succeed")
        .tenants;

    assert!(
        tenants.windows(2).all(|pair| pair[0] <= pair[1]),
        "tenant list should be sorted lexicographically, got: {:?}",
        tenants
    );
    assert!(
        tenants.contains(&"tenant-a".to_string()) && tenants.contains(&"tenant-z".to_string()),
        "tenant list should include tenants created via write API, got: {:?}",
        tenants
    );
    assert!(
        !tenants.iter().any(|tenant| tenant.starts_with('.')),
        "tenant list should not include hidden directories, got: {:?}",
        tenants
    );
    assert!(
        !tenants.iter().any(|tenant| tenant == "not-a-directory.txt"),
        "tenant list should not include plain files, got: {:?}",
        tenants
    );
}

/// End-to-end: write a document on authenticated node A, verify it replicates
/// to authenticated node B via PeerClient with admin key injection.
#[tokio::test]
async fn test_authenticated_two_node_replication() {
    let admin_key = "test-repl-e2e-key-002";
    let (addr_a, addr_b, key, _tmp_a, _tmp_b) =
        common::spawn_authenticated_replication_pair("auth-node-a", "auth-node-b", admin_key).await;
    let client = reqwest::Client::new();

    // Write a document on node A (must send auth headers)
    let resp = client
        .post(format!("http://{}/1/indexes/auth-test/batch", addr_a))
        .header("x-algolia-api-key", &key)
        .header("x-algolia-application-id", "test")
        .json(&serde_json::json!({
            "requests": [{
                "action": "addObject",
                "body": { "objectID": "auth-obj-1", "name": "Auth Test Document" }
            }]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "Batch write should succeed");
    common::wait_for_response_task_authed(&client, &addr_a, resp, Some(&key)).await;

    // Poll node B until the document appears (PeerClient should have
    // injected auth headers automatically during replication)
    let mut found = false;
    for _ in 0..200 {
        let resp = client
            .post(format!("http://{}/1/indexes/auth-test/query", addr_b))
            .header("x-algolia-api-key", &key)
            .header("x-algolia-application-id", "test")
            .json(&serde_json::json!({ "query": "Auth Test" }))
            .send()
            .await
            .unwrap();
        if resp.status().is_success() {
            let body: serde_json::Value = resp.json().await.unwrap();
            if body["nbHits"].as_u64().unwrap_or(0) >= 1 {
                found = true;
                break;
            }
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
    }
    assert!(
        found,
        "Document written on authenticated node A should replicate to authenticated node B"
    );
}

/// Verify that a PeerClient with the wrong admin key cannot replicate to
/// an authenticated server — the replication attempt returns an error.
#[tokio::test]
async fn test_peer_client_wrong_key_replication_fails() {
    use flapjack_replication::types::ReplicateOpsRequest;

    let correct_key = "test-repl-wrong-key-003";
    let (addr, _tmp) = common::spawn_server_with_key(Some(correct_key)).await;

    // Create a PeerClient with a WRONG admin key
    let peer = flapjack_replication::peer::PeerClient::new(
        "rogue-peer".to_string(),
        format!("http://{}", addr),
        Some("completely-wrong-key".to_string()),
    );

    let result = peer
        .replicate_ops(ReplicateOpsRequest {
            tenant_id: "test-tenant".to_string(),
            ops: vec![],
        })
        .await;

    assert!(
        result.is_err(),
        "Replication with wrong key should return an error, got: {:?}",
        result
    );
    let err = result.unwrap_err();
    assert!(
        err.contains("403"),
        "Error should mention 403 status, got: {}",
        err
    );
}
