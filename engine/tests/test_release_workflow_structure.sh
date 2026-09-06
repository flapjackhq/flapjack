#!/usr/bin/env bash

set -euo pipefail

SECONDS=0

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
RELEASE_WORKFLOW="${RELEASE_WORKFLOW_UNDER_TEST:-$REPO_DIR/.github/workflows/release.yml}"
DOCKER_WORKFLOW="$REPO_DIR/.github/workflows/docker.yml"
CI_WORKFLOW="$REPO_DIR/.github/workflows/ci.yml"
RELEASE_MANIFEST_HELPER="$REPO_DIR/engine/package/release_artifact_manifest"
RELEASE_RUNTIME_GATE="$REPO_DIR/engine/package/release_artifact_runtime_gate"
RELEASE_BUILD_HELPER="$REPO_DIR/engine/package/build_release_artifact"
RELEASE_BUILD_HELPER_CONTRACT="$REPO_DIR/engine/tests/build_release_artifact_contract.sh"
BUILD_IDENTITY_PACKAGE_CONTRACT="$REPO_DIR/engine/tests/build_identity_package_contract.sh"
ENGINE_COMPATIBILITY="$REPO_DIR/engine/package/engine_compatibility.json"
HTTP_MANIFEST="$REPO_DIR/engine/flapjack-http/Cargo.toml"
DOCKERFILE="$REPO_DIR/engine/Dockerfile"
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

