#!/usr/bin/env python3
"""Fail-closed ownership contract for Flapjack test execution tiers."""

import argparse
import json
import os
import re
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
MANIFEST_PATH = ROOT / "engine/tests/ci_test_tiers.json"
WORKFLOWS = (
    ".github/workflows/ci.yml",
    ".github/workflows/docker.yml",
    ".github/workflows/nightly.yml",
    ".github/workflows/union.yml",
    ".github/workflows/test-installer.yml",
    ".github/workflows/release.yml",
)
TOPOLOGY_AUTHORITIES = (
    ".github/workflows/ci.yml",
    ".github/workflows/nightly.yml",
    ".github/workflows/union.yml",
    ".github/workflows/release.yml",
    ".github/workflows/README.md",
    "engine/docs2/3_IMPLEMENTATION/DEPLOYMENT.md",
)
REQUIRED_RISKS = {
    "core_search_index",
    "durability",
    "auth_tenant",
    "api_compat",
    "startup_wiring",
    "test_harness_integrity",
    "vector_isolation",
    "process_global_isolation",
    "dashboard",
    "console",
    "sdks",
    "installer",
    "migration",
    "union",
    "release",
}
JOB_RE = re.compile(r"^  ([A-Za-z0-9_-]+):\s*$", re.MULTILINE)
IGNORE_FN_RE = re.compile(
    r'#\[ignore(?:\s*=\s*"[^"]*")?\]\s*'
    r'(?:#\[[^\]]+\]\s*)*'
    r'(?:pub(?:\([^)]*\))?\s+)?(?:async\s+)?fn\s+([A-Za-z0-9_]+)',
    re.MULTILINE,
)


class ContractError(AssertionError):
    """Raised when the tier manifest and executable owners diverge."""


def load_manifest(path=MANIFEST_PATH):
    with Path(path).open(encoding="utf-8") as handle:
        return json.load(handle)


def workflow_job_blocks(root=ROOT):
    blocks = {}
    for relative in WORKFLOWS:
        text = (Path(root) / relative).read_text(encoding="utf-8")
        for job_name, block in _jobs_from_workflow(text).items():
            blocks[f"{relative}#{job_name}"] = block
    return blocks


def _top_level_block(text, key):
    """Return one top-level YAML mapping without accepting same-named job content."""
    match = re.search(rf"^{re.escape(key)}:\s*$", text, re.MULTILINE)
    if match is None:
        raise ContractError(f"workflow has no top-level {key!r} mapping")
    remainder = text[match.end():]
    next_mapping = re.search(r"^[A-Za-z0-9_-]+:\s*$", remainder, re.MULTILINE)
    return remainder[: next_mapping.start() if next_mapping else len(remainder)]


def _event_block(on_block, event):
    match = re.search(rf"^  {re.escape(event)}:(?:\s*#.*)?\s*$", on_block, re.MULTILINE)
    if match is None:
        return None
    remainder = on_block[match.end():]
    next_event = re.search(r"^  [A-Za-z0-9_-]+:", remainder, re.MULTILINE)
    return remainder[: next_event.start() if next_event else len(remainder)]


def _workflow_events(text):
    on_block = _top_level_block(text, "on")
    return set(re.findall(r"^  ([A-Za-z0-9_-]+):", on_block, re.MULTILINE))


def _push_branches(text):
    push_block = _event_block(_top_level_block(text, "on"), "push")
    if push_block is None:
        return []
    inline = re.search(r"^    branches:\s*\[([^]]*)\]\s*$", push_block, re.MULTILINE)
    if inline:
        return [item.strip().strip("'\"") for item in inline.group(1).split(",")]
    branches = re.search(r"^    branches:\s*$", push_block, re.MULTILINE)
    if branches is None:
        return []
    remainder = push_block[branches.end():]
    lines = []
    for line in remainder.splitlines():
        if re.match(r"^    [A-Za-z0-9_-]+:", line):
            break
        item = re.match(r"^      -\s+(.+?)\s*$", line)
        if item:
            lines.append(item.group(1).strip("'\""))
    return lines


