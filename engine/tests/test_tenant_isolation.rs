use axum::http::{Method, StatusCode};
use serde_json::json;
use std::collections::BTreeMap;
use tower::ServiceExt;

mod common;

const ADMIN_KEY: &str = "test-admin-key-tenant-isolation";

use common::{authed_request, body_json};

async fn create_restricted_key(app: &axum::Router, acl: &[&str], indexes: &[&str]) -> String {
    let create_req = authed_request(
        Method::POST,
        "/1/keys",
        ADMIN_KEY,
        Some(json!({
            "acl": acl,
            "indexes": indexes,
            "description": "tenant isolation restricted key"
        })),
    );
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    body_json(create_resp).await["key"]
        .as_str()
        .expect("create key response must include key")
        .to_string()
}

async fn create_rate_limited_event_key(app: &axum::Router, index: &str) -> String {
    let create_req = authed_request(
        Method::POST,
        "/1/keys",
        ADMIN_KEY,
        Some(json!({
            "acl": ["search"],
            "indexes": [index],
            "maxQueriesPerIPPerHour": 2,
            "description": "SEC-EVENTS-2 bounded event ingress key"
        })),
    );
    let create_resp = app.clone().oneshot(create_req).await.unwrap();
    assert_eq!(create_resp.status(), StatusCode::OK);
    body_json(create_resp).await["key"]
        .as_str()
        .expect("create key response must include key")
        .to_string()
}

fn view_event(
    index: &str,
    event_name: &str,
    user_token: &str,
    object_id: &str,
) -> serde_json::Value {
    json!({
        "eventType": "view",
        "eventName": event_name,
        "index": index,
        "userToken": user_token,
        "objectIDs": [object_id]
    })
}

fn click_event(
    index: &str,
    event_name: &str,
    user_token: &str,
    object_id: &str,
    timestamp_ms: i64,
) -> serde_json::Value {
    json!({
        "eventType": "click",
        "eventName": event_name,
        "index": index,
        "userToken": user_token,
        "objectIDs": [object_id],
        "positions": [1],
        "timestamp": timestamp_ms
    })
}

