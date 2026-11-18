//! Criterion regression-guard benchmarks that enforce p99 latency budgets for search, faceted search, and full-stack queries against a synthetic 10k-document index.
use criterion::{black_box, criterion_group, criterion_main, Criterion};
use flapjack::index::SearchOptions;
use flapjack::{Document, FacetRequest, FieldValue, IndexManager};
use std::collections::HashMap;
use tempfile::TempDir;

/// Create a tenant and populate it with synthetic product documents for benchmarking.
///
/// Generates `num_docs` documents each containing title, description, category facet, and price fields.
/// Categories cycle through 100 buckets (`/cat0`–`/cat99`); prices start at 100 and increment by 5.
///
/// # Panics
///
/// Panics if tenant creation or document ingestion fails.
async fn setup_tenant(manager: &IndexManager, tenant_id: &str, num_docs: usize) {
    manager.create_tenant(tenant_id).unwrap();

    let mut docs = Vec::new();
    for i in 0..num_docs {
        let mut doc = Document {
            id: format!("doc_{}", i),
            fields: HashMap::new(),
        };
        doc.fields.insert(
            "title".to_string(),
            FieldValue::Text(format!("Laptop Product {}", i)),
        );
        doc.fields.insert(
            "description".to_string(),
            FieldValue::Text(format!("Gaming laptop description {}", i)),
        );
        doc.fields.insert(
            "category".to_string(),
            FieldValue::Facet(format!("/cat{}", i % 100)),
        );
        doc.fields.insert(
            "price".to_string(),
            FieldValue::Integer((100 + i * 5) as i64),
        );
        docs.push(doc);
    }

    manager.add_documents_sync(tenant_id, docs).await.unwrap();
}

/// Benchmark faceted search, text search, and full-stack query latency against p99 budgets.
///
/// Seeds a 10 000-document tenant then runs three sub-benchmarks with 500 samples over 30 s each:
/// - `facet_p99_under_50ms` — category facet query
/// - `text_search_p99_under_50ms` — multi-term text search
/// - `full_stack_p99_under_50ms` — text search with range filter, sort, and facets
///
/// Results are intended for CI regression gates that assert p99 latency stays below 50 ms.
fn query_p99_budget(c: &mut Criterion) {
    // IndexManager owns asynchronous task/write queues even though these
    // benchmark calls are synchronous. Keep their real runtime alive throughout.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        if std::env::var_os("FLAPJACK_RECALL_BENCH").is_some() {
            fuzzy_recall_p99_budget(c).await;
            return;
        }
        let temp = TempDir::new().unwrap();
        let manager = IndexManager::new(temp.path());
        setup_tenant(&manager, "bench", 10_000).await;

        let mut group = c.benchmark_group("p99_budgets");
        group.sample_size(500);
        group.measurement_time(std::time::Duration::from_secs(30));

        group.bench_function("facet_p99_under_50ms", |b| {
            b.iter(|| {
                let facet = FacetRequest {
                    field: "category".to_string(),
                    path: "/cat".to_string(),
                    value_query: None,
                };
                let options = SearchOptions {
                    limit: 10,
                    offset: 0,
                    facets: Some(&[facet]),
                    ..Default::default()
                };
                black_box(manager.search_with_options("bench", "laptop", &options))
            })
        });

        group.bench_function("text_search_p99_under_50ms", |b| {
            b.iter(|| black_box(manager.search("bench", "laptop gaming", None, None, 10)))
        });

        group.bench_function("full_stack_p99_under_50ms", |b| {
            b.iter(|| {
                let filter = flapjack::Filter::Range {
                    field: "price".to_string(),
                    min: 200.0,
                    max: 800.0,
                };
                let sort = flapjack::Sort::ByField {
                    field: "price".to_string(),
                    order: flapjack::SortOrder::Asc,
                };
                let facet = FacetRequest {
                    field: "category".to_string(),
                    path: "/cat".to_string(),
                    value_query: None,
                };
                let options = SearchOptions {
                    filter: Some(&filter),
                    sort: Some(&sort),
                    limit: 10,
                    offset: 0,
                    facets: Some(&[facet]),
                    ..Default::default()
                };
                black_box(manager.search_with_options("bench", "laptop", &options))
            })
        });

        group.finish();
        fuzzy_recall_p99_budget(c).await;
    });
}

