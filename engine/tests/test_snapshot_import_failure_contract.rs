use axum::{
    body::Body,
    http::{Method, StatusCode},
};
use flapjack_http::startup_catchup::snapshot_install_step_tags;
use serde_json::json;
use std::sync::Arc;

mod common;

async fn query_hits(
    app: &axum::Router,
    index_name: &str,
    query: &str,
) -> axum::http::Response<Body> {
    common::send_oneshot(
        app,
        Method::POST,
        &format!("/1/indexes/{index_name}/query"),
        &[("content-type", "application/json")],
        Body::from(json!({ "query": query }).to_string()),
    )
    .await
}

#[tokio::test]
async fn invalid_snapshot_import_returns_500_json_and_preserves_existing_index_data() {
    let (app, tmp) = common::build_test_app_for_local_requests(None);
    let index_name = "snapshot-invalid-import-contract";
    common::seed_doc_local(&app, index_name).await;

    let before = query_hits(&app, index_name, "alpha").await;
    assert_eq!(before.status(), StatusCode::OK);
    let before_body = common::parse_response_json(before).await;
    assert_eq!(before_body["nbHits"], json!(1), "seed precondition failed");
    assert_eq!(before_body["hits"][0]["objectID"], json!("doc-1"));

    let import_resp = common::send_oneshot(
        &app,
        Method::POST,
        &format!("/1/indexes/{index_name}/import"),
        &[("content-type", "application/gzip")],
        Body::from("not-a-valid-snapshot".as_bytes().to_vec()),
    )
    .await;
    assert_eq!(import_resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let import_ct = import_resp
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    assert!(
        import_ct.contains("application/json"),
        "expected middleware-wrapped JSON error content-type, got: {import_ct}"
    );

    let import_body = common::parse_response_json(import_resp).await;
    assert_eq!(import_body["status"], json!(500));
    let import_message = import_body["message"]
        .as_str()
        .expect("expected string message on import failure");
    assert_eq!(import_message, "Internal server error");
    assert!(
        !import_message.contains("Import failed:"),
        "500 response must not leak internal prefix text: {import_message}"
    );
    assert!(
        !import_message.contains("not-a-valid-snapshot"),
        "500 response must not leak raw backend text: {import_message}"
    );
    // Sub-step observability: invalid bytes must surface at import_extract so
    // operators can pin the failing branch without us leaking any underlying
    // error prose.
    assert_eq!(
        import_body["sub_step"],
        json!("import_extract"),
        "invalid bytes must pin sub_step=import_extract; got: {import_body}"
    );

    let after = query_hits(&app, index_name, "alpha").await;
    assert_eq!(
        after.status(),
        StatusCode::OK,
        "invalid import must not make existing index unreadable"
    );
    let after_body = common::parse_response_json(after).await;
    assert_eq!(
        after_body["nbHits"],
        json!(1),
        "invalid import must not remove existing documents"
    );
    assert_eq!(after_body["hits"][0]["objectID"], json!("doc-1"));

    // Validate durability, not just in-memory visibility: a fresh app over the same
    // on-disk data directory must still see the original document after failed import.
    let restarted_app = common::build_test_app_for_existing_data_dir(tmp.path(), None);
    let after_restart = query_hits(&restarted_app, index_name, "alpha").await;
    assert_eq!(
        after_restart.status(),
        StatusCode::OK,
        "invalid import must not make index unreadable after restart"
    );
    let after_restart_body = common::parse_response_json(after_restart).await;
    assert_eq!(
        after_restart_body["nbHits"],
        json!(1),
        "invalid import must preserve existing on-disk documents"
    );
    assert_eq!(after_restart_body["hits"][0]["objectID"], json!("doc-1"));
}

/// Hostile concurrent-import regression test for the HA snapshot-restore-under-load
/// flake diagnosed in Stage 1 (`docs/research/may31_pm_ha_snapshot_flake_diagnosis.md`).
///
/// Setup: seed a single-document index, export it to obtain valid snapshot bytes,
/// then fire N concurrent `/1/indexes/{name}/import` requests against the same
/// in-process router. Every request supplies a structurally-valid snapshot, so
/// the deterministic failure branches (`validate_tenant_id`, `clean_staging`,
/// `recover_interrupted`, `import_extract`) cannot trigger. Any non-2xx must
/// therefore come from a timing-dependent branch — overwhelmingly the rename
/// pair on Linux tmpfs.
///
/// Contract under test: either (a) all imports succeed (the rename retry loop
/// absorbed the transient `EBUSY`/`ENOTEMPTY`), or (b) any non-2xx response
/// carries a known `sub_step` tag so operators / nightly logs can pin the
/// failing branch. A missing `sub_step` on a 500 fails the test outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_imports_against_warm_server_pin_install_snapshot_step() {
    let (app, tmp) = common::build_test_app_for_local_requests(None);
    let app = Arc::new(app);
    let index_name = "snapshot-concurrent-import-contract";

    common::seed_doc_local(&app, index_name).await;

    // Produce valid tar.gz snapshot bytes by reusing the existing on-disk
    // export surface — same fixture pattern as the inline `import_snapshot`
    // success test in `handlers/snapshot.rs`.
    let snapshot_bytes = flapjack::index::snapshot::export_to_bytes(&tmp.path().join(index_name))
        .expect("export_to_bytes against the seeded tenant must succeed");
    let snapshot_bytes = Arc::new(snapshot_bytes);

    const CONCURRENT: usize = 8;
    let mut handles = Vec::with_capacity(CONCURRENT);
    for _ in 0..CONCURRENT {
        let app = Arc::clone(&app);
        let bytes = Arc::clone(&snapshot_bytes);
        let uri = format!("/1/indexes/{index_name}/import");
        handles.push(tokio::spawn(async move {
            common::send_oneshot(
                &app,
                Method::POST,
                &uri,
                &[("content-type", "application/gzip")],
                Body::from((*bytes).clone()),
            )
            .await
        }));
    }

    let mut statuses: Vec<StatusCode> = Vec::with_capacity(CONCURRENT);
    for handle in handles {
        let resp = handle.await.expect("import task must not panic");
        let status = resp.status();
        statuses.push(status);
        if !status.is_success() {
            let body = common::parse_response_json(resp).await;
            // The leak-prevention contract must continue to hold even under
            // load — message stays `"Internal server error"` for any 500.
            if status == StatusCode::INTERNAL_SERVER_ERROR {
                assert_eq!(
                    body["message"],
                    json!("Internal server error"),
                    "concurrent import 500 leaked underlying error: {body}"
                );
            }
            let sub_step = body["sub_step"].as_str().unwrap_or_else(|| {
                panic!(
                    "non-2xx response (status={status}) is missing sub_step: {body}; \
                     the install_snapshot_bytes owner must surface its failing branch",
                )
            });
            assert!(
                snapshot_install_step_tags().any(|known_sub_step| known_sub_step == sub_step),
                "non-2xx response carried unknown sub_step={sub_step}: {body}",
            );
        }
    }
}
