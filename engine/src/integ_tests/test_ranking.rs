//! Consolidated integration tests for ranking behavior including explicit field sorting, attribute-for-distinct deduplication, custom ranking settings, typo tiers, BM25 tuning, exact-vs-prefix tiers, and attribute bucket hard tiers.

use crate::index::settings::{DistinctValue, IndexSettings};
use crate::integ_tests::search_compat::SearchCompat;
use crate::types::{Document, FieldValue, Filter, Sort, SortOrder};
use crate::IndexManager;
use serde_json::json;
use std::sync::Arc;
use tempfile::TempDir;

// ============================================================
// Helpers (from test_sort.rs)
// ============================================================

struct SortFixture {
    _tmp: TempDir,
    mgr: Arc<IndexManager>,
}

static PRICE_FIXTURE: tokio::sync::OnceCell<SortFixture> = tokio::sync::OnceCell::const_new();

/// Return a lazily-initialized shared fixture containing five documents with ascending prices (5–200) across two brands.
///
/// The fixture is created once via a `tokio::sync::OnceCell` and reused across all sort tests that operate on the same dataset. The backing `TempDir` is kept alive for the lifetime of the process.
async fn get_price_fixture() -> &'static SortFixture {
    PRICE_FIXTURE.get_or_init(|| async {
        let tmp = TempDir::new().unwrap();
        let mgr = IndexManager::new(tmp.path());
        mgr.create_tenant("test").unwrap();

        let docs: Vec<Document> = vec![
            json!({"_id": "1", "objectID": "1", "title": "Cheap Widget", "price": 10, "brand": "Acme"}),
            json!({"_id": "2", "objectID": "2", "title": "Mid Widget", "price": 50, "brand": "Acme"}),
            json!({"_id": "3", "objectID": "3", "title": "Expensive Widget", "price": 100, "brand": "Luxe"}),
            json!({"_id": "4", "objectID": "4", "title": "Budget Item", "price": 5, "brand": "Value"}),
            json!({"_id": "5", "objectID": "5", "title": "Premium Item", "price": 200, "brand": "Luxe"}),
        ].into_iter().map(|v| Document::from_json(&v).unwrap()).collect();
        mgr.add_documents_sync("test", docs).await.unwrap();

        SortFixture { _tmp: tmp, mgr }
    }).await
}

fn sort_asc(field: &str) -> Sort {
    Sort::ByField {
        field: field.to_string(),
        order: SortOrder::Asc,
    }
}

fn sort_desc(field: &str) -> Sort {
    Sort::ByField {
        field: field.to_string(),
        order: SortOrder::Desc,
    }
}

fn get_field_i64(doc: &Document, field: &str) -> Option<i64> {
    doc.fields.get(field).and_then(|v| match v {
        crate::types::FieldValue::Integer(n) => Some(*n),
        _ => None,
    })
}

fn get_field_str<'a>(doc: &'a Document, field: &str) -> Option<&'a str> {
    doc.fields.get(field).and_then(|v| v.as_text())
}

// Helper (from test_distinct.rs)
fn create_doc(id: &str, name: &str, product_id: &str, popularity: i64) -> Document {
    let mut fields = std::collections::HashMap::new();
    fields.insert("name".to_string(), FieldValue::Text(name.to_string()));
    if !product_id.is_empty() {
        fields.insert(
            "product_id".to_string(),
            FieldValue::Text(product_id.to_string()),
        );
    }
    fields.insert("popularity".to_string(), FieldValue::Integer(popularity));
    Document {
        id: id.to_string(),
        fields,
    }
}

// ============================================================
// From test_sort.rs — explicit sort-by-field tests
// ============================================================

#[tokio::test]
async fn test_sort_price_asc() {
    let f = get_price_fixture().await;
    let sort = sort_asc("price");
    let results = f.mgr.search("test", "", None, Some(&sort), 100).unwrap();
    let prices: Vec<i64> = results
        .documents
        .iter()
        .filter_map(|d| get_field_i64(&d.document, "price"))
        .collect();
    assert_eq!(prices, vec![5, 10, 50, 100, 200]);
}

#[tokio::test]
async fn test_sort_price_desc() {
    let f = get_price_fixture().await;
    let sort = sort_desc("price");
    let results = f.mgr.search("test", "", None, Some(&sort), 100).unwrap();
    let prices: Vec<i64> = results
        .documents
        .iter()
        .filter_map(|d| get_field_i64(&d.document, "price"))
        .collect();
    assert_eq!(prices, vec![200, 100, 50, 10, 5]);
}

