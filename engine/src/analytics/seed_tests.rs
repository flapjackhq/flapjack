use super::*;
use crate::analytics::AnalyticsQueryEngine;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use tempfile::TempDir;

#[test]
fn rng_zero_seed_becomes_one() {
    let rng = Rng::new(0);
    assert_eq!(rng.state, 1);
}

#[test]
fn rng_nonzero_seed_kept() {
    let rng = Rng::new(42);
    assert_eq!(rng.state, 42);
}

#[test]
fn rng_deterministic() {
    let mut first = Rng::new(42);
    let mut second = Rng::new(42);
    for _ in 0..100 {
        assert_eq!(first.next_u32(), second.next_u32());
    }
}

#[test]
fn rng_next_f64_in_range() {
    let mut rng = Rng::new(123);
    for _ in 0..1000 {
        let value = rng.next_f64();
        assert!(
            (0.0..1.0).contains(&value),
            "next_f64 out of range: {value}"
        );
    }
}

#[test]
fn rng_range_within_bounds() {
    let mut rng = Rng::new(99);
    for _ in 0..500 {
        let value = rng.range(5, 10);
        assert!((5..=10).contains(&value), "range out of bounds: {value}");
    }
}

#[test]
fn rng_range_handles_equal_or_inverted_bounds() {
    let mut rng = Rng::new(1);
    assert_eq!(rng.range(7, 7), 7);
    assert_eq!(rng.range(10, 5), 10);
}

#[test]
fn rng_weighted_pick_single_weight() {
    let mut rng = Rng::new(42);
    for _ in 0..10 {
        assert_eq!(rng.weighted_pick(&[1.0]), 0);
    }
}

#[test]
fn rng_weighted_pick_extreme_weights() {
    let mut rng = Rng::new(42);
    let mut counts = [0u32; 2];
    for _ in 0..100 {
        counts[rng.weighted_pick(&[0.0, 1.0])] += 1;
    }
    assert_eq!(counts, [0, 100]);
}

#[test]
fn generate_query_id_has_32_hex_characters() {
    let mut rng = Rng::new(42);
    let id = generate_query_id(&mut rng);
    assert_eq!(id.len(), 32);
    assert!(id.chars().all(|character| character.is_ascii_hexdigit()));
}

#[test]
fn queries_for_index_selects_expected_corpus() {
    assert_eq!(queries_for_index("my_movies")[1].0, "batman");
    assert_eq!(queries_for_index("bestbuy_products")[1].0, "samsung");
    assert_eq!(queries_for_index("random_index")[1].0, "shoes");
    assert_eq!(queries_for_index("MOVIES").len(), MOVIE_QUERIES.len());
    assert_eq!(queries_for_index("MyShop").len(), PRODUCT_QUERIES.len());
}

#[test]
fn generated_identifiers_have_expected_formats() {
    let mut rng = Rng::new(42);
    let users = generate_users(&mut rng, 5);
    assert_eq!(users.len(), 5);
    assert!(users
        .iter()
        .all(|user| user.starts_with("user-") && user.len() == 13));

    let object_ids = generate_object_ids(&mut rng, 3);
    assert_eq!(object_ids.len(), 3);
    assert!(object_ids
        .iter()
        .all(|object_id| object_id.starts_with("obj-") && object_id.len() == 10));
}

#[test]
fn generated_click_positions_are_in_range() {
    let mut rng = Rng::new(42);
    for _ in 0..500 {
        assert!((1..=12).contains(&generate_click_position(&mut rng)));
    }
}

#[test]
fn generated_times_are_within_one_day() {
    let mut rng = Rng::new(42);
    let day_ms = 24 * 60 * 60 * 1000;
    for _ in 0..500 {
        assert!((0..day_ms).contains(&generate_time_of_day_ms(&mut rng)));
    }
}

#[test]
fn built_in_distributions_are_normalized() {
    let geo_sum: f64 = GEO_DISTRIBUTION
        .iter()
        .map(|(_, _, weight, _)| weight)
        .sum();
    assert!((geo_sum - 1.0).abs() < 0.05, "geo weights sum = {geo_sum}");

    let device_sum: f64 = DEVICE_TAGS.iter().map(|device| device.1).sum();
    assert!(
        (device_sum - 1.0).abs() < 0.01,
        "device weights sum = {device_sum}"
    );
}

