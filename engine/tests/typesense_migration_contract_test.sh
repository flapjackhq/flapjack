#!/usr/bin/env bash
set -euo pipefail

SCRIPT_PATH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/$(basename "${BASH_SOURCE[0]}")"
ORACLE="tests/typesense_migration_contract.sh"
FIXTURE_DIR="tests/fixtures/2026_07_26_m0b_typesense_migration"
SOURCE_RANGE_EVIDENCE="docs2/4_EVIDENCE/aug10_8pm_6_source_metadata_integration/source_range.md"
if [ -n "${FJ_TYPESENSE_SELFTEST_WORK_DIR:-}" ]; then
  WORK_DIR="$FJ_TYPESENSE_SELFTEST_WORK_DIR"
  if [ -e "$WORK_DIR" ]; then
    printf 'refusing pre-existing self-test work directory: %s\n' "$WORK_DIR" >&2
    exit 1
  fi
  mkdir -p -- "$WORK_DIR"
else
  WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/fj_typesense_migration_contract_selftest.XXXXXX")"
fi
WORK_DIR_OWNER="$WORK_DIR/.typesense_migration_contract_selftest_owned"
WRITE_FREEZE_ASSERTIONS="$WORK_DIR/write_freeze_served_contract_assertions.txt"
: >"$WORK_DIR_OWNER"
TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
EXPECTED_CASES=$'rejects_wrong_record_value_and_count\nrejects_dropped_id\nrejects_changed_schema_and_default_sort\nrejects_missing_synonym_and_curation\nrejects_wrong_alias_target\nrejects_wrong_discovery_name_set\nrejects_wrong_discovery_order\nrejects_wrong_discovery_slice\nrejects_truncated_export\nrejects_source_mutation_during_capture\nrejects_credential_leakage\nrejects_cleanup_residue'

cleanup() {
  local rc="$?"
  if [ "$rc" -ne 0 ] || [ "${TESTS_FAILED:-0}" -ne 0 ]; then
    printf 'self-test failure evidence retained at %s\n' "$WORK_DIR" >&2
  elif [ ! -f "$WORK_DIR_OWNER" ]; then
    printf 'refusing cleanup without self-test ownership marker: %s\n' "$WORK_DIR" >&2
  else
    rm -rf -- "$WORK_DIR"
  fi
}
trap cleanup EXIT

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf 'ok - %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf 'not ok - %s\n%s\n' "$1" "${2:-}" >&2
}

force_failure_for_cleanup_probe() {
  [ "${FJ_TYPESENSE_SELFTEST_FORCE_FAILURE:-0}" = 1 ] || return 0
  mkdir -p "$WORK_DIR/forced_failure_evidence"
  printf '%s\n' artifact >"$WORK_DIR/forced_failure_evidence/container.log"
  fail 'forced failure for cleanup probe' "$WORK_DIR"
  exit 1
}

false_schema_sentinels_are_exact() {
  local bundle="$1"
  jq -e '
    .source.collections
    | map(select(.name == "fj_ts_migration_products")) as $products
    | [$products[0].fields[] | select(.name == "nullable_note")] as $nullable_notes
    | [$products[0].fields[] | select(.name == "secret_note")] as $secret_notes
    | ($products | length) == 1
      and ($nullable_notes | length) == 1
      and ($secret_notes | length) == 1
      and ($nullable_notes[0].index == false and $nullable_notes[0].store == true)
      and ($secret_notes[0].index == false and $secret_notes[0].store == false)
  ' "$bundle" >/dev/null
}

exported_products_match_typesense_30_2_shape() {
  local products="$1" bundle="$2"
  jq -s --slurpfile bundle "$bundle" -e '
    def expected_export_shape($omitted_fields):
      map(with_entries(select(
        .value != null
        and ((.key as $key | $omitted_fields | index($key)) | not)
      )))
      | sort_by(.id);
    . as $imports
    | ($bundle[0].source.collections
       | map(select(.name == "fj_ts_migration_products"))) as $products
    | ([$products[0].fields[] | select(.store == false) | .name] | sort) as $omitted_fields
    | ($imports | length) == 137
      and ([ $imports[] | select(has("nullable_note")) ] | length) == 137
      and ([ $imports[] | select(.nullable_note == null) ] | length) == 136
      and ([ $imports[] | select(.nullable_note != null) | {id, nullable_note} ]
           == [{id:"prod_002", nullable_note:"backorder"}])
      and ([ $imports[] | select(has("secret_note")) | {id, secret_note} ]
           == [{id:"prod_001", secret_note:"stored sentinel remains exported"}])
      and ($products | length) == 1
      and ($omitted_fields == ["secret_note"])
      and (($products[0].documents | sort_by(.id))
           == ($imports | expected_export_shape($omitted_fields)))
  ' "$products" >/dev/null
}