#[tokio::test]
async fn test_sort_with_text_query() {
    let f = get_price_fixture().await;
    let sort = sort_asc("price");
    let results = f
        .mgr
        .search("test", "widget", None, Some(&sort), 100)
        .unwrap();
    assert_eq!(results.documents.len(), 3, "Should match 3 widgets");
    let prices: Vec<i64> = results
        .documents
        .iter()
        .filter_map(|d| get_field_i64(&d.document, "price"))
        .collect();
    assert_eq!(prices, vec![10, 50, 100]);
}

/// Verify that sorting combined with a numeric greater-than-or-equal filter returns only matching documents in the requested sort order.
#[tokio::test]
async fn test_sort_with_numeric_filter() {
    let f = get_price_fixture().await;
    let sort = sort_desc("price");
    let filter = Filter::GreaterThanOrEqual {
        field: "price".into(),
        value: FieldValue::Integer(50),
    };
    let results = f
        .mgr
        .search("test", "", Some(&filter), Some(&sort), 100)
        .unwrap();
    assert_eq!(results.documents.len(), 3);
    let prices: Vec<i64> = results
        .documents
        .iter()
        .filter_map(|d| get_field_i64(&d.document, "price"))
        .collect();
    assert_eq!(prices, vec![200, 100, 50]);
}

#[tokio::test]
async fn test_sort_string_field() {
    let f = get_price_fixture().await;
    let sort = sort_asc("title");
    let results = f.mgr.search("test", "", None, Some(&sort), 100).unwrap();
    let titles: Vec<&str> = results
        .documents
        .iter()
        .filter_map(|d| get_field_str(&d.document, "title"))
        .collect();
    assert_eq!(titles[0], "Budget Item");
    assert_eq!(titles[1], "Cheap Widget");
}

/// Verify that documents missing the sort field are placed first when sorting in ascending order, rather than causing a panic or being dropped.
#[tokio::test]
async fn test_sort_missing_field_handled() {
    let tmp = TempDir::new().unwrap();
    let mgr = IndexManager::new(tmp.path());
    mgr.create_tenant("test").unwrap();

    let docs: Vec<Document> = vec![
        json!({"_id": "1", "objectID": "1", "title": "Has Price", "price": 50}),
        json!({"_id": "2", "objectID": "2", "title": "No Price"}),
        json!({"_id": "3", "objectID": "3", "title": "Also Has Price", "price": 10}),
    ]
    .into_iter()
    .map(|v| Document::from_json(&v).unwrap())
    .collect();
    mgr.add_documents_sync("test", docs).await.unwrap();

    let sort = sort_asc("price");
    let results = mgr.search("test", "", None, Some(&sort), 100).unwrap();
    assert_eq!(results.documents.len(), 3);
    let first_title = get_field_str(&results.documents[0].document, "title").unwrap();
    assert_eq!(first_title, "No Price", "Missing values sort first in asc");
}

/// Verify that sorting works on dot-separated nested field paths (e.g. `meta.score`), ordering documents by the nested value.
#[tokio::test]
async fn test_sort_nested_field() {
    let tmp = TempDir::new().unwrap();
    let mgr = IndexManager::new(tmp.path());
    mgr.create_tenant("test").unwrap();

    let docs: Vec<Document> = vec![
        json!({"_id": "1", "objectID": "1", "meta": {"score": 75}}),
        json!({"_id": "2", "objectID": "2", "meta": {"score": 25}}),
        json!({"_id": "3", "objectID": "3", "meta": {"score": 100}}),
    ]
    .into_iter()
    .map(|v| Document::from_json(&v).unwrap())
    .collect();
    mgr.add_documents_sync("test", docs).await.unwrap();

    let sort = sort_desc("meta.score");
    let results = mgr.search("test", "", None, Some(&sort), 100).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();
    assert_eq!(ids, vec!["3", "1", "2"]);
}

/// Verify that explicit sort-by-field works correctly on floating-point values, ordering documents by descending `rating`.
#[tokio::test]
async fn test_sort_float_field() {
    let tmp = TempDir::new().unwrap();
    let mgr = IndexManager::new(tmp.path());
    mgr.create_tenant("test").unwrap();

    let docs: Vec<Document> = vec![
        json!({"_id": "1", "objectID": "1", "rating": 4.5}),
        json!({"_id": "2", "objectID": "2", "rating": 3.2}),
        json!({"_id": "3", "objectID": "3", "rating": 4.9}),
    ]
    .into_iter()
    .map(|v| Document::from_json(&v).unwrap())
    .collect();
    mgr.add_documents_sync("test", docs).await.unwrap();

    let sort = sort_desc("rating");
    let results = mgr.search("test", "", None, Some(&sort), 100).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();
    assert_eq!(ids, vec!["3", "1", "2"]);
}

// ============================================================
// From test_distinct.rs — attribute_for_distinct tests
// ============================================================

