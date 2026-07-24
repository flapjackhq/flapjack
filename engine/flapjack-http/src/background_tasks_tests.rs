    use super::{
        autoheal_enabled_from_env, completed_utc_day, extract_s3_snapshot_tenant_id,
        migration_spool_gc_interval_secs, parse_autoheal_enabled, rollup_window_bounds_ms,
        run_migration_spool_gc_loop, run_usage_rollover, HOUR_MS, AUTOHEAL_ENABLED_ENV,
        MIGRATION_SPOOL_GC_INTERVAL_ENV,
    };
    use crate::handlers::migration::spool::{
        MigrationDisposition, ResourceDenominators, SpoolLimits, SpoolStore,
    };
    use crate::test_helpers::{restore_env_var, with_env_var, TestStateBuilder, ENV_MUTEX};
    use crate::usage_persistence::UsagePersistence;
    use chrono::TimeZone;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;
    use tokio::time::{timeout, Duration};

    #[test]
    fn completed_utc_day_returns_prior_day_at_and_after_midnight() {
        let midnight = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 0).unwrap();
        assert_eq!(completed_utc_day(midnight), "2026-07-19");

        let one_second_after = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 1).unwrap();
        assert_eq!(completed_utc_day(one_second_after), "2026-07-19");

        let just_before_midnight = chrono::Utc
            .with_ymd_and_hms(2026, 7, 19, 23, 59, 59)
            .unwrap();
        assert_eq!(completed_utc_day(just_before_midnight), "2026-07-18");
    }

    #[test]
    fn s3_snapshot_tenant_prefix_rejects_path_traversal_components() {
        assert_eq!(
            extract_s3_snapshot_tenant_id("snapshots/products/"),
            Some("products".to_string())
        );
        assert_eq!(
            extract_s3_snapshot_tenant_id("snapshots/products_v2-2026/"),
            Some("products_v2-2026".to_string())
        );

        for prefix in [
            "snapshots/../",
            "snapshots/./",
            "snapshots//",
            "snapshots/nested/index/",
            "snapshots\\windows\\",
            "not-snapshots/products/",
        ] {
            assert_eq!(
                extract_s3_snapshot_tenant_id(prefix),
                None,
                "{prefix} must not become a local restore path component"
            );
        }
    }

    #[test]
    fn migration_spool_gc_interval_uses_default_when_absent() {
        let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
        let previous = std::env::var_os(MIGRATION_SPOOL_GC_INTERVAL_ENV);
        std::env::remove_var(MIGRATION_SPOOL_GC_INTERVAL_ENV);

        assert_eq!(migration_spool_gc_interval_secs(), 300);

        restore_env_var(MIGRATION_SPOOL_GC_INTERVAL_ENV, previous);
    }

    #[test]
    fn migration_spool_gc_interval_uses_default_for_invalid_text() {
        let _guard = with_env_var(MIGRATION_SPOOL_GC_INTERVAL_ENV, "not-a-number");

        assert_eq!(migration_spool_gc_interval_secs(), 300);
    }

    #[test]
    fn migration_spool_gc_interval_uses_default_for_zero() {
        let _guard = with_env_var(MIGRATION_SPOOL_GC_INTERVAL_ENV, "0");

        assert_eq!(migration_spool_gc_interval_secs(), 300);
    }

    #[test]
    fn migration_spool_gc_interval_preserves_positive_integer() {
        let _guard = with_env_var(MIGRATION_SPOOL_GC_INTERVAL_ENV, "42");

        assert_eq!(migration_spool_gc_interval_secs(), 42);
    }

    #[test]
    fn autoheal_enabled_parser_defaults_false_when_absent() {
        assert_eq!(parse_autoheal_enabled(None), Ok(false));
    }

    #[test]
    fn autoheal_enabled_parser_accepts_trimmed_ascii_case_insensitive_values() {
        for value in ["false", " FALSE ", "FaLsE", "\tfalse\n"] {
            assert_eq!(parse_autoheal_enabled(Some(value)), Ok(false));
        }
        for value in ["true", " TRUE ", "TrUe", "\ttrue\n"] {
            assert_eq!(parse_autoheal_enabled(Some(value)), Ok(true));
        }
    }

    #[test]
    fn autoheal_enabled_parser_rejects_invalid_values() {
        for value in ["", "1", "0", "yes", "enabled", "true false"] {
            assert_eq!(
                parse_autoheal_enabled(Some(value)),
                Err(value.to_string()),
                "{value:?} must not be accepted as an auto-heal boolean"
            );
        }
    }

    #[test]
    fn autoheal_enabled_env_reader_uses_parser_for_true_and_invalid_values() {
        {
            let _guard = with_env_var(AUTOHEAL_ENABLED_ENV, " TRUE ");
            assert!(autoheal_enabled_from_env());
        }
        {
            let _guard = with_env_var(AUTOHEAL_ENABLED_ENV, "1");
            assert!(!autoheal_enabled_from_env());
        }
    }

    #[tokio::test]
    async fn migration_spool_gc_loop_reclaims_payloads_after_delayed_first_tick() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let fixture_store = expired_fixture_store(&state);
        let job = seed_expired_gc_job(
            &fixture_store,
            uuid::Uuid::from_u128(0x30000000000000000000000000000001),
        );
        let task_store = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
        let task = tokio::spawn(run_migration_spool_gc_loop(
            Duration::from_millis(80),
            move || {
                let task_store = task_store.clone();
                async move { task_store.collect_garbage() }
            },
        ));

        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_payload_files_exist(&job.payload_paths);

        timeout(Duration::from_secs(2), async {
            loop {
                if job.payload_paths.iter().all(|path| !path.exists()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("migration spool GC loop should reclaim eligible payloads");

        assert_gc_job_reclaimed(&fixture_store, &job);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn migration_spool_gc_loop_continues_after_pass_error() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let success = Arc::new(Notify::new());
        let task = tokio::spawn(run_migration_spool_gc_loop(Duration::from_millis(10), {
            let attempts = Arc::clone(&attempts);
            let success = Arc::clone(&success);
            move || {
                let attempts = Arc::clone(&attempts);
                let success = Arc::clone(&success);
                async move {
                    if attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                        Err("first pass failed")
                    } else {
                        success.notify_one();
                        Ok(())
                    }
                }
            }
        }));

        timeout(Duration::from_secs(1), success.notified())
            .await
            .expect("loop should run a later pass after a pass-level failure");
        assert!(
            attempts.load(Ordering::SeqCst) >= 2,
            "loop must attempt at least one retry after the first error"
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn migration_spool_gc_loop_preserves_per_job_isolation() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let fixture_store = expired_fixture_store(&state);
        let malformed = seed_expired_gc_job(
            &fixture_store,
            uuid::Uuid::from_u128(0x30000000000000000000000000000002),
        );
        let eligible = seed_expired_gc_job(
            &fixture_store,
            uuid::Uuid::from_u128(0x30000000000000000000000000000003),
        );
        std::fs::write(&malformed.phase_path, b"not-json").unwrap();
        assert_payload_files_exist(&malformed.payload_paths);
        assert_payload_files_exist(&eligible.payload_paths);

        let task_store = SpoolStore::new(&state.manager.base_path, SpoolLimits::default()).unwrap();
        let task = tokio::spawn(run_migration_spool_gc_loop(
            Duration::from_millis(10),
            move || {
                let task_store = task_store.clone();
                async move { task_store.collect_garbage() }
            },
        ));

        timeout(Duration::from_secs(2), async {
            loop {
                if eligible.payload_paths.iter().all(|path| !path.exists()) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("later eligible job should be reclaimed despite malformed earlier job");

        assert_payload_files_exist(&malformed.payload_paths);
        assert_gc_job_reclaimed(&fixture_store, &eligible);
        task.abort();
        let _ = task.await;
    }

    #[derive(Debug)]
    struct GcJobFixture {
        job_uuid: uuid::Uuid,
        payload_paths: Vec<PathBuf>,
        reclaimable_bytes: u64,
        phase_path: PathBuf,
        phase_bytes: Vec<u8>,
        async_metadata_path: PathBuf,
        async_metadata_bytes: Vec<u8>,
    }

    fn expired_fixture_store(state: &Arc<crate::handlers::AppState>) -> SpoolStore {
        let limits = SpoolLimits::default();
        let terminal_now =
            chrono::Utc::now() - chrono::Duration::seconds(limits.retention_seconds + 60);
        SpoolStore::new_for_tests(
            &state.manager.base_path,
            limits,
            terminal_now,
            limits.minimum_free_bytes + 1_000_000,
        )
        .expect("expired fixture store should initialize")
    }

    fn seed_expired_gc_job(store: &SpoolStore, job_uuid: uuid::Uuid) -> GcJobFixture {
        store
            .create_async_migration_admission(job_uuid, "target-index")
            .unwrap();
        store
            .create_export(
                job_uuid,
                "6f757263652d6964656e74697479000000000000000000000000000000000000",
                ResourceDenominators {
                    settings: 1,
                    documents: 1,
                    rules: 1,
                    synonyms: 1,
                    config: 0,
                },
            )
            .unwrap();
        store
            .commit_settings(job_uuid, br#"{"ranking":["typo"]}"#, 1)
            .unwrap();
        store
            .commit_document_page_with_ids(job_uuid, br#"[{"objectID":"doc-1"}]"#, &["doc-1"])
            .unwrap();
        store
            .commit_rule_page_with_ids(job_uuid, br#"[{"objectID":"rule-1"}]"#, &["rule-1"])
            .unwrap();
        store
            .commit_synonym_page_with_ids(job_uuid, br#"[{"objectID":"syn-1"}]"#, &["syn-1"])
            .unwrap();
        store.fail_migration(job_uuid).unwrap();

        let payload_paths = payload_file_paths(store, job_uuid);
        assert_payload_files_exist(&payload_paths);
        let reclaimable_bytes = payload_paths
            .iter()
            .map(|path| std::fs::metadata(path).unwrap().len())
            .sum::<u64>();
        assert!(
            reclaimable_bytes > 0,
            "fixture must contain nonzero reclaimable payload bytes"
        );

        GcJobFixture {
            job_uuid,
            payload_paths,
            reclaimable_bytes,
            phase_path: store.job_dir(job_uuid).join("migration_phase.json"),
            phase_bytes: std::fs::read(store.job_dir(job_uuid).join("migration_phase.json"))
                .unwrap(),
            async_metadata_path: store.async_migration_metadata_path(job_uuid),
            async_metadata_bytes: std::fs::read(store.async_migration_metadata_path(job_uuid))
                .unwrap(),
        }
    }

    fn payload_file_paths(store: &SpoolStore, job_uuid: uuid::Uuid) -> Vec<PathBuf> {
        let mut paths = std::fs::read_dir(store.job_dir(job_uuid))
            .unwrap()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().unwrap().is_file())
            .filter(|entry| is_payload_file(&entry.file_name().to_string_lossy()))
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    fn is_payload_file(file_name: &str) -> bool {
        !matches!(
            file_name,
            "manifest.json" | "migration_phase.json" | "async_migration.json" | "tombstone.json"
        ) && !file_name.starts_with('.')
    }

    fn assert_payload_files_exist(payload_paths: &[PathBuf]) {
        assert!(
            !payload_paths.is_empty(),
            "fixture must name at least one payload file"
        );
        for path in payload_paths {
            assert!(
                path.exists(),
                "payload path should exist before reclamation: {}",
                path.display()
            );
            assert!(
                std::fs::metadata(path).unwrap().len() > 0,
                "payload path should contain bytes before reclamation: {}",
                path.display()
            );
        }
    }

    fn assert_gc_job_reclaimed(store: &SpoolStore, job: &GcJobFixture) {
        assert!(
            job.reclaimable_bytes > 0,
            "fixture must prove nonzero reclaimed bytes"
        );
        for path in &job.payload_paths {
            assert!(
                !path.exists(),
                "eligible payload path should be deleted: {}",
                path.display()
            );
        }
        assert_eq!(std::fs::read(&job.phase_path).unwrap(), job.phase_bytes);
        assert_eq!(
            std::fs::read(&job.async_metadata_path).unwrap(),
            job.async_metadata_bytes
        );
        assert_eq!(
            store
                .read_migration_phase(job.job_uuid)
                .unwrap()
                .disposition,
            MigrationDisposition::Failed
        );
        assert_eq!(
            store
                .read_async_migration_metadata(job.job_uuid)
                .unwrap()
                .job_uuid,
            job.job_uuid
        );

        let manifest: serde_json::Value =
            serde_json::from_str(&store.manifest_json(job.job_uuid).unwrap()).unwrap();
        assert_eq!(manifest["bytes_committed"], 0);
        assert_eq!(manifest["artifacts"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn one_shot_rollover_persists_completed_day_and_resets_counters() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let gauges = state.metrics_state.as_ref().unwrap().storage_gauges.clone();

        // Seed all seven counter fields with distinct non-zero values.
        {
            let entry = state
                .usage_counters
                .entry("products".to_string())
                .or_default();
            entry.search_count.store(11, Ordering::Relaxed);
            entry.write_count.store(22, Ordering::Relaxed);
            entry.read_count.store(33, Ordering::Relaxed);
            entry.bytes_in.store(44, Ordering::Relaxed);
            entry.search_results_total.store(55, Ordering::Relaxed);
            entry.documents_indexed_total.store(66, Ordering::Relaxed);
            entry.documents_deleted_total.store(77, Ordering::Relaxed);
        }

        // Wake just after midnight: the just-completed day is 2026-07-19.
        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 5).unwrap();
        let completed_day = run_usage_rollover(
            now,
            &persistence,
            &state.usage_counters,
            &state.manager,
            Some(&gauges),
        )
        .unwrap();

        assert_eq!(completed_day, "2026-07-19");
        assert!(
            tmp.path().join("_usage/2026-07-19.json").exists(),
            "completed-day snapshot must be written"
        );
        assert!(
            !tmp.path().join("_usage/2026-07-20.json").exists(),
            "the newly-started day must not be persisted"
        );

        // Persisted snapshot preserves the exact seeded counter values.
        let snapshot = persistence
            .load_snapshot("2026-07-19")
            .unwrap()
            .expect("completed-day snapshot should load");
        let products = &snapshot.indexes["products"];
        assert_eq!(products.search_operations, 11);
        assert_eq!(products.total_write_operations, 22);
        assert_eq!(products.total_read_operations, 33);
        assert_eq!(products.bytes_received, 44);
        assert_eq!(products.search_results_total, 55);
        assert_eq!(products.records, 66);
        assert_eq!(products.documents_deleted, 77);

        // Live atomics are reset to zero after the helper returns.
        let entry = state.usage_counters.get("products").unwrap();
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 0);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn one_shot_rollover_unions_gauges_and_preserves_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let gauges = state.metrics_state.as_ref().unwrap().storage_gauges.clone();
        gauges.clear();

        // "products": loaded with 3 documents and a storage gauge.
        state.manager.create_tenant("products").unwrap();
        state
            .manager
            .add_documents_sync(
                "products",
                (0..3u64)
                    .map(|i| flapjack::types::Document {
                        id: format!("products_{i}"),
                        fields: std::collections::HashMap::new(),
                    })
                    .collect(),
            )
            .await
            .unwrap();
        gauges.insert("products".to_string(), 12_345);

        // "storage_only": gauge-only index, not loaded — unions into the snapshot.
        gauges.insert("storage_only".to_string(), 4_096);

        // "empty": explicit-zero storage gauge, not loaded — Some(0) must survive.
        gauges.insert("empty".to_string(), 0);

        // "counter_only": counter-backed, not loaded, no gauge — gauges stay None.
        {
            let entry = state
                .usage_counters
                .entry("counter_only".to_string())
                .or_default();
            entry.search_count.store(11, Ordering::Relaxed);
            entry.write_count.store(22, Ordering::Relaxed);
            entry.read_count.store(33, Ordering::Relaxed);
            entry.bytes_in.store(44, Ordering::Relaxed);
            entry.search_results_total.store(55, Ordering::Relaxed);
            entry.documents_indexed_total.store(66, Ordering::Relaxed);
            entry.documents_deleted_total.store(77, Ordering::Relaxed);
        }

        let now = chrono::Utc.with_ymd_and_hms(2026, 7, 20, 0, 0, 5).unwrap();
        run_usage_rollover(
            now,
            &persistence,
            &state.usage_counters,
            &state.manager,
            Some(&gauges),
        )
        .unwrap();

        let snapshot = persistence
            .load_snapshot("2026-07-19")
            .unwrap()
            .expect("completed-day snapshot should load");

        // Union of counter-backed and gauge-only indexes.
        let mut names: Vec<_> = snapshot.indexes.keys().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            vec!["counter_only", "empty", "products", "storage_only"]
        );

        let products = &snapshot.indexes["products"];
        assert_eq!(products.documents_count, Some(3));
        assert_eq!(products.storage_bytes, Some(12_345));

        let storage_only = &snapshot.indexes["storage_only"];
        assert_eq!(storage_only.documents_count, None);
        assert_eq!(storage_only.storage_bytes, Some(4_096));

        let empty = &snapshot.indexes["empty"];
        assert_eq!(empty.documents_count, None);
        assert_eq!(empty.storage_bytes, Some(0));

        let counter_only = &snapshot.indexes["counter_only"];
        assert_eq!(counter_only.documents_count, None);
        assert_eq!(counter_only.storage_bytes, None);
        assert_eq!(counter_only.search_operations, 11);

        // The captured gauge source is not mutated by rollover.
        assert_eq!(gauges.len(), 3);
        assert_eq!(*gauges.get("products").unwrap().value(), 12_345);
        assert_eq!(*gauges.get("storage_only").unwrap().value(), 4_096);
        assert_eq!(*gauges.get("empty").unwrap().value(), 0);

        // Only the seven usage counter atomics are reset.
        let entry = state.usage_counters.get("counter_only").unwrap();
        assert_eq!(entry.search_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.write_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.read_count.load(Ordering::Relaxed), 0);
        assert_eq!(entry.bytes_in.load(Ordering::Relaxed), 0);
        assert_eq!(entry.search_results_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_indexed_total.load(Ordering::Relaxed), 0);
        assert_eq!(entry.documents_deleted_total.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn rollup_window_targets_last_completed_hour() {
        let now_ms = (10 * HOUR_MS) + 123;
        let (start_ms, end_ms) = rollup_window_bounds_ms(now_ms);
        assert_eq!(start_ms, 9 * HOUR_MS);
        assert_eq!(end_ms, 10 * HOUR_MS);
    }

    #[test]
    fn rollup_window_uses_completed_override_window_when_override_is_valid() {
        let _guard = with_env_var("FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS", "60000");
        let now_ms = (10 * HOUR_MS) + (2 * 60_000) + 12_345;
        let (start_ms, end_ms) = rollup_window_bounds_ms(now_ms);
        assert_eq!(start_ms, (10 * HOUR_MS) + 60_000);
        assert_eq!(end_ms, (10 * HOUR_MS) + (2 * 60_000));
    }

    #[test]
    fn rollup_window_falls_back_to_hour_bounds_when_override_is_invalid() {
        let now_ms = (10 * HOUR_MS) + 123;
        for invalid_override in ["not-a-number", "0", "-60000"] {
            let _guard = with_env_var("FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS", invalid_override);
            let (start_ms, end_ms) = rollup_window_bounds_ms(now_ms);
            assert_eq!(start_ms, 9 * HOUR_MS);
            assert_eq!(end_ms, 10 * HOUR_MS);
        }
    }
