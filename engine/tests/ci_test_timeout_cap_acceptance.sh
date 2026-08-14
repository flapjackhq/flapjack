#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
CI_WORKFLOW="$REPO_DIR/.github/workflows/ci.yml"
NIGHTLY_WORKFLOW="$REPO_DIR/.github/workflows/nightly.yml"

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
}

section() {
  printf '\n\033[1m%s\033[0m\n' "$1"
}

assert_contains() {
  local file_path="$1"
  local pattern="$2"
  local description="$3"
  if grep -Eq "$pattern" "$file_path"; then
    pass "$description"
  else
    fail "$description"
  fi
}

emit_binding_signals() {
  printf '__BINDING_SIGNAL__job_block_unique=%s\n' "$1"
  printf '__BINDING_SIGNAL__canonical_prebuild_present=%s\n' "$2"
  printf '__BINDING_SIGNAL__canonical_capped_present=%s\n' "$3"
  printf '__BINDING_SIGNAL__prebuild_before_capped=%s\n' "$4"
}

read_numeric_signal() {
  local signal_output="$1"
  local signal_name="$2"
  printf '%s\n' "$signal_output" | awk -F= -v signal_name="$signal_name" '
    $1 == signal_name {
      value = $2
    }
    END {
      print value + 0
    }
  '
}

read_named_signal() {
  local signal_output="$1"
  local signal_name="$2"
  printf '%s\n' "$signal_output" | awk -F= -v signal_name="$signal_name" '
    $1 == signal_name {
      value = $2
    }
    END {
      print value
    }
  '
}

