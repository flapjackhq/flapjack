#!/usr/bin/env bash
# Canonical-fixture, read-only Algolia migration proof against a real Flapjack server.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROVIDER="$SCRIPT_DIR/helpers/pbv2_algolia_provider.py"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
FIXTURE="${PBV2_CATALOG_FIXTURE:-}"
ADMIN_APP="pbv2-loopback-flapjack"
ADMIN_KEY="pbv2-loopback-flapjack-admin-key"
SOURCE_APP="PBV2APP"
SOURCE_KEY="pbv2-loopback-source-key"
TARGET_INDEX="pbv2_acceptance_imported"
TARGET_REPLICA="${TARGET_INDEX}_price_asc"
TMP=""
PROVIDER_PID=""
SERVER_PID=""
BASE=""
PROVIDER_BASE=""
REPLACEMENT_COMPLETED=0
CATALOG_MISMATCH=0
FIRST_CATALOG_MISMATCH=""

die() {
  printf 'PBV2_ALGOLIA_MIGRATION=RED reason=%s\n' "$1" >&2
  exit 1
}

terminate_pid() {
  local pid="$1" attempt
  [ -n "$pid" ] || return 0
  if kill -0 "$pid" 2>/dev/null; then
    kill "$pid" 2>/dev/null || true
    for attempt in $(seq 1 50); do
      kill -0 "$pid" 2>/dev/null || break
      sleep 0.1
    done
    kill -0 "$pid" 2>/dev/null && kill -KILL "$pid" 2>/dev/null || true
  fi
  wait "$pid" 2>/dev/null || true
}

cleanup() {
  local rc=$?
  terminate_pid "$SERVER_PID"
  terminate_pid "$PROVIDER_PID"
  if [ "$rc" -eq 0 ] && [ "$REPLACEMENT_COMPLETED" -eq 0 ] && [ -n "$TMP" ] && [ -d "$TMP" ]; then
    rm -rf "$TMP"
  elif [ -n "$TMP" ]; then
    printf 'PBV2_ALGOLIA_MIGRATION=INFO evidence=%s\n' "$TMP" >&2
  fi
  exit "$rc"
}
trap cleanup EXIT INT TERM

require_inputs() {
  local tool actual_sha
  [ -f "$FIXTURE" ] || die 'canonical_fixture_missing'
  for tool in cargo curl jq python3 sed; do
    command -v "$tool" >/dev/null 2>&1 || die "required_tool_missing_${tool}"
  done
  [ -f "$PROVIDER" ] || die 'provider_helper_missing'
  [ -x "$WAIT_HELPER" ] || die 'wait_helper_missing'
  actual_sha="$(shasum -a 256 "$FIXTURE" | awk '{print $1}')"
  [ "$actual_sha" = 111919b3780478fa5c653cb15551d170f8b6f8d96ee88333a19b47012686ef44 ] \
    || die "canonical_fixture_sha_mismatch_${actual_sha}"
}

target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

start_provider() {
  python3 "$PROVIDER" --fixture "$FIXTURE" --app-id "$SOURCE_APP" --api-key "$SOURCE_KEY" \
    >"$TMP/provider.ready" 2>"$TMP/provider.log" &
  PROVIDER_PID=$!
  local attempt ready=""
  for attempt in $(seq 1 80); do
    kill -0 "$PROVIDER_PID" 2>/dev/null || die 'provider_exited'
    ready="$(sed -n '1p' "$TMP/provider.ready")"
    [ -n "$ready" ] && break
    sleep 0.1
  done
  PROVIDER_BASE="$(printf '%s' "$ready" | jq -er .base_url)" || die 'provider_readiness_invalid'
  curl -fsS "$PROVIDER_BASE/__state" >"$TMP/source-before.json" || die 'provider_state_unreachable'
}