#[test]
fn every_query_corpus_supports_no_result_generation() {
    for queries in [DEFAULT_QUERIES, MOVIE_QUERIES, PRODUCT_QUERIES] {
        assert!(queries.iter().any(|(_, _, has_results)| !has_results));
    }
}

fn options_with_search_count(days: u32, search_count: u32) -> AnalyticsSeedOptions {
    AnalyticsSeedOptions {
        search_count: Some(search_count),
        ..AnalyticsSeedOptions::for_days(days)
    }
}

fn test_analytics_config(temp_dir: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: temp_dir.path().to_path_buf(),
        flush_interval_secs: 60,
        flush_size: 10_000,
        retention_days: 90,
    }
}

/// Same-index seed writers and readbacks overlap on the fixed
/// `seed_searches.parquet` / `seed_events.parquet` destinations. Readers must
/// never observe a truncated or corrupt parquet replacement, and completed seed
/// operations must leave the requested row count readable.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_same_index_seeds_never_break_readback() {
    const EXPECTED_SEARCH_COUNT: u32 = 20_000;
    const WRITER_COUNT: usize = 4;
    const SEEDS_PER_WRITER: usize = 3;

    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let index_name = "concurrent_seed_readback";
    let options = options_with_search_count(1, EXPECTED_SEARCH_COUNT);
    let initial_seed = seed_analytics_with_options(&config, index_name, &options).unwrap();
    let seeded_date = initial_seed.seeded_dates[0].clone();
    let engine = Arc::new(AnalyticsQueryEngine::new(config.clone()));

    let initial_readback = engine
        .search_count(index_name, &seeded_date, &seeded_date)
        .await
        .unwrap();
    assert_eq!(initial_readback["count"], EXPECTED_SEARCH_COUNT);

    let start = Arc::new(Barrier::new(WRITER_COUNT + 1));
    let writers_remaining = Arc::new(AtomicUsize::new(WRITER_COUNT));
    let mut writers = Vec::with_capacity(WRITER_COUNT);
    for _ in 0..WRITER_COUNT {
        let writer_config = config.clone();
        let writer_options = options.clone();
        let writer_start = Arc::clone(&start);
        let writer_count = Arc::clone(&writers_remaining);
        writers.push(std::thread::spawn(move || {
            writer_start.wait();
            let result = (0..SEEDS_PER_WRITER).try_for_each(|_| {
                seed_analytics_with_options(&writer_config, index_name, &writer_options).map(|_| ())
            });
            writer_count.fetch_sub(1, Ordering::Release);
            result
        }));
    }

    let reader_engine = Arc::clone(&engine);
    let reader_count = Arc::clone(&writers_remaining);
    let reader_date = seeded_date.clone();
    let readback = tokio::spawn(async move {
        let mut successful_reads = 0;
        while reader_count.load(Ordering::Acquire) > 0 {
            let result = reader_engine
                .search_count(index_name, &reader_date, &reader_date)
                .await
                .map_err(|error| format!("concurrent readback failed: {error}"))?;
            if result["count"] != EXPECTED_SEARCH_COUNT {
                return Err(format!(
                    "concurrent readback returned {}, expected {EXPECTED_SEARCH_COUNT}",
                    result["count"]
                ));
            }
            successful_reads += 1;
        }
        Ok::<_, String>(successful_reads)
    });

    start.wait();
    for writer in writers {
        writer.join().unwrap().unwrap();
    }
    let successful_reads = readback.await.unwrap().unwrap();
    assert!(successful_reads > 0, "readback must overlap seed writers");

    let final_readback = engine
        .search_count(index_name, &seeded_date, &seeded_date)
        .await
        .unwrap();
    assert_eq!(final_readback["count"], EXPECTED_SEARCH_COUNT);
}

