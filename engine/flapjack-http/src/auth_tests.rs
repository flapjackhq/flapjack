use super::*;
use crate::test_helpers::body_json;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, post},
    Extension, Router,
};
use std::collections::BTreeSet;
use std::sync::Arc;
use tempfile::TempDir;
use tower::ServiceExt;

fn test_search_api_key(description: &str) -> ApiKey {
    ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string()],
        description: description.to_string(),
        indexes: vec![],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        validity: 0,
        restrict_sources: None,
    }
}

fn create_non_admin_test_key(description: &str) -> (TempDir, Arc<KeyStore>, String) {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));
    let search_key = test_search_api_key(description);
    let (_, plaintext_key) = key_store.create_key(search_key);

    (temp_dir, key_store, plaintext_key)
}

#[path = "auth_tests/batch_acl_tests.rs"]
mod batch_acl_tests;
#[path = "auth_tests/key_store_tests.rs"]
mod key_store_tests;
#[path = "auth_tests/middleware_tests.rs"]
mod middleware_tests;
#[path = "auth_tests/peer_boundary_route_contract.rs"]
pub(crate) mod peer_boundary_route_contract;
#[path = "auth_tests/restrict_sources_tests.rs"]
mod restrict_sources_tests;
#[path = "auth_tests/route_acl_tests.rs"]
mod route_acl_tests;
#[path = "auth_tests/secured_key_tests.rs"]
mod secured_key_tests;
#[path = "auth_tests/session_store_tests.rs"]
mod session_store_tests;
#[path = "auth_tests/session_transport_tests.rs"]
mod session_transport_tests;
