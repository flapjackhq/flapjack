use arrow::array::{Array, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::{json, Value};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};
use tempfile::TempDir;

const ADMIN_KEY: &str = "stage3-admin-key-123456789";
const APPLICATION_ID: &str = "stage3-test-app";
const INDEX_NAME: &str = "analytics-ip-probe";
const FULL_CLIENT_IP: &str = "203.0.113.47";
const MINIMIZED_CLIENT_IP: &str = "203.0.113.0";

struct ChildGuard {
    child: Child,
    logs: Arc<Mutex<Vec<String>>>,
}

impl ChildGuard {
    fn accumulated_log(&self) -> String {
        self.logs.lock().unwrap().join("\n")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn spawn_log_reader<R: Read + Send + 'static>(
    reader: R,
    startup_tx: mpsc::SyncSender<String>,
    logs: Arc<Mutex<Vec<String>>>,
) {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            let Ok(line) = line else {
                break;
            };
            logs.lock().unwrap().push(line.clone());
            let _ = startup_tx.try_send(line);
        }
    });
}

fn local_url_from_banner(line: &str) -> Option<String> {
    let marker = "http://127.0.0.1:";
    let start = line.find(marker)?;
    let url: String = line[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || matches!(ch, 'h' | 't' | 'p' | ':' | '/' | '.'))
        .collect();
    (url.len() > marker.len()).then_some(url)
}

fn authenticated(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request
        .header("x-algolia-application-id", APPLICATION_ID)
        .header("x-algolia-api-key", ADMIN_KEY)
}

