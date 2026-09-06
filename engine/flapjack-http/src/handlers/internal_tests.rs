//! Stub summary for engine/flapjack-http/src/handlers/internal_tests.rs.
use super::*;
use flapjack::index::oplog::OpLogEntry;
use flapjack::index::settings::IndexSettings;
use flapjack::index::version_store::{VersionRecord, VersionStore};
use flapjack::index::SearchOptions;
use flapjack::types::{Document, FacetRequest};
use flapjack::IndexManager;
use std::path::Path;
use tempfile::TempDir;

#[tokio::test]
async fn release_write_fence_requires_one_exact_transaction_for_acquire_and_release() {
    let temp = TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&temp).build_shared();

    let acquired = acquire_release_write_fence(
        State(Arc::clone(&state)),
        Json(ReleaseWriteFenceRequest {
            transaction_id: "release-contract-1".to_string(),
        }),
    )
    .await;
    assert_eq!(acquired.status(), StatusCode::OK);

    let conflict = release_release_write_fence(
        State(Arc::clone(&state)),
        Json(ReleaseWriteFenceRequest {
            transaction_id: "release-contract-2".to_string(),
        }),
    )
    .await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert_eq!(
        state
            .global_mutation_fence
            .status()
            .await
            .unwrap()
            .transaction_id,
        "release-contract-1"
    );

    let released = release_release_write_fence(
        State(Arc::clone(&state)),
        Json(ReleaseWriteFenceRequest {
            transaction_id: "release-contract-1".to_string(),
        }),
    )
    .await;
    assert_eq!(released.status(), StatusCode::OK);
    assert!(state.global_mutation_fence.status().await.is_none());
}

#[tokio::test]
async fn release_inventory_is_sorted_and_fails_closed_on_missing_counts() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("zeta").unwrap();
    manager.create_tenant("alpha").unwrap();
    manager.unload_tenant("zeta");

    assert!(
        canonical_release_inventory(&manager).is_err(),
        "a durable but unloaded tenant must not disappear from release inventory"
    );
    prepare_release_inventory(&manager).unwrap();

    let inventory = canonical_release_inventory(&manager).unwrap();

    assert_eq!(
        serde_json::to_value(inventory).unwrap(),
        serde_json::json!([
            {"indexId": "alpha", "documentCount": 0},
            {"indexId": "zeta", "documentCount": 0}
        ])
    );
}

/// Build an `OpLogEntry` with op_type `"upsert"` for use in tests.
///
/// # Arguments
///
/// * `seq` - Sequence number.
/// * `ts` - Timestamp in milliseconds (used for conflict resolution).
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

fn expected_upsert_version(op: &OpLogEntry, destination_oplog_seq: u64) -> VersionRecord {
    let document = Document::from_json(
        op.payload
            .get("body")
            .expect("upsert fixture must carry an accepted body"),
    )
    .unwrap();
    let origin_seq = flapjack::index::oplog::replication_origin_seq(&op.payload)
        .unwrap()
        .unwrap_or(op.seq);
    VersionRecord::new(op.timestamp_ms, &op.node_id, false, destination_oplog_seq)
        .with_origin_proof(
            origin_seq,
            flapjack::index::oplog::upsert_effect_digest(&document),
        )
}

fn expected_delete_version(op: &OpLogEntry, destination_oplog_seq: u64) -> VersionRecord {
    let object_id = op
        .payload
        .get("objectID")
        .and_then(serde_json::Value::as_str)
        .expect("delete fixture must carry an objectID");
    let origin_seq = flapjack::index::oplog::replication_origin_seq(&op.payload)
        .unwrap()
        .unwrap_or(op.seq);
    VersionRecord::new(op.timestamp_ms, &op.node_id, true, destination_oplog_seq).with_origin_proof(
        origin_seq,
        flapjack::index::oplog::delete_effect_digest(object_id),
    )
}

fn clear_version_origin_proof(base_path: &Path, tenant_id: &str, object_id: &str) {
    let database_path = VersionStore::database_path(&base_path.join(tenant_id));
    let connection = rusqlite::Connection::open(database_path).unwrap();
    assert_eq!(
        connection
            .execute(
                "UPDATE object_versions
                 SET origin_seq = NULL, effect_digest = NULL
                 WHERE object_id = ?1",
                [object_id],
            )
            .unwrap(),
        1,
        "fixture must convert exactly one version row into legacy nullable evidence"
    );
}

async fn seed_legacy_local_upsert(
    tmp: &TempDir,
    tenant_id: &str,
    object_id: &str,
) -> (std::sync::Arc<IndexManager>, OpLogEntry) {
    let manager = IndexManager::new_with_node_id(tmp.path(), "local-node");
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_sync(
            tenant_id,
            vec![Document::from_json(&serde_json::json!({
                "objectID": object_id,
                "name": "legacy accepted body"
            }))
            .unwrap()],
        )
        .await
        .unwrap();
    let entry = manager
        .get_oplog(tenant_id)
        .unwrap()
        .read_since(0)
        .unwrap()
        .into_iter()
        .find(|entry| entry.op_type == "upsert")
        .expect("fixture must retain its exact local upsert evidence");
    clear_version_origin_proof(tmp.path(), tenant_id, object_id);
    (manager, entry)
}

#[cfg(feature = "fault-injection")]
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(flapjack_write_durable_timeout_env)]
async fn olr_replication_ack_refuses_admission_only_replay() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _durable_timeout = EnvVarRestoreGuard::set("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS", "25");
    let tmp = TempDir::new().unwrap();
    let tenant_id = "olr-admission-only-replay";
    let manager = IndexManager::new_with_node_id(tmp.path(), "destination-node");
    manager.create_tenant(tenant_id).unwrap();
    let _precommit_failure = manager.fail_next_before_tantivy_commit_for_test(tenant_id);
    let _compensation_failures = manager.fail_compensation_attempts_for_test(tenant_id, 2);
    let replicated = make_upsert_op(
        91,
        9_100,
        "source-node",
        tenant_id,
        "doc-1",
        "must remain replay-only",
    );

    let result = apply_ops_to_manager(&manager, tenant_id, &[replicated]).await;

    assert_eq!(
        manager.compensation_fault_attempts_remaining_for_test(tenant_id),
        0,
        "the worker failure and bounded waiter must consume the two injected compensation failures"
    );
    let tasks = manager.tenant_tasks_snapshot_for_test(tenant_id);
    assert_eq!(tasks.len(), 1);
    assert!(
        matches!(
            tasks[0].status,
            flapjack::types::TaskStatus::Enqueued | flapjack::types::TaskStatus::Processing
        ),
        "failed compensation must leave replay admission explicitly nonterminal: {:?}",
        tasks[0].status
    );
    assert!(manager.get_document(tenant_id, "doc-1").unwrap().is_none());
    assert_eq!(
        manager.get_object_version(tenant_id, "doc-1").unwrap(),
        None
    );
    assert!(
        result.is_err(),
        "replication must not acknowledge admission-only replay as a completed document effect: {result:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(olr_document_proof_hook)]
async fn olr_concurrent_same_object_lww_rechecks_before_effect() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "olr-concurrent-lww";
    let manager = IndexManager::new_with_node_id(tmp.path(), "destination-node");
    manager.create_tenant(tenant_id).unwrap();
    let older_accepted = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release_older = std::sync::Arc::new(std::sync::Barrier::new(2));
    let _proof_hook =
        crate::handlers::internal_ops::set_after_document_proof_accepted_hook_for_test({
            let older_accepted = std::sync::Arc::clone(&older_accepted);
            let release_older = std::sync::Arc::clone(&release_older);
            move |source_seq| {
                if source_seq == 1 {
                    older_accepted.wait();
                    release_older.wait();
                }
            }
        });

    let older_manager = std::sync::Arc::clone(&manager);
    let older = tokio::spawn(async move {
        apply_ops_to_manager(
            &older_manager,
            tenant_id,
            &[make_upsert_op(
                1,
                1_000,
                "source-node",
                tenant_id,
                "same-doc",
                "older",
            )],
        )
        .await
    });
    older_accepted.wait();

    let newer_result = apply_ops_to_manager(
        &manager,
        tenant_id,
        &[make_upsert_op(
            2,
            2_000,
            "source-node",
            tenant_id,
            "same-doc",
            "newer",
        )],
    )
    .await;
    release_older.wait();
    let older_result = older.await.unwrap();

    assert_eq!(newer_result.unwrap(), 2);
    assert_eq!(
        older_result.expect("a newly stale invocation remains idempotent success"),
        1
    );
    let document = manager
        .get_document(tenant_id, "same-doc")
        .unwrap()
        .expect("the newer document must remain searchable");
    assert!(matches!(
        document.fields.get("name"),
        Some(flapjack::types::FieldValue::Text(value)) if value == "newer"
    ));
    let version = manager
        .get_object_version(tenant_id, "same-doc")
        .unwrap()
        .expect("the newer durable proof must remain published");
    assert_eq!((version.timestamp_ms, version.origin_seq), (2_000, Some(2)));
    let durable_ops = manager.get_oplog(tenant_id).unwrap().read_since(0).unwrap();
    assert_eq!(
        durable_ops
            .iter()
            .filter(|entry| entry.op_type == "upsert")
            .count(),
        1,
        "the stale loser must not append a destination oplog effect"
    );
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

/// TODO: Document apply_single_index_op.
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

#[test]
fn count_only_search_requires_explicit_zero_page_size() {
    assert!(validate_count_only_hits_per_page(Some(0)).is_ok());

    let omitted = validate_count_only_hits_per_page(None).unwrap_err();
    assert_eq!(omitted.0, axum::http::StatusCode::BAD_REQUEST);

    let nonzero = validate_count_only_hits_per_page(Some(1)).unwrap_err();
    assert_eq!(nonzero.0, axum::http::StatusCode::BAD_REQUEST);
}

/// TODO: Document rotate_admin_key_error_response_hides_storage_details.
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

/// TODO: Document assert_batch_wrapper_flow.
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

/// Wait until every queued write reaches a terminal task state, which the write
/// queue marks only after `finalize_committed_batch` has written the durable
/// VersionStore and published the Tantivy searcher. Assertions that cover the
/// complete finalization contract drain here rather than synchronizing on only
/// one observable side effect.
async fn wait_for_finalization(manager: &IndexManager) {
    assert!(
        manager
            .wait_for_pending_tasks(std::time::Duration::from_secs(5))
            .await,
        "queued writes must finalize before reading the durable version store"
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
async fn replication_ack_slice_returns_final_adjacent_seq() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(3, 1000, "node-a", "t1", "d1", "Alice"),
        make_upsert_op(4, 2000, "node-a", "t1", "d2", "Bob"),
        make_upsert_op(5, 1500, "node-a", "t1", "d3", "Carol"),
    ];
    let result = apply_ops_to_manager(&manager, "t1", &ops).await.unwrap();
    assert_eq!(result, 5, "an adjacent batch acknowledges its final seq");
}

#[tokio::test]
async fn replication_ack_slice_rejects_inner_tenant_mismatch_before_effects() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(10, 1_000, "node-a", "outer", "doc-1", "MustNotLand"),
        make_upsert_op(11, 2_000, "node-a", "other", "doc-2", "MustNotLand"),
    ];

    let error = apply_ops_to_manager(&manager, "outer", &ops)
        .await
        .expect_err("every inner tenant must match the outer replication tenant");

    assert!(
        error.contains("inner tenant other does not match outer tenant outer"),
        "tenant mismatch must be explicit: {error}"
    );
    assert!(
        !tmp.path().join("outer").exists(),
        "tenant mismatch must fail before outer tenant creation"
    );
    assert!(
        !tmp.path().join("other").exists(),
        "tenant mismatch must not create the inner tenant either"
    );
}

#[tokio::test]
async fn replication_ack_slice_rejects_non_adjacent_sequences_before_effects() {
    for (first, second, case) in [
        (10, 10, "duplicate"),
        (10, 9, "decreasing"),
        (10, 12, "gapped"),
    ] {
        let tmp = TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let app = internal_replication_router(state);
        let tenant_id = format!("seq_{case}");
        let ops = vec![
            make_upsert_op(first, 1_000, "node-a", &tenant_id, "doc-1", "MustNotLand"),
            make_upsert_op(second, 2_000, "node-a", &tenant_id, "doc-2", "MustNotLand"),
        ];

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/replicate")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                            tenant_id: tenant_id.clone(),
                            ops,
                        })
                        .unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(
            response.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "{case} single-sender HTTP input must be rejected"
        );
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let error: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            error,
            serde_json::json!({
                "message": "Internal server error",
                "status": 500
            }),
            "{case} refusal must preserve the redacted public error contract"
        );
        assert!(
            !tmp.path().join(&tenant_id).exists(),
            "{case} sequence input must fail before tenant creation"
        );
    }
}

