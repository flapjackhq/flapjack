fn record_stage_6_observation(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    result: crate::error::Result<segment_observation::SegmentObservation>,
) {
    backpressure::record_observation_result_for_test(tmp.path(), tenant_id, result).unwrap();
}

fn force_stage_6_backpressure_pause(tmp: &tempfile::TempDir, tenant_id: &str) {
    backpressure::force_backpressure_pause_for_test(tmp.path(), tenant_id).unwrap();
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

fn assert_stage_6_io_error(result: crate::error::Result<TaskInfo>) {
    match result {
        Err(FlapjackError::Io(message)) => {
            assert!(
                !message.is_empty(),
                "I/O failures should preserve the underlying filesystem detail"
            );
        }
        other => panic!("expected I/O failure, got {other:?}"),
    }
}

const STAGE_6_DURABLE_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn add_stage_6_documents_and_wait_for_test(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
    docs: Vec<Document>,
) -> crate::error::Result<TaskInfo> {
    manager
        .add_documents_durable_with_timeout_for_test(tenant_id, docs, STAGE_6_DURABLE_WAIT_TIMEOUT)
        .await
}

async fn add_stage_6_documents_insert_and_wait_for_test(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
    docs: Vec<Document>,
) -> crate::error::Result<TaskInfo> {
    manager
        .add_documents_insert_durable_with_timeout_for_test(
            tenant_id,
            docs,
            STAGE_6_DURABLE_WAIT_TIMEOUT,
        )
        .await
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

fn sorted_stage_1_latencies(samples: &[(u64, std::time::Duration)]) -> Vec<std::time::Duration> {
    let mut latencies = samples
        .iter()
        .map(|(_, latency)| *latency)
        .collect::<Vec<_>>();
    latencies.sort_unstable();
    latencies
}

fn stage_1_latency_ms(latencies: &[std::time::Duration]) -> Vec<u128> {
    latencies
        .iter()
        .map(std::time::Duration::as_millis)
        .collect()
}

fn stage_1_p99(latencies: &[std::time::Duration]) -> std::time::Duration {
    let rank = (99 * latencies.len()).div_ceil(100);
    latencies[rank - 1]
}

const STAGE_1_COUNT_STALL_RED_THRESHOLD: std::time::Duration =
    std::time::Duration::from_millis(1_000);

fn stage_1_count_stall_detected(count_max: std::time::Duration) -> bool {
    count_max > STAGE_1_COUNT_STALL_RED_THRESHOLD
}

fn sample_counts_until_task_terminal(
    sample_interval: std::time::Duration,
    mut task_is_terminal: impl FnMut() -> bool,
    mut read_count: impl FnMut() -> u64,
) -> Vec<(u64, std::time::Duration)> {
    let mut samples = Vec::new();
    while !task_is_terminal() {
        let count_started = std::time::Instant::now();
        let count = read_count();
        samples.push((count, count_started.elapsed()));
        std::thread::sleep(sample_interval);
    }
    samples
}

async fn wait_for_stage_1_task_processing(
    manager: &crate::index::manager::IndexManager,
    task_id: &str,
) {
    let processing_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let task = manager.get_task(task_id).unwrap();
        if matches!(task.status, crate::types::TaskStatus::Processing) {
            return;
        }
        assert!(
            std::time::Instant::now() < processing_deadline,
            "delayed write never entered processing; task={task:?}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

fn hold_stage_1_backpressure_pause(
    tmp: &tempfile::TempDir,
    tenant_id: &str,
    manager: &crate::index::manager::IndexManager,
) -> impl Drop {
    let pause_guard =
        backpressure::hold_non_improving_pause_for_test(tmp.path(), tenant_id).unwrap();
    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document("paused_doc", "title", "stage1 paused")],
    ));
    pause_guard
}

fn assert_stage_1_count_samples(
    manager: &crate::index::manager::IndexManager,
    tenant_id: &str,
    count_samples: &[(u64, std::time::Duration)],
    overlap_elapsed: std::time::Duration,
) {
    assert!(
        count_samples.len() >= 25,
        "overlap sample denominator too small: count_samples={} overlap_ms={}",
        count_samples.len(),
        overlap_elapsed.as_millis()
    );
    assert_eq!(
        manager.tenant_doc_count(tenant_id),
        Some(2),
        "delayed write must publish after the measured overlap"
    );
    assert!(
        count_samples.iter().any(|(count, _)| *count == 1),
        "overlap count samples must include the pre-existing published reader state: {count_samples:?}"
    );
    assert!(
        count_samples.iter().all(|(count, _)| *count == 1 || *count == 2),
        "count samples must be either the pre-existing reader state or the final published state at the terminal boundary: {count_samples:?}"
    );
}

#[test]
fn count_sampler_keeps_read_that_started_before_task_became_terminal() {
    let task_is_terminal = std::cell::Cell::new(false);

    let samples = sample_counts_until_task_terminal(
        std::time::Duration::ZERO,
        || task_is_terminal.get(),
        || {
            task_is_terminal.set(true);
            2
        },
    );

    assert_eq!(samples.len(), 1);
    assert_eq!(samples[0].0, 2);
}

#[test]
fn count_stall_threshold_only_fires_above_one_second() {
    assert!(!stage_1_count_stall_detected(
        STAGE_1_COUNT_STALL_RED_THRESHOLD
    ));
    assert!(stage_1_count_stall_detected(
        STAGE_1_COUNT_STALL_RED_THRESHOLD + std::time::Duration::from_millis(1)
    ));
}

#[test]
fn failed_test_pause_setup_clears_in_memory_state() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "failed_test_pause_setup";
    std::fs::write(tmp.path().join(tenant_id), b"blocks artifact directory").unwrap();

    let result = backpressure::hold_non_improving_pause_for_test(tmp.path(), tenant_id);

    assert!(result.is_err(), "artifact persistence must fail");
    assert!(
        !backpressure::tenant_is_paused_for_test(tmp.path(), tenant_id),
        "failed setup must not leave a pause without a cleanup guard"
    );
}

