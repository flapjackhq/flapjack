use super::*;

#[cfg(unix)]
use std::os::unix::fs::symlink;

fn test_notifier() -> Arc<crate::notifications::NotificationService> {
    Arc::new(crate::notifications::NotificationService::disabled())
}

async fn ingest_view_events(app: &Router, user_tokens: &[&str]) {
    let events: Vec<_> = user_tokens
        .iter()
        .enumerate()
        .map(|(position, user_token)| {
            json!({
                "eventType": "view",
                "eventName": "Viewed",
                "index": "products",
                "userToken": user_token,
                "objectIDs": [format!("object-{position}")]
            })
        })
        .collect();
    let response =
        send_json_request(app, Method::POST, "/1/events", json!({ "events": events })).await;
    assert_eq!(response.status(), StatusCode::OK);
}

fn seed_outside_analytics(config: &AnalyticsConfig) {
    let events =
        ["delete-me", "safe-user"].map(|user_token| flapjack::analytics::schema::InsightEvent {
            event_type: "view".to_string(),
            event_subtype: None,
            event_name: "Viewed".to_string(),
            index: "products".to_string(),
            user_token: user_token.to_string(),
            authenticated_user_token: None,
            query_id: None,
            object_ids: vec![format!("object-{user_token}")],
            object_ids_alt: vec![],
            positions: None,
            timestamp: Some(chrono::Utc::now().timestamp_millis()),
            value: None,
            currency: None,
            interleaving_team: None,
        });
    flapjack::analytics::writer::flush_insight_events(&events, &config.events_dir("products"))
        .unwrap();
}

fn snapshot_regular_files(root: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
    fn visit(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut Vec<(std::path::PathBuf, Vec<u8>)>,
    ) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let file_type = entry.file_type().unwrap();
            assert!(
                !file_type.is_symlink(),
                "fixture must be a real directory tree"
            );
            if file_type.is_dir() {
                visit(root, &entry.path(), files);
            } else if file_type.is_file() {
                files.push((
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    std::fs::read(entry.path()).unwrap(),
                ));
            }
        }
    }

    let mut files = Vec::new();
    visit(root, root, &mut files);
    files.sort_by(|left, right| left.0.cmp(&right.0));
    assert!(
        !files.is_empty(),
        "outside fixture must contain persisted data"
    );
    files
}

async fn assert_debug_event_exists(app: &Router, user_token: &str) {
    let response = send_empty_request(app, Method::GET, "/1/events/debug?limit=100").await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = body_json(response).await;
    assert!(
        body["events"]
            .as_array()
            .is_some_and(|events| events.iter().any(|event| event["userToken"] == user_token)),
        "expected buffered debug event for {user_token}: {body}"
    );
}

fn assert_sanitized_internal_error(body: &serde_json::Value) {
    assert_eq!(
        body,
        &json!({
            "message": "Internal server error",
            "status": 500
        })
    );
    assert!(body.get("deletedAt").is_none());
}

/// Verify that the GDPR delete endpoint invokes `send_gdpr_confirmation` on the global notification service.
#[tokio::test]
async fn delete_usertoken_sends_gdpr_notification() {
    let notifier = test_notifier();
    let before = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router_with_base_and_notifier(
        collector,
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/user_test_gdpr").await;
    assert_eq!(response.status(), StatusCode::OK);

    let after = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after > before,
        "send_gdpr_confirmation should have been called: before={before}, after={after}"
    );
}

/// A configured analytics root may not redirect GDPR deletion outside the
/// configured tree. Refusal must happen before in-memory or external data is
/// changed, and the endpoint must not claim or notify successful deletion.
#[cfg(unix)]
#[tokio::test]
async fn gdpr_delete_rejects_symlinked_analytics_root_before_any_mutation() {
    let tmp = TempDir::new().unwrap();
    let outside_config = AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("outside-analytics"),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    };
    seed_outside_analytics(&outside_config);
    let outside_before = snapshot_regular_files(&outside_config.data_dir);

    let linked_root = tmp.path().join("analytics-link");
    symlink(&outside_config.data_dir, &linked_root).unwrap();
    let linked_config = AnalyticsConfig {
        data_dir: linked_root,
        ..outside_config.clone()
    };
    let linked_collector = AnalyticsCollector::new(linked_config);
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        Arc::clone(&linked_collector),
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["delete-me"]).await;

    let notifications_before = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    let status = response.status();
    let body = body_json(response).await;
    let notifications_after = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let outside_after = snapshot_regular_files(&outside_config.data_dir);
    assert_eq!(
        outside_after, outside_before,
        "symlink target outside the configured tree was mutated"
    );
    assert_debug_event_exists(&app, "delete-me").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(notifications_after, notifications_before);
}

