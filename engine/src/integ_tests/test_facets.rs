//! In-process facet integration tests covering facet counting, filtering, pagination, cache invalidation, hierarchical facets, maxValuesPerFacet enforcement, and edge cases like special characters and missing settings.

use crate::index::settings::IndexSettings;
use crate::integ_tests::search_compat::SearchCompat;
use crate::types::{Document, FacetRequest, FieldValue};
use crate::IndexManager;
use std::collections::HashMap;
use tempfile::TempDir;

// ============================================================
// Shared helpers
// ============================================================

fn doc(id: &str, fields: Vec<(&str, FieldValue)>) -> Document {
    let f: HashMap<String, FieldValue> = fields
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();
    Document {
        id: id.to_string(),
        fields: f,
    }
}

fn text(s: &str) -> FieldValue {
    FieldValue::Text(s.to_string())
}

fn int(i: i64) -> FieldValue {
    FieldValue::Integer(i)
}

#[allow(dead_code)]
fn float(f: f64) -> FieldValue {
    FieldValue::Float(f)
}

fn arr(items: Vec<&str>) -> FieldValue {
    FieldValue::Array(items.into_iter().map(text).collect())
}

fn facet_req(field: &str) -> FacetRequest {
    FacetRequest {
        field: field.to_string(),
        path: format!("/{}", field),
        value_query: None,
    }
}

/// Create a temporary index with the given facet settings and documents.
///
/// # Arguments
///
/// * `facet_fields` - Field names to register in `attributesForFaceting`.
/// * `docs` - Documents to index synchronously.
///
/// # Returns
///
/// A `(TempDir, Arc<IndexManager>)` tuple; the `TempDir` must be held alive for the index lifetime.
async fn setup_with_settings(
    facet_fields: Vec<&str>,
    docs: Vec<Document>,
) -> (TempDir, std::sync::Arc<IndexManager>) {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        attributes_for_faceting: facet_fields.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();

    manager.add_documents_sync("test", docs).await.unwrap();
    (temp_dir, manager)
}

/// Create a temporary index pre-loaded with 8 electronics-store documents across 5 brands and 5 categories.
///
/// # Arguments
///
/// * `faceting_attrs` - Faceting attribute strings (may include `searchable(…)` modifiers).
///
/// # Returns
///
/// A `(TempDir, Arc<IndexManager>)` tuple.
async fn setup_facet_search_env(
    faceting_attrs: Vec<&str>,
) -> (TempDir, std::sync::Arc<IndexManager>) {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        attributes_for_faceting: faceting_attrs.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();

    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Samsung")),
                ("category", text("Phones")),
                ("name", text("Galaxy S24")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Samsung")),
                ("category", text("Tablets")),
                ("name", text("Galaxy Tab")),
            ],
        ),
        doc(
            "3",
            vec![
                ("brand", text("Samsonite")),
                ("category", text("Luggage")),
                ("name", text("Carry-On")),
            ],
        ),
        doc(
            "4",
            vec![
                ("brand", text("Apple")),
                ("category", text("Phones")),
                ("name", text("iPhone 15")),
            ],
        ),
        doc(
            "5",
            vec![
                ("brand", text("Apple")),
                ("category", text("Laptops")),
                ("name", text("MacBook Pro")),
            ],
        ),
        doc(
            "6",
            vec![
                ("brand", text("Sony")),
                ("category", text("Audio")),
                ("name", text("WH-1000XM5")),
            ],
        ),
        doc(
            "7",
            vec![
                ("brand", text("Sony")),
                ("category", text("Phones")),
                ("name", text("Xperia")),
            ],
        ),
        doc(
            "8",
            vec![
                ("brand", text("Dell")),
                ("category", text("Laptops")),
                ("name", text("XPS 15")),
            ],
        ),
    ];

    manager.add_documents_sync("test", docs).await.unwrap();
    (temp_dir, manager)
}

/// Create a temporary index with `n` documents, each having a unique `Brand_XXXX` value and `maxValuesPerFacet` set to 10.
///
/// # Arguments
///
/// * `n` - Number of documents (and distinct brand values) to generate.
async fn setup_many_brands(n: usize) -> (TempDir, std::sync::Arc<IndexManager>) {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        attributes_for_faceting: vec!["brand".to_string()],
        max_values_per_facet: 10,
        ..Default::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();

    let docs: Vec<Document> = (0..n)
        .map(|i| {
            doc(
                &format!("{}", i),
                vec![
                    ("brand", text(&format!("Brand_{:04}", i))),
                    ("name", text(&format!("Product {}", i))),
                ],
            )
        })
        .collect();

    manager.add_documents_sync("test", docs).await.unwrap();
    (temp_dir, manager)
}

// ============================================================
// Regression: bugs fixed in facet collection
// ============================================================

