#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

//! Real-server crash/restart durability test for acknowledged batch writes.
mod support;

use serde_json::{json, Value};
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};
use support::{
    http_request_with_headers, http_request_with_read_timeout, HttpResponse, RunningServer, TempDir,
};

const TEST_WRITE_QUEUE_CHANNEL_CAPACITY: usize = 2;
const SERVED_WRITER_CONTENTION_RETRY_WINDOW: Duration = Duration::from_secs(5);
const RESUME_MIGRATION_OWNER_APP_ID: &str = "test-owner";
const RESUME_SOURCE_APP_ID: &str = "LOCALMIGRATIONTEST";
const RESUME_SOURCE_INDEX: &str = "source_products";
const RESUME_TARGET_INDEX: &str = "migration_resume_restart_target";
const INITIAL_RESUME_SOURCE_API_KEY: &str = "initial-source-key";
const FRESH_RESUME_SOURCE_API_KEY: &str = "fresh-resume-key";
const NO_AUTH_TEST_API_KEY: &str = "unused-no-auth-api-key";
const ALGOLIA_TEST_BASE_URL_ENV: &str = "FLAPJACK_TEST_ALGOLIA_BASE_URL";
const RESUME_STATUS_TIMEOUT: Duration = Duration::from_secs(30);

const RESUME_EXPECTED_DOCUMENTS: [(&str, &str, &str); 6] = [
    ("resume-1", "Resume Fixture One", "fixtures"),
    ("resume-2", "Resume Fixture Two", "fixtures"),
    ("resume-3", "Resume Fixture Three", "fixtures"),
    ("resume-4", "Resume Fixture Four", "fixtures"),
    ("resume-5", "Resume Fixture Five", "fixtures"),
    ("resume-6", "Resume Fixture Six", "fixtures"),
];

#[derive(Debug, Default, Clone, Copy)]
struct ResumeSourceSnapshot {
    traversal_starts: usize,
    resumed_page_requests: usize,
    blocked_second_page_started: bool,
    fresh_resume_key_seen: bool,
}

#[derive(Debug, Default)]
struct ResumeSourceState {
    traversal_starts: usize,
    resumed_page_requests: usize,
    blocked_second_page_started: bool,
    fresh_resume_key_seen: bool,
}

struct ResumeSourceFixture {
    bind_addr: String,
    shutdown: Arc<AtomicBool>,
    state: Arc<Mutex<ResumeSourceState>>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ResumeSourceFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local resume source fixture");
        listener
            .set_nonblocking(true)
            .expect("resume source fixture listener should be nonblocking");
        let bind_addr = listener
            .local_addr()
            .expect("resume source fixture bind address")
            .to_string();
        let shutdown = Arc::new(AtomicBool::new(false));
        let state = Arc::new(Mutex::new(ResumeSourceState::default()));
        let shutdown_for_thread = Arc::clone(&shutdown);
        let state_for_thread = Arc::clone(&state);
        let thread = thread::spawn(move || {
            while !shutdown_for_thread.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let shutdown = Arc::clone(&shutdown_for_thread);
                        let state = Arc::clone(&state_for_thread);
                        thread::spawn(move || {
                            handle_resume_source_connection(stream, &shutdown, &state)
                        });
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("resume source fixture accept failed: {error}"),
                }
            }
        });
        Self {
            bind_addr,
            shutdown,
            state,
            thread: Some(thread),
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.bind_addr)
    }

    fn assert_reachable(&self) {
        let response = http_request_with_headers(
            &self.bind_addr,
            "GET",
            "/1/indexes",
            &[
                ("x-algolia-application-id", RESUME_SOURCE_APP_ID),
                ("x-algolia-api-key", INITIAL_RESUME_SOURCE_API_KEY),
            ],
            None,
        )
        .expect("resume source fixture reachability probe should receive a response");
        assert_eq!(
            response.status, 200,
            "resume source fixture must be reachable before migration admission: {}",
            response.body
        );
    }

    fn snapshot(&self) -> ResumeSourceSnapshot {
        let state = self.state.lock().unwrap();
        ResumeSourceSnapshot {
            traversal_starts: state.traversal_starts,
            resumed_page_requests: state.resumed_page_requests,
            blocked_second_page_started: state.blocked_second_page_started,
            fresh_resume_key_seen: state.fresh_resume_key_seen,
        }
    }
}

