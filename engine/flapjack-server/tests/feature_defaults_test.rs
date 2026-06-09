// Regression guard: `flapjack-server`'s default features must be empty so
// `cargo build -p flapjack-server` does not transitively pull
// `vector-search-local`'s heavy embedding chain. See
// docs/reference/research/may29_pm_1_ci_vector_search_split_stage1_decision.md.

use std::path::PathBuf;
use std::process::Command;

fn manifest_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}

fn cargo_metadata_json() -> serde_json::Value {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest_path())
        .output()
        .expect("failed to invoke cargo metadata");
    assert!(
        out.status.success(),
        "cargo metadata failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cargo metadata stdout was not valid JSON")
}

fn default_features_for(meta: &serde_json::Value, pkg_name: &str) -> Vec<String> {
    let packages = meta["packages"]
        .as_array()
        .expect("cargo metadata packages[] missing");
    let pkg = packages
        .iter()
        .find(|p| p["name"].as_str() == Some(pkg_name))
        .unwrap_or_else(|| panic!("package {pkg_name} not found in cargo metadata --no-deps"));
    let arr = pkg["features"]["default"]
        .as_array()
        .unwrap_or_else(|| panic!("package {pkg_name} has no features.default array"));
    arr.iter()
        .map(|v| {
            v.as_str()
                .expect("default feature entry must be a string")
                .to_string()
        })
        .collect()
}

#[test]
fn flapjack_server_default_features_omit_vector_search_local() {
    let meta = cargo_metadata_json();
    let defaults = default_features_for(&meta, "flapjack-server");
    assert!(
        !defaults.iter().any(|f| f == "vector-search-local"),
        "flapjack-server default features must not contain `vector-search-local`; got {defaults:?}"
    );
    let expected: Vec<String> = Vec::new();
    assert_eq!(
        defaults, expected,
        "flapjack-server default features must be exactly the empty set"
    );
}