/// Same-index seed and clear operations must both use the analytics mutation
/// coordinator. Holding that index's coordinator blocks both public operations;
/// after release they complete in either valid serial order, never interleaved.
#[tokio::test]
async fn same_index_seed_and_clear_share_mutation_coordinator() {
    const EXPECTED_SEARCH_COUNT: u32 = 100_000;

    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let index_name = "concurrent_seed_clear";
    let options = options_with_search_count(1, EXPECTED_SEARCH_COUNT);

    let (coordinator_held_tx, coordinator_held_rx) = mpsc::channel();
    let (release_coordinator_tx, release_coordinator_rx) = mpsc::channel();
    let holder_config = config.clone();
    let holder = std::thread::spawn(move || {
        super::super::mutation::with_index_mutation(&holder_config, index_name, || {
            coordinator_held_tx.send(()).unwrap();
            release_coordinator_rx.recv().unwrap();
            Ok(())
        })
    });
    coordinator_held_rx.recv().unwrap();

    let writer_config = config.clone();
    let writer_options = options.clone();
    let (writer_started_tx, writer_started_rx) = mpsc::channel();
    let (writer_result_tx, writer_result_rx) = mpsc::channel();
    let writer = std::thread::spawn(move || {
        writer_started_tx.send(()).unwrap();
        writer_result_tx
            .send(seed_analytics_with_options(
                &writer_config,
                index_name,
                &writer_options,
            ))
            .unwrap();
    });

    let clear_config = config.clone();
    let (clear_started_tx, clear_started_rx) = mpsc::channel();
    let (clear_result_tx, clear_result_rx) = mpsc::channel();
    let clearer = std::thread::spawn(move || {
        clear_started_tx.send(()).unwrap();
        clear_result_tx
            .send(clear_analytics(&clear_config, index_name))
            .unwrap();
    });

    writer_started_rx.recv().unwrap();
    clear_started_rx.recv().unwrap();
    let blocked_for = std::time::Duration::from_millis(100);
    assert!(
        matches!(
            writer_result_rx.recv_timeout(blocked_for),
            Err(mpsc::RecvTimeoutError::Timeout)
        ),
        "seed must wait for the same-index mutation coordinator"
    );
    assert_eq!(
        clear_result_rx.recv_timeout(blocked_for),
        Err(mpsc::RecvTimeoutError::Timeout),
        "clear must wait for the same-index mutation coordinator"
    );

    release_coordinator_tx.send(()).unwrap();
    holder.join().unwrap().unwrap();
    let seed_result = writer_result_rx.recv().unwrap().unwrap();
    let removed = clear_result_rx.recv().unwrap().unwrap();
    writer.join().unwrap();
    clearer.join().unwrap();

    assert_eq!(seed_result.total_searches, EXPECTED_SEARCH_COUNT as usize);
    let seeded_date = &seed_result.seeded_dates[0];

    let engine = AnalyticsQueryEngine::new(config);
    let readback = engine
        .search_count(index_name, seeded_date, seeded_date)
        .await
        .unwrap();
    let serial_outcomes = [
        (0, serde_json::json!(EXPECTED_SEARCH_COUNT)),
        (2, serde_json::json!(0)),
    ];
    assert!(
        serial_outcomes.contains(&(removed, readback["count"].clone())),
        "seed and clear must produce one complete serial outcome"
    );
}

#[tokio::test]
async fn cleared_analytics_index_remains_readable_as_empty() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let index_name = "cleared_analytics_readback";
    let options = options_with_search_count(1, 10);
    let seed_result = seed_analytics_with_options(&config, index_name, &options).unwrap();
    let seeded_date = &seed_result.seeded_dates[0];

    assert_eq!(clear_analytics(&config, index_name).unwrap(), 2);
    let readback = AnalyticsQueryEngine::new(config)
        .search_count(index_name, seeded_date, seeded_date)
        .await
        .unwrap();
    assert_eq!(readback["count"], 0);
    assert_eq!(readback["dates"], serde_json::json!([]));
}