extract_matching_step_block() {
  local file_path="$1"
  local step_name="$2"
  local command_signature="$3"
  awk -v step="$step_name" -v command="$command_signature" '
    function flush_candidate(   normalized) {
      if (have_candidate != 1) {
        return
      }
      normalized = candidate_run
      gsub(/[[:space:]]+/, " ", normalized)
      sub(/^ /, "", normalized)
      sub(/ $/, "", normalized)
      if (candidate_name == step && normalized == command) {
        if (match_count == 0) {
          printf "%s", candidate_block
          first_match_start_line = candidate_start_line
        }
        match_count++
      }
      have_candidate = 0
      candidate_name = ""
      candidate_block = ""
      candidate_run = ""
      candidate_start_line = 0
      in_multiline_run = 0
      multiline_indent = -1
    }

    {
      line = $0
      line_indent = match(line, /[^ ]/) - 1
      if (line ~ /^[[:space:]]*$/) {
        line_indent = -1
      }

      if (have_candidate == 1 && in_multiline_run == 1) {
        if (line ~ /^[[:space:]]*$/) {
          # Ignore blank lines inside multiline run values.
        } else if (line_indent <= multiline_indent && line ~ /^[[:space:]]*[^#-]/) {
          in_multiline_run = 0
        } else if (line_indent > multiline_indent) {
          run_line = line
          sub(/^[[:space:]]+/, "", run_line)
          if (run_line ~ /^#/) {
            next
          }
          if (candidate_run != "") {
            candidate_run = candidate_run " " run_line
          } else {
            candidate_run = run_line
          }
          next
        }
      }

      if (line ~ /^[[:space:]]*-[[:space:]]*name:[[:space:]]*/) {
        flush_candidate()
        have_candidate = 1
        candidate_start_line = NR
        candidate_block = line "\n"
        candidate_name = line
        sub(/^[[:space:]]*-[[:space:]]*name:[[:space:]]*/, "", candidate_name)
        next
      }

      if (have_candidate == 1) {
        if (line ~ /^[[:space:]]*-[[:space:]]*[^ ]/) {
          flush_candidate()
        }
      }

      if (have_candidate == 1) {
        candidate_block = candidate_block line "\n"

        if (line ~ /^[[:space:]]*run:[[:space:]]*[^>|].*$/) {
          run_line = line
          sub(/^[[:space:]]*run:[[:space:]]*/, "", run_line)
          candidate_run = run_line
          next
        }

        if (line ~ /^[[:space:]]*run:[[:space:]]*[>|][[:space:]]*$/) {
          in_multiline_run = 1
          multiline_indent = line_indent
          next
        }
      }
    }

    END {
      flush_candidate()
      printf "\n__MATCH_COUNT__=%d\n", match_count
      printf "__FIRST_MATCH_START_LINE__=%d\n", first_match_start_line
    }
  ' "$file_path"
}

extract_matching_job_block() {
  local file_path="$1"
  local job_key="$2"
  awk -v target_job="$job_key" '
    function flush_target() {
      if (in_target_job == 1) {
        if (match_count == 0) {
          printf "%s", target_block
        }
        match_count++
      }
      in_target_job = 0
      target_block = ""
    }

    {
      line = $0
      if (line ~ /^  [A-Za-z0-9_-]+:[[:space:]]*$/) {
        job_name = line
        sub(/^  /, "", job_name)
        sub(/:[[:space:]]*$/, "", job_name)
        if (in_target_job == 1 && job_name != target_job) {
          flush_target()
        }
        if (job_name == target_job) {
          in_target_job = 1
          target_block = line "\n"
          next
        }
      }

      if (in_target_job == 1) {
        target_block = target_block line "\n"
      }
    }

    END {
      flush_target()
      printf "\n__JOB_MATCH_COUNT__=%d\n", match_count
    }
  ' "$file_path"
}

assert_capped_vector_job_prebuild_binding() {
  local workflow_file="$1"
  local workflow_label="$2"
  local job_key="$3"
  local capped_step_name="$4"
  local capped_step_command="$5"
  local prebuild_step_name="Build vector-search test binaries"
  local prebuild_command="cargo build --tests -p flapjack -p flapjack-http --features vector-search"
  local signal_job_block_unique="0"
  local signal_canonical_prebuild_present="0"
  local signal_canonical_capped_present="0"
  local signal_prebuild_before_capped="0"

  local job_block_file
  job_block_file="$(mktemp)"
  extract_matching_job_block "$workflow_file" "$job_key" > "$job_block_file"

  local job_match_count
  job_match_count="$(grep -Eo '__JOB_MATCH_COUNT__=[0-9]+' "$job_block_file" | head -n1 | cut -d= -f2)"
  job_match_count="${job_match_count:-0}"

  local sanitized_job_block
  sanitized_job_block="$(mktemp)"
  grep -v '^__' "$job_block_file" > "$sanitized_job_block"

  if [ "$job_match_count" -eq 1 ]; then
    signal_job_block_unique="1"
    pass "$workflow_label defines exactly one '$job_key' job block for vector-search timeout assertions"
  else
    fail "$workflow_label defines exactly one '$job_key' job block for vector-search timeout assertions"
    if [ "${EMIT_BINDING_SIGNALS:-0}" = "1" ]; then
      emit_binding_signals \
        "$signal_job_block_unique" \
        "$signal_canonical_prebuild_present" \
        "$signal_canonical_capped_present" \
        "$signal_prebuild_before_capped"
    fi
    rm -f "$job_block_file" "$sanitized_job_block"
    return
  fi

  local prebuild_block_file
  prebuild_block_file="$(mktemp)"
  extract_matching_step_block "$sanitized_job_block" "$prebuild_step_name" "$prebuild_command" > "$prebuild_block_file"
  local prebuild_match_count
  prebuild_match_count="$(grep -Eo '__MATCH_COUNT__=[0-9]+' "$prebuild_block_file" | head -n1 | cut -d= -f2)"
  prebuild_match_count="${prebuild_match_count:-0}"
  if [ "$prebuild_match_count" -eq 1 ]; then
    signal_canonical_prebuild_present="1"
    pass "$workflow_label '$job_key' contains '$prebuild_step_name' with canonical vector-search prebuild command"
  else
    fail "$workflow_label '$job_key' contains '$prebuild_step_name' with canonical vector-search prebuild command"
  fi

  local capped_block_file
  capped_block_file="$(mktemp)"
  extract_matching_step_block "$sanitized_job_block" "$capped_step_name" "$capped_step_command" > "$capped_block_file"
  local capped_match_count
  capped_match_count="$(grep -Eo '__MATCH_COUNT__=[0-9]+' "$capped_block_file" | head -n1 | cut -d= -f2)"
  capped_match_count="${capped_match_count:-0}"
  if [ "$capped_match_count" -eq 1 ]; then
    signal_canonical_capped_present="1"
    pass "$workflow_label '$job_key' contains capped vector-search step '$capped_step_name' with canonical nextest command"
  else
    fail "$workflow_label '$job_key' contains capped vector-search step '$capped_step_name' with canonical nextest command"
  fi

  local prebuild_match_start_line
  prebuild_match_start_line="$(grep -Eo '__FIRST_MATCH_START_LINE__=[0-9]+' "$prebuild_block_file" | head -n1 | cut -d= -f2)"
  prebuild_match_start_line="${prebuild_match_start_line:-0}"
  local capped_match_start_line
  capped_match_start_line="$(grep -Eo '__FIRST_MATCH_START_LINE__=[0-9]+' "$capped_block_file" | head -n1 | cut -d= -f2)"
  capped_match_start_line="${capped_match_start_line:-0}"

  local ordering_ok="0"
  if [ "$prebuild_match_start_line" -gt 0 ] && [ "$capped_match_start_line" -gt 0 ] && [ "$prebuild_match_start_line" -lt "$capped_match_start_line" ]; then
    ordering_ok="1"
  fi
  if [ "$ordering_ok" = "1" ]; then
    signal_prebuild_before_capped="1"
    pass "$workflow_label '$job_key' runs '$prebuild_step_name' before '$capped_step_name'"
  else
    fail "$workflow_label '$job_key' runs '$prebuild_step_name' before '$capped_step_name'"
  fi

  if [ "${EMIT_BINDING_SIGNALS:-0}" = "1" ]; then
    emit_binding_signals \
      "$signal_job_block_unique" \
      "$signal_canonical_prebuild_present" \
      "$signal_canonical_capped_present" \
      "$signal_prebuild_before_capped"
  fi

  rm -f "$job_block_file" "$sanitized_job_block" "$prebuild_block_file" "$capped_block_file"
}

assert_duplicate_step_name_regression_fixture() {
  local fixture_file
  fixture_file="$(mktemp)"
  cat > "$fixture_file" <<'YAML'
jobs:
  fixture-job:
    runs-on: ubuntu-latest
    steps:
      - name: Build vector-search test binaries
        run: cargo build -p flapjack
      - name: Fast tests (vector-search)
        run: cargo nextest run -p flapjack
      - name: Fast tests (vector-search)
        run: cargo nextest run -p flapjack -p flapjack-http --features vector-search
      - name: Build vector-search test binaries
        run: cargo build --tests -p flapjack -p flapjack-http --features vector-search
YAML

  local binding_output
  binding_output="$(
    EMIT_BINDING_SIGNALS=1 assert_capped_vector_job_prebuild_binding \
      "$fixture_file" \
      "fixture.yml" \
      "fixture-job" \
      "Fast tests (vector-search)" \
      "cargo nextest run -p flapjack -p flapjack-http --features vector-search"
  )"

  local signal_job_block_unique
  signal_job_block_unique="$(read_numeric_signal "$binding_output" "__BINDING_SIGNAL__job_block_unique")"
  local signal_canonical_prebuild_present
  signal_canonical_prebuild_present="$(read_numeric_signal "$binding_output" "__BINDING_SIGNAL__canonical_prebuild_present")"
  local signal_canonical_capped_present
  signal_canonical_capped_present="$(read_numeric_signal "$binding_output" "__BINDING_SIGNAL__canonical_capped_present")"
  local signal_prebuild_before_capped
  signal_prebuild_before_capped="$(read_numeric_signal "$binding_output" "__BINDING_SIGNAL__prebuild_before_capped")"

  local fixture_outcome="fail"
  if [ "$signal_job_block_unique" -eq 1 ] && [ "$signal_canonical_prebuild_present" -eq 1 ] && [ "$signal_canonical_capped_present" -eq 1 ] && [ "$signal_prebuild_before_capped" -eq 0 ]; then
    fixture_outcome="pass"
    pass "duplicate step-name fixture rejects canonical prebuild-after-capped ordering"
  else
    fail "duplicate step-name fixture rejects canonical prebuild-after-capped ordering"
  fi

  if [ "${EMIT_DUPLICATE_FIXTURE_OUTCOME:-0}" = "1" ]; then
    printf '__DUPLICATE_FIXTURE_OUTCOME__=%s\n' "$fixture_outcome"
  fi

  rm -f "$fixture_file"
}

