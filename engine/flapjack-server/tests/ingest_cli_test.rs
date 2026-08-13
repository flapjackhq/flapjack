#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

mod support;

use serde_json::{json, Value};
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use support::{flapjack_cmd, RunningServer, TempDir};

const API_KEY: &str = "fj_test_secret_stage1";
const RETRY_ATTEMPTS: usize = 3;
const RETRY_AFTER_CAP_MS: u64 = 100;
const MAX_HTTP_RESPONSE_BYTES: usize = 1024 * 1024;

#[test]
fn ingest_subcommand_never_starts_server_or_binds_listener() {
    let occupied = TcpListener::bind("127.0.0.1:0").expect("reserve server bind address");
    let occupied_addr = occupied.local_addr().unwrap().to_string();
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let source = write_source(
        "never_starts_server",
        r#"[{"objectID":"a","name":"Alpha"}]"#,
    );

    let mut command = flapjack_cmd();
    let output = command
        .arg("--bind-addr")
        .arg(&occupied_addr)
        .arg("ingest")
        .arg("--endpoint")
        .arg(sink.endpoint())
        .arg("--index")
        .arg("products")
        .arg("--source")
        .arg(source.source_path())
        .arg("--application-id")
        .arg("test-app")
        .arg("--api-key-env")
        .arg("FJ_INGEST_TEST_API_KEY")
        .arg("--idempotency-key-prefix")
        .arg("test-import")
        .arg("--report-json")
        .env("FJ_INGEST_TEST_API_KEY", API_KEY)
        .assert()
        .success()
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["confirmed_committed"], json!(1));
    assert_eq!(sink.next_request().path, "/1/indexes/products/batch");
    drop(occupied);
}

#[test]
fn json_array_and_ndjson_match_hand_calculated_upsert_target() {
    let array_sink = FakeBatchSink::start(vec![SinkResponse::ok(); 2]);
    let array_source = write_source(
        "json_array_upserts",
        r#"[{"objectID":"p1","name":"Alpha"},{"objectID":"p2","name":"Beta"},{"objectID":"p1","name":"Alpha Prime"}]"#,
    );
    ingest_cmd(array_sink.endpoint(), array_source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .success();

    let ndjson_sink = FakeBatchSink::start(vec![SinkResponse::ok(); 2]);
    let ndjson_source = write_source(
        "ndjson_upserts",
        "{\"objectID\":\"p1\",\"name\":\"Alpha\"}\n{\"objectID\":\"p2\",\"name\":\"Beta\"}\n{\"objectID\":\"p1\",\"name\":\"Alpha Prime\"}\n",
    );
    ingest_cmd(ndjson_sink.endpoint(), ndjson_source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .success();

    let array_bodies = array_sink.drain_bodies();
    assert_eq!(array_bodies, ndjson_sink.drain_bodies());
    let mut expected = BTreeMap::new();
    expected.insert(
        "p1".to_string(),
        json!({"objectID":"p1","name":"Alpha Prime"}),
    );
    expected.insert("p2".to_string(), json!({"objectID":"p2","name":"Beta"}));
    assert_eq!(apply_target_model(array_bodies), expected);
}

#[test]
fn upsert_preserves_target_only_records() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let source = write_source("target_only", r#"[{"objectID":"incoming","name":"New"}]"#);

    ingest_cmd(sink.endpoint(), source.source_path())
        .assert()
        .success();

    let mut target = BTreeMap::new();
    target.insert(
        "existing".to_string(),
        json!({"objectID":"existing","name":"Keep"}),
    );
    apply_bodies_to_target(&mut target, sink.drain_bodies());
    assert_eq!(
        target,
        BTreeMap::from([
            (
                "existing".to_string(),
                json!({"objectID":"existing","name":"Keep"})
            ),
            (
                "incoming".to_string(),
                json!({"objectID":"incoming","name":"New"})
            ),
        ])
    );
}

#[test]
fn ordered_upsert_delete_last_action_wins() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 4]);
    let source = write_source(
        "ordered_last_wins",
        concat!(
            "{\"objectID\":\"p1\",\"name\":\"First\"}\n",
            "{\"objectID\":\"p1\",\"_action\":\"delete\"}\n",
            "{\"objectID\":\"p1\",\"name\":\"Second\"}\n",
            "{\"objectID\":\"p2\",\"name\":\"Gone\",\"_action\":\"delete\"}\n",
        ),
    );

    ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("1")
        .assert()
        .success();

    assert_eq!(
        apply_target_model(sink.drain_bodies()),
        BTreeMap::from([("p1".to_string(), json!({"objectID":"p1","name":"Second"}))])
    );
}

#[test]
fn malformed_batch_sends_nothing_for_that_batch() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let source = write_source(
        "malformed_batch",
        "{\"objectID\":\"ok\",\"name\":\"Good\"}\n{\"objectID\":\"bad\",\"name\":\"Bad\",}\n",
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .failure()
        .get_output()
        .clone();

    assert_eq!(sink.try_next_request(Duration::from_millis(150)), None);
    assert!(String::from_utf8_lossy(&output.stderr).contains("malformed"));

    let duplicate_sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let duplicate_source = write_source(
        "duplicate_key_batch",
        "{\"objectID\":\"ok\",\"name\":\"Good\"}\n{\"objectID\":\"dup\",\"name\":\"First\",\"name\":\"Second\"}\n",
    );

    let duplicate_output = ingest_cmd(duplicate_sink.endpoint(), duplicate_source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .failure()
        .get_output()
        .clone();

    assert_eq!(
        duplicate_sink.try_next_request(Duration::from_millis(150)),
        None
    );
    assert!(String::from_utf8_lossy(&duplicate_output.stderr).contains("duplicate"));
}

#[test]
fn json_array_rejects_trailing_non_whitespace_data() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let source = write_source(
        "json_array_trailing_data",
        "[{\"objectID\":\"p1\"}] {\"objectID\":\"silently-ignored\"}",
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .assert()
        .code(2)
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(1));
    assert_eq!(report["confirmed_committed"], json!(1));
    assert_eq!(report["failure_classification"], json!("input"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("trailing"));
}

#[test]
fn mixed_actions_become_ordered_homogeneous_envelopes() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 3]);
    let source = write_source(
        "mixed_actions",
        concat!(
            "{\"objectID\":\"p1\",\"name\":\"One\"}\n",
            "{\"objectID\":\"p2\",\"_action\":\"delete\"}\n",
            "{\"objectID\":\"p3\",\"name\":\"Three\"}\n",
        ),
    );

    ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("3")
        .assert()
        .success();

    assert_eq!(
        sink.drain_bodies(),
        vec![
            json!({"requests":[{"action":"addObject","body":{"objectID":"p1","name":"One"}}]}),
            json!({"requests":[{"action":"deleteObject","body":{"objectID":"p2"}}]}),
            json!({"requests":[{"action":"addObject","body":{"objectID":"p3","name":"Three"}}]}),
        ]
    );
}