/// Faithful pre-fix builder for this four-token benchmark only. Both arms use
/// every path for exact matching; the old arm limits *only fuzzy* clauses to two.
/// Keep this baseline local to the benchmark so a correctness cap cannot return
/// to production behind a configuration switch.
fn legacy_capped_query(
    schema: &tantivy::schema::Schema,
    paths: &[String],
    text: &str,
) -> Box<dyn tantivy::query::Query> {
    use tantivy::query::{BooleanQuery, FuzzyTermQuery, Occur, Query, TermQuery};
    use tantivy::schema::IndexRecordOption::WithFreqsAndPositions;
    let search = schema.get_field("_json_search").unwrap();
    let exact = schema.get_field("_json_exact").unwrap();
    let tokens: Vec<_> = text.split_whitespace().collect();
    let words = tokens
        .iter()
        .enumerate()
        .map(|(token_idx, token)| {
            let prefix = token_idx == tokens.len() - 1;
            let fields = paths
                .iter()
                .enumerate()
                .map(|(path_idx, path)| {
                    let term_text = format!("{}\0s{}", path, token);
                    let term = tantivy::Term::from_field_text(
                        if prefix { search } else { exact },
                        &term_text,
                    );
                    let mut clauses: Vec<(Occur, Box<dyn Query>)> = vec![(
                        Occur::Should,
                        Box::new(TermQuery::new(term, WithFreqsAndPositions)),
                    )];
                    if path_idx < 2 {
                        let distance = if token.len() >= 8 {
                            2
                        } else if token.len() >= 4 {
                            1
                        } else {
                            0
                        };
                        if distance > 0 {
                            clauses.push((
                                Occur::Should,
                                Box::new(FuzzyTermQuery::new(
                                    tantivy::Term::from_field_text(exact, &term_text),
                                    distance,
                                    true,
                                )),
                            ));
                            if prefix {
                                clauses.push((
                                    Occur::Should,
                                    Box::new(TermQuery::new(
                                        tantivy::Term::from_field_text(
                                            search,
                                            &format!("{}\0s{}", path, &token[1..]),
                                        ),
                                        WithFreqsAndPositions,
                                    )),
                                ));
                            }
                        }
                    }
                    (
                        Occur::Should,
                        Box::new(BooleanQuery::new(clauses)) as Box<dyn Query>,
                    )
                })
                .collect();
            (
                Occur::Must,
                Box::new(BooleanQuery::new(fields)) as Box<dyn Query>,
            )
        })
        .collect();
    Box::new(BooleanQuery::new(words))
}

