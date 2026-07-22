#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

mod support;

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
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
fn replace_mode_refuses_typed_zero_mutation() {
    let data = TempDir::new("ingest_replace_refusal");
    let server = RunningServer::spawn_no_auth_auto_port(data.path());
    let task_id = server.add_documents_batch(
        "products",
        json!({"requests":[{"action":"addObject","body":{"objectID":"sentinel","name":"Keep"}}]}),
    );
    server.wait_for_task_published("products", task_id, Duration::from_secs(5));
    let source = write_source(
        "replace_refusal_source",
        r#"[{"objectID":"incoming","name":"Must Not Land"}]"#,
    );

    let output = ingest_cmd(
        format!("http://{}", server.bind_addr()),
        source.source_path(),
    )
    .arg("--mode")
    .arg("replace")
    .assert()
    .code(2)
    .get_output()
    .clone();

    let report = json_stdout(&output.stdout);
    assert_eq!(report["attempted"], json!(0));
    assert_eq!(report["confirmed_committed"], json!(0));
    assert_eq!(report["outcome_unknown"], json!(0));
    assert_eq!(
        report["failure_classification"],
        json!("replace_not_supported")
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("replace_not_supported"));

    let search = wait_for_search_hits(&server, "products", 1);
    assert!(search_hit_with(&search, "sentinel", "name", json!("Keep")));
    assert!(!search_hit_object_id(&search, "incoming"));
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
}

impl FakeBatchSink {
    fn start(responses: Vec<SinkResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake sink");
        let bind_addr = listener.local_addr().unwrap().to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || serve_fake_sink(listener, responses, tx));
        Self {
            bind_addr,
            requests: rx,
        }
    }

    fn endpoint(&self) -> String {
        format!("http://{}", self.bind_addr)
    }

    fn next_request(&self) -> RecordedRequest {
        self.requests
            .recv_timeout(Duration::from_secs(5))
            .expect("expected fake sink request")
    }

    fn try_next_request(&self, timeout: Duration) -> Option<RecordedRequest> {
        self.requests.recv_timeout(timeout).ok()
    }

    fn drain_bodies(&self) -> Vec<Value> {
        let mut bodies = Vec::new();
        while let Some(request) = self.try_next_request(Duration::from_millis(250)) {
            bodies.push(request.body);
        }
        bodies
    }

    fn take_requests(&self, count: usize) -> Vec<RecordedRequest> {
        (0..count).map(|_| self.next_request()).collect()
    }
}

fn serve_fake_sink(
    listener: TcpListener,
    responses: Vec<SinkResponse>,
    tx: Sender<RecordedRequest>,
) {
    for response in responses {
        let Ok((stream, _)) = listener.accept() else {
            return;
        };
        handle_fake_sink_connection(stream, response, &tx);
    }
}

fn handle_fake_sink_connection(
    mut stream: TcpStream,
    response: SinkResponse,
    tx: &Sender<RecordedRequest>,
) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut request_line = String::new();
    reader.read_line(&mut request_line).unwrap();
    let path = request_line.split_whitespace().nth(1).unwrap().to_string();
    let mut headers = BTreeMap::new();
    let mut line = String::new();
    loop {
        line.clear();
        reader.read_line(&mut line).unwrap();
        if line == "\r\n" {
            break;
        }
        let (name, value) = line.trim_end().split_once(':').unwrap();
        headers.insert(name.to_ascii_lowercase(), value.trim().to_string());
    }

    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body_bytes = vec![0; content_length];
    reader.read_exact(&mut body_bytes).unwrap();
    let body = serde_json::from_slice(&body_bytes).unwrap();
    tx.send(RecordedRequest {
        path,
        headers,
        raw_body: body_bytes,
        body,
    })
    .unwrap();

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
            stream.write_all(head.as_bytes()).unwrap();
        }
    }
}

fn write_response(stream: &mut TcpStream, status: u16, reason: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).unwrap();
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
    stream.write_all(response.as_bytes()).unwrap();
}
