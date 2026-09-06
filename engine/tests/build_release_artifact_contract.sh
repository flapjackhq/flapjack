#!/usr/bin/env bash

set -euo pipefail

SECONDS=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
HELPER_UNDER_TEST="${HELPER_UNDER_TEST:-$ENGINE_DIR/package/build_release_artifact}"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TMP_ROOT=""

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

cleanup() {
  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT

expect_success() {
  local description="$1"
  shift
  if "$@"; then
    pass "$description"
  else
    sed 's/^/    LOG: /' "$COMMAND_LOG"
    fail "$description"
  fi
}

expect_failure() {
  local description="$1"
  shift
  if "$@" >/dev/null 2>&1; then
    fail "$description"
  else
    pass "$description"
  fi
}

if [ ! -x "$HELPER_UNDER_TEST" ]; then
  printf '  [FAIL] build_release_artifact helper is executable\n'
  printf '\nRESULT: 0 passed, 1 failed, 1 total\n'
  exit 1
fi
pass "build_release_artifact helper is executable"

TMP_ROOT="$(mktemp -d)"
FIXTURE_REPO="$TMP_ROOT/repo"
FAKE_BIN="$TMP_ROOT/fake-bin"
OUTPUT_ROOT="$TMP_ROOT/output"
COMMAND_LOG="$TMP_ROOT/commands.log"
mkdir -p "$FIXTURE_REPO/engine/package" "$FIXTURE_REPO/engine/dashboard" \
  "$FAKE_BIN" "$OUTPUT_ROOT"
OUTPUT_ROOT="$(cd "$OUTPUT_ROOT" && pwd -P)"
cp "$HELPER_UNDER_TEST" "$FIXTURE_REPO/engine/package/build_release_artifact"

cat >"$FIXTURE_REPO/.gitignore" <<'EOF'
/engine/target/
EOF

cat >"$FIXTURE_REPO/engine/dashboard/package-lock.json" <<'EOF'
{}
EOF

cat >"$FIXTURE_REPO/engine/package/engine_compatibility.json" <<'EOF'
{"dataDisposition":"preserve","mixedVersionReplication":"not_guaranteed","schemaVersion":2,"targets":{"aarch64-unknown-linux-musl":[],"x86_64-unknown-linux-musl":[]}}
EOF

cat >"$FIXTURE_REPO/engine/package/release_artifact_manifest" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail

printf 'manifest %s\n' "$*" >>"$FAKE_COMMAND_LOG"
compatibility_source="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/engine_compatibility.json"

if [ "${1:-}" = "--compatibility-source" ]; then
  [ "$#" -ge 3 ] || exit 18
  compatibility_source="$2"
  shift 2
fi

emit_selected() {
  python3 - "$compatibility_source" "$1" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text())
target = sys.argv[2]
if not isinstance(source, dict) or set(source) != {
    "dataDisposition", "mixedVersionReplication", "schemaVersion", "targets"
}:
    raise SystemExit("compatibility source keys mismatch")
if type(source["schemaVersion"]) is not int or source["schemaVersion"] != 2:
    raise SystemExit("compatibility source schema mismatch")
if source["dataDisposition"] != "preserve" or source["mixedVersionReplication"] != "not_guaranteed":
    raise SystemExit("compatibility source policy mismatch")
targets = source["targets"]
if not isinstance(targets, dict) or set(targets) != {
    "aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"
}:
    raise SystemExit("compatibility source targets mismatch")
predecessor_keys = {
    "releaseTag", "manifestSha256", "binarySha256", "transitionMode",
    "forwardTransferMode", "rollbackMode", "parityProfile",
}
allowed_recipes = {
    (
        "routine_same_host", "reuse_same_data_directory",
        "binary_reactivate_same_data", "same_data_upgrade_smoke_v1",
    ),
    (
        "routine_same_host", "reuse_same_data_directory",
        "restore_pre_upgrade_backup", "same_data_upgrade_smoke_v1",
    ),
    (
        "exceptional_blue_green", "snapshot_then_tail_replication",
        "reverse_tail_to_retained_predecessor", "populated_engine_exact_v1",
    ),
}
for predecessors in targets.values():
    if not isinstance(predecessors, list):
        raise SystemExit("compatibility predecessors must be arrays")
    for predecessor in predecessors:
        if not isinstance(predecessor, dict) or set(predecessor) != predecessor_keys:
            raise SystemExit("compatibility predecessor keys mismatch")
        recipe = (
            predecessor["transitionMode"], predecessor["forwardTransferMode"],
            predecessor["rollbackMode"], predecessor["parityProfile"],
        )
        if recipe not in allowed_recipes:
            raise SystemExit("compatibility predecessor recipe mismatch")
selected = {
    "dataDisposition": source["dataDisposition"],
    "mixedVersionReplication": source["mixedVersionReplication"],
    "predecessors": targets.get(target, []),
    "schemaVersion": 1,
    "target": target,
}
print(json.dumps(selected, sort_keys=True, separators=(",", ":")))
PY
}