start_flapjack() {
  local bin="${FLAPJACK_BIN:-}"
  if [ -z "$bin" ]; then
    (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$TMP/build.log" 2>&1) \
      || { tail -60 "$TMP/build.log" >&2; die 'flapjack_build_failed'; }
    bin="$(target_dir)/debug/flapjack"
  fi
  [ -x "$bin" ] || die 'flapjack_binary_missing'
  env -u FLAPJACK_NO_AUTH -u FLAPJACK_PORT -u FLAPJACK_BIND_ADDR \
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
    FLAPJACK_DATA_DIR="$TMP/data" \
    FLAPJACK_TEST_ALGOLIA_BASE_URL="$PROVIDER_BASE" \
    "$bin" --auto-port >"$TMP/flapjack.log" 2>&1 &
  SERVER_PID=$!
  "$WAIT_HELPER" --pid "$SERVER_PID" --host 127.0.0.1 --port auto \
    --log-path "$TMP/flapjack.log" --retries 100 --interval-seconds 0.25 \
    || die 'flapjack_readiness_failed'
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$TMP/flapjack.log" | head -1)"
  [ -n "$port" ] || die 'flapjack_auto_port_missing'
  BASE="http://127.0.0.1:${port}"
}

request() {
  local label="$1" method="$2" path="$3" body="$4" expected="$5" status
  local args=(
    -sS --connect-timeout 2 --max-time 60 -o "$TMP/${label}.json" -w '%{http_code}'
    -X "$method" -H 'content-type: application/json'
    -H "x-algolia-application-id: $ADMIN_APP" -H "x-algolia-api-key: $ADMIN_KEY"
  )
  [ -z "$body" ] || args+=(--data "$body")
  status="$(curl "${args[@]}" "$BASE$path")" || die "${label}_transport"
  [ "$status" = "$expected" ] \
    || die "${label}_status_expected_${expected}_actual_${status}_body_$(jq -c . "$TMP/${label}.json" 2>/dev/null || true)"
}

poll_job() {
  local job_id="$1" label="${2:-terminal}" attempt disposition
  for attempt in $(seq 1 240); do
    request "$label" GET "/1/migrations/algolia/$job_id" '' 200
    disposition="$(jq -er .disposition "$TMP/${label}.json")"
    case "$disposition" in
      succeeded) return 0 ;;
      running) sleep 0.1 ;;
      *) die "migration_terminal_${disposition}" ;;
    esac
  done
  die 'migration_poll_timeout'
}

wait_for_task() {
  local label="$1" task_id="$2" attempt status
  [ -n "$task_id" ] && [ "$task_id" != null ] || die "${label}_task_id_missing"
  for attempt in $(seq 1 240); do
    request "${label}_task" GET "/1/indexes/$TARGET_INDEX/task/$task_id" '' 200
    status="$(jq -er .status "$TMP/${label}_task.json")" || die "${label}_task_status_invalid"
    case "$status" in
      published) return 0 ;;
      notPublished) sleep 0.1 ;;
      *) die "${label}_task_status_${status}" ;;
    esac
  done
  die "${label}_task_poll_timeout"
}

seed_write() {
  local label="$1" method="$2" path="$3" body="$4" task_id
  request "$label" "$method" "$path" "$body" 200
  task_id="$(jq -er '.taskID | select(type == "string" or type == "number")' "$TMP/${label}.json")" \
    || die "${label}_task_id_invalid"
  wait_for_task "$label" "$task_id"
}

assert_preview() {
  local payload primary
  primary="$(jq -er .oracles.replicas.source_primary "$FIXTURE")"
  payload="$(jq -cn --arg app "$SOURCE_APP" --arg key "$SOURCE_KEY" --arg source "$primary" \
    '{appId:$app,apiKey:$key,sourceIndex:$source,targetIndex:"pbv2_acceptance_imported"}')"
  request preview POST /1/migrations/algolia/preview "$payload" 200
  jq -e --slurpfile fixture "$FIXTURE" '
    .sourceCounts == {indexes:1,records:6} and
    .report.summary.hardRejections == 0 and
    ([.report.entries[] | select(.severity == "Warning") | {code,jsonPath}]) ==
      [
        {
          code:$fixture[0].source_preview.warning_codes[0],
          jsonPath:$fixture[0].source_preview.warning_paths[0]
        },
        {
          code:"ReplicaRelevancyStrictnessSemanticMismatch",
          jsonPath:"$.replicaSettings[\"pbv2_products_price_asc\"].relevancyStrictness"
        }
      ]
  ' "$TMP/preview.json" >/dev/null || die 'preview_oracle_mismatch'
}

