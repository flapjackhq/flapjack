//! Stub summary for engine/flapjack-http/src/server_shutdown_tests.rs.
use super::{
    await_plaintext_connection_drain, flush_analytics_before_shutdown,
    flush_then_wait_for_manager_shutdown, flush_then_wait_for_migration_and_manager_shutdown,
    full_graceful_shutdown, persist_shutdown_inventory_receipt, require_drained_shutdown,
    run_pre_serve_barrier_with_catchup, shutdown_inventory_receipt_path, ShutdownWaitOutcome,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[test]
fn request_replication_children_retain_mutation_permits_until_manager_drain() {
    let indices = include_str!("handlers/indices.rs");
    let batch = include_str!("handlers/objects/batch.rs");
    let manager = include_str!("../../flapjack-replication/src/manager.rs");
    for (owner, source) in [("indices", indices), ("batch", batch)] {
        assert!(
            source.contains("crate::pause_registry::request_mutation_permit()"),
            "{owner} replication child must clone the originating request permit"
        );
        assert!(
            source.contains("let _mutation_permit = mutation_permit;"),
            "{owner} replication child must retain its admitted request mutation permit"
        );
    }
    assert!(
        manager.contains("while let Some(result) = peer_tasks.join_next().await"),
        "replication manager must settle every tracked peer task before releasing the request child permit"
    );
}

#[test]
fn shutdown_analytics_flush_is_complete_before_the_helper_returns() {
    let temp = tempfile::TempDir::new().unwrap();
    let config = flapjack::analytics::AnalyticsConfig {
        enabled: true,
        data_dir: temp.path().join("analytics"),
        flush_interval_secs: 60,
        flush_size: 10_000,
        retention_days: 30,
    };
    let collector = flapjack::analytics::AnalyticsCollector::new(config.clone());
    collector.record_search(flapjack::analytics::schema::SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: "release drain".to_string(),
        query_id: Some("release-drain-qid".to_string()),
        index_name: "products".to_string(),
        nb_hits: 1,
        processing_time_ms: 1,
        user_token: Some("release-user".to_string()),
        user_ip: None,
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: None,
        region: None,
        experiment_id: None,
        variant_id: None,
        assignment_method: None,
    });

    flush_analytics_before_shutdown(true, &collector);

    let persisted = std::fs::read_dir(config.searches_dir("products"))
        .unwrap()
        .flat_map(|entry| std::fs::read_dir(entry.unwrap().path()).unwrap())
        .count();
    assert!(
        persisted > 0,
        "shutdown must synchronously persist buffered analytics"
    );
}

#[tokio::test]
async fn persisted_release_fence_skips_every_pre_serve_mutation_owner() {
    let temp = tempfile::TempDir::new().unwrap();
    let state = crate::test_helpers::TestStateBuilder::new(&temp).build();
    state
        .global_mutation_fence
        .acquire("release-restart-1")
        .await
        .unwrap();
    let catchup_polled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let catchup_observer = Arc::clone(&catchup_polled);

    let reports = run_pre_serve_barrier_with_catchup(&state, async move {
        catchup_observer.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    })
    .await
    .unwrap();

    assert!(reports.is_empty());
    assert!(!catchup_polled.load(std::sync::atomic::Ordering::SeqCst));
}

#[tokio::test]
async fn plaintext_connection_drain_timeout_is_an_explicit_error() {
    let observed = tokio::time::timeout(
        tokio::time::Duration::from_millis(100),
        await_plaintext_connection_drain(
            std::future::pending::<std::io::Result<()>>(),
            std::future::ready(()),
            tokio::time::Duration::from_millis(1),
        ),
    )
    .await;

    let error = observed
        .expect("the configured drain timeout must terminate the wait")
        .expect_err("a timed-out plaintext connection drain must fail the server stop");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(
        error.to_string(),
        "plaintext graceful connection drain timed out after 1ms"
    );
}

