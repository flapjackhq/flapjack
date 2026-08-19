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
  if [ "$rc" -eq 0 ] && [ -n "$TMP" ] && [ -d "$TMP" ]; then
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
  local job_id="$1" attempt disposition
  for attempt in $(seq 1 240); do
    request terminal GET "/1/migrations/algolia/$job_id" '' 200
    disposition="$(jq -er .disposition "$TMP/terminal.json")"
    case "$disposition" in
      succeeded) return 0 ;;
      running) sleep 0.1 ;;
      *) die "migration_terminal_${disposition}" ;;
    esac
  done
  die 'migration_poll_timeout'
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
  assert_source_unchanged
  printf 'PBV2_ALGOLIA_MIGRATION=PASS fixture_sha=111919b3780478fa5c653cb15551d170f8b6f8d96ee88333a19b47012686ef44 source_nonmutation=PASS zero_residue=PASS\n'
}

main "$@"
