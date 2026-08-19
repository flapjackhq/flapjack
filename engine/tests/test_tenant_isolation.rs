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
async fn gdpr_delete_removes_only_target_users_event_debug_records() {
    const INDEX: &str = "gdpr_event_debug";
    const TARGET_USER: &str = "gdpr_event_debug_target";
    const CONTROL_USER: &str = "gdpr_event_debug_control";

    let (app, _tmp, _analytics_collector, _analytics_engine) =
        common::build_test_app_for_local_requests_with_analytics(Some(ADMIN_KEY));
    let event_key = create_restricted_key(&app, &["search"], &[INDEX]).await;

    let ingest_response = app
        .clone()
        .oneshot(authed_request(
            Method::POST,
            "/1/events",
            &event_key,
            Some(json!({
                "events": [
                    view_event(INDEX, "GDPR Target", TARGET_USER, "target-object"),
                    view_event(INDEX, "GDPR Control", CONTROL_USER, "control-object")
                ]
            })),
        ))
        .await
        .unwrap();
    assert_eq!(ingest_response.status(), StatusCode::OK);

    let debug_users = |body: &serde_json::Value| {
        body["events"]
            .as_array()
            .expect("debug events must be an array")
            .iter()
            .map(|event| {
                event["userToken"]
                    .as_str()
                    .expect("debug userToken must be a string")
                    .to_string()
            })
            .collect::<Vec<_>>()
    };

    let before_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            &format!("/1/events/debug?index={INDEX}"),
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(before_response.status(), StatusCode::OK);
    let mut before_users = debug_users(&body_json(before_response).await);
    before_users.sort();
    assert_eq!(
        before_users,
        vec![CONTROL_USER.to_string(), TARGET_USER.to_string()],
        "precondition: both users must be observable before deletion"
    );

    let delete_response = app
        .clone()
        .oneshot(authed_request(
            Method::DELETE,
            &format!("/1/usertokens/{TARGET_USER}"),
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(delete_response.status(), StatusCode::OK);

    let after_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            &format!("/1/events/debug?index={INDEX}"),
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(after_response.status(), StatusCode::OK);
    let after_body = body_json(after_response).await;
    assert_eq!(
        debug_users(&after_body),
        vec![CONTROL_USER.to_string()],
        "GDPR deletion must remove target debug PII without deleting another user's record: {after_body}"
    );
}

#[tokio::test]
async fn event_ingress_idempotency_replay_has_no_duplicate_or_cross_index_side_effect() {
    const INDEX_A: &str = "sec_events_idempotency_a";
    const INDEX_B: &str = "sec_events_idempotency_b";
    const IDEMPOTENCY_KEY: &str = "event-retry-01";

    let (app, _tmp, analytics_collector, analytics_engine) =
        common::build_test_app_for_local_requests_with_analytics(Some(ADMIN_KEY));
    let key_a = create_restricted_key(&app, &["search"], &[INDEX_A]).await;
    let key_b = create_restricted_key(&app, &["search"], &[INDEX_B]).await;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let analytics_date = chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .expect("current timestamp must be representable")
        .format("%Y-%m-%d")
        .to_string();

    let event_a = click_event(
        INDEX_A,
        "Idempotent Event A",
        "idempotent_user_a",
        "idempotent_object_a",
        timestamp_ms,
    );
    let event_b = click_event(
        INDEX_B,
        "Idempotent Event B",
        "idempotent_user_b",
        "idempotent_object_b",
        timestamp_ms,
    );

    let first = common::send_authed_response(
        &app,
        Method::POST,
        "/1/events",
        &key_a,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({"events": [event_a.clone()]})),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    assert!(
        first
            .headers()
            .get("x-flapjack-idempotency-replayed")
            .is_none(),
        "the first accepted event must not be marked as a replay"
    );
    assert_eq!(
        body_json(first).await,
        json!({"status": 200, "message": "OK"})
    );

    let replay = common::send_authed_response(
        &app,
        Method::POST,
        "/1/events",
        &key_a,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({"events": [event_a]})),
    )
    .await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(
        replay
            .headers()
            .get("x-flapjack-idempotency-replayed")
            .and_then(|value| value.to_str().ok()),
        Some("true"),
        "a retry must replay the first response before recording another event"
    );
    assert_eq!(
        body_json(replay).await,
        json!({"status": 200, "message": "OK"})
    );

    let other_index = common::send_authed_response(
        &app,
        Method::POST,
        "/1/events",
        &key_b,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({"events": [event_b]})),
    )
    .await;
    assert_eq!(other_index.status(), StatusCode::OK);
    assert!(
        other_index
            .headers()
            .get("x-flapjack-idempotency-replayed")
            .is_none(),
        "the same token on a different authorized index must remain independent"
    );
    assert_eq!(
        body_json(other_index).await,
        json!({"status": 200, "message": "OK"})
    );

    for (index, expected_object) in [
        (INDEX_A, "idempotent_object_a"),
        (INDEX_B, "idempotent_object_b"),
    ] {
        let debug_response = app
            .clone()
            .oneshot(authed_request(
                Method::GET,
                &format!("/1/events/debug?index={index}"),
                ADMIN_KEY,
                None,
            ))
            .await
            .unwrap();
        assert_eq!(debug_response.status(), StatusCode::OK);
        let debug_body = body_json(debug_response).await;
        assert_eq!(debug_body["count"], 1, "debug body: {debug_body}");
        assert_eq!(
            debug_body["events"][0]["objectIds"],
            json!([expected_object])
        );
    }

    analytics_collector.flush_insights();
    for (index, expected_object) in [
        (INDEX_A, "idempotent_object_a"),
        (INDEX_B, "idempotent_object_b"),
    ] {
        let analytics_body = analytics_engine
            .top_hits(index, &analytics_date, &analytics_date, 10)
            .await
            .expect("analytics query must succeed");
        let hits = analytics_body["hits"]
            .as_array()
            .expect("analytics hits must be an array");
        assert_eq!(hits.len(), 1, "analytics body: {analytics_body}");
        assert_eq!(hits[0]["count"], 1, "analytics body: {analytics_body}");
        let object_ids: Vec<String> = serde_json::from_str(
            hits[0]["hit"]
                .as_str()
                .expect("analytics hit must encode object IDs"),
        )
        .expect("analytics hit must be valid JSON");
        assert_eq!(object_ids, vec![expected_object]);
    }
}

