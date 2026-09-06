use serde::de::DeserializeOwned;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::body::{to_bytes, Body};
use axum::http::{Method, Request, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api_profile::ApiProfile;
use crate::auth::{
    authenticate_and_authorize, ApiKey, KeyStore, RateLimiter, ReplicationPeerCredential,
};
use crate::handlers::crawler::{
    CrawlerRunAckResponse, CrawlerRunCancelResponse, CrawlerRunId, CrawlerRunIdAdmissionError,
    CrawlerRunOutcome, CrawlerRunStartRequest,
};
use crate::openapi_export::{default_pbv4_crawler_output_path, OpenApiDocument};
use crate::test_helpers::TestStateBuilder;

const ADMIN_KEY: &str = "pbv4-crawler-contract-admin";
const APPLICATION_ID: &str = "flapjack";
const VALID_RUN_ID: &str = "018f3e2a-7b1c-7d45-8c90-1234567890ab";
const NON_V7_RUN_ID: &str = "550e8400-e29b-41d4-a716-446655440000";
const NON_RFC_V7_RUN_ID: &str = "018f3e2a-7b1c-7d45-0c90-1234567890ab";
const INVALID_API_KEY: &str = "pbv4-crawler-contract-invalid-key";

fn fixture(name: &str) -> serde_json::Value {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/pbv4_crawler_wire");
    serde_json::from_str(
        &std::fs::read_to_string(root.join(name)).expect("crawler golden fixture must be readable"),
    )
    .expect("crawler golden fixture must be valid JSON")
}

fn assert_golden_round_trip<T>(name: &str)
where
    T: DeserializeOwned + Serialize,
{
    let expected = fixture(name);
    let parsed: T =
        serde_json::from_value(expected.clone()).expect("fixture must match frozen DTO");
    assert_eq!(
        serde_json::to_value(parsed).expect("frozen DTO must serialize"),
        expected,
        "{name} must remain byte-shape compatible across repositories"
    );
}

#[test]
fn pbv4_crawler_wire_golden_serialization_is_exact() {
    assert_golden_round_trip::<CrawlerRunStartRequest>("start_request.json");
    assert_golden_round_trip::<CrawlerRunOutcome>("succeeded_outcome.json");
    assert_golden_round_trip::<CrawlerRunOutcome>("canceled_outcome.json");
    assert_golden_round_trip::<CrawlerRunOutcome>("failed_outcome.json");
    assert_golden_round_trip::<CrawlerRunCancelResponse>("cancel_response.json");
    assert_golden_round_trip::<CrawlerRunAckResponse>("ack_response.json");

    let mut unknown_field = fixture("start_request.json");
    unknown_field["customer_id"] = serde_json::json!("must-not-cross-wire");
    assert!(serde_json::from_value::<CrawlerRunStartRequest>(unknown_field).is_err());

    let mut unknown_version = fixture("succeeded_outcome.json");
    unknown_version["schema_version"] = serde_json::json!(2);
    assert!(serde_json::from_value::<CrawlerRunOutcome>(unknown_version).is_err());

    for invalid_run_id in [NON_V7_RUN_ID, NON_RFC_V7_RUN_ID] {
        let mut invalid_start = fixture("start_request.json");
        invalid_start["run_id"] = serde_json::json!(invalid_run_id);
        assert!(serde_json::from_value::<CrawlerRunStartRequest>(invalid_start).is_err());

        for fixture_name in [
            "succeeded_outcome.json",
            "canceled_outcome.json",
            "failed_outcome.json",
        ] {
            let mut invalid_outcome = fixture(fixture_name);
            invalid_outcome["run_id"] = serde_json::json!(invalid_run_id);
            assert!(serde_json::from_value::<CrawlerRunOutcome>(invalid_outcome).is_err());
        }
        let mut invalid_cancel = fixture("cancel_response.json");
        invalid_cancel["run_id"] = serde_json::json!(invalid_run_id);
        assert!(serde_json::from_value::<CrawlerRunCancelResponse>(invalid_cancel).is_err());
        let mut invalid_ack = fixture("ack_response.json");
        invalid_ack["run_id"] = serde_json::json!(invalid_run_id);
        assert!(serde_json::from_value::<CrawlerRunAckResponse>(invalid_ack).is_err());
    }
}

#[test]
fn pbv4_crawler_version_seven_with_non_rfc_variant_is_not_a_run_id() {
    let uuid = uuid::Uuid::parse_str(NON_RFC_V7_RUN_ID).unwrap();
    assert_eq!(uuid.get_version_num(), 7);
    assert_eq!(uuid.get_variant(), uuid::Variant::NCS);
    assert!(CrawlerRunId::try_from(uuid).is_err());
}

#[test]
fn pbv4_crawler_run_id_admission_uses_injected_clock_and_exact_replay_window() {
    let uuid = uuid::Uuid::parse_str(VALID_RUN_ID).unwrap();
    let generated_at = UNIX_EPOCH + Duration::from_millis((uuid.as_u128() >> 80) as u64);
    let run_id = CrawlerRunId::try_from(uuid).unwrap();

    assert_eq!(run_id.validate_admission_at(generated_at), Ok(()));
    assert_eq!(
        run_id.validate_admission_at(generated_at + CrawlerRunId::REPLAY_WINDOW),
        Ok(())
    );
    assert_eq!(
        run_id.validate_admission_at(
            generated_at + CrawlerRunId::REPLAY_WINDOW + Duration::from_millis(1)
        ),
        Err(CrawlerRunIdAdmissionError::Expired)
    );
    assert_eq!(
        run_id.validate_admission_at(generated_at - Duration::from_millis(1)),
        Err(CrawlerRunIdAdmissionError::Future)
    );
}

#[test]
fn pbv4_crawler_openapi_is_closed_and_node_admin_only() {
    let doc = serde_json::to_value(crate::openapi::pbv4_crawler_openapi()).unwrap();
    let paths = doc["paths"].as_object().expect("crawler paths must exist");
    assert_eq!(
        paths.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "/internal/crawler/runs",
            "/internal/crawler/runs/{run_id}",
            "/internal/crawler/runs/{run_id}/ack",
            "/internal/crawler/runs/{run_id}/cancel",
        ])
    );
    for operation in [
        &paths["/internal/crawler/runs"]["post"],
        &paths["/internal/crawler/runs/{run_id}"]["get"],
        &paths["/internal/crawler/runs/{run_id}/ack"]["post"],
        &paths["/internal/crawler/runs/{run_id}/cancel"]["post"],
    ] {
        assert_eq!(
            operation["security"],
            serde_json::json!([{"application_id": [], "api_key": []}])
        );
        assert!(operation["responses"].get("403").is_none());
        assert!(operation["responses"]["404"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("concealed")));
    }
    assert_eq!(
        doc["components"]["securitySchemes"]["application_id"]["name"],
        "x-algolia-application-id"
    );
    assert_eq!(
        doc["components"]["securitySchemes"]["api_key"]["name"],
        "x-algolia-api-key"
    );
    assert_eq!(
        doc["components"]["schemas"]["CrawlerRunId"]["pattern"],
        "^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-7[0-9a-fA-F]{3}-[89abAB][0-9a-fA-F]{3}-[0-9a-fA-F]{12}$"
    );

    let serialized = serde_json::to_string(&doc).unwrap();
    for forbidden in [
        "customer_id",
        "billing",
        "aws",
        "api_key_payload",
        "engine_endpoint",
        "credential",
    ] {
        assert!(
            !serialized.contains(forbidden),
            "crawler wire must not contain forbidden identity/secret field {forbidden}"
        );
    }
}

