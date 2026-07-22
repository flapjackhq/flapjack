use super::*;

/// Verify that authentication middleware returns 403 Forbidden and 429 Too Many Requests responses in Algolia-compatible JSON format with `message` and `status` fields.
#[tokio::test]
async fn auth_middleware_returns_algolia_error_shape_for_403_and_429() {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));
    let mut rate_limited_key = test_search_api_key("Rate-limited test key");
    rate_limited_key.max_queries_per_ip_per_hour = 1;
    let (_, plaintext_key) = key_store.create_key(rate_limited_key);

    let app = Router::new()
        .route(
            "/1/indexes/products/query",
            post(|| async { (StatusCode::OK, "ok") }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store))
        .layer(Extension(RateLimiter::new()));

    let forbidden_resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/query")
                .header("x-algolia-api-key", &plaintext_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(forbidden_resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(forbidden_resp).await,
        serde_json::json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );

    // Keep this test focused on Algolia error payload shape rather than a specific
    // request count, since exact 429 timing is covered by dedicated rate-limit tests.
    let mut rate_limited_resp = None;
    for _ in 0..3 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/1/indexes/products/query")
                    .header("x-algolia-application-id", "app-id")
                    .header("x-algolia-api-key", &plaintext_key)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        if resp.status() == StatusCode::TOO_MANY_REQUESTS {
            rate_limited_resp = Some(resp);
            break;
        }
    }

    let rate_limited_resp =
        rate_limited_resp.expect("expected at least one 429 response from the limited key");
    assert_eq!(rate_limited_resp.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        body_json(rate_limited_resp).await,
        serde_json::json!({
            "message": "Too many requests per IP per hour",
            "status": 429
        })
    );
}
#[tokio::test]
async fn auth_middleware_enforces_secured_key_restrict_sources() {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));
    let search_key = test_search_api_key("Secured-key source restriction test key");
    let (_, plaintext_key) = key_store.create_key(search_key);
    let secured_key = generate_secured_api_key(
        &plaintext_key,
        "restrictSources=127.0.0.0/8&validUntil=9999999999",
    );

    let app = Router::new()
        .route(
            "/1/indexes/products/query",
            post(|| async { (StatusCode::OK, "ok") }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store));

    let mut allowed_req = Request::builder()
        .method("POST")
        .uri("/1/indexes/products/query")
        .header("x-algolia-application-id", "app-id")
        .header("x-algolia-api-key", &secured_key)
        .body(Body::empty())
        .unwrap();
    allowed_req
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(
            "127.0.0.77:7700"
                .parse::<std::net::SocketAddr>()
                .expect("valid socket address"),
        ));

    let allowed_resp = app.clone().oneshot(allowed_req).await.unwrap();
    assert_eq!(allowed_resp.status(), StatusCode::OK);

    let mut denied_req = Request::builder()
        .method("POST")
        .uri("/1/indexes/products/query")
        .header("x-algolia-application-id", "app-id")
        .header("x-algolia-api-key", &secured_key)
        .body(Body::empty())
        .unwrap();
    denied_req
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(
            "203.0.113.9:9000"
                .parse::<std::net::SocketAddr>()
                .expect("valid socket address"),
        ));

    let denied_resp = app.oneshot(denied_req).await.unwrap();
    assert_eq!(denied_resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(denied_resp).await,
        serde_json::json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}
