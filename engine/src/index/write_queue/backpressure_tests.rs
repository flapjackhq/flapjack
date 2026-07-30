fn stage_6_segment_observation(
    live_segment_count: usize,
    index_bytes: u64,
    orphan_file_set_count: usize,
) -> segment_observation::SegmentObservation {
    let live_segment_ids = (0..live_segment_count)
        .map(|index| format!("{index:032x}"))
        .collect::<BTreeSet<_>>();
    let per_segment_doc_counts = live_segment_ids
        .iter()
        .map(|segment_id| (segment_id.clone(), 1))
        .collect::<BTreeMap<_, _>>();
    let orphan_file_set_ids = (0..orphan_file_set_count)
        .map(|index| format!("{:032x}", index + 10_000))
        .collect::<BTreeSet<_>>();

    segment_observation::SegmentObservation {
        live_segment_count,
        live_segment_ids,
        live_docs: live_segment_count as u64,
        per_segment_doc_counts,
        managed_index_file_count: live_segment_count as u64,
        index_bytes,
        orphan_file_set_ids,
    }
}

fn record_stage_6_observation(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    result: crate::error::Result<segment_observation::SegmentObservation>,
) {
    backpressure::record_observation_result_for_test(tmp.path(), tenant_id, result).unwrap();
}

fn assert_stage_6_backpressure_error(result: crate::error::Result<TaskInfo>) {
    match result {
        Err(FlapjackError::IndexPaused(message)) => {
            assert!(
                message.contains("write backpressure"),
                "pause payload should identify write backpressure, got {message}"
            );
        }
        other => panic!("expected backpressure IndexPaused, got {other:?}"),
    }
}

fn assert_stage_6_pause_artifact(tmp: &tempfile::TempDir, tenant_id: &str, decision: &str) {
    let artifact_path = backpressure::pause_artifact_path(tmp.path(), tenant_id);
    let artifact = std::fs::read_to_string(&artifact_path)
        .unwrap_or_else(|error| panic!("{} should be readable: {error}", artifact_path.display()));
    let payload: serde_json::Value = serde_json::from_str(&artifact).unwrap();
    assert_eq!(payload["decision"], decision);
    assert_eq!(
        payload["selected_segment_band"],
        serde_json::json!([
            SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.0,
            SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1
        ])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn bulk_admission_pauses_when_segment_ceiling_persists_without_improvement() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_pauses_persistent_ceiling";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(stage_6_segment_observation(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3,
                bytes,
                1,
            )),
        );
    }

    assert_stage_6_backpressure_error(
        manager
            .add_documents_durable(
                tenant_id,
                vec![text_document("blocked_doc", "title", "stage6 blocked")],
            )
            .await,
    );
    assert!(
        manager.tenant_tasks_snapshot_for_test(tenant_id).is_empty(),
        "paused admission must not allocate task records"
    );
    assert!(
        !tmp.path()
            .join(tenant_id)
            .join(admission::WRITE_ADMISSION_DIR)
            .exists(),
        "paused durable admission must not append durable admission records"
    );
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");
}

#[tokio::test(flavor = "current_thread")]
async fn backpressure_does_not_fire_while_state_is_improving() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_improving_above_ceiling";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for (segments, bytes) in [
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 4, 12_000),
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3, 11_000),
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2, 10_000),
    ] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(stage_6_segment_observation(segments, bytes, 0)),
        );
    }

    manager
        .add_documents_durable(
            tenant_id,
            vec![text_document("accepted_doc", "title", "stage6 accepted")],
        )
        .await
        .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));
}

