# shellcheck shell=bash
# Foreign-artifact assertions sourced by build_identity_package_contract.sh.
assert_foreign_fixture_identity() {
  [ -f "$FOREIGN_FIXTURE" ] || die "foreign target fixture is missing: $FOREIGN_FIXTURE"
  [ -x "$FOREIGN_FIXTURE" ] || die "foreign target fixture must satisfy the helper executable precondition"

  python3 - "$FOREIGN_FIXTURE" "$FOREIGN_FIXTURE_SHA256" <<'PY'
import hashlib
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
expected_sha256 = sys.argv[2]
contents = path.read_bytes()
if hashlib.sha256(contents).hexdigest() != expected_sha256:
    raise SystemExit("foreign target fixture bytes do not match the known executable")
if contents[:4] != b"\x7fELF" or contents[4:6] != b"\x02\x01":
    raise SystemExit("foreign target fixture must be a 64-bit little-endian ELF")
if int.from_bytes(contents[18:20], "little") != 183:
    raise SystemExit("foreign target fixture must declare the Linux aarch64 machine type")
PY
}

run_foreign_package_without_execution() {
  local package_helper="$1"
  local proof_name="$2"
  local output_dir="$3"
  local stdout="$TMP_ROOT/${proof_name}_stdout.log"
  local stderr="$TMP_ROOT/${proof_name}_stderr.log"
  local execution_sentinel="$TMP_ROOT/${proof_name}_execution_sentinel"
  local status=0

  set +e
  COPYFILE_DISABLE=1 \
    FLAPJACK_EXECUTION_SENTINEL="$execution_sentinel" \
    "$package_helper" "$FOREIGN_TARGET" "$FOREIGN_FIXTURE" "$output_dir" >"$stdout" 2>"$stderr"
  status=$?
  set -e

  if [ -e "$execution_sentinel" ]; then
    die "foreign target package helper host-executed the target binary (execution sentinel created)"
  fi

  if [ "$status" -ne 0 ]; then
    cat "$stdout" "$stderr" >&2
    die "foreign target package helper must produce a manifest without host-executing the target binary (status $status)"
  fi
}

assert_foreign_package_outputs() {
  local output_dir="$1"
  python3 - \
    "$FOREIGN_FIXTURE" \
    "$output_dir/flapjack-${FOREIGN_TARGET}.manifest.json" \
    "$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz" <<'PY'
import hashlib
import json
import pathlib
import sys
import tarfile

fixture_path = pathlib.Path(sys.argv[1])
manifest_path = pathlib.Path(sys.argv[2])
archive_path = pathlib.Path(sys.argv[3])

expected_build = {
    "schemaVersion": 1,
    "version": "1.0.11-fixture",
    "revision": "0123456789abcdef0123456789abcdef01234567",
    "revisionKnown": True,
    "dirty": False,
    "dirtyKnown": True,
    "workspaceDigest": "a" * 64,
    "profile": "release",
    "target": "aarch64-unknown-linux-musl",
    "features": ["fixture-feature", "vector-search"],
    "capabilities": {
        "vectorSearch": True,
        "vectorSearchLocal": False,
    },
}

expected_compatibility = {
    "schemaVersion": 1,
    "target": "aarch64-unknown-linux-musl",
    "predecessors": [],
    "dataDisposition": "preserve",
    "mixedVersionReplication": "not_guaranteed",
}

manifest = json.loads(manifest_path.read_text())
if set(manifest) != {"schemaVersion", "artifact", "build", "compatibility"}:
    raise SystemExit(f"foreign target manifest keys mismatch: {sorted(manifest)}")
if manifest["schemaVersion"] != 2:
    raise SystemExit(f"foreign target schemaVersion mismatch: {manifest['schemaVersion']}")
if manifest["build"] != expected_build:
    raise SystemExit(
        "foreign target build metadata mismatch:\n"
        f"expected={json.dumps(expected_build, sort_keys=True)}\n"
        f"actual={json.dumps(manifest['build'], sort_keys=True)}"
    )
if manifest["compatibility"] != expected_compatibility:
    raise SystemExit(
        "foreign target compatibility metadata mismatch:\n"
        f"expected={json.dumps(expected_compatibility, sort_keys=True)}\n"
        f"actual={json.dumps(manifest['compatibility'], sort_keys=True)}"
    )

archive_sha256 = hashlib.sha256(archive_path.read_bytes()).hexdigest()
expected_artifact = {
    "file": "flapjack-aarch64-unknown-linux-musl.tar.gz",
    "target": "aarch64-unknown-linux-musl",
    "arch": "aarch64",
    "profile": "release",
    "binarySha256": hashlib.sha256(fixture_path.read_bytes()).hexdigest(),
    "sha256": archive_sha256,
}
if manifest["artifact"] != expected_artifact:
    raise SystemExit(
        "foreign target artifact metadata mismatch:\n"
        f"expected={json.dumps(expected_artifact, sort_keys=True)}\n"
        f"actual={json.dumps(manifest['artifact'], sort_keys=True)}"
    )

sidecar_path = pathlib.Path(str(archive_path) + ".sha256")
if sidecar_path.read_text().strip().split() != [archive_sha256, archive_path.name]:
    raise SystemExit("foreign target checksum sidecar does not match the packaged archive")

with tarfile.open(archive_path, "r:gz") as archive:
    packaged_files = [member for member in archive.getmembers() if member.isfile()]
    if [member.name for member in packaged_files] != ["./flapjack"]:
        raise SystemExit(
            f"foreign target archive file set mismatch: {[member.name for member in packaged_files]}"
        )
    packaged_binary = archive.extractfile(packaged_files[0])
    if packaged_binary is None or packaged_binary.read() != fixture_path.read_bytes():
        raise SystemExit("foreign target archive does not contain the exact fixture binary")
PY
}