/// Query-time cost guard: >=100k DISTINCT exact dictionary terms over 12 text
/// attributes, four terms and one typo, including matches in the last attribute.
/// Run alone: FLAPJACK_RECALL_BENCH=1 cargo bench --bench regression_guards.
/// The explicit PASS line is new-arm p99 <50ms (not Criterion's mean).
async fn fuzzy_recall_p99_budget(c: &mut Criterion) {
    use flapjack::{Query, QueryExecutor, QueryParser};
    use std::{collections::HashSet, time::Instant};
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    manager.create_tenant("recall_bench").unwrap();
    let paths: Vec<String> = [
        "title",
        "brand",
        "category",
        "description",
        "manufacturer",
        "material",
        "specification",
        "inventory",
        "details",
        "features",
        "summary",
        "narrative",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    let mut docs = Vec::new();
    for i in 0..10_000_u64 {
        let fields = paths
            .iter()
            .enumerate()
            .map(|(j, path)| {
                // Four English prefix neighborhoods force fuzzy dictionary traversal
                // near the timed vocabulary, instead of pruning irrelevant random IDs.
                // 137 is coprime to 26^4, so each neighborhood has unique suffixes.
                let ordinal = i * 12 + j as u64;
                let mut value = (ordinal * 137) % 26_u64.pow(4);
                let mut suffix = [b'a'; 4];
                for byte in &mut suffix {
                    *byte += (value % 26) as u8;
                    value /= 26;
                }
                // One neighborhood per noise document prevents accidental four-word
                // matches; every attribute still contains all four neighborhoods.
                let neighborhood = ["trai", "wate", "memb", "moun"][(i % 4) as usize];
                (
                    path.clone(),
                    FieldValue::Text(format!(
                        "{}{}",
                        neighborhood,
                        std::str::from_utf8(&suffix).unwrap()
                    )),
                )
            })
            .collect();
        docs.push(Document {
            id: format!("noise{i}"),
            fields,
        });
    }
    for (id, path) in [("early", &paths[0]), ("late", &paths[11])] {
        docs.push(Document {
            id: id.into(),
            fields: [(
                path.clone(),
                FieldValue::Text("trailing waterproof membrane mountain".into()),
            )]
            .into_iter()
            .collect(),
        });
    }
    manager
        .add_documents_sync("recall_bench", docs)
        .await
        .unwrap();
    let index = manager.get_or_load("recall_bench").unwrap();
    let schema = index.inner().schema();
    let searcher = index.reader().searcher();
    let mut dictionary = HashSet::new();
    for segment in searcher.segment_readers() {
        let inverted = segment
            .inverted_index(schema.get_field("_json_exact").unwrap())
            .unwrap();
        let mut terms = inverted.terms().stream().unwrap();
        while terms.advance() {
            let bytes = terms.key();
            if let Some(pos) = bytes.windows(2).position(|w| w == b"\0s") {
                dictionary.insert(bytes[pos + 2..].to_vec());
            }
        }
    }
    assert!(
        dictionary.len() >= 100_000,
        "actual distinct terms: {}",
        dictionary.len()
    );
    assert_eq!(index.searchable_paths().len(), 12);
    println!(
        "RECALL_CORPUS documents=10002 attributes=12 distinct_terms={}",
        dictionary.len()
    );
    let parser = QueryParser::new_with_weights(
        &schema,
        vec![schema.get_field("_json_search").unwrap()],
        vec![1.0; paths.len()],
        paths.clone(),
    )
    .with_exact_field(schema.get_field("_json_exact").unwrap());
    let executor = QueryExecutor::new(index.converter(), schema.clone());
    let queries = [
        "trailing waterprof membrane mountain",
        "trailnig waterproof membrane mountain",
        "trailing waterproof membrnae mountain",
        "trailing waterproof membrane mountian",
    ];
    let run = |old: bool, text: &str| {
        let query = if old {
            legacy_capped_query(&schema, &paths, text)
        } else {
            parser.parse(&Query { text: text.into() }).unwrap()
        };
        let result = executor.execute(&searcher, query, None, 10).unwrap();
        let mut ids: Vec<_> = result
            .documents
            .iter()
            .map(|doc| doc.document.id.as_str())
            .collect();
        ids.sort();
        if old {
            assert_eq!(ids, ["early"], "incorrect old cap must be visible");
        } else {
            assert_eq!(ids, ["early", "late"], "full recall is mandatory");
        }
        black_box(result);
    };
    let run_native = |text: &str| {
        let result = manager
            .search("recall_bench", text, None, None, 10)
            .unwrap();
        let mut ids: Vec<_> = result
            .documents
            .iter()
            .map(|doc| doc.document.id.as_str())
            .collect();
        ids.sort();
        assert_eq!(
            ids,
            ["early", "late"],
            "native manager must preserve full recall"
        );
        black_box(result);
    };
    for _ in 0..8 {
        for text in &queries {
            run(true, text);
            run(false, text);
            run_native(text);
        }
    }
    let mut old_samples = Vec::new();
    let mut new_samples = Vec::new();
    let mut native_samples = Vec::new();
    // Interleave and alternate first arm to reduce drift/order effects. Both
    // arms parse and execute the same full-path corpus on this one process.
    for i in 0..400 {
        for old in if i % 2 == 0 {
            [true, false]
        } else {
            [false, true]
        } {
            let started = Instant::now();
            run(old, queries[i % queries.len()]);
            let elapsed = started.elapsed().as_secs_f64() * 1000.0;
            if old {
                old_samples.push(elapsed);
            } else {
                new_samples.push(elapsed);
            }
        }
        let started = Instant::now();
        run_native(queries[i % queries.len()]);
        native_samples.push(started.elapsed().as_secs_f64() * 1000.0);
    }
    old_samples.sort_by(f64::total_cmp);
    new_samples.sort_by(f64::total_cmp);
    native_samples.sort_by(f64::total_cmp);
    let p99 = |samples: &[f64]| samples[(samples.len() * 99).div_ceil(100) - 1];
    println!("RECALL_AB samples=400 old_capped_incorrect_core_p99_ms={:.3} new_full_recall_core_p99_ms={:.3} new_native_manager_p99_ms={:.3} pass_line=p99<50ms", p99(&old_samples), p99(&new_samples), p99(&native_samples));
    assert!(p99(&new_samples) < 50.0, "full-recall p99 must be <50ms");
    assert!(
        p99(&native_samples) < 50.0,
        "native full-recall manager p99 must be <50ms"
    );
    c.bench_function(
        "four_term_typo_12_attributes_100k_terms_p99_under_50ms",
        |b| b.iter(|| run_native(queries[0])),
    );
}

criterion_group!(regression_guards, query_p99_budget);
criterion_main!(regression_guards);
