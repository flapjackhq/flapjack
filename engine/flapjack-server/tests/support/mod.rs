#![allow(dead_code)]

use assert_cmd::Command;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const AUTO_PORT_STARTUP_TIMEOUT: Duration = Duration::from_secs(20);
const AUTO_PORT_HEALTH_TIMEOUT: Duration = Duration::from_secs(30);
const FLAPJACK_AMBIENT_ENV_VARS: [&str; 16] = [
    "FLAPJACK_ADMIN_KEY",
    "FLAPJACK_NO_AUTH",
    "FLAPJACK_ENV",
    "FLAPJACK_BIND_ADDR",
    "FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND",
    "FLAPJACK_PORT",
    "FLAPJACK_DATA_DIR",
    "FLAPJACK_IDEMPOTENCY_TTL_SECS",
    "FLAPJACK_IDEMPOTENCY_PERSISTENT",
    "FLAPJACK_IDEMPOTENCY_PERSIST",
    "FLAPJACK_MAX_CONCURRENT_WRITERS",
    "FLAPJACK_WRITE_DURABLE_TIMEOUT_MS",
    "FLAPJACK_WRITE_QUEUE_BATCH_SIZE",
    "FLAPJACK_WRITE_QUEUE_WRITER_ACQUIRE_TIMEOUT_MS",
    "FLAPJACK_WRITE_QUEUE_CHANNEL_CAPACITY",
    "FLAPJACK_WRITE_QUEUE_START_DELAY_MS",
];

pub(crate) fn flapjack_cmd() -> Command {
    let mut command = Command::cargo_bin("flapjack").unwrap();
    strip_flapjack_ambient_env_from_assert_cmd(&mut command);
    command
}

pub(crate) fn flapjack_cmd_executable() -> PathBuf {
    flapjack_cmd().get_program().into()
}

pub(crate) fn server_spawn_executable() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_flapjack"))
}

fn with_each_flapjack_ambient_env_var(mut apply: impl FnMut(&str)) {
    for env_var in FLAPJACK_AMBIENT_ENV_VARS {
        apply(env_var);
    }
}

fn strip_flapjack_ambient_env_from_assert_cmd(command: &mut Command) {
    with_each_flapjack_ambient_env_var(|env_var| {
        command.env_remove(env_var);
    });
}

fn strip_flapjack_ambient_env_from_process_command(command: &mut std::process::Command) {
    with_each_flapjack_ambient_env_var(|env_var| {
        command.env_remove(env_var);
    });
}

#[cfg(test)]
mod tests {
    use super::{
        classify_task_poll_response, is_transient_task_poll_transport_error,
        with_each_flapjack_ambient_env_var, TaskPollOutcome, FLAPJACK_AMBIENT_ENV_VARS,
    };
    use serde_json::json;

