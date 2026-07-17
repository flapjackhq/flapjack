#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DRIVER="$SCRIPT_DIR/algolia_source_export_live.sh"

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
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2"
  fi
}

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT

run_driver() {
  local out="$1"
  shift
  set +e
  bash "$DRIVER" "$@" >"$out" 2>&1
  local rc=$?
  set -e
  printf '%s' "$rc"
}

write_stub_runtime() {
  local runtime="$1"
  mkdir -p "$runtime/bin" "$runtime/state"
  cat >"$runtime/fake-flapjack" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$FLAPJACK_DATA_DIR" >"$ALGOLIA_LIVE_TEST_STUB_DIR/data_dir"
printf '%s\n' "$$" >"$ALGOLIA_LIVE_TEST_STUB_DIR/server_pid"
printf 'Local: http://127.0.0.1:54321\n'
while :; do sleep 1; done
SH
  chmod +x "$runtime/fake-flapjack"
  cat >"$runtime/bin/curl" <<'PY'
#!/usr/bin/env python3
import hashlib
import json
import os
import signal
import sys
import time
import urllib.parse
from pathlib import Path

state = Path(os.environ["ALGOLIA_LIVE_TEST_STUB_DIR"])
mode = os.environ.get("ALGOLIA_LIVE_TEST_STUB_MODE", "success")
state.mkdir(parents=True, exist_ok=True)

def append(name, value):
    with (state / name).open("a", encoding="utf-8") as f:
        f.write(value + "\n")

def read_lines(name):
    path = state / name
    if not path.exists():
        return []
    return [line.strip() for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]

def respond(payload, code=200):
    if isinstance(payload, str):
        sys.stdout.write(payload)
    else:
        sys.stdout.write(json.dumps(payload, separators=(",", ":")))
    sys.stdout.write("\n" + str(code))

def parse_args(argv):
    method = "GET"
    data = ""
    url = ""
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "-X":
            method = argv[i + 1]
            i += 2
        elif arg == "--data":
            data = argv[i + 1]
            i += 2
        elif arg in ("-w", "-H"):
            i += 2
        elif arg.startswith("http://") or arg.startswith("https://"):
            url = arg
            i += 1
        else:
            i += 1
    return method, data, url

def decode_index(path):
    parts = path.split("/")
    if "indexes" not in parts:
        return ""
    idx = parts[parts.index("indexes") + 1]
    return urllib.parse.unquote(idx)

def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()

def item_hash(value):
    return hashlib.sha256(canonical(value)).hexdigest()

def aggregate(items):
    h = hashlib.sha256()
    for item in sorted(items, key=lambda it: it["objectID"]):
        h.update(item["objectID"].encode())
        h.update(b"\0")
        h.update(item_hash(item).encode())
        h.update(b"\n")
    return h.hexdigest()

def fixture_docs():
    return [
        {
            "objectID": f"doc-{i:04d}",
            "title": f"Stage 4 product {i:04d}",
            "category": "even" if i % 2 == 0 else "odd",
            "secret_note": f"hidden-{i:04d}",
        }
        for i in range(1005)
    ]

def fixture_rules():
    return [
        {
            "objectID": f"rule-{i}",
            "conditions": [{"pattern": f"product {i}", "anchoring": "contains"}],
            "consequence": {"params": {"filters": f"category:{'even' if i % 2 == 0 else 'odd'}"}},
        }
        for i in range(1, 4)
    ]

def fixture_synonyms():
    return [
        {"objectID": "syn-1", "type": "synonym", "synonyms": ["sneaker", "trainer"]},
        {"objectID": "syn-2", "type": "oneWaySynonym", "input": "tee", "synonyms": ["tshirt", "t-shirt"]},
        {"objectID": "syn-3", "type": "altCorrection1", "word": "flapjack", "corrections": ["flapjacks"]},
    ]

def data_dir():
    return Path((state / "data_dir").read_text(encoding="utf-8").strip())

def drift_barrier_dir():
    raw = os.environ.get("FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_BARRIER_DIR", "")
    return Path(raw) if raw else state / "missing-drift-barrier"

def create_accepted_spool():
    docs = fixture_docs()
    rules = fixture_rules()
    synonyms = fixture_synonyms()
    job = data_dir() / "migration_exports" / "jobs" / "accepted-job"
    job.mkdir(parents=True, exist_ok=True)
    (job / "documents-0.bin").write_text(json.dumps(docs, separators=(",", ":"), sort_keys=True), encoding="utf-8")
    (job / "rules-0.bin").write_text(json.dumps(rules, separators=(",", ":"), sort_keys=True), encoding="utf-8")
    (job / "synonyms-0.bin").write_text(json.dumps(synonyms, separators=(",", ":"), sort_keys=True), encoding="utf-8")
    manifest = {
        "lifecycle": "Accepted",
        "artifacts": [
            {"state": "Visible", "kind": "DocumentPage", "final_path": "documents-0.bin"},
            {"state": "Visible", "kind": "RulesPage", "final_path": "rules-0.bin"},
            {"state": "Visible", "kind": "SynonymsPage", "final_path": "synonyms-0.bin"},
        ],
        "resource_completions": {
            "documents": {"hash": aggregate(docs)},
            "rules": {"hash": aggregate(rules)},
            "synonyms": {"hash": aggregate(synonyms)},
        },
        "completed_objects": {"count": len(docs)},
        "completed_rules": {"count": len(rules)},
        "completed_synonyms": {"count": len(synonyms)},
    }
    (job / "manifest.json").write_text(json.dumps(manifest, separators=(",", ":"), sort_keys=True), encoding="utf-8")

def create_failed_spool():
    job = data_dir() / "migration_exports" / "jobs" / "failed-job"
    job.mkdir(parents=True, exist_ok=True)
    (job / "documents-0.bin").write_text("[]", encoding="utf-8")
    manifest = {
        "lifecycle": "Failed",
        "artifacts": [
            {"state": "Visible", "kind": "DocumentPage", "final_path": "documents-0.bin"},
        ],
        "resource_completions": {},
    }
    (job / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

if "-sf" in sys.argv[1:]:
    sys.exit(0)

method, body, url = parse_args(sys.argv[1:])
parsed = urllib.parse.urlparse(url)
path = parsed.path
query = urllib.parse.parse_qs(parsed.query)
append("curl_calls.log", f"{method} {path}?{parsed.query}")

if parsed.netloc.startswith("127.0.0.1:"):
    if method == "GET" and path == "/1/indexes":
        items = []
        if mode == "destination_leak" and (state / "destination_leaked").exists():
            items.append({"name": "unexpected_destination"})
        respond({"items": items})
    elif method == "POST" and path == "/1/migrate-from-algolia":
        payload = json.loads(body)
        source = payload["sourceIndex"]
        key = payload["apiKey"]
        if key == "stub-permitted-secret-canary":
            append("migration_key_roles.txt", "permitted")
        elif key == "stub-denied-secret-canary":
            append("migration_key_roles.txt", "denied")
        else:
            append("migration_key_roles.txt", "unexpected")
        if source.endswith("_drift"):
            (state / "drift_request_started").write_text("1", encoding="utf-8")
            time.sleep(0.25)
            create_failed_spool()
            barrier = drift_barrier_dir()
            observed = barrier / "observed"
            release = barrier / "release"
            barrier.mkdir(parents=True, exist_ok=True)
            observed.write_text("failed-job", encoding="utf-8")
            for _ in range(100):
                if release.exists():
                    respond({"message": "Algolia source changed during export", "status": 502}, 502)
                    break
                time.sleep(0.05)
            else:
                respond({"status": "complete", "settings": True, "objects": {"imported": 2500}, "rules": {"imported": 0}, "synonyms": {"imported": 0}, "taskID": 0})
        elif "denied" in key:
            respond({"message": "Algolia key cannot export unretrievable attributes", "status": 400}, 400)
        else:
            create_accepted_spool()
            if mode == "destination_leak":
                (state / "destination_leaked").write_text("1", encoding="utf-8")
            respond({"status": "complete", "settings": True, "objects": {"imported": 1005}, "rules": {"imported": 3}, "synonyms": {"imported": 3}, "taskID": 0})
    else:
        respond({"message": "unexpected local request", "path": path}, 500)
    sys.exit(0)

if method == "DELETE" and path.startswith("/1/keys/"):
    key = urllib.parse.unquote(path.rsplit("/", 1)[-1])
    append("deleted_keys.txt", key)
    respond({"deleted": True})
elif method == "GET" and path.startswith("/1/keys/"):
    key = urllib.parse.unquote(path.rsplit("/", 1)[-1])
    if key in read_lines("deleted_keys.txt"):
        respond({"message": "not found"}, 404)
    else:
        respond({"key": "<redacted>"})
elif method == "POST" and path == "/1/keys":
    count = len(read_lines("created_keys.txt"))
    key = "stub-permitted-secret-canary" if count == 0 else "stub-denied-secret-canary"
    append("created_keys.txt", key)
    if count == 1:
        (state / "setup_complete").write_text("1", encoding="utf-8")
    respond({"key": key})
elif method == "GET" and "/task/" in path:
    respond({"status": "published"})
elif method == "DELETE" and path.startswith("/1/indexes/"):
    index = decode_index(path)
    append("deleted_indexes.txt", index)
    respond({"taskID": 9000})
elif method == "POST" and path.endswith("/rules/search"):
    page = json.loads(body).get("page", 0)
    ids = ["rule-1", "rule-2", "rule-3"]
    respond({"hits": [{"objectID": ids[page]}] if page < len(ids) else [], "nbPages": len(ids)})
elif method == "POST" and path.endswith("/synonyms/search"):
    page = json.loads(body).get("page", 0)
    ids = ["syn-1", "syn-2", "syn-3"]
    respond({"hits": [{"objectID": ids[page]}] if page < len(ids) else [], "nbHits": len(ids)})
elif method == "GET" and path == "/1/indexes":
    seeded = read_lines("created_indexes.txt")
    deleted = set(read_lines("deleted_indexes.txt"))
    if mode == "fail_pagination" and "hitsPerPage" in query and query.get("hitsPerPage") == ["1"] and not (state / "failed_once").exists():
        (state / "failed_once").write_text("1", encoding="utf-8")
        respond({"message": "stub pagination failure"}, 500)
    elif mode in ("self_int", "self_term") and "hitsPerPage" in query and query.get("hitsPerPage") == ["1"] and not (state / "sent_signal").exists():
        (state / "sent_signal").write_text(mode, encoding="utf-8")
        driver_pid = int((state / "driver_pid").read_text(encoding="utf-8").strip())
        os.kill(driver_pid, signal.SIGINT if mode == "self_int" else signal.SIGTERM)
        sys.exit(130 if mode == "self_int" else 143)
    else:
        visible = [idx for idx in seeded if idx not in deleted]
        if query.get("hitsPerPage") == ["1"]:
            page = int(query.get("page", ["0"])[0])
            items = [{"name": visible[page]}] if page < len(visible) else []
            respond({"items": items, "nbPages": len(visible)})
        elif mode == "cleanup_residue_later_page" and query.get("hitsPerPage") == ["1000"]:
            page = int(query.get("page", ["0"])[0])
            items = [{"name": seeded[0]}] if page == 1 else []
            respond({"items": items, "nbPages": 2})
        else:
            respond({"items": [{"name": idx} for idx in visible], "nbPages": 1})
elif method == "POST" and path.endswith("_drift/batch") and any(
    request.get("action") == "partialUpdateObject"
    for request in json.loads(body).get("requests", [])
):
    expected_artifact = str(data_dir() / "migration_exports" / "jobs" / "failed-job" / "documents-0.bin")
    observation = state / "drift_barrier_observed"
    if not observation.exists() or observation.read_text(encoding="utf-8").strip() != expected_artifact:
        (state / "drift_mutation_before_barrier").write_text("1", encoding="utf-8")
        respond({"message": "mutation arrived before the driver read the registered drift artifact"}, 409)
    else:
        (state / "drift_mutation_after_barrier").write_text("1", encoding="utf-8")
        respond({"taskID": 9999})
elif method in ("PUT", "POST") and path.startswith("/1/indexes/"):
    index = decode_index(path)
    if index and index not in read_lines("created_indexes.txt"):
        append("created_indexes.txt", index)
    respond({"taskID": 1000 + len(read_lines("created_indexes.txt"))})
else:
    respond({"message": "unexpected request", "method": method, "path": path}, 500)
PY
  chmod +x "$runtime/bin/curl"
}

make_secret_fixture() {
  local env_file="$1"
  printf 'ALGOLIA_APP_ID=APPID123\nALGOLIA_ADMIN_KEY=ADMIN_SECRET_CANARY\n' >"$env_file"
}

run_driver_with_stub() {
  local mode="$1" out="$2" runtime="$3"
  make_secret_fixture "$runtime/secret.env"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    ALGOLIA_LIVE_TEST_STUB_DIR="$runtime/state" \
    ALGOLIA_LIVE_TEST_STUB_MODE="$mode" \
    ALGOLIA_LIVE_TEST_BARRIER_OBSERVATION_FILE="$runtime/state/drift_barrier_observed" \
    ALGOLIA_LIVE_TEST_DRIVER_PID_FILE="$runtime/state/driver_pid" \
    bash "$DRIVER" --secret-file "$runtime/secret.env" >"$out" 2>&1
  local rc=$?
  set -e
  printf '%s' "$rc"
}

extract_evidence_path() {
  sed -n 's/^INFO: preserved sanitized live evidence at //p' "$1" | tail -1
}

same_lines_unordered() {
  local expected="$1" actual="$2"
  [ -f "$expected" ] \
    && [ -f "$actual" ] \
    && cmp -s <(LC_ALL=C sort "$expected") <(LC_ALL=C sort "$actual")
}

exact_owned_cleanup_completed() {
  local runtime="$1"
  same_lines_unordered "$runtime/state/created_indexes.txt" "$runtime/state/deleted_indexes.txt" \
    && same_lines_unordered "$runtime/state/created_keys.txt" "$runtime/state/deleted_keys.txt"
}

json_output_has_checks() {
  local out="$1"
  sed '/^INFO: /,$d' "$out" \
    | jq -e '[.expected_observed[].name] | index("vendor_setup") and index("vendor_pagination") and index("migration_acl_and_spool") and index("drift_refusal")' >/dev/null
}

evidence_is_private_and_sanitized() {
  local evidence="$1" secret_path="$2" mode
  [[ "$evidence" =~ ^/tmp/flapjack_algolia_source_export_live_evidence_[0-9]+_[0-9]+$ ]] || return 1
  [ -d "$evidence/logs" ] || return 1
  [ -f "$evidence/receipt.json" ] || return 1
  mode="$(stat -f '%Lp' "$evidence" 2>/dev/null || stat -c '%a' "$evidence" 2>/dev/null)"
  [ "$mode" = "700" ] || return 1
  ! grep -R -F \
    -e 'ADMIN_SECRET_CANARY' \
    -e 'stub-permitted-secret-canary' \
    -e 'stub-denied-secret-canary' \
    -e "$secret_path" \
    "$evidence" >/dev/null 2>&1
}

assert_stubbed_success_cleanup() {
  local runtime out rc data_dir evidence expected_observation
  runtime="$WORK_DIR/stub-success"
  out="$runtime.out"
  write_stub_runtime "$runtime"
  rc="$(run_driver_with_stub success "$out" "$runtime")"
  data_dir="$(cat "$runtime/state/data_dir")"
  evidence="$(extract_evidence_path "$out")"
  expected_observation="$data_dir/migration_exports/jobs/failed-job/documents-0.bin"
  if [ "$rc" = "0" ] \
    && json_output_has_checks "$out" \
    && evidence_is_private_and_sanitized "$evidence" "$runtime/secret.env" \
    && [ -d "$evidence/migration_exports/jobs" ] \
    && [ ! -e "$data_dir" ] \
    && exact_owned_cleanup_completed "$runtime" \
    && [ -f "$runtime/state/migration_key_roles.txt" ] \
    && cmp -s <(printf 'permitted\ndenied\npermitted\n') "$runtime/state/migration_key_roles.txt" \
    && [ -f "$runtime/state/drift_mutation_after_barrier" ] \
    && [ "$(cat "$runtime/state/drift_barrier_observed" 2>/dev/null)" = "$expected_observation" ] \
    && [ ! -e "$runtime/state/drift_mutation_before_barrier" ] \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|stub-(permitted|denied)-secret-canary' "$out"; then
    rm -rf "$evidence"
    pass 'stubbed success uses exact permitted/denied key sequence, orders drift mutation, and cleans owned resources'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'stubbed success uses exact permitted/denied key sequence, orders drift mutation, and cleans owned resources' "rc=$rc output=$(cat "$out")"
  fi
}

assert_stubbed_failure_preserves_evidence_before_cleanup() {
  local runtime out rc evidence data_dir
  runtime="$WORK_DIR/stub-failure"
  out="$runtime.out"
  write_stub_runtime "$runtime"
  rc="$(run_driver_with_stub fail_pagination "$out" "$runtime")"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir")"
  if [ "$rc" != "0" ] \
    && [ -n "$evidence" ] \
    && evidence_is_private_and_sanitized "$evidence" "$runtime/secret.env" \
    && jq -e '.created_indexes | length == 4' "$evidence/receipt.json" >/dev/null \
    && [ ! -e "$data_dir" ] \
    && exact_owned_cleanup_completed "$runtime" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|stub-(permitted|denied)-secret-canary' "$out"; then
    rm -rf "$evidence"
    pass 'stubbed failure preserves evidence before deleting exact owned resources'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'stubbed failure preserves evidence before deleting exact owned resources' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_stubbed_destination_leak_fails_closed() {
  local runtime out rc evidence data_dir
  runtime="$WORK_DIR/stub-destination-leak"
  out="$runtime.out"
  write_stub_runtime "$runtime"
  rc="$(run_driver_with_stub destination_leak "$out" "$runtime")"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir")"
  if [ "$rc" != "0" ] \
    && grep -Fq 'Flapjack index membership changed during source-only export' "$out" \
    && evidence_is_private_and_sanitized "$evidence" "$runtime/secret.env" \
    && [ ! -e "$data_dir" ] \
    && exact_owned_cleanup_completed "$runtime"; then
    rm -rf "$evidence"
    pass 'stubbed destination creation fails the source-only isolation contract'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'stubbed destination creation fails the source-only isolation contract' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_stubbed_later_page_residue_fails_cleanup() {
  local runtime out rc remaining_index evidence data_dir
  runtime="$WORK_DIR/stub-cleanup-residue"
  out="$runtime.out"
  write_stub_runtime "$runtime"
  rc="$(run_driver_with_stub cleanup_residue_later_page "$out" "$runtime")"
  remaining_index="$(head -1 "$runtime/state/created_indexes.txt")"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir")"
  if [ "$rc" != "0" ] \
    && grep -Fq 'remaining Algolia indexes' "$out" \
    && grep -Fq "$remaining_index" "$out" \
    && evidence_is_private_and_sanitized "$evidence" "$runtime/secret.env" \
    && [ -d "$evidence/migration_exports/jobs" ] \
    && jq -e '[.checks[].name] | index("cleanup_precheck")' "$evidence/receipt.json" >/dev/null \
    && [ ! -e "$data_dir" ] \
    && grep -Fq 'Retry with: bash engine/tests/algolia_source_export_live.sh --secret-file <secret-file-with-ALGOLIA_APP_ID-and-ALGOLIA_ADMIN_KEY>' "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|stub-(permitted|denied)-secret-canary' "$out"; then
    rm -rf "$evidence"
    pass 'stubbed cleanup preserves evidence and fails on later-page vendor residue'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'stubbed cleanup preserves evidence and fails on later-page vendor residue' "rc=$rc remaining=$remaining_index evidence=$evidence output=$(cat "$out")"
  fi
}

assert_stubbed_signal_cleanup() {
  local runtime_int runtime_term out_int out_term rc_int rc_term evidence_int evidence_term data_dir_int data_dir_term
  runtime_int="$WORK_DIR/stub-int"
  runtime_term="$WORK_DIR/stub-term"
  out_int="$runtime_int.out"
  out_term="$runtime_term.out"
  write_stub_runtime "$runtime_int"
  write_stub_runtime "$runtime_term"
  rc_int="$(run_driver_with_stub self_int "$out_int" "$runtime_int")"
  rc_term="$(run_driver_with_stub self_term "$out_term" "$runtime_term")"
  evidence_int="$(extract_evidence_path "$out_int")"
  evidence_term="$(extract_evidence_path "$out_term")"
  data_dir_int="$(cat "$runtime_int/state/data_dir")"
  data_dir_term="$(cat "$runtime_term/state/data_dir")"
  if [ "$rc_int" = "130" ] \
    && [ "$rc_term" = "143" ] \
    && evidence_is_private_and_sanitized "$evidence_int" "$runtime_int/secret.env" \
    && evidence_is_private_and_sanitized "$evidence_term" "$runtime_term/secret.env" \
    && [ ! -e "$data_dir_int" ] \
    && [ ! -e "$data_dir_term" ] \
    && exact_owned_cleanup_completed "$runtime_int" \
    && exact_owned_cleanup_completed "$runtime_term"; then
    rm -rf "$evidence_int" "$evidence_term"
    pass 'stubbed INT and TERM runs preserve evidence and clean exact owned resources'
  else
    [ -z "$evidence_int" ] || rm -rf "$evidence_int"
    [ -z "$evidence_term" ] || rm -rf "$evidence_term"
    fail 'stubbed INT and TERM runs preserve evidence and clean exact owned resources' "INT rc=$rc_int output=$(cat "$out_int"); TERM rc=$rc_term output=$(cat "$out_term")"
  fi
}

assert_usage_requires_secret_file() {
  local out rc
  out="$WORK_DIR/usage.out"
  rc="$(run_driver "$out")"
  if [ "$rc" = "2" ] && grep -Fq 'Usage:' "$out"; then
    pass 'driver requires exactly --secret-file <path>'
  else
    fail 'driver requires exactly --secret-file <path>' "rc=$rc output=$(cat "$out")"
  fi
}

assert_missing_secret_is_sanitized() {
  local out rc missing secret_path
  out="$WORK_DIR/missing.out"
  secret_path="$WORK_DIR/path-with-secret-name.env"
  missing="$secret_path"
  rc="$(run_driver "$out" --secret-file "$missing")"
  if [ "$rc" != "0" ] \
    && grep -Fq 'required Algolia credentials could not be loaded' "$out" \
    && ! grep -Fq "$missing" "$out"; then
    pass 'missing secret file failure is generic and path-sanitized'
  else
    fail 'missing secret file failure is generic and path-sanitized' "rc=$rc output=$(cat "$out")"
  fi
}

assert_missing_key_is_sanitized() {
  local env_file out rc canary
  env_file="$WORK_DIR/missing-key.env"
  out="$WORK_DIR/missing-key.out"
  canary="ALGOLIA_ADMIN_KEY_SHOULD_NOT_PRINT"
  printf 'ALGOLIA_APP_ID=APPID123\nUNRELATED=%s\n' "$canary" >"$env_file"
  rc="$(run_driver "$out" --secret-file "$env_file")"
  if [ "$rc" != "0" ] \
    && grep -Fq 'required Algolia credentials could not be loaded' "$out" \
    && ! grep -Fq "$canary" "$out" \
    && ! grep -Fq "$env_file" "$out"; then
    pass 'missing required key failure is generic and secret-sanitized'
  else
    fail 'missing required key failure is generic and secret-sanitized' "rc=$rc output=$(cat "$out")"
  fi
}

assert_sources_only_required_loader() {
  local source_count
  source_count="$(grep -Fc 'source "$SECRET_HELPER"' "$DRIVER" || true)"
  if [ "$source_count" = "1" ] \
    && grep -Fq 'load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY' "$DRIVER" \
    && ! grep -Eq 'load_named_secrets[^\n]*(AWS_|GITHUB_|FLAPJACK_)' "$DRIVER"; then
    pass 'driver sources only load_named_secrets and requests exact Algolia keys'
  else
    fail 'driver sources only load_named_secrets and requests exact Algolia keys'
  fi
}

assert_server_lifecycle_contract() {
  if grep -Fq 'FLAPJACK_DATA_DIR="$DATA_DIR"' "$DRIVER" \
    && grep -Fq '"$BIN_PATH" --auto-port' "$DRIVER" \
    && grep -Fq '"$WAIT_HELPER" --pid "$SERVER_PID" --host 127.0.0.1 --port auto' "$DRIVER" \
    && grep -Fq 'kill "$SERVER_PID"' "$DRIVER" \
    && grep -Fq 'FLAPJACK_BIN' "$DRIVER"; then
    pass 'driver owns a narrow local-server lifecycle with exact PID cleanup'
  else
    fail 'driver owns a narrow local-server lifecycle with exact PID cleanup'
  fi
}

assert_cleanup_and_evidence_ordering() {
  local cleanup_line evidence_line vendor_line
  cleanup_line="$(grep -n '^cleanup()' "$DRIVER" | cut -d: -f1 | head -1)"
  evidence_line="$(awk -v start="$cleanup_line" 'NR>=start && /preserve_run_evidence/ {print NR; exit}' "$DRIVER")"
  vendor_line="$(awk -v start="$cleanup_line" 'NR>=start && /cleanup_vendor/ {print NR; exit}' "$DRIVER")"
  if [ -n "$evidence_line" ] && [ -n "$vendor_line" ] && [ "$evidence_line" -lt "$vendor_line" ] \
    && grep -Fq 'trap cleanup EXIT' "$DRIVER" \
    && grep -Fq "trap 'INTERRUPTED_EXIT_CODE=130; exit 130' INT" "$DRIVER" \
    && grep -Fq "trap 'INTERRUPTED_EXIT_CODE=143; exit 143' TERM" "$DRIVER" \
    && grep -Fq 'preserve_run_evidence 1' "$DRIVER"; then
    pass 'driver preserves evidence before cleanup and handles EXIT/INT/TERM'
  else
    fail 'driver preserves evidence before cleanup and handles EXIT/INT/TERM'
  fi
}

assert_owned_resource_cleanup_contract() {
  if grep -Fq 'created_indexes.txt' "$DRIVER" \
    && grep -Fq 'created_keys.txt' "$DRIVER" \
    && grep -Fq 'remaining Algolia restricted key fingerprint' "$DRIVER" \
    && grep -Fq 'Retry with: bash engine/tests/algolia_source_export_live.sh --secret-file <secret-file-with-ALGOLIA_APP_ID-and-ALGOLIA_ADMIN_KEY>' "$DRIVER" \
    && ! grep -Fq 'remaining Algolia restricted key: $key' "$DRIVER"; then
    pass 'driver deletes only recorded owned resources and redacts key residue'
  else
    fail 'driver deletes only recorded owned resources and redacts key residue'
  fi
}

assert_contract_arms_present() {
  if grep -Fq 'collect_paginated_ids_or_die index indexes "" "$index_ids" 1' "$DRIVER" \
    && grep -Fq 'collect_paginated_ids indexes "" "$remaining" 1000' "$DRIVER" \
    && grep -Fq '.objects.imported == 1005' "$DRIVER" \
    && grep -Fq 'Algolia key cannot export unretrievable attributes' "$DRIVER" \
    && grep -Fq 'drift artifact barrier was not established' "$DRIVER" \
    && grep -Fq 'artifact barrier plus awaited mutation refused export' "$DRIVER"; then
    pass 'driver contains pagination, ACL, spool, destination-isolation, and drift contract arms'
  else
    fail 'driver contains pagination, ACL, spool, destination-isolation, and drift contract arms'
  fi
}

assert_no_subcommand_bypasses_normal_usage() {
  local out rc
  out="$WORK_DIR/subcommand.out"
  rc="$(run_driver "$out" __unknown_internal)"
  if [ "$rc" = "2" ] \
    && grep -Fq 'Usage:' "$out" \
    && ! grep -Eq '(^|[[:space:]])__algolia_request|(^|[[:space:]])__test_' "$DRIVER"; then
    pass 'normal entrypoint rejects non-secret invocations and exposes no internal subcommands'
  else
    fail 'normal entrypoint rejects non-secret invocations and exposes no internal subcommands' "rc=$rc output=$(cat "$out")"
  fi
}

main() {
  echo 'algolia_source_export_live closed driver contract test'
  [ -f "$DRIVER" ] && pass 'live driver exists' || fail 'live driver exists'
  [ -x "$DRIVER" ] && pass 'live driver is executable' || fail 'live driver is executable'
  assert_usage_requires_secret_file
  assert_missing_secret_is_sanitized
  assert_missing_key_is_sanitized
  assert_sources_only_required_loader
  assert_server_lifecycle_contract
  assert_cleanup_and_evidence_ordering
  assert_owned_resource_cleanup_contract
  assert_contract_arms_present
  assert_no_subcommand_bypasses_normal_usage
  assert_stubbed_success_cleanup
  assert_stubbed_failure_preserves_evidence_before_cleanup
  assert_stubbed_destination_leak_fails_closed
  assert_stubbed_later_page_residue_fails_cleanup
  assert_stubbed_signal_cleanup

  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '%d test(s) failed\n' "$TESTS_FAILED"
    return 1
  fi
  echo 'All tests passed'
}

main "$@"