#[tokio::test]
async fn replication_ack_generic_merge_accepts_independent_peer_sequence_domains() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(1, 1_000, "peer-a", "merged", "doc-a", "FromA"),
        make_upsert_op(1, 2_000, "peer-b", "merged", "doc-b", "FromB"),
    ];

    let acked_seq = apply_ops_to_manager(&manager, "merged", &ops)
        .await
        .expect("generic merged-peer application must accept independent local seq domains");

    assert_eq!(acked_seq, 1);
    assert_eq!(manager.tenant_doc_count("merged"), Some(2));
    assert_eq!(
        manager.get_object_version("merged", "doc-a").unwrap(),
        Some(expected_upsert_version(&ops[0], 1))
    );
    assert_eq!(
        manager.get_object_version("merged", "doc-b").unwrap(),
        Some(expected_upsert_version(&ops[1], 2))
    );
}

#[tokio::test]
async fn replication_ack_slice_same_tuple_batch_higher_seq_upsert_wins() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(20, 5_000, "node-a", "t1", "doc-1", "LowerSeq"),
        make_upsert_op(21, 5_000, "node-a", "t1", "doc-1", "HigherSeq"),
    ];

    let acked_seq = apply_ops_to_manager(&manager, "t1", &ops)
        .await
        .expect("the higher source sequence must resolve an equal-tuple batch tie");

    assert_eq!(acked_seq, 21);
    let document = manager.get_document("t1", "doc-1").unwrap().unwrap();
    assert!(
        matches!(
            document.fields.get("name"),
            Some(flapjack::types::FieldValue::Text(value)) if value == "HigherSeq"
        ),
        "the higher source sequence body must win: {:?}",
        document.fields.get("name")
    );
}

#[tokio::test]
async fn replication_ack_slice_same_tuple_batch_higher_seq_delete_wins() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(30, 6_000, "node-a", "t1", "doc-1", "MustBeDeleted"),
        make_delete_op(31, 6_000, "node-a", "t1", "doc-1"),
    ];

    let acked_seq = apply_ops_to_manager(&manager, "t1", &ops)
        .await
        .expect("the higher source sequence delete must resolve an equal-tuple batch tie");

    assert_eq!(acked_seq, 31);
    assert!(
        manager.get_document("t1", "doc-1").unwrap().is_none(),
        "the higher source sequence delete must be the final effect"
    );
}

#[tokio::test]
async fn replication_ack_origin_proof_equal_tuple_retry_requires_identical_body_and_effect() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let original = make_upsert_op(40, 7_000, "node-a", "t1", "doc-1", "Original");

    assert_eq!(
        apply_ops_to_manager(&manager, "t1", std::slice::from_ref(&original))
            .await
            .unwrap(),
        40
    );
    assert_eq!(
        apply_ops_to_manager(&manager, "t1", std::slice::from_ref(&original))
            .await
            .expect("an exact equal-tuple upsert retry is idempotent success"),
        40
    );

    let different_body = make_upsert_op(40, 7_000, "node-a", "t1", "doc-1", "Ambiguous");
    let error = apply_ops_to_manager(&manager, "t1", &[different_body])
        .await
        .expect_err("an equal-tuple retry with a different body must fail closed");
    assert!(error.contains("ambiguous equal-tuple upsert"), "{error}");

    let different_effect = make_delete_op(40, 7_000, "node-a", "t1", "doc-1");
    let error = apply_ops_to_manager(&manager, "t1", &[different_effect])
        .await
        .expect_err("an equal-tuple retry with a different effect must fail closed");
    assert!(error.contains("ambiguous equal-tuple delete"), "{error}");

    let delete = make_delete_op(41, 8_000, "node-a", "t1", "doc-1");
    assert_eq!(
        apply_ops_to_manager(&manager, "t1", std::slice::from_ref(&delete))
            .await
            .unwrap(),
        41
    );
    assert_eq!(
        apply_ops_to_manager(&manager, "t1", std::slice::from_ref(&delete))
            .await
            .expect("an exact equal-tuple delete retry is idempotent success"),
        41
    );

    let resurrection = make_upsert_op(41, 8_000, "node-a", "t1", "doc-1", "Ambiguous");
    let error = apply_ops_to_manager(&manager, "t1", &[resurrection])
        .await
        .expect_err("an equal-tuple retry must not change a delete into an upsert");
    assert!(error.contains("ambiguous equal-tuple upsert"), "{error}");
    assert!(manager.get_document("t1", "doc-1").unwrap().is_none());
}

#[tokio::test]
async fn replication_ack_origin_proof_legacy_local_oplog_proves_identical_retry_without_rewrite() {
    let tmp = TempDir::new().unwrap();
    let tenant_id = "legacy-local-exact";
    let object_id = "legacy-doc";
    let (manager, retained_entry) = seed_legacy_local_upsert(&tmp, tenant_id, object_id).await;
    let legacy_version = manager
        .get_object_version(tenant_id, object_id)
        .unwrap()
        .expect("legacy version row must exist");
    assert_eq!(legacy_version.origin_seq, None);
    assert_eq!(legacy_version.effect_digest, None);
    let oplog_seq = manager.get_oplog(tenant_id).unwrap().current_seq();
    let task_count = manager.tenant_tasks_snapshot_for_test(tenant_id).len();

    let acked_seq =
        apply_ops_to_manager(&manager, tenant_id, std::slice::from_ref(&retained_entry))
            .await
            .expect("identical retained local oplog evidence must prove a legacy retry");

    assert_eq!(acked_seq, retained_entry.seq);
    assert_eq!(
        manager.get_oplog(tenant_id).unwrap().current_seq(),
        oplog_seq
    );
    assert_eq!(
        manager.tenant_tasks_snapshot_for_test(tenant_id).len(),
        task_count,
        "a proven legacy retry must not enqueue another write"
    );
    let version_after = manager
        .get_object_version(tenant_id, object_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            version_after.timestamp_ms,
            version_after.node_id.as_str(),
            version_after.tombstone,
            version_after.oplog_seq,
        ),
        (
            legacy_version.timestamp_ms,
            legacy_version.node_id.as_str(),
            legacy_version.tombstone,
            legacy_version.oplog_seq,
        ),
        "proof by retained oplog must not restamp the destination-local version tuple"
    );
    if let Some(origin_seq) = version_after.origin_seq {
        assert_eq!(origin_seq, retained_entry.seq);
        assert_eq!(
            version_after.effect_digest,
            Some(flapjack::index::oplog::upsert_effect_digest(
                &Document::from_json(retained_entry.payload.get("body").unwrap()).unwrap(),
            )),
            "an optional safe backfill must use the exact retained effect"
        );
    }
}

#[tokio::test]
async fn replication_ack_origin_proof_legacy_local_oplog_refuses_missing_or_mismatched_evidence() {
    {
        let tmp = TempDir::new().unwrap();
        let tenant_id = "legacy-local-pruned";
        let object_id = "legacy-doc";
        let (manager, retained_entry) = seed_legacy_local_upsert(&tmp, tenant_id, object_id).await;
        manager.graceful_shutdown().await;
        drop(manager);
        std::fs::remove_dir_all(tmp.path().join(tenant_id).join("oplog")).unwrap();
        let restarted = IndexManager::new_with_node_id(tmp.path(), "local-node");

        apply_ops_to_manager(&restarted, tenant_id, std::slice::from_ref(&retained_entry))
            .await
            .expect_err("a pruned legacy oplog cannot prove an exact retry");
        assert_eq!(
            restarted
                .get_object_version(tenant_id, object_id)
                .unwrap()
                .unwrap()
                .origin_seq,
            None
        );
    }

    {
        let tmp = TempDir::new().unwrap();
        let tenant_id = "legacy-local-tuple-mismatch";
        let object_id = "legacy-doc";
        let (manager, retained_entry) = seed_legacy_local_upsert(&tmp, tenant_id, object_id).await;
        let mut tuple_mismatch = retained_entry.clone();
        tuple_mismatch.timestamp_ms += 1;
        assert!(VersionStore::open(&tmp.path().join(tenant_id))
            .unwrap()
            .upsert(
                object_id,
                &VersionRecord::new(
                    tuple_mismatch.timestamp_ms,
                    &tuple_mismatch.node_id,
                    false,
                    retained_entry.seq,
                ),
            )
            .unwrap());

        apply_ops_to_manager(&manager, tenant_id, &[tuple_mismatch])
            .await
            .expect_err("retained evidence with a different tuple must not prove a retry");
    }

    {
        let tmp = TempDir::new().unwrap();
        let tenant_id = "legacy-local-object-mismatch";
        let (manager, retained_entry) =
            seed_legacy_local_upsert(&tmp, tenant_id, "legacy-doc").await;
        let mut object_mismatch = retained_entry.clone();
        object_mismatch.payload["objectID"] = serde_json::json!("other-doc");
        object_mismatch.payload["body"]["_id"] = serde_json::json!("other-doc");
        assert!(VersionStore::open(&tmp.path().join(tenant_id))
            .unwrap()
            .upsert(
                "other-doc",
                &VersionRecord::new(
                    object_mismatch.timestamp_ms,
                    &object_mismatch.node_id,
                    false,
                    retained_entry.seq,
                ),
            )
            .unwrap());

        apply_ops_to_manager(&manager, tenant_id, &[object_mismatch])
            .await
            .expect_err("retained evidence for another object must not prove a retry");
    }

    {
        let tmp = TempDir::new().unwrap();
        let tenant_id = "legacy-local-effect-mismatch";
        let (manager, retained_entry) =
            seed_legacy_local_upsert(&tmp, tenant_id, "legacy-doc").await;
        let mut effect_mismatch = retained_entry;
        effect_mismatch.payload["body"]["name"] = serde_json::json!("different body");

        apply_ops_to_manager(&manager, tenant_id, &[effect_mismatch])
            .await
            .expect_err("retained evidence with another effect must not prove a retry");
    }
}

#[tokio::test]
async fn replication_ack_origin_proof_multihop_preserves_original_source_identity() {
    let source_op = make_upsert_op(
        77,
        7_700,
        "source-a",
        "products",
        "multihop-doc",
        "OriginalEffect",
    );
    let b_dir = TempDir::new().unwrap();
    let manager_b = IndexManager::new_with_node_id(b_dir.path(), "replica-b");
    assert_eq!(
        apply_ops_to_manager(&manager_b, "products", std::slice::from_ref(&source_op))
            .await
            .unwrap(),
        77
    );
    let version_b = manager_b
        .get_object_version("products", "multihop-doc")
        .unwrap()
        .unwrap();
    assert_eq!(version_b.origin_seq, Some(77));
    let forwarded = manager_b
        .get_oplog("products")
        .unwrap()
        .read_since(0)
        .unwrap()
        .into_iter()
        .find(|entry| entry.op_type == "upsert")
        .expect("replica B must retain its destination-local oplog row");
    assert_eq!(forwarded.seq, 1, "B owns an independent local oplog domain");

    let c_dir = TempDir::new().unwrap();
    let manager_c = IndexManager::new_with_node_id(c_dir.path(), "replica-c");
    apply_ops_to_manager(&manager_c, "products", &[forwarded])
        .await
        .unwrap();
    let version_c = manager_c
        .get_object_version("products", "multihop-doc")
        .unwrap()
        .unwrap();

    assert_eq!(
        (version_c.origin_seq, version_c.effect_digest),
        (version_b.origin_seq, version_b.effect_digest),
        "B's destination-local oplog sequence must not replace A's source identity"
    );
}

#[tokio::test]
async fn replication_ack_origin_proof_whole_same_tuple_batch_replay_is_a_noop() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(42, 7_500, "node-a", "t1", "doc-1", "First"),
        make_upsert_op(43, 7_500, "node-a", "t1", "doc-1", "Second"),
    ];

    assert_eq!(
        apply_ops_to_manager(&manager, "t1", &ops).await.unwrap(),
        43
    );
    let oplog_seq = manager.get_oplog("t1").unwrap().current_seq();
    let task_count = manager.tenant_tasks_snapshot_for_test("t1").len();

    assert_eq!(
        apply_ops_to_manager(&manager, "t1", &ops)
            .await
            .expect("an exact whole-batch replay must be idempotent success"),
        43
    );
    assert_eq!(
        manager.get_oplog("t1").unwrap().current_seq(),
        oplog_seq,
        "an exact replay must not append destination-local oplog rows"
    );
    assert_eq!(
        manager.tenant_tasks_snapshot_for_test("t1").len(),
        task_count,
        "an exact replay must not enqueue another engine write"
    );
    let document = manager.get_document("t1", "doc-1").unwrap().unwrap();
    assert!(matches!(
        document.fields.get("name"),
        Some(flapjack::types::FieldValue::Text(value)) if value == "Second"
    ));
}

