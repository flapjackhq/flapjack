#!/usr/bin/env bash
# Live contract for the shipped repair-publication CLI against generated crash layouts.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"

BINARY_ARG=""
MANIFEST_ARG=""
ARTIFACT_ARG=""
BINARY_PATH=""
MANIFEST_PATH=""
ARTIFACT_DIR=""
TIMEOUT_BIN=""
HELPER=""
CHILD_PID=""
FLAPJACK_ENV_ARGS=()
SERVER_BIND_ADDR=""
SERVER_STDOUT_PATH=""
SERVER_STDERR_PATH=""

usage() {
  cat <<'EOF'
Usage:
  publication_repair_cli_live.sh --binary <absolute-path> --manifest <absolute-path> --artifact-dir <absolute-temp>

Options:
  --binary        Absolute path to the release flapjack executable to test.
  --manifest      Absolute path to publication_repair_cli_scenarios.json.
  --artifact-dir  Existing empty absolute artifact directory outside the checkout.
  --help          Show this help text.
EOF
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  local exit_code=$?
  local cleanup_failed=0
  stop_server || cleanup_failed=1
  if [ -n "$HELPER" ] && [ -f "$HELPER" ]; then
    rm -f "$HELPER"
  fi
  if [ "$exit_code" -eq 0 ] && [ "$cleanup_failed" -ne 0 ]; then
    exit_code=1
  fi
  exit "$exit_code"
}
trap cleanup EXIT

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --binary)
        [ -z "$BINARY_ARG" ] || die "--binary may only be provided once"
        [ "$#" -ge 2 ] || die "--binary requires a value"
        BINARY_ARG="$2"
        shift 2
        ;;
      --manifest)
        [ -z "$MANIFEST_ARG" ] || die "--manifest may only be provided once"
        [ "$#" -ge 2 ] || die "--manifest requires a value"
        MANIFEST_ARG="$2"
        shift 2
        ;;
      --artifact-dir)
        [ -z "$ARTIFACT_ARG" ] || die "--artifact-dir may only be provided once"
        [ "$#" -ge 2 ] || die "--artifact-dir requires a value"
        ARTIFACT_ARG="$2"
        shift 2
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        usage >&2
        die "unknown argument: $1"
        ;;
    esac
  done
  [ -n "$BINARY_ARG" ] || die "--binary is required"
  [ -n "$MANIFEST_ARG" ] || die "--manifest is required"
  [ -n "$ARTIFACT_ARG" ] || die "--artifact-dir is required"
}

require_tools() {
  local missing=0
  local tool
  for tool in bash cargo git python3 mktemp; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'ERROR: required tool not found: %s\n' "$tool" >&2
      missing=1
    fi
  done
  if command -v gtimeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v gtimeout)"
  elif command -v timeout >/dev/null 2>&1; then
    TIMEOUT_BIN="$(command -v timeout)"
  else
    printf 'ERROR: required tool not found: timeout or gtimeout\n' >&2
    missing=1
  fi
  [ "$missing" -eq 0 ] || exit 1
}

canonical_path() {
  python3 - "$1" <<'PY'
import pathlib
import sys
print(pathlib.Path(sys.argv[1]).resolve(strict=True))
PY
}

