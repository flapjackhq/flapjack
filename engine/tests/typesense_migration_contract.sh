#!/usr/bin/env bash
set -euo pipefail

IMAGE_REF="typesense/typesense:30.2"
IMAGE_DIGEST="sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110"
FIXTURE_DIR="tests/fixtures/2026_07_26_m0b_typesense_migration"
EXPECTED_BUNDLE="$FIXTURE_DIR/expected_bundle.json"
EXPECTED_PRODUCT_IDS=""
CAPTURED_PRODUCT_IDS=""
PRODUCTS="fj_ts_migration_products"
CATEGORIES="fj_ts_migration_categories"
ALIAS_NAME="fj_ts_migration_catalog"
SYNONYM_SET="fj_ts_migration_synonyms"
CURATION_SET="fj_ts_migration_curations"
UNRELATED_SYNONYM_SET="outside_stage2_global_synonyms"
BOOTSTRAP_KEY="TYPESENSE_STAGE2_BOOTSTRAP_CANARY"
CAPTURE_KEY_DESCRIPTION="fj-stage2-typesense-capture"
RUN_ID="${FJ_TYPESENSE_RUN_ID:-$(date +%s)_$$}"
if [[ ! "$RUN_ID" =~ ^[A-Za-z0-9_.-]{1,64}$ ]]; then
  printf 'FAIL: run id must contain only 1-64 alphanumeric, underscore, dot, or hyphen characters\n' >&2
  exit 1
fi
CONTAINER_NAME="fj_typesense_migration_contract_${RUN_ID}"
ROOT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fj_typesense_migration_contract.XXXXXX")"
DATA_DIR="$ROOT_DIR/data"
if [ -n "${FJ_TYPESENSE_EVIDENCE_DIR:-}" ]; then
  EVIDENCE_DIR="$FJ_TYPESENSE_EVIDENCE_DIR"
  if [ -e "$EVIDENCE_DIR" ]; then
    printf 'FAIL: refusing pre-existing evidence directory: %s\n' "$EVIDENCE_DIR" >&2
    rm -rf -- "$ROOT_DIR"
    exit 1
  fi
  mkdir -p -- "$EVIDENCE_DIR"
else
  EVIDENCE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fj_typesense_migration_evidence.XXXXXX")"
fi
EVIDENCE_DIR_OWNER="$EVIDENCE_DIR/.typesense_migration_contract_evidence_owned"
: >"$EVIDENCE_DIR_OWNER"
PASS_MARKER="$ROOT_DIR/pass"
SECRET_VALUES_FILE="$ROOT_DIR/secret_values.internal"
RESIDUE_MARKER="$ROOT_DIR/residue/fj_typesense_migration_contract_residue_marker"
PORT=""
SCOPED_KEY=""
EXPORT_KEY=""
FLAPJACK_PID=""
PROXY_PID=""
FLAPJACK_URL=""
FLAPJACK_ADMIN_KEY_VALUE=""
PROXY_PORT=""
readonly WRITE_FREEZE_SUPPORTED_ENDPOINTS='preview submit'
readonly WRITE_FREEZE_ATTESTATION_ARMS='missing false true'
readonly WRITE_FREEZE_RESUME_ARMS='missing false true'

fail() {
  printf 'FAIL: %s\n' "$*" >&2
  exit 1
}

sanitize_file() {
  local file="$1"
  [ -f "$file" ] || return 0
  [ -f "$SECRET_VALUES_FILE" ] || return 0
  while IFS= read -r secret_line; do
    local label secret escaped
    label="${secret_line%%	*}"
    secret="${secret_line#*	}"
    [ -n "$secret" ] || continue
    escaped="$(printf '%s' "$secret" | sed 's/[\/&|]/\\&/g')"
    sed -i.bak "s|$escaped|[REDACTED_${label}_KEY]|g" "$file" 2>/dev/null || true
    rm -f "$file.bak"
  done <"$SECRET_VALUES_FILE"
}

remember_secret() {
  local label="$1" value="$2"
  [ -n "$value" ] && [ "$value" != null ] || return 0
  grep -Fxq "${label}	${value}" "$SECRET_VALUES_FILE" 2>/dev/null || printf '%s\t%s\n' "$label" "$value" >>"$SECRET_VALUES_FILE"
}

