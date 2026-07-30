//! Inline performance tests providing manual latency measurement and P99 regression guards for core search operations.
use crate::index::settings::IndexSettings;
use crate::index::SearchOptions;
use crate::integ_tests::search_compat::SearchCompat;
/// Performance tests moved inline from engine/tests/test_perf.rs.
///
/// Contains both:
///   - `test_search_latency`: manual perf measurement (run with --nocapture)
///   - `regression_*_slow`: P99 latency regression guards (gated behind --release)
///
/// Run quick measurement:
///   cargo test --release --lib -p flapjack test_search_latency -- --nocapture
///
/// Run regression guards:
///   cargo test --release --lib -p flapjack regression_ -- --nocapture
// ─── Quick latency measurement ──────────────────────────────────────────────
use crate::query::executor::{
    build_parity_fixture, run_frozen_family, ExecutorParityFixture, FrozenFamily, QueryPhaseReport,
    SearchThreadsEnvGuard, IN_FLIGHT_SEARCHES_PER_WORKER_THREAD,
};
use crate::query::highlighter::Highlighter;
use crate::{Document, FacetRequest, FieldValue, Filter, IndexManager, Sort, SortOrder};
use std::collections::HashMap;
use sysinfo::{get_current_pid, System};
use tempfile::TempDir;

/// Populate a "bench" tenant with `num_docs` synthetic product documents for latency measurement.
///
/// Creates documents with title, description, brand, category (facet), and price fields.
/// Brand cycles through five vendors; category uses 50 buckets; price increases linearly.
///
/// # Arguments
///
/// * `manager` - Index manager to populate.
/// * `rt` - Tokio runtime used to block on async document ingestion.
/// * `num_docs` - Number of documents to generate and index.
fn setup_quick(manager: &IndexManager, rt: &tokio::runtime::Runtime, num_docs: usize) {
    manager.create_tenant("bench").unwrap();
    let mut docs = Vec::new();
    for i in 0..num_docs {
        let mut doc = Document {
            id: format!("doc_{}", i),
            fields: HashMap::new(),
        };
        doc.fields.insert(
            "title".to_string(),
            FieldValue::Text(format!(
                "Laptop Gaming Product {} electronics samsung apple",
                i
            )),
        );
        doc.fields.insert(
            "description".to_string(),
            FieldValue::Text(format!(
                "High performance gaming laptop with premium display description {}",
                i
            )),
        );
        doc.fields.insert(
            "brand".to_string(),
            FieldValue::Text(["Samsung", "Apple", "HP", "Dell", "Sony"][i % 5].to_string()),
        );
        doc.fields.insert(
            "category".to_string(),
            FieldValue::Facet(format!("/cat{}", i % 50)),
        );
        doc.fields.insert(
            "price".to_string(),
            FieldValue::Integer((100 + i * 5) as i64),
        );
        docs.push(doc);
    }
    rt.block_on(manager.add_documents_sync("bench", docs))
        .unwrap();
}

/// Run a micro-benchmark and print avg/p50/p99 latency to stdout.
///
/// Executes 3 warmup iterations, then `iterations` timed runs of `f`, collecting
/// per-invocation wall-clock microseconds.
///
/// # Arguments
///
/// * `label` - Human-readable name printed alongside the results.
/// * `iterations` - Number of timed iterations after warmup.
/// * `f` - Closure to benchmark (called `iterations + 3` times total).
#[derive(Clone, Copy, Debug, PartialEq)]
#[must_use]
struct LatencySummary {
    avg_us: f64,
    p50_us: f64,
    p95_us: f64,
    p99_us: f64,
}

fn summarize_latencies(mut times: Vec<f64>) -> LatencySummary {
    assert!(
        !times.is_empty(),
        "latency summary needs at least one sample"
    );
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let percentile = |fraction: f64| {
        let rank = (times.len() as f64 * fraction).ceil() as usize;
        times[rank.saturating_sub(1).min(times.len() - 1)]
    };
    LatencySummary {
        avg_us: times.iter().sum::<f64>() / times.len() as f64,
        p50_us: percentile(0.50),
        p95_us: percentile(0.95),
        p99_us: percentile(0.99),
    }
}

fn measure(label: &str, iterations: usize, f: impl Fn()) -> LatencySummary {
    assert!(iterations > 0, "measure needs at least one timed iteration");
    // Warmup
    for _ in 0..3 {
        f();
    }
    let mut times: Vec<f64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = std::time::Instant::now();
        f();
        times.push(start.elapsed().as_micros() as f64);
    }
    let summary = summarize_latencies(times);
    println!(
        "  {:<35} avg={:>8.0}us  p50={:>8.0}us  p99={:>8.0}us",
        label, summary.avg_us, summary.p50_us, summary.p99_us
    );
    summary
}

/// Manual latency measurement across ten search scenarios on a 10 K document corpus.
///
/// Covers text-only, short, multi-word, long queries, filter, sort, facets,
/// full-stack combinations, and empty-query facet browsing. Results are printed
/// to stdout—run with `--nocapture` to see them.
///
/// Suffixed `_slow` so it is excluded from default `cargo test` runs.
#[test]
fn test_search_latency_slow() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    println!("\n=== Setting up 10K docs ===");
    let start = std::time::Instant::now();
    setup_quick(&manager, &rt, 10_000);
    println!("  Setup took {:?}", start.elapsed());

    let iters = 100;
    println!("\n=== Search Latency ({} iterations each) ===", iters);

    let _ = measure("text_only (laptop)", iters, || {
        let _ = manager.search("bench", "laptop", None, None, 20);
    });
    let _ = measure("text_only (samsung)", iters, || {
        let _ = manager.search("bench", "samsung", None, None, 20);
    });
    let _ = measure("short_query (l)", iters, || {
        let _ = manager.search("bench", "l", None, None, 20);
    });
    let _ = measure("multi_word (laptop gaming)", iters, || {
        let _ = manager.search("bench", "laptop gaming", None, None, 20);
    });
    let _ = measure("long_query (samsung galaxy premium)", iters, || {
        let _ = manager.search("bench", "samsung galaxy premium display", None, None, 20);
    });
    let _ = measure("text + filter", iters, || {
        let filter = Filter::Range {
            field: "price".to_string(),
            min: 200.0,
            max: 800.0,
        };
        let _ = manager.search("bench", "laptop", Some(&filter), None, 20);
    });
    let _ = measure("text + sort", iters, || {
        let sort = Sort::ByField {
            field: "price".to_string(),
            order: SortOrder::Asc,
        };
        let _ = manager.search("bench", "laptop", None, Some(&sort), 20);
    });
    let _ = measure("text + facets", iters, || {
        let facet = FacetRequest {
            field: "category".to_string(),
            path: "/cat".to_string(),
            value_query: None,
        };
        let _ = manager.search_with_facets("bench", "laptop", None, None, 20, 0, Some(&[facet]));
    });
    let _ = measure("full_stack (text+filter+sort+facets)", iters, || {
        let filter = Filter::Range {
            field: "price".to_string(),
            min: 200.0,
            max: 800.0,
        };
        let sort = Sort::ByField {
            field: "price".to_string(),
            order: SortOrder::Asc,
        };
        let facet = FacetRequest {
            field: "category".to_string(),
            path: "/cat".to_string(),
            value_query: None,
        };
        let _ = manager.search_with_facets(
            "bench",
            "laptop",
            Some(&filter),
            Some(&sort),
            20,
            0,
            Some(&[facet]),
        );
    });
    let _ = measure("empty_query + facets", iters, || {
        let facet = FacetRequest {
            field: "category".to_string(),
            path: "/cat".to_string(),
            value_query: None,
        };
        let _ = manager.search_with_facets("bench", "", None, None, 20, 0, Some(&[facet]));
    });

    println!();
}

