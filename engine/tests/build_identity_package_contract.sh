#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
PACKAGE_HELPER="${PACKAGE_HELPER:-$ENGINE_DIR/package/release_artifact_manifest}"
FOREIGN_FIXTURE_DIR="$SCRIPT_DIR/fixtures/release_artifact_manifest"
FOREIGN_FIXTURE="$FOREIGN_FIXTURE_DIR/aarch64_build_info_fixture"
FOREIGN_TARGET="aarch64-unknown-linux-musl"
FOREIGN_FIXTURE_SHA256="822c228ccbdff8cea5b231626e7a88b08acfce8e38cfa7b7194bedf32bb6e888"

# Local passthrough KAT: prove engine/Cross.toml delivers the workflow-exported
# FLAPJACK_BUILD_REVISION into engine/build.rs inside a `cross` container. The
# fake revision is a freshly generated 40-hex string that differs from HEAD, so a
# green run can only come from the container passthrough, never from
# in-container git discovery. Generating it per run also makes the oracle immune
# to a cached build script: build.rs declares
# `cargo:rerun-if-env-changed=FLAPJACK_BUILD_REVISION`, so a revision no previous
# run used forces this invocation's build script to execute.
# CROSS_KAT_TARGET_ROOT defaults to $HOME because `cross` bind-mounts
# CARGO_TARGET_DIR into the VM and only $HOME is shared on this Colima host.
# CROSS_KAT_TARGET_CACHE optionally names a reusable target directory; the run is
# then only as long as the build script itself, which is the difference between a
# feasible and an infeasible KAT on an emulated host. Warm such a directory once
# with --cross-passthrough-warmup, then run --cross-passthrough-only against it:
#   CROSS_KAT_TARGET_CACHE="$HOME/flapjack_cross_kat_cache" \
#     bash engine/tests/build_identity_package_contract.sh --cross-passthrough-warmup
#   CROSS_KAT_TARGET_CACHE="$HOME/flapjack_cross_kat_cache" \
#     bash engine/tests/build_identity_package_contract.sh --cross-passthrough-only
CROSS_KAT_TARGET="aarch64-unknown-linux-musl"
CROSS_KAT_TIMEOUT="${CROSS_KAT_TIMEOUT:-600}"
CROSS_KAT_WARMUP_TIMEOUT="${CROSS_KAT_WARMUP_TIMEOUT:-3600}"
CROSS_KAT_POLL_INTERVAL="${CROSS_KAT_POLL_INTERVAL:-5}"
CROSS_KAT_TARGET_ROOT="${CROSS_KAT_TARGET_ROOT:-$HOME}"
CROSS_KAT_TARGET_CACHE="${CROSS_KAT_TARGET_CACHE:-}"
RELEASE_BUILD_TIMEOUT="${RELEASE_BUILD_TIMEOUT:-600}"
CROSS_KAT_CONTAINER_LABEL="com.flapjack.cross-kat"
CROSS_KAT_CACHE_MARKER=".flapjack_cross_kat_cache"
CROSS_KAT_TIMEOUT_CEILING=600
CROSS_KAT_WARMUP_TIMEOUT_CEILING=7200

TMP_ROOT=""
CROSS_KAT_FAKE_REVISION=""
CROSS_KAT_TARGET_DIR=""
CROSS_KAT_TARGET_DIR_IS_DISPOSABLE=0
CROSS_KAT_RUN_ID=""
CROSS_KAT_EMISSION=""
CROSS_KAT_BOUND_EXHAUSTED=0
CROSS_KAT_SUPERVISOR_PID=""
CROSS_KAT_SUPERVISOR_STATUS=0
FAILURE_EVIDENCE_DIR=""

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

log() {
  printf '%s\n' "$*"
}

owned_cross_container_ids() {
  [ -n "$CROSS_KAT_RUN_ID" ] || return 0
  timeout 15 docker ps -aq \
    --filter "label=${CROSS_KAT_CONTAINER_LABEL}=${CROSS_KAT_RUN_ID}"
}