/// Per-index events roots receive the same fail-closed treatment as the
/// configured analytics root. A regular parent directory must not make a
/// symlinked events leaf safe to traverse.
#[cfg(unix)]
#[tokio::test]
async fn gdpr_delete_rejects_symlinked_index_events_root_before_any_mutation() {
    let tmp = TempDir::new().unwrap();
    let outside_config = AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("outside-analytics"),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    };
    seed_outside_analytics(&outside_config);
    let outside_before = snapshot_regular_files(&outside_config.data_dir);

    let config = AnalyticsConfig {
        enabled: true,
        data_dir: tmp.path().join("analytics"),
        flush_interval_secs: 3600,
        flush_size: 10_000,
        retention_days: 90,
    };
    let events_dir = config.events_dir("products");
    std::fs::create_dir_all(events_dir.parent().unwrap()).unwrap();
    symlink(outside_config.events_dir("products"), &events_dir).unwrap();

    let collector = AnalyticsCollector::new(config);
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        Arc::clone(&collector),
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["delete-me"]).await;

    let notifications_before = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);
    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    let status = response.status();
    let body = body_json(response).await;
    let notifications_after = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let outside_after = snapshot_regular_files(&outside_config.data_dir);
    assert_eq!(
        outside_after, outside_before,
        "symlink target outside the configured tree was mutated"
    );
    assert_debug_event_exists(&app, "delete-me").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(notifications_after, notifications_before);
}

/// A profile deletion failure is a server-side failure, not a successful GDPR
/// deletion. In particular, no success timestamp or confirmation may escape.
#[cfg(unix)]
#[tokio::test]
async fn gdpr_delete_profile_obstruction_returns_sanitized_error_without_notification() {
    let tmp = TempDir::new().unwrap();
    let blocked_profile = tmp
        .path()
        .join("personalization/profiles/blocked-user.json");
    std::fs::create_dir_all(&blocked_profile).unwrap();

    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        collector,
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["blocked-user"]).await;
    let notifications_before = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/blocked-user").await;
    let status = response.status();
    let body = body_json(response).await;
    let notifications_after = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    assert!(
        blocked_profile.is_dir(),
        "obstructing directory was mutated"
    );
    assert_debug_event_exists(&app, "blocked-user").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(notifications_after, notifications_before);
}

#[cfg(unix)]
#[tokio::test]
async fn gdpr_delete_rejects_symlinked_profiles_parent_before_analytics_mutation() {
    let tmp = TempDir::new().unwrap();
    let outside_profiles = tmp.path().join("outside-profiles");
    std::fs::create_dir_all(&outside_profiles).unwrap();
    let outside_profile = outside_profiles.join("delete-me.json");
    let outside_bytes = br#"{"user_token":"delete-me","scores":{}}"#;
    std::fs::write(&outside_profile, outside_bytes).unwrap();

    let personalization_dir = tmp.path().join("personalization");
    std::fs::create_dir_all(&personalization_dir).unwrap();
    symlink(&outside_profiles, personalization_dir.join("profiles")).unwrap();

    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        collector,
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["delete-me"]).await;
    let notifications_before = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(std::fs::read(outside_profile).unwrap(), outside_bytes);
    assert_debug_event_exists(&app, "delete-me").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(
        notifier
            .gdpr_call_count
            .load(std::sync::atomic::Ordering::Relaxed),
        notifications_before
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gdpr_delete_rejects_symlinked_profile_file_before_analytics_mutation() {
    let tmp = TempDir::new().unwrap();
    let outside_profile = tmp.path().join("outside-profile.json");
    let outside_bytes = br#"{"user_token":"delete-me","scores":{}}"#;
    std::fs::write(&outside_profile, outside_bytes).unwrap();

    let profiles_dir = tmp.path().join("personalization/profiles");
    std::fs::create_dir_all(&profiles_dir).unwrap();
    symlink(&outside_profile, profiles_dir.join("delete-me.json")).unwrap();

    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        collector,
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["delete-me"]).await;
    let notifications_before = notifier
        .gdpr_call_count
        .load(std::sync::atomic::Ordering::Relaxed);

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(std::fs::read(outside_profile).unwrap(), outside_bytes);
    assert_debug_event_exists(&app, "delete-me").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(
        notifier
            .gdpr_call_count
            .load(std::sync::atomic::Ordering::Relaxed),
        notifications_before
    );
}