#[test]
fn pbv4_crawler_openapi_recursively_closes_every_object_schema() {
    fn assert_closed(value: &serde_json::Value, path: &str) {
        if value.get("type") == Some(&serde_json::json!("object")) {
            assert_eq!(
                value.get("additionalProperties"),
                Some(&serde_json::json!(false)),
                "object schema at {path} must be recursively closed"
            );
        }
        match value {
            serde_json::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    assert_closed(value, &format!("{path}/{index}"));
                }
            }
            serde_json::Value::Object(values) => {
                for (name, value) in values {
                    assert_closed(value, &format!("{path}/{name}"));
                }
            }
            _ => {}
        }
    }

    let doc = serde_json::to_value(crate::openapi::pbv4_crawler_openapi()).unwrap();
    assert_closed(&doc["components"]["schemas"], "/components/schemas");
}

fn restricted_key() -> ApiKey {
    ApiKey {
        hash: String::new(),
        salt: String::new(),
        hmac_key: None,
        created_at: 0,
        acl: vec!["search".to_string(), "browse".to_string()],
        description: "PBV4 crawler middleware refusal fixture".to_string(),
        indexes: vec!["tenant_123_products".to_string()],
        max_hits_per_query: 0,
        max_queries_per_ip_per_hour: 0,
        query_parameters: String::new(),
        referers: vec![],
        restrict_sources: None,
        validity: 0,
    }
}

