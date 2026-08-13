#!/usr/bin/env bash

# Process-lifecycle contract for the ignored Meilisearch live-preview test.
#
# The in-test deadline in `flapjack-http/src/handlers/migration/preview_tests.rs`
# exists to protect the *test process*, not the request: an upstream that accepts
# a connection and never answers used to wedge the test binary forever, and when
# `cargo` was killed the binary re-parented to `init` and survived unkillably.
# This script is the only owner of process-table matching, exact-child selection,
# terminal-state classification, and orphan baseline subtraction for that claim.
# Nextest, host-health probes, the served-CLI KAT, and production migration code
# must not grow a second copy of this logic.
#
# Modes:
#   --self-test               synthetic arms that prove the matchers can go red
#   --snapshot FILE           write the current lane-target PPID=1 identity set
#   --subtract BEFORE AFTER DELTA   set-difference; nonzero exit on any new orphan
#   (no argument)             the live specimen against a loopback blackhole
#
# There is no skip-success path: a missing tool, an ambiguous child, or an
# unclassifiable terminal state exits non-zero.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

: "${CARGO_TARGET_DIR:=$ENGINE_DIR/target}"
export CARGO_TARGET_DIR

LANE_TARGET_PREFIX="$CARGO_TARGET_DIR/debug/deps/flapjack_http-"
LIVE_TEST_NAME="handlers::migration::preview_tests::meilisearch_live_preview_reports_exact_seeded_counts_and_codes"
DEADLINE_MESSAGE="Meilisearch live preview request exceeded its in-test deadline"
PREVIEW_ENDPOINT_ENV="FJ_MEILISEARCH_PREVIEW_ENDPOINT"
PREVIEW_API_KEY_ENV="FJ_MEILISEARCH_PREVIEW_API_KEY"
PREVIEW_EXPECTED_RECORDS_ENV="FJ_MEILISEARCH_PREVIEW_EXPECTED_RECORDS"
# Non-secret canary: the blackhole never reads it, and a real key must never
# reach a fixture that logs its own environment.
PREVIEW_CANARY_API_KEY="process-deadline-contract-canary-not-a-secret"
PREVIEW_EXPECTED_RECORDS=3

CHILD_DISCOVERY_TIMEOUT_SECONDS=60
CONNECTION_WAIT_TIMEOUT_SECONDS=60
TERMINAL_STATE_TIMEOUT_SECONDS=60
POLL_INTERVAL_SECONDS=0.2

TMP_ROOT=""
CARGO_PID=""
BLACKHOLE_PID=""

die() {
  printf 'FAIL %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local script_exit_code=$?
  # Scoped to the exact PIDs this script started. Never the observed Rust child:
  # its survival or absence is the measurement.
  if [[ -n "$CARGO_PID" ]]; then
    kill -KILL "$CARGO_PID" 2>/dev/null || true
  fi
  if [[ -n "$BLACKHOLE_PID" ]]; then
    kill -KILL "$BLACKHOLE_PID" 2>/dev/null || true
  fi
  trap - EXIT
  exit "$script_exit_code"
}

# ---------------------------------------------------------------------------
# Shared process-table primitives. Every mode below reuses these; synthetic
# tables in --self-test go through the identical code path as live `ps` output.
# Canonical row format is TSV: PID, PPID, START_IDENTITY, COMMAND.
# START_IDENTITY is the process start timestamp with spaces folded to `_`, so a
# recycled PID with a different start time is a different identity.
# ---------------------------------------------------------------------------

capture_process_table() {
  local out="$1"
  # `-ww` defeats width-based truncation of the command column: the full exact
  # test name lives past column 80 and is load-bearing for child selection.
  ps -ww -eo pid=,ppid=,lstart=,command= | awk '
    {
      start = $3 "_" $4 "_" $5 "_" $6 "_" $7
      command = ""
      for (field = 8; field <= NF; field++) {
        command = command (field > 8 ? " " : "") $field
      }
      printf "%s\t%s\t%s\t%s\n", $1, $2, start, command
    }
  ' >"$out"
}

