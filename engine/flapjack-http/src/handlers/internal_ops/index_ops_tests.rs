//! Direct tests for the replicated index-operation position owner.

use super::*;
use flapjack::index::settings::IndexSettings;
use flapjack::types::Document;
use tempfile::TempDir;

#[cfg(unix)]
fn unix_mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_unix_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn assert_directory_write_denied(dir: &Path) {
    assert!(
        std::fs::write(dir.join("permission-probe"), b"probe").is_err(),
        "directory permission denial is unsupported on this host"
    );
}

fn positions_with(
    node_id: &str,
    index_op: Option<Position>,
    clear: Option<Position>,
) -> TenantPositions {
    let mut positions = TenantPositions::default();
    positions
        .nodes
        .insert(node_id.to_string(), NodePositions { index_op, clear });
    positions
}

fn empty_record_bytes() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"schema_version": 1, "nodes": {}})).unwrap()
}

fn write_evidence(base_path: &Path, tenant_id: &str, bytes: &[u8]) -> PathBuf {
    let path = positions_path(base_path, tenant_id);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    path
}

fn no_temp_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".tmp")
    })
}

#[tokio::test]
async fn tenant_positions_loader_accepts_valid_empty_and_populated_records() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    write_evidence(tmp.path(), "empty", br#"{"schema_version":1,"nodes":{}}"#);
    assert_eq!(
        load_tenant_positions(&manager, "empty").unwrap(),
        TenantPositions::default()
    );

    write_evidence(
        tmp.path(),
        "populated",
        br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[2000,4],"clear":null},"node-b":{"index_op":null,"clear":null},"node-c":{"index_op":[3000,7],"clear":[3000,7]}}}"#,
    );
    let mut expected = positions_with("node-a", Some((2_000, 4)), None);
    expected
        .nodes
        .insert("node-b".to_string(), NodePositions::default());
    expected.nodes.insert(
        "node-c".to_string(),
        NodePositions {
            index_op: Some((3_000, 7)),
            clear: Some((3_000, 7)),
        },
    );
    assert_eq!(
        load_tenant_positions(&manager, "populated").unwrap(),
        expected
    );
}

#[tokio::test]
async fn tenant_positions_loader_rejects_invalid_evidence_without_resetting_it() {
    let fixtures: [(&str, &[u8]); 27] = [
        ("malformed-json", b"{"),
        ("missing-schema-version", br#"{"nodes":{}}"#),
        ("missing-nodes", br#"{"schema_version":1}"#),
        ("unknown-root-field", br#"{"schema_version":1,"nodes":{},"extra":true}"#),
        ("unsupported-version", br#"{"schema_version":2,"nodes":{}}"#),
        ("nodes-not-object", br#"{"schema_version":1,"nodes":[]}"#),
        ("empty-node-id", br#"{"schema_version":1,"nodes":{"":{"index_op":null,"clear":null}}}"#),
        ("missing-node-field", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,1]}}}"#),
        ("missing-index-op-field", br#"{"schema_version":1,"nodes":{"node-a":{"clear":null}}}"#),
        ("unknown-node-field", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,1],"clear":null,"extra":true}}}"#),
        ("short-tuple", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1],"clear":null}}}"#),
        ("signed-tuple", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[-1,2],"clear":null}}}"#),
        ("overlong-tuple", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,2,3],"clear":null}}}"#),
        ("tuple-not-array", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":{"timestamp_ms":1,"seq":2},"clear":null}}}"#),
        ("float-timestamp", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1.5,2],"clear":null}}}"#),
        ("float-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,2.5],"clear":null}}}"#),
        ("string-timestamp", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":["1",2],"clear":null}}}"#),
        ("string-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,"2"],"clear":null}}}"#),
        ("null-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,null],"clear":null}}}"#),
        ("overflow-timestamp", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[18446744073709551616,2],"clear":null}}}"#),
        ("overflow-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,18446744073709551616],"clear":null}}}"#),
        ("negative-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":[1,-2],"clear":null}}}"#),
        ("clear-short-tuple", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":null,"clear":[1]}}}"#),
        ("clear-overlong-tuple", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":null,"clear":[1,2,3]}}}"#),
        ("clear-signed-tuple", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":null,"clear":[-1,2]}}}"#),
        ("clear-float-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":null,"clear":[1,2.5]}}}"#),
        ("clear-string-seq", br#"{"schema_version":1,"nodes":{"node-a":{"index_op":null,"clear":[1,"2"]}}}"#),
    ];
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let mut accepted = Vec::new();
    for (case, bytes) in fixtures {
        let path = write_evidence(tmp.path(), case, bytes);
        if load_tenant_positions(&manager, case).is_ok() {
            accepted.push(case);
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            bytes,
            "{case}: invalid evidence must never be rewritten"
        );
    }
    assert!(
        accepted.is_empty(),
        "invalid evidence must fail closed; accepted fixtures: {accepted:?}"
    );
    assert!(no_temp_files(&positions_root(tmp.path())));
}

#[cfg(unix)]
#[tokio::test]
async fn tenant_positions_loader_rejects_unsafe_evidence_files() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let root = positions_root(tmp.path());
    std::fs::create_dir(&root).unwrap();

    std::fs::create_dir(positions_path(tmp.path(), "directory")).unwrap();
    assert!(load_tenant_positions(&manager, "directory").is_err());

    let target = tmp.path().join("outside-position.json");
    std::fs::write(&target, empty_record_bytes()).unwrap();
    std::os::unix::fs::symlink(&target, positions_path(tmp.path(), "symlink")).unwrap();
    assert!(load_tenant_positions(&manager, "symlink").is_err());
    assert_eq!(std::fs::read(&target).unwrap(), empty_record_bytes());

    let unreadable = write_evidence(tmp.path(), "unreadable", &empty_record_bytes());
    set_unix_mode(&unreadable, 0o000);
    assert!(
        std::fs::read(&unreadable).is_err(),
        "unreadable fixture is unsupported on this host"
    );
    let result = load_tenant_positions(&manager, "unreadable");
    set_unix_mode(&unreadable, 0o600);
    assert!(result.is_err());
    assert_eq!(std::fs::read(&unreadable).unwrap(), empty_record_bytes());
    assert!(no_temp_files(&root));
}