fn crawler_contract_app() -> (TempDir, Router, String) {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let key_store = Arc::new(KeyStore::load_or_create(temp.path(), ADMIN_KEY));
    let (_, restricted_key) = key_store.create_key(restricted_key());
    let app = Router::new()
        .route(
            "/internal/crawler/runs",
            post(crate::handlers::crawler::start_crawler_run),
        )
        .route(
            "/internal/crawler/runs/:run_id",
            get(crate::handlers::crawler::get_crawler_run),
        )
        .route(
            "/internal/crawler/runs/:run_id/cancel",
            post(crate::handlers::crawler::cancel_crawler_run),
        )
        .route(
            "/internal/crawler/runs/:run_id/ack",
            post(crate::handlers::crawler::ack_crawler_run),
        )
        .with_state(state)
        .layer(axum::middleware::from_fn(|request, next| async move {
            authenticate_and_authorize(request, next, true).await
        }))
        .layer(Extension(ApiProfile::PaidBetaV4))
        .layer(Extension(key_store))
        .layer(Extension(ReplicationPeerCredential::from_optional_secret(
            None,
        )))
        .layer(Extension(RateLimiter::new()));
    (temp, app, restricted_key)
}

fn unauthenticated_crawler_routes(state: Arc<crate::handlers::AppState>) -> Router {
    let mutation_permit = state.global_mutation_fence.try_admit_mutation().unwrap();
    Router::new()
        .route(
            "/internal/crawler/runs",
            post(crate::handlers::crawler::start_crawler_run),
        )
        .route(
            "/internal/crawler/runs/:run_id",
            get(crate::handlers::crawler::get_crawler_run),
        )
        .route(
            "/internal/crawler/runs/:run_id/cancel",
            post(crate::handlers::crawler::cancel_crawler_run),
        )
        .route(
            "/internal/crawler/runs/:run_id/ack",
            post(crate::handlers::crawler::ack_crawler_run),
        )
        .with_state(state)
        .layer(Extension(mutation_permit))
}

fn fresh_run_id() -> String {
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let random = Uuid::new_v4().as_u128();
    let value = (timestamp_ms << 80) | (0x7_u128 << 76) | (random & ((1_u128 << 76) - 1));
    Uuid::from_u128((value & !(0x3_u128 << 62)) | (0x2_u128 << 62)).to_string()
}

fn fresh_start_fixture(run_id: &str) -> serde_json::Value {
    let mut start = fixture("start_request.json");
    start["run_id"] = serde_json::json!(run_id);
    // Admission fails locally and deterministically after the durable Running
    // response; no permanent HTTP test depends on the network.
    start["start_url"] = serde_json::json!("http://127.0.0.1/private?secret=must-not-leak");
    start["limits"]["max_records"] = serde_json::json!(100);
    start
}

async fn response_json(response: axum::response::Response) -> (StatusCode, serde_json::Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let body = serde_json::from_slice(&bytes).unwrap();
    (status, body)
}

fn crawler_request(
    method: Method,
    path: &str,
    application_id: Option<&str>,
    api_key: Option<&str>,
    body: Option<serde_json::Value>,
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(path);
    if let Some(application_id) = application_id {
        builder = builder.header("x-algolia-application-id", application_id);
    }
    if let Some(api_key) = api_key {
        builder = builder.header("x-algolia-api-key", api_key);
    }
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    builder
        .body(Body::from(
            body.map_or_else(String::new, |body| body.to_string()),
        ))
        .unwrap()
}