assert_exact_count() {
  local file_path="$1"
  local pattern="$2"
  local expected_count="$3"
  local description="$4"
  local actual_count
  actual_count="$(grep -Ec "$pattern" "$file_path" || true)"
  if [ "$actual_count" -eq "$expected_count" ]; then
    pass "$description"
  else
    fail "$description (expected $expected_count, found $actual_count)"
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

workflow_permissions_block() {
  awk '
    /^permissions:/ { in_block = 1; print; next }
    in_block && /^[^[:space:]]/ { in_block = 0 }
    in_block { print }
  ' "$RELEASE_WORKFLOW"
}

# cross reads Cross.toml relative to the crate it builds, so the release build's
# container-passthrough owner must be engine/Cross.toml and must deliver exactly
# the external FLAPJACK_BUILD_REVISION the workflow exports. The build.rs-emitted
# FLAPJACK_INTERNAL_BUILD_REVISION is produced inside the build script, never
# consumed from the container environment, so passing it through would be a
# false owner. A guard that only checks the release.yml env spelling is
# false-green because the value never crosses the container boundary without
# this passthrough.
# Exit codes are three-valued on purpose. The first version of this probe returned
# only 0/1, so a `ModuleNotFoundError` was indistinguishable from "the passthrough is
# missing" — and that is not hypothetical: `tomllib` is Python 3.11+, this repo's macOS
# host runs 3.9, and the probe therefore reported `engine/Cross.toml [build.env]
# passthrough delivers FLAPJACK_BUILD_REVISION` as FAILED on a tree where Cross.toml was
# correct and CI was green. A probe that cannot run must say so; reporting a verdict it
# did not compute sends the next reader to repair a file that has nothing wrong with it.
#   0 = present   1 = absent   2 = could not be determined
cross_passthrough_contains() {
  local variable_name="$1"
  python3 - "$CROSS_TOML" "$variable_name" <<'PY'
import sys

# tomllib is stdlib from 3.11; tomli is the identical API it was adopted from, and is
# what makes this probe runnable on older interpreters instead of merely honest about
# being unable to run.
try:
    import tomllib
except ModuleNotFoundError:
    try:
        import tomli as tomllib
    except ModuleNotFoundError:
        print(
            f"INDETERMINATE: reading {sys.argv[1]} needs tomllib (Python 3.11+) or tomli; "
            f"this interpreter is {sys.version.split()[0]} and has neither.",
            file=sys.stderr,
        )
        sys.exit(2)

try:
    with open(sys.argv[1], "rb") as handle:
        config = tomllib.load(handle)
except (OSError, tomllib.TOMLDecodeError) as error:
    print(f"INDETERMINATE: cannot parse {sys.argv[1]}: {error}", file=sys.stderr)
    sys.exit(2)

passthrough = config.get("build", {}).get("env", {}).get("passthrough", [])
sys.exit(0 if sys.argv[2] in passthrough else 1)
PY
}

assert_cross_passthrough_variable() {
  local variable_name="$1"
  local description="$2"
  local passthrough_status=0
  cross_passthrough_contains "$variable_name" || passthrough_status=$?
  if [ "$passthrough_status" -eq 0 ]; then
    pass "$description"
  elif [ "$passthrough_status" -eq 2 ]; then
    fail "$description (Cross.toml could not be read; see INDETERMINATE above)"
  else
    fail "$description"
  fi
}

assert_cross_build_revision_passthrough() {
  if [ ! -f "$CROSS_TOML" ]; then
    fail "engine/Cross.toml owns the cross container build-identity passthrough"
    return
  fi
  pass "engine/Cross.toml owns the cross container build-identity passthrough"

  assert_file_absent "$ROOT_CROSS_TOML" \
    "Cross.toml is not misplaced at the repo root where the release build never reads it"

  assert_cross_passthrough_variable \
    "FLAPJACK_BUILD_REVISION" \
    "engine/Cross.toml [build.env] passthrough delivers FLAPJACK_BUILD_REVISION into the container build"

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
#
# GNU awk/mawk can exit 141 on EPIPE when an early reader closes this function's
# pipe, and pipefail then turns a present match into failure. BSD awk completed
# the small observed macOS write without that failure, which hid the portability
# defect. Every consumer must therefore capture the complete block successfully
# before matching it; reintroducing `job_block ... | grep -q` is unsafe.
job_block() {
  local job_name="$1"
  awk -v job="$job_name" '
    $0 ~ "^  " job ":" { in_block = 1; print; next }
    in_block && /^  [a-zA-Z_]+:/ { in_block = 0 }
    in_block { print }
  ' "$RELEASE_WORKFLOW"
}

assert_job_pattern_presence() {
  local job_name="$1"
  local pattern="$2"
  local description="$3"
  local expected_presence="$4"
  local block
  if ! block="$(job_block "$job_name")"; then
    fail "$description (could not extract job block)"
    return
  fi

  local pattern_is_present=0
  if grep -Eq "$pattern" <<<"$block"; then
    pattern_is_present=1
  fi
  if [ "$pattern_is_present" -eq "$expected_presence" ]; then
    pass "$description"
  else
    fail "$description"
  fi
}

assert_job_contains() {
  assert_job_pattern_presence "$1" "$2" "$3" 1
}

assert_job_not_contains() {
  assert_job_pattern_presence "$1" "$2" "$3" 0
}

assert_job_needs() {
  local job_name="$1"
  local dependency="$2"
  local description="$3"
  local block needs normalized_needs
  if ! block="$(job_block "$job_name")"; then
    fail "$description (could not extract job block)"
    return
  fi
  needs="$(sed -nE 's/^[[:space:]]*needs:[[:space:]]*\[([^]]*)\][[:space:]]*$/\1/p' <<<"$block")"
  normalized_needs="$(tr ',' '\n' <<<"$needs" | sed -E 's/^[[:space:]]+|[[:space:]]+$//g')"
  if grep -Fxq "$dependency" <<<"$normalized_needs"; then
    pass "$description"
  else
    fail "$description"
  fi
}

assert_job_order() {
  local job_name="$1"
  local earlier_pattern="$2"
  local later_pattern="$3"
  local description="$4"
  local block earlier_match later_match earlier_line="" later_line=""
  if ! block="$(job_block "$job_name")"; then
    fail "$description (could not extract job block)"
    return
  fi
  if earlier_match="$(grep -Enm 1 "$earlier_pattern" <<<"$block")"; then
    earlier_line="${earlier_match%%:*}"
  fi
  if later_match="$(grep -Enm 1 "$later_pattern" <<<"$block")"; then
    later_line="${later_match%%:*}"
  fi
  if [ -n "$earlier_line" ] && [ -n "$later_line" ] && [ "$earlier_line" -lt "$later_line" ]; then
    pass "$description"
  else
    fail "$description"
  fi
}

write_job_block_contract_fixture() {
  local fixture_path="$1"
  awk 'BEGIN {
    print "name: job block contract"
    print "jobs:"
    print "  contract_job:"
    print "    needs: [ build, ghcr_publish_preflight , engine_compatibility_gatekeeper ]"
    print "    steps:"
    print "      - run: echo EARLY_MATCH"
    print "      - run: echo REPEATED_MATCH"
    for (line = 0; line < 30000; line++) {
      printf "      - run: echo filler-%05d-abcdefghijklmnopqrstuvwxyz0123456789\n", line
    }
    print "      - run: echo LATER_MATCH"
    print "      - run: echo REPEATED_MATCH"
    print "  following_job:"
    print "    steps:"
    print "      - run: echo FOLLOWING_JOB_MARKER"
  }' >"$fixture_path"
}

observe_job_assertion() {
  local expected_verdict="$1"
  local expected_output="$2"
  local case_description="$3"
  shift 3

  local tests_before="$TESTS_RUN"
  local passed_before="$TESTS_PASSED"
  local failed_before="$TESTS_FAILED"
  local assertion_output
  "$@" >"$JOB_BLOCK_ASSERTION_OUTPUT"
  assertion_output="$(<"$JOB_BLOCK_ASSERTION_OUTPUT")"

  local observed_verdict="invalid"
  if [ "$TESTS_RUN" -eq $((tests_before + 1)) ] \
    && [ "$TESTS_PASSED" -eq $((passed_before + 1)) ] \
    && [ "$TESTS_FAILED" -eq "$failed_before" ]; then
    observed_verdict="pass"
  elif [ "$TESTS_RUN" -eq $((tests_before + 1)) ] \
    && [ "$TESTS_PASSED" -eq "$passed_before" ] \
    && [ "$TESTS_FAILED" -eq $((failed_before + 1)) ]; then
    observed_verdict="fail"
  fi

  TESTS_RUN="$tests_before"
  TESTS_PASSED="$passed_before"
  TESTS_FAILED="$failed_before"
  if [ "$observed_verdict" = "$expected_verdict" ] && [ "$assertion_output" = "$expected_output" ]; then
    pass "$case_description"
  else
    fail "$case_description (expected $expected_verdict '$expected_output'; observed $observed_verdict '$assertion_output')"
  fi
}

exercise_job_block_assertion_cases() {
  local fixture_path="$1"
  RELEASE_WORKFLOW="$fixture_path"

  observe_job_assertion pass "  [PASS] early marker is present" \
    "positive presence survives a large job block" \
    assert_job_contains "contract_job" "EARLY_MATCH" "early marker is present"
  observe_job_assertion fail "  [FAIL] missing marker is present" \
    "positive absence is reported as failure" \
    assert_job_contains "contract_job" "MISSING_MARKER" "missing marker is present"
  observe_job_assertion fail "  [FAIL] early marker is absent" \
    "negative presence is reported as failure" \
    assert_job_not_contains "contract_job" "EARLY_MATCH" "early marker is absent"
  observe_job_assertion pass "  [PASS] missing marker is absent" \
    "negative absence is reported as success" \
    assert_job_not_contains "contract_job" "MISSING_MARKER" "missing marker is absent"
  observe_job_assertion pass "  [PASS] following job stays isolated" \
    "a following job cannot satisfy the selected block" \
    assert_job_not_contains "contract_job" "FOLLOWING_JOB_MARKER" "following job stays isolated"
  observe_job_assertion pass "  [PASS] selected following job is readable" \
    "the following job remains independently selectable" \
    assert_job_contains "following_job" "FOLLOWING_JOB_MARKER" "selected following job is readable"

  observe_job_assertion pass "  [PASS] early marker precedes later marker" \
    "correct ordering uses the first matching lines" \
    assert_job_order "contract_job" "EARLY_MATCH" "LATER_MATCH" "early marker precedes later marker"
  observe_job_assertion fail "  [FAIL] later marker precedes early marker" \
    "reversed ordering is rejected" \
    assert_job_order "contract_job" "LATER_MATCH" "EARLY_MATCH" "later marker precedes early marker"
  observe_job_assertion fail "  [FAIL] missing marker has an order" \
    "ordering rejects a missing match" \
    assert_job_order "contract_job" "MISSING_MARKER" "LATER_MATCH" "missing marker has an order"
  observe_job_assertion pass "  [PASS] first repeated marker precedes later marker" \
    "ordering preserves first-match semantics for repeated matches" \
    assert_job_order "contract_job" "REPEATED_MATCH" "LATER_MATCH" "first repeated marker precedes later marker"

  observe_job_assertion pass "  [PASS] trimmed dependency is present" \
    "dependency membership trims surrounding whitespace" \
    assert_job_needs "contract_job" "ghcr_publish_preflight" "trimmed dependency is present"
  observe_job_assertion pass "  [PASS] first dependency is present" \
    "dependency membership accepts an exact first member" \
    assert_job_needs "contract_job" "build" "first dependency is present"
  observe_job_assertion fail "  [FAIL] near-match dependency is present" \
    "dependency membership rejects a near match" \
    assert_job_needs "contract_job" "engine_compatibility_gate" "near-match dependency is present"

  RELEASE_WORKFLOW="${fixture_path}.missing"
  observe_job_assertion fail "  [FAIL] extraction failure is not absence (could not extract job block)" \
    "negative assertions preserve extraction errors" \
    assert_job_not_contains "contract_job" "MISSING_MARKER" "extraction failure is not absence"
  RELEASE_WORKFLOW="$fixture_path"
}

assert_legacy_job_block_consumer_is_killed() {
  local fixture_path="$1"
  local mutant_output mutant_status=0
  mutant_output="$({
    TESTS_RUN=0
    TESTS_PASSED=0
    TESTS_FAILED=0
    RELEASE_WORKFLOW="$fixture_path"
    RELEASE_STRUCTURE_SKIP_MUTANTS=1
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
    observe_job_assertion pass "  [PASS] early marker is present" \
      "legacy early-reader consumer loses the early match" \
      assert_job_contains "contract_job" "EARLY_MATCH" "early marker is present"
    [ "$TESTS_FAILED" -eq 0 ]
  } 2>&1)" || mutant_status=$?

  if [ "$mutant_status" -ne 0 ] \
    && grep -Fq "[FAIL] legacy early-reader consumer loses the early match" <<<"$mutant_output"; then
    pass "large-block case kills the legacy producer-to-grep consumer for the intended early-match failure"
  else
    fail "large-block case kills the legacy producer-to-grep consumer for the intended early-match failure"
  fi
}

