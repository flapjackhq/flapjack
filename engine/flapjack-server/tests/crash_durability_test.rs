#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

//! Real-server crash/restart durability test for acknowledged batch writes.
mod support;

use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};
use support::{http_request_with_read_timeout, HttpResponse, RunningServer, TempDir};

const TEST_WRITE_QUEUE_CHANNEL_CAPACITY: usize = 2;
const SERVED_WRITER_CONTENTION_RETRY_WINDOW: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
struct AdmissionRecordSample {
    task_id: i64,
    object_ids: Vec<String>,
}

struct PendingRawRequest {
    object_id: String,
    handle: thread::JoinHandle<Result<HttpResponse, String>>,
}

fn batch_payload(object_id: &str, token: &str) -> String {
    json!({
        "requests": [
            {
                "action": "addObject",
                "body": {
                    "objectID": object_id,
                    "title": format!("served admission {object_id}"),
                    "token": token
                }
            }
        ]
    })
    .to_string()
}

fn single_doc_payload(object_id: &str, token: &str) -> String {
    json!({
        "objectID": object_id,
        "title": format!("writer contention {object_id}"),
        "token": token
    })
    .to_string()
}

fn spawn_raw_batch_request(
    bind_addr: &str,
    index_name: &str,
    object_id: String,
    token: &str,
    read_timeout: Duration,
) -> PendingRawRequest {
    let bind_addr = bind_addr.to_string();
    let index_name = index_name.to_string();
    let token = token.to_string();
    let object_id_for_thread = object_id.clone();
    let handle = thread::spawn(move || {
        let path = format!("/1/indexes/{index_name}/batch");
        let body = batch_payload(&object_id_for_thread, &token);
        http_request_with_read_timeout(&bind_addr, "POST", &path, &[], Some(&body), read_timeout)
    });
    PendingRawRequest { object_id, handle }
}

fn read_admission_records(data_root: &Path, index_name: &str) -> Vec<AdmissionRecordSample> {
    let admission_dir = data_root.join(index_name).join("write_admission");
    if !admission_dir.exists() {
        return Vec::new();
    }
    let mut paths = fs::read_dir(&admission_dir)
        .unwrap_or_else(|error| {
            panic!(
                "admission dir should be readable at {}: {error}",
                admission_dir.display()
            )
        })
        .map(|entry| {
            entry
                .expect("admission dir entry should be readable")
                .path()
        })
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect::<Vec<_>>();
    paths.sort();

    paths
        .into_iter()
        .map(|path| {
            let value: Value = serde_json::from_slice(&fs::read(&path).unwrap_or_else(|error| {
                panic!(
                    "admission record {} should be readable: {error}",
                    path.display()
                )
            }))
            .unwrap_or_else(|error| {
                panic!(
                    "admission record {} should be valid json: {error}",
                    path.display()
                )
            });
            let record = value
                .get("record")
                .unwrap_or_else(|| panic!("admission envelope must contain record: {value}"));
            let task_id = record["numeric_id"]
                .as_i64()
                .unwrap_or_else(|| panic!("admission record must contain numeric_id: {record}"));
            let object_ids = record["actions"]
                .as_array()
                .unwrap_or_else(|| panic!("admission record must contain actions: {record}"))
                .iter()
                .filter_map(object_id_from_admission_action)
                .collect::<Vec<_>>();
            AdmissionRecordSample {
                task_id,
                object_ids,
            }
        })
        .collect()
}

fn object_id_from_admission_action(action: &Value) -> Option<String> {
    let action_payload = action.as_object()?.values().next()?;
    if let Some(id) = action_payload.get("id").and_then(Value::as_str) {
        return Some(id.to_string());
    }
    action_payload.as_str().map(str::to_string)
}