    #[test]
    fn with_each_flapjack_ambient_env_var_visits_each_env_var_once_in_order() {
        let mut seen = Vec::new();
        with_each_flapjack_ambient_env_var(|env_var| seen.push(env_var.to_string()));

        assert_eq!(
            seen,
            FLAPJACK_AMBIENT_ENV_VARS
                .iter()
                .map(|env_var| env_var.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn classify_task_poll_response_reports_published_tasks() {
        let outcome = classify_task_poll_response(
            7,
            &json!({
                "status": "published",
                "pendingTask": false
            }),
        );

        assert_eq!(outcome, TaskPollOutcome::Published);
    }

    #[test]
    fn classify_task_poll_response_keeps_pending_tasks_polling() {
        let outcome = classify_task_poll_response(
            7,
            &json!({
                "status": "processing",
                "pendingTask": true
            }),
        );

        assert_eq!(outcome, TaskPollOutcome::Pending);
    }

    #[test]
    fn classify_task_poll_response_surfaces_terminal_failures() {
        let outcome = classify_task_poll_response(
            7,
            &json!({
                "status": "failed",
                "pendingTask": false,
                "error": "disk full"
            }),
        );

        match outcome {
            TaskPollOutcome::TerminalFailure(message) => {
                assert!(message.contains("failed"));
                assert!(message.contains("disk full"));
            }
            other => panic!("expected terminal failure outcome, got {other:?}"),
        }
    }

    #[test]
    fn classify_task_poll_response_rejects_malformed_payloads() {
        let outcome = classify_task_poll_response(
            7,
            &json!({
                "error": "missing task fields"
            }),
        );

        match outcome {
            TaskPollOutcome::TerminalFailure(message) => {
                assert!(message.contains("malformed task payload"));
                assert!(message.contains("missing task fields"));
            }
            other => panic!("expected malformed payload failure, got {other:?}"),
        }
    }

    #[test]
    fn transient_task_poll_transport_errors_are_retryable() {
        assert!(is_transient_task_poll_transport_error(
            "failed reading response from 127.0.0.1:44051: Resource temporarily unavailable (os error 11)"
        ));
        assert!(is_transient_task_poll_transport_error(
            "failed to connect to 127.0.0.1:44051: Connection refused (os error 61)"
        ));
        assert!(!is_transient_task_poll_transport_error(
            "invalid HTTP response from 127.0.0.1: garbage"
        ));
    }
}

pub(crate) struct TempDir(std::path::PathBuf);

impl TempDir {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("{}_{}", name, unique_suffix()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    pub(crate) fn path(&self) -> &str {
        self.0.to_str().unwrap()
    }

    pub(crate) fn root(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock should be after unix epoch")
        .as_nanos();
    format!("{}_{}", std::process::id(), nanos)
}

pub(crate) struct RunningServer {
    child: Child,
    bind_addr: String,
}

#[derive(Debug, PartialEq, Eq)]
enum TaskPollOutcome {
    Pending,
    Published,
    TerminalFailure(String),
}

impl RunningServer {
    pub(crate) fn spawn_no_auth_auto_port(data_dir: &str) -> Self {
        Self::spawn_auto_port(data_dir, true)
    }

    pub(crate) fn spawn_no_auth_fixed_port(data_dir: &str) -> Self {
        let bind_addr = allocate_ephemeral_bind_addr();
        Self::spawn_fixed_bind_addr(data_dir, true, &bind_addr)
    }

    pub(crate) fn spawn_auth_auto_port(data_dir: &str) -> Self {
        Self::spawn_auto_port(data_dir, false)
    }

    pub(crate) fn spawn_no_auth_auto_port_with_persistent_idempotency(
        data_dir: &str,
        ttl: Duration,
    ) -> Self {
        let env = persistent_idempotency_env(ttl);
        let (child, bind_addr) = Self::start_auto_port_process_with_env(data_dir, true, &env);
        Self { child, bind_addr }
    }

    pub(crate) fn spawn_no_auth_auto_port_with_env(
        data_dir: &str,
        extra_env: &[(&str, &str)],
    ) -> Self {
        let (child, bind_addr) = Self::start_auto_port_process_with_env(data_dir, true, extra_env);
        Self { child, bind_addr }
    }

    fn spawn_auto_port(data_dir: &str, no_auth: bool) -> Self {
        let (child, bind_addr) = Self::start_auto_port_process(data_dir, no_auth);
        Self { child, bind_addr }
    }

    fn spawn_fixed_bind_addr(data_dir: &str, no_auth: bool, bind_addr: &str) -> Self {
        let child = Self::start_fixed_bind_addr_process(data_dir, no_auth, bind_addr);
        Self {
            child,
            bind_addr: bind_addr.to_string(),
        }
    }

    fn start_auto_port_process(data_dir: &str, no_auth: bool) -> (Child, String) {
        let mut child =
            spawn_flapjack_auto_port_process(data_dir, no_auth, Stdio::piped(), Stdio::piped());

        let bind_addr = wait_for_startup_bind_addr(&mut child, AUTO_PORT_STARTUP_TIMEOUT);
        wait_for_health(&bind_addr, AUTO_PORT_HEALTH_TIMEOUT);

        (child, bind_addr)
    }

    fn start_auto_port_process_with_env(
        data_dir: &str,
        no_auth: bool,
        extra_env: &[(&str, &str)],
    ) -> (Child, String) {
        let mut child = spawn_flapjack_auto_port_process_with_env(
            data_dir,
            no_auth,
            extra_env,
            Stdio::piped(),
            Stdio::piped(),
        );
        let bind_addr = wait_for_startup_bind_addr(&mut child, AUTO_PORT_STARTUP_TIMEOUT);
        wait_for_health(&bind_addr, AUTO_PORT_HEALTH_TIMEOUT);
        (child, bind_addr)
    }

    fn start_fixed_bind_addr_process(data_dir: &str, no_auth: bool, bind_addr: &str) -> Child {
        let child = spawn_flapjack_process(
            data_dir,
            no_auth,
            Some(bind_addr),
            Stdio::null(),
            Stdio::null(),
        );
        wait_for_health(bind_addr, AUTO_PORT_HEALTH_TIMEOUT);
        child
    }

    pub(crate) fn bind_addr(&self) -> &str {
        &self.bind_addr
    }

    pub(crate) fn add_documents_batch(&self, index_name: &str, payload: Value) -> i64 {
        add_documents_batch_at(self.bind_addr(), index_name, payload)
    }

    pub(crate) fn wait_for_task_published(
        &self,
        index_name: &str,
        task_id: i64,
        timeout: Duration,
    ) -> Value {
        wait_for_task_published_at(self.bind_addr(), index_name, task_id, timeout)
    }

    pub(crate) fn search(&self, index_name: &str, payload: Value) -> Value {
        let path = format!("/1/indexes/{index_name}/query");
        let response = http_json_request(self.bind_addr(), "POST", &path, &payload);
        assert_eq!(
            response.status, 200,
            "search should return HTTP 200, got {} with body {}",
            response.status, response.body
        );
        response.body
    }

    pub(crate) fn kill_and_restart_no_auth_auto_port(&mut self, data_dir: &str) {
        self.kill_child();
        let (child, bind_addr) = Self::start_auto_port_process(data_dir, true);
        self.child = child;
        self.bind_addr = bind_addr;
    }

    pub(crate) fn kill_and_restart_no_auth_auto_port_with_env(
        &mut self,
        data_dir: &str,
        extra_env: &[(&str, &str)],
    ) {
        self.kill_child();
        let (child, bind_addr) = Self::start_auto_port_process_with_env(data_dir, true, extra_env);
        self.child = child;
        self.bind_addr = bind_addr;
    }

    pub(crate) fn kill_and_restart_no_auth_auto_port_with_persistent_idempotency(
        &mut self,
        data_dir: &str,
        ttl: Duration,
    ) {
        self.kill_child();
        let env = persistent_idempotency_env(ttl);
        let (child, bind_addr) = Self::start_auto_port_process_with_env(data_dir, true, &env);
        self.child = child;
        self.bind_addr = bind_addr;
    }

    pub(crate) fn kill_and_restart_no_auth_same_bind_addr(&mut self, data_dir: &str) {
        self.kill_child();
        // Reusing the same bind address keeps background writers pointed at a stable
        // endpoint while the test proves restart behavior under active traffic.
        thread::sleep(Duration::from_millis(100));
        self.child = Self::start_fixed_bind_addr_process(data_dir, true, &self.bind_addr);
    }

    fn kill_child(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        self.kill_child();
    }
}

/// Boot the auth-enabled server on an auto-assigned port, wait until it is
/// serving `/health`, then stop it. Extra env vars let startup-path tests
/// exercise one-shot boot behavior without relying on fixed sleep/timeout
/// windows.
pub(crate) fn run_auth_auto_port_startup_once(data_dir: &str, extra_env: &[(&str, &str)]) {
    let mut child = spawn_flapjack_auto_port_process_with_env(
        data_dir,
        false,
        extra_env,
        Stdio::piped(),
        Stdio::piped(),
    );
    let bind_addr = wait_for_startup_bind_addr(&mut child, AUTO_PORT_STARTUP_TIMEOUT);
    wait_for_health(&bind_addr, AUTO_PORT_HEALTH_TIMEOUT);
    let _ = child.kill();
    let _ = child.wait();
}

pub(crate) struct HttpResponse {
    pub(crate) status: u16,
    pub(crate) headers: HashMap<String, String>,
    pub(crate) body: String,
}

pub(crate) fn spawn_flapjack_auto_port_process(
    data_dir: &str,
    no_auth: bool,
    stdout: Stdio,
    stderr: Stdio,
) -> Child {
    spawn_flapjack_auto_port_process_with_env(data_dir, no_auth, &[], stdout, stderr)
}

fn spawn_flapjack_auto_port_process_with_env(
    data_dir: &str,
    no_auth: bool,
    extra_env: &[(&str, &str)],
    stdout: Stdio,
    stderr: Stdio,
) -> Child {
    spawn_flapjack_process_with_env(data_dir, no_auth, None, extra_env, stdout, stderr)
}

pub(crate) fn add_documents_batch_at(bind_addr: &str, index_name: &str, payload: Value) -> i64 {
    let path = format!("/1/indexes/{index_name}/batch");
    let response = http_json_request(bind_addr, "POST", &path, &payload);
    let status_ok = response.status == 200 || response.status == 202;
    assert!(
        status_ok,
        "batch write should succeed, got status {}, body: {}",
        response.status, response.body
    );

    extract_task_id_from_body(&response.body)
}

pub(crate) fn wait_for_task_published_at(
    bind_addr: &str,
    index_name: &str,
    task_id: i64,
    timeout: Duration,
) -> Value {
    let path = format!("/1/indexes/{index_name}/task/{task_id}");
    let started_at = Instant::now();
    let mut last_body = Value::Null;

    loop {
        assert!(
            started_at.elapsed() <= timeout,
            "timed out waiting for task {task_id} to publish after {:?}, last body: {}",
            timeout,
            last_body
        );

        let response = match http_request(bind_addr, "GET", &path, None) {
            Ok(response) => response,
            // Crash/restart tests can briefly observe socket-level EAGAIN/timeout/refused
            // while the task-polling loop is still within its overall retry window.
            Err(error) if is_transient_task_poll_transport_error(&error) => {
                last_body = serde_json::json!({ "transientTransportError": error });
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(error) => panic!("task polling request should succeed: {error}"),
        };
        assert_eq!(
            response.status, 200,
            "task polling should return HTTP 200, got {} for task {} with body {}",
            response.status, task_id, response.body
        );

        let body: Value = serde_json::from_str(&response.body).unwrap_or_else(|error| {
            panic!(
                "task response must be valid JSON for task {}: {} ({})",
                task_id, response.body, error
            )
        });
        match classify_task_poll_response(task_id, &body) {
            TaskPollOutcome::Published => return body,
            TaskPollOutcome::Pending => last_body = body,
            TaskPollOutcome::TerminalFailure(message) => {
                panic!("task {task_id} reached terminal failure state: {message}. body: {body}");
            }
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn spawn_flapjack_process(
    data_dir: &str,
    no_auth: bool,
    bind_addr: Option<&str>,
    stdout: Stdio,
    stderr: Stdio,
) -> Child {
    spawn_flapjack_process_with_env(data_dir, no_auth, bind_addr, &[], stdout, stderr)
}

fn spawn_flapjack_process_with_env(
    data_dir: &str,
    no_auth: bool,
    bind_addr: Option<&str>,
    extra_env: &[(&str, &str)],
    stdout: Stdio,
    stderr: Stdio,
) -> Child {
    let mut command = std::process::Command::new(server_spawn_executable());
    if no_auth {
        command.arg("--no-auth");
    }
    strip_flapjack_ambient_env_from_process_command(&mut command);
    if let Some(bind_addr) = bind_addr {
        command.arg("--bind-addr").arg(bind_addr);
    } else {
        command.arg("--auto-port");
    }
    command
        .arg("--data-dir")
        .arg(data_dir)
        .envs(extra_env.iter().copied())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("failed to spawn flapjack process")
}

fn allocate_ephemeral_bind_addr() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("failed to reserve local test port");
    let bind_addr = listener
        .local_addr()
        .expect("reserved local test port must expose an address");
    format!("127.0.0.1:{}", bind_addr.port())
}

fn wait_for_startup_bind_addr(child: &mut Child, timeout: Duration) -> String {
    let stdout = child
        .stdout
        .take()
        .expect("child stdout should be piped for startup capture");
    let stderr = child
        .stderr
        .take()
        .expect("child stderr should be piped for startup capture");

    let (tx, rx) = mpsc::channel::<String>();
    spawn_pipe_reader(stdout, tx.clone());
    spawn_pipe_reader(stderr, tx);

    let start = Instant::now();
    let mut observed = Vec::new();
    loop {
        if let Some(status) = child
            .try_wait()
            .expect("failed checking flapjack child process status")
        {
            panic!(
                "flapjack exited before startup banner ({status}). output:\n{}",
                observed.join("\n")
            );
        }

        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for startup banner after {:?}. output:\n{}",
                timeout,
                observed.join("\n")
            );
        }

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                let clean = strip_ansi(&line);
                observed.push(clean.clone());
                if let Some(bind_addr) = extract_bind_addr_from_banner_line(&clean) {
                    return bind_addr;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "startup output stream closed before bind address was observed. output:\n{}",
                    observed.join("\n")
                );
            }
        }
    }
}

fn spawn_pipe_reader<R: Read + Send + 'static>(reader: R, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                    let _ = tx.send(trimmed);
                }
                Err(_) => break,
            }
        }
    });
}

