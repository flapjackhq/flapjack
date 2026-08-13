#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

#[path = "support/migration_fake_server.rs"]
mod migration_fake_server;
mod support;

use migration_fake_server::{FakeMigrationServer, RecordedRequest, StubResponse};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};
use support::{flapjack_cmd, http_request_with_headers, RunningServer, TempDir};

const FLAPJACK_API_KEY: &str = "fj_stage2_admin_secret";
const ALGOLIA_API_KEY: &str = "algolia_stage2_source_secret";
const SOURCE_API_KEY: &str = "provider_stage1_source_secret";
const ALGOLIA_TEST_BASE_URL_ENV: &str = "FLAPJACK_TEST_ALGOLIA_BASE_URL";
const JOB_ID: &str = "01890f8e-8b28-78e8-b542-8cfdcb2d4f24";
const EXIT_CONFIG: i32 = 2;
const EXIT_HTTP_REJECTION: i32 = 3;
const EXIT_TIMEOUT: i32 = 4;
const EXIT_FAILED_JOB: i32 = 5;
const EXIT_CANCELLED_JOB: i32 = 6;
const EXIT_CANCEL_TOO_LATE: i32 = 7;
const EXIT_ACK_TOO_EARLY: i32 = 8;
const EXIT_PREVIEW_HARD_REJECTIONS: i32 = 9;

struct UnresponsiveAlgoliaFixture {
    _listener: TcpListener,
    endpoint: String,
}

impl UnresponsiveAlgoliaFixture {
    fn start() -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("bind unresponsive Algolia source fixture");
        let endpoint = format!(
            "http://{}",
            listener
                .local_addr()
                .expect("unresponsive Algolia fixture address")
        );
        Self {
            _listener: listener,
            endpoint,
        }
    }

    fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

fn spawn_auth_server_with_algolia_fixture(data_dir: &str, fixture_endpoint: &str) -> RunningServer {
    RunningServer::spawn_auth_auto_port_with_env(
        data_dir,
        &[(ALGOLIA_TEST_BASE_URL_ENV, fixture_endpoint)],
    )
}

#[test]
fn startup_timeout_reaps_child_before_running_server_exists() {
    support::assert_startup_timeout_reaps_child_before_running_server_exists();
}

#[test]
fn migrate_subcommand_never_starts_server_or_binds_listener() {
    let occupied_listener = TcpListener::bind("127.0.0.1:0").expect("reserve server bind address");
    let occupied_addr = occupied_listener.local_addr().unwrap().to_string();

    support::flapjack_cmd()
        .arg("--bind-addr")
        .arg(occupied_addr)
        .arg("migrate")
        .arg("--help")
        .assert()
        .success();
}

#[test]
fn migrate_submits_expected_body_and_reports_terminal_success() {
    let server = FakeMigrationServer::start(vec![
        StubResponse::json(202, migration_status("submitted", "running")),
        StubResponse::json(
            200,
            json!({
                "jobId": JOB_ID,
                "phase": "activating",
                "disposition": "succeeded",
                "targetIndex": "products_copy",
                "topology": "single_node_only",
                "exportProgress": {"completed": 10, "total": 10},
                "createdAt": "2026-07-29T16:00:00Z",
                "updatedAt": "2026-07-29T16:00:03Z",
                "terminalAt": "2026-07-29T16:00:03Z",
                "settingsApplied": true,
                "objectsImported": {"imported": 3},
                "synonymsImported": {"imported": 2},
                "rulesImported": {"imported": 1},
                "warnings": []
            }),
        ),
    ]);

    let output = migrate_cmd(server.endpoint())
        .arg("--target-index")
        .arg("products_copy")
        .arg("--overwrite")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .clone();

    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON migration report");
    assert_eq!(
        report,
        json!({
            "jobId": JOB_ID,
            "phase": "activating",
            "disposition": "succeeded",
            "targetIndex": "products_copy",
            "topology": "single_node_only",
            "exportProgress": {"completed": 10, "total": 10},
            "createdAt": "2026-07-29T16:00:00Z",
            "updatedAt": "2026-07-29T16:00:03Z",
            "terminalAt": "2026-07-29T16:00:03Z",
            "settingsApplied": true,
            "objectsImported": {"imported": 3},
            "synonymsImported": {"imported": 2},
            "rulesImported": {"imported": 1}
        })
    );

    let requests = server.take_requests(2);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/1/migrations/algolia");
    assert_eq!(
        requests[0].body,
        json!({
            "appId": "UNREACHABLESTAGE2",
            "apiKey": ALGOLIA_API_KEY,
            "sourceIndex": "products",
            "targetIndex": "products_copy",
            "overwrite": true
        })
    );
    assert_eq!(requests[1].method, "GET");
    assert_eq!(requests[1].path, format!("/1/migrations/algolia/{JOB_ID}"));
    for request in requests {
        assert_migration_request_headers(&request);
        assert_eq!(
            request.header("content-type").as_deref(),
            Some("application/json")
        );
    }
}

#[test]
fn default_provider_submits_the_unchanged_algolia_payload() {
    let server = FakeMigrationServer::start(vec![StubResponse::json(
        202,
        migration_status("submitted", "succeeded"),
    )]);

    migrate_cmd(server.endpoint()).assert().success();

    let request = server.take_requests(1).remove(0);
    assert_provider_submit_request(
        &request,
        "/1/migrations/algolia",
        json!({
            "appId": "UNREACHABLESTAGE2",
            "apiKey": ALGOLIA_API_KEY,
            "sourceIndex": "products",
            "overwrite": false
        }),
    );
}

#[test]
fn algolia_key_flag_aliases_still_submit_the_algolia_payload() {
    let key_file_dir = TempDir::new("migrate_algolia_key_alias_file");
    let key_file = key_file_dir.root().join("source_key");
    std::fs::write(&key_file, ALGOLIA_API_KEY).expect("write source key fixture");

    for alias in [
        "--algolia-key-env",
        "--algolia-key-file",
        "--algolia-key-stdin",
    ] {
        let server = FakeMigrationServer::start(vec![StubResponse::json(
            202,
            migration_status("submitted", "succeeded"),
        )]);
        let mut command = flapjack_cmd();
        command.arg("migrate");
        add_flapjack_auth_args(&mut command, server.endpoint(), FLAPJACK_API_KEY);
        command
            .arg("--app-id")
            .arg("UNREACHABLESTAGE2")
            .arg("--source-index")
            .arg("products")
            .arg(alias);

        match alias {
            "--algolia-key-env" => {
                command
                    .arg("FJ_MIGRATE_TEST_ALGOLIA_KEY")
                    .env("FJ_MIGRATE_TEST_ALGOLIA_KEY", ALGOLIA_API_KEY);
            }
            "--algolia-key-file" => {
                command.arg(&key_file);
            }
            "--algolia-key-stdin" => {
                command.write_stdin(ALGOLIA_API_KEY);
            }
            _ => unreachable!("closed alias table"),
        }

        command.assert().success();
        let request = server.take_requests(1).remove(0);
        assert_provider_submit_request(
            &request,
            "/1/migrations/algolia",
            json!({
                "appId": "UNREACHABLESTAGE2",
                "apiKey": ALGOLIA_API_KEY,
                "sourceIndex": "products",
                "overwrite": false
            }),
        );
    }
}