#[tokio::test(flavor = "current_thread")]
async fn reads_stay_live_while_bulk_admission_is_paused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_reads_live_when_paused";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents_durable(
            tenant_id,
            vec![text_document("live_doc", "title", "stage6 searchable")],
        )
        .await
        .unwrap();
    let tenant_id = tenant_id.to_string();
    let quiesce = manager.quiesce_tenant(&tenant_id).await.unwrap();
    drop(quiesce);
    backpressure::clear_for_test(tmp.path(), &tenant_id);
    for bytes in [20_000, 20_000, 20_000] {
        record_stage_6_observation(
            &tmp,
            &tenant_id,
            Ok(stage_6_segment_observation(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }

    assert_stage_6_backpressure_error(manager.add_documents(
        &tenant_id,
        vec![text_document("paused_doc", "title", "stage6 paused")],
    ));
    assert_eq!(
        manager.tenant_doc_count(&tenant_id),
        Some(1),
        "pause must not hide committed documents from read counters"
    );
    let result = manager
        .search(&tenant_id, "stage6 searchable", None, None, 10)
        .unwrap();
    assert_eq!(result.total, 1);
    assert_eq!(result.documents[0].document.id, "live_doc");
}

#[tokio::test(flavor = "current_thread")]
async fn indeterminate_observation_pauses_bulk_admission() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_indeterminate_observation";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    record_stage_6_observation(
        &tmp,
        tenant_id,
        Err(FlapjackError::Io("segment metadata unreadable".to_string())),
    );

    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document(
            "indeterminate_doc",
            "title",
            "stage6 indeterminate",
        )],
    ));
    assert!(
        manager.tenant_tasks_snapshot_for_test(tenant_id).is_empty(),
        "indeterminate pause must not allocate task records"
    );
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause_indeterminate");
}

#[tokio::test(flavor = "current_thread")]
async fn backpressure_clears_when_segment_count_recovers_below_selected_band() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_recovers_below_selected_band";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(stage_6_segment_observation(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }
    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document("blocked_doc", "title", "blocked before recovery")],
    ));

    record_stage_6_observation(
        &tmp,
        tenant_id,
        Ok(stage_6_segment_observation(
            SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.0 - 1,
            5_000,
            0,
        )),
    );

    manager
        .add_documents(
            tenant_id,
            vec![text_document(
                "accepted_doc",
                "title",
                "accepted after recovery",
            )],
        )
        .unwrap();
    assert_stage_6_pause_artifact(&tmp, tenant_id, "admit");
}

#[tokio::test(flavor = "current_thread")]
async fn deleting_tenant_removes_stale_backpressure_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_deleted_tenant_state";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(stage_6_segment_observation(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }
    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document("blocked_doc", "title", "blocked before delete")],
    ));

    manager.delete_tenant(&tenant_id.to_string()).await.unwrap();
    manager.create_tenant(tenant_id).unwrap();
    manager
        .add_documents(
            tenant_id,
            vec![text_document(
                "accepted_doc",
                "title",
                "accepted after recreate",
            )],
        )
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn backpressure_pauses_every_write_queue_entrypoint_before_task_allocation() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_all_entrypoints_paused";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);
    for bytes in [30_000, 30_000, 30_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(stage_6_segment_observation(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }

    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document("upsert_doc", "title", "stage6 upsert")],
    ));
    assert_stage_6_backpressure_error(manager.add_documents_insert(
        tenant_id,
        vec![text_document("insert_doc", "title", "stage6 insert")],
    ));
    assert_stage_6_backpressure_error(manager.add_documents_for_replication(
        tenant_id,
        vec![text_document(
            "replicated_doc",
            "title",
            "stage6 replicated add",
        )],
    ));
    assert_stage_6_backpressure_error(
        manager.delete_documents(tenant_id, vec!["upsert_doc".to_string()]),
    );
    assert_stage_6_backpressure_error(
        manager.delete_documents_for_replication(tenant_id, vec!["replicated_doc".to_string()]),
    );
    assert_stage_6_backpressure_error(manager.compact_index(tenant_id));
    assert_stage_6_backpressure_error(
        manager
            .add_documents_durable(
                tenant_id,
                vec![text_document("durable_doc", "title", "stage6 durable add")],
            )
            .await,
    );
    assert_stage_6_backpressure_error(
        manager
            .delete_documents_durable(tenant_id, vec!["durable_doc".to_string()])
            .await,
    );
    assert!(
        manager.tenant_tasks_snapshot_for_test(tenant_id).is_empty(),
        "paused entry points must not allocate task records"
    );
}