assert_foreign_target_manifest_contract() {
  local package_helper="$1"
  local proof_name="$2"
  local output_dir="$TMP_ROOT/${proof_name}_output"

  mkdir -p "$output_dir"
  assert_foreign_fixture_identity
  run_foreign_package_without_execution "$package_helper" "$proof_name" "$output_dir"
  assert_foreign_package_outputs "$output_dir"
}

assert_compatibility_contract_rejects_ambiguous_claims() {
  local helper_dir="$TMP_ROOT/compatibility_helper"
  local helper="$helper_dir/release_artifact_manifest"
  local compatibility="$helper_dir/engine_compatibility.json"
  local fixtures="$TMP_ROOT/compatibility_fixtures"
  local case_name output_dir stderr status

  mkdir -p "$helper_dir" "$fixtures"
  cp "$PACKAGE_HELPER" "$helper"
  chmod +x "$helper"

  python3 - "$fixtures" <<'PY'
import json
import pathlib
import sys

fixtures = pathlib.Path(sys.argv[1])
base = {
    "schemaVersion": 2,
    "targets": {
        "aarch64-unknown-linux-musl": [],
        "x86_64-unknown-linux-musl": [],
    },
    "dataDisposition": "preserve",
    "mixedVersionReplication": "not_guaranteed",
}

def write(name, value):
    (fixtures / name).write_text(json.dumps(value, sort_keys=True, separators=(",", ":")) + "\n")

unknown = dict(base)
unknown["storageEpoch"] = 1
write("unknown_key", unknown)

version_only = json.loads(json.dumps(base))
version_only["targets"]["x86_64-unknown-linux-musl"] = [{"releaseTag": "v1.0.15"}]
write("version_only_predecessor", version_only)

invalid_rollback = json.loads(json.dumps(base))
invalid_rollback["targets"]["x86_64-unknown-linux-musl"] = [{
    "releaseTag": "v1.0.15",
    "manifestSha256": "a" * 64,
    "binarySha256": "b" * 64,
    "transitionMode": "routine_same_host",
    "forwardTransferMode": "reuse_same_data_directory",
    "rollbackMode": "binary_only",
    "parityProfile": "same_data_upgrade_smoke_v1",
}]
write("invalid_rollback", invalid_rollback)

too_many = json.loads(json.dumps(base))
too_many["targets"]["x86_64-unknown-linux-musl"] = [{
    "releaseTag": f"v1.0.{index}",
    "manifestSha256": f"{index}" * 64,
    "binarySha256": f"{index + 4}" * 64,
    "transitionMode": "routine_same_host",
    "forwardTransferMode": "reuse_same_data_directory",
    "rollbackMode": "restore_pre_upgrade_backup",
    "parityProfile": "same_data_upgrade_smoke_v1",
} for index in range(4)]
write("too_many_predecessors", too_many)

repeated_binary = json.loads(json.dumps(base))
repeated_binary["targets"]["x86_64-unknown-linux-musl"] = [{
    "releaseTag": f"v1.0.{index}",
    "manifestSha256": f"{index + 1}" * 64,
    "binarySha256": "f" * 64,
    "transitionMode": "routine_same_host",
    "forwardTransferMode": "reuse_same_data_directory",
    "rollbackMode": "binary_reactivate_same_data",
    "parityProfile": "same_data_upgrade_smoke_v1",
} for index in range(2)]
write("repeated_binary_sha256", repeated_binary)

ordered_records = [{
    "releaseTag": release_tag,
    "manifestSha256": manifest_character * 64,
    "binarySha256": binary_character * 64,
    "transitionMode": "routine_same_host",
    "forwardTransferMode": "reuse_same_data_directory",
    "rollbackMode": "binary_reactivate_same_data",
    "parityProfile": "same_data_upgrade_smoke_v1",
} for release_tag, manifest_character, binary_character in (
    ("v1.0.14", "a", "c"),
    ("v1.0.15", "b", "d"),
)]
out_of_order = json.loads(json.dumps(base))
out_of_order["targets"]["x86_64-unknown-linux-musl"] = list(reversed(ordered_records))
write("out_of_order_predecessors", out_of_order)

(fixtures / "duplicate_key").write_text(
    '{"dataDisposition":"preserve","dataDisposition":"preserve",'
    '"mixedVersionReplication":"not_guaranteed","schemaVersion":2,'
    '"targets":{"aarch64-unknown-linux-musl":[],"x86_64-unknown-linux-musl":[]}}\n'
)
(fixtures / "duplicate_transition_mode").write_text(
    '{"dataDisposition":"preserve","mixedVersionReplication":"not_guaranteed",'
    '"schemaVersion":2,"targets":{"aarch64-unknown-linux-musl":[],'
    '"x86_64-unknown-linux-musl":[{"binarySha256":"' + "b" * 64 + '",'
    '"forwardTransferMode":"reuse_same_data_directory",'
    '"manifestSha256":"' + "a" * 64 + '","parityProfile":"same_data_upgrade_smoke_v1",'
    '"releaseTag":"v1.0.15","rollbackMode":"binary_reactivate_same_data",'
    '"transitionMode":"routine_same_host","transitionMode":"routine_same_host"}]}}\n'
)
(fixtures / "noncanonical").write_text(json.dumps(base, indent=2) + "\n")

missing_aarch64 = json.loads(json.dumps(base))
del missing_aarch64["targets"]["aarch64-unknown-linux-musl"]
write("missing_aarch64_target", missing_aarch64)

uncertified_target = json.loads(json.dumps(base))
uncertified_target["targets"]["x86_64-apple-darwin"] = []
write("uncertified_target", uncertified_target)
PY

  for case_name in \
    unknown_key \
    version_only_predecessor \
    invalid_rollback \
    too_many_predecessors \
    repeated_binary_sha256 \
    out_of_order_predecessors \
    duplicate_key \
    duplicate_transition_mode \
    noncanonical \
    missing_aarch64_target \
    uncertified_target; do
    cp "$fixtures/$case_name" "$compatibility"
    output_dir="$TMP_ROOT/compatibility_${case_name}_output"
    stderr="$TMP_ROOT/compatibility_${case_name}_stderr.log"
    mkdir -p "$output_dir"
    set +e
    "$helper" "$FOREIGN_TARGET" "$FOREIGN_FIXTURE" "$output_dir" >/dev/null 2>"$stderr"
    status=$?
    set -e
    [ "$status" -ne 0 ] || die "ambiguous compatibility case $case_name was silently packaged"
    grep -Fq 'engine compatibility' "$stderr" \
      || die "ambiguous compatibility case $case_name failed without a compatibility diagnostic"
    [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.manifest.json" ] \
      || die "ambiguous compatibility case $case_name must not produce a manifest"
  done
}

assert_target_specific_predecessor_selection() {
  local helper_dir="$TMP_ROOT/target_selection_helper"
  local helper="$helper_dir/release_artifact_manifest"
  local compatibility="$helper_dir/engine_compatibility.json"

  mkdir -p "$helper_dir"
  cp "$PACKAGE_HELPER" "$helper"
  chmod +x "$helper"
  python3 - "$helper" "$compatibility" "$TMP_ROOT" <<'PY'
import json
import pathlib
import subprocess
import sys

helper = pathlib.Path(sys.argv[1])
compatibility_path = pathlib.Path(sys.argv[2])
root = pathlib.Path(sys.argv[3])
entry = {
    "releaseTag": "v1.0.15",
    "manifestSha256": "a" * 64,
    "binarySha256": "b" * 64,
    # A predecessor claim is executable only when it selects one complete,
    # closed transition recipe.  These fields deliberately travel with the
    # exact predecessor digests instead of being inferred from live state.
    "transitionMode": "exceptional_blue_green",
    "forwardTransferMode": "snapshot_then_tail_replication",
    "rollbackMode": "reverse_tail_to_retained_predecessor",
    "parityProfile": "populated_engine_exact_v1",
}
source = {
    "schemaVersion": 2,
    "targets": {
        "aarch64-unknown-linux-musl": [],
        "x86_64-unknown-linux-musl": [entry],
    },
    "dataDisposition": "preserve",
    "mixedVersionReplication": "not_guaranteed",
}
compatibility_path.write_text(
    json.dumps(source, sort_keys=True, separators=(",", ":")) + "\n"
)

def candidate(name, target, predecessors, binary_sha="c" * 64):
    path = root / name
    selected = {
        "schemaVersion": 1,
        "target": target,
        "predecessors": predecessors,
        "dataDisposition": "preserve",
        "mixedVersionReplication": "not_guaranteed",
    }
    manifest = {
        "schemaVersion": 2,
        "artifact": {"target": target, "binarySha256": binary_sha},
        "build": {"target": target},
        "compatibility": selected,
    }
    path.write_text(
        json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n"
    )
    return path, manifest

def invoke(path):
    return subprocess.run(
        [str(helper), "--compatibility-predecessors", str(path)],
        check=False,
        capture_output=True,
        text=True,
    )

x86_path, x86_manifest = candidate("x86_candidate.json", "x86_64-unknown-linux-musl", [entry])
x86 = invoke(x86_path)
expected = (
    "v1.0.15\t" + "a" * 64 + "\t" + "b" * 64
    + "\texceptional_blue_green\tsnapshot_then_tail_replication"
    + "\treverse_tail_to_retained_predecessor\tpopulated_engine_exact_v1\n"
)
if x86.returncode != 0 or x86.stdout != expected:
    raise SystemExit(f"exact x86 predecessor selection drifted: {x86.stdout!r} {x86.stderr!r}")

absent_path, _ = candidate("absent_candidate.json", "x86_64-apple-darwin", [])
absent = invoke(absent_path)
if absent.returncode != 0 or absent.stdout:
    raise SystemExit(f"absent target did not fail closed: {absent.stdout!r} {absent.stderr!r}")

_, arm = candidate("arm_candidate.json", "aarch64-unknown-linux-musl", [])
mutations = {}
wrong_selected = json.loads(json.dumps(arm))
wrong_selected["compatibility"]["target"] = "x86_64-unknown-linux-musl"
mutations["wrong_selected_target"] = wrong_selected
wrong_build = json.loads(json.dumps(arm))
wrong_build["build"]["target"] = "x86_64-unknown-linux-musl"
mutations["wrong_build_target"] = wrong_build
foreign_predecessor = json.loads(json.dumps(arm))
foreign_predecessor["compatibility"]["predecessors"] = [entry]
mutations["foreign_predecessor"] = foreign_predecessor
candidate_reuses_predecessor = json.loads(json.dumps(x86_manifest))
candidate_reuses_predecessor["artifact"]["binarySha256"] = "b" * 64
mutations["candidate_reuses_predecessor"] = candidate_reuses_predecessor
boolean_selected_schema = json.loads(json.dumps(x86_manifest))
boolean_selected_schema["compatibility"]["schemaVersion"] = True
mutations["boolean_selected_schema"] = boolean_selected_schema

# Each exact transition coordinate is mandatory.  Dropping any one would let a
# later release owner guess a deployment or parity path from ambient state.
for key in ("transitionMode", "forwardTransferMode", "rollbackMode", "parityProfile"):
    missing_coordinate = json.loads(json.dumps(x86_manifest))
    del missing_coordinate["compatibility"]["predecessors"][0][key]
    mutations[f"missing_{key}"] = missing_coordinate

unknown_transition = json.loads(json.dumps(x86_manifest))
unknown_transition["compatibility"]["predecessors"][0]["transitionMode"] = "automatic"
mutations["unknown_transition_mode"] = unknown_transition
unknown_forward = json.loads(json.dumps(x86_manifest))
unknown_forward["compatibility"]["predecessors"][0]["forwardTransferMode"] = "copy_somehow"
mutations["unknown_forward_transfer_mode"] = unknown_forward
unknown_rollback = json.loads(json.dumps(x86_manifest))
unknown_rollback["compatibility"]["predecessors"][0]["rollbackMode"] = "best_effort"
mutations["unknown_rollback_mode"] = unknown_rollback
unknown_parity = json.loads(json.dumps(x86_manifest))
unknown_parity["compatibility"]["predecessors"][0]["parityProfile"] = "best_effort"
mutations["unknown_parity_profile"] = unknown_parity

for key, invalid in (
    ("transitionMode", False),
    ("forwardTransferMode", []),
    ("rollbackMode", 1),
    ("parityProfile", None),
):
    wrong_type = json.loads(json.dumps(x86_manifest))
    wrong_type["compatibility"]["predecessors"][0][key] = invalid
    mutations[f"non_string_{key}"] = wrong_type

# Closed values are insufficient on their own: combining routine transfer with
# exceptional parity is ambiguous and must fail as a whole recipe.
ambiguous_recipe = json.loads(json.dumps(x86_manifest))
ambiguous_recipe["compatibility"]["predecessors"][0].update({
    "transitionMode": "routine_same_host",
    "forwardTransferMode": "reuse_same_data_directory",
})
mutations["ambiguous_transition_recipe"] = ambiguous_recipe

# Runtime observations are evidence, never transition selectors.  The exact
# key set rejects such ambient state even when it looks harmless or empty.
ambient_selector = json.loads(json.dumps(x86_manifest))
ambient_selector["compatibility"]["predecessors"][0]["activeWriters"] = 0
mutations["ambient_state_selector"] = ambient_selector

for name, manifest in mutations.items():
    path = root / f"{name}.json"
    path.write_text(
        json.dumps(manifest, sort_keys=True, separators=(",", ":")) + "\n"
    )
    result = invoke(path)
    if result.returncode == 0 or "engine compatibility" not in result.stderr:
        raise SystemExit(f"target mutation {name} was not rejected: {result.stdout!r} {result.stderr!r}")

# The routine recipe uses the already-executable same-data upgrade smoke.  It
# is checked separately so both allowed transition modes have a positive oracle
# without advertising either synthetic predecessor in the real source map.
routine = {
    "releaseTag": "v1.0.14",
    "manifestSha256": "d" * 64,
    "binarySha256": "e" * 64,
    "transitionMode": "routine_same_host",
    "forwardTransferMode": "reuse_same_data_directory",
    "rollbackMode": "binary_reactivate_same_data",
    "parityProfile": "same_data_upgrade_smoke_v1",
}
source["targets"]["x86_64-unknown-linux-musl"] = [routine]
compatibility_path.write_text(
    json.dumps(source, sort_keys=True, separators=(",", ":")) + "\n"
)
routine_path, _ = candidate(
    "routine_candidate.json", "x86_64-unknown-linux-musl", [routine]
)
routine_result = invoke(routine_path)
routine_expected = (
    "v1.0.14\t" + "d" * 64 + "\t" + "e" * 64
    + "\troutine_same_host\treuse_same_data_directory"
    + "\tbinary_reactivate_same_data\tsame_data_upgrade_smoke_v1\n"
)
if routine_result.returncode != 0 or routine_result.stdout != routine_expected:
    raise SystemExit(
        f"exact routine predecessor selection drifted: "
        f"{routine_result.stdout!r} {routine_result.stderr!r}"
    )

# Exhaust the closed coordinate product instead of sampling it.  This catches
# both directions of recipe drift: deleting any approved tuple or admitting any
# hybrid assembled from otherwise valid enum members.
transition_modes = ("routine_same_host", "exceptional_blue_green")
forward_modes = ("reuse_same_data_directory", "snapshot_then_tail_replication")
rollback_modes = (
    "binary_reactivate_same_data",
    "restore_pre_upgrade_backup",
    "reverse_tail_to_retained_predecessor",
)
parity_profiles = ("same_data_upgrade_smoke_v1", "populated_engine_exact_v1")
allowed_recipes = {
    (
        "routine_same_host",
        "reuse_same_data_directory",
        "binary_reactivate_same_data",
        "same_data_upgrade_smoke_v1",
    ),
    (
        "routine_same_host",
        "reuse_same_data_directory",
        "restore_pre_upgrade_backup",
        "same_data_upgrade_smoke_v1",
    ),
    (
        "exceptional_blue_green",
        "snapshot_then_tail_replication",
        "reverse_tail_to_retained_predecessor",
        "populated_engine_exact_v1",
    ),
}
observed_allowed = set()
case_index = 0
for transition_mode in transition_modes:
    for forward_mode in forward_modes:
        for rollback_mode in rollback_modes:
            for parity_profile in parity_profiles:
                recipe = (
                    transition_mode,
                    forward_mode,
                    rollback_mode,
                    parity_profile,
                )
                matrix_entry = {
                    "releaseTag": "v1.0.13",
                    "manifestSha256": "1" * 64,
                    "binarySha256": "2" * 64,
                    "transitionMode": transition_mode,
                    "forwardTransferMode": forward_mode,
                    "rollbackMode": rollback_mode,
                    "parityProfile": parity_profile,
                }
                source["targets"]["x86_64-unknown-linux-musl"] = [matrix_entry]
                compatibility_path.write_text(
                    json.dumps(source, sort_keys=True, separators=(",", ":")) + "\n"
                )
                matrix_path, _ = candidate(
                    f"recipe_matrix_{case_index}.json",
                    "x86_64-unknown-linux-musl",
                    [matrix_entry],
                )
                case_index += 1
                matrix_result = invoke(matrix_path)
                if recipe in allowed_recipes:
                    if matrix_result.returncode != 0:
                        raise SystemExit(
                            f"approved recipe was rejected: {recipe!r} "
                            f"{matrix_result.stderr!r}"
                        )
                    observed_allowed.add(recipe)
                elif matrix_result.returncode == 0:
                    raise SystemExit(f"unapproved recipe was accepted: {recipe!r}")
if observed_allowed != allowed_recipes:
    raise SystemExit(
        f"approved recipe coverage drifted: {observed_allowed!r} != {allowed_recipes!r}"
    )
PY
}

assert_malformed_embedded_records_rejected() {
  local fixtures_dir="$TMP_ROOT/malformed_record_fixtures"
  local case_name expected_error fixture output_dir stderr helper_status
  mkdir -p "$fixtures_dir"

  python3 - "$FOREIGN_FIXTURE" "$fixtures_dir" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1]).read_bytes()