capture_cross_container_diagnostics() {
  local destination="$1"
  local container_id container_ids_file
  container_ids_file="$TMP_ROOT/cross_kat_container_ids"
  owned_cross_container_ids >"$container_ids_file" || return 1
  while IFS= read -r container_id; do
    [ -n "$container_id" ] || continue
    {
      printf '\nowned cross container: %s\n' "$container_id"
      timeout 15 docker inspect "$container_id" || true
      timeout 15 docker top "$container_id" || true
      timeout 15 docker logs "$container_id" || true
    } >>"$destination" 2>&1
  done <"$container_ids_file"
}

remove_owned_cross_containers() {
  local container_id container_ids_file residue
  local failed=0
  container_ids_file="$TMP_ROOT/cross_kat_container_ids"
  owned_cross_container_ids >"$container_ids_file" || return 1
  while IFS= read -r container_id; do
    [ -n "$container_id" ] || continue
    timeout 15 docker rm -f "$container_id" >/dev/null || failed=1
  done <"$container_ids_file"
  [ "$failed" -eq 0 ] || return 1
  residue="$(owned_cross_container_ids)" || return 1
  [ -z "$residue" ]
}

cleanup() {
  local exit_code=$?
  local cleanup_failed=0
  trap - EXIT INT TERM HUP
  set +e

  if [ "$exit_code" -ne 0 ] && [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    capture_cross_container_diagnostics "$TMP_ROOT/cross_kat_container_cleanup.log"
    FAILURE_EVIDENCE_DIR="${TMPDIR:-/tmp}/flapjack_build_identity_package_failure_${$}_$(date +%s)"
    mkdir -p "$FAILURE_EVIDENCE_DIR"
    cp -R "$TMP_ROOT" "$FAILURE_EVIDENCE_DIR/tmp_root"
    printf 'INFO: preserved build identity package evidence at %s\n' "$FAILURE_EVIDENCE_DIR" >&2
  fi

  remove_owned_cross_containers || cleanup_failed=1
  if [ "$CROSS_KAT_TARGET_DIR_IS_DISPOSABLE" -eq 1 ] \
    && [ -n "$CROSS_KAT_TARGET_DIR" ] && [ -d "$CROSS_KAT_TARGET_DIR" ]; then
    rm -rf "$CROSS_KAT_TARGET_DIR" || cleanup_failed=1
  fi
  if [ "$exit_code" -eq 0 ] && [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi

  if [ "$cleanup_failed" -ne 0 ]; then
    printf 'ERROR: failed to remove the exact cross KAT container or temporary target\n' >&2
    exit 1
  fi
  exit "$exit_code"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

require_tools() {
  local missing=0
  local tool
  for tool in cargo git python3 tar; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  [ "$missing" -eq 0 ] || exit 1
  [ -x "$PACKAGE_HELPER" ] || die "package helper is not executable: $PACKAGE_HELPER"
}

build_release_binary() {
  local source_dir="$1"
  local target_dir="$2"
  local revision="$3"
  require_positive_integer_bound RELEASE_BUILD_TIMEOUT "$RELEASE_BUILD_TIMEOUT" 1800
  log "Building flapjack-server in $target_dir"
  # These flags must mirror the release workflow's build step exactly. A specimen
  # built with different features is a different optimized artifact, so it cannot
  # stand in for the binary the packaging gate actually sees in CI.
  (
    cd "$source_dir"
    FLAPJACK_BUILD_REVISION="$revision" \
      CARGO_TARGET_DIR="$target_dir" \
      timeout "$RELEASE_BUILD_TIMEOUT" cargo build --release \
      --package flapjack-server --no-default-features
  )
}

assert_real_release_embedded_record() {
  local revision target_dir binary_path output_dir marker_report target_triple
  revision="$(git -C "$REPO_DIR" rev-parse HEAD)"
  target_dir="${REAL_RELEASE_TARGET_DIR:-$ENGINE_DIR/target/stage3_real_release_contract}"
  binary_path="$target_dir/release/flapjack"
  output_dir="$TMP_ROOT/real_release_output"
  marker_report="$TMP_ROOT/real_release_marker_report.json"

  build_release_binary "$ENGINE_DIR" "$target_dir" "$revision"
  [ -x "$binary_path" ] || die "expected executable release binary at $binary_path"

  python3 - "$binary_path" "$marker_report" <<'PY'
import json
import pathlib
import subprocess
import sys

binary_path = pathlib.Path(sys.argv[1])
marker_report = pathlib.Path(sys.argv[2])
contents = binary_path.read_bytes()
start_marker = b"FLAPJACK_BUILD_INFO_JSON_BEGIN\n"
end_marker = b"\nFLAPJACK_BUILD_INFO_JSON_END\n"
start_count = contents.count(start_marker)
end_count = contents.count(end_marker)
if start_count != 1:
    raise SystemExit(
        f"real release binary must contain exactly one begin marker, found {start_count}"
    )
if end_count != 1:
    raise SystemExit(
        f"real release binary must contain exactly one end marker, found {end_count}"
    )

start = contents.index(start_marker) + len(start_marker)
end = contents.index(end_marker)
record = contents[start:end]
embedded = json.loads(record.decode("utf-8"))
executed = json.loads(
    subprocess.check_output([str(binary_path), "build-info", "--json"], text=True)
)
if embedded != executed:
    raise SystemExit("real release embedded build-info JSON must match CLI output")
if embedded.get("profile") != "release":
    raise SystemExit(f"real release profile mismatch: {embedded.get('profile')!r}")
if embedded.get("revisionKnown") is not True:
    raise SystemExit("real release revisionKnown must be true")
if not embedded.get("target"):
    raise SystemExit("real release target must be non-empty")
marker_report.write_text(json.dumps(embedded, sort_keys=True) + "\n")
PY

  mkdir -p "$output_dir"
  target_triple="$(build_target_from_binary "$binary_path")"
  "$PACKAGE_HELPER" "$target_triple" "$binary_path" "$output_dir" >/dev/null

  # The record only matters if it reaches the manifest, so assert the packaged
  # build object is the same record, carrying the revision this build was given.
  python3 - "$output_dir/flapjack-${target_triple}.manifest.json" "$marker_report" "$revision" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
embedded = json.loads(pathlib.Path(sys.argv[2]).read_text())
revision = sys.argv[3]

build = manifest["build"]
if build != embedded:
    raise SystemExit("manifest build object must be the binary's embedded record verbatim")
if build.get("revision") != revision:
    raise SystemExit(f"manifest revision {build.get('revision')!r} must equal {revision!r}")
if build.get("revisionKnown") is not True:
    raise SystemExit("manifest revisionKnown must be true")
PY
}

build_target_from_binary() {
  local binary_path="$1"
  "$binary_path" build-info --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["target"])'
}

package_binary() {
  local source_dir="$1"
  local target_dir="$2"
  local output_dir="$3"
  local binary_path target_triple
  binary_path="$target_dir/release/flapjack"
  [ -x "$binary_path" ] || die "expected executable binary at $binary_path"
  target_triple="$(build_target_from_binary "$binary_path")"
  "$source_dir/package/release_artifact_manifest" "$target_triple" "$binary_path" "$output_dir" >/dev/null
}

# shellcheck source-path=SCRIPTDIR
# shellcheck source=lib/build_identity_package_assertions.sh
source "$SCRIPT_DIR/lib/build_identity_package_assertions.sh"
# Distinguishes a host/toolchain locality failure (which must DEFER, not fail)
# from a real passthrough defect. Only concrete failure signatures belong here.
# The container prints a benign "expected behaviour if you are running under
# QEMU" jemalloc notice on every emulated run, including successful ones, so
# neither that phrase nor a bare `qemu` mention may be treated as a failure —
# doing so turns every unexplained cross failure into a false deferral.
# A native C toolchain crashing inside the container (observed with zstd-sys:
# aarch64-linux-musl-gcc SIGSEGVs while assembling under emulation) is locality,
# never a passthrough defect, so the concrete SIGSEGV signature belongs here too.
cross_kat_env_signature() {
  local log_path="$1"
  grep -Eqi \
    'qemu: uncaught target signal|rosetta error|Exec format error|Cannot connect to the Docker daemon|Unable to find image|error pulling image|failed to pull|failed to resolve reference|no space left|could not resolve|operation not permitted \(os error 1\)|permission denied \(os error 13\)|signal: 11 \(SIGSEGV\)' \
    "$log_path"
}

# A bound-exhausted run defers only when the log proves the container was still
# making forward progress under emulation and never printed a compiler error.
# That combination is the Stage 1 QEMU locality failure, where the whole
# dependency graph simply cannot be checked inside any sane bound. A bare
# timeout without that evidence stays a hard failure.
cross_kat_emulated_progress_without_error() {
  local log_path="$1"
  grep -Fq 'running under QEMU' "$log_path" || return 1
  grep -Eq '^[[:space:]]+(Checking|Compiling) ' "$log_path" || return 1
  ! grep -Eq '^error(\[|:)' "$log_path"
}

# The KAT's oracle is the build-script emission, not the exit status of the
# whole check: `engine/build.rs` runs early, while checking the rest of the
# dependency graph under emulation can take far longer than any sane bound.
# Emitting a line here means build.rs ran inside the container, so the poll can
# stop the supervisor as soon as the answer exists.
# "Not emitted yet" is the normal polling state, so an empty result is success
# for this reader; `pipefail` would otherwise turn every poll into a fatal error.
# Cargo writes a build script's captured output to
# <target-dir>/<profile>/build/<package>-<hash>/output, one directory deeper when
# the unit is built for a cross target. Naming those two layouts keeps the poll
# off the multi-gigabyte dependency tree it is racing against: a reused target
# cache is several GB, and re-walking it every interval competes for I/O with the
# emulated build the poll is waiting on.
build_script_output_paths() {
  local path
  for path in \
    "$CROSS_KAT_TARGET_DIR"/*/build/flapjack-*/output \
    "$CROSS_KAT_TARGET_DIR"/*/*/build/flapjack-*/output; do
    [ -f "$path" ] && printf '%s\n' "$path"
  done
  return 0
}

build_script_revision_emission() {
  local output_file
  while IFS= read -r output_file; do
    grep -hs -m1 -- 'cargo:rustc-env=FLAPJACK_INTERNAL_BUILD_REVISION=' \
      "$output_file" && return 0
  done < <(build_script_output_paths)
  return 0
}

# Polls until build.rs emits a revision, the supervisor exits, or the bound
# elapses, publishing the outcome in CROSS_KAT_EMISSION and
# CROSS_KAT_BOUND_EXHAUSTED.
wait_for_build_script_revision_emission() {
  local supervisor_pid="$1"
  local bound_seconds="$2"
  local elapsed=0
  while [ "$elapsed" -lt "$bound_seconds" ]; do
    CROSS_KAT_EMISSION="$(build_script_revision_emission)"
    [ -z "$CROSS_KAT_EMISSION" ] || return 0
    if ! kill -0 "$supervisor_pid" 2>/dev/null; then
      CROSS_KAT_EMISSION="$(build_script_revision_emission)"
      return 0
    fi
    sleep "$CROSS_KAT_POLL_INTERVAL"
    elapsed=$((elapsed + CROSS_KAT_POLL_INTERVAL))
  done
  CROSS_KAT_BOUND_EXHAUSTED=1
  CROSS_KAT_EMISSION="$(build_script_revision_emission)"
}

# A revision this host has never built before, so no cached build-script
# emission can satisfy the assertion below.
generate_unused_revision() {
  LC_ALL=C head -c 20 /dev/urandom | od -An -tx1 | tr -d ' \n'
}

# A reused target directory still holds the previous run's emission, which the
# poll below would otherwise read as this run's answer. Clearing it first makes
# the oracle report only what this invocation's build script wrote.
clear_stale_build_script_emissions() {
  local output_file
  while IFS= read -r output_file; do
    rm -f "$output_file"
  done < <(build_script_output_paths)
}

# The clearing above deletes build-script output inside the supplied directory, so
# a reusable cache is only accepted when this contract owns it: empty on first use
# (claimed now) or already carrying the marker. Without this check,
# CROSS_KAT_TARGET_CACHE pointed at a shared engine/target would silently discard
# build-script output nobody offered to this test.
claim_cross_kat_target_cache() {
  local cache_dir marker
  mkdir -p "$CROSS_KAT_TARGET_CACHE"
  cache_dir="$(cd "$CROSS_KAT_TARGET_CACHE" && pwd)"
  marker="$cache_dir/$CROSS_KAT_CACHE_MARKER"
  if [ ! -f "$marker" ]; then
    [ -z "$(ls -A "$cache_dir")" ] || die \
      "CROSS_KAT_TARGET_CACHE must be an empty directory or one already claimed with $CROSS_KAT_CACHE_MARKER: $cache_dir"
    printf 'flapjack cross passthrough KAT target cache\n' >"$marker"
  fi
  CROSS_KAT_TARGET_DIR="$cache_dir"
  CROSS_KAT_TARGET_DIR_IS_DISPOSABLE=0
}

select_cross_kat_target_dir() {
  if [ -n "$CROSS_KAT_TARGET_CACHE" ]; then
    claim_cross_kat_target_cache
    return
  fi
  CROSS_KAT_TARGET_DIR="$(mktemp -d "${CROSS_KAT_TARGET_ROOT}/flapjack_cross_kat.XXXXXX")"
  CROSS_KAT_TARGET_DIR_IS_DISPOSABLE=1
}

require_positive_integer_bound() {
  local name="$1"
  local value="$2"
  local ceiling="$3"
  if ! [[ "$value" =~ ^[1-9][0-9]*$ ]] || [ "$value" -gt "$ceiling" ]; then
    die "$name must be an integer from 1 through $ceiling seconds, got '$value'"
  fi
}

require_cross_available() {
  command -v cross >/dev/null 2>&1 && return 0
  printf 'DEFERRED-ENV: cross is not installed; cross-check passthrough KAT cannot run on this host\n' >&2
  exit 3
}

# Both the KAT and the warmup mode need engine/build.rs to run inside the same
# container command; they differ only in the bound and in what they do with the
# emission. `exec` makes the recorded PID the bounded supervisor itself, so
# stopping it stops the exact process this contract started and nothing else.
# `--keep-going` is load-bearing: engine/build.rs depends on none of the C
# dependencies, but cargo aborts the whole run on the first failed unit, and
# zstd-sys reliably crashes the emulated C toolchain on this host. Keeping going
# lets the build script still run so the oracle gets an answer instead of a
# verdict about an unrelated dependency.
start_bounded_cross_check() {
  local bound_seconds="$1"
  local log_path="$2"
  local container_opts
  CROSS_KAT_RUN_ID="kat_${$}_$(date +%s)_${RANDOM:-0}"
  container_opts="${CROSS_CONTAINER_OPTS:+${CROSS_CONTAINER_OPTS} }--label=${CROSS_KAT_CONTAINER_LABEL}=${CROSS_KAT_RUN_ID}"
  (
    cd "$ENGINE_DIR"
    exec env \
      FLAPJACK_BUILD_REVISION="$CROSS_KAT_FAKE_REVISION" \
      CARGO_TARGET_DIR="$CROSS_KAT_TARGET_DIR" \
      CROSS_CONTAINER_OPTS="$container_opts" \
      timeout "$bound_seconds" \
      cross check --package flapjack --no-default-features --keep-going \
      --target "$CROSS_KAT_TARGET"
  ) >"$log_path" 2>&1 &
  CROSS_KAT_SUPERVISOR_PID=$!
}

stop_cross_supervisor() {
  CROSS_KAT_SUPERVISOR_STATUS=0
  kill -TERM "$CROSS_KAT_SUPERVISOR_PID" 2>/dev/null || true
  wait "$CROSS_KAT_SUPERVISOR_PID" || CROSS_KAT_SUPERVISOR_STATUS=$?
}

# Shared exit path for "build.rs never emitted": a host/toolchain locality failure
# defers, and anything else is a hard failure. Both modes classify identically so
# a deferral can never mean two different things.
exit_for_missing_build_script_emission() {
  local log_path="$1"
  local bound_seconds="$2"
  capture_cross_container_diagnostics "$log_path"
  tail -20 "$log_path" >&2 || true
  if cross_kat_env_signature "$log_path"; then
    printf 'DEFERRED-ENV: cross check failed with a concrete host/toolchain locality signature (status %s); Stage 4 CI remains the binary-artifact oracle\n' \
      "$CROSS_KAT_SUPERVISOR_STATUS" >&2
    exit 3
  fi
  if [ "$CROSS_KAT_BOUND_EXHAUSTED" -eq 1 ] \
    && cross_kat_emulated_progress_without_error "$log_path"; then
    printf 'DEFERRED-ENV: the %ss bound elapsed while the emulated container was still compiling without error, so engine/build.rs never ran on this host; Stage 4 CI remains the binary-artifact oracle\n' \
      "$bound_seconds" >&2
    exit 3
  fi
  die "cross produced no build-script revision emission and no environment failure signature (status $CROSS_KAT_SUPERVISOR_STATUS)"
}

# Warms a reusable target cache until engine/build.rs has run once inside the
# container. Compiling the build script and its build-dependencies under
# emulation takes far longer than the KAT's own ceiling allows, so this mode owns
# that half; the KAT then only needs the build script to re-run, which
# `cargo:rerun-if-env-changed=FLAPJACK_BUILD_REVISION` guarantees for a revision
# no earlier run used.
warm_cross_kat_target_cache() {
  local log_path
  require_cross_available
  [ -n "$CROSS_KAT_TARGET_CACHE" ] || die \
    "CROSS_KAT_TARGET_CACHE must name the reusable target directory to warm"
  require_positive_integer_bound CROSS_KAT_WARMUP_TIMEOUT \
    "$CROSS_KAT_WARMUP_TIMEOUT" "$CROSS_KAT_WARMUP_TIMEOUT_CEILING"
  require_positive_integer_bound CROSS_KAT_POLL_INTERVAL \
    "$CROSS_KAT_POLL_INTERVAL" "$CROSS_KAT_WARMUP_TIMEOUT"
  CROSS_KAT_FAKE_REVISION="$(generate_unused_revision)"
  claim_cross_kat_target_cache
  clear_stale_build_script_emissions
  log_path="$TMP_ROOT/cross_kat_warmup.log"
  start_bounded_cross_check "$CROSS_KAT_WARMUP_TIMEOUT" "$log_path"
  wait_for_build_script_revision_emission \
    "$CROSS_KAT_SUPERVISOR_PID" "$CROSS_KAT_WARMUP_TIMEOUT"
  stop_cross_supervisor
  if [ -z "$CROSS_KAT_EMISSION" ]; then
    exit_for_missing_build_script_emission "$log_path" "$CROSS_KAT_WARMUP_TIMEOUT"
  fi
  log "cross passthrough KAT target cache warmed: engine/build.rs ran inside the container for $CROSS_KAT_TARGET_DIR"
}

assert_cross_revision_passthrough() {
  local delivered_revision head_revision log_path
  require_cross_available
  require_positive_integer_bound CROSS_KAT_TIMEOUT \
    "$CROSS_KAT_TIMEOUT" "$CROSS_KAT_TIMEOUT_CEILING"
  require_positive_integer_bound CROSS_KAT_POLL_INTERVAL \
    "$CROSS_KAT_POLL_INTERVAL" "$CROSS_KAT_TIMEOUT"
  head_revision="$(git -C "$REPO_DIR" rev-parse HEAD)"
  CROSS_KAT_FAKE_REVISION="$(generate_unused_revision)"
  [[ "$CROSS_KAT_FAKE_REVISION" =~ ^[0-9a-f]{40}$ ]] \
    || die "generated KAT revision must be 40 lowercase hex characters: $CROSS_KAT_FAKE_REVISION"
  [ "$CROSS_KAT_FAKE_REVISION" != "$head_revision" ] \
    || die "fake KAT revision must differ from HEAD to prove delivery"

  select_cross_kat_target_dir
  clear_stale_build_script_emissions
  log_path="$TMP_ROOT/cross_kat.log"

  start_bounded_cross_check "$CROSS_KAT_TIMEOUT" "$log_path"
  wait_for_build_script_revision_emission \
    "$CROSS_KAT_SUPERVISOR_PID" "$CROSS_KAT_TIMEOUT"
  stop_cross_supervisor

  if [ -z "$CROSS_KAT_EMISSION" ]; then
    exit_for_missing_build_script_emission "$log_path" "$CROSS_KAT_TIMEOUT"
  fi

  delivered_revision="${CROSS_KAT_EMISSION##*FLAPJACK_INTERNAL_BUILD_REVISION=}"
  [ "$delivered_revision" = "$CROSS_KAT_FAKE_REVISION" ] \
    || die "engine/Cross.toml did not deliver FLAPJACK_BUILD_REVISION to build.rs: build.rs emitted '${delivered_revision}', expected '${CROSS_KAT_FAKE_REVISION}'"
  log "cross passthrough KAT: engine/Cross.toml delivered FLAPJACK_BUILD_REVISION=${CROSS_KAT_FAKE_REVISION} to build.rs"
}

# TODO: Document copy_engine_tree.
copy_engine_tree() {
  local destination="$1"
  mkdir -p "$destination"
  (
    cd "$ENGINE_DIR"
    tar \
      --exclude='./target' \
      --exclude='./dashboard/node_modules' \
      --exclude='./dashboard/dist' \
      --exclude='./.git' \
      -cf - .
  ) | (
    cd "$destination"
    tar -xf -
  )
}

# TODO: Document assert_manifest_contract.
assert_manifest_contract() {
  local first_manifest="$1"
  local first_output="$2"
  local second_manifest="$3"
  local mutated_manifest="$4"
  python3 - "$first_manifest" "$first_output" "$second_manifest" "$mutated_manifest" <<'PY'
import hashlib
import json
import pathlib
import sys

first_manifest = pathlib.Path(sys.argv[1])
first_output = pathlib.Path(sys.argv[2])
second_manifest = pathlib.Path(sys.argv[3])
mutated_manifest = pathlib.Path(sys.argv[4])

first = json.loads(first_manifest.read_text())
second = json.loads(second_manifest.read_text())
mutated = json.loads(mutated_manifest.read_text())

if first["build"] != second["build"]:
    raise SystemExit("unchanged source builds must report identical canonical build JSON")
if first["build"]["workspaceDigest"] == mutated["build"]["workspaceDigest"]:
    raise SystemExit("workspaceDigest must change when an included build-identity source changes")

artifact = first.get("artifact") or {}
archive = first_output / artifact.get("file", "")
if not archive.is_file():
    raise SystemExit(f"manifest artifact file does not exist: {archive}")
expected_arch = artifact["target"].split("-", 1)[0]
if artifact.get("target") != first["build"].get("target"):
    raise SystemExit("artifact.target must match build.target")
if artifact.get("arch") != expected_arch:
    raise SystemExit(f"artifact.arch mismatch: {artifact.get('arch')} != {expected_arch}")
if artifact.get("profile") != "release":
    raise SystemExit("artifact.profile must be release")
if first["build"].get("profile") != "release":
    raise SystemExit("build.profile must be release")
if set(first["build"].get("capabilities") or {}) != {"vectorSearch", "vectorSearchLocal"}:
    raise SystemExit(f"capability keys are not canonical: {first['build'].get('capabilities')}")
if first.get("schemaVersion") != 1:
    raise SystemExit("schemaVersion must be 1")
if set(artifact) != {"file", "target", "arch", "profile", "sha256"}:
    raise SystemExit(f"artifact keys mismatch: {sorted(artifact)}")

archive_sha = hashlib.sha256(archive.read_bytes()).hexdigest()
if artifact.get("sha256") != archive_sha:
    raise SystemExit("artifact.sha256 must equal digest of final archive bytes")

sidecar = pathlib.Path(str(archive) + ".sha256")
parts = sidecar.read_text().strip().split()
if parts != [archive_sha, archive.name]:
    raise SystemExit(f"checksum sidecar must verify the same archive, got: {parts}")

mutated_archive = archive.with_name(archive.name + ".mutated")
mutated_archive.write_bytes(archive.read_bytes() + b"x")
mutated_sha = hashlib.sha256(mutated_archive.read_bytes()).hexdigest()
if mutated_sha == artifact["sha256"]:
    raise SystemExit("mutating one archive byte must invalidate artifact.sha256")

serialized = json.dumps(first, sort_keys=True, separators=(",", ":"))
for spelling in ("algolia_migration_v1", "algoliaMigrationV1"):
    if spelling in serialized:
        raise SystemExit(f"forbidden migration capability spelling present: {spelling}")
PY
}

require_tools
TMP_ROOT="$(mktemp -d)"

case "${1:-}" in
  --foreign-target-only)
    assert_foreign_target_manifest_contract "$PACKAGE_HELPER" "foreign_target"
    assert_malformed_embedded_records_rejected
    assert_traversal_target_rejected_without_outside_write
    assert_incomplete_build_record_rejected
    log "foreign target manifest contract passed for $FOREIGN_TARGET"
    exit 0
    ;;
  --linux-musl-native-mismatch-only)
    assert_linux_musl_cli_mismatch_rejected
    log "x86_64 Linux musl CLI/embedded mismatch was rejected"
    exit 0
    ;;
  --real-release-embedded-record-only)
    assert_real_release_embedded_record
    log "real optimized release binary embeds exactly one build-info record"
    exit 0
    ;;
  --foreign-target-repair-proof)
    die "--foreign-target-repair-proof was removed after the production helper took over embedded metadata extraction"
    ;;
  --cross-passthrough-only)
    assert_cross_revision_passthrough
    log "cross passthrough KAT passed for $CROSS_KAT_TARGET"
    exit 0
    ;;
  --cross-passthrough-warmup)
    warm_cross_kat_target_cache
    exit 0
    ;;
  "")
    assert_foreign_target_manifest_contract "$PACKAGE_HELPER" "foreign_target"
    assert_malformed_embedded_records_rejected
    assert_traversal_target_rejected_without_outside_write
    assert_incomplete_build_record_rejected
    ;;
  *)
    die "unknown argument: $1"
    ;;
