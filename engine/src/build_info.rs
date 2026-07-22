// The collector and digest policy below are imported directly by build.rs; they are
// intentionally unused by non-test runtime library builds.
#![cfg_attr(not(test), allow(dead_code))]

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

pub const BUILD_INFO_SCHEMA_VERSION: u8 = 1;

pub(crate) const WORKSPACE_DIGEST_FILES: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "build.rs",
    "flapjack-http/Cargo.toml",
    "flapjack-http/build.rs",
    "flapjack-replication/Cargo.toml",
    "flapjack-server/Cargo.toml",
    "flapjack-ssl/Cargo.toml",
];

pub(crate) const WORKSPACE_DIGEST_RUST_DIRS: &[&str] = &[
    "src",
    "flapjack-http/src",
    "flapjack-replication/src",
    "flapjack-server/src",
    "flapjack-ssl/src",
];

const WORKSPACE_DIGEST_EXCLUDED_PATH_PREFIXES: &[&str] = &[
    ".cache",
    ".git",
    ".idea",
    ".vscode",
    "dashboard/dist",
    "node_modules",
    "src/__pycache__",
    "target",
];

const WORKSPACE_DIGEST_EXCLUDED_FILE_NAMES: &[&str] = &[".DS_Store"];

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildCapabilities {
    pub vector_search: bool,
    pub vector_search_local: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildInfo {
    pub schema_version: u8,
    pub version: String,
    pub revision: Option<String>,
    pub revision_known: bool,
    pub dirty: Option<bool>,
    pub dirty_known: bool,
    pub workspace_digest: String,
    pub profile: String,
    pub target: String,
    pub features: Vec<String>,
    pub capabilities: BuildCapabilities,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RawBuildInputs {
    pub revision: Option<String>,
    pub dirty: Option<bool>,
    pub workspace_digest: String,
    pub profile: String,
    pub target: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct VcsState {
    pub revision: Option<String>,
    pub dirty: Option<bool>,
}

/// Returns the canonical build identity for this compiled core crate.
pub fn build_info() -> &'static BuildInfo {
    static BUILD_INFO: OnceLock<BuildInfo> = OnceLock::new();
    BUILD_INFO.get_or_init(|| {
        build_info_from_inputs(
            env!("CARGO_PKG_VERSION"),
            RawBuildInputs {
                revision: non_empty(option_env!("FLAPJACK_INTERNAL_BUILD_REVISION")),
                dirty: parse_dirty(option_env!("FLAPJACK_INTERNAL_BUILD_DIRTY")),
                workspace_digest: option_env!("FLAPJACK_INTERNAL_WORKSPACE_DIGEST")
                    .unwrap_or_default()
                    .to_owned(),
                profile: option_env!("FLAPJACK_INTERNAL_BUILD_PROFILE")
                    .unwrap_or_default()
                    .to_owned(),
                target: option_env!("FLAPJACK_INTERNAL_BUILD_TARGET")
                    .unwrap_or_default()
                    .to_owned(),
            },
            enabled_core_features(),
        )
    })
}

pub(crate) fn build_info_from_inputs<I, S>(
    version: &str,
    raw: RawBuildInputs,
    features: I,
) -> BuildInfo
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let features = normalized_features(features);
    let vector_search_local = features
        .binary_search_by_key(&"vector-search-local", String::as_str)
        .is_ok();
    let vector_search = vector_search_local
        || features
            .binary_search_by_key(&"vector-search", String::as_str)
            .is_ok();

    BuildInfo {
        schema_version: BUILD_INFO_SCHEMA_VERSION,
        version: version.to_owned(),
        revision_known: raw.revision.is_some(),
        revision: raw.revision,
        dirty_known: raw.dirty.is_some(),
        dirty: raw.dirty,
        workspace_digest: raw.workspace_digest,
        profile: raw.profile,
        target: raw.target,
        features,
        capabilities: BuildCapabilities {
            vector_search,
            vector_search_local,
        },
    }
}

pub(crate) fn collect_vcs_inputs<F>(
    revision_override: Option<&str>,
    discover: F,
) -> Result<VcsState, String>
where
    F: FnOnce() -> VcsState,
{
    match revision_override {
        Some(revision) => Ok(VcsState {
            revision: Some(validate_revision_override(revision)?),
            dirty: None,
        }),
        None => Ok(discover()),
    }
}

pub(crate) fn discover_vcs<F>(mut run_git: F) -> VcsState
where
    F: FnMut(&[&str]) -> Result<String, String>,
{
    let revision = run_git(&["rev-parse", "HEAD"])
        .ok()
        .and_then(|output| normalize_revision(&output));
    let dirty = run_git(&["status", "--porcelain"])
        .ok()
        .map(|output| !output.trim().is_empty());
    VcsState { revision, dirty }
}

pub(crate) fn vcs_invalidation_paths<F>(mut run_git: F) -> Vec<PathBuf>
where
    F: FnMut(&[&str]) -> Result<String, String>,
{
    let mut paths = Vec::new();
    if let Ok(head) = run_git(&["rev-parse", "--git-path", "HEAD"]) {
        push_non_empty_path(&mut paths, head);
    }
    if let Ok(symbolic_ref) = run_git(&["symbolic-ref", "-q", "HEAD"]) {
        let symbolic_ref = symbolic_ref.trim();
        if !symbolic_ref.is_empty() {
            if let Ok(ref_path) = run_git(&["rev-parse", "--git-path", symbolic_ref]) {
                push_non_empty_path(&mut paths, ref_path);
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn push_non_empty_path(paths: &mut Vec<PathBuf>, path: String) {
    let trimmed = path.trim();
    if !trimmed.is_empty() {
        paths.push(PathBuf::from(trimmed));
    }
}

pub(crate) fn workspace_digest(workspace_root: &Path) -> io::Result<String> {
    let included_paths = workspace_digest_paths(workspace_root)?;
    let mut hasher = Sha256::new();
    for relative_path in included_paths {
        let bytes = fs::read(workspace_root.join(&relative_path))?;
        let normalized_path = relative_path_to_slashes(&relative_path)?;
        update_length_prefixed(&mut hasher, normalized_path.as_bytes());
        update_length_prefixed(&mut hasher, &bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn workspace_digest_paths(workspace_root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for relative_path in WORKSPACE_DIGEST_FILES {
        let absolute_path = workspace_root.join(relative_path);
        if absolute_path.is_file() {
            paths.push(normalized_relative_path(workspace_root, &absolute_path)?);
        }
    }
    for relative_dir in WORKSPACE_DIGEST_RUST_DIRS {
        collect_rust_sources(
            workspace_root,
            &workspace_root.join(relative_dir),
            &mut paths,
        )?;
    }
    let mut normalized_paths = paths
        .into_iter()
        .map(|path| relative_path_to_slashes(&path).map(|normalized| (normalized, path)))
        .collect::<io::Result<Vec<_>>>()?;
    normalized_paths.sort_by(|left, right| left.0.cmp(&right.0));
    normalized_paths.dedup_by(|left, right| left.0 == right.0);
    Ok(normalized_paths.into_iter().map(|(_, path)| path).collect())
}

fn collect_rust_sources(
    workspace_root: &Path,
    directory: &Path,
    paths: &mut Vec<PathBuf>,
) -> io::Result<()> {
    if !directory.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let path = entry.path();
        if file_type.is_symlink() {
            continue;
        }
        let relative_path = normalized_relative_path(workspace_root, &path)?;
        if is_excluded(&relative_path) {
            continue;
        }
        if file_type.is_dir() {
            collect_rust_sources(workspace_root, &path, paths)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            paths.push(relative_path);
        }
    }
    Ok(())
}

fn normalized_relative_path(workspace_root: &Path, path: &Path) -> io::Result<PathBuf> {
    let relative = path.strip_prefix(workspace_root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("digest input is outside workspace root: {}", path.display()),
        )
    })?;
    if relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(relative.to_path_buf())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "digest input is not a normalized relative path: {}",
                relative.display()
            ),
        ))
    }
}

fn relative_path_to_slashes(path: &Path) -> io::Result<String> {
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str().map(str::to_owned).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "digest path is not valid UTF-8")
            }),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "digest path must be normalized and repo-relative",
            )),
        })
        .collect::<io::Result<Vec<_>>>()?;
    Ok(components.join("/"))
}