assert_import_and_search() {
  local payload primary job_id
  primary="$(jq -er .oracles.replicas.source_primary "$FIXTURE")"
  payload="$(jq -cn --arg app "$SOURCE_APP" --arg key "$SOURCE_KEY" --arg source "$primary" --arg target "$TARGET_INDEX" \
    '{appId:$app,apiKey:$key,sourceIndex:$source,targetIndex:$target,overwrite:false}')"
  request submit POST /1/migrations/algolia "$payload" 202
  job_id="$(jq -er .jobId "$TMP/submit.json")" || die 'submit_job_id_missing'
  poll_job "$job_id"
  jq -e '
    .disposition == "succeeded" and .settingsApplied == true and
    .objectsImported.imported == 6 and .synonymsImported.imported == 1 and
    .rulesImported.imported == 1 and
    ([.warnings[] | select(.code == "PersistedNoBehaviorSetting" and .jsonPath == "$.allowCompressionOfIntegerArray")] | length) == 1
  ' "$TMP/terminal.json" >/dev/null || die 'terminal_import_oracle_mismatch'

  request primary_search POST "/1/indexes/$TARGET_INDEX/query" '{"query":"trail","hitsPerPage":10}' 200
  jq -e --slurpfile fixture "$FIXTURE" \
    '[.hits[].objectID] == $fixture[0].oracles.search.trail_baseline_order' \
    "$TMP/primary_search.json" >/dev/null || die 'primary_search_order_mismatch'
  request replica_search POST "/1/indexes/$TARGET_REPLICA/query" '{"query":"trail","hitsPerPage":10}' 200
  jq -e --slurpfile fixture "$FIXTURE" \
    '[.hits[].objectID] == $fixture[0].oracles.replicas.price_asc_order' \
    "$TMP/replica_search.json" >/dev/null || die 'replica_search_order_mismatch'
  request settings GET "/1/indexes/$TARGET_INDEX/settings" '' 200
  jq -e --slurpfile fixture "$FIXTURE" '
    .searchableAttributes == $fixture[0].settings.searchableAttributes and
    .customRanking == $fixture[0].settings.customRanking and
    .ranking == $fixture[0].settings.ranking and
    .allowCompressionOfIntegerArray == true and
    .replicas == ["virtual(pbv2_acceptance_imported_price_asc)"]
  ' "$TMP/settings.json" >/dev/null || die 'settings_or_topology_mismatch'
  request synonym GET "/1/indexes/$TARGET_INDEX/synonyms/pbv2-syn-shell-jacket" '' 200
  jq -e --slurpfile fixture "$FIXTURE" '. == $fixture[0].synonyms[0]' "$TMP/synonym.json" >/dev/null \
    || die 'synonym_mismatch'
  request rule GET "/1/indexes/$TARGET_INDEX/rules/pbv2-rule-trail-outerwear" '' 200
  jq -e --slurpfile fixture "$FIXTURE" '. == $fixture[0].rules[0]' "$TMP/rule.json" >/dev/null \
    || die 'rule_mismatch'
  request acknowledge POST "/1/migrations/algolia/$job_id/acknowledge" '' 204

  request delete_primary DELETE "/1/indexes/$TARGET_INDEX" '' 200
  request delete_replica DELETE "/1/indexes/$TARGET_REPLICA" '' 200
  request deleted_primary_absent POST "/1/indexes/$TARGET_INDEX/query" '{"query":"trail"}' 404
  request deleted_replica_absent POST "/1/indexes/$TARGET_REPLICA/query" '{"query":"trail"}' 404
}