sanitize_preserved_evidence() {
  local artifact
  for artifact in "$EVIDENCE_DIR"/*; do
    [ -f "$artifact" ] || continue
    sanitize_file "$artifact"
  done
}

scan_preserved_evidence_for_secrets() {
  local scan="$EVIDENCE_DIR/evidence_secret_scan.txt" found=0
  : >"$scan"
  [ -f "$SECRET_VALUES_FILE" ] || {
    printf 'PASS: no generated key values found in preserved evidence\n' >>"$scan"
    return
  }
  while IFS= read -r secret_line; do
    local label secret
    label="${secret_line%%	*}"
    secret="${secret_line#*	}"
    [ -n "$secret" ] || continue
    if grep -R -F "$secret" "$EVIDENCE_DIR" >/dev/null 2>&1; then
      printf 'FAIL: %s key value remained in preserved evidence\n' "$label" >>"$scan"
      found=1
    fi
  done <"$SECRET_VALUES_FILE"
  [ "$found" -eq 0 ] && printf 'PASS: no generated key values found in preserved evidence\n' >>"$scan"
}

preserve_cleanup_residue_evidence() {
  [ -f "$RESIDUE_MARKER" ] || return 0
  cp "$RESIDUE_MARKER" "$EVIDENCE_DIR/cleanup_residue_marker.txt"
}

stop_local_processes() {
  local pid
  for pid in "$FLAPJACK_PID" "$PROXY_PID"; do
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null || continue
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  done
  FLAPJACK_PID=""
  PROXY_PID=""
}

cleanup() {
  local rc="$?" artifact
  stop_local_processes
  if [ "$rc" -ne 0 ] || [ ! -f "$PASS_MARKER" ]; then
    mkdir -p "$EVIDENCE_DIR"
    docker logs "$CONTAINER_NAME" >"$EVIDENCE_DIR/container.log" 2>&1 || true
    docker image inspect "$IMAGE_REF" >"$EVIDENCE_DIR/image_inspect.json" 2>&1 || true
    for artifact in "$ROOT_DIR"/*.json "$ROOT_DIR"/*.jsonl "$ROOT_DIR"/*.txt "$ROOT_DIR"/*.log; do
      [ -f "$artifact" ] || continue
      cp "$artifact" "$EVIDENCE_DIR"/
    done
    preserve_cleanup_residue_evidence
    docker ps -a --filter "name=^/${CONTAINER_NAME}$" --format '{{.Names}}' >"$EVIDENCE_DIR/container_residue.txt" 2>/dev/null || true
    sanitize_preserved_evidence
    scan_preserved_evidence_for_secrets
  fi
  docker rm -f "$CONTAINER_NAME" >/dev/null 2>&1 || true
  if [ -f "$PASS_MARKER" ] && [ -f "$EVIDENCE_DIR_OWNER" ]; then
    rm -f "$EVIDENCE_DIR_OWNER"
    rmdir "$EVIDENCE_DIR" 2>/dev/null || true
  fi
  rm -rf -- "$ROOT_DIR"
  exit "$rc"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

repo_root() {
  git rev-parse --show-toplevel
}

require_tools() {
  command -v docker >/dev/null || fail "docker is required"
  command -v curl >/dev/null || fail "curl is required"
  command -v jq >/dev/null || fail "jq is required"
  command -v openssl >/dev/null || fail "openssl is required"
  command -v python3 >/dev/null || fail "python3 is required"
  command -v timeout >/dev/null || fail "timeout is required"
}

http_json() {
  local method="$1" path="$2" key="$3" body="${4:-}" out="$5" code
  if [ -n "$body" ]; then
    code="$(curl -sS --connect-timeout 2 --max-time 10 -o "$out" -w '%{http_code}' -X "$method" \
      -H "X-TYPESENSE-API-KEY: $key" -H 'Content-Type: application/json' \
      --data-binary @"$body" "http://127.0.0.1:${PORT}${path}")"
  else
    code="$(curl -sS --connect-timeout 2 --max-time 10 -o "$out" -w '%{http_code}' -X "$method" \
      -H "X-TYPESENSE-API-KEY: $key" "http://127.0.0.1:${PORT}${path}")"
  fi
  printf '%s' "$code"
}

expect_http() {
  local expected="$1" method="$2" path="$3" key="$4" body="${5:-}" out="$6" code
  code="$(http_json "$method" "$path" "$key" "$body" "$out")"
  [ "$code" = "$expected" ] || fail "$method $path returned $code, expected $expected"
}

start_typesense() {
  mkdir -p "$DATA_DIR"
  if docker image inspect "$IMAGE_REF" >/dev/null 2>&1; then
    printf 'local image present; pull skipped after inspect preflight\n' >"$ROOT_DIR/docker_pull.txt"
  else
    docker pull "$IMAGE_REF" >"$ROOT_DIR/docker_pull.txt"
  fi
  docker image inspect "$IMAGE_REF" >"$ROOT_DIR/image_inspect.json"
  jq -e --arg digest "$IMAGE_DIGEST" '
    .[0].Id == ("sha256:" + $digest | sub("^sha256:sha256:"; "sha256:"))
    or any(.[0].RepoDigests[]?; endswith("@" + $digest))
  ' "$ROOT_DIR/image_inspect.json" >/dev/null || fail "pinned image identity mismatch"
  docker run -d --name "$CONTAINER_NAME" \
    --publish 127.0.0.1::8108 \
    --volume "$DATA_DIR:/data" \
    "$IMAGE_REF" --data-dir=/data --api-key="$BOOTSTRAP_KEY" >"$ROOT_DIR/container_id.txt"
  docker port "$CONTAINER_NAME" 8108/tcp >"$ROOT_DIR/docker_port.txt"
  PORT="$(awk -F: '/127.0.0.1/ {print $NF; exit}' "$ROOT_DIR/docker_port.txt")"
  [ -n "$PORT" ] || fail "container did not publish a loopback port"
  for _ in $(seq 1 120); do
    if curl -sS --connect-timeout 1 --max-time 2 "http://127.0.0.1:${PORT}/health" >"$ROOT_DIR/health.json" 2>"$ROOT_DIR/health.err" \
      && jq -e '.ok == true' "$ROOT_DIR/health.json" >/dev/null \
      && curl -sS --connect-timeout 1 --max-time 2 -H "X-TYPESENSE-API-KEY: $BOOTSTRAP_KEY" \
        "http://127.0.0.1:${PORT}/debug" >"$ROOT_DIR/debug_startup.json" 2>"$ROOT_DIR/debug_startup.err" \
      && jq -e '.state == 1 and .version == "30.2"' "$ROOT_DIR/debug_startup.json" >/dev/null; then
      return
    fi
    sleep 0.25
  done
  fail "typesense did not become healthy"
}

seed_collections() {
  cat >"$ROOT_DIR/products_schema.json" <<JSON
{"name":"$PRODUCTS","enable_nested_fields":true,"token_separators":["-"],"symbols_to_index":["#"],"default_sorting_field":"price","synonym_sets":["$SYNONYM_SET"],"curation_sets":["$CURATION_SET"],"fields":[{"name":"title","type":"string"},{"name":"sku","type":"string"},{"name":"price","type":"float","facet":true},{"name":"inventory","type":"int32"},{"name":"available","type":"bool","facet":true},{"name":"tags","type":"string[]","facet":true},{"name":"metadata","type":"object","optional":true},{"name":"metadata.color","type":"string","facet":true,"optional":true},{"name":"nullable_note","type":"string","optional":true,"index":false},{"name":"secret_note","type":"string","optional":true,"index":false,"store":false},{"name":"embedding","type":"float[]","num_dim":3,"vec_dist":"cosine","optional":true},{"name":"category_id","type":"string","reference":"$CATEGORIES.id","optional":true}]}
JSON
  cat >"$ROOT_DIR/categories_schema.json" <<JSON
{"name":"$CATEGORIES","default_sorting_field":"priority","fields":[{"name":"name","type":"string"},{"name":"priority","type":"int32"},{"name":"active","type":"bool","facet":true},{"name":"labels","type":"string[]","facet":true},{"name":"parent","type":"string","optional":true}]}
JSON
  expect_http 201 POST /collections "$BOOTSTRAP_KEY" "$ROOT_DIR/categories_schema.json" "$ROOT_DIR/create_categories.json"
  expect_http 201 POST /collections "$BOOTSTRAP_KEY" "$ROOT_DIR/products_schema.json" "$ROOT_DIR/create_products.json"
  import_documents "$CATEGORIES" "$FIXTURE_DIR/seed_categories.jsonl" 2
  import_documents "$PRODUCTS" "$FIXTURE_DIR/seed_products.jsonl" 137
}

derive_expected_product_ids() {
  EXPECTED_PRODUCT_IDS="$ROOT_DIR/expected_product_ids.txt"
  CAPTURED_PRODUCT_IDS="$ROOT_DIR/captured_product_ids.txt"
  jq -r '.id' "$FIXTURE_DIR/seed_products.jsonl" | LC_ALL=C sort >"$EXPECTED_PRODUCT_IDS"
  [ "$(wc -l <"$EXPECTED_PRODUCT_IDS" | tr -d ' ')" = 137 ] \
    || fail "expected product ID artifact did not contain 137 IDs"
  [ "$(LC_ALL=C sort -u "$EXPECTED_PRODUCT_IDS" | wc -l | tr -d ' ')" = 137 ] \
    || fail "expected product ID artifact contained duplicate IDs"
}

run_production_export_stream_contract() {
  # The live contract runs by default. Meta-tests that exercise unrelated
  # harness mutations opt out explicitly and must retain an observable skip.
  if [ "${FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED:-1}" != 1 ]; then
    printf 'SKIP: production export-stream live contract explicitly disabled with FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=0\n'
    return 0
  fi
  local live_log="$ROOT_DIR/typesense_export_stream_live_contract.log"
  set +e
  FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1 \
    TYPESENSE_ENDPOINT="http://127.0.0.1:${PORT}" \
    TYPESENSE_API_KEY="$SCOPED_KEY" \
    TYPESENSE_COLLECTION="$PRODUCTS" \
    TYPESENSE_EXPECTED_IDS_FILE="$EXPECTED_PRODUCT_IDS" \
    timeout 600 cargo test -p flapjack-http --lib -- \
      handlers::migration::typesense_client_tests::typesense_export_stream_live_contract \
      --ignored --exact --nocapture >"$live_log" 2>&1
  local rc="$?"
  set -e
  cat "$live_log"
  [ "$rc" -ne 124 ] || fail "production export-stream live test timed out"
  [ "$rc" -eq 0 ] || fail "production export-stream live test rejected the current traversal"
  cmp "$EXPECTED_PRODUCT_IDS" "$CAPTURED_PRODUCT_IDS"
  grep -Fqx 'TYPESENSE_EXPORT_STREAM_CONTRACT documents=137 exact_ids=PASS export_requests=1 query_pagination=absent no_terminal_newline=PASS discovery_export_requests=0' "$live_log" \
    || fail "production export-stream live test omitted its exact contract receipt"
}

import_documents() {
  local collection="$1" file="$2" expected="$3" code out
  out="$ROOT_DIR/import_${collection}.jsonl"
  code="$(curl -sS --connect-timeout 2 --max-time 10 -o "$out" -w '%{http_code}' -X POST \
    -H "X-TYPESENSE-API-KEY: $BOOTSTRAP_KEY" \
    --data-binary @"$file" \
    "http://127.0.0.1:${PORT}/collections/${collection}/documents/import?action=create")"
  [ "$code" = 200 ] || fail "import $collection returned $code"
  jq -s -e --argjson expected "$expected" 'length == $expected and all(.[]; .success == true)' "$out" >/dev/null \
    || fail "import $collection did not report $expected successful JSON values"
}

seed_linked_sets() {
  cat >"$ROOT_DIR/synonym_set.json" <<'JSON'
{"items":[{"id":"espresso_synonym","root":"espresso","synonyms":["coffee","café"]}]}
JSON
  expect_http 200 PUT "/synonym_sets/$SYNONYM_SET" "$BOOTSTRAP_KEY" "$ROOT_DIR/synonym_set.json" "$ROOT_DIR/synonym_create.json"
  cat >"$ROOT_DIR/unrelated_synonym_set.json" <<'JSON'
{"items":[{"id":"external_synonym","root":"external","synonyms":["outside-regex"]}]}
JSON
  expect_http 200 PUT "/synonym_sets/$UNRELATED_SYNONYM_SET" "$BOOTSTRAP_KEY" "$ROOT_DIR/unrelated_synonym_set.json" "$ROOT_DIR/unrelated_synonym_create.json"
  cat >"$ROOT_DIR/curation_set.json" <<'JSON'
{"items":[{"id":"pin_espresso","rule":{"query":"espresso","match":"exact"},"includes":[{"id":"prod_001","position":1}],"excludes":[{"id":"prod_002"}]}]}
JSON
  expect_http 200 PUT "/curation_sets/$CURATION_SET" "$BOOTSTRAP_KEY" "$ROOT_DIR/curation_set.json" "$ROOT_DIR/curation_create.json"
}

seed_alias() {
  cat >"$ROOT_DIR/alias.json" <<JSON
{"collection_name":"$PRODUCTS"}
JSON
  expect_http 200 PUT "/aliases/$ALIAS_NAME" "$BOOTSTRAP_KEY" "$ROOT_DIR/alias.json" "$ROOT_DIR/alias_create.json"
}

create_capture_key() {
  cat >"$ROOT_DIR/capture_key_request.json" <<JSON
{"description":"$CAPTURE_KEY_DESCRIPTION","actions":["collections:list","collections:get","documents:export","aliases:list","aliases:get","synonym_sets:list","synonym_sets:get","synonym_sets/items:list","synonym_sets/items:get","curation_sets:list","curation_sets:get","curation_sets/items:list","curation_sets/items:get","debug:list"],"collections":["fj_ts_migration_.*"]}
JSON
  expect_http 201 POST /keys "$BOOTSTRAP_KEY" "$ROOT_DIR/capture_key_request.json" "$ROOT_DIR/capture_key_response.json"
  SCOPED_KEY="$(jq -r '.value' "$ROOT_DIR/capture_key_response.json")"
  [ -n "$SCOPED_KEY" ] && [ "$SCOPED_KEY" != null ] || fail "scoped capture key creation did not return a key"
  remember_secret SCOPED "$SCOPED_KEY"
}

permission_controls() {
  local out="$ROOT_DIR/permission_control.json" code
  for path in /collections "/collections/$PRODUCTS" "/collections/$PRODUCTS/documents/export" /aliases "/aliases/$ALIAS_NAME" /synonym_sets "/synonym_sets/$SYNONYM_SET" "/synonym_sets/$SYNONYM_SET/items" "/synonym_sets/$SYNONYM_SET/items/espresso_synonym" "/synonym_sets/$UNRELATED_SYNONYM_SET" /curation_sets "/curation_sets/$CURATION_SET" "/curation_sets/$CURATION_SET/items" "/curation_sets/$CURATION_SET/items/pin_espresso" /debug; do
    code="$(http_json GET "$path" "$SCOPED_KEY" "" "$out")"
    [ "$code" = 200 ] || fail "least-privilege read action failed for $path with $code"
  done
  cat >"$ROOT_DIR/export_only_key_request.json" <<'JSON'
{"description":"fj-stage2-typesense-export-only","actions":["documents:export"],"collections":["fj_ts_migration_.*"]}
JSON
  expect_http 201 POST /keys "$BOOTSTRAP_KEY" "$ROOT_DIR/export_only_key_request.json" "$ROOT_DIR/export_key_response.json"
  EXPORT_KEY="$(jq -r '.value' "$ROOT_DIR/export_key_response.json")"
  remember_secret EXPORT "$EXPORT_KEY"
  code="$(http_json GET /collections "$EXPORT_KEY" "" "$out")"
  [ "$code" = 401 ] || fail "narrower key unexpectedly listed collections"
  code="$(http_json GET '/collections?limit=1' "$SCOPED_KEY" "" "$out")"
  [ "$code" = 200 ] || fail "least-privilege discovery listing failed with $code"
  code="$(http_json GET '/collections?limit=1' "$EXPORT_KEY" "" "$out")"
  [ "$code" = 401 ] || fail "narrower key unexpectedly listed a discovery slice"
  code="$(http_json GET /keys "$SCOPED_KEY" "" "$out")"
  [ "$code" = 401 ] || fail "capture key unexpectedly read /keys"
}

mutate_collection_listing_for_test() {
  local family="$1" listing="$2"
  case "${FJ_TYPESENSE_CONTRACT_MUTATION:-}" in
    wrong_discovery_name_set)
      [ "$family" = full ] || return 0
      jq '.[0].name = "fj_ts_migration_wrong"' "$listing" >"$listing.mutated"
      mv "$listing.mutated" "$listing"
      ;;
    wrong_discovery_order)
      [ "$family" = full ] || return 0
      jq 'reverse' "$listing" >"$listing.mutated"
      mv "$listing.mutated" "$listing"
      ;;
    wrong_discovery_slice)
      [ "$family" = offset_without_limit ] || return 0
      jq '[]' "$listing" >"$listing.mutated"
      mv "$listing.mutated" "$listing"
      ;;
  esac
}

assert_collection_listing_discovery_contract() {
  local full="$ROOT_DIR/discovery_collections.json"
  local first="$ROOT_DIR/discovery_limit_one.json"
  local second="$ROOT_DIR/discovery_offset_one_limit_one.json"
  local offset_without_limit="$ROOT_DIR/discovery_offset_one.json"
  local exhausted="$ROOT_DIR/discovery_offset_two.json"

  expect_http 200 GET /collections "$SCOPED_KEY" "" "$full"
  expect_http 200 GET '/collections?limit=1' "$SCOPED_KEY" "" "$first"
  expect_http 200 GET '/collections?offset=1&limit=1' "$SCOPED_KEY" "" "$second"
  expect_http 200 GET '/collections?offset=1' "$SCOPED_KEY" "" "$offset_without_limit"
  expect_http 400 GET '/collections?offset=2&limit=1' "$SCOPED_KEY" "" "$exhausted"
  mutate_collection_listing_for_test full "$full"
  mutate_collection_listing_for_test offset_without_limit "$offset_without_limit"

  jq -e --arg products "$PRODUCTS" --arg categories "$CATEGORIES" \
    'map(.name) | sort == ([$products, $categories] | sort)' "$full" >/dev/null \
    || fail "discovery name set mismatch rejected"
  jq -e --arg products "$PRODUCTS" --arg categories "$CATEGORIES" \
    'map(.name) == [$products, $categories]' "$full" >/dev/null \
    || fail "discovery newest-first order mismatch rejected"
  jq -e --arg products "$PRODUCTS" 'map(.name) == [$products]' "$first" >/dev/null \
    && jq -e --arg categories "$CATEGORIES" 'map(.name) == [$categories]' "$second" >/dev/null \
    && jq -e --arg categories "$CATEGORIES" 'map(.name) == [$categories]' "$offset_without_limit" >/dev/null \
    && jq -e '. == {"message":"Invalid offset param."}' "$exhausted" >/dev/null \
    || fail "discovery offset/limit slice mismatch rejected"
}

product_count() {
  local out="$ROOT_DIR/product_count.json"
  expect_http 200 GET "/collections/$PRODUCTS/documents/search?q=*&query_by=title&per_page=0" "$BOOTSTRAP_KEY" "" "$out"
  jq -r '.found' "$out"
}

attempt_source_mutation_during_capture() {
  [ "${FJ_TYPESENSE_CONTRACT_MUTATION:-}" = source_mutation_during_capture ] || return 0
  local before after code
  before="$(product_count)"
  cat >"$ROOT_DIR/source_mutation_patch.json" <<'JSON'
{"inventory":43}
JSON
  code="$(http_json PATCH "/collections/$PRODUCTS/documents/prod_001" "$BOOTSTRAP_KEY" "$ROOT_DIR/source_mutation_patch.json" "$ROOT_DIR/source_mutation_response.json")"
  after="$(product_count)"
  {
    printf 'mutation_http_code=%s\n' "$code"
    printf 'count_before=%s\n' "$before"
    printf 'count_after=%s\n' "$after"
  } >"$ROOT_DIR/mutation_observation.txt"
  [ "$code" = 200 ] || fail "source mutation setup failed: public document update returned $code"
  [ "$before" = "$after" ] || fail "source mutation setup failed: document count changed from $before to $after"
  fail "source mutation rejected: explicit write-freeze attestation was violated"
}

export_collection() {
  local collection="$1" out parsed last_byte
  out="$ROOT_DIR/export_${collection}.jsonl"
  parsed="$ROOT_DIR/export_${collection}.json"
  expect_http 200 GET "/collections/$collection/documents/export" "$SCOPED_KEY" "" "$out"
  corrupt_export_stream_for_test "$collection" "$out"
  [ -s "$out" ] || fail "export for $collection was empty"
  last_byte="$(tail -c 1 "$out" | od -An -t u1 | tr -d ' ')"
  [ "$last_byte" = 125 ] || fail "export for $collection did not end with a JSON object"
  jq -s -e 'if all(.[]; type == "object" and (has("error") | not)) then . else error("malformed export") end' "$out" >"$parsed" \
    || fail "export for $collection contained malformed JSON or an in-stream error object"
}

corrupt_export_stream_for_test() {
  local collection="$1" export_stream="$2" byte_count truncated_stream
  [ "${FJ_TYPESENSE_CONTRACT_MUTATION:-}" = truncated_export ] || return 0
  [ "$collection" = "$CATEGORIES" ] || return 0
  byte_count="$(wc -c <"$export_stream" | tr -d ' ')"
  [ "$byte_count" -gt 1 ] || fail "truncated export setup requires a non-empty raw export stream"
  truncated_stream="${export_stream}.truncated"
  dd if="$export_stream" of="$truncated_stream" bs=1 count=$((byte_count - 1)) 2>/dev/null
  mv "$truncated_stream" "$export_stream"
}

capture_bundle() {
  expect_http 200 GET /debug "$SCOPED_KEY" "" "$ROOT_DIR/debug.json"
  expect_http 200 GET /health "$SCOPED_KEY" "" "$ROOT_DIR/health_capture.json"
  expect_http 200 GET /aliases "$SCOPED_KEY" "" "$ROOT_DIR/aliases.json"
  expect_http 200 GET /synonym_sets "$SCOPED_KEY" "" "$ROOT_DIR/synonym_sets.json"
  expect_http 200 GET "/synonym_sets/$SYNONYM_SET/items" "$SCOPED_KEY" "" "$ROOT_DIR/synonym_items.json"
  expect_http 200 GET /curation_sets "$SCOPED_KEY" "" "$ROOT_DIR/curation_sets.json"
  expect_http 200 GET "/curation_sets/$CURATION_SET/items" "$SCOPED_KEY" "" "$ROOT_DIR/curation_items.json"
  expect_http 200 GET "/collections/$PRODUCTS" "$SCOPED_KEY" "" "$ROOT_DIR/schema_products.json"
  expect_http 200 GET "/collections/$CATEGORIES" "$SCOPED_KEY" "" "$ROOT_DIR/schema_categories.json"
  export_collection "$PRODUCTS"
  attempt_source_mutation_during_capture
  export_collection "$CATEGORIES"
  jq -n \
    --arg image_ref "$IMAGE_REF" \
    --arg image_digest "$IMAGE_DIGEST" \
    --arg synonym_set "$SYNONYM_SET" \
    --arg curation_set "$CURATION_SET" \
    --arg unrelated_synonym_set "$UNRELATED_SYNONYM_SET" \
    --slurpfile health "$ROOT_DIR/health_capture.json" \
    --slurpfile debug "$ROOT_DIR/debug.json" \
    --slurpfile products_schema "$ROOT_DIR/schema_products.json" \
    --slurpfile categories_schema "$ROOT_DIR/schema_categories.json" \
    --slurpfile products_docs "$ROOT_DIR/export_${PRODUCTS}.json" \
    --slurpfile categories_docs "$ROOT_DIR/export_${CATEGORIES}.json" \
    --slurpfile aliases "$ROOT_DIR/aliases.json" \
    --slurpfile synonym_sets "$ROOT_DIR/synonym_sets.json" \
    --slurpfile synonym_items "$ROOT_DIR/synonym_items.json" \
    --slurpfile curation_items "$ROOT_DIR/curation_items.json" \
    'def bool_default($key; $default):
       if has($key) and .[$key] != null then .[$key] else $default end;
     def field_norm:
       {name,type,facet:bool_default("facet"; false),optional:bool_default("optional"; false),index:bool_default("index"; true),store:bool_default("store"; true),sort:bool_default("sort"; (if (.type | test("^(int|float)")) then true else false end))}
       + (if has("num_dim") then {num_dim} else {} end)
       + (if has("vec_dist") then {vec_dist} else {} end)
       + (if has("reference") then {reference} else {} end);
     def least_privilege_actions:["collections:list","collections:get","documents:export","aliases:list","aliases:get","synonym_sets:list","synonym_sets:get","synonym_sets/items:list","synonym_sets/items:get","curation_sets:list","curation_sets:get","curation_sets/items:list","curation_sets/items:get","debug:list"];
     def unsupported_findings($product_fields):[
       {code:"typesense_api_keys_unsupported", detail:"Capture and export key values are created only for permission controls and are redacted from bundles and evidence."},
       (if any($product_fields[]; has("num_dim") or has("vec_dist")) then {code:"typesense_vectors_unsupported", detail:"Vector field sentinel is preserved as unsupported provider metadata only."} else empty end),
       (if any($product_fields[]; has("reference")) then {code:"typesense_references_unsupported", detail:"Reference field sentinel is preserved as unsupported provider metadata only."} else empty end),
       {code:"typesense_analytics_unsupported", detail:"Analytics APIs are not part of the source capture contract."},
       {code:"typesense_quiescence_requires_write_freeze", detail:"A green capture requires explicit write-freeze attestation; counts and timestamps are diagnostics only."}
     ];
     def warning_findings:[
       {code:"typesense_schema_requires_translation", detail:"Provider schema is captured exactly; target mapping is a later policy decision."},
       {code:"typesense_alias_requires_translation", detail:"Alias target is captured without assuming target-side alias lifecycle."},
       {code:"typesense_synonym_sets_require_translation", detail:"Global synonym set identity and linked collection membership must be preserved."},
       {code:"typesense_curation_sets_require_translation", detail:"Global curation set identity and linked collection membership must be preserved."},
       {code:"typesense_export_stream_not_newline_counted", detail:"Document export is parsed as JSON values and may omit a terminal newline."}
     ];
     def set_list($value): if ($value | type) == "array" then $value else ($value.synonym_sets // $value.curation_sets // []) end;
     ($products_schema[0].fields | map(field_norm)) as $product_fields |
     (set_list($synonym_sets[0])) as $visible_synonym_sets |
     {contract:{
        capture_requires_write_freeze:true,
        fixture_version:"2026_07_26_m0b_typesense_migration",
        image_digest:$image_digest,
        image_reference:$image_ref,
        least_privilege_actions:least_privilege_actions,
        unsupported_codes:(unsupported_findings($product_fields) | map(.code)),
        warning_codes:(warning_findings | map(.code))
      },
      source:{
        aliases:($aliases[0].aliases | map({name, collection_name}) | sort_by(.name)),
        collections:[
          ($products_schema[0] | {name, default_sorting_field, enable_nested_fields:(.enable_nested_fields // false), token_separators:(.token_separators // []), symbols_to_index:(.symbols_to_index // []), synonym_sets:(.synonym_sets // []), curation_sets:(.curation_sets // []), fields:$product_fields, documents:($products_docs[0] | sort_by(.id))}),
          ($categories_schema[0] | {name, default_sorting_field, enable_nested_fields:(.enable_nested_fields // false), token_separators:(.token_separators // []), symbols_to_index:(.symbols_to_index // []), synonym_sets:(.synonym_sets // []), curation_sets:(.curation_sets // []), fields:(.fields | map(field_norm)), documents:($categories_docs[0] | sort_by(.id))})
        ] | sort_by(.name),
        synonym_sets:[{name:$synonym_set, items:(if ($synonym_items[0] | type) == "array" then $synonym_items[0] else ($synonym_items[0].items // []) end | map({id, root, synonyms}) | sort_by(.id))}],
        curation_sets:[{name:$curation_set, items:(if ($curation_items[0] | type) == "array" then $curation_items[0] else ($curation_items[0].items // $curation_items[0].curations // $curation_items[0].overrides // []) end | map({id, rule, includes, excludes}) | sort_by(.id))}],
        provider_evidence:{
          health:{ok:$health[0].ok},
          debug:{state:$debug[0].state, version:$debug[0].version},
          global_resource_visibility:{
            unrelated_synonym_set:$unrelated_synonym_set,
            visible_to_capture_key:(any($visible_synonym_sets[]?; .name == $unrelated_synonym_set)),
            returned_synonym_set_names:($visible_synonym_sets | map(.name) | sort)
          }
        },
        unsupported_findings:unsupported_findings($product_fields),
        warning_findings:warning_findings
      }}' >"$ROOT_DIR/actual_bundle.json"
}

apply_test_mutation() {
  local mutation="${FJ_TYPESENSE_CONTRACT_MUTATION:-}"
  [ -n "$mutation" ] || return 0
  case "$mutation" in
    wrong_record_value_and_count)
      jq '(.source.collections[] | select(.name=="fj_ts_migration_products") | .documents[0].price)=130.95 | (.source.collections[] | select(.name=="fj_ts_migration_products") | .documents)+=[{"id":"extra"}]' "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/mutated.json" ;;
    dropped_id)
      jq 'del(.source.collections[] | select(.name=="fj_ts_migration_products") | .documents[0].id)' "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/mutated.json" ;;
    changed_schema_and_default_sort)
      jq '(.source.collections[] | select(.name=="fj_ts_migration_products") | .default_sorting_field)="inventory" | del(.source.collections[] | select(.name=="fj_ts_migration_products") | .fields[] | select(.name=="metadata.color").facet)' "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/mutated.json" ;;
    missing_synonym_and_curation)
      jq '(.source.synonym_sets[0].items)=[] | (.source.curation_sets[0].items)=[]' "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/mutated.json" ;;
    wrong_alias_target)
      jq '(.source.aliases[] | select(.name=="fj_ts_migration_catalog") | .collection_name)="fj_ts_migration_categories"' "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/mutated.json" ;;
    wrong_discovery_name_set|wrong_discovery_order|wrong_discovery_slice)
      fail "discovery mutation did not run during collection listing" ;;
    truncated_export)
      : ;;
    source_mutation_during_capture)
      fail "source mutation case did not run during active capture" ;;
    credential_leakage)
      jq --arg leaked "$BOOTSTRAP_KEY" '.source.warning_findings += [{"code":"credential_leak","detail":$leaked}]' "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/mutated.json" ;;
    cleanup_residue)
      mkdir -p "$(dirname "$RESIDUE_MARKER")"
      printf '%s\n' residue >"$RESIDUE_MARKER" ;;
    *) fail "unknown test mutation: $mutation" ;;
  esac
  if [ -f "$ROOT_DIR/mutated.json" ]; then
    mv "$ROOT_DIR/mutated.json" "$ROOT_DIR/actual_bundle.json"
  fi
}

assert_no_secret_leakage() {
  if grep -R -E 'TYPESENSE_STAGE2_BOOTSTRAP_CANARY|TYPESENSE_STAGE2_SCOPED_CANARY' "$FIXTURE_DIR" "$ROOT_DIR/actual_bundle.json" >/dev/null 2>&1; then
    fail "credential leakage rejected: committed fixture or bundle contained a key sentinel"
  fi
}

assert_no_cleanup_residue() {
  [ ! -e "$RESIDUE_MARKER" ] \
    || fail "cleanup residue rejected: exact stage-owned residue marker remained"
}

validate_bundle() {
  jq -S . "$EXPECTED_BUNDLE" >"$ROOT_DIR/expected.sorted.json"
  jq -S . "$ROOT_DIR/actual_bundle.json" >"$ROOT_DIR/actual.sorted.json"
  if ! diff -u "$ROOT_DIR/expected.sorted.json" "$ROOT_DIR/actual.sorted.json" >"$ROOT_DIR/bundle.diff"; then
    if ! jq -e 'all(.source.collections[].documents[]; has("id"))' "$ROOT_DIR/actual_bundle.json" >/dev/null; then
      fail "dropped id rejected: exported document id did not match expected_bundle.json"
    elif ! jq -n -e --slurpfile expected "$EXPECTED_BUNDLE" --slurpfile actual "$ROOT_DIR/actual_bundle.json" \
      '$expected[0].source.aliases == $actual[0].source.aliases' >/dev/null; then
      fail "alias target mismatch rejected: alias mapping differed from expected_bundle.json"
    elif ! jq -n -e --slurpfile expected "$EXPECTED_BUNDLE" --slurpfile actual "$ROOT_DIR/actual_bundle.json" \
      '$expected[0].source.synonym_sets == $actual[0].source.synonym_sets and $expected[0].source.curation_sets == $actual[0].source.curation_sets' >/dev/null; then
      fail "synonym/curation mismatch rejected: linked global set data differed from expected_bundle.json"
    elif ! jq -n -e --slurpfile expected "$EXPECTED_BUNDLE" --slurpfile actual "$ROOT_DIR/actual_bundle.json" '
      def schema_only: .source.collections | map(del(.documents));
      ($expected[0] | schema_only) == ($actual[0] | schema_only)
    ' >/dev/null; then
      fail "schema/default sort mismatch rejected: provider schema differed from expected_bundle.json"
    elif ! jq -n -e --slurpfile expected "$EXPECTED_BUNDLE" --slurpfile actual "$ROOT_DIR/actual_bundle.json" '
      def docs_by_collection: .source.collections | map({name, documents});
      ($expected[0] | docs_by_collection) == ($actual[0] | docs_by_collection)
    ' >/dev/null; then
      fail "record value/count mismatch rejected: actual bundle differed from expected_bundle.json"
    else
      fail "bundle mismatch rejected: actual bundle differed from expected_bundle.json"
    fi
  fi
}

start_counting_proxy() {
  cat >"$ROOT_DIR/counting_proxy.py" <<'PY'
import http.client
import os
import pathlib
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
upstream_port = int(os.environ["UPSTREAM_PORT"])
count_file = pathlib.Path(os.environ["COUNT_FILE"])
count_lock = threading.Lock()
class Handler(BaseHTTPRequestHandler):
    def relay(self):
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length) if length else None
        headers = {key: value for key, value in self.headers.items()
                   if key.lower() not in {"host", "connection", "content-length"}}
        with count_lock:
            with count_file.open("a", encoding="utf-8") as requests:
                requests.write(f"{self.command} {self.path}\n")
        connection = http.client.HTTPConnection("127.0.0.1", upstream_port, timeout=120)
        connection.request(self.command, self.path, body=body, headers=headers)
        response = connection.getresponse()
        payload = response.read()
        self.send_response(response.status)
        for key, value in response.getheaders():
            if key.lower() not in {"connection", "content-length", "transfer-encoding"}:
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)
        connection.close()
    do_GET = relay
    do_POST = relay
    do_PUT = relay
    do_PATCH = relay
    def log_message(self, *_args):
        pass
server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
pathlib.Path(os.environ["PORT_FILE"]).write_text(str(server.server_port), encoding="utf-8")
server.serve_forever()
PY
  : >"$ROOT_DIR/typesense_proxy_requests.txt"
  UPSTREAM_PORT="$PORT" COUNT_FILE="$ROOT_DIR/typesense_proxy_requests.txt" \
    PORT_FILE="$ROOT_DIR/proxy_port.txt" python3 "$ROOT_DIR/counting_proxy.py" \
    >"$ROOT_DIR/proxy.log" 2>&1 &
  PROXY_PID=$!
  for _ in $(seq 1 40); do
    [ -s "$ROOT_DIR/proxy_port.txt" ] && break
    kill -0 "$PROXY_PID" 2>/dev/null || fail "Typesense counting proxy exited during startup"
    sleep 0.1
  done
  PROXY_PORT="$(cat "$ROOT_DIR/proxy_port.txt" 2>/dev/null || true)"
  [[ "$PROXY_PORT" =~ ^[0-9]+$ ]] || fail "Typesense counting proxy did not publish a port"
}

start_flapjack_server() {
  cargo build -p flapjack-server >"$ROOT_DIR/flapjack_build.log" 2>&1
  local binary="$(pwd)/target/debug/flapjack"
  [ -x "$binary" ] || fail "flapjack-server build did not produce $binary"
  FLAPJACK_ADMIN_KEY_VALUE="fj-typesense-contract-$(openssl rand -hex 24)"
  remember_secret FLAPJACK_ADMIN "$FLAPJACK_ADMIN_KEY_VALUE"
  FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1 \
    FLAPJACK_ADMIN_KEY="$FLAPJACK_ADMIN_KEY_VALUE" \
    FLAPJACK_DATA_DIR="$ROOT_DIR/flapjack_data" \
    "$binary" --auto-port >"$ROOT_DIR/flapjack_server.log" 2>&1 &
  FLAPJACK_PID=$!
  tests/common/wait_for_flapjack.sh --pid "$FLAPJACK_PID" --host 127.0.0.1 --port auto \
    --log-path "$ROOT_DIR/flapjack_server.log" --retries 80 --interval-seconds 0.25
  local server_port
  server_port="$(sed -n -E 's/.*Local:.*http:\/\/(\[::\]|0\.0\.0\.0|127\.0\.0\.1):([0-9]+).*/\2/p' "$ROOT_DIR/flapjack_server.log" | head -1)"
  [ -n "$server_port" ] || fail "flapjack-server became ready without an auto-port"
  FLAPJACK_URL="http://127.0.0.1:${server_port}"
}

