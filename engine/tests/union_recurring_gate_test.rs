//! `.github/workflows/union.yml` must keep running the IN-PROCESS workspace union on a schedule.
//!
//! Why this gate exists, stated once so it is not re-derived:
//!
//! Every Linux Rust job in this repository runs `cargo nextest run` — `ci.yml:175,178,221,224` and
//! `nightly.yml`'s `Rust all tests`. nextest gives every test its OWN PROCESS. That is excellent for
//! speed (the full suite is green in ~16 minutes) and it is structurally blind to the entire bug
//! class `ROADMAP.md` rows `TEST-FLAKE-1`, `TEST-FLAKE-2` and `TEST-FLAKE-3` are about: one test's
//! mutation of process-global state leaking into another test's assertion. Process isolation means
//! there is no shared process to leak through. `ROADMAP.md` says this in as many words — Linux
//! nextest passing ~5,800 process-isolated tests does *not* refute the in-process union red.
//!
//! So the only run that can see those defects was, until now, a ~2.5-hour `cargo test --workspace`
//! on a single shared developer Mac. It has failed to complete twice for want of wall-clock
//! (`UNION_EXIT=124`), while every green nextest run looked like reassurance. This job moves the
//! measurement to a dedicated Linux runner where it is neither starved nor contended.
//!
//! WHY A SEPARATE WORKFLOW FILE RATHER THAN A JOB IN `nightly.yml`, which is the obvious place:
//! the union is EXPECTED RED right now — that is the whole reason those three rows are open. A red
//! job inside `nightly.yml` would flip that workflow's conclusion to `failure`, which would (a)
//! reset `NIGHT-1`, whose exit is two consecutive green SCHEDULED nightly runs, and (b) fail
//! `SYNC-1`'s gate clause `b` (`nightly_success`), which is the gate currently holding the prod
//! publish. Coupling a known-red diagnostic to the release gate would take the release hostage to
//! a flake investigation. Separate question, separate workflow, separate conclusion.
//!
//! This job is NOT `continue-on-error`. A gate that cannot fail is not a gate — that rule is why
//! three inert contracts have already been found in this repository — so the union's redness must
//! turn *its own* workflow red, just not anybody else's.

// Included by path rather than through `mod common;` on purpose: the `common` facade pulls in
// fixtures, HTTP helpers and their dependency tree, none of which a workflow-text assertion needs.
// `#[path]` gives one canonical owner for the parsing without the compile cost.
#[path = "common/workflow.rs"]
mod workflow;

use std::env;
use std::fs;
use std::path::PathBuf;
use workflow::{
    job_has_dispatch_only_condition, job_has_run_command, workflow_has_schedule_trigger,
    workflow_job,
};

/// Lets the mutation probe point the assertions at a rewritten copy instead of the real file.
/// Without this the only way to prove the gate can fail is to edit the workflow in place, which
/// is exactly the kind of "trust me, I checked" evidence this repository does not accept.
const WORKFLOW_PATH_OVERRIDE_ENV: &str = "FLAPJACK_UNION_WORKFLOW_PATH_UNDER_TEST";
const UNION_JOB: &str = "rust-in-process-union";
/// The literal command. `--workspace` so it is the whole union and not a crate subset;
/// `--no-fail-fast` so one early failure cannot hide the rest of the failure set, which is the
/// enumeration these rows need.
const UNION_COMMAND: &str = "cargo test --workspace --no-fail-fast";

#[test]
fn scheduled_union_job_runs_the_in_process_workspace_union() {
    let workflow = fs::read_to_string(workflow_path()).expect("union workflow must be readable");
    let job = workflow_job(&workflow, UNION_JOB);

    assert!(
        workflow_has_schedule_trigger(&workflow),
        "union workflow must retain its top-level schedule trigger; a union that only runs on \
         demand is the opt-in gate ROADMAP.md row MIG-22 was filed about"
    );
    assert!(
        job_has_run_command(job, UNION_COMMAND),
        "{UNION_JOB} must run `{UNION_COMMAND}` exactly. Narrowing it to a crate subset or \
         dropping --no-fail-fast changes what is being measured without changing the job name"
    );
    assert!(
        !job_has_dispatch_only_condition(job),
        "{UNION_JOB} is scheduled and must not be restricted to workflow_dispatch"
    );
}

/// The load-bearing negative assertion, and the reason this file is not just a copy of the
/// migration gate: swapping `cargo test` for `cargo nextest run` would leave a green job with the
/// same name that can no longer observe the defect class the job exists for. It would look like a
/// speed-up. Every other Rust job here already uses nextest, so this substitution is the single
/// most likely well-intentioned edit anyone will make to this file.
#[test]
fn union_job_does_not_use_process_isolated_nextest() {
    let workflow = fs::read_to_string(workflow_path()).expect("union workflow must be readable");
    let job = workflow_job(&workflow, UNION_JOB);

    // Comment lines are stripped before the check. The first version of this assertion tested
    // `job.contains("nextest")` over the raw text and went red on the workflow's own comment
    // explaining why nextest is banned — a test failing on prose rather than on behaviour, which
    // is the false-positive class this repository's testing rules forbid. Stripping comments keeps
    // the explanation (which is worth having exactly where the temptation lives) while still
    // catching every way the job could actually acquire nextest: a `run:` command, a `uses:`
    // install-action, or a `tool:` input.
    let executable_lines: String = job
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    assert!(
        !executable_lines.contains("nextest"),
        "{UNION_JOB} must not use nextest in any executable line — not a run: command, not a \
         uses: install-action, not a tool: input. nextest runs each test in its own process, \
         which cannot observe the shared-process-state leakage that TEST-FLAKE-1/-2/-3 are \
         about; the job would stay green precisely when it should not"
    );
}

/// A union that is allowed to fail silently is not a gate. Guards this against the tempting fix
/// someone will reach for the first time this job goes red and blocks nothing anyone wanted blocked.
#[test]
fn union_job_failure_is_not_suppressed() {
    let workflow = fs::read_to_string(workflow_path()).expect("union workflow must be readable");
    let job = workflow_job(&workflow, UNION_JOB);

    assert!(
        !job.contains("continue-on-error: true"),
        "{UNION_JOB} must not set continue-on-error: true. The union is expected red today; the \
         correct response is to fix TEST-FLAKE-1/-2/-3, not to make the gate incapable of failing. \
         If it must not block a release, that is why it lives in its own workflow file"
    );
}

fn workflow_path() -> PathBuf {
    env::var_os(WORKFLOW_PATH_OVERRIDE_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.github/workflows/union.yml")
        })
}