def _jobs_from_workflow(text):
    jobs_marker = re.search(r"^jobs:\s*$", text, re.MULTILINE)
    if jobs_marker is None:
        raise ContractError("workflow has no jobs mapping")
    jobs_text = text[jobs_marker.end():]
    # The aggregate denominator must not silently shrink when a valid YAML job key
    # uses syntax outside our deliberately small parser (for example, a quoted key).
    headers = [
        line
        for line in jobs_text.splitlines()
        if line.startswith("  ")
        and not line.startswith("   ")
        and line[2:].strip()
        and not line[2:].startswith("#")
    ]
    unsupported = [
        header
        for header in headers
        if not re.fullmatch(r"  [A-Za-z0-9_-]+:\s*", header)
    ]
    if unsupported:
        raise ContractError(f"workflow has unsupported job headers: {unsupported}")
    matches = list(JOB_RE.finditer(jobs_text))
    return {
        match.group(1): jobs_text[
            match.start(): matches[index + 1].start()
            if index + 1 < len(matches)
            else len(jobs_text)
        ]
        for index, match in enumerate(matches)
    }


def _job_needs(job):
    inline = re.search(r"^    needs:\s*\[([^]]*)\]\s*$", job, re.MULTILINE)
    if inline:
        return {item.strip() for item in inline.group(1).split(",") if item.strip()}
    needs = re.search(r"^    needs:\s*$", job, re.MULTILINE)
    if needs is None:
        return set()
    dependencies = set()
    for line in job[needs.end():].splitlines():
        if re.match(r"^    [A-Za-z0-9_-]+:", line):
            break
        item = re.match(r"^      -\s+([A-Za-z0-9_-]+)\s*$", line)
        if item:
            dependencies.add(item.group(1))
    return dependencies


def topology_texts(root=ROOT):
    return {
        relative: (Path(root) / relative).read_text(encoding="utf-8")
        for relative in TOPOLOGY_AUTHORITIES
    }


