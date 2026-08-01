#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
DEFAULT_FIXTURE_DIR="$SCRIPT_DIR/fixtures/2026_07_26_m0a_meilisearch_source_contract"
FIXTURE_DIR="$DEFAULT_FIXTURE_DIR"; STUB_RESPONSE_DIR=""; MODE=""; EXPECTED=""
TRACE_FILE=""; TEMP_DIR=""; CONTAINER_NAME=""
CONTAINER_CLEANUP_ARMED=0; LIVE_CLEANED=0
FAILURE_EVIDENCE_DIR="/tmp/flapjack_stage2_meilisearch_source_contract_failure_$$"
BASE_URL=""; MASTER_KEY=""
TASK_POLL_LIMIT=120
SECRET_VALUES=()
RESTRICTED_KEY=""
EXPECTED_WARNING_IDENTIFIERS='["meili_primary_key_ambiguous_candidates","meili_document_order_not_contractual","meili_search_pagination_bound_not_document_export_bound","meili_setting_value_normalized","meili_trailing_slash_redirect_unknown"]'
EXPECTED_RESTRICTED_KEY_ACTION_PROBES='[{"action":"indexes.get","method":"GET","path":"/indexes?limit=10"},{"action":"documents.get","method":"POST","path":"/indexes/configured_pk/documents/fetch","body":{"offset":0,"limit":1}},{"action":"settings.get","method":"GET","path":"/indexes/configured_pk/settings"},{"action":"tasks.get","method":"GET","path":"/tasks?limit=1"},{"action":"version","method":"GET","path":"/version"},{"action":"stats.get","method":"GET","path":"/stats"},{"action":"search","method":"POST","path":"/indexes/configured_pk/search","body":{"q":"rake"}},{"action":"dumps.create","method":"POST","path":"/dumps","body":{}},{"action":"snapshots.create","method":"POST","path":"/snapshots","body":{}}]'

usage() {
  cat <<'EOF'
Usage:
  bash tests/meilisearch_source_contract_kat.sh --stub-response-dir DIR [--fixture-dir DIR]
  bash tests/meilisearch_source_contract_kat.sh --live [--fixture-dir DIR]
  bash tests/meilisearch_source_contract_kat.sh --preview-live [--fixture-dir DIR]

Modes:
  --stub-response-dir DIR
      Run the fast deterministic contract against JSON HTTP responses.

  --live
      Run the positive control against the pinned Meilisearch v1.50.0 image.
      Refuses the performance lease, exact-name collisions, non-loopback binds,
      and preserves credential-free failure evidence before cleanup.

  --preview-live
      Run the positive control and then invoke the ignored production-route
      preview probe while the seeded source is still live. Cleanup remains
      owned by this script.

The live command is:
  cd engine && bash tests/meilisearch_source_contract_kat.sh --live
EOF
}

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

parse_args() {
  while [[ "$#" -gt 0 ]]; do
    case "$1" in
      --stub-response-dir)
        [[ "$#" -ge 2 ]] || die "--stub-response-dir requires a directory"
        [[ -z "$MODE" ]] || die "choose exactly one mode"
        MODE=stub
        STUB_RESPONSE_DIR="$2"
        shift 2
        ;;
      --live)
        [[ -z "$MODE" ]] || die "choose exactly one mode"
        MODE=live
        shift
        ;;
      --preview-live)
        [[ -z "$MODE" ]] || die "choose exactly one mode"
        MODE=preview_live
        shift
        ;;
      --fixture-dir)
        [[ "$#" -ge 2 ]] || die "--fixture-dir requires a directory"
        FIXTURE_DIR="$2"
        shift 2
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      *)
        die "unknown argument: $1"
        ;;
    esac
  done
  [[ -n "$MODE" ]] || die "choose --live or --stub-response-dir"
}

require_tools_and_fixture() {
  local file
  for file in \
    expected_bundle.json \
    configured_primary_key_documents.json \
    configured_primary_key_settings.json \
    inferred_primary_key_documents.json \
    ambiguous_primary_key_documents.json \
    restricted_key_action_probes.json; do
    [[ -f "$FIXTURE_DIR/$file" ]] || die "fixture file missing: $file"
    jq -e . "$FIXTURE_DIR/$file" >/dev/null \
      || die "fixture file is not valid JSON: $file"
  done
  EXPECTED="$FIXTURE_DIR/expected_bundle.json"
  TASK_POLL_LIMIT="$(jq -er '.tasks.taskPollLimit' "$EXPECTED")"
  [[ "$TASK_POLL_LIMIT" =~ ^[1-9][0-9]*$ ]] || die "task poll limit must be positive"
  if [[ "$MODE" == stub ]]; then
    [[ -d "$STUB_RESPONSE_DIR" ]] || die "stub response directory missing"
    TRACE_FILE="$STUB_RESPONSE_DIR/request_trace.jsonl"
    : >"$TRACE_FILE"
  else
    command -v docker >/dev/null || die "docker is required for --live"
    command -v curl >/dev/null || die "curl is required for --live"
    command -v lsof >/dev/null || die "lsof is required for --live"
    if [[ "$MODE" == preview_live ]]; then
      command -v cargo >/dev/null || die "cargo is required for --preview-live"
      command -v timeout >/dev/null || die "timeout is required for --preview-live"
    fi
  fi
}

