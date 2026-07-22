#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

//! Integration tests for flapjack server startup modes, authentication configuration, and multi-instance isolation.
mod support;

use predicates::str::contains;
use std::time::Duration;
use support::{
    admin_entry_exists_in_json, extract_admin_key_hash_from_json, extract_key_from_banner,
    flapjack_cmd, http_request, unique_suffix, RunningServer, TempDir,
};

#[test]
fn repair_publication_reports_clean_without_creating_evidence() {
    let tmp = TempDir::new("fj_repair_clean");
    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report,
        serde_json::json!({
            "tenant": "products", "status": "clean", "action": "none",
            "transaction_id": null, "phase": null, "evidence": null
        })
    );
    assert!(!tmp.root().join(".publication/products").exists());
}

#[test]
fn repair_publication_text_renders_complete_canonical_report() {
    let tmp = TempDir::new("fj_repair_text_clean");
    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        "tenant=products status=clean action=none transaction_id=null phase=null evidence=null\n"
    );
}

#[test]
fn repair_publication_reports_empty_namespace_as_unresolved() {
    let tmp = TempDir::new("fj_repair_empty_namespace");
    std::fs::create_dir_all(tmp.root().join(".publication/products")).unwrap();

    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report,
        serde_json::json!({
            "tenant": "products", "status": "unresolved", "action": "unresolved",
            "transaction_id": null, "phase": null, "evidence": ".publication/products"
        })
    );
    assert!(tmp.root().join(".publication/products").exists());
}

#[cfg(unix)]
#[test]
fn repair_publication_reports_symlinked_live_tenant_as_unresolved() {
    let tmp = TempDir::new("fj_repair_symlinked_tenant");
    let outside = TempDir::new("fj_repair_symlinked_tenant_outside");
    std::fs::create_dir_all(outside.root()).unwrap();
    std::fs::write(outside.root().join("settings.json"), "escaped").unwrap();
    std::os::unix::fs::symlink(outside.root(), tmp.root().join("products")).unwrap();

    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report,
        serde_json::json!({
            "tenant": "products", "status": "unresolved", "action": "unresolved",
            "transaction_id": null, "phase": null, "evidence": null
        })
    );
    assert_eq!(
        std::fs::read_to_string(outside.root().join("settings.json")).unwrap(),
        "escaped"
    );
}

#[test]
fn repair_publication_reports_repaired_and_unresolved_exit_codes() {
    use flapjack::index::manager::publication::*;
    let repaired = TempDir::new("fj_repair_repaired");
    let target = PublicationTarget::new("products").unwrap();
    let transaction = PublicationTransactionId::new("txn_repair").unwrap();
    let paths = PublicationPaths::new(repaired.root(), &target, &transaction);
    std::fs::create_dir_all(&paths.target).unwrap();
    std::fs::write(paths.target.join("settings.json"), "old").unwrap();
    std::fs::create_dir_all(&paths.staging).unwrap();
    std::fs::write(paths.staging.join("settings.json"), "new").unwrap();
    let inventory = TantivyManagedInventory::new([]).unwrap();
    let digest = canonical_tenant_tree_digest(&paths.staging, &inventory).unwrap();
    let prior = canonical_tenant_tree_digest(&paths.target, &inventory).unwrap();
    let mut journal = PublicationJournal::prepare(
        transaction,
        target,
        PublicationGenerationEvidence::new("generation").unwrap(),
        digest,
        paths.clone(),
    );
    journal.prior_digest = Some(prior);
    std::fs::create_dir_all(paths.journal.parent().unwrap()).unwrap();
    std::fs::write(&paths.journal, journal.to_json_value().to_string()).unwrap();
    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(repaired.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["tenant"], "products");
    assert_eq!(report["status"], "repaired");
    assert_eq!(report["action"], "complete");
    assert_eq!(report["transaction_id"], "txn_repair");
    assert_eq!(report["phase"], "prepared");
    assert_eq!(report["evidence"], ".publication/products/txn_repair");

    let unresolved = TempDir::new("fj_repair_unresolved");
    for transaction in ["txn_a", "txn_b"] {
        let path = unresolved
            .root()
            .join(".publication/products")
            .join(transaction)
            .join("staging");
        std::fs::create_dir_all(path).unwrap();
    }
    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(unresolved.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "unresolved");
    assert_eq!(report["action"], "unresolved");
    assert_eq!(report["transaction_id"], serde_json::Value::Null);
    assert_eq!(report["phase"], serde_json::Value::Null);
    assert_eq!(report["evidence"], ".publication/products");

    let quarantined = TempDir::new("fj_repair_quarantined");
    let staging = quarantined
        .root()
        .join(".publication/products/txn_stale/staging");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("settings.json"), "unproven").unwrap();
    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(quarantined.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report,
        serde_json::json!({
            "tenant": "products", "status": "quarantined", "action": "quarantine",
            "transaction_id": "txn_stale", "phase": null,
            "evidence": ".publication/products/txn_stale"
        })
    );
}