if [ "$#" -eq 2 ] && [ "$1" = "--compatibility-target" ]; then
  emit_selected "$2"
  exit 0
fi

if [ "$#" -eq 2 ] && [ "$1" = "--compatibility-predecessors" ]; then
  [ "${FAKE_MANIFEST_MODE:-valid}" != "compatibility-failure" ] || exit 19
  python3 - "$compatibility_source" "$2" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text())
manifest = json.loads(pathlib.Path(sys.argv[2]).read_text())
target = manifest["build"]["target"]
selected = {
    "dataDisposition": source["dataDisposition"],
    "mixedVersionReplication": source["mixedVersionReplication"],
    "predecessors": source["targets"].get(target, []),
    "schemaVersion": 1,
    "target": target,
}
if manifest.get("compatibility") != selected:
    raise SystemExit("candidate compatibility differs from selected source")
PY
  exit 0
fi

[ "$#" -eq 3 ] || exit 64
target="$1"
binary="$2"
output="$3"
[ -f "$binary" ] || exit 65
mkdir -p "$output"

case "$target" in
  *-windows-*) archive="flapjack-${target}.zip" ;;
  *) archive="flapjack-${target}.tar.gz" ;;
esac

if [ "${FAKE_MANIFEST_MODE:-valid}" = "missing-output" ]; then
  exit 0
fi

printf 'archive:%s\n' "$target" >"$output/$archive"
python3 - "$output/$archive" "$output/$archive.sha256" <<'PY'
import hashlib
import pathlib
import sys

archive = pathlib.Path(sys.argv[1])
pathlib.Path(sys.argv[2]).write_text(
    f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n"
)
PY

manifest_target="$target"
manifest_revision="$FLAPJACK_BUILD_REVISION"
manifest_dirty=null
manifest_dirty_known=false
case "${FAKE_MANIFEST_MODE:-valid}" in
  wrong-target) manifest_target="x86_64-unknown-linux-musl" ;;
  wrong-revision) manifest_revision="0000000000000000000000000000000000000000" ;;
  dirty-build) manifest_dirty=true; manifest_dirty_known=true ;;
esac

python3 - "$output/flapjack-${target}.manifest.json" "$manifest_target" \
  "$manifest_revision" "$manifest_dirty" "$manifest_dirty_known" "$archive" \
  "$compatibility_source" <<'PY'
import hashlib
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
target = sys.argv[2]
revision = sys.argv[3]
dirty = {"null": None, "false": False, "true": True}[sys.argv[4]]
dirty_known = sys.argv[5] == "true"
archive_name = sys.argv[6]
source_compatibility = json.loads(pathlib.Path(sys.argv[7]).read_text())
selected_compatibility = {
    "dataDisposition": source_compatibility["dataDisposition"],
    "mixedVersionReplication": source_compatibility["mixedVersionReplication"],
    "predecessors": source_compatibility["targets"].get(target, []),
    "schemaVersion": 1,
    "target": target,
}
archive_path = manifest_path.parent / archive_name
manifest = {
    "schemaVersion": 2,
    "artifact": {
        "arch": target.split("-", 1)[0],
        "binarySha256": "1" * 64,
        "file": archive_name,
        "profile": "release",
        "sha256": hashlib.sha256(archive_path.read_bytes()).hexdigest(),
        "target": target,
    },
    "build": {
        "capabilities": {"vectorSearch": target != "x86_64-pc-windows-msvc", "vectorSearchLocal": False},
        "dirty": dirty,
        "dirtyKnown": dirty_known,
        "features": [] if target == "x86_64-pc-windows-msvc" else ["vector-search"],
        "profile": "release",
        "revision": revision,
        "revisionKnown": True,
        "schemaVersion": 1,
        "target": target,
        "version": "1.0.16",
        "workspaceDigest": "2" * 64,
    },
    "compatibility": selected_compatibility,
}
manifest_path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n")
PY
EOF
chmod +x "$FIXTURE_REPO/engine/package/release_artifact_manifest"

