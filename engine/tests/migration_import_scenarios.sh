# shellcheck shell=bash

# Scenario implementations sourced by migration_import_contract.sh. The caller
# owns strict mode, transport helpers, evidence paths, and cleanup state.

# ---------------------------------------------------------------------------
# Async job scenario: disposable fj_async_ Algolia fixture plus the live
# submit/poll/verify oracle for POST /1/migrations/algolia.
# ---------------------------------------------------------------------------

# Emits the exact documents the async fixture seeds. This is the single source of
# truth for both the seeded corpus and the post-migration content assertions, so
# the oracle can never verify against expectations that drifted from the fixture.
async_fixture_documents() {
  cat <<'JSON'
[
  {"objectID":"fj-async-1","name":"Alpha async record","price":11},
  {"objectID":"fj-async-2","name":"Beta async record","price":22},
  {"objectID":"fj-async-3","name":"Gamma async record","price":33}
]
JSON
}

async_fixture_document_count() {
  async_fixture_documents | jq 'length'
}

async_fixture_rule_anchor_object_id() {
  printf '%s\n' "fj-async-1"
}

async_fixture_settings() {
  jq -cn --argjson pagination_limited_to "$(async_fixture_document_count)" '
    {
      searchableAttributes:["name","description","category"],
      customRanking:["desc(popularity)"],
      attributesForFaceting:["category","color"],
      paginationLimitedTo:$pagination_limited_to
    }'
}

async_fixture_synonyms() {
  cat <<'JSON'
[
  {"objectID":"synonym-trainer","type":"synonym","synonyms":["trainer","sneaker"]}
]
JSON
}

async_fixture_synonym_count() {
  async_fixture_synonyms | jq 'length'
}

async_fixture_rules() {
  jq -cn --arg rule_object_id "$(async_fixture_rule_anchor_object_id)" '
    [
      {
        objectID:"rule-promote",
        conditions:[{pattern:"trail",anchoring:"is"}],
        consequence:{promote:[{objectID:$rule_object_id,position:0}]}
      },
      {
        objectID:"rule-hide",
        conditions:[{pattern:"rain",anchoring:"is"}],
        consequence:{hide:[{objectID:$rule_object_id}]}
      }
    ]'
}

async_fixture_rule_count() {
  async_fixture_rules | jq 'length'
}

async_expected_status_warnings() {
  cat <<'JSON'
[
  {
    "code": "PersistedNoBehaviorSetting",
    "message": "Source setting is preserved for compatibility but has no Flapjack behavior.",
    "resource": "Settings",
    "jsonPath": "$.attributesToHighlight"
  },
  {
    "code": "PersistedNoBehaviorSetting",
    "message": "Source setting is preserved for compatibility but has no Flapjack behavior.",
    "resource": "Settings",
    "jsonPath": "$.attributesToSnippet"
  },
  {
    "code": "PersistedNoBehaviorSetting",
    "message": "Source setting is preserved for compatibility but has no Flapjack behavior.",
    "resource": "Settings",
    "jsonPath": "$.highlightPostTag"
  },
  {
    "code": "PersistedNoBehaviorSetting",
    "message": "Source setting is preserved for compatibility but has no Flapjack behavior.",
    "resource": "Settings",
    "jsonPath": "$.highlightPreTag"
  },
  {
    "code": "PersistedNoBehaviorSetting",
    "message": "Source setting is preserved for compatibility but has no Flapjack behavior.",
    "resource": "Settings",
    "jsonPath": "$.hitsPerPage"
  },
  {
    "code": "PersistedNoBehaviorSetting",
    "message": "Source setting is preserved for compatibility but has no Flapjack behavior.",
    "resource": "Settings",
    "jsonPath": "$.optionalWords"
  },
  {
    "code": "ReadOnlySourceField",
    "message": "Source field is read-only in Flapjack and is not applied during migration.",
    "resource": "Settings",
    "jsonPath": "$.synonyms"
  },
  {
    "code": "ReadOnlySourceField",
    "message": "Source field is read-only in Flapjack and is not applied during migration.",
    "resource": "Settings",
    "jsonPath": "$.version"
  }
]
JSON
}

async_expected_status_warning_count() {
  async_expected_status_warnings | jq 'length'
}

# Resolves one index name from its flag and environment inputs into
# ASYNC_RESOLVED_INDEX. Assigning a global rather than printing keeps `die` fatal:
# inside a command substitution the exit would only kill the subshell.
resolve_async_index_name() {
  local role="$1" flag_value="$2" env_name="$3" env_value="${!3:-}" resolved
  if [ -n "$flag_value" ] && [ -n "$env_value" ] && [ "$flag_value" != "$env_value" ]; then
    die "--${role}-index and ${env_name} disagree" 2
  fi
  resolved="${flag_value:-$env_value}"
  if [ -z "$resolved" ]; then
    resolved="${ASYNC_INDEX_PREFIX}${role}_$(date +%s)_$$_$((RANDOM % 100000))"
  fi
  case "$resolved" in
    "${ASYNC_INDEX_PREFIX}"*) ;;
    *) die "async ${role} index must start with ${ASYNC_INDEX_PREFIX}" 2 ;;
  esac
  ASYNC_RESOLVED_INDEX="$resolved"
}

# Normalizes async naming inputs into the canonical SOURCE_INDEX/TARGET_INDEX that
# seeding, submission, receipt writing, and cleanup already read.
resolve_async_index_names() {
  resolve_async_index_name source "$SOURCE_INDEX" FJ_ASYNC_SOURCE_INDEX
  SOURCE_INDEX="$ASYNC_RESOLVED_INDEX"
  resolve_async_index_name target "$TARGET_INDEX" FJ_ASYNC_TARGET_INDEX
  TARGET_INDEX="$ASYNC_RESOLVED_INDEX"
  [ "$SOURCE_INDEX" != "$TARGET_INDEX" ] || die "async source and target index names must differ" 2
}

# Performs one live Algolia request and verifies the endpoint-specific status
# plus the common JSON-object response contract. Callers that need to preserve
# control flow during cleanup use this predicate directly.
async_vendor_object_response() {
  local mode="$1" method="$2" path="$3" body="$4" out="$5"
  ASYNC_VENDOR_HTTP_CODE=""
  ASYNC_VENDOR_HTTP_CODE="$(algolia_vendor_request "$mode" "$method" "$path" "$body" "$out")" \
    || return 1
  [ "$ASYNC_VENDOR_HTTP_CODE" = "200" ] || return 1
  jq -e 'type == "object"' "$out" >/dev/null 2>&1
}

# Fatal wrapper for normal async setup requests. Every current Algolia fixture
# endpoint has a documented HTTP 200 response; accepting arbitrary 2xx statuses
# would make the live contract weaker than the endpoint it is exercising.
async_vendor_json() {
  local mode="$1" method="$2" path="$3" body="$4" out="$5"
  async_vendor_object_response "$mode" "$method" "$path" "$body" "$out" \
    || die "async Algolia ${method} request expected HTTP 200 with a JSON object, got ${ASYNC_VENDOR_HTTP_CODE:-transport failure}"
}

async_vendor_task_id() {
  jq -er '
    if type == "object"
      and (.taskID | type) == "number"
      and (.taskID | floor) == .taskID
      and .taskID > 0
    then .taskID else empty end
  ' "$1"
}

async_vendor_index_listing_is_valid() {
  jq -e '(.items | type) == "array"' "$1" >/dev/null 2>&1
}

async_register_algolia_index() {
  ASYNC_OWNED_ALGOLIA_INDICES+=("$1")
}

async_owned_algolia_indices_json() {
  printf '%s\n' "${ASYNC_OWNED_ALGOLIA_INDICES[@]:-}" \
    | jq -Rs 'split("\n") | map(select(length > 0))'
}

async_listed_owned_algolia_indices() {
  local listing="$1" owned="$2"
  jq -r --argjson owned "$owned" '
    .items[]? | .name? | strings | select(. as $name | $owned | index($name))
  ' "$listing"
}

async_listing_excludes_owned_algolia_indices() {
  local listing="$1" owned="$2"
  jq -e --argjson owned "$owned" '
    [.items[]? | .name? | strings | select(. as $name | $owned | index($name))] | length == 0
  ' "$listing" >/dev/null
}

async_log_label() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9_.-' '_'
}

