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
        jobs_marker = re.search(r"^jobs:\s*$", text, re.MULTILINE)
        if jobs_marker is None:
            raise ContractError(f"workflow has no jobs mapping: {relative}")
        # Event keys under `on:` use the same two-space indentation as job keys.
        # Restrict extraction to the jobs mapping so push/schedule/dispatch can
        # never be misclassified as executable test owners.
        jobs_text = text[jobs_marker.end():]
        matches = list(JOB_RE.finditer(jobs_text))
        for index, match in enumerate(matches):
            end = matches[index + 1].start() if index + 1 < len(matches) else len(jobs_text)
            blocks[f"{relative}#{match.group(1)}"] = jobs_text[match.start():end]
    return blocks


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
    prebuild = "cargo build --tests -p flapjack -p flapjack-http --features vector-search"
    if prebuild not in all_job or "RUSTFLAGS='" + "-C debuginfo=0 -C strip=debuginfo' " + prebuild in all_job:
        raise ContractError("vector prebuild and nextest must share the job-level compilation identity")


class TestTierContract(unittest.TestCase):
    def test_live_manifest_and_workflows_converge(self):
        verify()

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