/// Same-index clear and readback operations overlap while a clear-capable
/// mutation is already in progress. Readbacks must wait for a stable analytics
/// snapshot and then return either the complete pre-clear count or the empty
/// post-clear result, never a discovery, registration, or execution error.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn same_index_clear_and_search_count_share_query_snapshot_coordinator() {
    const EXPECTED_SEARCH_COUNT: u32 = 10_000;

    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let index_name = "concurrent_clear_readback";
    let options = options_with_search_count(1, EXPECTED_SEARCH_COUNT);
    let seed_result = seed_analytics_with_options(&config, index_name, &options).unwrap();
    let seeded_date = seed_result.seeded_dates[0].clone();

    let engine = AnalyticsQueryEngine::new(config.clone());
    let initial = engine
        .search_count(index_name, &seeded_date, &seeded_date)
        .await
        .unwrap();
    assert_eq!(initial["count"], EXPECTED_SEARCH_COUNT);

    let (coordinator_held_tx, coordinator_held_rx) = mpsc::channel();
    let (release_coordinator_tx, release_coordinator_rx) = mpsc::channel();
    let holder_config = config.clone();
    let holder = std::thread::spawn(move || {
        super::super::mutation::with_index_mutation(&holder_config, index_name, || {
            coordinator_held_tx.send(()).unwrap();
            release_coordinator_rx.recv().unwrap();
            Ok(())
        })
    });
    coordinator_held_rx.recv().unwrap();

    let clear_config = config.clone();
    let (clear_started_tx, clear_started_rx) = mpsc::channel();
    let (clear_result_tx, clear_result_rx) = mpsc::channel();
    let clearer = std::thread::spawn(move || {
        clear_started_tx.send(()).unwrap();
        clear_result_tx
            .send(clear_analytics(&clear_config, index_name))
            .unwrap();
    });
    clear_started_rx.recv().unwrap();

    let reader_engine = AnalyticsQueryEngine::new(config.clone());
    let reader_date = seeded_date.clone();
    let (reader_started_tx, reader_started_rx) = mpsc::channel();
    let mut readback = tokio::spawn(async move {
        reader_started_tx.send(()).unwrap();
        reader_engine
            .search_count(index_name, &reader_date, &reader_date)
            .await
    });
    reader_started_rx.recv().unwrap();

    let blocked_for = std::time::Duration::from_millis(100);
    assert_eq!(
        clear_result_rx.recv_timeout(blocked_for),
        Err(mpsc::RecvTimeoutError::Timeout),
        "clear must wait for the same-index mutation coordinator"
    );
    assert!(
        tokio::time::timeout(blocked_for, &mut readback)
            .await
            .is_err(),
        "search_count must wait for the same-index analytics snapshot coordinator"
    );

    release_coordinator_tx.send(()).unwrap();
    holder.join().unwrap().unwrap();
    let removed = clear_result_rx.recv().unwrap().unwrap();
    clearer.join().unwrap();
    assert_eq!(removed, 2);

    let readback = readback.await.unwrap().unwrap();
    let valid_counts = [
        serde_json::json!(EXPECTED_SEARCH_COUNT),
        serde_json::json!(0),
    ];
    assert!(
        valid_counts.contains(&readback["count"]),
        "search_count returned {}, expected pre-clear {EXPECTED_SEARCH_COUNT} or post-clear 0",
        readback["count"]
    );
}

/// A raw analytics endpoint other than `search_count` must share the same-index
/// snapshot coordinator with clear. It may return the complete pre-clear data or
/// the empty post-clear data, but never race parquet discovery with removal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn same_index_clear_and_top_searches_share_query_snapshot_coordinator() {
    const EXPECTED_SEARCH_COUNT: u32 = 100;

    let temp_dir = TempDir::new().unwrap();
    let config = test_analytics_config(&temp_dir);
    let index_name = "concurrent_clear_top_searches";
    let seed = seed_analytics_with_options(
        &config,
        index_name,
        &options_with_search_count(1, EXPECTED_SEARCH_COUNT),
    )
    .unwrap();
    let seeded_date = seed.seeded_dates[0].clone();

    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let holder_config = config.clone();
    let holder = std::thread::spawn(move || {
        super::super::mutation::with_index_mutation(&holder_config, index_name, || {
            held_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            Ok(())
        })
    });
    held_rx.recv().unwrap();

    let clear_config = config.clone();
    let clearer = std::thread::spawn(move || clear_analytics(&clear_config, index_name));
    for _ in 0..2_000 {
        if super::super::mutation::waiting_writers(&config, index_name) == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }

    let reader_engine = AnalyticsQueryEngine::new(config.clone());
    let reader_date = seeded_date.clone();
    let mut readback = tokio::spawn(async move {
        let params = crate::analytics::AnalyticsQueryParams {
            index_name,
            start_date: &reader_date,
            end_date: &reader_date,
            limit: 1_000,
            tags: None,
        };
        reader_engine.top_searches(&params, false, None).await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut readback)
            .await
            .is_err(),
        "top_searches must wait behind the queued same-index clear"
    );

    release_tx.send(()).unwrap();
    holder.join().unwrap().unwrap();
    assert_eq!(clearer.join().unwrap().unwrap(), 2);
    let result = readback.await.unwrap().unwrap();
    let returned_count: u64 = result["searches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["count"].as_u64().unwrap())
        .sum();
    assert!(
        [u64::from(EXPECTED_SEARCH_COUNT), 0].contains(&returned_count),
        "top_searches returned partial count {returned_count}"
    );
}