# Deletes one Algolia index and waits for the deletion task to publish. Refuses
# any name outside the async prefix so a caller bug cannot reach a sibling
# lane's fixture. Fails closed: rejects non-200 statuses, missing/invalid
# taskIDs, and unpublished tasks so a vendor error cannot leave stale state.
async_delete_algolia_index() {
  local index="$1" out task
  case "$index" in
    "${ASYNC_INDEX_PREFIX}"*) ;;
    *) return 1 ;;
  esac
  out="$LOG_DIR/async-delete-$(async_log_label "$index").json"
  if ! async_vendor_object_response write DELETE \
    "/1/indexes/$(algolia_vendor_url_encode "$index")" "" "$out"; then
    printf 'WARN: DELETE %s failed — HTTP %s\n' "$index" "${ASYNC_VENDOR_HTTP_CODE:-transport failure}" >&2
    return 1
  fi
  task="$(async_vendor_task_id "$out")"
  if [ -z "$task" ]; then
    printf 'WARN: DELETE %s returned HTTP 200 but lacked a valid taskID\n' "$index" >&2
    return 1
  fi
  algolia_vendor_wait_task "$index" "$task" "$LOG_DIR/async-delete-task-$(async_log_label "$index").json"
}

# Classifies every fj_async_ index the vendor lists as this run's own, provably
# stale, or indeterminate. Only the first two categories are deletable: a recent
# or unparseable-timestamp leftover may belong to a concurrent run, so it is
# recorded as skipped rather than swept.
async_sweep_candidates() {
  local listing="$1" now
  now="$(date +%s)"
  jq -r --arg prefix "$ASYNC_INDEX_PREFIX" \
    --arg source "$SOURCE_INDEX" --arg target "$TARGET_INDEX" \
    --argjson now "$now" --argjson max_age "$ASYNC_STALE_AGE_SECONDS" '
    def freshness:
      (.updatedAt // .createdAt)
      | strings
      | sub("\\.[0-9]+Z$"; "Z")
      | try fromdateiso8601 catch empty;
    .items[]?
    | select((.name | type) == "string")
    | select(.name | startswith($prefix))
    | . as $item
    | ([$item | freshness] | first) as $observed_at
    | if ($item.name == $source or $item.name == $target) then "owned\t\($item.name)"
      elif ($observed_at != null and ($now - $observed_at) > $max_age) then "stale\t\($item.name)"
      else "skipped\t\($item.name)"
      end
  ' "$listing"
}

# Proves the vendor is reachable, then removes exactly this run's names plus
# provably stale fj_async_ leftovers before any seeding happens.
async_preflight_sweep() {
  local listing="$1" disposition index swept=() skipped=()
  async_vendor_json read GET "/1/indexes" "" "$listing"
  async_vendor_index_listing_is_valid "$listing" \
    || die "async Algolia GET /1/indexes response was missing an items array"
  record_check "async_vendor_reachable" "pass" "GET /1/indexes returned 200"

  while IFS=$'\t' read -r disposition index; do
    [ -n "$index" ] || continue
    case "$disposition" in
      owned|stale)
        async_delete_algolia_index "$index" || die "async preflight failed to delete ${index}"
        swept+=("$index")
        ;;
      skipped)
        skipped+=("$index")
        ;;
      *)
        die "async preflight produced an unknown sweep disposition: ${disposition}"
        ;;
    esac
  done < <(async_sweep_candidates "$listing")

  printf 'INFO: async preflight swept=%s skipped=%s\n' \
    "${swept[*]:-none}" "${skipped[*]:-none}"
  record_check "async_preflight_sweep" "pass" \
    "swept=${swept[*]:-none} skipped=${skipped[*]:-none}"
}

async_seed_source_index() {
  local encoded out task
  encoded="$(algolia_vendor_url_encode "$SOURCE_INDEX")"
  async_register_algolia_index "$SOURCE_INDEX"

  out="$LOG_DIR/async-seed-batch.json"
  async_vendor_json write POST "/1/indexes/${encoded}/batch" \
    "$(async_fixture_documents | jq -c '{requests: [.[] | {action:"addObject", body:.}]}')" "$out"
  task="$(async_vendor_task_id "$out")" \
    || die "async seeding response did not carry a valid taskID"
  algolia_vendor_wait_task "$SOURCE_INDEX" "$task" "$LOG_DIR/async-seed-task.json" \
    || die "async seeding task did not publish"

  out="$LOG_DIR/async-seed-settings.json"
  async_vendor_json write PUT "/1/indexes/${encoded}/settings" \
    "$(async_fixture_settings)" "$out"
  task="$(async_vendor_task_id "$out")" \
    || die "async settings response did not carry a valid taskID"
  algolia_vendor_wait_task "$SOURCE_INDEX" "$task" "$LOG_DIR/async-settings-task.json" \
    || die "async settings task did not publish"

  out="$LOG_DIR/async-seed-synonyms.json"
  async_vendor_json write POST "/1/indexes/${encoded}/synonyms/batch" \
    "$(async_fixture_synonyms | jq -c '.')" "$out"
  task="$(async_vendor_task_id "$out")" \
    || die "async synonym response did not carry a valid taskID"
  algolia_vendor_wait_task "$SOURCE_INDEX" "$task" "$LOG_DIR/async-synonyms-task.json" \
    || die "async synonym task did not publish"

  out="$LOG_DIR/async-seed-rules.json"
  async_vendor_json write POST "/1/indexes/${encoded}/rules/batch" \
    "$(async_fixture_rules | jq -c '.')" "$out"
  task="$(async_vendor_task_id "$out")" \
    || die "async rule response did not carry a valid taskID"
  algolia_vendor_wait_task "$SOURCE_INDEX" "$task" "$LOG_DIR/async-rules-task.json" \
    || die "async rule task did not publish"
}

# Asks the vendor itself how many documents the fixture holds. Without this a
# zero-document source would let the whole contract pass by absence.
async_assert_source_seeded() {
  local encoded expected observed out expected_settings expected_synonyms expected_rules observed_synonyms observed_rules
  encoded="$(algolia_vendor_url_encode "$SOURCE_INDEX")"
  expected="$(async_fixture_document_count)"
  [ "$expected" -gt 0 ] || die "async fixture document set is empty"
  out="$LOG_DIR/async-source-count.json"
  async_vendor_json write POST \
    "/1/indexes/${encoded}/query" \
    '{"query":"","hitsPerPage":0}' "$out"
  observed="$(jq -er '
    if (.nbHits | type) == "number" and (.nbHits | floor) == .nbHits then .nbHits else empty end
  ' "$out")" || die "async source count response was malformed"
  [ "$observed" = "$expected" ] \
    || die "async source fixture held ${observed} documents, expected ${expected}"

  expected_settings="$LOG_DIR/async-source-settings-expected.json"
  async_fixture_settings >"$expected_settings"
  async_vendor_json write GET "/1/indexes/${encoded}/settings" "" "$LOG_DIR/async-source-settings.json"
  jq -e --slurpfile expected "$expected_settings" '
    .searchableAttributes == $expected[0].searchableAttributes
    and .customRanking == $expected[0].customRanking
    and .attributesForFaceting == $expected[0].attributesForFaceting
    and .paginationLimitedTo == $expected[0].paginationLimitedTo
  ' "$LOG_DIR/async-source-settings.json" >/dev/null \
    || die "async source settings did not match seeded fixture"

  expected_synonyms="$(async_fixture_synonyms | jq -S -c 'sort_by(.objectID)')"
  async_vendor_json write POST "/1/indexes/${encoded}/synonyms/search" \
    '{"query":"","hitsPerPage":1000}' "$LOG_DIR/async-source-synonyms.json"
  observed_synonyms="$(jq -S -c '[.hits[]? | {objectID, type, synonyms}] | sort_by(.objectID)' "$LOG_DIR/async-source-synonyms.json")" \
    || die "async source synonym search response was malformed"
  [ "$observed_synonyms" = "$expected_synonyms" ] \
    || die "async source synonyms did not match seeded fixture"

  expected_rules="$(async_fixture_rules | jq -S -c 'sort_by(.objectID)')"
  async_vendor_json write POST "/1/indexes/${encoded}/rules/search" \
    '{"query":"","hitsPerPage":1000}' "$LOG_DIR/async-source-rules.json"
  observed_rules="$(jq -S -c '[.hits[]? | {objectID, conditions, consequence}] | sort_by(.objectID)' "$LOG_DIR/async-source-rules.json")" \
    || die "async source rule search response was malformed"
  [ "$observed_rules" = "$expected_rules" ] \
    || die "async source rules did not match seeded fixture"

  record_check "async_source_seeded" "pass" "nbHits=${observed}"
}

prepare_async_fixture() {
  async_register_algolia_index "$TARGET_INDEX"
  async_preflight_sweep "$LOG_DIR/async-preflight-indexes.json"
  async_seed_source_index
  async_assert_source_seeded
}

cancel_postcommit_target_name() {
  printf '%s_postcommit' "$TARGET_INDEX"
}

