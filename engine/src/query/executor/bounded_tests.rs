//! Unit coverage for the bounded multithread execution seam owned by
//! `super`: `FLAPJACK_SEARCH_THREADS` resolution, the thread-count-keyed pool
//! cache, and the in-flight budget gate.
//!
//! This module also owns the shared `FLAPJACK_SEARCH_THREADS` test guard,
//! because every bounded-execution test — here and in `super::parity_tests` —
//! must apply and restore that process-global variable the same way.

use super::parity_fixtures::{
    build_parity_fixture, ExecutorParityFixture, SEARCH_LIMIT, TEXT_SPECS,
};
use super::{
    bounded_pool, bounded_pool_with, resolve_search_threads, BoundedSearchPool, SEARCH_THREADS_ENV,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};
use tantivy::Executor;

/// Thread count the parity and budget specimens force so the bounded
/// multithread path runs with a ceiling small enough to overrun visibly.
pub(super) const TEST_THREAD_COUNT: usize = 2;

/// Wait for unrelated full-suite searches to release the process-global pool.
///
/// A deadline preserves the release assertion: a leaked permit fails instead
/// of turning the test into an unbounded wait.
pub(super) fn wait_for_pool_quiescence(pool: &BoundedSearchPool) -> super::BoundedPoolCounters {
    let idle_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let counters = pool.counters();
        if counters.in_flight == 0 {
            return counters;
        }
        assert!(
            std::time::Instant::now() < idle_deadline,
            "shared bounded pool did not release {} permits before the idle deadline",
            counters.in_flight
        );
        std::thread::yield_now();
    }
}

/// RAII guard that applies a `FLAPJACK_SEARCH_THREADS` value for the duration
/// of a test and restores the process-original on drop.
///
/// The variable is process-global, so every user must additionally carry
/// `#[serial_test::serial(flapjack_search_threads_env)]`; the guard alone only
/// restores state, it does not exclude a concurrent reader.
pub(crate) struct SearchThreadsEnvGuard {
    previous_value: Option<String>,
}

impl SearchThreadsEnvGuard {
    pub(crate) fn set(value: &str) -> Self {
        let previous_value = std::env::var(SEARCH_THREADS_ENV).ok();
        std::env::set_var(SEARCH_THREADS_ENV, value);
        Self { previous_value }
    }

    pub(crate) fn unset() -> Self {
        let previous_value = std::env::var(SEARCH_THREADS_ENV).ok();
        std::env::remove_var(SEARCH_THREADS_ENV);
        Self { previous_value }
    }
}

impl Drop for SearchThreadsEnvGuard {
    fn drop(&mut self) {
        match &self.previous_value {
            Some(value) => std::env::set_var(SEARCH_THREADS_ENV, value),
            None => std::env::remove_var(SEARCH_THREADS_ENV),
        }
    }
}

