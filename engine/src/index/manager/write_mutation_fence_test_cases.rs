    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
    use std::sync::Mutex;
    use tempfile::TempDir;
    use tokio::sync::Barrier;

    const ATTEMPTED_MUTATIONS: usize = 24;
    const ACKED_BEFORE_REPLACEMENT: usize = 12;
    const OVERLAPPING_MUTATIONS: usize = ATTEMPTED_MUTATIONS - ACKED_BEFORE_REPLACEMENT;
    const ADMISSION_WAIT_YIELDS: usize = 10_000;
    const CONTROL_TENANT_ID: &str = "mutation_fence_control";

    #[derive(Debug)]
    struct MutationOutcome {
        object_id: String,
        result: std::result::Result<String, String>,
    }

    #[derive(Clone, Default)]
    struct MutationCounters {
        attempted: Arc<AtomicUsize>,
        active_durable_calls: Arc<AtomicUsize>,
        outcomes: Arc<Mutex<Vec<MutationOutcome>>>,
    }

    struct ReplacementObservation {
        admitted_tasks_before_replacement: usize,
        attempted_mutations_at_replacement_start: usize,
        active_durable_calls_at_replacement_start: usize,
        active_durable_calls_after_replacement: usize,
        replacement: std::result::Result<String, String>,
    }

    async fn mutation_fence_fixture(
    ) -> (
        TempDir,
        Arc<IndexManager>,
        publication::PublicationStagingBaseline,
    ) {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("mutation_fence_live").unwrap();
        manager.create_tenant("mutation_fence_staging").unwrap();
        manager.create_tenant(CONTROL_TENANT_ID).unwrap();
        manager
            .add_documents_sync(
                "mutation_fence_live",
                vec![document("live_seed", "live generation seed")],
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                "mutation_fence_staging",
                replacement_generation_documents(),
            )
            .await
            .unwrap();
        manager
            .add_documents_sync(
                CONTROL_TENANT_ID,
                vec![document("control_seed", "control generation seed")],
            )
            .await
            .unwrap();
        let staging_baseline = manager
            .capture_replacement_staging_baseline("mutation_fence_live")
            .unwrap();
        (temp_dir, manager, staging_baseline)
    }

    fn spawn_mutations(
        manager: &Arc<IndexManager>,
        counters: &MutationCounters,
        attempts: std::ops::Range<usize>,
        start_barrier: Option<Arc<Barrier>>,
    ) -> Vec<tokio::task::JoinHandle<()>> {
        attempts
            .map(|attempt| {
                let manager = Arc::clone(manager);
                let counters = counters.clone();
                let start_barrier = start_barrier.clone();
                tokio::spawn(async move {
                    if let Some(start_barrier) = start_barrier {
                        start_barrier.wait().await;
                    }
                    run_mutation_attempt(manager, counters, attempt).await;
                })
            })
            .collect()
    }

    async fn run_mutation_attempt(
        manager: Arc<IndexManager>,
        counters: MutationCounters,
        attempt: usize,
    ) {
        let object_id = format!("mutation_fence_acked_{attempt:02}");
        counters.attempted.fetch_add(1, AtomicOrdering::SeqCst);
        counters
            .active_durable_calls
            .fetch_add(1, AtomicOrdering::SeqCst);
        let result = manager
            .add_documents_durable(
                "mutation_fence_live",
                vec![document(&object_id, "acked during replacement")],
            )
            .await
            .map(|task| task.id)
            .map_err(|error| error.to_string());
        counters
            .active_durable_calls
            .fetch_sub(1, AtomicOrdering::SeqCst);
        counters
            .outcomes
            .lock()
            .unwrap()
            .push(MutationOutcome { object_id, result });
    }

    async fn await_mutations(handles: Vec<tokio::task::JoinHandle<()>>) {
        for handle in handles {
            handle.await.unwrap();
        }
    }

    async fn replace_while_mutations_are_active(
        manager: &Arc<IndexManager>,
        counters: &MutationCounters,
        staging_baseline: publication::PublicationStagingBaseline,
        overlapping: Vec<tokio::task::JoinHandle<()>>,
    ) -> ReplacementObservation {
        let admitted_tasks_before_replacement =
            wait_for_admitted_mutation_tasks(manager, "mutation_fence_live", ATTEMPTED_MUTATIONS)
                .await;
        let active_durable_calls_at_replacement_start = counters
            .active_durable_calls
            .load(AtomicOrdering::SeqCst);
        let attempted_mutations_at_replacement_start =
            counters.attempted.load(AtomicOrdering::SeqCst);
        let control_manager = Arc::clone(manager);
        let control_write = tokio::spawn(async move {
            control_manager
                .add_documents_sync(
                    CONTROL_TENANT_ID,
                    vec![document(
                        "control_during_replace",
                        "control during replacement",
                    )],
                )
                .await
        });
        let replacement = manager
            .replace_index_contents(
                "mutation_fence_staging",
                "mutation_fence_live",
                staging_baseline,
            )
            .await
            .map(|task| task.id)
            .map_err(|error| error.to_string());
        control_write.await.unwrap().unwrap();
        await_mutations(overlapping).await;
        ReplacementObservation {
            admitted_tasks_before_replacement,
            attempted_mutations_at_replacement_start,
            active_durable_calls_at_replacement_start,
            active_durable_calls_after_replacement: counters
                .active_durable_calls
                .load(AtomicOrdering::SeqCst),
            replacement,
        }
    }

    fn assert_mutation_fence_runtime_state(manager: &IndexManager) {
        assert!(!manager.write_queues.contains_key("mutation_fence_live"));
        assert!(!manager
            .write_task_handles
            .contains_key("mutation_fence_live"));
        assert!(manager.write_queues.contains_key(CONTROL_TENANT_ID));
        assert!(manager
            .write_task_handles
            .contains_key(CONTROL_TENANT_ID));
    }

    fn assert_mutation_fence_evidence(
        manager: &IndexManager,
        counters: &MutationCounters,
        observation: ReplacementObservation,
    ) {
        let admitted_tasks = manager
            .tenant_tasks_snapshot_for_test("mutation_fence_live")
            .into_iter()
            .filter(|task| task.received_documents == 1)
            .count()
            .saturating_sub(1);
        manager.unload(&"mutation_fence_live".to_string()).unwrap();
        let outcomes = counters.outcomes.lock().unwrap();
        let acked_object_ids = outcomes
            .iter()
            .filter_map(|outcome| outcome.result.as_ref().ok().map(|_| &outcome.object_id))
            .collect::<Vec<_>>();
        let missing_acked_effects = missing_acked_effects(manager, &acked_object_ids);
        let durable_success_acks = acked_object_ids.len();

        assert_mutation_fence_denominators(
            counters,
            admitted_tasks,
            durable_success_acks,
            &observation,
        );
        assert!(
            missing_acked_effects.is_empty(),
            "replacement lost ACKed objectIDs: {missing_acked_effects:?}, outcomes={outcomes:?}"
        );
        assert_document_title(
            manager,
            "mutation_fence_live",
            "staging_seed",
            "replacement generation seed",
        );
        assert_document_title(
            manager,
            CONTROL_TENANT_ID,
            "control_seed",
            "control generation seed",
        );
        assert_document_title(
            manager,
            CONTROL_TENANT_ID,
            "control_during_replace",
            "control during replacement",
        );
    }

    fn missing_acked_effects(
        manager: &IndexManager,
        acked_object_ids: &[&String],
    ) -> Vec<String> {
        acked_object_ids
            .iter()
            .filter(|object_id| {
                manager
                    .get_document("mutation_fence_live", object_id)
                    .expect("mutation_fence lookup must succeed")
                    .and_then(|document| document.fields.get("title").cloned())
                    != Some(FieldValue::Text("acked during replacement".to_string()))
            })
            .map(|object_id| (*object_id).clone())
            .collect()
    }

    fn assert_mutation_fence_denominators(
        counters: &MutationCounters,
        admitted_tasks: usize,
        durable_success_acks: usize,
        observation: &ReplacementObservation,
    ) {
        assert_eq!(
            counters.attempted.load(AtomicOrdering::SeqCst),
            ATTEMPTED_MUTATIONS
        );
        assert_eq!(
            observation.attempted_mutations_at_replacement_start,
            ATTEMPTED_MUTATIONS
        );
        assert_eq!(
            observation.admitted_tasks_before_replacement,
            ATTEMPTED_MUTATIONS
        );
        assert!(observation.active_durable_calls_at_replacement_start > 0);
        assert!(
            observation.active_durable_calls_at_replacement_start <= OVERLAPPING_MUTATIONS
        );
        assert_eq!(observation.active_durable_calls_after_replacement, 0);
        assert_eq!(
            admitted_tasks, ATTEMPTED_MUTATIONS,
            "post-replacement task ledger must include every attempted mutation"
        );
        assert_eq!(
            durable_success_acks, ATTEMPTED_MUTATIONS,
            "this overlap fixture must produce durable success ACKs for every attempted mutation"
        );
        assert!(observation.replacement.is_ok());
    }

    fn document(object_id: &str, title: &str) -> Document {
        Document {
            id: object_id.to_string(),
            fields: HashMap::from([("title".to_string(), FieldValue::Text(title.to_string()))]),
        }
    }

    #[cfg(feature = "vector-search")]
    fn vector_document(object_id: &str, title: &str, vector: [f64; 3]) -> Document {
        let vector_values = vector
            .into_iter()
            .map(FieldValue::Float)
            .collect::<Vec<_>>();
        Document {
            id: object_id.to_string(),
            fields: HashMap::from([
                ("title".to_string(), FieldValue::Text(title.to_string())),
                (
                    "_vectors".to_string(),
                    FieldValue::Object(HashMap::from([(
                        "default".to_string(),
                        FieldValue::Array(vector_values),
                    )])),
                ),
            ]),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn durable_acks_survive_replace_index_contents_mutation_fence() {
        let (_temp_dir, manager, staging_baseline) = mutation_fence_fixture().await;
        let counters = MutationCounters::default();

        let initial = spawn_mutations(
            &manager,
            &counters,
            0..ACKED_BEFORE_REPLACEMENT,
            None,
        );
        await_mutations(initial).await;

        let start_barrier = Arc::new(Barrier::new(OVERLAPPING_MUTATIONS + 1));
        let overlapping = spawn_mutations(
            &manager,
            &counters,
            ACKED_BEFORE_REPLACEMENT..ATTEMPTED_MUTATIONS,
            Some(Arc::clone(&start_barrier)),
        );
        start_barrier.wait().await;

        let observation = replace_while_mutations_are_active(
            &manager,
            &counters,
            staging_baseline,
            overlapping,
        )
        .await;
        assert_mutation_fence_runtime_state(&manager);
        assert_mutation_fence_evidence(&manager, &counters, observation);
    }
    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_fails_before_promotion_when_destination_drain_errors() {
        assert_replacement_drain_failure_blocks_promotion(DestinationDrainFailure::WorkerErr).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_fails_before_promotion_when_destination_drain_panics() {
        assert_replacement_drain_failure_blocks_promotion(DestinationDrainFailure::JoinPanic).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_retry_waits_for_cancelled_destination_drain() {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "cancel_drain_live";
        let staging_id = "cancel_drain_staging";
        let control_id = "cancel_drain_control";
        let manager = IndexManager::new(temp_dir.path());
        create_drain_failure_fixture(&manager, destination_id, staging_id, control_id).await;
        let staging_baseline = manager
            .capture_replacement_staging_baseline(destination_id)
            .unwrap();

        let release_drain =
            replace_destination_write_handle_with_blocked_success(&manager, destination_id).await;
        let first_replacement_manager = Arc::clone(&manager);
        let first_replacement = tokio::spawn(async move {
            first_replacement_manager
                .replace_index_contents(staging_id, destination_id, staging_baseline)
                .await
        });
        wait_for_destination_queue_removed(&manager, destination_id).await;
        first_replacement.abort();
        let _ = first_replacement.await;

        let retry_manager = Arc::clone(&manager);
        let retry = tokio::spawn(async move {
            retry_manager
                .replace_index_contents(staging_id, destination_id, staging_baseline)
                .await
        });
        assert_task_stays_pending(&retry).await;
        assert_destination_not_promoted(&manager, destination_id);
        assert_control_tenant_writable(&manager, control_id).await;

        release_drain
            .send(())
            .expect("blocked destination drain should still be tracked");
        retry.await.unwrap().unwrap();
        assert_document_title(
            &manager,
            destination_id,
            "staging_seed",
            "staging replacement",
        );
    }

    async fn wait_for_admitted_mutation_tasks(
        manager: &IndexManager,
        tenant_id: &str,
        expected_mutations: usize,
    ) -> usize {
        let mut admitted_tasks = 0usize;
        for _ in 0..ADMISSION_WAIT_YIELDS {
            admitted_tasks = admitted_mutation_task_count(manager, tenant_id);
            if admitted_tasks == expected_mutations {
                return admitted_tasks;
            }
            tokio::task::yield_now().await;
        }
        admitted_tasks
    }

    fn admitted_mutation_task_count(manager: &IndexManager, tenant_id: &str) -> usize {
        manager
            .tenant_tasks_snapshot_for_test(tenant_id)
            .into_iter()
            .filter(|task| task.received_documents == 1)
            .count()
            .saturating_sub(1)
    }

    fn replacement_generation_documents() -> Vec<Document> {
        vec![document("staging_seed", "replacement generation seed")]
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_replays_destination_tail_from_destination_watermark() {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "watermark_live";
        let staging_id = "watermark_staging";
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
        // Capture the baseline while the destination holds only the snapshot; the
        // tail lands after the baseline and must be replayed into staging.
        let staging_baseline = manager
            .capture_replacement_staging_baseline(destination_id)
            .unwrap();
        manager
            .add_documents_sync(
                destination_id,
                vec![document("destination_tail", "destination drained tail")],
            )
            .await
            .unwrap();
        // Give the staging generation a deliberately different local sequence
        // history (three writes, plus a forced high committed_seq) so a promoted
        // watermark equal to W cannot be a coincidence of matching counters.
        manager
            .add_documents_sync(
                staging_id,
                vec![
                    document("staging_seed", "replacement generation seed"),
                    document("destination_snapshot", "destination snapshot"),
                    document("staging_only", "staging only history"),
                ],
            )
            .await
            .unwrap();
        crate::index::oplog::write_committed_seq(&temp_dir.path().join(staging_id), 99).unwrap();

        // The drained destination proves committed_seq = W = 2 (snapshot + tail).
        let watermark = 2;
        manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await
            .unwrap();

        assert_document_title(
            &manager,
            destination_id,
            "staging_seed",
            "replacement generation seed",
        );
        assert_document_title(
            &manager,
            destination_id,
            "destination_snapshot",
            "destination snapshot",
        );
        assert_document_title(
            &manager,
            destination_id,
            "destination_tail",
            "destination drained tail",
        );
        assert_document_title(
            &manager,
            destination_id,
            "staging_only",
            "staging only history",
        );
        assert_promoted_watermark(&manager, temp_dir.path(), destination_id, watermark);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_advances_promoted_oplog_before_post_replacement_write() {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "sequence_live";
        let staging_id = "sequence_staging";
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
        // Distinct staging-local history: three staged writes and a forced high
        // committed_seq that must not leak into the promoted destination domain.
        manager
            .add_documents_sync(
                staging_id,
                vec![
                    document("staging_seed", "replacement generation seed"),
                    document("staging_extra_a", "staging only a"),
                    document("staging_extra_b", "staging only b"),
                ],
            )
            .await
            .unwrap();
        crate::index::oplog::write_committed_seq(&temp_dir.path().join(staging_id), 88).unwrap();
        // The drained destination proves committed_seq = W = 2 (snapshot + tail).
        let watermark = 2;
        let staging_baseline = publication::PublicationStagingBaseline::new(1);

        manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await
            .unwrap();

        assert_promoted_watermark(&manager, temp_dir.path(), destination_id, watermark);
        let committed_seq =
            crate::index::oplog::read_committed_seq(&temp_dir.path().join(destination_id));
        let appended_seq = append_uncommitted_upsert(
            &manager,
            destination_id,
            document(
                "post_replacement_uncommitted",
                "post replacement crash replay",
            ),
        );
        assert!(
            appended_seq > committed_seq,
            "first post-replacement oplog seq must be above committed watermark: appended_seq={appended_seq}, committed_seq={committed_seq}"
        );
        drop(manager);

        let restarted = IndexManager::new(temp_dir.path());
        assert_document_title(
            &restarted,
            destination_id,
            "post_replacement_uncommitted",
            "post replacement crash replay",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(replacement_reopen_proof_hook)]
    async fn replace_index_contents_refuses_reopen_when_committed_journal_loses_fence_evidence() {
        assert_post_commit_reopen_proof_refusal(
            "journal_evidence_live",
            "journal_evidence_staging",
            |_, _, journal| {
                journal.fence_evidence = None;
            },
            "committed journal",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(replacement_reopen_proof_hook)]
    async fn replace_index_contents_refuses_reopen_when_durable_epoch_is_stale() {
        assert_post_commit_reopen_proof_refusal(
            "epoch_stale_live",
            "epoch_stale_staging",
            |manager, destination_id, _| {
                let target = publication::PublicationTarget::new(destination_id).unwrap();
                let epoch_path = publication::PublicationPaths::new(
                    &manager.base_path,
                    &target,
                    &publication::PublicationTransactionId::new("epoch_probe").unwrap(),
                )
                .epoch_path();
                std::fs::write(epoch_path, b"0").unwrap();
            },
            "durable publication epoch",
        )
        .await;
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(replacement_reopen_proof_hook)]
    async fn replace_index_contents_refuses_reopen_when_promoted_committed_seq_mismatches_w() {
        assert_post_commit_reopen_proof_refusal(
            "promoted_seq_live",
            "promoted_seq_staging",
            |manager, destination_id, _| {
                crate::index::oplog::write_committed_seq(
                    &manager.base_path.join(destination_id),
                    9,
                )
                .unwrap();
            },
            "promoted committed_seq",
        )
        .await;
    }

    async fn assert_post_commit_reopen_proof_refusal(
        destination_id: &'static str,
        staging_id: &'static str,
        corrupt: impl Fn(&IndexManager, &str, &mut publication::PublicationJournal)
            + Send
            + Sync
            + 'static,
        expected_message: &str,
    ) {
        let temp_dir = TempDir::new().unwrap();
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
        let staging_baseline = manager
            .capture_replacement_staging_baseline(destination_id)
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
                vec![document("staging_seed", "replacement generation seed")],
            )
            .await
            .unwrap();
        let _hook =
            IndexManager::set_replacement_reopen_proof_hook_for_test(move |manager, observed, journal| {
                if observed == destination_id {
                    corrupt(manager, observed, journal);
                }
            });

        let error = manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await
            .expect_err("replacement must fail closed before certifying reopen");
        let message = error.to_string();
        assert!(
            message.contains(expected_message),
            "wrong reopen-proof refusal for {destination_id}: {message}"
        );
    }

    #[derive(Clone, Copy)]
    enum OldDestinationSidecarCorruption {
        Missing,
        NonRegular,
        Malformed,
        NumericMismatch,
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_refuses_when_old_destination_committed_seq_missing() {
        assert_old_destination_sidecar_refusal(OldDestinationSidecarCorruption::Missing).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_refuses_when_old_destination_committed_seq_non_regular() {
        assert_old_destination_sidecar_refusal(OldDestinationSidecarCorruption::NonRegular).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_refuses_when_old_destination_committed_seq_malformed() {
        assert_old_destination_sidecar_refusal(OldDestinationSidecarCorruption::Malformed).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_refuses_when_old_destination_committed_seq_mismatches_w() {
        assert_old_destination_sidecar_refusal(OldDestinationSidecarCorruption::NumericMismatch)
            .await;
    }

    /// Prove replacement refuses before any live-target mutation when the drained
    /// old destination `committed_seq` sidecar is not exactly the strict `W`. The
    /// already-durable epoch advance is expected and is not content promotion.
    async fn assert_old_destination_sidecar_refusal(corruption: OldDestinationSidecarCorruption) {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "old_sidecar_live";
        let staging_id = "old_sidecar_staging";
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
        let staging_baseline = manager
            .capture_replacement_staging_baseline(destination_id)
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
                vec![document("staging_seed", "replacement generation seed")],
            )
            .await
            .unwrap();

        // W = 2 (snapshot + tail). Corrupt the OLD destination committed_seq sidecar
        // so it can no longer prove exactly W.
        let committed_path = temp_dir
            .path()
            .join(destination_id)
            .join(crate::index::oplog::COMMITTED_SEQ_FILE);
        match corruption {
            OldDestinationSidecarCorruption::Missing => {
                std::fs::remove_file(&committed_path).unwrap();
            }
            OldDestinationSidecarCorruption::NonRegular => {
                std::fs::remove_file(&committed_path).unwrap();
                std::fs::create_dir(&committed_path).unwrap();
            }
            OldDestinationSidecarCorruption::Malformed => {
                std::fs::write(&committed_path, b"not-a-sequence").unwrap();
            }
            OldDestinationSidecarCorruption::NumericMismatch => {
                std::fs::write(&committed_path, b"5").unwrap();
            }
        }

        let error = manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await
            .expect_err("replacement must refuse when old destination committed_seq is not strict W");
        let message = error.to_string();
        assert!(
            message.contains("committed_seq") || message.contains("watermark"),
            "unexpected refusal message: {message}"
        );
        assert_replacement_refused_before_promotion(
            &manager,
            destination_id,
            "destination_snapshot",
            "destination snapshot",
            "staging_seed",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn replace_index_contents_refuses_when_retained_delta_has_a_gap() {
        let temp_dir = TempDir::new().unwrap();
        let destination_id = "gap_live";
        let staging_id = "gap_staging";
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant(destination_id).unwrap();
        manager.create_tenant(staging_id).unwrap();
        // Three committed destination writes → committed_seq = W = 3, oplog 1..3.
        manager
            .add_documents_sync(destination_id, vec![document("d1", "one")])
            .await
            .unwrap();
        manager
            .add_documents_sync(destination_id, vec![document("d2", "two")])
            .await
            .unwrap();
        manager
            .add_documents_sync(destination_id, vec![document("d3", "three")])
            .await
            .unwrap();
        manager
            .add_documents_sync(
                staging_id,
                vec![document("staging_seed", "replacement generation seed")],
            )
            .await
            .unwrap();

        // Remove the middle oplog entry (seq 2) so the retained delta over
        // (baseline 0, W 3] is non-contiguous and must refuse rather than certify W.
        remove_oplog_entry(temp_dir.path(), destination_id, 2);

        let staging_baseline = publication::PublicationStagingBaseline::new(0);
        let error = manager
            .replace_index_contents(staging_id, destination_id, staging_baseline)
            .await
            .expect_err("gapped retained delta must refuse rather than certify W");
        assert!(
            error.to_string().contains("retained destination delta"),
            "unexpected refusal message: {error}"
        );
        assert_replacement_refused_before_promotion(
            &manager,
            destination_id,
            "d3",
            "three",
            "staging_seed",
        );
    }