cancel_register_algolia_index() {
  CANCEL_OWNED_ALGOLIA_INDICES+=("$1")
}

cancel_owned_algolia_indices_json() {
  printf '%s\n' "${CANCEL_OWNED_ALGOLIA_INDICES[@]:-}" \
    | jq -Rs 'split("\n") | map(select(length > 0))'
}

cancel_log_label() {
  printf '%s' "$1" | tr -c 'A-Za-z0-9_.-' '_'
}

cancel_vendor_object_response() {
  local mode="$1" method="$2" path="$3" body="$4" out="$5"
  ASYNC_VENDOR_HTTP_CODE=""
  ASYNC_VENDOR_HTTP_CODE="$(algolia_vendor_request "$mode" "$method" "$path" "$body" "$out")" \
    || return 1
  [ "$ASYNC_VENDOR_HTTP_CODE" = "200" ] || return 1
  jq -e 'type == "object"' "$out" >/dev/null 2>&1
}

cancel_vendor_json() {
  local mode="$1" method="$2" path="$3" body="$4" out="$5"
  cancel_vendor_object_response "$mode" "$method" "$path" "$body" "$out" \
    || die "cancel Algolia ${method} request expected HTTP 200 with a JSON object, got ${ASYNC_VENDOR_HTTP_CODE:-transport failure}"
}

cancel_delete_algolia_index() {
  local index="$1" out task
  case "$index" in
    "${CANCEL_INDEX_PREFIX}"*) ;;
    *) return 1 ;;
  esac
  out="$LOG_DIR/cancel-delete-$(cancel_log_label "$index").json"
  if ! cancel_vendor_object_response write DELETE \
    "/1/indexes/$(algolia_vendor_url_encode "$index")" "" "$out"; then
    printf 'WARN: DELETE %s failed — HTTP %s\n' "$index" "${ASYNC_VENDOR_HTTP_CODE:-transport failure}" >&2
    return 1
  fi
  task="$(async_vendor_task_id "$out")" || return 1
  algolia_vendor_wait_task "$index" "$task" "$LOG_DIR/cancel-delete-task-$(cancel_log_label "$index").json"
}

cancel_sweep_candidates() {
  local listing="$1" now
  now="$(date +%s)"
  jq -r --arg prefix "$CANCEL_INDEX_PREFIX" \
    --arg source "$SOURCE_INDEX" --arg target "$TARGET_INDEX" --arg post_target "$(cancel_postcommit_target_name)" \
    --argjson now "$now" --argjson max_age "$CANCEL_STALE_AGE_SECONDS" '
    def freshness:
      (.updatedAt // .createdAt)
      | strings
      | sub("\\.[0-9]+Z$"; "Z")
      | try fromdateiso8601 catch empty;
    .items[]?
    | select((.name | type) == "string")
    | select(.name | startswith($prefix))
    | . as $item
    | ([$item | freshness] | first) as $observed_at
    | if ($item.name == $source or $item.name == $target or $item.name == $post_target) then "owned\t\($item.name)"
      elif ($observed_at != null and ($now - $observed_at) > $max_age) then "stale\t\($item.name)"
      else "skipped\t\($item.name)"
      end
  ' "$listing"
}

cancel_record_swept_indices() {
  local swept_json="$1" next
  next="$(mktemp)"
  jq --argjson swept "$swept_json" '.cancel.swept_algolia_indices = $swept' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

cancel_preflight_sweep() {
  local listing="$1" disposition index swept=() skipped=()
  cancel_vendor_json read GET "/1/indexes" "" "$listing"
  jq -e '(.items | type) == "array"' "$listing" >/dev/null \
    || die "cancel Algolia GET /1/indexes response was missing an items array"
  record_check "cancel_vendor_reachable" "pass" "GET /1/indexes returned 200"

  while IFS=$'\t' read -r disposition index; do
    [ -n "$index" ] || continue
    case "$disposition" in
      owned|stale)
        cancel_delete_algolia_index "$index" || die "cancel preflight failed to delete ${index}"
        swept+=("$index")
        ;;
      skipped)
        skipped+=("$index")
        ;;
      *)
        die "cancel preflight produced an unknown sweep disposition: ${disposition}"
        ;;
    esac
  done < <(cancel_sweep_candidates "$listing")

  printf 'INFO: cancel preflight swept=%s skipped=%s\n' \
    "${swept[*]:-none}" "${skipped[*]:-none}"
  cancel_record_swept_indices "$(printf '%s\n' "${swept[@]:-}" | jq -Rs 'split("\n") | map(select(length > 0))')"
  record_check "cancel_preflight_sweep" "pass" \
    "swept=${swept[*]:-none} skipped=${skipped[*]:-none}"
}

cancel_fixture_documents() {
  jq -cn --argjson n "$CANCEL_SOURCE_COUNT" '
    [range(0; $n) as $i
      | {objectID:("fj-cancel-" + ($i|tostring)), name:("Cancel fixture " + ($i|tostring)), seq:$i, bucket:($i % 17)}]
  '
}

cancel_seed_source_index() {
  local out task body
  cancel_register_algolia_index "$SOURCE_INDEX"
  body="$(cancel_fixture_documents | jq -c '{requests: [.[] | {action:"addObject", body:.}]}')"
  out="$LOG_DIR/cancel-seed-batch.json"
  cancel_vendor_json write POST "/1/indexes/$(algolia_vendor_url_encode "$SOURCE_INDEX")/batch" "$body" "$out"
  task="$(async_vendor_task_id "$out")" || die "cancel seeding response did not carry a valid taskID"
  algolia_vendor_wait_task "$SOURCE_INDEX" "$task" "$LOG_DIR/cancel-seed-task.json" \
    || die "cancel seeding task did not publish"
}

cancel_assert_source_seeded() {
  local observed out
  out="$LOG_DIR/cancel-source-count.json"
  cancel_vendor_json write POST \
    "/1/indexes/$(algolia_vendor_url_encode "$SOURCE_INDEX")/query" \
    '{"query":"","hitsPerPage":0}' "$out"
  observed="$(jq -er 'if (.nbHits | type) == "number" and (.nbHits | floor) == .nbHits then .nbHits else empty end' "$out")" \
    || die "cancel source count response was malformed"
  [ "$observed" -eq "$CANCEL_SOURCE_COUNT" ] \
    || die "cancel source fixture held ${observed} documents, expected ${CANCEL_SOURCE_COUNT}"
  [ "$observed" -gt "$CANCEL_BROWSE_PAGE_SIZE" ] \
    || die "VACUOUS: cancel source fixture did not exceed one browse page"
  update_counts "$observed" ""
  local next
  next="$(mktemp)"
  jq --argjson n "$observed" --argjson page "$CANCEL_BROWSE_PAGE_SIZE" \
    '.cancel.corpus_size = $n | .cancel.browse_page_size = $page' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
  record_check "cancel_source_seeded" "pass" "nbHits=${observed}; page_size=${CANCEL_BROWSE_PAGE_SIZE}"
}

prepare_cancel_fixture() {
  cancel_preflight_sweep "$LOG_DIR/cancel-preflight-indexes.json"
  cancel_seed_source_index
  cancel_assert_source_seeded
}

# Deletes every Algolia index this run registered and proves each one is gone.
cleanup_async_algolia_indices() {
  local index failed=0 listing="$LOG_DIR/async-cleanup-indexes-before.json"
  local final_listing="$LOG_DIR/async-cleanup-indexes-after.json"
  local owned listed_owned
  # Nothing was registered, so there is nothing to prove absent. This is the
  # path an early argument or init failure takes, and it must not be reported as
  # a cleanup failure.
  [ "${#ASYNC_OWNED_ALGOLIA_INDICES[@]}" -gt 0 ] || return 0
  owned="$(async_owned_algolia_indices_json)" || return 1
  async_vendor_object_response write GET "/1/indexes" "" "$listing" || return 1
  async_vendor_index_listing_is_valid "$listing" || return 1
  listed_owned="$(async_listed_owned_algolia_indices "$listing" "$owned")" || return 1
  while IFS= read -r index; do
    [ -n "$index" ] || continue
    async_delete_algolia_index "$index" || failed=1
  done <<<"$listed_owned"
  async_vendor_object_response write GET "/1/indexes" "" "$final_listing" || return 1
  async_vendor_index_listing_is_valid "$final_listing" || return 1
  async_listing_excludes_owned_algolia_indices "$final_listing" "$owned" || failed=1
  [ "$failed" -eq 0 ]
}