assert_missing_false_schema_sentinels_are_rejected() {
  local mutated_bundle="$WORK_DIR/expected_bundle_without_false_schema_sentinels.json"
  jq '
    (.source.collections[]
      | select(.name == "fj_ts_migration_products")
      | .fields) |= map(select(.name != "nullable_note" and .name != "secret_note"))
  ' "$FIXTURE_DIR/expected_bundle.json" >"$mutated_bundle"

  if ! false_schema_sentinels_are_exact "$mutated_bundle"; then
    pass 'false schema assertion rejects missing sentinels'
  else
    fail 'false schema assertion rejects missing sentinels'
  fi
}

assert_import_only_document_fields_are_rejected() {
  local imported_documents="$WORK_DIR/imported_product_documents.json"
  local mutated_bundle="$WORK_DIR/expected_bundle_with_import_only_fields.json"
  jq -s '.' "$FIXTURE_DIR/seed_products.jsonl" >"$imported_documents"
  jq --slurpfile imported "$imported_documents" '
    (.source.collections[]
      | select(.name == "fj_ts_migration_products")
      | .documents) = $imported[0]
  ' "$FIXTURE_DIR/expected_bundle.json" >"$mutated_bundle"

  if ! exported_products_match_typesense_30_2_shape \
    "$FIXTURE_DIR/seed_products.jsonl" "$mutated_bundle"; then
    pass 'export-shape assertion rejects raw import-only document fields'
  else
    fail 'export-shape assertion rejects raw import-only document fields'
  fi
}

assert_static_contract() {
  [ -f "$ORACLE" ] && pass 'runner file exists' || fail 'runner file exists'
  grep -Fq 'set -euo pipefail' "$ORACLE" && pass 'runner enables strict mode' || fail 'runner enables strict mode'
  grep -Fq 'typesense/typesense:30.2' "$ORACLE" \
    && grep -Fq 'sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110' "$ORACLE" \
    && pass 'runner pins Typesense image reference and digest' \
    || fail 'runner pins Typesense image reference and digest'
  grep -Fq '127.0.0.1::8108' "$ORACLE" && pass 'runner publishes Typesense on loopback only' || fail 'runner publishes Typesense on loopback only'
  grep -Fq 'documents/export' "$ORACLE" && grep -Fq 'jq -s' "$ORACLE" \
    && pass 'runner parses exports as JSON values' \
    || fail 'runner parses exports as JSON values'
  ! grep -Fq '/version' "$ORACLE" && pass 'runner does not call fabricated /version endpoint' || fail 'runner does not call fabricated /version endpoint'
  ! grep -Fq 'typesense.cloud' "$ORACLE" && pass 'runner does not use remote Typesense Cloud' || fail 'runner does not use remote Typesense Cloud'
  ! grep -Fq 'unsupported_findings:$e.source.unsupported_findings' "$ORACLE" \
    && ! grep -Fq 'warning_findings:$e.source.warning_findings' "$ORACLE" \
    && ! grep -Fq 'contract:$e.contract' "$ORACLE" \
    && pass 'runner derives contract, warning, and unsupported evidence independently' \
    || fail 'runner derives contract, warning, and unsupported evidence independently'
  grep -Fq 'num_dim' "$ORACLE" && grep -Fq 'vec_dist' "$ORACLE" && grep -Fq 'reference' "$ORACLE" \
    && pass 'runner preserves vector and reference schema sentinels' \
    || fail 'runner preserves vector and reference schema sentinels'
  grep -Fq 'provider_evidence' "$ORACLE" && grep -Fq 'health' "$ORACLE" && grep -Fq 'debug' "$ORACLE" \
    && pass 'runner compares health and debug provider evidence' \
    || fail 'runner compares health and debug provider evidence'
  grep -Fq 'debug_startup.json' "$ORACLE" && grep -Fq '.state == 1 and .version == "30.2"' "$ORACLE" \
    && pass 'runner waits for host-side health plus debug readiness before writes' \
    || fail 'runner waits for host-side health plus debug readiness before writes'
  false_schema_sentinels_are_exact "$FIXTURE_DIR/expected_bundle.json" \
    && pass 'expected bundle preserves explicit false schema flags' \
    || fail 'expected bundle preserves explicit false schema flags'
  exported_products_match_typesense_30_2_shape \
    "$FIXTURE_DIR/seed_products.jsonl" "$FIXTURE_DIR/expected_bundle.json" \
    && pass 'expected bundle matches Typesense 30.2 exported document shape' \
    || fail 'expected bundle matches Typesense 30.2 exported document shape'
  grep -Fq 'bool_default("index"; true)' "$ORACLE" \
    && grep -Fq 'bool_default("store"; true)' "$ORACLE" \
    && ! grep -Fq 'index:(.index // true)' "$ORACLE" \
    && ! grep -Fq 'store:(.store // true)' "$ORACLE" \
    && pass 'runner preserves explicit false flags during schema normalization' \
    || fail 'runner preserves explicit false flags during schema normalization'
  grep -Fq 'PATCH' "$ORACLE" && grep -Fq 'mutation_observation.txt' "$ORACLE" \
    && pass 'source mutation case performs a public same-count document update' \
    || fail 'source mutation case performs a public same-count document update'
  grep -Fq 'UNRELATED_SYNONYM_SET' "$ORACLE" && grep -Fq 'global_resource_visibility' "$ORACLE" \
    && pass 'runner proves unrelated global set visibility outside collection regex' \
    || fail 'runner proves unrelated global set visibility outside collection regex'
  grep -Fq 'assert_collection_listing_discovery_contract' "$ORACLE" \
    && grep -Fq "'/collections?offset=1&limit=1'" "$ORACLE" \
    && pass 'runner exercises discovery through the existing Typesense process' \
    || fail 'runner exercises discovery through the existing Typesense process'
}