#[test]
fn latency_summary_uses_nearest_rank_percentiles() {
    let summary = summarize_latencies((1..=100).map(f64::from).collect());
    assert_eq!(summary.avg_us, 50.5);
    assert_eq!(summary.p50_us, 50.0);
    assert_eq!(summary.p95_us, 95.0);
    assert_eq!(summary.p99_us, 99.0);
}

#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn local_standard_search_threads_ignore_ambient_override() {
    let _ambient_override = SearchThreadsEnvGuard::set("2");
    {
        let _local_standard_threads = SearchThreadsEnvGuard::unset();
        assert_eq!(
            std::env::var("FLAPJACK_SEARCH_THREADS"),
            Err(std::env::VarError::NotPresent)
        );
    }
    assert_eq!(std::env::var("FLAPJACK_SEARCH_THREADS").as_deref(), Ok("2"));
}

// ─── Local standard-profile calibration and frozen-gate consumers ──────────
//
// This fixture reproduces a local latency regime only. Its numbers are not
// reference-locality rung evidence and must never support a capacity claim.

const LOCAL_STANDARD_TENANT: &str = "local_standard";
const LOCAL_STANDARD_DEFAULT_DOCS: usize = 25_000;
const LOCAL_STANDARD_SAMPLES: usize = 30;
const NAME_PREFIX_P95_LIMIT_MS: f64 = 50.0;
const PER_QUERY_TYPE_P95_LIMIT_MS: f64 = 100.0;
const LOCAL_STANDARD_GATE_FAMILIES: &[FrozenFamily] = &[
    FrozenFamily::Text,
    FrozenFamily::Typo,
    FrozenFamily::MultiWord,
    FrozenFamily::Facet,
    FrozenFamily::Filter,
    FrozenFamily::Geo,
    FrozenFamily::Highlight,
];

#[derive(Clone, Copy, Debug)]
struct StandardBuildSummary {
    elapsed_seconds: f64,
    commits: usize,
}

fn env_usize_or_default(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(raw) => raw
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or_else(|| panic!("{name} must be a positive integer, got {raw:?}")),
        Err(std::env::VarError::NotPresent) => default,
        Err(error) => panic!("cannot read {name}: {error}"),
    }
}

fn standard_profile_document(index: usize) -> Document {
    const BRANDS: [&str; 8] = [
        "Apple", "Dell", "Lenovo", "ASUS", "HP", "Samsung", "Sony", "Razer",
    ];
    const CATEGORIES: [&str; 12] = [
        "Laptops",
        "Tablets",
        "Smartphones",
        "Audio",
        "Accessories",
        "Wearables",
        "Gaming",
        "Monitors",
        "Networking",
        "Home Office",
        "Smart Home",
        "Storage",
    ];
    const SUBCATEGORIES: [&str; 8] = [
        "Professional",
        "Business",
        "Gaming",
        "Budget",
        "Ultrabook",
        "Creator",
        "Wireless",
        "Performance",
    ];
    const COLORS: [&str; 8] = [
        "Space Black",
        "Midnight Blue",
        "Arctic White",
        "Titan Gray",
        "Graphite",
        "Forest Green",
        "Silver",
        "Rose Gold",
    ];

    let category = CATEGORIES[index % CATEGORIES.len()];
    let subcategory = SUBCATEGORIES[(index * 7) % SUBCATEGORIES.len()];
    let brand = BRANDS[(index * 5) % BRANDS.len()];
    let color = COLORS[(index * 3) % COLORS.len()];
    let topic = if index.is_multiple_of(3) {
        "wireless"
    } else {
        "professional"
    };
    let mut geoloc = HashMap::new();
    geoloc.insert(
        "lat".to_string(),
        FieldValue::Float(37.70 + (index % 100) as f64 * 0.0001),
    );
    geoloc.insert(
        "lng".to_string(),
        FieldValue::Float(-122.50 + (index % 100) as f64 * 0.0001),
    );

    let fields = HashMap::from([
        (
            "name".to_string(),
            FieldValue::Text(format!(
                "{brand} {category} {topic} Edition {}",
                index % 512
            )),
        ),
        (
            "description".to_string(),
            FieldValue::Text(format!(
                "{topic} {subcategory} product with durable battery and premium display {}",
                index % 2048
            )),
        ),
        ("brand".to_string(), FieldValue::Text(brand.to_string())),
        (
            "category".to_string(),
            FieldValue::Text(category.to_string()),
        ),
        (
            "subcategory".to_string(),
            FieldValue::Text(subcategory.to_string()),
        ),
        (
            "price".to_string(),
            FieldValue::Float(100.0 + (index % 3000) as f64),
        ),
        (
            "rating".to_string(),
            FieldValue::Float(1.0 + (index % 41) as f64 / 10.0),
        ),
        (
            "reviewCount".to_string(),
            FieldValue::Integer((index % 20_000) as i64),
        ),
        (
            "inStock".to_string(),
            FieldValue::Bool(!index.is_multiple_of(6)),
        ),
        (
            "tags".to_string(),
            FieldValue::Array(vec![
                FieldValue::Text(topic.to_string()),
                FieldValue::Text(format!("series-{}", index % 8192)),
                // The production standard profile contains high-cardinality
                // product variants. A deterministic SKU tag keeps the local
                // facet traversal in that regime instead of benchmarking only
                // the 12-value category field.
                FieldValue::Text(format!("sku-{index:08}")),
            ]),
        ),
        ("color".to_string(), FieldValue::Text(color.to_string())),
        (
            "releaseYear".to_string(),
            FieldValue::Integer((2020 + index % 6) as i64),
        ),
        ("_geoloc".to_string(), FieldValue::Object(geoloc)),
    ]);

    Document {
        id: format!("bench-{index:08}"),
        fields,
    }
}

