#!/usr/bin/env python3
"""Keep the flapjack compile-share receipt internally consistent.

The scratch measurement evidence is the source for each receipt, but this test's
job is narrower: prevent receipts from mixing an invalid measurement branch with
a numeric decision ratio, and make future numeric updates recompute the ratio
from the same file.
"""

from __future__ import annotations

import math
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
RECEIPT_DIR = REPO_ROOT / "docs" / "reference"
RECEIPT_GLOB = "compile_share_flapjack_*.md"

LABEL_RE = re.compile(r"^([a-z_]+):\s*(\S+)\s*$", re.MULTILINE)
NUMERIC_RE = re.compile(r"^-?(?:\d+(?:\.\d*)?|\.\d+)$")


def receipt_paths() -> list[Path]:
    paths = sorted(RECEIPT_DIR.glob(RECEIPT_GLOB))
    assert len(paths) >= 2, (
        f"expected at least 2 compile-share receipts matching "
        f"{RECEIPT_DIR.relative_to(REPO_ROOT) / RECEIPT_GLOB}, found {len(paths)}"
    )
    return paths


def receipt_name(path: Path) -> str:
    return str(path.relative_to(REPO_ROOT))


def receipt_labels(path: Path, text: str) -> dict[str, str]:
    labels: dict[str, str] = {}
    counts: dict[str, int] = {}
    for key, value in LABEL_RE.findall(text):
        counts[key] = counts.get(key, 0) + 1
        if counts[key] == 1:
            labels[key] = value
    required = {
        "cold_compile_seconds",
        "test_execution_seconds",
        "warm_compile_seconds",
        "decision_ratio",
        "decision_ratio_rounding_tolerance",
    }
    missing = sorted(required - labels.keys())
    assert not missing, (
        f"{receipt_name(path)} is missing stable label(s): {', '.join(missing)}"
    )
    duplicates = sorted(key for key in required if counts.get(key, 0) > 1)
    assert not duplicates, (
        f"{receipt_name(path)} must define each stable label exactly once; "
        f"duplicates: {', '.join(duplicates)}"
    )
    return labels


def numeric_value(path: Path, label: str, value: str) -> float | None:
    if value in {"INVALID", "UNMEASURED", "NONE"}:
        return None
    assert NUMERIC_RE.match(value), (
        f"{receipt_name(path)}: {label} must be numeric or an accepted sentinel, "
        f"got {value!r}"
    )
    return float(value)


def text_with_label(path: Path, text: str, label: str, replacement: str) -> str:
    labels = receipt_labels(path, text)
    return text.replace(f"{label}: {labels[label]}", f"{label}: {replacement}", 1)


def assert_receipt_keeps_invalid_branch_ratio_free(path: Path, text: str) -> None:
    labels = receipt_labels(path, text)

    ratio_label = f"decision_ratio: {labels['decision_ratio']}"
    duplicate_variants = (
        text.replace(
            ratio_label,
            f"decision_ratio: 0.9\n{ratio_label}",
            1,
        ),
        text.replace(
            ratio_label,
            "decision_ratio: 0.9\ndecision_ratio: 0.5",
            1,
        ),
    )
    for duplicate_text in duplicate_variants:
        try:
            receipt_labels(path, duplicate_text)
        except AssertionError as exc:
            assert "exactly once" in str(exc), receipt_name(path)
        else:
            raise AssertionError(
                f"{receipt_name(path)}: duplicate decision_ratio labels must be "
                "rejected"
            )

    cold = numeric_value(path, "cold_compile_seconds", labels["cold_compile_seconds"])
    test_execution = numeric_value(
        path,
        "test_execution_seconds",
        labels["test_execution_seconds"],
    )
    decision_ratio = numeric_value(path, "decision_ratio", labels["decision_ratio"])

    if cold is None or test_execution is None:
        assert labels["decision_ratio"] == "NONE", (
            f"{receipt_name(path)}: invalid or unmeasured decision inputs require "
            "decision_ratio: NONE"
        )
        assert decision_ratio is None, (
            f"{receipt_name(path)}: invalid or unmeasured decision inputs must not "
            "be paired with a numeric decision_ratio"
        )
        assert "no admissible ratio exists" in text.lower(), receipt_name(path)
        return

    assert decision_ratio is not None, (
        f"{receipt_name(path)}: numeric decision inputs require exactly one "
        "numeric decision_ratio label"
    )
    tolerance = numeric_value(
        path,
        "decision_ratio_rounding_tolerance",
        labels["decision_ratio_rounding_tolerance"],
    )
    assert tolerance is not None and tolerance >= 0, receipt_name(path)
    expected = cold / (cold + test_execution)
    assert math.isclose(decision_ratio, expected, abs_tol=tolerance), (
        f"{receipt_name(path)}: decision_ratio {decision_ratio} does not match "
        f"cold/(cold+test) {expected} within tolerance {tolerance}"
    )
    assert "no admissible ratio exists" not in text.lower(), (
        f"{receipt_name(path)}: numeric decision inputs contradict the invalid-branch "
        "verdict"
    )