#[test]
fn acknowledged_batches_report_exact_confirmed_counts() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 2]);
    let source = write_source(
        "ack_counts",
        r#"[{"objectID":"p1"},{"objectID":"p2"},{"objectID":"p3"}]"#,
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .success()
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(3));
    assert_eq!(report["confirmed_committed"], json!(3));
    assert_eq!(report["outcome_unknown"], json!(0));
}

#[test]
fn lost_response_reports_outcome_unknown_not_false_exact_count() {
    let sink = FakeBatchSink::start(vec![SinkResponse::close_after_read()]);
    let source = write_source("lost_response", r#"[{"objectID":"p1"},{"objectID":"p2"}]"#);

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .failure()
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(2));
    assert_eq!(report["confirmed_committed"], json!(0));
    assert_eq!(report["outcome_unknown"], json!(2));
}

#[test]
fn permanent_error_stops_without_overstating_commits() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(), SinkResponse::status(403)]);
    let source = write_source(
        "permanent_error",
        r#"[{"objectID":"p1"},{"objectID":"p2"},{"objectID":"p3"}]"#,
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .failure()
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(3));
    assert_eq!(report["confirmed_committed"], json!(2));
    assert_eq!(report["outcome_unknown"], json!(0));
}

#[test]
fn real_server_ingest_upserts_and_deletes_visible_records() {
    let data = TempDir::new("ingest_real_server");
    let server = RunningServer::spawn_no_auth_auto_port(data.path());
    let source = write_source(
        "real_server_ingest",
        concat!(
            "{\"objectID\":\"p1\",\"name\":\"Alpha\",\"score\":10}\n",
            "{\"objectID\":\"p2\",\"name\":\"Beta\",\"score\":20}\n",
            "{\"objectID\":\"p1\",\"_action\":\"delete\"}\n",
            "{\"objectID\":\"p3\",\"name\":\"Gamma\",\"score\":30}\n",
        ),
    );

    let output = ingest_cmd(
        format!("http://{}", server.bind_addr()),
        source.source_path(),
    )
    .arg("--batch-size")
    .arg("2")
    .assert()
    .success()
    .get_output()
    .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(4));
    assert_eq!(report["confirmed_committed"], json!(4));
    assert_eq!(report["outcome_unknown"], json!(0));

    let search = wait_for_search_hits(&server, "products", 2);
    assert_eq!(search["nbHits"], json!(2));
    assert!(search_hit_with(&search, "p2", "name", json!("Beta")));
    assert!(search_hit_with(&search, "p3", "score", json!(30)));
    assert!(!search_hit_object_id(&search, "p1"));
}

#[test]
fn default_idempotency_keys_do_not_collide_across_cli_invocations() {
    let data = TempDir::new("ingest_default_idempotency");
    let server = RunningServer::spawn_no_auth_auto_port(data.path());
    let first_source = write_source(
        "default_idempotency_first",
        r#"[{"objectID":"first-run","name":"First"}]"#,
    );
    let second_source = write_source(
        "default_idempotency_second",
        r#"[{"objectID":"second-run","name":"Second"}]"#,
    );

    default_prefix_ingest_cmd(
        format!("http://{}", server.bind_addr()),
        first_source.source_path(),
    )
    .assert()
    .success();
    default_prefix_ingest_cmd(
        format!("http://{}", server.bind_addr()),
        second_source.source_path(),
    )
    .assert()
    .success();

    let search = wait_for_search_hits(&server, "products", 2);
    assert!(search_hit_with(
        &search,
        "first-run",
        "name",
        json!("First")
    ));
    assert!(search_hit_with(
        &search,
        "second-run",
        "name",
        json!("Second")
    ));
}

#[test]
fn retry_reuses_serialized_envelope_and_idempotency_key() {
    let sink = FakeBatchSink::start(vec![
        SinkResponse::status(503),
        SinkResponse::status(503),
        SinkResponse::ok(),
    ]);
    let source = write_source("retry_identity", r#"[{"objectID":"p1","name":"Alpha"}]"#);

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .assert()
        .success()
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(1));
    assert_eq!(report["confirmed_committed"], json!(1));
    assert_eq!(report["outcome_unknown"], json!(0));
    assert_eq!(report["retries"], json!(2));

    let requests = sink.take_requests(3);
    assert_eq!(requests.len(), 3);
    assert_same_retry_envelope_and_key(&requests);
}

#[test]
fn retryable_statuses_use_exact_attempt_budget_and_capped_retry_after() {
    for status in [429, 503] {
        let sink = FakeBatchSink::start(vec![
            SinkResponse::retry_after(status, "5"),
            SinkResponse::retry_after(status, "5"),
            SinkResponse::retry_after(status, "5"),
        ]);
        let source = write_source(
            &format!("retry_budget_{status}"),
            r#"[{"objectID":"p1","name":"Alpha"}]"#,
        );

        let output = ingest_cmd(sink.endpoint(), source.source_path())
            .assert()
            .failure()
            .get_output()
            .clone();

        let report = json_stdout(&output.stdout);
        assert_eq!(sink.take_requests(RETRY_ATTEMPTS).len(), RETRY_ATTEMPTS);
        assert_eq!(sink.try_next_request(Duration::from_millis(150)), None);
        assert_eq!(report["attempted"], json!(1));
        assert_eq!(report["confirmed_committed"], json!(0));
        assert_eq!(report["outcome_unknown"], json!(0));
        assert_eq!(report["retries"], json!(2));
        assert_eq!(report["last_retry_after_ms"], json!(RETRY_AFTER_CAP_MS));
        assert_eq!(report["failure_classification"], json!("retry_exhausted"));
    }
}