#[tokio::test]
async fn auth_middleware_internal_storage_requires_app_id_even_for_admin_key() {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));

    let app = Router::new()
        .route("/internal/storage", get(|| async { StatusCode::OK }))
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/internal/storage")
                .header("x-algolia-api-key", "admin-key")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

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
async fn auth_middleware_secured_key_restrict_sources_rejection_does_not_consume_rate_limit() {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));
    let mut search_key = test_search_api_key("Secured-key restrictSources ordering test key");
    search_key.max_queries_per_ip_per_hour = 1;
    let (_, plaintext_key) = key_store.create_key(search_key);
    let secured_key = generate_secured_api_key(
        &plaintext_key,
        "restrictSources=127.0.0.0/8&validUntil=9999999999",
    );

    let app = Router::new()
        .route(
            "/1/indexes/products/query",
            post(|| async { (StatusCode::OK, "ok") }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store))
        .layer(Extension(RateLimiter::new()));

    for _ in 0..2 {
        let mut denied_req = Request::builder()
            .method("POST")
            .uri("/1/indexes/products/query")
            .header("x-algolia-application-id", "app-id")
            .header("x-algolia-api-key", &secured_key)
            .body(Body::empty())
            .unwrap();
        denied_req
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(
                "203.0.113.9:9000"
                    .parse::<std::net::SocketAddr>()
                    .expect("valid socket address"),
            ));

        let denied_resp = app.clone().oneshot(denied_req).await.unwrap();
        assert_eq!(denied_resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(
            body_json(denied_resp).await,
            serde_json::json!({
                "message": "Invalid Application-ID or API key",
                "status": 403
            })
        );
    }
}
/// Proves that requests with an unrecognized API key return 403 without consuming
/// a rate-limit bucket. The `authenticate_and_authorize` middleware calls
/// `lookup_authenticated_key` before `ensure_rate_limit_allows_request`; when the
/// key is not found, the early return exits before rate-limit accounting runs.
/// This prevents an attacker from exhausting a legitimate user's rate-limit quota
/// by spraying invalid keys from the same IP.
#[tokio::test]
async fn auth_middleware_invalid_key_does_not_consume_rate_limit() {
    let temp_dir = TempDir::new().unwrap();
    let key_store = Arc::new(KeyStore::load_or_create(temp_dir.path(), "admin-key"));
    // Create a key with a very tight rate limit so any bucket consumption would
    // immediately surface as a 429 instead of the expected 403.
    let mut search_key = test_search_api_key("Invalid-key rate-limit ordering test key");
    search_key.max_queries_per_ip_per_hour = 1;
    let _ = key_store.create_key(search_key);

    let app = Router::new()
        .route(
            "/1/indexes/products/query",
            post(|| async { (StatusCode::OK, "ok") }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store))
        .layer(Extension(RateLimiter::new()));

    // Send multiple requests with a completely unrecognized key.
    // If any request returns 429 instead of 403, the rate limiter was wrongly consulted.
    for i in 0..3 {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/1/indexes/products/query")
                    .header("x-algolia-application-id", "app-id")
                    .header("x-algolia-api-key", "completely-bogus-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "request {i} with invalid key must return 403, not 429"
        );
        assert_eq!(
            body_json(resp).await,
            serde_json::json!({
                "message": "Invalid Application-ID or API key",
                "status": 403
            })
        );
    }
}

#[tokio::test]
async fn auth_middleware_allows_non_admin_key_to_get_own_key_record() {
    let (_temp_dir, key_store, plaintext_key) = create_non_admin_test_key("Own-key read test key");

    let app = Router::new()
        .route("/1/keys/:key", get(|| async { StatusCode::OK }))
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store));

    let response = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/1/keys/{plaintext_key}"))
                .header("x-algolia-application-id", "app-id")
                .header("x-algolia-api-key", &plaintext_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}
#[tokio::test]
async fn auth_middleware_rejects_non_admin_key_for_own_restore_route() {
    let (_temp_dir, key_store, plaintext_key) =
        create_non_admin_test_key("Own-key restore test key");

    let app = Router::new()
        .route("/1/keys/:key/restore", post(|| async { StatusCode::OK }))
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }))
        .layer(Extension(key_store));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/1/keys/{plaintext_key}/restore"))
                .header("x-algolia-application-id", "app-id")
                .header("x-algolia-api-key", &plaintext_key)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "message": "Method not allowed with this API key",
            "status": 403
        })
    );
}
#[tokio::test]
async fn auth_middleware_rejects_protected_routes_when_keystore_is_missing() {
    let app = Router::new()
        .route(
            "/1/indexes/products/query",
            post(|| async { StatusCode::OK }),
        )
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, false).await
        }));

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/1/indexes/products/query")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body_json(response).await,
        serde_json::json!({
            "message": "Internal server error",
            "status": 500
        })
    );
}
