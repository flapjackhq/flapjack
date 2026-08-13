#!/usr/bin/env bash
#
# Live contract test for package/release_ci_status_preflight.
#
# This test intentionally reads immutable, known-answer GitHub Actions runs in
# flapjackhq/flapjack. It never dispatches a workflow, writes repository state,
# publishes an artifact, or uses a secret directly. Each pinned run is viewed
# and printed next to its verdict so a changed/deleted upstream specimen fails
# visibly instead of letting stale assumptions report green.
#
# Usage:
#   bash engine/tests/test_release_ci_status_preflight.sh

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PREFLIGHT="$REPO_DIR/engine/package/release_ci_status_preflight"
REPOSITORY="flapjackhq/flapjack"
WORKFLOW_FILE="ci.yml"

FAILED_SHA="35da0206f8d5cf567750da8d3c6fcb34859c5c69"
FAILED_RUN_ID="31096601354"
SUCCESS_SHA="fd0d5fefdc10e34ddb96bd7c594673fe5ae8341c"
SUCCESS_RUN_ID="31009411803"
NO_RUN_SHA="0000000000000000000000000000000000000000"
UPPERCASE_SUCCESS_SHA="$(printf '%s' "$SUCCESS_SHA" | tr '[:lower:]' '[:upper:]')"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-release-ci-preflight.XXXXXX")"

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1"
}

section() {
  printf '\n\033[1m%s\033[0m\n' "$1"
}

assert_exit_code() {
  local expected="$1"
  local actual="$2"
  local description="$3"
  if [ "$actual" = "$expected" ]; then
    pass "$description"
  else
    fail "$description (expected exit $expected, got $actual)"
    sed 's/^/        /' "$WORK_DIR/out.txt" 2>/dev/null | head -10
  fi
}

assert_output_contains() {
  local expected="$1"
  local description="$2"
  if grep -Fq "$expected" "$WORK_DIR/out.txt"; then
    pass "$description"
  else
    fail "$description (missing: $expected)"
    sed 's/^/        /' "$WORK_DIR/out.txt" 2>/dev/null | head -10
  fi
}

assert_output_excludes() {
  local rejected="$1"
  local description="$2"
  if grep -Fq "$rejected" "$WORK_DIR/out.txt"; then
    fail "$description (unexpected: $rejected)"
    sed 's/^/        /' "$WORK_DIR/out.txt" 2>/dev/null | head -10
  else
    pass "$description"
  fi
}

run_preflight() {
  "$PREFLIGHT" "$REPOSITORY" "$1" "$WORKFLOW_FILE" "$2" >"$WORK_DIR/out.txt" 2>&1
  echo "$?"
}

run_fake_preflight() {
  local scenario="$1"
  local acknowledgement="$2"
  FAKE_GH_SCENARIO="$scenario" \
  FAKE_TARGET_SHA="$FAILED_SHA" \
  FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" \
  PATH="$WORK_DIR/fake_bin:$PATH" \
    "$PREFLIGHT" "$REPOSITORY" "$FAILED_SHA" "$WORKFLOW_FILE" "$acknowledgement" >"$WORK_DIR/out.txt" 2>&1
  echo "$?"
}

capture_known_run() {
  local run_id="$1"
  local expected_sha="$2"
  local expected_conclusion="$3"
  local evidence_file="$WORK_DIR/run_${run_id}.json"

  if ! gh run view "$run_id" --repo "$REPOSITORY" \
    --json databaseId,headSha,event,conclusion,status >"$evidence_file"; then
    fail "run $run_id live evidence is readable"
    return
  fi

  printf '  Live evidence: %s\n' "$(cat "$evidence_file")"
  if python3 - "$run_id" "$expected_sha" "$expected_conclusion" "$evidence_file" <<'PY'
import json
import sys

expected_id, expected_sha, expected_conclusion = sys.argv[1:4]
with open(sys.argv[4], encoding="utf-8") as evidence:
    run = json.load(evidence)
expected = {
    "databaseId": int(expected_id),
    "headSha": expected_sha,
    "event": "push",
    "status": "completed",
    "conclusion": expected_conclusion,
}
raise SystemExit(0 if run == expected else 1)
PY
  then
    pass "run $run_id is the expected terminal push-run specimen"
  else
    fail "run $run_id is the expected terminal push-run specimen"
  fi
}

section "Preflight helper exists and is runnable"
if [ -x "$PREFLIGHT" ]; then
  pass "package/release_ci_status_preflight is executable"
else
  fail "package/release_ci_status_preflight is executable"
  printf '\n\033[0;31mHelper missing — remaining cases cannot run\033[0m\n'
  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  exit 1
fi