assert_duplicate_step_fixture_guards_wrong_failure_mode() {
  local original_binding_function
  original_binding_function="$(declare -f assert_capped_vector_job_prebuild_binding)"

  # Simulate a broken canonical extractor that still emits the expected ordering failure.
  assert_capped_vector_job_prebuild_binding() {
    printf '  [FAIL] fixture.yml '\''fixture-job'\'' contains '\''Build vector-search test binaries'\'' with canonical vector-search prebuild command\n'
    printf '  [FAIL] fixture.yml '\''fixture-job'\'' contains capped vector-search step '\''Fast tests (vector-search)'\'' with canonical nextest command\n'
    printf '  [FAIL] fixture.yml '\''fixture-job'\'' runs '\''Build vector-search test binaries'\'' before '\''Fast tests (vector-search)'\''\n'
  }

  local fixture_output
  fixture_output="$(EMIT_DUPLICATE_FIXTURE_OUTCOME=1 assert_duplicate_step_name_regression_fixture)"
  local fixture_outcome
  fixture_outcome="$(read_named_signal "$fixture_output" "__DUPLICATE_FIXTURE_OUTCOME__")"
  if [ "$fixture_outcome" = "fail" ]; then
    pass "duplicate step-name fixture rejects ordering failures when canonical binding checks fail"
  else
    fail "duplicate step-name fixture rejects ordering failures when canonical binding checks fail"
  fi

  eval "$original_binding_function"
}