fn extract_bind_addr_from_banner_line(line: &str) -> Option<String> {
    let marker = "http://127.0.0.1:";
    let start = line.find(marker)?;
    let candidate = &line[start + "http://".len()..];
    let end = candidate
        .find(char::is_whitespace)
        .unwrap_or(candidate.len());
    let bind_addr = candidate[..end].trim_end_matches('/');
    if bind_addr.starts_with("127.0.0.1:") {
        Some(bind_addr.to_string())
    } else {
        None
    }
}

fn wait_for_health(bind_addr: &str, timeout: Duration) {
    let start = Instant::now();
    loop {
        if start.elapsed() > timeout {
            panic!(
                "timed out waiting for /health on {} after {:?}",
                bind_addr, timeout
            );
        }

        if let Ok(response) = http_request(bind_addr, "GET", "/health", None) {
            if response.status == 200 && response.body.contains("\"status\":\"ok\"") {
                return;
            }
        }

        thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) fn http_request(
    bind_addr: &str,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> Result<HttpResponse, String> {
    http_request_with_headers(bind_addr, method, path, &[], body)
}

pub(crate) fn http_request_with_read_timeout(
    bind_addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    read_timeout: Duration,
) -> Result<HttpResponse, String> {
    http_request_with_headers_and_read_timeout(bind_addr, method, path, headers, body, read_timeout)
}

pub(crate) fn open_http_request_with_read_timeout(
    bind_addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    read_timeout: Duration,
) -> Result<TcpStream, String> {
    let body = body.unwrap_or("");
    let mut stream = TcpStream::connect(bind_addr)
        .map_err(|e| format!("failed to connect to {}: {}", bind_addr, e))?;
    stream
        .set_read_timeout(Some(read_timeout))
        .map_err(|e| format!("failed setting read timeout: {}", e))?;
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| format!("failed setting write timeout: {}", e))?;
    write_http_request(&mut stream, bind_addr, method, path, headers, body)?;
    Ok(stream)
}

fn persistent_idempotency_env(ttl: Duration) -> [(&'static str, &'static str); 2] {
    let ttl_secs = ttl.as_secs().max(1);
    let ttl = Box::leak(ttl_secs.to_string().into_boxed_str());
    [
        ("FLAPJACK_IDEMPOTENCY_PERSISTENT", "true"),
        ("FLAPJACK_IDEMPOTENCY_TTL_SECS", ttl),
    ]
}

fn http_json_request(bind_addr: &str, method: &str, path: &str, body: &Value) -> JsonResponse {
    let response = http_request_with_headers(bind_addr, method, path, &[], Some(&body.to_string()))
        .unwrap_or_else(|error| panic!("HTTP request should succeed for {method} {path}: {error}"));
    let parsed_body = serde_json::from_str::<Value>(&response.body).unwrap_or_else(|error| {
        panic!(
            "response body for {method} {path} must be valid JSON: {} ({error})",
            response.body
        )
    });

    JsonResponse {
        status: response.status,
        body: parsed_body,
    }
}

fn extract_task_id_from_body(body: &Value) -> i64 {
    body["taskID"]
        .as_i64()
        .or_else(|| body["taskID"].as_u64().map(|value| value as i64))
        .unwrap_or_else(|| panic!("batch response must include numeric taskID: {body}"))
}

fn classify_task_poll_response(task_id: i64, body: &Value) -> TaskPollOutcome {
    let status = body["status"].as_str();
    let pending_task = body["pendingTask"].as_bool();

    if status == Some("published") && pending_task == Some(false) {
        return TaskPollOutcome::Published;
    }

    if status.is_none() || pending_task.is_none() {
        return TaskPollOutcome::TerminalFailure(format!(
            "task {task_id} returned malformed task payload: {body}"
        ));
    }

    let terminal_status = matches!(status, Some("failed" | "canceled" | "cancelled"));
    if terminal_status || (pending_task == Some(false) && status != Some("published")) {
        let error = body
            .get("error")
            .map(Value::to_string)
            .unwrap_or_else(|| "no error payload".to_string());
        return TaskPollOutcome::TerminalFailure(format!(
            "task {task_id} returned status {:?} with pendingTask {:?} and error {}",
            status, pending_task, error
        ));
    }

    TaskPollOutcome::Pending
}

fn is_transient_task_poll_transport_error(error: &str) -> bool {
    error.contains("Resource temporarily unavailable")
        || error.contains("timed out")
        || error.contains("Connection refused")
        || error.contains("Connection reset")
}

struct JsonResponse {
    status: u16,
    body: Value,
}

pub(crate) fn http_request_with_headers(
    bind_addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
) -> Result<HttpResponse, String> {
    http_request_with_headers_and_read_timeout(
        bind_addr,
        method,
        path,
        headers,
        body,
        Duration::from_secs(2),
    )
}

fn http_request_with_headers_and_read_timeout(
    bind_addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Option<&str>,
    read_timeout: Duration,
) -> Result<HttpResponse, String> {
    let body = body.unwrap_or("");
    let mut stream = open_http_request_with_read_timeout(
        bind_addr,
        method,
        path,
        headers,
        Some(body),
        read_timeout,
    )?;

    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|e| format!("failed reading response from {}: {}", bind_addr, e))?;

    let text = String::from_utf8_lossy(&raw);
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| format!("invalid HTTP response from {}: {}", bind_addr, text))?;
    let status_line = head
        .lines()
        .next()
        .ok_or_else(|| format!("missing HTTP status line from {}: {}", bind_addr, head))?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("missing HTTP status code in line: {}", status_line))?
        .parse::<u16>()
        .map_err(|e| format!("invalid HTTP status in '{}': {}", status_line, e))?;
    let headers = head
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();

    Ok(HttpResponse {
        status,
        headers,
        body: payload.to_string(),
    })
}