validate_oracle_structure() {
  jq -e '
    (.source.image |
      startswith("getmeili/meilisearch@sha256:")) and
    (.source.imageDigest | test("^sha256:[0-9a-f]{64}$")) and
    (.source.image == ("getmeili/meilisearch@" + .source.imageDigest)) and
    (.source.version.pkgVersion | type == "string") and
    (.expectedPrimaryKeys | keys | length == 3) and
    (.documents.stableIds | length > 0) and
    (.tasks.terminalStatuses | sort == ["canceled","failed","succeeded"]) and
    (.requiredActions | length == 9) and
    (.warningIdentifiers | length == 5) and
    (.cleanup.residueExpected | all(.[]; . == false))
  ' "$EXPECTED" >/dev/null || die "source image digest mismatch or expected bundle is incomplete"
  [[ "$(jq -r '.pagination.documentExportMethod' "$EXPECTED")" == POST ]] \
    || die "document export must use POST fetch"
  [[ "$(jq -r '.pagination.documentExportPath' "$EXPECTED")" \
    == /indexes/configured_pk/documents/fetch ]] \
    || die "document export must use POST fetch"
  jq -e '
    (.pagination.offsets | length) == (.pagination.pageCounts | length) and
    (.pagination.offsets | all(.[]; type == "number" and . >= 0)) and
    (.pagination.limit | type == "number" and . > 0)
  ' "$EXPECTED" >/dev/null || die "pagination oracle is incomplete"

  local index_primary_keys
  index_primary_keys="$(jq -cS '
    . as $bundle |
    {
      ($bundle.indexes.configured.uid): $bundle.indexes.configured.primaryKey,
      ($bundle.indexes.inferred.uid): $bundle.indexes.inferred.primaryKey,
      ($bundle.indexes.ambiguous.uid): $bundle.indexes.ambiguous.primaryKey
    }
  ' "$EXPECTED")"
  [[ "$index_primary_keys" == "$(jq -cS '.expectedPrimaryKeys' "$EXPECTED")" ]] \
    || die "primary key oracle mismatch"

  local expected_actions probe_actions expected_probe_contract actual_probe_contract
  expected_actions="$(jq -c '.requiredActions | sort' "$EXPECTED")"
  probe_actions="$(jq -c '[.probes[].action] | sort' \
    "$FIXTURE_DIR/restricted_key_action_probes.json")"
  [[ "$probe_actions" == "$expected_actions" ]] \
    || die "required read actions mismatch"
  jq -e '([.probes[].action] | length) == ([.probes[].action] | unique | length)' \
    "$FIXTURE_DIR/restricted_key_action_probes.json" >/dev/null \
    || die "required read actions mismatch"
  expected_probe_contract="$(jq -cS . <<<"$EXPECTED_RESTRICTED_KEY_ACTION_PROBES")"
  actual_probe_contract="$(jq -cS '.probes' \
    "$FIXTURE_DIR/restricted_key_action_probes.json")"
  [[ "$actual_probe_contract" == "$expected_probe_contract" ]] \
    || die "restricted key action probe contract mismatch"
  [[ "$(jq -cS '.warningIdentifiers' "$EXPECTED")" \
    == "$(jq -cS . <<<"$EXPECTED_WARNING_IDENTIFIERS")" ]] \
    || die "warning identifier contract mismatch"
}

redact_and_reject_credentials() {
  local payload="$1" secret
  for secret in "${SECRET_VALUES[@]-}" "${MEILI_TEST_SECRET_CANARY:-}"; do
    [[ -z "$secret" ]] && continue
    [[ "$payload" != *"$secret"* ]] || die "credential leakage detected"
  done
}

record_trace() {
  local label="$1" method="$2" path="$3"
  jq -cn --arg label "$label" --arg method "$method" --arg path "$path" \
    '{label:$label,method:$method,path:$path,jsonParsed:true}' >>"$TRACE_FILE"
}

stub_response_envelope() {
  local label="$1" method="$2" path="$3"
  local status_file="$STUB_RESPONSE_DIR/${label}.status"
  local body_file="$STUB_RESPONSE_DIR/${label}.json" status payload
  [[ -f "$status_file" && -f "$body_file" ]] \
    || die "stub response missing for $label"
  status="$(tr -d '[:space:]' <"$status_file")"
  if ! payload="$(jq -c . "$body_file" 2>/dev/null)"; then
    die "response $label is not valid JSON"
  fi
  redact_and_reject_credentials "$payload"
  record_trace "$label" "$method" "$path"
  jq -cn --argjson status "$status" --argjson body "$payload" \
    '{status:$status,body:$body}'
}