served_request() {
  local method="$1" path="$2" body="$3" out="$4"
  curl -sS --connect-timeout 2 --max-time 120 -o "$out" -w '%{http_code}' -X "$method" \
    -H 'x-algolia-application-id: flapjack' -H "x-algolia-api-key: $FLAPJACK_ADMIN_KEY_VALUE" \
    -H 'content-type: application/json' --data-binary @"$body" "$FLAPJACK_URL$path"
}

source_request_count() {
  wc -l <"$ROOT_DIR/typesense_proxy_requests.txt" | tr -d ' '
}

write_freeze_body() {
  local attestation="$1" target="$2" out="$3"
  jq -n --arg node "http://127.0.0.1:$PROXY_PORT" --arg key "$SCOPED_KEY" \
    --arg source "$PRODUCTS" --arg target "$target" --arg attestation "$attestation" '
      {node:$node,apiKey:$key,sourceIndex:$source,targetIndex:$target,overwrite:false}
      + (if $attestation == "missing" then {} else {sourceWriteFrozen:($attestation == "true")} end)
    ' >"$out"
}

wait_for_submit_capture() {
  local job_id="$1" out="$2" disposition=""
  for _ in $(seq 1 240); do
    disposition="$(curl -sS --connect-timeout 2 --max-time 10 \
      -H 'x-algolia-application-id: flapjack' -H "x-algolia-api-key: $FLAPJACK_ADMIN_KEY_VALUE" \
      "$FLAPJACK_URL/1/migrations/typesense/$job_id" | tee "$out" | jq -r '.disposition // empty')"
    [ "$disposition" = succeeded ] && break
    [ "$disposition" != failed ] && [ "$disposition" != cancelled ] \
      || fail "attested Typesense submit ended with $disposition"
    sleep 0.25
  done
  [ "$disposition" = succeeded ] || fail "attested Typesense submit did not complete"
  jq -e '.objectsImported.imported == 137' "$out" >/dev/null \
    || fail "attested Typesense submit did not import 137 documents"
}