# Removes both sides of the async run: the local Flapjack destination and every
# registered vendor index. Returns nonzero if any residue survives.
cleanup_async_scenario() {
  [ "$EXPECT_MODE" = "async_job" ] || return 0
  [ "$ASYNC_FIXTURE_CLEANED" -eq 0 ] || return 0
  local failed=0
  if [ -n "$BASE_URL" ] && [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    delete_verified_target "$TARGET_INDEX" "async-target" || failed=1
  fi
  cleanup_async_algolia_indices || failed=1
  [ "$failed" -eq 0 ] || return 1
  ASYNC_FIXTURE_CLEANED=1
}

cleanup_cancel_algolia_indices() {
  local index failed=0 listing="$LOG_DIR/cancel-cleanup-indexes-before.json"
  local final_listing="$LOG_DIR/cancel-cleanup-indexes-after.json"
  local owned listed_owned
  [ "${#CANCEL_OWNED_ALGOLIA_INDICES[@]}" -gt 0 ] || return 0
  owned="$(cancel_owned_algolia_indices_json)" || return 1
  cancel_vendor_object_response write GET "/1/indexes" "" "$listing" || return 1
  jq -e '(.items | type) == "array"' "$listing" >/dev/null 2>&1 || return 1
  listed_owned="$(async_listed_owned_algolia_indices "$listing" "$owned")" || return 1
  while IFS= read -r index; do
    [ -n "$index" ] || continue
    cancel_delete_algolia_index "$index" || failed=1
  done <<<"$listed_owned"
  cancel_vendor_object_response write GET "/1/indexes" "" "$final_listing" || return 1
  jq -e '(.items | type) == "array"' "$final_listing" >/dev/null 2>&1 || return 1
  async_listing_excludes_owned_algolia_indices "$final_listing" "$owned" || failed=1
  [ "$failed" -eq 0 ]
}

cleanup_cancel_scenario() {
  [ "$SCENARIO" = "cancel" ] || return 0
  [ "$CANCEL_FIXTURE_CLEANED" -eq 0 ] || return 0
  local failed=0
  cleanup_cancel_algolia_indices || failed=1
  [ "$failed" -eq 0 ] || return 1
  CANCEL_FIXTURE_CLEANED=1
}

cancel_submit_migration() {
  local target="$1" label="$2" body code job_id
  body="$(migration_payload "$SOURCE_API_KEY" "$target")"
  flapjack_request POST "/1/migrations/algolia" "$body" "$LOG_DIR/${label}-submit.raw" \
    || die "${label} async migration submission transport failed"
  code="$(http_code <"$LOG_DIR/${label}-submit.raw")"
  http_body <"$LOG_DIR/${label}-submit.raw" >"$LOG_DIR/${label}-submit.json"
  [ "$code" = "202" ] || die "${label} async migration submission expected HTTP 202, got ${code}"
  job_id="$(jq -er '
    if (.jobId | type) == "string"
      and (.jobId | test("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"))
    then .jobId else empty end
  ' "$LOG_DIR/${label}-submit.json")" || die "${label} submission did not return a UUID jobId"
  printf '%s\n' "$job_id"
}

cancel_wait_for_observed_job() {
  local barrier_dir="$1" expected_job="$2" label="$3" observed_file observed attempt=0
  observed_file="$barrier_dir/observed"
  while [ "$attempt" -lt "$CANCEL_POLL_ATTEMPTS" ]; do
    if [ -f "$observed_file" ]; then
      observed="$(cat "$observed_file")"
      [ "$observed" = "$expected_job" ] \
        || die "${label} barrier observed job ${observed}, expected ${expected_job}"
      cp "$observed_file" "$LOG_DIR/${label}-observed-job.txt"
      record_check "${label}_barrier_observed" "pass" "jobId=${observed}"
      return 0
    fi
    attempt=$((attempt + 1))
    sleep "$CANCEL_POLL_INTERVAL_SECONDS"
  done
  die "${label} barrier did not record an observed job"
}

cancel_release_barrier() {
  local barrier_dir="$1"
  : >"$barrier_dir/release"
}

cancel_request() {
  local job_id="$1" label="$2" expected_code="$3" code
  flapjack_request POST "/1/migrations/algolia/${job_id}/cancel" '{}' "$LOG_DIR/${label}-cancel.raw" \
    || die "${label} cancel request transport failed"
  code="$(http_code <"$LOG_DIR/${label}-cancel.raw")"
  http_body <"$LOG_DIR/${label}-cancel.raw" >"$LOG_DIR/${label}-cancel.json"
  [ "$code" = "$expected_code" ] || die "${label} cancel expected HTTP ${expected_code}, got ${code}"
}

cancel_read_status() {
  local job_id="$1" label="$2" out code
  out="$LOG_DIR/${label}-status.json"
  flapjack_request GET "/1/migrations/algolia/${job_id}" "" "${out}.raw" \
    || die "${label} status transport failed"
  code="$(http_code <"${out}.raw")"
  http_body <"${out}.raw" >"$out"
  http_success_code "$code" || die "${label} status returned HTTP ${code}"
  printf '%s\n' "$out"
}

cancel_poll_disposition() {
  local job_id="$1" label="$2" expected="$3" status_file disposition attempt=0
  while [ "$attempt" -lt "$CANCEL_POLL_ATTEMPTS" ]; do
    status_file="$(cancel_read_status "$job_id" "$label")"
    disposition="$(jq -er '.disposition | strings' "$status_file")" \
      || die "${label} status disposition was malformed"
    case "$disposition" in
      "$expected")
        cp "$status_file" "$LOG_DIR/${label}-terminal-status.json"
        record_check "${label}_terminal_status" "pass" "disposition=${expected}"
        return 0
        ;;
      failed) die "${label} job reported failed" ;;
      succeeded)
        [ "$expected" = "succeeded" ] || die "${label} job succeeded after pre-commit cancel"
        ;;
      cancelled)
        [ "$expected" = "cancelled" ] || die "${label} job cancelled after post-commit cancel_too_late"
        ;;
      running) ;;
      *) die "${label} status reported unknown disposition: ${disposition}" ;;
    esac
    attempt=$((attempt + 1))
    sleep "$CANCEL_POLL_INTERVAL_SECONDS"
  done
  die "${label} job did not reach ${expected} within ${CANCEL_POLL_ATTEMPTS} polls"
}

cancel_seed_preexisting_target() {
  local target="$1" sentinel_body code count snapshot_dir
  sentinel_body='{"objectID":"sentinel-object","sentinel":"preserve-me","count":1}'
  flapjack_request PUT "$(encoded_index_path "$target")/sentinel-object" "$sentinel_body" "$LOG_DIR/cancel-precommit-sentinel-seed.raw" \
    || die "cancel precommit sentinel seed transport failed"
  code="$(http_code <"$LOG_DIR/cancel-precommit-sentinel-seed.raw")"
  http_body <"$LOG_DIR/cancel-precommit-sentinel-seed.raw" >"$LOG_DIR/cancel-precommit-sentinel-seed.json"
  http_success_code "$code" || die "cancel precommit sentinel seed returned HTTP ${code}"
  flapjack_request GET "$(encoded_index_path "$target")/sentinel-object" "" "$LOG_DIR/cancel-precommit-sentinel-before.raw" \
    || die "cancel precommit sentinel read transport failed"
  code="$(http_code <"$LOG_DIR/cancel-precommit-sentinel-before.raw")"
  http_body <"$LOG_DIR/cancel-precommit-sentinel-before.raw" >"$LOG_DIR/cancel-precommit-sentinel-before.json"
  http_success_code "$code" || die "cancel precommit sentinel read returned HTTP ${code}"
  count="$(target_listing_count "$target" "cancel-precommit-list-before")" \
    || die "cancel precommit target listing failed"
  [ "$count" = "1" ] || die "cancel precommit target was not listed exactly once before migration"
  snapshot_dir="$LOG_DIR/cancel-precommit-target-snapshot"
  rm -rf "$snapshot_dir"
  cp -R "$DATA_DIR/$target" "$snapshot_dir" || die "cancel precommit target byte snapshot failed"
  CANCEL_PRECOMMIT_SENTINEL="$LOG_DIR/cancel-precommit-sentinel-before.json"
  CANCEL_PRECOMMIT_LISTING="$LOG_DIR/cancel-precommit-list-before.json"
  record_check "cancel_precommit_sentinel_seeded" "pass" "target=${target}; entries=1"
}

