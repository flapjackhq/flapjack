#![allow(deprecated)] // Command::cargo_bin — macro alternative requires same-package binary

//! Integration tests for flapjack admin key lifecycle flows.
mod support;

use support::{
    admin_auth_headers, admin_entry_exists_in_json, extract_admin_key_hash_from_json, flapjack_cmd,
    http_request_with_headers, run_auth_auto_port_startup_once, RunningServer, TempDir,
};

#[test]
fn env_var_key_overrides_existing_keys_json() {
    let tmp = TempDir::new("fj_test_env_override");
    let admin_key_path = tmp.root().join(".admin_key");

    run_auth_auto_port_startup_once(tmp.path(), &[("FLAPJACK_ENV", "development")]);

    let first_admin_key = std::fs::read_to_string(&admin_key_path)
        .expect("first start should persist the generated admin key to .admin_key");
    assert!(
        first_admin_key.trim().starts_with("fj_admin_"),
        "first start should generate an fj_admin_ key, got: {}",
        first_admin_key.trim()
    );

    let keys_json_before = std::fs::read_to_string(tmp.root().join("keys.json"))
        .expect("keys.json should exist after first start");
    let hash_before = extract_admin_key_hash_from_json(&keys_json_before);

    let custom_key = "rotated_key_abcdef0123456789";
    run_auth_auto_port_startup_once(
        tmp.path(),
        &[
            ("FLAPJACK_ENV", "development"),
            ("FLAPJACK_ADMIN_KEY", custom_key),
        ],
    );

    let persisted_admin_key = std::fs::read_to_string(&admin_key_path)
        .expect("override start should rewrite .admin_key with the provided key");
    assert_eq!(
        persisted_admin_key.trim(),
        custom_key,
        "FLAPJACK_ADMIN_KEY should become the persisted admin key"
    );

    let keys_json_after = std::fs::read_to_string(tmp.root().join("keys.json")).unwrap();
    let hash_after = extract_admin_key_hash_from_json(&keys_json_after);
    assert_ne!(
        hash_before, hash_after,
        "Admin key hash must change after FLAPJACK_ADMIN_KEY rotation"
    );
    assert!(
        admin_entry_exists_in_json(&keys_json_after),
        "Admin API Key entry must still exist after rotation"
    );
}

#[test]
fn rotate_admin_key_endpoint_rewrites_admin_key_file_and_invalidates_old_key() {
    let tmp = TempDir::new("fj_test_rotate_admin_key_real_server");
    let server = RunningServer::spawn_auth_auto_port(tmp.path());

    let old_key_path = tmp.root().join(".admin_key");
    let old_key = std::fs::read_to_string(&old_key_path)
        .expect("startup should persist initial admin key to .admin_key")
        .trim()
        .to_string();
    assert!(
        old_key.starts_with("fj_admin_"),
        "initial admin key should use fj_admin_ prefix, got: {}",
        old_key
    );

    let rotate_response = http_request_with_headers(
        server.bind_addr(),
        "POST",
        "/internal/rotate-admin-key",
        &admin_auth_headers(old_key.as_str()),
        None,
    )
    .expect("rotate-admin-key request should return an HTTP response");
    assert_eq!(
        rotate_response.status, 200,
        "expected rotate-admin-key success, body: {}",
        rotate_response.body
    );

    let rotate_payload: serde_json::Value = serde_json::from_str(&rotate_response.body)
        .expect("rotation response should be valid JSON");
    let new_key = rotate_payload["key"]
        .as_str()
        .expect("rotation response should include `key`")
        .to_string();
    assert_ne!(
        old_key, new_key,
        "rotation should return a different admin key"
    );

    let admin_key_on_disk = std::fs::read_to_string(&old_key_path)
        .expect(".admin_key should still exist after rotation");
    assert_eq!(
        admin_key_on_disk.trim(),
        new_key,
        ".admin_key should be rewritten with rotated key"
    );

    let old_key_metrics = http_request_with_headers(
        server.bind_addr(),
        "GET",
        "/metrics",
        &admin_auth_headers(old_key.as_str()),
        None,
    )
    .expect("old-key metrics request should return an HTTP response");
    assert_eq!(
        old_key_metrics.status, 403,
        "old key should be rejected after rotation, body: {}",
        old_key_metrics.body
    );

    let new_key_metrics = http_request_with_headers(
        server.bind_addr(),
        "GET",
        "/metrics",
        &admin_auth_headers(new_key.as_str()),
        None,
    )
    .expect("new-key metrics request should return an HTTP response");
    assert_eq!(
        new_key_metrics.status, 200,
        "new key should be accepted after rotation, body: {}",
        new_key_metrics.body
    );
}

