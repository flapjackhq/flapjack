#!/usr/bin/env python3
"""Hermetic contract tests for TEST-FLAKE-1 evidence reconciliation."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


REPAIR_SHA = "ba01016520d0928d002bf03c9551c1d793409e69"
SPECIMEN = "index::write_queue::tests::merge_owner_survives_consecutive_commits"
OWNER_PATH = "engine/src/index/write_queue_tests.rs"
SCRIPT = Path(__file__).with_name("reconcile_test_flake_evidence.py")
REPO = SCRIPT.parents[2]

RED_LOG = f"""running 1 test
test {SPECIMEN} ... FAILED

failures:

---- {SPECIMEN} stdout ----
worker shutdown should retain channel-closed merge quiescence before segment census; got []

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2271 filtered out; finished in 0.25s
"""
GREEN_LOG = f"""running 1 test
test {SPECIMEN} ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2271 filtered out; finished in 0.28s
"""
OWNER_LOG = f"""running 93 tests
test {SPECIMEN} ... ok

test result: ok. 90 passed; 0 failed; 3 ignored; 0 measured; 2179 filtered out; finished in 90.48s
"""
PASS_LOG = f"""     Running unittests src/lib.rs (target/debug/deps/flapjack-fixture)

running 1 test
test {SPECIMEN} ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s
"""
FAIL_OTHER_LOG = f"""     Running unittests src/lib.rs (target/debug/deps/flapjack-fixture)

running 2 tests
test {SPECIMEN} ... ok
test unrelated_failure ... FAILED

test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s