impl Drop for ResumeSourceFixture {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.bind_addr);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

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

fn handle_resume_source_connection(
    mut stream: TcpStream,
    shutdown: &Arc<AtomicBool>,
    state: &Arc<Mutex<ResumeSourceState>>,
) {
    stream
        .set_nonblocking(false)
        .expect("accepted resume source connection should use blocking reads");
    let mut reader = BufReader::new(
        stream
            .try_clone()
            .expect("resume source fixture should clone stream"),
    );
    let mut request_line = String::new();
    if reader
        .read_line(&mut request_line)
        .expect("resume source fixture should read request line")
        == 0
    {
        return;
    }

    let mut request_parts = request_line.split_whitespace();
    let method = request_parts
        .next()
        .expect("resume source fixture request method")
        .to_string();
    let request_target = request_parts
        .next()
        .expect("resume source fixture request path")
        .to_string();
    let (path, _query) = request_target
        .split_once('?')
        .map_or((request_target.as_str(), ""), |(path, query)| (path, query));

    let mut headers = std::collections::BTreeMap::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .expect("resume source fixture should read headers");
        if line == "\r\n" {
            break;
        }
        let (name, value) = line
            .trim_end()
            .split_once(':')
            .expect("resume source fixture header should contain colon");
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body_bytes = vec![0; content_length];
    reader
        .read_exact(&mut body_bytes)
        .expect("resume source fixture should read body");
    let body = if body_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&body_bytes)
            .expect("resume source fixture body should be valid JSON")
    };

    let app_id = headers
        .get("x-algolia-application-id")
        .map(String::as_str)
        .unwrap_or_default();
    let api_key = headers
        .get("x-algolia-api-key")
        .map(String::as_str)
        .unwrap_or_default();
    if app_id != RESUME_SOURCE_APP_ID
        || (api_key != INITIAL_RESUME_SOURCE_API_KEY && api_key != FRESH_RESUME_SOURCE_API_KEY)
    {
        write_json_response(
            &mut stream,
            403,
            json!({"message": "unexpected source credentials"}),
        );
        return;
    }

    if api_key == FRESH_RESUME_SOURCE_API_KEY {
        state.lock().unwrap().fresh_resume_key_seen = true;
    }

    match (method.as_str(), path) {
        ("GET", "/1/indexes") => {
            write_json_response(
                &mut stream,
                200,
                json!({
                    "items": [{
                        "name": RESUME_SOURCE_INDEX,
                        "entries": RESUME_EXPECTED_DOCUMENTS.len(),
                        "updatedAt": "2026-07-31T00:00:00Z",
                        "pendingTask": false
                    }],
                    "page": 0,
                    "nbPages": 1
                }),
            );
        }
        ("GET", "/1/indexes/source_products/settings") => {
            write_json_response(
                &mut stream,
                200,
                json!({
                    "searchableAttributes": ["title"],
                    "attributesForFaceting": ["category"]
                }),
            );
        }
        ("POST", "/1/indexes/source_products/rules/search")
        | ("POST", "/1/indexes/source_products/synonyms/search") => {
            write_json_response(
                &mut stream,
                200,
                json!({
                    "hits": [],
                    "page": 0,
                    "nbPages": 0
                }),
            );
        }
        ("POST", "/1/indexes/source_products/browse") => {
            let cursor = body.get("cursor").and_then(Value::as_str);
            match cursor {
                None => {
                    let page = {
                        let mut state = state.lock().unwrap();
                        state.traversal_starts += 1;
                        state.traversal_starts
                    };
                    match page {
                        1 => write_json_response(
                            &mut stream,
                            200,
                            json!({
                                "hits": resume_source_documents(0, 3),
                                "cursor": "identity-cursor-3"
                            }),
                        ),
                        2 => {
                            state.lock().unwrap().blocked_second_page_started = false;
                            write_json_response(
                                &mut stream,
                                200,
                                json!({
                                    "hits": resume_source_documents(0, 2),
                                    "cursor": "export-cursor-2-block"
                                }),
                            );
                        }
                        3 => write_json_response(
                            &mut stream,
                            200,
                            json!({
                                "hits": resume_source_documents(0, 3),
                                "cursor": "identity-cursor-3"
                            }),
                        ),
                        4 => {
                            state.lock().unwrap().resumed_page_requests = 1;
                            write_json_response(
                                &mut stream,
                                200,
                                json!({
                                    "hits": resume_source_documents(0, 3),
                                    "cursor": "resume-cursor-3"
                                }),
                            );
                        }
                        other => write_json_response(
                            &mut stream,
                            500,
                            json!({"message": format!("unexpected traversal start {other}")}),
                        ),
                    }
                }
                Some("identity-cursor-3") => {
                    write_json_response(
                        &mut stream,
                        200,
                        json!({
                            "hits": resume_source_documents(3, 2),
                            "cursor": "identity-cursor-5"
                        }),
                    );
                }
                Some("identity-cursor-5") => {
                    write_json_response(
                        &mut stream,
                        200,
                        json!({
                            "hits": resume_source_documents(5, 1)
                        }),
                    );
                }
                Some("export-cursor-2-block") => {
                    state.lock().unwrap().blocked_second_page_started = true;
                    while !shutdown.load(Ordering::SeqCst) {
                        thread::sleep(Duration::from_millis(10));
                    }
                }
                Some("resume-cursor-3") => {
                    state.lock().unwrap().resumed_page_requests += 1;
                    write_json_response(
                        &mut stream,
                        200,
                        json!({
                            "hits": resume_source_documents(3, 2),
                            "cursor": "resume-cursor-5"
                        }),
                    );
                }
                Some("resume-cursor-5") => {
                    state.lock().unwrap().resumed_page_requests += 1;
                    write_json_response(
                        &mut stream,
                        200,
                        json!({
                            "hits": resume_source_documents(5, 1)
                        }),
                    );
                }
                Some(other) => {
                    write_json_response(
                        &mut stream,
                        500,
                        json!({"message": format!("unexpected browse cursor {other}")}),
                    );
                }
            }
        }
        _ => {
            write_json_response(
                &mut stream,
                500,
                json!({"message": "unexpected source request", "method": method, "path": path}),
            );
        }
    }
}