#[tokio::test]
async fn event_idempotency_does_not_replay_object_response_from_colliding_index_name() {
    const EVENT_INDEX: &str = "products";
    const OBJECT_INDEX: &str = "[\"products\"]";
    const IDEMPOTENCY_KEY: &str = "cross-route-retry-01";

    assert!(
        flapjack::validate_index_name(OBJECT_INDEX).is_ok(),
        "the regression requires the old event scope to be a valid object index"
    );
    let event_scope = flapjack_http::idempotency::event_index_set_segment([EVENT_INDEX]);
    assert!(
        flapjack::validate_index_name(&event_scope).is_err(),
        "event cache segments must stay outside the valid object-index namespace"
    );

    let (app, _tmp, analytics_collector, analytics_engine) =
        common::build_test_app_for_local_requests_with_analytics(Some(ADMIN_KEY));
    let event_key = create_restricted_key(&app, &["search"], &[EVENT_INDEX]).await;
    let timestamp_ms = chrono::Utc::now().timestamp_millis();
    let analytics_date = chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .expect("current timestamp must be representable")
        .format("%Y-%m-%d")
        .to_string();

    let object_response = common::send_authed_response(
        &app,
        Method::POST,
        "/1/indexes/%5B%22products%22%5D",
        ADMIN_KEY,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({"name": "literal collision fixture"})),
    )
    .await;
    assert_eq!(object_response.status(), StatusCode::CREATED);
    assert!(object_response
        .headers()
        .get("x-flapjack-idempotency-replayed")
        .is_none());
    let object_body = body_json(object_response).await;
    assert!(
        object_body["objectID"].is_string(),
        "object create response: {object_body}"
    );

    let event_response = common::send_authed_response(
        &app,
        Method::POST,
        "/1/events",
        &event_key,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({
            "events": [click_event(
                EVENT_INDEX,
                "Cross Route Event",
                "cross_route_user",
                "cross_route_object",
                timestamp_ms,
            )]
        })),
    )
    .await;
    assert_eq!(
        event_response.status(),
        StatusCode::OK,
        "an object cache entry must not replace the event response"
    );
    assert!(
        event_response
            .headers()
            .get("x-flapjack-idempotency-replayed")
            .is_none(),
        "the first event request must not replay another route's cache entry"
    );
    assert_eq!(
        body_json(event_response).await,
        json!({"status": 200, "message": "OK"})
    );

    let debug_response = app
        .clone()
        .oneshot(authed_request(
            Method::GET,
            "/1/events/debug?index=products",
            ADMIN_KEY,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(debug_response.status(), StatusCode::OK);
    let debug_body = body_json(debug_response).await;
    assert_eq!(debug_body["count"], 1, "debug body: {debug_body}");
    assert_eq!(
        debug_body["events"][0]["objectIds"],
        json!(["cross_route_object"])
    );

    analytics_collector.flush_insights();
    let analytics_body = analytics_engine
        .top_hits(EVENT_INDEX, &analytics_date, &analytics_date, 10)
        .await
        .expect("analytics query must succeed");
    let hits = analytics_body["hits"]
        .as_array()
        .expect("analytics hits must be an array");
    assert_eq!(hits.len(), 1, "analytics body: {analytics_body}");
    assert_eq!(hits[0]["count"], 1, "analytics body: {analytics_body}");
    let object_ids: Vec<String> = serde_json::from_str(
        hits[0]["hit"]
            .as_str()
            .expect("analytics hit must encode object IDs"),
    )
    .expect("analytics hit must be valid JSON");
    assert_eq!(object_ids, vec!["cross_route_object"]);
}

#[tokio::test]
async fn invalid_object_index_does_not_replay_colliding_event_response() {
    const EVENT_INDEX: &str = "products";
    const IDEMPOTENCY_KEY: &str = "reverse-cross-route-retry-01";

    let (app, _tmp) = common::build_test_app_for_local_requests(Some(ADMIN_KEY));
    let event_response = common::send_authed_response(
        &app,
        Method::POST,
        "/1/events",
        ADMIN_KEY,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({
            "events": [view_event(
                EVENT_INDEX,
                "Reverse Cross Route Event",
                "reverse_cross_route_user",
                "reverse_cross_route_object",
            )]
        })),
    )
    .await;
    assert_eq!(event_response.status(), StatusCode::OK);
    assert!(
        event_response
            .headers()
            .get("x-flapjack-idempotency-replayed")
            .is_none(),
        "the first event request must populate, not replay, its cache entry"
    );
    assert_eq!(
        body_json(event_response).await,
        json!({"status": 200, "message": "OK"})
    );

    let object_response = common::send_authed_response(
        &app,
        Method::POST,
        "/1/indexes/%2Fevents%2F%5B%22products%22%5D",
        ADMIN_KEY,
        "test",
        &[("x-flapjack-idempotency-key", IDEMPOTENCY_KEY)],
        Some(json!({"name": "must not be created"})),
    )
    .await;
    assert_eq!(
        object_response.status(),
        StatusCode::BAD_REQUEST,
        "an invalid decoded object index must be rejected before cache lookup"
    );
    assert!(
        object_response
            .headers()
            .get("x-flapjack-idempotency-replayed")
            .is_none(),
        "invalid object targets must not replay another route's response"
    );
    assert_eq!(
        body_json(object_response).await,
        json!({
            "message": "Index name contains invalid characters (path traversal not allowed)",
            "status": 400
        })
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