live_response_envelope() {
  local label="$1" method="$2" path="$3" body="$4" key="$5"
  local response_file="$TEMP_DIR/http_response.json" status payload sanitized
  local curl_args=(-sS -o "$response_file" -w '%{http_code}' -X "$method")
  curl_args+=(-H 'Content-Type: application/json')
  [[ -z "$key" ]] || curl_args+=(-H "Authorization: Bearer ${key}")
  [[ -z "$body" ]] || curl_args+=(--data-binary "$body")
  status="$(curl "${curl_args[@]}" "${BASE_URL}${path}")" \
    || die "request $label failed"
  if ! payload="$(jq -c . "$response_file" 2>/dev/null)"; then
    die "response $label is not valid JSON"
  fi
  redact_and_reject_credentials "$payload"
  sanitized="$payload"
  if [[ "$label" == restricted_key_create ]]; then
    sanitized="$(jq -c '.key = "<redacted>"' <<<"$payload")"
  fi
  printf '%s\n' "$sanitized" >"$TEMP_DIR/last_response.json"
  printf '%s\n' "$label" >"$TEMP_DIR/last_response_label.txt"
  record_trace "$label" "$method" "$path"
  jq -cn --argjson status "$status" --argjson body "$payload" \
    '{status:$status,body:$body}'
}

response_envelope() {
  local label="$1" method="$2" path="$3" body="${4:-}" key="${5:-$MASTER_KEY}"
  if [[ "$MODE" == stub ]]; then
    stub_response_envelope "$label" "$method" "$path"
  else
    live_response_envelope "$label" "$method" "$path" "$body" "$key"
  fi
}

request_json() {
  local label="$1" method="$2" path="$3" body="${4:-}" key="${5:-$MASTER_KEY}"
  local envelope status
  envelope="$(response_envelope "$label" "$method" "$path" "$body" "$key")" \
    || return $?
  status="$(jq -r '.status' <<<"$envelope")"
  [[ "$status" -ge 200 && "$status" -le 299 ]] \
    || die "request $label returned HTTP $status"
  jq -c '.body' <<<"$envelope"
}

request_forbidden() {
  local label="$1" method="$2" path="$3" body="$4" key="$5"
  local envelope
  envelope="$(response_envelope "$label" "$method" "$path" "$body" "$key")" \
    || return $?
  jq -e '
    .status == 403 and
    .body.code == "invalid_api_key" and
    .body.type == "auth"
  ' <<<"$envelope" >/dev/null || die "required action subtraction was not rejected"
}

poll_task() {
  local label_prefix="$1" uid="$2" attempt response status
  for ((attempt = 0; attempt < TASK_POLL_LIMIT; attempt++)); do
    response="$(request_json "${label_prefix}_poll_${attempt}" GET "/tasks/${uid}")" \
      || return $?
    status="$(jq -er '.status' <<<"$response")" || die "task response missing status"
    case "$status" in
      succeeded|failed|canceled)
        jq -c . <<<"$response"
        return 0
        ;;
      enqueued|processing)
        [[ "$MODE" == stub ]] || sleep 0.25
        ;;
      *)
        die "unexpected task status: $status"
        ;;
    esac
  done
  die "task polling exceeded bound for uid $uid"
}

submit_and_poll_task() {
  local label="$1" method="$2" path="$3" body="$4"
  local submitted uid
  submitted="$(request_json "${label}_submit" "$method" "$path" "$body")" \
    || return $?
  uid="$(jq -er '.taskUid' <<<"$submitted")" || die "$label response missing taskUid"
  poll_task "$label" "$uid"
}

assert_json_equal() {
  local actual="$1" expected="$2" failure="$3"
  [[ "$(jq -cS . <<<"$actual")" == "$(jq -cS . <<<"$expected")" ]] \
    || die "$failure"
}

assert_expected_fields_equal() {
  local actual="$1" expected="$2" failure="$3"
  jq -en --argjson actual "$actual" --argjson expected "$expected" '
    def expected_fields_equal($actual; $expected):
      if ($expected | type) == "object" then
        all(
          $expected | keys[];
          . as $key |
          ($actual | has($key)) and
          expected_fields_equal($actual[$key]; $expected[$key])
        )
      else
        $actual == $expected
      end;
    expected_fields_equal($actual; $expected)
  ' >/dev/null || die "$failure"
}

canonical_document_hash() {
  jq -cS 'sort_by(.sku)' | shasum -a 256 | awk '{print $1}'
}

assert_terminal_tasks() {
  local tasks="$1"
  jq -e '(.results | type == "array") and (.results | length > 0)' \
    <<<"$tasks" >/dev/null || die "task evidence missing"
  jq -e --argjson terminal "$(jq -c '.tasks.terminalStatuses' "$EXPECTED")" '
    all(.results[]; .status as $status | $terminal | index($status) != null)
  ' <<<"$tasks" >/dev/null || die "nonterminal task status"
}

assert_primary_key() {
  local indexes="$1" index_role="$2" failure="$3" uid expected
  uid="$(jq -r ".indexes.${index_role}.uid" "$EXPECTED")"
  expected="$(jq -c --arg uid "$uid" '.expectedPrimaryKeys[$uid]' "$EXPECTED")"
  jq -e --arg uid "$uid" --argjson expected "$expected" '
    [.results[] | select(.uid == $uid).primaryKey] == [$expected]
  ' <<<"$indexes" >/dev/null || die "$failure"
}

