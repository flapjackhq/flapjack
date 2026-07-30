//! Active-write snapshot restore proof.
//!
//! Drives the real file-snapshot HTTP surface (`GET .../export`, `POST .../import`)
//! while a deterministic batch writer is in flight against server A, restores the
//! captured bytes into a fresh data dir on server B, and asserts exact restored
//! doc count plus representative record/search parity. Emits the four
//! `KEY=value` measurement lines required by Stage 2 to stdout and to
//! `engine/target/dr_proof/latest/measurements.txt`.

#![cfg(debug_assertions)]

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::{Duration, Instant};

use serde_json::json;
use tokio::sync::Mutex;

const INDEX_NAME: &str = "snapshot_under_load";
const BATCH_SIZE: u64 = 50;
// Enough batches that meaningful HTTP traffic is in flight before/after the
// snapshot window; small enough to keep the test fast (~seconds).
const TOTAL_BATCHES: u64 = 40;
const SNAPSHOT_AFTER_BATCHES: u64 = 10;
const WRITE_QUEUE_TEST_COMMIT_DELAY_ENV_VAR: &str = "FLAPJACK_WRITE_QUEUE_TEST_COMMIT_DELAY_MS";

static ENV_MUTEX: StdMutex<()> = StdMutex::new(());

struct EnvVarGuard {
    name: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

struct SnapshotSearchCapture {
    hits: Vec<serde_json::Value>,
    total_hits: u64,
}

fn make_doc(id: u64) -> serde_json::Value {
    // Deterministic, content-addressable doc shape. `name` includes the id so
    // we can grep individual docs; `bucket` partitions for representative
    // search-parity assertions across the batch window.
    json!({
        "objectID": format!("doc_{id:06}"),
        "name": format!("alpha_{id}"),
        "bucket": format!("b{}", id % 8),
        "v": id,
    })
}

fn batch_payload(start_id: u64) -> serde_json::Value {
    let requests: Vec<serde_json::Value> = (start_id..start_id + BATCH_SIZE)
        .map(|id| json!({ "action": "addObject", "body": make_doc(id) }))
        .collect();
    json!({ "requests": requests })
}

fn retained_channel_closed_count(tenant_id: &str) -> usize {
    flapjack::index::write_queue::writer_lifecycle_test_events(tenant_id)
        .iter()
        .filter(|event| event.reason == "channel_closed" && event.phase == "merge_quiesced")
        .count()
}

fn assert_retained_channel_closed_delta(tenant_id: &str, before: usize, message: &str) {
    let after = retained_channel_closed_count(tenant_id);
    assert_eq!(after - before, 1, "{message}");
}

fn assert_quiescence_before_publication(tenant_id: &str, publication_phase: &'static str) {
    let events = flapjack::index::write_queue::writer_lifecycle_test_events(tenant_id);
    let quiesced_sequence = events
        .iter()
        .find(|event| event.reason == "channel_closed" && event.phase == "merge_quiesced")
        .map(|event| event.sequence)
        .unwrap_or_else(|| {
            panic!(
                "tenant {tenant_id} must retain a channel_closed merge-quiesced event: {events:?}"
            )
        });
    let publication_sequence = events
        .iter()
        .find(|event| event.phase == publication_phase)
        .map(|event| event.sequence)
        .unwrap_or_else(|| {
            panic!(
                "tenant {tenant_id} must retain publication event {publication_phase}: {events:?}"
            )
        });
    assert!(
        quiesced_sequence < publication_sequence,
        "tenant {tenant_id} must merge-quiesce before {publication_phase}; events: {events:?}"
    );
}

async fn post_batch_and_wait(
    client: &reqwest::Client,
    addr: &str,
    start_id: u64,
) -> Result<(), String> {
    let resp = client
        .post(format!("http://{addr}/1/indexes/{INDEX_NAME}/batch"))
        .json(&batch_payload(start_id))
        .send()
        .await
        .map_err(|e| format!("batch send failed at {start_id}: {e}"))?;
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("batch body parse failed at {start_id}: {e}"))?;
    if !status.is_success() {
        return Err(format!("batch failed at {start_id}: {status} {body}"));
    }
    let task_id = body
        .get("taskID")
        .and_then(|v| v.as_i64())
        .ok_or_else(|| format!("missing taskID in batch response at {start_id}: {body}"))?;
    common::wait_for_task(client, addr, task_id).await;
    Ok(())
}