cancel_assert_preexisting_target_unchanged() {
  local target="$1" code count before_canonical after_canonical
  flapjack_request GET "$(encoded_index_path "$target")/sentinel-object" "" "$LOG_DIR/cancel-precommit-sentinel-after.raw" \
    || die "cancel precommit sentinel re-read transport failed"
  code="$(http_code <"$LOG_DIR/cancel-precommit-sentinel-after.raw")"
  http_body <"$LOG_DIR/cancel-precommit-sentinel-after.raw" >"$LOG_DIR/cancel-precommit-sentinel-after.json"
  http_success_code "$code" || die "cancel precommit sentinel re-read returned HTTP ${code}"
  before_canonical="$LOG_DIR/cancel-precommit-sentinel-before.canonical.json"
  after_canonical="$LOG_DIR/cancel-precommit-sentinel-after.canonical.json"
  jq -S -c . "$CANCEL_PRECOMMIT_SENTINEL" >"$before_canonical" \
    || die "cancel precommit sentinel before JSON was malformed"
  jq -S -c . "$LOG_DIR/cancel-precommit-sentinel-after.json" >"$after_canonical" \
    || die "cancel precommit sentinel after JSON was malformed"
  cmp -s "$before_canonical" "$after_canonical" \
    || die "cancel precommit migration mutated the sentinel object"
  count="$(target_listing_count "$target" "cancel-precommit-list-after")" \
    || die "cancel precommit target listing after cancel failed"
  [ "$count" = "1" ] || die "cancel precommit target was absent or duplicated after cancel"
  diff -qr "$LOG_DIR/cancel-precommit-target-snapshot" "$DATA_DIR/$target" >/dev/null \
    || die "cancel precommit target bytes changed after cancellation"
  record_check "cancel_precommit_target_unchanged" "pass" "sentinel, listing, and directory snapshot unchanged"
}