validate_primary_keys() {
  local indexes="$1"
  assert_primary_key "$indexes" configured "configured primary key mismatch"
  assert_primary_key "$indexes" inferred "inferred primary key mismatch"
  assert_primary_key "$indexes" ambiguous "ambiguous primary key metadata mismatch"
}

pagination_request_body() {
  local page_index="$1"
  jq -c --argjson page_index "$page_index" '{
    offset: .pagination.offsets[$page_index],
    limit: .pagination.limit
  }' "$EXPECTED"
}

assert_pagination_request() {
  local page="$1" page_index="$2"
  jq -e \
    --argjson offset "$(jq ".pagination.offsets[$page_index]" "$EXPECTED")" \
    --argjson limit "$(jq '.pagination.limit' "$EXPECTED")" '
    .offset == $offset and .limit == $limit
  ' <<<"$page" >/dev/null || die "pagination request mismatch"
}

validate_pagination_and_documents() {
  local page0="$1" page1="$2" expected_counts actual_counts
  local documents ids unique_ids expected_ids hash expected_hash
  expected_counts="$(jq -c '.pagination.pageCounts' "$EXPECTED")"
  actual_counts="$(jq -cn --argjson p0 "$page0" --argjson p1 "$page1" \
    '[$p0.results|length,$p1.results|length]')"
  [[ "$actual_counts" == "$expected_counts" ]] || die "page count mismatch"
  assert_pagination_request "$page0" 0
  assert_pagination_request "$page1" 1
  jq -e --argjson total "$(jq '.pagination.total' "$EXPECTED")" '
    .total == $total
  ' <<<"$page0" >/dev/null || die "pagination total mismatch"
  jq -e --argjson total "$(jq '.pagination.total' "$EXPECTED")" '
    .total == $total
  ' <<<"$page1" >/dev/null || die "pagination total mismatch"

  documents="$(jq -cn --argjson p0 "$page0" --argjson p1 "$page1" \
    '$p0.results + $p1.results')"
  ids="$(jq -c '[.[].sku] | sort' <<<"$documents")"
  unique_ids="$(jq -c '[.[].sku] | unique | sort' <<<"$documents")"
  [[ "$ids" == "$unique_ids" ]] || die "duplicate stable ID"
  expected_ids="$(jq -c '.documents.stableIds | sort' "$EXPECTED")"
  [[ "$ids" == "$expected_ids" ]] || die "stable ID set mismatch"
  hash="$(canonical_document_hash <<<"$documents")"
  expected_hash="$(jq -r '.documents.hashBefore' "$EXPECTED")"
  [[ "$hash" == "$expected_hash" ]] || die "document hash before mutation mismatch"
  printf '%s\n' "$ids"
}

source_capture_snapshot() {
  local suffix="$1" index stats tasks
  index="$(request_json "configured_index_${suffix}" GET /indexes/configured_pk)" \
    || return $?
  stats="$(request_json "stats_${suffix}" GET /stats)" || return $?
  tasks="$(request_json "tasks_${suffix}" GET \
    '/tasks?indexUids=configured_pk&limit=100')" || return $?
  assert_terminal_tasks "$tasks"
  jq -cn --argjson index "$index" --argjson stats "$stats" --argjson tasks "$tasks" \
    '{index:$index,stats:$stats,tasks:$tasks}'
}

validate_pre_mutation_stats() {
  local snapshot="$1"
  jq -e --argjson count "$(jq '.documents.countBefore' "$EXPECTED")" \
    --argjson distribution "$(jq '.documents.fieldDistributionBefore' "$EXPECTED")" \
    --argjson size "$(jq '.documents.databaseSizeBefore' "$EXPECTED")" '
    .stats.databaseSize == $size and
    .stats.indexes.configured_pk.numberOfDocuments == $count and
    .stats.indexes.configured_pk.fieldDistribution == $distribution
  ' <<<"$snapshot" >/dev/null || die "pre-mutation source markers mismatch"
}