#[tokio::test]
async fn replication_ack_origin_proof_higher_seq_applies_across_requests_for_same_tuple() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let first = make_upsert_op(44, 7_600, "node-a", "t1", "doc-1", "First");
    let second = make_upsert_op(45, 7_600, "node-a", "t1", "doc-1", "Second");

    assert_eq!(
        apply_ops_to_manager(&manager, "t1", &[first])
            .await
            .unwrap(),
        44
    );
    let first_oplog_seq = manager.get_oplog("t1").unwrap().current_seq();
    assert_eq!(
        apply_ops_to_manager(&manager, "t1", &[second])
            .await
            .expect("a higher source sequence must apply across requests"),
        45
    );
    assert_eq!(
        manager.get_oplog("t1").unwrap().current_seq(),
        first_oplog_seq + 1
    );
    let document = manager.get_document("t1", "doc-1").unwrap().unwrap();
    assert!(matches!(
        document.fields.get("name"),
        Some(flapjack::types::FieldValue::Text(value)) if value == "Second"
    ));
}

#[tokio::test]
async fn replication_ack_origin_proof_settings_transformed_body_exact_retry_is_a_noop() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    let settings_op = make_index_op(
        46,
        7_700,
        "node-a",
        "t1",
        "settings",
        serde_json::to_value(settings).unwrap(),
    );
    assert_eq!(
        apply_ops_to_state(&state, "t1", &[settings_op])
            .await
            .unwrap(),
        46
    );

    let document_op = make_index_op(
        47,
        7_800,
        "node-a",
        "t1",
        "upsert",
        serde_json::json!({
            "objectID": "doc-1",
            "body": {
                "_id": "doc-1",
                "title": "Nebula handbook",
                "description": "text omitted by document reconstruction"
            }
        }),
    );
    assert_eq!(
        apply_ops_to_state(&state, "t1", std::slice::from_ref(&document_op))
            .await
            .unwrap(),
        47
    );
    let oplog_seq = state.manager.get_oplog("t1").unwrap().current_seq();
    let task_count = state.manager.tenant_tasks_snapshot_for_test("t1").len();

    assert_eq!(
        apply_ops_to_state(&state, "t1", std::slice::from_ref(&document_op))
            .await
            .expect("the accepted logical body digest must prove this exact retry"),
        47
    );
    assert_eq!(
        state.manager.get_oplog("t1").unwrap().current_seq(),
        oplog_seq
    );
    assert_eq!(
        state.manager.tenant_tasks_snapshot_for_test("t1").len(),
        task_count
    );
}

#[tokio::test]
async fn replication_ack_slice_move_index_is_fail_closed_before_effects() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("source").unwrap();
    manager
        .add_documents_sync(
            "source",
            vec![Document::from_json(&serde_json::json!({
                "objectID": "existing",
                "name": "Preserve"
            }))
            .unwrap()],
        )
        .await
        .unwrap();
    let move_op = make_index_op(
        50,
        9_000,
        "node-a",
        "source",
        "move_index",
        serde_json::json!({"source": "source", "destination": "destination"}),
    );

    let error = apply_ops_to_manager(&manager, "source", &[move_op])
        .await
        .expect_err("replicated move_index must fail closed for this release");

    assert!(
        error.contains("move_index replication is disabled"),
        "{error}"
    );
    assert!(manager
        .get_document("source", "existing")
        .unwrap()
        .is_some());
    assert!(!tmp.path().join("destination").exists());
}

#[tokio::test]
async fn replication_ack_slice_mixed_batch_with_move_fails_before_first_effect() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("source").unwrap();
    let ops = vec![
        make_index_op(
            60,
            10_000,
            "node-a",
            "source",
            "save_synonym",
            serde_json::json!({
                "objectID": "must-not-land",
                "type": "synonym",
                "synonyms": ["tv", "television"]
            }),
        ),
        make_index_op(
            61,
            10_001,
            "node-a",
            "source",
            "move_index",
            serde_json::json!({"source": "source", "destination": "destination"}),
        ),
    ];

    let error = apply_ops_to_manager(&manager, "source", &ops)
        .await
        .expect_err("a mixed batch containing move_index must fail before its first effect");

    assert!(
        error.contains("move_index replication is disabled"),
        "{error}"
    );
    assert!(!tmp.path().join("source/synonyms.json").exists());
    assert!(tmp.path().join("source/meta.json").exists());
    assert!(!tmp.path().join("destination").exists());
}

#[tokio::test]
async fn replication_ack_slice_same_endpoint_move_and_copy_fail_before_bootstrap() {
    for op_type in ["move_index", "copy_index"] {
        let tmp = TempDir::new().unwrap();
        let manager = IndexManager::new(tmp.path());
        let tenant_id = format!("same_endpoint_{op_type}");
        let ops = vec![
            make_upsert_op(70, 11_000, "node-a", &tenant_id, "doc-1", "MustNotLand"),
            make_index_op(
                71,
                11_001,
                "node-a",
                &tenant_id,
                op_type,
                serde_json::json!({
                    "source": tenant_id,
                    "destination": tenant_id
                }),
            ),
        ];

        let error = apply_ops_to_manager(&manager, &tenant_id, &ops)
            .await
            .expect_err("same-endpoint move/copy must fail in whole-batch preflight");

        assert!(
            error.contains("source and destination must differ"),
            "same-endpoint {op_type} refusal must be explicit: {error}"
        );
        assert!(
            !tmp.path().join(&tenant_id).exists(),
            "same-endpoint {op_type} must fail before document bootstrap"
        );
    }
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn replication_ack_slice_rejects_terminal_task_document_rejections() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let op = make_index_op(
        80,
        12_000,
        "node-a",
        "t1",
        "upsert",
        serde_json::json!({
            "body": {
                "objectID": "invalid-vector",
                "name": "valid JSON",
                "_vectors": [0.1, 0.2]
            }
        }),
    );

    let error = apply_ops_to_manager(&manager, "t1", &[op])
        .await
        .expect_err("a durable task with rejected documents must not be acknowledged");

    assert!(error.contains("rejected 1 document"), "{error}");
    let tasks = manager.tenant_tasks_snapshot_for_test("t1");
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].rejected_count, 1);
    assert!(manager
        .get_document("t1", "invalid-vector")
        .unwrap()
        .is_none());
    assert_eq!(
        manager.get_object_version("t1", "invalid-vector").unwrap(),
        None
    );
}

#[tokio::test(flavor = "current_thread")]
async fn replication_ack_rejected_second_document_keeps_first_durable_but_never_acknowledges() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _document_limit = EnvVarRestoreGuard::set("FLAPJACK_MAX_DOC_MB", "1");
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let tenant_id = "partial-rejection";
    let valid = make_upsert_op(90, 13_000, "node-a", tenant_id, "valid-doc", "Durable");
    let rejected = make_index_op(
        91,
        13_001,
        "node-a",
        tenant_id,
        "upsert",
        serde_json::json!({
            "objectID": "oversized-doc",
            "body": {
                "objectID": "oversized-doc",
                "payload": "x".repeat(4 * 1024 * 1024)
            }
        }),
    );
    let ops = vec![valid.clone(), rejected];

    let first_error = apply_ops_to_manager(&manager, tenant_id, &ops)
        .await
        .expect_err("a terminal partial rejection must not acknowledge the batch");

    assert!(first_error.contains("rejected 1 document"), "{first_error}");
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));
    assert_eq!(
        manager.get_object_version(tenant_id, "valid-doc").unwrap(),
        Some(expected_upsert_version(&valid, 1)),
        "the accepted prefix must remain durably proven"
    );
    assert_eq!(
        manager
            .get_object_version(tenant_id, "oversized-doc")
            .unwrap(),
        None
    );
    let first_oplog_seq = manager.get_oplog(tenant_id).unwrap().current_seq();

    let retry_error = apply_ops_to_manager(&manager, tenant_id, &ops)
        .await
        .expect_err("whole-batch retry must still refuse its rejected suffix");

    assert!(retry_error.contains("rejected 1 document"), "{retry_error}");
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));
    assert_eq!(
        manager.get_oplog(tenant_id).unwrap().current_seq(),
        first_oplog_seq,
        "the exact durable prefix must be idempotent on whole-batch retry"
    );
    assert_eq!(
        manager.get_object_version(tenant_id, "valid-doc").unwrap(),
        Some(expected_upsert_version(&valid, 1))
    );
}

#[tokio::test]
async fn replication_ack_rejects_malformed_document_payload() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let op = make_index_op(
        11,
        1_000,
        "node-a",
        "t1",
        "upsert",
        serde_json::json!({"body": ["not", "a", "document"]}),
    );

    let error = apply_ops_to_manager(&manager, "t1", &[op])
        .await
        .expect_err("malformed document operations must not be acknowledged");

    assert!(
        error.contains("failed to parse upsert seq 11"),
        "replication refusal should identify the malformed document: {error}"
    );
    assert!(
        !tmp.path().join("t1").exists(),
        "malformed input must be rejected before bootstrap creates the tenant"
    );
}

#[tokio::test]
async fn replication_ack_settings_only_batch_is_mutation_effective() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let manager = &state.manager;
    let tenant_id = "fresh_settings";
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_sync(
            tenant_id,
            vec![
                Document::from_json(&serde_json::json!({
                    "objectID": "visible",
                    "title": "Nebula handbook",
                    "description": "ordinary",
                    "category": "space"
                }))
                .unwrap(),
                Document::from_json(&serde_json::json!({
                    "objectID": "hidden",
                    "title": "Gardening guide",
                    "description": "Nebula",
                    "category": "earth"
                }))
                .unwrap(),
            ],
        )
        .await
        .unwrap();

    let oplog_seq_before_settings = manager.get_oplog(tenant_id).unwrap().current_seq();
    let settings = IndexSettings {
        attributes_for_faceting: vec!["category".to_string()],
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    let op = make_index_op(
        41,
        1_000,
        "node-a",
        tenant_id,
        "settings",
        serde_json::to_value(&settings).unwrap(),
    );

    let acked_seq = apply_ops_to_state(&state, tenant_id, &[op])
        .await
        .expect("a valid settings operation should be applied");
    assert_eq!(acked_seq, 41, "the applied settings sequence must be exact");

    let settings_path = tmp.path().join(tenant_id).join("settings.json");
    let persisted = IndexSettings::load(&settings_path)
        .expect("acknowledged settings must be persisted on a fresh index");
    assert_eq!(
        persisted.searchable_attributes,
        settings.searchable_attributes
    );
    assert_eq!(
        persisted.attributes_for_faceting,
        settings.attributes_for_faceting
    );

    let facet_requests = [FacetRequest {
        field: "category".to_string(),
        path: "/category".to_string(),
        value_query: None,
    }];
    let result = manager
        .search_with_options(
            tenant_id,
            "Nebula",
            &SearchOptions {
                facets: Some(&facet_requests),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        result.total, 2,
        "canonical search retains unlisted attributes at low weight"
    );
    assert_eq!(result.documents[0].document.id, "visible");
    let mut facet_counts = result.facets["category"]
        .iter()
        .map(|count| (count.path.as_str(), count.count))
        .collect::<Vec<_>>();
    facet_counts.sort_unstable();
    assert_eq!(
        facet_counts,
        vec![("earth", 1), ("space", 1)],
        "replicated attributesForFaceting must reindex documents that already existed"
    );

    let destination_ops = manager
        .get_oplog(tenant_id)
        .unwrap()
        .read_since(oplog_seq_before_settings)
        .unwrap();
    assert!(
        destination_ops
            .iter()
            .all(|entry| entry.op_type != "settings"),
        "applying replicated exact settings must not append a destination settings oplog entry"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn replication_ack_settings_faceting_rebuild_rejection_is_not_acknowledged() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _document_limit = EnvVarRestoreGuard::set("FLAPJACK_MAX_DOC_MB", "1");
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "settings-reindex-rejection";
    state.manager.create_tenant(tenant_id).unwrap();
    let index = state.manager.get_or_load(tenant_id).unwrap();
    let existing_document = Document::from_json(&serde_json::json!({
        "objectID": "existing-doc",
        "title": "x".repeat(4 * 1024 * 1024),
        "category": "space"
    }))
    .unwrap();
    let tantivy_document = index
        .converter()
        .to_tantivy(&existing_document, Some(&IndexSettings::default()))
        .unwrap();
    let mut writer = index.writer().unwrap();
    writer.add_document(tantivy_document).unwrap();
    writer.commit().unwrap();
    index.reader().reload().unwrap();
    drop(writer);
    let settings = IndexSettings {
        attributes_for_faceting: vec!["category".to_string()],
        ..Default::default()
    };
    let op = make_index_op(
        92,
        13_100,
        "node-a",
        tenant_id,
        "settings",
        serde_json::to_value(settings).unwrap(),
    );

    let error = apply_ops_to_state(&state, tenant_id, &[op])
        .await
        .expect_err("a rejected faceting rebuild task must prevent settings acknowledgement");

    assert!(error.contains("rejected 1 document"), "{error}");
    let tasks = state.manager.tenant_tasks_snapshot_for_test(tenant_id);
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].rejected_count, 1);
}