#[test]
fn stage_1_p99_uses_nearest_rank() {
    let latencies = (1..=100)
        .map(std::time::Duration::from_millis)
        .collect::<Vec<_>>();

    assert_eq!(
        stage_1_p99(&latencies),
        std::time::Duration::from_millis(99)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn read_count_stays_live_while_backpressure_pause_and_commit_overlap() {
    const COMMIT_DELAY: std::time::Duration = std::time::Duration::from_millis(1_500);
    const DURABLE_WAIT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
    const SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage1_pause_commit_count_live";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    let seed_task = manager
        .add_documents(
            tenant_id,
            vec![text_document("seed_doc", "title", "stage1 seed")],
        )
        .unwrap();
    manager
        .wait_for_write_durable_with_timeout_for_test(&seed_task.id, DURABLE_WAIT_TIMEOUT)
        .await
        .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));

    let _delay = delay_commits_for_test(tenant_id, COMMIT_DELAY);
    let delayed_task = manager
        .add_documents(
            tenant_id,
            vec![text_document(
                "delayed_doc",
                "title",
                "stage1 delayed commit",
            )],
        )
        .expect("delayed write must be admitted before pause is held");

    wait_for_stage_1_task_processing(&manager, &delayed_task.id).await;
    let _pause_guard = hold_stage_1_backpressure_pause(&tmp, tenant_id, &manager);

    let overlap_started = std::time::Instant::now();
    let sampler = std::thread::spawn({
        let manager = Arc::clone(&manager);
        let delayed_task_id = delayed_task.id.clone();
        move || {
            sample_counts_until_task_terminal(
                SAMPLE_INTERVAL,
                || {
                    matches!(
                        manager.get_task(&delayed_task_id).unwrap().status,
                        crate::types::TaskStatus::Succeeded | crate::types::TaskStatus::Failed(_)
                    )
                },
                || {
                    manager
                        .tenant_doc_count(tenant_id)
                        .expect("count tenant must remain loaded during overlap")
                },
            )
        }
    });

    manager
        .wait_for_write_durable_with_timeout_for_test(&delayed_task.id, DURABLE_WAIT_TIMEOUT)
        .await
        .unwrap();
    let overlap_elapsed = overlap_started.elapsed();
    let count_samples = sampler.join().unwrap();

    assert_stage_1_count_samples(&manager, tenant_id, &count_samples, overlap_elapsed);

    let count_latencies = sorted_stage_1_latencies(&count_samples);
    let count_p99 = stage_1_p99(&count_latencies);
    let count_max = *count_latencies.last().unwrap();
    let count_stall_detected = stage_1_count_stall_detected(count_max);
    eprintln!(
        "Stage 1 pause+commit count characterization: overlap_samples={} overlap_ms={} count={:?} count_p99_ms={} count_max_ms={} count_stall_detected={}",
        count_samples.len(),
        overlap_elapsed.as_millis(),
        stage_1_latency_ms(&count_latencies),
        count_p99.as_millis(),
        count_max.as_millis(),
        count_stall_detected
    );
    assert!(
        !count_stall_detected,
        "documents_count exceeded the Stage 1 red threshold during overlap: count_max={count_max:?}, distribution_ms={:?}",
        stage_1_latency_ms(&count_latencies)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn forced_backpressure_pause_blocks_first_durable_insert_then_recovers() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "forced_pause_blocks_durable_insert";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    crate::index::write_queue::force_backpressure_pause_for_test(tmp.path(), tenant_id).unwrap();

    assert_stage_6_backpressure_error(
        manager
            .add_documents_insert_durable(
                tenant_id,
                vec![text_document(
                    "blocked_insert",
                    "title",
                    "forced pause blocked insert",
                )],
            )
            .await,
    );
    assert!(
        manager.tenant_tasks_snapshot_for_test(tenant_id).is_empty(),
        "paused durable insert must not allocate task records"
    );
    assert!(
        !tmp.path()
            .join(tenant_id)
            .join(admission::WRITE_ADMISSION_DIR)
            .exists(),
        "paused durable insert must not append durable admission records"
    );

    add_stage_6_documents_insert_and_wait_for_test(
        &manager,
        tenant_id,
        vec![text_document(
            "retry_insert",
            "title",
            "forced pause retry accepted",
        )],
    )
    .await
    .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));
    assert!(
        manager.get_document(tenant_id, "retry_insert").unwrap().is_some(),
        "subsequent retry should write through normal durable admission"
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
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3,
                bytes,
                1,
            )),
        );
    }
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");

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
}