/// Verify that `attribute_for_distinct` with `distinct=true` collapses multiple documents sharing the same `product_id` into a single representative, keeping only the highest-ranked variant.
#[tokio::test]
async fn test_distinct_deduplicates_variants() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Bool(true)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let docs = vec![
        create_doc("1", "Laptop Red", "laptop-1", 100),
        create_doc("2", "Laptop Blue", "laptop-1", 90),
        create_doc("3", "Laptop Green", "laptop-1", 80),
        create_doc("4", "Mouse Red", "mouse-1", 50),
        create_doc("5", "Mouse Blue", "mouse-1", 40),
    ];
    manager.add_documents_sync("test", docs).await?;

    let result_empty = manager.search("test", "", None, None, 10)?;
    eprintln!("EMPTY query: {} docs", result_empty.documents.len());
    for doc in &result_empty.documents {
        eprintln!(
            "  Doc {}: {:?}",
            doc.document.id,
            doc.document.fields.keys().collect::<Vec<_>>()
        );
    }

    let result_lap = manager.search("test", "lap", None, None, 10)?;
    eprintln!("'lap' query: {} docs", result_lap.documents.len());

    let result_laptop = manager.search("test", "laptop", None, None, 10)?;
    eprintln!("'laptop' query: {} docs", result_laptop.documents.len());

    let result_red = manager.search("test", "red", None, None, 10)?;
    eprintln!("'red' query: {} docs", result_red.documents.len());

    let index = manager.get_or_load("test")?;
    let reader = index.reader();
    reader.reload()?;
    let searcher = reader.searcher();
    let schema = index.inner().schema();
    let json_search = schema.get_field("_json_search").unwrap();

    eprintln!("\nIndexed terms sample:");
    let segment = &searcher.segment_readers()[0];
    let inv = segment.inverted_index(json_search).unwrap();
    let mut terms = inv.terms().stream().unwrap();
    let mut count = 0;
    while terms.advance() && count < 20 {
        let term = String::from_utf8_lossy(terms.key());
        eprintln!("  {}", term);
        count += 1;
    }

    let result_without_distinct = manager.search("test", "laptop", None, None, 10)?;
    eprintln!(
        "\n'laptop' query: {} docs",
        result_without_distinct.documents.len()
    );

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        None,
        None,
        10,
        0,
        None,
        Some(1),
    )?;

    eprintln!(
        "WITH distinct: {} docs, total={}",
        result.documents.len(),
        result.total
    );

    assert_eq!(result.total, 1, "Should count 1 group (laptop product)");
    assert_eq!(
        result.documents.len(),
        1,
        "Should return 1 doc (top variant)"
    );
    assert_eq!(result.documents[0].document.id, "1");

    Ok(())
}

/// Verify that `distinct` set to an integer N returns up to N documents per distinct group, ranked by custom ranking within each group.
#[tokio::test]
async fn test_distinct_keeps_top_n_per_group() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Integer(2)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let docs = vec![
        create_doc("1", "Laptop Red", "laptop-1", 100),
        create_doc("2", "Laptop Blue", "laptop-1", 90),
        create_doc("3", "Laptop Green", "laptop-1", 80),
        create_doc("4", "Laptop Yellow", "laptop-1", 70),
    ];
    manager.add_documents_sync("test", docs).await?;

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        None,
        None,
        10,
        0,
        None,
        Some(2),
    )?;

    assert_eq!(result.total, 1, "Should count 1 group");
    assert_eq!(result.documents.len(), 2, "Should return top 2 variants");
    assert_eq!(
        result.documents[0].document.id, "1",
        "Highest popularity first"
    );
    assert_eq!(result.documents[1].document.id, "2", "Second highest");

    Ok(())
}

/// Verify that setting `distinct` to `false` (or passing `distinct_level=0`) bypasses deduplication and returns all matching documents.
#[tokio::test]
async fn test_distinct_disabled_returns_all() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Bool(false)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let docs = vec![
        create_doc("1", "Laptop Red", "laptop-1", 100),
        create_doc("2", "Laptop Blue", "laptop-1", 90),
        create_doc("3", "Laptop Green", "laptop-1", 80),
    ];
    manager.add_documents_sync("test", docs).await?;

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        None,
        None,
        10,
        0,
        None,
        Some(0),
    )?;

    assert_eq!(result.total, 3, "Should count all docs");
    assert_eq!(result.documents.len(), 3, "Should return all variants");

    Ok(())
}

/// Verify that documents lacking the `attribute_for_distinct` field are excluded from distinct-grouped results rather than forming their own group.
#[tokio::test]
async fn test_distinct_missing_field_skips_doc() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Bool(true)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let doc1 = create_doc("1", "Laptop Red", "laptop-1", 100);
    let doc2 = create_doc("2", "Laptop Blue", "laptop-1", 90);
    let mut doc3 = create_doc("3", "Mouse", "", 50);
    doc3.fields.remove("product_id");

    manager
        .add_documents_sync("test", vec![doc1, doc2, doc3])
        .await?;

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        None,
        None,
        10,
        0,
        None,
        Some(1),
    )?;

    assert_eq!(
        result.documents.len(),
        1,
        "Doc without product_id should be skipped"
    );
    assert_eq!(result.documents[0].document.id, "1");

    Ok(())
}