esac

REVISION="$(git -C "$REPO_DIR" rev-parse HEAD)"
if ! [[ "$REVISION" =~ ^[0-9a-f]{40}$ ]]; then
  die "git revision must be exactly 40 lowercase hex characters: $REVISION"
fi

TARGET_ONE="$TMP_ROOT/target_one"
TARGET_TWO="$TMP_ROOT/target_two"
OUT_ONE="$TMP_ROOT/out_one"
OUT_TWO="$TMP_ROOT/out_two"
MUTATED_ENGINE="$TMP_ROOT/mutated_engine"
MUTATED_TARGET="$TMP_ROOT/mutated_target"
MUTATED_OUT="$TMP_ROOT/mutated_out"
mkdir -p "$OUT_ONE" "$OUT_TWO" "$MUTATED_OUT"

build_release_binary "$ENGINE_DIR" "$TARGET_ONE" "$REVISION"
package_binary "$ENGINE_DIR" "$TARGET_ONE" "$OUT_ONE"

build_release_binary "$ENGINE_DIR" "$TARGET_TWO" "$REVISION"
package_binary "$ENGINE_DIR" "$TARGET_TWO" "$OUT_TWO"

copy_engine_tree "$MUTATED_ENGINE"
printf '\n// package contract digest mutation\n' >>"$MUTATED_ENGINE/src/build_info.rs"
build_release_binary "$MUTATED_ENGINE" "$MUTATED_TARGET" "$REVISION"
package_binary "$MUTATED_ENGINE" "$MUTATED_TARGET" "$MUTATED_OUT"

TARGET_TRIPLE="$(python3 - "$OUT_ONE"/flapjack-*.manifest.json <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
print(manifest["artifact"]["target"])
PY
)"

assert_manifest_contract \
  "$OUT_ONE/flapjack-${TARGET_TRIPLE}.manifest.json" \
  "$OUT_ONE" \
  "$OUT_TWO/flapjack-${TARGET_TRIPLE}.manifest.json" \
  "$MUTATED_OUT/flapjack-${TARGET_TRIPLE}.manifest.json"

log "build identity package contract passed for $TARGET_TRIPLE"