async fn post_batch_without_wait(client: &reqwest::Client, addr: &str, start_id: u64) -> i64 {
    let resp = client
        .post(format!("http://{addr}/1/indexes/{INDEX_NAME}/batch"))
        .json(&batch_payload(start_id))
        .send()
        .await
        .unwrap_or_else(|error| panic!("batch send failed at {start_id}: {error}"));
    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .unwrap_or_else(|error| panic!("batch body parse failed at {start_id}: {error}"));
    assert!(
        status.is_success(),
        "batch failed at {start_id}: {status} {body}"
    );
    body.get("taskID")
        .and_then(|value| value.as_i64())
        .unwrap_or_else(|| panic!("missing taskID in batch response at {start_id}: {body}"))
}

/// Background writer: loops sending batches until either `TOTAL_BATCHES`
/// acknowledged-docs / BATCH_SIZE batches have completed OR `shutdown` is set.
/// `pause_lock` is acquired around the batch+ack so the snapshot driver can
/// take it to pin the in-flight state at a deterministic boundary.
async fn run_writer(
    addr: String,
    next_id: Arc<AtomicU64>,
    total_acked: Arc<AtomicU64>,
    shutdown: Arc<AtomicBool>,
    pause_lock: Arc<Mutex<()>>,
) -> Result<(), String> {
    let client = reqwest::Client::new();
    let mut completed_batches: u64 = 0;
    while completed_batches < TOTAL_BATCHES && !shutdown.load(Ordering::Acquire) {
        let _guard = pause_lock.lock().await;
        if shutdown.load(Ordering::Acquire) {
            break;
        }
        let start_id = next_id.fetch_add(BATCH_SIZE, Ordering::AcqRel);
        post_batch_and_wait(&client, &addr, start_id).await?;
        total_acked.fetch_add(BATCH_SIZE, Ordering::AcqRel);
        completed_batches += 1;
        drop(_guard);
        // Yield so the snapshot driver has a fair chance to acquire pause_lock
        // between batches even on a busy runtime.
        tokio::task::yield_now().await;
    }
    Ok(())
}

async fn create_index(client: &reqwest::Client, addr: &str) {
    // Settings PUT auto-creates the tenant. Subsequent batches from the writer
    // task populate the index.
    let settings_resp = client
        .put(format!("http://{addr}/1/indexes/{INDEX_NAME}/settings"))
        .json(&json!({
            "searchableAttributes": ["name", "bucket"],
            "attributesForFaceting": ["bucket"],
        }))
        .send()
        .await
        .expect("set settings failed");
    assert!(
        settings_resp.status().is_success(),
        "settings PUT non-2xx: {}",
        settings_resp.status()
    );
    let body: serde_json::Value = settings_resp.json().await.unwrap();
    if let Some(task_id) = body.get("taskID").and_then(|v| v.as_i64()) {
        common::wait_for_task(client, addr, task_id).await;
    }
}

async fn fetch_total_hits(client: &reqwest::Client, addr: &str) -> u64 {
    let resp = client
        .post(format!("http://{addr}/1/indexes/{INDEX_NAME}/query"))
        .json(&json!({
            "query": "",
            "hitsPerPage": 0,
        }))
        .send()
        .await
        .expect("query send failed");
    assert!(
        resp.status().is_success(),
        "query non-2xx: {}",
        resp.status()
    );
    let body: serde_json::Value = resp.json().await.unwrap();
    body["nbHits"].as_u64().unwrap_or_else(|| {
        panic!("nbHits missing or not integer: {body}");
    })
}

