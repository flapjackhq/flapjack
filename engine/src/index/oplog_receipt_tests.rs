use super::*;
use tempfile::TempDir;

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
    assert_eq!(receipts[1].seq, 2);
    assert_eq!(receipts[1].object_id.as_deref(), Some("doc-b"));
    assert_eq!(receipts[1].node_id, "local-node");
    assert!(receipts[1].is_tombstone);
    assert_eq!(
        receipts[0].timestamp_ms, receipts[1].timestamp_ms,
        "primary batch receipts must share one local timestamp"
    );
}

#[test]
fn append_operations_returns_replicated_receipts_with_preserved_origin() {
    let tmp = TempDir::new().unwrap();
    let oplog = OpLog::open(tmp.path(), "t1", "local-node").unwrap();

    let receipts = oplog
        .append_operations_for_task(
            "replicated-task",
            vec![
                OpLogOperation::replicated(
                    "upsert",
                    serde_json::json!({"body": {"_id": "doc-a"}}),
                    OpLogOrigin::new(5000, "remote-a"),
                ),
                OpLogOperation::replicated(
                    "delete",
                    serde_json::json!({"objectID": "doc-b"}),
                    OpLogOrigin::new(1000, "remote-b"),
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
            },
            OpLogReceipt {
                seq: 2,
                object_id: Some("doc-b".to_string()),
                timestamp_ms: 1000,
                node_id: "remote-b".to_string(),
                is_tombstone: true,
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