section "Pinned live run evidence"
capture_known_run "$FAILED_RUN_ID" "$FAILED_SHA" "failure"
capture_known_run "$SUCCESS_RUN_ID" "$SUCCESS_SHA" "success"

section "Validates input before invoking GitHub"
mkdir "$WORK_DIR/fake_bin"
cat >"$WORK_DIR/fake_bin/gh" <<'EOF'
#!/usr/bin/env bash
set -u

printf 'called\n' >>"$FAKE_CALL_LOG"
scenario="${FAKE_GH_SCENARIO:-api_error}"

resolved_failure() {
  printf '[{"databaseId":9001,"headSha":"%s","event":"push","status":"completed","conclusion":"failure"}]\n' "$FAKE_TARGET_SHA"
}

case "$scenario:$1:$2" in
  api_error:*)
    exit 99
    ;;
  nonterminal:run:list)
    printf '[{"databaseId":9001,"headSha":"%s","event":"push","status":"in_progress","conclusion":null}]\n' "$FAKE_TARGET_SHA"
    ;;
  cancelled:run:list)
    printf '[{"databaseId":9001,"headSha":"%s","event":"push","status":"completed","conclusion":"cancelled"}]\n' "$FAKE_TARGET_SHA"
    ;;
  same_sha_different_run:run:list|ack_not_failure:run:list|ack_unreadable:run:list|jobs_unreadable:run:list)
    resolved_failure
    ;;
  same_sha_different_run:run:view)
    printf '{"databaseId":9002,"headSha":"%s","event":"push","status":"completed","conclusion":"failure"}\n' "$FAKE_TARGET_SHA"
    ;;
  ack_not_failure:run:view)
    printf '{"databaseId":9001,"headSha":"%s","event":"push","status":"completed","conclusion":"success"}\n' "$FAKE_TARGET_SHA"
    ;;
  jobs_unreadable:run:view)
    printf '{"databaseId":9001,"headSha":"%s","event":"push","status":"completed","conclusion":"failure"}\n' "$FAKE_TARGET_SHA"
    ;;
  ack_unreadable:run:view|jobs_unreadable:api:*)
    exit 99
    ;;
  *)
    printf 'unexpected fake gh call: %s\n' "$*" >&2
    exit 98
    ;;
esac
EOF
chmod +x "$WORK_DIR/fake_bin/gh"

FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" PATH="$WORK_DIR/fake_bin:$PATH" "$PREFLIGHT" "$REPOSITORY" "$SUCCESS_SHA" "$WORKFLOW_FILE" >"$WORK_DIR/out.txt" 2>&1
assert_exit_code 1 "$?" "fewer than four arguments is a usage error"
FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" PATH="$WORK_DIR/fake_bin:$PATH" "$PREFLIGHT" "$REPOSITORY" "$UPPERCASE_SUCCESS_SHA" "$WORKFLOW_FILE" "" >"$WORK_DIR/out.txt" 2>&1
assert_exit_code 1 "$?" "an uppercase SHA is rejected before API access"
FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" PATH="$WORK_DIR/fake_bin:$PATH" "$PREFLIGHT" "" "$SUCCESS_SHA" "$WORKFLOW_FILE" "" >"$WORK_DIR/out.txt" 2>&1
assert_exit_code 1 "$?" "an empty repository is rejected before API access"
FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" PATH="$WORK_DIR/fake_bin:$PATH" "$PREFLIGHT" "$REPOSITORY" "$SUCCESS_SHA" "" "" >"$WORK_DIR/out.txt" 2>&1
assert_exit_code 1 "$?" "an empty workflow file is rejected before API access"
FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" PATH="$WORK_DIR/fake_bin:$PATH" "$PREFLIGHT" "$REPOSITORY" "$SUCCESS_SHA" "$WORKFLOW_FILE" "not-a-run-id" >"$WORK_DIR/out.txt" 2>&1
assert_exit_code 1 "$?" "a malformed acknowledgement id is rejected before API access"
if [ ! -e "$WORK_DIR/fake_gh_calls" ]; then
  pass "invalid invocations made no GitHub API call"
else
  fail "invalid invocations made no GitHub API call"
fi

section "Distinguishes an indeterminate API read"
FAKE_CALL_LOG="$WORK_DIR/fake_gh_calls" PATH="$WORK_DIR/fake_bin:$PATH" "$PREFLIGHT" "$REPOSITORY" "$SUCCESS_SHA" "$WORKFLOW_FILE" "" >"$WORK_DIR/out.txt" 2>&1
assert_exit_code 2 "$?" "a failed gh read uses the indeterminate-status exit code"
assert_output_contains "could not determine CI status" "an API failure is described as indeterminate"
assert_output_excludes "CI status is not success" "an API failure is not described as a terminal CI verdict"

