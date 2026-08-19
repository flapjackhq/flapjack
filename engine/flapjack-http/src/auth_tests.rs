//! Stub summary for auth_tests.rs.
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

#[test]
fn application_id_accepts_official_browser_query_transport() {
    let query_only = Request::builder()
        .uri("/1/indexes/*/queries?x-algolia-application-id=browser-app")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        request_application_id(&query_only).as_deref(),
        Some("browser-app")
    );

    let header_wins = Request::builder()
        .uri("/1/indexes/*/queries?x-algolia-application-id=query-app")
        .header("x-algolia-application-id", "header-app")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        request_application_id(&header_wins).as_deref(),
        Some("header-app")
    );

    let encoded = Request::builder()
        .uri("/1/indexes/*/queries?x-algolia-application-id=browser%20app")
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        request_application_id(&encoded).as_deref(),
        Some("browser app")
    );
}

#[test]
fn rate_limiter_fails_closed_at_the_global_bucket_cap() {
    let limiter = RateLimiter::new();
    let key_hash = "bounded-rate-key";

    for address_number in 0..65_536_u128 {
        assert!(limiter.check_and_increment(
            key_hash,
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(address_number)),
            1,
        ));
    }

    assert!(
        !limiter.check_and_increment(
            key_hash,
            std::net::IpAddr::V6(std::net::Ipv6Addr::from(65_536_u128)),
            1,
        ),
        "a new cardinality key must fail closed once the global cap is full"
    );
    assert_eq!(
        limiter.counters.len(),
        65_536,
        "the denied key must not allocate a counter"
    );
}

#[test]
fn rate_limiter_reclaims_expired_buckets_before_failing_closed() {
    let limiter = RateLimiter::with_max_buckets(2);
    let expired_ip = "2001:db8::1".parse().unwrap();
    let active_ip = "2001:db8::2".parse().unwrap();
    let replacement_ip = "2001:db8::3".parse().unwrap();

    assert!(limiter.check_and_increment("key", expired_ip, 2));
    assert!(limiter.check_and_increment("key", active_ip, 2));
    limiter
        .counters
        .get_mut(&("key".to_string(), expired_ip))
        .unwrap()
        .value_mut()
        .1 = Instant::now() - RATE_LIMIT_WINDOW - Duration::from_secs(1);

    assert!(limiter.check_and_increment("key", replacement_ip, 2));
    assert_eq!(limiter.counters.len(), 2);
    assert!(!limiter
        .counters
        .contains_key(&("key".to_string(), expired_ip)));
    assert!(limiter
        .counters
        .contains_key(&("key".to_string(), active_ip)));
    assert!(limiter
        .counters
        .contains_key(&("key".to_string(), replacement_ip)));
}

#[test]
fn rate_limiter_parallel_admission_respects_the_global_bucket_cap() {
    const CAPACITY: usize = 32;
    const CONTENDERS: usize = 64;
    let limiter = Arc::new(RateLimiter::with_max_buckets(CAPACITY));
    let start = Arc::new(std::sync::Barrier::new(CONTENDERS));
    let mut workers = Vec::with_capacity(CONTENDERS);

    for address_number in 0..CONTENDERS {
        let limiter = Arc::clone(&limiter);
        let start = Arc::clone(&start);
        workers.push(std::thread::spawn(move || {
            start.wait();
            limiter.check_and_increment(
                "parallel-key",
                std::net::IpAddr::V6(std::net::Ipv6Addr::from(address_number as u128)),
                1,
            )
        }));
    }

    let admitted = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .filter(|admitted| *admitted)
        .count();
    assert_eq!(admitted, CAPACITY);
    assert_eq!(limiter.counters.len(), CAPACITY);
}

/// TODO: Document test_search_api_key.
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