probe_supported_write_freeze_arm() {
  local endpoint="$1" attestation="$2" body out code before after target path
  target="fj_ts_write_freeze_${endpoint}_target"
  body="$ROOT_DIR/${endpoint}_${attestation}_request.json"
  out="$ROOT_DIR/${endpoint}_${attestation}_response.json"
  write_freeze_body "$attestation" "$target" "$body"
  before="$(source_request_count)"
  [ "$endpoint" = preview ] && path=/1/migrations/typesense/preview || path=/1/migrations/typesense
  code="$(served_request POST "$path" "$body" "$out")"
  if [ "$attestation" != true ]; then
    [ "$code" = 400 ] || fail "$endpoint $attestation attestation returned $code"
    jq -e '.message | contains("external write freeze/attestation")' "$out" >/dev/null \
      || fail "$endpoint $attestation attestation returned the wrong refusal"
    [ "$(source_request_count)" = "$before" ] || fail "$endpoint $attestation reached Typesense"
    [ "$attestation" = missing ] && MISSING_REFUSED=$((MISSING_REFUSED + 1)) \
      || FALSE_REFUSED=$((FALSE_REFUSED + 1))
    ZERO_SOURCE_REQUESTS=$((ZERO_SOURCE_REQUESTS + 1))
    return
  fi
  [ "$code" = 200 ] || [ "$code" = 202 ] || fail "$endpoint true attestation returned $code"
  if [ "$endpoint" = preview ]; then
    jq -e '.sourceCounts == {indexes:1,records:137}' "$out" >/dev/null \
      || fail "attested Typesense preview did not observe 137 documents"
  else
    wait_for_submit_capture "$(jq -r '.jobId' "$out")" "$ROOT_DIR/submit_true_status.json"
  fi
  after="$(source_request_count)"
  [ "$after" -gt "$before" ] || fail "$endpoint true attestation did not reach Typesense"
  TRUE_PASSED=$((TRUE_PASSED + 1))
}