def verify_public_candidate_topology(root=ROOT, texts=None):
    """Keep candidate validation complete without broadening release/recurring triggers."""
    texts = topology_texts(root) if texts is None else texts
    ci = texts[".github/workflows/ci.yml"]
    ci_events = _workflow_events(ci)
    if ci_events != {"push"}:
        raise ContractError(f"public CI events must be push-only, found {sorted(ci_events)}")
    branches = _push_branches(ci)
    if branches != ["main", "public-candidate/**"]:
        raise ContractError(
            "public CI push branches must be exactly main and public-candidate/**, "
            f"found {branches}"
        )
    if not re.search(r"^permissions:\s*\n  contents:\s*read\s*$", ci, re.MULTILINE):
        raise ContractError("public CI must default to contents: read")

    jobs = _jobs_from_workflow(ci)
    ci_check = jobs.get("check-repo", "")
    if (
        "ACTUAL_REPOSITORY: ${{ github.repository }}" not in ci_check
        or '[ "$ACTUAL_REPOSITORY" = "flapjackhq/flapjack" ]' not in ci_check
    ):
        raise ContractError("public CI repository identity must be env-bound before shell use")
    gate = jobs.get("public-candidate-gate")
    if gate is None:
        raise ContractError("public CI is missing the public-candidate-gate job")
    expected_dependencies = set(jobs) - {"public-candidate-gate"}
    actual_dependencies = _job_needs(gate)
    if actual_dependencies != expected_dependencies:
        raise ContractError(
            "Public candidate gate dependencies drift: "
            f"missing={sorted(expected_dependencies - actual_dependencies)} "
            f"unexpected={sorted(actual_dependencies - expected_dependencies)}"
        )
    if not re.search(r"^    if:\s*always\(\)\s*$", gate, re.MULTILINE):
        raise ContractError("Public candidate gate must have a job-level if: always()")
    required_gate_fragments = (
        "name: Public candidate gate",
        "NEEDS_JSON: ${{ toJSON(needs) }}",
        'result.get("result") != "success"',
        "flapjackhq/flapjack",
        "refs/heads/public-candidate/",
    )
    for fragment in required_gate_fragments:
        if fragment not in gate:
            raise ContractError(f"Public candidate gate is missing fail-closed fragment: {fragment}")
    for relative in (".github/workflows/nightly.yml", ".github/workflows/union.yml"):
        events = _workflow_events(texts[relative])
        if events != {"schedule", "workflow_dispatch"}:
            raise ContractError(
                f"{relative} must remain scheduled/manual public-main validation, found {sorted(events)}"
            )
        recurring_check = _jobs_from_workflow(texts[relative]).get("check-repo", "")
        safe_fragments = (
            "ACTUAL_REPOSITORY: ${{ github.repository }}",
            "ACTUAL_REF: ${{ github.ref }}",
            '[ "$ACTUAL_REPOSITORY" = "flapjackhq/flapjack" ]',
            '[ "$ACTUAL_REF" = "refs/heads/main" ]',
        )
        if any(fragment not in recurring_check for fragment in safe_fragments):
            raise ContractError(f"{relative} must refuse non-main manual dispatches")

    release = texts[".github/workflows/release.yml"]
    if _workflow_events(release) != {"workflow_dispatch"}:
        raise ContractError("release.yml must remain workflow_dispatch-only")
    release_validation = _jobs_from_workflow(release).get("validate_release_version", "")
    ref_binding = "ACTUAL_REF: ${{ github.ref }}"
    main_ref_guard = 'if [[ "$ACTUAL_REF" != "refs/heads/main" ]]; then'
    if ref_binding not in release_validation or main_ref_guard not in release_validation:
        raise ContractError("release validation must refuse dispatch from a non-main ref")

    active_paths = (
        ".github/workflows/ci.yml",
        ".github/workflows/nightly.yml",
        ".github/workflows/union.yml",
        ".github/workflows/README.md",
        "engine/docs2/3_IMPLEMENTATION/DEPLOYMENT.md",
    )
    stale = [path for path in active_paths if "gridl-staging/flapjack" in texts[path]]
    if stale:
        raise ContractError(f"active workflow/topology authorities still name staging: {stale}")

    workflow_readme = texts[".github/workflows/README.md"]
    for fragment in ("public-candidate/**", "Public candidate gate", "publish_public_candidate.sh"):
        if fragment not in workflow_readme:
            raise ContractError(f"workflow README is missing candidate publication guidance: {fragment}")
    deployment = texts["engine/docs2/3_IMPLEMENTATION/DEPLOYMENT.md"]
    for fragment in ("gridl-dev/flapjack_dev", "flapjackhq/flapjack", "public-candidate/"):
        if fragment not in deployment:
            raise ContractError(f"deployment topology is missing: {fragment}")


def ignored_tests(root=ROOT):
    discovered = set()
    engine_root = Path(root) / "engine"
    for directory, child_dirs, filenames in os.walk(engine_root):
        # Build output can contain copied source and is neither a test owner nor
        # stable input. Pruning here keeps the contract fast on warm worktrees.
        child_dirs[:] = [
            name for name in child_dirs if name not in {".git", "node_modules", "target"}
        ]
        for filename in filenames:
            if not filename.endswith(".rs"):
                continue
            path = Path(directory) / filename
            text = path.read_text(encoding="utf-8")
            relative = path.relative_to(root).as_posix()
            for name in IGNORE_FN_RE.findall(text):
                discovered.add((relative, name))
    return discovered


def local_runner_path(root=ROOT):
    """Resolve the owned source runner or Debbie's public remap."""
    root = Path(root)
    source_runner = root / "engine/_dev/s/test"
    public_runner = root / "engine/s/test"
    if source_runner.exists():
        if not source_runner.is_file():
            raise ContractError(
                "local runner layout is unsupported: engine/_dev/s/test is not a file"
            )
        return source_runner
    if public_runner.exists():
        if not public_runner.is_file():
            raise ContractError(
                "local runner layout is unsupported: engine/s/test is not a file"
            )
        return public_runner
    raise ContractError(
        "local runner is missing from both engine/_dev/s/test and engine/s/test"
    )