#[tokio::test]
async fn replication_ack_virtual_settings_remain_settings_only_and_redirect_search() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let primary = "products";
    let virtual_replica = "products_relevance";
    state.manager.create_tenant(primary).unwrap();
    state
        .manager
        .add_documents_sync(
            primary,
            vec![
                Document::from_json(&serde_json::json!({
                    "objectID": "visible",
                    "title": "Nebula handbook",
                    "description": "ordinary"
                }))
                .unwrap(),
                Document::from_json(&serde_json::json!({
                    "objectID": "hidden",
                    "title": "Gardening guide",
                    "description": "Nebula"
                }))
                .unwrap(),
            ],
        )
        .await
        .unwrap();

    let settings = IndexSettings {
        primary: Some(primary.to_string()),
        searchable_attributes: Some(vec!["title".to_string()]),
        ..Default::default()
    };
    let op = make_index_op(
        43,
        1_000,
        "node-a",
        virtual_replica,
        "settings",
        serde_json::to_value(&settings).unwrap(),
    );

    let acked_seq = apply_ops_to_state(&state, virtual_replica, &[op])
        .await
        .expect("valid virtual settings must be applied");
    assert_eq!(acked_seq, 43);

    let virtual_path = tmp.path().join(virtual_replica);
    assert!(virtual_path.join("settings.json").exists());
    assert!(
        !virtual_path.join("meta.json").exists(),
        "an exact settings event with primary=Some must not materialize a virtual replica"
    );

    let target = crate::handlers::replicas::resolve_search_target(&state, virtual_replica);
    assert_eq!(target.data_index, primary);
    let override_settings = target
        .settings_override
        .as_ref()
        .expect("virtual search must use the replicated settings as an override");
    let result = state
        .manager
        .search_with_options(
            &target.data_index,
            "Nebula",
            &SearchOptions {
                settings_override: Some(override_settings),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(result.total, 2);
    assert_eq!(result.documents[0].document.id, "visible");
}

#[tokio::test]
async fn replication_ack_exact_settings_reconcile_replica_primary_links() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let manager = &state.manager;
    let primary = "products";
    manager.create_tenant(primary).unwrap();

    let first_settings = IndexSettings {
        replicas: Some(vec!["virtual(products_old)".to_string()]),
        ..Default::default()
    };
    let second_settings = IndexSettings {
        replicas: Some(vec!["virtual(products_new)".to_string()]),
        ..Default::default()
    };
    apply_ops_to_state(
        &state,
        primary,
        &[make_index_op(
            44,
            1_000,
            "node-a",
            primary,
            "settings",
            serde_json::to_value(first_settings).unwrap(),
        )],
    )
    .await
    .unwrap();
    apply_ops_to_state(
        &state,
        primary,
        &[make_index_op(
            45,
            2_000,
            "node-a",
            primary,
            "settings",
            serde_json::to_value(second_settings).unwrap(),
        )],
    )
    .await
    .unwrap();

    let old_settings = IndexSettings::load(tmp.path().join("products_old/settings.json")).unwrap();
    let new_settings = IndexSettings::load(tmp.path().join("products_new/settings.json")).unwrap();
    assert_eq!(old_settings.primary, None);
    assert_eq!(new_settings.primary.as_deref(), Some(primary));
    assert!(!tmp.path().join("products_new/meta.json").exists());
}

#[cfg(feature = "vector-search")]
#[tokio::test]
async fn replication_ack_exact_settings_invalidate_cached_embedder() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "vector_settings";
    let initial_settings = IndexSettings {
        embedders: Some(std::collections::HashMap::from([(
            "default".to_string(),
            serde_json::json!({"source": "userProvided", "dimensions": 3}),
        )])),
        ..Default::default()
    };
    let cached = state
        .embedder_store
        .get_or_create(tenant_id, "default", &initial_settings)
        .unwrap();

    let mut updated_settings = initial_settings.clone();
    updated_settings.embedders = Some(std::collections::HashMap::from([(
        "default".to_string(),
        serde_json::json!({"source": "userProvided", "dimensions": 4}),
    )]));
    let op = make_index_op(
        46,
        1_000,
        "node-a",
        tenant_id,
        "settings",
        serde_json::to_value(&updated_settings).unwrap(),
    );

    let acked_seq = apply_ops_to_state(&state, tenant_id, &[op])
        .await
        .expect("valid exact embedder settings must apply before acknowledgement");
    assert_eq!(acked_seq, 46);

    let reloaded = state
        .embedder_store
        .get_or_create(tenant_id, "default", &updated_settings)
        .unwrap();
    assert!(
        !std::sync::Arc::ptr_eq(&cached, &reloaded),
        "replication must invalidate the tenant's cached embedder before acknowledging"
    );
}

#[tokio::test]
async fn replication_ack_rejects_settings_persistence_failure() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let manager = &state.manager;
    let tenant_id = "settings_failure";
    manager.create_tenant(tenant_id).unwrap();
    let settings_path = tmp.path().join(tenant_id).join("settings.json");
    std::fs::remove_file(&settings_path).unwrap();
    std::fs::create_dir(&settings_path).unwrap();
    let op = make_index_op(
        42,
        1_000,
        "node-a",
        tenant_id,
        "settings",
        serde_json::to_value(IndexSettings::default()).unwrap(),
    );

    let error = apply_ops_to_state(&state, tenant_id, &[op])
        .await
        .expect_err("a settings persistence failure must not be acknowledged");

    assert!(
        error.contains("settings seq 42"),
        "replication refusal should identify the failed settings operation: {error}"
    );
}

#[tokio::test]
async fn replication_ack_mixed_valid_and_invalid_batch_has_no_success_or_document_effect() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_upsert_op(50, 1_000, "node-a", "t1", "doc1", "MustNotLand"),
        make_index_op(
            51,
            2_000,
            "node-a",
            "t1",
            "unknown_after_valid",
            serde_json::json!({}),
        ),
    ];

    let error = apply_ops_to_manager(&manager, "t1", &ops)
        .await
        .expect_err("a mixed valid and invalid batch must not return a success acknowledgement");

    assert!(error.contains("unknown op_type unknown_after_valid"));
    assert!(
        !tmp.path().join("t1").exists(),
        "the entire batch must be rejected before bootstrap or document staging"
    );
}

#[tokio::test]
async fn replication_ack_preflight_blocks_immediate_mutation_before_later_invalid_op() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let ops = vec![
        make_index_op(
            52,
            1_000,
            "node-a",
            "t1",
            "save_synonym",
            serde_json::json!({
                "objectID": "must-not-land",
                "type": "synonym",
                "synonyms": ["tv", "television"]
            }),
        ),
        make_index_op(
            53,
            2_000,
            "node-a",
            "t1",
            "upsert",
            serde_json::json!({"body": ["not", "a", "document"]}),
        ),
    ];

    let error = apply_ops_to_manager(&manager, "t1", &ops)
        .await
        .expect_err("every payload must be validated before the first mutation");

    assert!(error.contains("failed to parse upsert seq 53"));
    assert!(
        !tmp.path().join("t1/synonyms.json").exists(),
        "the first immediate operation must not run before later malformed input is rejected"
    );
    assert!(
        !tmp.path().join("t1").exists(),
        "whole-batch preflight must reject malformed input before bootstrap creates the tenant"
    );
}

#[tokio::test]
async fn replication_batch_preflights_index_operations_before_any_effect() {
    let copy_observation = {
        let tmp = TempDir::new().unwrap();
        let manager = IndexManager::new(tmp.path());
        let outer = "copy-proof-outer";
        let source = "copy-proof-source";
        let destination = "copy-proof-destination";
        for tenant_id in [outer, source, destination] {
            manager.create_tenant(tenant_id).unwrap();
        }
        manager
            .add_documents_sync(
                outer,
                vec![Document::from_json(&serde_json::json!({
                    "objectID": "outer-baseline",
                    "name": "outer baseline"
                }))
                .unwrap()],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                source,
                vec![Document::from_json(&serde_json::json!({
                    "objectID": "source-baseline",
                    "name": "source baseline"
                }))
                .unwrap()],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                destination,
                vec![Document::from_json(&serde_json::json!({
                    "objectID": "destination-baseline",
                    "name": "destination baseline"
                }))
                .unwrap()],
            )
            .await
            .unwrap();
        let batch = vec![
            make_upsert_op(
                1,
                1_000,
                "source-node",
                outer,
                "must-not-apply",
                "must not apply before copy refusal",
            ),
            make_index_op(
                2,
                2_000,
                "source-node",
                outer,
                "copy_index",
                serde_json::json!({
                    "source": source,
                    "destination": destination
                }),
            ),
        ];

        let result = apply_ops_to_manager(&manager, outer, &batch).await;
        (
            result,
            manager
                .get_document(outer, "must-not-apply")
                .unwrap()
                .is_none(),
            manager
                .get_document(source, "source-baseline")
                .unwrap()
                .is_some(),
            manager
                .get_document(destination, "destination-baseline")
                .unwrap()
                .is_some(),
            manager
                .get_document(destination, "source-baseline")
                .unwrap()
                .is_none(),
        )
    };

    let clear_observation = {
        let tmp = TempDir::new().unwrap();
        let manager = IndexManager::new(tmp.path());
        let outer = "clear-proof-outer";
        let clear_target = "clear-proof-target";
        manager.create_tenant(outer).unwrap();
        manager.create_tenant(clear_target).unwrap();
        manager
            .add_documents_sync(
                clear_target,
                vec![Document::from_json(&serde_json::json!({
                    "objectID": "clear-baseline",
                    "name": "must survive refusal"
                }))
                .unwrap()],
            )
            .await
            .unwrap();
        let batch = vec![
            make_upsert_op(
                1,
                1_000,
                "source-node",
                outer,
                "must-not-apply",
                "must not apply before clear refusal",
            ),
            make_index_op(
                2,
                2_000,
                "source-node",
                outer,
                "clear_index",
                serde_json::json!({"index_name": clear_target}),
            ),
        ];

        let result = apply_ops_to_manager(&manager, outer, &batch).await;
        (
            result,
            manager
                .get_document(outer, "must-not-apply")
                .unwrap()
                .is_none(),
            manager
                .get_document(clear_target, "clear-baseline")
                .unwrap()
                .is_some(),
        )
    };

    assert!(
        copy_observation.0.is_err()
            && copy_observation.1
            && copy_observation.2
            && copy_observation.3
            && copy_observation.4
            && clear_observation.0.is_err()
            && clear_observation.1
            && clear_observation.2,
        "copy and clear must both fail preflight without effects; copy={copy_observation:?}, clear={clear_observation:?}"
    );
}

#[tokio::test]
async fn replication_ack_rejects_failed_index_operation() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("same_index").unwrap();
    let op = make_index_op(
        56,
        1_000,
        "node-a",
        "same_index",
        "move_index",
        serde_json::json!({
            "source": "same_index",
            "destination": "same_index"
        }),
    );

    let error = apply_ops_to_manager(&manager, "same_index", &[op])
        .await
        .expect_err("an index mutation failure must not be acknowledged");

    assert!(error.contains("source and destination must differ"));
    assert!(tmp.path().join("same_index/meta.json").exists());
}

/// Verify that a `clear_index` op with a path-traversal `index_name` like `"../victim"` is rejected by validation and does not delete external directories.
#[tokio::test]
async fn replication_ack_clear_index_rejects_path_traversal_name() {
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

    let error = apply_ops_to_manager(&manager, "t1", &[op])
        .await
        .expect_err("a rejected clear_index must not be acknowledged");
    assert!(
        error.contains("invalid index_name"),
        "replication refusal should identify the rejected index name: {error}"
    );
    assert!(
        victim_dir.exists(),
        "clear_index with traversal name must not touch external directory"
    );
}

// ── Durable versions: newer timestamp wins ──