/// Verify that distinct grouping works correctly on integer-valued fields, grouping documents with the same integer `category_id` together.
#[tokio::test]
async fn test_distinct_numeric_field_rounds() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("category_id".to_string()),
        distinct: Some(DistinctValue::Bool(true)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let mut doc1 = create_doc("1", "Laptop Red", "", 100);
    doc1.fields
        .insert("category_id".to_string(), FieldValue::Integer(42));

    let mut doc2 = create_doc("2", "Laptop Blue", "", 90);
    doc2.fields
        .insert("category_id".to_string(), FieldValue::Integer(42));

    let mut doc3 = create_doc("3", "Mouse", "", 50);
    doc3.fields
        .insert("category_id".to_string(), FieldValue::Integer(99));

    manager
        .add_documents_sync("test", vec![doc1, doc2, doc3])
        .await?;

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        None,
        None,
        10,
        0,
        None,
        Some(1),
    )?;

    assert_eq!(result.total, 1, "Should group by integer category_id");
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");

    Ok(())
}

/// Verify that distinct grouping on float-valued fields rounds to a common bucket, so documents with close but not identical float values (e.g. 99.2 and 99.3) are grouped together.
#[tokio::test]
async fn test_distinct_float_field_rounds() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("price_bucket".to_string()),
        distinct: Some(DistinctValue::Bool(true)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let mut doc1 = create_doc("1", "Laptop Red", "", 100);
    doc1.fields
        .insert("price_bucket".to_string(), FieldValue::Float(99.2));

    let mut doc2 = create_doc("2", "Laptop Blue", "", 90);
    doc2.fields
        .insert("price_bucket".to_string(), FieldValue::Float(99.3));

    let mut doc3 = create_doc("3", "Mouse", "", 50);
    doc3.fields
        .insert("price_bucket".to_string(), FieldValue::Float(50.5));

    manager
        .add_documents_sync("test", vec![doc1, doc2, doc3])
        .await?;

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        None,
        None,
        10,
        0,
        None,
        Some(1),
    )?;

    assert_eq!(result.total, 1, "All laptops should be in same group");
    assert_eq!(result.documents.len(), 1);

    Ok(())
}

/// Verify that facet filters are applied before distinct grouping, so only documents matching the filter participate in deduplication.
#[tokio::test]
async fn test_distinct_with_filters() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        attributes_for_faceting: vec!["category".to_string()],
        searchable_attributes: None,
        ranking: None,
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attributes_to_retrieve: None,
        unretrievable_attributes: None,
        synonyms: None,
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Bool(true)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let mut doc1 = create_doc("1", "Laptop Red", "laptop-1", 100);
    doc1.fields.insert(
        "category".to_string(),
        FieldValue::Text("electronics".to_string()),
    );

    let mut doc2 = create_doc("2", "Laptop Blue", "laptop-1", 90);
    doc2.fields.insert(
        "category".to_string(),
        FieldValue::Text("electronics".to_string()),
    );

    let mut doc3 = create_doc("3", "Laptop Stand", "stand-1", 50);
    doc3.fields.insert(
        "category".to_string(),
        FieldValue::Text("accessories".to_string()),
    );

    manager
        .add_documents_sync("test", vec![doc1, doc2, doc3])
        .await?;

    let filter = Filter::Equals {
        field: "category".to_string(),
        value: FieldValue::Text("electronics".to_string()),
    };

    let result = manager.search_with_facets_and_distinct(
        "test",
        "laptop",
        Some(&filter),
        None,
        10,
        0,
        None,
        Some(1),
    )?;

    assert_eq!(result.total, 1, "Should only count electronics group");
    assert_eq!(result.documents.len(), 1);
    assert_eq!(result.documents[0].document.id, "1");

    Ok(())
}

/// Verify that after distinct deduplication, the surviving representatives from different groups are ordered by custom ranking (popularity) rather than insertion order.
#[tokio::test]
async fn test_distinct_preserves_ranking() -> Result<(), Box<dyn std::error::Error>> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());

    manager.create_tenant("test")?;

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        attribute_for_distinct: Some("product_id".to_string()),
        distinct: Some(DistinctValue::Bool(true)),
        ..Default::default()
    };
    settings.save(temp_dir.path().join("test/settings.json"))?;

    let docs = vec![
        create_doc("1", "Laptop Red", "laptop-1", 100),
        create_doc("2", "Laptop Blue", "laptop-1", 90),
        create_doc("3", "Mouse Red", "mouse-1", 200),
        create_doc("4", "Mouse Blue", "mouse-1", 190),
    ];
    manager.add_documents_sync("test", docs).await?;

    let result =
        manager.search_with_facets_and_distinct("test", "red", None, None, 10, 0, None, Some(1))?;

    assert_eq!(
        result.documents.len(),
        2,
        "Should return 2 groups (laptop and mouse)"
    );
    assert_eq!(
        result.documents[0].document.id, "3",
        "Mouse (200) before Laptop (100)"
    );
    assert_eq!(result.documents[1].document.id, "1", "Laptop second");

    Ok(())
}

