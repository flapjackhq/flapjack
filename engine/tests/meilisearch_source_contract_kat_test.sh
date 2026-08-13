#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUNNER="$SCRIPT_DIR/meilisearch_source_contract_kat.sh"
FIXTURE_DIR="$SCRIPT_DIR/fixtures/2026_07_26_m0a_meilisearch_source_contract"
EXPECTED_SCENARIO_IDS='
positive_control
missing_port_probe_tool
port_probe_operational_failure
docker_inventory_failure
partial_container_launch_cleanup
indeterminate_container_inspection
image_digest_mismatch
expected_primary_keys_drift
pagination_offsets_drift
pagination_limit_drift
empty_task_captures
wrong_record_value
wrong_record_count
configured_primary_key_drift
inferred_primary_key_drift
ambiguous_primary_key_metadata_drift
ambiguous_primary_key_acceptance
ambiguous_task_response_uid_drift
dropped_stable_id
duplicate_stable_id
changed_settings
changed_synonyms
nonterminal_task_acceptance
dump_task_uid_drift
dump_task_status_drift
dump_task_type_drift
snapshot_task_uid_drift
snapshot_task_status_drift
snapshot_task_type_drift
source_mutation_during_capture
missing_required_read_action
restricted_probe_path_drift
restricted_probe_body_drift
warning_identifier_drift
credential_leakage
truncated_pagination
cleanup_residue
search_limit_as_export
http_status_only_correctness
preview_fixture_count
preview_zero_match
preview_timeout
'

TMP_DIR=""
TESTS_RUN=0
TESTS_PASSED=0

die() {
  printf 'ERROR: %s\n' "$*" >&2
  exit 1
}

cleanup() {
  [[ -z "$TMP_DIR" ]] || rm -rf -- "$TMP_DIR"
}

scenario_id_for_label() {
  case "$1" in
    'positive control') printf '%s\n' positive_control ;;
    'port probe operational failure') printf '%s\n' port_probe_operational_failure ;;
    'Docker inventory failure') printf '%s\n' docker_inventory_failure ;;
    'partial container launch cleanup') printf '%s\n' partial_container_launch_cleanup ;;
    'indeterminate container inspection') printf '%s\n' indeterminate_container_inspection ;;
    'image digest mismatch') printf '%s\n' image_digest_mismatch ;;
    'expected primary keys drift') printf '%s\n' expected_primary_keys_drift ;;
    'pagination offsets drift') printf '%s\n' pagination_offsets_drift ;;
    'pagination limit drift') printf '%s\n' pagination_limit_drift ;;
    'empty task captures') printf '%s\n' empty_task_captures ;;
    'wrong record value') printf '%s\n' wrong_record_value ;;
    'wrong record count') printf '%s\n' wrong_record_count ;;
    'configured primary key drift') printf '%s\n' configured_primary_key_drift ;;
    'inferred primary key drift') printf '%s\n' inferred_primary_key_drift ;;
    'ambiguous primary key metadata drift') printf '%s\n' ambiguous_primary_key_metadata_drift ;;
    'ambiguous primary key acceptance') printf '%s\n' ambiguous_primary_key_acceptance ;;
    'ambiguous task response UID drift') printf '%s\n' ambiguous_task_response_uid_drift ;;
    'dropped stable ID') printf '%s\n' dropped_stable_id ;;
    'duplicate stable ID') printf '%s\n' duplicate_stable_id ;;
    'changed settings') printf '%s\n' changed_settings ;;
    'changed synonyms') printf '%s\n' changed_synonyms ;;
    'nonterminal task acceptance') printf '%s\n' nonterminal_task_acceptance ;;
    'dump task UID drift') printf '%s\n' dump_task_uid_drift ;;
    'dump task status drift') printf '%s\n' dump_task_status_drift ;;
    'dump task type drift') printf '%s\n' dump_task_type_drift ;;
    'snapshot task UID drift') printf '%s\n' snapshot_task_uid_drift ;;
    'snapshot task status drift') printf '%s\n' snapshot_task_status_drift ;;
    'snapshot task type drift') printf '%s\n' snapshot_task_type_drift ;;
    'source mutation during capture') printf '%s\n' source_mutation_during_capture ;;
    'missing required read action') printf '%s\n' missing_required_read_action ;;
    'restricted probe path drift') printf '%s\n' restricted_probe_path_drift ;;
    'restricted probe body drift') printf '%s\n' restricted_probe_body_drift ;;
    'warning identifier drift') printf '%s\n' warning_identifier_drift ;;
    'credential leakage') printf '%s\n' credential_leakage ;;
    'truncated pagination') printf '%s\n' truncated_pagination ;;
    'cleanup residue') printf '%s\n' cleanup_residue ;;
    'search limit as export') printf '%s\n' search_limit_as_export ;;
    'HTTP status only correctness') printf '%s\n' http_status_only_correctness ;;
    'preview fixture count') printf '%s\n' preview_fixture_count ;;
    'preview zero match') printf '%s\n' preview_zero_match ;;
    'preview timeout') printf '%s\n' preview_timeout ;;
    'missing port probe tool') printf '%s\n' missing_port_probe_tool ;;
    *) die "unmapped scenario label: $1" ;;
  esac
}

