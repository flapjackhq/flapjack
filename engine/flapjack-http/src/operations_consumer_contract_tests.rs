use std::path::Path;

const OPERATIONS_CONSUMER_CONTRACT_PATH: &str = "../docs2/operations_consumer_contract.md";
const FEATURES_PATH: &str = "../docs2/FEATURES.md";

#[test]
fn operations_consumer_contract_names_runtime_and_screen_spec_owners() {
    let operations_consumer_contract = read_contract_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        OPERATIONS_CONSUMER_CONTRACT_PATH,
    );
    assert_contains_all(
        &operations_consumer_contract,
        &[
            "docs/screen_specs/system.md",
            "docs/screen_specs/cluster.md",
            "docs/screen_specs/snapshots.md",
            "engine/flapjack-http/src/handlers/health.rs::health",
            "engine/flapjack-http/src/handlers/internal.rs::replication_status",
            "engine/flapjack-http/src/handlers/internal.rs::cluster_status",
            "engine/flapjack-http/src/handlers/snapshot.rs::snapshot_capability",
            "engine/flapjack-http/src/openapi.rs",
            "engine/docs2/openapi.json",
        ],
    );
}

#[test]
fn operations_consumer_contract_publishes_r5_wire_contract() {
    let operations_consumer_contract = read_contract_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        OPERATIONS_CONSUMER_CONTRACT_PATH,
    );
    assert_contains_all(
        &operations_consumer_contract,
        &[
            "/health",
            "14 top-level keys",
            "5 seconds",
            "/internal/status",
            "10 seconds",
            "\"unknown\"",
            "/internal/cluster/status",
            "replication_enabled: false",
            "replication_enabled: true",
            "peers_total",
            "peers_healthy",
            "/internal/snapshots/capability",
            "not_configured",
            "configured_unverified",
        ],
    );
}

#[test]
fn operations_consumer_contract_captures_known_bounds() {
    let operations_consumer_contract = read_contract_fixture(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        OPERATIONS_CONSUMER_CONTRACT_PATH,
    );
    assert_contains_all(
        &operations_consumer_contract,
        &[
            "build_profile",
            "not a top-level `/health` field",
            "schemaVersion",
            "profile",
            "intentionally omits revision, dirty state, workspace digest, target triple, and feature list",
            "S3 config is present",
            "credentials, bucket existence, or reachability have not been checked",
            "`bucket` field is always present",
            "`null` when no bucket is configured",
            "`peers: []`",
            "request or parse failure",
            "consumer error state",
        ],
    );
}

#[test]
fn features_mentions_published_operations_contracts() {
    let features = read_contract_fixture(Path::new(env!("CARGO_MANIFEST_DIR")), FEATURES_PATH);
    assert_contains_all(
        &features,
        &[
            "Published operations APIs",
            "operations_consumer_contract.md",
            "configured_unverified",
            "engine/flapjack-http/src/handlers/experiments/mod.rs::list_experiments",
            "exact `indexName`",
            "`indexPrefix`",
        ],
    );
}

#[test]
fn synced_contract_fixture_reader_fails_when_artifact_is_absent() {
    let temp_dir = tempfile::tempdir().unwrap();
    let missing_file = "docs2/operations_consumer_contract.md";

    let panic = std::panic::catch_unwind(|| {
        read_contract_fixture(temp_dir.path(), missing_file);
    })
    .expect_err("missing public contract fixture must fail the guard");
    let message = panic_message(panic);

    assert!(
        message.contains(missing_file),
        "missing fixture failure should name the absent synced artifact, got {message}"
    );
}

fn read_contract_fixture(manifest_dir: &Path, relative_path: &str) -> String {
    let path = manifest_dir.join(relative_path);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("required synced contract fixture missing at {path:?}: {error}")
    })
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = panic.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = panic.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    "<non-string panic>".to_string()
}

fn assert_contains_all(document: &str, expected_fragments: &[&str]) {
    let normalized_document = normalize_whitespace(document);
    for fragment in expected_fragments {
        let normalized_fragment = normalize_whitespace(fragment);
        assert!(
            normalized_document.contains(&normalized_fragment),
            "document should contain `{fragment}`"
        );
    }
}

fn normalize_whitespace(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}