def verify_local_runner(root=ROOT, runner_text=None, named_source_text=None):
    """Keep the documented default gate out of unsafe in-process HTTP unions."""
    runner_path = local_runner_path(root)
    runner_text = (
        runner_path.read_text(encoding="utf-8")
        if runner_text is None
        else runner_text
    )
    unsafe_http_lines = [
        line.strip()
        for line in runner_text.splitlines()
        if "cargo test" in line and "--lib" in line and "-p flapjack-http" in line
    ]
    if unsafe_http_lines:
        raise ContractError(
            "local runner contains unsafe in-process flapjack-http lib ownership: "
            f"{unsafe_http_lines}"
        )

    core_owner = "cargo test --lib -p flapjack -p flapjack-replication"
    if runner_text.count(core_owner) != 1:
        raise ContractError(
            "local runner must own the flapjack core and replication libs exactly once"
        )
    http_owner = "cargo nextest run -P ci -p flapjack-http --lib"
    if runner_text.count(http_owner) != 1:
        raise ContractError(
            "local runner must own the complete process-isolated flapjack-http lib surface "
            "exactly once"
        )
    integration_owner = "cargo nextest run --no-fail-fast"
    runner_commands = [line.strip() for line in runner_text.splitlines()]
    if runner_commands.count(integration_owner) != 1:
        raise ContractError(
            "local runner must run its integration surface exactly once without fail-fast"
        )

    console_commands = (
        'npm --prefix "$ENGINE_DIR/console" run test:unit:run',
        'npm --prefix "$ENGINE_DIR/console" run check',
        'npm --prefix "$ENGINE_DIR/console" run build',
        'npm --prefix "$ENGINE_DIR/console" run lint:browser-tests:unmocked',
        'npm --prefix "$ENGINE_DIR/console" run test:browser:unmocked',
    )
    console_start = "# -- Console checks --"
    console_end = "# -- End console checks --"
    if runner_text.count(console_start) != 1 or runner_text.count(console_end) != 1:
        raise ContractError("local runner must contain one bounded Console checks section")
    console_text = runner_text.split(console_start, 1)[1].split(console_end, 1)[0]
    runner_lines = [line.strip() for line in runner_text.splitlines()]
    for command in console_commands:
        count = runner_lines.count(command)
        if count != 1:
            raise ContractError(
                f"local runner Console checks must execute {command!r} exactly once "
                f"(found {count})"
            )

    named_source_path = (
        Path(root)
        / "engine/flapjack-http/src/handlers/migration/async_status_tests.rs"
    )
    named_source_text = (
        named_source_path.read_text(encoding="utf-8")
        if named_source_text is None
        else named_source_text
    )
    specimen = "stale_generation_cannot_mutate_terminal_or_ack_state_for_any_provider"
    if named_source_text.count(f"fn {specimen}") != 1:
        raise ContractError(
            "named interference specimen is missing or ambiguous; reconcile its "
            "process-isolated flapjack-http owner"
        )