#[tokio::test]
async fn event_ingress_rate_limit_allows_exact_allowance_then_rejects_without_side_effect() {
    const INDEX: &str = "sec_events_2_rate_limit";
    const ACCEPTED_A: &str = "sec-events-2-accepted-a";
    const ACCEPTED_B: &str = "sec-events-2-accepted-b";
    const REJECTED: &str = "sec-events-2-rejected-first-excess";

    let (app, _tmp, analytics_collector, analytics_engine) =
        common::build_test_app_for_local_requests_with_analytics(Some(ADMIN_KEY));
    let key = create_rate_limited_event_key(&app, INDEX).await;

    // One fixed timestamp keeps validation and the analytics date window on the same known day.
    // Distinct identifiers make an accidentally admitted excess request observable in both stores.
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let analytics_date = chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .expect("current timestamp must be representable")
        .format("%Y-%m-%d")
        .to_string();
    let events = [
        click_event(
            INDEX,
            "SEC Events 2 Accepted A",
            "sec_events_2_user_a",
            ACCEPTED_A,
            timestamp_ms,
        ),
        click_event(
            INDEX,
            "SEC Events 2 Accepted B",
            "sec_events_2_user_b",
            ACCEPTED_B,
            timestamp_ms,
        ),
        click_event(
            INDEX,
            "SEC Events 2 Rejected First Excess",
            "sec_events_2_user_rejected",
            REJECTED,
            timestamp_ms,
        ),
    ];

    for (request_number, event) in events[..2].iter().enumerate() {
        let response = app
            .clone()
            .oneshot(authed_request(
                Method::POST,
                "/1/events",
                &key,
                Some(json!({"events": [event]})),
            ))
            .await
            .unwrap();
        let status = response.status();
        let body = body_json(response).await;
        assert_eq!(
            status,
            StatusCode::OK,
            "configured allowance request {} must succeed: {body}",
            request_number + 1
        );
        assert_eq!(
            body,
            json!({"status": 200, "message": "OK"}),
            "configured allowance request {} must return the exact success body",
            request_number + 1
        );
    }

    let excess_response = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/events",
            &key,
            Some(json!({"events": [events[2].clone()]})),
        ))
        .await
        .unwrap();
    let excess_status = excess_response.status();
    let excess_body = body_json(excess_response).await;
    assert_eq!(
        excess_status,
        StatusCode::TOO_MANY_REQUESTS,
        "SEC_EVENTS_2_ASSERT_FIRST_EXCESS_STATUS body={excess_body}"
    );
    assert_eq!(
        excess_body,
        json!({
            "message": "Too many requests per IP per hour",
            "status": 429
        }),
        "SEC_EVENTS_2_ASSERT_FIRST_EXCESS_BODY"
    );

    // The debug ring and persisted analytics are separate stores. Querying both catches a
    // rejection that returns 429 but still leaks through either side-effect path.
    let debug_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            &format!("/1/events/debug?index={INDEX}"),
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(debug_response.status(), StatusCode::OK);
    let debug_body = body_json(debug_response).await;
    let mut debug_object_ids = debug_body["events"]
        .as_array()
        .expect("debug events must be an array")
        .iter()
        .flat_map(|event| {
            event["objectIds"]
                .as_array()
                .expect("debug objectIds must be an array")
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .expect("debug object ID must be a string")
                        .to_string()
                })
        })
        .collect::<Vec<_>>();
    debug_object_ids.sort();
    assert_eq!(
        debug_object_ids,
        vec![ACCEPTED_A.to_string(), ACCEPTED_B.to_string()],
        "SEC_EVENTS_2_ASSERT_DEBUG_ACCEPTED body={debug_body}"
    );
    assert!(
        !debug_body.to_string().contains(REJECTED),
        "SEC_EVENTS_2_ASSERT_DEBUG_REJECTED body={debug_body}"
    );

    analytics_collector.flush_insights();
    let analytics_body = analytics_engine
        .top_hits(INDEX, &analytics_date, &analytics_date, 10)
        .await
        .expect("analytics query plumbing must succeed");
    // Each accepted request sent exactly one object ID, so exact row shape matters too. Without
    // this check, duplicate rows could be overwritten while reducing the result into a map.
    let analytics_hits = analytics_body["hits"]
        .as_array()
        .expect("analytics hits must be an array");
    assert_eq!(
        analytics_hits.len(),
        2,
        "SEC_EVENTS_2_ASSERT_ANALYTICS_ACCEPTED expected exactly two rows: {analytics_body}"
    );
    let mut analytics_counts = BTreeMap::new();
    for row in analytics_hits {
        let object_ids: Vec<String> = serde_json::from_str(
            row["hit"]
                .as_str()
                .expect("analytics hit must encode object IDs"),
        )
        .expect("analytics hit object IDs must be valid JSON");
        assert_eq!(
            object_ids.len(),
            1,
            "SEC_EVENTS_2_ASSERT_ANALYTICS_ACCEPTED each event sent one object ID: {analytics_body}"
        );
        let count = row["count"]
            .as_u64()
            .expect("analytics hit count must be an unsigned integer");
        for object_id in object_ids {
            *analytics_counts.entry(object_id).or_insert(0) += count;
        }
    }
    assert_eq!(
        analytics_counts,
        BTreeMap::from([(ACCEPTED_A.to_string(), 1), (ACCEPTED_B.to_string(), 1)]),
        "SEC_EVENTS_2_ASSERT_ANALYTICS_ACCEPTED body={analytics_body}"
    );
    assert!(
        !analytics_counts.contains_key(REJECTED),
        "SEC_EVENTS_2_ASSERT_ANALYTICS_REJECTED body={analytics_body}"
    );
}

#[tokio::test]
async fn restricted_key_accepts_event_for_allowed_index() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let key = create_restricted_key(&app, &["search"], &["tenant_allowed"]).await;

    let response = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/events",
            &key,
            Some(json!({
                "events": [view_event(
                    "tenant_allowed",
                    "Allowed Event",
                    "allowed_user",
                    "allowed_object"
                )]
            })),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(status, StatusCode::OK, "allowed event response: {body}");
    assert_eq!(body, json!({"status": 200, "message": "OK"}));

    let debug_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug?index=tenant_allowed",
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    let debug_body = body_json(debug_response).await;
    assert_eq!(debug_body["count"], 1);
    assert_eq!(debug_body["events"][0]["eventName"], "Allowed Event");
}

