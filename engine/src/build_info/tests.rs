use super::*;
use serde_json::json;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

fn raw_inputs() -> RawBuildInputs {
    RawBuildInputs {
        revision: Some(REVISION.to_owned()),
        dirty: Some(false),
        workspace_digest: "b6b44f584f1f23eae6fbc857b48bb749518e3f61576e346d6afc568f3d51d89e"
            .to_owned(),
        profile: "release".to_owned(),
        target: "x86_64-unknown-linux-gnu".to_owned(),
    }
}

/// TODO: Document build_info_serializes_exact_public_contract.
#[test]
fn build_info_serializes_exact_public_contract() {
    let info = build_info_from_inputs(
        "1.2.3",
        raw_inputs(),
        [
            "vector-search-local",
            "analytics",
            "vector-search",
            "analytics",
        ],
    );

    let value = serde_json::to_value(&info).unwrap();
    assert_eq!(
        value,
        json!({
            "schemaVersion": 1,
            "version": "1.2.3",
            "revision": REVISION,
            "revisionKnown": true,
            "dirty": false,
            "dirtyKnown": true,
            "workspaceDigest": "b6b44f584f1f23eae6fbc857b48bb749518e3f61576e346d6afc568f3d51d89e",
            "profile": "release",
            "target": "x86_64-unknown-linux-gnu",
            "features": ["analytics", "vector-search", "vector-search-local"],
            "capabilities": {
                "vectorSearch": true,
                "vectorSearchLocal": true
            }
        })
    );

    let serialized = serde_json::to_string(&value).unwrap();
    assert_eq!(canonical_build_info_json(&info).unwrap(), serialized);
    for forbidden in [
        "migration",
        "Migration",
        "timestamp",
        "Timestamp",
        "/Users/",
        "/home/",
        r"C:\\",
    ] {
        assert!(!serialized.contains(forbidden), "found {forbidden}");
    }
}

/// TODO: Document unknown_vcs_values_serialize_as_honest_null_pairs.
#[test]
fn unknown_vcs_values_serialize_as_honest_null_pairs() {
    let mut inputs = raw_inputs();
    inputs.revision = None;
    inputs.dirty = None;

    let value = serde_json::to_value(build_info_from_inputs(
        "1.2.3",
        inputs,
        std::iter::empty::<&str>(),
    ))
    .unwrap();
    assert_eq!(value["revision"], serde_json::Value::Null);
    assert_eq!(value["revisionKnown"], false);
    assert_eq!(value["dirty"], serde_json::Value::Null);
    assert_eq!(value["dirtyKnown"], false);
    assert_eq!(
        value["capabilities"],
        json!({"vectorSearch": false, "vectorSearchLocal": false})
    );
}

/// TODO: Document capability_projection_covers_vector_feature_combinations.
#[test]
fn capability_projection_covers_vector_feature_combinations() {
    let cases = [
        (
            vec![
                "analytics",
                "axum-support",
                "decompound",
                "default",
                "openapi",
                "s3-snapshots",
            ],
            false,
            false,
        ),
        (vec!["vector-search"], true, false),
        (vec!["vector-search-local"], true, true),
        (vec!["vector-search", "vector-search-local"], true, true),
    ];

    for (features, vector_search, vector_search_local) in cases {
        let info = build_info_from_inputs("1.2.3", raw_inputs(), features);
        assert_eq!(info.capabilities.vector_search, vector_search);
        assert_eq!(info.capabilities.vector_search_local, vector_search_local);
        assert!(!info.capabilities.vector_search_local || info.capabilities.vector_search);
    }
}

#[test]
fn enabled_core_features_excludes_umbrella_and_reports_concrete_features() {
    let mut expected = vec![
        "analytics",
        "axum-support",
        "decompound",
        "openapi",
        "s3-snapshots",
    ];
    if cfg!(feature = "memory-stats") {
        expected.push("memory-stats");
    }
    if cfg!(feature = "vector-search") {
        expected.push("vector-search");
    }
    if cfg!(feature = "vector-search-local") {
        expected.push("vector-search-local");
    }
    expected.sort_unstable();

    let features = enabled_core_features();
    assert!(
        !features.contains(&"default"),
        "the Cargo umbrella default feature must not be reported as a runtime capability"
    );
    assert_eq!(
        features, expected,
        "the build must report every enabled concrete capability"
    );
}

#[test]
fn enabled_core_features_from_env_preserves_canonical_order() {
    let features = enabled_core_features_from_env(|env_name| {
        matches!(
            env_name,
            "CARGO_FEATURE_VECTOR_SEARCH"
                | "CARGO_FEATURE_ANALYTICS"
                | "CARGO_FEATURE_AXUM_SUPPORT"
        )
    });

    assert_eq!(features, vec!["analytics", "axum-support", "vector-search"]);
}