// Hardened offline-reset contract (engine/flapjack-http/src/auth/key_store.rs):
// once the server has persisted encrypted key material, offline
// `reset-admin-key` is refused so existing search keys are not orphaned; the
// operator must use the online `/internal/rotate-admin-key` endpoint instead.
#[test]
fn reset_admin_key_offline_refused_when_encrypted_key_material_present() {
    let tmp = TempDir::new("fj_test_reset_refused");

    let bootstrap_server = RunningServer::spawn_auth_auto_port(tmp.path());
    drop(bootstrap_server);

    // A normal server start persists encrypted HMAC material for the admin key.
    assert!(
        tmp.root().join("key_material.json").exists(),
        "server start should persist key_material.json"
    );

    let keys_before = std::fs::read_to_string(tmp.root().join("keys.json"))
        .expect("keys.json should exist after server start");
    let hash_before = extract_admin_key_hash_from_json(&keys_before);

    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("reset-admin-key")
        .output()
        .expect("failed to run");

    assert!(
        !output.status.success(),
        "offline reset-admin-key must be refused while encrypted key material exists"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("Cannot reset admin key offline"),
        "expected offline-reset refusal message, got: {}",
        stderr
    );
    assert!(
        stderr.contains("/internal/rotate-admin-key"),
        "refusal must point operators at the online rotate endpoint, got: {}",
        stderr
    );

    let keys_after = std::fs::read_to_string(tmp.root().join("keys.json"))
        .expect("keys.json should still exist after refused reset");
    let hash_after = extract_admin_key_hash_from_json(&keys_after);
    assert_eq!(
        hash_before, hash_after,
        "refused offline reset must not rotate the admin key hash"
    );
}