fn collect_search_parquet_files(root: &Path, output: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", root.display()));
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_search_parquet_files(&path, output);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("searches_") && name.ends_with(".parquet"))
        {
            output.push(path);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn served_search_persists_only_minimized_client_ip() {
    let executable = std::env::var_os("FLAPJACK_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("target/debug/flapjack"));
    assert!(
        executable.is_file(),
        "flapjack executable must exist at {}; build flapjack-server first",
        executable.display()
    );

    let data_dir = TempDir::new().unwrap();
    let mut command = Command::new(&executable);
    // Child startup is an allowlist: operator FLAPJACK_* settings must not
    // enable replication, bootstrap, TLS, or alternate storage in this probe.
    command.env_clear();
    let mut child = command
        .arg("--auto-port")
        .env("FLAPJACK_ADMIN_KEY", ADMIN_KEY)
        .env("FLAPJACK_DATA_DIR", data_dir.path())
        .env("FLAPJACK_ANALYTICS_ENABLED", "true")
        .env("FLAPJACK_TRUSTED_PROXY_CIDRS", "127.0.0.1/32")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|error| panic!("failed to spawn {}: {error}", executable.display()));

    let stdout = child.stdout.take().expect("child stdout must be piped");
    let stderr = child.stderr.take().expect("child stderr must be piped");
    let logs = Arc::new(Mutex::new(Vec::new()));
    let (startup_tx, startup_rx) = mpsc::sync_channel(256);
    spawn_log_reader(stdout, startup_tx.clone(), Arc::clone(&logs));
    spawn_log_reader(stderr, startup_tx, Arc::clone(&logs));
    let mut server = ChildGuard { child, logs };

    let startup_deadline = Instant::now() + Duration::from_secs(30);
    let base_url = loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            panic!(
                "flapjack exited before startup with {status}. child log:\n{}",
                server.accumulated_log()
            );
        }
        assert!(
            Instant::now() < startup_deadline,
            "timed out waiting for Local banner. child log:\n{}",
            server.accumulated_log()
        );
        match startup_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                if let Some(url) = local_url_from_banner(&line) {
                    break url;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "startup log streams closed before Local banner. child log:\n{}",
                    server.accumulated_log()
                );
            }
        }
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let health_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(response) = client.get(format!("{base_url}/health")).send().await {
            if response.status() == reqwest::StatusCode::OK {
                let body: Value = response.json().await.unwrap();
                assert_eq!(body["status"], "ok", "unexpected health body: {body}");
                break;
            }
        }
        assert!(
            Instant::now() < health_deadline,
            "timed out waiting for /health. child log:\n{}",
            server.accumulated_log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let write_response = authenticated(
        client
            .post(format!("{base_url}/1/indexes/{INDEX_NAME}"))
            .json(&json!({
                "objectID": "privacy-probe-document",
                "title": "privacy-probe-title"
            })),
    )
    .send()
    .await
    .unwrap();
    let write_status = write_response.status();
    let write_body: Value = write_response.json().await.unwrap();
    assert_eq!(
        write_status,
        reqwest::StatusCode::CREATED,
        "unexpected write response: {write_body}"
    );
    let object_id = write_body["objectID"]
        .as_str()
        .expect("write response must include objectID")
        .to_string();
    let task_id = write_body["taskID"]
        .as_i64()
        .expect("write response must include numeric taskID");

    let task_deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let task_response = authenticated(client.get(format!("{base_url}/1/tasks/{task_id}")))
            .send()
            .await
            .unwrap();
        let task_status = task_response.status();
        let task_body: Value = task_response.json().await.unwrap();
        assert_eq!(
            task_status,
            reqwest::StatusCode::OK,
            "unexpected task response: {task_body}"
        );
        if task_body == json!({"status": "published", "pendingTask": false}) {
            break;
        }
        assert_eq!(
            task_body,
            json!({"status": "notPublished", "pendingTask": true}),
            "unexpected task payload"
        );
        assert!(
            Instant::now() < task_deadline,
            "task {task_id} did not publish before deadline"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let query_response = authenticated(
        client
            .post(format!("{base_url}/1/indexes/{INDEX_NAME}/query"))
            .header("x-forwarded-for", FULL_CLIENT_IP)
            .json(&json!({"query": "privacy-probe-title"})),
    )
    .send()
    .await
    .unwrap();
    let query_status = query_response.status();
    let query_body: Value = query_response.json().await.unwrap();
    assert_eq!(
        query_status,
        reqwest::StatusCode::OK,
        "unexpected search response: {query_body}"
    );
    assert_eq!(
        query_body["nbHits"], 1,
        "unexpected search result: {query_body}"
    );
    assert_eq!(
        query_body["hits"][0]["objectID"], object_id,
        "search must return the document written by this test"
    );

    let flush_response = authenticated(client.post(format!("{base_url}/2/analytics/flush")))
        .send()
        .await
        .unwrap();
    let flush_status = flush_response.status();
    let flush_body: Value = flush_response.json().await.unwrap();
    assert_eq!(
        flush_status,
        reqwest::StatusCode::OK,
        "unexpected analytics flush response: {flush_body}"
    );
    assert_eq!(
        flush_body,
        json!({"status": "ok"}),
        "analytics flush must not report an uninitialized collector"
    );

    let analytics_dir = data_dir.path().join("analytics");
    let mut parquet_files = Vec::new();
    if analytics_dir.exists() {
        collect_search_parquet_files(&analytics_dir, &mut parquet_files);
    }
    assert!(
        !parquet_files.is_empty(),
        "INDETERMINATE: no searches_*.parquet files found below {}. child log:\n{}",
        analytics_dir.display(),
        server.accumulated_log()
    );
    parquet_files.sort();

    let mut row_denominator = 0usize;
    let mut persisted_ips = Vec::new();
    for parquet_file in &parquet_files {
        let reader = ParquetRecordBatchReaderBuilder::try_new(File::open(parquet_file).unwrap())
            .unwrap()
            .build()
            .unwrap();
        for batch in reader {
            let batch = batch.unwrap();
            let user_ip_index = batch.schema().index_of("user_ip").unwrap();
            let user_ips = batch
                .column(user_ip_index)
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("user_ip must remain nullable Utf8");
            row_denominator += batch.num_rows();
            for row in 0..batch.num_rows() {
                if !user_ips.is_null(row) {
                    persisted_ips.push(user_ips.value(row).to_string());
                }
            }
        }
    }

    assert!(
        row_denominator >= 1,
        "INDETERMINATE: decoded analytics files contained zero rows: {parquet_files:?}"
    );
    assert!(
        persisted_ips.iter().any(|ip| ip == MINIMIZED_CLIENT_IP),
        "privacy regression: decoded user_ip values {persisted_ips:?}; expected minimized value \
         {MINIMIZED_CLIENT_IP}; full client IP present={}",
        persisted_ips.iter().any(|ip| ip == FULL_CLIENT_IP)
    );
    assert!(
        !persisted_ips.iter().any(|ip| ip == FULL_CLIENT_IP),
        "privacy regression: decoded Parquet contains full client IP {FULL_CLIENT_IP}: \
         {persisted_ips:?}"
    );
}