validate_controlled_mutation() {
  local before_snapshot="$1" task after_index after_page after_stats after_tasks
  local documents hash configured_uid configured_primary_key
  if [[ "$MODE" != stub ]]; then
    task="$(submit_and_poll_task mutation POST \
      /indexes/configured_pk/documents \
      "[$(jq -c '.documents.mutation' "$EXPECTED")]")" || return $?
  else
    task="$(poll_task mutation_task \
      "$(jq -r '.tasks.mutation.uid' "$EXPECTED")")" || return $?
  fi
  jq -e --argjson uid "$(jq '.tasks.mutation.uid' "$EXPECTED")" \
    --arg status "$(jq -r '.tasks.mutation.status' "$EXPECTED")" \
    --arg type "$(jq -r '.tasks.mutation.type' "$EXPECTED")" '
    .uid == $uid and .status == $status and .type == $type
  ' <<<"$task" >/dev/null || die "controlled mutation task mismatch"

  after_index="$(request_json configured_index_after GET /indexes/configured_pk)" \
    || return $?
  after_page="$(request_json configured_after_page_0 POST \
    /indexes/configured_pk/documents/fetch '{"offset":0,"limit":10}')" || return $?
  after_stats="$(request_json stats_after GET /stats)" || return $?
  after_tasks="$(request_json tasks_after GET \
    '/tasks?indexUids=configured_pk&limit=100')" || return $?
  assert_terminal_tasks "$after_tasks"
  configured_uid="$(jq -r '.indexes.configured.uid' "$EXPECTED")"
  configured_primary_key="$(jq -r \
    --arg uid "$configured_uid" '.expectedPrimaryKeys[$uid]' "$EXPECTED")"
  jq -e --arg before_updated "$(jq -r '.index.updatedAt' <<<"$before_snapshot")" \
    --arg uid "$configured_uid" --arg primary_key "$configured_primary_key" '
    .uid == $uid and .primaryKey == $primary_key and
    .updatedAt != $before_updated
  ' <<<"$after_index" >/dev/null || die "controlled mutation markers mismatch"
  jq -e --argjson count "$(jq '.documents.countAfter' "$EXPECTED")" \
    --argjson distribution "$(jq '.documents.fieldDistributionAfter' "$EXPECTED")" \
    --argjson size "$(jq '.documents.databaseSizeAfter' "$EXPECTED")" '
    .databaseSize == $size and
    .indexes.configured_pk.numberOfDocuments == $count and
    .indexes.configured_pk.fieldDistribution == $distribution
  ' <<<"$after_stats" >/dev/null || die "post-mutation source markers mismatch"
  documents="$(jq -c '.results' <<<"$after_page")"
  hash="$(canonical_document_hash <<<"$documents")"
  [[ "$hash" == "$(jq -r '.documents.hashAfter' "$EXPECTED")" ]] \
    || die "document hash after mutation mismatch"
}

probe_label_for_action() {
  tr '.-' '__' <<<"$1" | sed 's/^/restricted_/'
}

create_restricted_key() {
  local actions="$1" response key
  response="$(request_json restricted_key_create POST /keys \
    "$(jq -cn --argjson actions "$actions" \
      '{name:"Stage 2 KAT",description:"ephemeral loopback contract",actions:$actions,indexes:["*"],expiresAt:null}')")" \
    || return $?
  key="$(jq -er '.key' <<<"$response")" || die "restricted key response missing key"
  SECRET_VALUES+=("$key")
  RESTRICTED_KEY="$key"
}

validate_restricted_task() {
  local action="$1" response="$2" task_name task
  case "$action" in
    dumps.create) task_name=dump ;;
    snapshots.create) task_name=snapshot ;;
    *) return 0 ;;
  esac
  task="$(poll_task "${task_name}_task" \
    "$(jq -er '.taskUid' <<<"$response")")" || return $?
  jq -e \
    --argjson uid "$(jq ".tasks.${task_name}.uid" "$EXPECTED")" \
    --arg status "$(jq -r ".tasks.${task_name}.status" "$EXPECTED")" \
    --arg type "$(jq -r ".tasks.${task_name}.type" "$EXPECTED")" '
    .uid == $uid and .status == $status and .type == $type
  ' <<<"$task" >/dev/null || die "${task_name} task mismatch"
}

validate_restricted_actions() {
  local expected_actions full_key=stub-key probe action method path body label response
  expected_actions="$(jq -c '.requiredActions' "$EXPECTED")"
  if [[ "$MODE" != stub ]]; then
    create_restricted_key "$expected_actions"
    full_key="$RESTRICTED_KEY"
  fi
  while IFS= read -r probe; do
    action="$(jq -r '.action' <<<"$probe")"
    method="$(jq -r '.method' <<<"$probe")"
    path="$(jq -r '.path' <<<"$probe")"
    body="$(jq -c '.body // empty' <<<"$probe")"
    label="$(probe_label_for_action "$action")"
    response="$(request_json "$label" "$method" "$path" "$body" "$full_key")" \
      || return $?
    validate_restricted_task "$action" "$response"
  done < <(jq -c '.probes[]' "$FIXTURE_DIR/restricted_key_action_probes.json")

  local denied_actions denied_key
  while IFS= read -r probe; do
    action="$(jq -r '.action' <<<"$probe")"
    method="$(jq -r '.method' <<<"$probe")"
    path="$(jq -r '.path' <<<"$probe")"
    body="$(jq -c '.body // empty' <<<"$probe")"
    label="$(probe_label_for_action "$action")"
    denied_key=stub-denied-key
    if [[ "$MODE" != stub ]]; then
      denied_actions="$(jq -cn --argjson actions "$expected_actions" \
        --arg action "$action" '$actions - [$action]')"
      create_restricted_key "$denied_actions" || return $?
      denied_key="$RESTRICTED_KEY"
    fi
    request_forbidden "denied_${label}" "$method" "$path" "$body" "$denied_key"
  done < <(jq -c '.probes[]' "$FIXTURE_DIR/restricted_key_action_probes.json")
}

live_performance_lease_is_active() { [[ -e /tmp/jul26_local_performance_lease ]]; }

