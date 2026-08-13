//! Canonical parser for GitHub Actions workflow files, shared by every recurring-gate test.
//!
//! Why this exists as one owner rather than per-test helpers: this repository keeps failing the
//! same way — a contract lands, nothing invokes it, and the "gate" is green because it never runs
//! (`ROADMAP.md` rows `MIG-22`, `REL-12`, and the third inert contract recorded on `CI-STAGING-1`).
//! The fix each time is a test asserting the workflow actually invokes the contract. The first such
//! test (`migration_release_loopback_recurring_gate_test.rs`) grew private helpers; the second
//! would have copied them. Two copies of "how do we read a workflow job" is exactly the duplicate-
//! owner defect `ROADMAP.md` row `DOC-SSOT-1` is the specimen for, so the helpers live here.
//!
//! These functions are deliberately line-oriented rather than YAML-deserialising. A real YAML
//! parser would accept semantically-equivalent reformattings that a human reviewer would not
//! notice, and the thing being defended against is an edit that *looks* wired. Matching the
//! literal `run:` command is the assertion with the fewest ways to pass while being wrong.

use regex::Regex;

/// True when the workflow declares a top-level `schedule:` trigger.
///
/// Scoped to the `on:` block on purpose: a job *named* `schedule` must not satisfy this, or a
/// dispatch-only workflow could pass by coincidence of naming.
pub fn workflow_has_schedule_trigger(workflow: &str) -> bool {
    workflow
        .lines()
        .skip_while(|line| *line != "on:")
        .skip(1)
        .take_while(|line| {
            line.is_empty() || line.starts_with(char::is_whitespace) || line.starts_with('#')
        })
        .any(|line| line == "  schedule:")
}

/// The text of one job block, from its `  <name>:` header to the next job header or EOF.
///
/// Panics when the job is absent. That is intentional: every caller is asserting the job exists,
/// and a `None` that silently short-circuits the remaining assertions would turn a missing job
/// into a passing test — the zero-denominator pass this repository's testing rules forbid.
pub fn workflow_job<'a>(workflow: &'a str, job_name: &str) -> &'a str {
    let job_header =
        Regex::new(&format!(r"^  {}:$", regex::escape(job_name))).expect("job regex must compile");
    let next_job_header =
        Regex::new(r"^  [A-Za-z0-9_-]+:$").expect("job header regex must compile");
    let start = workflow
        .lines()
        .scan(0, |offset, line| {
            let line_start = *offset;
            *offset += line.len() + 1;
            Some((line_start, line))
        })
        .find_map(|(offset, line)| job_header.is_match(line).then_some(offset))
        .unwrap_or_else(|| panic!("workflow must contain the {job_name} job"));
    let remainder = &workflow[start..];
    let end = remainder
        .lines()
        .scan(0, |offset, line| {
            let line_start = *offset;
            *offset += line.len() + 1;
            Some((line_start, line))
        })
        .skip(1)
        .find_map(|(offset, line)| next_job_header.is_match(line).then_some(offset))
        .unwrap_or(remainder.len());

    &remainder[..end]
}

/// True when the job is gated to `workflow_dispatch`, i.e. it will not run on the schedule.
///
/// A recurring gate restricted to manual dispatch is the failure mode these tests exist to catch:
/// it is wired, it is green, and it never fires unless a human asks.
pub fn job_has_dispatch_only_condition(job: &str) -> bool {
    job.lines()
        .filter_map(|line| line.trim().strip_prefix("if:"))
        .any(|condition| condition.contains("workflow_dispatch"))
}

/// True when the job runs exactly `command` as a single-line `run:` step.
///
/// Trimming the `- ` list marker before the `run:` prefix is what makes a **commented-out** step
/// fail to match: `# - run: x` trims to `# - run: x`, whose `- ` strip leaves `# - run: x` with no
/// `run:` prefix at position zero. A `contains()` check would happily match the comment.
pub fn job_has_run_command(job: &str, command: &str) -> bool {
    job.lines()
        .map(str::trim)
        .map(|line| line.strip_prefix("- ").unwrap_or(line))
        .filter_map(|line| line.strip_prefix("run:"))
        .any(|configured_command| configured_command.trim() == command)
}