#[cfg(feature = "fault-injection")]
#[tokio::test]
async fn replication_ack_replicated_commit_failure_does_not_advance_version() {
    use flapjack::types::TaskStatus;
    use std::collections::HashSet;

    let tmp = TempDir::new().unwrap();
    let tenant_id = "replicated_commit_failure";
    let manager = IndexManager::new_with_node_id(tmp.path(), "node-a");
    manager.create_tenant(tenant_id).unwrap();
    let original_task_ids: HashSet<String> = manager
        .tenant_tasks_snapshot_for_test(tenant_id)
        .into_iter()
        .map(|task| task.id)
        .collect();
    let replicated = make_upsert_op(1, 1000, "node-b", tenant_id, "doc-1", "RetryMustLand");

    let _commit_failure = manager.fail_next_commit_for_test(tenant_id);
    let error = apply_ops_to_manager(&manager, tenant_id, std::slice::from_ref(&replicated))
        .await
        .expect_err("replication must not acknowledge a failed durable commit");
    assert!(
        error.contains("injected write-queue commit failure"),
        "replication refusal must expose the terminal commit failure: {error}"
    );
    assert!(
        manager
            .wait_for_pending_tasks(std::time::Duration::from_secs(5))
            .await,
        "failed replicated commit must reach a terminal task state"
    );

    let tasks_after_failure = manager.tenant_tasks_snapshot_for_test(tenant_id);
    let failed_tasks: Vec<_> = tasks_after_failure
        .iter()
        .filter(|task| !original_task_ids.contains(&task.id))
        .collect();
    assert_eq!(
        failed_tasks.len(),
        1,
        "the failed attempt must create exactly one observable task"
    );
    assert!(
        matches!(&failed_tasks[0].status, TaskStatus::Failed(message) if message.contains("injected write-queue commit failure")),
        "the newly created task must reach the expected Failed state: {:?}",
        failed_tasks[0].status
    );
    assert_eq!(
        manager.tenant_doc_count(tenant_id),
        Some(0),
        "failed commit must leave the exact document count unchanged"
    );
    assert!(
        manager.get_document(tenant_id, "doc-1").unwrap().is_none(),
        "failed commit must not publish the replicated body"
    );
    assert_eq!(
        manager.get_object_version(tenant_id, "doc-1").unwrap(),
        None,
        "failed commit must not publish a durable conflict version"
    );

    manager.graceful_shutdown().await;
    drop(manager);

    let restarted = IndexManager::new_with_node_id(tmp.path(), "node-a");
    assert!(
        restarted
            .get_document(tenant_id, "doc-1")
            .unwrap()
            .is_none(),
        "a commit refused before Tantivy persistence must not resurrect after restart"
    );
    assert_eq!(restarted.tenant_doc_count(tenant_id), Some(0));
    assert_eq!(
        restarted.get_object_version(tenant_id, "doc-1").unwrap(),
        None,
        "the refused attempt must not publish a conflict version after restart"
    );

    let acked_seq = apply_ops_to_manager(&restarted, tenant_id, std::slice::from_ref(&replicated))
        .await
        .expect("retrying the refused replicated operation must succeed");
    assert_eq!(acked_seq, 1);
    let document = restarted
        .get_document(tenant_id, "doc-1")
        .unwrap()
        .expect("the successful retry must publish the replicated document");
    assert!(
        matches!(
            document.fields.get("name"),
            Some(flapjack::types::FieldValue::Text(value)) if value == "RetryMustLand"
        ),
        "restart recovery must publish the exact replicated body"
    );
    assert_eq!(restarted.tenant_doc_count(tenant_id), Some(1));
    assert_eq!(
        restarted.get_object_version(tenant_id, "doc-1").unwrap(),
        Some(expected_upsert_version(&replicated, 1))
    );

    restarted.graceful_shutdown().await;
    drop(restarted);

    let retried_restart = IndexManager::new_with_node_id(tmp.path(), "node-a");
    let document = retried_restart
        .get_document(tenant_id, "doc-1")
        .unwrap()
        .expect("the successful retry must remain present after restart");
    assert!(
        matches!(
            document.fields.get("name"),
            Some(flapjack::types::FieldValue::Text(value)) if value == "RetryMustLand"
        ),
        "restart must preserve the exact retried replicated body"
    );
    assert_eq!(retried_restart.tenant_doc_count(tenant_id), Some(1));
    assert_eq!(
        retried_restart
            .get_object_version(tenant_id, "doc-1")
            .unwrap(),
        Some(expected_upsert_version(&replicated, 1))
    );
}

/// Verify that a newer upsert wins over an older upsert for the same document.
#[tokio::test]
async fn replication_ack_stale_lww_upsert_is_idempotent_success() {
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
    // Establish the newer version durably before the conflicting op is admitted:
    // admission reads the durable VersionStore, which finalization writes only
    // after the searcher is refreshed.
    wait_for_finalization(&manager).await;

    // Apply op at ts=1000 (older) — rejected before any async work is queued.
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
    assert_eq!(manager.tenant_doc_count("t1"), Some(1));
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(expected_upsert_version(&op_newer[0], 1))
    );
}

#[tokio::test]
async fn durable_version_read_failure_refuses_replicated_document() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("t1").unwrap();
    let version_store_directory = tmp.path().join("t1").join("version_store");
    if version_store_directory.exists() {
        std::fs::remove_dir_all(&version_store_directory).unwrap();
    }
    std::fs::write(&version_store_directory, b"not a directory").unwrap();

    let error = apply_ops_to_manager(
        &manager,
        "t1",
        &[make_upsert_op(
            1,
            1000,
            "node-a",
            "t1",
            "doc1",
            "MustNotQueue",
        )],
    )
    .await
    .unwrap_err();

    assert!(
        error.contains("failed to read durable object version"),
        "storage failure must be surfaced as a replication refusal: {error}"
    );
    assert_eq!(manager.tenant_doc_count("t1"), Some(0));
    assert!(manager.get_document("t1", "doc1").unwrap().is_none());
}

/// Verify that when a batch contains both a newer and an older upsert for the same document, only the newer version is persisted.
#[tokio::test]
async fn replicated_batch_with_out_of_order_tuples_keeps_the_newest_body() {
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
    assert!(
        matches!(
            doc.fields.get("name"),
            Some(flapjack::types::FieldValue::Text(value)) if value == "Final"
        ),
        "stale op should not overwrite newer; got: {:?}",
        doc.fields.get("name")
    );
    assert_eq!(manager.tenant_doc_count("t1"), Some(1));
    wait_for_finalization(&manager).await;
    let versions = VersionStore::open(&tmp.path().join("t1")).unwrap();
    assert_eq!(
        versions.get("doc1").unwrap(),
        Some(expected_upsert_version(&ops[0], 1))
    );
}

// ── Durable versions: tie-break by node_id ──

/// Verify that equal timestamps are resolved by lexicographically ordered node ID.
#[tokio::test]
async fn durable_version_same_timestamp_higher_node_id_wins() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Apply from "z-node" — poll until visible
    let op_z = vec![make_upsert_op(1, 1000, "z-node", "t1", "doc1", "ZNode")];
    apply_ops_to_manager(&manager, "t1", &op_z).await.unwrap();
    wait_for_field(&manager, "t1", "doc1", "name", "ZNode").await;
    // Establish z-node's version durably before the tie-break op is admitted.
    wait_for_finalization(&manager).await;

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
    assert_eq!(manager.tenant_doc_count("t1"), Some(1));
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(expected_upsert_version(&op_z[0], 1))
    );
}

// ── Durable versions: stale delete is rejected ──

/// Verify that an older delete does not remove a document written by a newer upsert.
#[tokio::test]
async fn durable_version_stale_delete_does_not_remove_newer_upsert() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());

    // Write doc at ts=2000 — poll until visible
    let upsert = vec![make_upsert_op(1, 2000, "node-a", "t1", "doc1", "Alice")];
    apply_ops_to_manager(&manager, "t1", &upsert).await.unwrap();
    wait_for_doc_exists(&manager, "t1", "doc1").await;
    // Establish the newer upsert's version durably before the stale delete is
    // admitted: the delete gate reads the durable VersionStore.
    wait_for_finalization(&manager).await;

    // Try to delete with stale ts=1000 — rejected before any async work is queued.
    let del = vec![make_delete_op(2, 1000, "node-b", "t1", "doc1")];
    apply_ops_to_manager(&manager, "t1", &del).await.unwrap();

    let doc = manager.get_document("t1", "doc1").unwrap();
    assert!(doc.is_some(), "stale delete should not remove a newer doc");
    assert_eq!(manager.tenant_doc_count("t1"), Some(1));
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(expected_upsert_version(&upsert[0], 1))
    );
}

// ── Durable versions: same-node ops apply in sequence ──

/// Verify that sequential upserts from the same node with increasing timestamps are all applied in order.
#[tokio::test]
async fn durable_version_same_node_sequential_ops_always_apply() {
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
    assert_eq!(manager.tenant_doc_count("t1"), Some(1));
    wait_for_finalization(&manager).await;
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(expected_upsert_version(&op2[0], 2))
    );
}

// ── Durable version: primary write blocks stale replicated op ──

/// Verify that a primary write publishes durable conflict evidence which blocks an older replicated upsert.
#[tokio::test]
async fn durable_version_primary_write_blocks_stale_replicated_op() {
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

    let primary_entry = manager
        .get_or_create_oplog("t1")
        .unwrap()
        .read_since(0)
        .unwrap()
        .into_iter()
        .find(|entry| entry.op_type == "upsert")
        .expect("primary write must append an upsert");
    let primary_version = expected_upsert_version(&primary_entry, primary_entry.seq);
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(primary_version.clone())
    );

    // Now try to replicate a stale op with ts=1 (much older than primary write).
    // Durable conflict admission rejects this before queuing any async work.
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
    assert_eq!(manager.tenant_doc_count("t1"), Some(1));
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(primary_version)
    );
}

// ── Durable version: primary delete blocks stale replicated upsert ──

/// Verify that a primary delete publishes a durable tombstone which blocks an older replicated upsert.
#[tokio::test]
async fn durable_version_primary_delete_blocks_stale_replicated_upsert() {
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

    let delete_entry = manager
        .get_or_create_oplog("t1")
        .unwrap()
        .read_since(0)
        .unwrap()
        .into_iter()
        .find(|entry| entry.op_type == "delete")
        .expect("primary delete must append a tombstone");
    let delete_version = expected_delete_version(&delete_entry, delete_entry.seq);
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(delete_version.clone())
    );

    // Now try to replicate a stale upsert with ts=1 — rejected immediately.
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
    assert_eq!(manager.tenant_doc_count("t1"), Some(0));
    assert_eq!(
        manager.get_object_version("t1", "doc1").unwrap(),
        Some(delete_version)
    );
}

// ── Durable version survives restart ──

/// Verify that a durable version survives restart and blocks an older replicated upsert.
#[tokio::test]
async fn durable_version_blocks_stale_op_after_restart() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    // PHASE 1: Primary write establishes durable version state through the oplog.
    let primary_version;
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

        // Capture the oplog tuple that recovery must preserve.
        let oplog = manager.get_or_create_oplog("t_restart").unwrap();
        let ops = oplog.read_since(0).unwrap();
        let upsert_op = ops
            .iter()
            .find(|o| o.op_type == "upsert")
            .expect("should have upsert in oplog after primary write");
        assert!(
            upsert_op.timestamp_ms > 0,
            "oplog should record a real timestamp"
        );
        primary_version = expected_upsert_version(upsert_op, upsert_op.seq);
        assert_eq!(
            manager.get_object_version("t_restart", "doc1").unwrap(),
            Some(primary_version.clone())
        );

        manager.graceful_shutdown().await;
    }

    // PHASE 2: Restart and consult the durable version owner.
    {
        let manager = IndexManager::new(&base);

        let stale_op = vec![make_upsert_op(
            99,
            primary_version.timestamp_ms.saturating_sub(1),
            "remote-node",
            "t_restart",
            "doc1",
            "StaleOverwrite",
        )];
        apply_ops_to_manager(&manager, "t_restart", &stale_op)
            .await
            .unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let fetched = manager.get_document("t_restart", "doc1").unwrap();
        assert!(
            fetched.is_some(),
            "doc1 must exist (was written by primary)"
        );
        let name = fetched.unwrap().fields.get("name").cloned();
        assert!(
            matches!(&name, Some(flapjack::types::FieldValue::Text(s)) if s == "Original"),
            "stale replicated op must not overwrite after restart; got: {:?}",
            name
        );
        assert_eq!(manager.tenant_doc_count("t_restart"), Some(1));
        assert_eq!(
            manager.get_object_version("t_restart", "doc1").unwrap(),
            Some(primary_version)
        );

        manager.graceful_shutdown().await;
    }
}

// ── Durable version survives a clean restart ──