#[test]
fn embedded_build_info_json_round_trips_exact_canonical_bytes() {
    let info = build_info_from_inputs(
        "1.2.3",
        raw_inputs(),
        ["vector-search", "analytics", "analytics"],
    );
    let canonical = canonical_build_info_json(&info).unwrap();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"prefix");
    bytes.extend_from_slice(BUILD_INFO_JSON_BEGIN_MARKER);
    bytes.extend_from_slice(canonical.as_bytes());
    bytes.extend_from_slice(BUILD_INFO_JSON_END_MARKER);
    bytes.extend_from_slice(b"suffix");

    assert_eq!(
        embedded_build_info_json_from_bytes(&bytes).unwrap(),
        canonical
    );
}

#[test]
fn runtime_build_info_is_owned_by_the_embedded_record() {
    let scanned = embedded_build_info_json_from_bytes(&EMBEDDED_BUILD_INFO_JSON)
        .expect("the crate-owned static should hold exactly one well-formed record");

    assert!(
        !scanned.is_empty(),
        "the crate-owned static must carry the build script's JSON, not an empty payload"
    );
    // The offset-sliced runtime read and an independent marker scan of the same static
    // must agree, so a layout drift cannot silently feed callers a different record.
    assert_eq!(scanned, embedded_build_info_json());
    assert_eq!(scanned, canonical_build_info_json(build_info()).unwrap());
}

#[test]
fn runtime_build_info_json_is_served_from_retained_static() {
    let json = embedded_build_info_json();
    let static_start = EMBEDDED_BUILD_INFO_JSON.as_ptr() as usize;
    let static_payload_start = static_start + EMBEDDED_BUILD_INFO_JSON_OFFSET;
    let static_payload_end = static_payload_start + EMBEDDED_BUILD_INFO_JSON_LEN;
    let json_start = json.as_ptr() as usize;
    let json_end = json_start + json.len();

    assert!(
        !json.is_empty(),
        "runtime build-info JSON must not be empty"
    );
    assert_eq!(
        (json_start, json_end),
        (static_payload_start, static_payload_end),
        "runtime build-info JSON must be a slice of the retained embedded static"
    );
}

