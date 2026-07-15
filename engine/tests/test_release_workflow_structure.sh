#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
RELEASE_WORKFLOW="$REPO_DIR/.github/workflows/release.yml"
DOCKER_WORKFLOW="$REPO_DIR/.github/workflows/docker.yml"
RELEASE_MANIFEST_HELPER="$REPO_DIR/engine/package/release_artifact_manifest"

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

assert_not_contains() {
  local file_path="$1"
  local pattern="$2"
  local description="$3"
  if grep -Eq "$pattern" "$file_path"; then
    fail "$description"
  else
    pass "$description"
  fi
}

assert_file_executable() {
  local file_path="$1"
  local description="$2"
  if [ -x "$file_path" ]; then
    pass "$description"
  else
    fail "$description"
  fi
}

assert_release_helper_contract() {
  local tmp_dir bin_path output_dir manifest_path
  tmp_dir="$(mktemp -d)"
  bin_path="$tmp_dir/flapjack"
  output_dir="$tmp_dir/out"
  mkdir -p "$output_dir"

  cat >"$bin_path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ne 2 ] || [ "$1" != "build-info" ] || [ "$2" != "--json" ]; then
  echo "unexpected invocation: $*" >&2
  exit 64
fi
printf '%s\n' '{"schemaVersion":1,"version":"1.2.3","revision":"0123456789abcdef0123456789abcdef01234567","revisionKnown":true,"dirty":false,"dirtyKnown":true,"workspaceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile":"release","target":"x86_64-unknown-linux-gnu","features":["vector-search"],"capabilities":{"vectorSearch":true,"vectorSearchLocal":false}}'
EOF
  chmod +x "$bin_path"

  if "$RELEASE_MANIFEST_HELPER" "x86_64-unknown-linux-gnu" "$bin_path" "$output_dir" >/dev/null 2>&1; then
    manifest_path="$output_dir/flapjack-x86_64-unknown-linux-gnu.manifest.json"
    if python3 - "$manifest_path" "$output_dir/flapjack-x86_64-unknown-linux-gnu.tar.gz" <<'PY'
import hashlib
import json
import pathlib
import sys

manifest_path = pathlib.Path(sys.argv[1])
archive_path = pathlib.Path(sys.argv[2])
manifest = json.loads(manifest_path.read_text())
expected_build = {
    "schemaVersion": 1,
    "version": "1.2.3",
    "revision": "0123456789abcdef0123456789abcdef01234567",
    "revisionKnown": True,
    "dirty": False,
    "dirtyKnown": True,
    "workspaceDigest": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "profile": "release",
    "target": "x86_64-unknown-linux-gnu",
    "features": ["vector-search"],
    "capabilities": {"vectorSearch": True, "vectorSearchLocal": False},
}
expected_artifact = {
    "file": archive_path.name,
    "target": "x86_64-unknown-linux-gnu",
    "arch": "x86_64",
    "profile": "release",
    "sha256": hashlib.sha256(archive_path.read_bytes()).hexdigest(),
}
if manifest.get("schemaVersion") != 1:
    raise SystemExit("manifest schemaVersion must be 1")
if manifest.get("artifact") != expected_artifact:
    raise SystemExit(f"artifact contract mismatch: {manifest.get('artifact')}")
if manifest.get("build") != expected_build:
    raise SystemExit(f"build object must be copied verbatim: {manifest.get('build')}")
serialized = json.dumps(manifest, sort_keys=True, separators=(",", ":"))
for forbidden in ("algolia_migration_v1", "algoliaMigrationV1"):
    if forbidden in serialized:
        raise SystemExit(f"forbidden migration capability spelling present: {forbidden}")
PY
    then
      pass "release_artifact_manifest writes schemaVersion, artifact fields, and verbatim canonical build object"
    else
      fail "release_artifact_manifest writes schemaVersion, artifact fields, and verbatim canonical build object"
    fi
  else
    fail "release_artifact_manifest accepts target, binary path, and output directory CLI"
  fi

  rm -rf "$tmp_dir"
}

