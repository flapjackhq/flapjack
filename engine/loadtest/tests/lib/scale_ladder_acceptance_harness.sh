# shellcheck shell=bash
# Shared harness for the scale-ladder acceptance tests (smoke + backpressure evidence).
#
# Owns the boilerplate that was previously byte-duplicated across
# scale_ladder_smoke_acceptance.sh and scale_ladder_backpressure_evidence_acceptance.sh: path and
# binary resolution, the plumbing-fixture calibration exports, fail(), the stale-run_receipt seed,
# and the impossible-throughput ladder invocation. Keeping these in one place means a rename of a
# ladder flag (e.g. --min-docs-per-second, --stall-seconds) or a change to the reserve-bytes
# envelope updates both callers at once instead of half-breaking the pair.
#
# Test-only on purpose: this is intentionally NOT the same file as lib/loadtest_shell_helpers.sh,
# which run.sh and soak_proof.sh also source, so nothing here leaks into the production runners.
# Source it after `set -euo pipefail`; it defines functions and exports and runs no test logic.

_HARNESS_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$_HARNESS_DIR/../.." && pwd)"
ENGINE_DIR="$(cd "$LOADTEST_DIR/.." && pwd)"
LADDER_SCRIPT="${FLAPJACK_SCALE_LADDER_SCRIPT:-$LOADTEST_DIR/scale_ladder.sh}"
SERVER_BINARY="${FLAPJACK_SCALE_SERVER_BINARY:-$ENGINE_DIR/target/release/flapjack}"

# The campaign calibration starts at 1M. Keep this 10K plumbing fixture above fixed process/index
# overhead so the capacity preflight is not the thing under test; dedicated negative controls
# override both values to one byte per record.
export SCALE_INDEX_BYTES_PER_RECORD="${SCALE_INDEX_BYTES_PER_RECORD:-100000}"
export SCALE_RSS_BYTES_PER_RECORD="${SCALE_RSS_BYTES_PER_RECORD:-100000}"

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

# Preconditions shared by every ladder acceptance test: the driver and the server binary must both
# be present and executable before any run is attempted.
require_ladder_binaries() {
  [[ -x "$LADDER_SCRIPT" ]] || fail "scale ladder driver missing or not executable: $LADDER_SCRIPT"
  [[ -x "$SERVER_BINARY" ]] || fail "release server binary missing: $SERVER_BINARY"
}

# Seed a stale run_receipt.json so a later assertion can prove cleanup() rewrote this run's receipt
# rather than leaving a prior run's untouched.
seed_stale_run_receipt() {
  local results_dir="$1"
  mkdir -p "$results_dir"
  jq -n '{
    outcome: "PAUSED",
    runnerTmpDir: "/tmp/stale_prior_run",
    remainingGeneratedDatasetDirs: 0,
    terminalRung: null,
    terminalMetricsPath: null
  }' > "$results_dir/run_receipt.json"
}

# Drive the ladder with an impossible throughput floor so rung 10 starts the server, imports its
# batch, then fails the min-docs-per-second gate. Fails the test if the ladder unexpectedly passes.
run_ladder_expecting_failure() {
  local base_url="$1" data_dir="$2" results_dir="$3"
  if SCALE_DISK_RESERVE_BYTES=1048576 \
    SCALE_MEMORY_RESERVE_BYTES=1048576 \
    timeout 180 bash "$LADDER_SCRIPT" \
      --profile compact \
      --rungs 10 \
      --batch-size 10 \
      --stall-seconds 10 \
      --min-docs-per-second 999999999 \
      --base-url "$base_url" \
      --server-binary "$SERVER_BINARY" \
      --data-dir "$data_dir" \
      --results-dir "$results_dir"; then
    fail "impossible throughput floor should force a non-PASS outcome"
  fi
}