#[test]
fn exhausted_pre_send_connection_failure_reports_zero_unknown() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("reserve closed endpoint");
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let source = write_source("pre_send_connection_failure", r#"[{"objectID":"p1"}]"#);

    let output = ingest_cmd(endpoint, source.source_path())
        .assert()
        .code(5)
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(1));
    assert_eq!(report["confirmed_committed"], json!(0));
    assert_eq!(report["outcome_unknown"], json!(0));
    assert_eq!(report["failure_classification"], json!("retry_exhausted"));
}

#[test]
fn exhausted_lost_response_reports_unknown_not_confirmed() {
    let sink = FakeBatchSink::start(vec![
        SinkResponse::close_after_read(),
        SinkResponse::close_after_read(),
        SinkResponse::close_after_read(),
    ]);
    let source = write_source(
        "lost_response_exhausted",
        r#"[{"objectID":"p1"},{"objectID":"p2"}]"#,
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--batch-size")
        .arg("2")
        .assert()
        .code(4)
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(2));
    assert_eq!(report["confirmed_committed"], json!(0));
    assert_eq!(report["outcome_unknown"], json!(2));
    assert_eq!(report["failure_classification"], json!("outcome_unknown"));
}

#[test]
fn oversized_sink_response_is_bounded_and_reported_unknown() {
    let oversized_body = "x".repeat(MAX_HTTP_RESPONSE_BYTES + 1);
    let sink = FakeBatchSink::start(vec![
        SinkResponse::status_with_body(500, &oversized_body),
        SinkResponse::status_with_body(500, &oversized_body),
        SinkResponse::status_with_body(500, &oversized_body),
    ]);
    let source = write_source("oversized_sink_response", r#"[{"objectID":"p1"}]"#);

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .assert()
        .code(4)
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(1));
    assert_eq!(report["confirmed_committed"], json!(0));
    assert_eq!(report["outcome_unknown"], json!(1));
    assert_eq!(report["failure_classification"], json!("outcome_unknown"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("too large"));
}

#[test]
fn ingest_exit_codes_and_json_failure_classification_are_stable() {
    let missing_key_source = write_source("missing_key_source", r#"[{"objectID":"p1"}]"#);
    let missing_key = ingest_cmd_without_key(
        "http://127.0.0.1:1".to_string(),
        missing_key_source.source_path(),
    )
    .arg("--api-key-env")
    .arg("FJ_INGEST_MISSING_KEY")
    .assert()
    .code(2)
    .get_output()
    .clone();
    assert_eq!(
        json_stdout(&missing_key.stdout)["failure_classification"],
        json!("config")
    );

    let reject_sink = FakeBatchSink::start(vec![SinkResponse::status(403)]);
    let reject_source = write_source("permanent_reject_source", r#"[{"objectID":"p1"}]"#);
    let rejected = ingest_cmd(reject_sink.endpoint(), reject_source.source_path())
        .assert()
        .code(3)
        .get_output()
        .clone();
    assert_eq!(
        json_stdout(&rejected.stdout)["failure_classification"],
        json!("permanent_http_rejection")
    );
}

#[test]
fn blocked_sink_bounds_parser_readahead_and_queue_high_watermark() {
    let sink = FakeBatchSink::start(vec![SinkResponse::hold_open()]);
    let source = write_source("blocked_sink", &many_ndjson_records(128));

    let mut command = ingest_process_cmd(sink.endpoint(), source.source_path());
    command
        .arg("--batch-size")
        .arg("4")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("spawn ingest");

    let first = sink.next_request();
    assert_eq!(batch_operation_count(&first.body), 4);
    assert!(
        sink.try_next_request(Duration::from_millis(250)).is_none(),
        "ingest must not queue more HTTP batches while the first sink request is blocked"
    );
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn credentials_redirects_and_reports_are_secret_safe() {
    let help = flapjack_cmd()
        .arg("ingest")
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    assert!(!String::from_utf8_lossy(&help).contains("--api-key "));

    let secret_file = write_source("api_key_file", API_KEY);
    let file_sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let source = write_source("secret_file_source", r#"[{"objectID":"p1"}]"#);
    ingest_cmd_without_key(file_sink.endpoint(), source.source_path())
        .arg("--api-key-file")
        .arg(secret_file.source_path())
        .assert()
        .success();
    assert_eq!(
        file_sink.next_request().header("x-algolia-api-key"),
        Some(API_KEY.to_string())
    );

    let stdin_sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let stdin_source = write_source("stdin_key_source", r#"[{"objectID":"p2"}]"#);
    ingest_cmd_without_key(stdin_sink.endpoint(), stdin_source.source_path())
        .arg("--api-key-stdin")
        .write_stdin(API_KEY)
        .assert()
        .success();

    ingest_cmd_without_key(stdin_sink.endpoint(), "-")
        .arg("--api-key-stdin")
        .write_stdin(API_KEY)
        .assert()
        .failure()
        .stderr(predicates::str::contains("stdin"));

    let redirect_sink = FakeBatchSink::start(vec![SinkResponse::redirect_with_secret(API_KEY)]);
    let redirect_source = write_source("redirect_source", r#"[{"objectID":"p3"}]"#);
    let output = ingest_cmd(redirect_sink.endpoint(), redirect_source.source_path())
        .assert()
        .failure()
        .get_output()
        .clone();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stdout.contains(API_KEY), "stdout leaked API key: {stdout}");
    assert!(!stderr.contains(API_KEY), "stderr leaked API key: {stderr}");
    assert!(!redirect_sink.next_request().path.contains(API_KEY));

    let unknown_arg = ingest_cmd_without_key(file_sink.endpoint(), source.source_path())
        .arg("--api-key")
        .arg(API_KEY)
        .assert()
        .failure()
        .get_output()
        .clone();
    let unknown_stdout = String::from_utf8_lossy(&unknown_arg.stdout);
    let unknown_stderr = String::from_utf8_lossy(&unknown_arg.stderr);
    assert!(
        !unknown_stdout.contains(API_KEY),
        "unknown-argument stdout leaked API key: {unknown_stdout}"
    );
    assert!(
        !unknown_stderr.contains(API_KEY),
        "unknown-argument stderr leaked API key: {unknown_stderr}"
    );

    let malformed_sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let malformed_source = write_source(
        "secret_safe_malformed_source",
        &format!("{{\"objectID\":\"p4\",\"token\":\"{API_KEY}\",}}\n"),
    );
    let malformed = ingest_cmd(malformed_sink.endpoint(), malformed_source.source_path())
        .assert()
        .failure()
        .get_output()
        .clone();
    let malformed_stdout = String::from_utf8_lossy(&malformed.stdout);
    let malformed_stderr = String::from_utf8_lossy(&malformed.stderr);
    assert!(
        !malformed_stdout.contains(API_KEY),
        "malformed stdout leaked API key: {malformed_stdout}"
    );
    assert!(
        !malformed_stderr.contains(API_KEY),
        "malformed stderr leaked API key: {malformed_stderr}"
    );
}

#[test]
fn non_json_failure_redacts_api_key_from_stderr() {
    let sink = FakeBatchSink::start(vec![SinkResponse::status_with_body(
        403,
        &format!("credential {API_KEY} rejected"),
    )]);
    let source = write_source("non_json_secret_error", r#"[{"objectID":"p1"}]"#);

    let output = ingest_cmd_without_report(sink.endpoint(), source.source_path())
        .assert()
        .code(3)
        .get_output()
        .clone();

    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains(API_KEY), "stderr leaked API key: {stderr}");
    assert!(stderr.contains("[REDACTED]"));
}

/// The fake sink must keep answering after its scripted responses run out.
///
/// It used to serve exactly `responses.len()` connections and then return, dropping its
/// `TcpListener` and closing the port. The ingest CLI retries a failed batch up to
/// `RETRY_ATTEMPT_LIMIT` times, so any scenario producing a retry hit a dead port on the next
/// attempt and reported `failed to connect to sink: Connection refused` — a fact about the harness,
/// not about the product under test, and one that also changed the process exit code. Real HTTP
/// endpoints keep listening; so does this one now, replaying its last scripted response for
/// anything beyond the script. See `ROADMAP.md` row `TEST-SINK-1`.
#[test]
fn fake_sink_answers_connections_beyond_its_scripted_responses() {
    let sink = FakeBatchSink::start(vec![SinkResponse::status(403)]);

    let scripted = post_to_sink(&sink, r#"{"requests":[]}"#);
    assert!(
        scripted.starts_with("HTTP/1.1 403"),
        "scripted response should be the 403 the script names: {scripted}"
    );

    // Beyond the script. Before the repair this connect was refused outright.
    let unscripted = post_to_sink(&sink, r#"{"requests":[]}"#);
    assert!(
        unscripted.starts_with("HTTP/1.1 403"),
        "unscripted response should replay the last scripted status: {unscripted}"
    );

    // Both requests are still recorded, so a test asserting "the CLI must not send another batch"
    // keeps failing loudly when it does — the replay hides the connection, never the request.
    assert_eq!(sink.drain_bodies().len(), 2);
}

#[test]
fn fake_sink_ignores_unrelated_metrics_probes_without_consuming_its_script() {
    let sink = FakeBatchSink::start(vec![SinkResponse::status(403)]);

    let metrics_response = get_from_sink(&sink, "/metrics");
    assert!(
        metrics_response.starts_with("HTTP/1.1 404"),
        "an unrelated metrics probe must be rejected, got: {metrics_response}"
    );

    let ingest_response = post_to_sink(&sink, r#"{"requests":[]}"#);
    assert!(
        ingest_response.starts_with("HTTP/1.1 403"),
        "the metrics probe must not consume the scripted ingest response, got: {ingest_response}"
    );
    assert_eq!(
        sink.next_request().path,
        "/1/indexes/products/batch",
        "only ingest-protocol requests belong in the sink's request channel"
    );
    assert!(
        sink.try_next_request(Duration::from_millis(50)).is_none(),
        "the unrelated metrics probe must not be recorded as ingest traffic"
    );
}

/// When retries are exhausted the CLI must report what the sink actually said.
///
/// `503` is retryable, so the CLI makes all three attempts against a script holding one response.
/// Attempts two and three land beyond the script — previously a closed port, which made the final
/// user-visible message `failed to connect to sink: Connection refused` even though the sink had
/// answered perfectly well. That message named the harness's own bookkeeping as the failure.
#[test]
fn exhausted_retries_report_the_sink_status_not_a_harness_connect_failure() {
    let sink = FakeBatchSink::start(vec![SinkResponse::status(503)]);
    let source = write_source("retry_exhausted_message", r#"[{"objectID":"p1"}]"#);

    let output = ingest_cmd_without_report(sink.endpoint(), source.source_path())
        .assert()
        .code(5) // EXIT_RETRY_EXHAUSTED
        .get_output()
        .clone();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("retryable HTTP 503"),
        "stderr must name the sink's own response: {stderr}"
    );
    assert!(
        !stderr.contains("Connection refused"),
        "a sink that is still listening must never produce a connect failure: {stderr}"
    );
}

#[test]
fn api_key_with_http_delimiters_is_rejected_before_connecting() {
    let sink = FakeBatchSink::start(vec![SinkResponse::ok(); 1]);
    let source = write_source("api_key_header_injection", r#"[{"objectID":"p1"}]"#);

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .env(
            "FJ_INGEST_TEST_API_KEY",
            format!("{API_KEY}\r\nx-injected: true"),
        )
        .assert()
        .code(2)
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(0));
    assert_eq!(report["failure_classification"], json!("config"));
    assert_eq!(sink.try_next_request(Duration::from_millis(150)), None);
}

#[test]
fn replace_mode_submits_bulk_replace_job_and_reports_confirmed() {
    let job_id = "2fd16c8a-9b40-46d9-b252-c0d22ae6d27c";
    let sink = FakeBatchSink::start(vec![
        SinkResponse::status_with_body(
            202,
            &json!({
                "jobID": job_id,
                "targetIndex": "products",
                "topology": "single_node_only",
                "phase": "submitted",
                "disposition": "running"
            })
            .to_string(),
        ),
        SinkResponse::status_with_body(
            200,
            &json!({
                "jobID": job_id,
                "targetIndex": "products",
                "topology": "single_node_only",
                "phase": "activating",
                "disposition": "succeeded",
                "objectsImported": {"imported": 2}
            })
            .to_string(),
        ),
    ]);
    let source = write_source(
        "replace_success_source",
        r#"[{"objectID":"incoming-1","name":"First"},{"objectID":"incoming-2","name":"Second"}]"#,
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--mode")
        .arg("replace")
        .assert()
        .success()
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(2));
    assert_eq!(report["confirmed_committed"], json!(2));
    assert_eq!(report["outcome_unknown"], json!(0));
    assert_eq!(report["failure_classification"], Value::Null);

    let submit = sink.next_request();
    assert_eq!(submit.path, "/1/migrations/bulk-replace?indexName=products");
    assert_eq!(
        submit.header("content-type").as_deref(),
        Some("application/x-ndjson")
    );
    assert_eq!(
        submit.body,
        json!([
            {"objectID":"incoming-1","name":"First"},
            {"objectID":"incoming-2","name":"Second"}
        ])
    );
    let status = sink.next_request();
    assert_eq!(status.path, format!("/1/migrations/bulk-replace/{job_id}"));
}

#[test]
fn replace_mode_reports_server_refusal_without_confirming_mutation() {
    let sink = FakeBatchSink::start(vec![SinkResponse::status_with_body(
        503,
        r#"{"message":"Migration is only supported when no replication peers are configured","status":503,"code":"migration_ha_unsupported"}"#,
    )]);
    let source = write_source(
        "replace_server_refusal_source",
        "{\"objectID\":\"incoming\",\"name\":\"Must Not Be Confirmed\"}\n",
    );

    let output = ingest_cmd(sink.endpoint(), source.source_path())
        .arg("--mode")
        .arg("replace")
        .assert()
        .code(2)
        .get_output()
        .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(1));
    assert_eq!(report["confirmed_committed"], json!(0));
    assert_eq!(report["outcome_unknown"], json!(0));
    assert_eq!(
        report["failure_classification"],
        json!("replace_not_supported")
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("migration_ha_unsupported"));
    assert_eq!(
        sink.next_request().path,
        "/1/migrations/bulk-replace?indexName=products"
    );
}

fn ingest_cmd(endpoint: String, source: &str) -> assert_cmd::Command {
    let mut command = base_ingest_cmd(endpoint, source, true);
    command
        .arg("--api-key-env")
        .arg("FJ_INGEST_TEST_API_KEY")
        .env("FJ_INGEST_TEST_API_KEY", API_KEY);
    command
}

fn ingest_cmd_without_key(endpoint: String, source: &str) -> assert_cmd::Command {
    base_ingest_cmd(endpoint, source, true)
}

fn ingest_cmd_without_report(endpoint: String, source: &str) -> assert_cmd::Command {
    let mut command = base_ingest_cmd(endpoint, source, false);
    command
        .arg("--api-key-env")
        .arg("FJ_INGEST_TEST_API_KEY")
        .env("FJ_INGEST_TEST_API_KEY", API_KEY);
    command
}

fn default_prefix_ingest_cmd(endpoint: String, source: &str) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command
        .arg("ingest")
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--index")
        .arg("products")
        .arg("--source")
        .arg(source)
        .arg("--application-id")
        .arg("test-app")
        .arg("--api-key-env")
        .arg("FJ_INGEST_TEST_API_KEY")
        .arg("--report-json")
        .env("FJ_INGEST_TEST_API_KEY", API_KEY);
    command
}

fn base_ingest_cmd(endpoint: String, source: &str, report_json: bool) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command
        .arg("ingest")
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--index")
        .arg("products")
        .arg("--source")
        .arg(source)
        .arg("--application-id")
        .arg("test-app")
        .arg("--idempotency-key-prefix")
        .arg("test-import");
    if report_json {
        command.arg("--report-json");
    }
    command
}

fn ingest_process_cmd(endpoint: String, source: &str) -> ProcessCommand {
    let mut command = ProcessCommand::new(env!("CARGO_BIN_EXE_flapjack"));
    for env_var in [
        "FLAPJACK_ADMIN_KEY",
        "FLAPJACK_NO_AUTH",
        "FLAPJACK_ENV",
        "FLAPJACK_BIND_ADDR",
        "FLAPJACK_PORT",
        "FLAPJACK_DATA_DIR",
        "FLAPJACK_IDEMPOTENCY_TTL_SECS",
        "FLAPJACK_IDEMPOTENCY_PERSISTENT",
        "FLAPJACK_IDEMPOTENCY_PERSIST",
    ] {
        command.env_remove(env_var);
    }
    command
        .arg("ingest")
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--index")
        .arg("products")
        .arg("--source")
        .arg(source)
        .arg("--application-id")
        .arg("test-app")
        .arg("--api-key-env")
        .arg("FJ_INGEST_TEST_API_KEY")
        .arg("--idempotency-key-prefix")
        .arg("test-import")
        .arg("--report-json")
        .env("FJ_INGEST_TEST_API_KEY", API_KEY);
    command
}

fn write_source(name: &str, contents: &str) -> SourceFile {
    let tmp = TempDir::new(name);
    let path = tmp.root().join("source.json");
    std::fs::write(&path, contents).unwrap();
    SourceFile {
        _tmp: tmp,
        path: path.to_string_lossy().to_string(),
    }
}

struct SourceFile {
    _tmp: TempDir,
    path: String,
}

impl SourceFile {
    fn source_path(&self) -> &str {
        &self.path
    }
}

fn json_stdout(stdout: &[u8]) -> Value {
    serde_json::from_slice(stdout).unwrap_or_else(|error| {
        panic!(
            "stdout must be JSON: {} ({error})",
            String::from_utf8_lossy(stdout)
        )
    })
}

fn many_ndjson_records(count: usize) -> String {
    (0..count)
        .map(|i| format!("{{\"objectID\":\"p{i}\",\"value\":{i}}}\n"))
        .collect()
}

fn apply_target_model(bodies: Vec<Value>) -> BTreeMap<String, Value> {
    let mut target = BTreeMap::new();
    apply_bodies_to_target(&mut target, bodies);
    target
}

fn apply_bodies_to_target(target: &mut BTreeMap<String, Value>, bodies: Vec<Value>) {
    for body in bodies {
        for request in body["requests"].as_array().unwrap() {
            let object_id = request["body"]["objectID"].as_str().unwrap().to_string();
            match request["action"].as_str().unwrap() {
                "addObject" => {
                    target.insert(object_id, request["body"].clone());
                }
                "deleteObject" => {
                    target.remove(&object_id);
                }
                other => panic!("unexpected action {other}"),
            }
        }
    }
}

fn wait_for_search_hits(server: &RunningServer, index_name: &str, expected: u64) -> Value {
    let mut last = Value::Null;
    for _ in 0..80 {
        last = server.search(index_name, json!({"query":"","hitsPerPage":20}));
        if last["nbHits"] == json!(expected) {
            return last;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("expected {expected} search hits, last response: {last}");
}

fn search_hit_object_id(search: &Value, object_id: &str) -> bool {
    search["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hit| hit["objectID"] == json!(object_id))
}

fn search_hit_with(search: &Value, object_id: &str, field: &str, value: Value) -> bool {
    search["hits"]
        .as_array()
        .unwrap()
        .iter()
        .any(|hit| hit["objectID"] == json!(object_id) && hit.get(field) == Some(&value))
}

fn assert_same_retry_envelope_and_key(requests: &[RecordedRequest]) {
    let first_body = &requests[0].raw_body;
    let first_key = requests[0]
        .header("x-flapjack-idempotency-key")
        .expect("idempotency key header");
    for request in requests {
        assert_eq!(&request.raw_body, first_body);
        assert_eq!(
            request.header("x-flapjack-idempotency-key"),
            Some(first_key.clone())
        );
    }
}

fn batch_operation_count(body: &Value) -> usize {
    body["requests"].as_array().unwrap().len()
}

/// Minimal raw HTTP POST straight at the fake sink, with no CLI in the middle.
///
/// Used by the harness-contract tests so they assert what the sink does, not what the CLI does with
/// it — mixing the two is how a harness defect ends up diagnosed as a product defect.
fn post_to_sink(sink: &FakeBatchSink, body: &str) -> String {
    let mut stream = TcpStream::connect(sink.bind_addr.as_str())
        .unwrap_or_else(|error| panic!("connect to fake sink at {}: {error}", sink.bind_addr));
    // Bounded on purpose. A raw socket read with no timeout turns any sink hiccup into an
    // indefinite hang, which is the failure mode this whole repair exists to remove — a hang
    // reports nothing and costs a full CI timeout, while a bounded read fails with a message.
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set sink read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("set sink write timeout");
    let request = format!(
        "POST /1/indexes/products/batch HTTP/1.1\r\nHost: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        sink.bind_addr,
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write to sink");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read sink response");
    response
}

fn get_from_sink(sink: &FakeBatchSink, path: &str) -> String {
    let mut stream = TcpStream::connect(sink.bind_addr.as_str())
        .unwrap_or_else(|error| panic!("connect to fake sink at {}: {error}", sink.bind_addr));
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("set sink read timeout");
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .expect("set sink write timeout");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        sink.bind_addr
    );
    stream.write_all(request.as_bytes()).expect("write to sink");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read sink response");
    response
}

#[derive(Clone)]
enum SinkResponse {
    Ok,
    CloseAfterRead,
    HoldOpen,
    Status(u16),
    StatusWithBody(u16, String),
    RetryAfter(u16, String),
    RedirectWithSecret(String),
}

impl SinkResponse {
    fn ok() -> Self {
        Self::Ok
    }

    fn close_after_read() -> Self {
        Self::CloseAfterRead
    }

    fn hold_open() -> Self {
        Self::HoldOpen
    }

    fn status(status: u16) -> Self {
        Self::Status(status)
    }

    fn status_with_body(status: u16, body: &str) -> Self {
        Self::StatusWithBody(status, body.to_string())
    }

    fn retry_after(status: u16, retry_after: &str) -> Self {
        Self::RetryAfter(status, retry_after.to_string())
    }

    fn redirect_with_secret(secret: &str) -> Self {
        Self::RedirectWithSecret(secret.to_string())
    }
}

#[derive(Debug, PartialEq, Eq)]
struct RecordedRequest {
    path: String,
    headers: BTreeMap<String, String>,
    raw_body: Vec<u8>,
    body: Value,
}

impl RecordedRequest {
    fn header(&self, name: &str) -> Option<String> {
        self.headers.get(&name.to_ascii_lowercase()).cloned()
    }
}

struct FakeBatchSink {
    bind_addr: String,
    requests: Receiver<RecordedRequest>,
    /// Set by `Drop` so the accept loop stops and releases its listener. Without it the loop below
    /// would hold one file descriptor and one parked thread per sink for the life of the test
    /// binary, and this file starts 26 of them.
    shutdown: Arc<AtomicBool>,
    /// Faults the sink thread hit while serving. `Drop` fails the test on them, so a sink that dies
    /// mid-test reports *why* instead of silently becoming "connection refused" somewhere else.
    /// This is the whole lesson of `TEST-SINK-1`: the harness used to swallow its own death.
    faults: Arc<Mutex<Vec<String>>>,
}

impl FakeBatchSink {
    fn start(responses: Vec<SinkResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake sink");
        let bind_addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let faults = Arc::new(Mutex::new(Vec::new()));
        thread::spawn({
            let shutdown = Arc::clone(&shutdown);
            let faults = Arc::clone(&faults);
            move || serve_fake_sink(listener, responses, tx, shutdown, faults)
        });
        Self {
            bind_addr,
            requests: rx,
            shutdown,
            faults,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.bind_addr)
    }

    /// Wait for a request the test EXPECTS to arrive.
    ///
    /// The bound is generous on purpose, and the asymmetry with `try_next_request` below is the
    /// whole point. This call is waiting on a spawned `flapjack` process — a ~300 MB binary that
    /// must load, parse its source file, and open a connection — while the rest of this file runs
    /// in parallel and other suites contend for the same host. Five seconds measured as too tight:
    /// on an otherwise-idle run the whole file finishes in ~6 s, but under load
    /// `blocked_sink_bounds_parser_readahead_and_queue_high_watermark` reported
    /// `expected fake sink request: Timeout` in 4 of 5 consecutive full-file runs, on unmodified
    /// `main` as well as with the `TEST-SINK-1` repair. Nothing about parser readahead is being
    /// measured when it fails that way.
    ///
    /// This is not "raise the timeout instead of diagnosing". The diagnosis is that the bound was
    /// measuring process-start latency on a contended host rather than the behaviour under test;
    /// widening a bound that only ever gates *how long we wait for something we require* cannot
    /// hide a defect, because the assertion still has to hold when it arrives.
    fn next_request(&self) -> RecordedRequest {
        self.requests
            .recv_timeout(Duration::from_secs(30))
            .expect("expected fake sink request")
    }

    /// Wait for a request the test may be proving does NOT arrive.
    ///
    /// The caller supplies the bound and callers keep it SHORT deliberately — several tests assert
    /// `try_next_request(..).is_none()` to prove the CLI sent no further batch. Widening those
    /// would weaken a real assertion, which is exactly why `next_request` above was widened alone.
    fn try_next_request(&self, timeout: Duration) -> Option<RecordedRequest> {
        self.requests.recv_timeout(timeout).ok()
    }

    /// Collect every request already recorded, stopping once the sink goes quiet.
    ///
    /// The overall deadline is load-bearing and was added with the `TEST-SINK-1` repair. Before it,
    /// this loop terminated for an accidental reason: the sink stopped answering once its script ran
    /// out, so no further request could ever arrive. Now that the sink keeps serving — which is the
    /// point of the repair — a client that connects in a loop would keep this `while let` fed
    /// forever, and the test would HANG rather than fail. It did exactly that once during
    /// development, and a hang is strictly worse than a failure: it burns a full CI timeout and
    /// reports nothing. So the drain fails loudly instead, naming what it saw.
    fn drain_bodies(&self) -> Vec<Value> {
        const DRAIN_DEADLINE: Duration = Duration::from_secs(20);
        let started = std::time::Instant::now();
        let mut bodies = Vec::new();
        let mut paths = Vec::new();
        while let Some(request) = self.try_next_request(Duration::from_millis(250)) {
            paths.push(request.path.clone());
            bodies.push(request.body);
            assert!(
                started.elapsed() < DRAIN_DEADLINE,
                "fake sink was still receiving requests after {DRAIN_DEADLINE:?} \
                 ({} drained); the client under test is looping, not finishing. \
                 Distinct paths seen: {:?}",
                bodies.len(),
                {
                    // Naming the paths makes the next occurrence self-diagnosing: one path means
                    // this test's own CLI is looping, several means a client found the wrong sink.
                    let mut distinct = paths.clone();
                    distinct.sort();
                    distinct.dedup();
                    distinct
                }
            );
        }
        bodies
    }

    fn take_requests(&self, count: usize) -> Vec<RecordedRequest> {
        (0..count).map(|_| self.next_request()).collect()
    }
}

impl Drop for FakeBatchSink {
    fn drop(&mut self) {
        // Stop the accept loop first, then poke the port so a thread parked in `accept()` wakes and
        // observes the flag. Ordering matters: the store must be visible before the wake-up
        // connection, or the loop serves the poke as if it were a real request.
        self.shutdown.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.bind_addr.as_str());

        // A sink that faulted mid-test is the defect this harness exists to surface, so it fails
        // its own test by name. Skipped while already unwinding: a second panic there aborts the
        // process and destroys the original failure message.
        if std::thread::panicking() {
            return;
        }
        let faults = self
            .faults
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        assert!(
            faults.is_empty(),
            "fake sink faulted: {}",
            faults.join("; ")
        );
    }
}

fn record_fault(faults: &Mutex<Vec<String>>, message: String) {
    faults
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(message);
}

/// Serve connections until the owning `FakeBatchSink` is dropped.
///
/// This loop used to run exactly `responses.len()` times and then return, which dropped `listener`
/// and closed the port. The ingest CLI retries a batch up to `RETRY_ATTEMPT_LIMIT` times, so any
/// scenario that produced a retry met a dead port on the next attempt; `send_once` then reported
/// `failed to connect to sink: Connection refused`, every attempt counted as non-ambiguous, and the
/// process exited `5` (`EXIT_RETRY_EXHAUSTED`) instead of whatever the sink had actually said. The
/// harness's own bookkeeping became the product's reported failure. `ROADMAP.md` row `TEST-SINK-1`.
///
/// Real HTTP endpoints do not stop listening after N requests, so neither does this one: beyond the
/// script it replays the last scripted response, which is also what a real server would do — a 403
/// stays a 403 however many times you ask.
fn serve_fake_sink(
    listener: TcpListener,
    responses: Vec<SinkResponse>,
    tx: Sender<RecordedRequest>,
    shutdown: Arc<AtomicBool>,
    faults: Arc<Mutex<Vec<String>>>,
) {
    // Replay is bounded. "Keep listening" is the repair, but *unbounded* replay reintroduces a
    // worse failure than the one it fixes: a client that connects in a loop keeps the sink fed
    // forever and the test HANGS instead of failing. That happened during development — one run saw
    // 101 requests arrive on a sink scripted for 2. A cap converts that into a named fault, and the
    // number is far above any legitimate scenario: the CLI's own ceiling is
    // `script length x RETRY_ATTEMPT_LIMIT`, and no test here scripts more than a handful.
    const MAX_SERVED_CONNECTIONS: usize = 256;
    let mut served = 0usize;
    let mut scripted: VecDeque<_> = responses.into();
    let mut last: Option<SinkResponse> = None;
    loop {
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) => {
                // Previously swallowed by `let Ok(..) else { return }`, which closed the port and
                // left the cause invisible. Record it so `Drop` names it.
                record_fault(&faults, format!("accept failed: {error}"));
                return;
            }
        };
        if shutdown.load(Ordering::SeqCst) {
            return; // the wake-up connection from `Drop`
        }
        let response = scripted
            .front()
            .cloned()
            // `last` is only `None` for an empty script, which no test writes; answering 200 keeps
            // the port honest rather than silently closing it.
            .or_else(|| last.clone())
            .unwrap_or(SinkResponse::Ok);
        match handle_fake_sink_connection(stream, response, &tx) {
            Ok(false) => continue,
            Ok(true) => {
                served += 1;
                if let Some(consumed) = scripted.pop_front() {
                    last = Some(consumed);
                }
                if served > MAX_SERVED_CONNECTIONS {
                    record_fault(
                        &faults,
                        format!(
                            "sink served more than {MAX_SERVED_CONNECTIONS} ingest connections; \
                             an ingest client is looping rather than finishing"
                        ),
                    );
                    return;
                }
            }
            Err(error) => record_fault(&faults, error),
        }
    }
}

/// Serve one connection.
///
/// `Err` means the **sink** could not do its job — a malformed request line, a header with no
/// colon, a body that is not the JSON it claims to be — and `Drop` fails the test with it. A client
/// that hangs up mid-request is deliberately NOT an error: several tests kill the CLI on purpose,
/// and the wake-up connection from `Drop` sends nothing at all. Every `unwrap()` here used to panic
/// a detached thread instead, which killed the listener and turned the real cause into
/// `Connection refused` at whatever unrelated place next tried to connect.
fn handle_fake_sink_connection(
    mut stream: TcpStream,
    response: SinkResponse,
    tx: &Sender<RecordedRequest>,
) -> Result<bool, String> {
    let cloned = stream
        .try_clone()
        .map_err(|error| format!("clone sink stream: {error}"))?;
    let mut reader = BufReader::new(cloned);
    let mut request_line = String::new();
    match reader.read_line(&mut request_line) {
        Ok(0) | Err(_) => return Ok(false), // client hung up before sending anything
        Ok(_) => {}
    }
    let Some(path) = request_line.split_whitespace().nth(1).map(str::to_string) else {
        return Err(format!("malformed request line: {request_line:?}"));
    };
    // Parallel tests use ephemeral ports. A client that reserved a port and released it before
    // spawning its server can briefly race this already-bound sink and send a readiness probe here
    // instead. Such traffic is neither an ingest request nor evidence that the ingest CLI loops:
    // reject it without consuming a scripted response or recording it in the ingest channel.
    if !path.starts_with("/1/") {
        write_response(&mut stream, 404, "NOT FOUND", "{}");
        return Ok(false);
    }
    let mut headers = BTreeMap::new();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return Ok(false), // client hung up mid-headers
            Ok(_) => {}
        }
        if line == "\r\n" {
            break;
        }
        let Some((name, value)) = line.trim_end().split_once(':') else {
            return Err(format!("malformed header line: {line:?}"));
        };
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body_bytes = vec![0; content_length];
    if reader.read_exact(&mut body_bytes).is_err() {
        return Ok(false); // client hung up mid-body
    }
    let body = if headers
        .get("content-type")
        .is_some_and(|value| value.starts_with("application/x-ndjson"))
    {
        let mut entries = Vec::new();
        for line in body_bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        {
            entries.push(
                serde_json::from_slice(line)
                    .map_err(|error| format!("ndjson line is not JSON: {error}"))?,
            );
        }
        Value::Array(entries)
    } else if body_bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body_bytes)
            .map_err(|error| format!("request body is not JSON: {error}"))?
    };
    // A closed receiver just means the test finished and dropped its `FakeBatchSink`; that is not a
    // sink fault, and reporting it as one would race `Drop`'s own fault check.
    if tx
        .send(RecordedRequest {
            path,
            headers,
            raw_body: body_bytes,
            body,
        })
        .is_err()
    {
        return Ok(true);
    }

    match response {
        SinkResponse::Ok => write_response(&mut stream, 200, "OK", "{}"),
        SinkResponse::Status(status) => write_response(&mut stream, status, "ERR", "{}"),
        SinkResponse::StatusWithBody(status, body) => {
            write_response(&mut stream, status, "ERR", &body)
        }
        SinkResponse::RetryAfter(status, retry_after) => {
            write_retry_after_response(&mut stream, status, "RETRY", "{}", &retry_after)
        }
        SinkResponse::CloseAfterRead => {}
        SinkResponse::HoldOpen => thread::sleep(Duration::from_secs(30)),
        SinkResponse::RedirectWithSecret(secret) => {
            let head = format!(
                "HTTP/1.1 307 Redirect\r\nLocation: http://127.0.0.1/next?key={secret}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            // Write failures here mean the client is already gone — benign, never a sink fault.
            let _ = stream.write_all(head.as_bytes());
        }
    }
    Ok(true)
}

fn write_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // The client may have hung up (several tests kill the CLI mid-batch); that is not a fault.
    let _ = stream.write_all(response.as_bytes());
}

fn write_retry_after_response(
    stream: &mut TcpStream,
    status: u16,
    reason: &str,
    body: &str,
    retry_after: &str,
) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nRetry-After: {retry_after}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    // Same as `write_response`: a hung-up client is not a sink fault.
    let _ = stream.write_all(response.as_bytes());
}