cat >"$FAKE_BIN/npm" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf '10.8.3\n'
  exit 0
fi
printf 'npm %s\n' "$*" >>"$FAKE_COMMAND_LOG"
EOF

cat >"$FAKE_BIN/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "--version" ]; then
  printf '%s 1.89.0 (fixture)\n' "$(basename "$0")"
  exit 0
fi
[ "${FLAPJACK_BUILD_REVISION:-}" = "${FAKE_EXPECTED_SOURCE_SHA:-missing}" ] || exit 67
[ "${FLAPJACK_REQUIRE_DASHBOARD:-}" = "1" ] || exit 68
printf '%s %s\n' "$(basename "$0")" "$*" >>"$FAKE_COMMAND_LOG"
target=""
while [ "$#" -gt 0 ]; do
  if [ "$1" = "--target" ]; then
    target="$2"
    break
  fi
  shift
done
[ -n "$target" ] || exit 66
binary="target/${target}/release/flapjack"
case "$target" in *-windows-*) binary="${binary}.exe" ;; esac
mkdir -p "$(dirname "$binary")"
printf 'binary:%s\n' "$target" >"$binary"
chmod +x "$binary"
if [ "${FAKE_BUILD_MUTATE_TRACKED:-0}" = "1" ]; then
  printf 'mutated\n' >>dashboard/package-lock.json
fi
EOF

cp "$FAKE_BIN/cargo" "$FAKE_BIN/cross"
cat >"$FAKE_BIN/node" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = "--version" ] || exit 64
printf '%s\n' "${FAKE_NODE_VERSION:-v20.19.4}"
EOF
cat >"$FAKE_BIN/rustc" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[ "${1:-}" = "--version" ] || exit 64
printf 'rustc 1.89.0 (fixture)\n'
EOF
cat >"$FAKE_BIN/uname" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  -s) printf 'FixtureOS\n' ;;
  -m) printf 'fixture-arch\n' ;;
  *) exit 64 ;;
esac
EOF
chmod +x "$FAKE_BIN/npm" "$FAKE_BIN/cargo" "$FAKE_BIN/cross" \
  "$FAKE_BIN/node" "$FAKE_BIN/rustc" "$FAKE_BIN/uname"

git -C "$FIXTURE_REPO" init -q
git -C "$FIXTURE_REPO" config user.name "Release Contract"
git -C "$FIXTURE_REPO" config user.email "release-contract@example.invalid"
git -C "$FIXTURE_REPO" add .
git -C "$FIXTURE_REPO" commit -qm "fixture"
SOURCE_SHA="$(git -C "$FIXTURE_REPO" rev-parse HEAD)"
SOURCE_TREE="$(git -C "$FIXTURE_REPO" rev-parse 'HEAD^{tree}')"
CANONICAL_COMPATIBILITY="$FIXTURE_REPO/engine/package/engine_compatibility.json"

EXTERNAL_COMPATIBILITY="$TMP_ROOT/external-engine-compatibility.json"
cat >"$EXTERNAL_COMPATIBILITY" <<'EOF'
{"dataDisposition":"preserve","mixedVersionReplication":"not_guaranteed","schemaVersion":2,"targets":{"aarch64-unknown-linux-musl":[{"binarySha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","forwardTransferMode":"reuse_same_data_directory","manifestSha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","parityProfile":"same_data_upgrade_smoke_v1","releaseTag":"v1.0.16-private","rollbackMode":"binary_reactivate_same_data","transitionMode":"routine_same_host"}],"x86_64-unknown-linux-musl":[]}}
EOF

run_helper() {
  run_helper_with_compatibility "$CANONICAL_COMPATIBILITY" "$@"
}

run_helper_with_compatibility() {
  local compatibility_source="$1"
  shift
  local profile="$1"
  local target="$2"
  local output_name="$3"
  shift 3
  (
    cd "$FIXTURE_REPO"
    env \
      PATH="$FAKE_BIN:$PATH" \
      FAKE_COMMAND_LOG="$COMMAND_LOG" \
      FAKE_EXPECTED_SOURCE_SHA="$SOURCE_SHA" \
      "$@" \
      engine/package/build_release_artifact \
        "$profile" "$target" "$SOURCE_SHA" "$SOURCE_TREE" \
        "$compatibility_source" "$OUTPUT_ROOT/$output_name"
  )
}