fn save_local_standard_geo_rule(manager: &IndexManager) {
    let rule = serde_json::json!({
        "objectID": "local-standard-geo",
        "conditions": [{"pattern": "wireless", "anchoring": "contains"}],
        "consequence": {
            "params": {
                "aroundLatLng": "37.70,-122.50",
                "aroundRadius": 50000
            }
        }
    });
    let rules_path = manager
        .base_path
        .join(LOCAL_STANDARD_TENANT)
        .join("rules.json");
    std::fs::write(rules_path, serde_json::to_string(&vec![rule]).unwrap()).unwrap();
}

fn setup_local_standard_specimen(
    manager: &IndexManager,
    rt: &tokio::runtime::Runtime,
    num_docs: usize,
) -> StandardBuildSummary {
    manager.create_tenant(LOCAL_STANDARD_TENANT).unwrap();
    let settings = IndexSettings {
        attributes_for_faceting: vec![
            "brand".to_string(),
            "category".to_string(),
            "subcategory".to_string(),
            "tags".to_string(),
            "color".to_string(),
        ],
        searchable_attributes: Some(vec![
            "name".to_string(),
            "description".to_string(),
            "brand".to_string(),
            "category".to_string(),
            "subcategory".to_string(),
            "tags".to_string(),
        ]),
        ..Default::default()
    };
    settings
        .save(
            manager
                .base_path
                .join(LOCAL_STANDARD_TENANT)
                .join("settings.json"),
        )
        .unwrap();
    save_local_standard_geo_rule(manager);

    const BATCH_SIZE: usize = 5_000;
    let started = std::time::Instant::now();
    let mut commits = 0;
    for start in (0..num_docs).step_by(BATCH_SIZE) {
        let end = (start + BATCH_SIZE).min(num_docs);
        let docs = (start..end).map(standard_profile_document).collect();
        rt.block_on(manager.add_documents_sync(LOCAL_STANDARD_TENANT, docs))
            .unwrap();
        commits += 1;
    }
    StandardBuildSummary {
        elapsed_seconds: started.elapsed().as_secs_f64(),
        commits,
    }
}

fn local_standard_doc_count() -> usize {
    env_usize_or_default("FLAPJACK_LOCAL_STANDARD_DOCS", LOCAL_STANDARD_DEFAULT_DOCS)
}

fn facet_requests() -> [FacetRequest; 3] {
    [
        FacetRequest {
            field: "category".to_string(),
            path: "/category".to_string(),
            value_query: None,
        },
        FacetRequest {
            field: "brand".to_string(),
            path: "/brand".to_string(),
            value_query: None,
        },
        FacetRequest {
            field: "tags".to_string(),
            path: "/tags".to_string(),
            value_query: None,
        },
    ]
}

fn search_local_standard(
    manager: &IndexManager,
    query: &str,
    options: &SearchOptions<'_>,
) -> crate::types::SearchResult {
    let result = manager
        .search_with_options(LOCAL_STANDARD_TENANT, query, options)
        .unwrap();
    assert!(
        result.total > 0,
        "local standard query {query:?} must match documents"
    );
    result
}

fn execute_local_standard_geo(manager: &IndexManager) {
    let result = search_local_standard(manager, "wireless", &SearchOptions::with_limit(20));
    assert_eq!(
        result.effective_around_lat_lng.as_deref(),
        Some("37.70,-122.50"),
        "geo family must drive the search owner path that resolves aroundLatLng"
    );
    assert_eq!(
        result.effective_around_radius,
        Some(serde_json::json!(50000)),
        "geo family must drive the search owner path that resolves aroundRadius"
    );
}

fn execute_local_standard_highlight(manager: &IndexManager) {
    let result = search_local_standard(manager, "wireless premium", &SearchOptions::with_limit(20));
    let query_words = vec!["wireless".to_string(), "premium".to_string()];
    let highlighter = Highlighter::default();
    let highlighted_fields = result
        .documents
        .iter()
        .map(|document| {
            highlighter
                .highlight_document(&document.document, &query_words)
                .len()
        })
        .sum::<usize>();
    assert!(
        highlighted_fields > 0,
        "highlight family must transform returned fields"
    );
}

fn execute_local_standard_family(manager: &IndexManager, family: FrozenFamily) {
    match family {
        FrozenFamily::Text => {
            search_local_standard(
                manager,
                "wirel",
                &SearchOptions {
                    query_type: Some("prefixLast"),
                    ..SearchOptions::with_limit(20)
                },
            );
        }
        FrozenFamily::Typo => {
            search_local_standard(
                manager,
                "wireles",
                &SearchOptions {
                    query_type: Some("prefixNone"),
                    typo_tolerance: Some(true),
                    ..SearchOptions::with_limit(20)
                },
            );
        }
        FrozenFamily::MultiWord => {
            search_local_standard(
                manager,
                "wireless durable battery",
                &SearchOptions::with_limit(20),
            );
        }
        FrozenFamily::Facet => {
            let requests = facet_requests();
            let result = search_local_standard(
                manager,
                "wireless",
                &SearchOptions {
                    facets: Some(&requests),
                    max_values_per_facet: Some(25),
                    ..SearchOptions::with_limit(20)
                },
            );
            assert!(
                result
                    .facets
                    .get("tags")
                    .is_some_and(|values| !values.is_empty()),
                "facet family must exercise high-cardinality tag extraction"
            );
        }
        FrozenFamily::Filter => {
            let filter = Filter::And(vec![
                Filter::Range {
                    field: "price".to_string(),
                    min: 100.0,
                    max: 2_500.0,
                },
                Filter::Equals {
                    field: "inStock".to_string(),
                    value: FieldValue::Bool(true),
                },
            ]);
            search_local_standard(
                manager,
                "professional",
                &SearchOptions {
                    filter: Some(&filter),
                    ..SearchOptions::with_limit(20)
                },
            );
        }
        FrozenFamily::Geo => execute_local_standard_geo(manager),
        FrozenFamily::Highlight => execute_local_standard_highlight(manager),
        _ => unreachable!("local gate catalog contains only evaluator families"),
    }
}

fn measure_local_facet(manager: &IndexManager, cold: bool) -> LatencySummary {
    let requests = facet_requests();
    measure(
        if cold {
            "local_standard facet cold"
        } else {
            "local_standard facet warm"
        },
        LOCAL_STANDARD_SAMPLES,
        || {
            if cold {
                manager.invalidate_facet_cache(LOCAL_STANDARD_TENANT);
            }
            let result = manager
                .search_with_options(
                    LOCAL_STANDARD_TENANT,
                    "wireless",
                    &SearchOptions {
                        facets: Some(&requests),
                        limit: 20,
                        max_values_per_facet: Some(25),
                        ..Default::default()
                    },
                )
                .unwrap();
            assert!(
                result.total > 0,
                "facet specimen query must match documents"
            );
            assert!(
                result
                    .facets
                    .get("tags")
                    .is_some_and(|values| !values.is_empty()),
                "facet specimen must exercise high-cardinality tag extraction"
            );
        },
    )
}