fn assert_first_text_search_matches_fixture(fixture: &ExecutorParityFixture) {
    let spec = &TEXT_SPECS[0];
    let result = fixture
        .executor(spec.query)
        .execute(
            fixture.searcher(),
            fixture.text_query(spec),
            None,
            SEARCH_LIMIT,
        )
        .expect("fallback search");
    let actual_ids: Vec<&str> = result
        .documents
        .iter()
        .map(|doc| doc.document.id.as_str())
        .collect();

    assert_eq!(actual_ids, spec.expected_ids);
    assert_eq!(result.total, spec.expected_total);
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn resolver_uses_the_single_thread_default_when_the_env_var_is_unset() {
    let _env = SearchThreadsEnvGuard::unset();

    assert_eq!(resolve_search_threads(), super::DEFAULT_SEARCH_THREADS);
    assert_eq!(
        super::DEFAULT_SEARCH_THREADS, 1,
        "Stage 3's frozen-matrix benchmark measured no reliable multithread win at the frozen load level, so the measured default stays on the single-thread path"
    );
    assert_eq!(
        super::IN_FLIGHT_SEARCHES_PER_WORKER_THREAD, 1,
        "Stage 3 kept the measured single-search-per-worker budget: sequential load never engages the in-flight ceiling, so no larger budget was justified"
    );
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn resolver_maps_invalid_and_zero_values_to_single_thread() {
    for raw in [
        "0",
        "-1",
        "",
        "   ",
        "two",
        "2.5",
        "1e3",
        "99999999999999999999999",
    ] {
        let _env = SearchThreadsEnvGuard::set(raw);
        assert_eq!(resolve_search_threads(), 1, "raw={raw:?}");
    }
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn resolver_accepts_positive_counts_and_trims_surrounding_whitespace() {
    for (raw, expected) in [("1", 1), ("2", 2), (" 4 ", 4), ("\t8\n", 8)] {
        let _env = SearchThreadsEnvGuard::set(raw);
        assert_eq!(resolve_search_threads(), expected, "raw={raw:?}");
    }
}

#[test]
fn pool_cache_holds_exactly_one_pool_per_resolved_thread_count() {
    let first = bounded_pool(3).expect("pool for 3 threads");
    let cached = bounded_pool(3).expect("cached pool for 3 threads");
    let other = bounded_pool(4).expect("pool for 4 threads");

    assert!(
        Arc::ptr_eq(&first, &cached),
        "one thread count must resolve to one shared pool"
    );
    assert!(
        !Arc::ptr_eq(&first, &other),
        "distinct thread counts must not share a pool"
    );
    assert_eq!(first.counters().budget, 3);
    assert_eq!(other.counters().budget, 4);
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn pool_cache_memoizes_executor_creation_failure() {
    let build_attempts = AtomicUsize::new(0);
    let unavailable_thread_count = usize::MAX;

    for _ in 0..2 {
        assert!(
            bounded_pool_with(unavailable_thread_count, |_, _| {
                build_attempts.fetch_add(1, Ordering::Relaxed);
                None
            })
            .is_none(),
            "an unavailable executor must use the single-thread fallback"
        );
    }

    let _env = SearchThreadsEnvGuard::set(&unavailable_thread_count.to_string());
    assert_first_text_search_matches_fixture(&build_parity_fixture());

    assert_eq!(
        build_attempts.load(Ordering::Relaxed),
        1,
        "a real query must reuse the cached failure and fall back without another build attempt"
    );
}

#[test]
fn in_flight_permits_stop_at_the_budget_and_free_on_drop() {
    let executor = Executor::multi_thread(TEST_THREAD_COUNT, "flapjack-search-budget-test-")
        .expect("test pool");
    let pool = Arc::new(BoundedSearchPool::new(executor, TEST_THREAD_COUNT));

    let first = pool.try_acquire().expect("first permit within budget");
    let second = pool.try_acquire().expect("second permit within budget");
    assert!(
        pool.try_acquire().is_none(),
        "a third concurrent permit must be denied at budget {TEST_THREAD_COUNT}"
    );
    assert_eq!(pool.counters().in_flight, 2);
    assert_eq!(pool.counters().in_flight_high_water, 2);
    assert_eq!(pool.counters().multithread_executions, 2);

    drop(second);
    assert_eq!(pool.counters().in_flight, 1);
    let reused = pool
        .try_acquire()
        .expect("a released slot must be reusable");
    assert_eq!(pool.counters().in_flight, 2);

    drop(reused);
    drop(first);
    assert_eq!(pool.counters().in_flight, 0, "every permit released");
    assert_eq!(
        pool.counters().in_flight_high_water,
        2,
        "high water must record the peak, not the final in-flight count"
    );
    assert_eq!(pool.counters().multithread_executions, 3);
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn exhausted_budget_runs_the_query_on_the_fallback_path() {
    const SATURATED_POOL_THREAD_COUNT: usize = 5;

    let pool = bounded_pool(SATURATED_POOL_THREAD_COUNT).expect("pool for fallback-path coverage");
    wait_for_pool_quiescence(&pool);
    pool.reset_counters();
    let permits: Vec<_> = (0..pool.counters().budget)
        .map(|_| pool.try_acquire().expect("permit within test budget"))
        .collect();
    let executions_before_fallback = pool.counters().multithread_executions;

    let env = SearchThreadsEnvGuard::set(&SATURATED_POOL_THREAD_COUNT.to_string());
    assert_first_text_search_matches_fixture(&build_parity_fixture());

    let saturated_counters = pool.counters();
    assert_eq!(saturated_counters.in_flight, saturated_counters.budget);
    assert_eq!(
        saturated_counters.multithread_executions, executions_before_fallback,
        "the query entered the multithread executor despite an exhausted budget"
    );

    drop(env);
    drop(permits);
    assert_eq!(wait_for_pool_quiescence(&pool).in_flight, 0);
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn concurrent_searches_never_exceed_the_resolved_in_flight_budget() {
    const WORKERS: usize = 12;
    const SEARCHES_PER_WORKER: usize = 40;

    let _env = SearchThreadsEnvGuard::set("2");
    let fixture = build_parity_fixture();
    let pool = bounded_pool(TEST_THREAD_COUNT).expect("bounded pool for 2 threads");
    pool.reset_counters();

    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                for spec in TEXT_SPECS.iter().cycle().take(SEARCHES_PER_WORKER) {
                    fixture
                        .executor(spec.query)
                        .execute(
                            fixture.searcher(),
                            fixture.text_query(spec),
                            None,
                            super::parity_fixtures::SEARCH_LIMIT,
                        )
                        .unwrap();
                }
            });
        }
    });

    let counters = wait_for_pool_quiescence(&pool);
    assert_eq!(counters.budget, TEST_THREAD_COUNT);
    assert!(
        counters.multithread_executions > 0,
        "no search took the bounded multithread path, so the ceiling was never exercised"
    );
    assert_eq!(
        counters.in_flight, 0,
        "every permit must be released once the searches finish"
    );
    assert!(
        counters.in_flight_high_water <= TEST_THREAD_COUNT,
        "in-flight high water {} exceeded the budget of {TEST_THREAD_COUNT}",
        counters.in_flight_high_water
    );
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn bounded_aggregate_concurrency_across_simultaneous_requests() {
    const WORKERS: usize = 16;
    const SEARCHES_PER_WORKER: usize = 25;

    let _env = SearchThreadsEnvGuard::set(&TEST_THREAD_COUNT.to_string());
    let fixture = build_parity_fixture();
    let pool =
        bounded_pool(TEST_THREAD_COUNT).expect("bounded pool for aggregate-concurrency test");
    wait_for_pool_quiescence(&pool);
    pool.reset_counters();

    let start = Arc::new(Barrier::new(WORKERS));
    std::thread::scope(|scope| {
        for worker in 0..WORKERS {
            let start = Arc::clone(&start);
            let fixture = &fixture;
            scope.spawn(move || {
                start.wait();
                for spec in TEXT_SPECS
                    .iter()
                    .cycle()
                    .skip(worker % TEXT_SPECS.len())
                    .take(SEARCHES_PER_WORKER)
                {
                    fixture
                        .executor(spec.query)
                        .execute(
                            fixture.searcher(),
                            fixture.text_query(spec),
                            None,
                            SEARCH_LIMIT,
                        )
                        .unwrap();
                }
            });
        }
    });

    let counters = wait_for_pool_quiescence(&pool);
    assert_eq!(counters.budget, TEST_THREAD_COUNT);
    assert_eq!(counters.in_flight, 0);
    assert!(
        counters.multithread_executions >= WORKERS,
        "too few requests reached the bounded executor: {:?}",
        counters
    );
    assert_eq!(
        counters.in_flight_high_water, TEST_THREAD_COUNT,
        "simultaneous requests must saturate but not exceed the aggregate budget"
    );
}