assert_logged_build() {
  local target="$1"
  local builder="$2"
  local feature_pattern="$3"
  local description="$4"
  local expected_binary="target/${target}/release/flapjack"
  case "$target" in *-windows-*) expected_binary="${expected_binary}.exe" ;; esac

  if [ "$(wc -l <"$COMMAND_LOG" | tr -d ' ')" = "6" ] \
    && [ "$(grep -Fxc "npm ci" "$COMMAND_LOG")" = "1" ] \
    && [ "$(grep -Fxc "npm run build" "$COMMAND_LOG")" = "1" ] \
    && [ "$(grep -Ec "^${builder} build --release --target ${target} --package flapjack-server --no-default-features${feature_pattern}$" "$COMMAND_LOG")" = "1" ] \
    && [ "$(grep -Ec "^manifest --compatibility-source .+ --compatibility-target ${target}$" "$COMMAND_LOG")" = "1" ] \
    && [ "$(grep -Ec "^manifest --compatibility-source .+ ${target} ${expected_binary} $OUTPUT_ROOT/current$" "$COMMAND_LOG")" = "1" ] \
    && [ "$(grep -Ec "^manifest --compatibility-source .+ --compatibility-predecessors $OUTPUT_ROOT/current/flapjack-${target}.manifest.json$" "$COMMAND_LOG")" = "1" ]; then
    pass "$description"
  else
    sed 's/^/    LOG: /' "$COMMAND_LOG"
    fail "$description"
  fi
}

assert_build_receipt() {
  local profile="$1"
  local target="$2"
  local builder="$3"
  local feature="$4"
  local receipt="$OUTPUT_ROOT/current/flapjack-${target}.build.json"
  local manifest="$OUTPUT_ROOT/current/flapjack-${target}.manifest.json"
  local compatibility_source="${5:-$CANONICAL_COMPATIBILITY}"
  if python3 - "$receipt" "$manifest" "$profile" "$target" "$builder" \
    "$feature" "$SOURCE_SHA" "$SOURCE_TREE" "$compatibility_source" <<'PY'
import hashlib
import json
import pathlib
import sys

receipt_path = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
profile = sys.argv[3]
target = sys.argv[4]
builder = sys.argv[5]
feature = sys.argv[6]
source_sha = sys.argv[7]
source_tree = sys.argv[8]
compatibility_source = pathlib.Path(sys.argv[9])
source_compatibility = json.loads(compatibility_source.read_text())
selected_compatibility = {
    "dataDisposition": source_compatibility["dataDisposition"],
    "mixedVersionReplication": source_compatibility["mixedVersionReplication"],
    "predecessors": source_compatibility["targets"].get(target, []),
    "schemaVersion": 1,
    "target": target,
}

receipt = json.loads(receipt_path.read_text())
expected_argv = [
    builder, "build", "--release", "--target", target,
    "--package", "flapjack-server", "--no-default-features",
]
if feature:
    expected_argv.extend(["--features", feature])
expected = {
    "schemaVersion": 2,
    "profile": profile,
    "source": {"clean": True, "sha": source_sha, "tree": source_tree},
    "invocation": {
        "argv": expected_argv,
        "runner": {"arch": "fixture-arch", "os": "FixtureOS"},
        "tools": {
            "cargo": "cargo 1.89.0 (fixture)",
            "cross": "cross 1.89.0 (fixture)" if builder == "cross" else None,
            "node": "v20.19.4",
            "npm": "10.8.3",
            "rustc": "rustc 1.89.0 (fixture)",
        },
    },
    "manifest": {
        "file": manifest_path.name,
        "sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
    },
    "compatibility": {
        "selectedSha256": hashlib.sha256(
            (json.dumps(selected_compatibility, sort_keys=True, separators=(",", ":")) + "\n").encode()
        ).hexdigest(),
        "sourceSha256": hashlib.sha256(compatibility_source.read_bytes()).hexdigest(),
    },
}
if receipt != expected:
    raise SystemExit(f"unexpected build receipt: {receipt!r}")
if receipt_path.read_text() != json.dumps(expected, sort_keys=True, separators=(",", ":")) + "\n":
    raise SystemExit("build receipt is not canonical JSON")
PY
  then
    pass "$profile records exact source, invocation, toolchain, runner, and manifest identity for $target"
  else
    fail "$profile records exact source, invocation, toolchain, runner, and manifest identity for $target"
  fi
}

