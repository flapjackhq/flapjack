use std::fs;
use std::path::PathBuf;

const NEXTTEST_CONFIG: &str = ".config/nextest.toml";
const DEFAULT_PROFILE: &str = "profile.default";
const LIVENESS_TEST_FILTER: &str =
    "test(single_worker_runtime_serves_count_during_injected_two_second_commit)";
const LIVENESS_THREADS_REQUIRED: &str = "\"num-test-threads\"";

#[test]
fn default_profile_reserves_capacity_for_the_liveness_test() {
    let config =
        fs::read_to_string(nextest_config_path()).expect("nextest config must be readable");
    let default_profile = table_body(&config, DEFAULT_PROFILE);
    assert!(
        table_value(&default_profile, "test-threads").is_none(),
        "the shared default profile must not reduce small-runner CI to one test thread"
    );

    let override_body =
        array_table_body(&config, "profile.default.overrides", LIVENESS_TEST_FILTER);
    let threads_required = table_value(&override_body, "threads-required")
        .expect("the liveness test must reserve scheduler capacity");

    assert_eq!(
        threads_required, LIVENESS_THREADS_REQUIRED,
        "the liveness specimen must reserve every configured nextest thread so unrelated test \
         processes cannot recreate the measured scheduled-deadline tail"
    );
}

#[test]
fn default_profile_comment_names_the_liveness_repair_contract() {
    let config =
        fs::read_to_string(nextest_config_path()).expect("nextest config must be readable");
    let default_profile = table_body(&config, DEFAULT_PROFILE);

    for required_fact in [
        "18-way",
        "scheduled_deadline_tail",
        "full scheduler reservation",
        "admission-width",
        "ci.yml:175,178",
        "host-portable",
        "num-test-threads",
    ] {
        assert!(
            default_profile.contains(required_fact),
            "[profile.default] comment must document `{required_fact}`"
        );
    }
}

fn nextest_config_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(NEXTTEST_CONFIG)
}

fn table_body(config: &str, table_name: &str) -> String {
    let mut in_table = false;
    let mut body = String::new();
    for line in config.lines() {
        if let Some(current_table) = table_header(line) {
            in_table = current_table == table_name;
            continue;
        }
        if in_table {
            body.push_str(line);
            body.push('\n');
        }
    }
    assert!(!body.is_empty(), "[{table_name}] must exist");
    body
}

fn array_table_body(config: &str, table_name: &str, required_filter: &str) -> String {
    config
        .split(&format!("[[{table_name}]]"))
        .skip(1)
        .map(|body| body.split("[[").next().unwrap_or(body))
        .find(|body| table_value(body, "filter") == Some(format!("'{required_filter}'")))
        .unwrap_or_else(|| panic!("[[{table_name}]] must target `{required_filter}`"))
        .to_string()
}

fn table_header(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || !trimmed.ends_with(']') || trimmed.starts_with("[[") {
        return None;
    }
    Some(trimmed.trim_start_matches('[').trim_end_matches(']'))
}

fn table_value(table: &str, key: &str) -> Option<String> {
    table.lines().find_map(|line| {
        let without_comment = line.split_once('#').map_or(line, |(value, _)| value);
        let (candidate_key, value) = without_comment.split_once('=')?;
        if candidate_key.trim() == key {
            Some(value.trim().to_string())
        } else {
            None
        }
    })
}