#[tokio::test(flavor = "current_thread")]
async fn backpressure_does_not_fire_while_state_is_improving() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_improving_above_ceiling";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for (segments, bytes) in [
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 4, 10_000),
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3, 11_000),
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2, 12_000),
    ] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(backpressure::segment_observation_for_test(
                segments, bytes, 0,
            )),
        );
    }
    assert!(
        !backpressure::pause_artifact_path(tmp.path(), tenant_id).exists(),
        "decreasing live-segment counts are improving even while index bytes grow"
    );

    add_stage_6_documents_and_wait_for_test(
        &manager,
        tenant_id,
        vec![text_document("accepted_doc", "title", "stage6 accepted")],
    )
    .await
    .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_timeout_add_helper_uses_durable_admission_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_add_helper_durable_store";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    std::fs::write(
        tmp.path()
            .join(tenant_id)
            .join(admission::WRITE_ADMISSION_DIR),
        b"blocks durable admission directory",
    )
    .unwrap();

    assert_stage_6_io_error(
        add_stage_6_documents_and_wait_for_test(
            &manager,
            tenant_id,
            vec![text_document(
                "blocked_durable_add",
                "title",
                "durable admission must be used",
            )],
        )
        .await,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn explicit_timeout_insert_helper_uses_durable_admission_store() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_insert_helper_durable_store";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    std::fs::write(
        tmp.path()
            .join(tenant_id)
            .join(admission::WRITE_ADMISSION_DIR),
        b"blocks durable admission directory",
    )
    .unwrap();

    assert_stage_6_io_error(
        add_stage_6_documents_insert_and_wait_for_test(
            &manager,
            tenant_id,
            vec![text_document(
                "blocked_durable_insert",
                "title",
                "durable admission must be used",
            )],
        )
        .await,
    );
}

#[tokio::test(flavor = "current_thread")]
async fn reads_stay_live_while_bulk_admission_is_paused() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_reads_live_when_paused";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    add_stage_6_documents_and_wait_for_test(
        &manager,
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
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }
    assert_stage_6_pause_artifact(&tmp, &tenant_id, "pause");

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
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause_indeterminate");

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
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");

    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document(
            "blocked_doc",
            "title",
            "blocked before recovery",
        )],
    ));
    record_stage_6_observation(
        &tmp,
        tenant_id,
        Ok(backpressure::segment_observation_for_test(
            SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.0 - 1,
            5_000,
            0,
        )),
    );
    assert_stage_6_pause_artifact(&tmp, tenant_id, "admit");

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
}

#[tokio::test(flavor = "current_thread")]
async fn backpressure_clears_when_segment_growth_stops_above_selected_ceiling() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_stable_plateau_above_ceiling";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3,
                bytes,
                1,
            )),
        );
    }
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");

    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document(
            "blocked_doc",
            "title",
            "blocked before plateau recovery",
        )],
    ));
    record_stage_6_observation(
        &tmp,
        tenant_id,
        Ok(backpressure::segment_observation_for_test(
            SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3,
            12_000,
            1,
        )),
    );
    assert_stage_6_pause_artifact(&tmp, tenant_id, "admit");

    manager
        .add_documents(
            tenant_id,
            vec![text_document(
                "accepted_doc",
                "title",
                "accepted after segment growth stops",
            )],
        )
        .unwrap();
}