run_external_compatibility_case() {
  : >"$COMMAND_LOG"
  rm -rf "$OUTPUT_ROOT/current"
  if run_helper_with_compatibility "$EXTERNAL_COMPATIBILITY" \
    cloud-arm64 aarch64-unknown-linux-musl current; then
    pass "cloud-arm64 accepts an explicit separately published exact predecessor declaration"
  else
    fail "cloud-arm64 accepts an explicit separately published exact predecessor declaration"
    return
  fi
  assert_logged_build aarch64-unknown-linux-musl cross ' --features vector-search' \
    "external compatibility source still delegates one exact canonical build and package"
  assert_build_receipt cloud-arm64 aarch64-unknown-linux-musl cross vector-search \
    "$EXTERNAL_COMPATIBILITY"
  if python3 - "$OUTPUT_ROOT/current/flapjack-aarch64-unknown-linux-musl.manifest.json" \
    "$EXTERNAL_COMPATIBILITY" <<'PY'
import json
import pathlib
import sys

manifest = json.loads(pathlib.Path(sys.argv[1]).read_text())
source = json.loads(pathlib.Path(sys.argv[2]).read_text())
if manifest["compatibility"]["predecessors"] != source["targets"]["aarch64-unknown-linux-musl"]:
    raise SystemExit("manifest did not embed the explicit selected declaration")
PY
  then
    pass "manifest embeds the exact target-selected external predecessor declaration"
  else
    fail "manifest embeds the exact target-selected external predecessor declaration"
  fi

  local changed_compatibility="$TMP_ROOT/changed-external-compatibility.json"
  python3 - "$EXTERNAL_COMPATIBILITY" "$changed_compatibility" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text())