section "Refuses every resolved state except terminal success"
code="$(run_fake_preflight "nonterminal" "")"
assert_exit_code 3 "$code" "an in-progress push run refuses release"
assert_output_contains "run 9001 has status in_progress and conclusion none" "non-terminal refusal names the live run state"

code="$(run_fake_preflight "cancelled" "")"
assert_exit_code 3 "$code" "a completed cancelled push run refuses release"
assert_output_contains "run 9001 has status completed and conclusion cancelled" "non-success terminal refusal names the live conclusion"

section "Acknowledgement remains narrow under inconsistent API states"
code="$(run_fake_preflight "ack_unreadable" "9001")"
assert_exit_code 2 "$code" "an unreadable acknowledged run is indeterminate, never allowed"
assert_output_contains "acknowledged run 9001 could not be verified" "unreadable acknowledgement identifies the failed verification"
assert_output_contains "resolved run 9001 has status completed and conclusion failure" "acknowledgement read failure retains the resolved run state"

code="$(run_fake_preflight "same_sha_different_run" "9002")"
assert_exit_code 3 "$code" "a failed run at the same SHA cannot acknowledge a different resolved run"
assert_output_contains "acknowledged run 9002 is not resolved push CI run 9001" "same-SHA refusal enforces exact resolved-run identity"

code="$(run_fake_preflight "ack_not_failure" "9001")"
assert_exit_code 3 "$code" "the resolved run cannot be acknowledged without a live failure conclusion"
assert_output_contains "conclusion success, not terminal failure" "non-failure acknowledgement explains the rejected conclusion"

code="$(run_fake_preflight "jobs_unreadable" "9001")"
assert_exit_code 2 "$code" "acknowledgement refuses when failing job names are unreadable"
assert_output_contains "failing jobs for acknowledged run 9001 could not be read" "job API failure is described as indeterminate"

section "Refuses a known terminal failure without acknowledgement"
code="$(run_preflight "$FAILED_SHA" "")"
assert_exit_code 3 "$code" "failed run $FAILED_RUN_ID refuses release without acknowledgement"
assert_output_contains "run $FAILED_RUN_ID has status completed and conclusion failure" "failure verdict names the resolved run and state"
assert_output_contains "CI status is not success" "terminal failure is described as a CI verdict"
assert_output_excludes "could not determine CI status" "terminal failure is not described as an API read problem"

section "Refuses an acknowledgement from a different SHA"
code="$(run_preflight "$FAILED_SHA" "$SUCCESS_RUN_ID")"
assert_exit_code 3 "$code" "run $SUCCESS_RUN_ID cannot acknowledge failed run $FAILED_RUN_ID"
assert_output_contains "acknowledged run $SUCCESS_RUN_ID has head SHA $SUCCESS_SHA, not $FAILED_SHA" "mismatched acknowledgement explains the SHA mismatch"
assert_output_contains "resolved run $FAILED_RUN_ID has status completed and conclusion failure" "mismatched acknowledgement retains the resolved run state"

section "Allows a known terminal success"
code="$(run_preflight "$SUCCESS_SHA" "")"
assert_exit_code 0 "$code" "terminal-success run $SUCCESS_RUN_ID allows release"
assert_output_contains "run $SUCCESS_RUN_ID is terminal success" "success verdict names the resolved run"

section "Allows only the resolved failed run when acknowledged"
code="$(run_preflight "$FAILED_SHA" "$FAILED_RUN_ID")"
assert_exit_code 0 "$code" "resolved failed run $FAILED_RUN_ID is allowed when explicitly acknowledged"
assert_output_contains "Rust tests (all)" "acknowledgement logs the first failing job name"
assert_output_contains "Dashboard full e2e tests" "acknowledgement logs the second failing job name"

section "Refuses when no terminal or active push run exists"
if no_run_evidence="$(gh run list --repo "$REPOSITORY" --workflow "$WORKFLOW_FILE" \
  --commit "$NO_RUN_SHA" --event push --json databaseId,headSha,event,status,conclusion)"; then
  printf '  Live no-terminal-run evidence (no run observed, not in_progress): %s\n' "$no_run_evidence"
  if [ "$no_run_evidence" = "[]" ]; then
    pass "the no-run SHA currently has no push CI run"
  else
    fail "the no-run SHA currently has no push CI run"
  fi
else
  fail "the no-run live evidence is readable"
fi

code="$(run_preflight "$NO_RUN_SHA" "")"
assert_exit_code 3 "$code" "a SHA with no push CI run refuses release"
assert_output_contains "no push CI run found" "no-run refusal is distinct from terminal failure"
assert_output_excludes "run $FAILED_RUN_ID has status completed and conclusion failure" "no-run refusal does not reuse the terminal-failure verdict"

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
