#!/usr/bin/env bash
set -euo pipefail

ROOT="${1:-$(git rev-parse --show-toplevel)}"

python3 - "$ROOT" <<'PY'
from __future__ import annotations

import json
import re
import sys
from pathlib import Path

root = Path(sys.argv[1])
failures: list[str] = []
counts = {
    "backend": 0,
    "reader": 0,
    "rust_literals": 0,
    "rust_json": 0,
    "cli": 0,
    "raw_json": 0,
    "dashboard": 0,
    "openapi": 0,
    "docs": 0,
}


def read(relative: str) -> str:
    path = root / relative
    try:
        return path.read_text()
    except FileNotFoundError:
        failures.append(f"missing_file {relative}")
        return ""


def require(condition: bool, label: str) -> None:
    if not condition:
        failures.append(label)


def require_contains(relative: str, needle: str, label: str) -> None:
    text = read(relative)
    if needle in text:
        counts[label] += 1
    else:
        failures.append(f"{label}_missing {relative}: {needle}")


mod_rs = read("engine/flapjack-http/src/handlers/migration/mod.rs")
backend_needles = [
    '#[serde(rename = "sourceWriteFrozen", default)]',
    "pub source_write_frozen: bool",
    "TYPESENSE_WRITE_FREEZE_REQUIRED_MESSAGE",
    "if !payload.source_write_frozen",
    "payload.source_write_frozen",
]
for needle in backend_needles:
    if needle in mod_rs:
        counts["backend"] += 1
    else:
        failures.append(f"backend_missing {needle}")

reader_rs = read("engine/flapjack-http/src/handlers/migration/typesense_source_reader.rs")
reader_needles = [
    "source_write_frozen: bool",
    "if !self.source_write_frozen",
    "TYPESENSE_WRITE_FREEZE_REQUIRED_MESSAGE",
    "SourceExportErrorKind::Validation",
]
for needle in reader_needles:
    if needle in reader_rs:
        counts["reader"] += 1
    else:
        failures.append(f"reader_missing {needle}")
require(
    reader_rs.find("if !self.source_write_frozen") < reader_rs.find(".observe_source()")
    if "if !self.source_write_frozen" in reader_rs and ".observe_source()" in reader_rs
    else False,
    "reader_attestation_must_precede_observe_source",
)

literal_roots = [
    root / "engine/flapjack-http/src",
    root / "engine/flapjack-server/src",
    root / "engine/flapjack-server/tests",
    root / "engine/tests",
]
literal_pattern = re.compile(r"MigrateFromTypesenseRequest\s*\{(?P<body>.*?)\n\s*\}", re.S)
for base in literal_roots:
    if not base.exists():
        continue
    for path in base.rglob("*"):
        if path.suffix not in {".rs", ".sh"}:
            continue
        relative = path.relative_to(root)
        if str(relative) == "engine/tests/typesense_write_freeze_caller_census_test.sh":
            continue
        if str(relative) == "engine/flapjack-http/src/handlers/migration/mod.rs":
            continue
        if "target" in path.parts:
            continue
        text = path.read_text(errors="ignore")
        for match in literal_pattern.finditer(text):
            prefix = text[: match.start()].rstrip().split()[-1:]
            if prefix and prefix[0] in {"struct", "impl"}:
                continue
            counts["rust_literals"] += 1
            body = match.group("body")
            if not re.search(r"source_write_frozen\s*:\s*true\b", body):
                failures.append(f"rust_literal_missing_attestation {relative}")


def rust_function_body(text: str, function_name: str) -> str | None:
    signature = re.search(rf"\bfn\s+{re.escape(function_name)}\s*\(", text)
    if not signature:
        return None
    opening_brace = text.find("{", signature.end())
    if opening_brace < 0:
        return None
    depth = 0
    for offset in range(opening_brace, len(text)):
        if text[offset] == "{":
            depth += 1
        elif text[offset] == "}":
            depth -= 1
            if depth == 0:
                return text[opening_brace + 1 : offset]
    return None


rust_json_producers = [
    (
        "engine/flapjack-http/src/handlers/migration/async_status_tests.rs",
        "typesense_submit_payload_with_key",
    ),
]
for relative, function_name in rust_json_producers:
    body = rust_function_body(read(relative), function_name)
    if body is None:
        failures.append(f"rust_json_producer_missing {relative}::{function_name}")
        continue
    counts["rust_json"] += 1
    if not re.search(r'"sourceWriteFrozen"\s*:\s*true\b', body):
        failures.append(f"rust_json_missing_attestation {relative}::{function_name}")