assert_duplicate_step_fixture_ignores_helper_prose_changes() {
  local original_binding_function
  original_binding_function="$(declare -f assert_capped_vector_job_prebuild_binding)"

  # Simulate helper message rewording while preserving canonical binding outcomes.
  assert_capped_vector_job_prebuild_binding() {
    printf '__BINDING_SIGNAL__job_block_unique=1\n'
    printf '__BINDING_SIGNAL__canonical_prebuild_present=1\n'
    printf '__BINDING_SIGNAL__canonical_capped_present=1\n'
    printf '__BINDING_SIGNAL__prebuild_before_capped=0\n'
    printf 'CHECK: canonical prebuild located\n'
    printf 'CHECK: canonical capped step located\n'
    printf 'CHECK: ordering mismatch detected\n'
  }

  local fixture_output
  fixture_output="$(EMIT_DUPLICATE_FIXTURE_OUTCOME=1 assert_duplicate_step_name_regression_fixture)"
  local fixture_outcome
  fixture_outcome="$(read_named_signal "$fixture_output" "__DUPLICATE_FIXTURE_OUTCOME__")"
  if [ "$fixture_outcome" = "pass" ]; then
    pass "duplicate step-name fixture accepts structured canonical-binding signals despite helper prose changes"
  else
    fail "duplicate step-name fixture accepts structured canonical-binding signals despite helper prose changes"
  fi

  eval "$original_binding_function"
}

assert_step_contract() {
  local workflow_file="$1"
  local workflow_label="$2"
  local step_name="$3"
  local command_signature="$4"
  local expected_timeout="${5:-10}"

  local block_file
  block_file="$(mktemp)"
  extract_matching_step_block "$workflow_file" "$step_name" "$command_signature" > "$block_file"

  local match_count
  match_count="$(grep -Eo '__MATCH_COUNT__=[0-9]+' "$block_file" | head -n1 | cut -d= -f2)"
  match_count="${match_count:-0}"

  if [ "$match_count" -eq 1 ]; then
    pass "$workflow_label resolves '$step_name' to exactly one step using canonical command"
    pass "$workflow_label defines target step '$step_name' with canonical nextest command"
  else
    fail "$workflow_label resolves '$step_name' to exactly one step using canonical command"
    fail "$workflow_label defines target step '$step_name' with canonical nextest command"
    rm -f "$block_file"
    return
  fi

  if grep -v '^__' "$block_file" | grep -Eq "^[[:space:]]*timeout-minutes:[[:space:]]*${expected_timeout}$"; then
    pass "$workflow_label '$step_name' enforces timeout-minutes: $expected_timeout"
  else
    fail "$workflow_label '$step_name' enforces timeout-minutes: $expected_timeout"
  fi

  rm -f "$block_file"
}

assert_job_contains_pattern() {
  local workflow_file="$1"
  local workflow_label="$2"
  local job_key="$3"
  local pattern="$4"
  local description="$5"

  local job_block_file
  job_block_file="$(mktemp)"
  extract_matching_job_block "$workflow_file" "$job_key" > "$job_block_file"

  local job_match_count
  job_match_count="$(grep -Eo '__JOB_MATCH_COUNT__=[0-9]+' "$job_block_file" | head -n1 | cut -d= -f2)"
  job_match_count="${job_match_count:-0}"
  if [ "$job_match_count" -ne 1 ]; then
    fail "$description"
    rm -f "$job_block_file"
    return
  fi

  if grep -v '^__' "$job_block_file" | grep -Eq "$pattern"; then
    pass "$description"
  else
    fail "$description"
  fi

  rm -f "$job_block_file"
}