#[cfg(unix)]
#[tokio::test]
async fn tenant_positions_loader_rejects_unsafe_evidence_roots() {
    let file_root = TempDir::new().unwrap();
    std::fs::write(positions_root(file_root.path()), b"not a directory").unwrap();
    let manager = IndexManager::new(file_root.path());
    assert!(load_tenant_positions(&manager, "tenant").is_err());
    assert_eq!(
        std::fs::read(positions_root(file_root.path())).unwrap(),
        b"not a directory"
    );

    let link_root = TempDir::new().unwrap();
    let real = link_root.path().join("real-root");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("tenant.json"), empty_record_bytes()).unwrap();
    std::os::unix::fs::symlink(&real, positions_root(link_root.path())).unwrap();
    let manager = IndexManager::new(link_root.path());
    assert!(load_tenant_positions(&manager, "tenant").is_err());
    assert!(!real.join("missing.json").exists());
    assert!(load_tenant_positions(&manager, "missing").is_err());
    assert!(
        !real.join("missing.json").exists(),
        "a symlinked root must not receive initialized evidence"
    );
}

#[tokio::test]
async fn tenant_positions_absent_root_is_initialized_privately_and_durably() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let root = positions_root(tmp.path());
    assert!(!root.exists());

    assert_eq!(
        load_tenant_positions(&manager, "first").unwrap(),
        TenantPositions::default()
    );

    let path = positions_path(tmp.path(), "first");
    assert!(std::fs::symlink_metadata(&root).unwrap().is_dir());
    assert_eq!(std::fs::read(&path).unwrap(), empty_record_bytes());
    #[cfg(unix)]
    {
        assert_eq!(unix_mode(&root), 0o700);
        assert_eq!(unix_mode(&path), 0o600);
    }
    assert!(no_temp_files(&root));
}

#[tokio::test]
async fn tenant_positions_absent_file_in_existing_root_is_initialized() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let root = positions_root(tmp.path());
    flapjack::index::ensure_private_directory(&root).unwrap();
    write_evidence(tmp.path(), "other", &empty_record_bytes());

    assert_eq!(
        load_tenant_positions(&manager, "fresh").unwrap(),
        TenantPositions::default()
    );

    let path = positions_path(tmp.path(), "fresh");
    assert_eq!(std::fs::read(&path).unwrap(), empty_record_bytes());
    #[cfg(unix)]
    assert_eq!(unix_mode(&path), 0o600);
    assert_eq!(
        std::fs::read(positions_path(tmp.path(), "other")).unwrap(),
        empty_record_bytes()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn tenant_positions_absent_file_in_unwritable_root_fails_without_repairing_it() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    let root = positions_root(tmp.path());
    flapjack::index::ensure_private_directory(&root).unwrap();
    set_unix_mode(&root, 0o500);
    assert_directory_write_denied(&root);

    let result = load_tenant_positions(&manager, "blocked");
    let mode_after = unix_mode(&root);
    let evidence_created = positions_path(tmp.path(), "blocked").exists();
    set_unix_mode(&root, 0o700);

    assert!(
        result.is_err(),
        "an unwritable root must fail before dispatch"
    );
    assert_eq!(
        mode_after, 0o500,
        "loading must not repair an existing root"
    );
    assert!(!evidence_created);
    assert_eq!(
        load_tenant_positions(&manager, "blocked").unwrap(),
        TenantPositions::default(),
        "the same tenant must load once the root is writable again"
    );
}