# Rows whose executable is this lane's own `--lib` test binary. The hex suffix
# check keeps a differently named binary under the same deps directory out.
lane_target_rows() {
  awk -F'\t' -v prefix="$LANE_TARGET_PREFIX" '
    {
      position = index($4, prefix)
      if (position == 0) next
      remainder = substr($4, position + length(prefix))
      if (remainder !~ /^[0-9a-f]+/) next
      print
    }
  ' "$1"
}

# A direct child of the recorded cargo PID that is the lane target binary running
# the full exact test name. All four facts are required: a sibling test binary, a
# same-named test under a foreign target dir, and a same-shaped process under a
# different parent are each rejected.
select_exact_children() {
  local table="$1" parent_pid="$2"
  lane_target_rows "$table" \
    | awk -F'\t' -v parent="$parent_pid" -v test_name="$LIVE_TEST_NAME" \
      '$2 == parent && index($4, test_name) > 0'
}

row_identity() {
  awk -F'\t' '{ printf "pid=%s start=%s\n", $1, $3 }'
}

# PID plus start time, so a recycled PID never reads as the recorded child.
identity_present() {
  local table="$1" identity="$2"
  lane_target_rows "$table" | row_identity | grep -Fqx "$identity"
}

identity_reparented() {
  local table="$1" identity="$2"
  lane_target_rows "$table" | awk -F'\t' '$2 == 1' | row_identity | grep -Fqx "$identity"
}

lane_target_orphan_identities() {
  local table="$1"
  lane_target_rows "$table" | awk -F'\t' '$2 == 1' | row_identity | LC_ALL=C sort -u
}

# Set difference: identities orphaned after the run that were not already
# orphaned before it. Pre-existing residue from older lanes is not this lane's.
new_orphan_identities() {
  local before="$1" after="$2" delta="$3"
  LC_ALL=C comm -13 \
    <(LC_ALL=C sort -u "$before") \
    <(LC_ALL=C sort -u "$after") >"$delta"
  wc -l <"$delta" | tr -d ' '
}

# The only permitted terminal states. `parent_signal` requires the child to have
# died with its parent; `deadline` requires it to have outlived cargo, announced
# the in-test deadline, and only then disappeared.
classify_child_terminal_state() {
  local reparented="$1" absent="$2" deadline_message="$3"
  if [[ "$absent" != 1 ]]; then
    printf 'still_running\n'
  elif [[ "$reparented" != 1 ]]; then
    printf 'parent_signal\n'
  elif [[ "$deadline_message" == 1 ]]; then
    printf 'deadline\n'
  else
    printf 'reparented_without_deadline_message\n'
  fi
}

snapshot_mode() {
  local out="$1" table
  table="$(mktemp)"
  capture_process_table "$table"
  lane_target_orphan_identities "$table" >"$out"
  rm -f "$table"
  printf 'snapshot=%s count=%s\n' "$out" "$(wc -l <"$out" | tr -d ' ')"
}

subtract_mode() {
  local before="$1" after="$2" delta="$3" new_count
  [[ -f "$before" ]] || die "missing baseline snapshot: $before"
  [[ -f "$after" ]] || die "missing after snapshot: $after"
  new_count="$(new_orphan_identities "$before" "$after" "$delta")"
  printf 'new_count=%s delta=%s\n' "$new_count" "$delta"
  [[ "$new_count" == 0 ]] || return 1
}

# ---------------------------------------------------------------------------
# Self-test: synthetic tables that exercise the primitives above.
# ---------------------------------------------------------------------------

SELF_TEST_TOTAL=0
SELF_TEST_FAILED=0

