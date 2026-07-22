#!/usr/bin/env bash
# Live contract for the shipped repair-publication CLI against generated crash layouts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"

BINARY_ARG=""
MANIFEST_ARG=""
ARTIFACT_ARG=""
BINARY_PATH=""
MANIFEST_PATH=""
ARTIFACT_DIR=""
TIMEOUT_BIN=""
HELPER=""
CHILD_PID=""
CHILD_TARGET_MODE=""
FLAPJACK_ENV_ARGS=()
SERVER_BIND_ADDR=""
SERVER_STDOUT_PATH=""
SERVER_STDERR_PATH=""
LAST_CHILD_PID=""
LAST_SERVER_BIND_ADDR=""
LAST_SERVER_STDOUT_PATH=""
LAST_SERVER_STDERR_PATH=""
CURRENT_CASE_ID=""
FAILURE_PHASE="argument_validation"
FAILURE_REASON=""
EVIDENCE_READY=0

usage() {
  cat <<'EOF'
Usage:
  publication_repair_cli_live.sh --binary <absolute-path> --manifest <absolute-path> --artifact-dir <absolute-temp>

Options:
  --binary        Absolute path to the release flapjack executable to test.
  --manifest      Absolute path to publication_repair_cli_scenarios.json.
  --artifact-dir  Existing empty absolute artifact directory outside the checkout.
  --help          Show this help text.
EOF
}

die() {
  FAILURE_REASON="$*"
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

append_cleanup_trace() {
  [ "$EVIDENCE_READY" -eq 1 ] || return 0
  python3 "$HELPER" append-cleanup-trace \
    "$ARTIFACT_DIR/.runner/cleanup_trace.jsonl" "$@"
}

evidence_command() {
  local command="$1"
  local original_status="$2"
  local final_status="$3"
  local cleanup_outcome="$4"
  python3 "$HELPER" "$command" \
    "$ARTIFACT_DIR" "$MANIFEST_PATH" "$REPO_DIR" \
    "$original_status" "$final_status" "$FAILURE_PHASE" "$FAILURE_REASON" \
    "$LAST_CHILD_PID" "$LAST_SERVER_BIND_ADDR" \
    "$LAST_SERVER_STDOUT_PATH" "$LAST_SERVER_STDERR_PATH" \
    "$CURRENT_CASE_ID" "$cleanup_outcome"
}

cleanup() {
  local original_status=$?
  local final_status="$original_status"
  local cleanup_failed=0
  trap - EXIT
  if [ "$EVIDENCE_READY" -eq 1 ]; then
    append_cleanup_trace cleanup_started "${CHILD_PID:-}" "" "" "" "" started || cleanup_failed=1
    evidence_command snapshot-evidence "$original_status" "$final_status" in_progress || cleanup_failed=1
  fi
  stop_server || cleanup_failed=1
  if [ "$original_status" -eq 0 ] && [ "$cleanup_failed" -ne 0 ]; then
    final_status=1
    FAILURE_PHASE="cleanup"
  fi
  if [ "$EVIDENCE_READY" -eq 1 ] && [ -f "$HELPER" ]; then
    append_cleanup_trace cleanup_finished "" "" "" "" "" \
      "$(if [ "$cleanup_failed" -eq 0 ]; then printf succeeded; else printf failed; fi)" || cleanup_failed=1
    if ! evidence_command finalize-evidence "$original_status" "$final_status" \
      "$(if [ "$cleanup_failed" -eq 0 ]; then printf succeeded; else printf failed; fi)"; then
      cleanup_failed=1
      if [ "$original_status" -eq 0 ]; then
        final_status=1
      fi
    fi
  fi
  exit "$final_status"
}
trap cleanup EXIT

handle_signal() {
  local signal_name="$1"
  local signal_number="$2"
  FAILURE_PHASE="signal"
  FAILURE_REASON="runner received $signal_name"
  exit $((128 + signal_number))
}
trap 'handle_signal INT 2' INT
trap 'handle_signal TERM 15' TERM
trap 'handle_signal HUP 1' HUP

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --binary)
        [ -z "$BINARY_ARG" ] || die "--binary may only be provided once"
        [ "$#" -ge 2 ] || die "--binary requires a value"
        BINARY_ARG="$2"
        shift 2
        ;;
      --manifest)
        [ -z "$MANIFEST_ARG" ] || die "--manifest may only be provided once"
        [ "$#" -ge 2 ] || die "--manifest requires a value"
        MANIFEST_ARG="$2"
        shift 2
        ;;
      --artifact-dir)
        [ -z "$ARTIFACT_ARG" ] || die "--artifact-dir may only be provided once"
        [ "$#" -ge 2 ] || die "--artifact-dir requires a value"
        ARTIFACT_ARG="$2"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        usage >&2
        die "unknown argument: $1"
        ;;
    esac
  done
  [ -n "$BINARY_ARG" ] || die "--binary is required"
  [ -n "$MANIFEST_ARG" ] || die "--manifest is required"
  [ -n "$ARTIFACT_ARG" ] || die "--artifact-dir is required"
}

require_tools() {
  local missing=0
  local tool
  for tool in bash cargo git python3 mktemp; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v gtimeout)"
  elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v timeout)"
  else
    printf 'ERROR: required tool not found: timeout or gtimeout\n' >&2
    missing=1
  fi
  [ "$missing" -eq 0 ] || exit 1
}

canonical_path() {
  python3 - "$1" <<'PY'
import pathlib
import sys
print(pathlib.Path(sys.argv[1]).resolve(strict=True))
PY
}

