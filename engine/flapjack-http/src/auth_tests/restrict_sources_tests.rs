use super::*;
use std::net::IpAddr;

fn ip(addr: &str) -> IpAddr {
    addr.parse().expect("valid ip address")
}

fn source_list(entries: &[&str]) -> Vec<String> {
    entries.iter().map(|entry| (*entry).to_string()).collect()
}

async fn regular_key_restrict_sources_response(
    restrict_sources: Option<Vec<String>>,
    client_addr: &str,
) -> axum::response::Response {
    regular_key_restrict_sources_response_with_referer(restrict_sources, client_addr, None).await
}

async fn regular_key_restrict_sources_response_with_referer(
    restrict_sources: Option<Vec<String>>,
    client_addr: &str,
    referer: Option<&str>,
) -> axum::response::Response {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));

    let mut api_key = test_search_api_key("regular key restrictSources test key");
    api_key.restrict_sources = restrict_sources;
    let (_, plaintext_key) = key_store.create_key(api_key);

    let app = Router::new()
        .route(
            "/1/indexes/products/query",
            post(|| async { (StatusCode::OK, "ok") }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store));

    let mut request = Request::builder()
        .method("POST")
        .uri("/1/indexes/products/query")
        .header("x-algolia-application-id", "app-id")
        .header("x-algolia-api-key", plaintext_key)
        .body(Body::empty())
        .unwrap();
    if let Some(referer) = referer {
        request
            .headers_mut()
            .insert("referer", referer.parse().expect("valid referer header"));
    }
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        client_addr
            .parse::<std::net::SocketAddr>()
            .expect("valid socket address"),
    ));

    app.oneshot(request).await.unwrap()
}
#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_non_match_returns_algolia_403() {
    let response = regular_key_restrict_sources_response(
        Some(vec!["10.0.0.1".to_string()]),
        "203.0.113.9:9000",
    )
    .await;

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_exact_ip_match_returns_200() {
    let response = regular_key_restrict_sources_response(
        Some(vec!["203.0.113.9".to_string()]),
        "203.0.113.9:9000",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_cidr_match_returns_200() {
    let response = regular_key_restrict_sources_response(
        Some(vec!["10.0.0.0/8".to_string()]),
        "10.23.5.7:9000",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_none_allows_all_ips() {
    let response = regular_key_restrict_sources_response(None, "203.0.113.9:9000").await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_empty_list_allows_all_ips() {
    let response = regular_key_restrict_sources_response(Some(vec![]), "203.0.113.9:9000").await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_trims_entries() {
    let response = regular_key_restrict_sources_response(
        Some(vec![" 203.0.113.9 ".to_string(), "   ".to_string()]),
        "203.0.113.9:9000",
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_middleware_regular_key_restrict_sources_referer_pattern_match_returns_200() {
    let response = regular_key_restrict_sources_response_with_referer(
        Some(vec!["https://shop.example.com/*".to_string()]),
        "203.0.113.9:9000",
        Some("https://shop.example.com/products/123"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[test]
fn matches_restrict_sources_empty_slice_allows_all_ips() {
    assert!(matches_restrict_sources(&[], ip("203.0.113.9")));
}

#[test]
fn matches_restrict_sources_exact_ip_match_allows() {
    assert!(matches_restrict_sources(
        &source_list(&["203.0.113.9"]),
        ip("203.0.113.9")
    ));
}

#[test]
fn matches_restrict_sources_exact_ip_non_match_denies() {
    assert!(!matches_restrict_sources(
        &source_list(&["203.0.113.9"]),
        ip("203.0.113.10")
    ));
}

#[test]
fn matches_restrict_sources_cidr_range_match_allows() {
    assert!(matches_restrict_sources(
        &source_list(&["10.0.0.0/8"]),
        ip("10.42.1.9")
    ));
}

#[test]
fn matches_restrict_sources_cidr_non_match_denies() {
    assert!(!matches_restrict_sources(
        &source_list(&["10.0.0.0/8"]),
        ip("203.0.113.9")
    ));
}

#[test]
fn matches_restrict_sources_multiple_sources_with_one_match_allows() {
    assert!(matches_restrict_sources(
        &source_list(&["192.0.2.1", "203.0.113.9"]),
        ip("203.0.113.9")
    ));
}

#[test]
fn matches_restrict_sources_malformed_entry_denies_all() {
    assert!(!matches_restrict_sources(
        &source_list(&["not-a-network"]),
        ip("203.0.113.9")
    ));
}

#[test]
fn matches_restrict_sources_mixed_valid_and_invalid_entries_deny_all() {
    assert!(!matches_restrict_sources(
        &source_list(&["203.0.113.9", "not-a-network"]),
        ip("203.0.113.9")
    ));
}

#[test]
fn matches_restrict_sources_trims_and_drops_empty_entries() {
    assert!(matches_restrict_sources(
        &source_list(&[" 127.0.0.0/8 ", "   "]),
        ip("127.0.0.77")
    ));
}

#[test]
fn restrict_sources_match_trims_and_drops_empty_csv_segments() {
    assert!(restrict_sources_match(
        " 127.0.0.0/8 , , ",
        ip("127.0.0.77")
    ));
}

#[test]
fn validate_restrict_sources_entries_allows_referer_patterns_for_api_keys() {
    assert!(
        validate_restrict_sources_entries(&source_list(&["https://shop.example.com/*"])).is_ok()
    );
}

#[test]
fn validate_restrict_sources_entries_rejects_invalid_non_referer_tokens() {
    assert_eq!(
        validate_restrict_sources_entries(&source_list(&["bad-source"])),
        Err("bad-source".to_string())
    );
}

#[test]
fn validate_restrict_sources_csv_keeps_secured_key_sources_ip_only() {
    assert_eq!(
        validate_restrict_sources_csv("https://shop.example.com/*"),
        Err("https://shop.example.com/*".to_string())
    );
}