docker_safety_gate() {
  ! live_performance_lease_is_active || die "live performance lease is active"
  colima status >/dev/null 2>&1 || die "Colima is not running"
  CONTAINER_NAME="$(jq -r '.cleanup.containerName' "$EXPECTED")"
  local host port temp_relative inventory probe_output probe_status
  temp_relative="$(jq -r '.cleanup.tempDir' "$EXPECTED")"
  TEMP_DIR="$ENGINE_DIR/$temp_relative"
  host="$(jq -r '.cleanup.loopbackHost' "$EXPECTED")"
  port="$(jq -r '.cleanup.hostPort' "$EXPECTED")"
  [[ "$host" == 127.0.0.1 ]] || die "live binding must be loopback only"
  [[ "$temp_relative" == tests/flapjack_stage2_meilisearch_source_contract_tmp ]] \
    || die "cleanup temp path failed exact-name guard"
  [[ "$(dirname "$TEMP_DIR")" == "$SCRIPT_DIR" && "$(basename "$TEMP_DIR")" \
    == flapjack_stage2_meilisearch_source_contract_tmp ]] \
    || die "cleanup temp path failed exact-name guard"
  [[ ! -e "$TEMP_DIR" ]] || die "temp directory collision: $TEMP_DIR"
  case "$(container_inspection_state)" in
    present) die "container name collision: $CONTAINER_NAME" ;;
    absent) ;;
    indeterminate) die "container inspection failed for $CONTAINER_NAME" ;;
  esac
  if probe_output="$(lsof -nP -iTCP:"$port" -sTCP:LISTEN 2>&1)"; then
    die "loopback port collision: $port"
  else
    probe_status=$?
  fi
  [[ "$probe_status" -eq 1 && -z "$probe_output" ]] || die "host-port probe failed for port $port"
  inventory="$(docker ps --format '{{.Names}}\t{{.Ports}}\t{{.Mounts}}')" \
    || die "Docker inventory failed"
  awk -v name="$CONTAINER_NAME" -v port="$port" -v temp="$TEMP_DIR" '
      $1 == name || index($0, "127.0.0.1:" port "->") || index($0, temp) { found=1 }
      END { exit found ? 0 : 1 }
    ' <<<"$inventory" \
    && die "foreign container collision with exact name, port, or temp path"
  return 0
}

container_inspection_state() {
  local inspection
  if inspection="$(docker container inspect "$CONTAINER_NAME" 2>&1)"; then
    printf '%s\n' present
  elif [[ "$inspection" == *"No such container: ${CONTAINER_NAME}"* ]]; then
    printf '%s\n' absent
  else
    printf '%s\n' indeterminate
  fi
}

cleanup_live() {
  [[ "$MODE" != stub ]] || return 0
  [[ "$LIVE_CLEANED" -eq 0 ]] || return 0
  LIVE_CLEANED=1
  local cleanup_failed=0 container_state
  if [[ "$CONTAINER_CLEANUP_ARMED" -eq 1 ]]; then
    docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  fi
  if [[ -n "$TEMP_DIR" && "$(dirname "$TEMP_DIR")" == "$SCRIPT_DIR" \
    && "$(basename "$TEMP_DIR")" == flapjack_stage2_meilisearch_source_contract_tmp ]]; then
    rm -rf -- "$TEMP_DIR" || cleanup_failed=1
  fi
  [[ ! -e "$TEMP_DIR" ]] || cleanup_failed=1
  container_state="$(container_inspection_state)"
  [[ "$container_state" == absent ]] || cleanup_failed=1
  [[ "$cleanup_failed" -eq 0 ]]
}

stage_sanitized_failure_evidence() {
  [[ -n "$TRACE_FILE" && -f "$TRACE_FILE" ]] || return 0
  [[ "$(dirname "$FAILURE_EVIDENCE_DIR")" == /tmp ]] \
    || die "failure evidence path failed guard"
  mkdir -m 700 "$FAILURE_EVIDENCE_DIR"
  cp "$TRACE_FILE" "$FAILURE_EVIDENCE_DIR/request_trace.jsonl"
  if [[ -f "$TEMP_DIR/last_response.json" ]]; then
    cp "$TEMP_DIR/last_response.json" "$FAILURE_EVIDENCE_DIR/last_response.json"
  fi
  if [[ -f "$TEMP_DIR/last_response_label.txt" ]]; then
    cp "$TEMP_DIR/last_response_label.txt" \
      "$FAILURE_EVIDENCE_DIR/last_response_label.txt"
  fi
}

discard_staged_failure_evidence() {
  [[ "$(dirname "$FAILURE_EVIDENCE_DIR")" == /tmp ]] \
    || die "failure evidence path failed guard"
  [[ "$(basename "$FAILURE_EVIDENCE_DIR")" \
    == flapjack_stage2_meilisearch_source_contract_failure_$$ ]] \
    || die "failure evidence name failed guard"
  rm -rf -- "$FAILURE_EVIDENCE_DIR"
}