/// Verify that a clean restart retains durable conflict evidence with no replay tail.
#[tokio::test]
async fn durable_version_survives_normal_restart_without_uncommitted_ops() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let primary_version;
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
        let primary_entry = ops
            .iter()
            .find(|o| o.op_type == "upsert")
            .expect("primary upsert must be retained");
        assert!(primary_entry.timestamp_ms > 0);
        primary_version = expected_upsert_version(primary_entry, primary_entry.seq);
        assert_eq!(
            manager
                .get_object_version("t_normal_restart", "docA")
                .unwrap(),
            Some(primary_version.clone())
        );

        // Normal clean shutdown: committed_seq is updated, no uncommitted ops
        manager.graceful_shutdown().await;
    }

    // Restart: committed_seq is current, so no document replay is needed.
    {
        let manager = IndexManager::new(&base);

        let stale_op = vec![make_upsert_op(
            99,
            primary_version.timestamp_ms.saturating_sub(1),
            "remote-node",
            "t_normal_restart",
            "docA",
            "ShouldBeRejected",
        )];
        apply_ops_to_manager(&manager, "t_normal_restart", &stale_op)
            .await
            .unwrap();

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        let fetched = manager.get_document("t_normal_restart", "docA").unwrap();
        assert!(fetched.is_some());
        let name = fetched.unwrap().fields.get("name").cloned();
        assert!(
            matches!(&name, Some(flapjack::types::FieldValue::Text(s)) if s == "Persisted"),
            "stale op must be rejected even after clean shutdown restart; got: {:?}",
            name
        );
        assert_eq!(
            manager
                .get_object_version("t_normal_restart", "docA")
                .unwrap(),
            Some(primary_version)
        );

        manager.graceful_shutdown().await;
    }
}

/// Verify that restart exposes the committed durable version before any replay early return.
#[tokio::test]
async fn durable_version_is_readable_when_committed_seq_is_current() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let primary_version;
    {
        let manager = IndexManager::new(&base);
        manager.create_tenant("t_version_populate").unwrap();
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
            .add_documents_sync("t_version_populate", vec![doc])
            .await
            .unwrap();

        let oplog = manager.get_or_create_oplog("t_version_populate").unwrap();
        let upsert = oplog
            .read_since(0)
            .unwrap()
            .into_iter()
            .find(|entry| entry.op_type == "upsert")
            .expect("expected upsert in oplog");
        primary_version = expected_upsert_version(&upsert, upsert.seq);

        manager.graceful_shutdown().await;
    }

    {
        let manager = IndexManager::new(&base);
        let _ = manager.get_document("t_version_populate", "doc1").unwrap();

        assert_eq!(
            manager
                .get_object_version("t_version_populate", "doc1")
                .unwrap(),
            Some(primary_version)
        );

        manager.graceful_shutdown().await;
    }
}

// ── Durable version blocks stale DELETE after restart ──

/// Verify that after restart durable conflict evidence blocks an older replicated delete.
#[tokio::test]
async fn durable_version_blocks_stale_delete_after_restart() {
    let tmp = TempDir::new().unwrap();
    let base = tmp.path().to_path_buf();

    let primary_version;
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
        let primary_entry = ops
            .iter()
            .find(|o| o.op_type == "upsert")
            .expect("should have upsert in oplog");
        assert!(primary_entry.timestamp_ms > 0);
        primary_version = expected_upsert_version(primary_entry, primary_entry.seq);

        manager.graceful_shutdown().await;
    }

    // Restart: the durable tuple blocks a delete one millisecond older.
    {
        let manager = IndexManager::new(&base);

        let stale_delete = vec![make_delete_op(
            99,
            primary_version.timestamp_ms.saturating_sub(1),
            "remote-node",
            "t_del_restart",
            "doc1",
        )];
        apply_ops_to_manager(&manager, "t_del_restart", &stale_delete)
            .await
            .unwrap();

        let fetched = manager.get_document("t_del_restart", "doc1").unwrap();
        assert!(
            fetched.is_some(),
            "stale delete must not remove doc after restart"
        );
        assert_eq!(manager.tenant_doc_count("t_del_restart"), Some(1));
        assert_eq!(
            manager.get_object_version("t_del_restart", "doc1").unwrap(),
            Some(primary_version)
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

#[tokio::test]
async fn replication_ack_rejects_malformed_resource_payload() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let op = make_index_op(
        61,
        1_000,
        "node-a",
        "t1",
        "save_synonym",
        serde_json::json!({
            "objectID": "broken",
            "type": "synonym",
            "synonyms": "not-an-array"
        }),
    );

    let error = apply_ops_to_manager(&manager, "t1", &[op])
        .await
        .expect_err("a malformed resource operation must not be acknowledged");

    assert!(
        error.contains("save_synonym seq 61 invalid payload"),
        "replication refusal should identify the malformed resource: {error}"
    );
    assert!(!tmp.path().join("t1").join("synonyms.json").exists());
}

#[tokio::test]
async fn replication_ack_rejects_resource_persistence_failure() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("t1").unwrap();
    let synonyms_path = tmp.path().join("t1").join("synonyms.json");
    std::fs::create_dir(&synonyms_path).unwrap();
    let op = make_index_op(
        62,
        1_000,
        "node-a",
        "t1",
        "save_synonym",
        serde_json::json!({
            "objectID": "persist-me",
            "type": "synonym",
            "synonyms": ["tv", "television"]
        }),
    );

    let error = apply_ops_to_manager(&manager, "t1", &[op])
        .await
        .expect_err("a resource persistence failure must not be acknowledged");

    assert!(
        error.contains("save_synonym seq 62 failed"),
        "replication refusal should identify the failed resource write: {error}"
    );
    assert!(synonyms_path.is_dir());
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

// ── Unknown op type rejected ──

#[tokio::test]
async fn replication_ack_rejects_unknown_op_type() {
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
    let error = apply_ops_to_manager(&manager, "t1", &[op])
        .await
        .expect_err("unknown operation types must not be acknowledged");
    assert!(
        error.contains("unknown op_type noop_unknown"),
        "replication refusal should identify the unknown operation: {error}"
    );
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
        .route(
            "/internal/replicate",
            post(super::replicate_ops_with_headers),
        )
        .route("/internal/ops", get(super::get_ops))
        .route("/internal/tenants", get(super::list_tenants))
        .with_state(state)
}

fn release_transport_headers(
    after_seq: u64,
    through_seq: u64,
    ops: &[OpLogEntry],
) -> Vec<(&'static str, String)> {
    vec![
        (
            flapjack_replication::types::RELEASE_TRANSFER_CONTRACT_HEADER,
            flapjack_replication::types::RELEASE_TRANSFER_CONTRACT_V1.to_string(),
        ),
        (
            flapjack_replication::types::RELEASE_TRANSFER_TENANT_HEADER,
            "products".to_string(),
        ),
        (
            flapjack_replication::types::RELEASE_TRANSFER_TRANSACTION_HEADER,
            "rehx-replicate-transaction".to_string(),
        ),
        (
            flapjack_replication::types::RELEASE_TRANSFER_AFTER_SEQ_HEADER,
            after_seq.to_string(),
        ),
        (
            flapjack_replication::types::RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
            through_seq.to_string(),
        ),
        (
            flapjack_replication::types::RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
            canonical_release_operations_sha256(ops).unwrap(),
        ),
    ]
}

#[test]
fn reh_x2_release_request_headers_bind_tenant_and_reject_duplicates() {
    let mut read = HeaderMap::new();
    read.insert(
        RELEASE_TRANSFER_CONTRACT_HEADER,
        HeaderValue::from_static(RELEASE_TRANSFER_CONTRACT_V1),
    );
    read.insert(
        RELEASE_TRANSFER_TENANT_HEADER,
        HeaderValue::from_static("products"),
    );
    read.insert(
        RELEASE_TRANSFER_TRANSACTION_HEADER,
        HeaderValue::from_static("rehx-read-transaction"),
    );
    assert!(
        strict_release_request(&read, "products", ReleaseRequestMode::Snapshot)
            .unwrap()
            .is_some()
    );

    for (name, mutated) in [
        ("missing tenant", {
            let mut headers = read.clone();
            headers.remove(RELEASE_TRANSFER_TENANT_HEADER);
            headers
        }),
        ("foreign tenant", {
            let mut headers = read.clone();
            headers.insert(
                RELEASE_TRANSFER_TENANT_HEADER,
                HeaderValue::from_static("foreign"),
            );
            headers
        }),
        ("duplicate tenant", {
            let mut headers = read.clone();
            headers.append(
                RELEASE_TRANSFER_TENANT_HEADER,
                HeaderValue::from_static("products"),
            );
            headers
        }),
        ("duplicate contract", {
            let mut headers = read.clone();
            headers.append(
                RELEASE_TRANSFER_CONTRACT_HEADER,
                HeaderValue::from_static(RELEASE_TRANSFER_CONTRACT_V1),
            );
            headers
        }),
        ("response-only status", {
            let mut headers = read.clone();
            headers.insert(
                RELEASE_TRANSFER_STATUS_HEADER,
                HeaderValue::from_static(RELEASE_TRANSFER_STATUS_CONTIGUOUS),
            );
            headers
        }),
    ] {
        assert!(
            strict_release_request(&mutated, "products", ReleaseRequestMode::Snapshot).is_err(),
            "{name} must be rejected"
        );
    }

    let mut apply = read.clone();
    apply.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("0"),
    );
    apply.insert(
        RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
        HeaderValue::from_static("1"),
    );
    apply.insert(
        RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
        HeaderValue::from_static(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    );
    assert_eq!(
        strict_release_request(&apply, "products", ReleaseRequestMode::Apply)
            .unwrap()
            .unwrap()
            .window,
        Some((0, 1))
    );
    apply.append(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("0"),
    );
    assert!(strict_release_request(&apply, "products", ReleaseRequestMode::Apply).is_err());

    assert!(
        strict_release_request(&HeaderMap::new(), "products", ReleaseRequestMode::Snapshot)
            .unwrap()
            .is_none()
    );
    let mut stray = HeaderMap::new();
    stray.insert(
        RELEASE_TRANSFER_TENANT_HEADER,
        HeaderValue::from_static("products"),
    );
    assert!(strict_release_request(&stray, "products", ReleaseRequestMode::Snapshot).is_err());
}

#[test]
fn reh_x2_release_requests_bind_transaction_interval_and_payload_digest() {
    let mut snapshot = HeaderMap::new();
    snapshot.insert(
        RELEASE_TRANSFER_CONTRACT_HEADER,
        HeaderValue::from_static(RELEASE_TRANSFER_CONTRACT_V1),
    );
    snapshot.insert(
        RELEASE_TRANSFER_TENANT_HEADER,
        HeaderValue::from_static("products"),
    );
    snapshot.insert(
        RELEASE_TRANSFER_TRANSACTION_HEADER,
        HeaderValue::from_static("rehx-transaction-1"),
    );
    let parsed = strict_release_request(&snapshot, "products", ReleaseRequestMode::Snapshot)
        .unwrap()
        .unwrap();
    assert_eq!(parsed.transaction_id, "rehx-transaction-1");
    assert!(parsed.window.is_none());
    assert!(parsed.payload_sha256.is_none());

    let mut tail = snapshot.clone();
    tail.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("2"),
    );
    let parsed = strict_release_request(&tail, "products", ReleaseRequestMode::Tail(2))
        .unwrap()
        .unwrap();
    assert_eq!(parsed.window, Some((2, 2)));

    let mut apply = snapshot.clone();
    apply.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("2"),
    );
    apply.insert(
        RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
        HeaderValue::from_static("3"),
    );
    apply.insert(
        RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
        HeaderValue::from_static(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    );
    let parsed = strict_release_request(&apply, "products", ReleaseRequestMode::Apply)
        .unwrap()
        .unwrap();
    assert_eq!(parsed.window, Some((2, 3)));
    assert_eq!(
        parsed.payload_sha256.as_deref(),
        Some("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );

    for (name, mutated) in [
        ("missing transaction", {
            let mut headers = snapshot.clone();
            headers.remove(RELEASE_TRANSFER_TRANSACTION_HEADER);
            headers
        }),
        ("wrong tail interval", {
            let mut headers = tail.clone();
            headers.insert(
                RELEASE_TRANSFER_AFTER_SEQ_HEADER,
                HeaderValue::from_static("1"),
            );
            headers
        }),
        ("noncanonical payload digest", {
            let mut headers = apply.clone();
            headers.insert(
                RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
                HeaderValue::from_static(
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                ),
            );
            headers
        }),
    ] {
        let mode = if name == "wrong tail interval" {
            ReleaseRequestMode::Tail(2)
        } else if name == "noncanonical payload digest" {
            ReleaseRequestMode::Apply
        } else {
            ReleaseRequestMode::Snapshot
        };
        assert!(
            strict_release_request(&mutated, "products", mode).is_err(),
            "{name} must fail before effects"
        );
    }
}

#[test]
fn reh_x2_release_apply_receipt_survives_restart_and_rejects_drift() {
    let tmp = TempDir::new().unwrap();
    let data_root = tmp.path().join("data");
    std::fs::create_dir(&data_root).unwrap();
    let digest = "b".repeat(64);

    let first =
        prepare_release_apply_receipt(&data_root, "products", 2, 3, "rehx-transaction-1", &digest)
            .unwrap();
    let receipt_path = match first {
        ReleaseApplyDisposition::Apply(guard) => {
            assert!(!guard.receipt_path.starts_with(&data_root));
            let path = guard.receipt_path.clone();
            drop(guard);
            path
        }
        ReleaseApplyDisposition::Replay(_) => panic!("first interval must be prepared"),
    };
    assert!(receipt_path.exists());

    let restarted =
        prepare_release_apply_receipt(&data_root, "products", 2, 3, "rehx-transaction-1", &digest)
            .unwrap();
    match restarted {
        ReleaseApplyDisposition::Apply(guard) => guard.commit(3).unwrap(),
        ReleaseApplyDisposition::Replay(_) => panic!("prepared restart must reapply"),
    }

    match prepare_release_apply_receipt(&data_root, "products", 2, 3, "rehx-transaction-1", &digest)
        .unwrap()
    {
        ReleaseApplyDisposition::Replay(acked) => assert_eq!(acked, 3),
        ReleaseApplyDisposition::Apply(_) => panic!("committed replay must skip effects"),
    }

    assert!(prepare_release_apply_receipt(
        &data_root,
        "products",
        2,
        3,
        "rehx-transaction-2",
        &digest,
    )
    .unwrap_err()
    .contains("does not match"));
    assert!(prepare_release_apply_receipt(
        &data_root,
        "products",
        2,
        3,
        "rehx-transaction-1",
        &"c".repeat(64),
    )
    .unwrap_err()
    .contains("does not match"));
}

#[test]
fn reh_x2_release_payload_digest_uses_cross_runtime_canonical_json() {
    let mut op = make_upsert_op(1, 1_000, "source", "products", "one", "One");
    let body = op.payload["body"].as_object_mut().unwrap();
    body.insert("unicode".to_string(), serde_json::json!("é雪𝄞"));
    body.insert("small".to_string(), serde_json::json!(1e-7));
    body.insert("large".to_string(), serde_json::json!(1e21));
    body.insert("negativeZero".to_string(), serde_json::json!(-0.0));
    assert_eq!(
        canonical_release_operations_sha256(&[op]).unwrap(),
        "1cf6e676ad9fd2ce3af1b642d94825f81f6a0f1773533431dea481fad780e42f",
        "Rust must match the shared Python fixture for Unicode, exponents, and -0.0"
    );
    assert_eq!(
        canonical_release_json_bytes(&serde_json::from_str("1e-7").unwrap()).unwrap(),
        canonical_release_json_bytes(&serde_json::from_str("0.0000001").unwrap()).unwrap()
    );
    assert_eq!(
        canonical_release_json_bytes(&serde_json::json!(-0.0)).unwrap(),
        canonical_release_json_bytes(&serde_json::json!(0.0)).unwrap()
    );
    assert_eq!(
        format!(
            "{:x}",
            Sha256::digest(
                canonical_release_json_bytes(&serde_json::json!({
                    "é": 1,
                    "雪": 2,
                    "𝄞": 3,
                    "a": 4,
                }))
                .unwrap()
            )
        ),
        "c4f655a3795ba4dc6be1e92392fb020676b7cf60f4381c90bf22595a3e75db64"
    );
}

#[tokio::test]
async fn reh_x2_release_http_accepts_shared_canonical_digest_and_rejects_drift() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(Arc::clone(&state));
    let mut op = make_upsert_op(1, 1_000, "source", "products", "one", "One");
    let body = op.payload["body"].as_object_mut().unwrap();
    body.insert("unicode".to_string(), serde_json::json!("é雪𝄞"));
    body.insert("small".to_string(), serde_json::json!(1e-7));
    body.insert("large".to_string(), serde_json::json!(1e21));
    body.insert("negativeZero".to_string(), serde_json::json!(-0.0));
    let operations = vec![op];
    let shared_digest = "1cf6e676ad9fd2ce3af1b642d94825f81f6a0f1773533431dea481fad780e42f";
    let request_body = flapjack_replication::types::ReplicateOpsRequest {
        tenant_id: "products".to_string(),
        ops: operations.clone(),
    };

    let mut drifted = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, mut value) in release_transport_headers(0, 1, &operations) {
        if name == RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER {
            value = "0cf6e676ad9fd2ce3af1b642d94825f81f6a0f1773533431dea481fad780e42f".to_string();
        }
        drifted = drifted.header(name, value);
    }
    let drifted_response = app
        .clone()
        .oneshot(
            drifted
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(drifted_response.status(), StatusCode::BAD_REQUEST);
    assert!(!state.manager.base_path.join("products").exists());

    let mut exact = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in release_transport_headers(0, 1, &operations) {
        if name == RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER {
            assert_eq!(value, shared_digest);
        }
        exact = exact.header(name, value);
    }
    let exact_response = app
        .oneshot(
            exact
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(exact_response.status(), StatusCode::OK);
    assert_eq!(
        exact_response.headers()[RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER],
        shared_digest
    );
}

#[tokio::test]
async fn reh_x2_release_apply_recovers_after_effects_before_ack_commit() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let ops = vec![
        make_upsert_op(1, 1_000, "source", "products", "one", "One"),
        make_upsert_op(2, 1_001, "source", "products", "two", "Two"),
    ];
    let payload_sha256 = canonical_release_operations_sha256(&ops).unwrap();
    let prepared = prepare_release_apply_receipt(
        &state.manager.base_path,
        "products",
        0,
        2,
        "rehx-replicate-transaction",
        &payload_sha256,
    )
    .unwrap();
    let guard = match prepared {
        ReleaseApplyDisposition::Apply(guard) => guard,
        ReleaseApplyDisposition::Replay(_) => panic!("first interval must be prepared"),
    };

    assert_eq!(
        apply_ops_to_state(&state, "products", &ops).await.unwrap(),
        2
    );
    drop(guard); // Simulate termination after effects but before durable commit/ACK.

    let request_body = flapjack_replication::types::ReplicateOpsRequest {
        tenant_id: "products".to_string(),
        ops: ops.clone(),
    };
    let mut request = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in release_transport_headers(0, 2, &ops) {
        request = request.header(name, value);
    }
    let response = internal_replication_router(Arc::clone(&state))
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    match prepare_release_apply_receipt(
        &state.manager.base_path,
        "products",
        0,
        2,
        "rehx-replicate-transaction",
        &payload_sha256,
    )
    .unwrap()
    {
        ReleaseApplyDisposition::Replay(acked) => assert_eq!(acked, 2),
        ReleaseApplyDisposition::Apply(_) => panic!("retried prepared effects must commit"),
    }
    assert_eq!(
        state
            .manager
            .get_document("products", "one")
            .unwrap()
            .unwrap()
            .fields
            .get("name"),
        Some(&flapjack::types::FieldValue::Text("One".to_string()))
    );
}

#[tokio::test]
async fn reh_x2_release_apply_rejects_initial_body_digest_mismatch_before_effects() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let ops = vec![make_upsert_op(1, 1_000, "source", "products", "one", "One")];
    let mut request = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, mut value) in release_transport_headers(0, 1, &ops) {
        if name == RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER {
            value = "a".repeat(64);
        }
        request = request.header(name, value);
    }
    let response = internal_replication_router(Arc::clone(&state))
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                        tenant_id: "products".to_string(),
                        ops,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!state.manager.base_path.join("products").exists());
}