async fn wait_until_total_hits_eq(
    client: &reqwest::Client,
    addr: &str,
    expected: u64,
    deadline: Duration,
) -> u64 {
    let start = Instant::now();
    let mut last = 0u64;
    while start.elapsed() < deadline {
        // Tolerate transient 404 / "index not ready yet" right after import by
        // re-issuing the query a few times. fetch_total_hits already panics on
        // non-2xx, so we do a soft probe here.
        let resp = client
            .post(format!("http://{addr}/1/indexes/{INDEX_NAME}/query"))
            .json(&json!({ "query": "", "hitsPerPage": 0 }))
            .send()
            .await;
        if let Ok(resp) = resp {
            if resp.status().is_success() {
                let body: serde_json::Value = resp.json().await.unwrap();
                last = body["nbHits"].as_u64().unwrap_or(0);
                if last == expected {
                    return last;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    last
}

/// Poll the legacy `GET /1/indexes/{index}` search route until it returns
/// success. Import can return before read routes are consistently ready.
async fn wait_until_index_get_ready(client: &reqwest::Client, addr: &str, deadline: Duration) {
    let start = Instant::now();
    let mut last_status = String::new();
    let mut last_body = String::new();
    while start.elapsed() < deadline {
        let resp = client
            .get(format!("http://{addr}/1/indexes/{INDEX_NAME}"))
            .send()
            .await;
        if let Ok(resp) = resp {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_else(|_| String::new());
            if status.is_success() {
                return;
            }
            last_status = status.to_string();
            last_body = body;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!(
        "GET /1/indexes/{INDEX_NAME} never became ready on {addr} within {:?}; last status/body: {} {}",
        deadline, last_status, last_body
    );
}

async fn get_object(
    client: &reqwest::Client,
    addr: &str,
    object_id: &str,
) -> Option<serde_json::Value> {
    let resp = client
        .get(format!("http://{addr}/1/indexes/{INDEX_NAME}/{object_id}"))
        .send()
        .await
        .expect("get object send failed");
    if !resp.status().is_success() {
        return None;
    }
    Some(resp.json::<serde_json::Value>().await.unwrap())
}

fn measurements_path() -> PathBuf {
    // CARGO_MANIFEST_DIR is `engine/` for the `flapjack` crate's integration tests.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .join("target")
        .join("dr_proof")
        .join("latest")
        .join("measurements.txt")
}

/// Atomically write the measurements file. Overwrites any prior run.
fn write_measurements(
    path: &std::path::Path,
    rpo_ms: u64,
    rto_ms: u64,
    doc_count_at_snapshot: u64,
    doc_count_at_restore: u64,
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let contents = format!(
        "RPO_MEASURED_MS={rpo_ms}\n\
         RTO_MEASURED_MS={rto_ms}\n\
         DOC_COUNT_AT_SNAPSHOT={doc_count_at_snapshot}\n\
         DOC_COUNT_AT_RESTORE={doc_count_at_restore}\n",
    );
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Parse the measurements file back and assert all four required keys are
/// present and parseable as integers. Returns the parsed values for
/// downstream comparison if needed.
fn read_and_verify_measurements(path: &std::path::Path) -> [(String, u64); 4] {
    let contents = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("measurements file unreadable {}: {e}", path.display()));
    let mut found: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("malformed measurement line (no '='): {line}"));
        let parsed = v
            .parse::<u64>()
            .unwrap_or_else(|e| panic!("measurement value {v} for key {k} not u64: {e}"));
        found.insert(k.to_string(), parsed);
    }
    let required = [
        "RPO_MEASURED_MS",
        "RTO_MEASURED_MS",
        "DOC_COUNT_AT_SNAPSHOT",
        "DOC_COUNT_AT_RESTORE",
    ];
    let mut out: Vec<(String, u64)> = Vec::with_capacity(4);
    for key in required {
        let v = found.remove(key).unwrap_or_else(|| {
            panic!(
                "measurements file at {} missing required key {key}; contents:\n{}",
                path.display(),
                contents
            )
        });
        out.push((key.to_string(), v));
    }
    out.try_into().expect("exactly four entries")
}

/// Block until the writer has acknowledged at least `target` docs so the
/// snapshot is taken mid-stream rather than at t=0. Panics (after signaling
/// shutdown) if the writer never reaches the target within the warmup window.
async fn warm_up_writer(total_acked: &AtomicU64, target: u64, shutdown: &AtomicBool) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while total_acked.load(Ordering::Acquire) < target {
        if Instant::now() > deadline {
            shutdown.store(true, Ordering::Release);
            panic!("writer never reached {target} acked docs within warmup window");
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Pin the writer at a deterministic boundary, sanity-check HTTP-visible
/// state, then export. Returns `(snapshot_bytes, doc_count_at_snapshot,
/// rpo_measured_ms)`.
async fn take_snapshot_under_lock(
    client: &reqwest::Client,
    addr_a: &str,
    pause_lock: &Mutex<()>,
    total_acked: &AtomicU64,
    warmup_target: u64,
) -> (Vec<u8>, u64, u64, SnapshotSearchCapture) {
    let _guard = pause_lock.lock().await;
    // Any in-flight batch has fully acked (it was holding the lock); the
    // counter now exactly matches committed state.
    let doc_count_at_snapshot = total_acked.load(Ordering::Acquire);
    assert!(
        doc_count_at_snapshot >= warmup_target,
        "snapshot count {doc_count_at_snapshot} below warmup target {warmup_target}"
    );
    let writer_ceiling = TOTAL_BATCHES * BATCH_SIZE;
    assert!(
        doc_count_at_snapshot < writer_ceiling,
        "snapshot taken after writer finished all batches \
         ({doc_count_at_snapshot} >= {writer_ceiling}); does not prove export-under-load"
    );

    // Sanity: HTTP-visible count on server A matches the writer's counter.
    let hits_a = fetch_total_hits(client, addr_a).await;
    assert_eq!(
        hits_a, doc_count_at_snapshot,
        "server A HTTP nbHits ({hits_a}) disagrees with writer counter ({doc_count_at_snapshot}) at snapshot boundary"
    );

    let t_snapshot_start = Instant::now();
    let export_resp = client
        .get(format!("http://{addr_a}/1/indexes/{INDEX_NAME}/export"))
        .send()
        .await
        .expect("export send failed");
    assert!(
        export_resp.status().is_success(),
        "export non-2xx: {}",
        export_resp.status()
    );
    let snapshot_bytes = export_resp
        .bytes()
        .await
        .expect("export body read failed")
        .to_vec();
    let rpo_measured_ms = t_snapshot_start.elapsed().as_millis() as u64;
    assert!(
        !snapshot_bytes.is_empty(),
        "exported snapshot bytes were empty"
    );

    let parity_query = json!({
        "query": "",
        "filters": "bucket:b3",
        "hitsPerPage": 1000,
    });
    let parity_resp = client
        .post(format!("http://{addr_a}/1/indexes/{INDEX_NAME}/query"))
        .json(&parity_query)
        .send()
        .await
        .expect("snapshot-moment parity query on A failed");
    assert!(parity_resp.status().is_success());
    let parity_body: serde_json::Value = parity_resp.json().await.unwrap();
    let snapshot_search = SnapshotSearchCapture {
        hits: parity_body["hits"]
            .as_array()
            .expect("no hits array in snapshot-moment query")
            .clone(),
        total_hits: parity_body["nbHits"].as_u64().unwrap(),
    };

    (
        snapshot_bytes,
        doc_count_at_snapshot,
        rpo_measured_ms,
        snapshot_search,
    )
}

async fn export_snapshot_without_pause_lock(
    client: &reqwest::Client,
    addr_a: &str,
) -> (Vec<u8>, u64) {
    let t_snapshot_start = Instant::now();
    let export_resp = client
        .get(format!("http://{addr_a}/1/indexes/{INDEX_NAME}/export"))
        .send()
        .await
        .expect("export send failed");
    assert!(
        export_resp.status().is_success(),
        "export non-2xx: {}",
        export_resp.status()
    );
    let snapshot_bytes = export_resp
        .bytes()
        .await
        .expect("export body read failed")
        .to_vec();
    assert!(
        !snapshot_bytes.is_empty(),
        "exported snapshot bytes were empty"
    );
    (
        snapshot_bytes,
        t_snapshot_start.elapsed().as_millis() as u64,
    )
}

async fn capture_search_parity(client: &reqwest::Client, addr_a: &str) -> SnapshotSearchCapture {
    let parity_query = json!({
        "query": "",
        "filters": "bucket:b3",
        "hitsPerPage": 1000,
    });
    let parity_resp = client
        .post(format!("http://{addr_a}/1/indexes/{INDEX_NAME}/query"))
        .json(&parity_query)
        .send()
        .await
        .expect("snapshot-moment parity query on A failed");
    assert!(parity_resp.status().is_success());
    let parity_body: serde_json::Value = parity_resp.json().await.unwrap();
    SnapshotSearchCapture {
        hits: parity_body["hits"]
            .as_array()
            .expect("no hits array in snapshot-moment query")
            .clone(),
        total_hits: parity_body["nbHits"].as_u64().unwrap(),
    }
}

/// Import the captured bytes into server B, poll until queryable at the
/// expected count, and assert exact restored-count parity. Returns
/// `(doc_count_at_restore, rto_measured_ms)`.
async fn restore_and_verify_count(
    client: &reqwest::Client,
    addr_b: &str,
    snapshot_bytes: Vec<u8>,
    doc_count_at_snapshot: u64,
) -> (u64, u64) {
    let t_restore_start = Instant::now();
    let import_resp = client
        .post(format!("http://{addr_b}/1/indexes/{INDEX_NAME}/import"))
        .header("content-type", "application/gzip")
        .body(snapshot_bytes)
        .send()
        .await
        .expect("import send failed");
    let import_status = import_resp.status();
    let import_body: serde_json::Value = import_resp.json().await.expect("import body parse");
    assert!(
        import_status.is_success(),
        "import non-2xx on server B: {import_status} {import_body}"
    );
    assert_eq!(
        import_body["status"].as_str(),
        Some("imported"),
        "import response missing status=imported: {import_body}"
    );

    wait_until_index_get_ready(client, addr_b, Duration::from_secs(30)).await;
    let hits_b = wait_until_total_hits_eq(
        client,
        addr_b,
        doc_count_at_snapshot,
        Duration::from_secs(30),
    )
    .await;
    let rto_measured_ms = t_restore_start.elapsed().as_millis() as u64;
    let doc_count_at_restore = hits_b;
    assert_eq!(
        doc_count_at_restore, doc_count_at_snapshot,
        "restored doc count ({doc_count_at_restore}) does not equal snapshot doc count ({doc_count_at_snapshot})"
    );
    (doc_count_at_restore, rto_measured_ms)
}

/// Record-parity sweep: `getObject` for first/middle/last IDs in the snapshot
/// window, asserting all addressable fields agree between A and B.
async fn assert_record_parity(
    client: &reqwest::Client,
    addr_a: &str,
    addr_b: &str,
    doc_count_at_snapshot: u64,
) {
    let last = doc_count_at_snapshot - 1;
    let mid = doc_count_at_snapshot / 2;
    let sampled_ids = [
        format!("doc_{:06}", 0),
        format!("doc_{:06}", mid),
        format!("doc_{:06}", last),
    ];
    for object_id in &sampled_ids {
        let from_a = get_object(client, addr_a, object_id)
            .await
            .unwrap_or_else(|| panic!("server A missing sampled object {object_id}"));
        let from_b = get_object(client, addr_b, object_id)
            .await
            .unwrap_or_else(|| panic!("server B missing sampled object {object_id} after restore"));
        assert_eq!(
            from_a["objectID"], from_b["objectID"],
            "objectID mismatch for {object_id}"
        );
        assert_eq!(
            from_a["name"], from_b["name"],
            "name mismatch for {object_id}"
        );
        assert_eq!(from_a["v"], from_b["v"], "v mismatch for {object_id}");
        assert_eq!(
            from_a["bucket"], from_b["bucket"],
            "bucket mismatch for {object_id}"
        );
    }
}

/// Search-parity sweep: server B's `bucket:b3` hits must equal the
/// closed-form expected count from the deterministic doc generator; server A
/// is still mid-stream and may only have advanced past that count.
async fn assert_search_parity(
    client: &reqwest::Client,
    addr_b: &str,
    snapshot_search: &SnapshotSearchCapture,
    doc_count_at_snapshot: u64,
) {
    let expected_bucket_b3 = (0..doc_count_at_snapshot).filter(|i| i % 8 == 3).count() as u64;
    assert_eq!(
        snapshot_search.total_hits, expected_bucket_b3,
        "server A snapshot-moment bucket:b3 count ({}) != deterministic expected ({expected_bucket_b3})",
        snapshot_search.total_hits
    );

    let parity_query = json!({
        "query": "",
        "filters": "bucket:b3",
        "hitsPerPage": 1000,
    });
    let resp_b = client
        .post(format!("http://{addr_b}/1/indexes/{INDEX_NAME}/query"))
        .json(&parity_query)
        .send()
        .await
        .unwrap();
    assert!(resp_b.status().is_success());
    let body_b: serde_json::Value = resp_b.json().await.unwrap();
    let hits_b = body_b["hits"]
        .as_array()
        .expect("no hits array from server B");
    let nb_hits_b = body_b["nbHits"].as_u64().unwrap();
    assert_eq!(
        nb_hits_b, snapshot_search.total_hits,
        "server B bucket:b3 nbHits ({nb_hits_b}) != server A snapshot moment ({})",
        snapshot_search.total_hits
    );

    let ids_a: std::collections::BTreeSet<&str> = snapshot_search
        .hits
        .iter()
        .map(|h| h["objectID"].as_str().expect("hit missing objectID"))
        .collect();
    let ids_b: std::collections::BTreeSet<&str> = hits_b
        .iter()
        .map(|h| h["objectID"].as_str().expect("hit missing objectID"))
        .collect();
    assert_eq!(
        ids_a, ids_b,
        "objectID sets differ between snapshot-moment A and restored B"
    );

    let sample_size = std::cmp::min(5, hits_b.len());
    for hit_b in hits_b.iter().take(sample_size) {
        let oid = hit_b["objectID"].as_str().unwrap();
        let hit_a = snapshot_search
            .hits
            .iter()
            .find(|h| h["objectID"].as_str() == Some(oid))
            .unwrap_or_else(|| panic!("objectID {oid} in B not found in A's snapshot"));
        assert_eq!(hit_a["name"], hit_b["name"], "name mismatch for {oid}");
        assert_eq!(hit_a["v"], hit_b["v"], "v mismatch for {oid}");
        assert_eq!(
            hit_a["bucket"], hit_b["bucket"],
            "bucket mismatch for {oid}"
        );
    }
}

async fn fetch_total_hits_from_app(app: &axum::Router) -> u64 {
    let resp = common::send_oneshot(
        app,
        axum::http::Method::POST,
        &format!("/1/indexes/{INDEX_NAME}/query"),
        &[("content-type", "application/json")],
        axum::body::Body::from(json!({"query":"","hitsPerPage":0}).to_string()),
    )
    .await;
    assert!(resp.status().is_success(), "restarted app query failed");
    let body = common::parse_response_json(resp).await;
    body["nbHits"].as_u64().unwrap_or_else(|| {
        panic!("restarted app nbHits missing or not integer: {body}");
    })
}

async fn wait_until_total_hits_eq_app(
    app: &axum::Router,
    expected: u64,
    deadline: Duration,
) -> u64 {
    let start = Instant::now();
    let mut last = 0u64;
    while start.elapsed() < deadline {
        last = fetch_total_hits_from_app(app).await;
        if last == expected {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    last
}

async fn get_object_from_app(app: &axum::Router, object_id: &str) -> Option<serde_json::Value> {
    let resp = common::send_oneshot(
        app,
        axum::http::Method::GET,
        &format!("/1/indexes/{INDEX_NAME}/{object_id}"),
        &[],
        axum::body::Body::empty(),
    )
    .await;
    if !resp.status().is_success() {
        return None;
    }
    Some(common::parse_response_json(resp).await)
}

async fn assert_record_parity_on_restarted_app(
    client: &reqwest::Client,
    addr_a: &str,
    restarted_app: &axum::Router,
    doc_count_at_snapshot: u64,
) {
    let last = doc_count_at_snapshot - 1;
    let mid = doc_count_at_snapshot / 2;
    let sampled_ids = [
        format!("doc_{:06}", 0),
        format!("doc_{:06}", mid),
        format!("doc_{:06}", last),
    ];
    for object_id in &sampled_ids {
        let from_a = get_object(client, addr_a, object_id)
            .await
            .unwrap_or_else(|| panic!("server A missing sampled object {object_id}"));
        let from_restarted = get_object_from_app(restarted_app, object_id)
            .await
            .unwrap_or_else(|| panic!("restarted app missing sampled object {object_id}"));
        assert_eq!(from_a["objectID"], from_restarted["objectID"]);
        assert_eq!(from_a["name"], from_restarted["name"]);
        assert_eq!(from_a["v"], from_restarted["v"]);
        assert_eq!(from_a["bucket"], from_restarted["bucket"]);
    }
}

async fn assert_search_parity_on_restarted_app(
    restarted_app: &axum::Router,
    snapshot_search: &SnapshotSearchCapture,
    doc_count_at_snapshot: u64,
) {
    let expected_bucket_b3 = (0..doc_count_at_snapshot).filter(|i| i % 8 == 3).count() as u64;
    let query_resp = common::send_oneshot(
        restarted_app,
        axum::http::Method::POST,
        &format!("/1/indexes/{INDEX_NAME}/query"),
        &[("content-type", "application/json")],
        axum::body::Body::from(
            json!({"query":"","filters":"bucket:b3","hitsPerPage":1000}).to_string(),
        ),
    )
    .await;
    assert!(
        query_resp.status().is_success(),
        "restarted app parity query failed"
    );
    let body = common::parse_response_json(query_resp).await;
    let nb_hits = body["nbHits"].as_u64().unwrap();
    let hits = body["hits"].as_array().expect("restarted app hits missing");
    assert_eq!(
        nb_hits, snapshot_search.total_hits,
        "restarted app bucket:b3 count ({nb_hits}) != snapshot-moment count ({})",
        snapshot_search.total_hits
    );
    assert_eq!(
        nb_hits, expected_bucket_b3,
        "restarted app bucket:b3 count ({nb_hits}) != deterministic expected ({expected_bucket_b3})"
    );

    let ids_a: std::collections::BTreeSet<&str> = snapshot_search
        .hits
        .iter()
        .map(|h| {
            h["objectID"]
                .as_str()
                .expect("snapshot hit missing objectID")
        })
        .collect();
    let ids_restarted: std::collections::BTreeSet<&str> = hits
        .iter()
        .map(|h| {
            h["objectID"]
                .as_str()
                .expect("restarted hit missing objectID")
        })
        .collect();
    assert_eq!(
        ids_a, ids_restarted,
        "objectID sets differ between snapshot-moment A and restarted restored app"
    );
}

/// Re-open server B's imported data dir and re-run restored-count and
/// representative record/search parity checks against the restarted app.
async fn assert_restart_durability_after_import(
    client: &reqwest::Client,
    addr_a: &str,
    data_dir: &std::path::Path,
    snapshot_search: &SnapshotSearchCapture,
    doc_count_at_snapshot: u64,
) {
    let restarted_app = common::build_test_app_for_existing_data_dir(data_dir, None);
    let hits = wait_until_total_hits_eq_app(
        &restarted_app,
        doc_count_at_snapshot,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        hits, doc_count_at_snapshot,
        "restarted app doc count ({hits}) does not equal snapshot doc count ({doc_count_at_snapshot})"
    );

    assert_record_parity_on_restarted_app(client, addr_a, &restarted_app, doc_count_at_snapshot)
        .await;
    assert_search_parity_on_restarted_app(&restarted_app, snapshot_search, doc_count_at_snapshot)
        .await;
}

/// After releasing the snapshot lock, confirm the writer advances past the
/// snapshot boundary — proving it was genuinely in-flight during export, not
/// merely hadn't-yet-started-batch-11-through-40.
async fn assert_writer_still_active(total_acked: &AtomicU64, doc_count_at_snapshot: u64) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while total_acked.load(Ordering::Acquire) <= doc_count_at_snapshot {
        assert!(
            Instant::now() < deadline,
            "writer did not advance past snapshot count ({doc_count_at_snapshot}) \
             after lock released — load was not in-flight during export"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Signal the writer to stop, await its exit, and surface any error /
/// panic / timeout instead of silently swallowing them — a regression in
/// the batch endpoint after the snapshot window must fail this test.
async fn tear_down_writer(
    writer_handle: tokio::task::JoinHandle<Result<(), String>>,
    shutdown: &AtomicBool,
    total_acked: &AtomicU64,
    doc_count_at_snapshot: u64,
    writer_target_total: u64,
) {
    shutdown.store(true, Ordering::Release);
    match tokio::time::timeout(Duration::from_secs(30), writer_handle).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => panic!("writer returned error: {e}"),
        Ok(Err(e)) => panic!("writer task panicked: {e}"),
        Err(_) => panic!("writer did not shut down within 30s"),
    }
    let total_after = total_acked.load(Ordering::Acquire);
    assert!(
        total_after >= doc_count_at_snapshot,
        "writer regressed: total_after={total_after} < doc_count_at_snapshot={doc_count_at_snapshot}"
    );
    assert!(
        total_after <= writer_target_total,
        "writer overshot configured ceiling: total_after={total_after} > {writer_target_total}"
    );
}

/// Print the four required `KEY=value` lines to stdout, atomically write
/// them to the measurements artifact, then read the file back and assert
/// every key/value round-trips — Stage 2 must never transcribe garbage.
fn emit_and_verify_measurements(
    rpo_measured_ms: u64,
    rto_measured_ms: u64,
    doc_count_at_snapshot: u64,
    doc_count_at_restore: u64,
) {
    let measurements = [
        ("RPO_MEASURED_MS", rpo_measured_ms),
        ("RTO_MEASURED_MS", rto_measured_ms),
        ("DOC_COUNT_AT_SNAPSHOT", doc_count_at_snapshot),
        ("DOC_COUNT_AT_RESTORE", doc_count_at_restore),
    ];
    for (k, v) in &measurements {
        println!("{k}={v}");
    }
    let path = measurements_path();
    write_measurements(
        &path,
        rpo_measured_ms,
        rto_measured_ms,
        doc_count_at_snapshot,
        doc_count_at_restore,
    )
    .unwrap_or_else(|e| panic!("write_measurements at {} failed: {e}", path.display()));

    let parsed = read_and_verify_measurements(&path);
    for ((expected_k, expected_v), (got_k, got_v)) in measurements.iter().zip(parsed.iter()) {
        assert_eq!(
            expected_k, got_k,
            "measurements file key order: expected {expected_k} got {got_k}"
        );
        assert_eq!(
            expected_v, got_v,
            "measurements file value for {expected_k}: expected {expected_v} got {got_v}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[serial_test::serial(snapshot_under_load_shared_state)]
async fn snapshot_export_under_load_restores_with_exact_doc_count_and_record_parity() {
    let (addr_a, _tmp_a) = common::spawn_server_with_key(None).await;
    let client = reqwest::Client::new();
    create_index(&client, &addr_a).await;

    let next_id = Arc::new(AtomicU64::new(0));
    let total_acked = Arc::new(AtomicU64::new(0));
    let shutdown = Arc::new(AtomicBool::new(false));
    let pause_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));

    let writer_handle = tokio::spawn(run_writer(
        addr_a.clone(),
        Arc::clone(&next_id),
        Arc::clone(&total_acked),
        Arc::clone(&shutdown),
        Arc::clone(&pause_lock),
    ));

    let target_before_snapshot = SNAPSHOT_AFTER_BATCHES * BATCH_SIZE;
    warm_up_writer(&total_acked, target_before_snapshot, &shutdown).await;

    let (snapshot_bytes, doc_count_at_snapshot, rpo_measured_ms, snapshot_search) =
        take_snapshot_under_lock(
            &client,
            &addr_a,
            &pause_lock,
            &total_acked,
            target_before_snapshot,
        )
        .await;

    assert_writer_still_active(&total_acked, doc_count_at_snapshot).await;

    let (addr_b, tmp_b) = common::spawn_server_with_key(None).await;
    let (doc_count_at_restore, rto_measured_ms) =
        restore_and_verify_count(&client, &addr_b, snapshot_bytes, doc_count_at_snapshot).await;

    assert_record_parity(&client, &addr_a, &addr_b, doc_count_at_snapshot).await;
    assert_search_parity(&client, &addr_b, &snapshot_search, doc_count_at_snapshot).await;
    assert_restart_durability_after_import(
        &client,
        &addr_a,
        tmp_b.path(),
        &snapshot_search,
        doc_count_at_snapshot,
    )
    .await;

    let writer_target_total = TOTAL_BATCHES * BATCH_SIZE;
    tear_down_writer(
        writer_handle,
        &shutdown,
        &total_acked,
        doc_count_at_snapshot,
        writer_target_total,
    )
    .await;

    emit_and_verify_measurements(
        rpo_measured_ms,
        rto_measured_ms,
        doc_count_at_snapshot,
        doc_count_at_restore,
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(snapshot_under_load_shared_state)]
async fn snapshot_during_active_write_restores_exact_record_parity() {
    let _env_lock = ENV_MUTEX.lock().expect("env mutex should lock");
    flapjack::index::write_queue::clear_writer_lifecycle_test_events();
    let _commit_delay = EnvVarGuard::set(WRITE_QUEUE_TEST_COMMIT_DELAY_ENV_VAR, "250");
    let (addr_a, _tmp_a) = common::spawn_server_with_key(None).await;
    let client = reqwest::Client::new();
    create_index(&client, &addr_a).await;

    let expected_snapshot_count = SNAPSHOT_AFTER_BATCHES * BATCH_SIZE + BATCH_SIZE;
    assert!(
        expected_snapshot_count < TOTAL_BATCHES * BATCH_SIZE,
        "active snapshot specimen must remain mid-stream rather than consuming every configured batch"
    );
    for batch in 0..SNAPSHOT_AFTER_BATCHES {
        post_batch_and_wait(&client, &addr_a, batch * BATCH_SIZE)
            .await
            .unwrap();
    }
    assert_eq!(
        fetch_total_hits(&client, &addr_a).await,
        SNAPSHOT_AFTER_BATCHES * BATCH_SIZE,
        "fixture must publish exactly the first 10 batches before opening the active snapshot window"
    );

    let active_batch = tokio::spawn({
        let client = client.clone();
        let addr_a = addr_a.clone();
        async move {
            post_batch_without_wait(&client, &addr_a, SNAPSHOT_AFTER_BATCHES * BATCH_SIZE).await
        }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !active_batch.is_finished(),
        "fixture must keep the batch request in flight when snapshot export starts"
    );
    let merge_wait_before = retained_channel_closed_count(INDEX_NAME);
    let (snapshot_bytes, rpo_measured_ms) =
        export_snapshot_without_pause_lock(&client, &addr_a).await;
    assert_retained_channel_closed_delta(
        INDEX_NAME,
        merge_wait_before,
        "snapshot export must drain and merge-quiesce the active writer before reading files",
    );
    assert_quiescence_before_publication(INDEX_NAME, "snapshot_export_read");
    let active_task_id = active_batch
        .await
        .expect("active batch request task must not panic");
    common::wait_for_task(&client, &addr_a, active_task_id).await;

    let visible_after_task = fetch_total_hits(&client, &addr_a).await;
    assert_eq!(
        visible_after_task, expected_snapshot_count,
        "setup must make the extra active batch visible before parity assertions"
    );
    let snapshot_search = capture_search_parity(&client, &addr_a).await;

    let (addr_b, tmp_b) = common::spawn_server_with_key(None).await;
    let (doc_count_at_restore, rto_measured_ms) =
        restore_and_verify_count(&client, &addr_b, snapshot_bytes, expected_snapshot_count).await;

    assert_record_parity(&client, &addr_a, &addr_b, expected_snapshot_count).await;
    assert_search_parity(&client, &addr_b, &snapshot_search, expected_snapshot_count).await;
    assert_restart_durability_after_import(
        &client,
        &addr_a,
        tmp_b.path(),
        &snapshot_search,
        expected_snapshot_count,
    )
    .await;

    emit_and_verify_measurements(
        rpo_measured_ms,
        rto_measured_ms,
        expected_snapshot_count,
        doc_count_at_restore,
    );
}