report_arm() {
  local status="$1" name="$2" detail="${3:-}"
  SELF_TEST_TOTAL=$((SELF_TEST_TOTAL + 1))
  if [[ "$status" != PASS ]]; then
    SELF_TEST_FAILED=$((SELF_TEST_FAILED + 1))
  fi
  printf 'arm=%s status=%s %s\n' "$name" "$status" "$detail"
}

# An arm whose fixture carries no rows proves nothing; report it as VACUOUS so a
# silently emptied fixture can never read as a pass.
require_nonempty() {
  local name="$1" path="$2"
  if [[ ! -s "$path" ]]; then
    report_arm VACUOUS "$name" "empty_fixture=$path"
    return 1
  fi
  return 0
}

synthetic_row() {
  printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4"
}

lane_target_command() {
  printf '%s%s %s --ignored --exact --nocapture' "$LANE_TARGET_PREFIX" "$1" "$LIVE_TEST_NAME"
}

self_test_zero_children() {
  local name=zero_child_candidates_fail table="$SELF_TEST_DIR/zero.tsv" count
  {
    synthetic_row 4242 900 Mon_Aug_11_00:00:01_2026 "cargo test -p flapjack-http --lib"
    synthetic_row 4300 4242 Mon_Aug_11_00:00:02_2026 "/usr/bin/ld -o something"
  } >"$table"
  require_nonempty "$name" "$table" || return 0
  count="$(select_exact_children "$table" 4242 | grep -c . || true)"
  if [[ "$count" == 1 ]]; then
    report_arm FAIL "$name" "expected_not_one actual_count=$count"
  else
    report_arm PASS "$name" "count=$count"
  fi
}

self_test_two_children() {
  local name=two_child_candidates_fail table="$SELF_TEST_DIR/two.tsv" count
  {
    synthetic_row 4242 900 Mon_Aug_11_00:00:01_2026 "cargo test -p flapjack-http --lib"
    synthetic_row 4301 4242 Mon_Aug_11_00:00:02_2026 "$(lane_target_command aa11bb22)"
    synthetic_row 4302 4242 Mon_Aug_11_00:00:03_2026 "$(lane_target_command cc33dd44)"
  } >"$table"
  require_nonempty "$name" "$table" || return 0
  count="$(select_exact_children "$table" 4242 | grep -c . || true)"
  if [[ "$count" == 1 ]]; then
    report_arm FAIL "$name" "expected_not_one actual_count=$count"
  else
    report_arm PASS "$name" "count=$count"
  fi
}

self_test_single_child() {
  local name=single_matching_child_passes table="$SELF_TEST_DIR/one.tsv" count identity
  {
    synthetic_row 4242 900 Mon_Aug_11_00:00:01_2026 "cargo test -p flapjack-http --lib"
    synthetic_row 4301 4242 Mon_Aug_11_00:00:02_2026 "$(lane_target_command aa11bb22)"
    # Same parent, lane-target executable, but a different test: not the specimen.
    synthetic_row 4303 4242 Mon_Aug_11_00:00:04_2026 \
      "${LANE_TARGET_PREFIX}aa11bb22 handlers::migration::preview_tests::preview_http_report --exact"
    # Same parent and test name, but not the lane target executable.
    synthetic_row 4304 4242 Mon_Aug_11_00:00:05_2026 "/tmp/other/deps/flapjack_http-ff99 $LIVE_TEST_NAME"
    # Right shape, wrong parent.
    synthetic_row 4305 77 Mon_Aug_11_00:00:06_2026 "$(lane_target_command aa11bb22)"
  } >"$table"
  require_nonempty "$name" "$table" || return 0
  count="$(select_exact_children "$table" 4242 | grep -c . || true)"
  identity="$(select_exact_children "$table" 4242 | row_identity)"
  if [[ "$count" == 1 && "$identity" == "pid=4301 start=Mon_Aug_11_00:00:02_2026" ]]; then
    report_arm PASS "$name" "identity=$identity"
  else
    report_arm FAIL "$name" "count=$count identity=$identity"
  fi
}

