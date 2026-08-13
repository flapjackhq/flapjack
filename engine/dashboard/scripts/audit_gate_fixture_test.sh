#!/bin/bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
AUDIT_GATE="$SCRIPT_DIR/audit_gate.sh"
FIXTURE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/sec_g10_fixture.XXXXXX")"
trap 'rm -rf "$FIXTURE_ROOT"' EXIT

write_audit_json() {
  local fixture_name="$1"
  local fixture_file="$FIXTURE_ROOT/$fixture_name.json"

  # JSON is supplied through a quoted heredoc at each call site so every policy
  # branch is visible and independent of the mutable npm advisory database.
  while IFS= read -r line || [ -n "$line" ]; do
    printf '%s\n' "$line"
  done >"$fixture_file"
}

install_fake_npm() {
  local fake_bin="$FIXTURE_ROOT/fake_bin"

  mkdir -p "$fake_bin"
  # The generated helper expands these variables when the gate invokes it.
  # shellcheck disable=SC2016
  printf '%s\n' \
    '#!/bin/sh' \
    '/bin/cat "$FAKE_AUDIT_JSON"' \
    'exit "$FAKE_NPM_STATUS"' \
    >"$fake_bin/npm"
  chmod +x "$fake_bin/npm"
}

assert_fake_audit() {
  local fixture_name="$1"
  local expected_status="$2"
  local expected_output="$3"
  local npm_status="${4:-0}"
  local output_file="$FIXTURE_ROOT/$fixture_name.output"
  local gate_status

  set +e
  FAKE_AUDIT_JSON="$FIXTURE_ROOT/$fixture_name.json" \
    FAKE_NPM_STATUS="$npm_status" \
    PATH="$FIXTURE_ROOT/fake_bin:$PATH" \
    /bin/bash "$AUDIT_GATE" "$FIXTURE_ROOT" >"$output_file" 2>&1
  gate_status=$?
  set -e

  if [ "$gate_status" -ne "$expected_status" ]; then
    printf 'Unexpected gate result for %s: expected exit=%s actual exit=%s\n' \
      "$fixture_name" "$expected_status" "$gate_status" >&2
    /bin/cat "$output_file"
    return 1
  fi
  if ! grep -Fq "$expected_output" "$output_file"; then
    printf 'Expected output not found for %s: %s\n' "$fixture_name" "$expected_output" >&2
    /bin/cat "$output_file"
    return 1
  fi
}

assert_gate_fails_without_npm() {
  local output_file="$FIXTURE_ROOT/npm_unavailable.output"
  local gate_status

  set +e
  PATH=/nonexistent /bin/bash "$AUDIT_GATE" "$FIXTURE_ROOT" >"$output_file" 2>&1
  gate_status=$?
  set -e
  [ "$gate_status" -eq 1 ] && grep -Fq 'npm is unavailable' "$output_file"
}

assert_gate_fails_without_node() {
  local npm_only_path="$FIXTURE_ROOT/npm_only"
  local output_file="$FIXTURE_ROOT/node_unavailable.output"
  local gate_status

  mkdir -p "$npm_only_path"
  printf '%s\n' '#!/bin/sh' 'exit 0' >"$npm_only_path/npm"
  chmod +x "$npm_only_path/npm"
  set +e
  PATH="$npm_only_path" /bin/bash "$AUDIT_GATE" "$FIXTURE_ROOT" >"$output_file" 2>&1
  gate_status=$?
  set -e
  [ "$gate_status" -eq 1 ] && grep -Fq 'requires node' "$output_file"
}

install_fake_npm

write_audit_json clean <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit clean 0 'low=0 moderate=0 high=0 critical=0 total=0'