probe_resume_write_freeze_arm() {
  local attestation="$1" body out before code
  body="$ROOT_DIR/resume_${attestation}_request.json"
  out="$ROOT_DIR/resume_${attestation}_response.json"
  write_freeze_body "$attestation" fj_ts_write_freeze_resume_target "$body"
  before="$(source_request_count)"
  code="$(served_request POST '/1/migrations/typesense/01890f8e-8b28-78e8-b542-8cfdcb2d4f24/resume' "$body" "$out")"
  [ "$code" = 400 ] && jq -e '.code == "source_provider_unsupported"' "$out" >/dev/null \
    || fail "Typesense resume $attestation did not return source_provider_unsupported"
  [ "$(source_request_count)" = "$before" ] || fail "Typesense resume $attestation reached the source"
  RESUME_UNSUPPORTED=$((RESUME_UNSUPPORTED + 1))
}

assert_served_write_freeze_contract() {
  if [ "${FJ_TYPESENSE_RUN_SERVED_WRITE_FREEZE_CONTRACT:-1}" != 1 ]; then
    printf 'SKIP: served write-freeze contract explicitly disabled for harness self-tests\n'
    return
  fi
  MISSING_REFUSED=0 FALSE_REFUSED=0 ZERO_SOURCE_REQUESTS=0 TRUE_PASSED=0 RESUME_UNSUPPORTED=0
  start_counting_proxy
  start_flapjack_server
  for endpoint in $WRITE_FREEZE_SUPPORTED_ENDPOINTS; do
    for attestation in $WRITE_FREEZE_ATTESTATION_ARMS; do
      probe_supported_write_freeze_arm "$endpoint" "$attestation"
    done
  done
  for attestation in $WRITE_FREEZE_RESUME_ARMS; do
    probe_resume_write_freeze_arm "$attestation"
  done
  [ "$MISSING_REFUSED $FALSE_REFUSED $ZERO_SOURCE_REQUESTS $TRUE_PASSED $RESUME_UNSUPPORTED" = '2 2 4 2 3' ] \
    || fail "served write-freeze denominator mismatch"
  stop_local_processes
  printf 'TYPESENSE_WRITE_FREEZE_CONTRACT endpoints=preview,submit missing_refused=2 false_refused=2 zero_source_requests=4 true_passed=2 resume_unsupported=3 resume_source_requests=0 documents=137\n'
}

