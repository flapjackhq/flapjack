    use super::{
        autoheal_enabled_from_env, completed_utc_day, enforce_backup_retention,
        extract_s3_snapshot_tenant_id, migration_spool_gc_interval_secs, parse_autoheal_enabled,
        positive_interval_secs, rollup_window_bounds_ms, run_migration_spool_gc_loop,
        run_analytics_retention_pass_if_admitted, run_background_mutation_if_admitted,
        run_storage_maintenance_pass, run_usage_rollover, spawn_storage_maintenance_task,
        spawn_metrics_refresh_task, spawn_supervised, BackgroundTaskHealth, BackgroundTaskIntervals, HOUR_MS,
        AUTOHEAL_ENABLED_ENV,
        MIGRATION_SPOOL_GC_INTERVAL_ENV, ROLLUP_INTERVAL_ENV, SYNC_INTERVAL_ENV,
    };
    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use crate::handlers::migration::spool::{
        AsyncMigrationPublicationSemantic, MigrationDisposition, ResourceDenominators, SpoolLimits,
        SpoolStore,
    };
    use crate::test_helpers::{
        restore_env_var, with_env_var, EnvVarRestoreGuard, SharedLogBuffer, TestStateBuilder,
        ENV_MUTEX,
    };
    use crate::usage_persistence::UsagePersistence;
    use chrono::TimeZone;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::sync::Notify;
    use tokio::time::{timeout, Duration};
    use tower::ServiceExt;
    use tracing_subscriber::prelude::*;
    use wiremock::{matchers::method, Mock, MockServer, ResponseTemplate};

    async fn metrics_text(state: Arc<crate::handlers::AppState>) -> String {
        let app = Router::new()
            .route("/metrics", get(crate::handlers::metrics::metrics_handler))
            .with_state(state);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    fn metric_values(text: &str, metric_name: &str) -> std::collections::BTreeMap<String, f64> {
        let metric_prefix = format!("{metric_name}{{");
        text.lines()
            .filter(|line| line.starts_with(&metric_prefix))
            .map(|line| {
                let index_start = line.find("index=\"").expect("index label") + 7;
                let remainder = &line[index_start..];
                let index_end = remainder.find('"').expect("closing index label");
                let value = line
                    .split_whitespace()
                    .last()
                    .expect("metric value")
                    .parse()
                    .expect("numeric metric value");
                (remainder[..index_end].to_string(), value)
            })
            .collect()
    }

    #[test]
    fn metric_values_require_exact_metric_name() {
        let text = concat!(
            "flapjack_storage_bytes{index=\"products\"} 7\n",
            "flapjack_storage_bytes_suffix{index=\"products\"} 91\n",
        );

        assert_eq!(
            metric_values(text, "flapjack_storage_bytes"),
            std::collections::BTreeMap::from([("products".to_string(), 7.0)]),
            "a similarly named metric must not overwrite the real series"
        );
    }

    async fn let_metrics_refresh_run() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    fn document(id: &str) -> flapjack::types::Document {
        flapjack::types::Document {
            id: id.to_string(),
            fields: std::collections::HashMap::new(),
        }
    }

    /// RED 2: the periodic refresh must enumerate a committed index that is
    /// not loaded in the refreshing manager, without changing loaded_count.
    #[tokio::test(start_paused = true)]
    async fn metrics_refresh_includes_durable_unloaded_index_without_loading() {
        let tmp = tempfile::TempDir::new().unwrap();
        let creator = flapjack::IndexManager::new(tmp.path());
        creator.create_tenant("durable-unloaded").unwrap();
        creator
            .add_documents_sync("durable-unloaded", vec![document("d1"), document("d2")])
            .await
            .unwrap();

        let state = TestStateBuilder::new(&tmp).build_shared();
        assert_eq!(state.manager.loaded_count(), 0);
        spawn_metrics_refresh_task(&state);
        let_metrics_refresh_run().await;

        let text = metrics_text(Arc::clone(&state)).await;
        let storage = metric_values(&text, "flapjack_storage_bytes");
        let documents = metric_values(&text, "flapjack_documents_count");
        assert!(
            storage
                .get("durable-unloaded")
                .is_some_and(|bytes| *bytes > 0.0),
            "durable unloaded storage was omitted: {storage:?}"
        );
        assert_eq!(documents.get("durable-unloaded"), Some(&2.0));
        assert_eq!(
            state.manager.loaded_count(),
            0,
            "metrics refresh must not recover or load the durable index"
        );
    }

    /// RED 4: loaded, empty, and restarted indexes retain explicit gauge
    /// semantics, while an authoritative deletion removes both labels.
    #[tokio::test(start_paused = true)]
    async fn metrics_refresh_preserves_lifecycle_states_and_removes_deleted_labels() {
        let tmp = tempfile::TempDir::new().unwrap();
        let initial = TestStateBuilder::new(&tmp).build_shared();
        initial.manager.create_tenant("loaded").unwrap();
        initial
            .manager
            .add_documents_sync("loaded", vec![document("d1")])
            .await
            .unwrap();
        initial.manager.create_tenant("empty").unwrap();
        spawn_metrics_refresh_task(&initial);
        let_metrics_refresh_run().await;

        let before_restart = metrics_text(Arc::clone(&initial)).await;
        assert_eq!(
            metric_values(&before_restart, "flapjack_documents_count").get("loaded"),
            Some(&1.0)
        );
        assert_eq!(
            metric_values(&before_restart, "flapjack_documents_count").get("empty"),
            Some(&0.0)
        );

        let restarted = TestStateBuilder::new(&tmp).build_shared();
        assert_eq!(restarted.manager.loaded_count(), 0);
        spawn_metrics_refresh_task(&restarted);
        let_metrics_refresh_run().await;

        let after_restart = metrics_text(Arc::clone(&restarted)).await;
        let restarted_documents = metric_values(&after_restart, "flapjack_documents_count");
        assert_eq!(restarted_documents.get("loaded"), Some(&1.0));
        assert_eq!(restarted_documents.get("empty"), Some(&0.0));
        assert_eq!(restarted.manager.loaded_count(), 0);

        initial
            .manager
            .delete_tenant(&"loaded".to_string())
            .await
            .unwrap();
        tokio::time::advance(Duration::from_secs(60)).await;
        let_metrics_refresh_run().await;

        let after_delete = metrics_text(Arc::clone(&restarted)).await;
        for metric_name in ["flapjack_storage_bytes", "flapjack_documents_count"] {
            let values = metric_values(&after_delete, metric_name);
            assert!(
                !values.contains_key("loaded"),
                "deleted label remained in {metric_name}: {values:?}"
            );
            assert!(
                values.contains_key("empty"),
                "surviving empty index disappeared from {metric_name}: {values:?}"
            );
        }
    }

    /// RED 5: a transient directory-read failure must retain the last known
    /// storage value rather than replacing it with a billable zero.
    #[cfg(unix)]
    #[tokio::test(start_paused = true)]
    async fn metrics_refresh_retains_last_known_storage_on_measurement_failure() {
        use std::os::unix::fs::PermissionsExt;

        struct PermissionRestore {
            path: PathBuf,
            mode: u32,
        }
        impl Drop for PermissionRestore {
            fn drop(&mut self) {
                let _ = std::fs::set_permissions(
                    &self.path,
                    std::fs::Permissions::from_mode(self.mode),
                );
            }
        }

        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        state.manager.create_tenant("storage-fault").unwrap();
        state
            .manager
            .add_documents_sync("storage-fault", vec![document("d1")])
            .await
            .unwrap();
        spawn_metrics_refresh_task(&state);
        let_metrics_refresh_run().await;

        let metrics_state = state.metrics_state.as_ref().unwrap();
        let last_known = metrics_state
            .index_gauge_snapshot()
            .get("storage-fault")
            .expect("initial refresh must publish storage")
            .storage_bytes
            .expect("initial refresh must measure storage");
        assert!(last_known > 0);

        let index_path = tmp.path().join("storage-fault");
        let original_mode = std::fs::metadata(&index_path).unwrap().permissions().mode();
        let _restore = PermissionRestore {
            path: index_path.clone(),
            mode: original_mode,
        };
        std::fs::set_permissions(&index_path, std::fs::Permissions::from_mode(0o0)).unwrap();
        assert_eq!(
            state.manager.tenant_storage_bytes("storage-fault"),
            0,
            "fault fixture must exercise the current swallowed-error path"
        );

        tokio::time::advance(Duration::from_secs(60)).await;
        let_metrics_refresh_run().await;
        let after_error = metrics_state
            .index_gauge_snapshot()
            .get("storage-fault")
            .expect("transient measurement failure must not remove the known index")
            .storage_bytes
            .expect("last known storage must remain available");
        assert_eq!(after_error, last_known);
    }

    #[test]
    fn production_autoheal_registration_uses_complete_pass_mutation_admission() {
        let source = include_str!("background_tasks.rs");
        assert!(
            source.contains("start_health_probe_with_admission(10, autoheal_enabled"),
            "production autoheal registration must use the admission-aware manager owner"
        );
        assert!(
            source.contains("mutation_fence.admit_mutation().await.ok()"),
            "the complete autoheal pass must retain the existing global mutation permit"
        );
    }

    #[tokio::test]
    async fn active_startup_fence_defers_migration_spool_layout_until_release() {
        let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
        let _interval = EnvVarRestoreGuard::set(MIGRATION_SPOOL_GC_INTERVAL_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let state = Arc::new(TestStateBuilder::new(&temp).build());
        state
            .global_mutation_fence
            .acquire("release-spool-startup-1")
            .await
            .unwrap();

        let _registration = spawn_storage_maintenance_task(&state);
        tokio::time::sleep(Duration::from_millis(1_150)).await;
        assert!(
            !temp.path().join("migration_exports").exists(),
            "active fenced startup must not create the spool layout"
        );

        state
            .global_mutation_fence
            .release("release-spool-startup-1")
            .await
            .unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                if temp.path().join("migration_exports/jobs").is_dir() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the first admitted maintenance pass should initialize the spool");
    }

    #[tokio::test]
    async fn background_mutation_permit_lives_until_the_async_effect_finishes() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        std::fs::create_dir_all(&data_root).unwrap();
        let fence = crate::pause_registry::GlobalMutationFence::open(&data_root).unwrap();
        let pass_started = Arc::new(Notify::new());
        let finish_pass = Arc::new(Notify::new());

        let pass_fence = fence.clone();
        let started = Arc::clone(&pass_started);
        let finish = Arc::clone(&finish_pass);
        let pass = tokio::spawn(async move {
            run_background_mutation_if_admitted(&pass_fence, || async move {
                started.notify_one();
                finish.notified().await;
            })
            .await
        });
        pass_started.notified().await;

        let acquire_fence = fence.clone();
        let acquire = tokio::spawn(async move { acquire_fence.acquire("analytics-drain-1").await });
        tokio::task::yield_now().await;
        assert!(
            !acquire.is_finished(),
            "the release fence must wait for an admitted analytics write to finish"
        );

        finish_pass.notify_one();
        assert!(pass.await.unwrap());
        acquire.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn persisted_release_fence_suppresses_startup_analytics_retention() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_root = temp.path().join("flapjack/data");
        let analytics_root = temp.path().join("analytics");
        let expired = analytics_root
            .join("products/events/date=2000-01-01")
            .join("events.parquet");
        std::fs::create_dir_all(&data_root).unwrap();
        std::fs::create_dir_all(expired.parent().unwrap()).unwrap();
        std::fs::write(&expired, b"expired analytics").unwrap();

        let fence = crate::pause_registry::GlobalMutationFence::open(&data_root).unwrap();
        fence.acquire("analytics-retention-1").await.unwrap();
        assert!(
            !run_analytics_retention_pass_if_admitted(
                &fence,
                analytics_root.as_path(),
                30,
                "Startup",
            )
            .await
        );
        assert!(
            expired.exists(),
            "startup retention must not mutate while a persisted release fence is active"
        );

        fence.release("analytics-retention-1").await.unwrap();
        assert!(
            run_analytics_retention_pass_if_admitted(
                &fence,
                analytics_root.as_path(),
                30,
                "Startup",
            )
            .await
        );
        assert!(!expired.exists());
    }

    #[test]
    fn background_interval_parser_rejects_zero_and_invalid_values() {
        assert_eq!(positive_interval_secs("rollup", None, 300), Ok(300));
        assert_eq!(positive_interval_secs("rollup", Some("42"), 300), Ok(42));
        assert_eq!(
            positive_interval_secs("rollup", Some("0"), 300),
            Err("rollup must be a positive integer, got \"0\"".to_string())
        );
        assert_eq!(
            positive_interval_secs("sync", Some("not-a-number"), 60),
            Err("sync must be a positive integer, got \"not-a-number\"".to_string())
        );
    }

    #[test]
    fn background_interval_config_rejects_zero_before_task_admission() {
        let _env = ENV_MUTEX.lock().expect("env mutex should lock");
        let _rollup = EnvVarRestoreGuard::set(ROLLUP_INTERVAL_ENV, "0");
        let _sync = EnvVarRestoreGuard::set(SYNC_INTERVAL_ENV, "60");

        assert_eq!(
            BackgroundTaskIntervals::from_env().unwrap_err(),
            "FLAPJACK_ROLLUP_INTERVAL_SECS must be a positive integer, got \"0\""
        );
    }

    #[tokio::test(start_paused = true)]
    async fn storage_maintenance_registration_runs_under_production_supervision() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();

        let _registration = spawn_storage_maintenance_task(&state);

        assert_eq!(
            state
                .background_task_health
                .tasks
                .lock()
                .unwrap()
                .get("storage-maintenance"),
            Some(&true),
            "the production registration boundary must supervise storage maintenance"
        );
    }

    #[tokio::test]
    async fn supervised_task_exit_changes_liveness_state() {
        let health = Arc::new(BackgroundTaskHealth::default());
        spawn_supervised(Arc::clone(&health), "test-loop", async {});

        timeout(Duration::from_secs(1), async {
            loop {
                if health.failed_tasks() == vec!["test-loop".to_string()] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a completed supervised task must become unhealthy promptly");
    }

    #[tokio::test]
    async fn supervised_task_panic_changes_liveness_state() {
        let health = Arc::new(BackgroundTaskHealth::default());
        spawn_supervised(Arc::clone(&health), "panicking-loop", async {
            panic!("representative worker failure")
        });

        timeout(Duration::from_secs(1), async {
            loop {
                if health.failed_tasks() == vec!["panicking-loop".to_string()] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("a panicking supervised task must become unhealthy promptly");
    }

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

    /// TODO: Document s3_snapshot_tenant_prefix_rejects_path_traversal_components.
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

    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn enforce_backup_retention_logs_delete_failure_with_tenant() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>snapshot-bucket</Name>
  <Prefix>snapshots/products/</Prefix>
  <KeyCount>2</KeyCount>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>snapshots/products/20260731T010000Z.tar.gz</Key>
    <LastModified>2026-07-31T01:00:00.000Z</LastModified>
    <Size>10</Size>
  </Contents>
  <Contents>
    <Key>snapshots/products/20260731T020000Z.tar.gz</Key>
    <LastModified>2026-07-31T02:00:00.000Z</LastModified>
    <Size>10</Size>
  </Contents>
</ListBucketResult>"#,
            ))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;

        let logs = {
            let _lock = ENV_MUTEX.lock().expect("env mutex poisoned");
            let previous_retention = std::env::var_os("FLAPJACK_SNAPSHOT_RETENTION");
            let previous_access_key = std::env::var_os("AWS_ACCESS_KEY_ID");
            let previous_secret_key = std::env::var_os("AWS_SECRET_ACCESS_KEY");
            std::env::set_var("FLAPJACK_SNAPSHOT_RETENTION", "1");
            std::env::set_var("AWS_ACCESS_KEY_ID", "test");
            std::env::set_var("AWS_SECRET_ACCESS_KEY", "test");

            let logs = SharedLogBuffer::default();
            let subscriber = tracing_subscriber::registry().with(
                tracing_subscriber::fmt::layer()
                    .without_time()
                    .with_ansi(false)
                    .with_writer(logs.clone()),
            );
            let s3_config = flapjack::index::s3::S3Config {
                bucket_name: "snapshot-bucket".to_string(),
                region: "us-east-1".to_string(),
                endpoint: Some(server.uri()),
            };

            let _log_guard = tracing::subscriber::set_default(subscriber);
            enforce_backup_retention(&s3_config, "products").await;

            restore_env_var("FLAPJACK_SNAPSHOT_RETENTION", previous_retention);
            restore_env_var("AWS_ACCESS_KEY_ID", previous_access_key);
            restore_env_var("AWS_SECRET_ACCESS_KEY", previous_secret_key);
            logs.contents()
        };
        let requests = server
            .received_requests()
            .await
            .expect("recorded requests should be available");
        let list_requests: Vec<_> = requests
            .iter()
            .filter(|request| request.method.as_str() == "GET")
            .collect();
        assert_eq!(
            list_requests.len(),
            1,
            "retention pass should issue exactly one LIST, got {:?}",
            list_requests
                .iter()
                .map(|request| request.url.as_str())
                .collect::<Vec<_>>()
        );
        assert!(
            list_requests[0]
                .url
                .query_pairs()
                .any(|(name, value)| name == "prefix" && value == "snapshots/products/"),
            "retention pass should LIST the products snapshot prefix, got {}",
            list_requests[0].url
        );
        let delete_requests: Vec<_> = requests
            .iter()
            .filter(|request| request.method.as_str() == "DELETE")
            .collect();
        assert_eq!(
            delete_requests.len(),
            1,
            "retention pass should reach exactly one rejected DELETE, got {:?}",
            delete_requests
                .iter()
                .map(|request| request.url.as_str())
                .collect::<Vec<_>>()
        );
        let delete_path = delete_requests[0].url.path();
        assert!(
            delete_path.ends_with("snapshots/products/20260731T010000Z.tar.gz"),
            "the rejected DELETE should target the oldest products snapshot, got {delete_path}"
        );

        assert!(
            logs.contains("ERROR"),
            "retention delete failure should emit an ERROR log, got {logs:?}"
        );
        assert!(
            logs.contains("tenant=products") || logs.contains("tenant=\"products\""),
            "retention delete failure log should include the tenant field, got {logs:?}"
        );
        assert!(
            logs.contains("S3 delete"),
            "retention delete failure log should include delete context, got {logs:?}"
        );
        assert!(
            logs.split(|character: char| !character.is_ascii_digit())
                .any(|token| token == "403"),
            "retention delete failure log should include HTTP status 403, got {logs:?}"
        );
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
    fn storage_maintenance_prunes_acknowledged_crawler_files_across_restart() {
        use flapjack::index::manager::publication::{
            ContentDigest, CrawlerRunCountersEvidence, CrawlerRunExecutionClaimDisposition,
            CrawlerRunErrorCodeEvidence, CrawlerRunStore, CrawlerTerminalOutcome,
        };

        let tmp = tempfile::TempDir::new().unwrap();
        let spool = SpoolStore::new(tmp.path(), SpoolLimits::default()).unwrap();
        let store = CrawlerRunStore::new(tmp.path());
        let run_id = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
        let terminal_at = 10_000;
        store
            .start(
                run_id,
                ContentDigest::new(format!("sha256:{}", "a".repeat(64))).unwrap(),
                9_000,
            )
            .unwrap();
        let claim = match store.claim_execution(run_id).unwrap() {
            CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
            _ => panic!("new crawler run must own its execution lock"),
        };
        drop(claim);
        store
            .finish_without_publication(
                run_id,
                CrawlerTerminalOutcome::Failed {
                    error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
                },
                CrawlerRunCountersEvidence::default(),
                1_000,
                terminal_at,
            )
            .unwrap();
        store.acknowledge(run_id, terminal_at + 1).unwrap();

        let retained_root = tmp.path().join(".crawler_run_tombstones");
        let tombstone = retained_root.join(format!("{run_id}.json"));
        let execution_lock = retained_root.join(format!("{run_id}.execution.lock"));
        assert!(tombstone.exists());
        assert!(execution_lock.exists());

        assert_eq!(
            run_storage_maintenance_pass(
                Some(&spool),
                &store,
                terminal_at + CrawlerRunStore::RETENTION_MS,
            )
            .unwrap(),
            1
        );
        assert!(!tombstone.exists());
        assert!(!execution_lock.exists());
        assert!(CrawlerRunStore::new(tmp.path())
            .load(run_id)
            .unwrap()
            .is_none());
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
            .create_async_migration_admission(job_uuid, "target-index", AsyncMigrationPublicationSemantic::CreateOnly)
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

    /// TODO: Document one_shot_rollover_persists_completed_day_and_resets_counters.
    #[tokio::test]
    async fn one_shot_rollover_persists_completed_day_and_resets_counters() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();

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
            state.metrics_state.as_ref(),
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

    /// TODO: Document one_shot_rollover_unions_gauges_and_preserves_source.
    #[tokio::test]
    async fn one_shot_rollover_unions_gauges_and_preserves_source() {
        let tmp = tempfile::TempDir::new().unwrap();
        let state = TestStateBuilder::new(&tmp).build_shared();
        let persistence = UsagePersistence::new(tmp.path()).unwrap();
        let metrics_state = state.metrics_state.as_ref().unwrap();
        metrics_state.replace_index_gauges(std::collections::BTreeMap::from([
            (
                "products".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: Some(3),
                    storage_bytes: Some(12_345),
                },
            ),
            (
                "storage_only".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: None,
                    storage_bytes: Some(4_096),
                },
            ),
            (
                "empty".to_string(),
                crate::handlers::metrics::IndexGaugeValues {
                    documents_count: None,
                    storage_bytes: Some(0),
                },
            ),
        ]));

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
            Some(metrics_state),
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
        assert_eq!(
            metrics_state.index_gauge_snapshot(),
            std::sync::Arc::new(std::collections::BTreeMap::from([
                (
                    "products".to_string(),
                    crate::handlers::metrics::IndexGaugeValues {
                        documents_count: Some(3),
                        storage_bytes: Some(12_345),
                    },
                ),
                (
                    "storage_only".to_string(),
                    crate::handlers::metrics::IndexGaugeValues {
                        documents_count: None,
                        storage_bytes: Some(4_096),
                    },
                ),
                (
                    "empty".to_string(),
                    crate::handlers::metrics::IndexGaugeValues {
                        documents_count: None,
                        storage_bytes: Some(0),
                    },
                ),
            ])),
            "rollover must not mutate the captured generation"
        );

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
