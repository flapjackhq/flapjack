    use super::*;
    use std::io::{Seek, SeekFrom};
    use tempfile::TempDir;

    fn append_test_row(
        oplog: &OpLog,
        object_id: &str,
        durability: AppendDurability,
    ) -> crate::error::Result<u64> {
        oplog.append(
            "settings",
            serde_json::json!({"objectID": object_id}),
            durability,
        )
    }

    fn pause_committed_snapshot_after_bound(
        oplog: &OpLog,
    ) -> (
        CommittedSnapshotHookGuard,
        std::sync::mpsc::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
    ) {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let release_rx = std::sync::Arc::new(std::sync::Mutex::new(release_rx));
        let guard = oplog.set_committed_snapshot_after_bound_hook_for_test(std::sync::Arc::new({
            let release_rx = std::sync::Arc::clone(&release_rx);
            move || {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(2))
                    .expect("snapshot test must always release the segment lock");
            }
        }));
        (guard, entered_rx, release_tx)
    }

    #[test]
    fn synced_taskless_failure_restores_durable_prefix_and_watermark() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "synced-failure", "node1").unwrap();
        assert_eq!(
            append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap(),
            1
        );
        write_committed_seq(&tenant_path, 1).unwrap();
        oplog.advance_current_seq_floor(7);

        let fault = crate::index::write_queue::fail_next_finalization_for_test(
            "synced-failure",
            crate::index::write_queue::FinalizationFaultPoint::AfterTasklessOplogRowSyncBeforeCompletion,
        );
        let error = append_test_row(&oplog, "rejected", AppendDurability::Synced).unwrap_err();
        assert!(fault.was_triggered(), "intended row-sync fault did not fire: {error}");
        assert_eq!(oplog.current_seq(), 7);
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
        assert_eq!(
            oplog.read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.payload["objectID"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["retained"]
        );
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "synced-failure", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 7);
        assert_eq!(
            append_test_row(&reopened, "replacement", AppendDurability::Synced).unwrap(),
            8,
            "the rejected sequence must be reused exactly once"
        );
    }

    #[test]
    fn failed_taskless_rollback_is_sticky_and_blocks_watermark_publication() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "rollback-failure", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();

        let fault = crate::index::write_queue::fail_finalization_sequence_for_test(
            "rollback-failure",
            &[
                crate::index::write_queue::FinalizationFaultPoint::AfterTasklessOplogRowSyncBeforeCompletion,
                crate::index::write_queue::FinalizationFaultPoint::DuringTasklessOplogRollback,
            ],
        );
        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered(), "both append and rollback faults must fire");
        assert!(append_test_row(&oplog, "blocked", AppendDurability::Synced).is_err());
        assert!(write_committed_seq(&tenant_path, 2).is_err());
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "rollback-failure", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 1);
        assert_eq!(reopened.read_since(0).unwrap().len(), 1);
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
    }

    #[test]
    fn malformed_taskless_recovery_evidence_fails_closed_on_reopen_and_publication() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        {
            let oplog = OpLog::open(&oplog_dir, "t1", "node1").unwrap();
            append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
            write_committed_seq(&tenant_path, 1).unwrap();
        }
        fs::write(oplog_dir.join(TASKLESS_APPEND_INTENT_FILE), b"not-json").unwrap();

        assert!(OpLog::open(&oplog_dir, "t1", "node1").is_err());
        assert!(write_committed_seq(&tenant_path, 2).is_err());
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
    }

    #[test]
    fn matching_completion_with_invalid_boundary_fails_closed() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        {
            let oplog = OpLog::open(&oplog_dir, "invalid-completed-boundary", "node1").unwrap();
            append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
            write_committed_seq(&tenant_path, 1).unwrap();
        }
        write_recovery_json(
            &oplog_dir.join(TASKLESS_APPEND_INTENT_FILE),
            &TasklessAppendIntent {
                version: 1,
                generation: 7,
                prior_seq: 999,
                segment_id: 0,
                segment_size: 0,
            },
        )
        .unwrap();
        write_recovery_json(
            &oplog_dir.join(TASKLESS_APPEND_COMPLETION_FILE),
            &TasklessAppendCompletion {
                version: 1,
                generation: 7,
                resulting_seq: None,
            },
        )
        .unwrap();

        assert!(OpLog::open(&oplog_dir, "invalid-completed-boundary", "node1").is_err());
        assert!(write_committed_seq(&tenant_path, 2).is_err());
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
    }

    #[test]
    fn completed_taskless_append_remains_healthy_after_retention_removes_its_segment() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "completed-retention", "node1").unwrap();

        assert_eq!(append_test_row(&oplog, "prefix", AppendDurability::Buffered).unwrap(), 1);
        write_committed_seq(&tenant_path, 1).unwrap();
        assert_eq!(
            append_test_row(&oplog, "completed", AppendDurability::Synced).unwrap(),
            2
        );
        write_committed_seq(&tenant_path, 2).unwrap();
        oplog.rotate_segment_for_test().unwrap();
        assert!(oplog_dir.join(TASKLESS_APPEND_INTENT_FILE).is_file());
        assert!(oplog_dir.join(TASKLESS_APPEND_COMPLETION_FILE).is_file());
        let intent: TasklessAppendIntent = serde_json::from_slice(
            &fs::read(oplog_dir.join(TASKLESS_APPEND_INTENT_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(intent.segment_id, 1);
        assert!(intent.segment_size > 0);
        assert_eq!(oplog.truncate_before(3).unwrap(), 1);
        assert!(!oplog_dir.join("segment_0001.jsonl").exists());

        write_committed_seq(&tenant_path, 2).unwrap();
        drop(oplog);
        let oplog = OpLog::open(&oplog_dir, "completed-retention", "node1").unwrap();
        assert_eq!(oplog.current_seq(), 2);
        assert_eq!(
            append_test_row(&oplog, "after-retention", AppendDurability::Synced).unwrap(),
            3
        );
        write_committed_seq(&tenant_path, 3).unwrap();
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "completed-retention", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 3);
        assert_eq!(
            reopened
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.payload["objectID"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["after-retention"]
        );
    }

    #[test]
    fn retired_completion_does_not_authorize_an_uncommitted_sequence_floor() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "uncommitted-retention", "node1").unwrap();
        append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        oplog.rotate_segment_for_test().unwrap();
        drop(oplog);
        remove_segment_file(&oplog_dir.join("segment_0001.jsonl")).unwrap();

        write_recovery_json(
            &oplog_dir.join(TASKLESS_APPEND_INTENT_FILE),
            &serde_json::json!({
                "version": 1,
                "generation": 7,
                "prior_seq": 999,
                "segment_id": 1,
                "segment_size": 1
            }),
        )
        .unwrap();
        write_recovery_json(
            &oplog_dir.join(TASKLESS_APPEND_COMPLETION_FILE),
            &serde_json::json!({
                "version": 2,
                "generation": 7,
                "resulting_seq": 1000
            }),
        )
        .unwrap();

        assert!(OpLog::open(&oplog_dir, "uncommitted-retention", "node1").is_err());
        assert!(write_committed_seq(&tenant_path, 1000).is_err());
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
    }

    #[test]
    fn inconsistent_recovery_boundary_is_rejected_before_suffix_mutation() {
        let tmp = TempDir::new().unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        {
            let oplog = OpLog::open(tmp.path(), "inconsistent-boundary", "node1").unwrap();
            append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
            append_test_row(&oplog, "valid-suffix", AppendDurability::Synced).unwrap();
        }
        let original_segment = fs::read(&segment_path).unwrap();
        write_recovery_json(
            &tmp.path().join(TASKLESS_APPEND_INTENT_FILE),
            &TasklessAppendIntent {
                version: 1,
                generation: 3,
                prior_seq: 1,
                segment_id: 1,
                segment_size: original_segment.len() as u64 + 1,
            },
        )
        .unwrap();

        assert!(OpLog::open(tmp.path(), "inconsistent-boundary", "node1").is_err());
        assert_eq!(
            fs::read(&segment_path).unwrap(),
            original_segment,
            "invalid evidence must not authorize destructive recovery"
        );
    }

    #[test]
    fn recovery_boundary_with_newer_sequence_is_rejected_before_suffix_mutation() {
        let tmp = TempDir::new().unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        {
            let oplog = OpLog::open(tmp.path(), "inconsistent-sequence", "node1").unwrap();
            append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
            append_test_row(&oplog, "valid-suffix", AppendDurability::Synced).unwrap();
        }
        let original_segment = fs::read(&segment_path).unwrap();
        write_recovery_json(
            &tmp.path().join(TASKLESS_APPEND_INTENT_FILE),
            &TasklessAppendIntent {
                version: 1,
                generation: 3,
                prior_seq: 1,
                segment_id: 1,
                segment_size: original_segment.len() as u64,
            },
        )
        .unwrap();

        assert!(OpLog::open(tmp.path(), "inconsistent-sequence", "node1").is_err());
        assert_eq!(
            fs::read(&segment_path).unwrap(),
            original_segment,
            "a boundary containing a newer sequence must not authorize truncation"
        );
    }

    #[test]
    fn taskless_intent_failure_writes_no_row() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "intent-failure", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();

        let fault = crate::index::write_queue::fail_next_finalization_for_test(
            "intent-failure",
            crate::index::write_queue::FinalizationFaultPoint::BeforeTasklessOplogIntentPersistence,
        );
        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered());
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
    }

    #[test]
    fn reopen_after_empty_rotation_preserves_sequence() {
        let tmp = TempDir::new().unwrap();
        {
            let oplog = OpLog::open(tmp.path(), "empty-rotation", "node1").unwrap();
            assert_eq!(
                append_test_row(&oplog, "retained", AppendDurability::Buffered).unwrap(),
                1
            );
            oplog.rotate_segment_for_test().unwrap();
        }

        let reopened = OpLog::open(tmp.path(), "empty-rotation", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 1);
        assert_eq!(
            append_test_row(&reopened, "next", AppendDurability::Buffered).unwrap(),
            2
        );
    }

    #[test]
    fn taskless_pre_sync_and_partial_write_failures_restore_retained_prefix() {
        let cases = [
            (
                "taskless-pre-sync",
                crate::index::write_queue::FinalizationFaultPoint::BeforeTasklessOplogRowSync,
            ),
            (
                "taskless-partial-write",
                crate::index::write_queue::FinalizationFaultPoint::DuringTasklessOplogAppendAfterPartialWrite,
            ),
        ];
        for (tenant_id, fault_point) in cases {
            let tmp = TempDir::new().unwrap();
            let tenant_path = tmp.path().join("tenant");
            let oplog_dir = tenant_path.join(OPLOG_DIR);
            let oplog = OpLog::open(&oplog_dir, tenant_id, "node1").unwrap();
            append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
            write_committed_seq(&tenant_path, 1).unwrap();
            let fault = crate::index::write_queue::fail_next_finalization_for_test(
                tenant_id,
                fault_point,
            );

            assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
            assert!(fault.was_triggered(), "fault {fault_point:?} did not fire");
            assert_eq!(oplog.current_seq(), 1);
            assert_eq!(oplog.read_since(0).unwrap().len(), 1);
            assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
            drop(oplog);
            let reopened = OpLog::open(&oplog_dir, tenant_id, "node1").unwrap();
            assert_eq!(reopened.current_seq(), 1);
            assert_eq!(reopened.read_since(0).unwrap().len(), 1);
            assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
        }
    }

    #[test]
    fn oplog_open_retries_parent_sync_after_directory_creation_failure() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        fs::create_dir(&tenant_path).unwrap();
        let oplog_dir = tenant_path.join(OPLOG_DIR);

        let first_fault = crate::index::utils::fail_next_directory_sync_for_test(
            &tenant_path,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
        );
        assert!(OpLog::open(&oplog_dir, "directory-retry", "node1").is_err());
        assert!(first_fault.was_triggered());

        let retry_fault = crate::index::utils::fail_next_directory_sync_for_test(
            &tenant_path,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
        );
        assert!(
            OpLog::open(&oplog_dir, "directory-retry", "node1").is_err(),
            "retry must repeat the parent sync even though directory creation already succeeded"
        );
        assert!(retry_fault.was_triggered());
        OpLog::open(&oplog_dir, "directory-retry", "node1").unwrap();
    }

    #[test]
    fn synced_taskless_append_retries_directory_sync_before_writing() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "append-directory-sync", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        let fault = crate::index::utils::fail_next_directory_sync_for_test(
            &oplog_dir,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
        );

        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered());
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
        assert_eq!(
            append_test_row(&oplog, "retry", AppendDurability::Synced).unwrap(),
            2
        );
    }

    #[test]
    fn rotation_directory_sync_failure_rolls_back_and_allows_unique_retry() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "rotation-directory-sync", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        oplog.set_segment_max_bytes_for_test(1);
        let fault = crate::index::utils::fail_directory_sync_after_for_test(
            &oplog_dir,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
            2,
        );

        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered());
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
        assert_eq!(
            append_test_row(&oplog, "retry", AppendDurability::Synced).unwrap(),
            2
        );
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "rotation-directory-sync", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 2);
        assert_eq!(reopened.read_since(0).unwrap().len(), 2);
    }

    #[test]
    fn synced_append_into_rotated_segment_retries_directory_sync_before_writing() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "rotated-segment-append", "node1").unwrap();
        oplog.set_segment_max_bytes_for_test(1);
        assert_eq!(
            append_test_row(&oplog, "buffered-prefix", AppendDurability::Buffered).unwrap(),
            1
        );
        oplog.set_segment_max_bytes_for_test(u64::MAX);
        assert!(tmp.path().join("segment_0002.jsonl").is_file());

        let fault = crate::index::utils::fail_next_directory_sync_for_test(
            tmp.path(),
            crate::index::utils::DirectorySyncFaultPoint::Sync,
        );
        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered());
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
        assert_eq!(
            append_test_row(&oplog, "accepted", AppendDurability::Synced).unwrap(),
            2
        );
        assert_eq!(
            OpLog::open(tmp.path(), "rotated-segment-append", "node1")
                .unwrap()
                .current_seq(),
            2
        );
    }

    #[test]
    fn recovery_recreates_missing_active_segment_before_retry_append() {
        let tmp = TempDir::new().unwrap();
        drop(OpLog::open(tmp.path(), "recreated-segment", "node1").unwrap());
        remove_segment_file(&tmp.path().join("segment_0001.jsonl")).unwrap();
        write_recovery_json(
            &tmp.path().join(TASKLESS_APPEND_INTENT_FILE),
            &TasklessAppendIntent {
                version: 1,
                generation: 1,
                prior_seq: 0,
                segment_id: 1,
                segment_size: 0,
            },
        )
        .unwrap();

        let fault = crate::index::utils::fail_next_directory_sync_for_test(
            tmp.path(),
            crate::index::utils::DirectorySyncFaultPoint::Sync,
        );
        assert!(OpLog::open(tmp.path(), "recreated-segment", "node1").is_err());
        assert!(fault.was_triggered());

        let oplog = OpLog::open(tmp.path(), "recreated-segment", "node1").unwrap();
        assert_eq!(
            append_test_row(&oplog, "accepted", AppendDurability::Synced).unwrap(),
            1
        );
        drop(oplog);
        assert_eq!(
            OpLog::open(tmp.path(), "recreated-segment", "node1")
                .unwrap()
                .current_seq(),
            1
        );
    }

    #[test]
    fn non_taskless_rotation_failure_preserves_assigned_sequence() {
        for (tenant_id, task_id) in [
            ("task-rotation-failure", Some("task-1")),
            ("buffered-rotation-failure", None),
        ] {
            let tmp = TempDir::new().unwrap();
            let oplog = OpLog::open(tmp.path(), tenant_id, "node1").unwrap();
            oplog.set_segment_max_bytes_for_test(1);
            let fault = crate::index::utils::fail_next_directory_sync_for_test(
                tmp.path(),
                crate::index::utils::DirectorySyncFaultPoint::Sync,
            );

            let result = match task_id {
                Some(task_id) => oplog.append_batch_for_task(
                    task_id,
                    &[("upsert".into(), serde_json::json!({"objectID": "written"}))],
                ),
                None => oplog
                    .append(
                        "settings",
                        serde_json::json!({"objectID": "written"}),
                        AppendDurability::Buffered,
                    )
                    .map(|_| Vec::new()),
            };

            assert!(result.is_err());
            assert!(fault.was_triggered());
            assert_eq!(
                oplog.current_seq(),
                1,
                "a rotation error must not make the next in-process append reuse a written sequence"
            );
            assert_eq!(oplog.read_since(0).unwrap()[0].seq, 1);
        }
    }

    #[test]
    fn intent_publication_sync_failure_writes_no_row_and_recovers_on_reopen() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "intent-directory-sync", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        let fault = crate::index::utils::fail_directory_sync_after_for_test(
            &oplog_dir,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
            1,
        );

        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered());
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
        assert!(append_test_row(&oplog, "blocked", AppendDurability::Synced).is_err());
        assert!(
            append_test_row(&oplog, "buffered-blocked", AppendDurability::Buffered).is_err(),
            "unresolved recovery must block buffered appends too"
        );
        assert!(write_committed_seq(&tenant_path, 2).is_err());
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "intent-directory-sync", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 1);
        assert_eq!(reopened.read_since(0).unwrap().len(), 1);
        assert_eq!(
            append_test_row(&reopened, "retry", AppendDurability::Synced).unwrap(),
            2
        );
    }

    #[test]
    fn completion_directory_sync_failure_rolls_back_despite_visible_terminal_bytes() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "completion-directory-sync", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        let fault = crate::index::utils::fail_directory_sync_after_for_test(
            &oplog_dir,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
            2,
        );

        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(fault.was_triggered());
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "completion-directory-sync", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 1);
        assert_eq!(reopened.read_since(0).unwrap().len(), 1);
        assert_eq!(
            append_test_row(&reopened, "retry", AppendDurability::Synced).unwrap(),
            2
        );
    }

    #[test]
    fn completion_directory_sync_and_rollback_failure_blocks_watermark_publication() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "completion-rollback-failure", "node1").unwrap();
        append_test_row(&oplog, "retained", AppendDurability::Synced).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        let directory_fault = crate::index::utils::fail_directory_sync_after_for_test(
            &oplog_dir,
            crate::index::utils::DirectorySyncFaultPoint::Sync,
            2,
        );
        let rollback_fault = crate::index::write_queue::fail_next_finalization_for_test(
            "completion-rollback-failure",
            crate::index::write_queue::FinalizationFaultPoint::DuringTasklessOplogRollback,
        );

        assert!(append_test_row(&oplog, "rejected", AppendDurability::Synced).is_err());
        assert!(directory_fault.was_triggered());
        assert!(rollback_fault.was_triggered());
        assert!(append_test_row(&oplog, "blocked", AppendDurability::Synced).is_err());
        assert!(
            write_committed_seq(&tenant_path, 2).is_err(),
            "a matching but unconfirmed completion must not publish a failed append"
        );
        assert_eq!(read_checked_committed_seq(&tenant_path).unwrap(), Some(1));
        drop(oplog);

        let reopened = OpLog::open(&oplog_dir, "completion-rollback-failure", "node1").unwrap();
        assert_eq!(reopened.current_seq(), 1);
        assert_eq!(reopened.read_since(0).unwrap().len(), 1);
        assert_eq!(
            append_test_row(&reopened, "retry", AppendDurability::Synced).unwrap(),
            2
        );
    }

    /// Verify that appending entries increments the sequence counter and that `read_since` correctly filters by sequence number.
    #[test]
    fn test_append_and_read() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        assert_eq!(oplog.current_seq(), 0);
        let s1 = oplog
            .append("upsert", serde_json::json!({"objectID": "1"}), AppendDurability::Buffered)
            .unwrap();
        assert_eq!(s1, 1);
        let s2 = oplog
            .append("delete", serde_json::json!({"objectID": "2"}), AppendDurability::Buffered)
            .unwrap();
        assert_eq!(s2, 2);

        let all = oplog.read_since(0).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 1);
        assert_eq!(all[1].seq, 2);

        let since1 = oplog.read_since(1).unwrap();
        assert_eq!(since1.len(), 1);
        assert_eq!(since1[0].seq, 2);
    }

    #[test]
    fn committed_snapshot_clamps_rows_and_cursor_to_durable_watermark() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = OpLog::open(&tenant_path.join(OPLOG_DIR), "tenant", "node1").unwrap();
        append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        append_test_row(&oplog, "pending", AppendDurability::Buffered).unwrap();

        let snapshot = oplog.read_committed_since(0).unwrap();

        assert_eq!(snapshot.current_seq, 1);
        assert_eq!(snapshot.oldest_retained_seq, Some(1));
        assert_eq!(snapshot.ops.len(), 1);
        assert_eq!(snapshot.ops[0].payload["objectID"], "committed");
    }

    #[test]
    fn committed_snapshot_clamps_watermark_above_physical_tail() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = OpLog::open(&tenant_path.join(OPLOG_DIR), "tenant", "node1").unwrap();
        append_test_row(&oplog, "only-row", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 99).unwrap();

        let snapshot = oplog.read_committed_since(1).unwrap();

        assert_eq!(snapshot.current_seq, 1);
        assert_eq!(snapshot.oldest_retained_seq, Some(1));
        assert!(snapshot.ops.is_empty());
    }

    #[test]
    fn committed_snapshot_preserves_gap_boundary_after_full_retention() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = OpLog::open(&tenant_path.join(OPLOG_DIR), "tenant", "node1").unwrap();
        for sequence in 1..=3 {
            assert_eq!(
                append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap(),
                sequence
            );
        }
        write_committed_seq(&tenant_path, 3).unwrap();
        oplog.rotate_segment_for_test().unwrap();
        assert_eq!(oplog.truncate_before(4).unwrap(), 1);

        let snapshot = oplog.read_committed_since(0).unwrap();

        assert_eq!(snapshot.current_seq, 3);
        assert_eq!(snapshot.oldest_retained_seq, Some(4));
        assert!(snapshot.ops.is_empty());
    }

    #[test]
    fn committed_snapshot_rejects_nonempty_malformed_retained_row() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "tenant", "node1").unwrap();
        append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        drop(oplog);
        std::fs::OpenOptions::new()
            .append(true)
            .open(oplog_dir.join("segment_0001.jsonl"))
            .unwrap()
            .write_all(b"malformed retained row\n")
            .unwrap();
        let oplog = OpLog::open(&oplog_dir, "tenant", "node1").unwrap();

        assert!(oplog.read_committed_since(0).is_err());
    }

    #[test]
    fn committed_snapshot_holds_retained_prefix_stable_during_truncation() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = std::sync::Arc::new(
            OpLog::open(&tenant_path.join(OPLOG_DIR), "snapshot-truncate", "node1").unwrap(),
        );
        append_test_row(&oplog, "oldest", AppendDurability::Buffered).unwrap();
        oplog.rotate_segment_for_test().unwrap();
        append_test_row(&oplog, "newest", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 2).unwrap();
        let (_hook, entered, release) = pause_committed_snapshot_after_bound(&oplog);

        let snapshot_oplog = std::sync::Arc::clone(&oplog);
        let snapshot_thread = std::thread::spawn(move || snapshot_oplog.read_committed_since(0));
        entered.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let truncate_oplog = std::sync::Arc::clone(&oplog);
        let (truncated_tx, truncated_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || truncated_tx.send(truncate_oplog.truncate_before(2)).unwrap());
        assert!(truncated_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());

        release.send(()).unwrap();
        let snapshot = snapshot_thread.join().unwrap().unwrap();
        assert_eq!(snapshot.current_seq, 2);
        assert_eq!(snapshot.oldest_retained_seq, Some(1));
        assert_eq!(snapshot.ops.iter().map(|entry| entry.seq).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(truncated_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap(), 1);
    }

    #[test]
    fn committed_snapshot_excludes_pending_task_while_retraction_waits() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = std::sync::Arc::new(
            OpLog::open(&tenant_path.join(OPLOG_DIR), "snapshot-retract", "node1").unwrap(),
        );
        append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        oplog
            .append_operations_for_task(
                "rejected-task",
                vec![OpLogOperation::local(
                    "upsert",
                    serde_json::json!({"objectID": "rejected"}),
                )],
            )
            .unwrap();
        let (_hook, entered, release) = pause_committed_snapshot_after_bound(&oplog);

        let snapshot_oplog = std::sync::Arc::clone(&oplog);
        let snapshot_thread = std::thread::spawn(move || snapshot_oplog.read_committed_since(0));
        entered.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let retract_oplog = std::sync::Arc::clone(&oplog);
        let (retracted_tx, retracted_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            retracted_tx
                .send(retract_oplog.retract_tasks_from(2, ["rejected-task"]))
                .unwrap()
        });
        assert!(retracted_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());

        release.send(()).unwrap();
        let snapshot = snapshot_thread.join().unwrap().unwrap();
        assert_eq!(snapshot.current_seq, 1);
        assert_eq!(snapshot.ops.iter().map(|entry| entry.seq).collect::<Vec<_>>(), vec![1]);
        assert_eq!(retracted_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap(), 1);
    }

    #[test]
    fn committed_snapshot_keeps_captured_bound_during_append_commit_and_rotation() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = std::sync::Arc::new(
            OpLog::open(&tenant_path.join(OPLOG_DIR), "snapshot-append", "node1").unwrap(),
        );
        append_test_row(&oplog, "captured", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        oplog.set_segment_max_bytes_for_test(1);
        let (hook, entered, release) = pause_committed_snapshot_after_bound(&oplog);

        let snapshot_oplog = std::sync::Arc::clone(&oplog);
        let snapshot_thread = std::thread::spawn(move || snapshot_oplog.read_committed_since(0));
        entered.recv_timeout(std::time::Duration::from_secs(2)).unwrap();
        let append_oplog = std::sync::Arc::clone(&oplog);
        let append_tenant_path = tenant_path.clone();
        let (appended_tx, appended_rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = append_test_row(&append_oplog, "later", AppendDurability::Buffered)
                .and_then(|seq| {
                    write_committed_seq(&append_tenant_path, seq)?;
                    Ok(seq)
                });
            appended_tx.send(result).unwrap();
        });
        assert!(appended_rx
            .recv_timeout(std::time::Duration::from_millis(50))
            .is_err());

        release.send(()).unwrap();
        let snapshot = snapshot_thread.join().unwrap().unwrap();
        assert_eq!(snapshot.current_seq, 1);
        assert_eq!(snapshot.ops.iter().map(|entry| entry.seq).collect::<Vec<_>>(), vec![1]);
        assert_eq!(appended_rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap().unwrap(), 2);
        drop(hook);
        let later = oplog.read_committed_since(0).unwrap();
        assert_eq!(later.current_seq, 2);
        assert_eq!(later.ops.iter().map(|entry| entry.seq).collect::<Vec<_>>(), vec![1, 2]);
    }

    /// Retracting from a sequence floor erases the suffix, resets the counter,
    /// and lets subsequent appends continue from the surviving tail.
    #[test]
    fn retract_from_removes_suffix_and_resets_seq() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        for i in 1..=5 {
            oplog
                .append("upsert", serde_json::json!({ "objectID": i.to_string() }), AppendDurability::Buffered)
                .unwrap();
        }
        assert_eq!(oplog.current_seq(), 5);

        let removed = oplog.retract_from(3).unwrap();
        assert_eq!(removed, 3, "seqs 3, 4, 5 must be retracted");
        assert_eq!(oplog.current_seq(), 2);
        assert_eq!(
            oplog
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "only the committed prefix survives retraction"
        );

        let next = oplog
            .append("upsert", serde_json::json!({ "objectID": "6" }), AppendDurability::Buffered)
            .unwrap();
        assert_eq!(next, 3, "appends resume from the surviving tail");
        assert_eq!(oplog.read_since(0).unwrap().len(), 3);
    }

    /// Retracting from a floor above the tail removes nothing and preserves the
    /// counter, so a compensation call on an empty batch is inert.
    #[test]
    fn retract_from_above_tail_is_noop() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "1" }), AppendDurability::Buffered)
            .unwrap();

        let removed = oplog.retract_from(99).unwrap();
        assert_eq!(removed, 0);
        assert_eq!(oplog.current_seq(), 1);
        assert_eq!(oplog.read_since(0).unwrap().len(), 1);
    }

    #[test]
    fn retract_tasks_from_preserves_unrelated_metadata_suffix() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "baseline" }), AppendDurability::Buffered)
            .unwrap();
        oplog
            .append_operations_for_task(
                "failed-task",
                vec![OpLogOperation::local(
                    "upsert",
                    serde_json::json!({ "objectID": "failed" }),
                )],
            )
            .unwrap();
        let metadata_seq = oplog
            .append(
                "settings",
                serde_json::json!({"searchableAttributes": ["title"]}),
                AppendDurability::Synced,
            )
            .unwrap();

        let removed = oplog.retract_tasks_from(2, ["failed-task"]).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(oplog.current_seq(), metadata_seq);
        let entries = oplog.read_since(0).unwrap();
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.op_type.as_str())
                .collect::<Vec<_>>(),
            vec!["upsert", "settings"],
            "task retraction must preserve unrelated metadata in the same suffix"
        );
        assert_eq!(
            entries.iter().map(|entry| entry.seq).collect::<Vec<_>>(),
            vec![1, metadata_seq],
            "the surviving metadata keeps its committed sequence number"
        );
    }

    #[test]
    fn retract_tasks_from_truncates_contiguous_task_suffix() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        oplog
            .append("upsert", serde_json::json!({ "objectID": "committed" }), AppendDurability::Buffered)
            .unwrap();
        let committed_bytes = fs::metadata(&segment_path).unwrap().len();
        oplog
            .append_operations_for_task(
                "failed-task",
                vec![
                    OpLogOperation::local("upsert", serde_json::json!({ "objectID": "failed-a" })),
                    OpLogOperation::local("upsert", serde_json::json!({ "objectID": "failed-b" })),
                ],
            )
            .unwrap();
        assert!(fs::metadata(&segment_path).unwrap().len() > committed_bytes);

        assert_eq!(oplog.retract_tasks_from(2, ["failed-task"]).unwrap(), 2);

        assert_eq!(
            fs::metadata(&segment_path).unwrap().len(),
            committed_bytes,
            "a contiguous rejected suffix must use shrink-only truncation instead of rewriting or neutralizing the segment"
        );
        assert_eq!(
            oplog
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn task_retraction_write_error_preserves_retained_rows() {
        struct FailAfterBytes {
            inner: std::io::Cursor<Vec<u8>>,
            remaining_writable_bytes: usize,
        }

        impl Write for FailAfterBytes {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.remaining_writable_bytes == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::StorageFull));
                }
                let write_size = bytes.len().min(self.remaining_writable_bytes);
                let written = self.inner.write(&bytes[..write_size])?;
                self.remaining_writable_bytes -= written;
                Ok(written)
            }

            fn flush(&mut self) -> std::io::Result<()> {
                self.inner.flush()
            }
        }

        impl Seek for FailAfterBytes {
            fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
                self.inner.seek(position)
            }
        }

        let committed = br#"{"seq":1,"op_type":"upsert"}\n"#;
        let rejected = br#"{"seq":2,"op_type":"upsert"}\n"#;
        let metadata = br#"{"seq":3,"op_type":"settings"}\n"#;
        let original = [
            committed.as_slice(),
            rejected.as_slice(),
            metadata.as_slice(),
        ]
        .concat();
        let rejected_start = committed.len() as u64;
        let rejected_end = rejected_start + rejected.len() as u64;
        let rejected_range = rejected_start..rejected_end;
        let mut writer = FailAfterBytes {
            inner: std::io::Cursor::new(original.clone()),
            remaining_writable_bytes: 3,
        };

        let error = neutralize_segment_ranges(
            &mut writer,
            Path::new("segment_0001.jsonl"),
            std::slice::from_ref(&rejected_range),
        )
        .unwrap_err();
        let after_failure = writer.inner.into_inner();

        assert!(error.to_string().contains("neutralize rejected oplog row"));
        assert_eq!(
            &after_failure[..committed.len()],
            committed,
            "a failed task-row overwrite must not alter the committed prefix"
        );
        assert_eq!(
            &after_failure[rejected_end as usize..],
            metadata,
            "a failed task-row overwrite must not alter a retained metadata suffix"
        );
    }

    #[test]
    fn retract_from_removes_receiptless_malformed_tail() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "committed" }), AppendDurability::Buffered)
            .unwrap();
        oplog.read_since(0).unwrap();

        let segment_path = tmp.path().join("segment_0001.jsonl");
        let mut segment_file = OpenOptions::new().append(true).open(&segment_path).unwrap();
        segment_file
            .write_all(br#"{"seq":2,"timestamp_ms":1,"node_id":"node1""#)
            .unwrap();
        segment_file.sync_all().unwrap();
        drop(segment_file);

        assert_eq!(oplog.retract_from(2).unwrap(), 0);
        let bytes = fs::read(&segment_path).unwrap();
        assert!(
            !bytes.ends_with(b"node_id\":\"node1\""),
            "retraction must remove durable bytes for the receipt-less next sequence"
        );
        assert_eq!(oplog.current_seq(), 1);

        let next = oplog
            .append("upsert", serde_json::json!({ "objectID": "replacement" }), AppendDurability::Buffered)
            .unwrap();
        assert_eq!(next, 2);
        assert_eq!(
            oplog
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1, 2],
            "the active writer must reopen on the rewritten segment"
        );
    }

    #[test]
    fn retract_from_removes_non_utf8_receiptless_tail() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "committed" }), AppendDurability::Buffered)
            .unwrap();
        oplog.read_since(0).unwrap();

        let segment_path = tmp.path().join("segment_0001.jsonl");
        let mut segment_file = OpenOptions::new().append(true).open(&segment_path).unwrap();
        segment_file
            .write_all(b"{\"seq\":2,\"payload\":\"")
            .unwrap();
        segment_file.write_all(&[0xff]).unwrap();
        segment_file.sync_all().unwrap();
        drop(segment_file);

        oplog.retract_from(2).unwrap();
        assert!(
            !fs::read(&segment_path).unwrap().contains(&0xff),
            "retraction must remove a receipt-less tail torn inside a UTF-8 code point"
        );
    }

    #[cfg(unix)]
    #[test]
    fn retract_from_does_not_require_a_new_directory_entry() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "committed" }), AppendDurability::Buffered)
            .unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "rejected" }), AppendDurability::Buffered)
            .unwrap();

        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o500)).unwrap();
        let result = oplog.retract_from(2);
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700)).unwrap();

        assert!(
            result.is_ok(),
            "suffix retraction must not allocate a replacement segment: {result:?}"
        );
        assert_eq!(
            oplog
                .read_since(0)
                .unwrap()
                .iter()
                .map(|entry| entry.seq)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[cfg(unix)]
    #[test]
    fn truncate_segment_file_identifies_the_failing_operation() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        fs::write(&segment_path, b"old\n").unwrap();
        fs::set_permissions(&segment_path, fs::Permissions::from_mode(0o400)).unwrap();

        let error = truncate_segment_file(&segment_path, 0).unwrap_err();
        fs::set_permissions(&segment_path, fs::Permissions::from_mode(0o600)).unwrap();

        assert!(
            error
                .to_string()
                .contains("open oplog segment for suffix truncation"),
            "retraction errors must identify the failing durable operation: {error}"
        );
    }

    #[test]
    fn storage_full_truncation_neutralizes_the_suffix_in_place() {
        let tmp = TempDir::new().unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        let retained = b"committed\n";
        let rejected = b"rejected-row\n";
        fs::write(
            &segment_path,
            [retained.as_slice(), rejected.as_slice()].concat(),
        )
        .unwrap();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&segment_path)
            .unwrap();

        complete_segment_suffix_retraction(
            &mut file,
            &segment_path,
            retained.len() as u64..(retained.len() + rejected.len()) as u64,
            Err(std::io::Error::from(std::io::ErrorKind::StorageFull)),
        )
        .unwrap();

        let bytes = fs::read(&segment_path).unwrap();
        assert_eq!(&bytes[..retained.len()], retained);
        assert_eq!(bytes.len(), retained.len() + rejected.len());
        assert_eq!(bytes.last(), Some(&b'\n'));
        assert!(
            bytes[retained.len()..bytes.len() - 1]
                .iter()
                .all(|byte| *byte == b' '),
            "the fallback must leave no parseable rejected payload bytes"
        );
    }

    #[cfg(unix)]
    #[test]
    fn segment_remove_requires_parent_directory_sync() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let segment_path = tmp.path().join("segment_0001.jsonl");
        fs::write(&segment_path, b"old\n").unwrap();
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o300)).unwrap();

        let result = remove_segment_file(&segment_path);
        fs::set_permissions(tmp.path(), fs::Permissions::from_mode(0o700)).unwrap();

        assert!(
            result.is_err(),
            "segment removal must fail when its directory entry cannot be synced"
        );
    }

    /// Verify that `append_batch` assigns contiguous sequence numbers and all entries are retrievable.
    #[test]
    fn test_batch_append() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        let ops: Vec<(String, serde_json::Value)> = vec![
            ("upsert".into(), serde_json::json!({"objectID": "a"})),
            ("upsert".into(), serde_json::json!({"objectID": "b"})),
            ("delete".into(), serde_json::json!({"objectID": "c"})),
        ];
        let last = oplog.append_batch(&ops).unwrap();
        assert_eq!(last, 3);
        assert_eq!(oplog.current_seq(), 3);

        let all = oplog.read_since(0).unwrap();
        assert_eq!(all.len(), 3);
    }

    /// TODO: Document write_committed_seq_replaces_existing_path_instead_of_following_it.
    #[cfg(unix)]
    #[test]
    fn write_committed_seq_replaces_existing_path_instead_of_following_it() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();
        let committed_path = tenant_path.join("committed_seq");
        symlink("/dev/null", &committed_path).unwrap();

        write_committed_seq(&tenant_path, 42).unwrap();

        let metadata = std::fs::symlink_metadata(&committed_path).unwrap();
        assert!(
            !metadata.file_type().is_symlink() && metadata.file_type().is_file(),
            "committed_seq must be atomically installed as a regular durable sidecar"
        );
        assert_eq!(read_committed_seq(&tenant_path), 42);
    }

    /// Verify that reopening an oplog on the same directory resumes from the previously written sequence number without gaps or duplicates.
    #[test]
    fn test_reopen_continues_seq() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            oplog.append("upsert", serde_json::json!({"x": 1}), AppendDurability::Buffered).unwrap();
            oplog.append("upsert", serde_json::json!({"x": 2}), AppendDurability::Buffered).unwrap();
        }

        let oplog2 = OpLog::open(&dir, "t1", "node1").unwrap();
        assert_eq!(oplog2.current_seq(), 2);
        let s3 = oplog2
            .append("delete", serde_json::json!({"x": 3}), AppendDurability::Buffered)
            .unwrap();
        assert_eq!(s3, 3);

        let all = oplog2.read_since(0).unwrap();
        assert_eq!(all.len(), 3);
    }

    /// Verify that `truncate_before` removes only segments whose entries are entirely below the threshold, leaving newer entries intact.
    #[test]
    fn test_truncate() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            for i in 0..5 {
                oplog.append("upsert", serde_json::json!({"i": i}), AppendDurability::Buffered).unwrap();
            }
            oplog
                .rotate_segment_locked(&mut oplog.segment.lock().unwrap())
                .unwrap();
            for i in 5..10 {
                oplog.append("upsert", serde_json::json!({"i": i}), AppendDurability::Buffered).unwrap();
            }
        }

        let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
        let removed = oplog.truncate_before(6).unwrap();
        assert_eq!(removed, 1);

        let remaining = oplog.read_since(0).unwrap();
        assert_eq!(remaining.len(), 5);
        assert_eq!(remaining[0].seq, 6);
    }

    #[test]
    fn test_oldest_seq_none_when_no_entries() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        assert_eq!(oplog.oldest_seq(), None);
    }
    /// TODO: Document test_oldest_seq_after_truncate_before.
    #[test]
    fn test_oldest_seq_after_truncate_before() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            for i in 0..5 {
                oplog.append("upsert", serde_json::json!({"i": i}), AppendDurability::Buffered).unwrap();
            }
            oplog
                .rotate_segment_locked(&mut oplog.segment.lock().unwrap())
                .unwrap();
            for i in 5..10 {
                oplog.append("upsert", serde_json::json!({"i": i}), AppendDurability::Buffered).unwrap();
            }
        }

        let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
        assert_eq!(oplog.oldest_seq(), Some(1));

        let removed = oplog.truncate_before(6).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(oplog.oldest_seq(), Some(6));
    }

    #[tokio::test]
    async fn retention_floor_restores_sequence_after_every_committed_segment_is_removed() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = std::sync::Arc::new(OpLog::open(&oplog_dir, "retained", "node1").unwrap());
        for sequence in 1..=3 {
            assert_eq!(
                append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap(),
                sequence
            );
        }
        write_committed_seq(&tenant_path, 3).unwrap();
        oplog.rotate_segment_for_test().unwrap();
        assert_eq!(oplog.truncate_before(4).unwrap(), 1);
        drop(oplog);

        let reopened = std::sync::Arc::new(
            OpLog::open(&oplog_dir, "retained", "node1").expect("retention floor must be valid"),
        );
        assert_eq!(reopened.current_seq(), 3);
        let mut permit = reopened.acquire_finalization().await.unwrap();
        assert_eq!(
            append_test_row(&reopened, "next", AppendDurability::Synced).unwrap(),
            4
        );
        assert_eq!(permit.advance_committed_seq(4).unwrap(), 4);
        permit.mark_safe();
    }

    #[tokio::test]
    async fn retention_floor_does_not_authorize_a_watermark_beyond_removed_rows() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = OpLog::open(&oplog_dir, "retained-gap", "node1").unwrap();
        append_test_row(&oplog, "committed", AppendDurability::Buffered).unwrap();
        write_committed_seq(&tenant_path, 1).unwrap();
        oplog.rotate_segment_for_test().unwrap();
        assert_eq!(oplog.truncate_before(2).unwrap(), 1);
        write_committed_seq(&tenant_path, 2).unwrap();
        drop(oplog);

        let reopened = std::sync::Arc::new(
            OpLog::open(&oplog_dir, "retained-gap", "node1").expect("retention floor is valid"),
        );
        assert_eq!(reopened.current_seq(), 1);
        assert!(reopened.acquire_finalization().await.is_err());
        let mut recovery = reopened.acquire_recovery().unwrap();
        assert!(recovery.mark_recovered().is_err());
    }

    #[test]
    fn test_read_write_committed_seq_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();

        assert_eq!(read_committed_seq(&tenant_path), 0);
        write_committed_seq(&tenant_path, 42).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 42);
    }

    #[test]
    fn test_oldest_seq_active_segment_only() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        oplog.append("upsert", serde_json::json!({"a": 1}), AppendDurability::Buffered).unwrap();
        oplog.append("upsert", serde_json::json!({"a": 2}), AppendDurability::Buffered).unwrap();
        oplog.append("upsert", serde_json::json!({"a": 3}), AppendDurability::Buffered).unwrap();

        // Without any segment rotation, oldest_seq should still read
        // the first entry from the flushed active segment.
        assert_eq!(oplog.oldest_seq(), Some(1));
    }

    #[test]
    fn test_read_committed_seq_malformed_returns_zero() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();

        // Write non-numeric content to the sidecar file.
        std::fs::write(tenant_path.join("committed_seq"), "not-a-number").unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 0);

        // Write empty content.
        std::fs::write(tenant_path.join("committed_seq"), "").unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 0);
    }

    #[test]
    fn test_read_committed_seq_missing_file_returns_zero() {
        let tmp = TempDir::new().unwrap();
        // Tenant path exists as a directory but has no committed_seq file.
        let tenant_path = tmp.path().join("tenant_no_file");
        std::fs::create_dir_all(&tenant_path).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 0);

        // Tenant path does not exist at all.
        let missing_path = tmp.path().join("nonexistent_tenant");
        assert_eq!(read_committed_seq(&missing_path), 0);
    }

    #[test]
    fn test_write_committed_seq_overwrites_previous() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();

        write_committed_seq(&tenant_path, 42).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 42);

        write_committed_seq(&tenant_path, 100).unwrap();
        assert_eq!(read_committed_seq(&tenant_path), 100);
    }

    #[tokio::test]
    async fn finalization_permit_advances_committed_seq_monotonically() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = std::sync::Arc::new(
            OpLog::open(&tenant_path.join(OPLOG_DIR), "monotonic", "node1").unwrap(),
        );
        assert_eq!(oplog.committed_seq().unwrap(), None);

        let mut initial = oplog.acquire_finalization().await.unwrap();
        assert!(initial.advance_committed_seq(1).is_err());
        for sequence in 1..=5 {
            assert_eq!(
                append_test_row(&oplog, "committed", AppendDurability::Synced).unwrap(),
                sequence
            );
        }
        initial.advance_committed_seq(5).unwrap();
        initial.mark_safe();
        drop(initial);

        for requested in [3, 5] {
            let mut permit = oplog.acquire_finalization().await.unwrap();
            permit.advance_committed_seq(requested).unwrap();
            permit.mark_safe();
        }

        assert_eq!(oplog.committed_seq().unwrap(), Some(5));
    }

    #[tokio::test]
    async fn unsafe_permit_drop_requires_recovery_to_unblock() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog = std::sync::Arc::new(
            OpLog::open(&tenant_path.join(OPLOG_DIR), "poisoned", "node1").unwrap(),
        );

        drop(oplog.acquire_finalization().await.unwrap());
        assert!(oplog.acquire_finalization().await.is_err());

        let mut recovery = oplog.acquire_recovery().unwrap();
        assert!(recovery.advance_committed_seq(99).is_err());
        assert_eq!(oplog.committed_seq().unwrap(), None);
        recovery.mark_recovered().unwrap();
        drop(recovery);
        let mut ordinary = oplog.acquire_finalization().await.unwrap();
        ordinary.mark_safe();
    }

    #[tokio::test]
    async fn retained_tail_above_missing_watermark_reopens_recovery_required() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = std::sync::Arc::new(
            OpLog::open(&oplog_dir, "uncommitted-tail", "node1").unwrap(),
        );
        let permit = oplog.acquire_finalization().await.unwrap();
        append_test_row(&oplog, "uncommitted", AppendDurability::Buffered).unwrap();
        drop(permit);
        drop(oplog);

        let reopened = std::sync::Arc::new(
            OpLog::open(&oplog_dir, "uncommitted-tail", "node1").unwrap(),
        );
        assert!(reopened.acquire_finalization().await.is_err());
        let mut recovery = reopened.acquire_recovery().unwrap();
        assert!(recovery.advance_committed_seq(1).is_err());
        assert!(recovery.mark_recovered().is_err());
        reopened.retract_from(1).unwrap();
        recovery.mark_recovered().unwrap();
        drop(recovery);

        let mut ordinary = reopened.acquire_finalization().await.unwrap();
        assert_eq!(
            append_test_row(&reopened, "replacement", AppendDurability::Synced).unwrap(),
            1
        );
        assert_eq!(ordinary.advance_committed_seq(1).unwrap(), 1);
        ordinary.mark_safe();
    }

    #[tokio::test]
    async fn recovery_permit_cannot_certify_an_uncommitted_tail_without_cleanup() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        let oplog_dir = tenant_path.join(OPLOG_DIR);
        let oplog = std::sync::Arc::new(
            OpLog::open(&oplog_dir, "unclean-tail", "node1").unwrap(),
        );
        let permit = oplog.acquire_finalization().await.unwrap();
        append_test_row(&oplog, "uncommitted", AppendDurability::Buffered).unwrap();
        drop(permit);

        let mut recovery = oplog.acquire_recovery().unwrap();
        assert!(recovery.mark_recovered().is_err());
        drop(recovery);

        assert!(
            oplog.acquire_finalization().await.is_err(),
            "recovery must not reopen finalization while the oplog tail exceeds committed_seq"
        );
    }

    #[test]
    fn open_rejects_corrupt_committed_seq_evidence() {
        let tmp = TempDir::new().unwrap();
        let tenant_path = tmp.path().join("tenant");
        std::fs::create_dir_all(&tenant_path).unwrap();
        std::fs::write(tenant_path.join(COMMITTED_SEQ_FILE), "corrupt").unwrap();

        assert!(OpLog::open(&tenant_path.join(OPLOG_DIR), "corrupt", "node1").is_err());
    }