#[test]
fn repair_publication_uses_clap_for_missing_and_invalid_flags() {
    flapjack_cmd()
        .arg("--data-dir")
        .arg(TempDir::new("fj_repair_missing_tenant").path())
        .arg("repair-publication")
        .arg("--json")
        .assert()
        .failure()
        .code(2);
    flapjack_cmd()
        .arg("--data-dir")
        .arg(TempDir::new("fj_repair_missing_tenant_value").path())
        .arg("repair-publication")
        .arg("--tenant")
        .assert()
        .failure()
        .code(2);
    flapjack_cmd()
        .arg("--data-dir")
        .arg(TempDir::new("fj_repair_unknown_flag").path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--unknown")
        .assert()
        .failure()
        .code(2);
    flapjack_cmd()
        .arg("repair-publication")
        .arg("--tenant")
        .arg("products")
        .arg("--json")
        .arg("--data-dir")
        .arg(TempDir::new("fj_repair_subcommand_before_global").path())
        .assert()
        .failure()
        .code(2);
}

#[test]
fn repair_publication_rejects_path_traversal_tenant_after_parse() {
    let tmp = TempDir::new("fj_repair_path_traversal_tenant");

    flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("repair-publication")
        .arg("--tenant")
        .arg("../products")
        .arg("--json")
        .assert()
        .failure()
        .code(1)
        .stderr(contains(
            "Index name contains invalid characters (path traversal not allowed)",
        ));
}

// Startup assertions depend on the banner/error text being emitted before the
// helper timeout kills the child process. Keep this comfortably above observed
// startup jitter so parallel test execution does not produce false negatives.
const STARTUP_ASSERT_TIMEOUT: Duration = Duration::from_secs(8);

// ===== Production mode guards ==============================================

#[test]
fn production_mode_rejects_missing_key() {
    let tmp = TempDir::new("fj_test_prod_missing_key");
    flapjack_cmd()
        .env("FLAPJACK_ENV", "production")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .assert()
        .failure()
        .code(1)
        .stderr(contains(
            "FLAPJACK_ADMIN_KEY is required in production mode",
        ));
}

#[test]
fn production_mode_rejects_short_key() {
    let tmp = TempDir::new("fj_test_prod_short_key");
    flapjack_cmd()
        .env("FLAPJACK_ENV", "production")
        .env("FLAPJACK_ADMIN_KEY", "tooshort")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .assert()
        .failure()
        .code(1)
        .stderr(contains("at least 16 characters"));
}

/// Verify that production mode starts successfully when a valid admin key (>= 16 characters) is provided via `FLAPJACK_ADMIN_KEY`, and that the key is not echoed in the startup banner.
#[test]
fn production_mode_accepts_valid_key() {
    let tmp = TempDir::new("fj_test_prod_mode");
    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "production")
        .env("FLAPJACK_ADMIN_KEY", "abcdef0123456789")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Server should start successfully (banner printed) and NOT show the key
    // (key was supplied via env, not auto-generated, so it's not "new").
    assert!(
        stdout.contains("Flapjack"),
        "Expected startup banner, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("Admin API Key:"),
        "Provided key should NOT be printed in banner, got: {}",
        stdout
    );
}

#[test]
fn production_mode_rejects_no_auth() {
    let tmp = TempDir::new("fj_test_prod_no_auth");
    flapjack_cmd()
        .env("FLAPJACK_ENV", "production")
        .env("FLAPJACK_NO_AUTH", "1")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .assert()
        .failure()
        .code(1)
        .stderr(contains("--no-auth cannot be used in production"));
}

// ===== Development mode: auto-generate key =================================