#[test]
fn embedded_build_info_json_rejects_missing_duplicate_and_malformed_records() {
    assert_eq!(
        embedded_build_info_json_from_bytes(b"no record").unwrap_err(),
        "embedded build-info JSON begin marker must appear exactly once, found 0"
    );

    let info = canonical_build_info_json(&build_info_from_inputs(
        "1.2.3",
        raw_inputs(),
        std::iter::empty::<&str>(),
    ))
    .unwrap();
    let mut duplicate = Vec::new();
    for _ in 0..2 {
        duplicate.extend_from_slice(BUILD_INFO_JSON_BEGIN_MARKER);
        duplicate.extend_from_slice(info.as_bytes());
        duplicate.extend_from_slice(BUILD_INFO_JSON_END_MARKER);
    }
    assert_eq!(
        embedded_build_info_json_from_bytes(&duplicate).unwrap_err(),
        "embedded build-info JSON begin marker must appear exactly once, found 2"
    );

    let mut orphan_end = Vec::new();
    orphan_end.extend_from_slice(BUILD_INFO_JSON_END_MARKER);
    orphan_end.extend_from_slice(BUILD_INFO_JSON_BEGIN_MARKER);
    orphan_end.extend_from_slice(info.as_bytes());
    orphan_end.extend_from_slice(BUILD_INFO_JSON_END_MARKER);
    assert_eq!(
        embedded_build_info_json_from_bytes(&orphan_end).unwrap_err(),
        "embedded build-info JSON end marker must appear exactly once, found 2"
    );

    let mut malformed = Vec::new();
    malformed.extend_from_slice(BUILD_INFO_JSON_BEGIN_MARKER);
    malformed.extend_from_slice(br#"{"schemaVersion":1}"#);
    malformed.extend_from_slice(BUILD_INFO_JSON_END_MARKER);
    assert!(embedded_build_info_json_from_bytes(&malformed)
        .unwrap_err()
        .contains("embedded build-info JSON is malformed"),);
}

#[test]
fn build_info_from_inputs_normalizes_explicit_feature_capabilities() {
    let info = build_info_from_inputs(
        "1.2.3",
        raw_inputs(),
        ["vector-search-local", "axum-support", "axum-support"],
    );

    assert_eq!(info.features, vec!["axum-support", "vector-search-local"]);
    assert!(info.capabilities.vector_search);
    assert!(info.capabilities.vector_search_local);
}

/// TODO: Document vcs_discovery_preserves_independent_known_states.
#[test]
fn vcs_discovery_preserves_independent_known_states() {
    struct Case {
        name: &'static str,
        revision_result: Result<&'static str, &'static str>,
        dirty_result: Result<&'static str, &'static str>,
        expected_revision: Option<&'static str>,
        expected_dirty: Option<bool>,
    }

    let cases = [
        Case {
            name: "clean checkout",
            revision_result: Ok(REVISION),
            dirty_result: Ok(""),
            expected_revision: Some(REVISION),
            expected_dirty: Some(false),
        },
        Case {
            name: "dirty checkout",
            revision_result: Ok(REVISION),
            dirty_result: Ok(" M src/lib.rs\n"),
            expected_revision: Some(REVISION),
            expected_dirty: Some(true),
        },
        Case {
            name: "git unavailable",
            revision_result: Err("git unavailable"),
            dirty_result: Err("git unavailable"),
            expected_revision: None,
            expected_dirty: None,
        },
        Case {
            name: "dirty known while revision unavailable",
            revision_result: Err("not a checkout"),
            dirty_result: Ok(""),
            expected_revision: None,
            expected_dirty: Some(false),
        },
        Case {
            name: "revision known while dirty unavailable",
            revision_result: Ok(REVISION),
            dirty_result: Err("status unavailable"),
            expected_revision: Some(REVISION),
            expected_dirty: None,
        },
    ];

    for case in cases {
        let vcs = discover_vcs(|arguments| match arguments {
            ["rev-parse", "HEAD"] => case
                .revision_result
                .map(str::to_owned)
                .map_err(str::to_owned),
            ["status", "--porcelain"] => {
                case.dirty_result.map(str::to_owned).map_err(str::to_owned)
            }
            _ => unreachable!("unexpected Git arguments: {arguments:?}"),
        });
        assert_eq!(
            vcs,
            VcsState {
                revision: case.expected_revision.map(str::to_owned),
                dirty: case.expected_dirty,
            },
            "{}",
            case.name
        );
    }
}

/// TODO: Document revision_override_is_validated_and_does_not_invent_dirty_state.
#[test]
fn revision_override_is_validated_and_does_not_invent_dirty_state() {
    let uppercase_revision = REVISION.to_uppercase();
    let inputs = collect_vcs_inputs(Some(&uppercase_revision), || {
        panic!("valid override must not require VCS")
    })
    .unwrap();
    assert_eq!(inputs.revision.as_deref(), Some(REVISION));
    assert_eq!(inputs.dirty, None);

    let revision_with_leading_space = format!(" {REVISION}");
    for malformed in [
        "",
        "abc123",
        "g123456789abcdef0123456789abcdef01234567",
        &revision_with_leading_space,
    ] {
        let error = collect_vcs_inputs(Some(malformed), VcsState::default).unwrap_err();
        assert!(error.contains("FLAPJACK_BUILD_REVISION"), "{error}");
        assert!(error.contains("40 hexadecimal"), "{error}");
    }
}

/// TODO: Document vcs_invalidation_tracks_head_and_active_branch_ref.
#[test]
fn vcs_invalidation_tracks_head_and_active_branch_ref() {
    let paths = vcs_invalidation_paths(|arguments| match arguments {
        ["rev-parse", "--git-path", "HEAD"] => Ok(".git/worktrees/stage/HEAD\n".to_owned()),
        ["symbolic-ref", "-q", "HEAD"] => Ok("refs/heads/main\n".to_owned()),
        ["rev-parse", "--git-path", "refs/heads/main"] => Ok(".git/refs/heads/main\n".to_owned()),
        _ => unreachable!("unexpected Git arguments: {arguments:?}"),
    });

    assert_eq!(
        paths,
        vec![
            PathBuf::from(".git/refs/heads/main"),
            PathBuf::from(".git/worktrees/stage/HEAD"),
        ]
    );
}

/// TODO: Document workspace_digest_is_order_independent_and_known.
#[test]
fn workspace_digest_is_order_independent_and_known() {
    let first = TempDir::new().unwrap();
    let second = TempDir::new().unwrap();
    write_fixture(first.path(), false);
    write_fixture(second.path(), true);

    let first_digest = workspace_digest(first.path()).unwrap();
    let second_digest = workspace_digest(second.path()).unwrap();
    assert_eq!(first_digest, second_digest);
    assert_eq!(
        first_digest, "e4952c4d3e1bcc82810dd9a5df3f18601f6ffa5dccd08444bd535e75c74b8294",
        "digest contract must have a fixed known answer"
    );
    assert_eq!(first_digest.len(), 64);
    assert!(first_digest
        .chars()
        .all(|character| character.is_ascii_hexdigit()));
    assert_eq!(first_digest, first_digest.to_lowercase());
}

/// TODO: Document workspace_digest_changes_for_included_bytes_and_relative_paths.
#[test]
fn workspace_digest_changes_for_included_bytes_and_relative_paths() {
    let fixture = TempDir::new().unwrap();
    write_fixture(fixture.path(), false);
    let original = workspace_digest(fixture.path()).unwrap();

    fs::write(fixture.path().join("src/lib.rs"), b"pub fn changed() {}\n").unwrap();
    let bytes_changed = workspace_digest(fixture.path()).unwrap();
    assert_ne!(original, bytes_changed);

    fs::rename(
        fixture.path().join("src/lib.rs"),
        fixture.path().join("src/renamed.rs"),
    )
    .unwrap();
    let path_changed = workspace_digest(fixture.path()).unwrap();
    assert_ne!(bytes_changed, path_changed);
}

/// TODO: Document workspace_digest_includes_nested_rust_sources_with_generated_output_names.
#[test]
fn workspace_digest_includes_nested_rust_sources_with_generated_output_names() {
    let fixture = TempDir::new().unwrap();
    write_fixture(fixture.path(), false);
    let original = workspace_digest(fixture.path()).unwrap();

    write_file(
        fixture.path(),
        "src/dist/mod.rs",
        b"pub fn real_dist_module() {}\n",
    );
    let dist_module_digest = workspace_digest(fixture.path()).unwrap();
    assert_ne!(original, dist_module_digest);

    write_file(
        fixture.path(),
        "src/target/mod.rs",
        b"pub fn real_target_module() {}\n",
    );
    let target_module_digest = workspace_digest(fixture.path()).unwrap();
    assert_ne!(dist_module_digest, target_module_digest);

    let included_paths = workspace_digest_paths(fixture.path()).unwrap();
    assert!(included_paths
        .iter()
        .any(|path| path == Path::new("src/dist/mod.rs")));
    assert!(included_paths
        .iter()
        .any(|path| path == Path::new("src/target/mod.rs")));
}

/// TODO: Document workspace_digest_ignores_generated_local_metadata_and_absolute_root.
#[test]
fn workspace_digest_ignores_generated_local_metadata_and_absolute_root() {
    let fixture_root = TempDir::new().unwrap();
    let first = fixture_root.path().join("ordinary/workspace");
    let second = fixture_root.path().join("target/workspace");
    write_fixture(&first, false);
    write_fixture(&second, false);
    let expected = workspace_digest(&first).unwrap();

    for relative_path in [
        ".git/index",
        "target/debug/flapjack",
        "node_modules/package/index.js",
        "dashboard/dist/index.html",
        ".cache/value",
        "src/__pycache__/generated.rs",
        ".DS_Store",
    ] {
        write_file(&first, relative_path, b"local mutation");
    }
    let included_paths = workspace_digest_paths(&first).unwrap();
    let actual = workspace_digest(&first).unwrap();

    assert_eq!(actual, expected);
    assert_eq!(actual, workspace_digest(&second).unwrap());
    assert!(included_paths.iter().all(|path| !path.is_absolute()));
    assert!(included_paths.iter().all(|path| !path
        .to_string_lossy()
        .contains(first.to_string_lossy().as_ref())));
}

/// TODO: Document write_fixture.
fn write_fixture(root: &Path, reverse_order: bool) {
    let mut files = vec![
        ("Cargo.toml", b"[workspace]\n".as_slice()),
        ("Cargo.lock", b"version = 4\n".as_slice()),
        ("build.rs", b"fn main() {}\n".as_slice()),
        ("src/lib.rs", b"pub fn core() {}\n".as_slice()),
        (
            "flapjack-http/Cargo.toml",
            b"[package]\nname = \"flapjack-http\"\n".as_slice(),
        ),
        ("flapjack-http/build.rs", b"fn main() {}\n".as_slice()),
        ("flapjack-http/src/lib.rs", b"pub fn http() {}\n".as_slice()),
        (
            "flapjack-server/Cargo.toml",
            b"[package]\nname = \"flapjack-server\"\n".as_slice(),
        ),
        ("flapjack-server/src/main.rs", b"fn main() {}\n".as_slice()),
        (
            "flapjack-replication/Cargo.toml",
            b"[package]\nname = \"flapjack-replication\"\n".as_slice(),
        ),
        (
            "flapjack-replication/src/lib.rs",
            b"pub fn replication() {}\n".as_slice(),
        ),
        (
            "flapjack-ssl/Cargo.toml",
            b"[package]\nname = \"flapjack-ssl\"\n".as_slice(),
        ),
        ("flapjack-ssl/src/lib.rs", b"pub fn ssl() {}\n".as_slice()),
    ];
    if reverse_order {
        files.reverse();
    }
    for (path, contents) in files {
        write_file(root, path, contents);
    }
}

fn write_file(root: &Path, relative_path: &str, contents: &[u8]) {
    let path = root.join(relative_path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}