fn measure_local_family(
    manager: &IndexManager,
    family: FrozenFamily,
    cold_primary: bool,
) -> LatencySummary {
    let label = format!(
        "local_standard {} {}",
        family.label(),
        if cold_primary { "cold" } else { "warm" }
    );
    measure(&label, LOCAL_STANDARD_SAMPLES, || {
        if cold_primary && family == FrozenFamily::Facet {
            manager.invalidate_facet_cache(LOCAL_STANDARD_TENANT);
        }
        execute_local_standard_family(manager, family);
    })
}

#[derive(Debug)]
struct LocalFamilyMeasurement {
    family: FrozenFamily,
    warm: LatencySummary,
    cold_primary: LatencySummary,
}

fn measure_local_gate_matrix(manager: &IndexManager) -> Vec<LocalFamilyMeasurement> {
    LOCAL_STANDARD_GATE_FAMILIES
        .iter()
        .copied()
        .map(|family| LocalFamilyMeasurement {
            family,
            warm: measure_local_family(manager, family, false),
            cold_primary: measure_local_family(manager, family, true),
        })
        .collect()
}

fn parse_exported_limit(source: &str, name: &str) -> f64 {
    let prefix = format!("export const {name} = ");
    source
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix(&prefix)
                .and_then(|value| value.strip_suffix(';'))
                .and_then(|value| value.parse::<f64>().ok())
        })
        .unwrap_or_else(|| panic!("missing or invalid {name} in scale_rung_verdict.mjs"))
}

#[test]
fn frozen_per_query_gate_constants_match_loadtest_owner() {
    let owner_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("loadtest/lib/scale_rung_verdict.mjs");
    let owner = std::fs::read_to_string(&owner_path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", owner_path.display()));
    assert_eq!(
        parse_exported_limit(&owner, "NAME_PREFIX_P95_LIMIT_MS"),
        NAME_PREFIX_P95_LIMIT_MS
    );
    assert_eq!(
        parse_exported_limit(&owner, "PER_QUERY_TYPE_P95_LIMIT_MS"),
        PER_QUERY_TYPE_P95_LIMIT_MS
    );
}

#[test]
fn local_standard_gate_uses_exact_evaluator_families() {
    let labels = LOCAL_STANDARD_GATE_FAMILIES
        .iter()
        .map(|family| family.label())
        .collect::<Vec<_>>();

    assert_eq!(
        labels,
        [
            "text",
            "typo",
            "multi_word",
            "facet",
            "filter",
            "geo",
            "highlight",
        ]
    );
}

#[test]
fn local_standard_gate_families_execute_non_vacuously() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    setup_local_standard_specimen(&manager, &rt, 256);

    for family in LOCAL_STANDARD_GATE_FAMILIES {
        execute_local_standard_family(&manager, *family);
    }
}

#[test]
fn local_standard_geo_family_filters_through_search_owner_path() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant(LOCAL_STANDARD_TENANT).unwrap();
    let docs = vec![
        Document {
            id: "near".to_string(),
            fields: HashMap::from([
                (
                    "name".to_string(),
                    FieldValue::Text("wireless local standard near".to_string()),
                ),
                (
                    "_geoloc".to_string(),
                    FieldValue::Object(HashMap::from([
                        ("lat".to_string(), FieldValue::Float(37.70)),
                        ("lng".to_string(), FieldValue::Float(-122.50)),
                    ])),
                ),
            ]),
        },
        Document {
            id: "far".to_string(),
            fields: HashMap::from([
                (
                    "name".to_string(),
                    FieldValue::Text("wireless local standard far".to_string()),
                ),
                (
                    "_geoloc".to_string(),
                    FieldValue::Object(HashMap::from([
                        ("lat".to_string(), FieldValue::Float(34.0522)),
                        ("lng".to_string(), FieldValue::Float(-118.2437)),
                    ])),
                ),
            ]),
        },
    ];
    rt.block_on(manager.add_documents_sync(LOCAL_STANDARD_TENANT, docs))
        .unwrap();
    save_local_standard_geo_rule(&manager);

    execute_local_standard_geo(&manager);
    let result = search_local_standard(&manager, "wireless", &SearchOptions::with_limit(20));
    assert_eq!(result.total, 1);
    assert_eq!(result.documents[0].document.id, "near");
}

/// Local-only seven-family gate. Only the facet family has a production cache,
/// so its cold-primary samples invalidate that cache before every query. These
/// results describe the current machine, not a reference-locality capacity rung.
#[test]
#[ignore]
#[serial_test::serial(local_standard_perf_env, flapjack_search_threads_env)]
fn facet_p95_meets_frozen_per_query_gate_on_local_standard_specimen_very_slow() {
    let _search_threads = SearchThreadsEnvGuard::unset();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    let doc_count = local_standard_doc_count();
    let build = setup_local_standard_specimen(&manager, &rt, doc_count);
    let measurements = measure_local_gate_matrix(&manager);
    let mut breaches = Vec::new();
    for measurement in measurements {
        let limit_ms = if measurement.family == FrozenFamily::Text {
            NAME_PREFIX_P95_LIMIT_MS
        } else {
            PER_QUERY_TYPE_P95_LIMIT_MS
        };
        let warm_p95_ms = measurement.warm.p95_us / 1000.0;
        let cold_p95_ms = measurement.cold_primary.p95_us / 1000.0;
        let verdict = if cold_p95_ms <= limit_ms {
            "inside"
        } else {
            breaches.push(format!(
                "{}={cold_p95_ms:.3}ms>{limit_ms:.3}ms",
                measurement.family.label()
            ));
            "breach"
        };
        eprintln!(
            "local_standard_family family={} docs={doc_count} warm_p95_ms={warm_p95_ms:.3} cold_primary_p95_ms={cold_p95_ms:.3} consumed_limit_ms={limit_ms:.3} verdict={verdict}",
            measurement.family.label(),
        );
    }
    eprintln!(
        "local_standard_gate docs={doc_count} build_s={:.3} commits={}",
        build.elapsed_seconds, build.commits,
    );
    assert!(
        breaches.is_empty(),
        "local standard family gate breaches on {doc_count} documents: {}",
        breaches.join(", "),
    );
}