fixtures_dir = pathlib.Path(sys.argv[2])
begin = b"FLAPJACK_BUILD_INFO_JSON_BEGIN\n"
end = b"\nFLAPJACK_BUILD_INFO_JSON_END\n"
start = source.index(begin)
finish = source.index(end, start + len(begin))
prefix = source[:start]
record = source[start + len(begin):finish]
suffix = source[finish + len(end):]
revision_known = b'"revisionKnown":true'
if record.count(revision_known) != 1:
    raise SystemExit("fixture must contain exactly one revisionKnown member")

fixtures = {
    "missing_begin": prefix + record + end + suffix,
    "missing_end": prefix + begin + record + suffix,
    "duplicate_begin": prefix + begin + begin + record + end + suffix,
    "duplicate_end": prefix + begin + record + end + end + suffix,
    "end_before_begin": prefix + end + record + begin + suffix,
    "malformed_json": prefix + begin + b"{" + end + suffix,
    "invalid_utf8": prefix + begin + b"\xff" + end + suffix,
    "duplicate_json_key": prefix + begin + record.replace(
        revision_known, revision_known + b',"revisionKnown":true'
    ) + end + suffix,
}
for name, contents in fixtures.items():
    path = fixtures_dir / name
    path.write_bytes(contents)
    path.chmod(0o755)