/// Verify that development mode auto-generates an `fj_admin_`-prefixed admin key with 32 hex characters, displays it in the startup banner, and persists a hashed entry in `keys.json`.
#[test]
fn development_mode_auto_generates_key() {
    let tmp = TempDir::new("fj_test_auto_key");
    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Banner must show the auto-generated key
    assert!(
        stdout.contains("Admin API Key:"),
        "Expected auto-generated key in banner, got: {}",
        stdout
    );
    assert!(
        stdout.contains("fj_admin_"),
        "Expected fj_admin_ prefixed key, got: {}",
        stdout
    );
    assert!(
        stdout.contains(&format!(
            "flapjack --data-dir {} reset-admin-key",
            tmp.path()
        )),
        "Expected explicit data-dir reset command in banner, got: {}",
        stdout
    );

    // Validate key format: fj_admin_ + 32 hex chars = 41 chars
    let key = extract_key_from_banner(&stdout);
    assert_eq!(
        key.len(),
        41,
        "Key should be 41 chars (fj_admin_ + 32 hex), got {} chars: {}",
        key.len(),
        key
    );
    assert!(
        key[9..].chars().all(|c| c.is_ascii_hexdigit()),
        "Key suffix (after fj_admin_) should be hex, got: {}",
        key
    );

    // keys.json should exist with an Admin API Key entry (stored as hash, not plaintext)
    let keys_json = std::fs::read_to_string(tmp.root().join("keys.json"))
        .expect("keys.json should exist after first start");
    assert!(
        admin_entry_exists_in_json(&keys_json),
        "keys.json should have an Admin API Key entry"
    );
}

/// Verify that the startup banner shell-quotes the reset hint when `--data-dir` contains spaces.
#[test]
fn development_mode_banner_quotes_spaced_data_dir_reset_hint() {
    let tmp = TempDir::new("fj_test_auto_key_spaces");
    let data_dir = tmp.root().join("data dir with spaces");

    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", &data_dir)
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains(&format!(
            "flapjack --data-dir '{}' reset-admin-key",
            data_dir.display()
        )),
        "expected quoted data-dir reset command in banner, got: {}",
        stdout
    );
}

/// Verify that a blank `.admin_key` exits with an explicit data-dir reset command hint.
#[test]
fn blank_admin_key_file_prints_explicit_reset_hint() {
    let tmp = TempDir::new("fj_test_blank_admin_key");
    std::fs::write(tmp.root().join(".admin_key"), "   \n")
        .expect("failed to write blank .admin_key fixture");

    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");

    assert!(
        !output.status.success(),
        "startup should fail when .admin_key is blank"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(".admin_key file"),
        "expected blank-file error in stderr, got: {}",
        stderr
    );
    assert!(
        stderr.contains("is empty"),
        "expected blank-file reason in stderr, got: {}",
        stderr
    );
    assert!(
        stderr.contains(&format!(
            "Run: flapjack --data-dir {} reset-admin-key",
            tmp.path()
        )),
        "expected explicit data-dir reset hint in stderr, got: {}",
        stderr
    );
}

/// Verify that a blank `.admin_key` shell-quotes the reset hint when `--data-dir` contains spaces.
#[test]
fn blank_admin_key_file_quotes_spaced_data_dir_reset_hint() {
    let tmp = TempDir::new("fj_test_blank_admin_key_spaces");
    let data_dir = tmp.root().join("blank key dir");
    std::fs::create_dir_all(&data_dir).expect("failed to create spaced data dir fixture");
    std::fs::write(data_dir.join(".admin_key"), "   \n")
        .expect("failed to write blank .admin_key fixture");

    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", &data_dir)
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");

    assert!(
        !output.status.success(),
        "startup should fail when .admin_key is blank"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&format!(
            "Run: flapjack --data-dir '{}' reset-admin-key",
            data_dir.display()
        )),
        "expected quoted data-dir reset hint in stderr, got: {}",
        stderr
    );
}

// ===== Development mode: key persistence across restarts ===================

