mod common;

use axum::http::{Method, StatusCode};
use common::{build_test_app_for_local_requests, extract_task_id, send_json, wait_for_task_local};
use serde_json::{json, Value};
use std::collections::HashMap;

#[tokio::test]
async fn empty_query_search_preserves_boolean_fields() {
    let (app, _dir) = build_test_app_for_local_requests(None);

    let (write_status, write_body) = send_json(
        &app,
        Method::POST,
        "/1/indexes/boolean_roundtrip/batch",
        "test-key",
        Some(json!({
            "requests": [
                {
                    "action": "addObject",
                    "body": {
                        "objectID": "helm-live-a",
                        "name": "Amber Wrench",
                        "price": 17,
                        "featured": true
                    }
                },
                {
                    "action": "addObject",
                    "body": {
                        "objectID": "helm-live-b",
                        "name": "Blue Gauge",
                        "price": 29,
                        "featured": false
                    }
                }
            ]
        })),
    )
    .await;
    assert_eq!(write_status, StatusCode::OK);
    wait_for_task_local(&app, extract_task_id(&write_body)).await;

    let (search_status, search_body) = send_json(
        &app,
        Method::POST,
        "/1/indexes/boolean_roundtrip/query",
        "test-key",
        Some(json!({"query": ""})),
    )
    .await;
    assert_eq!(search_status, StatusCode::OK);
    assert_eq!(search_body["nbHits"], 2);

    let hits = search_body["hits"]
        .as_array()
        .expect("hits must be an array");
    let hits_by_id: HashMap<&str, &Value> = hits
        .iter()
        .map(|hit| {
            (
                hit["objectID"]
                    .as_str()
                    .expect("hit objectID must be string"),
                hit,
            )
        })
        .collect();

    assert_eq!(hits_by_id.len(), 2);
    assert_eq!(hits_by_id["helm-live-a"]["name"], "Amber Wrench");
    assert_eq!(hits_by_id["helm-live-a"]["price"], 17);
    assert_eq!(hits_by_id["helm-live-a"]["featured"], true);
    assert_eq!(hits_by_id["helm-live-b"]["name"], "Blue Gauge");
    assert_eq!(hits_by_id["helm-live-b"]["price"], 29);
    assert_eq!(hits_by_id["helm-live-b"]["featured"], false);
}
