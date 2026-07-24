#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

mod support;

use flapjack_http::handlers::health::PublicBuildInfo;
use predicates::str::contains;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::time::Duration;
use support::{
    flapjack_cmd, flapjack_cmd_executable, http_request, server_spawn_executable, RunningServer,
    TempDir,
};

#[test]
fn cli_and_server_helpers_select_same_executable_artifact() {
    let cli_path = std::fs::canonicalize(flapjack_cmd_executable())
        .expect("assert_cmd-selected flapjack executable should be canonicalizable");
    let server_path = std::fs::canonicalize(server_spawn_executable())
        .expect("server-spawn flapjack executable should be canonicalizable");
    let cli_digest = sha256_file(&cli_path);
    let server_digest = sha256_file(&server_path);
    let artifact_identity = format!(
        "CLI path: {cli_path:?}\n\
         CLI SHA-256: {cli_digest}\n\
         server path: {server_path:?}\n\
         server SHA-256: {server_digest}"
    );
    eprintln!("{artifact_identity}");

    assert!(
        cli_path == server_path && cli_digest == server_digest,
        "CLI and live-server helpers must select one executable artifact\n{artifact_identity}"
    );
}

fn sha256_file(path: &Path) -> String {
    let mut file = std::fs::File::open(path)
        .unwrap_or_else(|error| panic!("failed to open executable {path:?}: {error}"));
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let bytes_read = file
            .read(&mut buffer)
            .unwrap_or_else(|error| panic!("failed to read executable {path:?}: {error}"));
        if bytes_read == 0 {
            break;
        }
        hasher.update(&buffer[..bytes_read]);
    }
    format!("{:x}", hasher.finalize())
}

#[test]
fn build_info_json_outputs_canonical_build_info() {
    let output = flapjack_cmd()
        .arg("build-info")
        .arg("--json")
        .assert()
        .success()
        .stderr("")
        .get_output()
        .clone();

    let stdout = String::from_utf8(output.stdout).unwrap();
    let actual: Value = serde_json::from_str(&stdout).unwrap();

    assert_eq!(stdout.trim(), actual.to_string());
    assert_eq!(
        actual,
        serde_json::to_value(flapjack::build_info()).unwrap()
    );
}

#[test]
fn package_version_output_still_uses_package_version() {
    flapjack_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(format!("flapjack {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn existing_operational_subcommands_remain_routed() {
    let uninstall_home = TempDir::new("fj_test_uninstall_home");
    let uninstall_install = TempDir::new("fj_test_uninstall_install");
    flapjack_cmd()
        .arg("uninstall")
        .env("HOME", uninstall_home.path())
        .env("FLAPJACK_INSTALL", uninstall_install.path())
        .assert()
        .success()
        .stderr(contains("Flapjack has been uninstalled."));

    let reset_data = TempDir::new("fj_test_reset_missing");
    flapjack_cmd()
        .arg("--data-dir")
        .arg(reset_data.path())
        .arg("reset-admin-key")
        .assert()
        .failure()
        .stderr(contains("No keys.json found"));
}

#[test]
fn no_subcommand_starts_server_path_instead_of_printing_build_info() {
    let tmp = TempDir::new("fj_test_no_subcommand_starts");
    let output = flapjack_cmd()
        .arg("--no-auth")
        .arg("--auto-port")
        .arg("--data-dir")
        .arg(tmp.path())
        .timeout(Duration::from_secs(8))
        .output()
        .expect("failed to run flapjack server path");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Flapjack"),
        "no-subcommand execution must start server path, got stdout: {stdout}"
    );
    assert!(
        !stdout.trim_start().starts_with('{'),
        "no-subcommand execution must not print build-info JSON, got stdout: {stdout}"
    );
}

#[test]
fn cli_build_info_matches_live_health_build_info() {
    let cli_output = flapjack_cmd()
        .arg("build-info")
        .arg("--json")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let cli_build_info: flapjack::BuildInfo = serde_json::from_slice(&cli_output).unwrap();
    let cli_build = serde_json::to_value(&cli_build_info).unwrap();

    let tmp = TempDir::new("fj_test_build_info_health");
    let server = RunningServer::spawn_no_auth_auto_port(tmp.path());
    let response = http_request(server.bind_addr(), "GET", "/health", None)
        .expect("health endpoint should be reachable");
    assert_eq!(response.status, 200);
    let health: Value = serde_json::from_str(&response.body).unwrap();

    let expected_public_build =
        serde_json::to_value(PublicBuildInfo::from(&cli_build_info)).unwrap();
    assert_eq!(
        health["build"], expected_public_build,
        "live health build must equal the public CLI build projection\n\
         full CLI build: {cli_build}\n\
         live health build: {}",
        health["build"]
    );
    assert_no_migration_capability_spellings(&cli_build);
    assert_no_migration_capability_spellings(&health["capabilities"]);
    assert_known_flag_matches_nullable_value(&cli_build, "revision", "revisionKnown");
    assert_known_flag_matches_nullable_value(&cli_build, "dirty", "dirtyKnown");
    for field in [
        "revision",
        "revisionKnown",
        "dirty",
        "dirtyKnown",
        "workspaceDigest",
        "target",
        "features",
    ] {
        assert!(
            health["build"].get(field).is_none(),
            "public health build must omit private CLI field {field}: {}",
            health["build"]
        );
    }
}

fn assert_no_migration_capability_spellings(value: &Value) {
    let serialized = value.to_string();
    for spelling in ["algolia_migration_v1", "algoliaMigrationV1"] {
        assert!(
            !serialized.contains(spelling),
            "serialized build capability payload must not include {spelling}: {serialized}"
        );
    }
}

fn assert_known_flag_matches_nullable_value(value: &Value, value_key: &str, known_key: &str) {
    let known = value[known_key]
        .as_bool()
        .unwrap_or_else(|| panic!("{known_key} must be boolean in {value}"));
    assert_eq!(
        !value[value_key].is_null(),
        known,
        "{known_key} must track whether {value_key} is known in {value}"
    );
}
