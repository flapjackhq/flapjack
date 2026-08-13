#!/usr/bin/env python3
"""Reconcile the bounded raw evidence for the TEST-FLAKE-1 disposition.

This is intentionally not a generic evidence framework.  It owns the three
historical bundle layouts named by the Stage 1 contract and rejects anything
outside those closed shapes.
"""

from __future__ import annotations

import argparse
from dataclasses import dataclass
import hashlib
from pathlib import Path
import re
import subprocess
import sys
from typing import Iterable


REPAIR_SHA = "ba01016520d0928d002bf03c9551c1d793409e69"
SPECIMEN = "index::write_queue::tests::merge_owner_survives_consecutive_commits"
OWNER_PATH = "engine/src/index/write_queue_tests.rs"
REQUIRED_REPAIR_FRAGMENTS = (
    "drop(tx)",
    "handle",
    'event.reason == "channel_closed"',
    'event.phase == "merge_quiesced"',
    "observed_segments(index.as_ref())",
)
SUMMARY_RE = re.compile(
    r"^test result: (?P<status>ok|FAILED)\. (?P<passed>\d+) passed; "
    r"(?P<failed>\d+) failed; (?P<ignored>\d+) ignored;",
    re.MULTILINE,
)
ASSIGNMENT_RE = re.compile(r"\b([A-Z][A-Z0-9_]*)=([^\s#]+)")
SHA_RE = re.compile(r"^[0-9a-f]{40}$")
TIMESTAMP_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$")


class EvidenceError(ValueError):
    """Raised when raw evidence violates the closed reconciliation contract."""


@dataclass(frozen=True)
class LogSummary:
    started_binaries: int
    binaries: int
    tests: int
    failed: int
    specimen: str


@dataclass(frozen=True)
class RunEvidence:
    log: Path
    meta: Path
    sha: str
    timestamp: str
    exit_code: int
    binaries: int
    tests: int
    failed_tests: int
    specimen: str
    validity: str
    reason: str
    log_sha256: str
    meta_sha256: str

    @property
    def identity(self) -> str:
        return f"{self.sha}@{self.timestamp}"

    def machine_line(self) -> str:
        return (
            f"RUN_EVIDENCE identity={self.identity} log={self.log} meta={self.meta} "
            f"sha={self.sha} exit={self.exit_code} binaries={self.binaries} "
            f"tests={self.tests} specimen={self.specimen} validity={self.validity} "
            f"reason={self.reason} log_sha256={self.log_sha256} "
            f"meta_sha256={self.meta_sha256}"
        )


def sha256(path: Path) -> str:
    try:
        return hashlib.sha256(path.read_bytes()).hexdigest()
    except OSError as error:
        raise EvidenceError(f"{path}: cannot read evidence: {error}") from error


def machine_path(repo: Path, path: Path) -> Path:
    """Use durable repo-relative paths for evidence tracked inside the repository."""
    resolved = path.resolve()
    try:
        return resolved.relative_to(repo)
    except ValueError:
        return resolved


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise EvidenceError(f"{path}: cannot read text evidence: {error}") from error


def assignments(path: Path) -> dict[str, str]:
    values: dict[str, str] = {}
    for key, value in ASSIGNMENT_RE.findall(read_text(path)):
        previous = values.get(key)
        if previous is not None and previous != value:
            raise EvidenceError(
                f"{path}: contradictory repeated {key} fields: {previous} versus {value}"
            )
        values[key] = value
    return values


def integer_field(path: Path, values: dict[str, str], key: str) -> int:
    raw = values.get(key)
    if raw is None:
        raise EvidenceError(f"{path}: missing required {key} field")
    try:
        return int(raw)
    except ValueError as error:
        raise EvidenceError(f"{path}: {key} must be an integer, got {raw!r}") from error


def common_field(
    labeled_values: Iterable[tuple[Path, dict[str, str], tuple[str, ...]]],
    field_name: str,
) -> str:
    declarations: list[tuple[Path, str]] = []
    for path, values, keys in labeled_values:
        for key in keys:
            if key in values:
                declarations.append((path, values[key]))
    if not declarations:
        paths = ", ".join(str(path) for path, _, _ in labeled_values)
        raise EvidenceError(f"{paths}: missing required {field_name} field")
    distinct = {value for _, value in declarations}
    if len(distinct) != 1:
        detail = ", ".join(f"{path}={value}" for path, value in declarations)
        raise EvidenceError(f"contradictory {field_name} fields: {detail}")
    return declarations[0][1]