error: 1 target failed:
"""
TRUNCATED_LOG = PASS_LOG + "     Running tests/unfinished.rs (target/debug/deps/unfinished-fixture)\n\nrunning 1 test\n"
ZERO_TEST_LOG = "   Compiling fixture v0.1.0\nmake: *** internal error: invalid --jobserver-fds string\n"
STARTED_ONLY_LOG = (
    "     Running unittests src/lib.rs (target/debug/deps/flapjack-fixture)\n\nrunning 1 test\n"
)
EXPECTED_DIGESTS = {
    "red_merge_owner_exact_s19.log": "d28f791c6badac19e1470f6a8c9fa1c5465db44e43acc902102a467df80ac848",
    "green_merge_owner_exact.log": "ecb892c2a771190853aa4be83b96b688cb37082b1829093c1231fee32cdcdbba",
    "owner_module.log": "efb5446848fcca63f4a9e4947aa54d2c9afc5a630a56c89cf9ba837fe2bed437",
    "aug08_union.log": "d1d69e733254b39492e41818bbc5ba252b8e88a5ef59a368f0641c3c7fec8a4e",
    "aug08_union.done": "ea3b8e760798892af14703d8b772f443424bdedb38c8a1dacc15023fa28e6670",
    "aug08_killed": "f919c39688f0f56be87007d857d15620b791b34dec2072478fa96a09b8811b6f",
    "aug07_union.log": "218c759c4242c70fc2ebb645b8eee2077b02e7fe63fb0910accc78bbcfed0641",
    "aug07_union.done": "3c224e31b31996261d4bf98ad6d2ca368c65b4cdf083ec8bc92eccd8758e9ebd",
    "truncated_union.log": "86926ffe9a7dce44552f5429a32269984e46e08798550c113123d6896e040f98",
    "truncated_union.done": "9007e09defa041d44633f0ec819e67f7b605ab2183ae3209f1c593f2caaa0c40",
    "external_union.log": "d1d69e733254b39492e41818bbc5ba252b8e88a5ef59a368f0641c3c7fec8a4e",
    "external_union.done": "3cd2de0662dbe234b8bc697f6b0633bb116d36acb50f68e07619b9d67f837f4a",
    "invalid_union.log": "aa654999163af077cda0524802bbe8e5da9d9921a65a7b7649c695d877075106",
    "invalid_receipt": "9b2bf5934fb22b6b2e7898c923fe19211b5c7bd1f3a8646a0fd81b3b5c0ec187",
}


class EvidenceFixture:
    def __init__(self, root: Path, *, external_inside_repo: bool = False) -> None:
        self.root = root.resolve()
        self.repo = self.root / "repo"
        self.repair = self.root / "repair"
        self.aug08 = self.root / "aug08"
        self.aug07 = self.root / "aug07"
        self.external = (
            self.repo / "engine/docs2/4_EVIDENCE/external"
            if external_inside_repo
            else self.root / "external"
        )
        self.output = self.root / "output.txt"
        self.old_sha = ""
        self.nonancestor_sha = ""
        self._make_git_dag()
        self._make_repair_logs()
        self._make_run_roots()

    @staticmethod
    def _run(*args: str, cwd: Path, input_text: str | None = None) -> str:
        result = subprocess.run(
            args,
            cwd=cwd,
            input=input_text,
            text=True,
            capture_output=True,
            check=True,
        )
        return result.stdout.strip()

    @staticmethod
    def _git_env() -> dict[str, str]:
        env = os.environ.copy()
        env.update(
            {
                "GIT_AUTHOR_NAME": "Fixture",
                "GIT_AUTHOR_EMAIL": "fixture@example.invalid",
                "GIT_AUTHOR_DATE": "2001-01-01T00:00:00Z",
                "GIT_COMMITTER_NAME": "Fixture",
                "GIT_COMMITTER_EMAIL": "fixture@example.invalid",
                "GIT_COMMITTER_DATE": "2001-01-01T00:00:00Z",
            }
        )
        return env

    @staticmethod
    def _write(path: Path, text: str) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def _make_git_dag(self) -> None:
        self.repo.mkdir()
        self._run("git", "init", "--quiet", cwd=self.repo)
        self._run("git", "fetch", "--quiet", str(REPO), REPAIR_SHA, cwd=self.repo)
        self.old_sha = self._run("git", "rev-parse", f"{REPAIR_SHA}^", cwd=self.repo)
        env = self._git_env()
        repair_tree = self._run("git", "rev-parse", f"{REPAIR_SHA}^{{tree}}", cwd=self.repo)
        descendant_result = subprocess.run(
            ["git", "commit-tree", repair_tree, "-p", REPAIR_SHA, "-m", "fixture descendant"],
            cwd=self.repo,
            env=env,
            text=True,
            capture_output=True,
            check=True,
        )
        descendant = descendant_result.stdout.strip()
        self._run("git", "update-ref", "refs/remotes/origin/main", descendant, cwd=self.repo)
        empty_tree = self._run("git", "mktree", cwd=self.repo, input_text="")
        result = subprocess.run(
            ["git", "commit-tree", empty_tree, "-m", "nonancestor"],
            cwd=self.repo,
            env=env,
            text=True,
            capture_output=True,
            check=True,
        )
        self.nonancestor_sha = result.stdout.strip()

    def restore_after_superseding_repair_owner(self) -> None:
        original = self._run("git", "show", f"origin/main:{OWNER_PATH}", cwd=self.repo)
        mutated = original.replace(
            'event.phase == "merge_quiesced"',
            'event.phase == "merge_started"',
            1,
        )
        if mutated == original:
            raise AssertionError("fixture owner body did not contain repair fragment")

        self._run("git", "checkout", "--quiet", "-B", "fixture-main", "origin/main", cwd=self.repo)
        owner_file = self.repo / OWNER_PATH
        owner_file.write_text(mutated, encoding="utf-8")
        self._run("git", "add", OWNER_PATH, cwd=self.repo)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "supersede repair owner",
            ],
            cwd=self.repo,
            env=self._git_env(),
            check=True,
        )

        owner_file.write_text(original, encoding="utf-8")
        self._run("git", "add", OWNER_PATH, cwd=self.repo)
        subprocess.run(
            [
                "git",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "restore repair owner",
            ],
            cwd=self.repo,
            env=self._git_env(),
            check=True,
        )
        self._run("git", "update-ref", "refs/remotes/origin/main", "HEAD", cwd=self.repo)

    def _make_repair_logs(self) -> None:
        self._write(self.repair / "red_merge_owner_exact_s19.log", RED_LOG)
        self._write(self.repair / "green_merge_owner_exact.log", GREEN_LOG)
        self._write(self.repair / "owner_module.log", OWNER_LOG)

    def _make_run_roots(self) -> None:
        self._write(
            self.aug08 / "run_meta.txt",
            f"PGID=10 PID=10 START=2026-08-08T19:48:27Z\nSHA={REPAIR_SHA} WARM_ARTIFACTS=reused-no-clone\n",
        )
        self._write(
            self.aug08 / "union.done",
            "UNION_VERDICT=COMPLETE_WITH_FAILURES\n"
            "UNION_EXIT=101\nUNION_BINARIES=1\n"
            "UNION_END_UTC=2026-08-08T20:05:51Z\n"
            f"UNION_SHA={REPAIR_SHA}\n",
        )
        self._write(self.aug08 / "union.log", FAIL_OTHER_LOG)
        self._write(
            self.aug08 / "union.done.killed_attempt_1948",
            "UNION_VERDICT=INCOMPLETE_SHORT_RUN\n"
            "UNION_EXIT=137\nUNION_BINARIES=0\n"
            "UNION_END_UTC=2026-08-08T19:48:12Z\n"
            f"UNION_SHA={REPAIR_SHA}\n",
        )

        self._write(
            self.aug07 / "run_meta.txt",
            f"RUN=2\nSTART=2026-08-08T23:34:09Z\nWT_HEAD={REPAIR_SHA}\n",
        )
        self._write(self.aug07 / "union_sha.txt", f"{REPAIR_SHA}\n")
        self._write(
            self.aug07 / "union.done",
            "UNION_EXIT=0\nUNION_END_UTC=2026-08-09T00:51:44Z\n"
            f"UNION_SHA={REPAIR_SHA}\nUNION_BINARIES=1\n",
        )
        self._write(self.aug07 / "union.log", PASS_LOG)
        truncated = self.aug07 / "truncated_run_20260808T174108Z"
        self._write(truncated / "run_meta.txt", "START=2026-08-08T16:06:59Z\n")
        self._write(truncated / "union_sha.txt", f"{REPAIR_SHA}\n")
        self._write(
            truncated / "union.done",
            "UNION_EXIT=124\nUNION_END_UTC=2026-08-08T17:41:08Z\n"
            f"UNION_SHA={REPAIR_SHA}\nUNION_BINARIES=1\n",
        )
        self._write(truncated / "union.log", TRUNCATED_LOG)

        self._write(
            self.external / "union.done",
            "UNION_EXIT=101\n"
            f"LANE_HEAD={REPAIR_SHA}\nDETACHED_HEAD={REPAIR_SHA}\n"
            "FINISHED_AT=2026-08-08T12:44:37Z\n",
        )
        self._write(self.external / "union.log", FAIL_OTHER_LOG)
        self._write(
            self.external / "union_invalid_inherited_makeflags_receipt.md",
            "# Invalid Union Launch: Inherited MAKEFLAGS\n\n"
            "INVALID_ATTEMPT=invalid_inherited_makeflags\n"
            "INVALID_EXIT=101\nTESTS_STARTED=0\n"
            "STARTED_AT=2026-08-08T12:12:36Z\n",
        )
        self._write(self.external / "union_invalid_inherited_makeflags.log", ZERO_TEST_LOG)
        self._write(
            self.external / "union_launcher_invalid_inherited_makeflags.meta",
            f"LANE_HEAD={REPAIR_SHA}\nDETACHED_HEAD={REPAIR_SHA}\n",
        )

    def command(
        self,
        *extra: str,
        run_roots: list[tuple[str, Path]] | None = None,
    ) -> list[str]:
        command = [
            "python3",
            str(SCRIPT),
            "--repo",
            str(self.repo),
            "--repair-root",
            str(self.repair),
        ]
        declared_roots = run_roots or [
            ("aug08_rerun", self.aug08),
            ("aug07_postmerge", self.aug07),
            ("aug07_external", self.external),
        ]
        for name, path in declared_roots:
            command.extend(("--run-root", f"{name}={path}"))
        command.extend(("--output", str(self.output), *extra))
        return command

    def invoke(
        self,
        *extra: str,
        run_roots: list[tuple[str, Path]] | None = None,
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            self.command(*extra, run_roots=run_roots), text=True, capture_output=True
        )

    @staticmethod
    def digest(path: Path) -> str:
        return hashlib.sha256(path.read_bytes()).hexdigest()

    def expected_output(self) -> str:
        rows = [
            self._row(self.aug08 / "union.log", self.aug08 / "union.done", "2026-08-08T20:05:51Z", 101, 1, 2, "pass", "valid", "complete"),
            self._row(self.aug08 / "union.done.killed_attempt_1948", self.aug08 / "union.done.killed_attempt_1948", "2026-08-08T19:48:12Z", 137, 0, 0, "not_reached", "void", "missing_log"),
            self._row(self.aug07 / "union.log", self.aug07 / "union.done", "2026-08-09T00:51:44Z", 0, 1, 1, "pass", "valid", "complete"),
            self._row(self.aug07 / "truncated_run_20260808T174108Z/union.log", self.aug07 / "truncated_run_20260808T174108Z/union.done", "2026-08-08T17:41:08Z", 124, 1, 1, "pass", "void", "truncated"),
            self._row(self.external / "union.log", self.external / "union.done", "2026-08-08T12:44:37Z", 101, 1, 2, "pass", "valid", "complete"),
            self._row(self.external / "union_invalid_inherited_makeflags.log", self.external / "union_invalid_inherited_makeflags_receipt.md", "2026-08-08T12:12:36Z", 101, 0, 0, "not_reached", "void", "zero_tests"),
        ]
        negative = self.digest(self.repair / "red_merge_owner_exact_s19.log")
        green = self.digest(self.repair / "green_merge_owner_exact.log")
        rows.append(
            f"REPAIR_EVIDENCE sha={REPAIR_SHA} ancestor=yes pre_fix_reproducer=absent "
            "post_fix_negative_control=pass green=pass later_superseded=no "
            f"negative_log_sha256={negative} green_log_sha256={green}"
        )
        rows.append(
            "TEST_FLAKE_1_DISPOSITION valid_runs=3 specimen_passes=3 specimen_failures=0 "
            "complete_green_runs=1 pre_fix_reproducer=absent post_fix_negative_control=pass "
            "repair_green=pass verdict=keep_open"
        )
        return "\n".join(rows) + "\n"

    def literal_digest_specimens(self) -> dict[str, Path]:
        truncated = self.aug07 / "truncated_run_20260808T174108Z"
        return {
            "red_merge_owner_exact_s19.log": self.repair / "red_merge_owner_exact_s19.log",
            "green_merge_owner_exact.log": self.repair / "green_merge_owner_exact.log",
            "owner_module.log": self.repair / "owner_module.log",
            "aug08_union.log": self.aug08 / "union.log",
            "aug08_union.done": self.aug08 / "union.done",
            "aug08_killed": self.aug08 / "union.done.killed_attempt_1948",
            "aug07_union.log": self.aug07 / "union.log",
            "aug07_union.done": self.aug07 / "union.done",
            "truncated_union.log": truncated / "union.log",
            "truncated_union.done": truncated / "union.done",
            "external_union.log": self.external / "union.log",
            "external_union.done": self.external / "union.done",
            "invalid_union.log": self.external / "union_invalid_inherited_makeflags.log",
            "invalid_receipt": self.external / "union_invalid_inherited_makeflags_receipt.md",
        }

    def _row(
        self,
        log: Path,
        meta: Path,
        timestamp: str,
        exit_code: int,
        binaries: int,
        tests: int,
        specimen: str,
        validity: str,
        reason: str,
    ) -> str:
        return (
            f"RUN_EVIDENCE identity={REPAIR_SHA}@{timestamp} "
            f"log={self._display_path(log)} meta={self._display_path(meta)} "
            f"sha={REPAIR_SHA} exit={exit_code} binaries={binaries} tests={tests} "
            f"specimen={specimen} validity={validity} reason={reason} "
            f"log_sha256={self.digest(log)} meta_sha256={self.digest(meta)}"
        )

    def _display_path(self, path: Path) -> Path:
        try:
            return path.resolve().relative_to(self.repo)
        except ValueError:
            return path.resolve()


class ReconcileEvidenceTests(unittest.TestCase):
    def assert_rejected(self, fixture: EvidenceFixture, message: str) -> None:
        result = fixture.invoke()
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(message, result.stderr)

    def test_positive_fixture_matches_canonical_output_byte_for_byte(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            self.assertEqual(
                {name: fixture.digest(path) for name, path in fixture.literal_digest_specimens().items()},
                EXPECTED_DIGESTS,
                "fixture bytes must retain their independently precomputed SHA-256 values",
            )
            result = fixture.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                fixture.output.read_text(encoding="utf-8"),
                fixture.expected_output(),
                "canonical output must reconcile every declared evidence row",
            )

    def test_known_incomplete_attempts_emit_specific_void_rows(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            result = fixture.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            output = fixture.output.read_text(encoding="utf-8")
            self.assertEqual(output.count(" validity=void "), 3)
            self.assertIn("reason=missing_log", output)
            self.assertIn("reason=truncated", output)
            self.assertIn("reason=zero_tests", output)

    def test_repository_owned_evidence_paths_are_repo_relative(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory), external_inside_repo=True)
            result = fixture.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            output = fixture.output.read_text(encoding="utf-8")
            self.assertIn(
                "log=engine/docs2/4_EVIDENCE/external/union.log "
                "meta=engine/docs2/4_EVIDENCE/external/union.done",
                output,
            )
            self.assertNotIn(str(fixture.repo), output)

    def test_canonical_order_does_not_depend_on_run_root_argument_order(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            result = fixture.invoke(
                run_roots=[
                    ("external", fixture.external),
                    ("postmerge", fixture.aug07),
                    ("rerun", fixture.aug08),
                ]
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(fixture.output.read_text(), fixture.expected_output())

    def test_consistent_pre_repair_sha_is_void_as_moved_sha(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            done = fixture.external / "union.done"
            done.write_text(done.read_text().replace(REPAIR_SHA, fixture.old_sha), encoding="utf-8")
            result = fixture.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            row = next(
                line for line in fixture.output.read_text().splitlines()
                if f"meta={done}" in line
            )
            self.assertIn(f"sha={fixture.old_sha}", row)
            self.assertIn("validity=void reason=moved_sha", row)

    def test_later_superseded_uses_post_repair_touch_history(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            fixture.restore_after_superseding_repair_owner()
            result = fixture.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            repair_line = next(
                line
                for line in fixture.output.read_text(encoding="utf-8").splitlines()
                if line.startswith("REPAIR_EVIDENCE ")
            )
            self.assertIn("later_superseded=yes", repair_line)

    def test_duplicate_run_identity_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            killed = fixture.aug08 / "union.done.killed_attempt_1948"
            killed.write_text(
                killed.read_text().replace("2026-08-08T19:48:12Z", "2026-08-08T20:05:51Z"),
                encoding="utf-8",
            )
            self.assert_rejected(fixture, "duplicate run identity")

    def test_declared_binary_denominator_contradiction_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            done = fixture.aug07 / "union.done"
            done.write_text(done.read_text().replace("UNION_BINARIES=1", "UNION_BINARIES=2"), encoding="utf-8")
            self.assert_rejected(fixture, "declares 2 binaries but log contains 1 complete binaries")

    def test_exit_zero_with_failed_result_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.aug07 / "union.log").write_text(FAIL_OTHER_LOG, encoding="utf-8")
            self.assert_rejected(fixture, "exit 0 contradicts 1 failed tests")

    def test_repair_log_summary_status_failed_with_zero_failed_tests_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            log = fixture.repair / "red_merge_owner_exact_s19.log"
            log.write_text(
                RED_LOG.replace(
                    "test result: FAILED. 0 passed; 1 failed;",
                    "test result: FAILED. 1 passed; 0 failed;",
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "test result status FAILED contradicts zero failed tests",
            )

    def test_repair_log_summary_status_ok_with_failed_tests_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            log = fixture.repair / "green_merge_owner_exact.log"
            log.write_text(
                GREEN_LOG.replace(
                    "test result: ok. 1 passed; 0 failed;",
                    "test result: ok. 1 passed; 1 failed;",
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "test result status ok contradicts 1 failed tests",
            )

    def test_broad_log_summary_status_failed_with_zero_failed_tests_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            log = fixture.aug07 / "union.log"
            log.write_text(
                PASS_LOG.replace(
                    "test result: ok. 1 passed; 0 failed;",
                    "test result: FAILED. 1 passed; 0 failed;",
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "test result status FAILED contradicts zero failed tests",
            )

    def test_broad_log_summary_status_ok_with_failed_tests_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            log = fixture.aug08 / "union.log"
            log.write_text(
                FAIL_OTHER_LOG.replace(
                    "test result: FAILED. 1 passed; 1 failed;",
                    "test result: ok. 1 passed; 1 failed;",
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "test result status ok contradicts 1 failed tests",
            )

    def test_complete_exit_with_unfinished_binary_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            log = fixture.aug07 / "union.log"
            log.write_text(
                log.read_text()
                + "     Running tests/unfinished.rs (target/debug/deps/unfinished)\n\n"
                + "running 1 test\n",
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "complete exit 0 has 2 started binaries but only 1 result summaries",
            )

    def test_missing_invalid_attempt_metadata_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.external / "union_invalid_inherited_makeflags_receipt.md").unlink()
            self.assert_rejected(fixture, "unpairable invalid attempt")

    def test_raw_sha_declaration_contradiction_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            meta = fixture.aug08 / "run_meta.txt"
            meta.write_text(meta.read_text().replace(REPAIR_SHA, fixture.nonancestor_sha), encoding="utf-8")
            self.assert_rejected(fixture, "contradictory SHA fields")

    def test_aug07_root_raw_sha_declaration_contradiction_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.aug07 / "union_sha.txt").write_text(
                f"{fixture.old_sha}\n", encoding="utf-8"
            )
            self.assert_rejected(fixture, "contradictory SHA fields")

    def test_aug07_root_raw_sha_component_is_required(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.aug07 / "union_sha.txt").unlink()
            self.assert_rejected(fixture, "missing aug07 evidence component")

    def test_nonancestor_run_sha_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            done = fixture.external / "union.done"
            done.write_text(done.read_text().replace(REPAIR_SHA, fixture.nonancestor_sha), encoding="utf-8")
            self.assert_rejected(fixture, "is not an ancestor of origin/main")

    def test_valid_attempt_with_failing_specimen_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            mutated = FAIL_OTHER_LOG.replace(
                f"test {SPECIMEN} ... ok\ntest unrelated_failure ... FAILED",
                f"test {SPECIMEN} ... FAILED\ntest unrelated_failure ... ok",
            )
            (fixture.aug08 / "union.log").write_text(mutated, encoding="utf-8")
            self.assert_rejected(fixture, "valid run specimen must pass")

    def test_aug08_unknown_extra_candidate_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.aug08 / "union.done.unexpected_attempt").write_text("UNION_EXIT=137\n", encoding="utf-8")
            self.assert_rejected(fixture, "unknown extra candidate")

    def test_killed_sidecar_successful_exit_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            killed = fixture.aug08 / "union.done.killed_attempt_1948"
            killed.write_text(
                killed.read_text().replace("UNION_EXIT=137", "UNION_EXIT=0"),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "killed sidecar must declare UNION_EXIT=137, got 0",
            )

    def test_killed_sidecar_complete_verdict_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            killed = fixture.aug08 / "union.done.killed_attempt_1948"
            killed.write_text(
                killed.read_text().replace(
                    "UNION_VERDICT=INCOMPLETE_SHORT_RUN",
                    "UNION_VERDICT=COMPLETE_WITH_FAILURES",
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "killed sidecar must declare UNION_VERDICT=INCOMPLETE_SHORT_RUN",
            )

    def test_aug07_unknown_extra_candidate_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.aug07 / "union.done.unexpected_attempt").write_text("UNION_EXIT=137\n", encoding="utf-8")
            self.assert_rejected(fixture, "unknown extra candidate in aug07 layout")

    def test_aug07_truncated_nested_extra_candidate_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            nested = fixture.aug07 / "truncated_run_20260808T174108Z"
            (nested / "union.done.unexpected_attempt").write_text(
                "UNION_EXIT=137\n", encoding="utf-8"
            )
            self.assert_rejected(
                fixture,
                "unknown extra candidate in aug07 truncated layout",
            )

    def test_external_unknown_extra_candidate_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.external / "union.done.unexpected_attempt").write_text("UNION_EXIT=137\n", encoding="utf-8")
            self.assert_rejected(fixture, "unknown extra candidate in external layout")

    def test_zero_test_receipt_contradicted_by_executed_tests_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.external / "union_invalid_inherited_makeflags.log").write_text(
                FAIL_OTHER_LOG, encoding="utf-8"
            )
            self.assert_rejected(
                fixture,
                "receipt declares TESTS_STARTED=0 but the raw log contains "
                "1 started binaries, 1 completed denominators, 2 executed tests, "
                "specimen=pass",
            )

    def test_zero_test_receipt_contradicted_by_started_binary_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.external / "union_invalid_inherited_makeflags.log").write_text(
                STARTED_ONLY_LOG, encoding="utf-8"
            )
            self.assert_rejected(
                fixture,
                "receipt declares TESTS_STARTED=0 but the raw log contains "
                "1 started binaries, 0 completed denominators, 0 executed tests, "
                "specimen=not_reached",
            )

    def test_unexpected_post_repair_zero_test_run_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            (fixture.aug07 / "union.log").write_text(ZERO_TEST_LOG, encoding="utf-8")
            done = fixture.aug07 / "union.done"
            done.write_text(
                done.read_text(encoding="utf-8").replace(
                    "UNION_EXIT=0\n", "UNION_EXIT=101\n"
                ).replace("UNION_BINARIES=1\n", "UNION_BINARIES=0\n"),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "unexpected zero-test attempt; only the documented inherited-MAKEFLAGS "
                "receipt may classify reason=zero_tests",
            )

    def test_unexpected_truncated_zero_test_run_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            truncated = fixture.aug07 / "truncated_run_20260808T174108Z"
            (truncated / "union.log").write_text(ZERO_TEST_LOG, encoding="utf-8")
            done = truncated / "union.done"
            done.write_text(
                done.read_text(encoding="utf-8").replace(
                    "UNION_BINARIES=1\n", "UNION_BINARIES=0\n"
                ),
                encoding="utf-8",
            )
            self.assert_rejected(
                fixture,
                "unexpected zero-test attempt; only the documented inherited-MAKEFLAGS "
                "receipt may classify reason=zero_tests",
            )

    def test_renamed_external_invalid_attempt_pair_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            receipt = fixture.external / "union_invalid_inherited_makeflags_receipt.md"
            log = fixture.external / "union_invalid_inherited_makeflags.log"
            receipt.rename(fixture.external / "union_invalid_renamed_receipt.md")
            log.rename(fixture.external / "union_invalid_renamed.log")
            self.assert_rejected(
                fixture,
                "unknown invalid attempt candidate; expected "
                "union_invalid_inherited_makeflags_receipt.md and "
                "union_invalid_inherited_makeflags.log",
            )

    def test_verdict_has_no_caller_controlled_override(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            result = fixture.invoke("--verdict", "close")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("unrecognized arguments: --verdict close", result.stderr)

    def test_raw_log_byte_mutation_changes_derived_hash(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            fixture = EvidenceFixture(Path(directory))
            stale_expected = fixture.expected_output()
            log = fixture.aug07 / "union.log"
            stale_hash = fixture.digest(log)
            log.write_bytes(log.read_bytes() + b"\n")
            result = fixture.invoke()
            self.assertEqual(result.returncode, 0, result.stderr)
            actual = fixture.output.read_text(encoding="utf-8")
            self.assertNotEqual(actual, stale_expected)
            self.assertNotIn(f"log_sha256={stale_hash}", next(line for line in actual.splitlines() if f"log={log}" in line))


if __name__ == "__main__":
    unittest.main()