def test_compile_share_receipt_keeps_invalid_branch_ratio_free() -> None:
    paths = receipt_paths()
    for path in paths:
        assert_receipt_keeps_invalid_branch_ratio_free(
            path,
            path.read_text(encoding="utf-8"),
        )

    numeric_path = paths[0]
    numeric_text = numeric_path.read_text(encoding="utf-8")
    for label, replacement in (
        ("cold_compile_seconds", "3"),
        ("test_execution_seconds", "1"),
        ("decision_ratio", "0.75"),
    ):
        numeric_text = text_with_label(numeric_path, numeric_text, label, replacement)
    numeric_text = numeric_text.replace(
        "No admissible ratio exists",
        "An admissible ratio exists",
        1,
    )
    assert_receipt_keeps_invalid_branch_ratio_free(numeric_path, numeric_text)

    contradictory_verdict_text = numeric_text.replace(
        "An admissible ratio exists",
        "No admissible ratio exists",
        1,
    )
    try:
        assert_receipt_keeps_invalid_branch_ratio_free(
            numeric_path,
            contradictory_verdict_text,
        )
    except AssertionError as exc:
        assert "contradict the invalid-branch verdict" in str(exc), receipt_name(
            numeric_path
        )
    else:
        raise AssertionError(
            f"{receipt_name(numeric_path)}: numeric decision inputs must not retain "
            "the invalid-branch verdict"
        )

    wrong_ratio_text = text_with_label(
        numeric_path,
        numeric_text,
        "decision_ratio",
        "0.5",
    )
    try:
        assert_receipt_keeps_invalid_branch_ratio_free(numeric_path, wrong_ratio_text)
    except AssertionError as exc:
        assert "does not match cold/(cold+test)" in str(exc), receipt_name(numeric_path)
    else:
        raise AssertionError(
            f"{receipt_name(numeric_path)}: numeric decision_ratio must be recomputed "
            "from cold and test inputs"
        )


def test_compile_share_receipt_rejects_invalid_branch_ratio_sentinels() -> None:
    for path in receipt_paths():
        text = path.read_text(encoding="utf-8")
        labels = receipt_labels(path, text)
        cold = numeric_value(
            path, "cold_compile_seconds", labels["cold_compile_seconds"]
        )
        test_execution = numeric_value(
            path,
            "test_execution_seconds",
            labels["test_execution_seconds"],
        )
        if cold is not None and test_execution is not None:
            continue

        for wrong_ratio in ("INVALID", "UNMEASURED"):
            try:
                assert_receipt_keeps_invalid_branch_ratio_free(
                    path,
                    text_with_label(path, text, "decision_ratio", wrong_ratio),
                )
            except AssertionError as exc:
                assert "decision_ratio: NONE" in str(exc), receipt_name(path)
            else:
                raise AssertionError(
                    f"{receipt_name(path)}: invalid or unmeasured decision inputs "
                    f"must reject decision_ratio: {wrong_ratio}"
                )


def test_compile_share_receipt_records_warm_control_label() -> None:
    for path in receipt_paths():
        labels = receipt_labels(path, path.read_text(encoding="utf-8"))
        cold = numeric_value(
            path, "cold_compile_seconds", labels["cold_compile_seconds"]
        )
        test_execution = numeric_value(
            path,
            "test_execution_seconds",
            labels["test_execution_seconds"],
        )
        warm = numeric_value(
            path, "warm_compile_seconds", labels["warm_compile_seconds"]
        )
        decision_ratio = numeric_value(path, "decision_ratio", labels["decision_ratio"])

        if cold is not None and test_execution is not None:
            assert warm is not None, (
                f"{receipt_name(path)}: numeric cold and test inputs require a numeric "
                "warm_compile_seconds control"
            )
            continue

        assert decision_ratio is None, (
            f"{receipt_name(path)}: warm_compile_seconds may be non-numeric only on "
            "the decision_ratio: NONE branch"
        )