self_test_reused_pid() {
  local name=reused_pid_different_start_identity_is_not_recorded_child
  local table="$SELF_TEST_DIR/reused.tsv" recorded="pid=4301 start=Mon_Aug_11_00:00:02_2026"
  {
    # PID 4301 is alive again, but it started later: a different process.
    synthetic_row 4301 900 Mon_Aug_11_00:05:00_2026 "$(lane_target_command aa11bb22)"
  } >"$table"
  require_nonempty "$name" "$table" || return 0
  if identity_present "$table" "$recorded"; then
    report_arm FAIL "$name" "recycled_pid_read_as_recorded_child recorded=$recorded"
  else
    report_arm PASS "$name" "recorded=$recorded absent_despite_pid_reuse"
  fi
}

self_test_preexisting_orphan() {
  local name=preexisting_baseline_orphan_is_not_new
  local before_table="$SELF_TEST_DIR/pre_before.tsv" after_table="$SELF_TEST_DIR/pre_after.tsv"
  local before="$SELF_TEST_DIR/pre_before.set" after="$SELF_TEST_DIR/pre_after.set"
  local delta="$SELF_TEST_DIR/pre_delta.set" new_count
  synthetic_row 5001 1 Mon_Aug_04_09:00:00_2026 "$(lane_target_command 0011ee22)" >"$before_table"
  {
    synthetic_row 5001 1 Mon_Aug_04_09:00:00_2026 "$(lane_target_command 0011ee22)"
    synthetic_row 5100 900 Mon_Aug_11_00:00:02_2026 "$(lane_target_command aa11bb22)"
  } >"$after_table"
  require_nonempty "$name" "$before_table" || return 0
  lane_target_orphan_identities "$before_table" >"$before"
  lane_target_orphan_identities "$after_table" >"$after"
  new_count="$(new_orphan_identities "$before" "$after" "$delta")"
  if [[ "$new_count" == 0 && ! -s "$delta" && -s "$before" ]]; then
    report_arm PASS "$name" "new_count=$new_count baseline_retained"
  else
    report_arm FAIL "$name" "new_count=$new_count delta=$(tr '\n' ';' <"$delta")"
  fi
}

self_test_new_orphan() {
  local name=new_post_run_orphan_fails
  local before_table="$SELF_TEST_DIR/new_before.tsv" after_table="$SELF_TEST_DIR/new_after.tsv"
  local before="$SELF_TEST_DIR/new_before.set" after="$SELF_TEST_DIR/new_after.set"
  local delta="$SELF_TEST_DIR/new_delta.set" new_count
  synthetic_row 5001 1 Mon_Aug_04_09:00:00_2026 "$(lane_target_command 0011ee22)" >"$before_table"
  {
    synthetic_row 5001 1 Mon_Aug_04_09:00:00_2026 "$(lane_target_command 0011ee22)"
    synthetic_row 5200 1 Mon_Aug_11_00:00:09_2026 "$(lane_target_command aa11bb22)"
  } >"$after_table"
  require_nonempty "$name" "$after_table" || return 0
  lane_target_orphan_identities "$before_table" >"$before"
  lane_target_orphan_identities "$after_table" >"$after"
  new_count="$(new_orphan_identities "$before" "$after" "$delta")"
  if [[ "$new_count" == 1 ]] && grep -Fqx "pid=5200 start=Mon_Aug_11_00:00:09_2026" "$delta"; then
    report_arm PASS "$name" "new_count=$new_count"
  else
    report_arm FAIL "$name" "new_count=$new_count delta=$(tr '\n' ';' <"$delta")"
  fi
}

self_test_parent_signal_classification() {
  local name=parent_signal_requires_absent_identity absent_verdict present_verdict
  absent_verdict="$(classify_child_terminal_state 0 1 0)"
  present_verdict="$(classify_child_terminal_state 0 0 0)"
  if [[ "$absent_verdict" == parent_signal && "$present_verdict" != parent_signal ]]; then
    report_arm PASS "$name" "absent=$absent_verdict still_present=$present_verdict"
  else
    report_arm FAIL "$name" "absent=$absent_verdict still_present=$present_verdict"
  fi
}