#[tokio::test]
async fn tenant_positions_persist_round_trips_privately_and_survives_restart() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    assert_eq!(
        load_tenant_positions(&manager, "durable").unwrap(),
        TenantPositions::default()
    );
    let mut positions = positions_with("node-a", Some((2_000, 4)), Some((2_000, 4)));
    positions
        .nodes
        .insert("node-b".to_string(), NodePositions::default());

    persist_tenant_positions(&manager, "durable", &positions).unwrap();

    let path = positions_path(tmp.path(), "durable");
    let stored: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(
        stored,
        serde_json::json!({
            "schema_version": 1,
            "nodes": {
                "node-a": {"index_op": [2_000, 4], "clear": [2_000, 4]},
                "node-b": {"index_op": null, "clear": null}
            }
        })
    );
    #[cfg(unix)]
    assert_eq!(unix_mode(&path), 0o600);
    assert_eq!(
        load_tenant_positions(&manager, "durable").unwrap(),
        positions
    );

    drop(manager);
    let manager = IndexManager::new(tmp.path());
    assert_eq!(
        load_tenant_positions(&manager, "durable").unwrap(),
        positions
    );
    assert!(no_temp_files(&positions_root(tmp.path())));
}

#[cfg(unix)]
#[tokio::test]
async fn tenant_positions_persist_failure_keeps_prior_evidence_unchanged() {
    let tmp = TempDir::new().unwrap();
    let manager = IndexManager::new(tmp.path());
    assert_eq!(
        load_tenant_positions(&manager, "prior").unwrap(),
        TenantPositions::default()
    );
    let prior = positions_with("node-a", Some((1_000, 1)), None);
    persist_tenant_positions(&manager, "prior", &prior).unwrap();
    let path = positions_path(tmp.path(), "prior");
    let prior_bytes = std::fs::read(&path).unwrap();
    let root = positions_root(tmp.path());
    set_unix_mode(&root, 0o500);
    assert_directory_write_denied(&root);

    let result = persist_tenant_positions(
        &manager,
        "prior",
        &positions_with("node-a", Some((2_000, 4)), None),
    );
    let bytes_after = std::fs::read(&path).unwrap();
    let temp_free = no_temp_files(&root);
    set_unix_mode(&root, 0o700);

    assert!(result.is_err(), "a pre-rename failure must be reported");
    assert_eq!(
        bytes_after, prior_bytes,
        "prior evidence must survive a failed update"
    );
    assert!(temp_free);
    assert_eq!(load_tenant_positions(&manager, "prior").unwrap(), prior);
}

fn copy_index_entry(payload: serde_json::Value) -> OpLogEntry {
    OpLogEntry {
        seq: 9,
        timestamp_ms: 42,
        node_id: "node-a".to_string(),
        tenant_id: "tenant-a".to_string(),
        op_type: "copy_index".to_string(),
        payload,
    }
}

#[test]
fn parse_copy_scope_defaults_to_none_when_missing_or_null() {
    let missing_scope = copy_index_entry(serde_json::json!({
        "source": "src",
        "destination": "dst"
    }));
    let null_scope = copy_index_entry(serde_json::json!({
        "source": "src",
        "destination": "dst",
        "scope": null
    }));

    assert_eq!(parse_copy_scope("tenant-a", &missing_scope).unwrap(), None);
    assert_eq!(parse_copy_scope("tenant-a", &null_scope).unwrap(), None);
}

#[test]
fn parse_copy_scope_parses_string_list() {
    let scoped_entry = copy_index_entry(serde_json::json!({
        "source": "src",
        "destination": "dst",
        "scope": ["settings", "rules"]
    }));

    assert_eq!(
        parse_copy_scope("tenant-a", &scoped_entry).unwrap(),
        Some(vec!["settings".to_string(), "rules".to_string()])
    );
}

#[path = "index_ops_apply_tests.rs"]
mod apply_tests;