def validate_sha(path: Path, sha: str) -> None:
    if not SHA_RE.fullmatch(sha):
        raise EvidenceError(f"{path}: malformed SHA {sha!r}")


def validate_timestamp(path: Path, timestamp: str) -> None:
    if not TIMESTAMP_RE.fullmatch(timestamp):
        raise EvidenceError(f"{path}: malformed UTC timestamp {timestamp!r}")


def parse_log(path: Path) -> LogSummary:
    text = read_text(path)
    summaries = list(SUMMARY_RE.finditer(text))
    binary_headings = sum(
        1
        for line in text.splitlines()
        if re.match(r"^\s+Running (?:unittests |tests/)", line)
        or re.match(r"^\s+Doc-tests ", line)
    )
    if len(summaries) > binary_headings:
        raise EvidenceError(
            f"{path}: {len(summaries)} test summaries exceed {binary_headings} binary headings"
        )
    for match in summaries:
        validate_summary_status(path, match)
    passed = sum(int(match.group("passed")) for match in summaries)
    failed = sum(int(match.group("failed")) for match in summaries)
    specimen_lines = re.findall(
        rf"^test {re.escape(SPECIMEN)} \.\.\. (ok|FAILED)$", text, re.MULTILINE
    )
    if len(specimen_lines) > 1:
        raise EvidenceError(f"{path}: specimen appears more than once")
    specimen = (
        "pass"
        if specimen_lines == ["ok"]
        else "fail"
        if specimen_lines == ["FAILED"]
        else "not_reached"
    )
    return LogSummary(
        started_binaries=binary_headings,
        binaries=len(summaries),
        tests=passed + failed,
        failed=failed,
        specimen=specimen,
    )


def validate_summary_status(path: Path, summary: re.Match[str]) -> None:
    status = summary.group("status")
    failed = int(summary.group("failed"))
    if status == "ok" and failed != 0:
        raise EvidenceError(
            f"{path}: test result status ok contradicts {failed} failed tests"
        )
    if status == "FAILED" and failed == 0:
        raise EvidenceError(
            f"{path}: test result status FAILED contradicts zero failed tests"
        )