derive_generation_a() {
  jq '
    .documents
    | map(if .objectID == "pbv2-trail-001" then .title = "Generation A overlapping trail jacket" else . end)
    + [{objectID:"generation-a-document",title:"Generation A only document",popularity:999}]
  ' "$FIXTURE" >"$TMP/generation_a_documents.json"
  jq '
    .rules
    | map(if .objectID == "pbv2-rule-trail-outerwear" then .description = "Generation A overlapping rule" else . end)
    + [{objectID:"generation-a-rule",conditions:[],consequence:{}}]
  ' "$FIXTURE" >"$TMP/generation_a_rules.json"
  jq '
    .synonyms
    | map(if .objectID == "pbv2-syn-shell-jacket" then .synonyms = ["jacket","shell"] else . end)
    + [{objectID:"generation-a-synonym",type:"synonym",synonyms:["generation","alpha"]}]
  ' "$FIXTURE" >"$TMP/generation_a_synonyms.json"
  jq '
    .settings
    | .hitsPerPage = 3
    | .ranking = ["custom","exact","attribute","proximity","filters","words","geo","typo"]
    | .attributeForDistinct = "brand"
    | .distinct = true
  ' "$FIXTURE" >"$TMP/generation_a_settings.json"
}

seed_generation_a() {
  local body object_id
  request generation_a_create POST /1/indexes "$(jq -cn --arg uid "$TARGET_INDEX" '{uid:$uid}')" 200
  jq -e --arg uid "$TARGET_INDEX" '.uid == $uid and (has("taskID") | not)' \
    "$TMP/generation_a_create.json" >/dev/null || die 'generation_a_create_response_invalid'

  body="$(jq -c '{requests:map({action:"addObject",body:.})}' "$TMP/generation_a_documents.json")"
  seed_write generation_a_documents_seed POST "/1/indexes/$TARGET_INDEX/batch" "$body"
  seed_write generation_a_settings_seed PUT "/1/indexes/$TARGET_INDEX/settings" \
    "$(jq -c . "$TMP/generation_a_settings.json")"
  while IFS= read -r body; do
    object_id="$(jq -er .objectID <<<"$body")" || die 'generation_a_rule_id_invalid'
    seed_write "generation_a_rule_seed_${object_id}" PUT "/1/indexes/$TARGET_INDEX/rules/$object_id" "$body"
  done < <(jq -c '.[]' "$TMP/generation_a_rules.json")
  while IFS= read -r body; do
    object_id="$(jq -er .objectID <<<"$body")" || die 'generation_a_synonym_id_invalid'
    seed_write "generation_a_synonym_seed_${object_id}" PUT "/1/indexes/$TARGET_INDEX/synonyms/$object_id" "$body"
  done < <(jq -c '.[]' "$TMP/generation_a_synonyms.json")
}

enumerate_documents() {
  local label="$1" page=0 cursor="" body response next_cursor seen_cursors='[]'
  jq -cn '[]' >"$TMP/${label}_documents.json"
  while [ "$page" -lt 1000 ]; do
    if [ -z "$cursor" ]; then
      body='{"hitsPerPage":2}'
    else
      body="$(jq -cn --arg cursor "$cursor" '{cursor:$cursor,hitsPerPage:2}')"
    fi
    request "${label}_documents_page_${page}" POST "/1/indexes/$TARGET_INDEX/browse" "$body" 200
    response="$TMP/${label}_documents_page_${page}.json"
    jq -e '.hits | type == "array"' "$response" >/dev/null \
      || die "${label}_documents_page_${page}_malformed"
    jq -s '.[0] + .[1].hits' "$TMP/${label}_documents.json" "$response" \
      >"$TMP/${label}_documents.next.json"
    mv "$TMP/${label}_documents.next.json" "$TMP/${label}_documents.json"
    next_cursor="$(jq -er 'if (.cursor == null or has("cursor") == false) then "" elif (.cursor|type) == "string" and .cursor != "" then .cursor else error("invalid cursor") end' "$response")" \
      || die "${label}_documents_page_${page}_cursor_invalid"
    [ -n "$next_cursor" ] || break
    jq -e --arg cursor "$next_cursor" 'index($cursor) == null' <<<"$seen_cursors" >/dev/null \
      || die "${label}_documents_repeated_cursor"
    seen_cursors="$(jq -c --arg cursor "$next_cursor" '. + [$cursor]' <<<"$seen_cursors")"
    cursor="$next_cursor"
    page=$((page + 1))
  done
  [ "$page" -lt 1000 ] || die "${label}_documents_page_bound_exhausted"
  jq -e 'all(.[]; (.objectID|type) == "string") and ([.[].objectID] | length == (unique | length))' \
    "$TMP/${label}_documents.json" >/dev/null || die "${label}_documents_duplicate_or_invalid_id"
}