validate_paths() {
  [[ "$BINARY_ARG" = /* ]] || die "binary must be an absolute path"
  [[ "$MANIFEST_ARG" = /* ]] || die "manifest must be an absolute path"
  [[ "$ARTIFACT_ARG" = /* ]] || die "artifact directory must be an absolute path"

  BINARY_PATH="$(canonical_path "$BINARY_ARG")"
  MANIFEST_PATH="$(canonical_path "$MANIFEST_ARG")"
  ARTIFACT_DIR="$(canonical_path "$ARTIFACT_ARG")"

  [ -f "$BINARY_PATH" ] || die "binary must be a regular file: $BINARY_ARG"
  [ -x "$BINARY_PATH" ] || die "binary must be executable: $BINARY_ARG"
  [ -f "$MANIFEST_PATH" ] || die "manifest must be a regular file: $MANIFEST_ARG"
  [ -d "$ARTIFACT_DIR" ] || die "artifact directory must exist: $ARTIFACT_ARG"

  python3 - "$REPO_DIR" "$ARTIFACT_DIR" "$MANIFEST_PATH" <<'PY'
import json
import os
import pathlib
import re
import sys

repo = pathlib.Path(sys.argv[1]).resolve()
artifact = pathlib.Path(sys.argv[2]).resolve()
manifest_path = pathlib.Path(sys.argv[3])
home = pathlib.Path.home().resolve()
tmp = pathlib.Path("/tmp").resolve()
safe_path_component = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]*$")

def fail(message):
    raise SystemExit(message)

if artifact in (pathlib.Path("/").resolve(), home, tmp):
    fail(f"artifact directory is not an allowed root: {artifact}")
if os.path.commonpath([repo, artifact]) == str(repo):
    fail(f"artifact directory must be outside the checkout: {artifact}")
if any(artifact.iterdir()):
    fail(f"artifact directory must be empty: {artifact}")

with manifest_path.open(encoding="utf-8") as handle:
    manifest = json.load(handle)
scenarios = manifest.get("scenarios")
if not isinstance(scenarios, list):
    fail("manifest scenarios must be a list")
for index, scenario in enumerate(scenarios):
    if not isinstance(scenario, dict):
        fail(f"manifest scenario {index} must be an object")
    path_fields = {
        "id": scenario.get("id"),
        "tenant": scenario.get("tenant") or "products",
        "transaction": scenario.get("transaction") or "txn_001",
    }
    for field, value in path_fields.items():
        if not isinstance(value, str) or not safe_path_component.fullmatch(value):
            fail(
                f"manifest scenario {index} {field} must be a non-empty safe path component"
            )
PY
}

collect_flapjack_env() {
  local name
  FLAPJACK_ENV_ARGS=()
  while IFS='=' read -r name _; do
    case "$name" in
      FLAPJACK_*) FLAPJACK_ENV_ARGS+=("-u" "$name") ;;
    esac
  done < <(env)
}

run_bounded() {
  local status=0
  local restore_errexit=0
  [[ "$-" == *e* ]] && restore_errexit=1
  set +e
  "$TIMEOUT_BIN" --kill-after=30s "$@" &
  CHILD_PID=$!
  wait "$CHILD_PID"
  status=$?
  CHILD_PID=""
  [ "$restore_errexit" -eq 0 ] || set -e
  return "$status"
}

write_helper() {
  mkdir -p "$ARTIFACT_DIR/.runner"
  HELPER="$ARTIFACT_DIR/.runner/publication_repair_cli_live_helper.py"
  cat >"$HELPER" <<'PY'
import filecmp
import hashlib
import json
import os
import pathlib
import re
import shutil
import sys

REPORT_KEYS = {"tenant", "status", "action", "transaction_id", "phase", "evidence"}
RESIDUE_FIELDS = {
    "staging": lambda case, tenant, txn: case / ".publication" / tenant / txn / "staging",
    "backup": lambda case, tenant, txn: case / ".publication" / tenant / txn / "backup",
    "journal": lambda case, tenant, txn: case / ".publication" / tenant / txn / "journal.json",
    "quarantine": lambda case, tenant, txn: case / ".publication_quarantine" / tenant / txn,
}

def load_json(path):
    with pathlib.Path(path).open(encoding="utf-8") as handle:
        return json.load(handle)

def fail(message):
    raise SystemExit(message)

def scenario_id(scenario):
    return scenario["id"]

def target_for(layout, scenario):
    return layout.get("tenant") or scenario.get("tenant") or "products"

def transaction_for(layout, scenario):
    return layout.get("transaction") or scenario.get("transaction") or "txn_001"

def canonical_journal_missing_for_report(layout):
    return any(
        "rename:.publication/products/txn_001/journal.json.tmp->.publication/products/txn_001/journal.json|1" in boundary
        for boundary in layout.get("boundaries", [])
    )

def manifest_by_id(manifest):
    scenarios = manifest.get("scenarios")
    if not isinstance(scenarios, list):
        fail("manifest scenarios must be a list")
    by_id = {}
    for scenario in scenarios:
        ident = scenario_id(scenario)
        if ident in by_id:
            fail(f"duplicate manifest scenario id {ident}")
        by_id[ident] = scenario
    return by_id

def compare_layout_to_manifest(layout, scenario):
    ident = layout["scenario_id"]
    copied_fields = [
        "kind", "activation", "base", "mutation", "tenant", "transaction",
        "journal_phase", "policy_keys", "digests", "sidecars", "disposition",
        "cli", "visible", "residue",
    ]
    for field in copied_fields:
        expected = scenario.get(field, [] if field == "policy_keys" else None)
        if layout.get(field) != expected:
            fail(f"layout {ident} {field} mismatch")
    expected_boundary = scenario.get("boundary", {}).get("identity")
    if expected_boundary and expected_boundary not in layout.get("boundaries", []):
        fail(f"layout {ident} missing manifest boundary {expected_boundary}")

def validate_generated(manifest_path, generated_path, generated_root):
    manifest = load_json(manifest_path)
    generated = load_json(generated_path)
    root = pathlib.Path(generated_root)
    if manifest.get("schema_version") != 1:
        fail("manifest schema_version must be 1")
    by_id = manifest_by_id(manifest)
    if len(by_id) != manifest.get("layout_count"):
        fail(f"manifest layout_count {manifest.get('layout_count')} does not match scenario count {len(by_id)}")
    if not isinstance(generated, list):
        fail("generated layouts must be a list")
    if len(generated) != manifest.get("layout_count"):
        fail(f"generated layout count {len(generated)} does not match manifest layout_count {manifest.get('layout_count')}")

    generated_ids = set()
    boundary_counts = {}
    for layout in generated:
        ident = layout.get("scenario_id")
        if ident in generated_ids:
            fail(f"duplicate generated scenario id {ident}")
        generated_ids.add(ident)
        if ident not in by_id:
            fail(f"unknown generated scenario id {ident}")
        compare_layout_to_manifest(layout, by_id[ident])
        for boundary in layout.get("boundaries", []):
            boundary_counts[boundary] = boundary_counts.get(boundary, 0) + 1
    if generated_ids != set(by_id):
        fail(f"generated scenario IDs do not match manifest IDs")
    for scenario in by_id.values():
        boundary = scenario.get("boundary", {}).get("identity")
        if boundary and boundary_counts.get(boundary) != 1:
            fail(f"{scenario_id(scenario)} boundary {boundary} observed {boundary_counts.get(boundary, 0)} times")
    for ident in by_id:
        if not (root / ident).is_dir():
            fail(f"missing generated scenario directory {ident}")
    for child in root.iterdir():
        if child.name in {"generated_layouts.json", ".runner"}:
            continue
        if child.is_dir() and child.name not in by_id:
            fail(f"unknown generated scenario directory {child.name}")
    print("\n".join(sorted(by_id)))

def clone_case(generated_root, repair_root, ident):
    src = pathlib.Path(generated_root) / ident
    dst = pathlib.Path(repair_root) / ident
    if not src.is_dir():
        fail(f"missing generated scenario directory {ident}")
    if dst.exists():
        fail(f"duplicate repair scenario directory {ident}")
    shutil.copytree(src, dst, symlinks=True)

def target(manifest_path, generated_path, ident):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    print(target_for(generated[ident], scenario))

def snapshot(root):
    root = pathlib.Path(root)
    h = hashlib.sha256()
    for path in sorted(root.rglob("*")):
        rel = path.relative_to(root).as_posix()
        st = path.lstat()
        if path.is_symlink():
            h.update(f"L {rel} {os.readlink(path)}\n".encode())
        elif path.is_file():
            h.update(f"F {rel} {st.st_mode & 0o777} {st.st_size} ".encode())
            h.update(hashlib.sha256(path.read_bytes()).hexdigest().encode())
            h.update(b"\n")
        elif path.is_dir():
            h.update(f"D {rel} {st.st_mode & 0o777}\n".encode())
        else:
            fail(f"unsupported filesystem entry in snapshot: {path}")
    print(h.hexdigest())

def read_report(path, ident, label):
    raw = pathlib.Path(path).read_text(encoding="utf-8")
    try:
        report = json.loads(raw)
    except json.JSONDecodeError as error:
        fail(f"{ident} {label} CLI stdout is not valid JSON: {error}")
    if set(report) != REPORT_KEYS:
        fail(f"{ident} {label} report keys mismatch: {sorted(report)}")
    return report

def expected_report_identity(ident, layout, scenario, report, phase_override=None):
    txn = transaction_for(layout, scenario)
    phase = None if canonical_journal_missing_for_report(layout) else (
        phase_override or layout.get("journal_phase") or scenario.get("journal_phase")
    )
    evidence = f".publication/{target_for(layout, scenario)}/{txn}"
    if report["transaction_id"] is None:
        if report["phase"] is not None or report["evidence"] is not None:
            fail(f"{ident} null transaction report must also null phase and evidence")
        return
    if report["transaction_id"] != txn:
        fail(f"{ident} transaction_id {report['transaction_id']} does not match {txn}")
    if phase == "absent":
        if report["phase"] is not None:
            fail(f"{ident} phase {report['phase']} should be null for absent journal")
    elif phase is None:
        if report["phase"] is not None:
            fail(f"{ident} phase {report['phase']} should be null without canonical journal")
    elif phase is not None and report["phase"] != phase:
        fail(f"{ident} phase {report['phase']} does not match {phase}")
    if report["evidence"] != evidence:
        fail(f"{ident} evidence {report['evidence']} does not match {evidence}")

def assert_report(manifest_path, generated_path, ident, stdout_path, label):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    layout = generated[ident]
    report = read_report(stdout_path, ident, label)
    if report["tenant"] != target_for(layout, scenario):
        fail(f"{ident} report tenant {report['tenant']} does not match generated tenant")
    if report["status"] != scenario["cli"]["status"]:
        fail(f"{ident} status {report['status']} does not match manifest {scenario['cli']['status']}")
    if report["action"] != scenario["cli"]["action"]:
        fail(f"{ident} action {report['action']} does not match manifest {scenario['cli']['action']}")
    expected_report_identity(ident, layout, scenario, report)

def assert_clean_report(manifest_path, generated_path, ident, stdout_path):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    report = read_report(stdout_path, ident, "second")
    if report["tenant"] != target_for(generated[ident], scenario):
        fail(f"{ident} second report tenant {report['tenant']} does not match generated tenant")
    if report["status"] != "clean" or report["action"] != "none":
        fail(f"{ident} second report is not clean/none: {report}")
    expected_report_identity(
        ident,
        generated[ident],
        scenario,
        report,
        scenario.get("clean_report_phase"),
    )

def same_tree(left, right):
    left = pathlib.Path(left)
    right = pathlib.Path(right)
    comparison = filecmp.dircmp(left, right)
    if comparison.left_only or comparison.right_only or comparison.diff_files or comparison.funny_files:
        return False
    return all(same_tree(pathlib.Path(comparison.left) / child, pathlib.Path(comparison.right) / child)
               for child in comparison.common_dirs)

def assert_quarantine_file_evidence(ident, name, original, copied, tenant, txn):
    if name != "journal.json":
        if original.read_bytes() != copied.read_bytes():
            fail(f"{ident} quarantine evidence {name} does not match source bytes")
        return
    try:
        source_journal = json.loads(original.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        if original.read_bytes() != copied.read_bytes():
            fail(f"{ident} corrupt quarantine journal does not match source bytes")
        return
    copied_journal = load_json(copied)
    if copied_journal.get("target") != tenant or copied_journal.get("transaction_id") != txn:
        fail(f"{ident} quarantine journal identity mismatch")
    if source_journal.get("target") != copied_journal.get("target"):
        fail(f"{ident} quarantine journal changed target identity")
    if source_journal.get("transaction_id") != copied_journal.get("transaction_id"):
        fail(f"{ident} quarantine journal changed transaction identity")
    if copied_journal.get("phase") != "quarantined":
        fail(f"{ident} quarantine journal phase mismatch")

def assert_state(manifest_path, generated_path, generated_root, case_root, ident, after_first):
    manifest = load_json(manifest_path)
    generated = {layout["scenario_id"]: layout for layout in load_json(generated_path)}
    scenario = manifest_by_id(manifest)[ident]
    layout = generated[ident]
    case = pathlib.Path(case_root)
    source = pathlib.Path(generated_root) / ident
    tenant = target_for(layout, scenario)
    txn = transaction_for(layout, scenario)
    visible = scenario["visible"]
    residue = scenario["residue"]

    target_path = case / tenant
    if visible["target"] == "absent":
        if target_path.exists():
            fail(f"{ident} target should be absent")
    else:
        if not target_path.is_dir():
            fail(f"{ident} target should be present")
        meta = target_path / "index_meta.json"
        if visible["object"] != "absent" and meta.read_text(encoding="utf-8") != visible["object"]:
            fail(f"{ident} visible object mismatch")
    if visible["search"] == "loadable" and not target_path.exists():
        fail(f"{ident} search oracle requires a loadable target")
    if visible["search"] == "unavailable" and target_path.exists():
        fail(f"{ident} search oracle requires an unavailable target")

    for field, resolver in RESIDUE_FIELDS.items():
        path = resolver(case, tenant, txn)
        expected = residue[field]
        if expected == "present" and not path.exists():
            fail(f"{ident} residue {field} should be present")
        if expected == "absent" and path.exists():
            fail(f"{ident} residue {field} should be absent")

    if scenario["disposition"] == "quarantine":
        source_target = source / tenant
        if source_target.exists() and not same_tree(source_target, target_path):
            fail(f"{ident} quarantine changed live target")
        quarantine = RESIDUE_FIELDS["quarantine"](case, tenant, txn)
        if not quarantine.is_dir():
            fail(f"{ident} quarantine evidence directory missing")
        allowed = {"journal.json", "journal.json.tmp", "staging", "backup"}
        observed = {child.name for child in quarantine.iterdir()}
        if not observed or not observed.issubset(allowed):
            fail(f"{ident} quarantine copied unexpected evidence: {sorted(observed)}")
        for name in observed:
            original_name = "journal.json.tmp" if name == "journal.json.tmp" else name
            original = case / ".publication" / tenant / txn / original_name
            if original.exists() and original.is_dir() and not same_tree(original, quarantine / name):
                fail(f"{ident} quarantine evidence {name} does not match source bytes")
            if original.exists() and original.is_file():
                assert_quarantine_file_evidence(ident, name, original, quarantine / name, tenant, txn)
    elif after_first and scenario["cli"]["exit_code"] != 0:
        if not same_tree(source, case):
            fail(f"{ident} nonzero repair mutated generated evidence")

def assert_equal_json(left_path, right_path, ident):
    if read_report(left_path, ident, "first") != read_report(right_path, ident, "second"):
        fail(f"{ident} clean first report changed on second run")

def strip_ansi(text):
    result = []
    i = 0
    while i < len(text):
        if text[i] != "\x1b":
            result.append(text[i])
            i += 1
            continue
        i += 1
        if i >= len(text) or text[i] != "[":
            continue
        i += 1
        while i < len(text) and text[i] not in "@ABCDEFGHIJKLMNOPQRSTUVWXYZ[\\]^_`abcdefghijklmnopqrstuvwxyz{|}~":
            i += 1
        if i < len(text):
            i += 1
    return "".join(result)

def extract_bind_addr_from_banner(path):
    local_banner = re.compile(r"^\s*(?:[^\w\s]+\s+)?Local:\s+http://127\.0\.0\.1:(\d+)\s*$")
    for raw_line in pathlib.Path(path).read_text(encoding="utf-8", errors="replace").splitlines():
        line = strip_ansi(raw_line)
        match = local_banner.match(line)
        if match:
            return f"127.0.0.1:{match.group(1)}"
    return None

def probe_health(bind_addr):
    import http.client
    connection = http.client.HTTPConnection(bind_addr, timeout=0.2)
    try:
        connection.request("GET", "/health")
        response = connection.getresponse()
        body = response.read().decode("utf-8", errors="replace")
    finally:
        connection.close()
    if response.status != 200:
        raise SystemExit(f"unexpected /health status {response.status}")
    payload = json.loads(body)
    if payload.get("status") != "ok":
        raise SystemExit(f"/health payload missing status=ok: {payload}")

def main(argv):
    command = argv[1]
    if command == "validate-generated":
        validate_generated(*argv[2:])
    elif command == "clone-case":
        clone_case(*argv[2:])
    elif command == "target":
        target(*argv[2:])
    elif command == "snapshot":
        snapshot(*argv[2:])
    elif command == "assert-report":
        assert_report(*argv[2:])
    elif command == "assert-clean-report":
        assert_clean_report(*argv[2:])
    elif command == "assert-state":
        assert_state(*argv[2:])
    elif command == "assert-equal-json":
        assert_equal_json(*argv[2:])
    elif command == "startup-bind-addr":
        bind_addr = extract_bind_addr_from_banner(argv[2])
        if bind_addr is None:
            raise SystemExit(1)
        print(bind_addr)
    elif command == "probe-health":
        probe_health(argv[2])
    else:
        fail(f"unknown helper command {command}")

if __name__ == "__main__":
    main(sys.argv)
PY
}

assert_build_info_json() {
  local path="$1"
  local revision="$2"
  python3 - "$path" "$revision" <<'PY'
import json
import sys

path, revision = sys.argv[1], sys.argv[2]
with open(path, encoding="utf-8") as handle:
    value = json.load(handle)

def fail(message):
    raise SystemExit(f"{path}: {message}: {value}")

if value.get("schemaVersion") != 1:
    fail("schemaVersion must be 1")
if value.get("revision") != revision:
    fail("revision must match reviewed HEAD")
if value.get("revisionKnown") is not True:
    fail("revisionKnown must be true")
dirty = value.get("dirty")
dirty_known = value.get("dirtyKnown")
if dirty is True:
    fail("dirty must not be true")
if dirty is None:
    if dirty_known is not False:
        fail("dirtyKnown must be false when dirty is null")
elif dirty is False:
    if dirty_known is not True:
        fail("dirtyKnown must be true when dirty is false")
else:
    fail("dirty must be a boolean or null")
features = value.get("features")
if not isinstance(features, list) or features != sorted(features):
    fail("features must be sorted")
capabilities = value.get("capabilities")
if set(capabilities or {}) != {"vectorSearch", "vectorSearchLocal"}:
    fail("capabilities must contain only vectorSearch and vectorSearchLocal")
if not all(isinstance(capabilities[key], bool) for key in capabilities):
    fail("capabilities values must be booleans")
serialized = json.dumps(value, sort_keys=True, separators=(",", ":"))
for forbidden in ("algolia_migration_v1", "algoliaMigrationV1"):
    if forbidden in serialized:
        fail(f"serialized payload must not contain {forbidden}")
PY
}

identity_gate() {
  local revision=""
  local status=""
  local build_info="$ARTIFACT_DIR/.runner/build_info.json"
  revision="$(git -C "$REPO_DIR" rev-parse HEAD)"
  [[ "$revision" =~ ^[0-9a-f]{40}$ ]] || die "reviewed revision must be a 40-character lowercase SHA"
  status="$(git -C "$REPO_DIR" status --short)"
  [ -z "$status" ] || die "checkout must be clean before live publication repair contract"
  collect_flapjack_env
  run_bounded 60s env "${FLAPJACK_ENV_ARGS[@]}" "$BINARY_PATH" build-info --json >"$build_info"
  assert_build_info_json "$build_info" "$revision"
}

run_generator() {
  local generated_dir="$ARTIFACT_DIR/generated"
  mkdir -p "$generated_dir"
  (
    cd "$ENGINE_DIR"
    collect_flapjack_env
    run_bounded 600s env "${FLAPJACK_ENV_ARGS[@]}" \
      PUBLICATION_REPAIR_CLI_MANIFEST="$MANIFEST_PATH" \
      PUBLICATION_REPAIR_CLI_ARTIFACT_DIR="$generated_dir" \
      cargo test -p flapjack --lib publication_repair_cli -- --ignored
  )
  [ -f "$generated_dir/generated_layouts.json" ] || die "generator did not write generated_layouts.json"
  python3 "$HELPER" validate-generated "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$generated_dir" \
    >"$ARTIFACT_DIR/.runner/scenario_ids.txt"
}

invoke_cli() {
  local case_root="$1"
  local target="$2"
  local stdout_path="$3"
  local stderr_path="$4"
  collect_flapjack_env
  run_bounded 120s env "${FLAPJACK_ENV_ARGS[@]}" \
    "$BINARY_PATH" --data-dir "$case_root" repair-publication --tenant "$target" --json \
    >"$stdout_path" 2>"$stderr_path"
}

server_output_path() {
  local ident="$1"
  local label="$2"
  printf '%s\n' "$ARTIFACT_DIR/.runner/${ident}.${label}.server"
}

wait_for_startup_bind_addr() {
  local ident="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))
  local bind_addr=""

  while :; do
    if bind_addr="$(python3 "$HELPER" startup-bind-addr "$SERVER_STDOUT_PATH" 2>/dev/null)"; then
      SERVER_BIND_ADDR="$bind_addr"
      return 0
    fi
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
      local status=0
      set +e
      wait "$CHILD_PID"
      status=$?
      set -e
      CHILD_PID=""
      die "$ident server exited before startup banner (status $status); stdout: $SERVER_STDOUT_PATH stderr: $SERVER_STDERR_PATH"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      local stdout_path="$SERVER_STDOUT_PATH"
      local stderr_path="$SERVER_STDERR_PATH"
      stop_server
      die "$ident timed out waiting for startup banner; stdout: $stdout_path stderr: $stderr_path"
    fi
    sleep 0.1
  done
}

wait_for_health() {
  local ident="$1"
  local bind_addr="$2"
  local timeout_secs="$3"
  local deadline=$((SECONDS + timeout_secs))

  while :; do
    if python3 "$HELPER" probe-health "$bind_addr" >/dev/null 2>&1; then
      return 0
    fi
    if [ -n "$CHILD_PID" ] && ! kill -0 "$CHILD_PID" 2>/dev/null; then
      local status=0
      set +e
      wait "$CHILD_PID"
      status=$?
      set -e
      CHILD_PID=""
      die "$ident server exited before /health became ready (status $status); stdout: $SERVER_STDOUT_PATH stderr: $SERVER_STDERR_PATH"
    fi
    if [ "$SECONDS" -ge "$deadline" ]; then
      local stdout_path="$SERVER_STDOUT_PATH"
      local stderr_path="$SERVER_STDERR_PATH"
      stop_server
      die "$ident /health probe timed out; stdout: $stdout_path stderr: $stderr_path"
    fi
    sleep 0.05
  done
}

start_server() {
  local case_root="$1"
  local ident="$2"
  local label="$3"

  SERVER_STDOUT_PATH="$(server_output_path "$ident" "$label").stdout"
  SERVER_STDERR_PATH="$(server_output_path "$ident" "$label").stderr"
  : >"$SERVER_STDOUT_PATH"
  : >"$SERVER_STDERR_PATH"

  collect_flapjack_env
  python3 -c 'import os, sys; os.setsid(); os.execvp(sys.argv[1], sys.argv[1:])' \
    env "${FLAPJACK_ENV_ARGS[@]}" \
    "$BINARY_PATH" --data-dir "$case_root" --auto-port --no-auth \
    >"$SERVER_STDOUT_PATH" 2>"$SERVER_STDERR_PATH" &
  CHILD_PID=$!

  wait_for_startup_bind_addr "$ident" 5
  wait_for_health "$ident" "$SERVER_BIND_ADDR" 5
}

server_process_group_alive() {
  local pid="$1"
  kill -0 -- "-$pid" 2>/dev/null || kill -0 "$pid" 2>/dev/null
}

send_server_signal() {
  local signal="$1"
  local pid="$2"
  kill "-$signal" -- "-$pid" 2>/dev/null || kill "-$signal" "$pid" 2>/dev/null || true
}

wait_for_server_process_group_exit() {
  local pid="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))

  while server_process_group_alive "$pid"; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.1
  done
}

# Reap the recorded direct child without blocking past the bounded shutdown
# budget. A plain `wait "$pid"` blocks until the child exits, so a child that
# has not yet died (e.g. delayed KILL delivery or an uninterruptible state)
# would stall shutdown indefinitely before the final process-group check runs.
# Poll the child's liveness instead; once it is gone the child is a reapable
# zombie and `wait` returns its status immediately. If the child is still alive
# after the budget, give up so the caller fails closed rather than blocking.
reap_child_within_budget() {
  local pid="$1"
  local timeout_secs="$2"
  local deadline=$((SECONDS + timeout_secs))

  while kill -0 "$pid" 2>/dev/null; do
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 0.05
  done

  wait "$pid" >/dev/null 2>&1 || true
  return 0
}

clear_server_state() {
  CHILD_PID=""
  SERVER_BIND_ADDR=""
  SERVER_STDOUT_PATH=""
  SERVER_STDERR_PATH=""
}

stop_server() {
  local pid="${CHILD_PID:-}"

  if [ -z "$pid" ]; then
    clear_server_state
    return 0
  fi

  if server_process_group_alive "$pid"; then
    send_server_signal TERM "$pid"
    if ! wait_for_server_process_group_exit "$pid" 1; then
      send_server_signal KILL "$pid"
      if ! reap_child_within_budget "$pid" 1; then
        printf 'ERROR: server child could not be reaped within the bounded shutdown budget after KILL (pid %s)\n' "$pid" >&2
        return 1
      fi
      if ! wait_for_server_process_group_exit "$pid" 1; then
        printf 'ERROR: server process group remained alive after KILL (pid %s)\n' "$pid" >&2
        return 1
      fi
    else
      reap_child_within_budget "$pid" 1 || true
    fi
  fi

  clear_server_state
}

run_server_lifecycle() {
  local case_root="$1"
  local ident="$2"
  local label="$3"

  start_server "$case_root" "$ident" "$label"
  stop_server
}

assert_exit_code() {
  local actual="$1"
  local expected="$2"
  local ident="$3"
  [ "$actual" -eq "$expected" ] || die "$ident CLI exit code $actual does not match manifest $expected"
}

manifest_exit_code() {
  python3 - "$MANIFEST_PATH" "$1" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for scenario in manifest["scenarios"]:
    if scenario["id"] == sys.argv[2]:
        print(scenario["cli"]["exit_code"])
        break
else:
    raise SystemExit(f"unknown scenario {sys.argv[2]}")
PY
}

manifest_disposition() {
  python3 - "$MANIFEST_PATH" "$1" <<'PY'
import json
import sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
for scenario in manifest["scenarios"]:
    if scenario["id"] == sys.argv[2]:
        print(scenario["disposition"])
        break
else:
    raise SystemExit(f"unknown scenario {sys.argv[2]}")
PY
}

run_case() {
  local ident="$1"
  local generated_dir="$ARTIFACT_DIR/generated"
  local repair_root="$ARTIFACT_DIR/repair"
  local case_root="$repair_root/$ident"
  local target=""
  local expected_exit=""
  local first_status=""
  local second_status=""
  local first_stdout="$ARTIFACT_DIR/.runner/${ident}.first.stdout.json"
  local first_stderr="$ARTIFACT_DIR/.runner/${ident}.first.stderr"
  local second_stdout="$ARTIFACT_DIR/.runner/${ident}.second.stdout.json"
  local second_stderr="$ARTIFACT_DIR/.runner/${ident}.second.stderr"
  local post_first=""
  local post_second=""
  local disposition=""

  python3 "$HELPER" clone-case "$generated_dir" "$repair_root" "$ident"
  target="$(python3 "$HELPER" target "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident")"
  expected_exit="$(manifest_exit_code "$ident")"
  disposition="$(manifest_disposition "$ident")"

  set +e
  invoke_cli "$case_root" "$target" "$first_stdout" "$first_stderr"
  first_status=$?
  set -e
  assert_exit_code "$first_status" "$expected_exit" "$ident"
  python3 "$HELPER" assert-report "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident" "$first_stdout" first
  python3 "$HELPER" assert-state "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$generated_dir" "$case_root" "$ident" true
  run_server_lifecycle "$case_root" "$ident" first
  post_first="$(python3 "$HELPER" snapshot "$case_root")"

  if [ "$disposition" = "commit" ] || [ "$disposition" = "rollback" ] || [ "$disposition" = "absent-create" ]; then
    set +e
    invoke_cli "$case_root" "$target" "$second_stdout" "$second_stderr"
    second_status=$?
    set -e
    assert_exit_code "$second_status" 0 "$ident second"
    python3 "$HELPER" assert-clean-report "$MANIFEST_PATH" "$generated_dir/generated_layouts.json" "$ident" "$second_stdout"
    run_server_lifecycle "$case_root" "$ident" second
    post_second="$(python3 "$HELPER" snapshot "$case_root")"
    [ "$post_first" = "$post_second" ] || die "$ident idempotence snapshot changed"
    if [ "$(cat "$first_stdout")" = "$(cat "$second_stdout")" ]; then
      python3 "$HELPER" assert-equal-json "$first_stdout" "$second_stdout" "$ident"
    fi
  else
    [ "$expected_exit" -ne 0 ] || die "$ident unclassified disposition has zero exit"
  fi
}

run_contract() {
  local ident=""
  mkdir -p "$ARTIFACT_DIR/repair"
  while IFS= read -r ident; do
    [ -n "$ident" ] || continue
    run_case "$ident"
  done <"$ARTIFACT_DIR/.runner/scenario_ids.txt"
  printf 'PASS: publication repair CLI live contract passed\n'
}

main() {
  parse_args "$@"
  require_tools
  validate_paths
  write_helper
  identity_gate
  run_generator
  run_contract
}

main "$@"