#[test]
fn configured_search_count_is_spread_evenly_with_the_remainder_up_front() {
    let mut rng = Rng::new(7);
    let counts = daily_search_counts(
        &options_with_search_count(4, 10),
        chrono::Utc::now(),
        &mut rng,
    );
    assert_eq!(counts, vec![3, 3, 2, 2]);
    assert_eq!(counts.iter().sum::<u32>(), 10);
}

#[test]
fn configured_search_count_below_day_count_still_totals_exactly() {
    let mut rng = Rng::new(7);
    let counts = daily_search_counts(
        &options_with_search_count(5, 2),
        chrono::Utc::now(),
        &mut rng,
    );
    assert_eq!(counts, vec![1, 1, 0, 0, 0]);
}

#[test]
fn unconfigured_search_count_generates_one_organic_day_per_requested_day() {
    let mut rng = Rng::new(7);
    let counts = daily_search_counts(
        &AnalyticsSeedOptions::for_days(3),
        chrono::Utc::now(),
        &mut rng,
    );
    assert_eq!(counts.len(), 3);
    // 800 base * 0.6 weekend floor * 0.8 jitter floor = 384 is the lowest organic day.
    assert!(
        counts.iter().all(|count| (384..=960).contains(count)),
        "organic counts out of range: {counts:?}"
    );
}