fn wait_for_admission_record_count(
    data_root: &Path,
    index_name: &str,
    minimum_count: usize,
    timeout: Duration,
) -> Vec<AdmissionRecordSample> {
    let started_at = Instant::now();
    loop {
        let records = read_admission_records(data_root, index_name);
        if records.len() >= minimum_count {
            return records;
        }
        assert!(
            started_at.elapsed() <= timeout,
            "timed out waiting for {minimum_count} admission records; last count={}",
            records.len()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn parse_json_response(response: &HttpResponse, context: &str) -> Value {
    serde_json::from_str(&response.body).unwrap_or_else(|error| {
        panic!(
            "{context} response should be valid json: {} ({error})",
            response.body
        )
    })
}

fn assert_retry_after_one(response: &HttpResponse, context: &str) {
    assert_eq!(
        response.headers.get("retry-after").map(String::as_str),
        Some("1"),
        "{context} must include Retry-After: 1; headers={:?}",
        response.headers
    );
}

fn assert_search_lacks_object(server: &RunningServer, index_name: &str, object_id: &str) {
    let search = server.search(index_name, json!({ "query": object_id }));
    assert_eq!(
        search["nbHits"],
        json!(0),
        "rejected sentinel {object_id} must not be searchable: {search}"
    );
}

fn create_index_via_http(server: &RunningServer, index_name: &str) {
    let create_body = json!({ "uid": index_name }).to_string();
    let create_response = http_request_with_read_timeout(
        server.bind_addr(),
        "POST",
        "/1/indexes",
        &[],
        Some(&create_body),
        Duration::from_secs(2),
    )
    .expect("create-index precondition must receive a served HTTP response");
    assert_eq!(
        create_response.status, 200,
        "create-index precondition must succeed before probe: {}",
        create_response.body
    );
}

#[test]
#[serial_test::serial(flapjack_server_write_env)]
fn acknowledged_batch_write_remains_searchable_after_crash_restart() {
    let tmp = TempDir::new("fj_test_crash_durability");
    let index_name = "crash_durability_idx";
    let object_id = "durability-doc-1";
    let query_token = "durability-proof-token";

    let mut server = RunningServer::spawn_no_auth_auto_port(tmp.path());

    let task_id = server.add_documents_batch(
        index_name,
        json!({
            "requests": [
                {
                    "action": "addObject",
                    "body": {
                        "objectID": object_id,
                        "title": "Crash durability proof",
                        "token": query_token
                    }
                }
            ]
        }),
    );

    let task = server.wait_for_task_published(index_name, task_id, Duration::from_secs(10));
    assert_eq!(task["status"], json!("published"));
    assert_eq!(task["pendingTask"], json!(false));

    let pre_crash_search = server.search(index_name, json!({ "query": query_token }));
    let pre_crash_hits = pre_crash_search["hits"]
        .as_array()
        .expect("search response must contain hits array before crash");
    assert!(
        pre_crash_hits
            .iter()
            .any(|hit| hit["objectID"] == json!(object_id)),
        "pre-crash search must contain acknowledged document: {}",
        pre_crash_search
    );

    server.kill_and_restart_no_auth_auto_port(tmp.path());

    let post_restart_search = server.search(index_name, json!({ "query": query_token }));
    let post_restart_hits = post_restart_search["hits"]
        .as_array()
        .expect("search response must contain hits array after restart");
    assert!(
        post_restart_hits
            .iter()
            .any(|hit| hit["objectID"] == json!(object_id)),
        "post-restart search must contain acknowledged document: {}",
        post_restart_search
    );
}

#[test]
#[serial_test::serial(flapjack_server_write_env)]
fn nontrivial_acknowledged_dataset_survives_crash_restart() {
    let tmp = TempDir::new("fj_test_crash_durability_nontrivial");
    let index_name = "crash_durability_nontrivial_idx";
    let total_docs = 180usize;
    let batch_size = 30usize;

    let mut server = RunningServer::spawn_no_auth_auto_port(tmp.path());

    // Use repeated shared tokens plus deterministic per-doc values so the proof
    // checks both corpus-wide recovery and a specific targeted lookup.
    for batch_start in (0..total_docs).step_by(batch_size) {
        let batch_end = (batch_start + batch_size).min(total_docs);
        let requests = (batch_start..batch_end)
            .map(|doc_index| {
                let tier = if doc_index % 2 == 0 { "alpha" } else { "beta" };
                let family = doc_index % 3;
                json!({
                    "action": "addObject",
                    "body": {
                        "objectID": format!("durability-doc-{doc_index:03}"),
                        "title": format!("Crash durability batch document {doc_index:03}"),
                        "token": "nontrivial-durability-proof",
                        "tier": tier,
                        "family": format!("family-{family}"),
                        "marker": format!("marker{doc_index:03}"),
                    }
                })
            })
            .collect::<Vec<_>>();

        let task_id = server.add_documents_batch(index_name, json!({ "requests": requests }));
        let task = server.wait_for_task_published(index_name, task_id, Duration::from_secs(20));
        assert_eq!(task["status"], json!("published"));
        assert_eq!(task["pendingTask"], json!(false));
    }

    let pre_crash_all = server.search(index_name, json!({ "query": "" }));
    assert_eq!(
        pre_crash_all["nbHits"],
        json!(total_docs),
        "expected all seeded docs before crash: {pre_crash_all}"
    );

    let pre_crash_targeted = server.search(index_name, json!({ "query": "alpha" }));
    assert_eq!(
        pre_crash_targeted["nbHits"],
        json!(total_docs / 2),
        "expected deterministic tier subset before crash: {pre_crash_targeted}"
    );

    server.kill_and_restart_no_auth_auto_port(tmp.path());

    let post_restart_all = server.search(index_name, json!({ "query": "" }));
    assert_eq!(
        post_restart_all["nbHits"],
        json!(total_docs),
        "expected all seeded docs after restart: {post_restart_all}"
    );

    let post_restart_targeted = server.search(index_name, json!({ "query": "alpha" }));
    assert_eq!(
        post_restart_targeted["nbHits"],
        json!(total_docs / 2),
        "expected deterministic tier subset after restart: {post_restart_targeted}"
    );

    let post_restart_specific = server.search(index_name, json!({ "query": "marker121" }));
    let specific_hits = post_restart_specific["hits"]
        .as_array()
        .expect("targeted post-restart search must contain hits");
    assert!(
        specific_hits
            .iter()
            .any(|hit| hit["objectID"] == json!("durability-doc-121")),
        "post-restart search must still contain durability-doc-121 via marker121: {post_restart_specific}"
    );
}

#[test]
#[serial_test::serial(flapjack_server_write_env)]
fn admitted_in_flight_batch_replays_after_served_crash_restart() {
    let tmp = TempDir::new("fj_test_served_admission_replay");
    let index_name = "served_admission_replay_idx";
    let replay_token = "served-admission-replay-token";
    let mut server = RunningServer::spawn_no_auth_auto_port(tmp.path());
    create_index_via_http(&server, index_name);
    server.kill_and_restart_no_auth_auto_port_with_env(
        tmp.path(),
        &[
            ("FLAPJACK_MAX_CONCURRENT_WRITERS", "0"),
            ("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS", "10000"),
            ("FLAPJACK_WRITE_QUEUE_BATCH_SIZE", "1"),
        ],
    );

    let mut requests = (0..4)
        .map(|i| {
            spawn_raw_batch_request(
                server.bind_addr(),
                index_name,
                format!("served-replay-doc-{i}"),
                replay_token,
                Duration::from_secs(20),
            )
        })
        .collect::<Vec<_>>();
    let requested_object_ids = requests
        .iter()
        .map(|request| request.object_id.clone())
        .collect::<HashSet<_>>();

    let records =
        wait_for_admission_record_count(tmp.root(), index_name, 1, Duration::from_secs(3));
    let sampled_records = records
        .into_iter()
        .filter(|record| {
            record
                .object_ids
                .iter()
                .all(|object_id| requested_object_ids.contains(object_id))
        })
        .collect::<Vec<_>>();
    assert!(
        !sampled_records.is_empty(),
        "pre-kill admission-log sample must include at least one replayable in-flight request"
    );
    for record in &sampled_records {
        for object_id in &record.object_ids {
            let request = requests
                .iter()
                .find(|request| &request.object_id == object_id)
                .unwrap_or_else(|| panic!("sampled object {object_id} must belong to the probe"));
            assert!(
                !request.handle.is_finished(),
                "sampled admitted request for {object_id} must still be in flight before kill"
            );
        }
    }

    server.kill_and_restart_no_auth_auto_port(tmp.path());

    for request in requests.drain(..) {
        let _ = request.handle.join();
    }

    let expected_object_ids = sampled_records
        .iter()
        .flat_map(|record| record.object_ids.iter().cloned())
        .collect::<HashSet<_>>();
    let replayed_search = server.search(index_name, json!({ "query": replay_token }));
    assert_eq!(
        replayed_search["nbHits"],
        json!(expected_object_ids.len()),
        "restart must replay exactly the sampled admitted records: {replayed_search}"
    );
    let hits = replayed_search["hits"]
        .as_array()
        .expect("replayed search response must contain hits");
    for object_id in &expected_object_ids {
        assert!(
            hits.iter().any(|hit| hit["objectID"] == json!(object_id)),
            "replayed search must contain sampled objectID {object_id}: {replayed_search}"
        );
    }
    for record in &sampled_records {
        let task =
            server.wait_for_task_published(index_name, record.task_id, Duration::from_secs(10));
        assert_eq!(
            task["pendingTask"],
            json!(false),
            "replayed task {} must publish after restart",
            record.task_id
        );
    }
}

#[test]
#[serial_test::serial(flapjack_server_write_env)]
fn served_batch_queue_full_returns_429_without_admitting_sentinel() {
    let tmp = TempDir::new("fj_test_served_queue_full");
    let index_name = "served_queue_full_idx";
    let fill_token = "served-queue-full-fill";
    let sentinel_object_id = "served-queue-full-sentinel";
    let mut server = RunningServer::spawn_no_auth_auto_port(tmp.path());
    create_index_via_http(&server, index_name);
    server.kill_and_restart_no_auth_auto_port_with_env(
        tmp.path(),
        &[
            ("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS", "20000"),
            ("FLAPJACK_WRITE_QUEUE_CHANNEL_CAPACITY", "2"),
            ("FLAPJACK_WRITE_QUEUE_START_DELAY_MS", "10000"),
        ],
    );

    let prefill_count = TEST_WRITE_QUEUE_CHANNEL_CAPACITY;
    let mut held_requests = Vec::with_capacity(prefill_count);
    for i in 0..prefill_count {
        held_requests.push(spawn_raw_batch_request(
            server.bind_addr(),
            index_name,
            format!("served-queue-fill-{i}"),
            fill_token,
            Duration::from_secs(100),
        ));
    }
    let prefill_records = wait_for_admission_record_count(
        tmp.root(),
        index_name,
        prefill_count,
        Duration::from_secs(30),
    );
    assert_eq!(
        prefill_records.len(),
        prefill_count,
        "QueueFull precondition must fill the effective channel capacity"
    );

    let sentinel_body = batch_payload(sentinel_object_id, sentinel_object_id);
    let sentinel_path = format!("/1/indexes/{index_name}/batch");
    let response = http_request_with_read_timeout(
        server.bind_addr(),
        "POST",
        &sentinel_path,
        &[],
        Some(&sentinel_body),
        Duration::from_secs(5),
    )
    .expect("overflow request must receive a served HTTP response");
    assert_eq!(
        response.status, 429,
        "overflow batch must return QueueFull, got {} with body {}",
        response.status, response.body
    );
    assert_retry_after_one(&response, "QueueFull");
    let body = parse_json_response(&response, "QueueFull");
    assert_eq!(body["status"], json!(429));
    assert_eq!(body["message"], json!("Write queue full"));
    assert!(
        body.get("taskID").is_none(),
        "pre-admission QueueFull must not allocate taskID: {body}"
    );

    server.kill_and_restart_no_auth_auto_port(tmp.path());
    for request in held_requests {
        let _ = request.handle.join();
    }

    assert_search_lacks_object(&server, index_name, sentinel_object_id);
    let remaining_records = read_admission_records(tmp.root(), index_name);
    assert!(
        remaining_records
            .iter()
            .flat_map(|record| record.object_ids.iter())
            .all(|object_id| object_id != sentinel_object_id),
        "rejected sentinel must not appear in admission records: {remaining_records:?}"
    );
}

#[test]
#[serial_test::serial(flapjack_server_write_env)]
fn served_writer_slot_contention_returns_503_not_queue_full() {
    let tmp = TempDir::new("fj_test_served_writer_contention");
    let index_name = "served_writer_contention_idx";
    let object_id = "served-writer-contention-doc";
    let mut server = RunningServer::spawn_no_auth_auto_port(tmp.path());
    create_index_via_http(&server, index_name);
    server.kill_and_restart_no_auth_auto_port_with_env(
        tmp.path(),
        &[
            ("FLAPJACK_MAX_CONCURRENT_WRITERS", "0"),
            ("FLAPJACK_WRITE_DURABLE_TIMEOUT_MS", "15000"),
            ("FLAPJACK_WRITE_QUEUE_BATCH_SIZE", "1"),
            ("FLAPJACK_WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_MS", "5000"),
        ],
    );

    let path = format!("/1/indexes/{index_name}/batch");
    let response = http_request_with_read_timeout(
        server.bind_addr(),
        "POST",
        &path,
        &[],
        Some(&single_doc_payload(object_id, "writer-contention-token")),
        SERVED_WRITER_CONTENTION_RETRY_WINDOW + Duration::from_secs(15),
    )
    .expect("writer-slot contention request must receive a served HTTP response");

    assert_eq!(
        response.status, 503,
        "writer-slot contention must return 503, got {} with body {}",
        response.status, response.body
    );
    assert_retry_after_one(&response, "writer-slot contention");
    let body = parse_json_response(&response, "writer-slot contention");
    assert_eq!(body["status"], json!(503));
    assert!(
        body["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("Too many concurrent writes: ")),
        "writer-slot contention must preserve TooManyConcurrentWrites message: {body}"
    );
    assert!(
        body["taskID"].is_i64(),
        "post-admission writer-slot contention must preserve taskID: {body}"
    );
    assert_ne!(
        body["message"],
        json!("Write queue full"),
        "writer-slot contention must not collapse into QueueFull"
    );
}