#[cfg(unix)]
#[tokio::test]
async fn gdpr_delete_nested_symlink_preflight_preserves_all_selected_data() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    seed_outside_analytics(&config);
    let products_before = snapshot_regular_files(&config.events_dir("products"));

    let broken_partition = config.events_dir("broken").join("date=2026-08-16");
    let outside_dir = tmp.path().join("outside-events");
    std::fs::create_dir_all(&broken_partition).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();
    symlink(&outside_dir, broken_partition.join("escape")).unwrap();

    let collector = AnalyticsCollector::new(config.clone());
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        collector,
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["delete-me"]).await;

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(
        snapshot_regular_files(&config.events_dir("products")),
        products_before,
        "a selected real events tree was mutated before nested-symlink refusal"
    );
    assert_debug_event_exists(&app, "delete-me").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(
        notifier
            .gdpr_call_count
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

#[tokio::test]
async fn gdpr_delete_non_directory_events_root_preserves_all_selected_data() {
    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    seed_outside_analytics(&config);
    let products_before = snapshot_regular_files(&config.events_dir("products"));

    let broken_events = config.events_dir("broken");
    std::fs::create_dir_all(broken_events.parent().unwrap()).unwrap();
    std::fs::write(&broken_events, b"not a directory").unwrap();

    let collector = AnalyticsCollector::new(config.clone());
    let notifier = test_notifier();
    let app = app_router_with_base_and_notifier(
        collector,
        tmp.path().to_path_buf(),
        Some(Arc::clone(&notifier)),
    );
    ingest_view_events(&app, &["delete-me"]).await;

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/delete-me").await;
    let status = response.status();
    let body = body_json(response).await;

    assert_eq!(std::fs::read(&broken_events).unwrap(), b"not a directory");
    assert_eq!(
        snapshot_regular_files(&config.events_dir("products")),
        products_before,
        "a selected real events tree was mutated before invalid-leaf refusal"
    );
    assert_debug_event_exists(&app, "delete-me").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_sanitized_internal_error(&body);
    assert_eq!(
        notifier
            .gdpr_call_count
            .load(std::sync::atomic::Ordering::Relaxed),
        0
    );
}

/// Verify that the `limit` parameter caps the number of returned debug events.
#[tokio::test]
async fn debug_endpoint_respects_limit() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let events: Vec<_> = (0..5)
        .map(|i| {
            json!({
                "eventType": "view",
                "eventName": format!("V{i}"),
                "index": "products",
                "userToken": "user_a",
                "objectIDs": [format!("o{i}")]
            })
        })
        .collect();
    send_json_request(&app, Method::POST, "/1/events", json!({ "events": events })).await;

    let resp = send_empty_request(&app, Method::GET, "/1/events/debug?limit=2").await;
    let body = body_json(resp).await;
    assert_eq!(body["count"], 2);
}

// ── Stage D: GDPR Multi-Store Deletion Tests ──

/// Verify that GDPR delete removes the target user's personalization profile from disk.
#[tokio::test]
async fn gdpr_delete_usertoken_removes_personalization_profile_cache() {
    use flapjack::personalization::{PersonalizationProfile, PersonalizationProfileStore};
    use std::collections::BTreeMap;

    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));

    let store = PersonalizationProfileStore::new(tmp.path());
    let mut brand_scores = BTreeMap::new();
    brand_scores.insert("Nike".to_string(), 15);
    let mut scores = BTreeMap::new();
    scores.insert("brand".to_string(), brand_scores);
    store
        .save_profile(&PersonalizationProfile {
            user_token: "gdpr-target".to_string(),
            last_event_at: Some(chrono::Utc::now().to_rfc3339()),
            scores,
        })
        .unwrap();
    assert!(
        store.load_profile("gdpr-target").unwrap().is_some(),
        "profile should exist before GDPR delete"
    );

    let app = app_router_with_base(collector, tmp.path().to_path_buf());

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/gdpr-target").await;
    assert_eq!(response.status(), StatusCode::OK);

    assert!(
        store.load_profile("gdpr-target").unwrap().is_none(),
        "personalization profile should be deleted after GDPR usertoken delete"
    );
}