/// Cold-primary companion to the headline gate. Explicit invalidation before
/// every sample prevents the existing five-second facet cache from hiding the
/// executor cost; no production cache-disable knob is introduced.
#[test]
#[ignore]
#[serial_test::serial(local_standard_perf_env, flapjack_search_threads_env)]
fn result_cache_disabled_still_meets_scale_gate_very_slow() {
    let _search_threads = SearchThreadsEnvGuard::unset();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    let doc_count = local_standard_doc_count();
    let build = setup_local_standard_specimen(&manager, &rt, doc_count);
    let cold = measure_local_facet(&manager, true);
    eprintln!(
        "local_standard_cold_primary docs={doc_count} build_s={:.3} commits={} cold_p95_ms={:.3}",
        build.elapsed_seconds,
        build.commits,
        cold.p95_us / 1000.0,
    );
    assert!(
        cold.p95_us / 1000.0 <= PER_QUERY_TYPE_P95_LIMIT_MS,
        "cache-disabled facet p95 {:.3}ms exceeds consumed {:.3}ms gate on {doc_count}-document local standard specimen",
        cold.p95_us / 1000.0,
        PER_QUERY_TYPE_P95_LIMIT_MS,
    );
}

/// Emit the machine-readable p95-vs-corpus-size curve after the required
/// same-locality throughput probe. Override sizes with
/// `FLAPJACK_LOCAL_STANDARD_SWEEP=25000,50000,100000,200000`.
#[test]
#[ignore]
#[serial_test::serial(local_standard_perf_env, flapjack_search_threads_env)]
fn local_standard_specimen_calibration_curve_very_slow() {
    let _search_threads = SearchThreadsEnvGuard::unset();
    let sizes = std::env::var("FLAPJACK_LOCAL_STANDARD_SWEEP")
        .unwrap_or_else(|_| "25000,50000,100000,200000".to_string())
        .split(',')
        .map(|raw| {
            raw.parse::<usize>()
                .ok()
                .filter(|value| *value > 0)
                .unwrap_or_else(|| panic!("invalid calibration size {raw:?}"))
        })
        .collect::<Vec<_>>();
    assert!(
        sizes.len() >= 4,
        "calibration curve requires at least four sizes"
    );

    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let mut points = Vec::new();
    for doc_count in sizes {
        let temp = TempDir::new().unwrap();
        let manager = IndexManager::new(temp.path());
        let build = setup_local_standard_specimen(&manager, &rt, doc_count);
        let samples_started = std::time::Instant::now();
        let warm = measure_local_facet(&manager, false);
        let cold = measure_local_facet(&manager, true);
        let sample_seconds = samples_started.elapsed().as_secs_f64();
        points.push(serde_json::json!({
            "docs": doc_count,
            "build_seconds": build.elapsed_seconds,
            "docs_per_second": doc_count as f64 / build.elapsed_seconds,
            "commits": build.commits,
            "seconds_per_commit": build.elapsed_seconds / build.commits as f64,
            "facet_sample_seconds": sample_seconds,
            "warm_facet_p95_ms": warm.p95_us / 1000.0,
            "cold_facet_p95_ms": cold.p95_us / 1000.0,
        }));
    }
    println!(
        "calibration_json={}",
        serde_json::to_string(&points).unwrap()
    );
}

// ─── Regression guards (release-only) ───────────────────────────────────────

#[cfg(not(debug_assertions))]
const P99_TEXT_SEARCH_US: u64 = 5_000;
#[cfg(not(debug_assertions))]
const P99_MULTI_WORD_US: u64 = 10_000;
#[cfg(not(debug_assertions))]
const P99_LONG_QUERY_US: u64 = 25_000;
#[cfg(not(debug_assertions))]
const P99_FILTER_US: u64 = 10_000;
#[cfg(not(debug_assertions))]
const P99_SORT_US: u64 = 10_000;
#[cfg(not(debug_assertions))]
const P99_FACET_US: u64 = 30_000;
#[cfg(not(debug_assertions))]
const P99_FULL_STACK_US: u64 = 40_000;
#[cfg(not(debug_assertions))]
const P99_SHORT_QUERY_US: u64 = 15_000;
#[cfg(not(debug_assertions))]
const P99_TYPEAHEAD_TOTAL_US: u64 = 60_000;

/// Populate a "regr" tenant with 1 000 synthetic documents for regression testing.
///
/// Uses 8 brands, 5 adjectives, 20 facet categories, and linearly spaced prices.
/// Designed to be deterministic so P99 thresholds remain stable across runs.
///
/// # Arguments
///
/// * `manager` - Index manager to populate.
/// * `rt` - Tokio runtime used to block on async document ingestion.
#[cfg(not(debug_assertions))]
fn build_corpus(manager: &IndexManager, rt: &tokio::runtime::Runtime) {
    manager.create_tenant("regr").unwrap();
    let brands = [
        "Samsung", "Apple", "HP", "Dell", "Sony", "LG", "Lenovo", "Asus",
    ];
    let adjectives = ["premium", "budget", "gaming", "professional", "compact"];
    let mut docs = Vec::with_capacity(1000);
    for i in 0..1000 {
        let mut fields = HashMap::new();
        fields.insert(
            "name".into(),
            FieldValue::Text(format!(
                "{} {} laptop model-{}",
                brands[i % brands.len()],
                adjectives[i % adjectives.len()],
                i
            )),
        );
        fields.insert(
            "description".into(),
            FieldValue::Text(format!(
                "High quality {} electronics device with display screen {}",
                brands[i % brands.len()],
                i
            )),
        );
        fields.insert(
            "brand".into(),
            FieldValue::Text(brands[i % brands.len()].into()),
        );
        fields.insert(
            "category".into(),
            FieldValue::Facet(format!("/electronics/cat{}", i % 20)),
        );
        fields.insert("price".into(), FieldValue::Integer(100 + (i * 7) as i64));
        docs.push(Document {
            id: format!("d{}", i),
            fields,
        });
    }
    rt.block_on(manager.add_documents_sync("regr", docs))
        .unwrap();
}

#[cfg(not(debug_assertions))]
fn bench(iterations: usize, f: impl Fn()) -> (u64, u64) {
    for _ in 0..5 {
        f();
    }
    let mut times: Vec<u64> = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let t = std::time::Instant::now();
        f();
        times.push(t.elapsed().as_micros() as u64);
    }
    times.sort_unstable();
    let avg = times.iter().sum::<u64>() / times.len() as u64;
    let p99 = times[(times.len() as f64 * 0.99) as usize];
    (avg, p99)
}

#[cfg(not(debug_assertions))]
fn with_manager(f: impl FnOnce(&IndexManager)) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let tmp = TempDir::new().unwrap();
    let mgr = IndexManager::new(tmp.path());
    build_corpus(&mgr, &rt);
    f(&mgr);
}

#[cfg(not(debug_assertions))]
#[test]
fn regression_text_search_slow() {
    with_manager(|m| {
        let (avg, p99) = bench(200, || {
            let _ = m.search("regr", "samsung", None, None, 20);
        });
        eprintln!("  text_search:  avg={avg}us  p99={p99}us  (limit {P99_TEXT_SEARCH_US}us)");
        assert!(
            p99 < P99_TEXT_SEARCH_US,
            "text_search P99 regression: {p99}us > {P99_TEXT_SEARCH_US}us"
        );
    });
}

