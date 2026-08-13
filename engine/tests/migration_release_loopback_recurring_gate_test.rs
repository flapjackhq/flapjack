// The workflow-parsing helpers live in ONE place, shared with union_recurring_gate_test.rs.
// Included by path rather than through `mod common;` because the `common` facade pulls in
// fixtures and HTTP helpers that a workflow-text assertion has no use for.
#[path = "common/workflow.rs"]
mod workflow;

use std::env;
use std::fs;
use std::path::PathBuf;
use workflow::{
    job_has_dispatch_only_condition, job_has_run_command, workflow_has_schedule_trigger,
    workflow_job,
};

const WORKFLOW_PATH_OVERRIDE_ENV: &str = "FLAPJACK_NIGHTLY_WORKFLOW_PATH_UNDER_TEST";
const MIGRATION_JOB: &str = "migration-import-contract";
const LOOPBACK_CONTRACT_COMMAND: &str = "bash engine/tests/migration_release_loopback_contract.sh";
const RELEASE_MEILISEARCH_TEST_COMMAND: &str = "cargo test --release -p flapjack-http --lib -- preview_loopback_constructor_is_reachable_in_release_with_explicit_opt_in";

#[test]
fn scheduled_migration_import_job_runs_release_loopback_gate() {
    let workflow = fs::read_to_string(workflow_path()).expect("nightly workflow must be readable");
    let job = workflow_job(&workflow, MIGRATION_JOB);

    assert!(
        workflow_has_schedule_trigger(&workflow),
        "nightly workflow must retain its top-level schedule trigger for {MIGRATION_JOB}"
    );
    assert!(
        job_has_run_command(job, RELEASE_MEILISEARCH_TEST_COMMAND),
        "{MIGRATION_JOB} must run the release-only Meilisearch loopback constructor proof"
    );
    assert!(
        job_has_run_command(job, LOOPBACK_CONTRACT_COMMAND),
        "{MIGRATION_JOB} must run the shipped-profile migration release loopback contract"
    );
    assert!(
        !job_has_dispatch_only_condition(job),
        "{MIGRATION_JOB} is scheduled and must not be restricted to workflow_dispatch"
    );
}

fn workflow_path() -> PathBuf {
    env::var_os(WORKFLOW_PATH_OVERRIDE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/nightly.yml")
        })
}

#[test]
fn schedule_trigger_detection_is_scoped_to_top_level_on_section() {
    assert!(workflow_has_schedule_trigger(
        "on:\n  schedule:\n    - cron: '0 2 * * *'\njobs:\n"
    ));
    assert!(!workflow_has_schedule_trigger(
        "on:\n  workflow_dispatch:\njobs:\n  schedule:\n    runs-on: ubuntu-latest\n"
    ));
}

#[test]
fn run_command_detection_rejects_commented_commands() {
    assert!(job_has_run_command(
        "  migration-import-contract:\n    steps:\n      - run: required command\n",
        "required command"
    ));
    assert!(!job_has_run_command(
        "  migration-import-contract:\n    steps:\n      # - run: required command\n",
        "required command"
    ));
}
