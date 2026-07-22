use super::{
    flush_then_wait_for_manager_shutdown, flush_then_wait_for_migration_and_manager_shutdown,
    full_graceful_shutdown, ShutdownWaitOutcome,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        },
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::TimedOut);
    assert_eq!(
        events.lock().unwrap().as_slice(),
        ["analytics-flushed", "manager-wait-begins"]
    );
}

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
        },
    )
    .await;

    assert_eq!(outcome, ShutdownWaitOutcome::Drained);
    let events = events.lock().unwrap();
    assert_eq!(events[0], "analytics-flushed");
    assert!(events.contains(&"migrations-drained"));
    assert!(events.contains(&"manager-drained"));
}

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