section "Release workflow sequencing"
assert_contains "$RELEASE_WORKFLOW" '^\s*validate_release_version:' "release.yml defines a release-version validation gate"
assert_contains "$RELEASE_WORKFLOW" '^\s*needs:\s*validate_release_version\s*$' "build job waits for the release-version validation gate"
assert_contains "$RELEASE_WORKFLOW" '^\s*docker_prepare:' "release.yml defines docker_prepare tag owner"
assert_contains "$RELEASE_WORKFLOW" '^\s*docker_build_amd64:' "release.yml defines amd64 build lane"
assert_contains "$RELEASE_WORKFLOW" '^\s*docker_build_arm64_native:' "release.yml defines arm64 native lane"
assert_contains "$RELEASE_WORKFLOW" '^\s*docker_build_arm64_qemu:' "release.yml defines arm64 qemu fallback lane"
assert_contains "$RELEASE_WORKFLOW" '^\s*docker_manifest_verify:' "release.yml defines manifest verification gate"
assert_contains "$RELEASE_WORKFLOW" '^\s*docker_promote_stable:' "release.yml defines stable promotion lane"
assert_contains "$RELEASE_WORKFLOW" "linux/amd64" "release.yml references linux/amd64"
assert_contains "$RELEASE_WORKFLOW" "linux/arm64" "release.yml references linux/arm64"
assert_contains "$RELEASE_WORKFLOW" "docker/setup-qemu-action@v3" "release.yml defines explicit qemu fallback path"
assert_contains "$RELEASE_WORKFLOW" "docker buildx imagetools inspect" "release.yml verifies candidate manifest contents"
assert_contains "$RELEASE_WORKFLOW" "ghcr\\.io/flapjackhq/flapjack" "release.yml uses canonical image owner"
assert_contains "$RELEASE_WORKFLOW" 'engine/flapjack-http/Cargo.toml' "release.yml verifies crate manifest versions before building"
assert_contains "$RELEASE_WORKFLOW" 'CHANGELOG\.md' "release.yml verifies changelog version before building"
assert_contains "$RELEASE_WORKFLOW" 'grep -Fxq "version = \\"\$VERSION\\""' "release.yml uses literal Cargo manifest matching for the requested version"
assert_contains "$RELEASE_WORKFLOW" 'grep -Fq "## \[\$\{VERSION\}\] - "' "release.yml uses literal changelog heading matching for the requested version"
assert_contains "$RELEASE_WORKFLOW" 'version must match MAJOR\.MINOR\.PATCH or MAJOR\.MINOR\.PATCH-prerelease' "release.yml rejects unsafe release-version syntax before tagging or publishing"
assert_contains "$RELEASE_WORKFLOW" "^\\s*if:\\s*\\$\\{\\{\\s*runner\\.os\\s*!=\\s*'Windows'\\s*\\}\\}" "unix packaging step uses valid runner.os expression syntax"
assert_contains "$RELEASE_WORKFLOW" "^\\s*if:\\s*\\$\\{\\{\\s*runner\\.os\\s*==\\s*'Windows'\\s*\\}\\}" "windows packaging step uses valid runner.os expression syntax"

section "Release build identity packaging"
assert_contains "$RELEASE_WORKFLOW" "github\\.sha.*\\^\\[0-9a-f\\]\\{40\\}\\$|\\^\\[0-9a-f\\]\\{40\\}\\$.*github\\.sha" "release.yml verifies github.sha is exactly 40 lowercase hex characters"
assert_contains "$RELEASE_WORKFLOW" "FLAPJACK_BUILD_REVISION: \\$\\{\\{ github\\.sha \\}\\}" "release.yml exports github.sha as FLAPJACK_BUILD_REVISION for release builds"
assert_contains "$RELEASE_WORKFLOW" "package/release_artifact_manifest \\$\\{\\{ matrix\\.target \\}\\} target/\\$\\{\\{ matrix\\.target \\}\\}/release/flapjack " "unix packaging calls the shared release_artifact_manifest helper"
assert_contains "$RELEASE_WORKFLOW" "package/release_artifact_manifest \\$\\{\\{ matrix\\.target \\}\\} target/\\$\\{\\{ matrix\\.target \\}\\}/release/flapjack\\.exe " "windows packaging calls the shared release_artifact_manifest helper"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.manifest\\.json" "release.yml uploads and publishes manifest JSON assets"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.tar\\.gz" "release.yml uploads and publishes Unix archives"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.tar\\.gz\\.sha256" "release.yml uploads and publishes Unix checksum sidecars"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.zip" "release.yml uploads and publishes Windows archives"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.zip\\.sha256" "release.yml uploads and publishes Windows checksum sidecars"
assert_file_executable "$RELEASE_MANIFEST_HELPER" "release_artifact_manifest helper is executable"
assert_release_helper_contract

section "Docker build hang protection and retry safety"
# The qemu arm64 fallback once hung the release pipeline indefinitely because it
# had no runtime cap. Require an explicit, generous-but-bounded timeout on it so
# a stalled emulated build fails fast instead of stalling the whole release.
assert_contains "$RELEASE_WORKFLOW" "^\\s*timeout-minutes: 90" "release.yml caps the qemu arm64 build runtime so a stalled emulated build cannot hang the pipeline"
assert_contains "$RELEASE_WORKFLOW" "^\\s*timeout-minutes: 45" "release.yml caps native docker build runtime"
# release.yml creates the git tag before Docker promotion, so a partial run
# leaves the tag published. Re-dispatching to finish the release must not abort
# at tag creation when the tag already exists.
assert_contains "$RELEASE_WORKFLOW" "git ls-remote --exit-code --tags origin" "release.yml tag creation is idempotent for safe retry after a partial release"
# One arm64 lane (native or qemu) is always skipped. GitHub transitively
# propagates that skip to docker_promote_stable unless it has an explicit guard,
# silently skipping stable-tag publication. Require the same always()+result
# guard docker_manifest_verify uses so promotion survives the skipped lane.
assert_contains "$RELEASE_WORKFLOW" "needs\\.docker_manifest_verify\\.result == 'success'" "release.yml promotes stable tags whenever manifest verification succeeded, surviving the skipped arm64 lane"

section "docker.yml ownership boundaries"
assert_not_contains "$DOCKER_WORKFLOW" '^\s*push:\s*$' "docker.yml no longer auto-publishes on push"
assert_not_contains "$DOCKER_WORKFLOW" '^\s*tags:\s*\["v\*"\]' "docker.yml no longer publishes release tags"
assert_not_contains "$DOCKER_WORKFLOW" "type=semver,pattern=\\{\\{version\\}\\}" "docker.yml no longer publishes semver stable tags"
assert_not_contains "$DOCKER_WORKFLOW" "type=raw,value=latest" "docker.yml no longer publishes latest stable tag"

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