assert_export_stream_live_contract_wiring() {
  local products="$FIXTURE_DIR/seed_products.jsonl"
  if jq -s -e '
      length == 137
      and (map(.id) | length == (unique | length))
      and (map(.title) | length == (unique | length))
      and all(.[]; (.id | type) == "string" and (.id | length) > 0)
    ' "$products" >/dev/null \
    && grep -Fq 'EXPECTED_PRODUCT_IDS="$ROOT_DIR/expected_product_ids.txt"' "$ORACLE" \
    && grep -Fq 'jq -r '\''.id'\'' "$FIXTURE_DIR/seed_products.jsonl" | LC_ALL=C sort >"$EXPECTED_PRODUCT_IDS"' "$ORACLE" \
    && grep -Fq 'TYPESENSE_EXPECTED_IDS_FILE="$EXPECTED_PRODUCT_IDS"' "$ORACLE" \
    && grep -Fq 'handlers::migration::typesense_client_tests::typesense_export_stream_live_contract' "$ORACLE" \
    && grep -Fq 'TYPESENSE_EXPORT_STREAM_CONTRACT documents=137 exact_ids=PASS export_requests=1 query_pagination=absent no_terminal_newline=PASS discovery_export_requests=0' "$ORACLE" \
    && grep -Fq 'cmp "$EXPECTED_PRODUCT_IDS" "$CAPTURED_PRODUCT_IDS"' "$ORACLE" \
    && grep -Fq 'export_requests=1' "$ORACLE" \
    && grep -Fq 'query_pagination=absent' "$ORACLE" \
    && grep -Fq 'discovery_export_requests=0' "$ORACLE" \
    && grep -Fq 'LIVE_EXPORT_RED_COMMAND FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=1 bash engine/tests/typesense_migration_contract.sh' "$SOURCE_RANGE_EVIDENCE" \
    && grep -Fq 'last_byte' "$ORACLE" \
    && grep -Fq '[ "$last_byte" = 125 ]' "$ORACLE"; then
    pass 'runner wires the 137-document production export-stream live contract'
  else
    fail 'runner wires the 137-document production export-stream live contract' \
      'missing 137 exact-ID fixture, production live test, one-request recorder, pagination rejection, discovery guard, supplied-input command, or no-terminal-newline assertion'
  fi
}