cli_rs = read("engine/flapjack-server/src/migrate.rs")
cli_needles = [
    "--source-write-frozen",
    'map.serialize_entry("sourceWriteFrozen", source_write_frozen)',
    "--source-write-frozen is required for --source-provider typesense",
    "--source-write-frozen is not valid with --source-provider algolia",
    "--source-write-frozen is not valid with --source-provider meilisearch",
]
for needle in cli_needles:
    if needle in cli_rs:
        counts["cli"] += 1
    else:
        failures.append(f"cli_missing {needle}")
cli_tests = read("engine/flapjack-server/tests/migrate_cli_test.rs")
if '"sourceWriteFrozen": true' in cli_tests:
    counts["cli"] += 1
else:
    failures.append("cli_tests_missing_sourceWriteFrozen_true")

raw_probe = read("engine/tests/source_migration_provider_parity_http_probe.sh")
for label, snippet in [
    ("typesense_categories_submit", '"sourceWriteFrozen":true'),
    ("typesense_preview", '"targetIndex":"shop","sourceWriteFrozen":true'),
]:
    if snippet in raw_probe:
        counts["raw_json"] += 1
    else:
        failures.append(f"raw_json_missing {label}")

release_loopback = read("engine/tests/migration_release_loopback_contract.sh")
for label, snippet in [
    (
        "release_typesense_categories_submit",
        '\\"sourceIndex\\":\\"${TYPESENSE_CATEGORIES}\\",\\"sourceWriteFrozen\\":true',
    ),
    (
        "release_typesense_products_submit",
        '\\"sourceIndex\\":\\"${TYPESENSE_PRODUCTS}\\",\\"sourceWriteFrozen\\":true',
    ),
]:
    if snippet in release_loopback:
        counts["raw_json"] += 1
    else:
        failures.append(f"raw_json_missing {label}")

dashboard = read("engine/dashboard/src/pages/migrateHelpers.ts")
dashboard_test = read("engine/dashboard/src/pages/migrateHelpers.test.ts")
for label, text, needle in [
    ("helper", dashboard, "export function buildMigrationRequestBody"),
    ("test", dashboard_test, "buildMigrationRequestBody({"),
]:
    if needle in text:
        counts["dashboard"] += 1
    else:
        failures.append(f"dashboard_missing {label}")
require(
    "if (provider.id === 'typesense') {\n    body.sourceWriteFrozen = true;\n  }" not in dashboard,
    "dashboard_must_not_fabricate_write_freeze_attestation",
)

for relative in [
    "engine/docs2/openapi.json",
    "engine/demo-dualclient/public/openapi.json",
]:
    raw = read(relative)
    if not raw:
        continue
    try:
        doc = json.loads(raw)
    except json.JSONDecodeError as error:
        failures.append(f"openapi_invalid_json {relative}: {error}")
        continue
    schema = doc.get("components", {}).get("schemas", {}).get("MigrateFromTypesenseRequest", {})
    properties = schema.get("properties", {})
    if properties.get("sourceWriteFrozen", {}).get("type") == "boolean":
        counts["openapi"] += 1
    else:
        failures.append(f"openapi_missing_boolean_sourceWriteFrozen {relative}")
    for path in ["/1/migrations/typesense", "/1/migrations/typesense/preview"]:
        ref = (
            doc.get("paths", {})
            .get(path, {})
            .get("post", {})
            .get("requestBody", {})
            .get("content", {})
            .get("application/json", {})
            .get("schema", {})
            .get("$ref")
        )
        if ref != "#/components/schemas/MigrateFromTypesenseRequest":
            failures.append(f"openapi_route_ref_mismatch {relative} {path}")

operations = read("engine/docs2/3_IMPLEMENTATION/OPERATIONS.md")
contract = read("engine/docs2/3_IMPLEMENTATION/2026_07_26_m0b_typesense_source_contract.md")
for label, text, needle in [
    ("operations", operations, "sourceWriteFrozen: true"),
    ("contract", contract, "Require an external write freeze/attestation"),
]:
    if needle in text:
        counts["docs"] += 1
    else:
        failures.append(f"docs_missing {label}")

for key, count in counts.items():
    if count <= 0:
        failures.append(f"denominator_zero {key}")

if failures:
    for failure in failures:
        print(f"CALLER_CENSUS_FAIL {failure}", file=sys.stderr)
    sys.exit(1)

print(
    "CALLER_CENSUS=PASS "
    + " ".join(f"{key}={counts[key]}" for key in sorted(counts))
)
PY