validate_paths() {
  [[ "$BINARY_ARG" = /* ]] || die "binary must be an absolute path"
  [[ "$MANIFEST_ARG" = /* ]] || die "manifest must be an absolute path"
  [[ "$ARTIFACT_ARG" = /* ]] || die "artifact directory must be an absolute path"

  BINARY_PATH="$(canonical_path "$BINARY_ARG")"
  MANIFEST_PATH="$(canonical_path "$MANIFEST_ARG")"
  ARTIFACT_DIR="$(canonical_path "$ARTIFACT_ARG")"

  [ -f "$BINARY_PATH" ] || die "binary must be a regular file: $BINARY_ARG"
  [ -x "$BINARY_PATH" ] || die "binary must be executable: $BINARY_ARG"
  [ -f "$MANIFEST_PATH" ] || die "manifest must be a regular file: $MANIFEST_ARG"
  [ -d "$ARTIFACT_DIR" ] || die "artifact directory must exist: $ARTIFACT_ARG"

  python3 - "$REPO_DIR" "$ARTIFACT_DIR" "$MANIFEST_PATH" <<'PY'
import json
import os
import pathlib
import re
import sys

repo = pathlib.Path(sys.argv[1]).resolve()
artifact = pathlib.Path(sys.argv[2]).resolve()
manifest_path = pathlib.Path(sys.argv[3])
home = pathlib.Path.home().resolve()
tmp = pathlib.Path("/tmp").resolve()
safe_path_component = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")

def fail(message):
    raise SystemExit(message)

if artifact in (pathlib.Path("/").resolve(), home, tmp):
    fail(f"artifact directory is not an allowed root: {artifact}")
if os.path.commonpath([repo, artifact]) == str(repo):
    fail(f"artifact directory must be outside the checkout: {artifact}")
if any(artifact.iterdir()):
    fail(f"artifact directory must be empty: {artifact}")

with manifest_path.open(encoding="utf-8") as handle:
    manifest = json.load(handle)
scenarios = manifest.get("scenarios")
if not isinstance(scenarios, list):
    fail("manifest scenarios must be a list")
for index, scenario in enumerate(scenarios):
    if not isinstance(scenario, dict):
        fail(f"manifest scenario {index} must be an object")
    path_fields = {
        "id": scenario.get("id"),
        "tenant": scenario.get("tenant") or "products",
        "transaction": scenario.get("transaction") or "txn_001",
    }
    for field, value in path_fields.items():
        if not isinstance(value, str) or not safe_path_component.fullmatch(value):
            fail(
                f"manifest scenario {index} {field} must be a non-empty safe path component"
            )
PY
}

validate_test_invoke_mode() {
  local mode="${PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST:-}"
  local harness_pid="${PUBLICATION_REPAIR_CLI_TEST_HARNESS_PID:-}"
  local harness_command="$(ps -o command= -p "$PPID" 2>/dev/null || true)"
  case "$mode" in
    ""|skip_first_repair) ;;
    *) die "unknown PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST: $mode" ;;
  esac
  if [ -n "$mode" ] && [ "${PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST:-0}" != "1" ]; then
    die "PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST requires PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST=1"
  fi
  if [ -n "$mode" ] && { [ "$harness_pid" != "$PPID" ] || ! [[ "$harness_pid" =~ ^[0-9]+$ ]] || [[ "$harness_command" != *publication_repair_cli_live_test.sh* ]]; }; then
    die "PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST is restricted to publication_repair_cli_live_test.sh"
  fi
}

collect_flapjack_env() {
  local name
  FLAPJACK_ENV_ARGS=()
  while IFS='=' read -r name _; do
    case "$name" in
      FLAPJACK_*) FLAPJACK_ENV_ARGS+=("-u" "$name") ;;
    esac
  done < <(env)
}

run_bounded() {
  local status=0
  local restore_errexit=0
  [[ "$-" == *e* ]] && restore_errexit=1
  set +e
  "$TIMEOUT_BIN" --kill-after=30s "$@" &
  CHILD_PID=$!
  CHILD_TARGET_MODE="direct"
  wait "$CHILD_PID"
  status=$?
  CHILD_PID=""
  CHILD_TARGET_MODE=""
  [ "$restore_errexit" -eq 0 ] || set -e
  return "$status"
}

write_helper() {
  mkdir -p "$ARTIFACT_DIR/.runner"
  HELPER="$ARTIFACT_DIR/.runner/publication_repair_cli_live_helper.py"
  cat >"$HELPER" <<'PY'
import filecmp
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request

REPORT_KEYS = {"tenant", "status", "action", "transaction_id", "phase", "evidence"}
RESIDUE_FIELDS = {
    "staging": lambda case, tenant, txn: case / ".publication" / tenant / txn / "staging",
    "backup": lambda case, tenant, txn: case / ".publication" / tenant / txn / "backup",
    "journal": lambda case, tenant, txn: case / ".publication" / tenant / txn / "journal.json",
    "quarantine": lambda case, tenant, txn: case / ".publication_quarantine" / tenant / txn,
}
HEALTH_KEYS = {
    "status", "version", "build", "uptime_secs", "capabilities",
    "active_writers", "max_concurrent_writers", "facet_cache_entries",
    "facet_cache_cap", "heap_allocated_mb", "system_limit_mb",
    "pressure_level", "allocator", "tenants_loaded",
}
SEARCH_REQUIRED_KEYS = {
    "hits", "nbHits", "page", "nbPages", "hitsPerPage",
    "processingTimeMS", "serverTimeMS", "query", "params",
    "exhaustive", "exhaustiveNbHits", "exhaustiveTypo", "index",
    "renderingContent", "serverUsed", "_automaticInsights",
    "processingTimingsMS",
}
SEARCH_OPTIONAL_KEYS = {
    "queryAfterRemoval", "parsedQuery", "nbSortedHits",
    "appliedRelevancyStrictness", "exhaustiveFacetsCount", "facets",
    "facets_stats", "userData", "automaticRadius", "appliedRules",
    "queryID", "message", "abTestID", "abTestVariantID",
    "interleavedTeams", "indexUsed",
}
FORBIDDEN_PROJECTION_TOKENS = (
    ".publication",
    ".publication_quarantine",
    "publication_",
    "staging",
    "backup",
    "journal",
    "generated_layouts.json",
    "publication_repair_cli_scenarios",
)
SEARCH_HIT_METADATA_KEYS = {"_highlightResult", "_snippetResult", "_rankingInfo"}

def load_json(path):
    with pathlib.Path(path).open(encoding="utf-8") as handle:
        return json.load(handle)

def fail(message):
    raise SystemExit(message)

def http_json(bind_addr, method, path, expected_status):
    url = f"http://{bind_addr}{path}"
    request = urllib.request.Request(url, method=method)
    try:
        with urllib.request.urlopen(request, timeout=0.5) as response:
            status = response.status
            raw = response.read().decode("utf-8")
    except urllib.error.HTTPError as error:
        status = error.code
        raw = error.read().decode("utf-8")
    except Exception as error:
        fail(f"{path} request failed: {error}")
    if status != expected_status:
        fail(f"{path} status {status} does not match {expected_status}")
    try:
        payload = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"{path} response is not valid JSON: {error}")
    return payload

def assert_no_projection_token(value, path="$"):
    if isinstance(value, dict):
        for key, child in value.items():
            assert_no_projection_token(key, f"{path}.{key}")
            assert_no_projection_token(child, f"{path}.{key}")
        return
    if isinstance(value, list):
        for index, child in enumerate(value):
            assert_no_projection_token(child, f"{path}[{index}]")
        return
    if isinstance(value, str):
        for token in FORBIDDEN_PROJECTION_TOKENS:
            if token in value:
                fail(f"user projection leaked forbidden token {token} at {path}")

def assert_exact_object(actual, expected, ident, index):
    assert_no_projection_token(actual)
    if actual != expected:
        fail(f"{ident} {index} object body mismatch")

def is_nonnegative_int(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0

def assert_search_timing_payload(payload, ident, index):
    for key in ["processingTimeMS", "serverTimeMS"]:
        if not is_nonnegative_int(payload[key]):
            fail(f"{ident} {index} {key} must be a non-negative integer")
    timings = payload["processingTimingsMS"]
    if not isinstance(timings, dict):
        fail(f"{ident} {index} processingTimingsMS must be an object")
    if set(timings) != {"queue", "search", "highlight", "total"}:
        fail(f"{ident} {index} processingTimingsMS keys mismatch: {sorted(timings)}")
    for key in ["queue", "search", "highlight", "total"]:
        if not is_nonnegative_int(timings[key]):
            fail(f"{ident} {index} processingTimingsMS.{key} must be a non-negative integer")
    if timings["total"] < timings["queue"]:
        fail(f"{ident} {index} processingTimingsMS total must include queue time")

def assert_search_exhaustive_payload(payload, ident, index):
    exhaustive = payload["exhaustive"]
    if not isinstance(exhaustive, dict):
        fail(f"{ident} {index} exhaustive must be an object")
    allowed = {"nbHits", "typo", "facetValues", "rulesMatch", "facetsCount"}
    if set(exhaustive) - allowed:
        fail(f"{ident} {index} exhaustive has unexpected keys: {sorted(exhaustive)}")
    required = {"nbHits", "typo", "facetValues", "rulesMatch"}
    if not required.issubset(exhaustive):
        fail(f"{ident} {index} exhaustive missing required keys: {sorted(required - set(exhaustive))}")
    for key, value in exhaustive.items():
        if not isinstance(value, bool):
            fail(f"{ident} {index} exhaustive.{key} must be boolean")
    for key in ["exhaustiveNbHits", "exhaustiveTypo", "_automaticInsights"]:
        if not isinstance(payload[key], bool):
            fail(f"{ident} {index} {key} must be boolean")

def assert_search_optional_payload(payload, ident, index):
    for key in ["queryAfterRemoval", "parsedQuery", "automaticRadius", "queryID", "message", "abTestID", "abTestVariantID", "indexUsed"]:
        if key in payload and not isinstance(payload[key], str):
            fail(f"{ident} {index} {key} must be a string when present")
    for key in ["nbSortedHits", "appliedRelevancyStrictness"]:
        if key in payload and not is_nonnegative_int(payload[key]):
            fail(f"{ident} {index} {key} must be a non-negative integer when present")
    if "exhaustiveFacetsCount" in payload and not isinstance(payload["exhaustiveFacetsCount"], bool):
        fail(f"{ident} {index} exhaustiveFacetsCount must be boolean when present")
    for key in ["facets", "facets_stats", "interleavedTeams"]:
        if key in payload and not isinstance(payload[key], dict):
            fail(f"{ident} {index} {key} must be an object when present")
    if "appliedRules" in payload and not isinstance(payload["appliedRules"], list):
        fail(f"{ident} {index} appliedRules must be a list when present")

HIGHLIGHT_TAGS = ("<em>", "</em>")

def unhighlighted_text(value):
    """Recover a text leaf's source by removing the tags the server wraps matches in."""
    for tag in HIGHLIGHT_TAGS:
        value = value.replace(tag, "")
    return value

class HitHighlightOracle:
    """The manifest's production `_highlightResult` leaf strings for one search hit.

    The manifest owns these and Rust validates them against the real highlighter, so
    this helper reads them rather than re-deriving them: Rust's `f64` Display keeps
    negative zero signed, never uses exponent notation, and prints the shortest
    round-tripping decimal, none of which Python's `str` reproduces.
    """

    def __init__(self, object_fixture, ident, index, position):
        self.expected_highlight = object_fixture["expected_highlight"]
        self.body = object_fixture["body"]
        self.ident = ident
        self.index = index
        self.position = position

    def label(self, path):
        return f"{self.ident} {self.index} hit {self.position}._highlightResult.{path}"

    def expected_value(self, path):
        if path not in self.expected_highlight:
            fail(f"{self.label(path)} has no manifest-owned expected highlight value")
        return self.expected_highlight[path]

def assert_leaf_highlight_entry(value, expected_source, oracle, path):
    label = oracle.label(path)
    if not isinstance(value, dict):
        fail(f"{label} highlight value must be an object")
    allowed = {"value", "matchLevel", "matchedWords", "fullyHighlighted"}
    if set(value) - allowed:
        fail(f"{label} highlight has unexpected keys: {sorted(value)}")
    if not {"value", "matchLevel", "matchedWords"}.issubset(value):
        fail(f"{label} highlight missing leaf keys")
    if not isinstance(value["value"], str):
        fail(f"{label}.value must be a string")
    # Only text leaves carry query-dependent markup, so compare those with the tags
    # removed; every other leaf is rendered verbatim and must match the oracle exactly.
    observed_value = (
        unhighlighted_text(value["value"])
        if isinstance(expected_source, str)
        else value["value"]
    )
    expected_value = oracle.expected_value(path)
    if observed_value != expected_value:
        fail(f"{label}.value mismatch: {observed_value!r} != {expected_value!r}")
    if value["matchLevel"] not in {"none", "partial", "full"}:
        fail(f"{label}.matchLevel is invalid")
    if (
        not isinstance(value["matchedWords"], list)
        or not all(isinstance(word, str) for word in value["matchedWords"])
    ):
        fail(f"{label}.matchedWords must be a string list")
    if "fullyHighlighted" in value and not isinstance(value["fullyHighlighted"], bool):
        fail(f"{label}.fullyHighlighted must be boolean")

def assert_highlight_entry(value, expected_source, oracle, path):
    # The production highlighter recurses into objects but renders array items as
    # leaves, so an array never nests further regardless of what its items hold.
    if isinstance(expected_source, list):
        if not isinstance(value, list):
            fail(f"{oracle.label(path)} highlight value must be a list")
        if len(value) != len(expected_source):
            fail(f"{oracle.label(path)} highlight list length mismatch")
        for position, (child, expected_child) in enumerate(zip(value, expected_source)):
            assert_leaf_highlight_entry(child, expected_child, oracle, f"{path}[{position}]")
        return
    if isinstance(expected_source, dict):
        if not isinstance(value, dict):
            fail(f"{oracle.label(path)} highlight value must be an object")
        expected_keys = set(expected_source)
        observed_keys = set(value)
        if observed_keys != expected_keys:
            fail(f"{oracle.label(path)} highlight object keys mismatch: {sorted(observed_keys)} != {sorted(expected_keys)}")
        for key, child in value.items():
            assert_highlight_entry(child, expected_source[key], oracle, f"{path}.{key}")
        return
    assert_leaf_highlight_entry(value, expected_source, oracle, path)

def assert_highlight_result(hit, oracle):
    highlight = hit.get("_highlightResult")
    if not isinstance(highlight, dict):
        fail(
            f"{oracle.ident} {oracle.index} hit {oracle.position} "
            "missing required hit metadata _highlightResult"
        )
    # Every non-objectID manifest body field must carry a highlight entry. A field the
    # document model discards has no oracle value, so a body declaring one fails closed
    # here rather than silently narrowing what the projection checks.
    expected_fields = {key for key in oracle.body if key != "objectID"}
    observed_fields = set(highlight)
    if expected_fields - observed_fields:
        fail(
            f"{oracle.ident} {oracle.index} hit {oracle.position} _highlightResult missing keys: "
            f"{sorted(expected_fields - observed_fields)}"
        )
    if observed_fields - expected_fields:
        fail(
            f"{oracle.ident} {oracle.index} hit {oracle.position} "
            f"_highlightResult has unexpected keys: {sorted(highlight)}"
        )
    for key, value in highlight.items():
        assert_highlight_entry(value, oracle.body[key], oracle, key)

def assert_search_hit(hit, object_fixture, ident, index, position):
    if not isinstance(hit, dict):
        fail(f"{ident} {index} search hit must be an object")
    assert_no_projection_token(hit)
    metadata_keys = {key for key in hit if key.startswith("_")}
    if metadata_keys - SEARCH_HIT_METADATA_KEYS:
        fail(f"{ident} {index} hit {position} has unexpected metadata keys: {sorted(metadata_keys)}")
    assert_highlight_result(hit, HitHighlightOracle(object_fixture, ident, index, position))
    body = {key: value for key, value in hit.items() if key not in SEARCH_HIT_METADATA_KEYS}
    if body != object_fixture["body"]:
        fail(f"{ident} {index} hit {position} body mismatch")
    return body.get("objectID")

def assert_search_payload(payload, expected_ids, expected_objects, query, ident, index):
    keys = set(payload)
    if not SEARCH_REQUIRED_KEYS.issubset(keys):
        fail(f"{ident} {index} search missing keys: {sorted(SEARCH_REQUIRED_KEYS - keys)}")
    if keys - SEARCH_REQUIRED_KEYS - SEARCH_OPTIONAL_KEYS:
        fail(f"{ident} {index} search has unexpected keys: {sorted(keys - SEARCH_REQUIRED_KEYS - SEARCH_OPTIONAL_KEYS)}")
    hits = payload["hits"]
    if not isinstance(hits, list):
        fail(f"{ident} {index} search hits must be a list")
    if len(hits) != len(expected_objects):
        fail(f"{ident} {index} search hit count mismatch")
    observed_ids = []
    for position, (hit, expected_object) in enumerate(zip(hits, expected_objects)):
        observed_ids.append(assert_search_hit(hit, expected_object, ident, index, position))
    if observed_ids != expected_ids:
        fail(f"{ident} {index} ordered hit IDs {observed_ids} do not match {expected_ids}")
    if payload["nbHits"] != len(expected_ids):
        fail(f"{ident} {index} nbHits mismatch")
    if payload["page"] != 0 or payload["hitsPerPage"] < len(expected_ids):
        fail(f"{ident} {index} pagination values mismatch")
    if payload["nbPages"] != (1 if expected_ids else 0):
        fail(f"{ident} {index} nbPages mismatch")
    assert_search_timing_payload(payload, ident, index)
    assert_search_exhaustive_payload(payload, ident, index)
    assert_search_optional_payload(payload, ident, index)
    if payload["query"] != query:
        fail(f"{ident} {index} search query mismatch")
    if not isinstance(payload["params"], str):
        fail(f"{ident} {index} params must be a string")
    if payload["index"] != index:
        fail(f"{ident} {index} search index mismatch")
    if not isinstance(payload["renderingContent"], dict):
        fail(f"{ident} {index} renderingContent must be an object")
    if not isinstance(payload["serverUsed"], str):
        fail(f"{ident} {index} serverUsed must be a string")

def assert_error_payload(payload, expected, ident, surface):
    if payload != expected["body"]:
        fail(f"{ident} {surface} error body mismatch")

def assert_health_payload(payload, expected_build):
    if set(payload) != HEALTH_KEYS:
        fail(f"/health keys mismatch: {sorted(payload)}")
    if payload["status"] != "ok":
        fail(f"/health status mismatch: {payload}")
    if not isinstance(expected_build, dict):
        fail("/health expected build-info must be an object")
    if payload["version"] != expected_build.get("version"):
        fail("/health version mismatch")
    if payload["build"] != expected_build:
        fail("/health build mismatch")
    if payload["capabilities"] != expected_build.get("capabilities"):
        fail("/health capabilities mismatch")
    for key in [
        "uptime_secs", "active_writers", "max_concurrent_writers",
        "facet_cache_entries", "facet_cache_cap", "heap_allocated_mb",
        "system_limit_mb", "tenants_loaded",
    ]:
        if not isinstance(payload[key], int) or payload[key] < 0:
            fail(f"/health {key} must be a non-negative integer")
    if not isinstance(payload["pressure_level"], str) or not payload["pressure_level"]:
        fail("/health pressure_level must be a non-empty string")
    if not isinstance(payload["allocator"], str) or not payload["allocator"]:
        fail("/health allocator must be a non-empty string")

def assert_ready_payload(payload):
    if payload != {"ready": True}:
        fail(f"/health/ready payload mismatch: {payload}")

def scenario_id(scenario):
    return scenario["id"]

def target_for(layout, scenario):
    return layout.get("tenant") or scenario.get("tenant") or "products"

def transaction_for(layout, scenario):
    return layout.get("transaction") or scenario.get("transaction") or "txn_001"

def canonical_journal_missing_for_report(layout):
    return any(
        "rename:.publication/products/txn_001/journal.json.tmp->.publication/products/txn_001/journal.json|1" in boundary
        for boundary in layout.get("boundaries", [])
    )

def manifest_by_id(manifest):
    scenarios = manifest.get("scenarios")
    if not isinstance(scenarios, list):
        fail("manifest scenarios must be a list")
    by_id = {}
    for scenario in scenarios:
        ident = scenario_id(scenario)
        if ident in by_id:
            fail(f"duplicate manifest scenario id {ident}")
        by_id[ident] = scenario
    return by_id

def compare_layout_to_manifest(layout, scenario):
    ident = layout["scenario_id"]
    copied_fields = [
        "kind", "activation", "base", "mutation", "tenant", "transaction",
        "journal_phase", "policy_keys", "digests", "sidecars", "disposition",
        "cli", "visible", "residue",
    ]
    for field in copied_fields:
        expected = scenario.get(field, [] if field == "policy_keys" else None)
        if layout.get(field) != expected:
            fail(f"layout {ident} {field} mismatch")
    expected_boundary = scenario.get("boundary", {}).get("identity")
    if expected_boundary and expected_boundary not in layout.get("boundaries", []):
        fail(f"layout {ident} missing manifest boundary {expected_boundary}")

def validate_generated(manifest_path, generated_path, generated_root):
    manifest = load_json(manifest_path)
    generated = load_json(generated_path)
    root = pathlib.Path(generated_root)
    if manifest.get("schema_version") != 1:
        fail("manifest schema_version must be 1")
    by_id = manifest_by_id(manifest)
    if len(by_id) != manifest.get("layout_count"):
        fail(f"manifest layout_count {manifest.get('layout_count')} does not match scenario count {len(by_id)}")
    if not isinstance(generated, list):
        fail("generated layouts must be a list")
    if len(generated) != manifest.get("layout_count"):
        fail(f"generated layout count {len(generated)} does not match manifest layout_count {manifest.get('layout_count')}")

    generated_ids = set()
    boundary_counts = {}
    for layout in generated:
        ident = layout.get("scenario_id")
        if ident in generated_ids:
            fail(f"duplicate generated scenario id {ident}")
        generated_ids.add(ident)
        if ident not in by_id:
            fail(f"unknown generated scenario id {ident}")
        compare_layout_to_manifest(layout, by_id[ident])
        for boundary in layout.get("boundaries", []):
            boundary_counts[boundary] = boundary_counts.get(boundary, 0) + 1
    if generated_ids != set(by_id):
        fail(f"generated scenario IDs do not match manifest IDs")
    for scenario in by_id.values():
        boundary = scenario.get("boundary", {}).get("identity")
        if boundary and boundary_counts.get(boundary) != 1:
            fail(f"{scenario_id(scenario)} boundary {boundary} observed {boundary_counts.get(boundary, 0)} times")
    for ident in by_id:
        if not (root / ident).is_dir():
            fail(f"missing generated scenario directory {ident}")
    for child in root.iterdir():
        if child.name in {"generated_layouts.json", ".runner"}:
            continue
        if child.is_dir() and child.name not in by_id:
            fail(f"unknown generated scenario directory {child.name}")
    print("\n".join(sorted(by_id)))

def clone_case(generated_root, repair_root, ident):
    src = pathlib.Path(generated_root) / ident
    dst = pathlib.Path(repair_root) / ident
    if not src.is_dir():
        fail(f"missing generated scenario directory {ident}")
    if dst.exists():
        fail(f"duplicate repair scenario directory {ident}")
    shutil.copytree(src, dst, symlinks=True)

def target(manifest_path, generated_path, ident):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    print(target_for(generated[ident], scenario))

def digest_records(root, include_root):
    root = pathlib.Path(root)
    records = []
    if include_root:
        paths = [root, *root.rglob("*")]
    else:
        paths = list(root.rglob("*"))
    for path in paths:
        rel = path.relative_to(root).as_posix()
        if rel == ".":
            key = "."
        else:
            key = rel
        st = path.lstat()
        if path.is_symlink():
            fail(f"refusing symlink publication artifact {path}")
        if path.is_dir():
            records.append((key, b"d", b""))
        elif path.is_file():
            records.append((key, b"f", path.read_bytes()))
        else:
            fail(f"unsupported publication artifact {path}")
    return sorted(records, key=lambda item: item[0])

def framed_digest(records):
    h = hashlib.sha256()
    for rel, entry_type, data in records:
        rel_bytes = rel.encode("utf-8")
        h.update(len(rel_bytes).to_bytes(8, "big"))
        h.update(rel_bytes)
        h.update(entry_type)
        h.update(len(data).to_bytes(8, "big"))
        h.update(data)
    return "sha256:" + h.hexdigest()

def canonical_tenant_tree_digest(path):
    return framed_digest(digest_records(path, include_root=False))

def artifact_digest(path):
    return framed_digest(digest_records(path, include_root=True))

def managed_path_records(case, path):
    case = pathlib.Path(case)
    path = pathlib.Path(path)
    if not path.exists():
        return [{
            "path": path.relative_to(case).as_posix(),
            "kind": "absent",
        }]
    records = []
    paths = [path]
    if path.is_dir():
        paths.extend(path.rglob("*"))
    for child in sorted(paths):
        rel = child.relative_to(case).as_posix()
        if child.is_symlink():
            fail(f"refusing symlink publication artifact {child}")
        if child.is_dir():
            records.append({"path": rel, "kind": "dir"})
        elif child.is_file():
            records.append({
                "path": rel,
                "kind": "file",
                "digest": "sha256:" + hashlib.sha256(child.read_bytes()).hexdigest(),
            })
        else:
            fail(f"unsupported publication artifact {child}")
    return records

def add_managed_record(records, case, label, paths):
    records[label] = [
        item
        for path in paths
        for item in managed_path_records(case, path)
    ]

def managed_snapshot(manifest_path, generated_path, case_root, ident):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    layout = generated[ident]
    case = pathlib.Path(case_root)
    tenant = target_for(layout, scenario)
    txn = transaction_for(layout, scenario)
    records = {}

    add_managed_record(records, case, "target", [case / tenant])
    for field, resolver in RESIDUE_FIELDS.items():
        add_managed_record(records, case, f"residue {field}", [resolver(case, tenant, txn)])
    for name, values in scenario["sidecars"].items():
        for field in ["target", "staging", "backup"]:
            add_managed_record(
                records,
                case,
                f"sidecar {name}.{field}",
                sidecar_paths(case, tenant, txn, name, field),
            )
    print(json.dumps(records, sort_keys=True, separators=(",", ":")))

def assert_equal_managed_snapshot(left_path, right_path, ident):
    left = load_json(left_path)
    right = load_json(right_path)
    if left == right:
        return
    labels = sorted(set(left) | set(right))
    for label in labels:
        if left.get(label) != right.get(label):
            fail(f"{ident} managed {label} changed after second repair")
    fail(f"{ident} managed publication snapshot changed after second repair")

def assert_digest_or_absent(ident, label, path, expected, digest_fn):
    path = pathlib.Path(path)
    if expected == "absent":
        if path.exists():
            fail(f"{ident} {label} digest expected absent but path exists")
        return
    if not path.exists():
        fail(f"{ident} {label} digest expected {expected} but path is absent")
    observed = digest_fn(path)
    if observed != expected:
        fail(f"{ident} {label} digest {observed} does not match manifest {expected}")

def sidecar_paths(case, tenant, txn, name, field):
    staging_key = f"publication_{txn}"
    if name == "query_suggestions":
        root = case / ".query_suggestions"
        names = {
            "target": [f"{tenant}.json", f"{tenant}.log.jsonl", f"{tenant}.status.json"],
            "staging": [f"{staging_key}.json", f"{staging_key}.log.jsonl", f"{staging_key}.status.json"],
            "backup": [
                f".publication/{tenant}/{txn}/sidecars/query_suggestions/{tenant}.json",
                f".publication/{tenant}/{txn}/sidecars/query_suggestions/{tenant}.log.jsonl",
                f".publication/{tenant}/{txn}/sidecars/query_suggestions/{tenant}.status.json",
            ],
        }[field]
        return [root / name for name in names] if field != "backup" else [case / name for name in names]
    if name == "analytics":
        roots = {
            "target": case / "analytics" / tenant,
            "staging": case / "analytics" / staging_key,
            "backup": case / ".publication" / tenant / txn / "sidecars" / "analytics" / tenant,
        }
        return [roots[field]]
    fail(f"unknown sidecar oracle {name}")

def assert_sidecar_digest_or_absent(ident, name, field, paths, expected):
    for path in paths:
        assert_digest_or_absent(ident, f"{name}.{field}", path, expected, artifact_digest)

def residue_tree_digest(digests, residue, field):
    expected = digests[field]
    if expected != "absent" or residue[field] != "present":
        return expected
    if field == "staging":
        return digests["new"]
    if field == "backup":
        return digests["old"]
    return expected

def read_report(path, ident, label):
    raw = pathlib.Path(path).read_text(encoding="utf-8")
    try:
        report = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"{ident} {label} CLI stdout is not valid JSON: {error}")
    if set(report) != REPORT_KEYS:
        fail(f"{ident} {label} report keys mismatch: {sorted(report)}")
    return report

def expected_report_identity(ident, layout, scenario, report, phase_override=None):
    txn = transaction_for(layout, scenario)
    phase = None if canonical_journal_missing_for_report(layout) else (
        phase_override or layout.get("journal_phase") or scenario.get("journal_phase")
    )
    evidence = f".publication/{target_for(layout, scenario)}/{txn}"
    if report["transaction_id"] is None:
        if report["phase"] is not None or report["evidence"] is not None:
            fail(f"{ident} null transaction report must also null phase and evidence")
        return
    if report["transaction_id"] != txn:
        fail(f"{ident} transaction_id {report['transaction_id']} does not match {txn}")
    if phase == "absent":
        if report["phase"] is not None:
            fail(f"{ident} phase {report['phase']} should be null for absent journal")
    elif phase is None:
        if report["phase"] is not None:
            fail(f"{ident} phase {report['phase']} should be null without canonical journal")
    elif phase is not None and report["phase"] != phase:
        fail(f"{ident} phase {report['phase']} does not match {phase}")
    if report["evidence"] != evidence:
        fail(f"{ident} evidence {report['evidence']} does not match {evidence}")

def assert_report(manifest_path, generated_path, ident, stdout_path, label):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    layout = generated[ident]
    report = read_report(stdout_path, ident, label)
    if report["tenant"] != target_for(layout, scenario):
        fail(f"{ident} report tenant {report['tenant']} does not match generated tenant")
    if report["status"] != scenario["cli"]["status"]:
        fail(f"{ident} status {report['status']} does not match manifest {scenario['cli']['status']}")
    if report["action"] != scenario["cli"]["action"]:
        fail(f"{ident} action {report['action']} does not match manifest {scenario['cli']['action']}")
    expected_report_identity(ident, layout, scenario, report)

def assert_clean_report(manifest_path, generated_path, ident, stdout_path):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    report = read_report(stdout_path, ident, "second")
    if report["tenant"] != target_for(generated[ident], scenario):
        fail(f"{ident} second report tenant {report['tenant']} does not match generated tenant")
    if report["status"] != "clean" or report["action"] != "none":
        fail(f"{ident} second report is not clean/none: {report}")
    expected_report_identity(
        ident,
        generated[ident],
        scenario,
        report,
        scenario.get("clean_report_phase"),
    )

def same_tree(left, right):
    left = pathlib.Path(left)
    right = pathlib.Path(right)
    comparison = filecmp.dircmp(left, right)
    if comparison.left_only or comparison.right_only or comparison.diff_files or comparison.funny_files:
        return False
    return all(same_tree(pathlib.Path(comparison.left) / child, pathlib.Path(comparison.right) / child)
               for child in comparison.common_dirs)

def assert_quarantine_file_evidence(ident, name, original, copied, tenant, txn):
    if name != "journal.json":
        if original.read_bytes() != copied.read_bytes():
            fail(f"{ident} quarantine evidence {name} does not match source bytes")
        return
    try:
        source_journal = json.loads(original.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        if original.read_bytes() != copied.read_bytes():
            fail(f"{ident} corrupt quarantine journal does not match source bytes")
        return
    copied_journal = load_json(copied)
    if copied_journal.get("target") != tenant or copied_journal.get("transaction_id") != txn:
        fail(f"{ident} quarantine journal identity mismatch")
    if source_journal.get("target") != copied_journal.get("target"):
        fail(f"{ident} quarantine journal changed target identity")
    if source_journal.get("transaction_id") != copied_journal.get("transaction_id"):
        fail(f"{ident} quarantine journal changed transaction identity")
    if copied_journal.get("phase") != "quarantined":
        fail(f"{ident} quarantine journal phase mismatch")

def assert_state(manifest_path, generated_path, generated_root, case_root, ident, after_first):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    layout = generated[ident]
    case = pathlib.Path(case_root)
    source = pathlib.Path(generated_root) / ident
    tenant = target_for(layout, scenario)
    txn = transaction_for(layout, scenario)
    visible = scenario["visible"]
    residue = scenario["residue"]

    target_path = case / tenant
    if visible["target"] != "absent":
        if not target_path.is_dir():
            fail(f"{ident} target should be present")
        meta = target_path / "index_meta.json"
        if visible["object"] != "absent" and meta.read_text(encoding="utf-8") != visible["object"]:
            fail(f"{ident} visible object mismatch")
    if visible["search"] == "loadable" and not target_path.is_dir():
        fail(f"{ident} search oracle requires a loadable target")

    digests = scenario["digests"]
    digest_paths = {
        "target": target_path,
        "staging": RESIDUE_FIELDS["staging"](case, tenant, txn),
        "backup": RESIDUE_FIELDS["backup"](case, tenant, txn),
    }
    for field, path in digest_paths.items():
        expected_digest = digests[field]
        if field in {"staging", "backup"}:
            expected_digest = residue_tree_digest(digests, residue, field)
        assert_digest_or_absent(ident, field, path, expected_digest, canonical_tenant_tree_digest)
    for name, values in scenario["sidecars"].items():
        for field in ["target", "staging", "backup"]:
            assert_sidecar_digest_or_absent(
                ident,
                name,
                field,
                sidecar_paths(case, tenant, txn, name, field),
                values[field],
            )

    for field, resolver in RESIDUE_FIELDS.items():
        path = resolver(case, tenant, txn)
        expected = residue[field]
        if expected == "present" and not path.exists():
            fail(f"{ident} residue {field} should be present")
        if expected == "absent" and path.exists():
            fail(f"{ident} residue {field} should be absent")

    if scenario["disposition"] == "quarantine":
        source_target = source / tenant
        if source_target.exists() and not same_tree(source_target, target_path):
            fail(f"{ident} quarantine changed live target")
        quarantine = RESIDUE_FIELDS["quarantine"](case, tenant, txn)
        if not quarantine.is_dir():
            fail(f"{ident} quarantine evidence directory missing")
        allowed = {"journal.json", "journal.json.tmp", "staging", "backup"}
        observed = {child.name for child in quarantine.iterdir()}
        if not observed or not observed.issubset(allowed):
            fail(f"{ident} quarantine copied unexpected evidence: {sorted(observed)}")
        for name in observed:
            original_name = "journal.json.tmp" if name == "journal.json.tmp" else name
            original = case / ".publication" / tenant / txn / original_name
            if original.exists() and original.is_dir() and not same_tree(original, quarantine / name):
                fail(f"{ident} quarantine evidence {name} does not match source bytes")
            if original.exists() and original.is_file():
                assert_quarantine_file_evidence(ident, name, original, quarantine / name, tenant, txn)
    elif after_first and scenario["cli"]["exit_code"] != 0:
        if not same_tree(source, case):
            fail(f"{ident} nonzero repair mutated generated evidence")

def assert_equal_json(left_path, right_path, ident):
    if load_json(left_path) != load_json(right_path):
        fail(f"{ident} JSON receipts differ")

def assert_equal_report_json(left_path, right_path, ident):
    if read_report(left_path, ident, "first") != read_report(right_path, ident, "second"):
        fail(f"{ident} clean first report changed on second run")

def assert_equal_projection_receipt(left_path, right_path, ident):
    if load_json(left_path) != load_json(right_path):
        fail(f"{ident} stable HTTP projection changed after restart")

def strip_ansi(text):
    result = []
    i = 0
    while i < len(text):
        if text[i] != "\x1b":
            result.append(text[i])
            i += 1
            continue
        i += 1
        if i >= len(text) or text[i] != "[":
            continue
        i += 1
        while i < len(text) and text[i] not in "@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~":
            i += 1
        if i < len(text):
            i += 1
    return "".join(result)

def extract_bind_addr_from_banner(path):
    local_banner = re.compile(r"^\s*(?:[^\w\s]+\s+)?Local:\s+http://127\.0\.0\.1:(\d+)\s*$")
    for raw_line in pathlib.Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
        line = strip_ansi(raw_line)
        match = local_banner.match(line)
        if match:
            return f"127.0.0.1:{match.group(1)}"
    return None

def probe_health(bind_addr, build_info_path):
    assert_health_payload(http_json(bind_addr, "GET", "/health", 200), load_json(build_info_path))

def stable_health_receipt(payload):
    return {
        key: payload[key]
        for key in ["status", "version", "build", "capabilities", "pressure_level", "allocator"]
    }

def stable_index_list_receipt(payload):
    dynamic = {
        "createdAt", "updatedAt", "entries", "dataSize", "fileSize",
        "lastBuildTimeS", "numberOfPendingTasks", "pendingTask",
    }
    return {
        "items": [
            {key: value for key, value in sorted(item.items()) if key not in dynamic}
            for item in payload["items"]
        ],
        "nbPages": payload["nbPages"],
    }

def stable_search_receipt(payload):
    dynamic = {"processingTimeMS", "serverTimeMS", "processingTimingsMS", "serverUsed"}
    return {key: value for key, value in sorted(payload.items()) if key not in dynamic}

def target_object_fixture(fixture, visible_object):
    projections = fixture.get("target_projections")
    if not isinstance(projections, dict):
        fail("live_http_fixture target_projections must be an object")
    projection = projections.get(visible_object)
    if not isinstance(projection, dict):
        fail(f"unknown loadable object projection {visible_object}")
    object_key = projection.get("object")
    query_key = projection.get("query")
    object_fixture = fixture.get(object_key)
    query_fixture = fixture.get(query_key)
    if not isinstance(object_fixture, dict):
        fail(f"target projection {visible_object} references unknown object fixture {object_key}")
    if not isinstance(query_fixture, dict):
        fail(f"target projection {visible_object} references unknown query fixture {query_key}")
    return object_fixture, query_fixture

def assert_index_list(payload, expected_names, ident):
    if set(payload) != {"items", "nbPages"}:
        fail(f"{ident} index-list keys mismatch: {sorted(payload)}")
    if not isinstance(payload["items"], list):
        fail(f"{ident} index-list items must be a list")
    names = []
    for item in payload["items"]:
        if not isinstance(item, dict):
            fail(f"{ident} index-list item must be an object")
        allowed = {
            "name", "createdAt", "updatedAt", "entries", "dataSize", "fileSize",
            "lastBuildTimeS", "numberOfPendingTasks", "pendingTask", "replicas",
            "primary", "virtual",
        }
        if set(item) - allowed:
            fail(f"{ident} index-list item has unexpected keys: {sorted(item)}")
        name = item.get("name")
        if not isinstance(name, str) or not name:
            fail(f"{ident} index-list item missing name")
        for key in ["entries", "dataSize", "fileSize", "lastBuildTimeS", "numberOfPendingTasks"]:
            if key in item and (not isinstance(item[key], int) or item[key] < 0):
                fail(f"{ident} index-list {name}.{key} must be a non-negative integer")
        if "pendingTask" in item and not isinstance(item["pendingTask"], bool):
            fail(f"{ident} index-list {name}.pendingTask must be boolean")
        names.append(name)
        assert_no_projection_token(item)
    if sorted(names) != expected_names:
        fail(f"{ident} index-list names {sorted(names)} do not match {expected_names}")
    if not isinstance(payload["nbPages"], int) or payload["nbPages"] < 1:
        fail(f"{ident} index-list nbPages must be a positive integer")

def assert_query_projection(bind_addr, ident, index, object_fixture, query_fixture):
    object_id = urllib.parse.quote(object_fixture["object_id"])
    body = http_json(bind_addr, "GET", f"/1/indexes/{index}/{object_id}", 200)
    assert_exact_object(body, object_fixture["body"], ident, index)
    encoded_query = urllib.parse.quote(query_fixture["text"])
    payload = http_json(bind_addr, "GET", f"/1/indexes/{index}/query?query={encoded_query}", 200)
    assert_search_payload(
        payload,
        query_fixture["ordered_hit_ids"],
        [object_fixture],
        query_fixture["text"],
        ident,
        index,
    )
    return {"object": body, "query": stable_search_receipt(payload)}

def assert_unavailable_projection(bind_addr, ident, index, object_id, fixture, visible):
    statuses = fixture["surface_statuses"]
    quoted_id = urllib.parse.quote(object_id)
    if visible["target"] == "absent":
        object_expected = statuses["index_absent"]
        query_expected = statuses["index_absent"]
    elif visible["object"] == "absent":
        object_expected = statuses["object_absent"]
        query_expected = statuses["search_unavailable"]
    else:
        object_expected = statuses["object_unavailable"]
        query_expected = statuses["search_unavailable"]
    object_payload = http_json(bind_addr, "GET", f"/1/indexes/{index}/{quoted_id}", object_expected["status"])
    assert_error_payload(object_payload, object_expected, ident, f"{index} object")
    query_payload = http_json(bind_addr, "GET", f"/1/indexes/{index}/query", query_expected["status"])
    assert_error_payload(query_payload, query_expected, ident, f"{index} search")
    return {
        "object": {"status": object_expected["status"], "body": object_payload},
        "query": {"status": query_expected["status"], "body": query_payload},
    }

def assert_http_projection(manifest_path, generated_path, ident, bind_addr, build_info_path, receipt_path):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    layout = generated[ident]
    fixture = manifest["live_http_fixture"]
    target_index = target_for(layout, scenario)
    visible = scenario["visible"]
    expected_names = [fixture["control_index"]]
    if visible["target"] != "absent":
        expected_names.append(target_index)
    expected_names = sorted(expected_names)

    health = http_json(bind_addr, "GET", "/health", 200)
    ready = http_json(bind_addr, "GET", "/health/ready", 200)
    index_list = http_json(bind_addr, "GET", "/1/indexes", 200)
    assert_health_payload(health, load_json(build_info_path))
    assert_ready_payload(ready)
    assert_index_list(index_list, expected_names, ident)
    control_projection = assert_query_projection(
        bind_addr,
        ident,
        fixture["control_index"],
        fixture["control_object"],
        fixture["control_query"],
    )
    if visible["search"] == "loadable":
        object_fixture, query_fixture = target_object_fixture(fixture, visible["object"])
        target_projection = assert_query_projection(bind_addr, ident, target_index, object_fixture, query_fixture)
    else:
        target_projection = assert_unavailable_projection(
            bind_addr,
            ident,
            target_index,
            fixture["target_object"]["object_id"],
            fixture,
            visible,
        )
    receipt = {
        "health": stable_health_receipt(health),
        "ready": ready,
        "indexes": stable_index_list_receipt(index_list),
        "control": control_projection,
        "target": target_projection,
    }
    pathlib.Path(receipt_path).write_text(
        json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n",
        encoding="utf-8",
    )

def unavailable(reason):
    return {"unavailable": reason}

def artifact_reference(artifact_dir, candidate, reason="not produced before failure"):
    if not candidate:
        return unavailable(reason)
    artifact = pathlib.Path(artifact_dir).resolve()
    path = pathlib.Path(candidate).resolve()
    try:
        relative = path.relative_to(artifact)
    except ValueError:
        return unavailable("path was outside the artifact directory")
    return relative.as_posix() if path.exists() else unavailable(reason)

def artifact_inventory(artifact_dir):
    artifact = pathlib.Path(artifact_dir)
    inventory = {}
    for owner in ("generated", "repair", ".runner"):
        root = artifact / owner
        paths = []
        if root.exists():
            for path in root.rglob("*"):
                if not path.is_file():
                    continue
                relative = path.relative_to(artifact).as_posix()
                if relative.startswith(".runner/failure_evidence/"):
                    continue
                paths.append(relative)
        inventory[owner] = sorted(paths)
    return inventory

def disposition_counts(items, id_field):
    counts = {}
    for item in items:
        if id_field not in item:
            continue
        disposition = item.get("disposition")
        if isinstance(disposition, str):
            counts[disposition] = counts.get(disposition, 0) + 1
    return dict(sorted(counts.items()))

def manifest_summary(manifest_path, generated_path):
    manifest = load_json(manifest_path)
    scenarios = manifest.get("scenarios", [])
    generated = pathlib.Path(generated_path)
    summary = {
        "expected_count": manifest.get("layout_count"),
        "expected_dispositions": disposition_counts(scenarios, "id"),
    }
    if not generated.exists():
        missing = unavailable("generated layouts were not produced before failure")
        summary["generated_observed_count"] = missing
        summary["generated_dispositions"] = missing
        return summary
    try:
        layouts = load_json(generated)
    except (OSError, ValueError, json.JSONDecodeError):
        invalid = unavailable("generated layouts were not readable after failure")
        summary["generated_observed_count"] = invalid
        summary["generated_dispositions"] = invalid
        return summary
    summary["generated_observed_count"] = len(layouts)
    summary["generated_dispositions"] = disposition_counts(layouts, "scenario_id")
    return summary

def valid_build_info_reference(artifact_dir):
    path = pathlib.Path(artifact_dir) / ".runner" / "build_info.json"
    try:
        value = load_json(path)
    except (OSError, ValueError, json.JSONDecodeError):
        return {
            "availability": "unavailable",
            "reason": "valid build info was not produced before failure",
        }
    if not isinstance(value, dict) or value.get("schemaVersion") != 1:
        return {
            "availability": "unavailable",
            "reason": "valid build info was not produced before failure",
        }
    return {"availability": "available", "path": ".runner/build_info.json"}

def checkout_summary(repo_dir):
    head = subprocess.run(
        ["git", "-C", repo_dir, "rev-parse", "HEAD"],
        check=True, capture_output=True, text=True,
    ).stdout.strip()
    status = subprocess.run(
        ["git", "-C", repo_dir, "status", "--short"],
        check=True, capture_output=True, text=True,
    ).stdout
    return {"head": head, "porcelain": "dirty" if status else "clean"}

def repair_artifacts(artifact_dir, ident, label):
    prefix = pathlib.Path(artifact_dir) / ".runner" / f"{ident}.{label}" if ident else None
    stdout = f"{prefix}.stdout.json" if prefix else ""
    stderr = f"{prefix}.stderr" if prefix else ""
    return {
        "stdout": artifact_reference(artifact_dir, stdout),
        "stderr": artifact_reference(artifact_dir, stderr),
        "report": artifact_reference(artifact_dir, stdout),
    }

def redacted_failure_reason(reason, artifact_dir, repo_dir):
    if not reason:
        return unavailable("runner did not record a reason")
    redacted = reason.replace(str(pathlib.Path(artifact_dir).resolve()), "<artifact>")
    redacted = redacted.replace(str(pathlib.Path(repo_dir).resolve()), "<checkout>")
    redacted = re.sub(r"; last health error: .*; stdout:", "; stdout:", redacted)
    return redacted

def evidence_index(args, cleanup):
    (
        artifact_dir, manifest_path, repo_dir, original, final, phase, reason,
        child_pid, bind_addr, server_stdout, server_stderr, ident, cleanup_outcome,
    ) = args
    generated_path = pathlib.Path(artifact_dir) / "generated" / "generated_layouts.json"
    server_missing = "server was not started before failure"
    health_path = pathlib.Path(artifact_dir) / ".runner" / f"{ident}.health_probe.stderr" if ident else ""
    projection_prefix = pathlib.Path(artifact_dir) / ".runner" / ident if ident else None
    return {
        "schema_version": 1,
        "status": {
            "original_exit_status": int(original),
            "final_exit_status": int(final),
            "failure_phase": phase,
            "failure_reason": redacted_failure_reason(reason, artifact_dir, repo_dir),
        },
        "checkout": checkout_summary(repo_dir),
        "build_info": valid_build_info_reference(artifact_dir),
        "manifest_summary": manifest_summary(manifest_path, generated_path),
        "server": {
            "recorded_child_pid": int(child_pid) if child_pid else unavailable(server_missing),
            "bind_address": bind_addr or unavailable(
                "server bind address was not observed before failure" if child_pid else server_missing
            ),
            "stdout": artifact_reference(artifact_dir, server_stdout, server_missing),
            "stderr": artifact_reference(artifact_dir, server_stderr, server_missing),
        },
        "artifacts": {
            "generated_layouts": artifact_reference(
                artifact_dir, generated_path, "generated layouts were not produced before failure"
            ),
            "cleanup_trace": ".runner/cleanup_trace.jsonl",
            "repair_invocations": {
                "first": repair_artifacts(artifact_dir, ident, "first"),
                "second": repair_artifacts(artifact_dir, ident, "second"),
            },
            "health_probe_stderr": artifact_reference(artifact_dir, health_path),
            "http_projection_receipts": {
                "first": artifact_reference(
                    artifact_dir, f"{projection_prefix}.first.projection.json" if projection_prefix else ""
                ),
                "second": artifact_reference(
                    artifact_dir, f"{projection_prefix}.second.projection.json" if projection_prefix else ""
                ),
            },
        },
        "cleanup": cleanup,
    }

def atomic_json_write(path, value):
    path = pathlib.Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(path.suffix + ".tmp")
    temporary.write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n", encoding="utf-8")
    temporary.replace(path)

def append_cleanup_trace(path, event, child_pid, signal, target_type, target, fallback, result):
    trace = pathlib.Path(path)
    trace.parent.mkdir(parents=True, exist_ok=True)
    sequence = 1
    if trace.exists():
        sequence += sum(1 for line in trace.read_text(encoding="utf-8").splitlines() if line)
    record = {
        "sequence": sequence,
        "event": event,
        "recorded_child_pid": int(child_pid) if child_pid else None,
        "attempted_signal": signal or None,
        "target_type": target_type or None,
        "target": target or None,
        "fallback_result": fallback or None,
        "result": result,
    }
    with trace.open("a", encoding="utf-8") as handle:
        handle.write(json.dumps(record, sort_keys=True, separators=(",", ":")) + "\n")

def snapshot_evidence(*args):
    artifact_dir, original = args[0], int(args[3])
    evidence_dir = pathlib.Path(artifact_dir) / ".runner" / "failure_evidence"
    pre_inventory = artifact_inventory(artifact_dir)
    atomic_json_write(evidence_dir / "pre_cleanup_inventory.json", pre_inventory)
    cleanup = {
        "outcome": "in_progress",
        "attempted_removals": [".runner/publication_repair_cli_live_helper.py"],
        "pre_inventory": pre_inventory,
        "post_inventory": unavailable("cleanup has not completed"),
    }
    if original != 0:
        atomic_json_write(evidence_dir / "index.json", evidence_index(args, cleanup))

def finalize_evidence(*args):
    artifact_dir, original, final = args[0], int(args[3]), int(args[4])
    evidence_dir = pathlib.Path(artifact_dir) / ".runner" / "failure_evidence"
    pre_path = evidence_dir / "pre_cleanup_inventory.json"
    pre_inventory = load_json(pre_path) if pre_path.exists() else artifact_inventory(artifact_dir)
    pathlib.Path(__file__).unlink(missing_ok=True)
    pre_path.unlink(missing_ok=True)
    cleanup = {
        "outcome": args[-1],
        "attempted_removals": [".runner/publication_repair_cli_live_helper.py"],
        "pre_inventory": pre_inventory,
        "post_inventory": artifact_inventory(artifact_dir),
    }
    if original != 0 or final != 0:
        atomic_json_write(evidence_dir / "index.json", evidence_index(args, cleanup))
    else:
        (evidence_dir / "index.json").unlink(missing_ok=True)
        try:
            evidence_dir.rmdir()
        except OSError:
            pass

def main(argv):
    command = argv[1]
    if command == "validate-generated":
        validate_generated(*argv[2:])
    elif command == "clone-case":
        clone_case(*argv[2:])
    elif command == "target":
        target(*argv[2:])
    elif command == "managed-snapshot":
        managed_snapshot(*argv[2:])
    elif command == "assert-report":
        assert_report(*argv[2:])
    elif command == "assert-clean-report":
        assert_clean_report(*argv[2:])
    elif command == "assert-state":
        assert_state(*argv[2:])
    elif command == "assert-equal-json":
        assert_equal_json(*argv[2:])
    elif command == "assert-equal-managed-snapshot":
        assert_equal_managed_snapshot(*argv[2:])
    elif command == "assert-equal-report-json":
        assert_equal_report_json(*argv[2:])
    elif command == "assert-equal-projection-receipt":
        assert_equal_projection_receipt(*argv[2:])
    elif command == "startup-bind-addr":
        bind_addr = extract_bind_addr_from_banner(argv[2])
        if bind_addr is None:
            raise SystemExit(1)
        print(bind_addr)
    elif command == "probe-health":
        probe_health(*argv[2:])
    elif command == "assert-http-projection":
        assert_http_projection(*argv[2:])
    elif command == "append-cleanup-trace":
        append_cleanup_trace(*argv[2:])
    elif command == "snapshot-evidence":
        snapshot_evidence(*argv[2:])
    elif command == "finalize-evidence":
        finalize_evidence(*argv[2:])
    else:
        fail(f"unknown helper command {command}")

if __name__ == "__main__":
    main(sys.argv)
PY
}

assert_build_info_json() {
  local path="$1"
  local revision="$2"
  python3 - "$path" "$revision" <<'PY'
import json
import sys

path, revision = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    value = json.load(handle)

def fail(message):
    raise SystemExit(f"{path}: {message}: {value}")

if value.get("schemaVersion") != 1:
    fail("schemaVersion must be 1")
if value.get("revision") != revision:
    fail("revision must match reviewed HEAD")
if value.get("revisionKnown") is not True:
    fail("revisionKnown must be true")
dirty = value.get("dirty")
dirty_known = value.get("dirtyKnown")
if dirty is True:
    fail("dirty must not be true")
if dirty is None:
    if dirty_known is not False:
        fail("dirtyKnown must be false when dirty is null")
elif dirty is False:
    if dirty_known is not True:
        fail("dirtyKnown must be true when dirty is false")
else:
    fail("dirty must be a boolean or null")
features = value.get("features")
if not isinstance(features, list) or features != sorted(features):
    fail("features must be sorted")
capabilities = value.get("capabilities")
if set(capabilities or {}) != {"vectorSearch", "vectorSearchLocal"}:
    fail("capabilities must contain only vectorSearch and vectorSearchLocal")
if not all(isinstance(capabilities[key], bool) for key in capabilities):
    fail("capabilities values must be booleans")
serialized = json.dumps(value, sort_keys=True, separators=(",", ":"))
for forbidden in ("algolia_migration_v1", "algoliaMigrationV1"):
    if forbidden in serialized:
        fail(f"serialized payload must not contain {forbidden}")
PY
}

identity_gate() {
  local revision=""
  local status=""
  local build_info="$ARTIFACT_DIR/.runner/build_info.json"
  revision="$(git -C "$REPO_DIR" rev-parse HEAD)"
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || die "reviewed revision must be a 40-character lowercase SHA"
  if [ "${PUBLICATION_REPAIR_CLI_ALLOW_DIRTY_FOR_TEST:-0}" != "1" ]; then
    status="$(git -C "$REPO_DIR" status --short)"
    [ -z "$status" ] || die "checkout must be clean before live publication repair contract"
  fi
  collect_flapjack_env
  run_bounded 60s env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} "$BINARY_PATH" build-info --json >"$build_info"
  assert_build_info_json "$build_info" "$revision"
}

run_generator() {
  local generated_dir="$ARTIFACT_DIR/generated"
  mkdir -p "$generated_dir"
  (
    cd "$ENGINE_DIR"
    collect_flapjack_env
    run_bounded 600s env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} \
      PUBLICATION_REPAIR_CLI_MANIFEST="$MANIFEST_PATH" \
      PUBLICATION_REPAIR_CLI_ARTIFACT_DIR="$generated_dir" \
      cargo test -p flapjack --lib publication_repair_cli -- --ignored
  )
  [ -f "$generated_dir/generated_layouts.json" ] || die "generator did not write generated_layouts.json"
  python3 "$HELPER" validate-generated "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$generated_dir" \
    >"$ARTIFACT_DIR/.runner/scenario_ids.txt"
}

invoke_cli() {
  local case_root="$1"
  local target="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  local mode="${PUBLICATION_REPAIR_CLI_INVOKE_MODE_FOR_TEST:-}"
  local receipt_path="${stderr_path}.invoke_receipt"
  local status=0
  local restore_errexit=0
  if [ "$mode" = "skip_first_repair" ] && [[ "$(basename "$stdout_path")" = *.first.stdout.json ]]; then
    python3 - "$target" >"$stdout_path" <<'PY'
import json
import sys
print(json.dumps({
    "tenant": sys.argv[1],
    "status": "clean",
    "action": "none",
    "transaction_id": None,
    "phase": None,
    "evidence": None,
}, separators=(",", ":")))
PY
    : >"$stderr_path"
    [ -f "$receipt_path" ] || die "repair-publication subprocess invocation missing for $(basename "$stdout_path")"
  fi
  collect_flapjack_env
  [[ "$-" == *e* ]] && restore_errexit=1
  set +e
  run_bounded 120s env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} \
    "$BINARY_PATH" --data-dir "$case_root" repair-publication --tenant "$target" --json \
    >"$stdout_path" 2>"$stderr_path"
  status=$?
  [ "$restore_errexit" -eq 0 ] || set -e
  printf 'repair-publication subprocess invoked\n' >"$receipt_path"
  return "$status"
}

server_output_path() {
  local ident="$1"
  local label="$2"
  printf '%s\n' "$ARTIFACT_DIR/.runner/${ident}.${label}.server"
}

wait_for_startup_bind_addr() {
  local ident="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))
  local bind_addr=""

  while :; do
    if bind_addr="$(python3 "$HELPER" startup-bind-addr "$SERVER_STDOUT_PATH" 2>/dev/null)"; then
      SERVER_BIND_ADDR="$bind_addr"
      return 0
    fi
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
      local status=0
      set +e
      wait "$CHILD_PID"
      status=$?
      set -e
      CHILD_PID=""
      die "$ident server exited before startup banner (status $status); stdout: $SERVER_STDOUT_PATH stderr: $SERVER_STDERR_PATH"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      local stdout_path="$SERVER_STDOUT_PATH"
      local stderr_path="$SERVER_STDERR_PATH"
      stop_server
      die "$ident timed out waiting for startup banner; stdout: $stdout_path stderr: $stderr_path"
    fi
    sleep 0.1
  done
}

wait_for_health() {
  local ident="$1"
  local bind_addr="$2"
  local timeout_secs="$3"
  local deadline=$((SECONDS + timeout_secs))
  local health_probe_stderr="$ARTIFACT_DIR/.runner/${ident}.health_probe.stderr"
  : >"$health_probe_stderr"

  while :; do
    if python3 "$HELPER" probe-health "$bind_addr" "$ARTIFACT_DIR/.runner/build_info.json" >/dev/null 2>"$health_probe_stderr"; then
      return 0
    fi
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
      local status=0
      set +e
      wait "$CHILD_PID"
      status=$?
      set -e
      CHILD_PID=""
      die "$ident server exited before /health became ready (status $status); stdout: $SERVER_STDOUT_PATH stderr: $SERVER_STDERR_PATH"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      local stdout_path="$SERVER_STDOUT_PATH"
      local stderr_path="$SERVER_STDERR_PATH"
      local health_error=""
      health_error="$(tail -n 1 "$health_probe_stderr" 2>/dev/null || true)"
      stop_server
      die "$ident /health probe timed out; last health error: $health_error; stdout: $stdout_path stderr: $stderr_path"
    fi
    sleep 0.05
  done
}

start_server() {
  local case_root="$1"
  local ident="$2"
  local label="$3"

  FAILURE_PHASE="server_startup"
  SERVER_STDOUT_PATH="$(server_output_path "$ident" "$label").stdout"
  SERVER_STDERR_PATH="$(server_output_path "$ident" "$label").stderr"
  : >"$SERVER_STDOUT_PATH"
  : >"$SERVER_STDERR_PATH"

  collect_flapjack_env
  python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' \
    env ${FLAPJACK_ENV_ARGS[@]+"${FLAPJACK_ENV_ARGS[@]}"} \
    FLAPJACK_ANALYTICS_ENABLED=false \
    "$BINARY_PATH" --data-dir "$case_root" --auto-port --no-auth \
    >"$SERVER_STDOUT_PATH" 2>"$SERVER_STDERR_PATH" &
  CHILD_PID=$!
  CHILD_TARGET_MODE="group"
  LAST_CHILD_PID="$CHILD_PID"
  LAST_SERVER_STDOUT_PATH="$SERVER_STDOUT_PATH"
  LAST_SERVER_STDERR_PATH="$SERVER_STDERR_PATH"

  wait_for_startup_bind_addr "$ident" 5
  LAST_SERVER_BIND_ADDR="$SERVER_BIND_ADDR"
  FAILURE_PHASE="server_health"
  wait_for_health "$ident" "$SERVER_BIND_ADDR" 5
}

server_process_group_alive() {
  local pid="$1"
  if [ "$CHILD_TARGET_MODE" = "group" ]; then
    kill -0 -- "-$pid" 2>/dev/null || kill -0 "$pid" 2>/dev/null
  else
    kill -0 "$pid" 2>/dev/null
  fi
}

send_server_signal() {
  local signal="$1"
  local pid="$2"
  if [ "$CHILD_TARGET_MODE" = "group" ]; then
    if kill "-$signal" -- "-$pid" 2>/dev/null; then
      append_cleanup_trace signal_attempt "$pid" "$signal" group "-$pid" not_needed success
      return 0
    fi
    if kill "-$signal" "$pid" 2>/dev/null; then
      append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" group_failed success
      return 0
    fi
    append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" group_failed failed
    return 1
  fi
  if kill "-$signal" "$pid" 2>/dev/null; then
    append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" not_applicable success
    return 0
  fi
  append_cleanup_trace signal_attempt "$pid" "$signal" direct "$pid" not_applicable failed
  return 1
}

wait_for_server_process_group_exit() {
  local pid="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))

  while server_process_group_alive "$pid"; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.1
  done
}

# Reap the recorded direct child without blocking past the bounded shutdown
# budget. A plain `wait "$pid"` blocks until the child exits, so a child that
# has not yet died (e.g. delayed KILL delivery or an uninterruptible state)
# would stall shutdown indefinitely before the final process-group check runs.
# Poll the child's liveness instead; once it is gone the child is a reapable
# zombie and `wait` returns its status immediately. If the child is still alive
# after the budget, give up so the caller fails closed rather than blocking.
reap_child_within_budget() {
  local pid="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))

  while kill -0 "$pid" 2>/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.05
  done

  wait "$pid" >/dev/null 2>&1 || true
  return 0
}

clear_server_state() {
  append_cleanup_trace state_cleared "${CHILD_PID:-}" "" "${CHILD_TARGET_MODE:-}" "${CHILD_PID:-}" "" cleared || true
  CHILD_PID=""
  CHILD_TARGET_MODE=""
  SERVER_BIND_ADDR=""
  SERVER_STDOUT_PATH=""
  SERVER_STDERR_PATH=""
}

stop_server() {
  local pid="${CHILD_PID:-}"

  if [ -z "$pid" ]; then
    clear_server_state
    return 0
  fi

  if server_process_group_alive "$pid"; then
    send_server_signal TERM "$pid" || true
    if ! wait_for_server_process_group_exit "$pid" 1; then
      append_cleanup_trace wait_timeout "$pid" TERM "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
      send_server_signal KILL "$pid" || true
      if ! reap_child_within_budget "$pid" 1; then
        append_cleanup_trace reap_result "$pid" KILL "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
        printf 'ERROR: server child could not be reaped within the bounded shutdown budget after KILL (pid %s)\n' "$pid" >&2
        clear_server_state
        return 1
      fi
      append_cleanup_trace reap_result "$pid" KILL "$CHILD_TARGET_MODE" "$pid" "" reaped || true
      if ! wait_for_server_process_group_exit "$pid" 1; then
        append_cleanup_trace wait_timeout "$pid" KILL "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
        printf 'ERROR: server process group remained alive after KILL (pid %s)\n' "$pid" >&2
        clear_server_state
        return 1
      fi
    else
      if reap_child_within_budget "$pid" 1; then
        append_cleanup_trace reap_result "$pid" TERM "$CHILD_TARGET_MODE" "$pid" "" reaped || true
      else
        append_cleanup_trace reap_result "$pid" TERM "$CHILD_TARGET_MODE" "$pid" "" timed_out || true
      fi
    fi
  fi

  clear_server_state
}

run_server_lifecycle() {
  local case_root="$1"
  local ident="$2"
  local label="$3"
  local receipt_path="$4"

  start_server "$case_root" "$ident" "$label"
  FAILURE_PHASE="http_projection"
  python3 "$HELPER" assert-http-projection "$MANIFEST_PATH" "$ARTIFACT_DIR/generated/generated_layouts.json" "$ident" "$SERVER_BIND_ADDR" "$ARTIFACT_DIR/.runner/build_info.json" "$receipt_path" || {
    local status=$?
    stop_server || true
    return "$status"
  }
  stop_server
}

assert_exit_code() {
  local actual="$1"
  local expected="$2"
  local ident="$3"
  [ "$actual" -eq "$expected" ] || die "$ident CLI exit code $actual does not match manifest $expected"
}

manifest_exit_code() {
  python3 - "$MANIFEST_PATH" "$1" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for scenario in manifest["scenarios"]:
    if scenario["id"] == sys.argv[2]:
        print(scenario["cli"]["exit_code"])
        break
else:
    raise SystemExit(f"unknown scenario {sys.argv[2]}")
PY
}

manifest_disposition() {
  python3 - "$MANIFEST_PATH" "$1" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for scenario in manifest["scenarios"]:
    if scenario["id"] == sys.argv[2]:
        print(scenario["disposition"])
        break
else:
    raise SystemExit(f"unknown scenario {sys.argv[2]}")
PY
}

run_case() {
  local ident="$1"
  CURRENT_CASE_ID="$ident"
  local generated_dir="$ARTIFACT_DIR/generated"
  local repair_root="$ARTIFACT_DIR/repair"
  local case_root="$repair_root/$ident"
  local target=""
  local expected_exit=""
  local first_status=""
  local second_status=""
  local first_stdout="$ARTIFACT_DIR/.runner/${ident}.first.stdout.json"
  local first_stderr="$ARTIFACT_DIR/.runner/${ident}.first.stderr"
  local second_stdout="$ARTIFACT_DIR/.runner/${ident}.second.stdout.json"
  local second_stderr="$ARTIFACT_DIR/.runner/${ident}.second.stderr"
  local first_managed="$ARTIFACT_DIR/.runner/${ident}.first.managed.json"
  local second_managed="$ARTIFACT_DIR/.runner/${ident}.second.managed.json"
  local first_projection="$ARTIFACT_DIR/.runner/${ident}.first.projection.json"
  local second_projection="$ARTIFACT_DIR/.runner/${ident}.second.projection.json"
  local disposition=""

  python3 "$HELPER" clone-case "$generated_dir" "$repair_root" "$ident"
  target="$(python3 "$HELPER" target "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident")"
  expected_exit="$(manifest_exit_code "$ident")"
  disposition="$(manifest_disposition "$ident")"

  set +e
  invoke_cli "$case_root" "$target" "$first_stdout" "$first_stderr"
  first_status=$?
  set -e
  assert_exit_code "$first_status" "$expected_exit" "$ident"
  FAILURE_PHASE="report_assertion"
  python3 "$HELPER" assert-report "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident" "$first_stdout" first
  FAILURE_PHASE="state_assertion"
  python3 "$HELPER" assert-state "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$generated_dir" "$case_root" "$ident" true
  run_server_lifecycle "$case_root" "$ident" first "$first_projection"
  python3 "$HELPER" managed-snapshot "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$case_root" "$ident" >"$first_managed"

  if [ "$disposition" = "commit" ] || [ "$disposition" = "rollback" ] || [ "$disposition" = "absent-create" ]; then
    set +e
    invoke_cli "$case_root" "$target" "$second_stdout" "$second_stderr"
    second_status=$?
    set -e
    assert_exit_code "$second_status" 0 "$ident second"
    python3 "$HELPER" assert-clean-report "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident" "$second_stdout"
    python3 "$HELPER" managed-snapshot "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$case_root" "$ident" >"$second_managed"
    python3 "$HELPER" assert-equal-managed-snapshot "$first_managed" "$second_managed" "$ident"
    run_server_lifecycle "$case_root" "$ident" second "$second_projection"
    python3 "$HELPER" assert-equal-projection-receipt "$first_projection" "$second_projection" "$ident"
    if [ "$(cat "$first_stdout")" = "$(cat "$second_stdout")" ]; then
      python3 "$HELPER" assert-equal-report-json "$first_stdout" "$second_stdout" "$ident"
    fi
  else
    [ "$expected_exit" -ne 0 ] || die "$ident unclassified disposition has zero exit"
  fi
}

run_contract() {
  local ident=""
  mkdir -p "$ARTIFACT_DIR/repair"
  while IFS= read -r ident; do
    [ -n "$ident" ] || continue
    run_case "$ident"
  done <"$ARTIFACT_DIR/.runner/scenario_ids.txt"
  printf 'PASS: publication repair CLI live contract passed\n'
}

main() {
  FAILURE_PHASE="argument_validation"
  parse_args "$@"
  require_tools
  validate_paths
  validate_test_invoke_mode
  write_helper
  EVIDENCE_READY=1
  FAILURE_PHASE="identity_gate"
  identity_gate
  FAILURE_PHASE="generator"
  run_generator
  FAILURE_PHASE="contract_case"
  run_contract
  FAILURE_PHASE="complete"
}

main "$@"