enumerate_numbered() {
  local label="$1" dimension="$2" path="$3" page=0 response nb_pages="" nb_hits=""
  jq -cn '[]' >"$TMP/${label}_${dimension}.json"
  while [ "$page" -lt 1000 ]; do
    request "${label}_${dimension}_page_${page}" POST "$path" \
      "$(jq -cn --argjson page "$page" '{query:"",page:$page,hitsPerPage:1}')" 200
    response="$TMP/${label}_${dimension}_page_${page}.json"
    jq -e --argjson page "$page" '
      (.hits|type) == "array" and .page == $page and
      (.nbPages|type) == "number" and .nbPages >= 0 and (.nbPages|floor) == .nbPages and
      (.nbHits|type) == "number" and .nbHits >= 0 and (.nbHits|floor) == .nbHits
    ' "$response" >/dev/null || die "${label}_${dimension}_page_${page}_malformed"
    if [ -z "$nb_pages" ]; then
      nb_pages="$(jq -er .nbPages "$response")"
      nb_hits="$(jq -er .nbHits "$response")"
    else
      jq -e --argjson pages "$nb_pages" --argjson hits "$nb_hits" \
        '.nbPages == $pages and .nbHits == $hits' "$response" >/dev/null \
        || die "${label}_${dimension}_unstable_counts"
    fi
    jq -s '.[0] + .[1].hits' "$TMP/${label}_${dimension}.json" "$response" \
      >"$TMP/${label}_${dimension}.next.json"
    mv "$TMP/${label}_${dimension}.next.json" "$TMP/${label}_${dimension}.json"
    [ "$nb_pages" -eq 0 ] || [ $((page + 1)) -ge "$nb_pages" ] && break
    page=$((page + 1))
  done
  [ "$page" -lt 1000 ] || die "${label}_${dimension}_page_bound_exhausted"
  jq -e --argjson hits "$nb_hits" '
    length == $hits and all(.[]; (.objectID|type) == "string") and
    ([.[].objectID] | length == (unique | length))
  ' "$TMP/${label}_${dimension}.json" >/dev/null \
    || die "${label}_${dimension}_truncated_duplicate_or_invalid_id"
}

record_catalog_mismatch() {
  local phase="$1" dimension="$2" id_path="$3" expected="$4" actual="$5" mismatch
  mismatch="$(jq -cn --arg phase "$phase" --arg dimension "$dimension" --arg path "$id_path" \
    --argjson expected "$expected" --argjson actual "$actual" \
    '{phase:$phase,dimension:$dimension,path:$path,expected:$expected,actual:$actual}')"
  printf '%s\n' "$mismatch" >>"$TMP/catalog_mismatches.ndjson"
  if [ "$CATALOG_MISMATCH" -eq 0 ]; then
    FIRST_CATALOG_MISMATCH="$mismatch"
  fi
  CATALOG_MISMATCH=1
}