cancel_job_dir_is_terminal_cancel_metadata() {
  local job_dir="$1" job_id="$2"
  [ "$(basename "$job_dir")" = "$job_id" ] || return 1
  [ -f "$job_dir/migration_phase.json" ] || return 1
  [ -f "$job_dir/async_migration.json" ] || return 1
  find "$job_dir" -mindepth 1 -maxdepth 1 \
    ! -name "migration_phase.json" \
    ! -name "async_migration.json" \
    ! -name "manifest.json" \
    ! -name ".job.lock" \
    -print -quit | grep -q . && return 1
  jq -e '
    .disposition == "Cancelled"
    and .cancel_requested == true
    and (.terminal_at | type == "string" and length > 0)
  ' "$job_dir/migration_phase.json" >/dev/null || return 1
  [ ! -f "$job_dir/manifest.json" ] || jq -e '
    .lifecycle == "Deleted"
    and ((.artifacts // []) | length == 0)
  ' "$job_dir/manifest.json" >/dev/null
}

cancel_assert_no_uncommitted_artifacts() {
  local target="$1" label="$2" allowed_job_id="${3:-}" jobs_dir publication job_dir
  jobs_dir="$DATA_DIR/migration_exports/jobs"
  if [ -d "$jobs_dir" ]; then
    while IFS= read -r job_dir; do
      [ -n "$job_dir" ] || continue
      if [ -n "$allowed_job_id" ] \
        && cancel_job_dir_is_terminal_cancel_metadata "$job_dir" "$allowed_job_id"; then
        continue
      fi
      die "${label} leaked migration spool artifacts under $(basename "$job_dir")"
    done < <(find "$jobs_dir" -mindepth 1 -maxdepth 1 -type d)
  fi
  publication="$DATA_DIR/.publication/$target"
  if [ -d "$publication" ] && [ -n "$(find "$publication" -mindepth 1 -print -quit 2>/dev/null)" ]; then
    die "${label} left publication staging artifacts under ${publication}"
  fi
  record_check "${label}_artifact_cleanup" "pass" "no spool or publication staging residue"
}

cancel_query_target_objects() {
  local target="$1" label="$2" request cursor="" ids_file page_label ordinal=0 page_count fetched_count duplicate_count
  ids_file="$LOG_DIR/${label}.jsonl"
  : >"$ids_file"
  page_count=$(((CANCEL_SOURCE_COUNT + CANCEL_BROWSE_PAGE_SIZE - 1) / CANCEL_BROWSE_PAGE_SIZE))
  while :; do
    printf -v page_label '%s-page-%06d' "$label" "$ordinal"
    if [ -n "$cursor" ]; then
      request="$(jq -cn --arg cursor "$cursor" --argjson ordinal "$ordinal" --argjson page_size "$CANCEL_BROWSE_PAGE_SIZE" \
        '{browse:true,ordinal:$ordinal,cursor:$cursor,hitsPerPage:$page_size,attributesToRetrieve:["objectID","name","seq","bucket"]}')"
    else
      request="$(jq -cn --argjson ordinal "$ordinal" --argjson page_size "$CANCEL_BROWSE_PAGE_SIZE" \
        '{browse:true,ordinal:$ordinal,query:"",hitsPerPage:$page_size,attributesToRetrieve:["objectID","name","seq","bucket"]}')"
    fi
    browse_index "$target" "$page_label" "$request"
    jq -e --argjson expected "$CANCEL_SOURCE_COUNT" '
      .nbHits == $expected
      and all(.hits[]; (.objectID | type) == "string" and (.objectID | length) > 0)
    ' "$LOG_DIR/${page_label}.json" >/dev/null \
      || die "cancel postcommit target page response was malformed"
    jq -c '.hits[] | {objectID, name, seq, bucket}' "$LOG_DIR/${page_label}.json" >>"$ids_file"
    cursor="$(jq -er 'if .cursor == null then "" else .cursor end' "$LOG_DIR/${page_label}.json")" \
      || die "cancel postcommit target browse cursor was malformed"
    [ -n "$cursor" ] || break
    ordinal=$((ordinal + 1))
    [ "$ordinal" -lt "$page_count" ] \
      || die "cancel postcommit target browse cursor did not terminate after expected object count"
  done
  fetched_count="$(wc -l <"$ids_file" | tr -d ' ')"
  [ "$fetched_count" = "$CANCEL_SOURCE_COUNT" ] \
    || die "cancel postcommit target browsed objectID count did not equal live source count"
  duplicate_count="$(jq -r '.objectID' "$ids_file" | sort | uniq -d | wc -l | tr -d ' ')"
  [ "$duplicate_count" = "0" ] \
    || die "cancel postcommit target returned duplicate objectID values"
  jq -S -s -c 'sort_by(.objectID)' "$ids_file"
}

cancel_assert_postcommit_target_matches_source() {
  local target="$1" observed expected count
  count="$(target_listing_count "$target" "cancel-postcommit-list-after")" \
    || die "cancel postcommit target listing failed"
  [ "$count" = "1" ] || die "cancel postcommit target was not listed exactly once"
  jq -e --arg target "$target" --argjson expected "$CANCEL_SOURCE_COUNT" \
    '[.items[]? | select(.name == $target)][0].entries == $expected' \
    "$LOG_DIR/cancel-postcommit-list-after.json" >/dev/null \
    || die "cancel postcommit target entries did not equal source count"
  observed="$(cancel_query_target_objects "$target" "cancel-postcommit-target-documents")"
  expected="$(cancel_fixture_documents | jq -S -c 'sort_by(.objectID)')"
  [ "$observed" = "$expected" ] \
    || die "cancel postcommit target documents did not match the seeded source"
  update_counts "$CANCEL_SOURCE_COUNT" "$CANCEL_SOURCE_COUNT"
  record_check "cancel_postcommit_target_documents" "pass" "${CANCEL_SOURCE_COUNT} objectIDs matched seeded source"
}

cancel_record_arm_receipt() {
  local arm="$1" job_id="$2" target="$3" status_file="$4" next
  next="$(mktemp)"
  jq --arg arm "$arm" --arg job "$job_id" --arg target "$target" --slurpfile status "$status_file" '
    .cancel[$arm] = {job_id:$job, target_index:$target, terminal_status:$status[0]}
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

assert_cancel_scenario() {
  local pre_status post_status code
  CANCEL_PRECOMMIT_TARGET="$TARGET_INDEX"
  CANCEL_POSTCOMMIT_TARGET="$(cancel_postcommit_target_name)"

  prepare_cancel_fixture
  cancel_seed_preexisting_target "$CANCEL_PRECOMMIT_TARGET"

  CANCEL_PRECOMMIT_JOB_ID="$(cancel_submit_migration "$CANCEL_PRECOMMIT_TARGET" "cancel-precommit")"
  cancel_wait_for_observed_job "$CANCEL_PRECOMMIT_BARRIER_DIR" "$CANCEL_PRECOMMIT_JOB_ID" "cancel_precommit"
  cancel_request "$CANCEL_PRECOMMIT_JOB_ID" "cancel-precommit" "200"
  jq -e '.disposition == "running"' "$LOG_DIR/cancel-precommit-cancel.json" >/dev/null \
    || die "cancel precommit cancel response did not keep job running for cooperative settlement"
  cp "$LOG_DIR/cancel-precommit-cancel.json" "$LOG_DIR/cancel-precommit-cancel-status.json"
  cancel_release_barrier "$CANCEL_PRECOMMIT_BARRIER_DIR"
  cancel_poll_disposition "$CANCEL_PRECOMMIT_JOB_ID" "cancel-precommit" "cancelled"
  pre_status="$LOG_DIR/cancel-precommit-terminal-status.json"
  jq -e '.disposition == "cancelled" and .phase == "activating"' "$pre_status" >/dev/null \
    || die "cancel precommit terminal status was not cancelled while activating"
  cancel_assert_preexisting_target_unchanged "$CANCEL_PRECOMMIT_TARGET"
  cancel_assert_no_uncommitted_artifacts "$CANCEL_PRECOMMIT_TARGET" "cancel_precommit" "$CANCEL_PRECOMMIT_JOB_ID"
  cancel_record_arm_receipt "precommit" "$CANCEL_PRECOMMIT_JOB_ID" "$CANCEL_PRECOMMIT_TARGET" "$pre_status"

  CANCEL_POSTCOMMIT_JOB_ID="$(cancel_submit_migration "$CANCEL_POSTCOMMIT_TARGET" "cancel-postcommit")"
  cancel_wait_for_observed_job "$CANCEL_POSTCOMMIT_BARRIER_DIR" "$CANCEL_POSTCOMMIT_JOB_ID" "cancel_postcommit"
  cancel_read_status "$CANCEL_POSTCOMMIT_JOB_ID" "cancel-postcommit-running" >/dev/null
  jq -e '.disposition == "running" and .phase == "activating"' \
    "$LOG_DIR/cancel-postcommit-running-status.json" >/dev/null \
    || die "cancel postcommit status was not still running while held after commit"
  cancel_request "$CANCEL_POSTCOMMIT_JOB_ID" "cancel-postcommit" "409"
  jq -e '.code == "cancel_too_late"' "$LOG_DIR/cancel-postcommit-cancel.json" >/dev/null \
    || die "cancel postcommit 409 response was not cancel_too_late"
  cancel_release_barrier "$CANCEL_POSTCOMMIT_BARRIER_DIR"
  cancel_poll_disposition "$CANCEL_POSTCOMMIT_JOB_ID" "cancel-postcommit" "succeeded"
  post_status="$LOG_DIR/cancel-postcommit-terminal-status.json"
  cancel_assert_postcommit_target_matches_source "$CANCEL_POSTCOMMIT_TARGET"
  cancel_record_arm_receipt "postcommit" "$CANCEL_POSTCOMMIT_JOB_ID" "$CANCEL_POSTCOMMIT_TARGET" "$post_status"
  record_check "cancel_too_late" "pass" "HTTP 409 code=cancel_too_late; target committed"
}

# Maps a phase name onto its position in the Stage 3 order. Returns nonzero for
# any name outside that closed set so an unknown phase fails closed.
async_phase_rank() {
  local phase="$1" candidate rank=0
  for candidate in $ASYNC_PHASE_ORDER; do
    rank=$((rank + 1))
    if [ "$candidate" = "$phase" ]; then
      ASYNC_PHASE_RANK="$rank"
      return 0
    fi
  done
  return 1
}

delete_replica_source_fixture_target() {
  local index_name="$1" label="$2" code payload task_id remaining=40 status
  source_algolia_request DELETE "$(source_algolia_index_path "$index_name")" "" "$LOG_DIR/replica-cleanup-${label}.raw" \
    || return 1
  code="$(http_code <"$LOG_DIR/replica-cleanup-${label}.raw")"
  payload="$(http_body <"$LOG_DIR/replica-cleanup-${label}.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/replica-cleanup-${label}.json"
  http_success_code "$code" || return 1
  task_id="$(jq -er '.taskID' "$LOG_DIR/replica-cleanup-${label}.json")" || return 1
  while [ "$remaining" -gt 0 ]; do
    source_algolia_request GET "$(source_algolia_index_path "$index_name")/task/${task_id}" "" "$LOG_DIR/replica-cleanup-${label}-task.raw" \
      || return 1
    code="$(http_code <"$LOG_DIR/replica-cleanup-${label}-task.raw")"
    payload="$(http_body <"$LOG_DIR/replica-cleanup-${label}-task.raw")"
    printf '%s\n' "$payload" >"$LOG_DIR/replica-cleanup-${label}-task.json"
    http_success_code "$code" || return 1
    status="$(jq -r '.status // empty' "$LOG_DIR/replica-cleanup-${label}-task.json")" || return 1
    [ "$status" = "published" ] && return 0
    sleep 0.25
    remaining=$((remaining - 1))
  done
  return 1
}

submit_async_migration() {
  local body="$1" code
  flapjack_request POST "/1/migrations/algolia" "$body" "$LOG_DIR/migration-response.raw" \
    || die "async migration submission transport failed"
  code="$(http_code <"$LOG_DIR/migration-response.raw")"
  http_body <"$LOG_DIR/migration-response.raw" >"$LOG_DIR/migration-response.json"
  [ "$code" = "202" ] || die "async migration submission expected HTTP 202, got ${code}"
  ASYNC_JOB_ID="$(jq -er '
    if (.jobId | type) == "string"
      and (.jobId | test("^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$"))
    then .jobId else empty end
  ' "$LOG_DIR/migration-response.json")" \
    || die "async migration submission did not return a UUID jobId"
  record_check "async_submission" "pass" "202 jobId=${ASYNC_JOB_ID}"
}

# Reads one status response and enforces the per-poll invariants: the job identity
# is echoed back, the phase belongs to the Stage 3 order, and it never moves
# backwards. Repeated phases are expected and allowed.
read_async_status() {
  local out="$1" previous_rank="$2" code phase
  flapjack_request GET "/1/migrations/algolia/${ASYNC_JOB_ID}" "" "${out}.raw" \
    || die "async migration status transport failed"
  code="$(http_code <"${out}.raw")"
  http_body <"${out}.raw" >"$out"
  http_success_code "$code" || die "async migration status returned HTTP ${code}"
  jq -e --arg job "$ASYNC_JOB_ID" '.jobId == $job' "$out" >/dev/null \
    || die "async migration status returned a malformed or unknown jobId"
  phase="$(jq -er '.phase | strings' "$out")" \
    || die "async migration status phase was malformed"
  async_phase_rank "$phase" || die "async migration status reported unknown phase: ${phase}"
  [ "$ASYNC_PHASE_RANK" -ge "$previous_rank" ] \
    || die "async migration phase regressed to ${phase}"
  # Record transitions, not polls: a repeated phase is legal but says nothing new,
  # and poll counts vary with timing. Leading-space padding makes the first
  # element match the same way every later one does.
  case " ${ASYNC_PHASE_SEQUENCE}" in
    *" ${phase}") ;;
    *) ASYNC_PHASE_SEQUENCE="${ASYNC_PHASE_SEQUENCE:+${ASYNC_PHASE_SEQUENCE} }${phase}" ;;
  esac
  printf '%s\n' "$phase"
}

# Polls until the job reports a terminal disposition. Every other exit from this
# loop is a failure: timeout, transport error, unknown disposition, or `failed`.
poll_async_job_until_terminal() {
  local out="$LOG_DIR/async-status.json" attempt=0 disposition export_progress
  local previous_rank=0
  ASYNC_PHASE_SEQUENCE=""
  while [ "$attempt" -lt "$ASYNC_POLL_ATTEMPTS" ]; do
    read_async_status "$out" "$previous_rank" >/dev/null
    previous_rank="$ASYNC_PHASE_RANK"
    disposition="$(jq -er '.disposition | strings' "$out")" \
      || die "async migration status disposition was malformed"
    case "$disposition" in
      running) ;;
      succeeded)
        printf '%s\n' "$ASYNC_PHASE_SEQUENCE" >"$LOG_DIR/async-phase-sequence.txt"
        export_progress="$(jq -c '.exportProgress // null' "$out")"
        printf '%s\n' "$export_progress" >"$LOG_DIR/async-export-progress.json"
        jq -c '{
          settingsApplied:(.settingsApplied // null),
          synonymsImported:(.synonymsImported // null),
          rulesImported:(.rulesImported // null),
          warnings:(.warnings // [])
        }' "$out" >"$LOG_DIR/async-import-status-counts.json"
        record_check "async_phase_sequence" "pass" "$ASYNC_PHASE_SEQUENCE"
        return 0
        ;;
      failed)
        die "async migration job reported terminal disposition failed"
        ;;
      *)
        die "async migration status reported unknown disposition: ${disposition}"
        ;;
    esac
    attempt=$((attempt + 1))
    sleep "$ASYNC_POLL_INTERVAL_SECONDS"
  done
  die "async migration job did not reach a terminal disposition within ${ASYNC_POLL_ATTEMPTS} polls"
}