#[tokio::test(flavor = "current_thread")]
async fn backpressure_stays_active_when_segments_rise_to_paused_window_peak() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_segments_rise_to_paused_peak";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for (segments, bytes) in [
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 5, 10_000),
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 3, 10_500),
        (SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 4, 11_000),
    ] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(backpressure::segment_observation_for_test(
                segments, bytes, 1,
            )),
        );
    }
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");

    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document(
            "blocked_doc",
            "title",
            "blocked before renewed segment growth",
        )],
    ));
    record_stage_6_observation(
        &tmp,
        tenant_id,
        Ok(backpressure::segment_observation_for_test(
            SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 5,
            12_000,
            1,
        )),
    );

    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");
}

#[tokio::test(flavor = "current_thread")]
async fn paused_bulk_admission_resamples_loaded_index_and_recovers() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_paused_admission_resamples_loaded_index";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }

    assert_stage_6_backpressure_error(
        manager
            .add_documents_insert_durable(
                tenant_id,
                vec![text_document(
                    "blocked_doc",
                    "title",
                    "blocked before admission resample",
                )],
            )
            .await,
    );
    add_stage_6_documents_insert_and_wait_for_test(
        &manager,
        tenant_id,
        vec![text_document(
            "accepted_doc",
            "title",
            "accepted after admission resample",
        )],
    )
    .await
    .unwrap();
    assert_eq!(manager.tenant_doc_count(tenant_id), Some(1));
    assert_stage_6_pause_artifact(&tmp, tenant_id, "admit");
}

#[tokio::test(flavor = "current_thread")]
async fn paused_bulk_admission_propagates_resample_artifact_write_failures() {
    let tmp = tempfile::TempDir::new().unwrap();
    let tenant_id = "stage6_resample_artifact_write_failure";
    let manager = crate::index::manager::IndexManager::new(tmp.path());
    manager.create_tenant(tenant_id).unwrap();
    backpressure::clear_for_test(tmp.path(), tenant_id);

    for bytes in [10_000, 10_500, 11_000] {
        record_stage_6_observation(
            &tmp,
            tenant_id,
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }

    assert_stage_6_backpressure_error(
        manager
            .add_documents_insert_durable(
                tenant_id,
                vec![text_document(
                    "blocked_doc",
                    "title",
                    "blocked before artifact write failure",
                )],
            )
            .await,
    );

    let artifact_path = backpressure::pause_artifact_path(tmp.path(), tenant_id);
    let artifact_parent = artifact_path.parent().unwrap().to_path_buf();
    let _hook = backpressure::set_pause_artifact_publication_hook_for_test(
        &artifact_path,
        std::sync::Arc::new(|temp_path| {
            assert!(crate::index::utils::is_temporary_entry(temp_path));
            std::fs::remove_file(temp_path).unwrap();
        }),
    );

    assert_stage_6_io_error(
        manager
            .add_documents_insert_durable(
                tenant_id,
                vec![text_document(
                    "io_failure_doc",
                    "title",
                    "artifact write failure should surface",
                )],
            )
            .await,
    );
    assert!(
        manager.tenant_tasks_snapshot_for_test(tenant_id).is_empty(),
        "failed resample writes must not allocate task records"
    );
    assert!(
        std::fs::read_dir(&artifact_parent)
            .unwrap()
            .all(|entry| !crate::index::utils::is_temporary_entry(&entry.unwrap().path())),
        "failed artifact publication must remove its temporary replacement"
    );
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
            Ok(backpressure::segment_observation_for_test(
                SELECTED_MERGE_POLICY_SETTLED_SEGMENT_BAND.1 + 2,
                bytes,
                0,
            )),
        );
    }
    assert_stage_6_pause_artifact(&tmp, tenant_id, "pause");

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

    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(manager.add_documents(
        tenant_id,
        vec![text_document("upsert_doc", "title", "stage6 upsert")],
    ));
    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(manager.add_documents_insert(
        tenant_id,
        vec![text_document("insert_doc", "title", "stage6 insert")],
    ));
    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(manager.add_documents_for_replication(
        tenant_id,
        vec![text_document(
            "replicated_doc",
            "title",
            "stage6 replicated add",
        )],
    ));
    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(
        manager.delete_documents(tenant_id, vec!["upsert_doc".to_string()]),
    );
    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(
        manager.delete_documents_for_replication(tenant_id, vec!["replicated_doc".to_string()]),
    );
    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(manager.compact_index(tenant_id));
    force_stage_6_backpressure_pause(&tmp, tenant_id);
    assert_stage_6_backpressure_error(
        manager
            .add_documents_durable(
                tenant_id,
                vec![text_document("durable_doc", "title", "stage6 durable add")],
            )
            .await,
    );
    force_stage_6_backpressure_pause(&tmp, tenant_id);
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