fn operation_specimens(run_id: &str) -> [(Method, String, Option<serde_json::Value>); 4] {
    let mut start = fixture("start_request.json");
    start["run_id"] = serde_json::json!(run_id);
    [
        (
            Method::POST,
            "/internal/crawler/runs".to_string(),
            Some(start),
        ),
        (
            Method::POST,
            format!("/internal/crawler/runs/{run_id}/cancel"),
            None,
        ),
        (
            Method::GET,
            format!("/internal/crawler/runs/{run_id}"),
            None,
        ),
        (
            Method::POST,
            format!("/internal/crawler/runs/{run_id}/ack"),
            None,
        ),
    ]
}

#[tokio::test]
async fn pbv4_crawler_real_middleware_requires_both_headers_and_conceals_auth_failures() {
    let (_temp, app, restricted_key) = crawler_contract_app();
    for (method, path, body) in operation_specimens(VALID_RUN_ID) {
        for (application_id, api_key, case) in [
            (None, Some(ADMIN_KEY), "missing application ID"),
            (
                Some("wrong-application"),
                Some(ADMIN_KEY),
                "invalid application ID",
            ),
            (Some(APPLICATION_ID), None, "missing admin key"),
            (
                Some(APPLICATION_ID),
                Some(INVALID_API_KEY),
                "invalid admin key",
            ),
            (
                Some(APPLICATION_ID),
                Some(restricted_key.as_str()),
                "non-admin key",
            ),
        ] {
            let response = app
                .clone()
                .oneshot(crawler_request(
                    method.clone(),
                    &path,
                    application_id,
                    api_key,
                    body.clone(),
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "{case} must be concealed for {method} {path}"
            );
        }

        let valid = app
            .clone()
            .oneshot(crawler_request(
                method.clone(),
                &path,
                Some(APPLICATION_ID),
                Some(ADMIN_KEY),
                body,
            ))
            .await
            .unwrap();
        let expected = if method == Method::GET || path.ends_with("/ack") {
            StatusCode::NOT_FOUND
        } else {
            // This historical UUIDv7 is intentionally outside today's replay
            // window; authenticated start/cancel reaches handler admission.
            StatusCode::BAD_REQUEST
        };
        assert_eq!(
            valid.status(),
            expected,
            "valid middleware credentials must reach crawler admission for {method} {path}"
        );
    }
}

#[tokio::test]
async fn crawler_http_start_is_durable_prompt_and_conflicting_replay_fails_safely() {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let app = unauthenticated_crawler_routes(Arc::clone(&state));
    let run_id = fresh_run_id();
    let start = fresh_start_fixture(&run_id);

    let (status, running) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(start.clone()),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(running["status"], "running");
    assert_eq!(running["run_id"], run_id);
    let durable_run =
        flapjack::index::manager::publication::CrawlerRunStore::new(&state.manager.base_path)
            .load(&run_id)
            .unwrap()
            .unwrap()
            .crawler_run
            .unwrap();
    assert_eq!(
        durable_run.deadline_at_unix_ms.unwrap() - durable_run.started_at_unix_ms.unwrap(),
        start["max_run_duration_ms"].as_u64().unwrap(),
        "HTTP admission must durably retain the caller's bounded duration"
    );

    let (replay_status, replay) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(start.clone()),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(replay["run_id"], run_id);

    let mut conflicting = start;
    conflicting["destination_index"] = serde_json::json!("other_physical_destination");
    let (conflict_status, conflict) = response_json(
        app.oneshot(crawler_request(
            Method::POST,
            "/internal/crawler/runs",
            None,
            None,
            Some(conflicting),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(conflict_status, StatusCode::CONFLICT);
    let rendered = conflict.to_string();
    assert!(!rendered.contains("must-not-leak"));
    assert!(!rendered.contains("127.0.0.1"));
    assert!(!rendered.contains(temp.path().to_string_lossy().as_ref()));
}

#[tokio::test]
async fn crawler_http_retained_truth_replays_after_uuid_admission_window() {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let start = fresh_start_fixture(VALID_RUN_ID);
    let typed: CrawlerRunStartRequest = serde_json::from_value(start.clone()).unwrap();
    let digest = flapjack::index::manager::publication::ContentDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&typed).unwrap()))
    ))
    .unwrap();
    let store =
        flapjack::index::manager::publication::CrawlerRunStore::new(&state.manager.base_path);
    store.start(VALID_RUN_ID, digest, 1).unwrap();
    store
        .finish_without_publication(
            VALID_RUN_ID,
            flapjack::index::manager::publication::CrawlerTerminalOutcome::Canceled,
            Default::default(),
            1,
            2,
        )
        .unwrap();

    let (status, replay) = response_json(
        unauthenticated_crawler_routes(state)
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(start),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay["status"], "canceled");
}

