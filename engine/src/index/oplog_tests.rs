    use super::*;
    use std::io::{Seek, SeekFrom};
    use tempfile::TempDir;

    /// Verify that appending entries increments the sequence counter and that `read_since` correctly filters by sequence number.
    #[test]
    fn test_append_and_read() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();

        assert_eq!(oplog.current_seq(), 0);
        let s1 = oplog
            .append("upsert", serde_json::json!({"objectID": "1"}))
            .unwrap();
        assert_eq!(s1, 1);
        let s2 = oplog
            .append("delete", serde_json::json!({"objectID": "2"}))
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

    /// Retracting from a sequence floor erases the suffix, resets the counter,
    /// and lets subsequent appends continue from the surviving tail.
    #[test]
    fn retract_from_removes_suffix_and_resets_seq() {
        let tmp = TempDir::new().unwrap();
        let oplog = OpLog::open(tmp.path(), "t1", "node1").unwrap();
        for i in 1..=5 {
            oplog
                .append("upsert", serde_json::json!({ "objectID": i.to_string() }))
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
            .append("upsert", serde_json::json!({ "objectID": "6" }))
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
            .append("upsert", serde_json::json!({ "objectID": "1" }))
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
            .append("upsert", serde_json::json!({ "objectID": "baseline" }))
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
            .append("upsert", serde_json::json!({ "objectID": "committed" }))
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
            .append("upsert", serde_json::json!({ "objectID": "committed" }))
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
            .append("upsert", serde_json::json!({ "objectID": "replacement" }))
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
            .append("upsert", serde_json::json!({ "objectID": "committed" }))
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
            .append("upsert", serde_json::json!({ "objectID": "committed" }))
            .unwrap();
        oplog
            .append("upsert", serde_json::json!({ "objectID": "rejected" }))
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
            oplog.append("upsert", serde_json::json!({"x": 1})).unwrap();
            oplog.append("upsert", serde_json::json!({"x": 2})).unwrap();
        }

        let oplog2 = OpLog::open(&dir, "t1", "node1").unwrap();
        assert_eq!(oplog2.current_seq(), 2);
        let s3 = oplog2
            .append("delete", serde_json::json!({"x": 3}))
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
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
            oplog
                .rotate_segment_locked(&mut oplog.segment.lock().unwrap())
                .unwrap();
            for i in 5..10 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
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
    #[test]
    fn test_oldest_seq_after_truncate_before() {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path().to_path_buf();

        {
            let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
            for i in 0..5 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
            oplog
                .rotate_segment_locked(&mut oplog.segment.lock().unwrap())
                .unwrap();
            for i in 5..10 {
                oplog.append("upsert", serde_json::json!({"i": i})).unwrap();
            }
        }

        let oplog = OpLog::open(&dir, "t1", "node1").unwrap();
        assert_eq!(oplog.oldest_seq(), Some(1));

        let removed = oplog.truncate_before(6).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(oplog.oldest_seq(), Some(6));
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

        oplog.append("upsert", serde_json::json!({"a": 1})).unwrap();
        oplog.append("upsert", serde_json::json!({"a": 2})).unwrap();
        oplog.append("upsert", serde_json::json!({"a": 3})).unwrap();

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