assert_scenario_inventory() {
  local mapped expected
  mapped="$(
    while IFS= read -r label; do
      [[ -z "$label" ]] || scenario_id_for_label "$label"
    done <<'EOF'
positive control
port probe operational failure
Docker inventory failure
partial container launch cleanup
indeterminate container inspection
image digest mismatch
expected primary keys drift
pagination offsets drift
pagination limit drift
empty task captures
wrong record value
wrong record count
configured primary key drift
inferred primary key drift
ambiguous primary key metadata drift
ambiguous primary key acceptance
ambiguous task response UID drift
dropped stable ID
duplicate stable ID
changed settings
changed synonyms
nonterminal task acceptance
dump task UID drift
dump task status drift
dump task type drift
snapshot task UID drift
snapshot task status drift
snapshot task type drift
source mutation during capture
missing required read action
restricted probe path drift
restricted probe body drift
warning identifier drift
credential leakage
truncated pagination
cleanup residue
search limit as export
HTTP status only correctness
missing port probe tool
preview fixture count
preview zero match
preview timeout
EOF
  )"
  expected="$(printf '%s\n' "$EXPECTED_SCENARIO_IDS" | sed '/^$/d')"
  [[ "$(printf '%s\n' "$mapped" | sort)" == "$(printf '%s\n' "$expected" | sort)" ]] \
    || die "scenario denominator does not match EXPECTED_SCENARIO_IDS"
  [[ "$(printf '%s\n' "$mapped" | sort -u | wc -l | tr -d ' ')" == 42 ]] \
    || die "scenario denominator must contain 42 unique cases"
}

write_response() {
  local response_dir="$1" label="$2" status="$3" body="$4"
  printf '%s\n' "$status" >"$response_dir/${label}.status"
  jq -c . <<<"$body" >"$response_dir/${label}.json"
}

build_stub_responses() {
  local fixture_dir="$1" response_dir="$2" expected before after inferred settings
  expected="$fixture_dir/expected_bundle.json"
  before="$(jq -c '.documents.beforeMutation' "$expected")"
  after="$(jq -c '.documents.afterMutation' "$expected")"
  inferred="$(cat "$fixture_dir/inferred_primary_key_documents.json")"
  settings="$(jq -c '.settings' "$expected")"

  mkdir -p "$response_dir"
  write_response "$response_dir" version 200 "$(jq -c '.source.version' "$expected")"
  write_response "$response_dir" indexes 200 "$(jq -c '{
    total: 3,
    offset: 0,
    limit: 10,
    results: [
      {uid: .indexes.ambiguous.uid, primaryKey: .indexes.ambiguous.primaryKey},
      {uid: .indexes.configured.uid, primaryKey: .indexes.configured.primaryKey},
      {uid: .indexes.inferred.uid, primaryKey: .indexes.inferred.primaryKey}
    ]
  }' "$expected")"
  write_response "$response_dir" configured_page_0 200 \
    "$(jq -cn --argjson docs "$before" '{offset:0,limit:2,total:3,results:$docs[0:2]}')"
  write_response "$response_dir" configured_page_1 200 \
    "$(jq -cn --argjson docs "$before" '{offset:2,limit:2,total:3,results:$docs[2:4]}')"
  write_response "$response_dir" inferred_page_0 200 \
    "$(jq -cn --argjson docs "$inferred" '{offset:0,limit:20,total:2,results:$docs}')"
  write_response "$response_dir" settings 200 "$settings"
  write_response "$response_dir" synonyms 200 "$(jq -c '.synonyms' "$expected")"
  write_response "$response_dir" ambiguous_task_poll_0 200 \
    "$(jq -c '{uid:.tasks.ambiguous.uid,status:"failed",type:"documentAdditionOrUpdate",error:{code:.tasks.ambiguous.failureCode}}' "$expected")"

  write_response "$response_dir" configured_index_before 200 \
    '{"uid":"configured_pk","primaryKey":"sku","updatedAt":"2026-07-26T19:20:26Z"}'
  write_response "$response_dir" configured_index_capture_after 200 \
    '{"uid":"configured_pk","primaryKey":"sku","updatedAt":"2026-07-26T19:20:26Z"}'
  write_response "$response_dir" stats_before 200 "$(jq -c '{
    databaseSize: .documents.databaseSizeBefore,
    indexes: {configured_pk: {
      numberOfDocuments: .documents.countBefore,
      fieldDistribution: .documents.fieldDistributionBefore
    }}
  }' "$expected")"
  cp "$response_dir/stats_before.json" "$response_dir/stats_capture_after.json"
  cp "$response_dir/stats_before.status" "$response_dir/stats_capture_after.status"
  write_response "$response_dir" tasks_before 200 \
    '{"results":[{"uid":1,"status":"succeeded"},{"uid":2,"status":"succeeded"},{"uid":3,"status":"succeeded"},{"uid":4,"status":"succeeded"},{"uid":5,"status":"succeeded"},{"uid":6,"status":"failed"}]}'
  cp "$response_dir/tasks_before.json" "$response_dir/tasks_capture_after.json"
  cp "$response_dir/tasks_before.status" "$response_dir/tasks_capture_after.status"

  write_response "$response_dir" mutation_task_poll_0 200 \
    "$(jq -c '{uid:.tasks.mutation.uid,status:.tasks.mutation.status,type:.tasks.mutation.type,indexUid:"configured_pk",details:{receivedDocuments:1,indexedDocuments:1},error:null}' "$expected")"
  write_response "$response_dir" configured_index_after 200 \
    '{"uid":"configured_pk","primaryKey":"sku","updatedAt":"2026-07-26T19:20:28Z"}'
  write_response "$response_dir" configured_after_page_0 200 \
    "$(jq -cn --argjson docs "$after" '{offset:0,limit:10,total:4,results:$docs}')"
  write_response "$response_dir" stats_after 200 "$(jq -c '{
    databaseSize: .documents.databaseSizeAfter,
    indexes: {configured_pk: {
      numberOfDocuments: .documents.countAfter,
      fieldDistribution: .documents.fieldDistributionAfter
    }}
  }' "$expected")"
  write_response "$response_dir" tasks_after 200 \
    '{"results":[{"uid":7,"status":"succeeded"},{"uid":6,"status":"failed"},{"uid":5,"status":"succeeded"}]}'

  local action label
  while IFS= read -r action; do
    label="restricted_$(tr '.-' '__' <<<"$action")"
    write_response "$response_dir" "$label" 200 '{"ok":true}'
    write_response "$response_dir" "denied_${label}" 403 \
      '{"message":"The provided API key is invalid.","code":"invalid_api_key","type":"auth","link":"https://docs.meilisearch.com/errors#invalid_api_key"}'
  done < <(jq -r '.requiredActions[]' "$expected")
  write_response "$response_dir" restricted_dumps_create 202 \
    "$(jq -c '{taskUid:.tasks.dump.uid}' "$expected")"
  write_response "$response_dir" dump_task_poll_0 200 \
    "$(jq -c '.tasks.dump + {error:null}' "$expected")"
  write_response "$response_dir" restricted_snapshots_create 202 \
    "$(jq -c '{taskUid:.tasks.snapshot.uid}' "$expected")"
  write_response "$response_dir" snapshot_task_poll_0 200 \
    "$(jq -c '.tasks.snapshot + {error:null}' "$expected")"

  jq -n '{
    containerPresent:false,
    tempDirPresent:false,
    rawLogsPresent:false,
    credentialFilesPresent:false
  }' >"$response_dir/cleanup_state.json"
}