// ============================================================
// From test_custom_ranking.rs — custom_ranking setting tests
// ============================================================

/// Verify that `custom_ranking: ["desc(popularity)"]` orders search results by descending popularity when all documents match the query.
#[tokio::test]
async fn test_custom_ranking_desc() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Product A", "popularity": 100}),
        json!({"_id": "2", "name": "Product B", "popularity": 500}),
        json!({"_id": "3", "name": "Product C", "popularity": 200}),
    ];

    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "product", None, None, 10).unwrap();

    assert_eq!(results.documents.len(), 3);
    assert_eq!(results.documents[0].document.id, "2");
    assert_eq!(results.documents[1].document.id, "3");
    assert_eq!(results.documents[2].document.id, "1");
}

/// Empty-query browse must still honor customRanking: with
/// `custom_ranking: ["desc(popularity)"]`, an empty query returns custom-ranked
/// order, not the objectID-descending browse default. This is the sort-dropdown
/// contract — replica indexes are browsed with an empty query
/// (live L4 replica evidence 2026-07-19).
#[tokio::test]
async fn test_custom_ranking_applies_to_empty_query() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Product A", "popularity": 100}),
        json!({"_id": "2", "name": "Product B", "popularity": 500}),
        json!({"_id": "3", "name": "Product C", "popularity": 200}),
    ];

    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "", None, None, 10).unwrap();

    assert_eq!(results.documents.len(), 3);
    assert_eq!(results.documents[0].document.id, "2");
    assert_eq!(results.documents[1].document.id, "3");
    assert_eq!(results.documents[2].document.id, "1");
}

/// Without customRanking, empty-query browse keeps its stable objectID-descending
/// default — the companion pin for the branch exercised above.
#[tokio::test]
async fn test_empty_query_without_custom_ranking_keeps_object_id_desc_default() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Product A", "popularity": 100}),
        json!({"_id": "2", "name": "Product B", "popularity": 500}),
        json!({"_id": "3", "name": "Product C", "popularity": 200}),
    ];

    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "", None, None, 10).unwrap();

    assert_eq!(results.documents.len(), 3);
    assert_eq!(results.documents[0].document.id, "3");
    assert_eq!(results.documents[1].document.id, "2");
    assert_eq!(results.documents[2].document.id, "1");
}

/// Verify that `custom_ranking: ["asc(price)"]` orders search results by ascending price when all documents match the query.
#[tokio::test]
async fn test_custom_ranking_asc() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        custom_ranking: Some(vec!["asc(price)".to_string()]),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Product A", "price": 100}),
        json!({"_id": "2", "name": "Product B", "price": 50}),
        json!({"_id": "3", "name": "Product C", "price": 200}),
    ];

    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "product", None, None, 10).unwrap();

    assert_eq!(results.documents.len(), 3);
    assert_eq!(results.documents[0].document.id, "2");
    assert_eq!(results.documents[1].document.id, "1");
    assert_eq!(results.documents[2].document.id, "3");
}

/// Verify that multiple custom ranking criteria are applied in declaration order: first by `desc(category_rank)`, then by `asc(price)` as tiebreaker within the same category rank.
#[tokio::test]
async fn test_custom_ranking_multiple() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        custom_ranking: Some(vec![
            "desc(category_rank)".to_string(),
            "asc(price)".to_string(),
        ]),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Product A", "category_rank": 1, "price": 100}),
        json!({"_id": "2", "name": "Product B", "category_rank": 2, "price": 50}),
        json!({"_id": "3", "name": "Product C", "category_rank": 2, "price": 200}),
        json!({"_id": "4", "name": "Product D", "category_rank": 1, "price": 80}),
    ];

    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "product", None, None, 10).unwrap();

    assert_eq!(results.documents.len(), 4);
    assert_eq!(results.documents[0].document.id, "2");
    assert_eq!(results.documents[1].document.id, "3");
    assert_eq!(results.documents[2].document.id, "4");
    assert_eq!(results.documents[3].document.id, "1");
}