assert_live_export_arm_skip_is_announced() {
  local live_gate first_return_line skip_line
  live_gate="$WORK_DIR/run_production_export_stream_contract.sh"
  awk '
    /^run_production_export_stream_contract\(\) \{/ {capture=1}
    capture {print}
    capture && /^}/ {exit}
  ' "$ORACLE" >"$live_gate"
  first_return_line="$(grep -nF 'return 0' "$live_gate" | head -n 1 | cut -d: -f1)"
  skip_line="$(grep -nF 'SKIP: production export-stream live contract explicitly disabled with FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=0' "$live_gate" | head -n 1 | cut -d: -f1)"
  # Meta-tests opt out of the deliberately red Stage 1 live arm. A silent
  # `return 0` would make that explicit skip invisible in an otherwise green
  # run, so the gate must announce the skip and name its disabling input.
  if grep -Fq 'FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED' "$live_gate" \
    && [ -n "$skip_line" ] \
    && [ -n "$first_return_line" ] \
    && [ "$skip_line" -lt "$first_return_line" ] \
    && ! grep -Fq '[ "${FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED:-0}" = 1 ] || return 0' "$ORACLE"; then
    pass 'skipped live export arm announces itself instead of returning silently'
  else
    fail 'skipped live export arm announces itself instead of returning silently' \
      'the explicit opt-out returns without reporting the skip or its disabling input'
  fi
}

assert_live_export_arm_runs_by_default() {
  local live_gate="$WORK_DIR/run_production_export_stream_contract_default.sh"
  local marker="$WORK_DIR/default_live_arm_entered" out="$WORK_DIR/default_live_arm.out" rc
  awk '
    /^run_production_export_stream_contract\(\) \{/ {capture=1}
    capture {print}
    capture && /^}/ {exit}
  ' "$ORACLE" >"$live_gate"

  set +e
  (
    unset FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED
    ROOT_DIR="$WORK_DIR/default_live_probe"
    PORT=1
    SCOPED_KEY=probe
    PRODUCTS=fj_ts_migration_products
    EXPECTED_PRODUCT_IDS="$WORK_DIR/default_expected_ids.txt"
    mkdir -p "$ROOT_DIR"
    timeout() {
      : >"$marker"
      return 1
    }
    fail() {
      exit 97
    }
    source "$live_gate"
    run_production_export_stream_contract
  ) >"$out" 2>&1
  rc="$?"
  set -e

  if [ "$rc" = 97 ] \
    && [ -f "$marker" ] \
    && ! grep -Fq 'SKIP: production export-stream live contract' "$out"; then
    pass 'production export-stream live contract runs by default'
  else
    fail 'production export-stream live contract runs by default' \
      "rc=$rc entered=$([ -f "$marker" ] && printf yes || printf no) output=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_expected_case_denominator() {
  local expected observed missing extra
  expected="$WORK_DIR/expected_cases.txt"
  observed="$WORK_DIR/observed_cases.txt"
  printf '%s\n' "$EXPECTED_CASES" | sort >"$expected"
  grep -E '^[[:space:]]*assert_rejection ' "$SCRIPT_PATH" | awk '{print $2}' | sort >"$observed"
  missing="$(comm -23 "$expected" "$observed" || true)"
  extra="$(comm -13 "$expected" "$observed" || true)"
  if [ -z "$missing" ] && [ -z "$extra" ]; then
    pass 'self-test declares all expected mutation cases exactly once'
  else
    fail 'self-test declares all expected mutation cases exactly once' "missing=$missing extra=$extra"
  fi
}