compare_record_dimension() {
  local phase="$1" dimension="$2" expected_file="$3" actual_file="$4" difference
  difference="$(jq -cn --slurpfile expected "$expected_file" --slurpfile actual "$actual_file" '
    def by_id: map({key:.objectID,value:.}) | from_entries;
    def display_path:
      map(if type == "number" then "[\(.)]" else tostring end) | join(".");
    ($expected[0] | by_id) as $e | ($actual[0] | by_id) as $a |
    (($e|keys) + ($a|keys) | unique) as $ids |
    first($ids[] as $id | select($e[$id] != $a[$id]) |
      $e[$id] as $expected_body | $a[$id] as $actual_body |
      if $expected_body == null or $actual_body == null then
        {id:$id,path:"$",expected:$expected_body,actual:$actual_body}
      else
        ([($expected_body | paths), ($actual_body | paths)] | unique_by(tojson)) as $paths |
        (first($paths[] as $path |
          select(($expected_body | getpath($path)) != ($actual_body | getpath($path))) |
          {id:$id,path:($path | display_path),
           expected:($expected_body | getpath($path)),actual:($actual_body | getpath($path))}) //
         {id:$id,path:"$",expected:$expected_body,actual:$actual_body})
      end) // empty
  ')"
  [ -z "$difference" ] || record_catalog_mismatch "$phase" "$dimension" \
    "$(jq -r '.id + ":" + .path' <<<"$difference")" \
    "$(jq -c .expected <<<"$difference")" "$(jq -c .actual <<<"$difference")"
}