/// Verify that documents missing a custom ranking field are sorted last, behind documents that have the field, rather than causing errors or receiving a default high value.
#[tokio::test]
async fn test_custom_ranking_missing_values() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        custom_ranking: Some(vec!["desc(popularity)".to_string()]),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Product A", "popularity": 100}),
        json!({"_id": "2", "name": "Product B"}),
        json!({"_id": "3", "name": "Product C", "popularity": 200}),
    ];

    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "product", None, None, 10).unwrap();

    assert_eq!(results.documents.len(), 3);
    assert_eq!(results.documents[0].document.id, "3");
    assert_eq!(results.documents[1].document.id, "1");
    assert_eq!(results.documents[2].document.id, "2");
}

/// Verify that an exact match always outranks a typo match regardless of BM25 score.
///
/// Doc 2 has a single exact `"iphone"` in attribute 8, while Doc 1 has `"iPhome"` (1-typo) repeated ten times in attribute 1 for a much higher BM25 score. The typo tier must override BM25.
#[tokio::test]
async fn test_typo_tier_exact_beats_higher_scoring_typo() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec![
            "a1".to_string(),
            "a2".to_string(),
            "a3".to_string(),
            "a4".to_string(),
            "a5".to_string(),
            "a6".to_string(),
            "a7".to_string(),
            "a8".to_string(),
        ]),
        query_type: "prefixNone".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({
            "_id": "1",
            "a1": "iPhome iPhome iPhome iPhome iPhome iPhome iPhome iPhome iPhome iPhome",
            "a2": "filler",
            "a3": "filler",
            "a4": "filler",
            "a5": "filler",
            "a6": "filler",
            "a7": "filler",
            "a8": "filler"
        }),
        json!({
            "_id": "2",
            "a1": "filler",
            "a2": "filler",
            "a3": "filler",
            "a4": "filler",
            "a5": "filler",
            "a6": "filler",
            "a7": "filler",
            "a8": "iphone"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "iphone", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(ids.contains(&"1"));
    assert!(ids.contains(&"2"));
    assert_eq!(
        ids[0], "2",
        "Exact match must outrank typo match regardless of BM25 score"
    );
}

/// Verify that a 1-typo match outranks a 2-typo match even when the 2-typo document has a higher BM25 score from term repetition.
///
/// Doc 2 has `"keyboarr"` (1 edit from `"keyboard"`), while Doc 1 has `"kexboarr"` (2 edits) repeated for high term frequency. The typo bucket tier must take precedence.
#[tokio::test]
async fn test_typo_tier_one_typo_beats_two_typos() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        query_type: "prefixNone".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    // Doc 1: "kexboarr" = 2 typos from "keyboard" (x for y, r for d), repeated
    //        for high BM25 score via tf.
    // Doc 2: "keyboarr" = 1 typo from "keyboard" (r for d), in both fields
    //        but NO exact "keyboard" anywhere — ensures bucket 1, not 0.
    let docs = vec![
        json!({
            "_id": "1",
            "title": "kexboarr kexboarr kexboarr kexboarr",
            "description": "two typos in a high-scoring field"
        }),
        json!({
            "_id": "2",
            "title": "keyboarr cover",
            "description": "keyboarr accessories"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "keyboard", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.contains(&"1"),
        "2-typo doc should match via fuzzy search"
    );
    assert!(
        ids.contains(&"2"),
        "1-typo doc should match via fuzzy search"
    );
    assert_eq!(
        ids[0], "2",
        "1-typo match (bucket 1) must outrank 2-typo match (bucket 2) even when 2-typo BM25 is higher"
    );
}

/// BM25 tuning test (A0a): relevance-path scores should be adjusted from Tantivy's
/// default `b=0.75` toward short-field behavior (`b=0.4`).
///
/// We compare:
/// - relevance path (`sort = None`): stage-2 ranking applies BM25 tuning
/// - explicit field sort path (`sort = ByField`): stage-2 ranking is bypassed,
///   leaving raw Tantivy scores untouched.
///
/// For the same matching docs:
/// - short doc score should be reduced by tuning (penalize short-doc bias)
/// - longer doc score should be increased by tuning
#[tokio::test]
async fn test_bm25_short_field_length_correction() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        query_type: "prefixNone".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    // "simple" has tf=1 for "wallet" in a 2-word title (gets short-doc BM25 bonus).
    // "featured" has tf=2 for "wallet" in a 6-word title (higher relevance but
    // heavily penalized by length normalization at b=0.75).
    // This pair is intentionally chosen so b=0.75 prefers "simple", while
    // b=0.4 should prefer "featured".
    let docs = vec![
        json!({"_id": "simple", "title": "Leather Wallet"}),
        json!({"_id": "featured", "title": "Premium Wallet Collection Wallet Gift Set"}),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| crate::types::Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let relevance_results = manager.search("test", "wallet", None, None, 10).unwrap();
    let raw_results = manager
        .search("test", "wallet", None, Some(&sort_asc("title")), 10)
        .unwrap();

    let ids: Vec<&str> = relevance_results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    // Both docs must appear in results
    assert!(ids.contains(&"simple"), "simple doc should match 'wallet'");
    assert!(
        ids.contains(&"featured"),
        "featured doc should match 'wallet'"
    );

    let relevance_simple = relevance_results
        .documents
        .iter()
        .find(|d| d.document.id == "simple")
        .unwrap()
        .score;
    let relevance_featured = relevance_results
        .documents
        .iter()
        .find(|d| d.document.id == "featured")
        .unwrap()
        .score;
    let raw_simple = raw_results
        .documents
        .iter()
        .find(|d| d.document.id == "simple")
        .unwrap()
        .score;
    let raw_featured = raw_results
        .documents
        .iter()
        .find(|d| d.document.id == "featured")
        .unwrap()
        .score;

    // Relevance path must apply score tuning relative to raw Tantivy scores.
    assert!(
        relevance_simple < raw_simple,
        "short-doc score should be reduced by BM25 length normalization tuning"
    );
    assert!(
        relevance_featured > raw_featured,
        "longer-doc score should be increased by BM25 length normalization tuning"
    );

    assert_eq!(
        ids[0], "featured",
        "Doc with higher tf for query term must rank above shorter doc after BM25 length correction"
    );
}

// ============================================================
// Stage 3: Exact-vs-Prefix tier (A0c)
// ============================================================

/// An exact word match must outrank prefix-only matches regardless of BM25 score.
/// With prefixLast, a single-word query means that word is the last (and only) term,
/// so it is prefix-eligible. Doc "run" (exact) must beat "running running running"
/// (prefix match with higher BM25).
#[tokio::test]
async fn test_exact_word_beats_prefix_match() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        query_type: "prefixLast".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        json!({"_id": "exact", "title": "run"}),
        json!({"_id": "prefix_running", "title": "running running running running running"}),
        json!({"_id": "prefix_runner", "title": "runner runner runner runner runner"}),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "run", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.len() >= 2,
        "Should match exact and prefix docs, got: {:?}",
        ids
    );
    assert_eq!(
        ids[0], "exact",
        "Exact word match 'run' must rank above prefix matches 'running'/'runner' regardless of BM25. Got: {:?}",
        ids
    );
}