assert_failure_retains_work_dir() {
  local probe_dir="$WORK_DIR/retention_probe" out rc
  out="$WORK_DIR/retention_probe.out"
  rm -rf "$probe_dir"
  set +e
  FJ_TYPESENSE_SELFTEST_FORCE_FAILURE=1 \
    FJ_TYPESENSE_SELFTEST_WORK_DIR="$probe_dir" \
    bash "$SCRIPT_PATH" >"$out" 2>&1
  rc="$?"
  set -e
  if [ "$rc" != 0 ] && [ -f "$probe_dir/forced_failure_evidence/container.log" ]; then
    pass 'self-test retains per-case evidence on failure'
  else
    fail 'self-test retains per-case evidence on failure' "rc=$rc output=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_preexisting_work_dir_is_rejected() {
  local probe_dir="$WORK_DIR/preexisting_work_dir_probe" out rc
  out="$WORK_DIR/preexisting_work_dir_probe.out"
  mkdir -p "$probe_dir"
  printf '%s\n' preserve >"$probe_dir/sentinel"
  set +e
  FJ_TYPESENSE_SELFTEST_FORCE_FAILURE=1 \
    FJ_TYPESENSE_SELFTEST_WORK_DIR="$probe_dir" \
    bash "$SCRIPT_PATH" >"$out" 2>&1
  rc="$?"
  set -e
  if [ "$rc" != 0 ] \
    && [ "$(cat "$probe_dir/sentinel" 2>/dev/null || true)" = preserve ] \
    && grep -Fq 'refusing pre-existing self-test work directory' "$out"; then
    pass 'self-test refuses an unowned pre-existing cleanup path'
  else
    fail 'self-test refuses an unowned pre-existing cleanup path' "rc=$rc output=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_unsafe_run_id_is_rejected() {
  local out="$WORK_DIR/unsafe_run_id.out" rc
  set +e
  FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=0 \
    FJ_TYPESENSE_RUN_ID='../unsafe/path' bash "$ORACLE" >"$out" 2>&1
  rc="$?"
  set -e
  if [ "$rc" != 0 ] && grep -Fq 'run id must contain only' "$out"; then
    pass 'runner rejects path traversal in run id'
  else
    fail 'runner rejects path traversal in run id' "rc=$rc output=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_preexisting_evidence_dir_is_rejected() {
  local evidence="$WORK_DIR/preexisting_evidence_probe" out="$WORK_DIR/preexisting_evidence_probe.out" rc
  mkdir -p "$evidence"
  printf '%s\n' preserve >"$evidence/sentinel"
  set +e
  FJ_TYPESENSE_WRITE_FREEZE_ATTESTED=1 \
    FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=0 \
    FJ_TYPESENSE_RUN_ID="preexisting_evidence_$$" \
    FJ_TYPESENSE_EVIDENCE_DIR="$evidence" \
    bash "$ORACLE" >"$out" 2>&1
  rc="$?"
  set -e
  if [ "$rc" != 0 ] \
    && [ "$(cat "$evidence/sentinel" 2>/dev/null || true)" = preserve ] \
    && grep -Fq 'refusing pre-existing evidence directory' "$out"; then
    pass 'runner refuses an unowned pre-existing evidence path'
  else
    fail 'runner refuses an unowned pre-existing evidence path' "rc=$rc output=$(cat "$out" 2>/dev/null || true)"
  fi
}

run_oracle() {
  local mutation="$1" out="$2" evidence="$3"
  set +e
  FJ_TYPESENSE_WRITE_FREEZE_ATTESTED=1 \
    FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=0 \
    FJ_TYPESENSE_RUN_SERVED_WRITE_FREEZE_CONTRACT=0 \
    FJ_TYPESENSE_CONTRACT_MUTATION="$mutation" \
    FJ_TYPESENSE_RUN_ID="selftest_${mutation}_$$" \
    FJ_TYPESENSE_EVIDENCE_DIR="$evidence" \
    bash "$ORACLE" >"$out" 2>&1
  local rc="$?"
  set -e
  printf '%s' "$rc"
}

assert_rejection() {
  local case_name="$1" mutation="$2" expected_text="$3" out evidence rc
  out="$WORK_DIR/${case_name}.out"
  evidence="$WORK_DIR/${case_name}_evidence"
  rc="$(run_oracle "$mutation" "$out" "$evidence")"
  if [ "$rc" != 0 ] && grep -Fq "$expected_text" "$out"; then
    pass "$case_name"
    assert_no_secret_artifacts "$case_name" "$out" "$evidence"
    if [ "$case_name" = rejects_source_mutation_during_capture ]; then
      assert_mutation_observation "$evidence/mutation_observation.txt"
      assert_source_fixture_teardown "$case_name" "$mutation" "$out" "$evidence"
    elif [ "$case_name" = rejects_truncated_export ]; then
      assert_truncated_export_evidence "$evidence/export_fj_ts_migration_categories.jsonl"
    elif [ "$case_name" = rejects_cleanup_residue ]; then
      assert_cleanup_residue_evidence "$evidence/cleanup_residue_marker.txt"
      assert_source_fixture_teardown "$case_name" "$mutation" "$out" "$evidence"
    elif [[ "$case_name" == rejects_wrong_discovery_* ]]; then
      assert_discovery_failure_evidence "$evidence"
    fi
  else
    fail "$case_name" "rc=$rc expected=$expected_text output=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_discovery_failure_evidence() {
  local evidence="$1" file
  for file in discovery_collections.json discovery_limit_one.json \
    discovery_offset_one_limit_one.json discovery_offset_one.json \
    discovery_offset_two.json; do
    if [ ! -s "$evidence/$file" ] || ! jq -e . "$evidence/$file" >/dev/null 2>&1; then
      fail 'discovery mutation preserves JSON failure evidence' "missing or invalid: $evidence/$file"
      return
    fi
  done
  pass 'discovery mutation preserves JSON failure evidence'
}