assert_named_step_order_in_job() {
  local workflow_file="$1"
  local workflow_label="$2"
  local job_key="$3"
  local first_step_name="$4"
  local second_step_name="$5"

  local job_block_file
  job_block_file="$(mktemp)"
  extract_matching_job_block "$workflow_file" "$job_key" > "$job_block_file"

  local job_match_count
  job_match_count="$(grep -Eo '__JOB_MATCH_COUNT__=[0-9]+' "$job_block_file" | head -n1 | cut -d= -f2)"
  job_match_count="${job_match_count:-0}"
  if [ "$job_match_count" -ne 1 ]; then
    fail "$workflow_label '$job_key' defines named steps '$first_step_name' and '$second_step_name' in order"
    rm -f "$job_block_file"
    return
  fi

  local sanitized_job_block
  sanitized_job_block="$(mktemp)"
  grep -v '^__' "$job_block_file" > "$sanitized_job_block"

  local first_line
  first_line="$(grep -nF -- "- name: $first_step_name" "$sanitized_job_block" | head -n1 | cut -d: -f1)"
  local second_line
  second_line="$(grep -nF -- "- name: $second_step_name" "$sanitized_job_block" | head -n1 | cut -d: -f1)"
  first_line="${first_line:-0}"
  second_line="${second_line:-0}"

  if [ "$first_line" -gt 0 ] && [ "$second_line" -gt 0 ] && [ "$first_line" -lt "$second_line" ]; then
    pass "$workflow_label '$job_key' runs '$first_step_name' before '$second_step_name'"
  else
    fail "$workflow_label '$job_key' runs '$first_step_name' before '$second_step_name'"
  fi

  rm -f "$job_block_file" "$sanitized_job_block"
}

section "Rust test timeout acceptance contract"
assert_duplicate_step_name_regression_fixture
assert_duplicate_step_fixture_guards_wrong_failure_mode
assert_duplicate_step_fixture_ignores_helper_prose_changes
assert_step_contract "$CI_WORKFLOW" "ci.yml" "Build candidate core test binary" 'cargo test -p flapjack --lib --no-run --message-format=json > candidate_build.json && python3 tests/extract_candidate_binary.py candidate_build.json "$GITHUB_OUTPUT"' "30"
assert_step_contract "$CI_WORKFLOW" "ci.yml" "Candidate test-tier inventory" "python3 tests/test_ci_test_tiers.py --verify" "10"
assert_step_contract "$CI_WORKFLOW" "ci.yml" "Candidate core tests" 'timeout --kill-after=60s 300s "${{ steps.candidate-build.outputs.executable }}"' "6"
assert_step_contract "$CI_WORKFLOW" "ci.yml" "All tests (vector-search)" "cargo nextest run -p flapjack -p flapjack-http --features vector-search -P ci --no-fail-fast" "20"
assert_step_contract "$CI_WORKFLOW" "ci.yml" "All tests (remaining)" "cargo nextest run -p flapjack-server -p flapjack-ssl -p flapjack-replication -P ci --no-fail-fast"
assert_job_contains_pattern "$CI_WORKFLOW" "ci.yml" "rust-tests-all" '^[[:space:]]*tool:[[:space:]]*cargo-audit,cargo-deny[[:space:]]*$' "ci.yml 'rust-tests-all' installs cargo-audit and cargo-deny before running vector-search tests"
assert_named_step_order_in_job "$CI_WORKFLOW" "ci.yml" "rust-tests-all" "Install cargo audit/deny tools for security audit tests" "All tests (vector-search)"
assert_capped_vector_job_prebuild_binding "$CI_WORKFLOW" "ci.yml" "rust-tests-all" "All tests (vector-search)" "cargo nextest run -p flapjack -p flapjack-http --features vector-search -P ci --no-fail-fast"
assert_step_contract "$NIGHTLY_WORKFLOW" "nightly.yml" "Run all tests" "cargo nextest run -P ci" "45"

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