/// Multi-word query with prefixLast: only the last term is prefix-eligible.
/// "red shoe" → "red" is non-prefix (must be exact/fuzzy), "shoe" is prefix-eligible.
/// Doc A: exact "red" + exact "shoe" → exact_vs_prefix = 0
/// Doc B: exact "red" + "shoes" (prefix on last word) → exact_vs_prefix = 1
/// Doc C: exact "red" + "shoelace" (prefix on last word) → exact_vs_prefix = 1
/// A must rank above B and C. B and C are in the same prefix bucket, tiebreak by BM25.
#[tokio::test]
async fn test_exact_vs_prefix_multiword_prefix_last() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string()]),
        query_type: "prefixLast".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        // Doc A: exact matches for both words
        json!({"_id": "exact_both", "title": "red shoe"}),
        // Doc B: exact "red" + prefix match "shoes" (higher BM25 from repetition)
        json!({"_id": "prefix_shoes", "title": "red shoes shoes shoes shoes shoes"}),
        // Doc C: exact "red" + prefix match "shoelace" (also high BM25)
        json!({"_id": "prefix_shoelace", "title": "red shoelace shoelace shoelace shoelace"}),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "red shoe", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.len() >= 2,
        "Should match docs with 'red' and 'shoe*', got: {:?}",
        ids
    );
    assert_eq!(
        ids[0], "exact_both",
        "Doc with exact matches for all terms must rank above prefix matches. Got: {:?}",
        ids
    );
}

// ============================================================
// Stage 3: Attribute bucket hard tier (A0d)
// ============================================================

/// Match in searchableAttributes[0] (title) must always outrank match in
/// searchableAttributes[1] (description), regardless of BM25 score.
/// Doc A: match only in description with high BM25 (term repeated 10x).
/// Doc B: match in title with low BM25 (single mention).
/// Doc B must rank above Doc A because title > description as hard tier.
#[tokio::test]
async fn test_attribute_bucket_hard_tier() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        query_type: "prefixNone".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        // Doc A: match in description only (high BM25 from repetition), filler in title
        json!({
            "_id": "desc_match",
            "title": "something completely unrelated here",
            "description": "wallet wallet wallet wallet wallet wallet wallet wallet wallet wallet"
        }),
        // Doc B: match in title (low BM25, single mention), filler in description
        json!({
            "_id": "title_match",
            "title": "wallet",
            "description": "something completely unrelated here filler text padding"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "wallet", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.len() >= 2,
        "Both docs should match 'wallet', got: {:?}",
        ids
    );
    assert_eq!(
        ids[0], "title_match",
        "Match in title (attr 0) must outrank match in description (attr 1) regardless of BM25. Got: {:?}",
        ids
    );
}