self_test_deadline_classification() {
  local name=deadline_requires_reparent_message_and_absence
  local full silent still_present
  full="$(classify_child_terminal_state 1 1 1)"
  silent="$(classify_child_terminal_state 1 1 0)"
  still_present="$(classify_child_terminal_state 1 0 1)"
  if [[ "$full" == deadline && "$silent" != deadline && "$still_present" != deadline ]]; then
    report_arm PASS "$name" "full=$full silent=$silent still_present=$still_present"
  else
    report_arm FAIL "$name" "full=$full silent=$silent still_present=$still_present"
  fi
}

self_test_mode() {
  SELF_TEST_DIR="$(mktemp -d)"
  trap 'rm -rf "$SELF_TEST_DIR"' RETURN
  self_test_zero_children
  self_test_two_children
  self_test_single_child
  self_test_reused_pid
  self_test_preexisting_orphan
  self_test_new_orphan
  self_test_parent_signal_classification
  self_test_deadline_classification
  printf 'synthetic_denominator=%s failed=%s\n' "$SELF_TEST_TOTAL" "$SELF_TEST_FAILED"
  [[ "$SELF_TEST_TOTAL" -gt 0 ]] || die "self-test executed no arms"
  [[ "$SELF_TEST_FAILED" == 0 ]] || return 1
  printf 'SELF_TEST=PASS\n'
}

# ---------------------------------------------------------------------------
# Live specimen.
# ---------------------------------------------------------------------------

write_blackhole_fixture() {
  cat >"$TMP_ROOT/blackhole.py" <<'PY'
"""Loopback TCP blackhole: accepts, records, and never answers."""
import socket
import sys
import time

port_file, records_file = sys.argv[1], sys.argv[2]
listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
try:
    listener.bind(("127.0.0.1", 0))
except PermissionError as error:
    sys.stderr.write("BLACKHOLE_BIND_PERMISSION_DENIED %s\n" % error)
    raise SystemExit(13)
listener.listen(16)
with open(port_file, "w") as handle:
    handle.write(str(listener.getsockname()[1]))
held = []
accepted = 0
while True:
    connection, _ = listener.accept()
    accepted += 1
    held.append(connection)
    with open(records_file, "a") as handle:
        handle.write("record=%d monotonic=%.6f\n" % (accepted, time.monotonic()))
PY
}

start_blackhole() {
  write_blackhole_fixture
  : >"$TMP_ROOT/blackhole_records.txt"
  python3 "$TMP_ROOT/blackhole.py" "$TMP_ROOT/blackhole_port.txt" \
    "$TMP_ROOT/blackhole_records.txt" >"$TMP_ROOT/blackhole.log" 2>&1 &
  BLACKHOLE_PID=$!
  local waited=0
  while [[ ! -s "$TMP_ROOT/blackhole_port.txt" ]]; do
    if ! kill -0 "$BLACKHOLE_PID" 2>/dev/null; then
      if grep -q 'BLACKHOLE_BIND_PERMISSION_DENIED' "$TMP_ROOT/blackhole.log"; then
        printf 'SANDBOX BLOCKER: port binding Operation not permitted -- deferred\n'
        exit 2
      fi
      die "blackhole fixture exited before binding: $(cat "$TMP_ROOT/blackhole.log")"
    fi
    sleep "$POLL_INTERVAL_SECONDS"
    waited=$((waited + 1))
    [[ "$waited" -lt 100 ]] || die "blackhole fixture never published a port"
  done
  BLACKHOLE_PORT="$(cat "$TMP_ROOT/blackhole_port.txt")"
  kill -0 "$BLACKHOLE_PID" 2>/dev/null || die "blackhole fixture died after binding"
  python3 -c 'import socket,sys; s=socket.create_connection(("127.0.0.1", int(sys.argv[1])), 5); sys.stdout.write("ready")' \
    "$BLACKHOLE_PORT" >/dev/null || die "blackhole readiness connection failed"
  wait_for_accepted_record 1 "readiness"
  printf 'blackhole_pid=%s port=%s readiness_record=1\n' "$BLACKHOLE_PID" "$BLACKHOLE_PORT"
}