compare_settings() {
  local phase="$1" generation="$2" expected_file actual_file difference
  actual_file="$TMP/${phase}_settings.json"
  if [ "$generation" = A ]; then
    expected_file="$TMP/generation_a_settings.json"
    difference="$(jq -cn --slurpfile expected "$expected_file" --slurpfile actual "$actual_file" '
      $expected[0] as $e | $actual[0] as $a |
      first(($e|keys[]) as $key | select($e[$key] != $a[$key]) |
        {path:$key,expected:$e[$key],actual:$a[$key]}) //
      if (($a.replicas // []) != []) then {path:"replicas",expected:[],actual:$a.replicas} else empty end
    ')"
  else
    expected_file="$FIXTURE"
    difference="$(jq -cn --slurpfile fixture "$expected_file" --slurpfile actual "$actual_file" '
      $fixture[0].settings as $e | $actual[0] as $a |
      first(($e|keys[]) as $key | select($e[$key] != $a[$key]) |
        {path:$key,expected:$e[$key],actual:$a[$key]}) //
      if $a.replicas != ["virtual(pbv2_acceptance_imported_price_asc)"] then
        {path:"replicas",expected:["virtual(pbv2_acceptance_imported_price_asc)"],actual:($a.replicas // null)}
      elif $a.attributeForDistinct != null then
        {path:"attributeForDistinct",expected:null,actual:$a.attributeForDistinct}
      elif ($a|has("distinct")) then
        {path:"distinct",expected:"absent",actual:$a.distinct}
      else empty end
    ')"
  fi
  [ -z "$difference" ] || record_catalog_mismatch "$phase" settings \
    "$(jq -r .path <<<"$difference")" "$(jq -c .expected <<<"$difference")" \
    "$(jq -c .actual <<<"$difference")"
}

observe_catalog() {
  local label="$1" generation="$2" prefix expected_order actual_order
  enumerate_documents "$label"
  enumerate_numbered "$label" rules "/1/indexes/$TARGET_INDEX/rules/search"
  enumerate_numbered "$label" synonyms "/1/indexes/$TARGET_INDEX/synonyms/search"
  request "${label}_settings" GET "/1/indexes/$TARGET_INDEX/settings" '' 200
  request "${label}_search" POST "/1/indexes/$TARGET_INDEX/query" \
    '{"query":"trail","hitsPerPage":100,"distinct":false}' 200

  if [ "$generation" = A ]; then
    prefix="$TMP/generation_a"
  else
    jq '.documents' "$FIXTURE" >"$TMP/canonical_b_documents.json"
    jq '.rules' "$FIXTURE" >"$TMP/canonical_b_rules.json"
    jq '.synonyms' "$FIXTURE" >"$TMP/canonical_b_synonyms.json"
    prefix="$TMP/canonical_b"
  fi
  compare_record_dimension "$label" documents "${prefix}_documents.json" "$TMP/${label}_documents.json"
  compare_record_dimension "$label" rules "${prefix}_rules.json" "$TMP/${label}_rules.json"
  compare_record_dimension "$label" synonyms "${prefix}_synonyms.json" "$TMP/${label}_synonyms.json"
  compare_settings "$label" "$generation"
  expected_order="$(jq -c '.oracles.search.trail_baseline_order' "$FIXTURE")"
  actual_order="$(jq -c '[.hits[].objectID]' "$TMP/${label}_search.json")" \
    || die "${label}_search_envelope_invalid"
  [ "$actual_order" = "$expected_order" ] || record_catalog_mismatch "$label" search hit_order \
    "$expected_order" "$actual_order"
}

assert_replacement() {
  local primary payload job_id
  derive_generation_a
  seed_generation_a
  observe_catalog generation_a A
  [ "$CATALOG_MISMATCH" -eq 0 ] || die "generation_a_setup_mismatch_${FIRST_CATALOG_MISMATCH}"

  primary="$(jq -er .oracles.replicas.source_primary "$FIXTURE")"
  payload="$(jq -cn --arg app "$SOURCE_APP" --arg key "$SOURCE_KEY" --arg source "$primary" \
    --arg target "$TARGET_INDEX" \
    '{appId:$app,apiKey:$key,sourceIndex:$source,targetIndex:$target,overwrite:true}')"
  request replacement_submit POST /1/migrations/algolia "$payload" 202
  job_id="$(jq -er .jobId "$TMP/replacement_submit.json")" || die 'replacement_submit_job_id_missing'
  poll_job "$job_id" replacement_terminal
  jq -e '.phase == "activating" and .disposition == "succeeded" and .terminalAt != null' \
    "$TMP/replacement_terminal.json" >/dev/null || die 'replacement_terminal_contract_mismatch'

  observe_catalog post_terminal B
  request replacement_ack_1 POST "/1/migrations/algolia/$job_id/acknowledge" '' 204
  observe_catalog post_ack_1 B
  request replacement_ack_2 POST "/1/migrations/algolia/$job_id/acknowledge" '' 204
  observe_catalog post_ack_2 B
  request replacement_delete_primary DELETE "/1/indexes/$TARGET_INDEX" '' 200
  request replacement_delete_replica DELETE "/1/indexes/$TARGET_REPLICA" '' 200
  request replacement_deleted_primary_absent POST "/1/indexes/$TARGET_INDEX/query" '{"query":"trail"}' 404
  request replacement_deleted_replica_absent POST "/1/indexes/$TARGET_REPLICA/query" '{"query":"trail"}' 404
  REPLACEMENT_COMPLETED=1
}

assert_source_unchanged() {
  curl -fsS "$PROVIDER_BASE/__state" >"$TMP/source-after.json" || die 'provider_final_state_unreachable'
  jq -e --slurpfile before "$TMP/source-before.json" '
    .source_digest == $before[0].source_digest and .mutation_attempts == 0 and
    ([.requests[].method] | all(. == "GET" or . == "POST"))
  ' "$TMP/source-after.json" >/dev/null || die 'source_mutation_or_digest_mismatch'
}

main() {
  require_inputs
  TMP="$(mktemp -d "${TMPDIR:-/tmp}/pbv2_algolia_migration.XXXXXX")"
  mkdir -p "$TMP/data"
  start_provider
  start_flapjack
  assert_preview
  assert_import_and_search
  assert_replacement
  assert_source_unchanged
  if [ "$CATALOG_MISMATCH" -ne 0 ]; then
    printf 'PBV2_ALGOLIA_MIGRATION=RED reason=catalog_mismatch first=%s\n' "$FIRST_CATALOG_MISMATCH" >&2
    return 1
  fi
  printf 'PBV2_ALGOLIA_MIGRATION=PASS fixture_sha=111919b3780478fa5c653cb15551d170f8b6f8d96ee88333a19b47012686ef44 source_nonmutation=PASS zero_residue=PASS\n'
}

main "$@"