cleanup_on_exit() {
  local script_status=$?
  trap - EXIT INT TERM
  if [[ "$script_status" -ne 0 ]]; then
    stage_sanitized_failure_evidence
  fi
  if ! cleanup_live; then
    script_status=1
  fi
  if [[ "$script_status" -ne 0 && -d "$FAILURE_EVIDENCE_DIR" ]]; then
    printf 'INFO: preserved sanitized failure evidence at %s\n' \
      "$FAILURE_EVIDENCE_DIR" >&2
  fi
  exit "$script_status"
}

start_live_source() {
  docker_safety_gate
  mkdir -m 700 "$TEMP_DIR"
  mkdir -m 700 "$TEMP_DIR/data"
  TRACE_FILE="$TEMP_DIR/request_trace.jsonl"
  : >"$TRACE_FILE"
  MASTER_KEY="stage2-kat-$(openssl rand -hex 24)"
  SECRET_VALUES+=("$MASTER_KEY")
  local image port
  image="$(jq -r '.source.image' "$EXPECTED")"
  port="$(jq -r '.cleanup.hostPort' "$EXPECTED")"
  CONTAINER_CLEANUP_ARMED=1
  docker run -d --name "$CONTAINER_NAME" \
    -p "127.0.0.1:${port}:7700" \
    -v "$TEMP_DIR/data:/meili_data" \
    -e "MEILI_MASTER_KEY=${MASTER_KEY}" \
    -e MEILI_ENV=development \
    "$image" >/dev/null
  BASE_URL="http://127.0.0.1:${port}"
  wait_for_live_health
}

wait_for_live_health() {
  local response_file="$TEMP_DIR/health_response.json"
  local status payload attempt
  for ((attempt = 0; attempt < 60; attempt++)); do
    if status="$(curl -s -o "$response_file" -w '%{http_code}' \
      "${BASE_URL}/health")"; then
      payload="$(jq -c . "$response_file" 2>/dev/null)" \
        || die "response health_${attempt} is not valid JSON"
      redact_and_reject_credentials "$payload"
      record_trace "health_${attempt}" GET /health
      if [[ "$status" == 200 ]] \
        && jq -e '.status == "available"' <<<"$payload" >/dev/null; then
        return 0
      fi
    fi
    sleep 0.25
  done
  die "Meilisearch did not become healthy"
}

live_task_request() {
  local label="$1" method="$2" path="$3" body="$4" expected_status="$5"
  local task
  task="$(submit_and_poll_task "$label" "$method" "$path" "$body")" \
    || return $?
  [[ "$(jq -r '.status' <<<"$task")" == "$expected_status" ]] \
    || die "$label task status mismatch"
}

seed_live_source() {
  live_task_request create_configured POST /indexes \
    "$(jq -c '.indexes.configured | {uid,primaryKey}' "$EXPECTED")" succeeded
  live_task_request seed_configured POST /indexes/configured_pk/documents \
    "$(cat "$FIXTURE_DIR/configured_primary_key_documents.json")" succeeded
  live_task_request settings_configured PATCH /indexes/configured_pk/settings \
    "$(cat "$FIXTURE_DIR/configured_primary_key_settings.json")" succeeded
  live_task_request create_inferred POST /indexes '{"uid":"inferred_pk"}' succeeded
  live_task_request seed_inferred POST /indexes/inferred_pk/documents \
    "$(cat "$FIXTURE_DIR/inferred_primary_key_documents.json")" succeeded
  live_task_request create_ambiguous POST /indexes '{"uid":"ambiguous_pk"}' succeeded
  live_task_request seed_ambiguous POST /indexes/ambiguous_pk/documents \
    "$(cat "$FIXTURE_DIR/ambiguous_primary_key_documents.json")" failed
}

validate_contract() {
  local version indexes ambiguous_task page0 page1 inferred settings synonyms
  local before_snapshot capture_after sorted_ids export_path
  version="$(request_json version GET /version)" || return $?
  assert_json_equal "$version" "$(jq -c '.source.version' "$EXPECTED")" \
    "source version mismatch"
  indexes="$(request_json indexes GET '/indexes?limit=10')" || return $?
  validate_primary_keys "$indexes"
  ambiguous_task="$(poll_task ambiguous_task \
    "$(jq -r '.tasks.ambiguous.uid' "$EXPECTED")")" || return $?
  jq -e --argjson uid "$(jq -c '.tasks.ambiguous.uid' "$EXPECTED")" \
    '.uid == $uid' <<<"$ambiguous_task" >/dev/null \
    || die "ambiguous primary key task mismatch"
  jq -e --arg status "$(jq -r '.tasks.ambiguous.status' "$EXPECTED")" \
    --arg code "$(jq -r '.tasks.ambiguous.failureCode' "$EXPECTED")" '
    .status == $status and .error.code == $code
  ' <<<"$ambiguous_task" >/dev/null || die "ambiguous primary key task was accepted"

  export_path="$(jq -r '.pagination.documentExportPath' "$EXPECTED")"
  page0="$(request_json configured_page_0 POST "$export_path" \
    "$(pagination_request_body 0)")" || return $?
  page1="$(request_json configured_page_1 POST "$export_path" \
    "$(pagination_request_body 1)")" || return $?
  sorted_ids="$(validate_pagination_and_documents "$page0" "$page1")" \
    || return $?
  inferred="$(request_json inferred_page_0 POST \
    /indexes/inferred_pk/documents/fetch '{"offset":0,"limit":20}')" || return $?
  assert_json_equal "$(jq -c '.results' <<<"$inferred")" \
    "$(jq -c '.documents.inferred' "$EXPECTED")" "inferred documents mismatch"
  settings="$(request_json settings GET /indexes/configured_pk/settings)" \
    || return $?
  assert_expected_fields_equal "$settings" "$(jq -c '.settings' "$EXPECTED")" \
    "settings mismatch"
  synonyms="$(request_json synonyms GET /indexes/configured_pk/settings/synonyms)" \
    || return $?
  assert_json_equal "$synonyms" "$(jq -c '.synonyms' "$EXPECTED")" "synonyms mismatch"

  before_snapshot="$(source_capture_snapshot before)" || return $?
  validate_pre_mutation_stats "$before_snapshot"
  capture_after="$(source_capture_snapshot capture_after)" || return $?
  assert_json_equal "$capture_after" "$before_snapshot" "source mutated during capture"
  validate_controlled_mutation "$before_snapshot"
  validate_restricted_actions
  printf '%s\n' "$sorted_ids"
}