assert_truncated_export_evidence() {
  local export_stream="$1" last_byte
  last_byte="$(tail -c 1 "$export_stream" 2>/dev/null | od -An -t u1 | tr -d ' ')"
  if [ -s "$export_stream" ] && [ "$last_byte" != 125 ] && ! jq -s -e 'all(.[]; type == "object")' "$export_stream" >/dev/null 2>&1; then
    pass 'truncated export case preserves the malformed raw API stream'
  else
    fail 'truncated export case preserves the malformed raw API stream' "last_byte=$last_byte"
  fi
}

assert_cleanup_residue_evidence() {
  local residue_marker="$1"
  if [ "$(cat "$residue_marker" 2>/dev/null || true)" = residue ]; then
    pass 'cleanup residue case preserves the exact residue marker'
  else
    fail 'cleanup residue case preserves the exact residue marker' "missing or changed: $residue_marker"
  fi
}

# This KAT's Docker container is the seeded Typesense source fixture, not a
# migrated Flapjack target. The oracle names it from the run id run_oracle
# supplies, so the exact name is derivable here without reaching into the
# oracle's private temp root. Three halves, each load-bearing:
#   - container_residue.txt must name the exact container, proving the source
#     fixture was live when the guard fired. Without it the case could go green
#     on a run that died before it captured the source.
#   - a successful Docker inventory must show that source fixture removed. A
#     failed inventory is indeterminate and must fail closed, never masquerade
#     as an empty survivor list.
#   - the transcript must carry no PASS verdict, so a rejected capture cannot
#     also report success.
# Goes red if the oracle stops force-removing the container on the failure path,
# stops snapshotting the source fixture before teardown, Docker inspection
# fails, or the oracle ever prints the PASS verdict alongside a rejection. This
# assertion deliberately makes no migrated-target or erased-job claim: those
# require provider-aware lifecycle owners outside this source-contract KAT.
assert_source_fixture_teardown() {
  local case_name="$1" mutation="$2" out="$3" evidence="$4"
  local container="fj_typesense_migration_contract_selftest_${mutation}_$$"
  local live_at_guard survivors docker_status
  live_at_guard="$(cat "$evidence/container_residue.txt" 2>/dev/null || true)"
  set +e
  survivors="$(docker ps -a --filter "name=^/${container}$" --format '{{.Names}}' 2>"$evidence/docker_inventory.err")"
  docker_status="$?"
  set -e
  if [ "$docker_status" -ne 0 ]; then
    fail "$case_name source fixture teardown is observable" \
      "docker ps exited $docker_status: $(cat "$evidence/docker_inventory.err" 2>/dev/null || true)"
    return
  fi
  if [ "$live_at_guard" = "$container" ] \
    && [ -z "$survivors" ] \
    && ! grep -Fq 'PASS: Typesense migration source contract KAT verified' "$out"; then
    pass "$case_name removes the source fixture and declares no pass"
  else
    fail "$case_name removes the source fixture and declares no pass" \
      "live_at_guard=$live_at_guard survivors=$survivors"
  fi
}

assert_positive_control() {
  local out="$WORK_DIR/positive.out" evidence="$WORK_DIR/positive_evidence" rc
  rc="$(run_oracle "" "$out" "$evidence")"
  if [ "$rc" = 0 ] && grep -Fq 'PASS: Typesense migration source contract KAT verified' "$out"; then
    pass 'positive control accepts unmutated public-API capture'
  else
    fail 'positive control accepts unmutated public-API capture' "rc=$rc output=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_secret_absence() {
  local fixture_hits
  fixture_hits="$(grep -R -E 'TYPESENSE_STAGE2_BOOTSTRAP_CANARY|TYPESENSE_STAGE2_SCOPED_CANARY' tests/fixtures/2026_07_26_m0b_typesense_migration 2>/dev/null || true)"
  [ -z "$fixture_hits" ] && pass 'committed fixtures do not contain credential sentinels' \
    || fail 'committed fixtures do not contain credential sentinels' "$fixture_hits"
}

