#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HELPER_PATH="$SCRIPT_DIR/load_named_secrets.sh"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2"
  fi
}

assert_helper_file_contract() {
  if [ -f "$HELPER_PATH" ]; then
    pass 'load_named_secrets helper file exists'
  else
    fail 'load_named_secrets helper file exists' "$HELPER_PATH"
    return
  fi

  if [ -x "$HELPER_PATH" ]; then
    pass 'load_named_secrets helper is executable'
  else
    fail 'load_named_secrets helper is executable'
  fi

  local first_line
  first_line="$(head -n 1 "$HELPER_PATH")"
  if [ "$first_line" = '#!/usr/bin/env bash' ]; then
    pass 'load_named_secrets helper has bash shebang'
  else
    fail 'load_named_secrets helper has bash shebang' "$first_line"
  fi
}

assert_missing_file_fails_without_secret_output() {
  local work_dir output_file missing_file secret_value
  work_dir="$(mktemp -d)"
  output_file="$work_dir/output.txt"
  missing_file="$work_dir/missing.env"
  secret_value="super-secret-missing-file"

  if SECRET_VALUE="$secret_value" bash -c 'source "$1"; load_named_secrets "$2" SECRET_VALUE' _ "$HELPER_PATH" "$missing_file" >"$output_file" 2>&1; then
    fail 'load_named_secrets fails when the secret file is missing'
  elif grep -q "$secret_value" "$output_file"; then
    fail 'load_named_secrets missing-file failure does not print secret values' "$(cat "$output_file")"
  else
    pass 'load_named_secrets fails when the secret file is missing'
  fi

  rm -rf "$work_dir"
}

assert_missing_key_fails_without_secret_output() {
  local work_dir env_file output_file secret_value
  work_dir="$(mktemp -d)"
  env_file="$work_dir/.env.secret"
  output_file="$work_dir/output.txt"
  secret_value="super-secret-present-value"
  printf 'PRESENT_SECRET=%s\n' "$secret_value" >"$env_file"

  if bash -c 'source "$1"; load_named_secrets "$2" PRESENT_SECRET MISSING_SECRET' _ "$HELPER_PATH" "$env_file" >"$output_file" 2>&1; then
    fail 'load_named_secrets fails when any requested key is missing'
  elif grep -q "$secret_value" "$output_file"; then
    fail 'load_named_secrets missing-key failure does not print secret values' "$(cat "$output_file")"
  else
    pass 'load_named_secrets fails when any requested key is missing'
  fi

  rm -rf "$work_dir"
}

assert_omitted_path_is_usage_failure() {
  local work_dir output_file secret_value
  work_dir="$(mktemp -d)"
  output_file="$work_dir/output.txt"
  secret_value="super-secret-usage"

  if SECRET_VALUE="$secret_value" bash "$HELPER_PATH" >"$output_file" 2>&1; then
    fail 'load_named_secrets without a path is a usage failure'
  elif ! grep -q 'Usage:' "$output_file"; then
    fail 'load_named_secrets usage failure prints usage' "$(cat "$output_file")"
  elif grep -q "$secret_value" "$output_file"; then
    fail 'load_named_secrets usage failure does not print secret values' "$(cat "$output_file")"
  else
    pass 'load_named_secrets without a path is a usage failure'
  fi

  rm -rf "$work_dir"
}

assert_sourcing_does_not_consume_caller_args() {
  local work_dir output_file secret_value
  work_dir="$(mktemp -d)"
  output_file="$work_dir/output.txt"
  secret_value="caller-secret-arg"

  if bash -c 'helper_path="$1"; expected_arg="$3"; set -- "$expected_arg"; source "$helper_path"; [ "$1" = "$expected_arg" ] && [ -z "${SECRET_VALUE:-}" ]' _ "$HELPER_PATH" "$work_dir/unused.env" "$secret_value" >"$output_file" 2>&1; then
    pass 'sourcing load_named_secrets does not consume caller arguments'
  else
    fail 'sourcing load_named_secrets does not consume caller arguments' "$(cat "$output_file")"
  fi

  rm -rf "$work_dir"
}

assert_success_exports_without_output() {
  local work_dir env_file output_file secret_value quoted_value
  work_dir="$(mktemp -d)"
  env_file="$work_dir/.env.secret"
  output_file="$work_dir/output.txt"
  secret_value="super-secret-success"
  quoted_value="quoted secret success"
  {
    printf 'PLAIN_SECRET=%s\n' "$secret_value"
    printf 'QUOTED_SECRET="%s"\n' "$quoted_value"
  } >"$env_file"

  if bash -c 'source "$1"; load_named_secrets "$2" PLAIN_SECRET QUOTED_SECRET; [ "$PLAIN_SECRET" = "$3" ] && [ "$QUOTED_SECRET" = "$4" ]' _ "$HELPER_PATH" "$env_file" "$secret_value" "$quoted_value" >"$output_file" 2>&1; then
    if [ -s "$output_file" ]; then
      fail 'load_named_secrets successful load is silent' "$(cat "$output_file")"
    else
      pass 'load_named_secrets successful load is silent'
    fi
  else
    fail 'load_named_secrets exports requested keys' "$(cat "$output_file")"
  fi

  rm -rf "$work_dir"
}

main() {
  echo 'load_named_secrets shared secret loader test'
  assert_helper_file_contract
  assert_missing_file_fails_without_secret_output
  assert_missing_key_fails_without_secret_output
  assert_omitted_path_is_usage_failure
  assert_sourcing_does_not_consume_caller_args
  assert_success_exports_without_output

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '%d test(s) failed\n' "$TESTS_FAILED"
    return 1
  fi
  echo 'All tests passed'
}

main "$@"
