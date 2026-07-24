#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

//! Real-server restart-during-active-writes proof on a stable bind address.
mod support;

use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use support::{http_request, wait_for_task_published_at, RunningServer, TempDir};

#[test]
fn acknowledged_writes_remain_searchable_across_restart_during_active_traffic() {
    let tmp = TempDir::new("fj_test_restart_during_writes");
    let index_name = "restart_during_writes_idx";
    let shared_query_token = "restart-active-writes-proof";
    let target_acknowledged_docs = 36usize;

    let mut server = RunningServer::spawn_no_auth_fixed_port(tmp.path());
    let bind_addr = server.bind_addr().to_string();

    let stop_writes = Arc::new(AtomicBool::new(false));
    let acknowledged_ids = Arc::new(Mutex::new(Vec::<String>::new()));

    let writer_stop = Arc::clone(&stop_writes);
    let writer_ids = Arc::clone(&acknowledged_ids);
    let writer_bind_addr = bind_addr.clone();
    let writer_handle = thread::spawn(move || {
        let mut next_doc_index = 0usize;
        while !writer_stop.load(Ordering::SeqCst) {
            if writer_ids.lock().expect("writer ids mutex poisoned").len()
                >= target_acknowledged_docs
            {
                break;
            }

            let object_id = format!("restart-proof-doc-{next_doc_index:03}");
            next_doc_index += 1;

            if write_single_document_and_wait(
                &writer_bind_addr,
                index_name,
                &object_id,
                shared_query_token,
            )
            .is_some()
            {
                writer_ids
                    .lock()
                    .expect("writer ids mutex poisoned")
                    .push(object_id);
            } else {
                // Restart windows intentionally produce transient connection failures;
                // keep pressure on the same endpoint until the restarted process is healthy.
                thread::sleep(Duration::from_millis(25));
            }
        }
    });

    wait_for_acknowledged_docs(&acknowledged_ids, 12, Duration::from_secs(20));

    server.kill_and_restart_no_auth_same_bind_addr(tmp.path());

    wait_for_acknowledged_docs(
        &acknowledged_ids,
        target_acknowledged_docs,
        Duration::from_secs(30),
    );

    stop_writes.store(true, Ordering::SeqCst);
    writer_handle
        .join()
        .expect("writer thread should join cleanly after restart proof");

    let acknowledged_ids = acknowledged_ids
        .lock()
        .expect("writer ids mutex poisoned")
        .clone();
    let acknowledged_count = acknowledged_ids.len();
    assert!(
        acknowledged_count >= target_acknowledged_docs,
        "expected at least {target_acknowledged_docs} acknowledged docs across restart, got {acknowledged_count}"
    );

    // A write can become durable immediately before SIGKILL prevents the client
    // from observing its response. Prove every acknowledged ID survived without
    // treating such correctly replayed, outcome-unknown writes as data corruption.
    let post_restart_search = server.search(
        index_name,
        json!({
            "query": shared_query_token,
            "hitsPerPage": 1000
        }),
    );
    let post_restart_count = post_restart_search["nbHits"]
        .as_u64()
        .expect("post-restart search must report an integer nbHits")
        as usize;
    let searchable_ids = post_restart_search["hits"]
        .as_array()
        .expect("post-restart search must report hits")
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .expect("post-restart hit must report an objectID")
        })
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(
        searchable_ids.len(),
        post_restart_count,
        "hitsPerPage must expose every post-restart hit: {post_restart_search}"
    );
    for acknowledged_id in &acknowledged_ids {
        assert!(
            searchable_ids.contains(acknowledged_id.as_str()),
            "acknowledged document {acknowledged_id} must remain searchable after restart: {post_restart_search}"
        );
    }

    let final_task_id = server.add_documents_batch(
        index_name,
        json!({
            "requests": [
                {
                    "action": "addObject",
                    "body": {
                        "objectID": "restart-proof-post-check",
                        "title": "Restart proof post-check doc",
                        "token": shared_query_token
                    }
                }
            ]
        }),
    );
    let final_task =
        server.wait_for_task_published(index_name, final_task_id, Duration::from_secs(20));
    assert_eq!(final_task["status"], json!("published"));
    assert_eq!(final_task["pendingTask"], json!(false));

    let final_search = server.search(index_name, json!({ "query": shared_query_token }));
    assert_eq!(
        final_search["nbHits"],
        json!(post_restart_count + 1),
        "server must keep accepting writes after the restart-under-load proof: {final_search}"
    );
}

fn wait_for_acknowledged_docs(
    acknowledged_ids: &Arc<Mutex<Vec<String>>>,
    minimum_count: usize,
    timeout: Duration,
) {
    let started = Instant::now();
    loop {
        let current_count = acknowledged_ids
            .lock()
            .expect("writer ids mutex poisoned")
            .len();
        if current_count >= minimum_count {
            return;
        }
        assert!(
            started.elapsed() <= timeout,
            "timed out waiting for {minimum_count} acknowledged docs; last count={current_count}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn write_single_document_and_wait(
    bind_addr: &str,
    index_name: &str,
    object_id: &str,
    shared_query_token: &str,
) -> Option<()> {
    let payload = json!({
        "requests": [
            {
                "action": "addObject",
                "body": {
                    "objectID": object_id,
                    "title": format!("Restart proof document {}", object_id),
                    "token": shared_query_token,
                }
            }
        ]
    });
    let path = format!("/1/indexes/{index_name}/batch");

    let response = http_request(bind_addr, "POST", &path, Some(&payload.to_string())).ok()?;
    if response.status != 200 && response.status != 202 {
        return None;
    }

    let response_json: Value = serde_json::from_str(&response.body).ok()?;
    let task_id = response_json["taskID"]
        .as_i64()
        .or_else(|| response_json["taskID"].as_u64().map(|value| value as i64))?;

    std::panic::catch_unwind(|| {
        wait_for_task_published_at(bind_addr, index_name, task_id, Duration::from_secs(20))
    })
    .ok()?;

    Some(())
}