/// Verify that an auto-generated admin key is persisted to `keys.json` and reused across server restarts without regeneration, keeping the stored hash stable.
#[test]
fn key_persists_across_restarts() {
    let tmp = TempDir::new("fj_test_key_persist");

    // First start: auto-generate a key
    let output1 = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout1 = String::from_utf8_lossy(&output1.stdout);
    assert!(
        stdout1.contains("Admin API Key:"),
        "First start should show auto-generated key"
    );
    let key1 = extract_key_from_banner(&stdout1);

    // Second start: should reuse the existing key from keys.json
    let output2 = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout2 = String::from_utf8_lossy(&output2.stdout);

    // Banner should NOT show the key on restart (it's not new)
    assert!(
        !stdout2.contains("Admin API Key:"),
        "Restart should NOT print the key again, got: {}",
        stdout2
    );
    assert!(
        stdout2.contains("Flapjack"),
        "Restart should still show the banner"
    );

    // Verify keys.json still has the same admin hash (key was not regenerated on restart)
    let keys_json = std::fs::read_to_string(tmp.root().join("keys.json")).unwrap();
    let hash1 = extract_admin_key_hash_from_json(&keys_json);
    // The key from the banner was key1; verify its hash is present in keys.json (unchanged)
    assert!(
        !hash1.is_empty(),
        "keys.json should still have a valid admin key hash after restart"
    );
    // Restarting with the same keys.json should not change the hash
    let output3 = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    drop(output3); // just ensuring it starts again
    let keys_json2 = std::fs::read_to_string(tmp.root().join("keys.json")).unwrap();
    let hash2 = extract_admin_key_hash_from_json(&keys_json2);
    assert_eq!(
        hash1, hash2,
        "admin key hash must be stable across restarts"
    );
    // Also verify the banner key extracted from first start is valid format
    assert!(
        key1.starts_with("fj_admin_"),
        "auto-generated key must start with fj_admin_"
    );
}

// ===== Development mode: custom env var key ================================

/// Verify that providing `FLAPJACK_ADMIN_KEY` in development mode uses the custom key without printing it in the banner, and still persists a hashed entry in `keys.json`.
#[test]
fn development_mode_with_custom_key() {
    let tmp = TempDir::new("fj_test_dev_custom_key");
    let custom_key = "my_custom_dev_key_1234";
    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_ADMIN_KEY", custom_key)
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Should start successfully without showing the key (it was provided, not new)
    assert!(
        stdout.contains("Flapjack"),
        "Expected startup banner, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("Admin API Key:"),
        "Custom key should NOT be printed in banner"
    );

    // keys.json should exist with an Admin API Key entry (key stored as hash, not plaintext)
    let keys_json = std::fs::read_to_string(tmp.root().join("keys.json")).unwrap();
    assert!(
        admin_entry_exists_in_json(&keys_json),
        "keys.json should have an Admin API Key entry even when using custom env var key"
    );
}

// ===== --no-auth via env var ===============================================

/// Verify that setting `FLAPJACK_NO_AUTH=1` disables authentication, prints an "Auth disabled" warning, suppresses key generation, and does not create `keys.json`.
#[test]
fn no_auth_env_var_disables_auth() {
    let tmp = TempDir::new("fj_test_no_auth_env");
    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_NO_AUTH", "1")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Auth disabled"),
        "Expected auth disabled warning, got: {}",
        stdout
    );
    // Should NOT auto-generate a key when auth is disabled
    assert!(
        !stdout.contains("Admin API Key:"),
        "No key should be shown when auth is disabled"
    );
    // keys.json should NOT be created
    assert!(
        !tmp.root().join("keys.json").exists(),
        "keys.json should not exist when auth is disabled"
    );
}

// ===== --no-auth via CLI flag ==============================================

/// Verify that the `--no-auth` CLI flag disables authentication and prints the "Auth disabled" warning, matching the behavior of the `FLAPJACK_NO_AUTH` env var.
#[test]
fn no_auth_cli_flag_disables_auth() {
    let tmp = TempDir::new("fj_test_no_auth_cli");
    let output = flapjack_cmd()
        .env("FLAPJACK_ENV", "development")
        .env("FLAPJACK_BIND_ADDR", "127.0.0.1:0")
        .env("FLAPJACK_DATA_DIR", tmp.path())
        .arg("--no-auth")
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Auth disabled"),
        "Expected auth disabled warning via CLI flag, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("Admin API Key:"),
        "No key should be shown when auth is disabled via CLI flag"
    );
}

