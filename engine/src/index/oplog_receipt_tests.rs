use super::*;
use tempfile::TempDir;

#[test]
fn replication_origin_proof_upsert_digest_is_canonical_and_binds_vectors() {
    let first = crate::types::Document::from_json(&serde_json::json!({
        "objectID": "doc-a",
        "title": "same",
        "_vectors": {"default": [0.1, 0.2]}
    }))
    .unwrap();
    let reordered = crate::types::Document::from_json(&serde_json::json!({
        "_vectors": {"default": [0.1, 0.2]},
        "title": "same",
        "objectID": "doc-a"
    }))
    .unwrap();
    let changed_vector = crate::types::Document::from_json(&serde_json::json!({
        "objectID": "doc-a",
        "title": "same",
        "_vectors": {"default": [0.1, 0.3]}
    }))
    .unwrap();

    assert_eq!(
        upsert_effect_digest(&first),
        upsert_effect_digest(&reordered)
    );
    assert_ne!(
        upsert_effect_digest(&first),
        upsert_effect_digest(&changed_vector),
        "vector content is part of the accepted logical effect"
    );
}

#[test]
fn append_batch_returns_primary_receipts_with_local_origin() {
    let tmp = TempDir::new().unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "local-node").unwrap();

    let receipts = oplog
        .append_batch_for_task(
            "primary-task",
            &[
                (
                    "upsert".into(),
                    serde_json::json!({"objectID": "doc-a", "body": {"_id": "doc-a"}}),
                ),
                ("delete".into(), serde_json::json!({"objectID": "doc-b"})),
            ],
        )
        .unwrap();

    assert_eq!(receipts.len(), 2);
    assert_eq!(receipts[0].seq, 1);
    assert_eq!(receipts[0].object_id.as_deref(), Some("doc-a"));
    assert_eq!(receipts[0].node_id, "local-node");
    assert!(!receipts[0].is_tombstone);
    assert_eq!(receipts[0].origin_seq, Some(1));
    assert_eq!(
        receipts[0].effect_digest,
        Some(upsert_effect_digest(
            &crate::types::Document::from_json(&serde_json::json!({"_id": "doc-a"})).unwrap()
        ))
    );
    assert_eq!(receipts[1].seq, 2);
    assert_eq!(receipts[1].object_id.as_deref(), Some("doc-b"));
    assert_eq!(receipts[1].node_id, "local-node");
    assert!(receipts[1].is_tombstone);
    assert_eq!(receipts[1].origin_seq, Some(2));
    assert_eq!(
        receipts[1].effect_digest,
        Some(delete_effect_digest("doc-b"))
    );
    assert_eq!(
        receipts[0].timestamp_ms, receipts[1].timestamp_ms,
        "primary batch receipts must share one local timestamp"
    );
}

#[test]
fn replication_origin_proof_replicated_receipts_preserve_source_seq_and_digest() {
    let tmp = TempDir::new().unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "local-node").unwrap();

    let receipts = oplog
        .append_operations_for_task(
            "replicated-task",
            vec![
                OpLogOperation::replicated(
                    "upsert",
                    serde_json::json!({"body": {"_id": "doc-a"}}),
                    OpLogOrigin::new(5000, "remote-a").with_origin_seq(50),
                ),
                OpLogOperation::replicated(
                    "delete",
                    serde_json::json!({"objectID": "doc-b"}),
                    OpLogOrigin::new(1000, "remote-b").with_origin_seq(10),
                ),
            ],
        )
        .unwrap();

    assert_eq!(
        receipts,
        vec![
            OpLogReceipt {
                seq: 1,
                object_id: Some("doc-a".to_string()),
                timestamp_ms: 5000,
                node_id: "remote-a".to_string(),
                is_tombstone: false,
                origin_seq: Some(50),
                effect_digest: Some(upsert_effect_digest(
                    &crate::types::Document::from_json(&serde_json::json!({"_id": "doc-a"}),)
                        .unwrap(),
                )),
            },
            OpLogReceipt {
                seq: 2,
                object_id: Some("doc-b".to_string()),
                timestamp_ms: 1000,
                node_id: "remote-b".to_string(),
                is_tombstone: true,
                origin_seq: Some(10),
                effect_digest: Some(delete_effect_digest("doc-b")),
            },
        ]
    );

    let entries = oplog.read_since(0).unwrap();
    assert_eq!(entries[0].timestamp_ms, 5000);
    assert_eq!(entries[0].node_id, "remote-a");
    assert_eq!(entries[1].timestamp_ms, 1000);
    assert_eq!(entries[1].node_id, "remote-b");
}

