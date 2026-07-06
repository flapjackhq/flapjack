//! Criterion benchmarks for core IndexManager operations: search queries, document indexing, and tenant export/import.
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use flapjack::index::SearchOptions;
use flapjack::{Document, FacetRequest, FieldValue, Filter, IndexManager, Sort, SortOrder};
use std::collections::HashMap;
use tempfile::TempDir;

/// Create a tenant with synthetic product documents for benchmarking.
///
/// Populates the tenant with `num_docs` documents, each containing title, description,
/// category (facet), and price fields.
///
/// # Arguments
///
/// * `manager` - The index manager to create the tenant in.
/// * `tenant_id` - Identifier for the new tenant.
/// * `num_docs` - Number of documents to generate and index.
///
/// # Panics
///
/// Panics if tenant creation or document insertion fails.
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
            FieldValue::Text(format!("Laptop Gaming Product {}", i)),
        );
        doc.fields.insert(
            "description".to_string(),
            FieldValue::Text(format!("High performance gaming laptop description {}", i)),
        );
        doc.fields.insert(
            "category".to_string(),
            FieldValue::Facet("/electronics/computers".to_string()),
        );
        doc.fields.insert(
            "price".to_string(),
            FieldValue::Integer((100 + i * 10) as i64),
        );
        docs.push(doc);
    }

    manager.add_documents(tenant_id, docs).unwrap();
}

/// Generate a batch of synthetic product documents for indexing benchmarks.
///
/// # Arguments
///
/// * `count` - Number of documents to create.
///
/// # Returns
///
/// A `Vec<Document>` with title, description, category, and price fields populated.
fn setup_docs(count: usize) -> Vec<Document> {
    let mut docs = Vec::new();
    for i in 0..count {
        let mut doc = Document {
            id: format!("doc_{}", i),
            fields: HashMap::new(),
        };
        doc.fields.insert(
            "title".to_string(),
            FieldValue::Text(format!("Product {}", i)),
        );
        doc.fields.insert(
            "description".to_string(),
            FieldValue::Text(format!("Description text {}", i)),
        );
        doc.fields.insert(
            "category".to_string(),
            FieldValue::Facet("/electronics".to_string()),
        );
        doc.fields
            .insert("price".to_string(), FieldValue::Integer((50 + i) as i64));
        docs.push(doc);
    }
    docs
}

/// Benchmark search query performance across five scenarios.
///
/// Runs text-only, range-filtered, sorted, faceted, and full-stack queries
/// against a 5,000-document tenant. Each scenario isolates a different
/// combination of search features to identify bottlenecks.
fn bench_query(c: &mut Criterion) {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());

    setup_tenant(&manager, "bench", 5000);

    let mut group = c.benchmark_group("query");
    group.throughput(Throughput::Elements(5000));

    group.bench_function("text_only", |b| {
        b.iter(|| manager.search("bench", "laptop", None, None, 10))
    });

    group.bench_function("filter_range", |b| {
        b.iter(|| {
            let filter = Filter::Range {
                field: "price".to_string(),
                min: 200.0,
                max: 800.0,
            };
            manager.search("bench", "laptop", Some(&filter), None, 10)
        })
    });

    group.bench_function("text_plus_sort", |b| {
        b.iter(|| {
            let sort = Sort::ByField {
                field: "price".to_string(),
                order: SortOrder::Asc,
            };
            manager.search("bench", "laptop", None, Some(&sort), 10)
        })
    });

    group.bench_function("text_plus_facet", |b| {
        b.iter(|| {
            let facet = FacetRequest {
                field: "category".to_string(),
                path: "/electronics".to_string(),
                value_query: None,
            };
            let options = SearchOptions {
                limit: 10,
                offset: 0,
                facets: Some(&[facet]),
                ..Default::default()
            };
            manager.search_with_options("bench", "laptop", &options)
        })
    });

    group.bench_function("full_stack", |b| {
        b.iter(|| {
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
                path: "/electronics".to_string(),
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
            manager.search_with_options("bench", "laptop", &options)
        })
    });

    group.finish();
}

/// Benchmark document ingestion and commit for varying batch sizes.
///
/// Tests `add_documents` with batches of 10, 100, and 500 documents,
/// each on a freshly created tenant to measure cold-path indexing cost.
fn bench_indexing(c: &mut Criterion) {
    let mut group = c.benchmark_group("indexing");

    for batch_size in [10, 100, 500] {
        group.bench_with_input(
            BenchmarkId::new("batch_commit", batch_size),
            &batch_size,
            |b, &size| {
                b.iter_batched(
                    || {
                        let temp = TempDir::new().unwrap();
                        let manager = IndexManager::new(temp.path());
                        manager.create_tenant("bench").unwrap();
                        (temp, manager, setup_docs(size))
                    },
                    |(temp, manager, docs)| {
                        manager.add_documents("bench", docs).unwrap();
                        drop(temp);
                    },
                    criterion::BatchSize::SmallInput,
                )
            },
        );
    }

    group.finish();
}

/// Benchmark tenant export and import operations on a 5,000-document index.
///
/// Measures `export_tenant` and `import_tenant` throughput using fresh
/// temporary directories for each iteration to avoid cache effects.
fn bench_migration(c: &mut Criterion) {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    setup_tenant(&manager, "bench", 5000);

    c.bench_function("export_5k_docs", |b| {
        b.iter_batched(
            || TempDir::new().unwrap(),
            |export_dir| {
                let export_path = export_dir.path().join("bench");
                manager
                    .export_tenant(&"bench".to_string(), export_path)
                    .unwrap();
            },
            criterion::BatchSize::SmallInput,
        )
    });

    let export_temp = TempDir::new().unwrap();
    let export_path = export_temp.path().join("bench");
    manager
        .export_tenant(&"bench".to_string(), export_path.clone())
        .unwrap();

    c.bench_function("import_5k_docs", |b| {
        b.iter_batched(
            || {
                let import_temp = TempDir::new().unwrap();
                let import_manager = IndexManager::new(import_temp.path());
                (import_temp, import_manager)
            },
            |(temp, import_manager)| {
                import_manager
                    .import_tenant(&"bench".to_string(), &export_path)
                    .unwrap();
                drop(temp);
            },
            criterion::BatchSize::SmallInput,
        )
    });
}

criterion_group!(benches, bench_query, bench_indexing, bench_migration);
criterion_main!(benches);