validate_stub_cleanup() {
  local cleanup_state expected
  cleanup_state="$(jq -c . "$STUB_RESPONSE_DIR/cleanup_state.json")" \
    || die "cleanup state is not valid JSON"
  expected="$(jq -c '.cleanup.residueExpected' "$EXPECTED")"
  [[ "$cleanup_state" == "$expected" ]] || die "cleanup residue detected"
}

emit_receipt() {
  local sorted_ids="$1"
  jq -cn \
    --argjson ids "$sorted_ids" \
    --argjson poll_limit "$TASK_POLL_LIMIT" \
    --arg container "$(jq -r '.cleanup.containerName' "$EXPECTED")" \
    --arg temp_dir "$(jq -r '.cleanup.tempDir' "$EXPECTED")" \
    --arg export_method "$(jq -r '.pagination.documentExportMethod' "$EXPECTED")" \
    --arg export_path "$(jq -r '.pagination.documentExportPath' "$EXPECTED")" \
    '{
      result:"PASS",
      sortedStableIds:$ids,
      responseParsing:"json",
      documentExport:{method:$export_method,path:$export_path},
      taskPolling:{bounded:true,limit:$poll_limit},
      cleanup:{containerName:$container,tempDir:$temp_dir}
    }'
}

run_preview_probe() {
  [[ "$MODE" == preview_live ]] || return 0
  local expected_records output status
  expected_records="$(jq -er '.documents.countAfter' "$EXPECTED")" \
    || die "preview record count fixture is missing"
  [[ "$expected_records" =~ ^[1-9][0-9]*$ ]] \
    || die "preview record count fixture must be positive"

  if output="$(
    FJ_MEILISEARCH_PREVIEW_ENDPOINT="$BASE_URL" \
      FJ_MEILISEARCH_PREVIEW_API_KEY="$MASTER_KEY" \
      FJ_MEILISEARCH_PREVIEW_EXPECTED_RECORDS="$expected_records" \
      timeout 600 cargo test -p flapjack-http -- \
        handlers::migration::preview_tests::meilisearch_live_preview_reports_exact_seeded_counts_and_codes \
        --ignored --exact --nocapture 2>&1
  )"; then
    printf '%s\n' "$output"
  else
    status=$?
    printf '%s\n' "$output" >&2
    [[ "$status" -ne 124 ]] \
      || die "preview probe timed out after 600 seconds"
    die "preview probe test failed (exit=$status)"
  fi

  grep -Eq 'test result: ok\. 1 passed;' <<<"$output" \
    || die "preview probe did not execute exactly one passing test"
  grep -Fq '"previewProof":"PASS"' <<<"$output" \
    || die "preview probe PASS receipt is missing"
}

main() {
  parse_args "$@"
  require_tools_and_fixture
  validate_oracle_structure
  if [[ "$MODE" != stub ]]; then
    trap cleanup_on_exit EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    start_live_source
    seed_live_source
  fi

  local sorted_ids
  sorted_ids="$(validate_contract)" || return $?
  run_preview_probe
  if [[ "$MODE" != stub ]]; then
    stage_sanitized_failure_evidence
    cleanup_live || die "cleanup residue detected"
    [[ ! -e "$TEMP_DIR" ]] || die "cleanup residue detected"
    [[ "$(container_inspection_state)" == absent ]] \
      || die "cleanup residue detected"
    local secret
    for secret in "${SECRET_VALUES[@]-}"; do
      [[ -z "$secret" ]] && continue
      if git -C "$ENGINE_DIR/.." grep -Fq -- "$secret"; then
        die "credential leakage detected"
      fi
    done
    discard_staged_failure_evidence
    trap - EXIT INT TERM
  else
    validate_stub_cleanup
  fi
  emit_receipt "$sorted_ids"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