#[test]
fn append_operations_preserves_mixed_order_and_missing_object_receipts() {
    let tmp = TempDir::new().unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "local-node").unwrap();

    let receipts = oplog
        .append_operations_for_task(
            "mixed-task",
            vec![
                OpLogOperation::local("delete", serde_json::json!({"objectID": "doc-a"})),
                OpLogOperation::local("upsert", serde_json::json!({"body": {"_id": "doc-b"}})),
                OpLogOperation::local("config", serde_json::json!({"settings": true})),
            ],
        )
        .unwrap();

    assert_eq!(
        receipts
            .iter()
            .map(|receipt| receipt.seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(receipts[0].object_id.as_deref(), Some("doc-a"));
    assert!(receipts[0].is_tombstone);
    assert_eq!(receipts[1].object_id.as_deref(), Some("doc-b"));
    assert!(!receipts[1].is_tombstone);
    assert_eq!(receipts[2].object_id, None);
    assert!(!receipts[2].is_tombstone);
}

#[test]
fn olr_concurrent_batch_and_single_append_allocate_unique_sequences() {
    let tmp = TempDir::new().unwrap();
    let oplog = std::sync::Arc::new(OpLog::open(tmp.path(), "t1", "local-node").unwrap());
    let batch_snapshot_entered = std::sync::Arc::new(std::sync::Barrier::new(2));
    let release_batch = std::sync::Arc::new(std::sync::Barrier::new(2));
    let intercepted = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let _hook = set_after_batch_sequence_snapshot_hook_for_test({
        let batch_snapshot_entered = std::sync::Arc::clone(&batch_snapshot_entered);
        let release_batch = std::sync::Arc::clone(&release_batch);
        let intercepted = std::sync::Arc::clone(&intercepted);
        move || {
            if !intercepted.swap(true, std::sync::atomic::Ordering::SeqCst) {
                batch_snapshot_entered.wait();
                release_batch.wait();
            }
        }
    });

    let batch_oplog = std::sync::Arc::clone(&oplog);
    let batch = std::thread::spawn(move || {
        batch_oplog.append_operations_for_task(
            "concurrent-batch",
            vec![OpLogOperation::local(
                "upsert",
                serde_json::json!({
                    "objectID": "batch-doc",
                    "body": {"_id": "batch-doc", "title": "batch"}
                }),
            )],
        )
    });

    batch_snapshot_entered.wait();
    let single_seq = oplog
        .append(
            "upsert",
            serde_json::json!({
                "objectID": "single-doc",
                "body": {"_id": "single-doc", "title": "single"}
            }),
        )
        .unwrap();
    release_batch.wait();
    let batch_receipts = batch.join().unwrap().unwrap();

    assert_eq!(single_seq, 1);
    assert_eq!(batch_receipts.len(), 1);
    assert_eq!(batch_receipts[0].seq, 2);
    assert_eq!(batch_receipts[0].origin_seq, Some(2));
    assert_eq!(oplog.current_seq(), 2);

    let durable_entries = oplog.read_since(0).unwrap();
    assert_eq!(
        durable_entries
            .iter()
            .map(|entry| entry.seq)
            .collect::<Vec<_>>(),
        vec![1, 2],
        "concurrent single and batch appends must allocate one unique durable sequence each"
    );
    assert_eq!(
        replication_origin_seq(&durable_entries[1].payload).unwrap(),
        Some(2),
        "the batch receipt and embedded local origin must bind the same allocated sequence"
    );
}

#[test]
fn committed_task_ids_exclude_logged_but_uncommitted_entries() {
    let tmp = TempDir::new().unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
    oplog
        .append_batch_for_task(
            "committed_task",
            &[(
                "upsert".into(),
                serde_json::json!({"objectID": "a", "body": {"objectID": "a"}}),
            )],
        )
        .unwrap();
    oplog
        .append_batch_for_task(
            "logged_uncommitted_task",
            &[(
                "upsert".into(),
                serde_json::json!({"objectID": "b", "body": {"objectID": "b"}}),
            )],
        )
        .unwrap();

    assert_eq!(
        oplog.committed_task_ids(1).unwrap(),
        BTreeSet::from(["committed_task".to_string()]),
        "admission reconciliation must not treat pre-commit oplog append as durable completion"
    );
}

#[cfg(unix)]
#[test]
fn task_tagged_append_rejects_unsyncable_segment_before_advancing_seq() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let segment_path = tmp.path().join("segment_0001.jsonl");
    symlink("/dev/null", &segment_path).unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

    let result = oplog.append_batch_for_task(
        "crash_boundary_task",
        &[(
            "upsert".into(),
            serde_json::json!({"objectID": "a", "body": {"objectID": "a"}}),
        )],
    );

    assert!(
        result.is_err(),
        "task-tagged append must fail when the segment cannot be synced"
    );
    assert_eq!(
        oplog.current_seq(),
        0,
        "task-tagged append must not publish a sequence before durable sync succeeds"
    );
}

#[cfg(unix)]
#[test]
fn task_scoped_append_sync_error_returns_no_receipts_before_advancing_seq() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().unwrap();
    let segment_path = tmp.path().join("segment_0001.jsonl");
    symlink("/dev/null", &segment_path).unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

    let result = oplog.append_operations_for_task(
        "crash-boundary-task",
        vec![OpLogOperation::local(
            "upsert",
            serde_json::json!({"objectID": "doc-a"}),
        )],
    );

    assert!(
        result.is_err(),
        "task-scoped append must surface sync failure"
    );
    assert_eq!(
        oplog.current_seq(),
        0,
        "task-scoped append must not publish a sequence before durable sync succeeds"
    );
}