def git(repo: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess[str]:
    try:
        result = subprocess.run(
            ["git", *args],
            cwd=repo,
            text=True,
            capture_output=True,
            timeout=20,
        )
    except (OSError, subprocess.SubprocessError) as error:
        raise EvidenceError(f"{repo}: git {' '.join(args)} failed: {error}") from error
    if check and result.returncode != 0:
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise EvidenceError(f"{repo}: git {' '.join(args)} failed: {detail}")
    return result


def is_ancestor(repo: Path, older: str, newer: str) -> bool:
    result = git(repo, "merge-base", "--is-ancestor", older, newer, check=False)
    if result.returncode not in (0, 1):
        detail = result.stderr.strip() or result.stdout.strip() or f"exit {result.returncode}"
        raise EvidenceError(
            f"{repo}: cannot compare ancestry {older}..{newer}: {detail}"
        )
    return result.returncode == 0


def extract_specimen_body(source: str, label: str) -> str:
    start_match = re.search(
        r"async fn merge_owner_survives_consecutive_commits\(\) \{", source
    )
    if start_match is None:
        raise EvidenceError(f"{label}: specimen body is missing")
    remainder = source[start_match.start() :]
    next_test = re.search(r"\n#\[(?:tokio::)?test", remainder[1:])
    return remainder if next_test is None else remainder[: next_test.start() + 1]


def missing_repair_fragment(body: str) -> str | None:
    cursor = 0
    for fragment in REQUIRED_REPAIR_FRAGMENTS:
        position = body.find(fragment, cursor)
        if position < 0:
            return fragment
        cursor = position + len(fragment)
    return None


def require_repaired_specimen(body: str, label: str) -> None:
    missing = missing_repair_fragment(body)
    if missing is not None:
        raise EvidenceError(
            f"{label}: specimen supersedes required repair fragment {missing!r}"
        )


def later_superseded(repo: Path) -> bool:
    commits = git(
        repo,
        "log",
        "--format=%H",
        f"{REPAIR_SHA}..origin/main",
        "--",
        OWNER_PATH,
    ).stdout.splitlines()
    for commit in commits:
        result = git(repo, "show", f"{commit}:{OWNER_PATH}", check=False)
        if result.returncode != 0:
            return True
        try:
            body = extract_specimen_body(result.stdout, f"{OWNER_PATH}@{commit}")
        except EvidenceError:
            return True
        if missing_repair_fragment(body) is not None:
            return True
    return False


def verify_repair(repo: Path, repair_root: Path) -> tuple[str, str, str]:
    if not is_ancestor(repo, REPAIR_SHA, "origin/main"):
        raise EvidenceError(f"{REPAIR_SHA} is not an ancestor of origin/main")

    source = git(repo, "show", f"origin/main:{OWNER_PATH}").stdout
    require_repaired_specimen(
        extract_specimen_body(source, f"origin/main:{OWNER_PATH}"),
        f"origin/main:{OWNER_PATH}",
    )
    superseded = "yes" if later_superseded(repo) else "no"

    negative_path = repair_root / "red_merge_owner_exact_s19.log"
    green_path = repair_root / "green_merge_owner_exact.log"
    owner_path = repair_root / "owner_module.log"
    negative_text = read_text(negative_path)
    green_text = read_text(green_path)
    owner_text = read_text(owner_path)

    negative = parse_repair_log(negative_path, negative_text, "FAILED", 0, 1)
    if "worker shutdown should retain channel-closed merge quiescence" not in negative_text:
        raise EvidenceError(
            f"{negative_path}: negative control did not fail at the retained lifecycle assertion"
        )
    green = parse_repair_log(green_path, green_text, "ok", 1, 0)
    parse_repair_log(owner_path, owner_text, "ok", 90, 0, expected_ignored=3)
    return (
        "pass" if negative else "fail",
        "pass" if green else "fail",
        (
            f"REPAIR_EVIDENCE sha={REPAIR_SHA} ancestor=yes "
            "pre_fix_reproducer=absent post_fix_negative_control=pass green=pass "
            f"later_superseded={superseded} "
            f"negative_log_sha256={sha256(negative_path)} "
            f"green_log_sha256={sha256(green_path)}"
        ),
    )


def parse_repair_log(
    path: Path,
    text: str,
    outcome: str,
    passed: int,
    failed: int,
    *,
    expected_ignored: int = 0,
) -> bool:
    specimen_lines = re.findall(
        rf"^test {re.escape(SPECIMEN)} \.\.\. (ok|FAILED)$", text, re.MULTILINE
    )
    if specimen_lines != [outcome]:
        raise EvidenceError(
            f"{path}: expected exactly one specimen outcome {outcome}, got {specimen_lines}"
        )
    summaries = list(SUMMARY_RE.finditer(text))
    if len(summaries) != 1:
        raise EvidenceError(f"{path}: expected exactly one test-result denominator")
    observed = summaries[0]
    validate_summary_status(path, observed)
    denominator = (
        int(observed.group("passed")),
        int(observed.group("failed")),
        int(observed.group("ignored")),
    )
    expected = (passed, failed, expected_ignored)
    if denominator != expected:
        raise EvidenceError(
            f"{path}: expected denominator {expected}, got {denominator}"
        )
    return True


def build_run(
    repo: Path,
    *,
    log: Path,
    meta: Path,
    supplemental: Iterable[Path] = (),
    sha_keys: tuple[str, ...] = ("UNION_SHA",),
    timestamp_keys: tuple[str, ...] = ("UNION_END_UTC",),
    exit_key: str = "UNION_EXIT",
    declared_binary_key: str | None = "UNION_BINARIES",
    forced_reason: str | None = None,
    missing_log: bool = False,
    killed_sidecar: bool = False,
    zero_test_receipt: bool = False,
) -> RunEvidence:
    all_meta = (meta, *supplemental)
    parsed = [(path, assignments(path)) for path in all_meta]
    sha = common_field(
        [(path, values, sha_keys) for path, values in parsed], "SHA"
    )
    validate_sha(meta, sha)
    timestamp = common_field(
        [(path, values, timestamp_keys) for path, values in parsed], "terminal/start timestamp"
    )
    validate_timestamp(meta, timestamp)
    exit_code = integer_field(meta, parsed[0][1], exit_key)
    if killed_sidecar:
        verdict = parsed[0][1].get("UNION_VERDICT")
        if verdict != "INCOMPLETE_SHORT_RUN":
            raise EvidenceError(
                f"{meta}: killed sidecar must declare UNION_VERDICT=INCOMPLETE_SHORT_RUN, "
                f"got {verdict!r}"
            )
        if exit_code != 137:
            raise EvidenceError(
                f"{meta}: killed sidecar must declare UNION_EXIT=137, got {exit_code}"
            )

    if missing_log:
        summary = LogSummary(0, 0, 0, 0, "not_reached")
        log_hash = sha256(meta)
    else:
        summary = parse_log(log)
        log_hash = sha256(log)

    if zero_test_receipt:
        # A receipt may only claim it started no tests when its own raw log agrees:
        # any started binary, completed denominator, executed test, or specimen
        # result contradicts the declaration and must terminate rather than void.
        if (
            summary.started_binaries
            or summary.binaries
            or summary.tests
            or summary.specimen != "not_reached"
        ):
            raise EvidenceError(
                f"{log}: receipt declares TESTS_STARTED=0 but the raw log contains "
                f"{summary.started_binaries} started binaries, {summary.binaries} "
                f"completed denominators, {summary.tests} executed tests, "
                f"specimen={summary.specimen}"
            )
        forced_reason = "zero_tests"

    declared_binaries: list[tuple[Path, int]] = []
    if declared_binary_key is not None:
        for path, values in parsed:
            if declared_binary_key in values:
                declared_binaries.append(
                    (path, integer_field(path, values, declared_binary_key))
                )
    for path, declared in declared_binaries:
        if declared != summary.binaries:
            raise EvidenceError(
                f"{path}: declares {declared} binaries but log contains "
                f"{summary.binaries} complete binaries"
            )

    if exit_code == 0 and summary.failed:
        raise EvidenceError(
            f"{meta}: exit 0 contradicts {summary.failed} failed tests in {log}"
        )
    if exit_code == 101 and summary.tests > 0 and summary.failed == 0:
        raise EvidenceError(
            f"{meta}: exit 101 contradicts a log with zero failed tests in {log}"
        )
    if exit_code in (0, 101) and summary.started_binaries != summary.binaries:
        raise EvidenceError(
            f"{log}: complete exit {exit_code} has {summary.started_binaries} started "
            f"binaries but only {summary.binaries} result summaries"
        )

    if not is_ancestor(repo, sha, "origin/main"):
        raise EvidenceError(f"{meta}: run SHA {sha} is not an ancestor of origin/main")
    post_repair = is_ancestor(repo, REPAIR_SHA, sha)

    if forced_reason is not None:
        validity, reason = "void", forced_reason
    elif not post_repair:
        validity, reason = "void", "moved_sha"
    elif summary.tests == 0:
        raise EvidenceError(
            f"{log}: unexpected zero-test attempt; only the documented "
            "inherited-MAKEFLAGS receipt may classify reason=zero_tests"
        )
    elif exit_code == 124:
        validity, reason = "void", "truncated"
    elif exit_code in (0, 101) and summary.binaries > 0:
        validity, reason = "valid", "complete"
    else:
        raise EvidenceError(
            f"{meta}: unclassified attempt exit={exit_code} binaries={summary.binaries} "
            f"tests={summary.tests}"
        )
    if validity == "valid" and summary.specimen != "pass":
        raise EvidenceError(
            f"{log}: valid run specimen must pass, got {summary.specimen}"
        )

    return RunEvidence(
        log=machine_path(repo, log),
        meta=machine_path(repo, meta),
        sha=sha,
        timestamp=timestamp,
        exit_code=exit_code,
        binaries=summary.binaries,
        tests=summary.tests,
        failed_tests=summary.failed,
        specimen=summary.specimen,
        validity=validity,
        reason=reason,
        log_sha256=log_hash,
        meta_sha256=sha256(meta),
    )


def reject_extra_sidecars(root: Path, layout: str, allowed: Iterable[Path] = ()) -> None:
    allowed_paths = {path.resolve() for path in allowed}
    candidate_patterns = ("union.done.*", "union.log.*")
    for pattern in candidate_patterns:
        for path in sorted(root.glob(pattern)):
            if path.resolve() not in allowed_paths:
                raise EvidenceError(f"{path}: unknown extra candidate in {layout} layout")


def parse_aug08(repo: Path, root: Path) -> list[RunEvidence]:
    expected_sidecar = root / "union.done.killed_attempt_1948"
    reject_extra_sidecars(root, "aug08", (expected_sidecar,))
    for required in (root / "union.done", root / "union.log", root / "run_meta.txt", expected_sidecar):
        if not required.is_file():
            raise EvidenceError(f"{required}: missing aug08 evidence component")
    valid = build_run(
        repo,
        log=root / "union.log",
        meta=root / "union.done",
        supplemental=(root / "run_meta.txt",),
        sha_keys=("UNION_SHA", "SHA"),
        timestamp_keys=("UNION_END_UTC",),
    )
    killed = build_run(
        repo,
        log=expected_sidecar,
        meta=expected_sidecar,
        forced_reason="missing_log",
        missing_log=True,
        killed_sidecar=True,
    )
    return [valid, killed]


def cross_check_raw_sha(directory: Path) -> None:
    """Both aug07 attempts carry a raw union_sha.txt beside their declaring receipt.

    The raw file is the only independent witness of which commit actually ran, so a
    receipt that has moved away from it is a contradiction, not a void attempt.
    """
    raw_path = directory / "union_sha.txt"
    raw_sha = read_text(raw_path).strip()
    if not SHA_RE.fullmatch(raw_sha):
        raise EvidenceError(f"{raw_path}: malformed SHA {raw_sha!r}")
    declared = assignments(directory / "union.done").get("UNION_SHA")
    if declared != raw_sha:
        raise EvidenceError(
            f"contradictory SHA fields: {directory / 'union.done'}={declared}, "
            f"{raw_path}={raw_sha}"
        )


def parse_aug07(repo: Path, root: Path) -> list[RunEvidence]:
    reject_extra_sidecars(root, "aug07")
    nested = sorted(path for path in root.glob("truncated_run_*") if path.is_dir())
    if len(nested) != 1:
        raise EvidenceError(
            f"{root}: expected exactly one truncated_run_* candidate, got {len(nested)}"
        )
    for required in (
        root / "union.done",
        root / "union.log",
        root / "run_meta.txt",
        root / "union_sha.txt",
    ):
        if not required.is_file():
            raise EvidenceError(f"{required}: missing aug07 evidence component")
    cross_check_raw_sha(root)
    valid = build_run(
        repo,
        log=root / "union.log",
        meta=root / "union.done",
        supplemental=(root / "run_meta.txt",),
        sha_keys=("UNION_SHA", "WT_HEAD"),
        timestamp_keys=("UNION_END_UTC",),
    )
    truncated_root = nested[0]
    reject_extra_sidecars(truncated_root, "aug07 truncated")
    required_nested = (
        truncated_root / "union.done",
        truncated_root / "union.log",
        truncated_root / "run_meta.txt",
        truncated_root / "union_sha.txt",
    )
    for required in required_nested:
        if not required.is_file():
            raise EvidenceError(f"{required}: missing truncated attempt component")
    cross_check_raw_sha(truncated_root)
    truncated = build_run(
        repo,
        log=truncated_root / "union.log",
        meta=truncated_root / "union.done",
        supplemental=(truncated_root / "run_meta.txt",),
        timestamp_keys=("UNION_END_UTC",),
    )
    return [valid, truncated]


def parse_external(repo: Path, root: Path) -> list[RunEvidence]:
    reject_extra_sidecars(root, "external")
    receipts = sorted(root.glob("union_invalid_*_receipt.md"))
    logs = sorted(root.glob("union_invalid_*.log"))
    if len(receipts) != 1 or len(logs) != 1:
        raise EvidenceError(
            f"{root}: unpairable invalid attempt: found {len(logs)} logs and "
            f"{len(receipts)} metadata receipts"
        )
    receipt = root / "union_invalid_inherited_makeflags_receipt.md"
    invalid_log = root / "union_invalid_inherited_makeflags.log"
    if receipts != [receipt] or logs != [invalid_log]:
        raise EvidenceError(
            f"{root}: unknown invalid attempt candidate; expected "
            "union_invalid_inherited_makeflags_receipt.md and "
            "union_invalid_inherited_makeflags.log"
        )
    for required in (root / "union.done", root / "union.log"):
        if not required.is_file():
            raise EvidenceError(f"{required}: missing external evidence component")

    launcher = root / "union_launcher.meta"
    valid_supplemental = (launcher,) if launcher.is_file() else ()
    valid = build_run(
        repo,
        log=root / "union.log",
        meta=root / "union.done",
        supplemental=valid_supplemental,
        sha_keys=("UNION_SHA", "LANE_HEAD", "DETACHED_HEAD"),
        timestamp_keys=("FINISHED_AT",),
        declared_binary_key=None,
    )

    invalid_launcher = root / "union_launcher_invalid_inherited_makeflags.meta"
    if not invalid_launcher.is_file():
        raise EvidenceError(f"{invalid_launcher}: missing invalid attempt launcher metadata")
    invalid_values = assignments(receipt)
    if invalid_values.get("INVALID_ATTEMPT") != "invalid_inherited_makeflags":
        raise EvidenceError(f"{receipt}: unknown invalid attempt declaration")
    if integer_field(receipt, invalid_values, "TESTS_STARTED") != 0:
        raise EvidenceError(f"{receipt}: inherited-MAKEFLAGS attempt must declare TESTS_STARTED=0")
    invalid = build_run(
        repo,
        log=invalid_log,
        meta=receipt,
        supplemental=(invalid_launcher,),
        sha_keys=("LANE_HEAD", "DETACHED_HEAD"),
        timestamp_keys=("STARTED_AT",),
        exit_key="INVALID_EXIT",
        declared_binary_key=None,
        zero_test_receipt=True,
    )
    return [valid, invalid]


def parse_run_roots(repo: Path, declarations: list[str]) -> list[RunEvidence]:
    if len(declarations) != 3:
        raise EvidenceError(f"expected exactly three --run-root declarations, got {len(declarations)}")
    names: set[str] = set()
    roots_by_layout: dict[str, Path] = {}
    for declaration in declarations:
        if declaration.count("=") != 1:
            raise EvidenceError(
                f"invalid --run-root {declaration!r}; expected name=path"
            )
        name, raw_path = declaration.split("=", 1)
        if not name or name in names:
            raise EvidenceError(f"duplicate or empty --run-root name {name!r}")
        names.add(name)
        root = Path(raw_path).resolve()
        if " " in str(root):
            raise EvidenceError(f"{root}: evidence paths may not contain spaces")
        markers = {
            "aug08": (root / "union.done.killed_attempt_1948").exists(),
            "aug07": any(root.glob("truncated_run_*")),
            "external": any(root.glob("union_invalid_*_receipt.md"))
            or any(root.glob("union_invalid_*.log")),
        }
        matched = [layout for layout, present in markers.items() if present]
        if len(matched) != 1:
            raise EvidenceError(
                f"{root}: unidentifiable or ambiguous evidence layout {matched}"
            )
        layout = matched[0]
        if layout in roots_by_layout:
            raise EvidenceError(f"{root}: duplicate {layout} evidence layout")
        roots_by_layout[layout] = root
    if set(roots_by_layout) != {"aug08", "aug07", "external"}:
        raise EvidenceError(
            f"missing declared evidence layout: found {sorted(roots_by_layout)}"
        )
    runs: list[RunEvidence] = []
    for layout in ("aug08", "aug07", "external"):
        parser = {
            "aug08": parse_aug08,
            "aug07": parse_aug07,
            "external": parse_external,
        }[layout]
        runs.extend(parser(repo, roots_by_layout[layout]))
    identities: dict[str, Path] = {}
    for run in runs:
        previous = identities.get(run.identity)
        if previous is not None:
            raise EvidenceError(
                f"{run.meta}: duplicate run identity {run.identity}; first seen at {previous}"
            )
        identities[run.identity] = run.meta
    return runs


def reconcile(repo: Path, repair_root: Path, run_roots: list[str]) -> str:
    negative, green, repair_line = verify_repair(repo, repair_root)
    runs = parse_run_roots(repo, run_roots)
    valid = [run for run in runs if run.validity == "valid"]
    specimen_passes = sum(run.specimen == "pass" for run in valid)
    specimen_failures = sum(run.specimen == "fail" for run in valid)
    complete_green_runs = sum(run.exit_code == 0 for run in valid)
    lines = [run.machine_line() for run in runs]
    lines.append(repair_line)
    lines.append(
        f"TEST_FLAKE_1_DISPOSITION valid_runs={len(valid)} "
        f"specimen_passes={specimen_passes} specimen_failures={specimen_failures} "
        f"complete_green_runs={complete_green_runs} pre_fix_reproducer=absent "
        f"post_fix_negative_control={negative} repair_green={green} verdict=keep_open"
    )
    return "\n".join(lines) + "\n"


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--repo", required=True)
    parser.add_argument("--repair-root", required=True)
    parser.add_argument("--run-root", action="append", required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    try:
        repo = Path(args.repo).resolve()
        repair_root = Path(args.repair_root).resolve()
        output = Path(args.output).resolve()
        if " " in str(repair_root) or " " in str(output):
            raise EvidenceError("repair and output paths may not contain spaces")
        rendered = reconcile(repo, repair_root, args.run_root)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(rendered, encoding="utf-8")
    except EvidenceError as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
