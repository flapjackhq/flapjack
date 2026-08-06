#!/usr/bin/env bash

# Meta-test for the cross passthrough KAT in build_identity_package_contract.sh.
# It drives the KAT against mock `cross`, `timeout`, and `docker` binaries so the
# supervision, classification, and revision-oracle behaviour can be proven
# without a container. The modes below cover the KAT's outcome set: delivered
# revision, mismatched revision, unsigned failure, concrete environment
# failures, and a bound exhausted while the emulated container was still
# compiling without error.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACT="$SCRIPT_DIR/build_identity_package_contract.sh"
TEST_ROOT="$(mktemp -d)"
MOCK_BIN="$TEST_ROOT/bin"
STATE_DIR="$TEST_ROOT/state"
OUTPUT_LOG="$TEST_ROOT/contract.log"
CONTAINER_ID="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
WRONG_REVISION="00000000000000000000000000000000deadbeef"
PREVIOUS_PASSED_REVISION=""
CACHE_MARKER=".flapjack_cross_kat_cache"

cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT

mkdir -p "$MOCK_BIN" "$STATE_DIR"

# The mock emulates the container side: it records how it was supervised, then
# reproduces one of the outcomes. The benign jemalloc notice is printed on every
# run because the real aarch64 host prints it even when nothing is wrong, so it
# must never on its own be read as an environment failure. `delivered` echoes
# back whatever revision the KAT passed in, which is how the meta-test proves the
# KAT asserts against its own freshly generated revision.
cat >"$MOCK_BIN/cross" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >"$CROSS_KAT_TEST_STATE/cross_arguments"
printf '%s\n' "${CROSS_CONTAINER_OPTS:-}" >"$CROSS_KAT_TEST_STATE/container_opts"
printf '%s\n' "${FLAPJACK_BUILD_REVISION:-}" >"$CROSS_KAT_TEST_STATE/passed_revision"
touch "$CROSS_KAT_TEST_STATE/container_running"
printf '<jemalloc>: MADV_DONTNEED does not work (memset will be used instead)\n' >&2
printf '<jemalloc>: (This is the expected behaviour if you are running under QEMU)\n' >&2

emit_build_script_revision() {
  local output_dir="$CARGO_TARGET_DIR/debug/build/flapjack-4d1f2c3b4a5e6f70"
  mkdir -p "$output_dir"
  printf 'cargo:rustc-env=FLAPJACK_INTERNAL_BUILD_REVISION=%s\n' "$1" \
    >"$output_dir/output"
}

case "${CROSS_KAT_TEST_MODE:-failure}" in
  delivered)
    emit_build_script_revision "$FLAPJACK_BUILD_REVISION"
    exit 0
    ;;
  mismatched)
    emit_build_script_revision "$CROSS_KAT_TEST_WRONG_REVISION"
    exit 0
    ;;
  environment)
    printf 'qemu: uncaught target signal 11 (Segmentation fault)\n' >&2
    exit 124
    ;;
  emulated-progress)
    printf '   Compiling serde v1.0.0\n'
    printf '    Checking flapjack v1.0.11\n'
    sleep 6
    exit 124
    ;;
  toolchain-segv)
    printf '   Compiling zstd-sys v2.0.13+zstd.1.5.6\n'
    printf 'error: failed to run custom build command for `zstd-sys v2.0.13+zstd.1.5.6`\n' >&2
    printf '  error occurred in cc-rs: command did not execute successfully (status code signal: 11 (SIGSEGV) (core dumped)): "aarch64-linux-musl-gcc"\n' >&2
    exit 101
    ;;
  toolchain-generic-cc-rs)
    printf '   Compiling zstd-sys v2.0.13+zstd.1.5.6\n'
    printf 'error: failed to run custom build command for `zstd-sys v2.0.13+zstd.1.5.6`\n' >&2
    printf '  error occurred in cc-rs: command did not execute successfully (status code exit status: 1): "aarch64-linux-musl-gcc"\n' >&2
    exit 101
    ;;
  silent)
    exit 0
    ;;
  *)
    printf 'simulated supervisor timeout without an environment failure signature\n' >&2
    exit 124
    ;;
esac
MOCK

cat >"$MOCK_BIN/timeout" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [ "${2:-}" = "cross" ]; then
  printf '%s\n' "$1" >"$CROSS_KAT_TEST_STATE/timeout_seconds"
