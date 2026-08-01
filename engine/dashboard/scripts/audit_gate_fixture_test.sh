#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AUDIT_GATE="$SCRIPT_DIR/audit_gate.sh"
FIXTURE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/sec_g10_fixture.XXXXXX")"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

materialize_fixture() {
  local fixture_dir="$1"
  local package_name="$2"
  local package_version="$3"

  mkdir -p "$fixture_dir"
  printf '%s\n' \
    "{\"name\":\"sec-g10-fixture\",\"version\":\"1.0.0\",\"private\":true,\"dependencies\":{\"$package_name\":\"$package_version\"}}" \
    > "$fixture_dir/package.json"
  (
    cd "$fixture_dir"
    npm install --package-lock-only --ignore-scripts --no-audit >/dev/null
  )
}

run_gate() {
  local fixture_dir="$1"
  local output_file="$2"

  set +e
  bash "$AUDIT_GATE" "$fixture_dir" >"$output_file" 2>&1
  GATE_STATUS=$?
  set -e
}

assert_gate_result() {
  local expected_status="$1"
  local expected_output="$2"
  local output_file="$3"

  if [ "$GATE_STATUS" -ne "$expected_status" ]; then
    cat "$output_file"
    echo "Unexpected gate result for $output_file: expected exit=$expected_status actual exit=$GATE_STATUS" >&2
    exit 1
  fi
  grep -Fq "$expected_output" "$output_file"
}

assert_gate_fails_for() {
  local fixture_dir="$1"
  local expected_denominator="$2"
  local output_file="$fixture_dir/gate_output.txt"

  run_gate "$fixture_dir" "$output_file"
  assert_gate_result 1 "$expected_denominator" "$output_file"
}

assert_gate_passes_moderate_fixture() {
  local fixture_dir="$1"
  local output_file="$fixture_dir/gate_output.txt"

  run_gate "$fixture_dir" "$output_file"
  assert_gate_result 0 'moderate=1 high=0 critical=0 total=1' "$output_file"
}

assert_gate_fails_closed_without_npm() {
  local output_file="$FIXTURE_ROOT/npm_unavailable_output.txt"

  set +e
  PATH=/nonexistent /bin/bash "$AUDIT_GATE" "$FIXTURE_ROOT" >"$output_file" 2>&1
  GATE_STATUS=$?
  set -e
  assert_gate_result 1 'npm is unavailable' "$output_file"
}

assert_gate_fails_closed_without_node() {
  local npm_only_path="$FIXTURE_ROOT/npm_only_path"
  local output_file="$FIXTURE_ROOT/node_unavailable_output.txt"

  mkdir -p "$npm_only_path"
  printf '%s\n' '#!/bin/sh' 'exit 0' >"$npm_only_path/npm"
  chmod +x "$npm_only_path/npm"

  set +e
  PATH="$npm_only_path" /bin/bash "$AUDIT_GATE" "$FIXTURE_ROOT" >"$output_file" 2>&1
  GATE_STATUS=$?
  set -e
  assert_gate_result 1 'requires node' "$output_file"
}

assert_gate_enforces_validated_threshold() {
  local severity="$1"
  local high=0
  local critical=0
  local fake_bin="$FIXTURE_ROOT/fake_npm_${severity}_bin"
  local output_file="$FIXTURE_ROOT/npm_${severity}_status_mismatch_output.txt"

  case "$severity" in
    high) high=1 ;;
    critical) critical=1 ;;
    *)
      echo "Unsupported threshold severity: $severity" >&2
      exit 1
      ;;
  esac

  mkdir -p "$fake_bin"
  printf '%s\n' \
    '#!/bin/sh' \
    "printf '%s\\n' '{\"metadata\":{\"vulnerabilities\":{\"low\":0,\"moderate\":0,\"high\":$high,\"critical\":$critical}}}'" \
    'exit 0' \
    >"$fake_bin/npm"
  chmod +x "$fake_bin/npm"

  set +e
  PATH="$fake_bin:$PATH" /bin/bash "$AUDIT_GATE" "$FIXTURE_ROOT" >"$output_file" 2>&1
  GATE_STATUS=$?
  set -e
  assert_gate_result 1 "low=0 moderate=0 high=$high critical=$critical total=1" "$output_file"
}

assert_gate_fails_closed_without_audit_json() {
  local invalid_target="$FIXTURE_ROOT/not_a_directory"
  local output_file="$FIXTURE_ROOT/audit_unavailable_output.txt"

  printf '%s\n' "not a directory" >"$invalid_target"
  run_gate "$invalid_target" "$output_file"
  assert_gate_result 1 'could not validate npm audit JSON' "$output_file"
}

materialize_fixture "$FIXTURE_ROOT/critical" "form-data" "4.0.0"
assert_gate_fails_for "$FIXTURE_ROOT/critical" "low=0 moderate=0 high=0 critical=1 total=1"

materialize_fixture "$FIXTURE_ROOT/high" "form-data" "4.0.5"
assert_gate_fails_for "$FIXTURE_ROOT/high" "low=0 moderate=0 high=1 critical=0 total=1"

materialize_fixture "$FIXTURE_ROOT/moderate" "dompurify" "3.3.1"
assert_gate_passes_moderate_fixture "$FIXTURE_ROOT/moderate"

assert_gate_fails_closed_without_npm
assert_gate_fails_closed_without_node
assert_gate_enforces_validated_threshold high
assert_gate_enforces_validated_threshold critical
assert_gate_fails_closed_without_audit_json

echo "Dashboard audit gate fixture tests passed"