/// Verify that the `--port` CLI flag takes precedence over an invalid `FLAPJACK_BIND_ADDR` env var, and that port 0 resolves to an actual OS-assigned port in the banner.
#[test]
fn cli_port_flag_overrides_env_bind_addr() {
    let tmp = TempDir::new(&format!("fj_test_port_flag_{}", unique_suffix()));

    let output = flapjack_cmd()
        .env("FLAPJACK_BIND_ADDR", "not-an-addr")
        .arg("--no-auth")
        .arg("--port")
        .arg("0")
        .arg("--data-dir")
        .arg(tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Flapjack"),
        "expected startup banner when using --port override, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("http://127.0.0.1:"),
        "--port should control bind address when --bind-addr is not set, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("http://127.0.0.1:0"),
        "startup banner should print resolved OS-assigned port, got: {}",
        stdout
    );
}

/// Verify that a second flapjack process targeting the same data directory exits with an "already in use" error and a remediation hint suggesting a unique `--data-dir`.
#[test]
fn second_process_same_data_dir_fails_fast_with_lock_message() {
    let tmp = TempDir::new(&format!("fj_test_data_lock_{}", unique_suffix()));

    let first = RunningServer::spawn_no_auth_auto_port(tmp.path());

    let output = flapjack_cmd()
        .arg("--no-auth")
        .arg("--auto-port")
        .arg("--data-dir")
        .arg(tmp.path())
        .timeout(Duration::from_secs(2))
        .output()
        .expect("failed to run second process");

    assert!(
        !output.status.success(),
        "second process should fail when sharing data-dir"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("already in use"),
        "expected lock contention error, got stderr: {}",
        stderr
    );
    assert!(
        stderr.contains("unique --data-dir"),
        "expected remediation hint in stderr, got: {}",
        stderr
    );

    drop(first);
}

/// Verify that `--instance <name>` derives a data directory under the system temp path at `flapjack/<name>` and creates it on startup.
#[test]
fn instance_flag_derives_isolated_data_dir() {
    let instance = format!("fj_instance_{}", unique_suffix());
    let expected_data_dir = std::env::temp_dir().join("flapjack").join(&instance);
    let _ = std::fs::remove_dir_all(&expected_data_dir);

    let output = flapjack_cmd()
        .arg("--no-auth")
        .arg("--instance")
        .arg(&instance)
        .arg("--auto-port")
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Flapjack"),
        "Expected startup banner when using --instance, got: {}",
        stdout
    );
    assert!(
        expected_data_dir.exists(),
        "expected instance-derived data dir to exist: {}",
        expected_data_dir.display()
    );

    let _ = std::fs::remove_dir_all(&expected_data_dir);
}

/// Verify that `--auto-port` binds to an ephemeral loopback port, prints the resolved address in the startup banner, and responds to health checks.
#[test]
fn auto_port_binds_ephemeral_loopback_and_prints_resolved_addr() {
    let tmp = TempDir::new(&format!("fj_test_auto_port_{}", unique_suffix()));
    let server = RunningServer::spawn_no_auth_auto_port(tmp.path());

    assert!(
        server.bind_addr().starts_with("127.0.0.1:"),
        "expected loopback bind addr, got: {}",
        server.bind_addr()
    );
    assert!(
        !server.bind_addr().ends_with(":0"),
        "resolved bind addr must not remain :0, got: {}",
        server.bind_addr()
    );

    let health = http_request(server.bind_addr(), "GET", "/health", None)
        .expect("health endpoint should be reachable on auto-port");
    assert_eq!(
        health.status, 200,
        "expected /health status 200, body: {}",
        health.body
    );
    assert!(
        health.body.contains("\"status\":\"ok\""),
        "expected healthy status payload, got: {}",
        health.body
    );
}

/// Verify that `--auto-port` ignores both `FLAPJACK_BIND_ADDR` and `FLAPJACK_PORT` env vars, binding to an ephemeral loopback port instead.
#[test]
fn auto_port_overrides_env_bind_addr_and_port() {
    let tmp = TempDir::new(&format!(
        "fj_test_auto_port_env_override_{}",
        unique_suffix()
    ));

    let output = flapjack_cmd()
        .env("FLAPJACK_BIND_ADDR", "not-an-addr")
        .env("FLAPJACK_PORT", "17777")
        .arg("--no-auth")
        .arg("--auto-port")
        .arg("--data-dir")
        .arg(tmp.path())
        .timeout(STARTUP_ASSERT_TIMEOUT)
        .output()
        .expect("failed to run");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Flapjack"),
        "expected startup banner when using --auto-port override, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("http://127.0.0.1:"),
        "expected startup URL in banner, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("http://127.0.0.1:0"),
        "startup banner should print resolved OS-assigned port, got: {}",
        stdout
    );
    assert!(
        !stdout.contains("http://127.0.0.1:17777"),
        "--auto-port should not bind using FLAPJACK_PORT, got: {}",
        stdout
    );
}