/// Verify that GDPR delete for one user leaves another user's analytics events and personalization profile intact.
#[tokio::test]
async fn gdpr_delete_usertoken_keeps_other_users_data() {
    use flapjack::personalization::{PersonalizationProfile, PersonalizationProfileStore};
    use std::collections::BTreeMap;

    let tmp = TempDir::new().unwrap();
    let config = test_analytics_config(&tmp);
    let collector = AnalyticsCollector::new(config.clone());
    let app = app_router_with_base(Arc::clone(&collector), tmp.path().to_path_buf());

    let ingest_body = json!({
        "events": [
            {
                "eventType": "view",
                "eventName": "Viewed",
                "index": "products",
                "userToken": "target-user",
                "objectIDs": ["obj1"]
            },
            {
                "eventType": "view",
                "eventName": "Viewed",
                "index": "products",
                "userToken": "safe-user",
                "objectIDs": ["obj2"]
            }
        ]
    });
    let resp = send_json_request(&app, Method::POST, "/1/events", ingest_body).await;
    assert_eq!(resp.status(), StatusCode::OK);
    collector.flush_all();

    let store = PersonalizationProfileStore::new(tmp.path());
    for token in &["target-user", "safe-user"] {
        let mut scores = BTreeMap::new();
        let mut brand = BTreeMap::new();
        brand.insert("Nike".to_string(), 10);
        scores.insert("brand".to_string(), brand);
        store
            .save_profile(&PersonalizationProfile {
                user_token: token.to_string(),
                last_event_at: Some(chrono::Utc::now().to_rfc3339()),
                scores,
            })
            .unwrap();
    }

    let response = send_empty_request(&app, Method::DELETE, "/1/usertokens/target-user").await;
    assert_eq!(response.status(), StatusCode::OK);

    let engine = AnalyticsQueryEngine::new(config);
    let rows = engine
        .query_events(
            "products",
            "SELECT user_token, COUNT(*) as count FROM events GROUP BY user_token ORDER BY user_token",
        )
        .await
        .unwrap();
    assert!(
        !rows
            .iter()
            .any(|row| row.get("user_token") == Some(&json!("target-user"))),
        "target-user events should be purged: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.get("user_token") == Some(&json!("safe-user"))),
        "safe-user events should remain: {rows:?}"
    );

    assert!(
        store.load_profile("safe-user").unwrap().is_some(),
        "safe-user profile should remain after target-user GDPR delete"
    );
    assert!(
        store.load_profile("target-user").unwrap().is_none(),
        "target-user profile should be removed"
    );
}

/// Verify that deleting the same user token twice succeeds both times and returns `deletedAt` on each call.
#[tokio::test]
async fn gdpr_delete_usertoken_is_idempotent() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router_with_base(collector, tmp.path().to_path_buf());

    let r1 = send_empty_request(&app, Method::DELETE, "/1/usertokens/nonexistent-user").await;
    assert_eq!(r1.status(), StatusCode::OK);
    let body1 = body_json(r1).await;
    assert_eq!(body1["status"], json!(200));

    let r2 = send_empty_request(&app, Method::DELETE, "/1/usertokens/nonexistent-user").await;
    assert_eq!(r2.status(), StatusCode::OK);
    let body2 = body_json(r2).await;
    assert_eq!(body2["status"], json!(200));
    assert!(
        body2["deletedAt"].as_str().is_some(),
        "second delete should still return deletedAt"
    );
}

/// Stage 3 §4: When all events in a batch are invalid, the error response must use the
/// standard `{ "message": "...", "status": N }` shape with no extra fields leaked.
#[tokio::test]
async fn all_invalid_events_rejected_with_standard_error_body_shape() {
    let tmp = TempDir::new().unwrap();
    let collector = AnalyticsCollector::new(test_analytics_config(&tmp));
    let app = app_router(collector);

    let body = json!({
        "events": [{
            "eventType": "click",
            "eventName": "Bad Click",
            "index": "products",
            "userToken": "user@invalid",
            "objectIDs": ["obj1"],
            "positions": [1]
        }]
    });

    let response = send_json_request(&app, Method::POST, "/1/events", body).await;
    assert_rejected_event_status(response.status());

    let error_body = body_json(response).await;
    assert!(
        error_body["message"]
            .as_str()
            .is_some_and(|m| !m.is_empty()),
        "error body must contain a non-empty message field"
    );
    let status_code = error_body["status"]
        .as_u64()
        .expect("error body must contain a numeric status field");
    assert!(
        status_code == 400 || status_code == 422,
        "status field should match HTTP status: {status_code}"
    );
    let keys: Vec<&str> = error_body
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.as_str())
        .collect();
    assert!(
        keys.iter().all(|k| *k == "message" || *k == "status"),
        "error body must not contain extra fields: {keys:?}"
    );
}