wait_for_accepted_record() {
  local wanted="$1" label="$2" waited=0 limit
  limit=$(python3 -c "print(int($CONNECTION_WAIT_TIMEOUT_SECONDS / $POLL_INTERVAL_SECONDS))")
  while ! grep -q "^record=$wanted " "$TMP_ROOT/blackhole_records.txt"; do
    sleep "$POLL_INTERVAL_SECONDS"
    waited=$((waited + 1))
    [[ "$waited" -lt "$limit" ]] || die "blackhole never accepted connection $wanted ($label)"
  done
}

discover_recorded_child() {
  local waited=0 limit table rows count
  limit=$(python3 -c "print(int($CHILD_DISCOVERY_TIMEOUT_SECONDS / $POLL_INTERVAL_SECONDS))")
  table="$TMP_ROOT/discovery.tsv"
  while true; do
    capture_process_table "$table"
    rows="$(select_exact_children "$table" "$CARGO_PID" || true)"
    count="$(printf '%s' "$rows" | grep -c . || true)"
    if [[ "$count" == 1 ]]; then
      CHILD_PID="$(printf '%s\n' "$rows" | cut -f1)"
      CHILD_START="$(printf '%s\n' "$rows" | cut -f3)"
      CHILD_COMMAND="$(printf '%s\n' "$rows" | cut -f4)"
      CHILD_IDENTITY="$(printf '%s\n' "$rows" | row_identity)"
      return 0
    fi
    if [[ "$count" -gt 1 ]]; then
      die "setup failure: $count direct test children matched, expected exactly 1"
    fi
    if ! kill -0 "$CARGO_PID" 2>/dev/null; then
      die "setup failure: cargo exited before a test child was discovered"
    fi
    sleep "$POLL_INTERVAL_SECONDS"
    waited=$((waited + 1))
    [[ "$waited" -lt "$limit" ]] || die "setup failure: no direct test child appeared"
  done
}

await_terminal_state() {
  local waited=0 limit table reparented=0 absent=0 message=0
  limit=$(python3 -c "print(int($TERMINAL_STATE_TIMEOUT_SECONDS / $POLL_INTERVAL_SECONDS))")
  table="$TMP_ROOT/terminal.tsv"
  while true; do
    capture_process_table "$table"
    if identity_present "$table" "$CHILD_IDENTITY"; then
      if identity_reparented "$table" "$CHILD_IDENTITY"; then
        reparented=1
      fi
    else
      absent=1
    fi
    if grep -Fq "$DEADLINE_MESSAGE" "$TMP_ROOT/cargo_test.log"; then
      message=1
    fi
    if [[ "$absent" == 1 ]]; then
      break
    fi
    sleep "$POLL_INTERVAL_SECONDS"
    waited=$((waited + 1))
    [[ "$waited" -lt "$limit" ]] || break
  done
  CHILD_REPARENTED="$reparented"
  CHILD_ABSENT="$absent"
  CHILD_DEADLINE_MESSAGE="$message"
}