assert_broken_focused_contract_exits_nonzero() {
  local broken_output broken_status=0
  broken_output="$(RELEASE_STRUCTURE_SKIP_MUTANTS=1 \
    RELEASE_STRUCTURE_FORCE_JOB_BLOCK_CONTRACT_FAILURE=1 \
    bash "$0" --job-block-contract-only 2>&1)" || broken_status=$?

  if [ "$broken_status" -ne 0 ] \
    && grep -Fq "[FAIL] intentional broken job-block contract case" <<<"$broken_output"; then
    pass "focused job-block contract exits nonzero for an intentional contract failure"
  else
    fail "focused job-block contract exits nonzero for an intentional contract failure"
  fi
}

job_block_contract_verdict() (
  local contract_tmp_dir fixture_path
  contract_tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-job-block-contract.XXXXXX")"
  trap 'rm -rf "$contract_tmp_dir"' EXIT
  fixture_path="$contract_tmp_dir/release.yml"
  JOB_BLOCK_ASSERTION_OUTPUT="$contract_tmp_dir/assertion-output"
  TESTS_RUN=0
  TESTS_PASSED=0
  TESTS_FAILED=0

  write_job_block_contract_fixture "$fixture_path"
  exercise_job_block_assertion_cases "$fixture_path"
  if [ "${RELEASE_STRUCTURE_SKIP_MUTANTS:-0}" != "1" ]; then
    assert_legacy_job_block_consumer_is_killed "$fixture_path"
    assert_broken_focused_contract_exits_nonzero
  fi
  if [ "${RELEASE_STRUCTURE_FORCE_JOB_BLOCK_CONTRACT_FAILURE:-0}" = "1" ]; then
    fail "intentional broken job-block contract case"
  fi

  [ "$TESTS_FAILED" -eq 0 ]
)

assert_job_block_contract() {
  local contract_output contract_status=0
  contract_output="$(job_block_contract_verdict 2>&1)" || contract_status=$?
  if [ "$contract_status" -eq 0 ]; then
    pass "job-block assertions consume complete blocks and reject the legacy early-reader path"
  else
    printf '%s\n' "$contract_output" >&2
    fail "job-block assertions consume complete blocks and reject the legacy early-reader path"
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
  local preflight_block publish_secrets preflight_secrets
  publish_secrets="$(grep -oE '^[[:space:]]*password: \$\{\{ secrets\.[A-Z_]+ \}\}' "$RELEASE_WORKFLOW" \
    | grep -oE 'secrets\.[A-Z_]+' | sort -u)"
  if ! preflight_block="$(job_block "ghcr_publish_preflight")"; then
    fail "preflight proves the credential the registry logins use (could not extract job block)"
    return
  fi
  preflight_secrets="$(grep -oE 'secrets\.[A-Z_]+' <<<"$preflight_block" | sort -u)"

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
  local tmp_dir bin_path output_dir manifest_path fake_bin predecessors mutated_manifest
  tmp_dir="$(mktemp -d)"
  bin_path="$tmp_dir/flapjack"
  output_dir="$tmp_dir/out"
  fake_bin="$tmp_dir/fake-bin"
  mkdir -p "$output_dir" "$fake_bin"

  # macOS bsdtar can materialize AppleDouble entries for extended attributes.
  # This fake tar makes that failure portable: a release owner that delegates
  # archive creation to ambient tar will emit the forbidden extra member, while
  # the canonical xattr-free packager is unaffected.
  cat >"$fake_bin/tar" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ne 5 ] || [ "$1" != "czf" ] || [ "$3" != "-C" ] || [ "$5" != "." ]; then
  printf 'unexpected tar invocation\n' >&2
  exit 64
