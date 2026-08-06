#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
RELEASE_WORKFLOW="$REPO_DIR/.github/workflows/release.yml"
DOCKER_WORKFLOW="$REPO_DIR/.github/workflows/docker.yml"
CI_WORKFLOW="$REPO_DIR/.github/workflows/ci.yml"
RELEASE_MANIFEST_HELPER="$REPO_DIR/engine/package/release_artifact_manifest"
CROSS_TOML="$REPO_DIR/engine/Cross.toml"
ROOT_CROSS_TOML="$REPO_DIR/Cross.toml"

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

assert_file_absent() {
  local file_path="$1"
  local description="$2"
  if [ ! -e "$file_path" ]; then
    pass "$description"
  else
    fail "$description"
  fi
}

# cross reads Cross.toml relative to the crate it builds, so the release build's
# container-passthrough owner must be engine/Cross.toml and must deliver exactly
# the external FLAPJACK_BUILD_REVISION the workflow exports. The build.rs-emitted
# FLAPJACK_INTERNAL_BUILD_REVISION is produced inside the build script, never
# consumed from the container environment, so passing it through would be a
# false owner. A guard that only checks the release.yml env spelling is
# false-green because the value never crosses the container boundary without
# this passthrough.
cross_passthrough_contains() {
  local variable_name="$1"
  python3 - "$CROSS_TOML" "$variable_name" <<'PY'
import sys
import tomllib

with open(sys.argv[1], "rb") as handle:
    config = tomllib.load(handle)
passthrough = config.get("build", {}).get("env", {}).get("passthrough", [])
sys.exit(0 if sys.argv[2] in passthrough else 1)
PY
}

assert_cross_build_revision_passthrough() {
  if [ ! -f "$CROSS_TOML" ]; then
    fail "engine/Cross.toml owns the cross container build-identity passthrough"
    return
  fi
  pass "engine/Cross.toml owns the cross container build-identity passthrough"

  assert_file_absent "$ROOT_CROSS_TOML" \
    "Cross.toml is not misplaced at the repo root where the release build never reads it"

  if cross_passthrough_contains "FLAPJACK_BUILD_REVISION"; then
    pass "engine/Cross.toml [build.env] passthrough delivers FLAPJACK_BUILD_REVISION into the container build"
  else
    fail "engine/Cross.toml [build.env] passthrough delivers FLAPJACK_BUILD_REVISION into the container build"
  fi

  if cross_passthrough_contains "FLAPJACK_INTERNAL_BUILD_REVISION"; then
    fail "engine/Cross.toml passthrough must not carry the build.rs-emitted internal revision name"
  else
    pass "engine/Cross.toml passthrough must not carry the build.rs-emitted internal revision name"
  fi
}

# Slices one job out of the workflow so an assertion cannot be satisfied by a
# match somewhere else in the file. `secrets.GHCR_TOKEN`, for example, appears
# in every Docker job, so a whole-file grep would pass even if the preflight
# job never referenced it.
job_block() {
  local job_name="$1"
  awk -v job="$job_name" '
    $0 ~ "^  " job ":" { in_block = 1; print; next }
    in_block && /^  [a-zA-Z_]+:/ { in_block = 0 }
    in_block { print }
  ' "$RELEASE_WORKFLOW"
}

assert_job_contains() {
  local job_name="$1"
  local pattern="$2"
  local description="$3"
  if job_block "$job_name" | grep -Eq "$pattern"; then
    pass "$description"
  else
    fail "$description"
  fi
}

