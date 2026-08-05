mod support;

use rcgen::{generate_simple_self_signed, CertifiedKey};
use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use support::{flapjack_cmd, flapjack_process_command, TempDir};

struct StartupBanner {
    output: String,
    local_url: String,
}

fn write_test_cert_files(temp_dir: &TempDir) -> (String, String) {
    let CertifiedKey { cert, key_pair } =
        generate_simple_self_signed(vec!["localhost".to_string()])
            .expect("test certificate should generate");
    let cert_path = temp_dir.root().join("cert.pem");
    let key_path = temp_dir.root().join("key.pem");
    std::fs::write(&cert_path, cert.pem()).expect("test cert should write");
    std::fs::write(&key_path, key_pair.serialize_pem()).expect("test key should write");
    (
        cert_path.display().to_string(),
        key_path.display().to_string(),
    )
}

fn reserve_bind_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("test port should reserve");
    listener
        .local_addr()
        .expect("reserved port should have addr")
}

fn spawn_tls_process(
    data_dir: &str,
    cert_path: &str,
    key_path: &str,
    bind_addr: Option<SocketAddr>,
) -> Child {
    let mut command = flapjack_process_command();
    command
        .arg("--no-auth")
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--ssl-cert-path")
        .arg(cert_path)
        .arg("--ssl-key-path")
        .arg(key_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(bind_addr) = bind_addr {
        command.arg("--bind-addr").arg(bind_addr.to_string());
    } else {
        command.arg("--auto-port");
    }
    command.spawn().expect("flapjack process should spawn")
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Option<std::process::ExitStatus> {
    let started_at = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("child status should poll") {
            return Some(status);
        }
        if started_at.elapsed() > timeout {
            return None;
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn strip_ansi(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && matches!(chars.peek(), Some('[')) {
            chars.next();
            for code in chars.by_ref() {
                if code.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

#[test]
#[serial_test::serial(tls_startup_process)]
fn malformed_tls_input_exits_before_banner_and_plaintext_bind() {
    let data_dir = TempDir::new("fj_tls_malformed_data");
    let pem_dir = TempDir::new("fj_tls_malformed_pem");
    let bind_addr = reserve_bind_addr();
    let cert_path = pem_dir.root().join("cert.pem");
    let key_path = pem_dir.root().join("key.pem");
    std::fs::write(
        &cert_path,
        "-----BEGIN CERTIFICATE-----\nnot-base64\n-----END CERTIFICATE-----\n",
    )
    .unwrap();
    std::fs::write(&key_path, "not a private key").unwrap();

    let command_output = flapjack_cmd()
        .arg("--no-auth")
        .arg("--data-dir")
        .arg(data_dir.path())
        .arg("--bind-addr")
        .arg(bind_addr.to_string())
        .arg("--ssl-cert-path")
        .arg(cert_path.display().to_string())
        .arg("--ssl-key-path")
        .arg(key_path.display().to_string())
        .timeout(Duration::from_secs(10))
        .output()
        .expect("malformed TLS command should exit");
    let output = strip_ansi(&format!(
        "{}{}",
        String::from_utf8_lossy(&command_output.stdout),
        String::from_utf8_lossy(&command_output.stderr)
    ));
    assert!(
        !command_output.status.success(),
        "malformed TLS input must exit nonzero; output:\n{output}"
    );
    assert!(
        !output.contains("Local:"),
        "startup banner must not print for malformed TLS input; output:\n{output}"
    );
    assert!(
        TcpStream::connect_timeout(&bind_addr, Duration::from_millis(200)).is_err(),
        "malformed TLS input must not leave a plaintext listener on {bind_addr}; output:\n{output}"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial(tls_startup_process)]
fn tls_server_sigterm_drains_write_queues() {
    let data_dir = TempDir::new("fj_tls_sigterm_data");
    let pem_dir = TempDir::new("fj_tls_sigterm_pem");
    let (cert_path, key_path) = write_test_cert_files(&pem_dir);
    let mut child = spawn_tls_process(data_dir.path(), &cert_path, &key_path, None);
    let (line_tx, line_rx) = mpsc::channel();
    spawn_output_reader(child.stdout.take().unwrap(), line_tx.clone());
    spawn_output_reader(child.stderr.take().unwrap(), line_tx);

    let banner = wait_for_https_banner(&mut child, &line_rx, Duration::from_secs(20));
    assert!(
        banner.local_url.starts_with("https://127.0.0.1:"),
        "TLS startup should print an HTTPS banner; output:\n{}",
        banner.output
    );
    assert_tls_health_and_plaintext_rejection(&banner.local_url);

    send_sigterm(&child);

    let exited = wait_for_exit(&mut child, Duration::from_secs(10))
        .expect("TLS server should exit after SIGTERM");
    let output_after_signal = drain_lines(&line_rx, Duration::from_secs(2));
    assert!(
        exited.success(),
        "TLS server should exit successfully after SIGTERM; output:\n{}{output_after_signal}",
        banner.output
    );
    assert!(
        output_after_signal.contains("All write queues drained before deadline"),
        "SIGTERM path must run the write queue drain; output:\n{}{output_after_signal}",
        banner.output
    );
}

#[cfg(unix)]
fn send_sigterm(child: &Child) {
    // libc::kill is the portable way to send SIGTERM without spawning another
    // process, which can fail under integration-test process pressure.
    let result = unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    assert_eq!(result, 0, "SIGTERM should send to test child");
}

fn assert_tls_health_and_plaintext_rejection(local_url: &str) {
    let client = reqwest::blocking::Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(3))
        .build()
        .expect("TLS test client should build");
    let tls_response = client
        .get(format!("{local_url}/health"))
        .send()
        .expect("configured listener should complete a TLS request");
    assert_eq!(tls_response.status(), reqwest::StatusCode::OK);
    let health: serde_json::Value = tls_response
        .json()
        .expect("TLS health response should be JSON");
    assert_eq!(health["status"], "ok");

    // Probe a route the application router actually serves. `/` is unrouted, so
    // asserting 404 there passes whether or not the plaintext ACME gate exists —
    // a vacuous guard. `/health` answers 200 over TLS above, so a 404 here can
    // only come from the gate refusing to serve application routes in cleartext.
    let plaintext_url = local_url.replacen("https://", "http://", 1);
    let plaintext_response = client
        .get(format!("{plaintext_url}/health"))
        .send()
        .expect("plaintext challenge gate should reject non-ACME HTTP requests");
    assert_eq!(
        plaintext_response.status(),
        reqwest::StatusCode::NOT_FOUND,
        "application routes must not be served over plaintext on the TLS listener"
    );
}

fn spawn_output_reader<R: std::io::Read + Send + 'static>(reader: R, tx: mpsc::Sender<String>) {
    thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) => break,
                Ok(_) => {
                    let _ = tx.send(strip_ansi(line.trim_end_matches(['\r', '\n'])));
                }
                Err(_) => break,
            }
        }
    });
}

fn wait_for_https_banner(
    child: &mut Child,
    rx: &mpsc::Receiver<String>,
    timeout: Duration,
) -> StartupBanner {
    let started_at = Instant::now();
    let mut output = String::new();
    loop {
        if let Some(status) = child.try_wait().expect("child status should poll") {
            panic!("flapjack exited before TLS startup banner ({status}); output:\n{output}");
        }
        assert!(
            started_at.elapsed() <= timeout,
            "timed out waiting for TLS startup banner; output:\n{output}"
        );
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if line.contains("Local:") {
                    let local_url = line
                        .split_whitespace()
                        .find(|field| field.starts_with("https://"))
                        .expect("Local banner line should contain an HTTPS URL")
                        .to_string();
                    return StartupBanner { output, local_url };
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                panic!("startup output closed before TLS banner; output:\n{output}");
            }
        }
    }
}

fn drain_lines(rx: &mpsc::Receiver<String>, timeout: Duration) -> String {
    let started_at = Instant::now();
    let mut output = String::new();
    while started_at.elapsed() <= timeout {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                output.push_str(&line);
                output.push('\n');
                if line.contains("All write queues drained before deadline") {
                    return output;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return output,
        }
    }
    output
}