PY

  while IFS='|' read -r case_name expected_error; do
    fixture="$fixtures_dir/$case_name"
    output_dir="$TMP_ROOT/${case_name}_output"
    stderr="$TMP_ROOT/${case_name}_stderr.log"
    mkdir -p "$output_dir"

    set +e
    # shellcheck disable=SC2153 # Assigned by the sourcing contract driver.
    "$PACKAGE_HELPER" "$FOREIGN_TARGET" "$fixture" "$output_dir" >/dev/null 2>"$stderr"
    helper_status=$?
    set -e

    [ "$helper_status" -ne 0 ] \
      || die "$case_name embedded build-info record was silently accepted"
    grep -Fq "$expected_error" "$stderr" \
      || die "$case_name record failed without the expected diagnostic"
    [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz" ] \
      || die "$case_name record must not produce an archive"
    [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz.sha256" ] \
      || die "$case_name record must not produce a checksum"
    [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.manifest.json" ] \
      || die "$case_name record must not produce a manifest"
  done <<'CASES'
missing_begin|embedded build-info JSON begin marker must appear exactly once, found 0
missing_end|embedded build-info JSON end marker must appear exactly once, found 0
duplicate_begin|embedded build-info JSON begin marker must appear exactly once, found 2
duplicate_end|embedded build-info JSON end marker must appear exactly once, found 2
end_before_begin|embedded build-info JSON end marker precedes begin marker
malformed_json|embedded build-info JSON is malformed
invalid_utf8|embedded build-info JSON is not UTF-8
duplicate_json_key|embedded build-info JSON contains duplicate key: revisionKnown
CASES
}

assert_traversal_target_rejected_without_outside_write() {
  local output_dir="$TMP_ROOT/traversal_target_output"
  local escaped_path="$TMP_ROOT/escaped.tar.gz"
  local stderr="$TMP_ROOT/traversal_target_stderr.log"
  local status=0

  # This directory makes the old interpolation resolve
  # <output>/flapjack-aarch64/../../escaped.tar.gz outside output_dir.
  mkdir -p "$output_dir/flapjack-aarch64"
  set +e
  "$PACKAGE_HELPER" 'aarch64/../../escaped' "$FOREIGN_FIXTURE" "$output_dir" \
    >/dev/null 2>"$stderr"
  status=$?
  set -e

  [ "$status" -ne 0 ] || die "path-bearing target triple was silently accepted"
  grep -Fq 'target triple must be a hyphen-separated Rust target name' "$stderr" \
    || die "path-bearing target triple failed without the expected diagnostic"
  [ ! -e "$escaped_path" ] \
    || die "path-bearing target triple wrote an archive outside the requested output directory"
}

assert_incomplete_build_record_rejected() {
  local fixture="$TMP_ROOT/incomplete_build_record_fixture"
  local output_dir="$TMP_ROOT/incomplete_build_record_output"
  local stderr="$TMP_ROOT/incomplete_build_record_stderr.log"
  local status=0

  mkdir -p "$output_dir"
  printf 'stale archive\n' >"$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz"
  printf 'stale checksum\n' >"$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz.sha256"
  printf 'stale manifest\n' >"$output_dir/flapjack-${FOREIGN_TARGET}.manifest.json"
  python3 - "$fixture" <<'PY'
import pathlib
import sys

fixture = pathlib.Path(sys.argv[1])
fixture.write_text(
    "FLAPJACK_BUILD_INFO_JSON_BEGIN\n"
    '{"profile":"release","revisionKnown":true,'
    '"target":"aarch64-unknown-linux-musl"}\n'
    "FLAPJACK_BUILD_INFO_JSON_END\n"
)
fixture.chmod(0o755)
PY

  set +e
  "$PACKAGE_HELPER" "$FOREIGN_TARGET" "$fixture" "$output_dir" >/dev/null 2>"$stderr"
  status=$?
  set -e

  [ "$status" -ne 0 ] || die "incomplete embedded build-info record was silently packaged"
  grep -Fq 'embedded build-info JSON keys mismatch' "$stderr" \
    || die "incomplete embedded build-info record failed without the expected diagnostic"
  [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz" ] \
    || die "incomplete embedded build-info record must not produce an archive"
  [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.tar.gz.sha256" ] \
    || die "incomplete embedded build-info record must not produce a checksum"
  [ ! -e "$output_dir/flapjack-${FOREIGN_TARGET}.manifest.json" ] \
    || die "incomplete embedded build-info record must not produce a manifest"
}

assert_linux_musl_cli_mismatch_rejected() {
  local fixture="$TMP_ROOT/x86_64_musl_fixture"
  local fake_tools="$TMP_ROOT/fake_tools"
  local execution_sentinel="$TMP_ROOT/linux_musl_execution_sentinel"
  local output_dir="$TMP_ROOT/linux_musl_output"
  local stdout="$TMP_ROOT/linux_musl_stdout.log"
  local stderr="$TMP_ROOT/linux_musl_stderr.log"
  local status=0

  mkdir -p "$fake_tools" "$output_dir"
  python3 - "$fixture" "$fake_tools/rustc" <<'PY'
import pathlib
import sys

fixture = pathlib.Path(sys.argv[1])
fake_rustc = pathlib.Path(sys.argv[2])
embedded = '{"capabilities":{"vectorSearch":true,"vectorSearchLocal":false},"dirty":false,"dirtyKnown":true,"features":["vector-search"],"profile":"release","revision":"0123456789abcdef0123456789abcdef01234567","revisionKnown":true,"schemaVersion":1,"target":"x86_64-unknown-linux-musl","version":"1.0.11-fixture","workspaceDigest":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}'
executed = embedded.replace('"version":"1.0.11-fixture"', '"version":"1.0.11-mismatch"')
fixture.write_text(
    "#!/usr/bin/env bash\n"
    ": <<'BUILD_INFO_RECORD'\n"
    "FLAPJACK_BUILD_INFO_JSON_BEGIN\n"
    f"{embedded}\n"
    "FLAPJACK_BUILD_INFO_JSON_END\n"
    "BUILD_INFO_RECORD\n"
    'printf "executed\\n" >"${FLAPJACK_EXECUTION_SENTINEL:?}"\n'
    f"printf '%s\\n' '{executed}'\n"
)
fake_rustc.write_text("#!/bin/sh\nprintf 'host: x86_64-unknown-linux-gnu\\n'\n")
fixture.chmod(0o755)
fake_rustc.chmod(0o755)
PY

  set +e
  PATH="$fake_tools:$PATH" FLAPJACK_EXECUTION_SENTINEL="$execution_sentinel" \
    "$PACKAGE_HELPER" x86_64-unknown-linux-musl "$fixture" "$output_dir" \
    >"$stdout" 2>"$stderr"
  status=$?
  set -e

  [ -f "$execution_sentinel" ] \
    || die "x86_64 Linux musl artifact was not executed on its compatible GNU host"
  [ "$status" -ne 0 ] || die "CLI/embedded build-info mismatch was silently packaged"
  grep -Fq 'executed build-info JSON does not match embedded build-info JSON' "$stderr" \
    || die "CLI/embedded mismatch failed without the expected diagnostic"
  [ ! -e "$output_dir/flapjack-x86_64-unknown-linux-musl.manifest.json" ] \
    || die "CLI/embedded mismatch must not produce a manifest"
}