#[tokio::test]
async fn restricted_key_rejects_forbidden_event_without_recording_it() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let key = create_restricted_key(&app, &["search"], &["tenant_allowed"]).await;

    let response = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/events",
            &key,
            Some(json!({
                "events": [view_event(
                    "tenant_forbidden",
                    "Forbidden Event",
                    "forbidden_user",
                    "forbidden_object"
                )]
            })),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );

    let debug_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug?index=tenant_forbidden",
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    let debug_body = body_json(debug_response).await;
    assert_eq!(
        debug_body,
        json!({"events": [], "count": 0}),
        "authorization failures must not enter the process-global debug buffer"
    );
}

#[tokio::test]
async fn restricted_key_rejects_mixed_event_batch_without_partial_recording() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let key = create_restricted_key(&app, &["search"], &["tenant_allowed"]).await;

    let response = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/events",
            &key,
            Some(json!({
                "events": [
                    view_event("tenant_allowed", "Allowed Half", "allowed_user", "allowed_object"),
                    view_event("tenant_forbidden", "Forbidden Half", "forbidden_user", "forbidden_object")
                ]
            })),
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );

    let debug_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug",
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    let debug_body = body_json(debug_response).await;
    assert_eq!(
        debug_body,
        json!({"events": [], "count": 0}),
        "a mixed-index request must be authorized atomically before recording any event"
    );
}

#[tokio::test]
async fn index_restricted_analytics_key_only_reads_allowed_debug_events() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let ingest_response = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/events",
            ADMIN_KEY,
            Some(json!({
                "events": [
                    view_event("tenant_allowed", "Allowed Debug", "allowed_user", "allowed_object"),
                    view_event("tenant_forbidden", "Forbidden Debug", "forbidden_user", "forbidden_object")
                ]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);
    let analytics_key = create_restricted_key(&app, &["analytics"], &["tenant_allowed"]).await;

    let response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug?limit=1",
            &analytics_key,
            None,
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = body_json(response).await;
    assert_eq!(status, StatusCode::OK, "debug response: {body}");
    assert_eq!(body["count"], 1);
    assert_eq!(body["events"][0]["index"], "tenant_allowed");
    assert_eq!(body["events"][0]["userToken"], "allowed_user");
    assert_eq!(body["events"][0]["objectIds"], json!(["allowed_object"]));
    assert!(
        !body.to_string().contains("forbidden_user")
            && !body.to_string().contains("forbidden_object"),
        "debug response exposed a forbidden tenant event: {body}"
    );

    let forbidden_query_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug?index=tenant_forbidden",
            &analytics_key,
            None,
        ))
        .await
        .unwrap();
    let status = forbidden_query_response.status();
    let body = common::assert_error_contract_from_oneshot(forbidden_query_response, 403).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn search_only_key_cannot_read_debug_events() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let search_key = create_restricted_key(&app, &["search"], &[]).await;

    let response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug",
            &search_key,
            None,
        ))
        .await
        .unwrap();
    let status = response.status();
    let body = common::assert_error_contract_from_oneshot(response, 403).await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        body,
        json!({
            "message": "Method not allowed with this API key",
            "status": 403
        })
    );
}

#[tokio::test]
async fn batch_search_restricted_key_rejects_mixed_allowed_and_forbidden_indexes() {
    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));

    common::seed_docs(
        &app,
        "tenant_allowed",
        ADMIN_KEY,
        vec![json!({"objectID": "allowed-1", "name": "Allowed Document"})],
    )
    .await;

    let key_value = create_restricted_key(&app, &["search"], &["tenant_allowed"]).await;

    let mixed_batch_req = authed_request(
        Method::POST,
        "/1/indexes/*/queries",
        &key_value,
        Some(json!({
            "requests": [
                {"indexName": "tenant_allowed", "query": "Allowed"},
                {"indexName": "tenant_forbidden", "query": "Forbidden"}
            ]
        })),
    );
    let mixed_batch_resp = app.clone().oneshot(mixed_batch_req).await.unwrap();
    let status = mixed_batch_resp.status();
    let body = common::assert_error_contract_from_oneshot(mixed_batch_resp, 403).await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "mixed-index batch search must be denied when any query targets a forbidden index",
    );
    assert_eq!(
        body,
        json!({
            "message": "Invalid Application-ID or API key",
            "status": 403
        }),
        "index-restricted batch rejection must use canonical invalid-credentials envelope",
    );
}