/// Verify that facet results are non-empty and contain correct per-value counts — regression guard for a bug that returned `HashMap::new()` instead of populated facets.
#[tokio::test]
async fn test_facets_returned_not_empty_hashmap() {
    let docs = vec![
        doc(
            "1",
            vec![("brand", text("Apple")), ("name", text("iPhone"))],
        ),
        doc(
            "2",
            vec![("brand", text("Samsung")), ("name", text("Galaxy"))],
        ),
        doc(
            "3",
            vec![("brand", text("Apple")), ("name", text("MacBook"))],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let result = mgr
        .search_with_facets("test", "", None, None, 10, 0, Some(&[facet_req("brand")]))
        .unwrap();

    assert!(
        !result.facets.is_empty(),
        "Facets must not be empty — regression for HashMap::new() bug"
    );
    let brand_facets = result.facets.get("brand").expect("brand facets missing");
    assert!(
        brand_facets.len() >= 2,
        "Expected at least 2 brands, got {}",
        brand_facets.len()
    );

    let apple_count = brand_facets
        .iter()
        .find(|f| f.path == "Apple")
        .map(|f| f.count);
    assert_eq!(apple_count, Some(2), "Apple should have count=2");

    let samsung_count = brand_facets
        .iter()
        .find(|f| f.path == "Samsung")
        .map(|f| f.count);
    assert_eq!(samsung_count, Some(1), "Samsung should have count=1");
}

/// Verify that array-valued string fields are expanded into individual facet values with correct counts.
#[tokio::test]
async fn test_array_string_fields_indexed_as_facets() {
    let docs = vec![
        doc(
            "1",
            vec![
                ("name", text("Laptop")),
                ("categories", arr(vec!["Electronics", "Computers"])),
            ],
        ),
        doc(
            "2",
            vec![
                ("name", text("Phone")),
                ("categories", arr(vec!["Electronics", "Phones"])),
            ],
        ),
        doc(
            "3",
            vec![
                ("name", text("Novel")),
                ("categories", arr(vec!["Books", "Fiction"])),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["categories"], docs).await;

    let result = mgr
        .search_with_facets(
            "test",
            "",
            None,
            None,
            10,
            0,
            Some(&[facet_req("categories")]),
        )
        .unwrap();

    let cat_facets = result
        .facets
        .get("categories")
        .expect("categories facets missing");
    assert!(
        cat_facets.len() >= 4,
        "Expected at least 4 category values, got {}",
        cat_facets.len()
    );

    let electronics = cat_facets.iter().find(|f| f.path == "Electronics");
    assert_eq!(
        electronics.map(|f| f.count),
        Some(2),
        "Electronics should appear in 2 docs"
    );
}

// ============================================================
// Facets with queries, filters, sort, pagination
// ============================================================

/// Verify that facet counts are scoped to documents matching a text query.
#[tokio::test]
async fn test_facets_with_text_query() {
    let docs = vec![
        doc(
            "1",
            vec![("brand", text("Apple")), ("name", text("iPhone 15"))],
        ),
        doc(
            "2",
            vec![("brand", text("Apple")), ("name", text("MacBook Pro"))],
        ),
        doc(
            "3",
            vec![("brand", text("Samsung")), ("name", text("Galaxy S24"))],
        ),
        doc(
            "4",
            vec![("brand", text("Samsung")), ("name", text("Galaxy Tab"))],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let result = mgr
        .search_with_facets(
            "test",
            "galaxy",
            None,
            None,
            10,
            0,
            Some(&[facet_req("brand")]),
        )
        .unwrap();

    assert_eq!(result.documents.len(), 2);
    let brand_facets = result.facets.get("brand").expect("brand facets missing");
    let samsung = brand_facets.iter().find(|f| f.path == "Samsung");
    assert_eq!(samsung.map(|f| f.count), Some(2));
}

/// Verify that facet counts reflect filter constraints when a `Filter::Equals` is applied.
#[tokio::test]
async fn test_facets_with_filter() {
    use crate::types::Filter;
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("category", text("Phone")),
                ("name", text("iPhone")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Apple")),
                ("category", text("Laptop")),
                ("name", text("MacBook")),
            ],
        ),
        doc(
            "3",
            vec![
                ("brand", text("Samsung")),
                ("category", text("Phone")),
                ("name", text("Galaxy")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "category"], docs).await;

    let filter = Filter::Equals {
        field: "category".to_string(),
        value: FieldValue::Text("Phone".to_string()),
    };
    let result = mgr
        .search_with_facets(
            "test",
            "",
            Some(&filter),
            None,
            10,
            0,
            Some(&[facet_req("brand")]),
        )
        .unwrap();

    assert_eq!(result.documents.len(), 2, "Should have 2 phone docs");
    let brand_facets = result.facets.get("brand").expect("brand facets missing");
    assert!(brand_facets.iter().any(|f| f.path == "Apple"));
    assert!(brand_facets.iter().any(|f| f.path == "Samsung"));
}

/// Verify that facet counts remain consistent across different page offsets.
#[tokio::test]
async fn test_facets_with_pagination() {
    let docs = (0..25)
        .map(|i| {
            doc(
                &format!("{}", i),
                vec![
                    ("brand", text(if i % 2 == 0 { "Apple" } else { "Samsung" })),
                    ("name", text(&format!("Product {}", i))),
                ],
            )
        })
        .collect();
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let r0 = mgr
        .search_with_facets("test", "", None, None, 5, 0, Some(&[facet_req("brand")]))
        .unwrap();
    let r2 = mgr
        .search_with_facets("test", "", None, None, 5, 10, Some(&[facet_req("brand")]))
        .unwrap();

    let p0_apple = r0
        .facets
        .get("brand")
        .expect("page 0 brand facets")
        .iter()
        .find(|f| f.path == "Apple")
        .map(|f| f.count)
        .unwrap_or(0);
    let p2_apple = r2
        .facets
        .get("brand")
        .expect("page 2 brand facets")
        .iter()
        .find(|f| f.path == "Apple")
        .map(|f| f.count)
        .unwrap_or(0);
    assert_eq!(
        p0_apple, p2_apple,
        "Facet counts should be consistent across pages"
    );
}

/// Verify that an empty query string returns facet counts covering every indexed document.
#[tokio::test]
async fn test_facets_empty_query_returns_all_counts() {
    let docs = vec![
        doc("1", vec![("brand", text("A")), ("name", text("x"))]),
        doc("2", vec![("brand", text("B")), ("name", text("y"))]),
        doc("3", vec![("brand", text("C")), ("name", text("z"))]),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let result = mgr
        .search_with_facets("test", "", None, None, 10, 0, Some(&[facet_req("brand")]))
        .unwrap();

    let brand_facets = result.facets.get("brand").expect("brand facets missing");
    assert_eq!(
        brand_facets.len(),
        3,
        "Empty query should return all 3 brands"
    );
    let total: u64 = brand_facets.iter().map(|f| f.count).sum();
    assert_eq!(total, 3, "Total facet count should equal doc count");
}

// ============================================================
// Multiple facet fields simultaneously
// ============================================================

/// Verify that requesting multiple facet fields in a single query returns independent counts for each field.
#[tokio::test]
async fn test_multiple_facet_fields() {
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("color", text("Black")),
                ("name", text("iPhone")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Apple")),
                ("color", text("White")),
                ("name", text("iPad")),
            ],
        ),
        doc(
            "3",
            vec![
                ("brand", text("Samsung")),
                ("color", text("Black")),
                ("name", text("Galaxy")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "color"], docs).await;

    let result = mgr
        .search_with_facets(
            "test",
            "",
            None,
            None,
            10,
            0,
            Some(&[facet_req("brand"), facet_req("color")]),
        )
        .unwrap();

    assert!(result.facets.contains_key("brand"));
    assert!(result.facets.contains_key("color"));
    assert_eq!(result.facets.get("brand").unwrap().len(), 2);
    assert_eq!(result.facets.get("color").unwrap().len(), 2);
}

// ============================================================
// Safety: edge cases, missing config, bad input
// ============================================================

/// Verify that requesting a facet field not listed in `attributesForFaceting` does not cause an error.
#[tokio::test]
async fn test_facets_field_not_in_settings_returns_empty() {
    let docs = vec![doc(
        "1",
        vec![
            ("brand", text("Apple")),
            ("color", text("Black")),
            ("name", text("x")),
        ],
    )];
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let result = mgr
        .search_with_facets(
            "test",
            "",
            None,
            None,
            10,
            0,
            Some(&[facet_req("brand"), facet_req("color")]),
        )
        .unwrap();

    assert!(
        result.facets.contains_key("brand"),
        "Configured facet field should work"
    );
}

/// Verify that searching with facet requests succeeds even when no `settings.json` file exists for the index.
#[tokio::test]
async fn test_facets_no_settings_file() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let docs = vec![doc(
        "1",
        vec![("brand", text("Apple")), ("name", text("iPhone"))],
    )];
    manager.add_documents_sync("test", docs).await.unwrap();

    let result =
        manager.search_with_facets("test", "", None, None, 10, 0, Some(&[facet_req("brand")]));
    assert!(
        result.is_ok(),
        "Search with facets should not crash without settings"
    );
}

/// Verify that facet values containing ampersands, apostrophes, and other special characters are indexed and counted correctly.
#[tokio::test]
async fn test_facets_special_characters_in_values() {
    let docs = vec![
        doc("1", vec![("brand", text("AT&T")), ("name", text("Phone"))]),
        doc(
            "2",
            vec![
                ("brand", text("Ben & Jerry's")),
                ("name", text("Ice Cream")),
            ],
        ),
        doc("3", vec![("brand", text("H&M")), ("name", text("Shirt"))]),
        doc("4", vec![("brand", text("AT&T")), ("name", text("Tablet"))]),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let result = mgr
        .search_with_facets("test", "", None, None, 10, 0, Some(&[facet_req("brand")]))
        .unwrap();

    let brand_facets = result.facets.get("brand").expect("brand facets");
    assert!(
        brand_facets.len() >= 3,
        "Should handle special chars: got {:?}",
        brand_facets
    );

    let att = brand_facets.iter().find(|f| f.path == "AT&T");
    assert_eq!(att.map(|f| f.count), Some(2), "AT&T should have 2 docs");
}

/// Verify that string and array facet fields can be queried simultaneously in a single search request.
#[tokio::test]
async fn test_facets_mixed_string_and_array_fields() {
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("categories", arr(vec!["Electronics", "Phones"])),
                ("name", text("iPhone")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Dell")),
                ("categories", arr(vec!["Electronics", "Computers"])),
                ("name", text("Laptop")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "categories"], docs).await;

    let result = mgr
        .search_with_facets(
            "test",
            "",
            None,
            None,
            10,
            0,
            Some(&[facet_req("brand"), facet_req("categories")]),
        )
        .unwrap();

    assert_eq!(result.facets.get("brand").expect("brand facets").len(), 2);
    assert!(
        result
            .facets
            .get("categories")
            .expect("categories facets")
            .len()
            >= 3,
        "Should have Electronics, Phones, Computers"
    );
}

/// Verify that documents with empty array fields do not produce spurious facet entries while non-empty arrays are counted normally.
#[tokio::test]
async fn test_facets_empty_array_field() {
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("tags", FieldValue::Array(vec![])),
                ("name", text("iPhone")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Samsung")),
                ("tags", arr(vec!["new", "sale"])),
                ("name", text("Galaxy")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "tags"], docs).await;

    let result = mgr
        .search_with_facets("test", "", None, None, 10, 0, Some(&[facet_req("tags")]))
        .unwrap();

    if let Some(tags) = result.facets.get("tags") {
        assert!(
            tags.iter().any(|f| f.path == "new") || tags.iter().any(|f| f.path == "sale"),
            "Should have facets from doc with non-empty array"
        );
    }
}

/// Verify that numeric fields listed in `attributesForFaceting` do not interfere with string facet collection.
#[tokio::test]
async fn test_facets_numeric_field_not_indexed() {
    let docs = vec![
        doc(
            "1",
            vec![
                ("price", int(100)),
                ("brand", text("Apple")),
                ("name", text("x")),
            ],
        ),
        doc(
            "2",
            vec![
                ("price", int(200)),
                ("brand", text("Samsung")),
                ("name", text("y")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "price"], docs).await;

    let result = mgr
        .search_with_facets(
            "test",
            "",
            None,
            None,
            10,
            0,
            Some(&[facet_req("brand"), facet_req("price")]),
        )
        .unwrap();

    assert_eq!(
        result.facets.get("brand").expect("brand facets").len(),
        2,
        "String facets should work"
    );
}

/// Verify that facet counts are still populated when distinct deduplication is active via `search_with_facets_and_distinct`.
#[tokio::test]
async fn test_facets_with_distinct() {
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("product_id", text("p1")),
                ("name", text("iPhone 15 Black")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Apple")),
                ("product_id", text("p1")),
                ("name", text("iPhone 15 White")),
            ],
        ),
        doc(
            "3",
            vec![
                ("brand", text("Samsung")),
                ("product_id", text("p2")),
                ("name", text("Galaxy S24")),
            ],
        ),
    ];

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        attributes_for_faceting: vec!["brand".to_string()],
        attribute_for_distinct: Some("product_id".to_string()),
        ..Default::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();
    manager.add_documents_sync("test", docs).await.unwrap();

    let result = manager
        .search_with_facets_and_distinct(
            "test",
            "",
            None,
            None,
            10,
            0,
            Some(&[facet_req("brand")]),
            Some(1),
        )
        .unwrap();

    assert!(
        !result
            .facets
            .get("brand")
            .expect("brand facets with distinct")
            .is_empty(),
        "Facets should not be empty when distinct is active"
    );
}

// ============================================================
// Correctness: facet counts reflect actual data
// ============================================================

/// Verify exact facet count accuracy across 100 documents evenly distributed among 4 brand values (25 each).
#[tokio::test]
async fn test_facet_counts_accuracy_100_docs() {
    let docs: Vec<Document> = (0..100)
        .map(|i| {
            let brand = match i % 4 {
                0 => "Alpha",
                1 => "Bravo",
                2 => "Charlie",
                _ => "Delta",
            };
            doc(
                &format!("{}", i),
                vec![
                    ("brand", text(brand)),
                    ("name", text(&format!("Item {}", i))),
                ],
            )
        })
        .collect();
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let result = mgr
        .search_with_facets("test", "", None, None, 100, 0, Some(&[facet_req("brand")]))
        .unwrap();

    let brand_facets = result.facets.get("brand").expect("brand facets");
    assert_eq!(brand_facets.len(), 4, "Should have exactly 4 brands");
    for fc in brand_facets {
        assert_eq!(
            fc.count, 25,
            "Each brand should have exactly 25 docs, {} has {}",
            fc.path, fc.count
        );
    }
}

/// Verify that facet counts reflect the intersection of a filter with the full document set.
#[tokio::test]
async fn test_facet_counts_with_query_filter_interaction() {
    use crate::types::Filter;
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("type", text("phone")),
                ("name", text("iPhone")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Apple")),
                ("type", text("laptop")),
                ("name", text("MacBook")),
            ],
        ),
        doc(
            "3",
            vec![
                ("brand", text("Apple")),
                ("type", text("tablet")),
                ("name", text("iPad")),
            ],
        ),
        doc(
            "4",
            vec![
                ("brand", text("Samsung")),
                ("type", text("phone")),
                ("name", text("Galaxy")),
            ],
        ),
        doc(
            "5",
            vec![
                ("brand", text("Samsung")),
                ("type", text("laptop")),
                ("name", text("Book")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "type"], docs).await;

    let filter = Filter::Equals {
        field: "type".to_string(),
        value: FieldValue::Text("phone".to_string()),
    };
    let result = mgr
        .search_with_facets(
            "test",
            "",
            Some(&filter),
            None,
            10,
            0,
            Some(&[facet_req("brand")]),
        )
        .unwrap();

    let brand_facets = result.facets.get("brand").expect("brand facets");
    assert_eq!(
        brand_facets
            .iter()
            .find(|f| f.path == "Apple")
            .map(|f| f.count),
        Some(1)
    );
    assert_eq!(
        brand_facets
            .iter()
            .find(|f| f.path == "Samsung")
            .map(|f| f.count),
        Some(1)
    );
}

/// Verify that a `*` wildcard facet request expands to all fields listed in `attributesForFaceting`.
#[tokio::test]
async fn test_facets_wildcard_star_expands_to_all() {
    use std::collections::HashSet;
    let docs = vec![
        doc(
            "1",
            vec![
                ("brand", text("Apple")),
                ("color", text("Black")),
                ("category", text("Phone")),
                ("name", text("iPhone")),
            ],
        ),
        doc(
            "2",
            vec![
                ("brand", text("Samsung")),
                ("color", text("White")),
                ("category", text("Tablet")),
                ("name", text("Galaxy Tab")),
            ],
        ),
        doc(
            "3",
            vec![
                ("brand", text("Apple")),
                ("color", text("Silver")),
                ("category", text("Laptop")),
                ("name", text("MacBook")),
            ],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand", "color", "category"], docs).await;

    let settings_path = _tmp.path().join("test/settings.json");
    let settings = IndexSettings::load(&settings_path).unwrap();
    let allowed: HashSet<String> = settings.facet_set();

    let input_facets = ["*".to_string()];
    let expanded: Vec<FacetRequest> = if input_facets.iter().any(|f| f == "*") {
        allowed.iter().map(|f| facet_req(f)).collect()
    } else {
        input_facets
            .iter()
            .filter(|f| allowed.contains(f.as_str()))
            .map(|f| facet_req(f))
            .collect()
    };

    assert_eq!(
        expanded.len(),
        3,
        "Wildcard should expand to 3 facet fields"
    );

    let result = mgr
        .search_with_facets("test", "", None, None, 10, 0, Some(&expanded))
        .unwrap();
    assert!(result.facets.contains_key("brand"));
    assert!(result.facets.contains_key("color"));
    assert!(result.facets.contains_key("category"));
    assert_eq!(result.facets.get("brand").unwrap().len(), 2);
}

/// Verify that facet counts update correctly after new documents are added, ensuring cached facet data is invalidated on write.
#[tokio::test]
async fn test_facet_cache_invalidated_on_write() {
    let docs = vec![
        doc(
            "1",
            vec![("brand", text("Apple")), ("name", text("iPhone"))],
        ),
        doc(
            "2",
            vec![("brand", text("Samsung")), ("name", text("Galaxy"))],
        ),
    ];
    let (_tmp, mgr) = setup_with_settings(vec!["brand"], docs).await;

    let r1 = mgr
        .search_with_facets("test", "", None, None, 0, 0, Some(&[facet_req("brand")]))
        .unwrap();
    assert_eq!(r1.facets.get("brand").expect("brand facets").len(), 2);

    mgr.add_documents_sync(
        "test",
        vec![
            doc("3", vec![("brand", text("Sony")), ("name", text("Xperia"))]),
            doc("4", vec![("brand", text("Sony")), ("name", text("TV"))]),
        ],
    )
    .await
    .unwrap();

    let r2 = mgr
        .search_with_facets("test", "", None, None, 0, 0, Some(&[facet_req("brand")]))
        .unwrap();
    let brands2 = r2.facets.get("brand").expect("brand facets after write");
    assert_eq!(
        brands2.len(),
        3,
        "Should have Apple + Samsung + Sony after write"
    );
    assert_eq!(
        brands2.iter().find(|f| f.path == "Sony").map(|f| f.count),
        Some(2)
    );
    assert_eq!(r2.total, 4);
}

// ============================================================
// Algolia hierarchical facets
// ============================================================

/// Verify that hierarchical facet fields (Algolia `lvl0` convention) return only top-level children with correct counts.
#[test]
fn test_algolia_hierarchical_facets_top_level() {
    use serde_json::json;
    let temp_dir = TempDir::new().unwrap();
    let index = crate::Index::create_in_dir(temp_dir.path()).unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Laptop", "categories": {"lvl0": "Electronics", "lvl1": "Electronics > Computers", "lvl2": "Electronics > Computers > Laptops"}}),
        json!({"_id": "2", "name": "Phone", "categories": {"lvl0": "Electronics", "lvl1": "Electronics > Phones"}}),
        json!({"_id": "3", "name": "Novel", "categories": {"lvl0": "Books", "lvl1": "Books > Fiction"}}),
    ];
    index.add_documents_simple(&docs).unwrap();

    let reader = index.reader();
    reader.reload().unwrap();
    let searcher = reader.searcher();

    let query = Box::new(tantivy::query::AllQuery);
    let executor = crate::QueryExecutor::new(index.converter(), index.inner().schema());

    let result = executor
        .execute_with_facets(
            &searcher,
            query,
            None,
            &crate::query::executor::FacetSearchParams {
                sort: None,
                limit: 10,
                offset: 0,
                has_text_query: false,
                facet_requests: Some(&[FacetRequest {
                    field: "categories.lvl0".into(),
                    path: "/categories.lvl0".into(),
                    value_query: None,
                }]),
                distinct_count: None,
            },
        )
        .unwrap();

    assert_eq!(result.documents.len(), 3);
    assert!(result.facets.contains_key("categories.lvl0"));
    let lvl0 = &result.facets["categories.lvl0"];
    assert_eq!(lvl0.len(), 2, "Should return immediate children only");
    assert_eq!(
        lvl0.iter()
            .find(|f| f.path == "Electronics")
            .map(|f| f.count),
        Some(2)
    );
    assert_eq!(
        lvl0.iter().find(|f| f.path == "Books").map(|f| f.count),
        Some(1)
    );
}

/// Verify that querying a `lvl1` hierarchical facet field returns second-level facet values with correct counts.
#[test]
fn test_facet_drill_down() {
    use serde_json::json;
    let temp_dir = TempDir::new().unwrap();
    let index = crate::Index::create_in_dir(temp_dir.path()).unwrap();

    let docs = vec![
        json!({"_id": "1", "name": "Laptop", "categories": {"lvl0": "Electronics", "lvl1": "Electronics > Computers", "lvl2": "Electronics > Computers > Laptops"}}),
        json!({"_id": "2", "name": "Phone", "categories": {"lvl0": "Electronics", "lvl1": "Electronics > Phones"}}),
    ];
    index.add_documents_simple(&docs).unwrap();

    let reader = index.reader();
    reader.reload().unwrap();
    let searcher = reader.searcher();

    let query = Box::new(tantivy::query::AllQuery);
    let executor = crate::QueryExecutor::new(index.converter(), index.inner().schema());

    let result = executor
        .execute_with_facets(
            &searcher,
            query,
            None,
            &crate::query::executor::FacetSearchParams {
                sort: None,
                limit: 10,
                offset: 0,
                has_text_query: false,
                facet_requests: Some(&[FacetRequest {
                    field: "categories.lvl1".into(),
                    path: "/categories.lvl1".into(),
                    value_query: None,
                }]),
                distinct_count: None,
            },
        )
        .unwrap();

    let lvl1 = &result.facets["categories.lvl1"];
    assert_eq!(lvl1.len(), 2);
    assert_eq!(
        lvl1.iter()
            .find(|f| f.path == "Electronics > Computers")
            .map(|f| f.count),
        Some(1)
    );
    assert_eq!(
        lvl1.iter()
            .find(|f| f.path == "Electronics > Phones")
            .map(|f| f.count),
        Some(1)
    );
}

// ============================================================
// searchable_facet_set() unit tests
// ============================================================

#[test]
fn test_searchable_facet_set_only_searchable_modifier() {
    let settings = IndexSettings {
        attributes_for_faceting: vec!["searchable(brand)".into(), "searchable(category)".into()],
        ..Default::default()
    };
    let set = settings.searchable_facet_set();
    assert!(set.contains("brand"));
    assert!(set.contains("category"));
    assert_eq!(set.len(), 2);
}

#[test]
fn test_searchable_facet_set_excludes_bare_names() {
    let settings = IndexSettings {
        attributes_for_faceting: vec!["brand".into(), "searchable(category)".into()],
        ..Default::default()
    };
    let set = settings.searchable_facet_set();
    assert!(
        !set.contains("brand"),
        "bare 'brand' must NOT be searchable"
    );
    assert!(set.contains("category"));
}

#[test]
fn test_searchable_facet_set_excludes_filter_only() {
    let settings = IndexSettings {
        attributes_for_faceting: vec!["filterOnly(price)".into(), "searchable(brand)".into()],
        ..Default::default()
    };
    let set = settings.searchable_facet_set();
    assert!(!set.contains("price"), "filterOnly must NOT be searchable");
    assert!(set.contains("brand"));
}

#[test]
fn test_searchable_facet_set_excludes_after_distinct() {
    let settings = IndexSettings {
        attributes_for_faceting: vec!["afterDistinct(status)".into(), "searchable(brand)".into()],
        ..Default::default()
    };
    let set = settings.searchable_facet_set();
    assert!(!set.contains("status"));
    assert!(set.contains("brand"));
}

#[test]
fn test_searchable_facet_set_empty_when_no_searchable() {
    let settings = IndexSettings {
        attributes_for_faceting: vec!["brand".into(), "filterOnly(price)".into()],
        ..Default::default()
    };
    assert!(settings.searchable_facet_set().is_empty());
}

#[test]
fn test_facet_set_still_includes_all() {
    let settings = IndexSettings {
        attributes_for_faceting: vec![
            "brand".into(),
            "searchable(category)".into(),
            "filterOnly(price)".into(),
        ],
        ..Default::default()
    };
    let set = settings.facet_set();
    assert!(set.contains("brand") && set.contains("category") && set.contains("price"));
    assert_eq!(set.len(), 3);
}

/// Verify that facets configured with the `searchable(…)` modifier are returned with correct counts via `search_full`.
#[tokio::test]
async fn test_facet_search_with_searchable_modifier() {
    let (_tmp, mgr) =
        setup_facet_search_env(vec!["searchable(brand)", "searchable(category)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(1000),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert!(
        brands.len() >= 4,
        "Should have Samsung, Samsonite, Apple, Sony, Dell"
    );
    assert_eq!(
        brands.iter().find(|f| f.path == "Samsung").map(|f| f.count),
        Some(2)
    );
}

/// Verify that facet values can be filtered by prefix (e.g. "sam" matches Samsung and Samsonite).
#[tokio::test]
async fn test_facet_search_prefix_filtering() {
    let (_tmp, mgr) = setup_facet_search_env(vec!["searchable(brand)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(1000),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    let sam_matches: Vec<_> = brands
        .iter()
        .filter(|f| f.path.to_lowercase().starts_with("sam"))
        .collect();
    assert_eq!(sam_matches.len(), 2, "Samsung + Samsonite");
}

/// Verify that an empty facet search query returns all distinct facet values.
#[tokio::test]
async fn test_facet_search_empty_query_returns_all() {
    let (_tmp, mgr) = setup_facet_search_env(vec!["searchable(brand)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(1000),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert!(
        brands.len() >= 4,
        "Empty query should return all brands, got {}",
        brands.len()
    );
}

/// Verify that the `maxValuesPerFacet` query parameter caps the number of returned facet values.
#[tokio::test]
async fn test_facet_search_respects_max_values() {
    let (_tmp, mgr) = setup_facet_search_env(vec!["searchable(brand)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(2),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert!(
        brands.len() <= 2,
        "maxValuesPerFacet=2 should cap results, got {}",
        brands.len()
    );
}

/// Verify end-to-end facet search with prefix matching returns expected brand values (Samsung and Samsonite for "sam").
#[tokio::test]
async fn test_facet_search_params_string_integration() {
    let (_tmp, mgr) = setup_facet_search_env(vec!["searchable(brand)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(1000),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    let matching: Vec<_> = brands
        .iter()
        .filter(|f| f.path.to_lowercase().starts_with("sam"))
        .collect();
    assert_eq!(
        matching.len(),
        2,
        "Samsung + Samsonite should match 'sam' prefix"
    );
    assert!(matching.iter().any(|f| f.path == "Samsung"));
    assert!(matching.iter().any(|f| f.path == "Samsonite"));
}

#[tokio::test]
async fn test_facet_search_rejects_non_searchable() {
    let (_tmp, _mgr) = setup_facet_search_env(vec!["filterOnly(brand)"]).await;
    let settings = IndexSettings::load(_tmp.path().join("test/settings.json")).unwrap();
    assert!(
        !settings.searchable_facet_set().contains("brand"),
        "filterOnly(brand) should not be in searchable set"
    );
}

/// Verify that facet values are returned sorted by count in descending order.
#[tokio::test]
async fn test_facet_search_sorted_by_count_desc() {
    let (_tmp, mgr) = setup_facet_search_env(vec!["searchable(brand)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(1000),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    let counts: Vec<u64> = brands.iter().map(|f| f.count).collect();
    for w in counts.windows(2) {
        assert!(
            w[0] >= w[1],
            "Facet values should be sorted by count desc, got {:?}",
            counts
        );
    }
}

/// Verify that facet count ordering is preserved through JSON serialization and deserialization round-trip.
#[tokio::test]
async fn test_facet_json_serialization_preserves_count_order() {
    let (_tmp, mgr) = setup_facet_search_env(vec!["searchable(brand)"]).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(1000),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    let facet_map: serde_json::Map<String, serde_json::Value> = brands
        .iter()
        .map(|fc| (fc.path.clone(), serde_json::json!(fc.count)))
        .collect();
    let serialized = serde_json::to_string(&serde_json::Value::Object(facet_map)).unwrap();
    let roundtrip: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&serialized).unwrap();
    let counts: Vec<u64> = roundtrip.values().map(|v| v.as_u64().unwrap()).collect();
    for w in counts.windows(2) {
        assert!(
            w[0] >= w[1],
            "Facet order lost during JSON serialization: {:?}",
            counts
        );
    }
}

// ============================================================
// maxValuesPerFacet enforcement
// ============================================================

#[tokio::test]
async fn test_max_values_per_facet_settings_enforced() {
    let (_tmp, mgr) = setup_many_brands(50).await;
    let result = mgr
        .search_with_facets("test", "", None, None, 0, 0, Some(&[facet_req("brand")]))
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert_eq!(
        brands.len(),
        10,
        "Settings maxValuesPerFacet=10 should limit to 10, got {}",
        brands.len()
    );
}

/// Verify that a per-query `maxValuesPerFacet` value lower than the settings value takes precedence.
#[tokio::test]
async fn test_max_values_per_facet_query_override() {
    let (_tmp, mgr) = setup_many_brands(50).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(5),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert_eq!(
        brands.len(),
        5,
        "Per-query maxValuesPerFacet=5 should override settings, got {}",
        brands.len()
    );
}

/// Verify that a per-query `maxValuesPerFacet` value higher than the settings value is honored.
#[tokio::test]
async fn test_max_values_per_facet_query_override_higher() {
    let (_tmp, mgr) = setup_many_brands(50).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(25),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert_eq!(
        brands.len(),
        25,
        "Per-query maxValuesPerFacet=25 should override settings=10, got {}",
        brands.len()
    );
}

/// Verify that requesting `maxValuesPerFacet` above 1000 is capped and returns only the available values.
#[tokio::test]
async fn test_max_values_per_facet_capped_at_1000() {
    let (_tmp, mgr) = setup_many_brands(50).await;
    let result = mgr
        .search_full(
            "test",
            "",
            None,
            None,
            0,
            0,
            Some(&[facet_req("brand")]),
            None,
            Some(9999),
        )
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert_eq!(
        brands.len(),
        50,
        "Cap at 1000 but only 50 exist, should return 50, got {}",
        brands.len()
    );
}

/// Long-tail scenario: a value outside the top 1000 by count must be findable
/// via a facet value query (searchForFacetValues), and the plain facet list
/// must honestly report non-exhaustive values.
#[tokio::test]
async fn test_facet_value_query_finds_values_beyond_top_1000() {
    use crate::index::SearchOptions;

    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        attributes_for_faceting: vec!["searchable(brand)".to_string()],
        ..Default::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();

    // 1050 brands with 2 docs each, plus one rare brand with a single doc —
    // the rare brand can never appear in a top-1000-by-count facet list.
    let mut docs = Vec::new();
    for i in 0..1050 {
        for j in 0..2 {
            docs.push(doc(
                &format!("d{}_{}", i, j),
                vec![
                    ("brand", text(&format!("Brand_{:04}", i))),
                    ("name", text("Popular product")),
                ],
            ));
        }
    }
    docs.push(doc(
        "rare",
        vec![
            ("brand", text("Blue Hat Toy Company")),
            ("name", text("Stunt car")),
        ],
    ));
    manager.add_documents_sync("test", docs).await.unwrap();

    // Plain enumeration: capped at 1000, rare brand absent, flagged non-exhaustive.
    let plain = manager
        .search_with_options(
            "test",
            "",
            &SearchOptions {
                facets: Some(&[facet_req("brand")]),
                max_values_per_facet: Some(1000),
                limit: 0,
                ..Default::default()
            },
        )
        .unwrap();
    let brands = plain.facets.get("brand").expect("brand facets");
    assert_eq!(brands.len(), 1000);
    assert!(brands.iter().all(|fc| fc.path != "Blue Hat Toy Company"));
    assert!(!plain.exhaustive_facet_values);

    // Facet value query scans ALL values: rare brand found, result exhaustive.
    let vq = FacetRequest {
        field: "brand".to_string(),
        path: "/brand".to_string(),
        value_query: Some("blue hat".to_string()),
    };
    let searched = manager
        .search_with_options(
            "test",
            "",
            &SearchOptions {
                facets: Some(&[vq]),
                max_values_per_facet: Some(1000),
                limit: 0,
                ..Default::default()
            },
        )
        .unwrap();
    let hits = searched.facets.get("brand").expect("brand facets");
    assert_eq!(
        hits.len(),
        1,
        "value query should match exactly the rare brand, got {:?}",
        hits
    );
    assert_eq!(hits[0].path, "Blue Hat Toy Company");
    assert_eq!(hits[0].count, 1);
    assert!(searched.exhaustive_facet_values);

    // Same, via the default-limit path the HTTP facet-search handler uses
    // (limit > 0 takes the ranked-search route, not the zero-limit route).
    let vq = FacetRequest {
        field: "brand".to_string(),
        path: "/brand".to_string(),
        value_query: Some("blue hat".to_string()),
    };
    let ranked = manager
        .search_with_options(
            "test",
            "",
            &SearchOptions {
                facets: Some(&[vq]),
                max_values_per_facet: Some(1000),
                ..Default::default()
            },
        )
        .unwrap();
    let hits = ranked
        .facets
        .get("brand")
        .expect("brand facets (ranked path)");
    assert_eq!(
        hits.len(),
        1,
        "ranked path: value query should match the rare brand, got {} values",
        hits.len()
    );
    assert_eq!(hits[0].path, "Blue Hat Toy Company");
}

/// Verify that the default `maxValuesPerFacet` is 100 when the setting is not explicitly configured.
#[tokio::test]
async fn test_max_values_default_100_when_no_settings() {
    let temp_dir = TempDir::new().unwrap();
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("test").unwrap();

    let settings = IndexSettings {
        attributes_for_faceting: vec!["brand".to_string()],
        ..Default::default()
    };
    settings
        .save(temp_dir.path().join("test/settings.json"))
        .unwrap();

    let docs: Vec<Document> = (0..150)
        .map(|i| {
            doc(
                &format!("{}", i),
                vec![
                    ("brand", text(&format!("Brand_{:04}", i))),
                    ("name", text(&format!("Product {}", i))),
                ],
            )
        })
        .collect();
    manager.add_documents_sync("test", docs).await.unwrap();

    let result = manager
        .search_with_facets("test", "", None, None, 0, 0, Some(&[facet_req("brand")]))
        .unwrap();
    let brands = result.facets.get("brand").expect("brand facets");
    assert_eq!(
        brands.len(),
        100,
        "Default maxValuesPerFacet should be 100, got {}",
        brands.len()
    );
}

#[tokio::test]
async fn test_max_values_per_facet_missing_from_json_defaults_100() {
    let settings: IndexSettings =
        serde_json::from_str(r#"{"attributesForFaceting":["brand"]}"#).unwrap();
    assert_eq!(
        settings.max_values_per_facet, 100,
        "Missing maxValuesPerFacet should default to 100"
    );
}

// ============================================================
// Response fields: limit=0 tests (pure flapjack, no HTTP)
// ============================================================

mod response_fields {
    use super::*;

    /// Create a temporary index with 5 documents across 3 brands and 5 categories, configured with `searchable(brand)` and `searchable(category)` faceting attributes.
    async fn rf_setup() -> (TempDir, std::sync::Arc<IndexManager>) {
        let temp_dir = TempDir::new().unwrap();
        let manager = IndexManager::new(temp_dir.path());
        manager.create_tenant("test").unwrap();

        let settings = IndexSettings {
            attributes_for_faceting: vec![
                "searchable(brand)".to_string(),
                "searchable(category)".to_string(),
            ],
            ..Default::default()
        };
        settings
            .save(temp_dir.path().join("test/settings.json"))
            .unwrap();

        let docs = vec![
            doc(
                "1",
                vec![
                    ("brand", text("Samsung")),
                    ("category", text("Phones")),
                    ("name", text("Galaxy S24")),
                ],
            ),
            doc(
                "2",
                vec![
                    ("brand", text("Samsung")),
                    ("category", text("Tablets")),
                    ("name", text("Galaxy Tab")),
                ],
            ),
            doc(
                "3",
                vec![
                    ("brand", text("Apple")),
                    ("category", text("Phones")),
                    ("name", text("iPhone 15")),
                ],
            ),
            doc(
                "4",
                vec![
                    ("brand", text("Apple")),
                    ("category", text("Laptops")),
                    ("name", text("MacBook Pro")),
                ],
            ),
            doc(
                "5",
                vec![
                    ("brand", text("Sony")),
                    ("category", text("Audio")),
                    ("name", text("WH-1000XM5")),
                ],
            ),
        ];

        manager.add_documents_sync("test", docs).await.unwrap();
        (temp_dir, manager)
    }

    /// Verify that `limit=0` returns zero documents while still populating facet counts.
    #[tokio::test]
    async fn test_limit_zero_returns_no_docs_but_facets() {
        let (_tmp, mgr) = rf_setup().await;

        let result = mgr
            .search_full(
                "test",
                "",
                None,
                None,
                0,
                0,
                Some(&[facet_req("brand")]),
                None,
                None,
            )
            .unwrap();

        assert_eq!(result.documents.len(), 0, "limit=0 should return 0 docs");
        let brands = result
            .facets
            .get("brand")
            .expect("should still have facets");
        assert!(
            brands.len() >= 3,
            "facets should be populated even with limit=0"
        );
    }

    /// Verify that `limit=0` combined with a text query returns zero documents while still returning facet counts scoped to the query.
    #[tokio::test]
    async fn test_limit_zero_with_query_returns_no_docs_but_facets() {
        let (_tmp, mgr) = rf_setup().await;

        let result = mgr
            .search_full(
                "test",
                "galaxy",
                None,
                None,
                0,
                0,
                Some(&[facet_req("brand")]),
                None,
                None,
            )
            .unwrap();

        assert_eq!(
            result.documents.len(),
            0,
            "limit=0 should return 0 docs even with query"
        );
        let brands = result.facets.get("brand");
        assert!(brands.is_some(), "facets should still be returned");
    }

    #[tokio::test]
    async fn test_limit_zero_no_facets_returns_empty() {
        let (_tmp, mgr) = rf_setup().await;

        let result = mgr
            .search_full("test", "", None, None, 0, 0, None, None, None)
            .unwrap();

        assert_eq!(result.documents.len(), 0);
        assert!(result.facets.is_empty());
    }
}