live_mode() {
  command -v cargo >/dev/null || die "cargo is required"
  command -v python3 >/dev/null || die "python3 is required"
  TMP_ROOT="$(mktemp -d)"
  trap cleanup EXIT

  # Compilation must not run between the baseline and child discovery, or a
  # transient rustc child would pollute the lane-target identity sets.
  (cd "$ENGINE_DIR" && cargo test -p flapjack-http --lib --no-run) \
    >"$TMP_ROOT/precompile.log" 2>&1 || die "precompile failed; see $TMP_ROOT/precompile.log"

  capture_process_table "$TMP_ROOT/baseline.tsv"
  lane_target_orphan_identities "$TMP_ROOT/baseline.tsv" >"$TMP_ROOT/baseline.set"
  printf 'baseline_orphans=%s\n' "$(wc -l <"$TMP_ROOT/baseline.set" | tr -d ' ')"

  start_blackhole

  (
    cd "$ENGINE_DIR"
    exec env \
      "$PREVIEW_ENDPOINT_ENV=http://127.0.0.1:$BLACKHOLE_PORT" \
      "$PREVIEW_API_KEY_ENV=$PREVIEW_CANARY_API_KEY" \
      "$PREVIEW_EXPECTED_RECORDS_ENV=$PREVIEW_EXPECTED_RECORDS" \
      cargo test -p flapjack-http --lib -- "$LIVE_TEST_NAME" \
      --ignored --exact --nocapture
  ) >"$TMP_ROOT/cargo_test.log" 2>&1 &
  CARGO_PID=$!
  printf 'cargo_pid=%s\n' "$CARGO_PID"

  discover_recorded_child
  printf 'child_pid=%s child_start=%s child_command=%s\n' \
    "$CHILD_PID" "$CHILD_START" "$CHILD_COMMAND"

  wait_for_accepted_record 2 "test-owned"
  identity_present "$TMP_ROOT/discovery.tsv" "$CHILD_IDENTITY" \
    || die "recorded child identity vanished before the connection was observed"
  printf 'test_connection_observed=1\n'

  kill -TERM "$CARGO_PID" 2>/dev/null || true
  CARGO_EXIT=0
  # The signal exit is expected, so it must not abort the run; stderr is muted
  # only to drop bash's job-termination notice, never a classification fact.
  { wait "$CARGO_PID" || CARGO_EXIT=$?; } 2>/dev/null
  CARGO_PID=""
  printf 'cargo_exit=%s\n' "$CARGO_EXIT"

  await_terminal_state
  printf 'child_reparented=%s child_absent=%s child_deadline_message=%s evidence_dir=%s\n' \
    "$CHILD_REPARENTED" "$CHILD_ABSENT" "$CHILD_DEADLINE_MESSAGE" "$TMP_ROOT"
  CHILD_EXIT="$(classify_child_terminal_state \
    "$CHILD_REPARENTED" "$CHILD_ABSENT" "$CHILD_DEADLINE_MESSAGE")"
  [[ "$CHILD_EXIT" == deadline || "$CHILD_EXIT" == parent_signal ]] \
    || die "unclassified terminal state (reparented=$CHILD_REPARENTED absent=$CHILD_ABSENT message=$CHILD_DEADLINE_MESSAGE)"

  capture_process_table "$TMP_ROOT/after.tsv"
  lane_target_orphan_identities "$TMP_ROOT/after.tsv" >"$TMP_ROOT/after.set"
  NEW_COUNT="$(new_orphan_identities "$TMP_ROOT/baseline.set" "$TMP_ROOT/after.set" "$TMP_ROOT/delta.set")"
  [[ "$NEW_COUNT" == 0 ]] \
    || die "new lane-target orphans after the run: $(tr '\n' ';' <"$TMP_ROOT/delta.set")"

  printf 'PROCESS_DEADLINE_CONTRACT result=PASS child_pid=%s child_start=%s child_exit=%s new_count=%s\n' \
    "$CHILD_PID" "$CHILD_START" "$CHILD_EXIT" "$NEW_COUNT"
}

main() {
  case "${1:---live}" in
    --self-test) self_test_mode ;;
    --snapshot) snapshot_mode "$2" ;;
    --subtract) subtract_mode "$2" "$3" "$4" ;;
    --live) live_mode ;;
    *) die "unknown mode: $1" ;;
  esac
}

main "$@"
