use axum::{
    body::Body,
    http::{Method, StatusCode},
    routing::{delete, get, post},
    Router,
};
use flapjack::{
    analytics::{schema::SearchEvent, AnalyticsCollector, AnalyticsConfig, AnalyticsQueryEngine},
    experiments::store::ExperimentStore,
};
use flapjack_http::handlers::{self};
use serde_json::{json, Value};
use std::sync::Arc;
use tempfile::TempDir;
mod common;
use common::body_json;

fn sorted_object_keys(json: &Value) -> Vec<String> {
    let mut keys: Vec<String> = json
        .as_object()
        .expect("expected json object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

fn analytics_config(tmp: &TempDir) -> AnalyticsConfig {
    AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    }
}

fn build_stage4_app(tmp: &TempDir) -> (Router, Arc<AnalyticsCollector>) {
    let config = analytics_config(tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let engine = Arc::new(AnalyticsQueryEngine::new(config));

    let state = common::make_test_app_state(
        tmp.path(),
        None,
        None,
        None,
        Some(engine.clone()),
        Some(Arc::new(ExperimentStore::new(tmp.path()).unwrap())),
        None,
    );

    let index_routes = Router::new()
        .route("/1/indexes", post(handlers::create_index))
        .route("/1/indexes/:indexName/batch", post(handlers::add_documents))
        .route("/1/indexes/:indexName/query", post(handlers::search))
        .route("/1/tasks/:task_id", get(handlers::get_task))
        .with_state(state.clone());

    let analytics_routes = Router::new()
        .route("/2/searches", get(handlers::analytics::get_top_searches))
        .route(
            "/2/clicks/clickThroughRate",
            get(handlers::analytics::get_click_through_rate),
        )
        .route(
            "/2/conversions/conversionRate",
            get(handlers::analytics::get_conversion_rate),
        )
        .route(
            "/2/conversions/addToCartRate",
            get(handlers::analytics::get_add_to_cart_rate),
        )
        .route(
            "/2/conversions/purchaseRate",
            get(handlers::analytics::get_purchase_rate),
        )
        .route(
            "/2/conversions/revenue",
            get(handlers::analytics::get_revenue),
        )
        .with_state(engine.clone());

    let experiments_routes = Router::new()
        .route(
            "/2/abtests",
            post(handlers::experiments::create_experiment)
                .get(handlers::experiments::list_experiments),
        )
        .route("/2/abtests/:id", get(handlers::experiments::get_experiment))
        .route(
            "/2/abtests/:id/start",
            post(handlers::experiments::start_experiment),
        )
        .route(
            "/2/abtests/:id/stop",
            post(handlers::experiments::stop_experiment),
        )
        .with_state(state);

    let insights_routes = Router::new()
        .route("/1/events", post(handlers::insights::post_events))
        .with_state(collector.clone());

    let gdpr_routes = Router::new()
        .route(
            "/1/usertokens/:userToken",
            delete(handlers::insights::delete_usertoken),
        )
        .with_state(handlers::insights::GdprDeleteState {
            analytics_collector: collector.clone(),
            profile_store_base_path: tmp.path().to_path_buf(),
        });

    (
        common::apply_test_app_id_layer(
            Router::new()
                .merge(index_routes)
                .merge(analytics_routes)
                .merge(experiments_routes)
                .merge(insights_routes)
                .merge(gdpr_routes),
        ),
        collector,
    )
}

async fn send_json(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
) -> axum::http::Response<Body> {
    common::send_json_response(app, method, uri, body).await
}

async fn send_json_with_headers(
    app: &Router,
    method: Method,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> axum::http::Response<Body> {
    common::send_json_response_with_headers(app, method, uri, body, headers).await
}

async fn wait_for_task_published(app: &Router, task_id: i64) {
    for _ in 0..5000 {
        let resp = send_json(app, Method::GET, &format!("/1/tasks/{task_id}"), None).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body_json(resp).await;
        if body == json!({"status": "published", "pendingTask": false}) {
            return;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }
    panic!("task {task_id} did not reach published");
}

async fn create_index(app: &Router, index_name: &str) {
    let create_resp = send_json(
        app,
        Method::POST,
        "/1/indexes",
        Some(json!({ "uid": index_name })),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_body = body_json(create_resp).await;
    assert_eq!(create_body["uid"], index_name);
}

async fn add_records(app: &Router, index_name: &str, records: Vec<Value>) {
    let requests: Vec<Value> = records
        .into_iter()
        .map(|record| json!({ "action": "addObject", "body": record }))
        .collect();

    let batch_resp = send_json(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/batch"),
        Some(json!({ "requests": requests })),
    )
    .await;
    assert_eq!(batch_resp.status(), StatusCode::OK);

    let batch_body = body_json(batch_resp).await;
    let task_id = batch_body["taskID"].as_i64().unwrap();
    wait_for_task_published(app, task_id).await;
}

fn record_search_event(
    collector: &Arc<AnalyticsCollector>,
    query: &str,
    query_id: &str,
    index_name: &str,
    user_token: &str,
    experiment_id: Option<&str>,
    variant_id: Option<&str>,
) {
    collector.record_search(SearchEvent {
        timestamp_ms: chrono::Utc::now().timestamp_millis(),
        query: query.to_string(),
        query_id: Some(query_id.to_string()),
        index_name: index_name.to_string(),
        nb_hits: 2,
        processing_time_ms: 5,
        user_token: Some(user_token.to_string()),
        user_ip: Some("127.0.0.1".to_string()),
        filters: None,
        facets: None,
        analytics_tags: None,
        page: 0,
        hits_per_page: 20,
        has_results: true,
        country: Some("US".to_string()),
        region: None,
        experiment_id: experiment_id.map(ToString::to_string),
        variant_id: variant_id.map(ToString::to_string),
        assignment_method: experiment_id.map(|_| "user_token".to_string()),
    });
}

#[tokio::test]
async fn insights_to_analytics_full_lifecycle_smoke_has_expected_shapes() {
    let tmp = TempDir::new().unwrap();
    let (app, collector) = build_stage4_app(&tmp);
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

    create_index(&app, "products").await;
    add_records(
        &app,
        "products",
        vec![
            json!({"objectID": "p1", "name": "running shoe"}),
            json!({"objectID": "p2", "name": "trail shoe"}),
        ],
    )
    .await;

    let search_resp = send_json_with_headers(
        &app,
        Method::POST,
        "/1/indexes/products/query",
        Some(json!({ "query": "shoe", "clickAnalytics": true })),
        &[("x-algolia-usertoken", "sdk-user-1")],
    )
    .await;
    assert_eq!(search_resp.status(), StatusCode::OK);
    let search_body = body_json(search_resp).await;
    let query_id = search_body["queryID"].as_str().unwrap().to_string();

    record_search_event(
        &collector,
        "shoe",
        &query_id,
        "products",
        "sdk-user-1",
        None,
        None,
    );

    let events_resp = send_json(
        &app,
        Method::POST,
        "/1/events",
        Some(json!({
            "events": [
                {
                    "eventType": "click",
                    "eventName": "Product Clicked",
                    "index": "products",
                    "userToken": "sdk-user-1",
                    "queryID": query_id,
                    "objectIDs": ["p1"],
                    "positions": [1]
                },
                {
                    "eventType": "conversion",
                    "eventSubtype": "purchase",
                    "eventName": "Purchased",
                    "index": "products",
                    "userToken": "sdk-user-1",
                    "queryID": query_id,
                    "objectIDs": ["p1"],
                    "value": 49.99,
                    "currency": "USD"
                },
                {
                    "eventType": "conversion",
                    "eventSubtype": "addToCart",
                    "eventName": "Added to Cart",
                    "index": "products",
                    "userToken": "sdk-user-1",
                    "queryID": query_id,
                    "objectIDs": ["p1"]
                }
            ]
        })),
    )
    .await;
    assert_eq!(events_resp.status(), StatusCode::OK);

    collector.flush_all();

    let ctr_resp = send_json(
        &app,
        Method::GET,
        &format!("/2/clicks/clickThroughRate?index=products&startDate={today}&endDate={today}"),
        None,
    )
    .await;
    assert_eq!(ctr_resp.status(), StatusCode::OK);
    let ctr_body = body_json(ctr_resp).await;
    assert_eq!(
        sorted_object_keys(&ctr_body),
        vec!["clickCount", "dates", "rate", "trackedSearchCount"]
    );
    assert!(ctr_body["clickCount"].as_i64().unwrap() > 0);
    assert!(ctr_body["trackedSearchCount"].as_i64().unwrap() > 0);

    let purchase_rate_resp = send_json(
        &app,
        Method::GET,
        &format!("/2/conversions/purchaseRate?index=products&startDate={today}&endDate={today}"),
        None,
    )
    .await;
    assert_eq!(purchase_rate_resp.status(), StatusCode::OK);
    let purchase_rate_body = body_json(purchase_rate_resp).await;
    assert_eq!(
        sorted_object_keys(&purchase_rate_body),
        vec!["dates", "purchaseCount", "rate", "trackedSearchCount"]
    );
    assert!(purchase_rate_body["purchaseCount"].as_i64().unwrap() > 0);

    let add_to_cart_resp = send_json(
        &app,
        Method::GET,
        &format!("/2/conversions/addToCartRate?index=products&startDate={today}&endDate={today}"),
        None,
    )
    .await;
    assert_eq!(add_to_cart_resp.status(), StatusCode::OK);
    let add_to_cart_body = body_json(add_to_cart_resp).await;
    assert_eq!(
        sorted_object_keys(&add_to_cart_body),
        vec!["addToCartCount", "dates", "rate", "trackedSearchCount"]
    );
    assert!(add_to_cart_body["addToCartCount"].as_i64().unwrap() > 0);

    let revenue_resp = send_json(
        &app,
        Method::GET,
        &format!("/2/conversions/revenue?index=products&startDate={today}&endDate={today}"),
        None,
    )
    .await;
    assert_eq!(revenue_resp.status(), StatusCode::OK);
    let revenue_body = body_json(revenue_resp).await;
    assert_eq!(
        sorted_object_keys(&revenue_body),
        vec!["currencies", "dates"]
    );
    let usd = &revenue_body["currencies"]["USD"];
    assert_eq!(
        sorted_object_keys(usd),
        vec!["currency", "revenue"],
        "USD payload keys changed"
    );
    assert_eq!(usd["currency"], "USD");
    assert!((usd["revenue"].as_f64().unwrap() - 49.99).abs() < 0.01);

    let revenue_dates = revenue_body["dates"].as_array().unwrap();
    assert_eq!(revenue_dates.len(), 1);
    assert_eq!(
        sorted_object_keys(&revenue_dates[0]),
        vec!["currencies", "date"],
        "revenue day keys changed"
    );

    let searches_resp = send_json(
        &app,
        Method::GET,
        &format!(
            "/2/searches?index=products&startDate={today}&endDate={today}&clickAnalytics=true"
        ),
        None,
    )
    .await;
    assert_eq!(searches_resp.status(), StatusCode::OK);
    let searches_body = body_json(searches_resp).await;
    assert_eq!(sorted_object_keys(&searches_body), vec!["searches"]);
    let top_search = &searches_body["searches"].as_array().unwrap()[0];
    assert_eq!(
        sorted_object_keys(top_search),
        vec![
            "averageClickPosition",
            "clickCount",
            "clickThroughRate",
            "conversionCount",
            "conversionRate",
            "count",
            "nbHits",
            "search",
            "trackedSearchCount"
        ]
    );

    let conversion_rate_resp = send_json(
        &app,
        Method::GET,
        &format!("/2/conversions/conversionRate?index=products&startDate={today}&endDate={today}"),
        None,
    )
    .await;
    assert_eq!(conversion_rate_resp.status(), StatusCode::OK);
    let conversion_rate_body = body_json(conversion_rate_resp).await;
    assert_eq!(
        sorted_object_keys(&conversion_rate_body),
        vec!["conversionCount", "dates", "rate", "trackedSearchCount"]
    );
    assert!(conversion_rate_body["conversionCount"].as_i64().unwrap() >= 2);
}

#[tokio::test]
async fn ab_lifecycle_smoke_populates_variant_metrics_and_honors_stop_side_effects() {
    let tmp = TempDir::new().unwrap();
    let (app, collector) = build_stage4_app(&tmp);

    create_index(&app, "products").await;
    create_index(&app, "products_v2").await;
    add_records(
        &app,
        "products",
        vec![json!({"objectID": "p1", "name": "shoe control"})],
    )
    .await;
    add_records(
        &app,
        "products_v2",
        vec![json!({"objectID": "p2", "name": "shoe variant"})],
    )
    .await;

    let create_resp = send_json(
        &app,
        Method::POST,
        "/2/abtests",
        Some(json!({
            "name": "Stage4 Lifecycle",
            "variants": [
                {
                    "index": "products",
                    "trafficPercentage": 50,
                    "description": "control"
                },
                {
                    "index": "products_v2",
                    "trafficPercentage": 50,
                    "description": "variant"
                }
            ],
            "endAt": "2099-01-01T00:00:00Z",
            "metrics": [{ "name": "clickThroughRate" }]
        })),
    )
    .await;
    assert_eq!(create_resp.status(), StatusCode::OK);
    let create_body = body_json(create_resp).await;
    assert_eq!(
        sorted_object_keys(&create_body),
        vec!["abTestID", "index", "taskID"]
    );
    let ab_test_id = create_body["abTestID"].as_i64().unwrap();
    let start_resp = send_json(
        &app,
        Method::POST,
        &format!("/2/abtests/{ab_test_id}/start"),
        None,
    )
    .await;
    assert_eq!(start_resp.status(), StatusCode::OK);

    let mut user_token_by_arm = std::collections::HashMap::new();
    let mut internal_experiment_id = String::new();

    for i in 0..200 {
        if user_token_by_arm.len() == 2 {
            break;
        }
        let user_token = format!("ab-user-{i}");
        let search_resp = send_json_with_headers(
            &app,
            Method::POST,
            "/1/indexes/products/query",
            Some(json!({ "query": "shoe", "clickAnalytics": true })),
            &[("x-algolia-usertoken", user_token.as_str())],
        )
        .await;
        assert_eq!(search_resp.status(), StatusCode::OK);
        let search_body = body_json(search_resp).await;

        let arm = search_body["abTestVariantID"].as_str().unwrap().to_string();
        internal_experiment_id = search_body["abTestID"].as_str().unwrap().to_string();

        user_token_by_arm.entry(arm).or_insert(user_token);
    }

    assert_eq!(
        user_token_by_arm.len(),
        2,
        "expected both control and variant assignments"
    );

    let query_id_by_arm = std::collections::HashMap::from([
        ("control".to_string(), "a".repeat(32)),
        ("variant".to_string(), "b".repeat(32)),
    ]);

    for (arm, user_token) in &user_token_by_arm {
        record_search_event(
            &collector,
            "shoe",
            query_id_by_arm.get(arm).unwrap(),
            if arm == "variant" {
                "products_v2"
            } else {
                "products"
            },
            user_token,
            Some(&internal_experiment_id),
            Some(arm),
        );
    }

    let events: Vec<Value> = user_token_by_arm
        .iter()
        .flat_map(|(arm, user_token)| {
            let index_name = if arm == "variant" {
                "products_v2"
            } else {
                "products"
            };
            let object_id = if arm == "variant" { "p2" } else { "p1" };
            let query_id = query_id_by_arm.get(arm).unwrap();
            vec![
                json!({
                    "eventType": "click",
                    "eventName": "Click",
                    "index": index_name,
                    "userToken": user_token,
                    "queryID": query_id,
                    "objectIDs": [object_id],
                    "positions": [1]
                }),
                json!({
                    "eventType": "conversion",
                    "eventSubtype": "purchase",
                    "eventName": "Purchase",
                    "index": index_name,
                    "userToken": user_token,
                    "queryID": query_id,
                    "objectIDs": [object_id],
                    "value": 10.0,
                    "currency": "USD"
                }),
            ]
        })
        .collect();

    let events_resp = send_json(
        &app,
        Method::POST,
        "/1/events",
        Some(json!({ "events": events })),
    )
    .await;
    assert_eq!(events_resp.status(), StatusCode::OK);
    collector.flush_all();

    let get_resp = send_json(&app, Method::GET, &format!("/2/abtests/{ab_test_id}"), None).await;
    assert_eq!(get_resp.status(), StatusCode::OK);
    let get_body = body_json(get_resp).await;
    assert_eq!(
        sorted_object_keys(&get_body),
        vec![
            "abTestID",
            "addToCartSignificance",
            "clickSignificance",
            "configuration",
            "conversionSignificance",
            "createdAt",
            "endAt",
            "name",
            "purchaseSignificance",
            "revenueSignificance",
            "startedAt",
            "status",
            "updatedAt",
            "variants"
        ]
    );
    assert_eq!(get_body["status"], "active");

    let variants = get_body["variants"].as_array().unwrap();
    assert_eq!(variants.len(), 2);
    for variant in variants {
        assert!(variant["clickCount"].as_i64().unwrap_or_default() >= 1);
        assert!(variant["conversionCount"].as_i64().unwrap_or_default() >= 1);
    }

    let stop_resp = send_json(
        &app,
        Method::POST,
        &format!("/2/abtests/{ab_test_id}/stop"),
        None,
    )
    .await;
    assert_eq!(stop_resp.status(), StatusCode::OK);
    let stop_body = body_json(stop_resp).await;
    assert_eq!(
        sorted_object_keys(&stop_body),
        vec!["abTestID", "index", "taskID"]
    );

    let stopped_get_resp =
        send_json(&app, Method::GET, &format!("/2/abtests/{ab_test_id}"), None).await;
    assert_eq!(stopped_get_resp.status(), StatusCode::OK);
    let stopped_get_body = body_json(stopped_get_resp).await;
    assert_eq!(stopped_get_body["status"], "stopped");
    assert!(stopped_get_body["stoppedAt"].is_string());
    assert_eq!(stopped_get_body["endAt"], "2099-01-01T00:00:00Z");

    let post_stop_search = send_json_with_headers(
        &app,
        Method::POST,
        "/1/indexes/products/query",
        Some(json!({ "query": "shoe", "clickAnalytics": true })),
        &[("x-algolia-usertoken", "ab-stop-check")],
    )
    .await;
    assert_eq!(post_stop_search.status(), StatusCode::OK);
    let post_stop_search_body = body_json(post_stop_search).await;
    assert!(
        post_stop_search_body.get("abTestID").is_none(),
        "stopped test should not be active for assignment"
    );
    assert!(post_stop_search_body.get("abTestVariantID").is_none());
}