fn is_excluded(path: &Path) -> bool {
    path_is_prefixed_by_excluded_path(path) || file_name_is_excluded(path)
}

fn path_is_prefixed_by_excluded_path(path: &Path) -> bool {
    WORKSPACE_DIGEST_EXCLUDED_PATH_PREFIXES
        .iter()
        .any(|excluded_path| path.starts_with(excluded_path))
}

fn file_name_is_excluded(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| WORKSPACE_DIGEST_EXCLUDED_FILE_NAMES.contains(&name))
}

fn update_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn normalized_features<I, S>(features: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    features
        .into_iter()
        .map(|feature| feature.as_ref().to_owned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn validate_revision_override(revision: &str) -> Result<String, String> {
    (revision.len() == 40
        && revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()))
    .then(|| revision.to_ascii_lowercase())
    .ok_or_else(|| {
        format!(
            "FLAPJACK_BUILD_REVISION must be exactly 40 hexadecimal characters; received {revision:?}"
        )
    })
}

fn normalize_revision(revision: &str) -> Option<String> {
    let revision = revision.trim();
    (revision.len() == 40
        && revision
            .chars()
            .all(|character| character.is_ascii_hexdigit()))
    .then(|| revision.to_ascii_lowercase())
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_owned)
}

fn parse_dirty(value: Option<&str>) -> Option<bool> {
    match value {
        Some("true") => Some(true),
        Some("false") => Some(false),
        _ => None,
    }
}

fn enabled_core_features() -> Vec<&'static str> {
    let mut features = Vec::new();
    for (enabled, name) in [
        (cfg!(feature = "analytics"), "analytics"),
        (cfg!(feature = "axum-support"), "axum-support"),
        (cfg!(feature = "decompound"), "decompound"),
        (cfg!(feature = "default"), "default"),
        (cfg!(feature = "memory-stats"), "memory-stats"),
        (cfg!(feature = "openapi"), "openapi"),
        (cfg!(feature = "s3-snapshots"), "s3-snapshots"),
        (cfg!(feature = "vector-search"), "vector-search"),
        (cfg!(feature = "vector-search-local"), "vector-search-local"),
    ] {
        if enabled {
            features.push(name);
        }
    }
    features
}

#[cfg(test)]
mod tests {
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

        let value = serde_json::to_value(info).unwrap();
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

    #[test]
    fn vcs_invalidation_tracks_head_and_active_branch_ref() {
        let paths = vcs_invalidation_paths(|arguments| match arguments {
            ["rev-parse", "--git-path", "HEAD"] => Ok(".git/worktrees/stage/HEAD\n".to_owned()),
            ["symbolic-ref", "-q", "HEAD"] => Ok("refs/heads/main\n".to_owned()),
            ["rev-parse", "--git-path", "refs/heads/main"] => {
                Ok(".git/refs/heads/main\n".to_owned())
            }
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
}