fi
python3 - "$2" "$4" <<'PY'
import io
import pathlib
import tarfile
import sys

archive_path = pathlib.Path(sys.argv[1])
staging_dir = pathlib.Path(sys.argv[2])
with tarfile.open(archive_path, "w:gz") as archive:
    archive.add(staging_dir, arcname=".")
    payload = b"forbidden AppleDouble metadata\n"
    member = tarfile.TarInfo("./._flapjack")
    member.size = len(payload)
    archive.addfile(member, io.BytesIO(payload))
PY
EOF
  chmod +x "$fake_bin/tar"

  cat >"$bin_path" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ne 2 ] || [ "$1" != "build-info" ] || [ "$2" != "--json" ]; then
  echo "unexpected invocation: $*" >&2
  exit 64
fi
printf '%s\n' '{"schemaVersion":1,"version":"1.2.3","revision":"0123456789abcdef0123456789abcdef01234567","revisionKnown":true,"dirty":false,"dirtyKnown":true,"workspaceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile":"release","target":"x86_64-unknown-linux-musl","features":["vector-search"],"capabilities":{"vectorSearch":true,"vectorSearchLocal":false}}'
: <<'FLAPJACK_BUILD_INFO_EMBED'
FLAPJACK_BUILD_INFO_JSON_BEGIN
{"schemaVersion":1,"version":"1.2.3","revision":"0123456789abcdef0123456789abcdef01234567","revisionKnown":true,"dirty":false,"dirtyKnown":true,"workspaceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","profile":"release","target":"x86_64-unknown-linux-musl","features":["vector-search"],"capabilities":{"vectorSearch":true,"vectorSearchLocal":false}}
FLAPJACK_BUILD_INFO_JSON_END
FLAPJACK_BUILD_INFO_EMBED
EOF
  chmod +x "$bin_path"

  if PATH="$fake_bin:$PATH" "$RELEASE_MANIFEST_HELPER" "x86_64-unknown-linux-musl" "$bin_path" "$output_dir" >/dev/null 2>&1; then
    manifest_path="$output_dir/flapjack-x86_64-unknown-linux-musl.manifest.json"
    if python3 - "$manifest_path" "$output_dir/flapjack-x86_64-unknown-linux-musl.tar.gz" "$bin_path" <<'PY'
import hashlib
import json
import pathlib
import sys
import tarfile

manifest_path = pathlib.Path(sys.argv[1])
archive_path = pathlib.Path(sys.argv[2])
binary_path = pathlib.Path(sys.argv[3])
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
    "target": "x86_64-unknown-linux-musl",
    "features": ["vector-search"],
    "capabilities": {"vectorSearch": True, "vectorSearchLocal": False},
}
expected_compatibility = {
    "schemaVersion": 1,
    "target": "x86_64-unknown-linux-musl",
    "predecessors": [],
    "dataDisposition": "preserve",
    "mixedVersionReplication": "not_guaranteed",
}

expected_artifact = {
    "file": archive_path.name,
    "target": "x86_64-unknown-linux-musl",
    "arch": "x86_64",
    "profile": "release",
    "binarySha256": hashlib.sha256(binary_path.read_bytes()).hexdigest(),
    "sha256": hashlib.sha256(archive_path.read_bytes()).hexdigest(),
}
with tarfile.open(archive_path, "r:gz") as archive:
    members = [member.name for member in archive.getmembers()]
if members != [".", "./flapjack"]:
    raise SystemExit(f"archive member contract mismatch: {members}")
if manifest.get("schemaVersion") != 2:
    raise SystemExit("manifest schemaVersion must be 2")
if manifest.get("artifact") != expected_artifact:
    raise SystemExit(f"artifact contract mismatch: {manifest.get('artifact')}")
if manifest.get("build") != expected_build:
    raise SystemExit(f"build object must be copied verbatim: {manifest.get('build')}")
if manifest.get("compatibility") != expected_compatibility:
    raise SystemExit(
        f"compatibility object must be copied from the canonical contract: {manifest.get('compatibility')}"
    )
serialized = json.dumps(manifest, sort_keys=True, separators=(",", ":"))
for forbidden in ("algolia_migration_v1", "algoliaMigrationV1"):
    if forbidden in serialized:
        raise SystemExit(f"forbidden migration capability spelling present: {forbidden}")
PY
    then
      pass "release_artifact_manifest writes artifact, build, and canonical compatibility objects"
    else
      fail "release_artifact_manifest writes artifact, build, and canonical compatibility objects"
    fi

    if predecessors="$($RELEASE_MANIFEST_HELPER --compatibility-predecessors "$manifest_path" 2>/dev/null)" \
      && [ -z "$predecessors" ]; then
      pass "release_artifact_manifest validates the candidate manifest and reports no unclaimed predecessors"
    else
      fail "release_artifact_manifest validates the candidate manifest and reports no unclaimed predecessors"
    fi

    mutated_manifest="$tmp_dir/divergent.manifest.json"
    python3 - "$manifest_path" "$mutated_manifest" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text())
source.setdefault("compatibility", {})["dataDisposition"] = "replace"
pathlib.Path(sys.argv[2]).write_text(json.dumps(source, sort_keys=True, separators=(",", ":")) + "\n")
PY
    if "$RELEASE_MANIFEST_HELPER" --compatibility-predecessors "$mutated_manifest" >/dev/null 2>&1; then
      fail "release_artifact_manifest rejects compatibility that diverges from the source contract"
    else
      pass "release_artifact_manifest rejects compatibility that diverges from the source contract"
    fi
  else
    fail "release_artifact_manifest accepts target, binary path, and output directory CLI"
  fi

  rm -rf "$tmp_dir"
}