#[cfg(not(debug_assertions))]
#[test]
fn regression_multi_word_slow() {
    with_manager(|m| {
        let (avg, p99) = bench(200, || {
            let _ = m.search("regr", "samsung laptop", None, None, 20);
        });
        eprintln!("  multi_word:   avg={avg}us  p99={p99}us  (limit {P99_MULTI_WORD_US}us)");
        assert!(
            p99 < P99_MULTI_WORD_US,
            "multi_word P99 regression: {p99}us > {P99_MULTI_WORD_US}us"
        );
    });
}

/// Assert that a five-term query stays below the `P99_LONG_QUERY_US` threshold.
///
/// Release-only. Searches "samsung premium laptop display screen" over the 1 K
/// regression corpus and fails if P99 exceeds the budget.
#[cfg(not(debug_assertions))]
#[test]
fn regression_long_query_slow() {
    with_manager(|m| {
        let (avg, p99) = bench(200, || {
            let _ = m.search(
                "regr",
                "samsung premium laptop display screen",
                None,
                None,
                20,
            );
        });
        eprintln!("  long_query:   avg={avg}us  p99={p99}us  (limit {P99_LONG_QUERY_US}us)");
        assert!(
            p99 < P99_LONG_QUERY_US,
            "long_query P99 regression: {p99}us > {P99_LONG_QUERY_US}us"
        );
    });
}

/// Assert that text search combined with a price-range filter stays below the `P99_FILTER_US` threshold.
///
/// Release-only. Applies a `Filter::Range` on the price field alongside a
/// "laptop" text query over the 1 K regression corpus.
#[cfg(not(debug_assertions))]
#[test]
fn regression_filter_slow() {
    with_manager(|m| {
        let filter = Filter::Range {
            field: "price".into(),
            min: 200.0,
            max: 800.0,
        };
        let (avg, p99) = bench(200, || {
            let _ = m.search("regr", "laptop", Some(&filter), None, 20);
        });
        eprintln!("  filter:       avg={avg}us  p99={p99}us  (limit {P99_FILTER_US}us)");
        assert!(
            p99 < P99_FILTER_US,
            "filter P99 regression: {p99}us > {P99_FILTER_US}us"
        );
    });
}

/// Assert that text search with field-based sorting stays below the `P99_SORT_US` threshold.
///
/// Release-only. Sorts by price ascending alongside a "laptop" text query
/// over the 1 K regression corpus.
#[cfg(not(debug_assertions))]
#[test]
fn regression_sort_slow() {
    with_manager(|m| {
        let sort = Sort::ByField {
            field: "price".into(),
            order: SortOrder::Asc,
        };
        let (avg, p99) = bench(200, || {
            let _ = m.search("regr", "laptop", None, Some(&sort), 20);
        });
        eprintln!("  sort:         avg={avg}us  p99={p99}us  (limit {P99_SORT_US}us)");
        assert!(
            p99 < P99_SORT_US,
            "sort P99 regression: {p99}us > {P99_SORT_US}us"
        );
    });
}

/// Assert that text search with a facet request stays below the `P99_FACET_US` threshold.
///
/// Release-only. Requests `/electronics` category facets alongside a "laptop"
/// text query over the 1 K regression corpus.
#[cfg(not(debug_assertions))]
#[test]
fn regression_facets_slow() {
    with_manager(|m| {
        let facet = FacetRequest {
            field: "category".into(),
            path: "/electronics".into(),
            value_query: None,
        };
        let (avg, p99) = bench(200, || {
            let _ = m.search_with_facets(
                "regr",
                "laptop",
                None,
                None,
                20,
                0,
                Some(std::slice::from_ref(&facet)),
            );
        });
        eprintln!("  facets:       avg={avg}us  p99={p99}us  (limit {P99_FACET_US}us)");
        assert!(
            p99 < P99_FACET_US,
            "facets P99 regression: {p99}us > {P99_FACET_US}us"
        );
    });
}

/// Assert that a combined text + filter + sort + facets query stays below the `P99_FULL_STACK_US` threshold.
///
/// Release-only. Exercises the most expensive realistic query path over the 1 K
/// regression corpus.
#[cfg(not(debug_assertions))]
#[test]
fn regression_full_stack_slow() {
    with_manager(|m| {
        let filter = Filter::Range {
            field: "price".into(),
            min: 200.0,
            max: 800.0,
        };
        let sort = Sort::ByField {
            field: "price".into(),
            order: SortOrder::Asc,
        };
        let facet = FacetRequest {
            field: "category".into(),
            path: "/electronics".into(),
            value_query: None,
        };
        let (avg, p99) = bench(200, || {
            let _ = m.search_with_facets(
                "regr",
                "samsung laptop",
                Some(&filter),
                Some(&sort),
                20,
                0,
                Some(std::slice::from_ref(&facet)),
            );
        });
        eprintln!("  full_stack:   avg={avg}us  p99={p99}us  (limit {P99_FULL_STACK_US}us)");
        assert!(
            p99 < P99_FULL_STACK_US,
            "full_stack P99 regression: {p99}us > {P99_FULL_STACK_US}us"
        );
    });
}

/// Assert that single-character and two-character prefix queries stay below the `P99_SHORT_QUERY_US` threshold.
///
/// Release-only. Tests both "s" and "sa" queries independently, each against
/// the 1 K regression corpus.
#[cfg(not(debug_assertions))]
#[test]
fn regression_short_query_slow() {
    with_manager(|m| {
        let (avg1, p99_1) = bench(200, || {
            let _ = m.search("regr", "s", None, None, 20);
        });
        eprintln!("  short_1char:  avg={avg1}us  p99={p99_1}us  (limit {P99_SHORT_QUERY_US}us)");
        assert!(
            p99_1 < P99_SHORT_QUERY_US,
            "short_query(1char) P99 regression: {p99_1}us > {P99_SHORT_QUERY_US}us"
        );

        let (avg2, p99_2) = bench(200, || {
            let _ = m.search("regr", "sa", None, None, 20);
        });
        eprintln!("  short_2char:  avg={avg2}us  p99={p99_2}us  (limit {P99_SHORT_QUERY_US}us)");
        assert!(
            p99_2 < P99_SHORT_QUERY_US,
            "short_query(2char) P99 regression: {p99_2}us > {P99_SHORT_QUERY_US}us"
        );
    });
}