#[test]
fn reh_x2_release_tail_projection_rejects_false_zero_and_sequence_defects() {
    let contiguous = vec![
        make_upsert_op(1, 1_000, "source", "products", "one", "One"),
        make_upsert_op(2, 1_001, "source", "products", "two", "Two"),
    ];
    let projection = release_tail_projection(0, 2, Some(1), contiguous.clone()).unwrap();
    assert_eq!(projection.status, ReleaseTailStatus::Contiguous);
    assert_eq!(projection.through_seq, 2);
    assert_eq!(projection.ops.len(), 2);

    let sequence_zero = vec![make_upsert_op(
        0, 1_000, "source", "products", "zero", "Zero",
    )];
    assert!(release_tail_projection(0, 0, Some(0), sequence_zero)
        .unwrap_err()
        .contains("sequence 0"));

    let missing = vec![
        contiguous[0].clone(),
        make_upsert_op(3, 1_002, "source", "products", "three", "Three"),
    ];
    assert!(release_tail_projection(0, 3, Some(1), missing)
        .unwrap_err()
        .contains("noncontiguous"));

    let reversed = vec![contiguous[1].clone(), contiguous[0].clone()];
    assert!(release_tail_projection(0, 2, Some(1), reversed)
        .unwrap_err()
        .contains("noncontiguous"));

    assert!(release_tail_projection(0, 2, Some(1), Vec::new())
        .unwrap_err()
        .contains("omitted operations"));
}

#[test]
fn reh_x2_release_tail_projection_requires_resnapshot_on_retention_gap() {
    let retained = vec![
        make_upsert_op(3, 1_003, "source", "products", "three", "Three"),
        make_upsert_op(4, 1_004, "source", "products", "four", "Four"),
    ];
    let projection = release_tail_projection(0, 4, Some(3), retained).unwrap();
    assert_eq!(projection.status, ReleaseTailStatus::ResnapshotRequired);
    assert_eq!(projection.through_seq, 4);
    assert!(
        projection.ops.is_empty(),
        "a retained suffix must not masquerade as a complete tail"
    );
}

#[test]
fn reh_x2_release_tail_headers_reject_partial_unknown_and_noncanonical_windows() {
    let mut headers = HeaderMap::new();
    headers.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("0"),
    );
    assert!(
        strict_release_request(&headers, "products", ReleaseRequestMode::Apply)
            .unwrap_err()
            .contains("require the exact contract")
    );

    headers.insert(
        RELEASE_TRANSFER_CONTRACT_HEADER,
        HeaderValue::from_static("unknown-v2"),
    );
    assert!(
        strict_release_request(&headers, "products", ReleaseRequestMode::Apply)
            .unwrap_err()
            .contains("unknown")
    );

    headers.insert(
        RELEASE_TRANSFER_CONTRACT_HEADER,
        HeaderValue::from_static(RELEASE_TRANSFER_CONTRACT_V1),
    );
    headers.insert(
        RELEASE_TRANSFER_TENANT_HEADER,
        HeaderValue::from_static("products"),
    );
    headers.insert(
        RELEASE_TRANSFER_TRANSACTION_HEADER,
        HeaderValue::from_static("rehx-window-transaction"),
    );
    headers.insert(
        RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER,
        HeaderValue::from_static(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        ),
    );
    headers.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("00"),
    );
    headers.insert(
        RELEASE_TRANSFER_THROUGH_SEQ_HEADER,
        HeaderValue::from_static("1"),
    );
    assert!(
        strict_release_request(&headers, "products", ReleaseRequestMode::Apply)
            .unwrap_err()
            .contains("not canonical")
    );

    headers.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("2"),
    );
    assert!(
        strict_release_request(&headers, "products", ReleaseRequestMode::Apply)
            .unwrap_err()
            .contains("precedes")
    );
}