# The MIG-1 regression guard: a succeeded disposition is only believed once the
# destination actually exists exactly once, with the exact seeded document count
# and the exact seeded objectIDs and field values.
assert_async_target_activated() {
  local expected code matches request observed
  expected="$(async_fixture_document_count)"

  flapjack_request GET "/1/indexes" "" "$LOG_DIR/list-indices.raw" \
    || die "list-indices request transport failed"
  code="$(http_code <"$LOG_DIR/list-indices.raw")"
  http_body <"$LOG_DIR/list-indices.raw" >"$LOG_DIR/list-indices.json"
  http_success_code "$code" || die "GET /1/indexes returned HTTP ${code}"
  matches="$(jq -cer --arg target "$TARGET_INDEX" '[.items[]? | select(.name == $target)]' "$LOG_DIR/list-indices.json")" \
    || die "GET /1/indexes response was malformed"
  [ "$(printf '%s\n' "$matches" | jq 'length')" = "1" ] \
    || die "async scenario expected exactly one target index listing"
  [ "$(printf '%s\n' "$matches" | jq -r '.[0].entries')" = "$expected" ] \
    || die "async target entries did not equal the seeded document count"
  update_counts "$expected" "$expected"
  record_check "async_target_entries" "pass" "entries=${expected}"

  request="$(jq -cn --argjson hits_per_page "$expected" '
    {query:"",hitsPerPage:$hits_per_page,attributesToRetrieve:["objectID","name","price"],attributesToHighlight:[],attributesToSnippet:[]}
  ')"
  query_index "$TARGET_INDEX" "async-target-documents" "$request"
  observed="$(jq -S -c '[.hits[] | {objectID, name, price}] | sort_by(.objectID)' "$LOG_DIR/async-target-documents.json")"
  [ "$observed" = "$(async_fixture_documents | jq -S -c 'sort_by(.objectID)')" ] \
    || die "async target documents did not match the seeded objectIDs and field values"
  record_check "async_target_documents" "pass" "${expected} documents matched seeded content exactly"
}

assert_async_status_import_counts() {
  local synonym_count rule_count warning_count expected_warnings
  synonym_count="$(async_fixture_synonym_count)"
  rule_count="$(async_fixture_rule_count)"
  warning_count="$(async_expected_status_warning_count)"
  expected_warnings="$(async_expected_status_warnings)"
  jq -e \
    --argjson synonym_count "$synonym_count" \
    --argjson rule_count "$rule_count" \
    --argjson expected_warnings "$expected_warnings" '
    .settingsApplied == true
    and .synonymsImported.imported == $synonym_count
    and .rulesImported.imported == $rule_count
    and .warnings == $expected_warnings
  ' "$LOG_DIR/async-status.json" >/dev/null \
    || die "async terminal status import counts did not match the seeded fixture"
  record_check "async_status_import_counts" "pass" \
    "settings=true synonyms=${synonym_count} rules=${rule_count} warnings=${warning_count}"
}

assert_async_job() {
  submit_async_migration "$(migration_payload)"
  poll_async_job_until_terminal
  assert_async_status_import_counts
  assert_async_target_activated
}

cleanup_replica_source_fixture() {
  [ "$SCENARIO" = "replicas" ] || return 0
  [ "$REPLICA_SOURCE_FIXTURE_CLEANED" -eq 0 ] || return 0
  [ -n "$LOG_DIR" ] || return 1

  local failed=0
  delete_replica_source_fixture_target "$SOURCE_INDEX" "primary" || failed=1
  delete_replica_source_fixture_target "$(replica_source_relevance_index)" "relevance" || failed=1
  delete_replica_source_fixture_target "$(replica_source_standard_index)" "standard" || failed=1
  if [ "$failed" -eq 0 ]; then
    REPLICA_SOURCE_FIXTURE_CLEANED=1
    record_check "replica_cleanup" "pass" "source fixture names deleted exactly"
    return 0
  fi
  record_check "replica_cleanup" "fail" "source fixture cleanup failed"
  return 1
}

assert_unavailable() {
  local body code payload target_count
  body="$(migration_payload)"
  flapjack_request POST "/1/migrate-from-algolia" "$body" "$LOG_DIR/migration-response.raw" \
    || die "migration request transport failed"
  code="$(http_code <"$LOG_DIR/migration-response.raw")"
  payload="$(http_body <"$LOG_DIR/migration-response.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/migration-response.json"

  [ "$code" = "503" ] || die "unavailable mode expected HTTP 503, got ${code}"
  jq -e '.code == "migration_ha_unsupported"' "$LOG_DIR/migration-response.json" >/dev/null \
    || die "unavailable mode response code was not migration_ha_unsupported"
  record_check "migration_refusal" "pass" "503 migration_ha_unsupported"

  flapjack_request GET "/1/indexes" "" "$LOG_DIR/list-indices.raw" \
    || die "list-indices request transport failed"
  code="$(http_code <"$LOG_DIR/list-indices.raw")"
  payload="$(http_body <"$LOG_DIR/list-indices.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/list-indices.json"
  http_success_code "$code" || die "GET /1/indexes returned HTTP ${code}"
  target_count="$(jq -er --arg target "$TARGET_INDEX" '[.items[]? | select(.name == $target)] | length' "$LOG_DIR/list-indices.json")" \
    || die "GET /1/indexes response was malformed"
  [ "$target_count" = "0" ] || die "unavailable mode created or exposed target index"
  record_check "target_absent" "pass" "target not listed"
}

capture_target_absence() {
  local check_name="$1" code payload target_count
  flapjack_request GET "/1/indexes" "" "$LOG_DIR/list-indices.raw" \
    || die "list-indices request transport failed"
  code="$(http_code <"$LOG_DIR/list-indices.raw")"
  payload="$(http_body <"$LOG_DIR/list-indices.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/list-indices.json"
  http_success_code "$code" || die "GET /1/indexes returned HTTP ${code}"
  target_count="$(jq -er --arg target "$TARGET_INDEX" '[.items[]? | select(.name == $target)] | length' "$LOG_DIR/list-indices.json")" \
    || die "GET /1/indexes response was malformed"
  [ "$target_count" = "0" ] || die "failed import created or exposed target index"
  record_check "$check_name" "pass" "target not listed"
}

assert_importing() {
  local body code payload imported matches
  body="$(migration_payload)"
  execute_migration_request "$body" "$LOG_DIR/migration-response.raw" \
    || die "migration request transport failed"
  code="$(http_code <"$LOG_DIR/migration-response.raw")"
  payload="$(http_body <"$LOG_DIR/migration-response.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/migration-response.json"

  if ! http_success_code "$code"; then
    capture_target_absence "target_absent_after_failed_import"
    die "importing mode expected 2xx, got HTTP ${code}"
  fi
  imported="$(jq -er 'if (.objects.imported | type) == "number" and (.objects.imported | floor) == .objects.imported then .objects.imported else empty end' "$LOG_DIR/migration-response.json")" \
    || die "importing mode response was missing integer objects.imported"
  update_counts "$imported" ""
  record_check "migration_import" "pass" "objects.imported=${imported}"

  flapjack_request GET "/1/indexes" "" "$LOG_DIR/list-indices.raw" \
    || die "list-indices request transport failed"
  code="$(http_code <"$LOG_DIR/list-indices.raw")"
  payload="$(http_body <"$LOG_DIR/list-indices.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/list-indices.json"
  http_success_code "$code" || die "GET /1/indexes returned HTTP ${code}"
  matches="$(jq -cer --arg target "$TARGET_INDEX" '[.items[]? | select(.name == $target)]' "$LOG_DIR/list-indices.json")" \
    || die "GET /1/indexes response was malformed"
  [ "$(printf '%s\n' "$matches" | jq 'length')" = "1" ] \
    || die "importing mode expected exactly one target index listing"
  [ "$(printf '%s\n' "$matches" | jq -r '.[0].entries')" = "$imported" ] \
    || die "importing mode target entries did not equal imported count"
  update_counts "$imported" "$imported"
  record_check "target_entries" "pass" "entries=${imported}"

  if [ "$SCENARIO" = "replicas" ]; then
    assert_replica_scenario_import
  elif [ -n "$VERIFICATION_MANIFEST" ]; then
    assert_verified_import "$imported"
  fi
}