/// Doc with match in both title (attr 0) and description (attr 1) should get
/// attribute bucket 0 (best match wins).
#[tokio::test]
async fn test_attribute_bucket_multi_attribute_match() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        query_type: "prefixNone".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        // Doc A: match in both title and description
        json!({
            "_id": "both_attrs",
            "title": "wallet holder",
            "description": "wallet wallet wallet wallet wallet"
        }),
        // Doc B: match only in description (high BM25)
        json!({
            "_id": "desc_only",
            "title": "something else entirely",
            "description": "wallet wallet wallet wallet wallet wallet wallet wallet"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "wallet", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.len() >= 2,
        "Both docs should match 'wallet', got: {:?}",
        ids
    );
    assert_eq!(
        ids[0], "both_attrs",
        "Doc with match in title (attr 0) should rank above doc with match only in description (attr 1). Got: {:?}",
        ids
    );
}

/// `unordered()` should preserve attribute priority while disabling only the
/// intra-attribute word-position penalty.
#[tokio::test]
async fn test_unordered_searchable_attribute_preserves_attribute_priority() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let docs = vec![
        json!({
            "_id": "description_match",
            "title": "other text",
            "description": "wallet wallet wallet wallet wallet wallet wallet wallet wallet wallet"
        }),
        json!({
            "_id": "title_match",
            "title": "wallet",
            "description": "filler words only"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();

    manager.add_documents_sync("test", docs).await.unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec![
            "unordered(title)".to_string(),
            "description".to_string(),
        ]),
        query_type: "prefixNone".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let results = manager.search("test", "wallet", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert_eq!(
        ids[0], "title_match",
        "unordered(title) should still outrank a later description match because unordered() must keep attribute priority"
    );
}

/// With prefixLast, non-last terms are not prefix-eligible. A title token like
/// "redness" must not count as an attribute-0 match for query term "red".
/// The doc with true title exact match must outrank a high-BM25 doc that only
/// has the exact terms in description.
#[tokio::test]
async fn test_attribute_bucket_ignores_non_prefix_term_prefix_last() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        query_type: "prefixLast".to_string(),
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        // True attr-0 match: exact words in title.
        json!({
            "_id": "title_exact",
            "title": "red shoe",
            "description": "filler text"
        }),
        // Prefix leak candidate: title has only prefix for non-prefix-eligible term "red".
        // Exact terms exist in description with high tf to maximize BM25 pressure.
        json!({
            "_id": "prefix_leak_candidate",
            "title": "redness",
            "description": "red shoe red shoe red shoe red shoe red shoe red shoe red shoe red shoe red shoe red shoe"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "red shoe", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.len() >= 2,
        "Both docs should match query terms, got: {:?}",
        ids
    );
    assert_eq!(
        ids[0], "title_exact",
        "Non-prefix term prefix in title must not elevate attribute bucket under prefixLast. Got: {:?}",
        ids
    );
}

/// Short query terms below typo thresholds must not count fuzzy matches toward
/// higher-priority attribute buckets. For query "cat" (len=3), "cut" in title
/// is not a valid typo match, so this doc should still be treated as matching
/// in description (attr 1), not title (attr 0).
#[tokio::test]
async fn test_attribute_bucket_respects_short_word_typo_thresholds() {
    let temp = TempDir::new().unwrap();
    let manager = IndexManager::new(temp.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        searchable_attributes: Some(vec!["title".to_string(), "description".to_string()]),
        query_type: "prefixNone".to_string(),
        min_word_size_for_1_typo: 4,
        min_word_size_for_2_typos: 8,
        ..Default::default()
    };
    settings
        .save(temp.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        // Should remain attr-1: title has short-word typo only ("cut"), description has exact match.
        json!({
            "_id": "short_typo_title",
            "title": "cut",
            "description": "cat cat cat cat cat cat cat cat cat cat"
        }),
        // True attr-0 exact match.
        json!({
            "_id": "exact_title",
            "title": "cat",
            "description": "filler filler filler"
        }),
    ];
    let docs: Vec<_> = docs
        .into_iter()
        .map(|v| Document::from_json(&v).unwrap())
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let results = manager.search("test", "cat", None, None, 10).unwrap();
    let ids: Vec<&str> = results
        .documents
        .iter()
        .map(|d| d.document.id.as_str())
        .collect();

    assert!(
        ids.len() >= 2,
        "Both docs should match query through exact title/description terms, got: {:?}",
        ids
    );
    assert_eq!(
        ids[0], "exact_title",
        "Short-word typo in title must not be treated as attr-0 match for bucket ranking. Got: {:?}",
        ids
    );
}
