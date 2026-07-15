#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
PACKAGE_HELPER="$ENGINE_DIR/package/release_artifact_manifest"

TMP_ROOT=""
FAILURE_EVIDENCE_DIR=""

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

log() {
  printf '%s\n' "$*"
}

cleanup() {
  local exit_code=$?
  if [ "$exit_code" -ne 0 ] && [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    FAILURE_EVIDENCE_DIR="${TMPDIR:-/tmp}/flapjack_build_identity_package_failure_${$}_$(date +%s)"
    mkdir -p "$FAILURE_EVIDENCE_DIR"
    cp -R "$TMP_ROOT" "$FAILURE_EVIDENCE_DIR/tmp_root"
    printf 'INFO: preserved build identity package evidence at %s\n' "$FAILURE_EVIDENCE_DIR" >&2
  elif [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT

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
  log "Building flapjack-server in $target_dir"
  (
    cd "$source_dir"
    FLAPJACK_BUILD_REVISION="$revision" \
      CARGO_TARGET_DIR="$target_dir" \
      cargo build --release --package flapjack-server
  )
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