#[tokio::test]
async fn crawler_http_transform_validation_is_mutation_effective() {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let app = unauthenticated_crawler_routes(state);
    let run_id = fresh_run_id();
    let mut start = fresh_start_fixture(&run_id);
    let four = serde_json::json!([
        {"source": "url", "output": "url"},
        {"source": "title", "output": "title"},
        {"source": "metadata", "output": "metadata.path"},
        {"source": "text", "output": "text-value"}
    ]);
    let mut five = four.as_array().unwrap().clone();
    five.push(serde_json::json!({"source": "url", "output": "fifth"}));
    for invalid_fields in [
        serde_json::json!([]),
        serde_json::Value::Array(five),
        serde_json::json!([{"source": "url", "output": "a".repeat(65)}]),
        serde_json::json!([{"source": "url", "output": "9leading"}]),
        serde_json::json!([{"source": "url", "output": "bad/slash"}]),
        serde_json::json!([{"source": "url", "output": "objectID"}]),
    ] {
        start["transform"]["fields"] = invalid_fields;
        let (invalid_status, invalid) = response_json(
            app.clone()
                .oneshot(crawler_request(
                    Method::POST,
                    "/internal/crawler/runs",
                    None,
                    None,
                    Some(start.clone()),
                ))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
        assert_eq!(invalid["code"], "crawler_request_invalid");
    }

    start["transform"]["fields"] = serde_json::json!([{
        "source": "url",
        "output": format!("_{}", "a".repeat(63))
    }]);
    let (valid_status, valid) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(start),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(valid_status, StatusCode::OK);
    assert_eq!(valid["status"], "running");

    let four_run_id = fresh_run_id();
    let mut four_start = fresh_start_fixture(&four_run_id);
    four_start["transform"]["fields"] = four;
    let (four_status, _) = response_json(
        app.oneshot(crawler_request(
            Method::POST,
            "/internal/crawler/runs",
            None,
            None,
            Some(four_start),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(four_status, StatusCode::OK);
}

#[tokio::test]
async fn crawler_http_cancel_before_start_terminal_ack_and_restart_replay_are_durable() {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let app = unauthenticated_crawler_routes(state);
    let run_id = fresh_run_id();
    let cancel_path = format!("/internal/crawler/runs/{run_id}/cancel");

    let (first_cancel_status, first_cancel) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                &cancel_path,
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(first_cancel_status, StatusCode::ACCEPTED);
    assert_eq!(first_cancel["disposition"], "cancel_requested");

    let (_, duplicate_cancel) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                &cancel_path,
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(duplicate_cancel["disposition"], "already_terminal");

    let get_path = format!("/internal/crawler/runs/{run_id}");
    let (get_status, canceled) = response_json(
        app.clone()
            .oneshot(crawler_request(Method::GET, &get_path, None, None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(canceled["status"], "canceled");

    let ack_path = format!("/internal/crawler/runs/{run_id}/ack");
    for _ in 0..2 {
        let (ack_status, ack) = response_json(
            app.clone()
                .oneshot(crawler_request(Method::POST, &ack_path, None, None, None))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(ack_status, StatusCode::OK);
        assert_eq!(ack["acknowledged"], true);
    }

    let start = fresh_start_fixture(&run_id);
    let (start_status, late_start) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(start.clone()),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(start_status, StatusCode::OK);
    assert_eq!(late_start, canceled);

    let (_, terminal_cancel) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                &cancel_path,
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal_cancel["disposition"], "already_terminal");

    drop(app);
    let restarted = unauthenticated_crawler_routes(TestStateBuilder::new(&temp).build_shared());
    let (get_status, retained) = response_json(
        restarted
            .clone()
            .oneshot(crawler_request(Method::GET, &get_path, None, None, None))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(retained, canceled);

    let (replay_status, replay) = response_json(
        restarted
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(start),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(replay_status, StatusCode::OK);
    assert_eq!(replay, canceled);
}

#[tokio::test]
async fn crawler_http_get_is_canonical_and_ack_refuses_nonterminal_truth() {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let run_id = fresh_run_id();
    let started_at_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    flapjack::index::manager::publication::CrawlerRunStore::new(&state.manager.base_path)
        .start_classified_with_deadline_admission(
            &run_id,
            flapjack::index::manager::publication::ContentDigest::new(format!(
                "sha256:{}",
                "a".repeat(64)
            ))
            .unwrap(),
            started_at_unix_ms,
            Some(
                started_at_unix_ms
                    .saturating_add(flapjack::crawler::MAX_CRAWLER_RUN_DURATION.as_millis() as u64),
            ),
            true,
        )
        .unwrap();
    let app = unauthenticated_crawler_routes(state);

    let (get_status, running) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::GET,
                &format!("/internal/crawler/runs/{run_id}"),
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK);
    assert_eq!(running["status"], "running");
    assert_eq!(
        running["counters"],
        serde_json::json!({
            "fetched": 0,
            "discovered": 0,
            "transformed": 0,
            "published": 0
        })
    );

    let (ack_status, ack_error) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                &format!("/internal/crawler/runs/{run_id}/ack"),
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ack_status, StatusCode::CONFLICT);
    assert_eq!(ack_error["code"], "crawler_run_not_terminal");

    let (cancel_status, _) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                &format!("/internal/crawler/runs/{run_id}/cancel"),
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(cancel_status, StatusCode::ACCEPTED);
    let (_, canceled) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::GET,
                &format!("/internal/crawler/runs/{run_id}"),
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(canceled["status"], "canceled");
    let (terminal_ack_status, _) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                &format!("/internal/crawler/runs/{run_id}/ack"),
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(terminal_ack_status, StatusCode::OK);

    let missing = fresh_run_id();
    let (missing_status, missing_error) = response_json(
        app.oneshot(crawler_request(
            Method::GET,
            &format!("/internal/crawler/runs/{missing}"),
            None,
            None,
            None,
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
    assert_eq!(missing_error["code"], "crawler_run_not_found");
}

#[tokio::test]
async fn crawler_http_get_terminalizes_expired_unowned_running_truth() {
    let temp = TempDir::new().unwrap();
    let state = TestStateBuilder::new(&temp).build_shared();
    let run_id = fresh_run_id();
    let start = fresh_start_fixture(&run_id);
    let typed: CrawlerRunStartRequest = serde_json::from_value(start.clone()).unwrap();
    let digest = flapjack::index::manager::publication::ContentDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&typed).unwrap()))
    ))
    .unwrap();
    let now_unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let started_at_unix_ms = now_unix_ms
        .saturating_sub(flapjack::crawler::MAX_CRAWLER_RUN_DURATION.as_millis() as u64)
        .saturating_sub(1);
    flapjack::index::manager::publication::CrawlerRunStore::new(&state.manager.base_path)
        .start(&run_id, digest, started_at_unix_ms)
        .unwrap();
    let app = unauthenticated_crawler_routes(Arc::clone(&state));

    let (status, terminal) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::GET,
                &format!("/internal/crawler/runs/{run_id}"),
                None,
                None,
                None,
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(terminal["status"], "failed");
    assert_eq!(terminal["error_code"], "worker_lost");

    for (method, path, body) in [
        (
            Method::GET,
            format!("/internal/crawler/runs/{run_id}"),
            None,
        ),
        (
            Method::POST,
            "/internal/crawler/runs".to_string(),
            Some(start.clone()),
        ),
        (
            Method::POST,
            format!("/internal/crawler/runs/{run_id}/ack"),
            None,
        ),
        (
            Method::GET,
            format!("/internal/crawler/runs/{run_id}"),
            None,
        ),
        (
            Method::POST,
            "/internal/crawler/runs".to_string(),
            Some(start),
        ),
    ] {
        let (replay_status, replay) = response_json(
            app.clone()
                .oneshot(crawler_request(method, &path, None, None, body))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            replay_status,
            StatusCode::OK,
            "terminal replay failed for {path}"
        );
        if !path.ends_with("/ack") {
            assert_eq!(replay["status"], "failed");
            assert_eq!(replay["error_code"], "worker_lost");
        }
    }

    let post_first_run_id = fresh_run_id();
    let post_first_start = fresh_start_fixture(&post_first_run_id);
    let post_first_typed: CrawlerRunStartRequest =
        serde_json::from_value(post_first_start.clone()).unwrap();
    let post_first_digest = flapjack::index::manager::publication::ContentDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            serde_json::to_vec(&post_first_typed).unwrap()
        ))
    ))
    .unwrap();
    let store =
        flapjack::index::manager::publication::CrawlerRunStore::new(&state.manager.base_path);
    store
        .start(&post_first_run_id, post_first_digest, started_at_unix_ms)
        .unwrap();
    let (post_first_status, post_first_terminal) = response_json(
        app.clone()
            .oneshot(crawler_request(
                Method::POST,
                "/internal/crawler/runs",
                None,
                None,
                Some(post_first_start),
            ))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(post_first_status, StatusCode::OK);
    assert_eq!(post_first_terminal["status"], "failed");
    assert_eq!(post_first_terminal["error_code"], "worker_lost");

    let canceled_run_id = fresh_run_id();
    let canceled_start = fresh_start_fixture(&canceled_run_id);
    let canceled_typed: CrawlerRunStartRequest =
        serde_json::from_value(canceled_start.clone()).unwrap();
    let canceled_digest = flapjack::index::manager::publication::ContentDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(serde_json::to_vec(&canceled_typed).unwrap()))
    ))
    .unwrap();
    store
        .start(&canceled_run_id, canceled_digest, now_unix_ms)
        .unwrap();
    store.request_cancel(&canceled_run_id, now_unix_ms).unwrap();
    let (canceled_status, canceled) = response_json(
        app.oneshot(crawler_request(
            Method::POST,
            "/internal/crawler/runs",
            None,
            None,
            Some(canceled_start),
        ))
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(canceled_status, StatusCode::OK);
    assert_eq!(canceled["status"], "canceled");
}