fi
shift
"$@"
MOCK

cat >"$MOCK_BIN/docker" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$CROSS_KAT_TEST_STATE/docker_calls"
case "${1:-}" in
  ps)
    if [ -e "$CROSS_KAT_TEST_STATE/container_running" ]; then
      printf '%s\n' "$CROSS_KAT_TEST_CONTAINER_ID"
    fi
    ;;
  inspect)
    printf '{"Id":"%s"}\n' "$CROSS_KAT_TEST_CONTAINER_ID"
    ;;
  logs)
    printf 'simulated container evidence\n'
    ;;
  top)
    printf 'PID COMMAND\n1 cargo check\n'
    ;;
  rm)
    [ "${2:-}" = "-f" ]
    [ "${3:-}" = "$CROSS_KAT_TEST_CONTAINER_ID" ]
    rm -f "$CROSS_KAT_TEST_STATE/container_running"
    printf '%s\n' "$CROSS_KAT_TEST_CONTAINER_ID"
    ;;
  *)
    printf 'unexpected docker invocation: %s\n' "$*" >&2
    exit 64
    ;;
esac
MOCK

chmod +x "$MOCK_BIN/cross" "$MOCK_BIN/timeout" "$MOCK_BIN/docker"

fail() {
  cat "$OUTPUT_LOG" >&2 || true
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

# Runs one contract mode under the mocks and echoes its exit status. Every
# environment knob the KAT and the warmup mode read is supplied here so a single
# helper can drive both.
run_contract() {
  local contract_mode="$1"
  local outcome_mode="$2"
  local status=0
  rm -f "$STATE_DIR/docker_calls" "$STATE_DIR/container_opts" \
    "$STATE_DIR/timeout_seconds" "$STATE_DIR/container_running" \
    "$STATE_DIR/passed_revision"
  PATH="$MOCK_BIN:$PATH" \
    CROSS_KAT_TARGET_ROOT="$TEST_ROOT" \
    CROSS_KAT_TARGET_CACHE="$RUN_TARGET_CACHE" \
    CROSS_KAT_TIMEOUT="$RUN_BOUND_SECONDS" \
    CROSS_KAT_WARMUP_TIMEOUT="$RUN_WARMUP_BOUND_SECONDS" \
    CROSS_KAT_POLL_INTERVAL="$RUN_POLL_INTERVAL" \
    CROSS_KAT_TEST_STATE="$STATE_DIR" \
    CROSS_KAT_TEST_CONTAINER_ID="$CONTAINER_ID" \
    CROSS_KAT_TEST_MODE="$outcome_mode" \
    CROSS_KAT_TEST_WRONG_REVISION="$WRONG_REVISION" \
    bash "$CONTRACT" "$contract_mode" >"$OUTPUT_LOG" 2>&1 || status=$?
  printf '%s\n' "$status"
}

# Resets every run knob to its default so each case only states what it varies.
reset_run_knobs() {
  RUN_BOUND_SECONDS=600
  RUN_WARMUP_BOUND_SECONDS=3600
  RUN_TARGET_CACHE=""
  RUN_POLL_INTERVAL=1
}

# Runs the KAT under the mocks in one outcome mode and echoes its exit status.
run_kat() {
  local mode="$1"
  reset_run_knobs
  RUN_BOUND_SECONDS="${2:-600}"
  RUN_TARGET_CACHE="${3:-}"
  RUN_POLL_INTERVAL="${4:-1}"
  run_contract --cross-passthrough-only "$mode"
}

# Runs the warmup mode, which owns the long half of the KAT: it exists so the
# build script has already been compiled inside the container by the time the
# bounded KAT runs.
run_warmup() {
  local mode="$1"
  reset_run_knobs
  RUN_WARMUP_BOUND_SECONDS="${2:-3600}"
  RUN_TARGET_CACHE="${3:-}"
  run_contract --cross-passthrough-warmup "$mode"
}

assert_container_was_reaped() {
  local mode="$1"
  if ! grep -Eq '^ps -aq --filter label=com\.flapjack\.cross-kat=' "$STATE_DIR/docker_calls"; then
    fail "$mode: cleanup must discover only the KAT-owned container label"
  fi
  if ! grep -Fq "rm -f $CONTAINER_ID" "$STATE_DIR/docker_calls"; then
    fail "$mode: cleanup must force-remove the exact KAT-owned container ID"
  fi
  if [ -e "$STATE_DIR/container_running" ]; then
    fail "$mode: cross KAT container remained after the contract exited"
  fi
}

status="$(run_kat delivered)"
[ "$status" -eq 0 ] || fail "delivered revision must pass, got status $status"
passed_revision="$(cat "$STATE_DIR/passed_revision")"
if ! [[ "$passed_revision" =~ ^[0-9a-f]{40}$ ]]; then
  fail "the KAT must generate a 40-hex revision, got '$passed_revision'"
fi
if [ "$passed_revision" = "$PREVIOUS_PASSED_REVISION" ]; then
  fail "the KAT must generate a revision no earlier run used"
fi
PREVIOUS_PASSED_REVISION="$passed_revision"
if ! grep -Fq "$passed_revision" "$OUTPUT_LOG"; then
  fail "delivered revision must be reported in the pass line"
fi
if [ "$(cat "$STATE_DIR/timeout_seconds")" != "600" ]; then
  fail "cross KAT timeout must be exactly 600 seconds"
fi
if ! grep -Eq -- '--label(=|[[:space:]])com\.flapjack\.cross-kat=' "$STATE_DIR/container_opts"; then
  fail "cross KAT must label its container through CROSS_CONTAINER_OPTS"
fi
assert_container_was_reaped delivered

status="$(run_kat mismatched)"
[ "$status" -eq 1 ] || fail "mismatched revision must fail with status 1, got $status"
if grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "a wrong delivered revision is a defect and must never defer"
fi
if ! grep -Fq "$WRONG_REVISION" "$OUTPUT_LOG"; then
  fail "mismatched revision failure must report the revision that was delivered"
fi
assert_container_was_reaped mismatched

status="$(run_kat failure)"
[ "$status" -eq 1 ] || fail "unsigned timeout must fail with status 1, got $status"
if grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "a timeout carrying only the benign jemalloc QEMU notice must not defer"
fi
assert_container_was_reaped failure

status="$(run_kat environment)"
[ "$status" -eq 3 ] || fail "concrete QEMU signature must defer with status 3, got $status"
if ! grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "concrete QEMU signature must produce an explicit environment deferral"
fi
assert_container_was_reaped environment

status="$(run_kat emulated-progress 2)"
[ "$status" -eq 3 ] || fail "exhausted bound under emulation must defer with status 3, got $status"
if ! grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "an exhausted bound with emulated forward progress must defer explicitly"
fi
assert_container_was_reaped emulated-progress

# A reused target directory must never let a previous run's emission stand in for
# this run's answer.
STALE_CACHE="$TEST_ROOT/stale_cache"
mkdir -p "$STALE_CACHE/debug/build/flapjack-4d1f2c3b4a5e6f70"
printf 'flapjack cross passthrough KAT target cache\n' >"$STALE_CACHE/$CACHE_MARKER"
printf 'cargo:rustc-env=FLAPJACK_INTERNAL_BUILD_REVISION=%s\n' "$WRONG_REVISION" \
  >"$STALE_CACHE/debug/build/flapjack-4d1f2c3b4a5e6f70/output"
status="$(run_kat silent 2 "$STALE_CACHE")"
[ "$status" -eq 1 ] || fail "a stale cached emission must not pass the KAT, got status $status"
if grep -Fq "$WRONG_REVISION" "$OUTPUT_LOG"; then
  fail "the KAT read a previous run's cached emission as this run's answer"
fi
assert_container_was_reaped silent

# The generated revision must differ run to run, otherwise a cached build-script
# emission from an earlier run could satisfy the delivery assertion.
status="$(run_kat delivered)"
[ "$status" -eq 0 ] || fail "second delivered run must pass, got status $status"
if [ "$(cat "$STATE_DIR/passed_revision")" = "$PREVIOUS_PASSED_REVISION" ]; then
  fail "the KAT must generate a revision no earlier run used"
fi

# The stale-emission clearing deletes build-script output inside the supplied
# target directory, so the contract may only accept a directory it owns: one that
# is empty on first use, or one already carrying its marker.
UNCLAIMED_CACHE="$TEST_ROOT/unclaimed_cache"
mkdir -p "$UNCLAIMED_CACHE/debug/build/flapjack-4d1f2c3b4a5e6f70"
printf 'cargo:rustc-env=FLAPJACK_INTERNAL_BUILD_REVISION=%s\n' "$WRONG_REVISION" \
  >"$UNCLAIMED_CACHE/debug/build/flapjack-4d1f2c3b4a5e6f70/output"
status="$(run_kat delivered 600 "$UNCLAIMED_CACHE")"
[ "$status" -eq 1 ] || fail "an unclaimed non-empty target cache must be refused, got status $status"
if ! grep -Fq "$CACHE_MARKER" "$OUTPUT_LOG"; then
  fail "refusing an unclaimed target cache must name the marker that claims one"
fi
if [ ! -f "$UNCLAIMED_CACHE/debug/build/flapjack-4d1f2c3b4a5e6f70/output" ]; then
  fail "refusing an unclaimed target cache must not delete anything inside it"
fi

EMPTY_CACHE="$TEST_ROOT/empty_cache"
status="$(run_kat delivered 600 "$EMPTY_CACHE")"
[ "$status" -eq 0 ] || fail "an empty target cache is first use and must be claimed, got status $status"
if [ ! -f "$EMPTY_CACHE/$CACHE_MARKER" ]; then
  fail "first use of a target cache must write the marker that claims it"
fi

# The bound is the reason this KAT is trustworthy, so both of its inputs are
# validated. A zero interval never advances the loop's own elapsed counter.
status="$(run_kat delivered 600 "" 0)"
[ "$status" -eq 1 ] || fail "a zero poll interval must be refused, got status $status"
if ! grep -Fq 'CROSS_KAT_POLL_INTERVAL' "$OUTPUT_LOG"; then
  fail "refusing a bad poll interval must name the variable"
fi
status="$(run_kat delivered 600 "" 601)"
[ "$status" -eq 1 ] || fail "a poll interval longer than the bound must be refused, got status $status"

# Warmup mode owns the long half: it runs the same container command under its own
# larger bound until engine/build.rs has run once, so the KAT above only needs the
# build script to re-run.
WARMUP_CACHE="$TEST_ROOT/warmup_cache"
status="$(run_warmup delivered 900 "$WARMUP_CACHE")"
[ "$status" -eq 0 ] || fail "warmup must succeed once build.rs emitted, got status $status"
if [ "$(cat "$STATE_DIR/timeout_seconds")" != "900" ]; then
  fail "warmup must bound the container with CROSS_KAT_WARMUP_TIMEOUT"
fi
if [ ! -f "$WARMUP_CACHE/$CACHE_MARKER" ]; then
  fail "warmup must claim the target cache it warms"
fi
assert_container_was_reaped warmup

status="$(run_warmup delivered 900 "")"
[ "$status" -eq 1 ] || fail "warmup without a target cache must fail, got status $status"
if ! grep -Fq 'CROSS_KAT_TARGET_CACHE' "$OUTPUT_LOG"; then
  fail "warmup without a target cache must name the variable it requires"
fi

status="$(run_warmup environment 900 "$TEST_ROOT/warmup_env_cache")"
[ "$status" -eq 3 ] || fail "warmup must defer on a concrete environment signature, got status $status"
if ! grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "warmup must classify a host/toolchain failure as an environment deferral"
fi

# One dependency's build script crashing the emulated C toolchain (observed with
# zstd-sys under QEMU) is a locality failure, not a passthrough defect, and it
# must not be reported as an unexplained failure.
status="$(run_kat toolchain-segv 600)"
[ "$status" -eq 3 ] || fail "an emulated C-toolchain SIGSEGV must defer with status 3, got $status"
if ! grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "an emulated C-toolchain SIGSEGV must produce an explicit environment deferral"
fi

status="$(run_kat toolchain-generic-cc-rs 600)"
[ "$status" -eq 1 ] || fail "a generic cc-rs failure must be a hard failure with status 1, got $status"
if grep -Fq 'DEFERRED-ENV:' "$OUTPUT_LOG"; then
  fail "a generic cc-rs failure must not produce an environment deferral"
fi

# That same dependency failure must not decide the KAT's outcome: build.rs does
# not depend on it, so the run keeps going and the oracle still gets its answer.
if ! grep -Fq -- '--keep-going' "$STATE_DIR/cross_arguments"; then
  fail "the cross command must keep going so one dependency failure cannot stop engine/build.rs from running"
fi

printf 'build identity cross KAT supervision contract passed\n'
