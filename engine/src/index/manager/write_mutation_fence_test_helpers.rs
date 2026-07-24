    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_evicts_replay_opened_staging_oplog_before_source_reuse() {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "reuse_live";
        let staging_id = "reuse_staging";
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant(destination_id).unwrap();
        manager.create_tenant(staging_id).unwrap();
        manager
            .add_documents_sync(
                destination_id,
                vec![document("destination_snapshot", "destination snapshot")],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                destination_id,
                vec![document("destination_tail", "destination drained tail")],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                staging_id,
                vec![document("staging_v1", "first staging replacement")],
            )
            .await
            .unwrap();
        // The drained destination proves committed_seq = W = 2 (snapshot + tail);
        // the baseline captures the pre-tail snapshot point so the tail replays.
        let first_staging_baseline = publication::PublicationStagingBaseline::new(1);

        manager
            .replace_index_contents(staging_id, destination_id, first_staging_baseline)
            .await
            .unwrap();

        manager.create_tenant(staging_id).unwrap();
        manager
            .add_documents_sync(
                staging_id,
                vec![document("staging_v2", "second staging replacement")],
            )
            .await
            .unwrap();
        assert_tenant_oplog_contains_object(temp_dir.path(), staging_id, "staging_v2");
        let second_staging_baseline = manager
            .capture_replacement_staging_baseline(destination_id)
            .unwrap();

        manager
            .replace_index_contents(staging_id, destination_id, second_staging_baseline)
            .await
            .unwrap();
        assert_document_title(
            &manager,
            destination_id,
            "staging_v2",
            "second staging replacement",
        );
        let committed_seq =
            crate::index::oplog::read_committed_seq(&temp_dir.path().join(destination_id));
        let appended_seq = append_uncommitted_upsert(
            &manager,
            destination_id,
            document("post_reuse_uncommitted", "post reuse crash replay"),
        );
        assert!(
            appended_seq > committed_seq,
            "first post-reuse oplog seq must be above committed watermark: appended_seq={appended_seq}, committed_seq={committed_seq}"
        );
        drop(manager);

        let restarted = IndexManager::new(temp_dir.path());
        assert_document_title(
            &restarted,
            destination_id,
            "post_reuse_uncommitted",
            "post reuse crash replay",
        );
    }

    #[cfg(feature = "vector-search")]
    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_replays_destination_tail_vector_effects() {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "vector_tail_live";
        let staging_id = "vector_tail_staging";
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant(destination_id).unwrap();
        manager.create_tenant(staging_id).unwrap();
        save_user_provided_vector_settings(temp_dir.path(), destination_id);
        save_user_provided_vector_settings(temp_dir.path(), staging_id);
        manager
            .add_documents_sync(
                staging_id,
                vec![vector_document(
                    "staging_seed",
                    "staging replacement",
                    [0.2, 0.2, 0.2],
                )],
            )
            .await
            .unwrap();

        let destination_oplog = manager.get_or_create_oplog_result(destination_id).unwrap();
        // Baseline is the pre-tail destination watermark; the vector tail below lands
        // after it and must be replayed into staging.
        let staging_baseline =
            publication::PublicationStagingBaseline::new(destination_oplog.current_seq());
        let deleted_tail = vector_document("vector_deleted", "deleted tail", [1.0, 0.0, 0.0]);
        let cleared_tail = vector_document("vector_cleared", "cleared tail", [0.0, 1.0, 0.0]);
        let surviving_tail = vector_document("vector_survivor", "surviving tail", [0.0, 0.0, 1.0]);
        destination_oplog
            .append_batch(&[
                oplog_upsert_payload(&deleted_tail),
                (
                    "delete".to_string(),
                    serde_json::json!({"objectID": "vector_deleted"}),
                ),
                oplog_upsert_payload(&cleared_tail),
                ("clear".to_string(), serde_json::json!({})),
                oplog_upsert_payload(&surviving_tail),
            ])
            .unwrap();
        // The drained destination proves committed_seq = W = the post-tail oplog max.
        crate::index::oplog::write_committed_seq(
            &temp_dir.path().join(destination_id),
            destination_oplog.current_seq(),
        )
        .unwrap();

        manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await
            .unwrap();
        drop(manager);

        let restarted = IndexManager::new(temp_dir.path());
        restarted.get_or_load(destination_id).unwrap();
        assert_promoted_vector_state(&restarted, destination_id);
    }

    enum DestinationDrainFailure {
        WorkerErr,
        JoinPanic,
    }

    async fn assert_replacement_drain_failure_blocks_promotion(failure: DestinationDrainFailure) {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "drain_failure_live";
        let staging_id = "drain_failure_staging";
        let control_id = "drain_failure_control";
        let manager = IndexManager::new(temp_dir.path());
        create_drain_failure_fixture(&manager, destination_id, staging_id, control_id).await;
        let staging_baseline = manager
            .capture_replacement_staging_baseline(destination_id)
            .unwrap();

        replace_destination_write_handle(&manager, destination_id, failure).await;
        let result = manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await;

        assert_drain_failure_result(result, destination_id);
        assert_destination_not_promoted(&manager, destination_id);
        assert_control_tenant_writable(&manager, control_id).await;

        let retry_result = manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await;
        assert_drain_failure_result(retry_result, destination_id);
        assert_destination_not_promoted(&manager, destination_id);
        assert_control_tenant_writable(&manager, control_id).await;
    }

    async fn create_drain_failure_fixture(
        manager: &IndexManager,
        destination_id: &str,
        staging_id: &str,
        control_id: &str,
    ) {
        manager.create_tenant(destination_id).unwrap();
        manager.create_tenant(staging_id).unwrap();
        manager.create_tenant(control_id).unwrap();
        manager
            .add_documents_sync(
                destination_id,
                vec![document("destination_seed", "destination before failure")],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                staging_id,
                vec![document("staging_seed", "staging replacement")],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(control_id, vec![document("control_seed", "control before")])
            .await
            .unwrap();
    }

    fn assert_drain_failure_result(result: Result<TaskInfo>, destination_id: &str) {
        let error =
            result.expect_err("destination drain failure must fail replacement before promotion");
        let message = error.to_string();
        assert!(
            message.contains("destination write queue")
                && message.contains(destination_id)
                && message.contains("drain"),
            "replacement failure must identify the destination drain context, got {message}"
        );
    }

    fn assert_destination_not_promoted(manager: &IndexManager, destination_id: &str) {
        assert!(
            manager.loaded.contains_key(destination_id),
            "failed destination drain must not clear destination runtime state"
        );
        assert_document_title(
            manager,
            destination_id,
            "destination_seed",
            "destination before failure",
        );
        assert!(
            manager
                .get_document(destination_id, "staging_seed")
                .unwrap()
                .is_none(),
            "failed destination drain must not promote staging content"
        );
    }

    async fn assert_control_tenant_writable(manager: &IndexManager, control_id: &str) {
        manager
            .add_documents_sync(
                control_id,
                vec![document("control_after_failure", "control after failure")],
            )
            .await
            .unwrap();
        assert_document_title(manager, control_id, "control_seed", "control before");
        assert_document_title(
            manager,
            control_id,
            "control_after_failure",
            "control after failure",
        );
        assert!(
            manager.write_queues.contains_key(control_id),
            "failed destination drain must not close unrelated tenant queue"
        );
        assert!(
            manager.write_task_handles.contains_key(control_id),
            "failed destination drain must not remove unrelated tenant handle"
        );
    }

    async fn replace_destination_write_handle(
        manager: &IndexManager,
        tenant_id: &str,
        failure: DestinationDrainFailure,
    ) {
        if let Some((_, handle)) = manager.write_task_handles.remove(tenant_id) {
            handle.abort();
        }
        let handle = match failure {
            DestinationDrainFailure::WorkerErr => tokio::spawn(async {
                Err(FlapjackError::Tantivy(
                    "injected destination drain failure".to_string(),
                ))
            }),
            DestinationDrainFailure::JoinPanic => tokio::spawn(async {
                panic!("injected destination drain panic");
                #[allow(unreachable_code)]
                Ok(())
            }),
        };
        manager
            .write_task_handles
            .insert(tenant_id.to_string(), WriteTaskHandle::new(handle));
    }

    async fn replace_destination_write_handle_with_blocked_success(
        manager: &IndexManager,
        tenant_id: &str,
    ) -> tokio::sync::oneshot::Sender<()> {
        if let Some((_, handle)) = manager.write_task_handles.remove(tenant_id) {
            handle.abort();
        }
        let (release_drain, await_release) = tokio::sync::oneshot::channel();
        manager.write_task_handles.insert(
            tenant_id.to_string(),
            WriteTaskHandle::new(tokio::spawn(async move {
                await_release
                    .await
                    .expect("test controls blocked destination drain release");
                Ok(())
            })),
        );
        release_drain
    }

    async fn wait_for_destination_queue_removed(manager: &IndexManager, tenant_id: &str) {
        for _ in 0..ADMISSION_WAIT_YIELDS {
            if !manager.write_queues.contains_key(tenant_id) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("replacement did not reach destination drain for {tenant_id}");
    }

    async fn assert_task_stays_pending<T>(task: &tokio::task::JoinHandle<T>) {
        for _ in 0..ADMISSION_WAIT_YIELDS {
            assert!(
                !task.is_finished(),
                "retry must not finish before the cancelled destination drain has terminal evidence"
            );
            tokio::task::yield_now().await;
        }
    }

    fn append_uncommitted_upsert(
        manager: &IndexManager,
        tenant_id: &str,
        document: Document,
    ) -> u64 {
        manager
            .get_or_create_oplog_result(tenant_id)
            .unwrap()
            .append(
                "upsert",
                serde_json::json!({"objectID": document.id, "body": document.to_json()}),
            )
            .unwrap()
    }

    #[cfg(feature = "vector-search")]
    fn oplog_upsert_payload(document: &Document) -> (String, serde_json::Value) {
        (
            "upsert".to_string(),
            serde_json::json!({"objectID": document.id, "body": document.to_json()}),
        )
    }

    fn assert_replacement_refused_before_promotion(
        manager: &IndexManager,
        destination_id: &str,
        present_object_id: &str,
        present_title: &str,
        absent_staging_object_id: &str,
    ) {
        assert!(
            manager.loaded.contains_key(destination_id),
            "refused replacement must leave the old destination loaded"
        );
        assert_document_title(manager, destination_id, present_object_id, present_title);
        assert!(
            manager
                .get_document(destination_id, absent_staging_object_id)
                .unwrap()
                .is_none(),
            "refused replacement must not promote staging content"
        );
    }

    fn remove_oplog_entry(base_path: &std::path::Path, tenant_id: &str, seq: u64) {
        let oplog_dir = base_path
            .join(tenant_id)
            .join(crate::index::oplog::OPLOG_DIR);
        let needle = format!("\"seq\":{seq},");
        for entry in std::fs::read_dir(&oplog_dir).unwrap() {
            let path = entry.unwrap().path();
            let is_segment = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("segment_") && name.ends_with(".jsonl"))
                .unwrap_or(false);
            if !is_segment {
                continue;
            }
            let contents = std::fs::read_to_string(&path).unwrap();
            let filtered: String = contents
                .lines()
                .filter(|line| !line.contains(&needle))
                .map(|line| format!("{line}\n"))
                .collect();
            std::fs::write(&path, filtered).unwrap();
        }
    }

    fn assert_promoted_watermark(
        manager: &IndexManager,
        base_path: &std::path::Path,
        tenant_id: &str,
        watermark: u64,
    ) {
        assert_eq!(
            crate::index::oplog::read_committed_seq(&base_path.join(tenant_id)),
            watermark,
            "promoted committed_seq must equal the drained watermark W"
        );
        assert_eq!(
            manager
                .get_or_create_oplog_result(tenant_id)
                .unwrap()
                .current_seq(),
            watermark,
            "promoted oplog maximum must equal the drained watermark W"
        );
    }

    fn assert_document_title(
        manager: &IndexManager,
        tenant_id: &str,
        object_id: &str,
        expected_title: &str,
    ) {
        let document = manager
            .get_document(tenant_id, object_id)
            .unwrap()
            .unwrap_or_else(|| panic!("{tenant_id}/{object_id} should exist"));
        assert_eq!(
            document.fields.get("title"),
            Some(&FieldValue::Text(expected_title.to_string()))
        );
    }

    fn assert_tenant_oplog_contains_object(
        base_path: &std::path::Path,
        tenant_id: &str,
        object_id: &str,
    ) {
        let oplog_dir = base_path.join(tenant_id).join("oplog");
        let segment = std::fs::read_dir(&oplog_dir)
            .unwrap_or_else(|error| {
                panic!(
                    "tenant {tenant_id} should have a durable oplog directory at {}: {error}",
                    oplog_dir.display()
                )
            })
            .filter_map(|entry| entry.ok())
            .find(|entry| entry.file_name().to_string_lossy().starts_with("segment_"))
            .unwrap_or_else(|| panic!("tenant {tenant_id} should have an oplog segment"));
        let contents = std::fs::read_to_string(segment.path()).unwrap();
        assert!(
            contents.contains(object_id),
            "tenant {tenant_id} oplog segment must include acknowledged object {object_id}: {contents}"
        );
    }

    #[cfg(feature = "vector-search")]
    fn save_user_provided_vector_settings(base_path: &std::path::Path, tenant_id: &str) {
        let settings = IndexSettings {
            embedders: Some(HashMap::from([(
                "default".to_string(),
                serde_json::json!({
                    "source": "userProvided",
                    "dimensions": 3
                }),
            )])),
            ..Default::default()
        };
        settings
            .save(base_path.join(tenant_id).join("settings.json"))
            .unwrap();
    }

    #[cfg(feature = "vector-search")]
    fn assert_promoted_vector_state(manager: &IndexManager, tenant_id: &str) {
        let vector_index = manager
            .get_vector_index(tenant_id)
            .unwrap_or_else(|| panic!("{tenant_id} should load promoted vector index"));
        let guard = vector_index.read().unwrap();
        assert_eq!(
            guard.len(),
            1,
            "tail delete and clear must leave only the final survivor vector"
        );
        assert!(
            guard.get("vector_deleted").unwrap().is_none(),
            "tail delete must remove vector_deleted"
        );
        assert!(
            guard.get("vector_cleared").unwrap().is_none(),
            "tail clear must remove vector_cleared"
        );
        assert_eq!(
            guard.get("vector_survivor").unwrap(),
            Some(vec![0.0, 0.0, 1.0]),
            "tail survivor upsert vector must be promoted exactly"
        );
        let results = guard.search(&[0.0, 0.0, 1.0], 1).unwrap();
        assert_eq!(results[0].doc_id, "vector_survivor");
    }