/// Assert that a six-keystroke typeahead sequence with facets stays below the `P99_TYPEAHEAD_TOTAL_US` threshold.
///
/// Release-only. Simulates progressive prefix queries ("s" → "samsun") each
/// including a category facet request over the 1 K regression corpus. The budget
/// applies to the aggregate wall time of all six queries per iteration.
#[cfg(not(debug_assertions))]
#[test]
fn regression_typeahead_sequence_slow() {
    with_manager(|m| {
        let facet = FacetRequest {
            field: "category".into(),
            path: "/electronics".into(),
            value_query: None,
        };
        let prefixes = ["s", "sa", "sam", "sams", "samsu", "samsun"];

        for _ in 0..3 {
            for q in &prefixes {
                let _ = m.search_with_facets(
                    "regr",
                    q,
                    None,
                    None,
                    20,
                    0,
                    Some(std::slice::from_ref(&facet)),
                );
            }
        }

        let mut times: Vec<u64> = Vec::with_capacity(50);
        for _ in 0..50 {
            let t = std::time::Instant::now();
            for q in &prefixes {
                let _ = m.search_with_facets(
                    "regr",
                    q,
                    None,
                    None,
                    20,
                    0,
                    Some(std::slice::from_ref(&facet)),
                );
            }
            times.push(t.elapsed().as_micros() as u64);
        }
        times.sort_unstable();
        let avg = times.iter().sum::<u64>() / times.len() as u64;
        let p99 = times[(times.len() as f64 * 0.99) as usize];
        let per_key = avg / prefixes.len() as u64;
        eprintln!("  typeahead:    avg={avg}us  p99={p99}us  per_key={per_key}us  (limit {P99_TYPEAHEAD_TOTAL_US}us)");
        assert!(
            p99 < P99_TYPEAHEAD_TOTAL_US,
            "typeahead P99 regression: {p99}us > {P99_TYPEAHEAD_TOTAL_US}us (6 keystrokes)"
        );
    });
}

// ─── Bounded-executor frozen-matrix benchmark ───────────────────────────────
//
// Measures the bounded search executor on the same frozen fixture and frozen
// query families the parity suite owns, sweeping only the existing thread-count
// knob (`FLAPJACK_SEARCH_THREADS`). Stage 3 uses the machine-readable rows this
// emits to choose `DEFAULT_SEARCH_THREADS` and the in-flight budget from
// measured data rather than intuition. Nothing here defines a second corpus,
// catalog, or budget concept.

/// One benchmark measurement: a single frozen-family execute under one
/// thread/budget arm and sample index, carrying its full phase report.
struct BenchRecord {
    family: &'static str,
    query_label: &'static str,
    threads: usize,
    budget_per_worker: usize,
    sample: usize,
    report: QueryPhaseReport,
}

/// Process resources measured over one thread arm of the frozen matrix.
struct BenchResourceMeasurement {
    threads: usize,
    elapsed_ns: u128,
    cpu_usage_percent: f32,
    rss_before_bytes: u64,
    rss_after_bytes: u64,
}

struct BenchResourceMonitor {
    system: System,
    pid: sysinfo::Pid,
}

struct LocalThreadArmMeasurement {
    threads: usize,
    family: FrozenFamily,
    warm: LatencySummary,
    cold_primary: LatencySummary,
}

impl BenchResourceMonitor {
    fn new() -> Self {
        let mut system = System::new();
        let pid = get_current_pid().expect("current process id");
        assert!(
            system.refresh_process(pid),
            "current process must be visible to sysinfo"
        );
        Self { system, pid }
    }

    fn begin_arm(&mut self) -> u64 {
        assert!(
            self.system.refresh_process(self.pid),
            "current process must be visible to sysinfo"
        );
        self.current_process().memory()
    }

    fn finish_arm(
        &mut self,
        threads: usize,
        elapsed_ns: u128,
        rss_before_bytes: u64,
    ) -> BenchResourceMeasurement {
        assert!(
            self.system.refresh_process(self.pid),
            "current process must be visible to sysinfo"
        );
        let process = self.current_process();
        BenchResourceMeasurement {
            threads,
            elapsed_ns,
            cpu_usage_percent: process.cpu_usage(),
            rss_before_bytes,
            rss_after_bytes: process.memory(),
        }
    }

    fn current_process(&self) -> &sysinfo::Process {
        self.system
            .process(self.pid)
            .expect("refreshed current process")
    }
}

fn collect_frozen_matrix_arm(threads: usize, samples: usize) -> Vec<BenchRecord> {
    let mut records = Vec::new();
    let _env = SearchThreadsEnvGuard::set(&threads.to_string());
    for &family in FrozenFamily::ALL {
        let fixture: ExecutorParityFixture = build_parity_fixture();
        for sample in 0..samples {
            for (query_label, report) in run_frozen_family(family, &fixture) {
                records.push(BenchRecord {
                    family: family.label(),
                    query_label,
                    threads,
                    budget_per_worker: IN_FLIGHT_SEARCHES_PER_WORKER_THREAD,
                    sample,
                    report,
                });
            }
        }
    }
    records
}

fn collect_bounded_executor_ab(
    local_standard_manager: &IndexManager,
    thread_arms: &[usize],
    samples: usize,
) -> (
    Vec<BenchRecord>,
    Vec<LocalThreadArmMeasurement>,
    Vec<BenchResourceMeasurement>,
) {
    let mut monitor = BenchResourceMonitor::new();
    let mut records = Vec::new();
    let mut local_measurements = Vec::new();
    let mut resources = Vec::with_capacity(thread_arms.len());
    for &threads in thread_arms {
        let _env = SearchThreadsEnvGuard::set(&threads.to_string());
        let rss_before_bytes = monitor.begin_arm();
        let started = std::time::Instant::now();
        let arm_records = collect_frozen_matrix_arm(threads, samples);
        local_measurements.extend(
            measure_local_gate_matrix(local_standard_manager)
                .into_iter()
                .map(|measurement| LocalThreadArmMeasurement {
                    threads,
                    family: measurement.family,
                    warm: measurement.warm,
                    cold_primary: measurement.cold_primary,
                }),
        );
        let arm_resources =
            monitor.finish_arm(threads, started.elapsed().as_nanos(), rss_before_bytes);
        records.extend(arm_records);
        resources.push(arm_resources);
    }
    (records, local_measurements, resources)
}

fn print_resource_measurements(measurements: &[BenchResourceMeasurement]) {
    println!(
        "bench_resource_col\tthreads\telapsed_ns\tcpu_usage_percent\trss_before_bytes\trss_after_bytes\trss_delta_bytes"
    );
    for measurement in measurements {
        let rss_delta_bytes =
            i128::from(measurement.rss_after_bytes) - i128::from(measurement.rss_before_bytes);
        println!(
            "bench_resource\t{}\t{}\t{:.3}\t{}\t{}\t{}",
            measurement.threads,
            measurement.elapsed_ns,
            measurement.cpu_usage_percent,
            measurement.rss_before_bytes,
            measurement.rss_after_bytes,
            rss_delta_bytes,
        );
    }
}