#[test]
fn migrate_submits_meilisearch_payload_to_meilisearch_route() {
    let server = FakeMigrationServer::start(vec![StubResponse::json(
        202,
        migration_status("submitted", "succeeded"),
    )]);

    migrate_cmd_for_provider(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    .assert()
    .success();

    let request = server.take_requests(1).remove(0);
    assert_provider_submit_request(
        &request,
        "/1/migrations/meilisearch",
        json!({
            "endpoint": "https://tenant.meilisearch.io",
            "apiKey": SOURCE_API_KEY,
            "sourceIndex": "products",
            "overwrite": false
        }),
    );
}

#[test]
fn migrate_preview_human_output_reports_entries_summary_and_exit_code() {
    let server =
        FakeMigrationServer::start(vec![StubResponse::json(200, migration_preview_response(0))]);

    let output = migrate_preview_cmd(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    .assert()
    .code(0)
    .get_output()
    .clone();

    let stdout = String::from_utf8(output.stdout.clone()).expect("UTF-8 human preview report");
    let expected_lines = [
        "total_entries=2 hard_rejections=0 warnings=1 scope_gaps=1 source_indexes=1 source_records=37 report_digest=preview-contract-digest",
        "severity=Warning code=MeilisearchSettingNotMigrated resource=Settings jsonPath=$.stopWords",
        "severity=ScopeGap code=ProductNotMigrated resource=Analytics jsonPath=$",
    ];
    assert_eq!(stdout.lines().collect::<Vec<_>>(), expected_lines);
    assert_secrets_absent(&output, &[FLAPJACK_API_KEY, SOURCE_API_KEY]);

    let request = server.take_requests(1).remove(0);
    assert_provider_submit_request(
        &request,
        "/1/migrations/meilisearch/preview",
        json!({
            "endpoint": "https://tenant.meilisearch.io",
            "apiKey": SOURCE_API_KEY,
            "sourceIndex": "products",
            "overwrite": false
        }),
    );
}

#[test]
fn migrate_preview_json_reports_contract_fields_and_exit_code() {
    let server =
        FakeMigrationServer::start(vec![StubResponse::json(200, migration_preview_response(2))]);

    let output = migrate_preview_cmd(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    .arg("--json")
    .assert()
    .code(EXIT_PREVIEW_HARD_REJECTIONS)
    .get_output()
    .clone();

    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON preview report");
    assert_eq!(report, migration_preview_response(2));
    assert_eq!(report["report"]["summary"]["hardRejections"], json!(2));
    assert_eq!(report["report"]["summary"]["warnings"], json!(1));
    assert_eq!(report["report"]["summary"]["scopeGaps"], json!(1));
    assert_eq!(
        report["report"]["reportDigest"],
        json!("preview-contract-digest")
    );
    assert_secrets_absent(&output, &[FLAPJACK_API_KEY, SOURCE_API_KEY]);

    let request = server.take_requests(1).remove(0);
    assert_provider_submit_request(
        &request,
        "/1/migrations/meilisearch/preview",
        json!({
            "endpoint": "https://tenant.meilisearch.io",
            "apiKey": SOURCE_API_KEY,
            "sourceIndex": "products",
            "overwrite": false
        }),
    );
}

#[test]
fn migrate_preview_uses_operator_timeout_for_the_synchronous_request() {
    let server =
        FakeMigrationServer::start(vec![StubResponse::json(200, migration_preview_response(0))
            .delayed_by(Duration::from_secs(31))]);

    let output = migrate_preview_cmd(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    // The margin over the retired 30s submit cap is wide so a scheduling
    // overshoot on a loaded host cannot flip this success assertion.
    .args(["--timeout", "10m"])
    .assert()
    .code(0)
    .get_output()
    .clone();

    assert_secrets_absent(&output, &[FLAPJACK_API_KEY, SOURCE_API_KEY]);
    let request = server.take_requests(1).remove(0);
    assert_provider_submit_request(
        &request,
        "/1/migrations/meilisearch/preview",
        json!({
            "endpoint": "https://tenant.meilisearch.io",
            "apiKey": SOURCE_API_KEY,
            "sourceIndex": "products",
            "overwrite": false
        }),
    );
}

#[test]
fn migrate_preview_gives_up_when_the_operator_timeout_is_shorter_than_the_response() {
    let server =
        FakeMigrationServer::start(vec![StubResponse::json(200, migration_preview_response(0))
            .delayed_by(Duration::from_secs(30))]);

    let output = migrate_preview_cmd(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    .args(["--timeout", "1s"])
    .assert()
    .code(EXIT_TIMEOUT)
    .get_output()
    .clone();

    assert_secrets_absent(&output, &[FLAPJACK_API_KEY, SOURCE_API_KEY]);
}

#[test]
fn migrate_preview_rejects_submit_only_flags_before_contacting_server() {
    for (flag, values) in [
        ("--overwrite", Vec::new()),
        ("--target-index", vec!["products_copy"]),
        ("--poll-interval", vec!["1s"]),
    ] {
        let mut command = flapjack_cmd();
        command.arg("migrate");
        add_flapjack_auth_args(
            &mut command,
            "http://127.0.0.1:1".to_string(),
            FLAPJACK_API_KEY,
        );
        command
            .arg(flag)
            .args(values)
            .arg("preview")
            .arg("--source-provider")
            .arg("meilisearch")
            .arg("--source-endpoint")
            .arg("https://tenant.meilisearch.io")
            .arg("--source-key-env")
            .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
            .arg("--source-index")
            .arg("products")
            .env("FJ_MIGRATE_TEST_SOURCE_KEY", SOURCE_API_KEY);

        let output = command.assert().code(EXIT_CONFIG).get_output().clone();

        let combined = combined_output(&output);
        assert!(
            combined.contains(&format!("{flag} is only valid when submitting a migration")),
            "{flag}: {combined}"
        );
    }
}

#[test]
fn migrate_preview_missing_source_index_names_the_required_flag() {
    let output = flapjack_cmd()
        .arg("migrate")
        .arg("preview")
        .arg("--endpoint")
        .arg("http://127.0.0.1:1")
        .arg("--source-provider")
        .arg("meilisearch")
        .arg("--source-endpoint")
        .arg("https://tenant.meilisearch.io")
        .arg("--source-key-env")
        .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
        .arg("--application-id")
        .arg("test-owner")
        .arg("--api-key-env")
        .arg("FJ_MIGRATE_TEST_API_KEY")
        .arg("--json")
        .env("FJ_MIGRATE_TEST_SOURCE_KEY", SOURCE_API_KEY)
        .env("FJ_MIGRATE_TEST_API_KEY", FLAPJACK_API_KEY)
        .assert()
        .code(EXIT_CONFIG)
        .get_output()
        .clone();

    let report: Value =
        serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
    assert_eq!(report["message"], json!("--source-index is required"));
}

#[test]
fn migrate_preview_redacts_non_success_and_incompatible_response_failures() {
    for response in [
        StubResponse::text(
            403,
            format!("credentials {FLAPJACK_API_KEY} and {SOURCE_API_KEY} rejected"),
        ),
        StubResponse::text(
            200,
            format!("not JSON: {FLAPJACK_API_KEY} {SOURCE_API_KEY}"),
        ),
        StubResponse::json(
            200,
            json!({"report": {"summary": {"hardRejections": 0}}, "sourceCounts": {}}),
        ),
    ] {
        let server = FakeMigrationServer::start(vec![response]);

        let output = migrate_preview_cmd(
            server.endpoint(),
            "meilisearch",
            "https://tenant.meilisearch.io",
            SOURCE_API_KEY,
        )
        .arg("--json")
        .assert()
        .code(EXIT_HTTP_REJECTION)
        .get_output()
        .clone();

        assert_preview_http_rejection_is_redacted(&output);
        let request = server.take_requests(1).remove(0);
        assert_eq!(request.path, "/1/migrations/meilisearch/preview");
    }
}

#[test]
fn migrate_preview_rejects_internally_inconsistent_reports() {
    let mut mismatched_summary = migration_preview_response(0);
    mismatched_summary["report"]["summary"]["warnings"] = json!(0);
    let mut unknown_severity = migration_preview_response(0);
    unknown_severity["report"]["entries"][0]["severity"] = json!("Advisory");

    for response in [mismatched_summary, unknown_severity] {
        let server = FakeMigrationServer::start(vec![StubResponse::json(200, response)]);
        let output = migrate_preview_cmd(
            server.endpoint(),
            "meilisearch",
            "https://tenant.meilisearch.io",
            SOURCE_API_KEY,
        )
        .arg("--json")
        .assert()
        .code(EXIT_HTTP_REJECTION)
        .get_output()
        .clone();

        assert_preview_http_rejection_is_redacted(&output);
        let request = server.take_requests(1).remove(0);
        assert_eq!(request.path, "/1/migrations/meilisearch/preview");
    }
}

#[test]
fn migrate_preview_routes_algolia_and_typesense_to_the_selected_provider() {
    for (provider, endpoint, expected_path, expected_body) in [
        (
            "algolia",
            "",
            "/1/migrations/algolia/preview",
            json!({
                "appId": "UNREACHABLESTAGE2",
                "apiKey": SOURCE_API_KEY,
                "sourceIndex": "products",
                "overwrite": false
            }),
        ),
        (
            "typesense",
            "https://tenant.typesense.net",
            "/1/migrations/typesense/preview",
            json!({
                "node": "https://tenant.typesense.net",
                "apiKey": SOURCE_API_KEY,
                "sourceIndex": "products",
                "sourceWriteFrozen": true,
                "overwrite": false
            }),
        ),
    ] {
        let server = FakeMigrationServer::start(vec![StubResponse::json(
            200,
            migration_preview_response(0),
        )]);

        migrate_preview_cmd(server.endpoint(), provider, endpoint, SOURCE_API_KEY)
            .assert()
            .success();

        let request = server.take_requests(1).remove(0);
        assert_provider_submit_request(&request, expected_path, expected_body);
    }
}

#[test]
fn migrate_submits_typesense_payload_to_typesense_route() {
    let server = FakeMigrationServer::start(vec![StubResponse::json(
        202,
        migration_status("submitted", "succeeded"),
    )]);

    migrate_cmd_for_provider(
        server.endpoint(),
        "typesense",
        "https://tenant.typesense.net",
        SOURCE_API_KEY,
    )
    .assert()
    .success();

    let request = server.take_requests(1).remove(0);
    assert_provider_submit_request(
        &request,
        "/1/migrations/typesense",
        json!({
            "node": "https://tenant.typesense.net",
            "apiKey": SOURCE_API_KEY,
            "sourceIndex": "products",
            "sourceWriteFrozen": true,
            "overwrite": false
        }),
    );
}

#[test]
fn source_write_frozen_typesense_submit_and_preview_emit_attestation() {
    for (action, expected_path) in [
        ("submit", "/1/migrations/typesense"),
        ("preview", "/1/migrations/typesense/preview"),
    ] {
        let response = match action {
            "submit" => StubResponse::json(202, migration_status("submitted", "succeeded")),
            "preview" => StubResponse::json(200, migration_preview_response(0)),
            _ => unreachable!("closed action table"),
        };
        let server = FakeMigrationServer::start(vec![response]);

        match action {
            "submit" => {
                migrate_cmd_for_provider(
                    server.endpoint(),
                    "typesense",
                    "https://tenant.typesense.net",
                    SOURCE_API_KEY,
                )
                .assert()
                .success();
            }
            "preview" => {
                migrate_preview_cmd(
                    server.endpoint(),
                    "typesense",
                    "https://tenant.typesense.net",
                    SOURCE_API_KEY,
                )
                .assert()
                .success();
            }
            _ => unreachable!("closed action table"),
        }

        let request = server.take_requests(1).remove(0);
        assert_provider_submit_request(
            &request,
            expected_path,
            json!({
                "node": "https://tenant.typesense.net",
                "apiKey": SOURCE_API_KEY,
                "sourceIndex": "products",
                "sourceWriteFrozen": true,
                "overwrite": false
            }),
        );
    }
}

#[test]
fn source_write_frozen_missing_flag_fails_typesense_locally_before_http() {
    let output = migrate_cmd_for_provider_without_write_freeze(
        "http://127.0.0.1:1".to_string(),
        "typesense",
        "https://tenant.typesense.net",
        SOURCE_API_KEY,
    )
    .arg("--json")
    .assert()
    .code(EXIT_CONFIG)
    .get_output()
    .clone();
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
    assert_eq!(report["errorType"], json!("config"));
    assert_eq!(report["exitCode"], json!(EXIT_CONFIG));
    assert_eq!(
        report["message"],
        json!("--source-write-frozen is required for --source-provider typesense")
    );
}

#[test]
fn source_write_frozen_is_typesense_only_and_documented_in_help() {
    let help = flapjack_cmd()
        .arg("migrate")
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(help).expect("help is UTF-8");
    assert!(help.contains("--source-write-frozen"), "{help}");

    for (provider, expected_message) in [
        (
            "algolia",
            "--source-write-frozen is not valid with --source-provider algolia",
        ),
        (
            "meilisearch",
            "--source-write-frozen is not valid with --source-provider meilisearch",
        ),
    ] {
        let mut command = if provider == "algolia" {
            migrate_cmd("http://127.0.0.1:1".to_string())
        } else {
            migrate_cmd_for_provider_without_write_freeze(
                "http://127.0.0.1:1".to_string(),
                provider,
                "https://tenant.meilisearch.io",
                SOURCE_API_KEY,
            )
        };
        command.arg("--source-write-frozen").arg("--json");
        let output = command.assert().code(EXIT_CONFIG).get_output().clone();
        let report: Value =
            serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
        assert_eq!(report["message"], json!(expected_message), "{provider}");
    }
}

#[test]
fn migrate_polls_status_on_the_selected_provider_route() {
    let server = FakeMigrationServer::start(vec![
        StubResponse::json(202, migration_status("submitted", "running")),
        StubResponse::json(200, migration_status("activating", "succeeded")),
    ]);

    migrate_cmd_for_provider(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    .assert()
    .success();

    let requests = server.take_requests(2);
    assert_eq!(requests[1].method, "GET");
    assert_eq!(
        requests[1].path,
        format!("/1/migrations/meilisearch/{JOB_ID}")
    );
    assert_migration_request_headers(&requests[1]);
}

#[test]
fn cancel_and_ack_use_the_selected_provider_route() {
    let cancel_server = FakeMigrationServer::start(vec![StubResponse::json(
        200,
        migration_status("cancel_requested", "running"),
    )]);
    migrate_action_cmd_for_provider(
        cancel_server.endpoint(),
        "cancel",
        JOB_ID,
        FLAPJACK_API_KEY,
        "typesense",
    )
    .assert()
    .success();

    let ack_server = FakeMigrationServer::start(vec![StubResponse::text(204, String::new())]);
    migrate_action_cmd_for_provider(
        ack_server.endpoint(),
        "ack",
        JOB_ID,
        FLAPJACK_API_KEY,
        "typesense",
    )
    .assert()
    .success();

    assert_action_requests(
        cancel_server.take_requests(1),
        &format!("/1/migrations/typesense/{JOB_ID}/cancel"),
    );
    assert_action_requests(
        ack_server.take_requests(1),
        &format!("/1/migrations/typesense/{JOB_ID}/acknowledge"),
    );
}

#[test]
fn migrate_exits_nonzero_and_names_the_server_error_on_failed_job() {
    let data = TempDir::new("migrate_failed_real_server");
    let source_fixture = FakeMigrationServer::start(vec![StubResponse::text(
        500,
        "source fixture failure".to_string(),
    )]);
    let server = spawn_auth_server_with_algolia_fixture(data.path(), &source_fixture.endpoint());
    let admin_key = std::fs::read_to_string(data.root().join(".admin_key"))
        .expect("auth server should persist its admin key");

    let output = migrate_cmd_with_key(
        format!("http://{}", server.bind_addr()),
        admin_key.trim(),
        "unreachable-stage2-source",
    )
    .arg("--timeout")
    .arg("20s")
    .arg("--poll-interval")
    .arg("10ms")
    .arg("--json")
    .assert()
    .code(EXIT_FAILED_JOB)
    .get_output()
    .clone();

    let combined = combined_output(&output);
    assert!(combined.contains(JOB_ID) || combined.contains("jobId"));
    assert!(combined.contains("failed"), "output was: {combined}");
    assert!(!combined.contains("objectsImported"));
    assert!(!combined.contains("synonymsImported"));
    assert!(!combined.contains("rulesImported"));
}

#[test]
fn real_server_rejects_non_vendor_meilisearch_endpoint() {
    let data = TempDir::new("migrate_meilisearch_rejection_real_server");
    let server = RunningServer::spawn_auth_auto_port(data.path());
    let admin_key = std::fs::read_to_string(data.root().join(".admin_key"))
        .expect("auth server should persist its admin key");

    let mut command = flapjack_cmd();
    command.arg("migrate");
    add_flapjack_auth_args(
        &mut command,
        format!("http://{}", server.bind_addr()),
        admin_key.trim(),
    );
    let output = command
        .arg("--source-provider")
        .arg("meilisearch")
        .arg("--source-endpoint")
        .arg("https://offline.invalid")
        .arg("--source-key-env")
        .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
        .arg("--source-index")
        .arg("products")
        .arg("--json")
        .env("FJ_MIGRATE_TEST_SOURCE_KEY", SOURCE_API_KEY)
        .assert()
        .code(EXIT_HTTP_REJECTION)
        .get_output()
        .clone();

    assert_json_http_rejection(
        &output,
        "migration submission returned HTTP 400: {\"message\":\"Meilisearch Cloud endpoint is not allowed\",\"status\":400}",
    );
    assert_secrets_absent(&output, &[admin_key.trim(), SOURCE_API_KEY]);
}

#[test]
fn real_server_rejects_non_vendor_typesense_endpoint() {
    let data = TempDir::new("migrate_typesense_rejection_real_server");
    let server = RunningServer::spawn_auth_auto_port(data.path());
    let admin_key = std::fs::read_to_string(data.root().join(".admin_key"))
        .expect("auth server should persist its admin key");

    let mut command = flapjack_cmd();
    command.arg("migrate");
    add_flapjack_auth_args(
        &mut command,
        format!("http://{}", server.bind_addr()),
        admin_key.trim(),
    );
    let output = command
        .arg("--source-provider")
        .arg("typesense")
        .arg("--source-endpoint")
        .arg("https://offline.invalid")
        .arg("--source-key-env")
        .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
        .arg("--source-index")
        .arg("products")
        .arg("--source-write-frozen")
        .arg("--json")
        .env("FJ_MIGRATE_TEST_SOURCE_KEY", SOURCE_API_KEY)
        .assert()
        .code(EXIT_HTTP_REJECTION)
        .get_output()
        .clone();

    assert_json_http_rejection(
        &output,
        "migration submission returned HTTP 400: {\"message\":\"Typesense Cloud endpoint is not allowed\",\"status\":400}",
    );
    assert_secrets_absent(&output, &[admin_key.trim(), SOURCE_API_KEY]);
}

#[test]
fn migrate_exits_nonzero_on_timeout_without_claiming_success() {
    let mut responses = vec![StubResponse::json(
        202,
        migration_status("submitted", "running"),
    )];
    responses
        .extend((0..20).map(|_| StubResponse::json(200, migration_status("exporting", "running"))));
    let server = FakeMigrationServer::start(responses);

    let output = migrate_cmd(server.endpoint())
        .args(["--timeout", "30ms", "--poll-interval", "5ms"])
        .assert()
        .code(EXIT_TIMEOUT)
        .get_output()
        .clone();

    assert_ne!(EXIT_TIMEOUT, EXIT_FAILED_JOB);
    let combined = combined_output(&output).to_ascii_lowercase();
    assert!(combined.contains("timed out"), "output was: {combined}");
    assert!(!combined.contains("succeeded"));
    assert!(!combined.contains("success"));
    assert!(!combined.contains("objects imported"));
    assert!(!combined.contains("objectsimported"));
}

#[test]
fn migrate_request_timeout_uses_timeout_exit_code() {
    let server = FakeMigrationServer::start(vec![
        StubResponse::json(202, migration_status("submitted", "running")),
        StubResponse::json(200, migration_status("exporting", "running"))
            .delayed_by(Duration::from_millis(250)),
    ]);

    let output = migrate_cmd(server.endpoint())
        .args(["--timeout", "40ms", "--poll-interval", "5ms"])
        .assert()
        .code(EXIT_TIMEOUT)
        .get_output()
        .clone();

    let combined = combined_output(&output).to_ascii_lowercase();
    assert!(combined.contains("timed out"), "output was: {combined}");
    assert!(!combined.contains("http_rejection"));
    assert!(!combined.contains("succeeded"));
}

#[test]
fn migrate_json_terminal_failure_reports_status_and_exit_metadata() {
    let server = FakeMigrationServer::start(vec![
        StubResponse::json(202, migration_status("submitted", "running")),
        StubResponse::json(
            200,
            json!({
                "jobId": JOB_ID,
                "phase": "importing",
                "disposition": "failed",
                "targetIndex": "products",
                "topology": "single_node_only",
                "exportProgress": {"completed": 3, "total": 10},
                "createdAt": "2026-07-29T16:00:00Z",
                "updatedAt": "2026-07-29T16:00:04Z",
                "terminalAt": "2026-07-29T16:00:04Z",
                "warnings": [format!(
                    "source migration failed for {ALGOLIA_API_KEY}"
                )]
            }),
        ),
    ]);

    let output = migrate_cmd(server.endpoint())
        .arg("--json")
        .assert()
        .code(EXIT_FAILED_JOB)
        .get_output()
        .clone();

    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON failure report");
    assert_eq!(report["errorType"], json!("failed_job"));
    assert_eq!(report["exitCode"], json!(EXIT_FAILED_JOB));
    assert_eq!(report["jobId"], json!(JOB_ID));
    assert_eq!(report["phase"], json!("importing"));
    assert_eq!(report["disposition"], json!("failed"));
    assert_eq!(report["targetIndex"], json!("products"));
    assert_eq!(report["topology"], json!("single_node_only"));
    assert_eq!(report["exportProgress"]["completed"], json!(3));
    assert_eq!(report["exportProgress"]["total"], json!(10));
    assert_eq!(report["createdAt"], json!("2026-07-29T16:00:00Z"));
    assert_eq!(report["updatedAt"], json!("2026-07-29T16:00:04Z"));
    assert_eq!(report["terminalAt"], json!("2026-07-29T16:00:04Z"));
    assert_eq!(
        report["warnings"][0],
        json!("source migration failed for [REDACTED]")
    );
    let combined = combined_output(&output);
    assert!(!combined.contains(ALGOLIA_API_KEY));
}

#[test]
fn migrate_human_output_reports_terminal_cancelled_status() {
    let server = FakeMigrationServer::start(vec![
        StubResponse::json(202, migration_status("submitted", "running")),
        StubResponse::json(
            200,
            json!({
                "jobId": JOB_ID,
                "phase": "importing",
                "disposition": "cancelled",
                "targetIndex": "products",
                "topology": "single_node_only",
                "warnings": [format!(
                    "source migration was cancelled for {ALGOLIA_API_KEY}"
                )]
            }),
        ),
    ]);

    let output = migrate_cmd(server.endpoint())
        .assert()
        .code(EXIT_CANCELLED_JOB)
        .get_output()
        .clone();

    assert_ne!(EXIT_CANCELLED_JOB, EXIT_FAILED_JOB);
    assert_ne!(EXIT_CANCELLED_JOB, EXIT_TIMEOUT);
    let combined = combined_output(&output);
    assert!(combined.contains(&format!("job_id={JOB_ID}")));
    assert!(combined.contains("phase=importing"));
    assert!(combined.contains("disposition=cancelled"));
    assert!(combined.contains("target_index=products"));
    assert!(combined.contains("topology=single_node_only"));
    assert!(combined.contains("warning=\"source migration was cancelled for [REDACTED]\""));
    assert!(!combined.contains(ALGOLIA_API_KEY));
    assert!(!combined.contains("objects_imported"));
    assert!(!combined.contains("synonyms_imported"));
    assert!(!combined.contains("rules_imported"));
}

#[test]
fn cancel_maps_too_late_to_distinct_nonzero_exit() {
    assert_action_refusal(ActionRefusalExpectation {
        action: "cancel",
        path: format!("/1/migrations/algolia/{JOB_ID}/cancel"),
        server_code: "cancel_too_late",
        message: "Migration job has already reached the publication commit boundary",
        exit_code: EXIT_CANCEL_TOO_LATE,
    });
}

#[test]
fn acknowledge_refuses_non_terminal_job_with_named_code() {
    assert_action_refusal(ActionRefusalExpectation {
        action: "ack",
        path: format!("/1/migrations/algolia/{JOB_ID}/acknowledge"),
        server_code: "migration_ack_too_early",
        message: "Migration job must be terminal before it can be acknowledged",
        exit_code: EXIT_ACK_TOO_EARLY,
    });
}

#[test]
fn acknowledge_terminal_job_reports_only_honest_action_result() {
    let server = FakeMigrationServer::start(vec![
        StubResponse::text(204, String::new()),
        StubResponse::text(204, String::new()),
    ]);

    let human_output =
        migrate_action_cmd_with_key(server.endpoint(), "ack", JOB_ID, FLAPJACK_API_KEY)
            .assert()
            .success()
            .get_output()
            .clone();
    let json_output =
        migrate_action_cmd_with_key(server.endpoint(), "ack", JOB_ID, FLAPJACK_API_KEY)
            .arg("--json")
            .assert()
            .success()
            .get_output()
            .clone();

    assert_eq!(
        String::from_utf8(human_output.stdout).unwrap().trim(),
        format!("job_id={JOB_ID} acknowledged=true")
    );
    let report: Value =
        serde_json::from_slice(&json_output.stdout).expect("JSON acknowledgement report");
    assert_eq!(
        report,
        json!({
            "jobId": JOB_ID,
            "acknowledged": true
        })
    );
    assert_action_requests(
        server.take_requests(2),
        &format!("/1/migrations/algolia/{JOB_ID}/acknowledge"),
    );
}

#[test]
fn cancel_human_success_redacts_server_echoed_flapjack_key() {
    let server = cancel_success_server_echoing_flapjack_key();

    let output = migrate_action_cmd_with_key(server.endpoint(), "cancel", JOB_ID, FLAPJACK_API_KEY)
        .assert()
        .success()
        .get_output()
        .clone();

    let combined = combined_output(&output);
    assert!(combined.contains(&format!("job_id={JOB_ID}")));
    assert!(combined.contains("disposition=running"));
    assert!(combined.contains("warning=\"cancel warning for [REDACTED]\""));
    assert!(!combined.contains(FLAPJACK_API_KEY));
    assert_action_requests(
        server.take_requests(1),
        &format!("/1/migrations/algolia/{JOB_ID}/cancel"),
    );
}

#[test]
fn cancel_json_success_redacts_server_echoed_flapjack_key() {
    let server = cancel_success_server_echoing_flapjack_key();

    let output = migrate_action_cmd_with_key(server.endpoint(), "cancel", JOB_ID, FLAPJACK_API_KEY)
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .clone();

    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON cancel status");
    assert_eq!(report["jobId"], json!(JOB_ID));
    assert_eq!(report["disposition"], json!("running"));
    assert_eq!(
        report["warnings"][0],
        json!("cancel warning for [REDACTED]")
    );
    let combined = combined_output(&output);
    assert!(!combined.contains(FLAPJACK_API_KEY));
    assert_action_requests(
        server.take_requests(1),
        &format!("/1/migrations/algolia/{JOB_ID}/cancel"),
    );
}

#[test]
fn action_missing_endpoint_fails_without_consuming_api_key_stdin() {
    let output = migrate_cancel_with_open_api_key_stdin(None, JOB_ID);
    assert_eq!(output.status.code(), Some(EXIT_CONFIG));
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
    assert_eq!(report["errorType"], json!("config"));
    assert_eq!(report["exitCode"], json!(EXIT_CONFIG));
    assert!(report["message"]
        .as_str()
        .is_some_and(|message| message.contains("--endpoint is required")));
}

#[test]
fn malformed_action_job_id_is_local_config_error() {
    let output = migrate_cancel_with_open_api_key_stdin(Some("http://127.0.0.1:1"), "not-a-job-id");

    assert_eq!(output.status.code(), Some(EXIT_CONFIG));
    let report: Value =
        serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
    assert_eq!(report["errorType"], json!("config"));
    assert_eq!(report["exitCode"], json!(EXIT_CONFIG));
    assert!(report["message"]
        .as_str()
        .is_some_and(|message| message.contains("--job-id")));
    let combined = combined_output(&output);
    assert!(!combined.contains("server returned"));
    assert!(!combined.contains("http_rejection"));
}

#[test]
fn migrate_rejects_remote_http_endpoint_before_sending_secrets() {
    let output = migrate_cmd_with_key(
        "http://example.com:7700".to_string(),
        FLAPJACK_API_KEY,
        ALGOLIA_API_KEY,
    )
    .assert()
    .code(EXIT_CONFIG)
    .get_output()
    .clone();

    let combined = combined_output(&output);
    assert!(
        combined.contains("--endpoint must use https unless it targets localhost or a loopback IP")
    );
    assert!(!combined.contains(FLAPJACK_API_KEY));
    assert!(!combined.contains(ALGOLIA_API_KEY));
}

#[test]
fn provider_connection_flags_are_mutually_exclusive() {
    let cases = [
        ConfigRefusalCase {
            provider: None,
            app_id: None,
            source_endpoint: None,
            expected_message: "--app-id is required",
        },
        ConfigRefusalCase {
            provider: None,
            app_id: Some("UNREACHABLESTAGE1"),
            source_endpoint: Some("https://tenant.meilisearch.io"),
            expected_message: "--source-endpoint is not valid with --source-provider algolia",
        },
        ConfigRefusalCase {
            provider: Some("meilisearch"),
            app_id: Some("UNREACHABLESTAGE1"),
            source_endpoint: Some("https://tenant.meilisearch.io"),
            expected_message: "--app-id is not valid with --source-provider meilisearch",
        },
        ConfigRefusalCase {
            provider: Some("typesense"),
            app_id: Some("UNREACHABLESTAGE1"),
            source_endpoint: Some("https://tenant.typesense.net"),
            expected_message: "--app-id is not valid with --source-provider typesense",
        },
        ConfigRefusalCase {
            provider: Some("meilisearch"),
            app_id: None,
            source_endpoint: None,
            expected_message: "--source-endpoint is required for --source-provider meilisearch",
        },
        ConfigRefusalCase {
            provider: Some("typesense"),
            app_id: None,
            source_endpoint: None,
            expected_message: "--source-endpoint is required for --source-provider typesense",
        },
    ];

    for case in cases {
        let output = migrate_config_refusal_cmd(case)
            .assert()
            .code(EXIT_CONFIG)
            .get_output()
            .clone();
        let report: Value =
            serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
        assert_eq!(report["errorType"], json!("config"), "{case:?}");
        assert_eq!(report["exitCode"], json!(EXIT_CONFIG), "{case:?}");
        assert_eq!(report["message"], json!(case.expected_message), "{case:?}");
    }
}

#[test]
fn source_endpoint_validation_is_syntactic_only() {
    let cases = [
        (
            "://missing-scheme",
            "invalid --source-endpoint",
            "malformed URL should name the source endpoint flag",
        ),
        (
            "ftp://example.com",
            "--source-endpoint must be an absolute http or https URL",
            "non-http scheme should be refused locally",
        ),
        (
            "https://",
            "invalid --source-endpoint",
            "missing host should be refused locally",
        ),
    ];

    for (source_endpoint, expected_message, label) in cases {
        let output = migrate_cmd_for_provider(
            "http://127.0.0.1:1".to_string(),
            "meilisearch",
            source_endpoint,
            SOURCE_API_KEY,
        )
        .arg("--json")
        .assert()
        .code(EXIT_CONFIG)
        .get_output()
        .clone();
        let report: Value =
            serde_json::from_slice(&output.stdout).expect("JSON local configuration failure");
        assert_eq!(report["errorType"], json!("config"), "{label}");
        assert_eq!(report["exitCode"], json!(EXIT_CONFIG), "{label}");
        assert!(
            report["message"]
                .as_str()
                .is_some_and(|message| message.contains(expected_message)),
            "{label}: {report}"
        );
    }
}

#[test]
fn actions_reject_every_submit_only_flag() {
    let submit_only_arguments: &[(&str, &[&str])] = &[
        ("--app-id", &["UNREACHABLESTAGE3"]),
        ("--source-endpoint", &["https://tenant.meilisearch.io"]),
        ("--source-key-env", &["FJ_MIGRATE_TEST_ALGOLIA_KEY"]),
        ("--source-key-file", &["unused_algolia_key_file"]),
        ("--source-key-stdin", &[]),
        ("--algolia-key-env", &["FJ_MIGRATE_TEST_ALGOLIA_KEY"]),
        ("--algolia-key-file", &["unused_algolia_key_file"]),
        ("--algolia-key-stdin", &[]),
        ("--source-index", &["products"]),
        ("--target-index", &["products_copy"]),
        ("--overwrite", &[]),
        ("--poll-interval", &["1s"]),
    ];

    for action in ["cancel", "ack"] {
        for (flag, values) in submit_only_arguments {
            let report = action_rejection_report(action, flag, values);
            assert_eq!(report["errorType"], json!("config"), "{action} {flag}");
            assert_eq!(report["exitCode"], json!(EXIT_CONFIG), "{action} {flag}");
            assert!(
                report["message"]
                    .as_str()
                    .is_some_and(|message| message.contains(flag)),
                "{action} did not identify ignored flag {flag}: {report}"
            );
        }
    }
}

#[test]
fn actions_offer_preview_only_for_the_flags_preview_accepts() {
    for action in ["cancel", "ack"] {
        let source_flag_report =
            action_rejection_report(action, "--app-id", &["UNREACHABLESTAGE3"]);
        assert_eq!(
            source_flag_report["message"],
            json!("--app-id is only valid when submitting a migration or previewing one"),
            "{action} must offer preview as a retry for source connection flags"
        );

        for (flag, values) in [
            ("--target-index", &["products_copy"][..]),
            ("--overwrite", &[][..]),
            ("--poll-interval", &["1s"][..]),
        ] {
            let report = action_rejection_report(action, flag, values);
            assert_eq!(
                report["message"],
                json!(format!("{flag} is only valid when submitting a migration")),
                "{action} must not offer preview as a retry for a submit-only flag that \
                 run_preview itself rejects"
            );
        }
    }
}

fn action_rejection_report(action: &str, flag: &str, values: &[&str]) -> Value {
    let mut command = flapjack_cmd();
    command
        .arg("migrate")
        .arg("--endpoint")
        .arg("http://127.0.0.1:1")
        .arg("--api-key-env")
        .arg("FJ_MIGRATE_TEST_API_KEY")
        .arg(flag)
        .args(values)
        .arg(action)
        .arg("--job-id")
        .arg(JOB_ID)
        .arg("--json")
        .env("FJ_MIGRATE_TEST_API_KEY", FLAPJACK_API_KEY)
        .env("FJ_MIGRATE_TEST_ALGOLIA_KEY", ALGOLIA_API_KEY);

    let output = command.assert().code(EXIT_CONFIG).get_output().clone();
    serde_json::from_slice(&output.stdout).expect("JSON local configuration failure")
}

#[test]
fn cancel_of_a_real_owned_job_succeeds_without_stub_transport() {
    let data = TempDir::new("migrate_cancel_real_server");
    let source_fixture = UnresponsiveAlgoliaFixture::start();
    let server = spawn_auth_server_with_algolia_fixture(data.path(), source_fixture.endpoint());
    let admin_key = std::fs::read_to_string(data.root().join(".admin_key"))
        .expect("auth server should persist its admin key");
    let request_body = json!({
        "appId": "UNREACHABLESTAGE3",
        "apiKey": "algolia_stage3_test_key",
        "sourceIndex": "products",
        "overwrite": false
    })
    .to_string();
    let headers = [
        ("x-algolia-application-id", "test-owner"),
        ("x-algolia-api-key", admin_key.trim()),
    ];
    let admission = http_request_with_headers(
        server.bind_addr(),
        "POST",
        "/1/migrations/algolia",
        &headers,
        Some(&request_body),
    )
    .expect("direct migration admission should return an HTTP response");
    assert_eq!(
        admission.status, 202,
        "direct migration admission failed: {}",
        admission.body
    );
    let admission_body: Value =
        serde_json::from_str(&admission.body).expect("migration admission JSON");
    let job_id = admission_body["jobId"]
        .as_str()
        .expect("migration admission must return jobId");

    let output = migrate_action_cmd_with_key(
        format!("http://{}", server.bind_addr()),
        "cancel",
        job_id,
        admin_key.trim(),
    )
    .arg("--json")
    .assert()
    .success()
    .get_output()
    .clone();

    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON cancel status");
    assert_eq!(report["jobId"], json!(job_id));
    assert!(
        report["disposition"].is_string(),
        "cancel response must print the server-returned disposition: {report}"
    );
    assert!(
        report.get("cancelRequested").is_none(),
        "CLI must not fabricate cancelRequested: {report}"
    );
}

#[test]
fn real_server_wrong_provider_cancel_is_not_found_before_mutation() {
    let data = TempDir::new("migrate_wrong_provider_cancel_real_server");
    let source_fixture = UnresponsiveAlgoliaFixture::start();
    let server = spawn_auth_server_with_algolia_fixture(data.path(), source_fixture.endpoint());
    let admin_key = std::fs::read_to_string(data.root().join(".admin_key"))
        .expect("auth server should persist its admin key");
    let request_body = json!({
        "appId": "UNREACHABLESTAGE3",
        "apiKey": "algolia_stage3_test_key",
        "sourceIndex": "products",
        "overwrite": false
    })
    .to_string();
    let headers = [
        ("x-algolia-application-id", "test-owner"),
        ("x-algolia-api-key", admin_key.trim()),
    ];
    let admission = http_request_with_headers(
        server.bind_addr(),
        "POST",
        "/1/migrations/algolia",
        &headers,
        Some(&request_body),
    )
    .expect("direct migration admission should return an HTTP response");
    assert_eq!(
        admission.status, 202,
        "direct migration admission failed: {}",
        admission.body
    );
    let admission_body: Value =
        serde_json::from_str(&admission.body).expect("migration admission JSON");
    let job_id = admission_body["jobId"]
        .as_str()
        .expect("migration admission must return jobId");

    let output = migrate_action_cmd_for_provider(
        format!("http://{}", server.bind_addr()),
        "cancel",
        job_id,
        admin_key.trim(),
        "typesense",
    )
    .arg("--json")
    .assert()
    .code(EXIT_HTTP_REJECTION)
    .get_output()
    .clone();

    assert_json_http_rejection(
        &output,
        "migration cancellation returned HTTP 404: code=migration_job_not_found status=404 message=Migration job not found",
    );
    assert_secrets_absent(&output, &[admin_key.trim()]);
}

#[test]
fn real_server_wrong_provider_ack_is_not_found_before_mutation() {
    let data = TempDir::new("migrate_wrong_provider_ack_real_server");
    let source_fixture = UnresponsiveAlgoliaFixture::start();
    let server = spawn_auth_server_with_algolia_fixture(data.path(), source_fixture.endpoint());
    let admin_key = std::fs::read_to_string(data.root().join(".admin_key"))
        .expect("auth server should persist its admin key");
    let request_body = json!({
        "appId": "UNREACHABLESTAGE3",
        "apiKey": "algolia_stage3_test_key",
        "sourceIndex": "products",
        "overwrite": false
    })
    .to_string();
    let headers = [
        ("x-algolia-application-id", "test-owner"),
        ("x-algolia-api-key", admin_key.trim()),
    ];
    let admission = http_request_with_headers(
        server.bind_addr(),
        "POST",
        "/1/migrations/algolia",
        &headers,
        Some(&request_body),
    )
    .expect("direct migration admission should return an HTTP response");
    assert_eq!(
        admission.status, 202,
        "direct migration admission failed: {}",
        admission.body
    );
    let admission_body: Value =
        serde_json::from_str(&admission.body).expect("migration admission JSON");
    let job_id = admission_body["jobId"]
        .as_str()
        .expect("migration admission must return jobId");

    let output = migrate_action_cmd_for_provider(
        format!("http://{}", server.bind_addr()),
        "ack",
        job_id,
        admin_key.trim(),
        "typesense",
    )
    .arg("--json")
    .assert()
    .code(EXIT_HTTP_REJECTION)
    .get_output()
    .clone();

    assert_json_http_rejection(
        &output,
        "migration acknowledgement returned HTTP 404: code=migration_job_not_found status=404 message=Migration job not found",
    );
    assert_secrets_absent(&output, &[admin_key.trim()]);
}

#[test]
fn migrate_never_accepts_a_secret_as_an_argv_value() {
    let help = flapjack_cmd()
        .arg("migrate")
        .arg("--help")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(help).unwrap();
    for safe_flag in [
        "--api-key-env",
        "--api-key-file",
        "--api-key-stdin",
        "--source-key-env",
        "--source-key-file",
        "--source-key-stdin",
        "--algolia-key-env",
        "--algolia-key-file",
        "--algolia-key-stdin",
    ] {
        assert!(help.contains(safe_flag), "help omitted {safe_flag}: {help}");
    }
    assert!(!help
        .lines()
        .any(|line| line.trim_start().starts_with("--api-key ")));
    assert!(!help
        .lines()
        .any(|line| line.trim_start().starts_with("--algolia-key ")));
    assert!(!help
        .lines()
        .any(|line| line.trim_start().starts_with("--source-key ")));

    for (flag, secret) in [
        ("--api-key", FLAPJACK_API_KEY),
        ("--source-key", SOURCE_API_KEY),
        ("--algolia-key", ALGOLIA_API_KEY),
    ] {
        flapjack_cmd()
            .arg("migrate")
            .arg(flag)
            .arg(secret)
            .assert()
            .code(2);
    }
    flapjack_cmd()
        .args(
            "migrate --endpoint http://127.0.0.1:1 --app-id UNREACHABLESTAGE2 \
             --source-index products --api-key-stdin --algolia-key-stdin"
                .split_whitespace(),
        )
        .write_stdin("one-secret-stream")
        .assert()
        .code(2);
    flapjack_cmd()
        .args(
            "migrate --endpoint http://127.0.0.1:1 --source-provider meilisearch \
             --source-endpoint https://tenant.meilisearch.io --source-index products \
             --api-key-stdin --source-key-stdin"
                .split_whitespace(),
        )
        .write_stdin("one-secret-stream")
        .assert()
        .code(2);

    let server = FakeMigrationServer::start(vec![StubResponse::text(
        403,
        format!("credentials {FLAPJACK_API_KEY} and {ALGOLIA_API_KEY} rejected"),
    )]);
    let output = migrate_cmd(server.endpoint())
        .assert()
        .code(EXIT_HTTP_REJECTION)
        .get_output()
        .clone();
    let combined = combined_output(&output);
    assert!(!combined.contains(FLAPJACK_API_KEY));
    assert!(!combined.contains(ALGOLIA_API_KEY));
    assert!(combined.contains("[REDACTED]"));

    let server = FakeMigrationServer::start(vec![StubResponse::text(
        403,
        format!("credentials {FLAPJACK_API_KEY} and {SOURCE_API_KEY} rejected"),
    )]);
    let output = migrate_cmd_for_provider(
        server.endpoint(),
        "meilisearch",
        "https://tenant.meilisearch.io",
        SOURCE_API_KEY,
    )
    .assert()
    .code(EXIT_HTTP_REJECTION)
    .get_output()
    .clone();
    let combined = combined_output(&output);
    assert!(!combined.contains(FLAPJACK_API_KEY));
    assert!(!combined.contains(SOURCE_API_KEY));
    assert!(combined.contains("[REDACTED]"));
}

fn migrate_cmd(endpoint: String) -> assert_cmd::Command {
    migrate_cmd_with_key(endpoint, FLAPJACK_API_KEY, ALGOLIA_API_KEY)
}

fn migrate_cmd_with_key(
    endpoint: String,
    flapjack_api_key: &str,
    algolia_api_key: &str,
) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command.arg("migrate");
    add_flapjack_auth_args(&mut command, endpoint, flapjack_api_key);
    command
        .arg("--algolia-key-env")
        .arg("FJ_MIGRATE_TEST_ALGOLIA_KEY")
        .arg("--app-id")
        .arg("UNREACHABLESTAGE2")
        .arg("--source-index")
        .arg("products")
        .env("FJ_MIGRATE_TEST_ALGOLIA_KEY", algolia_api_key);
    command
}

fn migrate_cmd_for_provider(
    endpoint: String,
    provider: &str,
    source_endpoint: &str,
    source_api_key: &str,
) -> assert_cmd::Command {
    let mut command = migrate_cmd_for_provider_without_write_freeze(
        endpoint,
        provider,
        source_endpoint,
        source_api_key,
    );
    if provider == "typesense" {
        command.arg("--source-write-frozen");
    }
    command
}

fn migrate_cmd_for_provider_without_write_freeze(
    endpoint: String,
    provider: &str,
    source_endpoint: &str,
    source_api_key: &str,
) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command.arg("migrate");
    add_flapjack_auth_args(&mut command, endpoint, FLAPJACK_API_KEY);
    command
        .arg("--source-provider")
        .arg(provider)
        .arg("--source-endpoint")
        .arg(source_endpoint)
        .arg("--source-key-env")
        .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
        .arg("--source-index")
        .arg("products")
        .env("FJ_MIGRATE_TEST_SOURCE_KEY", source_api_key);
    command
}

fn migrate_preview_cmd(
    endpoint: String,
    provider: &str,
    source_endpoint: &str,
    source_api_key: &str,
) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command
        .arg("migrate")
        .arg("preview")
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--source-provider")
        .arg(provider)
        .arg("--source-key-env")
        .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
        .arg("--source-index")
        .arg("products")
        .arg("--application-id")
        .arg("test-owner")
        .arg("--api-key-env")
        .arg("FJ_MIGRATE_TEST_API_KEY")
        .env("FJ_MIGRATE_TEST_SOURCE_KEY", source_api_key)
        .env("FJ_MIGRATE_TEST_API_KEY", FLAPJACK_API_KEY);
    if provider == "algolia" {
        command.arg("--app-id").arg("UNREACHABLESTAGE2");
    } else {
        command.arg("--source-endpoint").arg(source_endpoint);
    }
    if provider == "typesense" {
        command.arg("--source-write-frozen");
    }
    command
}

fn migrate_action_cmd_with_key(
    endpoint: String,
    action: &str,
    job_id: &str,
    flapjack_api_key: &str,
) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command
        .arg("migrate")
        .arg(action)
        .arg("--job-id")
        .arg(job_id);
    add_flapjack_auth_args(&mut command, endpoint, flapjack_api_key);
    command
}

fn migrate_action_cmd_for_provider(
    endpoint: String,
    action: &str,
    job_id: &str,
    flapjack_api_key: &str,
    provider: &str,
) -> assert_cmd::Command {
    let mut command = migrate_action_cmd_with_key(endpoint, action, job_id, flapjack_api_key);
    command.arg("--source-provider").arg(provider);
    command
}

#[derive(Clone, Copy, Debug)]
struct ConfigRefusalCase {
    provider: Option<&'static str>,
    app_id: Option<&'static str>,
    source_endpoint: Option<&'static str>,
    expected_message: &'static str,
}

fn migrate_config_refusal_cmd(case: ConfigRefusalCase) -> assert_cmd::Command {
    let mut command = flapjack_cmd();
    command.arg("migrate");
    add_flapjack_auth_args(
        &mut command,
        "http://127.0.0.1:1".to_string(),
        FLAPJACK_API_KEY,
    );
    command
        .arg("--source-key-env")
        .arg("FJ_MIGRATE_TEST_SOURCE_KEY")
        .arg("--source-index")
        .arg("products")
        .arg("--json")
        .env("FJ_MIGRATE_TEST_SOURCE_KEY", SOURCE_API_KEY);
    if let Some(provider) = case.provider {
        command.arg("--source-provider").arg(provider);
    }
    if let Some(app_id) = case.app_id {
        command.arg("--app-id").arg(app_id);
    }
    if let Some(source_endpoint) = case.source_endpoint {
        command.arg("--source-endpoint").arg(source_endpoint);
    }
    command
}

fn migrate_cancel_with_open_api_key_stdin(endpoint: Option<&str>, job_id: &str) -> Output {
    let mut command = std::process::Command::new(support::flapjack_cmd_executable());
    command
        .arg("migrate")
        .arg("cancel")
        .arg("--job-id")
        .arg(job_id)
        .arg("--api-key-stdin")
        .arg("--json")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(endpoint) = endpoint {
        command.arg("--endpoint").arg(endpoint);
    }
    let mut child = command
        .spawn()
        .expect("spawn migrate action with open API-key stdin");
    let deadline = Instant::now() + Duration::from_secs(30);

    loop {
        if child.try_wait().expect("poll migrate action").is_some() {
            return child.wait_with_output().expect("collect migrate action");
        }
        if Instant::now() >= deadline {
            child.kill().expect("stop blocked migrate action");
            let output = child.wait_with_output().expect("collect blocked action");
            panic!(
                "local action validation consumed API-key stdin: {}",
                combined_output(&output)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn add_flapjack_auth_args(
    command: &mut assert_cmd::Command,
    endpoint: String,
    flapjack_api_key: &str,
) {
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--application-id")
        .arg("test-owner")
        .arg("--api-key-env")
        .arg("FJ_MIGRATE_TEST_API_KEY")
        .env("FJ_MIGRATE_TEST_API_KEY", flapjack_api_key);
}

struct ActionRefusalExpectation {
    action: &'static str,
    path: String,
    server_code: &'static str,
    message: &'static str,
    exit_code: i32,
}

fn assert_action_refusal(expected: ActionRefusalExpectation) {
    let refusal = json!({
        "code": expected.server_code,
        "status": 409,
        "message": expected.message
    });
    let server = FakeMigrationServer::start(vec![
        StubResponse::json(409, refusal.clone()),
        StubResponse::json(409, refusal),
    ]);
    let human_output =
        migrate_action_cmd_with_key(server.endpoint(), expected.action, JOB_ID, FLAPJACK_API_KEY)
            .assert()
            .code(expected.exit_code)
            .get_output()
            .clone();
    let json_output =
        migrate_action_cmd_with_key(server.endpoint(), expected.action, JOB_ID, FLAPJACK_API_KEY)
            .arg("--json")
            .assert()
            .code(expected.exit_code)
            .get_output()
            .clone();

    assert_distinct_action_exit(expected.exit_code);
    assert_ne!(EXIT_ACK_TOO_EARLY, EXIT_CANCEL_TOO_LATE);
    assert_named_refusal_is_redacted(&human_output, expected.server_code);
    assert_named_refusal_is_redacted(&json_output, expected.server_code);
    let report: Value = serde_json::from_slice(&json_output.stdout).expect("JSON refusal report");
    assert_eq!(report["errorType"], json!(expected.server_code));
    assert_eq!(report["exitCode"], json!(expected.exit_code));
    assert_action_requests(server.take_requests(2), &expected.path);
}

fn cancel_success_server_echoing_flapjack_key() -> FakeMigrationServer {
    FakeMigrationServer::start(vec![StubResponse::json(
        200,
        json!({
            "jobId": JOB_ID,
            "phase": "cancel_requested",
            "disposition": "running",
            "warnings": [format!("cancel warning for {FLAPJACK_API_KEY}")]
        }),
    )])
}

fn assert_distinct_action_exit(exit_code: i32) {
    assert_ne!(exit_code, 0);
    for existing in [
        EXIT_HTTP_REJECTION,
        EXIT_TIMEOUT,
        EXIT_FAILED_JOB,
        EXIT_CANCELLED_JOB,
    ] {
        assert_ne!(exit_code, existing);
    }
}

fn assert_named_refusal_is_redacted(output: &std::process::Output, expected_code: &str) {
    let combined = combined_output(output);
    assert!(
        combined.contains(expected_code),
        "output omitted {expected_code}: {combined}"
    );
    assert!(!combined.contains(FLAPJACK_API_KEY));
    assert!(!combined.contains(ALGOLIA_API_KEY));
}

fn assert_preview_http_rejection_is_redacted(output: &std::process::Output) {
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON rejection report");
    assert_eq!(report["errorType"], json!("http_rejection"));
    assert_eq!(report["exitCode"], json!(EXIT_HTTP_REJECTION));
    assert!(
        report["message"]
            .as_str()
            .is_some_and(|message| message.contains("migration preview")),
        "preview rejection omitted operation name: {report}"
    );
    assert_secrets_absent(output, &[FLAPJACK_API_KEY, SOURCE_API_KEY]);
}

fn assert_action_requests(requests: Vec<RecordedRequest>, expected_path: &str) {
    for request in requests {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, expected_path);
        assert_migration_request_headers(&request);
        assert_eq!(request.body, Value::Null);
    }
}

fn assert_json_http_rejection(output: &std::process::Output, expected_message: &str) {
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON rejection report");
    assert_eq!(report["errorType"], json!("http_rejection"));
    assert_eq!(report["exitCode"], json!(EXIT_HTTP_REJECTION));
    assert_eq!(report["message"], json!(expected_message));
}

fn assert_secrets_absent(output: &std::process::Output, secrets: &[&str]) {
    let combined = combined_output(output);
    for secret in secrets {
        assert!(
            !combined.contains(secret),
            "output exposed secret: {combined}"
        );
    }
}

fn assert_provider_submit_request(
    request: &RecordedRequest,
    expected_path: &str,
    expected_body: Value,
) {
    assert_eq!(request.method, "POST");
    assert_eq!(request.path, expected_path);
    assert_eq!(request.body, expected_body);
    assert_migration_request_headers(request);
    assert_eq!(
        request.header("content-type").as_deref(),
        Some("application/json")
    );
}

fn assert_migration_request_headers(request: &RecordedRequest) {
    assert_eq!(
        request.header("x-algolia-application-id").as_deref(),
        Some("test-owner")
    );
    assert_eq!(
        request.header("x-algolia-api-key").as_deref(),
        Some(FLAPJACK_API_KEY)
    );
}

fn migration_status(phase: &str, disposition: &str) -> Value {
    json!({"jobId": JOB_ID, "phase": phase, "disposition": disposition})
}

fn migration_preview_response(hard_rejections: usize) -> Value {
    let mut entries = vec![
        json!({
            "severity": "Warning",
            "code": "MeilisearchSettingNotMigrated",
            "resource": "Settings",
            "pageIndex": null,
            "itemIndex": null,
            "jsonPath": "$.stopWords"
        }),
        json!({
            "severity": "ScopeGap",
            "code": "ProductNotMigrated",
            "resource": "Analytics",
            "pageIndex": null,
            "itemIndex": null,
            "jsonPath": "$"
        }),
    ];
    entries.extend((0..hard_rejections).map(|item_index| {
        json!({
            "severity": "HardRejection",
            "code": "MalformedSourcePayload",
            "resource": "Settings",
            "pageIndex": null,
            "itemIndex": item_index,
            "jsonPath": "$.rankingRules"
        })
    }));
    json!({
        "report": {
            "entries": entries,
            "summary": {
                "totalEntries": 2 + hard_rejections,
                "hardRejections": hard_rejections,
                "warnings": 1,
                "scopeGaps": 1
            },
            "reportDigest": "preview-contract-digest"
        },
        "sourceCounts": {
            "indexes": 1,
            "records": 37
        }
    })
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}