// Offline `reset-admin-key` still works in the supported scenario: a keys.json
// with an admin entry but no encrypted search-key material, so rotating the
// admin key orphans nothing.
#[test]
fn reset_admin_key_offline_succeeds_without_encrypted_key_material() {
    let tmp = TempDir::new("fj_test_reset_no_material");

    let bootstrap_server = RunningServer::spawn_auth_auto_port(tmp.path());
    drop(bootstrap_server);

    // Remove the encrypted key material so the offline-reset guard allows the
    // rotation (no search keys would be orphaned).
    std::fs::remove_file(tmp.root().join("key_material.json"))
        .expect("key_material.json should exist to remove");

    let keys_before = std::fs::read_to_string(tmp.root().join("keys.json"))
        .expect("keys.json should exist after server start");
    let hash_before = extract_admin_key_hash_from_json(&keys_before);

    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("reset-admin-key")
        .output()
        .expect("failed to run");

    assert!(
        output.status.success(),
        "offline reset-admin-key should succeed without encrypted key material, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let new_key = stdout.trim();

    assert!(
        new_key.starts_with("fj_admin_"),
        "Expected fj_admin_ prefixed key, got: {}",
        new_key
    );
    assert_eq!(
        new_key.len(),
        41,
        "Key should be 41 chars (fj_admin_ + 32 hex), got {} chars: {}",
        new_key.len(),
        new_key
    );
    assert!(
        new_key[9..].chars().all(|c| c.is_ascii_hexdigit()),
        "Key suffix (after fj_admin_) should be hex, got: {}",
        new_key
    );

    let keys_after = std::fs::read_to_string(tmp.root().join("keys.json"))
        .expect("keys.json should still exist after reset");
    let hash_after = extract_admin_key_hash_from_json(&keys_after);
    assert_ne!(
        hash_before, hash_after,
        "Admin key hash must change after reset-admin-key"
    );
    assert!(
        admin_entry_exists_in_json(&keys_after),
        "Admin API Key entry must still exist in keys.json after reset"
    );
}

#[test]
fn reset_admin_key_fails_without_keys_json() {
    let tmp = TempDir::new("fj_test_reset_no_file");

    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("reset-admin-key")
        .output()
        .expect("failed to run");

    assert!(
        !output.status.success(),
        "reset-admin-key should fail without keys.json"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("No keys.json found"),
        "Expected 'No keys.json found' error, got: {}",
        stderr
    );
}

// Hardened contract: when offline `reset-admin-key` is refused (encrypted key
// material present) it must be a no-op — the previously issued admin key keeps
// authorizing requests both on the running server and after a restart. Rotating
// in a way that actually invalidates the old key is the online
// `/internal/rotate-admin-key` endpoint's job (see the rotate test above).
#[test]
fn reset_admin_key_offline_refusal_preserves_previous_key_after_restart() {
    let tmp = TempDir::new("fj_test_reset_refusal_preserves_key");
    let server_before_restart = RunningServer::spawn_auth_auto_port(tmp.path());
    let admin_key_path = tmp.root().join(".admin_key");
    let old_key = std::fs::read_to_string(&admin_key_path)
        .expect("startup should persist admin key to .admin_key")
        .trim()
        .to_string();

    let output = flapjack_cmd()
        .arg("--data-dir")
        .arg(tmp.path())
        .arg("reset-admin-key")
        .output()
        .expect("failed to run");
    assert!(
        !output.status.success(),
        "offline reset-admin-key must be refused while encrypted key material exists"
    );

    let old_key_before_restart = http_request_with_headers(
        server_before_restart.bind_addr(),
        "GET",
        "/metrics",
        &admin_auth_headers(old_key.as_str()),
        None,
    )
    .expect("old-key request before restart should return an HTTP response");
    assert_eq!(
        old_key_before_restart.status, 200,
        "refused reset must not change in-memory auth for a running server"
    );

    drop(server_before_restart);
    let server_after_restart = RunningServer::spawn_auth_auto_port(tmp.path());

    let old_key_metrics = http_request_with_headers(
        server_after_restart.bind_addr(),
        "GET",
        "/metrics",
        &admin_auth_headers(old_key.as_str()),
        None,
    )
    .expect("old-key metrics request should return an HTTP response");
    assert_eq!(
        old_key_metrics.status, 200,
        "refused offline reset must leave the previous admin key valid after restart, body: {}",
        old_key_metrics.body
    );
}

// Concurrent rotation requests can legitimately split into 200s that reached
// the handler before the first rotation committed and 403s that authenticated
// after the starting key was invalidated. Regardless of that timing, the final
// `.admin_key` file must contain an authorized key.
#[test]
fn rotate_admin_key_concurrent_requests_allow_only_documented_outcomes() {
    let tmp = TempDir::new("fj_test_rotate_admin_key_concurrent");
    let server = RunningServer::spawn_auth_auto_port(tmp.path());
    let key_path = tmp.root().join(".admin_key");
    let starting_key = std::fs::read_to_string(&key_path)
        .expect("startup should persist admin key to .admin_key")
        .trim()
        .to_string();

    let mut handles = Vec::new();
    for _ in 0..6 {
        let bind_addr = server.bind_addr().to_string();
        let key = starting_key.clone();
        handles.push(std::thread::spawn(move || {
            http_request_with_headers(
                &bind_addr,
                "POST",
                "/internal/rotate-admin-key",
                &admin_auth_headers(key.as_str()),
                None,
            )
            .expect("rotate-admin-key request should return an HTTP response")
        }));
    }

    let responses: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().expect("rotate worker should join"))
        .collect();
    assert!(
        responses
            .iter()
            .all(|response| response.status == 200 || response.status == 403),
        "concurrent rotate-admin-key responses must be only 200 or 403"
    );

    let successful_payload_keys: Vec<String> = responses
        .iter()
        .filter(|response| response.status == 200)
        .map(|response| {
            serde_json::from_str::<serde_json::Value>(&response.body)
                .expect("200 rotate response should be valid JSON")["key"]
                .as_str()
                .expect("200 rotate response should include key")
                .to_string()
        })
        .collect();
    assert!(
        !successful_payload_keys.is_empty(),
        "at least one concurrent rotate request must succeed"
    );

    let persisted_key = std::fs::read_to_string(&key_path)
        .expect(".admin_key should exist after concurrent rotations")
        .trim()
        .to_string();
    assert!(
        successful_payload_keys.contains(&persisted_key),
        "persisted admin key must come from one successful rotation response"
    );

    let old_key_metrics = http_request_with_headers(
        server.bind_addr(),
        "GET",
        "/metrics",
        &admin_auth_headers(starting_key.as_str()),
        None,
    )
    .expect("old-key metrics request should return an HTTP response");
    assert_eq!(
        old_key_metrics.status, 403,
        "starting key should be invalid after concurrent rotations"
    );

    let persisted_key_metrics = http_request_with_headers(
        server.bind_addr(),
        "GET",
        "/metrics",
        &admin_auth_headers(persisted_key.as_str()),
        None,
    )
    .expect("persisted-key metrics request should return an HTTP response");
    assert_eq!(
        persisted_key_metrics.status, 200,
        "persisted final key should authorize requests after concurrent rotations"
    );
}