assert_verified_import() {
  local imported="$1" source_count manifest_source_count synonym_count rule_count
  local request expected_first competitor hidden expected_rule
  local conflict_target invalid_target sentinel_body body code payload count

  manifest_source_count="$(jq -r '.source_count' "$VERIFICATION_MANIFEST")"
  source_count="$manifest_source_count"
  if [ "$EXPECT_MODE" = "scale" ]; then
    source_count="$(read_live_scale_source_count)"
    [ "$source_count" = "$manifest_source_count" ] \
      || fail_scale_check "scale_source_count" "live scale source count did not equal generator manifest count"
    record_check "scale_source_count" "pass" "nbHits=${source_count}"
  fi
  synonym_count="$(jq -r '.synonym_count' "$VERIFICATION_MANIFEST")"
  rule_count="$(jq -r '.rule_count' "$VERIFICATION_MANIFEST")"
  [ "$imported" = "$source_count" ] || die "imported object count did not equal source manifest count"
  jq -e --argjson source_count "$source_count" --argjson synonym_count "$synonym_count" --argjson rule_count "$rule_count" '
    .settings == true
    and .objects.imported == $source_count
    and .synonyms.imported == $synonym_count
    and .rules.imported == $rule_count
  ' "$LOG_DIR/migration-response.json" >/dev/null \
    || die "migration response counts did not equal source manifest counts"
  update_verified_counts "$source_count" "$imported" "$synonym_count" "$rule_count"
  record_check "migration_counts" "pass" "objects=${source_count} synonyms=${synonym_count} rules=${rule_count}"

  if [ "$EXPECT_MODE" = "scale" ]; then
    assert_scale_aggregates "$source_count"
  fi

  request="$(jq -c '{query:.known_answers_query,hitsPerPage:(.known_answers | length)}' "$VERIFICATION_MANIFEST")"
  query_index "$TARGET_INDEX" "known-answers" "$request"
  jq -e --slurpfile manifest "$VERIFICATION_MANIFEST" '
    def without_response_metadata:
      with_entries(select(.key | startswith("_") | not));
    ([.hits[] | without_response_metadata] | sort_by(.objectID))
      == ($manifest[0].known_answers | sort_by(.objectID))
  ' "$LOG_DIR/known-answers.json" >/dev/null \
    || die "known-answer documents did not exactly match the source manifest"
  record_check "known_answers" "pass" "exact full fields matched"

  request="$(jq -c '.probes.settings.request' "$VERIFICATION_MANIFEST")"
  query_index "$TARGET_INDEX" "settings-effective" "$request"
  jq -e --slurpfile manifest "$VERIFICATION_MANIFEST" \
    '(.hits | map(.objectID)) == $manifest[0].probes.settings.expected_object_ids' \
    "$LOG_DIR/settings-effective.json" >/dev/null || die "settings behavior probe did not match expected ordering"
  record_check "settings_effective" "pass" "expected ordering observed"

  request="$(jq -c '.probes.synonym.request' "$VERIFICATION_MANIFEST")"
  query_index "$TARGET_INDEX" "synonym-effective" "$request"
  jq -e --slurpfile manifest "$VERIFICATION_MANIFEST" \
    '(.hits | map(.objectID)) == $manifest[0].probes.synonym.expected_object_ids' \
    "$LOG_DIR/synonym-effective.json" >/dev/null || die "synonym behavior probe did not match expected hits"
  record_check "synonym_effective" "pass" "expected expansion observed"

  request="$(jq -c '.probes.promotion.request' "$VERIFICATION_MANIFEST")"
  query_index "$TARGET_INDEX" "rule-promotion-effective" "$request"
  expected_first="$(jq -r '.probes.promotion.expected_first_object_id' "$VERIFICATION_MANIFEST")"
  competitor="$(jq -r '.probes.promotion.competitor_object_id' "$VERIFICATION_MANIFEST")"
  expected_rule="$(jq -r '.probes.promotion.expected_rule_id' "$VERIFICATION_MANIFEST")"
  jq -e --arg first "$expected_first" --arg competitor "$competitor" --arg rule "$expected_rule" '
    .hits[0].objectID == $first
    and ((.hits | map(.objectID) | index($competitor)) // -1) > 0
    and ([.appliedRules[]?.objectID] | index($rule)) != null
  ' "$LOG_DIR/rule-promotion-effective.json" >/dev/null || die "promotion rule behavior probe failed"
  record_check "rule_promotion_effective" "pass" "promoted result and applied rule observed"

  request="$(jq -c '.probes.hiding.request' "$VERIFICATION_MANIFEST")"
  query_index "$TARGET_INDEX" "rule-hiding-effective" "$request"
  hidden="$(jq -r '.probes.hiding.hidden_object_id' "$VERIFICATION_MANIFEST")"
  expected_rule="$(jq -r '.probes.hiding.expected_rule_id' "$VERIFICATION_MANIFEST")"
  jq -e --arg hidden "$hidden" --arg rule "$expected_rule" --slurpfile manifest "$VERIFICATION_MANIFEST" '
    (.hits | map(.objectID)) == $manifest[0].probes.hiding.expected_object_ids
    and ((.hits | map(.objectID) | index($hidden)) == null)
    and ([.appliedRules[]?.objectID] | index($rule)) != null
  ' "$LOG_DIR/rule-hiding-effective.json" >/dev/null || die "hiding rule behavior probe failed"
  record_check "rule_hiding_effective" "pass" "hidden result absent and applied rule observed"

  conflict_target="${TARGET_INDEX}_conflict"
  invalid_target="${TARGET_INDEX}_invalid_key"
  sentinel_body='{"objectID":"sentinel-object","sentinel":"preserve-me","count":1}'
  flapjack_request PUT "$(encoded_index_path "$conflict_target")/sentinel-object" "$sentinel_body" "$LOG_DIR/conflict-seed.raw" \
    || die "conflict target seed transport failed"
  code="$(http_code <"$LOG_DIR/conflict-seed.raw")"
  payload="$(http_body <"$LOG_DIR/conflict-seed.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/conflict-seed.json"
  http_success_code "$code" || die "conflict target seed returned HTTP ${code}"

  body="$(migration_payload "$SOURCE_API_KEY" "$conflict_target")"
  flapjack_request POST "/1/migrate-from-algolia" "$body" "$LOG_DIR/conflict-migration-response.raw" \
    || die "conflict migration transport failed"
  code="$(http_code <"$LOG_DIR/conflict-migration-response.raw")"
  payload="$(http_body <"$LOG_DIR/conflict-migration-response.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/conflict-migration-response.json"
  [ "$code" = "409" ] || die "existing-target migration expected HTTP 409, got ${code}"
  flapjack_request GET "$(encoded_index_path "$conflict_target")/sentinel-object" "" "$LOG_DIR/conflict-sentinel-after.raw" \
    || die "conflict sentinel re-query transport failed"
  code="$(http_code <"$LOG_DIR/conflict-sentinel-after.raw")"
  payload="$(http_body <"$LOG_DIR/conflict-sentinel-after.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/conflict-sentinel-after.json"
  http_success_code "$code" || die "conflict sentinel re-query returned HTTP ${code}"
  jq -e --argjson expected "$sentinel_body" '. == $expected' "$LOG_DIR/conflict-sentinel-after.json" >/dev/null \
    || die "existing-target migration mutated the sentinel document"
  count="$(target_listing_count "$conflict_target" "conflict-list-after")" || die "conflict target listing failed"
  [ "$count" = "1" ] || die "conflict target was absent or duplicated after HTTP 409"
  jq -e --arg target "$conflict_target" \
    '[.items[] | select(.name == $target)][0].entries == 1' "$LOG_DIR/conflict-list-after.json" >/dev/null \
    || die "existing-target migration changed the sentinel index count"
  record_check "conflict_target_immutable" "pass" "HTTP 409; sentinel and count unchanged"

  body="$(migration_payload "fj_invalid_key_for_contract" "$invalid_target")"
  flapjack_request POST "/1/migrate-from-algolia" "$body" "$LOG_DIR/invalid-key-migration-response.raw" \
    || die "invalid-key migration transport failed"
  code="$(http_code <"$LOG_DIR/invalid-key-migration-response.raw")"
  payload="$(http_body <"$LOG_DIR/invalid-key-migration-response.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/invalid-key-migration-response.json"
  [ "$code" = "502" ] || die "invalid-key migration expected HTTP 502, got ${code}"
  count="$(target_listing_count "$invalid_target" "invalid-key-list-after")" || die "invalid-key target listing failed"
  [ "$count" = "0" ] || die "invalid-key migration created or exposed its target"
  record_check "invalid_key_target_absent" "pass" "HTTP 502; target absent"

  cleanup_verified_targets || die "exact-name target cleanup failed or left residue"
  record_check "target_cleanup" "pass" "all ledgered target names absent"
}