write_audit_json unsupported_audit_version <<'JSON'
{"auditReportVersion":3,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit unsupported_audit_version 1 'unsupported auditReportVersion: 3'

write_audit_json missing_audit_version <<'JSON'
{"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit missing_audit_version 1 'unsupported auditReportVersion: undefined'

write_audit_json contradictory_total <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-package":{"name":"low-package","severity":"low","via":[{"source":14,"url":"https://github.com/advisories/GHSA-low1-low2-low3","severity":"low"}]}},"metadata":{"vulnerabilities":{"info":0,"low":1,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit contradictory_total 1 'total vulnerability count mismatch: metadata=0 entries=1'

write_audit_json valid_info <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"info-package":{"name":"info-package","severity":"info","via":[{"source":15,"url":"https://github.com/advisories/GHSA-info-info-info","severity":"info"}]}},"metadata":{"vulnerabilities":{"info":1,"low":0,"moderate":0,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit valid_info 0 'info=1 low=0 moderate=0 high=0 critical=0 total=1' 1

write_audit_json missing_package_name <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-package":{"severity":"low","via":[{"source":18,"url":"https://example.invalid/low","severity":"low"}]}},"metadata":{"vulnerabilities":{"info":0,"low":1,"moderate":0,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit missing_package_name 1 'invalid vulnerability name for low-package'

write_audit_json unresolved_low <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-package":{"name":"low-package","severity":"low","via":[]}},"metadata":{"vulnerabilities":{"info":0,"low":1,"moderate":0,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit unresolved_low 1 'unresolved via chain for low-package'

write_audit_json unresolved_low_cycle <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-a":{"name":"low-a","severity":"low","via":["low-b"]},"low-b":{"name":"low-b","severity":"low","via":["low-a"]}},"metadata":{"vulnerabilities":{"info":0,"low":2,"moderate":0,"high":0,"critical":0,"total":2}}}
JSON
assert_fake_audit unresolved_low_cycle 1 'unresolved via chain for low-a'

write_audit_json all_accepted <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"react-router":{"name":"react-router","severity":"moderate","via":[{"source":1,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"},{"source":2,"url":"https://github.com/advisories/GHSA-337j-9hxr-rhxg","severity":"moderate"}]},"react-router-dom":{"name":"react-router-dom","severity":"moderate","via":[{"source":3,"url":"https://github.com/advisories/GHSA-jjmj-jmhj-qwj2","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":2,"high":0,"critical":0,"total":2}}}
JSON
assert_fake_audit all_accepted 0 'Accepted moderate advisories: GHSA-337j-9hxr-rhxg, GHSA-jjmj-jmhj-qwj2, GHSA-wrjc-x8rr-h8h6' 1

write_audit_json accepted_with_hidden_high <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"react-router":{"name":"react-router","severity":"moderate","via":[{"source":1,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"},{"source":8,"url":"https://example.invalid/high","severity":"high"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit accepted_with_hidden_high 1 'via severity exceeds package severity for react-router: package=moderate via=high'

write_audit_json accepted_with_hidden_critical <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"react-router":{"name":"react-router","severity":"moderate","via":[{"source":1,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"},{"source":12,"url":"https://example.invalid/critical","severity":"critical"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit accepted_with_hidden_critical 1 'via severity exceeds package severity for react-router: package=moderate via=critical'

write_audit_json low_with_unrecognized_moderate <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-package":{"name":"low-package","severity":"low","via":[{"source":9,"url":"https://github.com/advisories/GHSA-new1-new2-new3","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":1,"moderate":0,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit low_with_unrecognized_moderate 1 'via severity exceeds package severity for low-package: package=low via=moderate'

write_audit_json accepted_with_unknown_severity <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"react-router":{"name":"react-router","severity":"moderate","via":[{"source":1,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"},{"source":10,"url":"https://example.invalid/unknown","severity":"unknown"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit accepted_with_unknown_severity 1 'invalid via advisory severity for react-router: unknown'

write_audit_json accepted_with_malformed_via <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"react-router":{"name":"react-router","severity":"moderate","via":[{"source":1,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"},{"severity":"low"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit accepted_with_malformed_via 1 'invalid via advisory for react-router'

write_audit_json valid_low <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-package":{"name":"low-package","severity":"low","via":[{"source":11,"url":"https://github.com/advisories/GHSA-low1-low2-low3","severity":"low"}]}},"metadata":{"vulnerabilities":{"info":0,"low":1,"moderate":0,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit valid_low 0 'low=1 moderate=0 high=0 critical=0 total=1' 1

write_audit_json accepted_wrong_package <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"wrong-package":{"name":"wrong-package","severity":"moderate","via":[{"source":4,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit accepted_wrong_package 1 'expected=react-router actual=wrong-package'

write_audit_json new_moderate <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"new-package":{"name":"new-package","severity":"moderate","via":[{"source":5,"url":"https://github.com/advisories/GHSA-new1-new2-new3","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit new_moderate 1 'Unrecognized moderate advisories: GHSA-new1-new2-new3'

write_audit_json indirect_chain <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"dashboard-wrapper":{"name":"dashboard-wrapper","severity":"moderate","via":["react-router"]},"react-router":{"name":"react-router","severity":"moderate","via":[{"source":6,"url":"https://github.com/advisories/GHSA-wrjc-x8rr-h8h6","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":2,"high":0,"critical":0,"total":2}}}
JSON
assert_fake_audit indirect_chain 0 'Accepted moderate advisories: GHSA-wrjc-x8rr-h8h6' 1

write_audit_json understated_indirect_chain <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"low-wrapper":{"name":"low-wrapper","severity":"low","via":["moderate-leaf"]},"moderate-leaf":{"name":"moderate-leaf","severity":"moderate","via":[{"source":13,"url":"https://github.com/advisories/GHSA-new1-new2-new3","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":1,"moderate":1,"high":0,"critical":0,"total":2}}}
JSON
assert_fake_audit understated_indirect_chain 1 'via severity exceeds package severity for low-wrapper: package=low via=moderate'

write_audit_json unresolved_chain <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"dashboard-wrapper":{"name":"dashboard-wrapper","severity":"moderate","via":["missing-package"]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit unresolved_chain 1 'unresolved via reference for dashboard-wrapper: missing-package'

write_audit_json moderate_without_ghsa <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"react-router":{"name":"react-router","severity":"moderate","via":[{"source":7,"url":"https://example.invalid/advisory/7","severity":"moderate"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit moderate_without_ghsa 1 'Moderate advisories without exact GHSA IDs: 7'

write_audit_json high_finding <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"high-package":{"name":"high-package","severity":"high","via":[{"source":16,"url":"https://example.invalid/high","severity":"high"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":1,"critical":0,"total":1}}}
JSON
assert_fake_audit high_finding 1 'low=0 moderate=0 high=1 critical=0 total=1'

write_audit_json critical_finding <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"critical-package":{"name":"critical-package","severity":"critical","via":[{"source":17,"url":"https://example.invalid/critical","severity":"critical"}]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":1,"total":1}}}
JSON
assert_fake_audit critical_finding 1 'low=0 moderate=0 high=0 critical=1 total=1'

# These are the two regression controls for the metadata-bypass defect: the
# package entry says fail while the old high/critical decision saw only zero.
write_audit_json high_entry_zero_metadata <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"hidden-high":{"name":"hidden-high","severity":"high","via":[]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit high_entry_zero_metadata 1 'high package count mismatch: metadata=0 entries=1'

write_audit_json low_entry_zero_metadata <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"hidden-low":{"name":"hidden-low","severity":"low","via":[]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit low_entry_zero_metadata 1 'low package count mismatch: metadata=0 entries=1'

write_audit_json critical_entry_zero_metadata <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"hidden-critical":{"name":"hidden-critical","severity":"critical","via":[]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit critical_entry_zero_metadata 1 'critical package count mismatch: metadata=0 entries=1'

write_audit_json high_metadata_without_entry <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":1,"critical":0,"total":1}}}
JSON
assert_fake_audit high_metadata_without_entry 1 'high package count mismatch: metadata=1 entries=0'

write_audit_json moderate_metadata_without_entry <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":1,"high":0,"critical":0,"total":1}}}
JSON
assert_fake_audit moderate_metadata_without_entry 1 'moderate package count mismatch: metadata=1 entries=0'

write_audit_json invalid_severity <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{"mystery-package":{"name":"mystery-package","severity":"unknown","via":[]}},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit invalid_severity 1 'invalid vulnerability severity for mystery-package'

write_audit_json malformed_count <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":"0","critical":0,"total":0}}}
JSON
assert_fake_audit malformed_count 1 'invalid high count'

write_audit_json missing_info_count <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit missing_info_count 1 'invalid info count'

write_audit_json missing_total_count <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{},"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0}}}
JSON
assert_fake_audit missing_total_count 1 'invalid total count'

write_audit_json missing_vulnerabilities <<'JSON'
{"auditReportVersion":2,"metadata":{"vulnerabilities":{"info":0,"low":0,"moderate":0,"high":0,"critical":0,"total":0}}}
JSON
assert_fake_audit missing_vulnerabilities 1 'missing vulnerabilities'

write_audit_json missing_metadata <<'JSON'
{"auditReportVersion":2,"vulnerabilities":{}}
JSON
assert_fake_audit missing_metadata 1 'missing metadata.vulnerabilities'

write_audit_json invalid_json <<'JSON'
not-json
JSON
assert_fake_audit invalid_json 1 'could not validate npm audit JSON'

write_audit_json missing_json <<'JSON'
JSON
assert_fake_audit missing_json 1 'could not validate npm audit JSON' 1

assert_gate_fails_without_npm
assert_gate_fails_without_node

echo "Dashboard audit gate fixture tests passed"