copy_fixture() {
  local destination="$1"
  mkdir -p "$destination"
  cp "$FIXTURE_DIR"/*.json "$destination/"
}

run_runner() {
  local fixture_dir="$1" response_dir="$2" output_file="$3"
  MEILI_TEST_SECRET_CANARY='kat-secret-canary-never-commit' \
    bash "$RUNNER" --fixture-dir "$fixture_dir" \
      --stub-response-dir "$response_dir" >"$output_file" 2>&1
}

assert_positive_control() {
  local case_dir fixture responses output
  case_dir="$TMP_DIR/positive"
  fixture="$case_dir/fixture"
  responses="$case_dir/responses"
  output="$case_dir/output"
  mkdir -p "$case_dir"
  copy_fixture "$fixture"
  build_stub_responses "$fixture" "$responses"
  run_runner "$fixture" "$responses" "$output" \
    || die "positive control failed: $(tail -20 "$output")"

  local receipt trace
  receipt="$(tail -1 "$output")"
  trace="$responses/request_trace.jsonl"
  jq -e '
    .result == "PASS" and
    .sortedStableIds == ["SKU-001","SKU-002","SKU-003"] and
    .taskPolling.bounded == true and
    .cleanup.containerName == "flapjack_stage2_meilisearch_source_contract" and
    .cleanup.tempDir == "tests/flapjack_stage2_meilisearch_source_contract_tmp"
  ' <<<"$receipt" >/dev/null || die "positive receipt omitted exact contract proof"
  jq -s -e '
    any(.[]; .label == "configured_page_0" and .method == "POST" and
      .path == "/indexes/configured_pk/documents/fetch" and .jsonParsed == true) and
    any(.[]; .label == "configured_page_1" and .method == "POST" and
      .path == "/indexes/configured_pk/documents/fetch" and .jsonParsed == true) and
    any(.[]; .label == "mutation_task_poll_0" and
      .path == "/tasks/7" and .jsonParsed == true)
  ' "$trace" >/dev/null || die "positive control did not exercise POST fetch and bounded task polling"
}

mutate_case() {
  local scenario="$1" fixture="$2" responses="$3"
  case "$scenario" in
    image_digest_mismatch)
      jq '.source.imageDigest = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"' \
        "$fixture/expected_bundle.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/expected_bundle.json"
      ;;
    expected_primary_keys_drift)
      jq '.expectedPrimaryKeys.configured_pk = "id"' \
        "$fixture/expected_bundle.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/expected_bundle.json"
      ;;
    pagination_offsets_drift)
      jq '.pagination.offsets = [1, 3]' \
        "$fixture/expected_bundle.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/expected_bundle.json"
      ;;
    pagination_limit_drift)
      jq '.pagination.limit = 3' \
        "$fixture/expected_bundle.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/expected_bundle.json"
      ;;
    empty_task_captures)
      local task_capture
      for task_capture in tasks_before tasks_capture_after tasks_after; do
        jq '.results = []' "$responses/${task_capture}.json" \
          >"$responses/next" && mv "$responses/next" "$responses/${task_capture}.json"
      done
      ;;
    wrong_record_value)
      jq '.results[1].price = 999' "$responses/configured_page_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/configured_page_0.json"
      ;;
    wrong_record_count)
      jq '.total = 2' "$responses/configured_page_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/configured_page_0.json"
      ;;
    configured_primary_key_drift)
      jq '(.results[] | select(.uid == "configured_pk").primaryKey) = "id"' \
        "$responses/indexes.json" >"$responses/next" && mv "$responses/next" "$responses/indexes.json"
      ;;
    inferred_primary_key_drift)
      jq '(.results[] | select(.uid == "inferred_pk").primaryKey) = "id"' \
        "$responses/indexes.json" >"$responses/next" && mv "$responses/next" "$responses/indexes.json"
      ;;
    ambiguous_primary_key_metadata_drift)
      jq '(.results[] | select(.uid == "ambiguous_pk").primaryKey) = "id"' \
        "$responses/indexes.json" >"$responses/next" && mv "$responses/next" "$responses/indexes.json"
      ;;
    ambiguous_primary_key_acceptance)
      jq '.status = "succeeded" | .error = null' "$responses/ambiguous_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/ambiguous_task_poll_0.json"
      ;;
    ambiguous_task_response_uid_drift)
      jq '.uid += 100' "$responses/ambiguous_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/ambiguous_task_poll_0.json"
      ;;
    dropped_stable_id)
      jq '.results[0].sku = "SKU-999"' "$responses/configured_page_1.json" \
        >"$responses/next" && mv "$responses/next" "$responses/configured_page_1.json"
      ;;
    duplicate_stable_id)
      jq '.results[0].sku = "SKU-002"' "$responses/configured_page_1.json" \
        >"$responses/next" && mv "$responses/next" "$responses/configured_page_1.json"
      ;;
    changed_settings)
      jq '.pagination.maxTotalHits = 51' "$responses/settings.json" \
        >"$responses/next" && mv "$responses/next" "$responses/settings.json"
      ;;
    changed_synonyms)
      jq '.wrench = ["tool"]' "$responses/synonyms.json" \
        >"$responses/next" && mv "$responses/next" "$responses/synonyms.json"
      ;;
    nonterminal_task_acceptance)
      jq '.results[0].status = "processing"' "$responses/tasks_after.json" \
        >"$responses/next" && mv "$responses/next" "$responses/tasks_after.json"
      ;;
    dump_task_uid_drift)
      jq '.uid += 100' "$responses/dump_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/dump_task_poll_0.json"
      ;;
    dump_task_status_drift)
      jq '.status = "failed"' "$responses/dump_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/dump_task_poll_0.json"
      ;;
    dump_task_type_drift)
      jq '.type = "snapshotCreation"' "$responses/dump_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/dump_task_poll_0.json"
      ;;
    snapshot_task_uid_drift)
      jq '.uid += 100' "$responses/snapshot_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/snapshot_task_poll_0.json"
      ;;
    snapshot_task_status_drift)
      jq '.status = "failed"' "$responses/snapshot_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/snapshot_task_poll_0.json"
      ;;
    snapshot_task_type_drift)
      jq '.type = "dumpCreation"' "$responses/snapshot_task_poll_0.json" \
        >"$responses/next" && mv "$responses/next" "$responses/snapshot_task_poll_0.json"
      ;;
    source_mutation_during_capture)
      jq '.updatedAt = "2026-07-26T19:20:27Z"' \
        "$responses/configured_index_capture_after.json" >"$responses/next" \
        && mv "$responses/next" "$responses/configured_index_capture_after.json"
      ;;
    missing_required_read_action)
      jq '.probes |= map(select(.action != "stats.get"))' \
        "$fixture/restricted_key_action_probes.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/restricted_key_action_probes.json"
      ;;
    restricted_probe_path_drift)
      jq '(.probes[] | select(.action == "documents.get").path) = "/indexes/configured_pk/search"' \
        "$fixture/restricted_key_action_probes.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/restricted_key_action_probes.json"
      ;;
    restricted_probe_body_drift)
      jq '(.probes[] | select(.action == "documents.get").body.limit) = 2' \
        "$fixture/restricted_key_action_probes.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/restricted_key_action_probes.json"
      ;;
    warning_identifier_drift)
      jq '.warningIdentifiers[0] = "meili_wrong_warning_identifier"' \
        "$fixture/expected_bundle.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/expected_bundle.json"
      ;;
    credential_leakage)
      jq '.debug = "kat-secret-canary-never-commit"' "$responses/version.json" \
        >"$responses/next" && mv "$responses/next" "$responses/version.json"
      ;;
    truncated_pagination)
      jq '.results = []' "$responses/configured_page_1.json" \
        >"$responses/next" && mv "$responses/next" "$responses/configured_page_1.json"
      ;;
    cleanup_residue)
      jq '.containerPresent = true' "$responses/cleanup_state.json" \
        >"$responses/next" && mv "$responses/next" "$responses/cleanup_state.json"
      ;;
    search_limit_as_export)
      jq '.pagination.documentExportPath = "/indexes/configured_pk/search"' \
        "$fixture/expected_bundle.json" >"$fixture/next" \
        && mv "$fixture/next" "$fixture/expected_bundle.json"
      ;;
    http_status_only_correctness)
      printf '%s\n' 'not-json' >"$responses/configured_page_0.json"
      ;;
    *) die "unknown mutation case: $scenario" ;;
  esac
}

expected_failure_text() {
  case "$1" in
    image_digest_mismatch) printf '%s\n' 'source image digest mismatch' ;;
    expected_primary_keys_drift) printf '%s\n' 'primary key oracle mismatch' ;;
    pagination_offsets_drift) printf '%s\n' 'pagination request mismatch' ;;
    pagination_limit_drift) printf '%s\n' 'pagination request mismatch' ;;
    empty_task_captures) printf '%s\n' 'task evidence missing' ;;
    wrong_record_value) printf '%s\n' 'document hash before mutation mismatch' ;;
    wrong_record_count) printf '%s\n' 'pagination total mismatch' ;;
    configured_primary_key_drift) printf '%s\n' 'configured primary key mismatch' ;;
    inferred_primary_key_drift) printf '%s\n' 'inferred primary key mismatch' ;;
    ambiguous_primary_key_metadata_drift) printf '%s\n' 'ambiguous primary key metadata mismatch' ;;
    ambiguous_primary_key_acceptance) printf '%s\n' 'ambiguous primary key task was accepted' ;;
    ambiguous_task_response_uid_drift) printf '%s\n' 'ambiguous primary key task mismatch' ;;
    dropped_stable_id) printf '%s\n' 'stable ID set mismatch' ;;
    duplicate_stable_id) printf '%s\n' 'duplicate stable ID' ;;
    changed_settings) printf '%s\n' 'settings mismatch' ;;
    changed_synonyms) printf '%s\n' 'synonyms mismatch' ;;
    nonterminal_task_acceptance) printf '%s\n' 'nonterminal task status' ;;
    dump_task_uid_drift) printf '%s\n' 'dump task mismatch' ;;
    dump_task_status_drift) printf '%s\n' 'dump task mismatch' ;;
    dump_task_type_drift) printf '%s\n' 'dump task mismatch' ;;
    snapshot_task_uid_drift) printf '%s\n' 'snapshot task mismatch' ;;
    snapshot_task_status_drift) printf '%s\n' 'snapshot task mismatch' ;;
    snapshot_task_type_drift) printf '%s\n' 'snapshot task mismatch' ;;
    source_mutation_during_capture) printf '%s\n' 'source mutated during capture' ;;
    missing_required_read_action) printf '%s\n' 'required read actions mismatch' ;;
    restricted_probe_path_drift) printf '%s\n' 'restricted key action probe contract mismatch' ;;
    restricted_probe_body_drift) printf '%s\n' 'restricted key action probe contract mismatch' ;;
    warning_identifier_drift) printf '%s\n' 'warning identifier contract mismatch' ;;
    credential_leakage) printf '%s\n' 'credential leakage detected' ;;
    truncated_pagination) printf '%s\n' 'page count mismatch' ;;
    cleanup_residue) printf '%s\n' 'cleanup residue detected' ;;
    search_limit_as_export) printf '%s\n' 'document export must use POST fetch' ;;
    http_status_only_correctness) printf '%s\n' 'response configured_page_0 is not valid JSON' ;;
    *) die "missing failure text for $1" ;;
  esac
}

assert_mutation_rejected() {
  local scenario="$1" case_dir fixture responses output expected_text status
  case_dir="$TMP_DIR/$scenario"
  fixture="$case_dir/fixture"
  responses="$case_dir/responses"
  output="$case_dir/output"
  mkdir -p "$case_dir"
  copy_fixture "$fixture"
  build_stub_responses "$fixture" "$responses"
  mutate_case "$scenario" "$fixture" "$responses"
  expected_text="$(expected_failure_text "$scenario")"

  set +e
  run_runner "$fixture" "$responses" "$output"
  status=$?
  set -e
  if [[ "$status" -eq 0 ]]; then
    die "ORACLE_NOT_LOAD_BEARING: $scenario remained green"
  fi
  if ! grep -Fq "$expected_text" "$output"; then
    die "ORACLE_WRONG_FAILURE: $scenario did not fail with '$expected_text': $(tail -20 "$output")"
  fi
  if grep -Eq 'command not found|No such file or directory|syntax error|invalid JSON text passed' "$output"; then
    die "ORACLE_INCIDENTAL_FAILURE: $scenario failed outside its intended assertion"
  fi
  assert_rejection_stopped_at_its_guard "$scenario" "$responses" "$output"
}

# Exit status plus expected text only proves the oracle failed with the right
# words. These trace assertions prove *where* it stopped: a guard that fires
# after the source was already re-read, mutated, or receipted is a guard that
# fired too late to protect anything.
assert_rejection_stopped_at_its_guard() {
  local scenario="$1" responses="$2" output="$3"
  case "$scenario" in
    dropped_stable_id | duplicate_stable_id)
      assert_stable_id_failure_stops_before_downstream_source_reads "$scenario" "$responses"
      ;;
    source_mutation_during_capture)
      assert_source_mutation_blocks_controlled_mutation "$responses"
      ;;
    cleanup_residue)
      assert_cleanup_residue_blocks_receipt "$responses" "$output"
      ;;
  esac
}

assert_trace_contains_label() {
  local trace="$1" label="$2"
  jq -s -e --arg label "$label" 'any(.[]; .label == $label)' "$trace" >/dev/null \
    || die "ORACLE_NOT_LOAD_BEARING: trace never reached $label"
}

assert_trace_omits_label() {
  local trace="$1" label="$2"
  jq -s -e --arg label "$label" 'all(.[]; .label != $label)' "$trace" >/dev/null \
    || die "ORACLE_NOT_LOAD_BEARING: trace continued into $label"
}

assert_stable_id_failure_stops_before_downstream_source_reads() {
  local scenario="$1" responses="$2" trace="$responses/request_trace.jsonl"
  assert_trace_contains_label "$trace" configured_page_0
  assert_trace_contains_label "$trace" configured_page_1
  assert_trace_omits_label "$trace" inferred_page_0
  assert_trace_omits_label "$trace" settings
  assert_trace_omits_label "$trace" configured_index_before
  assert_trace_omits_label "$trace" mutation_task_poll_0
  assert_trace_omits_label "$trace" configured_after_page_0
  printf 'ASSERT %s stopped before downstream source reads\n' "$scenario"
}

assert_source_mutation_blocks_controlled_mutation() {
  local responses="$1" trace="$responses/request_trace.jsonl"
  assert_trace_contains_label "$trace" configured_index_before
  assert_trace_contains_label "$trace" stats_before
  assert_trace_contains_label "$trace" tasks_before
  assert_trace_contains_label "$trace" configured_index_capture_after
  assert_trace_contains_label "$trace" stats_capture_after
  assert_trace_contains_label "$trace" tasks_capture_after
  assert_trace_omits_label "$trace" mutation_task_poll_0
  assert_trace_omits_label "$trace" configured_after_page_0
  assert_trace_omits_label "$trace" stats_after
  printf 'ASSERT source mutation blocked controlled mutation\n'
}

assert_cleanup_residue_blocks_receipt() {
  local responses="$1" output="$2"
  jq -e 'any(.[]; . == true)' "$responses/cleanup_state.json" >/dev/null \
    || die "ORACLE_NOT_LOAD_BEARING: cleanup_residue did not seed residue"
  if tail -1 "$output" | jq -e '.result == "PASS"' >/dev/null 2>&1; then
    die "ORACLE_NOT_LOAD_BEARING: cleanup residue still emitted a PASS receipt"
  fi
  printf 'ASSERT cleanup residue blocked PASS receipt\n'
}

write_preflight_tool_shim() {
  local tool_dir="$1" tool="$2" body="$3"
  printf '#!/bin/bash\n%s\n' "$body" >"$tool_dir/$tool"
  chmod +x "$tool_dir/$tool"
}

assert_missing_port_probe_tool_rejected() {
  local case_dir fixture tool_dir output marker status
  case_dir="$TMP_DIR/missing_port_probe_tool"
  fixture="$case_dir/fixture"
  tool_dir="$case_dir/tools"
  output="$case_dir/output"
  marker="$case_dir/docker_executed"
  mkdir -p "$tool_dir"
  copy_fixture "$fixture"
  ln -s "$(command -v basename)" "$tool_dir/basename"
  ln -s "$(command -v dirname)" "$tool_dir/dirname"
  ln -s "$(command -v jq)" "$tool_dir/jq"
  write_preflight_tool_shim "$tool_dir" colima 'exit 0'
  write_preflight_tool_shim "$tool_dir" curl 'exit 1'
  # The marker expansion belongs to the generated Docker shim, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" docker \
    ': >"${PORT_PROBE_DOCKER_MARKER:?}"; exit 1'

  set +e
  PATH="$tool_dir" PORT_PROBE_DOCKER_MARKER="$marker" \
    /bin/bash "$RUNNER" --fixture-dir "$fixture" --live >"$output" 2>&1
  status=$?
  set -e
  [[ "$status" -ne 0 ]] \
    || die "ORACLE_NOT_LOAD_BEARING: missing_port_probe_tool remained green"
  grep -Fq 'lsof is required for --live' "$output" \
    || die "ORACLE_WRONG_FAILURE: missing_port_probe_tool did not fail closed: $(tail -20 "$output")"
  [[ ! -e "$marker" ]] \
    || die "ORACLE_NOT_LOAD_BEARING: Docker executed without a port probe tool"
}

write_safety_gate_shims() {
  local tool_dir="$1"
  write_preflight_tool_shim "$tool_dir" colima 'exit 0'
  # Expansion belongs to the generated lsof shim, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" lsof \
    '[[ -z "${SAFETY_GATE_LSOF_ERROR:-}" ]] || printf "%s\n" "$SAFETY_GATE_LSOF_ERROR" >&2
exit "${SAFETY_GATE_LSOF_STATUS:?}"'
  # Expansions belong to the generated Docker shim, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" docker '
if [[ "${1:-} ${2:-}" == "container inspect" ]]; then
  printf "%s\n" "Error response from daemon: No such container: ${3:-}" >&2
  exit 1
fi
if [[ "${1:-} ${2:-}" == "ps --format" ]]; then
  : >"${SAFETY_GATE_INVENTORY_MARKER:?}"
  exit "${SAFETY_GATE_DOCKER_PS_STATUS:?}"
fi
exit 2'
  # Expansions belong to the generated harness, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" safety_gate_harness '
source "${SAFETY_GATE_RUNNER:?}"
MODE=live
EXPECTED="${SAFETY_GATE_EXPECTED:?}"
SCRIPT_DIR="${SAFETY_GATE_SCRIPT_DIR:?}"
live_performance_lease_is_active() {
  return 1
}
docker_safety_gate'
}

assert_safety_probe_failure_rejected() {
  local scenario="$1" expected_text="$2" lsof_status="$3" docker_ps_status="$4"
  local case_dir fixture tool_dir output inventory_marker harness lsof_error status
  case_dir="$TMP_DIR/$scenario"
  fixture="$case_dir/fixture"
  tool_dir="$case_dir/tools"
  output="$case_dir/output"
  inventory_marker="$case_dir/docker_inventory_executed"
  harness="$tool_dir/safety_gate_harness"
  lsof_error=""
  [[ "$scenario" != port_probe_operational_failure ]] \
    || lsof_error="simulated lsof operational failure"
  mkdir -p "$tool_dir"
  copy_fixture "$fixture"
  write_safety_gate_shims "$tool_dir"
  set +e
  PATH="$tool_dir:$PATH" \
    SAFETY_GATE_RUNNER="$RUNNER" \
    SAFETY_GATE_EXPECTED="$fixture/expected_bundle.json" \
    SAFETY_GATE_SCRIPT_DIR="$(dirname "$RUNNER")" \
    SAFETY_GATE_LSOF_STATUS="$lsof_status" \
    SAFETY_GATE_LSOF_ERROR="$lsof_error" \
    SAFETY_GATE_DOCKER_PS_STATUS="$docker_ps_status" \
    SAFETY_GATE_INVENTORY_MARKER="$inventory_marker" \
    bash "$harness" >"$output" 2>&1
  status=$?
  set -e
  [[ "$status" -ne 0 ]] \
    || die "ORACLE_NOT_LOAD_BEARING: $scenario remained green"
  grep -Fq "$expected_text" "$output" \
    || die "ORACLE_WRONG_FAILURE: $scenario did not fail with '$expected_text': $(tail -20 "$output")"
  if [[ "$scenario" == port_probe_operational_failure && -e "$inventory_marker" ]]; then
    die "ORACLE_NOT_LOAD_BEARING: Docker inventory ran after port probe failure"
  fi
}

assert_partial_container_launch_cleanup() {
  local case_dir fixture tool_dir output state cleanup_marker harness status
  case_dir="$TMP_DIR/partial_container_launch_cleanup"
  fixture="$case_dir/fixture"
  tool_dir="$case_dir/tools"
  output="$case_dir/output"
  state="$case_dir/container_present"
  cleanup_marker="$case_dir/container_removed"
  harness="$tool_dir/partial_launch_harness"
  mkdir -p "$tool_dir"
  copy_fixture "$fixture"
  write_preflight_tool_shim "$tool_dir" colima 'exit 0'
  write_preflight_tool_shim "$tool_dir" lsof 'exit 1'
  # Expansions belong to the generated Docker shim, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" docker '
case "${1:-} ${2:-}" in
  "container inspect")
    if [[ -e "${PARTIAL_LAUNCH_STATE:?}" ]]; then
      printf "%s\n" "{}"
      exit 0
    fi
    printf "%s\n" "Error response from daemon: No such container: ${3:-}" >&2
    exit 1
    ;;
  "ps --format")
    exit 0
    ;;
  "run -d")
    : >"${PARTIAL_LAUNCH_STATE:?}"
    printf "%s\n" "simulated Docker start failure" >&2
    exit 1
    ;;
  "rm -f")
    [[ "${3:-}" == "${PARTIAL_LAUNCH_EXPECTED_CONTAINER:?}" ]] || exit 3
    rm -f -- "${PARTIAL_LAUNCH_STATE:?}"
    : >"${PARTIAL_LAUNCH_CLEANUP_MARKER:?}"
    exit 0
    ;;
esac
printf "%s\n" "unexpected Docker shim arguments: $*" >&2
exit 2'
  # Expansions belong to the generated harness, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" partial_launch_harness '
source "${PARTIAL_LAUNCH_RUNNER:?}"
MODE=live
EXPECTED="${PARTIAL_LAUNCH_EXPECTED:?}"
SCRIPT_DIR="${PARTIAL_LAUNCH_SCRIPT_DIR:?}"
CONTAINER_NAME="$(jq -r ".cleanup.containerName" "$EXPECTED")"
TEMP_DIR="$SCRIPT_DIR/flapjack_stage2_meilisearch_source_contract_tmp"
docker_safety_gate() {
  return 0
}
trap cleanup_on_exit EXIT
start_live_source'

  set +e
  PATH="$tool_dir:$PATH" \
    PARTIAL_LAUNCH_RUNNER="$RUNNER" \
    PARTIAL_LAUNCH_EXPECTED="$fixture/expected_bundle.json" \
    PARTIAL_LAUNCH_SCRIPT_DIR="$case_dir" \
    PARTIAL_LAUNCH_STATE="$state" \
    PARTIAL_LAUNCH_CLEANUP_MARKER="$cleanup_marker" \
    PARTIAL_LAUNCH_EXPECTED_CONTAINER="flapjack_stage2_meilisearch_source_contract" \
    bash "$harness" >"$output" 2>&1
  status=$?
  set -e
  [[ "$status" -ne 0 ]] \
    || die "ORACLE_NOT_LOAD_BEARING: partial_container_launch_cleanup remained green"
  grep -Fq 'simulated Docker start failure' "$output" \
    || die "ORACLE_WRONG_FAILURE: partial launch did not reach Docker run: $(tail -20 "$output")"
  [[ -e "$cleanup_marker" ]] \
    || die "ORACLE_NOT_LOAD_BEARING: partial launch did not attempt exact-name cleanup"
  [[ ! -e "$state" ]] \
    || die "ORACLE_NOT_LOAD_BEARING: partial launch left container residue"
}

assert_indeterminate_container_inspection_rejected() {
  local case_dir tool_dir output harness status
  case_dir="$TMP_DIR/indeterminate_container_inspection"
  tool_dir="$case_dir/tools"
  output="$case_dir/output"
  harness="$tool_dir/inspection_failure_harness"
  mkdir -p "$tool_dir"
  # Expansions belong to the generated Docker shim, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" docker '
case "${1:-} ${2:-}" in
  "rm -f")
    exit 0
    ;;
  "container inspect")
    printf "%s\n" "Cannot connect to the Docker daemon" >&2
    exit 1
    ;;
esac
exit 2'
  # Expansions belong to the generated harness, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" inspection_failure_harness '
source "${INSPECTION_FAILURE_RUNNER:?}"
MODE=live
SCRIPT_DIR="${INSPECTION_FAILURE_SCRIPT_DIR:?}"
CONTAINER_NAME="flapjack_stage2_meilisearch_source_contract"
TEMP_DIR="$SCRIPT_DIR/flapjack_stage2_meilisearch_source_contract_tmp"
CONTAINER_CLEANUP_ARMED=1
mkdir -p "$TEMP_DIR"
if cleanup_live; then
  printf "%s\n" "container inspection failure was accepted" >&2
  exit 0
fi
printf "%s\n" "container inspection failure rejected" >&2
exit 7'

  set +e
  PATH="$tool_dir:$PATH" \
    INSPECTION_FAILURE_RUNNER="$RUNNER" \
    INSPECTION_FAILURE_SCRIPT_DIR="$case_dir" \
    bash "$harness" >"$output" 2>&1
  status=$?
  set -e
  [[ "$status" -eq 7 ]] \
    || die "ORACLE_NOT_LOAD_BEARING: indeterminate inspection was accepted: $(tail -20 "$output")"
  grep -Fq 'container inspection failure rejected' "$output" \
    || die "ORACLE_WRONG_FAILURE: inspection failure did not fail closed"
}

assert_preview_probe_contract() {
  local scenario="$1" case_dir tool_dir output harness expected_records shim_mode status fake_engine
  case_dir="$TMP_DIR/$scenario"
  tool_dir="$case_dir/tools"
  output="$case_dir/output"
  harness="$case_dir/preview_probe_harness"
  fake_engine="$case_dir/fake_engine"
  expected_records="$(jq -er '.documents.countAfter' "$FIXTURE_DIR/expected_bundle.json")"
  mkdir -p "$tool_dir" "$fake_engine/target/release"

  # Expansions belong to the generated timeout shim, not this process.
  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" timeout '
if [[ "${1:-} ${2:-} ${3:-}" == "1800 cargo build" ]]; then
  exit 0
fi
[[ "${1:-}" == "600" && "${2:-}" == "cargo" ]] || exit 8
case "${PREVIEW_SHIM_MODE:?}" in
  success)
    [[ "${FJ_MEILISEARCH_PREVIEW_EXPECTED_RECORDS:-}" == "${PREVIEW_EXPECTED_RECORDS:?}" ]] \
      || exit 9
    printf "%s\n" \
      "running 1 test" \
      "{\"previewProof\":\"PASS\",\"sourceCounts\":{\"indexes\":1,\"records\":${PREVIEW_EXPECTED_RECORDS}}}" \
      "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured"
    ;;
  zero_match)
    printf "%s\n" \
      "running 0 tests" \
      "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured"
    ;;
  timed_out)
    exit 124
    ;;
  *) exit 10 ;;
esac'

  # shellcheck disable=SC2016
  write_preflight_tool_shim "$tool_dir" curl '
out=""
while [[ "$#" -gt 0 ]]; do
  case "$1" in
    -o)
      out="${2:?}"
      shift 2
      ;;
    -w)
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
[[ -n "$out" ]] || exit 11
printf "%s\n" "{\"status\":\"ok\"}" >"$out"
printf "%s" "200"'

  # shellcheck disable=SC2016
  write_preflight_tool_shim "$fake_engine/target/release" flapjack '
if [[ "${1:-}" == "--data-dir" ]]; then
  printf "%s\n" "listening on http://127.0.0.1:18765"
  while :; do sleep 1; done
fi
[[ "${1:-} ${2:-}" == "migrate preview" ]] || exit 12
[[ "${PREVIEW_SHIM_MODE:?}" != timed_out ]] || exit 124
records="${PREVIEW_EXPECTED_RECORDS:?}"
[[ "${PREVIEW_SHIM_MODE:?}" != zero_match ]] || records=0
if [[ " $* " == *" --json "* ]]; then
  printf "%s\n" \
    "{\"sourceCounts\":{\"indexes\":1,\"records\":${records}},\"report\":{\"entries\":[{\"severity\":\"HardRejection\"},{\"severity\":\"Warning\"}],\"summary\":{\"totalEntries\":2,\"hardRejections\":1,\"warnings\":1,\"scopeGaps\":0}}}"
else
  printf "%s\n" \
    "source_indexes=1" \
    "source_records=${records}" \
    "severity=HardRejection" \
    "severity=Warning"
fi
exit 9'

  cat >"$harness" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
source "${PREVIEW_RUNNER:?}"
MODE=preview_live
EXPECTED="${PREVIEW_EXPECTED_FIXTURE:?}"
TEMP_DIR="${PREVIEW_TEMP_DIR:?}"
ENGINE_DIR="${PREVIEW_FAKE_ENGINE_DIR:?}"
mkdir -p "$TEMP_DIR"
BASE_URL="http://127.0.0.1:7700"
MASTER_KEY="preview-contract-key"
cleanup_preview_harness() {
  local status=$?
  trap - EXIT
  if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" >/dev/null 2>&1; then
    kill "$SERVER_PID" >/dev/null 2>&1 || true
    wait "$SERVER_PID" >/dev/null 2>&1 || true
  fi
  exit "$status"
}
trap cleanup_preview_harness EXIT
run_preview_probe
printf "%s\n" "$PREVIEW_PROBE_RECEIPT"
EOF
  chmod +x "$harness"

  case "$scenario" in
    preview_fixture_count) shim_mode=success ;;
    preview_zero_match) shim_mode=zero_match ;;
    preview_timeout) shim_mode=timed_out ;;
    *) die "unknown preview probe scenario: $scenario" ;;
  esac

  set +e
  PATH="$tool_dir:$PATH" \
    PREVIEW_RUNNER="$RUNNER" \
    PREVIEW_EXPECTED_FIXTURE="$FIXTURE_DIR/expected_bundle.json" \
    PREVIEW_EXPECTED_RECORDS="$expected_records" \
    PREVIEW_TEMP_DIR="$case_dir/preview_tmp" \
    PREVIEW_FAKE_ENGINE_DIR="$fake_engine" \
    PREVIEW_SHIM_MODE="$shim_mode" \
    FJ_MEILISEARCH_PREVIEW_EXPECTED_RECORDS="$expected_records" \
    bash "$harness" >"$output" 2>&1
  status=$?
  set -e

  case "$scenario" in
    preview_fixture_count)
      [[ "$status" -eq 0 ]] \
        || die "ORACLE_WRONG_FAILURE: fixture count probe failed: $(tail -20 "$output")"
      grep -Fq "\"records\":${expected_records}" "$output" \
        || die "ORACLE_NOT_LOAD_BEARING: fixture count did not reach the preview probe"
      ;;
    preview_zero_match)
      [[ "$status" -ne 0 ]] \
        || die "ORACLE_NOT_LOAD_BEARING: zero-match preview probe remained green"
      grep -Fq 'JSON CLI preview report mismatch' "$output" \
        || die "ORACLE_WRONG_FAILURE: zero-match guard did not own the failure"
      ;;
    preview_timeout)
      [[ "$status" -ne 0 ]] \
        || die "ORACLE_NOT_LOAD_BEARING: timed-out preview probe remained green"
      grep -Fq 'human CLI preview exit mismatch: 124' "$output" \
        || die "ORACLE_WRONG_FAILURE: timeout guard did not own the failure"
      ;;
  esac
}

record_pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf 'PASS %s\n' "$1"
}

main() {
  assert_scenario_inventory
  [[ -x "$RUNNER" ]] || die "runner missing or not executable: $RUNNER"
  [[ -f "$FIXTURE_DIR/expected_bundle.json" ]] || die "fixture oracle missing"
  TMP_DIR="$(mktemp -d)"
  trap cleanup EXIT INT TERM

  assert_positive_control
  record_pass positive_control

  local scenario
  while IFS= read -r scenario; do
    [[ -z "$scenario" || "$scenario" == positive_control ]] && continue
    case "$scenario" in
      missing_port_probe_tool) assert_missing_port_probe_tool_rejected ;;
      port_probe_operational_failure)
        assert_safety_probe_failure_rejected "$scenario" "host-port probe failed" 1 0
        ;;
      docker_inventory_failure)
        assert_safety_probe_failure_rejected "$scenario" "Docker inventory failed" 1 2
        ;;
      partial_container_launch_cleanup) assert_partial_container_launch_cleanup ;;
      indeterminate_container_inspection) assert_indeterminate_container_inspection_rejected ;;
      preview_fixture_count|preview_zero_match|preview_timeout)
        assert_preview_probe_contract "$scenario"
        ;;
      *) assert_mutation_rejected "$scenario" ;;
    esac
    record_pass "$scenario"
  done <<<"$EXPECTED_SCENARIO_IDS"

  [[ "$TESTS_RUN" -eq 42 && "$TESTS_PASSED" -eq 42 ]] \
    || die "meta-suite denominator mismatch: ${TESTS_PASSED}/${TESTS_RUN}"
  printf 'PASS denominator=%s/%s\n' "$TESTS_PASSED" "$TESTS_RUN"
}

main "$@"