assert_no_secret_artifacts() {
  local case_name="$1" out="$2" evidence="$3" hits value_hits
  hits="$(grep -R -E 'TYPESENSE_STAGE2_BOOTSTRAP_CANARY|TYPESENSE_STAGE2_SCOPED_CANARY|TYPESENSE_STAGE2_EXPORT_CANARY' "$out" "$evidence" 2>/dev/null || true)"
  value_hits="$(jq -r 'select(type == "object" and has("value")) | .value' "$evidence"/capture_key_response.json "$evidence"/export_key_response.json 2>/dev/null \
    | grep -Ev '^\[REDACTED_(SCOPED|EXPORT)_KEY\]$' || true)"
  if [ -z "$hits" ] && [ -z "$value_hits" ] && grep -Fq 'PASS: no generated key values found in preserved evidence' "$evidence/evidence_secret_scan.txt" 2>/dev/null; then
    pass "$case_name preserves sanitized failure evidence"
  else
    fail "$case_name preserves sanitized failure evidence" "sentinel_hits=$hits value_hits=$value_hits"
  fi
}

assert_mutation_observation() {
  local observation="$1"
  if grep -Fq 'mutation_http_code=200' "$observation" 2>/dev/null \
    && grep -Fq 'count_before=137' "$observation" \
    && grep -Fq 'count_after=137' "$observation"; then
    pass 'source mutation case records real same-count public update'
  else
    fail 'source mutation case records real same-count public update' "$(cat "$observation" 2>/dev/null || true)"
  fi
}

write_freeze_served_contract_assertion_manifest() {
  cat <<'EOF'
supported_http_status::[ "$code" = 400 ] || fail "$endpoint $attestation attestation returned $code"
supported_refusal_message::.message | contains("external write freeze/attestation")
supported_zero_source_io::[ "$(source_request_count)" = "$before" ] || fail "$endpoint $attestation reached Typesense"
preview_document_count::.sourceCounts == {indexes:1,records:137}
submit_document_count::.objectsImported.imported == 137
true_source_reachability::[ "$after" -gt "$before" ] || fail "$endpoint true attestation did not reach Typesense"
resume_http_status::[ "$code" = 400 ] && jq -e
resume_error_code::.code == "source_provider_unsupported"
resume_zero_source_io::[ "$(source_request_count)" = "$before" ] || fail "Typesense resume $attestation reached the source"
served_denominator::[ "$MISSING_REFUSED $FALSE_REFUSED $ZERO_SOURCE_REQUESTS $TRUE_PASSED $RESUME_UNSUPPORTED" = '2 2 4 2 3' ]
EOF
}

write_freeze_served_contract_shape_is_complete() {
  local candidate="$1"
  python3 - "$candidate" "$WRITE_FREEZE_ASSERTIONS" <<'PY'
import pathlib
import sys

driver = pathlib.Path(sys.argv[1]).read_text()
assertions = tuple(
    line.split("::", 1)[1]
    for line in pathlib.Path(sys.argv[2]).read_text().splitlines()
)
required = (
    "readonly WRITE_FREEZE_SUPPORTED_ENDPOINTS='preview submit'",
    "readonly WRITE_FREEZE_ATTESTATION_ARMS='missing false true'",
    "readonly WRITE_FREEZE_RESUME_ARMS='missing false true'",
    'for endpoint in $WRITE_FREEZE_SUPPORTED_ENDPOINTS; do',
    'for attestation in $WRITE_FREEZE_ATTESTATION_ARMS; do',
    'probe_supported_write_freeze_arm "$endpoint" "$attestation"',
    'for attestation in $WRITE_FREEZE_RESUME_ARMS; do',
    'probe_resume_write_freeze_arm "$attestation"',
    'TYPESENSE_WRITE_FREEZE_CONTRACT endpoints=preview,submit missing_refused=2 false_refused=2 zero_source_requests=4 true_passed=2 resume_unsupported=3 resume_source_requests=0 documents=137',
 ) + assertions
raise SystemExit(0 if all(needle in driver for needle in required) else 1)
PY
}

write_freeze_served_contract_without_assertion() {
  local source="$1" assertion="$2" destination="$3"
  python3 - "$source" "$assertion" "$destination" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1])
assertion = sys.argv[2]
destination = pathlib.Path(sys.argv[3])
driver = source.read_text()
if driver.count(assertion) != 1:
    raise SystemExit(f"expected exactly one served-contract assertion: {assertion}")
destination.write_text(driver.replace(assertion, "", 1))
PY
}