source["targets"]["aarch64-unknown-linux-musl"][0]["rollbackMode"] = "restore_pre_upgrade_backup"
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(source, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
  if python3 - "$OUTPUT_ROOT/current/flapjack-aarch64-unknown-linux-musl.build.json" \
    "$EXTERNAL_COMPATIBILITY" "$changed_compatibility" <<'PY'
import hashlib
import json
import pathlib
import sys

receipt = json.loads(pathlib.Path(sys.argv[1]).read_text())
before_path = pathlib.Path(sys.argv[2])
after_path = pathlib.Path(sys.argv[3])
before = json.loads(before_path.read_text())
after = json.loads(after_path.read_text())
target = "aarch64-unknown-linux-musl"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def selected(source):
    value = {
        "dataDisposition": source["dataDisposition"],
        "mixedVersionReplication": source["mixedVersionReplication"],
        "predecessors": source["targets"][target],
        "schemaVersion": 1,
        "target": target,
    }
    raw = (json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n").encode()
    return hashlib.sha256(raw).hexdigest()


if receipt["compatibility"] != {
    "sourceSha256": digest(before_path),
    "selectedSha256": selected(before),
}:
    raise SystemExit("receipt does not bind the original compatibility coordinates")
if digest(before_path) == digest(after_path):
    raise SystemExit("source coordinate did not change")
if selected(before) == selected(after):
    raise SystemExit("selected coordinate did not change")
PY
  then
    pass "one predecessor recipe byte change moves the receipt's source and selected coordinates"
  else
    fail "one predecessor recipe byte change moves the receipt's source and selected coordinates"
  fi
}

run_valid_case() {
  local profile="$1"
  local target="$2"
  local builder="$3"
  local feature_pattern="$4"
  : >"$COMMAND_LOG"
  rm -rf "$OUTPUT_ROOT/current"
  if run_helper "$profile" "$target" current; then
    pass "$profile accepts its closed target $target"
  else
    fail "$profile accepts its closed target $target"
    return
  fi
  assert_logged_build "$target" "$builder" "$feature_pattern" \
    "$profile delegates dashboard, one exact build, packaging, and compatibility to the canonical owners for $target"
  if [ -n "$feature_pattern" ]; then
    assert_build_receipt "$profile" "$target" "$builder" vector-search
  else
    assert_build_receipt "$profile" "$target" "$builder" ""
  fi
}

run_valid_case public-all x86_64-unknown-linux-musl cross ' --features vector-search'
run_valid_case public-all x86_64-apple-darwin cargo ' --features vector-search'
run_valid_case public-all x86_64-pc-windows-msvc cargo ''
run_external_compatibility_case

expect_failure "cloud-arm64 rejects every non-production target" \
  run_helper cloud-arm64 x86_64-unknown-linux-musl rejected-cloud
expect_failure "public-all rejects targets outside the existing public matrix" \
  run_helper public-all riscv64gc-unknown-linux-gnu rejected-public
expect_failure "unknown build profiles are rejected" \
  run_helper cloud-all aarch64-unknown-linux-musl rejected-profile

ORIGINAL_SHA="$SOURCE_SHA"
SOURCE_SHA="0000000000000000000000000000000000000000"
expect_failure "source SHA must equal the checked-out commit" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl wrong-sha
SOURCE_SHA="$ORIGINAL_SHA"

ORIGINAL_TREE="$SOURCE_TREE"
SOURCE_TREE="0000000000000000000000000000000000000000"
expect_failure "source tree must equal the checked-out commit tree" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl wrong-tree
SOURCE_TREE="$ORIGINAL_TREE"

printf 'untracked\n' >"$FIXTURE_REPO/untracked-source"
expect_failure "untracked source makes the exact candidate fail closed" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl dirty-source
rm "$FIXTURE_REPO/untracked-source"

expect_failure "manifest revision substitution is rejected" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl wrong-manifest-revision \
    FAKE_MANIFEST_MODE=wrong-revision
expect_failure "missing canonical package outputs are rejected" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl missing-package \
    FAKE_MANIFEST_MODE=missing-output
expect_failure "missing explicit compatibility source is rejected" \
  run_helper_with_compatibility "$TMP_ROOT/missing-compatibility.json" \
    cloud-arm64 aarch64-unknown-linux-musl missing-compatibility
INVALID_COMPATIBILITY="$TMP_ROOT/invalid-compatibility.json"
printf '%s\n' '{"schemaVersion":1,"target":"x86_64-unknown-linux-musl"}' >"$INVALID_COMPATIBILITY"
expect_failure "foreign target-selected declarations cannot substitute for a source contract" \
  run_helper_with_compatibility "$INVALID_COMPATIBILITY" \
    cloud-arm64 aarch64-unknown-linux-musl foreign-compatibility
INVALID_RECIPE_COMPATIBILITY="$TMP_ROOT/invalid-recipe-compatibility.json"
python3 - "$EXTERNAL_COMPATIBILITY" "$INVALID_RECIPE_COMPATIBILITY" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text())
source["targets"]["aarch64-unknown-linux-musl"][0]["forwardTransferMode"] = "snapshot_then_tail_replication"
pathlib.Path(sys.argv[2]).write_text(
    json.dumps(source, sort_keys=True, separators=(",", ":")) + "\n"
)
PY
expect_failure "hybrid predecessor recipes are rejected before build" \
  run_helper_with_compatibility "$INVALID_RECIPE_COMPATIBILITY" \
    cloud-arm64 aarch64-unknown-linux-musl invalid-recipe
expect_failure "public profile rejects a separately supplied compatibility source" \
  run_helper_with_compatibility "$EXTERNAL_COMPATIBILITY" \
    public-all aarch64-unknown-linux-musl public-foreign-compatibility
expect_failure "non-Node-20 dashboard toolchains are rejected before build" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl wrong-node \
    FAKE_NODE_VERSION=v22.1.0

expect_failure "tracked source mutation during the build is rejected before handoff" \
  run_helper cloud-arm64 aarch64-unknown-linux-musl build-mutates-source \
    FAKE_BUILD_MUTATE_TRACKED=1
git -C "$FIXTURE_REPO" checkout -q -- engine/dashboard/package-lock.json

if [ "$SECONDS" -le 8 ]; then
  pass "focused helper contract stays within its 8-second hard cap (${SECONDS}s)"
else
  fail "focused helper contract stays within its 8-second hard cap (${SECONDS}s)"
fi

printf '\nRESULT: %d passed, %d failed, %d total\n' \
  "$TESTS_PASSED" "$TESTS_FAILED" "$TESTS_RUN"
[ "$TESTS_FAILED" -eq 0 ]