#[tokio::test]
async fn plaintext_connection_drain_preserves_successful_completion() {
    await_plaintext_connection_drain(
        std::future::ready(Ok(())),
        std::future::pending::<()>(),
        tokio::time::Duration::from_millis(1),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn plaintext_connection_drain_has_no_deadline_before_shutdown_starts() {
    let observed = tokio::time::timeout(
        tokio::time::Duration::from_millis(10),
        await_plaintext_connection_drain(
            std::future::pending::<std::io::Result<()>>(),
            std::future::pending::<()>(),
            tokio::time::Duration::from_millis(1),
        ),
    )
    .await;

    assert!(
        observed.is_err(),
        "the connection-drain deadline must not run before shutdown starts"
    );
}

/// Ensures the shutdown helper reports success when the manager drain
/// completes before the configured timeout.
#[tokio::test]
async fn shutdown_wait_helper_returns_drained_when_manager_completes_before_deadline() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let flush_events = Arc::clone(&events);
    let manager_events = Arc::clone(&events);

    let outcome = flush_then_wait_for_manager_shutdown(
        1,
        move || flush_events.lock().unwrap().push("analytics-flushed"),
        async move {
            manager_events.lock().unwrap().push("manager-wait-begins");
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(())
        },
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::Drained);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["analytics-flushed", "manager-wait-begins"]
    );
}

/// Ensures the shutdown helper reports a timeout when the manager drain
/// exceeds the configured deadline.
#[tokio::test]
async fn shutdown_wait_helper_returns_timed_out_when_manager_exceeds_deadline() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let flush_events = Arc::clone(&events);
    let manager_events = Arc::clone(&events);

    let outcome = flush_then_wait_for_manager_shutdown(
        1,
        move || flush_events.lock().unwrap().push("analytics-flushed"),
        async move {
            manager_events.lock().unwrap().push("manager-wait-begins");
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        },
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::TimedOut);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["analytics-flushed", "manager-wait-begins"]
    );
}

/// TODO: Document shutdown_wait_helper_waits_for_migrations_and_manager_under_one_deadline.
#[tokio::test]
async fn shutdown_wait_helper_waits_for_migrations_and_manager_under_one_deadline() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let flush_events = Arc::clone(&events);
    let migration_events = Arc::clone(&events);
    let manager_events = Arc::clone(&events);

    let outcome = flush_then_wait_for_migration_and_manager_shutdown(
        1,
        move || flush_events.lock().unwrap().push("analytics-flushed"),
        async move {
            migration_events.lock().unwrap().push("migrations-drained");
            tokio::time::sleep(Duration::from_millis(10)).await;
        },
        async move {
            manager_events.lock().unwrap().push("manager-drained");
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(())
        },
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::Drained);
    let events = events.lock().unwrap();
    assert_eq!(events[0], "analytics-flushed");
    assert!(events.contains(&"migrations-drained"));
    assert!(events.contains(&"manager-drained"));
}

/// TODO: Document shutdown_wait_helper_times_out_once_for_combined_migration_and_manager_work.
#[tokio::test]
async fn shutdown_wait_helper_times_out_once_for_combined_migration_and_manager_work() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let flush_events = Arc::clone(&events);
    let migration_events = Arc::clone(&events);
    let manager_events = Arc::clone(&events);

    let outcome = flush_then_wait_for_migration_and_manager_shutdown(
        1,
        move || flush_events.lock().unwrap().push("analytics-flushed"),
        async move {
            migration_events
                .lock()
                .unwrap()
                .push("migration-wait-begins");
            tokio::time::sleep(Duration::from_secs(5)).await;
        },
        async move {
            manager_events.lock().unwrap().push("manager-wait-begins");
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(())
        },
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::TimedOut);
    assert_eq!(events.lock().unwrap()[0], "analytics-flushed");
}

/// Ensures graceful shutdown flushes analytics, waits for the manager, and
/// then shuts OTEL down in that order on the success path.
#[tokio::test]
async fn full_graceful_shutdown_calls_otel_after_manager_drain() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let flush_events = Arc::clone(&events);
    let manager_events = Arc::clone(&events);
    let otel_events = Arc::clone(&events);

    let outcome = full_graceful_shutdown(
        5,
        move || flush_events.lock().unwrap().push("analytics-flushed"),
        async move {
            manager_events.lock().unwrap().push("manager-drained");
            tokio::time::sleep(Duration::from_millis(10)).await;
            Ok(())
        },
        move || otel_events.lock().unwrap().push("otel-shutdown"),
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::Drained);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["analytics-flushed", "manager-drained", "otel-shutdown"],
        "shutdown order must be: analytics flush -> manager drain -> otel shutdown"
    );
}