#[test]
fn auto_port_rejects_explicit_port_flag() {
    flapjack_cmd()
        .arg("--auto-port")
        .arg("--port")
        .arg("7701")
        .assert()
        .failure()
        .stderr(contains("--auto-port cannot be used with --port"));
}

#[test]
fn auto_port_rejects_explicit_bind_addr_flag() {
    flapjack_cmd()
        .arg("--auto-port")
        .arg("--bind-addr")
        .arg("127.0.0.1:7701")
        .assert()
        .failure()
        .stderr(contains("--auto-port cannot be used with --bind-addr"));
}

/// Verify that two flapjack instances with separate data directories maintain fully isolated index state — documents written to one instance are not visible from the other.
#[test]
fn two_instances_with_unique_data_dirs_serve_independent_index_state() {
    let tmp_a = TempDir::new(&format!("fj_test_instance_a_{}", unique_suffix()));
    let tmp_b = TempDir::new(&format!("fj_test_instance_b_{}", unique_suffix()));
    let server_a = RunningServer::spawn_no_auth_auto_port(tmp_a.path());
    let server_b = RunningServer::spawn_no_auth_auto_port(tmp_b.path());

    assert_ne!(
        server_a.bind_addr(),
        server_b.bind_addr(),
        "two auto-port instances should bind distinct ports"
    );

    let put_a = http_request(
        server_a.bind_addr(),
        "PUT",
        "/1/indexes/shared/test-doc-a",
        Some(r#"{"title":"from A","marker":"A"}"#),
    )
    .expect("instance A should accept document writes");
    assert_eq!(
        put_a.status, 200,
        "instance A write failed with status {}, body: {}",
        put_a.status, put_a.body
    );

    let put_b = http_request(
        server_b.bind_addr(),
        "PUT",
        "/1/indexes/shared/test-doc-b",
        Some(r#"{"title":"from B","marker":"B"}"#),
    )
    .expect("instance B should accept document writes");
    assert_eq!(
        put_b.status, 200,
        "instance B write failed with status {}, body: {}",
        put_b.status, put_b.body
    );

    let get_a_from_a = http_request(
        server_a.bind_addr(),
        "GET",
        "/1/indexes/shared/test-doc-a",
        None,
    )
    .expect("instance A should return its own document");
    assert_eq!(
        get_a_from_a.status, 200,
        "expected instance A to return stored doc, body: {}",
        get_a_from_a.body
    );
    assert!(
        get_a_from_a.body.contains("\"marker\":\"A\""),
        "instance A returned unexpected payload: {}",
        get_a_from_a.body
    );

    let get_a_from_b = http_request(
        server_b.bind_addr(),
        "GET",
        "/1/indexes/shared/test-doc-a",
        None,
    )
    .expect("instance B read for instance A doc should return response");
    assert_eq!(
        get_a_from_b.status, 404,
        "instance B should not see instance A doc, got status {}, body: {}",
        get_a_from_b.status, get_a_from_b.body
    );

    let get_b_from_b = http_request(
        server_b.bind_addr(),
        "GET",
        "/1/indexes/shared/test-doc-b",
        None,
    )
    .expect("instance B should return its own document");
    assert_eq!(
        get_b_from_b.status, 200,
        "expected instance B to return stored doc, body: {}",
        get_b_from_b.body
    );
    assert!(
        get_b_from_b.body.contains("\"marker\":\"B\""),
        "instance B returned unexpected payload: {}",
        get_b_from_b.body
    );

    let get_b_from_a = http_request(
        server_a.bind_addr(),
        "GET",
        "/1/indexes/shared/test-doc-b",
        None,
    )
    .expect("instance A read for instance B doc should return response");
    assert_eq!(
        get_b_from_a.status, 404,
        "instance A should not see instance B doc, got status {}, body: {}",
        get_b_from_a.status, get_b_from_a.body
    );
}