assert_release_runtime_gate_contract() {
  local tmp_dir fixtures_dir output_dir case_name weakened_gate
  tmp_dir="$(mktemp -d)"
  fixtures_dir="$tmp_dir/fixtures"
  mkdir -p "$fixtures_dir"

  if [ ! -x "$RELEASE_RUNTIME_GATE" ]; then
    fail "release_artifact_runtime_gate is an executable shared archive and identity owner"
    rm -rf "$tmp_dir"
    return
  fi
  pass "release_artifact_runtime_gate is an executable shared archive and identity owner"

  python3 - "$fixtures_dir" <<'PY'
import io
import json
import pathlib
import tarfile
import sys

root = pathlib.Path(sys.argv[1])
build = {
    "schemaVersion": 1,
    "version": "1.2.3",
    "revision": "0123456789abcdef0123456789abcdef01234567",
    "revisionKnown": True,
    "dirty": False,
    "dirtyKnown": True,
    "workspaceDigest": "a" * 64,
    "profile": "release",
    "target": "x86_64-unknown-linux-musl",
    "features": ["vector-search"],
    "capabilities": {"vectorSearch": True, "vectorSearchLocal": False},
}
(root / "manifest.json").write_text(
    json.dumps({"schemaVersion": 2, "artifact": {}, "build": build, "compatibility": {}}, sort_keys=True, separators=(",", ":")) + "\n"
)
rust_order = json.dumps(build, separators=(",", ":"))
outputs = (
    ("good-bin", rust_order),
    ("wrong-bin", rust_order.replace('"version":"1.2.3"', '"version":"9.9.9"')),
    ("duplicate-bin", rust_order.replace('"version":"1.2.3",', '"version":"1.2.3","version":"1.2.3",')),
)
for name, output in outputs:
    path = root / name
    path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n[ \"$#\" -eq 2 ] && [ \"$1\" = build-info ] && [ \"$2\" = --json ]\nprintf '%s\\n' '" + output + "'\n")
    path.chmod(0o755)

def member(name, member_type=tarfile.REGTYPE, linkname="", payload=b"candidate-binary"):
    item = tarfile.TarInfo(name)
    item.type = member_type
    item.linkname = linkname
    item.mode = 0o755
    if member_type == tarfile.REGTYPE:
        item.size = len(payload)
    return item, payload

def archive(name, members):
    with tarfile.open(root / f"{name}.tar.gz", "w:gz") as tar:
        directory = tarfile.TarInfo(".")
        directory.type = tarfile.DIRTYPE
        directory.mode = 0o755
        tar.addfile(directory)
        for item, payload in members:
            tar.addfile(item, io.BytesIO(payload) if item.type == tarfile.REGTYPE else None)

archive("good", [member("./flapjack")])
archive("extra", [member("./flapjack"), member("./extra", payload=b"extra")])
archive("absolute", [member("/flapjack")])
archive("traversal", [member("../flapjack")])
archive("symlink", [member("./flapjack", tarfile.SYMTYPE, "./target", b"")])
archive("hardlink", [member("./flapjack", tarfile.LNKTYPE, "./target", b"")])
archive("device", [member("./flapjack", tarfile.CHRTYPE, "", b"")])
PY

  output_dir="$tmp_dir/good-output"
  mkdir -p "$output_dir"
  if "$RELEASE_RUNTIME_GATE" extract "$fixtures_dir/good.tar.gz" "$output_dir" \
    && [ -x "$output_dir/flapjack" ] \
    && [ "$(cat "$output_dir/flapjack")" = "candidate-binary" ]; then
    pass "runtime gate safely materializes the exact release archive contract"
  else
    fail "runtime gate safely materializes the exact release archive contract"
  fi

  for case_name in extra absolute traversal symlink hardlink device; do
    output_dir="$tmp_dir/${case_name}-output"
    mkdir -p "$output_dir"
    if "$RELEASE_RUNTIME_GATE" extract "$fixtures_dir/${case_name}.tar.gz" "$output_dir" >/dev/null 2>&1 \
      || find "$output_dir" -mindepth 1 -print -quit | grep -q .; then
      fail "runtime gate rejects ${case_name} archive members before materialization"
    else
      pass "runtime gate rejects ${case_name} archive members before materialization"
    fi
  done

  if "$RELEASE_RUNTIME_GATE" attest "$fixtures_dir/good-bin" "$fixtures_dir/manifest.json"; then
    pass "runtime gate accepts real Rust-order build-info JSON by semantic object equality"
  else
    fail "runtime gate accepts real Rust-order build-info JSON by semantic object equality"
  fi
  if "$RELEASE_RUNTIME_GATE" attest "$fixtures_dir/wrong-bin" "$fixtures_dir/manifest.json" >/dev/null 2>&1; then
    fail "runtime gate rejects a candidate binary whose build-info differs from its manifest"
  else
    pass "runtime gate rejects a candidate binary whose build-info differs from its manifest"
  fi
  if "$RELEASE_RUNTIME_GATE" attest "$fixtures_dir/duplicate-bin" "$fixtures_dir/manifest.json" >/dev/null 2>&1; then
    fail "runtime gate rejects duplicate-key build-info even when the duplicate value is unchanged"
  else
    pass "runtime gate rejects duplicate-key build-info even when the duplicate value is unchanged"
  fi
  weakened_gate="$tmp_dir/weakened-runtime-gate"
  python3 - "$RELEASE_RUNTIME_GATE" "$weakened_gate" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1]).read_text()
needle = "            object_pairs_hook=reject_duplicate_keys,\n"
if source.count(needle) != 1:
    raise SystemExit("strict decoder mutation point must occur exactly once")
pathlib.Path(sys.argv[2]).write_text(source.replace(needle, ""))
PY
  chmod +x "$weakened_gate"
  if "$weakened_gate" attest "$fixtures_dir/duplicate-bin" "$fixtures_dir/manifest.json" >/dev/null 2>&1; then
    pass "duplicate-key fixture kills a decoder mutation that silently keeps the last value"
  else
    fail "duplicate-key fixture kills a decoder mutation that silently keeps the last value"
  fi

  rm -rf "$tmp_dir"
}

assert_public_release_matrix_is_closed() {
  local block actual expected
  if ! block="$(job_block "build")"; then
    fail "the public release workflow retains exactly its five existing target lanes (could not extract job block)"
    return
  fi
  actual="$(sed -nE 's/^[[:space:]]*- target:[[:space:]]*([^[:space:]]+)[[:space:]]*$/\1/p' <<<"$block" | sort)"
  expected="$(printf '%s\n' \
    aarch64-apple-darwin \
    aarch64-unknown-linux-musl \
    x86_64-apple-darwin \
    x86_64-pc-windows-msvc \
    x86_64-unknown-linux-musl | sort)"

  if [ "$actual" = "$expected" ]; then
    pass "the public release workflow retains exactly its five existing target lanes"
  else
    fail "the public release workflow retains exactly its five existing target lanes"
  fi
}