# The image repository must be declared exactly once and never re-composed
# inline beside that declaration — otherwise the credential preflight and the
# publish jobs can drift onto different repositories, leaving a guard that
# proves push to somewhere the release does not publish.
#
# Both patterns match the repository identity by SHAPE rather than by name,
# because debbie rewrites that identity per mirror. This is run against the real
# workflow and against an identity-rewritten copy of it; see the call sites.
# The credential the preflight proves must be the credential the publish jobs
# actually use, and naming either one literally is what lets them drift: point
# the registry logins at a different secret and a literal assertion still
# passes, leaving the preflight proving a credential the release never uses — a
# green guard over an unproven credential.
#
# This is not hypothetical. Granting the release repository Actions access to
# the GHCR package lets the publish jobs move from the PAT to the built-in
# GITHUB_TOKEN, and that migration touches the login steps, not the preflight.
# Comparing the two names rather than asserting either lets that migration pass
# cleanly while still catching a half-done one.
assert_preflight_proves_the_publish_credential() {
  local publish_secrets preflight_secrets
  publish_secrets="$(grep -oE '^[[:space:]]*password: \$\{\{ secrets\.[A-Z_]+ \}\}' "$RELEASE_WORKFLOW" \
    | grep -oE 'secrets\.[A-Z_]+' | sort -u)"
  preflight_secrets="$(job_block "ghcr_publish_preflight" | grep -oE 'secrets\.[A-Z_]+' | sort -u)"

  if [ -z "$publish_secrets" ]; then
    fail "preflight proves the credential the registry logins use (no registry login credential found)"
    return
  fi
  # More than one distinct name means the publish jobs disagree with each other,
  # so no single preflight can prove all of them.
  if [ "$(printf '%s\n' "$publish_secrets" | wc -l | tr -d ' ')" != "1" ]; then
    fail "preflight proves the credential the registry logins use (logins disagree: $(printf '%s ' $publish_secrets))"
    return
  fi
  if [ "$publish_secrets" = "$preflight_secrets" ]; then
    pass "preflight proves the credential the registry logins use ($publish_secrets)"
  else
    fail "preflight proves the credential the registry logins use (logins=$publish_secrets preflight=${preflight_secrets:-none})"
  fi
}