/// Ensures OTEL shutdown still runs when the manager drain times out so
/// tracing flush semantics stay deterministic.
#[tokio::test]
async fn full_graceful_shutdown_calls_otel_even_after_timeout() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let flush_events = Arc::clone(&events);
    let manager_events = Arc::clone(&events);
    let otel_events = Arc::clone(&events);

    let outcome = full_graceful_shutdown(
        1,
        move || flush_events.lock().unwrap().push("analytics-flushed"),
        async move {
            manager_events.lock().unwrap().push("manager-wait-begins");
            tokio::time::sleep(Duration::from_secs(5)).await;
            Ok(())
        },
        move || otel_events.lock().unwrap().push("otel-shutdown"),
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::TimedOut);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["analytics-flushed", "manager-wait-begins", "otel-shutdown"],
        "OTEL shutdown must run even when manager drain times out"
    );
}

/// A timed-out drain is a failed service stop, not an informational success.
/// systemd and the host updater rely on this result to refuse activation when
/// migrations or write queues may still own unflushed work.
#[test]
fn shutdown_timeout_is_a_process_failure() {
    let error = require_drained_shutdown(ShutdownWaitOutcome::TimedOut)
        .expect_err("a timed-out drain must fail the server process");

    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert!(
        error.to_string().contains("write queues"),
        "the failure should identify the unfinished durability boundary"
    );
}

/// A fully drained shutdown remains a clean process exit.
#[test]
fn drained_shutdown_is_a_clean_process_exit() {
    require_drained_shutdown(ShutdownWaitOutcome::Drained)
        .expect("a completed drain must retain the normal clean-stop behavior");
}

#[tokio::test]
async fn write_queue_failure_is_a_failed_service_stop() {
    let outcome = flush_then_wait_for_manager_shutdown(
        1,
        || {},
        std::future::ready(Err("durable worker failed".to_string())),
    )
    .await;

    assert_eq!(
        outcome,
        ShutdownWaitOutcome::Failed("durable worker failed".to_string())
    );
    let error = require_drained_shutdown(outcome).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Other);
    assert!(error.to_string().contains("durable worker failed"));
}

/// The receipt is emitted only after the listener is closed and every write
/// owner has drained. It binds the exact runtime, data-root identity, and
/// sorted document inventory without reading or hashing customer files.
#[tokio::test]
async fn drained_shutdown_persists_one_strict_atomic_inventory_receipt() {
    let temp = tempfile::TempDir::new().unwrap();
    let data_root = temp.path().join("data");
    std::fs::create_dir(&data_root).unwrap();
    let manager = flapjack::IndexManager::new(&data_root);
    manager.create_tenant("zeta").unwrap();
    manager.create_tenant("alpha").unwrap();
    let receipt_path = temp.path().join("shutdown-inventory.json");

    persist_shutdown_inventory_receipt(&manager, &receipt_path, "release-receipt-1").unwrap();

    let receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&receipt_path).unwrap()).unwrap();
    assert_eq!(receipt["schemaVersion"], 1);
    assert_eq!(receipt["kind"], "flapjack_shutdown_inventory");
    assert_eq!(receipt["transactionId"], "release-receipt-1");
    assert_eq!(
        receipt["runtime"],
        serde_json::to_value(flapjack::build_info()).unwrap()
    );
    assert_eq!(
        receipt["inventory"],
        serde_json::json!([
            {"indexId": "alpha", "documentCount": 0},
            {"indexId": "zeta", "documentCount": 0}
        ])
    );
    assert_eq!(
        receipt["dataRoot"]["path"],
        data_root.canonicalize().unwrap().display().to_string()
    );
    assert!(receipt["dataRoot"]["device"].is_u64());
    assert!(receipt["dataRoot"]["inode"].is_u64());
    assert!(std::fs::read_dir(temp.path()).unwrap().all(|entry| !entry
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains("atomic-write")));
}

#[test]
fn production_shutdown_receipt_path_is_outside_customer_data() {
    assert_eq!(
        shutdown_inventory_receipt_path(std::path::Path::new("/var/lib/flapjack/data")).unwrap(),
        std::path::Path::new("/var/lib/flapjack/shutdown-inventory.json")
    );
}