assert_job_block_contract
if [ "${1:-}" = "--job-block-contract-only" ]; then
  printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
    exit 1
  fi
  printf '\033[0;32mAll tests passed\033[0m\n'
  exit 0
fi

section "Release workflow sequencing"
if workflow_permissions_block | grep -Eq '^\s*permissions:\s*$'; then
  pass "release.yml declares explicit workflow-token permissions"
else
  fail "release.yml declares explicit workflow-token permissions"
fi
if workflow_permissions_block | grep -Eq '^\s*contents:\s*read\s*$'; then
  pass "release.yml defaults the workflow token to read-only repository contents"
else
  fail "release.yml defaults the workflow token to read-only repository contents"
fi
if workflow_permissions_block | grep -Eq '^\s*packages:\s*write\s*$'; then
  fail "release.yml does not grant package-write scope workflow-wide"
else
  pass "release.yml does not grant package-write scope workflow-wide"
fi
if workflow_permissions_block | grep -Eq '^\s*id-token:\s*write\s*$'; then
  fail "release.yml does not grant OIDC workflow-wide"
else
  pass "release.yml does not grant OIDC workflow-wide"
fi
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
assert_exact_count "$RELEASE_WORKFLOW" 'docker/setup-qemu-action@c7c53464625b32c7a7e944ae62b3e17d2b600130' 2 "both QEMU lanes use the same full reviewed setup-qemu commit"
assert_exact_count "$RELEASE_WORKFLOW" 'image: docker.io/tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0' 2 "both QEMU lanes use the same immutable binfmt image index"
assert_not_contains "$RELEASE_WORKFLOW" 'docker/setup-qemu-action@v[0-9]' "release.yml rejects mutable setup-qemu tags"
assert_not_contains "$RELEASE_WORKFLOW" 'tonistiigi/binfmt:(latest|[A-Za-z0-9._-]+)' "release.yml rejects tagged or default-latest binfmt images"
assert_contains "$RELEASE_WORKFLOW" "docker buildx imagetools inspect" "release.yml verifies candidate manifest contents"
assert_contains "$RELEASE_WORKFLOW" "^\\s*RELEASE_REGISTRY: ghcr\\.io$" "release.yml declares one owner for the release registry host"
assert_image_identity_ssot "$RELEASE_WORKFLOW" "this checkout"
assert_job_contains "docker_prepare" 'image="[$][{]RELEASE_REGISTRY[}]/[$][{]RELEASE_IMAGE_REPOSITORY[}]"' "docker_prepare composes its tags from the declared registry coordinates"
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
assert_job_needs "release" "build" "the public tag and GitHub Release wait for release artifacts"
assert_job_needs "release" "ghcr_publish_preflight" "the public tag and GitHub Release wait on proven push capability"
assert_job_contains "release" '^\s*permissions:\s*$' "release job declares its elevated token scope locally"
assert_job_contains "release" '^\s*contents:\s*write\s*$' "release job alone receives repository-write scope for tag and release publication"
assert_file_executable "$REPO_DIR/engine/package/ghcr_publish_preflight" "ghcr_publish_preflight helper is executable"

section "Release CI status preflight"
# This gate reads the ordinary push-CI run for github.sha and must be a true
# prerequisite of `release`; a sibling job would leave tag creation unguarded.
assert_contains "$RELEASE_WORKFLOW" '^\s*acknowledged_ci_failure_run_id:' "release.yml accepts an exact failed push-CI run acknowledgement"
assert_contains "$RELEASE_WORKFLOW" '^\s*default:\s*["'\'']{2}\s*$' "CI failure acknowledgement defaults to empty"
assert_contains "$RELEASE_WORKFLOW" '^\s*release_ci_status_preflight:' "release.yml defines the release CI-status preflight"
assert_job_contains "release_ci_status_preflight" '^\s*needs:\s*validate_release_version\s*$' "CI-status preflight is gated only on version validation"
assert_job_contains "release_ci_status_preflight" '^\s*timeout-minutes:' "CI-status preflight is time-bounded"
assert_job_contains "release_ci_status_preflight" '^\s*actions:\s*read\s*$' "CI-status preflight can read Actions run status"
assert_job_contains "release_ci_status_preflight" '^\s*contents:\s*read\s*$' "CI-status preflight has least-privilege checkout access"
assert_job_contains "release_ci_status_preflight" '^\s*GH_TOKEN:\s*\$\{\{ github\.token \}\}\s*$' "CI-status preflight authenticates gh with the workflow token"
assert_job_contains "release_ci_status_preflight" '^\s*ACKNOWLEDGED_CI_FAILURE_RUN_ID:\s*\$\{\{ github\.event\.inputs\.acknowledged_ci_failure_run_id \}\}\s*$' "CI-status preflight passes acknowledgement through a dedicated environment variable"
assert_job_contains "release_ci_status_preflight" 'package/release_ci_status_preflight' "CI-status preflight calls the Stage 1 owner instead of duplicating decision logic"
assert_job_contains "release_ci_status_preflight" '"\$\{\{ github\.repository \}\}" "\$\{\{ github\.sha \}\}" "ci\.yml" "\$ACKNOWLEDGED_CI_FAILURE_RUN_ID"' "CI-status preflight uses workflow-owned repository, SHA, workflow, and acknowledgement context"
assert_job_needs "release" "release_ci_status_preflight" "the public tag and GitHub Release wait for terminal push CI status"