main() {
  cd "$(repo_root)/engine"
  require_tools
  [ "${FJ_TYPESENSE_WRITE_FREEZE_ATTESTED:-0}" = 1 ] \
    || fail "write-freeze attestation required before capture"
  [ -f "$EXPECTED_BUNDLE" ] || fail "missing expected bundle: $EXPECTED_BUNDLE"
  mkdir -p "$ROOT_DIR"
  derive_expected_product_ids
  remember_secret BOOTSTRAP "$BOOTSTRAP_KEY"
  start_typesense
  seed_linked_sets
  seed_collections
  seed_alias
  create_capture_key
  permission_controls
  assert_served_write_freeze_contract
  assert_collection_listing_discovery_contract
  run_production_export_stream_contract
  capture_bundle
  apply_test_mutation
  assert_no_secret_leakage
  validate_bundle
  assert_no_cleanup_residue
  docker rm -f "$CONTAINER_NAME" >/dev/null
  rm -rf "$DATA_DIR"
  if docker ps -a --filter "name=^/${CONTAINER_NAME}$" --format '{{.Names}}' | grep -Fxq "$CONTAINER_NAME"; then
    fail "cleanup residue rejected: exact container remained"
  fi
  touch "$PASS_MARKER"
  printf 'PASS: Typesense migration source contract KAT verified\n'
  printf 'image=%s@%s expected_bundle=%s\n' "$IMAGE_REF" "$IMAGE_DIGEST" "$EXPECTED_BUNDLE"
}

main "$@"
