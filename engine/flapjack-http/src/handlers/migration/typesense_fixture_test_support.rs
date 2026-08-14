//! Shared access to the canonical M0B Typesense product fixture.
//!
//! This module is exposed from the crate's test-only router module so test
//! consumers at different points in the private module tree share one fixture
//! traversal and one cached parse.

use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

const PRODUCTS_COLLECTION: &str = "fj_ts_migration_products";

fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json")
}

pub(crate) fn expected_bundle() -> &'static Value {
    static BUNDLE: OnceLock<Value> = OnceLock::new();
    BUNDLE.get_or_init(|| {
        let path = fixture_path();
        serde_json::from_slice(&std::fs::read(&path).unwrap_or_else(|error| {
            panic!(
                "Typesense fixture {} must be readable: {error}",
                path.display()
            )
        }))
        .unwrap_or_else(|error| {
            panic!(
                "Typesense fixture {} must be valid JSON: {error}",
                path.display()
            )
        })
    })
}

pub(crate) fn products_collection() -> &'static Value {
    expected_bundle()["source"]["collections"]
        .as_array()
        .expect("M0B Typesense fixture collections must be an array")
        .iter()
        .find(|collection| collection["name"] == PRODUCTS_COLLECTION)
        .expect("M0B Typesense fixture must contain the products collection")
}

pub(crate) fn products_documents() -> &'static [Value] {
    products_collection()["documents"]
        .as_array()
        .expect("M0B Typesense product documents must be an array")
}

pub(crate) fn product_count() -> usize {
    products_documents().len()
}

pub(crate) fn product_ids() -> Vec<String> {
    products_documents()
        .iter()
        .map(|document| {
            document["id"]
                .as_str()
                .expect("M0B Typesense product IDs must be strings")
                .to_string()
        })
        .collect()
}