section "Release build identity packaging"
assert_public_release_matrix_is_closed
assert_contains "$HTTP_MANIFEST" 'utoipa-swagger-ui = \{ version = "8\.0", features = \["axum", "vendored"\] \}' "release builds vendor Swagger UI instead of downloading it during compilation"
assert_file_executable "$RELEASE_BUILD_HELPER" "build_release_artifact helper is executable"
assert_file_executable "$RELEASE_BUILD_HELPER_CONTRACT" "build_release_artifact focused contract is executable"
assert_contains "$RELEASE_BUILD_HELPER" '\^\[0-9a-f\]\{40\}\$' "build helper validates exact lowercase source SHA and tree coordinates"
assert_contains "$RELEASE_BUILD_HELPER" '^export FLAPJACK_BUILD_REVISION="\$SOURCE_SHA"$' "build helper binds the embedded revision to the exact checked-out source"
assert_contains "$RELEASE_BUILD_HELPER" '^export FLAPJACK_REQUIRE_DASHBOARD=1$' "build helper owns the fail-closed dashboard requirement for every profile"
assert_cross_build_revision_passthrough
assert_cross_passthrough_variable \
  "FLAPJACK_REQUIRE_DASHBOARD" \
  "engine/Cross.toml delivers the dashboard asset requirement into cross release builds"
assert_job_contains "build" 'package/build_release_artifact[[:space:]]*\\?$' "the public build matrix calls the shared build helper"
assert_job_contains "build" '^[[:space:]]*public-all[[:space:]]*\\?$' "the public build matrix selects only the public-all closed profile"
assert_job_contains "build" 'git rev-parse.*github\.sha.*\^\{tree\}' "the public build matrix supplies the exact candidate tree to the helper"
assert_job_contains "build" '^[[:space:]]*package/engine_compatibility\.json[[:space:]]*\\?$' "the public build matrix passes only the checked-in compatibility SSOT"
assert_job_not_contains "build" '^[[:space:]]*(cargo|cross) build|npm (ci|run build)|package/release_artifact_manifest' "release.yml cannot bypass the shared build and packaging owner"
assert_job_not_contains "build" '^[[:space:]]*(features|use_cross):' "release.yml does not duplicate the helper's builder or feature map"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.manifest\\.json" "release.yml uploads and publishes manifest JSON assets"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.tar\\.gz" "release.yml uploads and publishes Unix archives"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.tar\\.gz\\.sha256" "release.yml uploads and publishes Unix checksum sidecars"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.zip" "release.yml uploads and publishes Windows archives"
assert_contains "$RELEASE_WORKFLOW" "flapjack-\\*\\.zip\\.sha256" "release.yml uploads and publishes Windows checksum sidecars"
assert_job_contains "build" 'engine/flapjack-\$\{\{ matrix\.target \}\}\.build\.json' "the build job uploads each exact source/toolchain build receipt"
assert_job_contains "release" 'flapjack-\*\.build\.json' "the public release publishes every exact source/toolchain build receipt"
assert_file_executable "$RELEASE_MANIFEST_HELPER" "release_artifact_manifest helper is executable"
assert_contains "$ENGINE_COMPATIBILITY" '^\{"dataDisposition":"preserve","mixedVersionReplication":"not_guaranteed","schemaVersion":2,"targets":\{"aarch64-unknown-linux-musl":\[\{"binarySha256":"70912ad660d67f0c2457814d6e0c6149e9676b787a37ad761848725731bed88c","forwardTransferMode":"reuse_same_data_directory","manifestSha256":"9d567fc7a6c902793d51859a4808eba2cf5b26c7bdb6da5b81516c9798edadff","parityProfile":"same_data_upgrade_smoke_v1","releaseTag":"v1\.0\.16","rollbackMode":"binary_reactivate_same_data","transitionMode":"routine_same_host"\}\],"x86_64-unknown-linux-musl":\[\]\}\}$' "engine compatibility SSOT binds the normalized PBV6 predecessor for routine aarch64 replacement"
assert_contains "$RELEASE_BUILD_HELPER" 'COMPATIBILITY_SOURCE_SHA256' "build helper binds the exact compatibility source digest in its receipt"
assert_contains "$RELEASE_BUILD_HELPER" 'COMPATIBILITY_SELECTED_SHA256' "build helper binds the exact target-selected compatibility digest in its receipt"
assert_release_helper_contract
assert_release_runtime_gate_contract
if bash "$RELEASE_BUILD_HELPER_CONTRACT"; then
  pass "the fast closed-profile build helper contract is green"
else
  fail "the fast closed-profile build helper contract is green"
fi
if bash "$BUILD_IDENTITY_PACKAGE_CONTRACT" --legacy-normalization-only; then
  pass "the fast legacy predecessor normalization contract is green"
else
  fail "the fast legacy predecessor normalization contract is green"
fi

section "Exact predecessor compatibility gate"
assert_contains "$RELEASE_WORKFLOW" '^\s*engine_compatibility_gate:' "release.yml defines the pre-publication engine compatibility gate"
assert_job_contains "engine_compatibility_gate" '^\s*needs:\s*build\s*$' "engine compatibility gate waits for packaged artifacts"
assert_job_contains "engine_compatibility_gate" '^\s*timeout-minutes:\s*6\s*$' "engine compatibility gate has a bounded release-only runtime"
assert_job_contains "engine_compatibility_gate" 'aarch64-unknown-linux-musl' "engine compatibility gate certifies the production aarch64 target"
assert_job_contains "engine_compatibility_gate" 'x86_64-unknown-linux-musl' "engine compatibility gate certifies the x86_64 target"
assert_job_contains "engine_compatibility_gate" 'docker/setup-qemu-action@c7c53464625b32c7a7e944ae62b3e17d2b600130' "engine compatibility gate enables the pinned arm64 QEMU authority"
assert_job_contains "engine_compatibility_gate" 'image: docker.io/tonistiigi/binfmt@sha256:400a4873b838d1b89194d982c45e5fb3cda4593fbfd7e08a02e76b03b21166f0' "engine compatibility gate pins the arm64 interpreter image by digest"
assert_job_contains "engine_compatibility_gate" 'compatibility-predecessors' "engine compatibility gate enumerates only validated exact predecessor manifests"
assert_job_contains "engine_compatibility_gate" 'timeout --signal=TERM --kill-after=5s 15s package/release_artifact_runtime_gate attest' "every candidate executes under a hard build-info attestation timeout"
assert_job_order "engine_compatibility_gate" 'release_artifact_runtime_gate attest' 'if \[ ! -s "\$predecessors_file" \]' "candidate build-info attestation precedes the empty-predecessor decision"
if [ "${RELEASE_STRUCTURE_SKIP_MUTANTS:-0}" != "1" ]; then
  ATTESTATION_REMOVED_WORKFLOW="$(mktemp "${TMPDIR:-/tmp}/flapjack-release-no-attest.XXXXXX")"
  sed '/timeout --signal=TERM --kill-after=5s 15s package\/release_artifact_runtime_gate attest/d' \
    "$RELEASE_WORKFLOW" >"$ATTESTATION_REMOVED_WORKFLOW"
  if RELEASE_STRUCTURE_SKIP_MUTANTS=1 RELEASE_WORKFLOW_UNDER_TEST="$ATTESTATION_REMOVED_WORKFLOW" \
    bash "$0" >/dev/null 2>&1; then
    fail "release structure contract kills removal of candidate runtime attestation"
  else
    pass "release structure contract kills removal of candidate runtime attestation"
  fi
  rm -f "$ATTESTATION_REMOVED_WORKFLOW"