def verify(root=ROOT, manifest_path=MANIFEST_PATH, jobs=None, actual_ignored=None):
    verify_local_runner(root)
    verify_public_candidate_topology(root)
    manifest = load_manifest(manifest_path)
    if manifest.get("schema_version") != 1:
        raise ContractError("test-tier manifest schema_version must be 1")

    tiers = manifest.get("tier_order", [])
    if len(tiers) != len(set(tiers)) or not tiers:
        raise ContractError("tier_order must be a non-empty unique closed set")

    classes = manifest.get("classes", [])
    class_ids = [entry.get("id") for entry in classes]
    if len(class_ids) != len(set(class_ids)) or None in class_ids:
        raise ContractError("every test class must have one unique id")

    jobs = workflow_job_blocks(root) if jobs is None else jobs
    owned_jobs = set()
    risks = set()
    for entry in classes:
        tier = entry.get("minimum_tier")
        if tier not in tiers:
            raise ContractError(f"{entry['id']} has unknown minimum tier {tier!r}")
        owner_jobs = entry.get("owner_jobs", [])
        if not owner_jobs:
            raise ContractError(f"{entry['id']} has no executable owner job")
        for owner in owner_jobs:
            if owner in owned_jobs:
                raise ContractError(f"workflow job has more than one class owner: {owner}")
            if owner not in jobs:
                raise ContractError(f"manifest owner job does not exist: {owner}")
            owned_jobs.add(owner)
        combined_owner_text = "\n".join(jobs[owner] for owner in owner_jobs)
        for fragment in entry.get("required_fragments", []):
            if fragment not in combined_owner_text:
                raise ContractError(
                    f"{entry['id']} owner jobs are missing required fragment: {fragment}"
                )
        owner_lines = [line.strip() for line in combined_owner_text.splitlines()]
        for command in entry.get("required_exact_commands", []):
            count = owner_lines.count(command)
            if count != 1:
                raise ContractError(
                    f"{entry['id']} owner must execute {command!r} exactly once "
                    f"(found {count})"
                )
        for source_contract in entry.get("source_contracts", []):
            source_path = Path(root) / source_contract["path"]
            if not source_path.is_file():
                raise ContractError(
                    f"{entry['id']} source contract does not exist: "
                    f"{source_contract['path']}"
                )
            source_text = source_path.read_text(encoding="utf-8")
            for fragment in source_contract.get("required_fragments", []):
                if fragment not in source_text:
                    raise ContractError(
                        f"{entry['id']} source contract is missing required fragment: "
                        f"{fragment}"
                    )
        risks.update(entry.get("risks", []))

    infrastructure = set(manifest.get("infrastructure_jobs", []))
    overlap = owned_jobs & infrastructure
    if overlap:
        raise ContractError(f"jobs cannot be both test owners and infrastructure: {sorted(overlap)}")
    unclassified = set(jobs) - owned_jobs - infrastructure
    stale = (owned_jobs | infrastructure) - set(jobs)
    if unclassified or stale:
        raise ContractError(
            f"workflow job classification drift: unclassified={sorted(unclassified)} stale={sorted(stale)}"
        )

    missing_risks = REQUIRED_RISKS - risks
    if missing_risks:
        raise ContractError(f"required candidate/complete risks are unowned: {sorted(missing_risks)}")

    ignored_entries = manifest.get("ignored_tests", [])
    ignored_ids = [(entry["source"], entry["name"]) for entry in ignored_entries]
    if len(ignored_ids) != len(set(ignored_ids)):
        raise ContractError("ignored-test ownership entries must be unique")
    for entry in ignored_entries:
        if entry.get("minimum_tier") not in tiers:
            raise ContractError(
                f"ignored test {entry['name']} has unknown minimum tier "
                f"{entry.get('minimum_tier')!r}"
            )
    expected_ignored = set(ignored_ids)
    actual_ignored = ignored_tests(root) if actual_ignored is None else actual_ignored
    if actual_ignored != expected_ignored:
        raise ContractError(
            "ignored-test ownership drift: "
            f"unclassified={sorted(actual_ignored - expected_ignored)} "
            f"stale={sorted(expected_ignored - actual_ignored)}"
        )

    all_job = jobs[".github/workflows/ci.yml#rust-tests-all"]
    if "RUSTFLAGS: -C debuginfo=0" not in all_job:
        raise ContractError("rust-tests-all must own one canonical job-level RUSTFLAGS profile")
    prebuild = (
        "cargo nextest run -p flapjack -p flapjack-http "
        "--features vector-search -P ci --no-run"
    )
    if prebuild not in all_job or "RUSTFLAGS='" + "-C debuginfo=0 -C strip=debuginfo' " + prebuild in all_job:
        raise ContractError("vector prebuild and nextest must share the job-level compilation identity")
    remaining_prebuild = (
        "cargo nextest run -p flapjack-server -p flapjack-ssl "
        "-p flapjack-replication -P ci --no-run"
    )
    if remaining_prebuild not in all_job:
        raise ContractError("remaining-crates prebuild must share the capped nextest compilation identity")