#[test]
fn seed_options_reject_out_of_contract_values() {
    let invalid = [
        (
            AnalyticsSeedOptions::for_days(0),
            "days must be between 1 and 90",
        ),
        (
            AnalyticsSeedOptions::for_days(91),
            "days must be between 1 and 90",
        ),
        (
            options_with_search_count(1, 0),
            "searchCount must be between 1 and 100000",
        ),
        (
            options_with_search_count(1, 100_001),
            "searchCount must be between 1 and 100000",
        ),
        (
            AnalyticsSeedOptions {
                no_result_rate: Some(1.5),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "noResultRate must be between 0 and 1",
        ),
        (
            AnalyticsSeedOptions {
                no_result_rate: Some(f64::NAN),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "noResultRate must be between 0 and 1",
        ),
        (
            AnalyticsSeedOptions {
                device_distribution: Some(BTreeMap::from([("desktop".to_string(), 0.9)])),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "deviceDistribution weights must sum to 1",
        ),
        (
            AnalyticsSeedOptions {
                device_distribution: Some(BTreeMap::from([("watch".to_string(), 1.0)])),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "deviceDistribution supports desktop, mobile, and tablet",
        ),
        (
            AnalyticsSeedOptions {
                device_distribution: Some(BTreeMap::new()),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "deviceDistribution must not be empty",
        ),
        (
            AnalyticsSeedOptions {
                country_distribution: Some(BTreeMap::from([("usa".to_string(), 1.0)])),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "countryDistribution keys must be ISO alpha-2 uppercase codes",
        ),
        (
            AnalyticsSeedOptions {
                country_distribution: Some(BTreeMap::from([
                    ("US".to_string(), 1.5),
                    ("DE".to_string(), -0.5),
                ])),
                ..AnalyticsSeedOptions::for_days(1)
            },
            "countryDistribution weights must be finite and non-negative",
        ),
    ];
    for (options, expected) in invalid {
        assert_eq!(validate_seed_options(&options), Err(expected.to_string()));
    }
}

#[test]
fn seed_options_accept_the_documented_boundaries() {
    for options in [
        AnalyticsSeedOptions::for_days(1),
        AnalyticsSeedOptions::for_days(90),
        options_with_search_count(1, 100_000),
        AnalyticsSeedOptions {
            no_result_rate: Some(0.0),
            ..AnalyticsSeedOptions::for_days(1)
        },
        AnalyticsSeedOptions {
            no_result_rate: Some(1.0),
            ..AnalyticsSeedOptions::for_days(1)
        },
        AnalyticsSeedOptions {
            device_distribution: Some(BTreeMap::from([
                ("desktop".to_string(), 0.5),
                ("mobile".to_string(), 0.5),
            ])),
            country_distribution: Some(BTreeMap::from([("DE".to_string(), 1.0)])),
            ..AnalyticsSeedOptions::for_days(1)
        },
    ] {
        assert_eq!(validate_seed_options(&options), Ok(()));
    }
}

#[test]
fn configured_devices_become_platform_tags_with_their_weights() {
    let devices = resolve_devices(Some(&BTreeMap::from([
        ("desktop".to_string(), 0.25),
        ("mobile".to_string(), 0.75),
    ])));
    let tagged: Vec<_> = devices
        .iter()
        .map(|choice| (choice.tag.as_str(), choice.weight))
        .collect();
    assert_eq!(
        tagged,
        vec![("platform:desktop", 0.25), ("platform:mobile", 0.75)]
    );
}

#[test]
fn configured_country_keeps_every_reference_region_and_its_total_weight() {
    let geography = resolve_geography(Some(&BTreeMap::from([
        ("US".to_string(), 0.6),
        ("DE".to_string(), 0.4),
    ])));

    let us: Vec<_> = geography
        .iter()
        .filter(|choice| choice.country == "US")
        .collect();
    let us_reference_rows = GEO_DISTRIBUTION
        .iter()
        .filter(|entry| entry.0 == "US")
        .count();
    assert_eq!(us.len(), us_reference_rows);
    assert!(us.iter().all(|choice| choice.region.is_some()));
    // The US region breakdown is what /2/geo/US/regions renders, so every state must survive.
    assert!(
        us_reference_rows >= 10,
        "US reference rows: {us_reference_rows}"
    );

    let us_weight: f64 = us.iter().map(|choice| choice.weight).sum();
    assert!((us_weight - 0.6).abs() < 1e-9, "US weight = {us_weight}");
    // California carries 0.08 of the 0.45 built-in US weight, so 0.6 * 0.08/0.45.
    let california = us
        .iter()
        .find(|choice| choice.region.as_deref() == Some("California"))
        .expect("California row");
    assert!((california.weight - 0.6 * 0.08 / 0.45).abs() < 1e-9);

    let de: Vec<_> = geography
        .iter()
        .filter(|choice| choice.country == "DE")
        .collect();
    assert_eq!(de.len(), 1);
    assert!((de[0].weight - 0.4).abs() < 1e-9);
    assert_eq!(de[0].region, None);
    assert_eq!(de[0].ip_prefix, "46.114.5.");
}

#[test]
fn country_without_reference_data_falls_back_to_a_single_documentation_prefix() {
    let geography = resolve_geography(Some(&BTreeMap::from([("ZZ".to_string(), 1.0)])));
    assert_eq!(geography.len(), 1);
    assert_eq!(geography[0].country, "ZZ");
    assert_eq!(geography[0].region, None);
    assert_eq!(geography[0].ip_prefix, "203.0.113.");
    assert!((geography[0].weight - 1.0).abs() < 1e-9);
}

#[test]
fn resolved_geography_weights_stay_normalized() {
    let total: f64 = resolve_geography(None)
        .iter()
        .map(|choice| choice.weight)
        .sum();
    assert!((total - 1.0).abs() < 0.05, "geo weights sum = {total}");
}