fn print_local_thread_arm_measurements(measurements: &[LocalThreadArmMeasurement]) {
    println!("local_ab_col\tthreads\tfamily\twarm_p95_ms\tcold_primary_p95_ms");
    for measurement in measurements {
        println!(
            "local_ab\t{}\t{}\t{:.3}\t{:.3}",
            measurement.threads,
            measurement.family.label(),
            measurement.warm.p95_us / 1000.0,
            measurement.cold_primary.p95_us / 1000.0,
        );
    }
}

/// The execution paths the query executor can report. A benchmark row must
/// carry exactly one of these, so every cold/warm row stays attributable to a
/// real collector path.
const KNOWN_EXECUTION_PATHS: [&str; 5] = [
    "relevance",
    "relevance_facets",
    "sort_fast",
    "sort_fallback",
    "count_only",
];

/// Measure every frozen family under each thread-count arm.
///
/// A fresh fixture per (arm, family) makes sample 0's first execute a cold
/// searcher generation and every later execute on that fixture warm, so cold
/// and warm rows stay separable rather than blended. A dropped fixture keeps
/// its index-identity allocation reserved through the metrics tracker's weak
/// reference until the next observe purges it, so a freshly built fixture can
/// never reuse a still-tracked identity and be misread as warm.
fn collect_frozen_matrix_records(thread_arms: &[usize], samples: usize) -> Vec<BenchRecord> {
    let mut records = Vec::new();
    for &threads in thread_arms {
        records.extend(collect_frozen_matrix_arm(threads, samples));
    }
    records
}

/// Print the frozen-matrix measurements as tab-separated rows: one header line
/// then one `bench_row` per record with the family, thread/budget arm, sample,
/// cold flag, execution path, wall time (`total_ns`), and every phase field.
fn print_frozen_matrix_rows(records: &[BenchRecord]) {
    println!(
        "bench_col\tfamily\tquery\tthreads\tbudget_per_worker\tsample\tcold\texecution_path\ttotal_ns\tprepare_ns\tcollect_ns\trank_ns\tfetch_ns\tfacet_extract_ns\tunattributed_ns\tmatched_docs\tvisited_segments\tcandidates_collected\tfacet_cardinality"
    );
    for record in records {
        let phase = &record.report;
        println!(
            "bench_row\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            record.family,
            record.query_label,
            record.threads,
            record.budget_per_worker,
            record.sample,
            phase.cold,
            phase.execution_path,
            phase.total_ns,
            phase.prepare_ns,
            phase.collect_ns,
            phase.rank_ns,
            phase.fetch_ns,
            phase.facet_extract_ns,
            phase.unattributed_ns,
            phase.matched_docs,
            phase.visited_segments,
            phase.candidates_collected,
            phase.facet_cardinality,
        );
    }
}

/// Ignored release benchmark: measure the bounded executor across the frozen
/// query families at each thread-count arm and print machine-readable rows.
///
/// Kept `#[ignore]` so routine `cargo test -p flapjack --lib` never runs the
/// release matrix. Run it explicitly with:
///   cd engine && timeout 900 cargo test --release -p flapjack --lib -- \
///     bounded_executor_frozen_matrix_benchmark_slow --ignored --nocapture
#[test]
#[ignore]
#[serial_test::serial(flapjack_search_threads_env)]
fn bounded_executor_frozen_matrix_benchmark_slow() {
    const THREAD_ARMS: [usize; 3] = [1, 2, 4];
    const SAMPLES: usize = 30;

    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    let doc_count = local_standard_doc_count();
    let build = setup_local_standard_specimen(&manager, &rt, doc_count);
    let (records, local_measurements, resources) =
        collect_bounded_executor_ab(&manager, &THREAD_ARMS, SAMPLES);
    println!(
        "# bounded_executor_frozen_matrix_benchmark thread_arms={:?} samples={} budget_per_worker={} records={} local_standard_docs={} local_standard_build_s={:.3}",
        THREAD_ARMS,
        SAMPLES,
        IN_FLIGHT_SEARCHES_PER_WORKER_THREAD,
        records.len(),
        doc_count,
        build.elapsed_seconds,
    );
    print_resource_measurements(&resources);
    print_local_thread_arm_measurements(&local_measurements);
    print_frozen_matrix_rows(&records);
}

/// Fast guard on the benchmark record shape: it must emit one record per frozen
/// query per sample, preserve each report's `execution_path`, and keep cold and
/// warm samples separated rather than blended.
#[test]
#[serial_test::serial(flapjack_search_threads_env)]
fn frozen_matrix_benchmark_emits_one_record_per_family_sample_and_separates_cold_warm() {
    let thread_arms = [1usize];
    let samples = 2usize;
    let records = collect_frozen_matrix_records(&thread_arms, samples);
    assert!(!records.is_empty(), "benchmark must emit records");

    for record in &records {
        assert!(
            KNOWN_EXECUTION_PATHS.contains(&record.report.execution_path),
            "{}/{} carried unknown execution_path {:?}",
            record.family,
            record.query_label,
            record.report.execution_path,
        );
        assert!(!record.family.is_empty(), "family label must be present");
        assert!(
            !record.query_label.is_empty(),
            "query label must be present"
        );
    }

    for &threads in &thread_arms {
        for family in FrozenFamily::ALL {
            let group: Vec<&BenchRecord> = records
                .iter()
                .filter(|record| record.threads == threads && record.family == family.label())
                .collect();
            assert!(
                !group.is_empty(),
                "no records for {} arm {}",
                family.label(),
                threads
            );

            // One record per query per sample: every sample index yields the
            // same per-family record count.
            let per_sample = group.iter().filter(|record| record.sample == 0).count();
            assert!(
                per_sample >= 1,
                "{} arm {}: empty sample 0",
                family.label(),
                threads
            );
            for sample in 0..samples {
                let count = group
                    .iter()
                    .filter(|record| record.sample == sample)
                    .count();
                assert_eq!(
                    count,
                    per_sample,
                    "{} arm {}: sample {} record count diverged",
                    family.label(),
                    threads,
                    sample
                );
            }

            // Cold/warm separation: exactly one cold specimen — the first
            // execute of sample 0 — and no warm sample is misreported cold.
            let cold_count = group.iter().filter(|record| record.report.cold).count();
            assert_eq!(
                cold_count,
                1,
                "{} arm {}: expected exactly one cold specimen",
                family.label(),
                threads
            );
            assert!(
                group[0].report.cold && group[0].sample == 0,
                "{} arm {}: cold specimen must be the first sample-0 execute",
                family.label(),
                threads
            );
            for warm in group.iter().filter(|record| record.sample > 0) {
                assert!(
                    !warm.report.cold,
                    "{} arm {}: warm sample {} marked cold",
                    family.label(),
                    threads,
                    warm.sample
                );
            }
        }
    }
}