fn write_http_request(
    stream: &mut TcpStream,
    bind_addr: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Result<(), String> {
    let mut request = format!(
        "{method} {path} HTTP/1.0\r\nHost: {bind_addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (header_name, header_value) in headers {
        request.push_str(header_name);
        request.push_str(": ");
        request.push_str(header_value);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    request.push_str(body);

    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("failed writing request to {}: {}", bind_addr, e))
}

pub(crate) fn admin_auth_headers(api_key: &str) -> [(&'static str, &str); 2] {
    [
        ("x-algolia-application-id", "test-app"),
        ("x-algolia-api-key", api_key),
    ]
}

pub(crate) fn extract_key_from_banner(stdout: &str) -> String {
    for line in stdout.lines() {
        if let Some(position) = line.find("Admin API Key:") {
            let after = &line[position + "Admin API Key:".len()..];
            let cleaned = strip_ansi(after);
            let key = cleaned.trim();
            if !key.is_empty() {
                return key.to_string();
            }
        }
    }
    panic!("Could not extract key from banner:\n{}", stdout);
}

pub(crate) fn extract_admin_key_hash_from_json(json_str: &str) -> String {
    let data: serde_json::Value = serde_json::from_str(json_str).expect("valid JSON");
    data["keys"]
        .as_array()
        .expect("keys array")
        .iter()
        .find(|entry| entry["description"] == "Admin API Key")
        .expect("admin key entry")["hash"]
        .as_str()
        .expect("hash field")
        .to_string()
}

pub(crate) fn admin_entry_exists_in_json(json_str: &str) -> bool {
    let Ok(data) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return false;
    };
    data["keys"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .any(|entry| entry["description"] == "Admin API Key")
        })
        .unwrap_or(false)
}

fn strip_ansi(text: &str) -> String {
    let mut result = String::new();
    let mut chars = text.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '\x1b' {
            while let Some(&next) = chars.peek() {
                chars.next();
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            result.push(character);
        }
    }
    result
}
