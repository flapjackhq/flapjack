use super::*;
use crate::analytics::seed::{clear_analytics, seed_analytics_with_options, AnalyticsSeedOptions};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use tempfile::TempDir;

fn test_analytics_config(temp_dir: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: temp_dir.path().to_path_buf(),
        flush_interval_secs: 60,
        flush_size: 10_000,
        retention_days: 90,
    }
}

fn options_with_search_count(search_count: u32) -> AnalyticsSeedOptions {
    AnalyticsSeedOptions {
        search_count: Some(search_count),
        ..AnalyticsSeedOptions::for_days(1)
    }
}

fn wait_for_queued_writer(config: &AnalyticsConfig, index_name: &str) {
    for _ in 0..2_000 {
        if super::super::mutation::waiting_writers(config, index_name) == 1 {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("same-index mutation did not enter the waiting-writer queue");
}

/// A same-index `clear_analytics` queued from inside a live analytics read, plus
/// the proof that it actually reached the coordinator's waiting-writer queue.
struct QueuedClear {
    _hook: AnalyticsReadStageHookGuard,
    result: mpsc::Receiver<Result<u64, String>>,
    queued: Arc<AtomicBool>,
}

impl QueuedClear {
    fn removed_entries(&self) -> u64 {
        self.result
            .recv_timeout(Duration::from_secs(2))
            .expect("queued clear must finish after the query snapshot releases")
            .unwrap()
    }

    fn assert_queued_inside_read(&self) {
        assert!(
            self.queued.load(Ordering::SeqCst),
            "regression must queue clear inside the read it is racing"
        );
    }
}

/// Install a hook that starts a same-index `clear_analytics` the first time the
/// read under test reaches `stage`, and blocks that stage until the clear has
/// registered as a waiting writer. The read therefore continues with a mutation
/// already queued behind it, which is the exact window in which a second read
/// admission would deadlock.
fn queue_clear_at_stage(
    config: &AnalyticsConfig,
    index_name: &str,
    stage: AnalyticsReadStage,
) -> QueuedClear {
    let hook_config = config.clone();
    let hook_index_name = index_name.to_string();
    let queued = Arc::new(AtomicBool::new(false));
    let hook_queued = Arc::clone(&queued);
    let (result_tx, result_rx) = mpsc::channel();

    let hook = set_analytics_read_stage_hook(
        index_name,
        Arc::new(move |hook_stage| {
            if hook_stage != stage || hook_queued.swap(true, Ordering::SeqCst) {
                return;
            }
            let thread_config = hook_config.clone();
            let thread_index_name = hook_index_name.clone();
            let result_tx = result_tx.clone();
            std::thread::spawn(move || {
                result_tx
                    .send(clear_analytics(&thread_config, &thread_index_name))
                    .unwrap();
            });
            wait_for_queued_writer(&hook_config, &hook_index_name);
        }),
    );

    QueuedClear {
        _hook: hook,
        result: result_rx,
        queued,
    }
}

/// Sum one numeric field across the `searches` rows of a `top_searches` response.
fn sum_field(response: &serde_json::Value, field: &str) -> u64 {
    response["searches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row[field].as_u64().unwrap())
        .sum()
}

type SearchCountTask = tokio::task::JoinHandle<Result<serde_json::Value, String>>;

struct HeldSearchCount {
    result: SearchCountTask,
    release: mpsc::Sender<()>,
    _hook: AnalyticsReadStageHookGuard,
}

fn hold_search_count(
    runtime: &tokio::runtime::Runtime,
    config: &AnalyticsConfig,
    index_name: &'static str,
    date: &str,
) -> HeldSearchCount {
    let (active_tx, active_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    let reached_stage = Arc::new(AtomicBool::new(false));
    let hook_reached_stage = Arc::clone(&reached_stage);
    let hook = set_analytics_read_stage_hook(
        index_name,
        Arc::new(move |stage| {
            if stage != AnalyticsReadStage::AfterSearchesRegistration
                || hook_reached_stage.swap(true, Ordering::SeqCst)
            {
                return;
            }
            active_tx.send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }),
    );
    let engine = AnalyticsQueryEngine::new(config.clone());
    let date = date.to_string();
    let result = runtime.spawn(async move { engine.search_count(index_name, &date, &date).await });
    active_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("first search_count must hold a live query snapshot");
    HeldSearchCount {
        result,
        release: release_tx,
        _hook: hook,
    }
}

fn spawn_signaled_search_count(
    runtime: &tokio::runtime::Runtime,
    config: &AnalyticsConfig,
    index_name: &'static str,
    date: &str,
) -> (SearchCountTask, mpsc::Receiver<()>) {
    let engine = AnalyticsQueryEngine::new(config.clone());
    let date = date.to_string();
    let (started_tx, started_rx) = mpsc::channel();
    let result = runtime.spawn(async move {
        started_tx.send(()).unwrap();
        engine.search_count(index_name, &date, &date).await
    });
    (result, started_rx)
}

/// A real `search_count` holds one worker and its same-index snapshot while a
/// real clear queues on the blocking pool. A second `search_count` then needs
/// admission on the only remaining Tokio worker. Admission must park off that
/// worker so the runtime can release the active query, finish clear, and return
/// exact pre-clear and post-clear counts instead of deadlocking.
#[test]
fn saturated_runtime_search_count_and_clear_complete() {
    const EXPECTED_SEARCH_COUNT: u32 = 1_000;

    let temp_dir = TempDir::new().unwrap();
    let index_name = "saturated_runtime_search_count_clear";
    let config = test_analytics_config(&temp_dir);
    let seed = seed_analytics_with_options(
        &config,
        index_name,
        &options_with_search_count(EXPECTED_SEARCH_COUNT),
    )
    .unwrap();
    let seeded_date = seed.seeded_dates[0].clone();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let active_read = hold_search_count(&runtime, &config, index_name, &seeded_date);

    let clear_config = config.clone();
    let clear = runtime.spawn(async move {
        tokio::task::spawn_blocking(move || clear_analytics(&clear_config, index_name))
            .await
            .unwrap()
    });
    wait_for_queued_writer(&config, index_name);

    let (later_read, later_started_rx) =
        spawn_signaled_search_count(&runtime, &config, index_name, &seeded_date);
    later_started_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("later search_count must attempt admission");

    let (runtime_progress_tx, runtime_progress_rx) = mpsc::channel();
    runtime.spawn(async move {
        let _ = runtime_progress_tx.send(());
    });
    let runtime_remained_responsive = runtime_progress_rx
        .recv_timeout(Duration::from_secs(1))
        .is_ok();
    active_read.release.send(()).unwrap();

    let (active_result, clear_result, later_result) = runtime
        .block_on(async {
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(active_read.result, clear, later_read)
            })
            .await
        })
        .expect("search_count requests and clear must complete without deadlock");
    assert!(
        runtime_remained_responsive,
        "queued read admission blocked the only available Tokio worker"
    );
    assert_eq!(
        active_result.unwrap().unwrap()["count"],
        EXPECTED_SEARCH_COUNT
    );
    assert_eq!(clear_result.unwrap().unwrap(), 2);
    assert_eq!(later_result.unwrap().unwrap()["count"], 0);
}

/// A cross-table analytics endpoint must hold one coherent same-index snapshot
/// while it registers both `searches` and `events`. This queues clear after the
/// endpoint has already read searches but before event registration; the endpoint
/// and mutation must both complete instead of self-deadlocking on nested reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cross_table_top_searches_and_clear_complete_with_one_snapshot() {
    const EXPECTED_SEARCH_COUNT: u32 = 1_000;

    let temp_dir = TempDir::new().unwrap();
    let index_name = "cross_table_deadlock_products";
    let config = test_analytics_config(&temp_dir);
    let seed = seed_analytics_with_options(
        &config,
        index_name,
        &options_with_search_count(EXPECTED_SEARCH_COUNT),
    )
    .unwrap();
    let seeded_date = seed.seeded_dates[0].clone();
    let queued_clear = queue_clear_at_stage(
        &config,
        index_name,
        AnalyticsReadStage::BeforeEventsRegistration,
    );

    let engine = AnalyticsQueryEngine::new(config);
    let query = async move {
        let params = AnalyticsQueryParams {
            index_name,
            start_date: &seeded_date,
            end_date: &seeded_date,
            limit: 1_000,
            tags: None,
        };
        engine.top_searches(&params, true, None).await
    };
    let result = tokio::time::timeout(Duration::from_secs(2), query)
        .await
        .expect("cross-table query must not deadlock behind queued clear")
        .unwrap();
    assert_eq!(queued_clear.removed_entries(), 2);
    queued_clear.assert_queued_inside_read();

    assert_eq!(
        sum_field(&result, "count"),
        u64::from(EXPECTED_SEARCH_COUNT)
    );
}

/// Click enrichment runs after `top_searches` has already collected its search
/// rows, so a seed or clear can queue in that handoff. The endpoint must reuse
/// the snapshot it already holds instead of asking for a second same-index read
/// there: the queued mutation waits for the live snapshot, so a nested admission
/// would leave both waiting forever. Enrichment must also see the same
/// pre-clear filesystem state the initial query saw, proving the whole endpoint
/// answers from one coherent snapshot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn top_search_click_enrichment_and_clear_complete_with_one_snapshot() {
    const EXPECTED_SEARCH_COUNT: u32 = 1_000;

    let temp_dir = TempDir::new().unwrap();
    let index_name = "click_enrichment_deadlock_products";
    let config = test_analytics_config(&temp_dir);
    let seed = seed_analytics_with_options(
        &config,
        index_name,
        &options_with_search_count(EXPECTED_SEARCH_COUNT),
    )
    .unwrap();
    let seeded_date = seed.seeded_dates[0].clone();
    let engine = AnalyticsQueryEngine::new(config.clone());
    let params = AnalyticsQueryParams {
        index_name,
        start_date: &seeded_date,
        end_date: &seeded_date,
        limit: 1_000,
        tags: None,
    };

    // Quiescent baseline: the exact enriched answer for the seeded data with no
    // concurrent mutation. The raced query below must reproduce it field for field.
    let baseline = engine.top_searches(&params, true, None).await.unwrap();
    assert_eq!(
        sum_field(&baseline, "count"),
        u64::from(EXPECTED_SEARCH_COUNT)
    );
    assert_eq!(
        sum_field(&baseline, "trackedSearchCount"),
        u64::from(EXPECTED_SEARCH_COUNT)
    );
    let baseline_clicks = sum_field(&baseline, "clickCount");
    assert!(
        baseline_clicks > 0,
        "seeded fixture must produce click events for enrichment to read"
    );

    let queued_clear = queue_clear_at_stage(
        &config,
        index_name,
        AnalyticsReadStage::AfterInitialSearchCollection,
    );
    let raced = tokio::time::timeout(
        Duration::from_secs(2),
        engine.top_searches(&params, true, None),
    )
    .await
    .expect("click enrichment must not deadlock behind clear queued before enrichment")
    .unwrap();
    assert_eq!(queued_clear.removed_entries(), 2);
    queued_clear.assert_queued_inside_read();

    assert_eq!(raced, baseline);

    // After the queued clear ran, the same endpoint must report the empty state.
    let after_clear = engine.top_searches(&params, true, None).await.unwrap();
    assert_eq!(after_clear["searches"].as_array().unwrap().len(), 0);
}