assert_image_identity_ssot() {
  local workflow_path="$1"
  local context="$2"
  assert_contains "$workflow_path" "^\\s*RELEASE_IMAGE_REPOSITORY: [A-Za-z0-9._-]+/flapjack$" \
    "release.yml declares one owner for the canonical image repository ($context)"
  assert_not_contains "$workflow_path" 'ghcr\.io/[A-Za-z0-9._-]+/flapjack' \
    "release.yml never re-hardcodes the composed image reference ($context)"
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
: <<'FLAPJACK_BUILD_INFO_EMBED'
FLAPJACK_BUILD_INFO_JSON_BEGIN
{"schemaVersion":1,"version":"1.2.3","revision":"0123456789abcdef0123456789abcdef01234567","revisionKnown":true,"dirty":false,"dirtyKnown":true,"workspaceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile":"release","target":"x86_64-unknown-linux-gnu","features":["vector-search"],"capabilities":{"vectorSearch":true,"vectorSearchLocal":false}}
FLAPJACK_BUILD_INFO_JSON_END
FLAPJACK_BUILD_INFO_EMBED
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
assert_contains "$RELEASE_WORKFLOW" "^\\s*RELEASE_REGISTRY: ghcr\\.io$" "release.yml declares one owner for the release registry host"
assert_image_identity_ssot "$RELEASE_WORKFLOW" "this checkout"
assert_job_contains "docker_prepare" 'image="\$\{RELEASE_REGISTRY\}/\$\{RELEASE_IMAGE_REPOSITORY\}"' "docker_prepare composes its tags from the declared registry coordinates"
# Re-run the identity-sensitive assertions against a copy whose repository
# identity has been rewritten, which is exactly what debbie does when it syncs
# to each mirror. Without this the suite passes locally and on the production
# mirror but is red on staging — where a red is a release hard stop — because
# the dev checkout carries the same identity production does, so no local run
# can tell a pinned identity from a portable one.
IDENTITY_REWRITTEN_WORKFLOW="$(mktemp "${TMPDIR:-/tmp}/flapjack-release-mirror.XXXXXX")"
trap 'rm -f "$IDENTITY_REWRITTEN_WORKFLOW"' EXIT
sed -E 's#[A-Za-z0-9._-]+/flapjack#some-other-mirror/flapjack#g' \
  "$RELEASE_WORKFLOW" >"$IDENTITY_REWRITTEN_WORKFLOW"
assert_image_identity_ssot "$IDENTITY_REWRITTEN_WORKFLOW" "a mirror with a different identity"

assert_contains "$RELEASE_WORKFLOW" 'engine/flapjack-http/Cargo.toml' "release.yml verifies crate manifest versions before building"
assert_contains "$RELEASE_WORKFLOW" 'CHANGELOG\.md' "release.yml verifies changelog version before building"
assert_contains "$RELEASE_WORKFLOW" 'grep -Fxq "version = \\"\$VERSION\\""' "release.yml uses literal Cargo manifest matching for the requested version"
assert_contains "$RELEASE_WORKFLOW" 'grep -Fq "## \[\$\{VERSION\}\] - "' "release.yml uses literal changelog heading matching for the requested version"
assert_contains "$RELEASE_WORKFLOW" 'version must match MAJOR\.MINOR\.PATCH or MAJOR\.MINOR\.PATCH-prerelease' "release.yml rejects unsafe release-version syntax before tagging or publishing"
assert_contains "$RELEASE_WORKFLOW" "^\\s*if:\\s*\\$\\{\\{\\s*runner\\.os\\s*!=\\s*'Windows'\\s*\\}\\}" "unix packaging step uses valid runner.os expression syntax"
assert_contains "$RELEASE_WORKFLOW" "^\\s*if:\\s*\\$\\{\\{\\s*runner\\.os\\s*==\\s*'Windows'\\s*\\}\\}" "windows packaging step uses valid runner.os expression syntax"

section "GHCR publish credential preflight"
# release.yml cuts the git tag and publishes the GitHub Release in `release`,
# and only reaches the first job that uses secrets.GHCR_TOKEN two jobs later.
# An expired or unscoped credential was therefore discoverable only after the
# release was already public — the v1.0.9 half-release shape: binaries live,
# container images missing, tag irreversible. These assertions keep the
# credential proof ahead of the first irreversible act.
assert_contains "$RELEASE_WORKFLOW" '^\s*ghcr_publish_preflight:' "release.yml defines a GHCR publish-credential preflight"
assert_job_contains "ghcr_publish_preflight" '^\s*needs:\s*validate_release_version\s*$' "preflight is gated only on version validation, so it runs beside the build matrix"
assert_preflight_proves_the_publish_credential
assert_job_contains "ghcr_publish_preflight" 'package/ghcr_publish_preflight' "preflight calls the shared helper instead of inlining probe logic in YAML"
assert_job_contains "ghcr_publish_preflight" 'RELEASE_IMAGE_REPOSITORY' "preflight probes the same image repository the Docker jobs publish to"
assert_job_contains "ghcr_publish_preflight" '^\s*timeout-minutes:' "preflight is time-bounded so a hung registry cannot stall the release"
# The load-bearing assertion: without this the preflight is decorative, because
# `release` would still create the public tag while the credential is unproven.
assert_job_contains "release" '^\s*needs:\s*\[build, ghcr_publish_preflight\]\s*$' "the public tag and GitHub Release wait on proven push capability"
assert_file_executable "$REPO_DIR/engine/package/ghcr_publish_preflight" "ghcr_publish_preflight helper is executable"

section "Release build identity packaging"
assert_contains "$RELEASE_WORKFLOW" "github\\.sha.*\\^\\[0-9a-f\\]\\{40\\}\\$|\\^\\[0-9a-f\\]\\{40\\}\\$.*github\\.sha" "release.yml verifies github.sha is exactly 40 lowercase hex characters"
assert_contains "$RELEASE_WORKFLOW" "FLAPJACK_BUILD_REVISION: \\$\\{\\{ github\\.sha \\}\\}" "release.yml exports github.sha as FLAPJACK_BUILD_REVISION for release builds"
assert_cross_build_revision_passthrough
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

section "Release contracts actually run"
# This file asserted release.yml's shape for months while no workflow invoked
# it, so every assertion in it was inert. A contract test that nothing runs is
# not a guard. These two assertions make that failure mode self-detecting:
# unwire the suite from CI and this suite goes red.
# Anchored to an actual `run:` line. A bare path match would also be satisfied
# by the invocation sitting commented out, which is the exact way a suite gets
# quietly disabled.
assert_contains "$CI_WORKFLOW" '^\s*run: bash engine/tests/test_release_workflow_structure\.sh\s*$' "ci.yml runs the release workflow structure contract"
assert_contains "$CI_WORKFLOW" '^\s*run: bash engine/tests/test_ghcr_publish_preflight\.sh\s*$' "ci.yml runs the GHCR publish preflight contract"
assert_contains "$CI_WORKFLOW" '^\s*run: bash engine/tests/build_identity_cross_kat_supervision_test\.sh\s*$' "ci.yml runs the cross passthrough KAT supervision contract"

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