fi
assert_exact_count "$RELEASE_WORKFLOW" 'package/release_artifact_runtime_gate extract' 2 "candidate and predecessor archives share one safe extraction owner"
assert_job_not_contains "engine_compatibility_gate" 'tar -xzf' \
  "engine compatibility gate never delegates candidate or predecessor extraction to ambient tar"
assert_job_contains "engine_compatibility_gate" 'gh release download "\$release_tag"' "engine compatibility gate fetches each named published predecessor"
assert_job_contains "engine_compatibility_gate" 'manifestSha256' "engine compatibility gate verifies the exact predecessor manifest digest"
assert_job_contains "engine_compatibility_gate" 'old-binary-sha256 "\$binary_sha"' "engine compatibility smoke binds the predecessor executable to the exact declaration"
assert_job_contains "engine_compatibility_gate" 'tests/upgrade_smoke\.sh' "engine compatibility gate executes the same-data-dir smoke owner"
assert_job_contains "engine_compatibility_gate" 'timeout --signal=TERM --kill-after=5s 90s' "each historical upgrade smoke has a 90-second hard ceiling"
assert_job_needs "release" "engine_compatibility_gate" "the public tag and release wait for exact predecessor compatibility proof"

section "Docker build hang protection and retry safety"
assert_contains "$DOCKERFILE" '^ARG FLAPJACK_BUILD_REVISION$' "Dockerfile accepts the canonical build revision as a build argument"
assert_job_contains "docker_build_amd64" '^\s*FLAPJACK_BUILD_REVISION=\$\{\{ github\.sha \}\}\s*$' "amd64 release image embeds the exact release commit"
assert_job_contains "docker_build_arm64_native" '^\s*FLAPJACK_BUILD_REVISION=\$\{\{ github\.sha \}\}\s*$' "native arm64 release image embeds the exact release commit"
assert_job_contains "docker_build_arm64_qemu" '^\s*FLAPJACK_BUILD_REVISION=\$\{\{ github\.sha \}\}\s*$' "qemu arm64 release image embeds the exact release commit"
assert_job_contains "docker_build_amd64" '^\s*packages:\s*write\s*$' "amd64 Docker publish job keeps package-write scope local to the publishing lane"
assert_job_contains "docker_build_amd64" '^\s*id-token:\s*write\s*$' "amd64 Docker publish job keeps OIDC scope local to the publishing lane"
assert_job_contains "docker_build_arm64_native" '^\s*packages:\s*write\s*$' "arm64 native Docker publish job keeps package-write scope local to the publishing lane"
assert_job_contains "docker_build_arm64_native" '^\s*id-token:\s*write\s*$' "arm64 native Docker publish job keeps OIDC scope local to the publishing lane"
assert_job_contains "docker_build_arm64_qemu" '^\s*packages:\s*write\s*$' "arm64 qemu Docker publish job keeps package-write scope local to the publishing lane"
assert_job_contains "docker_build_arm64_qemu" '^\s*id-token:\s*write\s*$' "arm64 qemu Docker publish job keeps OIDC scope local to the publishing lane"
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
assert_contains "$CI_WORKFLOW" '^\s*run: bash engine/tests/validate_public_ledger_citations_test\.sh\s*$' "ci.yml runs the public-ledger citation contract tests"
assert_contains "$CI_WORKFLOW" '^\s*run: bash engine/tests/validate_public_ledger_citations\.sh --mode mirror\s*$' "ci.yml runs the public-ledger citation oracle in mirror mode"
# The assertions above prove release.yml DECLARES the CI-status preflight job. This one
# proves the preflight's behavioural contract is executed rather than merely present:
# engine/tests/test_release_ci_status_preflight.sh shipped with REL-12 and was invoked by
# no workflow, which is the same inert-contract failure this section was written for.
#
# GH_TOKEN is part of the invocation contract, not incidental (tightened 2026-08-07).
# The contract drives the preflight against live pinned runs in flapjackhq/flapjack, so an
# unauthenticated `gh` fails 17 of its 41 cases. This assertion previously matched a BARE
# `run:` line, which is what an unauthenticated copy of the step looks like — so the one
# invocation shape guaranteed to fail in CI was the only shape this test accepted. Staging
# run 31213083385 failed exactly that way while this assertion was green. Requiring the
# token here means the failing shape can no longer satisfy the contract.
assert_contains "$CI_WORKFLOW" '^\s*run: GH_TOKEN="\$\{\{ github\.token \}\}" bash engine/tests/test_release_ci_status_preflight\.sh\s*$' "ci.yml runs the release CI-status preflight contract with GH_TOKEN"

if [ "$SECONDS" -le 30 ]; then
  pass "release workflow structure contract stays within its 30-second hard cap (${SECONDS}s)"
else
  fail "release workflow structure contract stays within its 30-second hard cap (${SECONDS}s)"
fi

printf '\n\033[1mResults: %d/%d passed\033[0m\n' "$TESTS_PASSED" "$TESTS_RUN"
if [ "$TESTS_FAILED" -gt 0 ]; then
  printf '\033[0;31m%d test(s) failed\033[0m\n' "$TESTS_FAILED"
  exit 1
fi
printf '\033[0;32mAll tests passed\033[0m\n'