assert_write_freeze_served_contract_assertion_mutation() {
  local case_name="$1" assertion="$2"
  local mutated="$WORK_DIR/write_freeze_${case_name}.sh"
  write_freeze_served_contract_without_assertion "$ORACLE" "$assertion" "$mutated"
  if write_freeze_served_contract_shape_is_complete "$mutated"; then
    fail "write-freeze served-contract mutation removes $case_name assertion"
    return 1
  fi
  pass "write-freeze served-contract mutation removes $case_name assertion"
}

assert_write_freeze_served_contract_meta() {
  local missing_endpoint="$WORK_DIR/write_freeze_missing_endpoint.sh"
  local missing_resume_arm="$WORK_DIR/write_freeze_missing_resume_arm.sh"
  local mutation case_name assertion

  write_freeze_served_contract_assertion_manifest >"$WRITE_FREEZE_ASSERTIONS"

  if ! write_freeze_served_contract_shape_is_complete "$ORACLE"; then
    printf 'TYPESENSE_WRITE_FREEZE_META=RED missing_supported_endpoint_or_resume_refusal_arm\n' >&2
    exit 1
  fi

  sed "s/readonly WRITE_FREEZE_SUPPORTED_ENDPOINTS='preview submit'/readonly WRITE_FREEZE_SUPPORTED_ENDPOINTS='preview'/" \
    "$ORACLE" >"$missing_endpoint"
  sed "s/readonly WRITE_FREEZE_RESUME_ARMS='missing false true'/readonly WRITE_FREEZE_RESUME_ARMS='missing true'/" \
    "$ORACLE" >"$missing_resume_arm"
  if write_freeze_served_contract_shape_is_complete "$missing_endpoint" \
    || write_freeze_served_contract_shape_is_complete "$missing_resume_arm"; then
    fail 'write-freeze served-contract mutation removes a supported or resume arm'
  else
    pass 'write-freeze served-contract mutation removes a supported or resume arm'
  fi
  while IFS= read -r mutation; do
    case_name="${mutation%%::*}"
    assertion="${mutation#*::}"
    assert_write_freeze_served_contract_assertion_mutation "$case_name" "$assertion"
  done <"$WRITE_FREEZE_ASSERTIONS"
}

main() {
  cd "$(git rev-parse --show-toplevel)/engine"
  mkdir -p "$WORK_DIR"
  force_failure_for_cleanup_probe
  echo 'typesense_migration_contract meta-test'
  assert_write_freeze_served_contract_meta
  assert_static_contract
  assert_export_stream_live_contract_wiring
  assert_live_export_arm_skip_is_announced
  assert_live_export_arm_runs_by_default
  assert_missing_false_schema_sentinels_are_rejected
  assert_import_only_document_fields_are_rejected
  assert_expected_case_denominator
  assert_failure_retains_work_dir
  assert_preexisting_work_dir_is_rejected
  assert_unsafe_run_id_is_rejected
  assert_preexisting_evidence_dir_is_rejected
  assert_positive_control
  assert_rejection rejects_wrong_record_value_and_count wrong_record_value_and_count 'record value/count mismatch rejected'
  assert_rejection rejects_dropped_id dropped_id 'dropped id rejected'
  assert_rejection rejects_changed_schema_and_default_sort changed_schema_and_default_sort 'schema/default sort mismatch rejected'
  assert_rejection rejects_missing_synonym_and_curation missing_synonym_and_curation 'synonym/curation mismatch rejected'
  assert_rejection rejects_wrong_alias_target wrong_alias_target 'alias target mismatch rejected'
  assert_rejection rejects_wrong_discovery_name_set wrong_discovery_name_set 'discovery name set mismatch rejected'
  assert_rejection rejects_wrong_discovery_order wrong_discovery_order 'discovery newest-first order mismatch rejected'
  assert_rejection rejects_wrong_discovery_slice wrong_discovery_slice 'discovery offset/limit slice mismatch rejected'
  assert_rejection rejects_truncated_export truncated_export 'export for fj_ts_migration_categories did not end with a JSON object'
  assert_rejection rejects_source_mutation_during_capture source_mutation_during_capture 'source mutation rejected: explicit write-freeze attestation was violated'
  assert_rejection rejects_credential_leakage credential_leakage 'credential leakage rejected'
  assert_rejection rejects_cleanup_residue cleanup_residue 'cleanup residue rejected'
  assert_secret_absence
  printf '\nResults: %d/%d passed\n' "$TESTS_PASSED" "$TESTS_RUN"
  [ "$TESTS_FAILED" -eq 0 ]
}

main "$@"