async fn assert_run_id_rejected_by_all_four_operation_extractors(invalid_run_id: &str) {
    let (_temp, app, _restricted_key) = crawler_contract_app();
    for (method, path, body) in operation_specimens(invalid_run_id) {
        let response = app
            .clone()
            .oneshot(crawler_request(
                method.clone(),
                &path,
                Some(APPLICATION_ID),
                Some(ADMIN_KEY),
                body,
            ))
            .await
            .unwrap();
        assert!(
            response.status().is_client_error(),
            "invalid run_id {invalid_run_id} reached the handler for {method} {path}: {}",
            response.status()
        );
    }
}

#[tokio::test]
async fn pbv4_crawler_non_v7_run_id_is_rejected_by_all_four_operation_extractors() {
    assert_run_id_rejected_by_all_four_operation_extractors(NON_V7_RUN_ID).await;
}

#[tokio::test]
async fn pbv4_crawler_non_rfc_v7_run_id_is_rejected_by_all_four_operation_extractors() {
    assert_run_id_rejected_by_all_four_operation_extractors(NON_RFC_V7_RUN_ID).await;
}

#[test]
fn committed_pbv4_crawler_openapi_matches_generator() {
    let committed = std::fs::read_to_string(default_pbv4_crawler_output_path())
        .expect("committed PBV4 crawler OpenAPI must exist");
    let expected = serde_json::to_string_pretty(&crate::openapi::pbv4_crawler_openapi()).unwrap();
    assert_eq!(committed, expected);
}

#[test]
fn pbv4_crawler_export_is_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path().join("crawler.json");
    crate::openapi_export::write_openapi_document_json(OpenApiDocument::Pbv4Crawler, &output)
        .unwrap();
    let first = std::fs::read(&output).unwrap();
    crate::openapi_export::write_openapi_document_json(OpenApiDocument::Pbv4Crawler, &output)
        .unwrap();
    assert_eq!(first, std::fs::read(output).unwrap());
}
