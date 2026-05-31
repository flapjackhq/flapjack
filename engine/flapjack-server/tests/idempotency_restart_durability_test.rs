#![allow(deprecated)]

mod support;

use flapjack_http::idempotency::{IdempotencyCache, IdempotencyRecord};
use serde_json::{json, Value};
use std::path::Path;
use std::time::{Duration, Instant};
use support::{http_request_with_headers, wait_for_task_published_at, RunningServer, TempDir};

#[test]
fn restart_replays_cached_response_and_preserves_single_execution_state() {
    let tmp = TempDir::new("fj_test_idempotency_restart_durability");
    let index_name = "idempotency_restart_idx";
    let object_id = "restart-proof-object";
    let idempotency_key = "same-key-across-restart";

    let mut server = RunningServer::spawn_no_auth_auto_port_with_persistent_idempotency(
        tmp.path(),
        Duration::from_secs(180),
    );

    let seed_task_id = server.add_documents_batch(
        index_name,
        json!({
            "requests": [
                {
                    "action": "addObject",
                    "body": {
                        "objectID": object_id,
                        "counter": 0,
                        "title": "seed"
                    }
                }
            ]
        }),
    );
    let seed_task =
        server.wait_for_task_published(index_name, seed_task_id, Duration::from_secs(20));
    assert_eq!(seed_task["status"], json!("published"));

    let increment_payload = json!({
        "requests": [
            {
                "action": "partialUpdateObject",
                "body": {
                    "objectID": object_id,
                    "counter": {
                        "_operation": "Increment",
                        "value": 1
                    }
                }
            }
        ]
    });

    let write_path = format!("/1/indexes/{index_name}/batch");
    let first = http_request_with_headers(
        server.bind_addr(),
        "POST",
        &write_path,
        &[("x-flapjack-idempotency-key", idempotency_key)],
        Some(&increment_payload.to_string()),
    )
    .expect("initial idempotent request should succeed");
    assert!(
        first.status == 200 || first.status == 202,
        "first write should be accepted, got {} body {}",
        first.status,
        first.body
    );

    let first_body: Value =
        serde_json::from_str(&first.body).expect("first response should be JSON");
    let first_task_id = first_body["taskID"]
        .as_i64()
        .or_else(|| first_body["taskID"].as_u64().map(|value| value as i64))
        .expect("first response should include taskID");
    wait_for_task_published_at(
        server.bind_addr(),
        index_name,
        first_task_id,
        Duration::from_secs(20),
    );

    server.kill_and_restart_no_auth_auto_port_with_persistent_idempotency(
        tmp.path(),
        Duration::from_secs(180),
    );

    let replay = http_request_with_headers(
        server.bind_addr(),
        "POST",
        &write_path,
        &[("x-flapjack-idempotency-key", idempotency_key)],
        Some(&increment_payload.to_string()),
    )
    .expect("replay request should succeed");

    assert_eq!(replay.status, first.status);
    assert_eq!(
        replay.body, first.body,
        "replay must return cached body exactly"
    );

    let replay_body: Value =
        serde_json::from_str(&replay.body).expect("replay response should be JSON");
    let replay_task_id = replay_body["taskID"]
        .as_i64()
        .or_else(|| replay_body["taskID"].as_u64().map(|value| value as i64))
        .expect("replay response should include taskID");
    assert_eq!(
        replay_task_id, first_task_id,
        "replay must return original taskID"
    );

    let object_path = format!("/1/indexes/{index_name}/{object_id}");
    let fetched = http_request_with_headers(server.bind_addr(), "GET", &object_path, &[], None)
        .expect("fetch object after replay should succeed");
    assert_eq!(
        fetched.status, 200,
        "object fetch should succeed: {}",
        fetched.body
    );

    let fetched_body: Value =
        serde_json::from_str(&fetched.body).expect("fetched object body should be JSON");
    assert_eq!(
        fetched_body["counter"],
        json!(1),
        "counter must reflect single execution only"
    );
}

#[test]
fn persistent_idempotency_sqlite_probe_prints_baseline() {
    let tmp = TempDir::new("fj_test_idempotency_sqlite_probe");
    let data_dir = tmp.root();
    let ttl = Duration::from_secs(300);

    let cache = IdempotencyCache::persistent_under_data_dir(ttl, data_dir)
        .expect("persistent cache should initialize");
    let cache_path = IdempotencyCache::canonical_db_path(data_dir);
    assert!(
        cache_path.exists(),
        "expected persistent sqlite path to exist at {}",
        cache_path.display()
    );

    cache
        .store_scoped(
            "probe-app",
            "probe-index",
            "known-key",
            IdempotencyRecord::json(
                axum::http::StatusCode::OK,
                axum::body::Bytes::from_static(b"{\"ok\":true}"),
            ),
        )
        .expect("known record should store");
    let known = cache
        .lookup_scoped("probe-app", "probe-index", "known-key")
        .expect("known lookup should work");
    assert!(
        known.is_some(),
        "known record must round-trip before probe timing"
    );

    let iterations: usize = 300;
    let mut store_micros = Vec::with_capacity(iterations);
    let mut lookup_micros = Vec::with_capacity(iterations);

    for i in 0..iterations {
        let key = format!("probe-key-{i:04}");
        let store_started = Instant::now();
        cache
            .store_scoped(
                "probe-app",
                "probe-index",
                &key,
                IdempotencyRecord::json(
                    axum::http::StatusCode::OK,
                    axum::body::Bytes::from_static(b"{\"ok\":true}"),
                ),
            )
            .expect("probe store should succeed");
        store_micros.push(store_started.elapsed().as_secs_f64() * 1_000_000.0);

        let lookup_started = Instant::now();
        let hit = cache
            .lookup_scoped("probe-app", "probe-index", &key)
            .expect("probe lookup should succeed");
        lookup_micros.push(lookup_started.elapsed().as_secs_f64() * 1_000_000.0);
        assert!(hit.is_some(), "probe lookup should hit the stored key");
    }

    println!(
        "idempotency_sqlite_probe path={} iterations={} store_avg_us={:.2} store_p95_us={:.2} store_p99_us={:.2} lookup_avg_us={:.2} lookup_p95_us={:.2} lookup_p99_us={:.2}",
        display_path(&cache_path),
        iterations,
        mean(&store_micros),
        percentile(&store_micros, 95),
        percentile(&store_micros, 99),
        mean(&lookup_micros),
        percentile(&lookup_micros, 95),
        percentile(&lookup_micros, 99)
    );
}

fn display_path(path: &Path) -> String {
    path.display().to_string()
}

fn mean(values: &[f64]) -> f64 {
    values.iter().sum::<f64>() / values.len() as f64
}

fn percentile(values: &[f64], p: usize) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).expect("no NaN expected"));
    let idx = ((sorted.len() - 1) * p) / 100;
    sorted[idx]
}