class TestTierContract(unittest.TestCase):
    def topology_mutation(self, relative, old, new):
        texts = topology_texts()
        mutated = texts[relative].replace(old, new, 1)
        self.assertNotEqual(texts[relative], mutated, f"mutation must change {relative}")
        texts[relative] = mutated
        return texts

    def test_live_manifest_and_workflows_converge(self):
        verify()

    def test_candidate_branch_trigger_cannot_be_removed(self):
        texts = self.topology_mutation(
            ".github/workflows/ci.yml", "      - public-candidate/**\n", ""
        )
        with self.assertRaisesRegex(ContractError, "push branches must be exactly"):
            verify_public_candidate_topology(texts=texts)

    def test_untrusted_pull_request_trigger_is_rejected(self):
        texts = self.topology_mutation(
            ".github/workflows/ci.yml", "on:\n", "on:\n  pull_request:\n",
        )
        with self.assertRaisesRegex(ContractError, "push-only"):
            verify_public_candidate_topology(texts=texts)

    def test_aggregate_gate_cannot_drop_a_required_job(self):
        texts = self.topology_mutation(
            ".github/workflows/ci.yml", "      - rust-tests-all\n", ""
        )
        with self.assertRaisesRegex(ContractError, "gate dependencies drift"):
            verify_public_candidate_topology(texts=texts)

    def test_aggregate_denominator_rejects_unsupported_job_header(self):
        texts = self.topology_mutation(
            ".github/workflows/ci.yml",
            "jobs:\n",
            "jobs:\n  hidden-job: # valid YAML, unsupported parser shape\n"
            "    runs-on: ubuntu-latest\n",
        )
        with self.assertRaisesRegex(ContractError, "unsupported job headers"):
            verify_public_candidate_topology(texts=texts)

    def test_aggregate_gate_always_condition_must_remain_job_level(self):
        texts = self.topology_mutation(
            ".github/workflows/ci.yml",
            "    name: Public candidate gate\n    if: always()\n",
            "    name: Public candidate gate\n",
        )
        with self.assertRaisesRegex(ContractError, "job-level if: always"):
            verify_public_candidate_topology(texts=texts)

    def test_staging_identity_cannot_return_to_active_topology(self):
        texts = self.topology_mutation(
            "engine/docs2/3_IMPLEMENTATION/DEPLOYMENT.md",
            "flapjackhq/flapjack",
            "gridl-staging/flapjack",
        )
        with self.assertRaisesRegex(ContractError, "still name staging"):
            verify_public_candidate_topology(texts=texts)

    def test_recurring_workflow_rejects_any_extra_automatic_trigger(self):
        texts = self.topology_mutation(
            ".github/workflows/nightly.yml", "on:\n", "on:\n  pull_request:\n"
        )
        with self.assertRaisesRegex(ContractError, "scheduled/manual public-main"):
            verify_public_candidate_topology(texts=texts)

    def test_recurring_workflow_cannot_accept_non_main_dispatch(self):
        texts = self.topology_mutation(
            ".github/workflows/union.yml",
            ' && [ "$ACTUAL_REF" = "refs/heads/main" ]',
            "",
        )
        with self.assertRaisesRegex(ContractError, "refuse non-main manual dispatches"):
            verify_public_candidate_topology(texts=texts)

    def test_recurring_ref_expression_must_be_env_bound(self):
        texts = self.topology_mutation(
            ".github/workflows/nightly.yml",
            "          ACTUAL_REF: ${{ github.ref }}\n",
            "",
        )
        with self.assertRaisesRegex(ContractError, "refuse non-main manual dispatches"):
            verify_public_candidate_topology(texts=texts)

    def test_release_cannot_gain_an_automatic_trigger(self):
        texts = self.topology_mutation(
            ".github/workflows/release.yml", "on:\n", "on:\n  push:\n"
        )
        with self.assertRaisesRegex(ContractError, "workflow_dispatch-only"):
            verify_public_candidate_topology(texts=texts)

    def test_release_main_ref_guard_cannot_be_removed(self):
        texts = self.topology_mutation(
            ".github/workflows/release.yml",
            '          if [[ "$ACTUAL_REF" != "refs/heads/main" ]]; then\n',
            "",
        )
        with self.assertRaisesRegex(ContractError, "refuse dispatch from a non-main ref"):
            verify_public_candidate_topology(texts=texts)

    def test_release_ref_expression_must_be_env_bound(self):
        texts = self.topology_mutation(
            ".github/workflows/release.yml",
            "          ACTUAL_REF: ${{ github.ref }}\n",
            "",
        )
        with self.assertRaisesRegex(ContractError, "refuse dispatch from a non-main ref"):
            verify_public_candidate_topology(texts=texts)

    def test_unknown_workflow_job_is_rejected(self):
        jobs = workflow_job_blocks()
        jobs[".github/workflows/ci.yml#new_unowned_test"] = "  new_unowned_test:\n"
        with self.assertRaisesRegex(ContractError, "new_unowned_test"):
            verify(jobs=jobs)

    def test_unknown_ignored_test_is_rejected(self):
        expected = {
            (entry["source"], entry["name"])
            for entry in load_manifest()["ignored_tests"]
        }
        mutated = set(expected)
        mutated.add(("engine/src/new_tests.rs", "silently_skipped"))
        with self.assertRaisesRegex(ContractError, "silently_skipped"):
            verify(actual_ignored=mutated)

    def test_public_complete_owners_cannot_regress_to_fail_fast(self):
        owner = ".github/workflows/ci.yml#rust-tests-all"
        commands = (
            "cargo nextest run -p flapjack -p flapjack-http --features vector-search "
            "-P ci --no-fail-fast",
            "cargo nextest run -p flapjack-server -p flapjack-ssl "
            "-p flapjack-replication -P ci --no-fail-fast",
        )
        for command in commands:
            with self.subTest(command=command):
                jobs = workflow_job_blocks()
                mutated = jobs[owner].replace(
                    command,
                    command.removesuffix(" --no-fail-fast"),
                    1,
                )
                self.assertNotEqual(
                    jobs[owner], mutated, "mutation must restore fail-fast"
                )
                jobs[owner] = mutated
                with self.assertRaisesRegex(ContractError, "missing required fragment"):
                    verify(jobs=jobs)

    def test_console_ci_owner_rejects_omitted_and_duplicated_commands(self):
        console_class = next(
            entry for entry in load_manifest()["classes"] if entry["id"] == "console"
        )
        for command in console_class["required_exact_commands"]:
            with self.subTest(command=command, mutation="omitted"):
                jobs = workflow_job_blocks()
                owner = ".github/workflows/ci.yml#console"
                mutated = jobs[owner].replace(f"          {command}\n", "", 1)
                self.assertNotEqual(jobs[owner], mutated)
                jobs[owner] = mutated
                with self.assertRaisesRegex(ContractError, "exactly once .*found 0"):
                    verify(jobs=jobs)

            with self.subTest(command=command, mutation="duplicated"):
                jobs = workflow_job_blocks()
                owner = ".github/workflows/ci.yml#console"
                mutated = jobs[owner].replace(
                    f"          {command}\n",
                    f"          {command}\n          {command}\n",
                    1,
                )
                self.assertNotEqual(jobs[owner], mutated)
                jobs[owner] = mutated
                with self.assertRaisesRegex(ContractError, "exactly once .*found 2"):
                    verify(jobs=jobs)

    def test_console_local_runner_rejects_omitted_and_duplicated_commands(self):
        commands = (
            'npm --prefix "$ENGINE_DIR/console" run test:unit:run',
            'npm --prefix "$ENGINE_DIR/console" run check',
            'npm --prefix "$ENGINE_DIR/console" run build',
            'npm --prefix "$ENGINE_DIR/console" run lint:browser-tests:unmocked',
            'npm --prefix "$ENGINE_DIR/console" run test:browser:unmocked',
        )
        runner = local_runner_path().read_text(encoding="utf-8")
        prefix, console_and_suffix = runner.split("# -- Console checks --", 1)
        console, suffix = console_and_suffix.split("# -- End console checks --", 1)
        for command in commands:
            with self.subTest(command=command, mutation="omitted"):
                mutated_console = console.replace(f"  {command}\n", "", 1)
                mutated = (
                    prefix
                    + "# -- Console checks --"
                    + mutated_console
                    + "# -- End console checks --"
                    + suffix
                )
                self.assertNotEqual(runner, mutated)
                with self.assertRaisesRegex(ContractError, "exactly once .*found 0"):
                    verify_local_runner(runner_text=mutated)

            with self.subTest(command=command, mutation="duplicated"):
                mutated_console = console.replace(
                    f"  {command}\n", f"  {command}\n  {command}\n", 1
                )
                mutated = (
                    prefix
                    + "# -- Console checks --"
                    + mutated_console
                    + "# -- End console checks --"
                    + suffix
                )
                self.assertNotEqual(runner, mutated)
                with self.assertRaisesRegex(ContractError, "exactly once .*found 2"):
                    verify_local_runner(runner_text=mutated)

            with self.subTest(command=command, mutation="duplicated_outside_owner"):
                mutated = f"{command}\n{runner}"
                with self.assertRaisesRegex(ContractError, "exactly once .*found 2"):
                    verify_local_runner(runner_text=mutated)

    def test_local_runner_rejects_flapjack_http_in_the_in_process_lib_union(self):
        runner = local_runner_path().read_text(encoding="utf-8")
        mutated = runner.replace(
            "cargo test --lib -p flapjack -p flapjack-replication",
            "cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication",
            1,
        )
        self.assertNotEqual(runner, mutated, "runner mutation must change the live command")
        with self.assertRaisesRegex(ContractError, "unsafe in-process flapjack-http"):
            verify_local_runner(runner_text=mutated)

    def test_local_runner_requires_complete_isolated_flapjack_http_lib_ownership(self):
        runner = local_runner_path().read_text(encoding="utf-8")
        mutated = runner.replace(
            "cargo nextest run -P ci -p flapjack-http --lib",
            "true # removed flapjack-http lib owner",
            1,
        )
        self.assertNotEqual(runner, mutated, "runner mutation must remove the live owner")
        with self.assertRaisesRegex(ContractError, "process-isolated flapjack-http"):
            verify_local_runner(runner_text=mutated)

    def test_local_runner_named_interference_specimen_remains_owned(self):
        source = (
            ROOT
            / "engine/flapjack-http/src/handlers/migration/async_status_tests.rs"
        ).read_text(encoding="utf-8")
        mutated = source.replace(
            "stale_generation_cannot_mutate_terminal_or_ack_state_for_any_provider",
            "renamed_without_reconciling_the_runner_contract",
            1,
        )
        self.assertNotEqual(source, mutated, "source mutation must remove the live specimen")
        with self.assertRaisesRegex(ContractError, "named interference specimen"):
            verify_local_runner(named_source_text=mutated)

    def test_local_runner_integration_owner_cannot_regress_to_fail_fast(self):
        runner = local_runner_path().read_text(encoding="utf-8")
        mutated = runner.replace(
            "  cargo nextest run --no-fail-fast\n",
            "  cargo nextest run\n",
            1,
        )
        self.assertNotEqual(runner, mutated, "runner mutation must restore fail-fast")
        with self.assertRaisesRegex(ContractError, "without fail-fast"):
            verify_local_runner(runner_text=mutated)

    def test_local_runner_path_prefers_owned_source_runner(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "engine/_dev/s/test"
            public = Path(temp) / "engine/s/test"
            source.parent.mkdir(parents=True)
            public.parent.mkdir(parents=True)
            source.write_text("#!/bin/bash\n", encoding="utf-8")
            public.write_text("#!/bin/bash\n", encoding="utf-8")
            self.assertEqual(local_runner_path(temp), source)

    def test_local_runner_path_accepts_public_mirror_layout(self):
        with tempfile.TemporaryDirectory() as temp:
            public = Path(temp) / "engine/s/test"
            public.parent.mkdir(parents=True)
            public.write_text("#!/bin/bash\n", encoding="utf-8")
            self.assertEqual(local_runner_path(temp), public)

    def test_local_runner_path_rejects_missing_layout(self):
        with tempfile.TemporaryDirectory() as temp:
            with self.assertRaisesRegex(ContractError, "missing from both"):
                local_runner_path(temp)

    def test_local_runner_path_rejects_unsupported_source_shape(self):
        with tempfile.TemporaryDirectory() as temp:
            source = Path(temp) / "engine/_dev/s/test"
            public = Path(temp) / "engine/s/test"
            source.mkdir(parents=True)
            public.parent.mkdir(parents=True, exist_ok=True)
            public.write_text("#!/bin/bash\n", encoding="utf-8")
            with self.assertRaisesRegex(ContractError, "unsupported"):
                local_runner_path(temp)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    if args.verify:
        try:
            verify()
        except (ContractError, OSError, ValueError, KeyError) as error:
            print(f"FAIL: {error}", file=sys.stderr)
            return 1
        print("PASS: every test class, workflow job, risk, and ignored test has an explicit tier owner")
        return 0
    unittest.main(argv=[sys.argv[0]])
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