fn write_json_response(stream: &mut TcpStream, status: u16, body: Value) {
    let body = body.to_string();
    let reason = if status < 400 { "OK" } else { "ERR" };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    stream
        .write_all(response.as_bytes())
        .expect("resume source fixture should write response");
}

fn resume_source_documents(start: usize, count: usize) -> Vec<Value> {
    RESUME_EXPECTED_DOCUMENTS[start..start + count]
        .iter()
        .map(|(object_id, title, category)| {
            json!({
                "objectID": object_id,
                "title": title,
                "category": category
            })
        })
        .collect()
}

fn resume_expected_target_ids() -> HashSet<String> {
    RESUME_EXPECTED_DOCUMENTS
        .iter()
        .map(|(object_id, _, _)| object_id.to_string())
        .collect()
}

fn migration_auth_headers(api_key: &str) -> [(&'static str, &str); 2] {
    [
        ("x-algolia-application-id", RESUME_MIGRATION_OWNER_APP_ID),
        ("x-algolia-api-key", api_key),
    ]
}

fn submit_resume_migration(server: &RunningServer, admin_key: &str) -> serde_json::Value {
    let response = http_request_with_headers(
        server.bind_addr(),
        "POST",
        "/1/migrations/algolia",
        &migration_auth_headers(admin_key),
        Some(
            &json!({
                "appId": RESUME_SOURCE_APP_ID,
                "apiKey": INITIAL_RESUME_SOURCE_API_KEY,
                "sourceIndex": RESUME_SOURCE_INDEX,
                "targetIndex": RESUME_TARGET_INDEX
            })
            .to_string(),
        ),
    )
    .expect("async migration submit should receive an HTTP response");
    assert_eq!(
        response.status, 202,
        "resume restart submit must be admitted: {}",
        response.body
    );
    parse_json_response(&response, "resume migration submit")
}

fn get_migration_status(server: &RunningServer, admin_key: &str, job_id: &str) -> Value {
    let response = http_request_with_headers(
        server.bind_addr(),
        "GET",
        &format!("/1/migrations/algolia/{job_id}"),
        &migration_auth_headers(admin_key),
        None,
    )
    .expect("migration status should receive an HTTP response");
    assert_eq!(
        response.status, 200,
        "migration status should return HTTP 200: {}",
        response.body
    );
    parse_json_response(&response, "resume migration status")
}

fn post_resume_migration(server: &RunningServer, admin_key: &str, job_id: &str) -> Value {
    let response = http_request_with_headers(
        server.bind_addr(),
        "POST",
        &format!("/1/migrations/algolia/{job_id}/resume"),
        &migration_auth_headers(admin_key),
        Some(
            &json!({
                "appId": RESUME_SOURCE_APP_ID,
                "apiKey": FRESH_RESUME_SOURCE_API_KEY,
                "sourceIndex": RESUME_SOURCE_INDEX,
                "targetIndex": RESUME_TARGET_INDEX
            })
            .to_string(),
        ),
    )
    .expect("migration resume should receive an HTTP response");
    assert_eq!(
        response.status, 202,
        "resume route must accept an interrupted migration: {}",
        response.body
    );
    parse_json_response(&response, "resume migration admission")
}

fn search_with_auth(
    server: &RunningServer,
    admin_key: &str,
    index_name: &str,
    payload: Value,
) -> Value {
    let response = http_request_with_headers(
        server.bind_addr(),
        "POST",
        &format!("/1/indexes/{index_name}/query"),
        &migration_auth_headers(admin_key),
        Some(&payload.to_string()),
    )
    .expect("auth search should receive an HTTP response");
    assert_eq!(
        response.status, 200,
        "auth search should return HTTP 200: {}",
        response.body
    );
    parse_json_response(&response, "resume target search")
}

fn wait_for_resume_export_pre_crash(
    server: &RunningServer,
    admin_key: &str,
    job_id: &str,
    fixture: &ResumeSourceFixture,
) -> Value {
    let started_at = Instant::now();
    loop {
        let status = get_migration_status(server, admin_key, job_id);
        let source = fixture.snapshot();
        if status["phase"] == json!("exporting")
            && status["disposition"] == json!("running")
            && status["terminalAt"].is_null()
            && status["exportProgress"] == json!({"completed": 2, "total": 6})
            && source.traversal_starts >= 2
            && source.blocked_second_page_started
        {
            return status;
        }
        assert!(
            started_at.elapsed() <= RESUME_STATUS_TIMEOUT,
            "resume pre-crash export should reach 2/6 progress with the second export page blocked; last status={status}, source={source:?}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_interrupted_status_after_restart(
    server: &RunningServer,
    admin_key: &str,
    job_id: &str,
) -> Value {
    let started_at = Instant::now();
    loop {
        let status = get_migration_status(server, admin_key, job_id);
        if status["phase"] == json!("exporting")
            && status["disposition"] == json!("running")
            && status["terminalAt"].is_null()
            && status["exportProgress"] == json!({"completed": 2, "total": 6})
            && status["resumable"] == json!(true)
            && status["operation"] == json!("resume")
            && status["resumeHandle"]
                .as_str()
                .is_some_and(|handle| !handle.is_empty())
        {
            return status;
        }
        assert!(
            started_at.elapsed() <= RESUME_STATUS_TIMEOUT,
            "restart recovery should expose the interrupted positive arm; last status={status}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_for_terminal_resume_success(
    server: &RunningServer,
    admin_key: &str,
    job_id: &str,
) -> Value {
    let started_at = Instant::now();
    loop {
        let status = get_migration_status(server, admin_key, job_id);
        if status["disposition"] == json!("succeeded") {
            assert_eq!(status["phase"], json!("activating"));
            assert!(
                status["terminalAt"].is_string(),
                "terminal success must expose terminalAt: {status}"
            );
            return status;
        }
        assert!(
            started_at.elapsed() <= RESUME_STATUS_TIMEOUT,
            "resume after restart should reach terminal success; last status={status}"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[serial_test::serial(flapjack_server_write_env)]
fn interrupted_async_migration_resumes_exactly_once_after_process_restart() {
    let tmp = TempDir::new("fj_test_migration_resume_restart");
    let source_fixture = ResumeSourceFixture::start();
    source_fixture.assert_reachable();
    let source_base_url = source_fixture.endpoint();
    let env = [(ALGOLIA_TEST_BASE_URL_ENV, source_base_url.as_str())];

    let mut server = RunningServer::spawn_no_auth_auto_port_with_env(tmp.path(), &env);
    let admin_key = NO_AUTH_TEST_API_KEY;

    let submit = submit_resume_migration(&server, admin_key);
    let job_id = submit["jobId"]
        .as_str()
        .expect("resume migration submit must return a durable jobId")
        .to_string();

    let pre_crash = wait_for_resume_export_pre_crash(&server, admin_key, &job_id, &source_fixture);
    assert!(
        pre_crash.get("resumable").is_none(),
        "a live export worker must not advertise resumable status before restart classification: {pre_crash}"
    );

    server.kill_and_restart_no_auth_auto_port_with_env(tmp.path(), &env);

    let recovered = wait_for_interrupted_status_after_restart(&server, admin_key, &job_id);
    let resume_handle = recovered["resumeHandle"]
        .as_str()
        .expect("interrupted status must expose resumeHandle")
        .to_string();

    let resume_admission = post_resume_migration(&server, admin_key, &job_id);
    assert_eq!(resume_admission["jobId"], json!(job_id));
    assert_eq!(resume_admission["disposition"], json!("running"));

    let terminal = wait_for_terminal_resume_success(&server, admin_key, &job_id);
    assert_eq!(terminal["jobId"], json!(job_id));
    assert!(
        terminal.get("resumeHandle").is_none(),
        "terminal success must omit resumeHandle: {terminal}"
    );

    let target_search = search_with_auth(
        &server,
        admin_key,
        RESUME_TARGET_INDEX,
        json!({"query": "", "hitsPerPage": 100}),
    );
    assert_eq!(
        target_search["nbHits"],
        json!(RESUME_EXPECTED_DOCUMENTS.len()),
        "resumed target must contain the exact six expected documents: {target_search}"
    );
    let actual_ids = target_search["hits"]
        .as_array()
        .expect("resumed target search must return hits")
        .iter()
        .map(|hit| {
            hit["objectID"]
                .as_str()
                .unwrap_or_else(|| panic!("resumed target hit must include objectID: {hit}"))
                .to_string()
        })
        .collect::<HashSet<_>>();
    assert_eq!(actual_ids, resume_expected_target_ids());

    let source = source_fixture.snapshot();
    assert_eq!(
        source.traversal_starts, 4,
        "the source fixture must observe initial identity/export and resumed identity/export traversals"
    );
    assert_eq!(
        source.resumed_page_requests, 3,
        "the resumed traversal must serve the three shifted pages"
    );
    assert!(
        source.fresh_resume_key_seen,
        "resume must use fresh source credentials on the second traversal"
    );
    assert!(
        !resume_handle.is_empty(),
        "interrupted status must expose a non-empty opaque resume handle"
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