#[tokio::test]
async fn reh_x2_release_ops_gap_response_omits_suffix_and_names_resnapshot() {
    let mut headers = HeaderMap::new();
    headers.insert(
        RELEASE_TRANSFER_CONTRACT_HEADER,
        HeaderValue::from_static(RELEASE_TRANSFER_CONTRACT_V1),
    );
    headers.insert(
        RELEASE_TRANSFER_TENANT_HEADER,
        HeaderValue::from_static("products"),
    );
    headers.insert(
        RELEASE_TRANSFER_TRANSACTION_HEADER,
        HeaderValue::from_static("rehx-gap-transaction"),
    );
    headers.insert(
        RELEASE_TRANSFER_AFTER_SEQ_HEADER,
        HeaderValue::from_static("0"),
    );
    let release_request = strict_release_request(&headers, "products", ReleaseRequestMode::Tail(0))
        .unwrap()
        .unwrap();
    let response = match release_ops_response(
        Some(&release_request),
        0,
        flapjack_replication::types::GetOpsResponse {
            tenant_id: "products".to_string(),
            ops: vec![make_upsert_op(
                3, 1_003, "source", "products", "three", "Three",
            )],
            current_seq: 3,
            oldest_retained_seq: Some(3),
            node_current_seqs: std::collections::BTreeMap::new(),
        },
    ) {
        Ok(response) => response,
        Err(_) => panic!("retention-gap projection should produce a structured response"),
    };

    assert_eq!(
        response.headers()[RELEASE_TRANSFER_STATUS_HEADER],
        RELEASE_TRANSFER_STATUS_RESNAPSHOT_REQUIRED
    );
    assert_eq!(
        response.headers()[RELEASE_TRANSFER_TENANT_HEADER],
        "products"
    );
    assert_eq!(
        response.headers()[RELEASE_TRANSFER_TRANSACTION_HEADER],
        "rehx-gap-transaction"
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let payload: flapjack_replication::types::GetOpsResponse =
        serde_json::from_slice(&body).unwrap();
    assert!(payload.ops.is_empty());
    assert_eq!(payload.current_seq, 3);
    assert_eq!(payload.oldest_retained_seq, Some(3));
}

#[tokio::test]
async fn reh_x2_release_replicate_ack_binds_the_exact_contiguous_window() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(Arc::clone(&state));
    let ops = vec![
        make_upsert_op(1, 1_000, "source", "products", "one", "One"),
        make_upsert_op(2, 1_001, "source", "products", "two", "Two"),
    ];
    let request_body = flapjack_replication::types::ReplicateOpsRequest {
        tenant_id: "products".to_string(),
        ops: ops.clone(),
    };
    let payload_sha256 = canonical_release_operations_sha256(&ops).unwrap();
    let headers = release_transport_headers(0, 2, &ops);
    let mut request = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_STATUS_HEADER],
        flapjack_replication::types::RELEASE_TRANSFER_STATUS_ACKNOWLEDGED
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_TENANT_HEADER],
        "products"
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_AFTER_SEQ_HEADER],
        "0"
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_THROUGH_SEQ_HEADER],
        "2"
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_TRANSACTION_HEADER],
        "rehx-replicate-transaction"
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER],
        payload_sha256
    );
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let ack: flapjack_replication::types::ReplicateOpsResponse =
        serde_json::from_slice(&body).unwrap();
    assert_eq!(ack.tenant_id, "products");
    assert_eq!(ack.acked_seq, 2);

    let headers = release_transport_headers(0, 2, &ops);
    let mut replay = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in headers {
        replay = replay.header(name, value);
    }
    let replay_response = app
        .clone()
        .oneshot(
            replay
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay_response.status(), StatusCode::OK);
    assert_eq!(
        replay_response.headers()[RELEASE_TRANSFER_STATUS_HEADER],
        RELEASE_TRANSFER_STATUS_ACKNOWLEDGED,
        "idempotent replay of the exact same source interval must retain the same ACK"
    );

    let changed_ops = vec![
        make_upsert_op(1, 1_000, "source", "products", "one", "Changed"),
        make_upsert_op(2, 1_001, "source", "products", "two", "Two"),
    ];
    let mut changed = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in release_transport_headers(0, 2, &ops) {
        changed = changed.header(name, value);
    }
    let changed_response = app
        .clone()
        .oneshot(
            changed
                .body(Body::from(
                    serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                        tenant_id: "products".to_string(),
                        ops: changed_ops,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(changed_response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        state
            .manager
            .get_document("products", "one")
            .unwrap()
            .unwrap()
            .fields
            .get("name"),
        Some(&flapjack::types::FieldValue::Text("One".to_string())),
        "changed-body replay must fail before changing destination state"
    );

    let mut changed_transaction = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, mut value) in release_transport_headers(0, 2, &ops) {
        if name == RELEASE_TRANSFER_TRANSACTION_HEADER {
            value = "rehx-replicate-transaction-2".to_string();
        }
        changed_transaction = changed_transaction.header(name, value);
    }
    let changed_transaction_response = app
        .oneshot(
            changed_transaction
                .body(Body::from(serde_json::to_vec(&request_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        changed_transaction_response.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn reh_x2_release_replicate_rejects_sequence_zero_before_effects() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let app = internal_replication_router(Arc::clone(&state));
    let invalid_ops = vec![make_upsert_op(
        0, 1_000, "source", "products", "zero", "Zero",
    )];
    let headers = release_transport_headers(0, 0, &invalid_ops);
    let mut request = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = app
        .clone()
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                        tenant_id: "products".to_string(),
                        ops: invalid_ops,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert!(!state.manager.base_path.join("products").exists());

    let empty_ops = Vec::new();
    let headers = release_transport_headers(2, 2, &empty_ops);
    let mut request = Request::builder()
        .method("POST")
        .uri("/internal/replicate")
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = app
        .oneshot(
            request
                .body(Body::from(
                    serde_json::to_vec(&flapjack_replication::types::ReplicateOpsRequest {
                        tenant_id: "products".to_string(),
                        ops: empty_ops,
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn reh_x2_release_snapshot_binds_one_uid_watermark_and_bytes() {
    use sha2::{Digest, Sha256};

    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "release_snapshot_products";
    state.manager.create_tenant(tenant_id).unwrap();
    state
        .manager
        .get_or_create_oplog(tenant_id)
        .unwrap()
        .append(
            "upsert",
            serde_json::json!({"objectID": "one", "body": {"_id": "one"}}),
        )
        .unwrap();
    let app = Router::new()
        .route(
            "/internal/snapshot/:indexName",
            get(super::internal_snapshot),
        )
        .with_state(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri(format!("/internal/snapshot/{tenant_id}"))
                .header(
                    flapjack_replication::types::RELEASE_TRANSFER_CONTRACT_HEADER,
                    flapjack_replication::types::RELEASE_TRANSFER_CONTRACT_V1,
                )
                .header(
                    flapjack_replication::types::RELEASE_TRANSFER_TENANT_HEADER,
                    tenant_id,
                )
                .header(
                    flapjack_replication::types::RELEASE_TRANSFER_TRANSACTION_HEADER,
                    "rehx-snapshot-transaction",
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_TENANT_HEADER],
        tenant_id
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_THROUGH_SEQ_HEADER],
        "1"
    );
    assert_eq!(
        response.headers()[flapjack_replication::types::RELEASE_TRANSFER_TRANSACTION_HEADER],
        "rehx-snapshot-transaction"
    );
    let expected_digest = response.headers()
        [flapjack_replication::types::RELEASE_TRANSFER_SNAPSHOT_SHA256_HEADER]
        .to_str()
        .unwrap()
        .to_string();
    let payload_digest = response.headers()
        [flapjack_replication::types::RELEASE_TRANSFER_PAYLOAD_SHA256_HEADER]
        .to_str()
        .unwrap()
        .to_string();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&body)), expected_digest);
    assert_eq!(format!("{:x}", Sha256::digest(&body)), payload_digest);
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

#[tokio::test]
async fn replication_ack_http_endpoint_propagates_apply_failure_without_ack() {
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
                        tenant_id: "products".to_string(),
                        ops: vec![make_index_op(
                            57,
                            1_000,
                            "node-a",
                            "products",
                            "unknown_http_op",
                            serde_json::json!({}),
                        )],
                    })
                    .unwrap(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert!(
        !resp.status().is_success(),
        "a failed apply must be visible as a non-success HTTP response"
    );
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.get("acked_seq").is_none());
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

/// TODO: Document list_tenants_excludes_publication_roots.
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

/// TODO: Document get_ops_does_not_open_publication_roots_as_moved_source_candidates.
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

/// TODO: Document get_ops_moved_source_fallback_scans_valid_destinations.
#[tokio::test]
async fn get_ops_moved_source_fallback_scans_valid_destinations() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    state.manager.create_tenant("shadow_replica").unwrap();
    let destination_oplog = state.manager.get_or_create_oplog("shadow_replica").unwrap();
    destination_oplog
        .append(
            "move_index",
            serde_json::json!({"source": "missing_source", "destination": "shadow_replica"}),
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
    assert_eq!(ops[0]["payload"]["destination"], "shadow_replica");
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
    state.manager.unload_tenant("tenant_b");

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
/// TODO: Document contains_document_replication_ops_detects_upsert_and_delete.
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

/// TODO: Document test_replication_manager_with_two_peers.
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
#[allow(clippy::await_holding_lock)] // Process-global env guard must span the request.
async fn add_cluster_peer_retries_are_idempotent_but_address_changes_conflict() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _allow_cleartext =
        EnvVarRestoreGuard::set("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS", "1");
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
#[allow(clippy::await_holding_lock)] // Process-global env guard must span the request.
async fn add_cluster_peer_rejects_cleartext_transport_when_peer_key_is_configured() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _peer_key = EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "stage-2-peer-secret");
    let _allow_cleartext = EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");

    let tmp = TempDir::new().unwrap();
    let repl_mgr = flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: Vec::new(),
        },
        Some("stage-2-peer-secret".to_string()),
        tmp.path().to_path_buf(),
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
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = crate::test_helpers::body_json(response).await;
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1")),
        "runtime cleartext rejection must name the override, got {body:?}"
    );
    assert_eq!(repl_mgr.peer_count(), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Process-global env guard must span the request.
async fn add_cluster_peer_rejects_cleartext_transport_without_peer_key() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _peer_key = EnvVarRestoreGuard::remove("FLAPJACK_REPLICATION_API_KEY");
    let _allow_cleartext = EnvVarRestoreGuard::remove("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS");

    let tmp = TempDir::new().unwrap();
    let repl_mgr = flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: Vec::new(),
        },
        None,
        tmp.path().to_path_buf(),
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
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body = crate::test_helpers::body_json(response).await;
    assert!(
        body["message"].as_str().is_some_and(|message| {
            message.contains("analytics")
                && message.contains("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1")
        }),
        "runtime refusal must protect caller credentials forwarded by analytics, got {body:?}"
    );
    assert_eq!(repl_mgr.peer_count(), 0);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)] // Process-global env guard must span the request.
async fn add_cluster_peer_cleartext_escape_repermits_runtime_membership() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    let _peer_key = EnvVarRestoreGuard::set("FLAPJACK_REPLICATION_API_KEY", "stage-2-peer-secret");
    let _allow_cleartext =
        EnvVarRestoreGuard::set("FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS", "1");

    let tmp = TempDir::new().unwrap();
    let repl_mgr = flapjack_replication::manager::ReplicationManager::new(
        flapjack_replication::config::NodeConfig {
            node_id: "test-node-a".to_string(),
            bind_addr: "127.0.0.1:7700".to_string(),
            advertise_addr: None,
            bootstrap_peer: None,
            peers: Vec::new(),
        },
        Some("stage-2-peer-secret".to_string()),
        tmp.path().to_path_buf(),
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
    assert_eq!(response.status(), StatusCode::OK);
    let body = crate::test_helpers::body_json(response).await;
    assert_eq!(body["node_id"], "test-node-b");
    assert_eq!(body["addr"], "http://test-node-b:7700");
    assert_eq!(body["peers_total"], 1);
    assert_eq!(repl_mgr.peer_count(), 1);
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
        "https://test-node-b:7700",
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

/// TODO: Document remove_cluster_peer_known_peer_returns_200_and_removes_membership.
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

/// TODO: Document remove_cluster_peer_unknown_peer_returns_404_without_mutation.
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

/// The internal replication snapshot endpoint reads the tenant directory with the
/// same guarantee as every other snapshot producer: the persistent writer is
/// drained through merge quiescence first, so a replica never catches up from a
/// mid-commit generation.
#[tokio::test]
async fn internal_snapshot_quiesces_the_persistent_writer_before_reading_bytes() {
    let tmp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&tmp).build_shared();
    let tenant_id = "internal_snapshot_quiesce";
    state.manager.create_tenant(tenant_id).unwrap();
    state
        .manager
        .add_documents_sync(
            tenant_id,
            vec![flapjack::types::Document {
                id: "replicated_one".to_string(),
                fields: std::collections::HashMap::from([(
                    "title".to_string(),
                    flapjack::types::FieldValue::Text("replicated first".to_string()),
                )]),
            }],
        )
        .await
        .unwrap();
    let merge_wait_before = crate::test_helpers::retained_channel_closed_count(tenant_id);

    let app = Router::new()
        .route(
            "/internal/snapshot/:indexName",
            get(super::internal_snapshot),
        )
        .with_state(std::sync::Arc::clone(&state));
    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/internal/snapshot/{tenant_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    crate::test_helpers::assert_retained_channel_closed_delta(
        tenant_id,
        merge_wait_before,
        "internal snapshot export must drain and merge-quiesce the persistent writer before reading bytes",
    );
    crate::test_helpers::assert_quiescence_before_publication(tenant_id, "snapshot_export_read");

    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let restore_dir = TempDir::new().unwrap();
    let restored = restore_dir.path().join(tenant_id);
    flapjack::index::snapshot::import_from_bytes(&bytes, &restored).unwrap();
    let restored_manager = IndexManager::new(restore_dir.path());
    assert_eq!(
        restored_manager
            .search(tenant_id, "", None, None, 10)
            .unwrap()
            .total,
        1,
        "internal snapshot bytes must contain the committed generation"
    );
}
