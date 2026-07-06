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
fn setup_tenant(manager: &IndexManager, tenant_id: &str, num_docs: usize) {
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

    manager.add_documents(tenant_id, docs).unwrap();
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
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    setup_tenant(&manager, "bench", 10_000);

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
}

criterion_group!(regression_guards, query_p99_budget);
criterion_main!(regression_guards);
