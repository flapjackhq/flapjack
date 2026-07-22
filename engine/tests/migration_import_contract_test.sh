#!/usr/bin/env bash
# shellcheck disable=SC2015,SC2016

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ORACLE="$SCRIPT_DIR/migration_import_contract.sh"
NIGHTLY_WORKFLOW="$SCRIPT_DIR/../../.github/workflows/nightly.yml"
SCALE_EVIDENCE_HEAD="bbfd59bf64dae52626ee584e39bb7bff0b580494"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
TESTS_SKIPPED=0
TEST_RESULTS=""

# Scenario IDs are the meta-suite denominator. Add every new scenario here in
# the same change that introduces it so missing or renamed cases cannot pass.
EXPECTED_SCENARIO_IDS='
oracle_file_exists
oracle_executable
oracle_strict_mode
oracle_algolia_secret_scope
oracle_list_indices_surface
nightly_import_job_count
nightly_import_invocation_count
nightly_import_public_mirror_gate
nightly_import_release_binary_build
nightly_import_release_binary_path
nightly_import_no_unavailable_mode
nightly_import_secret_source
nightly_import_seed_count
nightly_import_source_count_gate
nightly_import_receipt_count_gate
nightly_import_exact_cleanup
nightly_import_evidence_upload
nightly_import_seeded_object_count_two
nightly_scale_job_count
nightly_scale_dispatch_gate
nightly_scale_two_point_command
nightly_scale_timeout_budget
nightly_scale_evidence_root
nightly_scale_receipt_discovery
nightly_scale_receipt_validation
nightly_scale_secret_source
nightly_scale_downloads_build_server_artifact
nightly_scale_secret_file_single_write
nightly_scale_uploads_evidence
nightly_scale_dispatch_only_comment
nightly_scale_rejects_missing_scoped_secrets
nightly_scale_rejects_missing_scoped_artifact
generator_internal_consistency
generator_rejects_non_positive_size
generator_rejects_uncovered_probe_size
fixture_source_count_live_query
fixture_reuse_unowned_source
fixture_prefix_preflight_cleanup
fixture_prepare_waits_tasks
fixture_selftest_failure_cleans_source
fixture_selftest_rejects_reuse
unavailable_positive_control
importing_positive_control
async_positive_control
async_preflight_sweep_scope
async_delete_http_failure
async_delete_malformed_success
async_cleanup_stale_dsn_listing
async_terminal_success_without_target
async_phase_regression
cancel_positive_control
cancel_preflight_sweep_scope
cancel_zero_source_rejected
cancel_cleanup_residue
cancel_spool_residue
cancel_sentinel_field_order
cancel_postcommit_pagination
args_scenario_async_job_requires_secret_file
args_unknown_scenario_rejected
args_async_index_conflict_rejected
args_importing_accepts_scenario_cancel
args_cancel_index_conflict_rejected
importing_success_evidence
verified_importing_success
scale_single_size_success
scale_two_point_success
scale_init_failure_cleanup
scale_cleanup_failure_receipt
scale_bad_total_count
scale_duplicate_object_ids
scale_missing_final_page
scale_short_facet_cardinality
scale_request_timeout
scale_wall_clock_over_budget
scale_sidecar_incomplete
scale_large_sidecar_undersampled
scale_sampler_hot_path_cheap
scale_manifest_generation_drift
scale_manifest_count_drift
scale_manifest_length_drift
scale_manifest_deleted_snapshot
scale_multiple_jobs
scale_growth_breach
scale_trial_count_floor
scale_request_budget_ceiling
scale_accepted_trial_count
scale_even_trial_medians
scale_repository_receipt
inventory_rejects_missing_stage3_id
verified_importing_bad_counts
verified_importing_bad_known_answers
verified_importing_bad_settings
verified_importing_bad_synonym
verified_importing_bad_promotion
verified_importing_bad_hiding
verified_importing_conflict_mutates
verified_importing_invalid_creates
verified_importing_cleanup_residue
unavailable_returns_2xx
unavailable_wrong_code
unavailable_lists_target
importing_returns_503
importing_omits_target
importing_empty_target
importing_duplicates_target
importing_wrong_count
malformed_migration_json
malformed_indexes_json
args_missing_expect_mode
args_unknown_expect_mode
args_unavailable_refuses_secret_file
args_unavailable_refuses_source_index
args_unavailable_refuses_target_index
args_unavailable_refuses_corpus_size
args_importing_requires_secret_file
args_importing_requires_source_index
args_importing_requires_target_index
args_importing_refuses_corpus_size
args_verification_manifest_importing_only
args_verification_manifest_absolute
args_importing_secret_file_absolute
args_missing_importing_secret_sanitized
args_scale_requires_secret_file
args_scale_secret_file_absolute
args_scale_accepts_explicit_corpus_size
args_scale_two_point_rejects_explicit_corpus_size
args_scale_refuses_source_index
args_scale_refuses_target_index
args_scale_refuses_verification_manifest
args_scale_rejects_too_small_corpus
signal_int_evidence
signal_term_evidence
cleanup_failure_evidence
testing_docs_scale_proof_contract
debbie_public_sync_surface
args_importing_accepts_scenario_replicas
args_unavailable_refuses_scenario
args_unknown_scenario_fails
importing_replicas_success
importing_replicas_missing_warnings
importing_replicas_empty_warnings
importing_replicas_sidecar_warning
importing_replicas_missing_sidecar
importing_replicas_wrong_sidecar_ranking
importing_replicas_physical_replica_data
importing_replicas_physical_replica_corpus
importing_replicas_primary_order_leak
importing_replicas_standard_ranking_unnormalized
importing_replicas_standard_desc_dropped
importing_replicas_no_checks
importing_replicas_skipped_check
importing_replicas_bad_check_name
importing_replicas_source_path_traversal
importing_replicas_target_path_traversal
'

scenario_id_for_label() {
  case "$1" in
    'oracle file exists') printf '%s\n' 'oracle_file_exists' ;;
    'oracle is executable') printf '%s\n' 'oracle_executable' ;;
    'oracle enables strict mode') printf '%s\n' 'oracle_strict_mode' ;;
    'oracle loads only required Algolia secrets in importing mode') printf '%s\n' 'oracle_algolia_secret_scope' ;;
    'oracle uses list-indices metadata surface, not single-index search route') printf '%s\n' 'oracle_list_indices_surface' ;;
    'nightly has exactly one migration import contract job') printf '%s\n' 'nightly_import_job_count' ;;
    'nightly invokes importing oracle exactly once') printf '%s\n' 'nightly_import_invocation_count' ;;
    'nightly importing oracle is public-mirror gated') printf '%s\n' 'nightly_import_public_mirror_gate' ;;
    'nightly builds release server binary for importing oracle') printf '%s\n' 'nightly_import_release_binary_build' ;;
    'nightly sets FLAPJACK_BIN to release binary') printf '%s\n' 'nightly_import_release_binary_path' ;;
    'nightly importing oracle never uses unavailable mode') printf '%s\n' 'nightly_import_no_unavailable_mode' ;;
    'nightly passes Algolia credentials from repo secrets') printf '%s\n' 'nightly_import_secret_source' ;;
    'nightly seeds a non-empty source fixture') printf '%s\n' 'nightly_import_seed_count' ;;
    'nightly verifies source fixture count before oracle') printf '%s\n' 'nightly_import_source_count_gate' ;;
    'nightly asserts oracle reported seeded object count') printf '%s\n' 'nightly_import_receipt_count_gate' ;;
    'nightly cleanup deletes exactly created source index') printf '%s\n' 'nightly_import_exact_cleanup' ;;
    'nightly uploads preserved oracle evidence') printf '%s\n' 'nightly_import_evidence_upload' ;;
    'nightly scheduled import pins SEEDED_OBJECT_COUNT=2') printf '%s\n' 'nightly_import_seeded_object_count_two' ;;
    'nightly has exactly one migration scale contract job') printf '%s\n' 'nightly_scale_job_count' ;;
    'nightly scale contract is public-mirror and dispatch-input gated') printf '%s\n' 'nightly_scale_dispatch_gate' ;;
    'nightly scale contract invokes the two-point scale oracle') printf '%s\n' 'nightly_scale_two_point_command' ;;
    'nightly scale contract is bounded to 5,400 seconds') printf '%s\n' 'nightly_scale_timeout_budget' ;;
    'nightly scale contract preserves evidence under RUNNER_TEMP') printf '%s\n' 'nightly_scale_evidence_root' ;;
    'nightly scale contract resolves exactly one receipt') printf '%s\n' 'nightly_scale_receipt_discovery' ;;
    'nightly scale contract validates receipt contents') printf '%s\n' 'nightly_scale_receipt_validation' ;;
    'nightly scale contract sources Algolia credentials only from repo secrets') printf '%s\n' 'nightly_scale_secret_source' ;;
    'nightly scale contract reuses build-server artifact') printf '%s\n' 'nightly_scale_downloads_build_server_artifact' ;;
    'nightly scale contract writes the temporary secret file exactly once') printf '%s\n' 'nightly_scale_secret_file_single_write' ;;
    'nightly scale contract uploads receipt and spool evidence') printf '%s\n' 'nightly_scale_uploads_evidence' ;;
    'nightly scale contract documents dispatch-only isolation') printf '%s\n' 'nightly_scale_dispatch_only_comment' ;;
    'nightly scale contract rejects missing scoped credential environment') printf '%s\n' 'nightly_scale_rejects_missing_scoped_secrets' ;;
    'nightly scale contract rejects missing scoped server artifact download') printf '%s\n' 'nightly_scale_rejects_missing_scoped_artifact' ;;
    'generator keeps documents, setup, and live scale probes internally consistent') printf '%s\n' 'generator_internal_consistency' ;;
    'generator rejects non-positive explicit size') printf '%s\n' 'generator_rejects_non_positive_size' ;;
    'generator rejects corpus sizes that cannot satisfy scale probe references') printf '%s\n' 'generator_rejects_uncovered_probe_size' ;;
    'fixture source-count emits only the live empty-query nbHits value') printf '%s\n' 'fixture_source_count_live_query' ;;
    'fixture reuse records unowned source and refuses cleanup deletion') printf '%s\n' 'fixture_reuse_unowned_source' ;;
    'fixture preflight deletes stale fj_scale indices before fresh seeding') printf '%s\n' 'fixture_prefix_preflight_cleanup' ;;
    'fixture waits for settings, synonyms, rules, and document batch tasks before prepare returns') printf '%s\n' 'fixture_prepare_waits_tasks' ;;
    'fixture selftest cleans its prepared source when count verification fails') printf '%s\n' 'fixture_selftest_failure_cleans_source' ;;
    'fixture selftest rejects reuse fixture before seeding') printf '%s\n' 'fixture_selftest_rejects_reuse' ;;
    'unavailable positive control passes') printf '%s\n' 'unavailable_positive_control' ;;
    'importing positive control passes') printf '%s\n' 'importing_positive_control' ;;
    'async job control submits, walks monotonic phases, and verifies the target') printf '%s\n' 'async_positive_control' ;;
    'async preflight sweeps owned and stale names but skips unowned recent leftovers') printf '%s\n' 'async_preflight_sweep_scope' ;;
    'async preflight rejects a failed exact-target deletion') printf '%s\n' 'async_delete_http_failure' ;;
    'async preflight rejects a malformed deletion success') printf '%s\n' 'async_delete_malformed_success' ;;
    'async cleanup deletes owned source when DSN listing is stale') printf '%s\n' 'async_cleanup_stale_dsn_listing' ;;
    'async terminal success without a present target fails closed') printf '%s\n' 'async_terminal_success_without_target' ;;
    'async backward phase movement fails closed') printf '%s\n' 'async_phase_regression' ;;
    'cancel scenario proves pre-commit cancel and post-commit cancel_too_late') printf '%s\n' 'cancel_positive_control' ;;
    'cancel preflight sweeps owned and stale names but skips other prefixes') printf '%s\n' 'cancel_preflight_sweep_scope' ;;
    'cancel scenario rejects a zero-document source') printf '%s\n' 'cancel_zero_source_rejected' ;;
    'cancel scenario rejects cleanup residue') printf '%s\n' 'cancel_cleanup_residue' ;;
    'cancel scenario rejects spool residue') printf '%s\n' 'cancel_spool_residue' ;;
    'cancel scenario accepts unchanged sentinel with reordered JSON fields') printf '%s\n' 'cancel_sentinel_field_order' ;;
    'cancel postcommit verification paginates target reads') printf '%s\n' 'cancel_postcommit_pagination' ;;
    'async job scenario requires secret-file') printf '%s\n' 'args_scenario_async_job_requires_secret_file' ;;
    'async unknown scenario fails') printf '%s\n' 'args_unknown_scenario_rejected' ;;
    'async index flag and environment conflict fails') printf '%s\n' 'args_async_index_conflict_rejected' ;;
    'scenario cancel is accepted for importing mode') printf '%s\n' 'args_importing_accepts_scenario_cancel' ;;
    'cancel index flag and environment conflict fails') printf '%s\n' 'args_cancel_index_conflict_rejected' ;;
    'opt-in success evidence preserves receipt metadata and counts') printf '%s\n' 'importing_success_evidence' ;;
    'verified importing proves content, behavior, negative arms, and target cleanup') printf '%s\n' 'verified_importing_success' ;;
    'scale mode prepares fixture, assigns manifest before gate, records ledger, and cleans up') printf '%s\n' 'scale_single_size_success' ;;
    'scale two-point mode records three complete trials at 2,000 and 20,000') printf '%s\n' 'scale_two_point_success' ;;
    'scale init failure cleans the prepared Algolia fixture') printf '%s\n' 'scale_init_failure_cleanup' ;;
    'scale cleanup failure downgrades the persisted receipt') printf '%s\n' 'scale_cleanup_failure_receipt' ;;
    'scale rejects target total drift') printf '%s\n' 'scale_bad_total_count' ;;
    'scale rejects duplicate objectIDs') printf '%s\n' 'scale_duplicate_object_ids' ;;
    'scale rejects a missing final-page object') printf '%s\n' 'scale_missing_final_page' ;;
    'scale rejects facet cardinality drift') printf '%s\n' 'scale_short_facet_cardinality' ;;
    'scale two-point fails timed-out migration requests') printf '%s\n' 'scale_request_timeout' ;;
    'scale two-point rejects a trial over the wall-clock budget') printf '%s\n' 'scale_wall_clock_over_budget' ;;
    'scale two-point fails incomplete sidecar samples') printf '%s\n' 'scale_sidecar_incomplete' ;;
    'scale two-point rejects large-only sidecar undersampling') printf '%s\n' 'scale_large_sidecar_undersampled' ;;
    'scale sampler hot path avoids manifest validation and size subprocesses') printf '%s\n' 'scale_sampler_hot_path_cheap' ;;
    'scale two-point fails manifest generation drift') printf '%s\n' 'scale_manifest_generation_drift' ;;
    'scale two-point fails manifest count drift') printf '%s\n' 'scale_manifest_count_drift' ;;
    'scale two-point fails manifest length drift') printf '%s\n' 'scale_manifest_length_drift' ;;
    'scale two-point preserves completed manifest after deleted snapshot') printf '%s\n' 'scale_manifest_deleted_snapshot' ;;
    'scale two-point fails multiple live job directories') printf '%s\n' 'scale_multiple_jobs' ;;
    'scale two-point fails rewrite growth ceiling breach') printf '%s\n' 'scale_growth_breach' ;;
    'scale two-point rejects a trial count below three') printf '%s\n' 'scale_trial_count_floor' ;;
    'scale two-point rejects inherited request budgets above the pinned ceiling') printf '%s\n' 'scale_request_budget_ceiling' ;;
    'scale two-point receipt reports the accepted trial count') printf '%s\n' 'scale_accepted_trial_count' ;;
    'scale two-point computes mathematical medians for four accepted trials') printf '%s\n' 'scale_even_trial_medians' ;;
    'repository selected scale receipt is clean-head and archive-resolving') printf '%s\n' 'scale_repository_receipt' ;;
    'scenario inventory rejects a missing Stage 3 scenario ID') printf '%s\n' 'inventory_rejects_missing_stage3_id' ;;
    'verified importing rejects response count drift') printf '%s\n' 'verified_importing_bad_counts' ;;
    'verified importing rejects known-answer field drift') printf '%s\n' 'verified_importing_bad_known_answers' ;;
    'verified importing rejects ineffective settings') printf '%s\n' 'verified_importing_bad_settings' ;;
    'verified importing rejects ineffective synonym') printf '%s\n' 'verified_importing_bad_synonym' ;;
    'verified importing rejects ineffective promotion') printf '%s\n' 'verified_importing_bad_promotion' ;;
    'verified importing rejects ineffective hiding') printf '%s\n' 'verified_importing_bad_hiding' ;;
    'verified importing rejects conflict mutation') printf '%s\n' 'verified_importing_conflict_mutates' ;;
    'verified importing rejects invalid-key target creation') printf '%s\n' 'verified_importing_invalid_creates' ;;
    'verified importing rejects cleanup residue') printf '%s\n' 'verified_importing_cleanup_residue' ;;
    'unavailable returning 2xx fails closed') printf '%s\n' 'unavailable_returns_2xx' ;;
    'unavailable wrong 503 code fails closed') printf '%s\n' 'unavailable_wrong_code' ;;
    'unavailable listed target fails closed') printf '%s\n' 'unavailable_lists_target' ;;
    'importing returning 503 fails closed') printf '%s\n' 'importing_returns_503' ;;
    'importing omitted target fails closed') printf '%s\n' 'importing_omits_target' ;;
    'importing empty target fails closed') printf '%s\n' 'importing_empty_target' ;;
    'importing duplicated target fails closed') printf '%s\n' 'importing_duplicates_target' ;;
    'importing count mismatch fails closed') printf '%s\n' 'importing_wrong_count' ;;
    'malformed migration response fails closed') printf '%s\n' 'malformed_migration_json' ;;
    'malformed list-indices response fails closed') printf '%s\n' 'malformed_indexes_json' ;;
    'missing expect-mode fails') printf '%s\n' 'args_missing_expect_mode' ;;
    'unknown expect-mode fails') printf '%s\n' 'args_unknown_expect_mode' ;;
    'unavailable refuses secret-file') printf '%s\n' 'args_unavailable_refuses_secret_file' ;;
    'unavailable refuses source-index') printf '%s\n' 'args_unavailable_refuses_source_index' ;;
    'unavailable refuses target-index') printf '%s\n' 'args_unavailable_refuses_target_index' ;;
    'unavailable refuses explicit default corpus-size') printf '%s\n' 'args_unavailable_refuses_corpus_size' ;;
    'importing requires secret-file') printf '%s\n' 'args_importing_requires_secret_file' ;;
    'importing requires source-index') printf '%s\n' 'args_importing_requires_source_index' ;;
    'importing requires target-index') printf '%s\n' 'args_importing_requires_target_index' ;;
    'importing refuses explicit default corpus-size') printf '%s\n' 'args_importing_refuses_corpus_size' ;;
    'verification manifest is importing-only') printf '%s\n' 'args_verification_manifest_importing_only' ;;
    'verification manifest requires absolute path') printf '%s\n' 'args_verification_manifest_absolute' ;;
    'importing requires absolute secret-file path') printf '%s\n' 'args_importing_secret_file_absolute' ;;
    'missing importing secret file is sanitized') printf '%s\n' 'args_missing_importing_secret_sanitized' ;;
    'scale requires secret-file') printf '%s\n' 'args_scale_requires_secret_file' ;;
    'scale requires absolute secret-file path') printf '%s\n' 'args_scale_secret_file_absolute' ;;
    'scale accepts explicit corpus size') printf '%s\n' 'args_scale_accepts_explicit_corpus_size' ;;
    'scale two-point rejects explicit corpus size') printf '%s\n' 'args_scale_two_point_rejects_explicit_corpus_size' ;;
    'scale refuses source-index') printf '%s\n' 'args_scale_refuses_source_index' ;;
    'scale refuses target-index') printf '%s\n' 'args_scale_refuses_target_index' ;;
    'scale refuses verification-manifest') printf '%s\n' 'args_scale_refuses_verification_manifest' ;;
    'scale rejects too-small corpus') printf '%s\n' 'args_scale_rejects_too_small_corpus' ;;
    'INT preserves evidence, stops server, and returns 130') printf '%s\n' 'signal_int_evidence' ;;
    'TERM preserves evidence, stops server, and returns 143') printf '%s\n' 'signal_term_evidence' ;;
    'simulated cleanup failure preserves evidence and exits nonzero') printf '%s\n' 'cleanup_failure_evidence' ;;
    'testing docs describe the local migration scale proof') printf '%s\n' 'testing_docs_scale_proof_contract' ;;
    'debbie sync surface publishes migration test, docs, and workflow assets') printf '%s\n' 'debbie_public_sync_surface' ;;
    'scenario replicas is accepted for importing mode') printf '%s\n' 'args_importing_accepts_scenario_replicas' ;;
    'unavailable refuses scenario') printf '%s\n' 'args_unavailable_refuses_scenario' ;;
    'unknown scenario fails') printf '%s\n' 'args_unknown_scenario_fails' ;;
    'replica scenario proves public order, sidecars, receipt, and exact cleanup') printf '%s\n' 'importing_replicas_success' ;;
    'replica scenario rejects missing warnings field') printf '%s\n' 'importing_replicas_missing_warnings' ;;
    'replica scenario rejects empty warnings list') printf '%s\n' 'importing_replicas_empty_warnings' ;;
    'replica scenario rejects sidecar warning') printf '%s\n' 'importing_replicas_sidecar_warning' ;;
    'replica scenario rejects missing migrated replica listing') printf '%s\n' 'importing_replicas_missing_sidecar' ;;
    'replica scenario rejects mistranslated sidecar ranking settings') printf '%s\n' 'importing_replicas_wrong_sidecar_ranking' ;;
    'replica scenario rejects physical replica meta') printf '%s\n' 'importing_replicas_physical_replica_data' ;;
    'replica scenario rejects physical replica corpus') printf '%s\n' 'importing_replicas_physical_replica_corpus' ;;
    'replica scenario rejects virtual replica primary-order leakage') printf '%s\n' 'importing_replicas_primary_order_leak' ;;
    'replica scenario rejects unnormalized standard sidecar ranking') printf '%s\n' 'importing_replicas_standard_ranking_unnormalized' ;;
    'replica scenario rejects dropped standard desc ranking') printf '%s\n' 'importing_replicas_standard_desc_dropped' ;;
    'replica scenario rejects no replica checks') printf '%s\n' 'importing_replicas_no_checks' ;;
    'replica scenario rejects skipped replica checks') printf '%s\n' 'importing_replicas_skipped_check' ;;
    'replica scenario rejects check names without replica') printf '%s\n' 'importing_replicas_bad_check_name' ;;
    'replica scenario source index rejects path traversal') printf '%s\n' 'importing_replicas_source_path_traversal' ;;
    'replica scenario target index rejects path traversal') printf '%s\n' 'importing_replicas_target_path_traversal' ;;
    'terminal scenario inventory exact set') printf '%s\n' 'terminal_scenario_inventory' ;;
    *) return 1 ;;
  esac
}

record_test_result() {
  local status="$1" label="$2" id
  id="$(scenario_id_for_label "$label")" || id="UNMAPPED:${label}"
  TEST_RESULTS="${TEST_RESULTS}${status}	${id}	${label}
"
}

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  record_test_result pass "$1"
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  record_test_result fail "$1"
  printf '  [FAIL] %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2"
  fi
}

skip() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_SKIPPED=$((TESTS_SKIPPED + 1))
  record_test_result skip "$1"
  printf '  [SKIP] %s\n' "$1"
}

WORK_DIR="$(mktemp -d)"
OWNED_PIDS=()

cleanup() {
  local pid
  for pid in "${OWNED_PIDS[@]:-}"; do
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
      kill "$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    fi
  done
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

write_fake_runtime() {
  local runtime="$1"
  mkdir -p "$runtime/bin" "$runtime/state"
  command -v jq >"$runtime/state/real_jq"

  cat >"$runtime/fake-flapjack" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "${1:-}" = "build-info" ] && [ "${2:-}" = "--json" ]; then
  printf '%s\n' '{"schemaVersion":1,"version":"test","revision":"stubbed-revision","revisionKnown":true,"dirty":false,"dirtyKnown":true}'
  exit 0
fi
mkdir -p "$MIGRATION_IMPORT_CONTRACT_STUB_DIR"
printf '%s\n' "$0" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/binary_ran"
printf '%s\n' "$$" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/server_pid"
printf '%s\n' "$FLAPJACK_DATA_DIR" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/data_dir"
printf '%s\n' "${FLAPJACK_NODE_ID:-}" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/node_id"
printf '%s\n' "${FLAPJACK_PEERS:-}" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/peers"
printf '%s\n' "${FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR:-}" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/pre_activation_barrier_dir"
printf '%s\n' "${FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_BARRIER_DIR:-}" >"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/post_commit_barrier_dir"
mkdir -p "$FLAPJACK_DATA_DIR/migration_exports/jobs"
printf 'Local: http://127.0.0.1:54321\n'
trap 'exit 0' TERM INT
while :; do sleep 1; done
SH
  chmod +x "$runtime/fake-flapjack"

  cat >"$runtime/bin/jq" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

real_jq="$(<"$MIGRATION_IMPORT_CONTRACT_STUB_DIR/real_jq")"
scenario="${MIGRATION_IMPORT_CONTRACT_SCENARIO:-}"
output="$(mktemp)"
trap 'rm -f "$output"' EXIT

set +e
"$real_jq" "$@" >"$output"
rc=$?
set -e
if [ "$rc" -ne 0 ]; then
  cat "$output"
  exit "$rc"
fi

check_name=""
args=("$@")
for ((i = 0; i + 2 < ${#args[@]}; i++)); do
  if [ "${args[$i]}" = "--arg" ] && [ "${args[$((i + 1))]}" = "name" ]; then
    check_name="${args[$((i + 2))]}"
    break
  fi
done

# Alter only receipt writes so the full driver reaches its production guard.
case "$scenario:$check_name" in
  importing_replicas_no_checks:*replica*)
    "$real_jq" '.checks |= map(select((.name | contains("replica")) | not))' "$output"
    ;;
  importing_replicas_skipped_check:replica_source_fixture)
    "$real_jq" '.checks |= map(if .name == "replica_source_fixture" then .status = "skip" else . end)' "$output"
    ;;
  importing_replicas_bad_check_name:replica_primary_order)
    "$real_jq" '.checks |= map(if .name == "replica_primary_order" then .name = "primary_order" else . end)' "$output"
    ;;
  *)
    cat "$output"
    ;;
esac
SH
  chmod +x "$runtime/bin/jq"

  cat >"$runtime/bin/curl" <<'PY'
#!/usr/bin/env python3
import json
import os
import signal
import sys
import time
import urllib.parse
from pathlib import Path

state = Path(os.environ["MIGRATION_IMPORT_CONTRACT_STUB_DIR"])
scenario = os.environ.get("MIGRATION_IMPORT_CONTRACT_SCENARIO", "unavailable_ok")
scale_scenario = scenario in (
    "scale_ok",
    "scale_cleanup_failure",
    "scale_bad_total_count",
    "scale_duplicate_object_ids",
    "scale_growth_breach",
    "scale_large_sidecar_undersampled",
    "scale_manifest_count_drift",
    "scale_manifest_deleted_snapshot",
    "scale_manifest_generation_drift",
    "scale_manifest_length_drift",
    "scale_median_four_trials",
    "scale_missing_final_page",
    "scale_multiple_jobs",
    "scale_request_timeout",
    "scale_short_facet_cardinality",
    "scale_sidecar_incomplete",
    "scale_wall_clock_over_budget",
)
fixture_scenario = scenario in ("fixture_prepare_waits_tasks", "fixture_selftest_count_failure", "fixture_prefix_preflight")
state.mkdir(parents=True, exist_ok=True)

def append(name, value):
    with (state / name).open("a", encoding="utf-8") as f:
        f.write(value + "\n")

def active_corpus_size():
    size_file = state / "fixture_corpus_size"
    if size_file.exists():
        return int(size_file.read_text(encoding="utf-8").strip())
    return 20000

def write_fake_job(target, corpus_size):
    data_dir_file = state / "data_dir"
    if not data_dir_file.exists():
        return
    data_dir = Path(data_dir_file.read_text(encoding="utf-8").strip())
    jobs_dir = data_dir / "migration_exports" / "jobs"
    jobs_dir.mkdir(parents=True, exist_ok=True)
    job_dir = jobs_dir / f"fake-job-{target}"
    job_dir.mkdir(parents=True, exist_ok=True)
    if scenario == "scale_multiple_jobs":
        (jobs_dir / f"extra-job-{target}").mkdir(parents=True, exist_ok=True)
    trial_number = 1
    if "_trial_" in target:
        try:
            trial_number = int(target.rsplit("_trial_", 1)[1])
        except ValueError:
            trial_number = 1
    if scenario == "scale_median_four_trials":
        (state / "current_rss_kb").write_text(str(1000 * trial_number), encoding="utf-8")
    sidecar = job_dir / "completed_object_ids"
    sidecar_tmp = job_dir / "completed_object_ids.tmp"
    expected_page_count = (corpus_size + 999) // 1000
    page_count = expected_page_count
    if scenario == "scale_sidecar_incomplete":
        page_count = max(1, page_count - 1)
    if scenario == "scale_large_sidecar_undersampled" and corpus_size == 20000:
        page_count = expected_page_count // 2
    size_unit = 50 if scenario == "scale_growth_breach" and corpus_size <= 2000 else 100
    time.sleep(0.2)
    for page in range(1, page_count + 1):
        sidecar_tmp.write_text("x" * (page * size_unit), encoding="utf-8")
        sidecar_tmp.replace(sidecar)
        delay = 0.5
        if scenario == "scale_growth_breach" and corpus_size <= 2000:
            delay = 0.8
        if scenario in ("scale_manifest_generation_drift", "scale_manifest_count_drift", "scale_manifest_length_drift"):
            delay = 0.8
        if scenario == "scale_median_four_trials":
            delay = 0.80 + (trial_number * 0.05)
        time.sleep(delay)
    manifest = {
        "completed_objects": {
            "generation": expected_page_count,
            "count": corpus_size,
            "length": page_count * size_unit,
        }
    }
    if scenario == "scale_manifest_generation_drift":
        manifest["completed_objects"]["generation"] = page_count + 99
    if scenario == "scale_manifest_count_drift":
        manifest["completed_objects"]["count"] = corpus_size - 1
    if scenario == "scale_manifest_length_drift":
        manifest["completed_objects"]["length"] = 1
    manifest_path = job_dir / "manifest.json"
    manifest_path.write_text(json.dumps(manifest, separators=(",", ":")), encoding="utf-8")
    if scenario == "scale_manifest_deleted_snapshot":
        sampled_manifest = (
            data_dir.parent / "logs" / "scale-trials" / str(corpus_size)
            / f"trial-{trial_number}" / "sampler.json.candidates" / "manifest.0.json"
        )
        deadline = time.monotonic() + 5
        while not sampled_manifest.exists() and time.monotonic() < deadline:
            time.sleep(0.01)
        manifest_path.write_text(json.dumps({
            "lifecycle": "Deleted",
            "completed_objects": {"generation": 0, "count": 0, "length": 0},
        }, separators=(",", ":")), encoding="utf-8")

def respond(payload, code=200):
    if isinstance(payload, str):
        sys.stdout.write(payload)
    else:
        sys.stdout.write(json.dumps(payload, separators=(",", ":")))
    sys.stdout.write("\n" + str(code))

def parse_args(argv):
    method = "GET"
    body = ""
    url = ""
    fail_health = False
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "-X":
            method = argv[i + 1]
            i += 2
        elif arg == "--data":
            body = argv[i + 1]
            i += 2
        elif arg in ("-H", "-w"):
            i += 2
        elif arg == "-sf":
            fail_health = True
            i += 1
        elif arg == "--config":
            config_path = argv[i + 1]
            config = sys.stdin.read() if config_path == "-" else Path(config_path).read_text(encoding="utf-8")
            for line in config.splitlines():
                key, separator, raw_value = line.partition("=")
                if not separator:
                    continue
                value = json.loads(raw_value.strip())
                if key.strip() == "request":
                    method = value
                elif key.strip() == "url":
                    url = value
                elif key.strip() == "data-binary":
                    body = Path(value.removeprefix("@")).read_text(encoding="utf-8")
            i += 2
        elif arg.startswith("http://") or arg.startswith("https://"):
            url = arg
            i += 1
        else:
            i += 1
    return method, body, url, fail_health

method, body, url, fail_health = parse_args(sys.argv[1:])
parsed = urllib.parse.urlparse(url)

if fail_health and parsed.path == "/health":
    sys.exit(0)

append("request_order.log", f"{method} {parsed.path}")
if scenario.startswith("importing_replicas") and parsed.hostname and parsed.hostname.endswith(".algolia.net"):
    path_parts = parsed.path.split("/")
    if len(path_parts) > 3 and path_parts[1:3] == ["1", "indexes"]:
        source_index_name = urllib.parse.unquote(path_parts[3])
        append("source_api_indices.log", source_index_name)
        if source_index_name.startswith("virtual("):
            respond({"message": "virtual replica wrapper is not an index API identifier"}, 400)
            sys.exit(0)
if body:
    append("request_bodies.log", body)
    try:
        payload = json.loads(body)
        if "targetIndex" in payload:
            (state / "target_index").write_text(payload["targetIndex"], encoding="utf-8")
    except json.JSONDecodeError:
        pass

if scenario == "fixture_prefix_preflight" and parsed.path == "/1/indexes" and method == "GET":
    respond({"items": [
        {"name": "fj_scale_stale_source"},
        {"name": "keep_me"},
        {"name": "fj_scale_stale_target"},
    ]}, 200)
    sys.exit(0)

if fixture_scenario and parsed.path.startswith("/1/indexes/"):
    parts = parsed.path.split("/")
    index_name = urllib.parse.unquote(parts[3]) if len(parts) > 3 else ""
    if method in ("PUT", "POST") and parsed.path.endswith("/settings"):
        respond({"taskID": 101}, 200)
    elif method == "POST" and parsed.path.endswith("/synonyms/batch"):
        respond({"taskID": 102}, 200)
    elif method == "POST" and parsed.path.endswith("/rules/batch"):
        respond({"taskID": 103}, 200)
    elif method == "POST" and parsed.path.endswith("/batch"):
        respond({"taskID": 200}, 200)
    elif method == "GET" and "/task/" in parsed.path:
        task_id = parts[-1]
        append("waited_tasks.log", task_id)
        respond({"status": "published"}, 200)
    elif method == "POST" and parsed.path.endswith("/query"):
        if scenario == "fixture_selftest_count_failure":
            respond({"message": "count unavailable"}, 500)
        else:
            respond({"hits": [], "nbHits": 2000}, 200)
    elif method == "DELETE":
        (state / f"active_{index_name}").unlink(missing_ok=True)
        append("deleted_indices.log", index_name)
        respond({"taskID": 300}, 200)
    elif method == "GET":
        code = 404 if not (state / f"active_{index_name}").exists() else 200
        respond({"name": index_name}, code)
    else:
        respond({"message": "unexpected fixture request", "method": method, "path": parsed.path}, 500)
    sys.exit(0)

def marker(name):
    return state / f"active_{name}"

def activate(name):
    marker(name).write_text("active", encoding="utf-8")

def deactivate(name):
    marker(name).unlink(missing_ok=True)

def is_active(name):
    return marker(name).exists()

if scenario in ("self_int", "self_term") and parsed.path == "/1/indexes":
    os.kill(os.getppid(), signal.SIGINT if scenario == "self_int" else signal.SIGTERM)
    sys.exit(130 if scenario == "self_int" else 143)

target = ""
target_file = state / "target_index"
if target_file.exists():
    target = target_file.read_text(encoding="utf-8").strip()

source_indices = {}

def write_sidecar(index_name, settings):
    data_dir_file = state / "data_dir"
    if not data_dir_file.exists():
        return
    if scenario == "importing_replicas_wrong_sidecar_ranking":
        settings = dict(settings)
        settings["customRanking"] = ["asc(stale_rank)"]
    data_dir = Path(data_dir_file.read_text(encoding="utf-8").strip())
    index_dir = data_dir / index_name
    index_dir.mkdir(parents=True, exist_ok=True)
    (index_dir / "settings.json").write_text(json.dumps(settings, separators=(",", ":")), encoding="utf-8")
    if scenario == "importing_replicas_physical_replica_data":
        (index_dir / "meta.json").write_text("{}", encoding="utf-8")
    if scenario == "importing_replicas_physical_replica_corpus":
        (index_dir / "index").mkdir(exist_ok=True)
        (index_dir / "index" / "segments").write_text("physical", encoding="utf-8")

def replica_ids_for(index_name):
    relevance = f"{target}_relevance"
    standard = f"{target}_standard_rank"
    primary_ids = ["replica-001", "replica-002", "replica-003"]
    relevance_ids = ["replica-002", "replica-003", "replica-001"]
    standard_ids = ["replica-003", "replica-001", "replica-002"]
    if scenario == "importing_replicas_primary_order_leak":
        relevance_ids = primary_ids
    if index_name == relevance:
        return relevance_ids
    if index_name == standard:
        return standard_ids
    return primary_ids

async_scenario = scenario.startswith("async_")
cancel_scenario = scenario.startswith("cancel_")
is_vendor_request = parsed.netloc.endswith("algolia.net")
is_dsn_request = parsed.netloc.endswith("-dsn.algolia.net")

def vendor_index_path_parts():
    parts = parsed.path.split("/")
    return urllib.parse.unquote(parts[3]) if len(parts) > 3 else ""

def iso_now(offset_seconds=0):
    return time.strftime("%Y-%m-%dT%H:%M:%S", time.gmtime(time.time() + offset_seconds)) + ".000Z"

def seeded_documents():
    path = state / "seeded_documents.json"
    return json.loads(path.read_text(encoding="utf-8")) if path.exists() else []

# The preflight listing is the whole point of the sweep contract: it mixes this
# run's own names, a provably stale leftover, a recent leftover, a leftover with
# no parseable freshness evidence, and a foreign-prefix index. An implementation
# that deletes on prefix alone deletes the last three and fails the meta-test.
PREFLIGHT_LISTING = [
    {"name": "fj_async_meta_source", "updatedAt": iso_now(-60)},
    {"name": "fj_async_meta_target", "updatedAt": iso_now(-60)},
    {"name": "fj_async_stale_leftover", "updatedAt": "2020-01-01T00:00:00.000Z"},
    {"name": "fj_async_recent_leftover", "updatedAt": iso_now(-60)},
    {"name": "fj_async_no_timestamp"},
    {"name": "fj_scale_stale_source", "updatedAt": "2020-01-01T00:00:00.000Z"},
    {"name": "keep_me", "updatedAt": "2020-01-01T00:00:00.000Z"},
]

if async_scenario and is_vendor_request:
    append("vendor_request_order.log", f"{method} {parsed.path}")
    index_name = vendor_index_path_parts()
    if parsed.path == "/1/indexes" and method == "GET":
        preflight_marker = state / "vendor_preflight_served"
        if scenario == "async_preflight_sweep" and not preflight_marker.exists():
            preflight_marker.write_text("served", encoding="utf-8")
            respond({"items": PREFLIGHT_LISTING}, 200)
        elif scenario == "async_cleanup_stale_dsn_listing" and is_dsn_request:
            respond({"items": []}, 200)
        elif scenario in ("async_delete_http_failure", "async_delete_malformed_success") and not preflight_marker.exists():
            preflight_marker.write_text("served", encoding="utf-8")
            (state / "vendor_active_fj_async_meta_target").write_text("active", encoding="utf-8")
            (state / "seeded_documents.json").write_text(json.dumps([
                {"objectID": "fj-async-1", "name": "Alpha async record", "price": 11},
                {"objectID": "fj-async-2", "name": "Beta async record", "price": 22},
                {"objectID": "fj-async-3", "name": "Gamma async record", "price": 33},
            ]), encoding="utf-8")
            respond({"items": [{"name": "fj_async_meta_target", "updatedAt": iso_now(-60)}]}, 200)
        else:
            items = [
                {"name": path.name.removeprefix("vendor_active_"), "updatedAt": iso_now(-60)}
                for path in state.glob("vendor_active_*")
            ]
            respond({"items": items}, 200)
    elif method == "POST" and parsed.path.endswith("/batch"):
        documents = [request["body"] for request in json.loads(body)["requests"]]
        (state / "seeded_documents.json").write_text(json.dumps(documents), encoding="utf-8")
        (state / f"vendor_active_{index_name}").write_text("active", encoding="utf-8")
        respond({"taskID": 700}, 200)
    elif method == "GET" and "/task/" in parsed.path:
        append("vendor_waited_tasks.log", parsed.path.rsplit("/", 1)[1])
        respond({"status": "published"}, 200)
    elif method == "POST" and parsed.path.endswith("/query"):
        respond({"hits": [], "nbHits": len(seeded_documents())}, 200)
    elif method == "DELETE":
        if scenario == "async_delete_http_failure" and index_name == "fj_async_meta_target":
            respond({"message": "vendor refused deletion"}, 500)
        elif scenario == "async_delete_malformed_success" and index_name == "fj_async_meta_target":
            respond({"deletedAt": iso_now()}, 200)
        elif scenario == "async_ok" and not (state / f"vendor_active_{index_name}").exists():
            respond({"message": "Index does not exist"}, 404)
        else:
            (state / f"vendor_active_{index_name}").unlink(missing_ok=True)
            append("deleted_indices.log", index_name)
            respond({"taskID": 701}, 200)
    else:
        respond({"message": "unexpected async vendor request", "method": method, "path": parsed.path}, 500)
    sys.exit(0)

if cancel_scenario and is_vendor_request:
    append("vendor_request_order.log", f"{method} {parsed.path}")
    index_name = vendor_index_path_parts()
    if parsed.path == "/1/indexes" and method == "GET":
        items = [
            {"name": path.name.removeprefix("vendor_active_"), "updatedAt": iso_now(-60)}
            for path in state.glob("vendor_active_*")
        ]
        if scenario == "cancel_preflight_sweep":
            items += [
                {"name": "fj_cancel_stale_leftover", "updatedAt": "2020-01-01T00:00:00.000Z"},
                {"name": "fj_cancel_recent_leftover", "updatedAt": iso_now(-60)},
                {"name": "fj_async_stale_leftover", "updatedAt": "2020-01-01T00:00:00.000Z"},
            ]
        respond({"items": items}, 200)
    elif method == "POST" and parsed.path.endswith("/batch"):
        documents = [request["body"] for request in json.loads(body)["requests"]]
        (state / "seeded_documents.json").write_text(json.dumps(documents), encoding="utf-8")
        (state / f"vendor_active_{index_name}").write_text("active", encoding="utf-8")
        respond({"taskID": 800}, 200)
    elif method == "GET" and "/task/" in parsed.path:
        append("vendor_waited_tasks.log", parsed.path.rsplit("/", 1)[1])
        respond({"status": "published"}, 200)
    elif method == "POST" and parsed.path.endswith("/query"):
        n = 0 if scenario == "cancel_zero_source" else len(seeded_documents())
        respond({"hits": [], "nbHits": n}, 200)
    elif method == "DELETE":
        if scenario == "cancel_cleanup_residue" and index_name.startswith("fj_cancel_"):
            append("deleted_indices.log", index_name)
            respond({"taskID": 801}, 200)
        else:
            (state / f"vendor_active_{index_name}").unlink(missing_ok=True)
            append("deleted_indices.log", index_name)
            respond({"taskID": 801}, 200)
    else:
        respond({"message": "unexpected cancel vendor request", "method": method, "path": parsed.path}, 500)
    sys.exit(0)

# Each entry is one poll response: (phase, disposition). Repeated phases are
# legal; the regression script exists to prove backward movement is not.
ASYNC_STATUS_SCRIPTS = {
    "async_ok": [
        ("exporting", "running"),
        ("exporting", "running"),
        ("preparing", "running"),
        ("staging", "running"),
        ("activating", "running"),
        ("activating", "succeeded"),
    ],
    "async_preflight_sweep": [("activating", "succeeded")],
    "async_delete_http_failure": [("activating", "succeeded")],
    "async_delete_malformed_success": [("activating", "succeeded")],
    "async_cleanup_stale_dsn_listing": [("activating", "succeeded")],
    "async_success_target_absent": [("exporting", "running"), ("activating", "succeeded")],
    "async_phase_regression": [("staging", "running"), ("exporting", "succeeded")],
}

if async_scenario:
    job_id = "01890f8e-8b28-78e8-b542-8cfdcb2d4f24"
    if parsed.path == "/1/migrations/algolia" and method == "POST":
        respond({
            "jobId": job_id,
            "phase": "submitted",
            "disposition": "running",
            "createdAt": iso_now(),
            "updatedAt": iso_now(),
        }, 202)
    elif parsed.path.startswith("/1/migrations/algolia/") and method == "GET":
        script = ASYNC_STATUS_SCRIPTS[scenario]
        poll_file = state / "async_poll_count"
        poll = int(poll_file.read_text(encoding="utf-8")) if poll_file.exists() else 0
        poll_file.write_text(str(poll + 1), encoding="utf-8")
        phase, disposition = script[min(poll, len(script) - 1)]
        if disposition == "succeeded" and scenario != "async_success_target_absent":
            activate(target)
        respond({
            "jobId": job_id,
            "phase": phase,
            "disposition": disposition,
            "exportProgress": {"completed": len(seeded_documents()), "total": len(seeded_documents())},
            "createdAt": iso_now(-5),
            "updatedAt": iso_now(),
            "terminalAt": iso_now() if disposition != "running" else None,
        }, 200)
    elif parsed.path == "/1/indexes" and method == "GET":
        items = [
            {"name": path.name.removeprefix("active_"), "entries": len(seeded_documents())}
            for path in state.glob("active_*")
        ]
        respond({"items": items}, 200)
    elif method == "POST" and parsed.path.endswith("/query"):
        documents = seeded_documents()
        respond({"hits": documents, "nbHits": len(documents)}, 200)
    elif method == "DELETE" and parsed.path.startswith("/1/indexes/"):
        deactivate(urllib.parse.unquote(parsed.path.split("/")[3]))
        respond({"taskID": 2, "deletedAt": "2026-07-19T00:00:00Z"}, 200)
    else:
        respond({"message": "unexpected async request", "method": method, "path": parsed.path}, 500)
    sys.exit(0)

if cancel_scenario:
    pre_job = "01890f8e-8b28-78e8-b542-8cfdcb2d4f25"
    post_job = "01890f8e-8b28-78e8-b542-8cfdcb2d4f26"
    data_dir_file = state / "data_dir"
    data_dir = Path(data_dir_file.read_text(encoding="utf-8").strip()) if data_dir_file.exists() else state / "data"

    def write_observed(which, job_id):
        barrier_file = state / f"{which}_barrier_dir"
        if barrier_file.exists():
            barrier_dir = Path(barrier_file.read_text(encoding="utf-8").strip())
            barrier_dir.mkdir(parents=True, exist_ok=True)
            (barrier_dir / "observed").write_text(job_id, encoding="utf-8")

    def activate_with_docs(name, count):
        activate(name)
        target_dir = data_dir / name
        target_dir.mkdir(parents=True, exist_ok=True)
        (target_dir / "stub-index.json").write_text(json.dumps({"entries": count}, separators=(",", ":")), encoding="utf-8")

    def write_cancelled_precommit_spool(job_id):
        job_dir = data_dir / "migration_exports" / "jobs" / job_id
        job_dir.mkdir(parents=True, exist_ok=True)
        (job_dir / "async_migration.json").write_text(json.dumps({
            "job_uuid": job_id,
            "target_index": "fj_cancel_meta_target",
            "publication_transaction_id": "snapshot_stubbed",
        }, separators=(",", ":")), encoding="utf-8")
        (job_dir / "migration_phase.json").write_text(json.dumps({
            "job_uuid": job_id,
            "phase": "Activating",
            "disposition": "Cancelled",
            "cancel_requested": True,
            "terminal_at": iso_now(),
        }, separators=(",", ":")), encoding="utf-8")
        (job_dir / "manifest.json").write_text(json.dumps({
            "job_uuid": job_id,
            "lifecycle": "Deleted",
            "artifacts": [],
            "deleted_at": iso_now(),
        }, separators=(",", ":")), encoding="utf-8")
        if scenario == "cancel_spool_residue":
            (job_dir / "documents-000001.jsonl").write_text("staged", encoding="utf-8")

    if parsed.path == "/1/migrations/algolia" and method == "POST":
        request_payload = json.loads(body or "{}")
        request_target = request_payload.get("targetIndex", "")
        (state / "target_index").write_text(request_target, encoding="utf-8")
        if request_target.endswith("_postcommit"):
            (state / "post_job_target").write_text(request_target, encoding="utf-8")
            write_observed("post_commit", post_job)
            activate_with_docs(request_target, len(seeded_documents()))
            job_id = post_job
        else:
            (state / "pre_job_target").write_text(request_target, encoding="utf-8")
            write_observed("pre_activation", pre_job)
            job_id = pre_job
        respond({
            "jobId": job_id,
            "phase": "submitted",
            "disposition": "running",
            "createdAt": iso_now(),
            "updatedAt": iso_now(),
        }, 202)
    elif parsed.path.endswith("/cancel") and method == "POST":
        job_id = parsed.path.split("/")[-2]
        if job_id == post_job:
            (state / "post_cancel_seen").write_text("yes", encoding="utf-8")
            respond({"code": "cancel_too_late", "message": "Migration already committed"}, 409)
        else:
            (state / "pre_cancel_seen").write_text("yes", encoding="utf-8")
            respond({
                "jobId": job_id,
                "phase": "activating",
                "disposition": "running",
                "createdAt": iso_now(-5),
                "updatedAt": iso_now(),
            }, 200)
    elif parsed.path.startswith("/1/migrations/algolia/") and method == "GET":
        job_id = parsed.path.rsplit("/", 1)[-1]
        if job_id == post_job:
            disposition = "succeeded" if (state / "post_cancel_seen").exists() else "running"
        else:
            disposition = "cancelled" if (state / "pre_cancel_seen").exists() else "running"
            if disposition == "cancelled":
                write_cancelled_precommit_spool(job_id)
        respond({
            "jobId": job_id,
            "phase": "activating",
            "disposition": disposition,
            "exportProgress": {"completed": len(seeded_documents()), "total": len(seeded_documents())},
            "createdAt": iso_now(-5),
            "updatedAt": iso_now(),
            "terminalAt": iso_now() if disposition != "running" else None,
        }, 200)
    elif parsed.path == "/1/indexes" and method == "GET":
        items = []
        pre_target = (state / "pre_job_target").read_text(encoding="utf-8").strip() if (state / "pre_job_target").exists() else ""
        for path in state.glob("active_*"):
            name = path.name.removeprefix("active_")
            entries = 1 if name == pre_target else len(seeded_documents())
            items.append({"name": name, "entries": entries})
        respond({"items": items}, 200)
    elif method == "PUT" and parsed.path.startswith("/1/indexes/"):
        parts = parsed.path.split("/")
        index_name = urllib.parse.unquote(parts[3])
        object_id = urllib.parse.unquote(parts[4]) if len(parts) > 4 else "sentinel-object"
        activate(index_name)
        target_dir = data_dir / index_name
        target_dir.mkdir(parents=True, exist_ok=True)
        (target_dir / "sentinel.json").write_text(body or "{}", encoding="utf-8")
        (state / f"object_{index_name}_{object_id}").write_text(body or "{}", encoding="utf-8")
        respond({"taskID": 2, "updatedAt": iso_now()}, 200)
    elif method == "GET" and parsed.path.startswith("/1/indexes/"):
        parts = parsed.path.split("/")
        index_name = urllib.parse.unquote(parts[3])
        object_id = urllib.parse.unquote(parts[4]) if len(parts) > 4 else ""
        object_path = state / f"object_{index_name}_{object_id}"
        if object_path.exists():
            if scenario == "cancel_sentinel_field_order" and (state / "pre_cancel_seen").exists():
                respond({"objectID": "sentinel-object", "count": 1, "sentinel": "preserve-me"}, 200)
            else:
                respond(json.loads(object_path.read_text(encoding="utf-8")), 200)
        else:
            respond({"message": "not found"}, 404)
    elif method == "POST" and parsed.path.endswith("/browse"):
        query_payload = json.loads(body or "{}")
        index_name = urllib.parse.unquote(parsed.path.split("/")[3])
        if index_name.endswith("_postcommit"):
            documents = seeded_documents()
            page_size = query_payload.get("hitsPerPage", len(documents))
            if page_size > 1000:
                respond({"message": f"hitsPerPage exceeds maximum of 1000 (got {page_size})", "status": 400}, 400)
            cursor = query_payload.get("cursor")
            start = int(cursor.removeprefix("cursor-")) if isinstance(cursor, str) and cursor.startswith("cursor-") else 0
            next_offset = start + page_size
            response = {"hits": documents[start:next_offset], "nbHits": len(documents), "cursor": None}
            if next_offset < len(documents):
                response["cursor"] = f"cursor-{next_offset}"
            respond(response, 200)
        else:
            respond({"hits": [], "nbHits": 1, "cursor": None}, 200)
    elif method == "POST" and parsed.path.endswith("/query"):
        query_payload = json.loads(body or "{}")
        index_name = urllib.parse.unquote(parsed.path.split("/")[3])
        if index_name.endswith("_postcommit"):
            documents = seeded_documents()
            page_size = query_payload.get("hitsPerPage", len(documents))
            if page_size > 1000:
                respond({"message": f"hitsPerPage exceeds maximum of 1000 (got {page_size})", "status": 400}, 400)
            page = query_payload.get("page", 0)
            start = page * page_size
            respond({"hits": documents[start:start + page_size], "nbHits": len(documents)}, 200)
        else:
            respond({"hits": [], "nbHits": 1}, 200)
    else:
        respond({"message": "unexpected cancel request", "method": method, "path": parsed.path}, 500)
    sys.exit(0)

if parsed.path == "/1/migrate-from-algolia" and method == "POST":
    request_payload = json.loads(body or "{}")
    request_target = request_payload.get("targetIndex", "")
    request_key = request_payload.get("apiKey", "")
    if scenario.startswith("importing_verified") or scale_scenario:
        if scenario == "scale_request_timeout":
            sys.exit(28)
        if scenario == "scale_wall_clock_over_budget":
            time.sleep(0.2)
        if request_target.endswith("_conflict"):
            if scenario == "importing_verified_conflict_mutates":
                (state / "sentinel_mutated").write_text("yes", encoding="utf-8")
            respond({"message": "Target index already exists"}, 409)
        elif request_key == "fj_invalid_key_for_contract":
            if scenario == "importing_verified_invalid_creates":
                activate(request_target)
            respond({"message": "Algolia upstream request failed"}, 502)
        else:
            activate(request_target)
            imported_count = active_corpus_size() if scale_scenario else 7
            synonym_count = 9 if scenario == "importing_verified_bad_counts" else 1
            if scale_scenario:
                write_fake_job(request_target, imported_count)
            respond({
                "status": "complete",
                "settings": True,
                "objects": {"imported": imported_count},
                "rules": {"imported": 2},
                "synonyms": {"imported": synonym_count},
                "taskID": 0,
            }, 200)
    elif scenario.startswith("importing_replicas"):
        activate(request_target)
        activate(f"{request_target}_relevance")
        activate(f"{request_target}_standard_rank")
        write_sidecar(f"{request_target}_relevance", {
            "primary": request_target,
            "customRanking": ["asc(price)"],
            "relevancyStrictness": 80,
        })
        # Mirrors the real translated sidecar (pinned by translation_tests.rs): the
        # trailing "custom" token is consumed by translation, and default-equivalent
        # relevancyStrictness (100) is normalized away.
        standard_ranking = ["typo", "geo", "words", "filters", "proximity", "attribute", "exact"]
        if scenario == "importing_replicas_standard_ranking_unnormalized":
            standard_ranking.insert(0, "desc(standard_rank)")
        standard_custom_ranking = [] if scenario == "importing_replicas_standard_desc_dropped" else ["desc(standard_rank)"]
        write_sidecar(f"{request_target}_standard_rank", {
            "primary": request_target,
            "ranking": standard_ranking,
            "customRanking": standard_custom_ranking,
        })
        warnings = []
        if scenario == "importing_replicas_missing_warnings":
            warnings = None
        elif scenario == "importing_replicas_sidecar_warning":
            warnings = [{"code": "ReplicaSidecarNotMaterialized", "message": "sidecar missing"}]
        elif scenario == "importing_replicas_empty_warnings":
            warnings = []
        else:
            # Mirrors the live vendor shape (2026-07-19): real Algolia settings
            # responses carry defaulted fields (hitsPerPage, version, and legacy null
            # echoes for never-set canonical fields) that yield benign translation
            # warnings alongside the replica-specific ones.
            warnings = [
                {"code": "PersistedNoBehaviorSetting", "message": "Source setting is preserved for compatibility but has no Flapjack behavior.", "resource": "Settings", "jsonPath": "$.hitsPerPage"},
                {"code": "ReplicaExhaustiveSortApproximated", "message": "standard replica materialized as a virtual replica"},
                {"code": "ReplicaRelevancyStrictnessSemanticMismatch", "message": "relevancy strictness semantics differ"},
                {"code": "ReadOnlySourceField", "message": "Source field is read-only in Flapjack and is not applied during migration.", "resource": "Settings", "jsonPath": "$.version"},
            ]
        payload = {
            "status": "complete",
            "settings": True,
            "objects": {"imported": 3},
            "taskID": 0,
        }
        if warnings is not None:
            payload["warnings"] = warnings
        respond(payload, 200)
    elif scenario == "unavailable_ok":
        respond({"code": "migration_ha_unsupported", "message": "migration import is not supported on HA nodes"}, 503)
    elif scenario == "unavailable_returns_2xx":
        respond({"status": "complete", "objects": {"imported": 7}}, 200)
    elif scenario == "unavailable_wrong_code":
        respond({"code": "wrong_code", "message": "not this"}, 503)
    elif scenario == "malformed_migration_json":
        respond("{not-json", 503)
    elif scenario in ("importing_ok", "importing_empty_target"):
        respond({"status": "complete", "objects": {"imported": 7}}, 200)
    elif scenario == "importing_returns_503":
        respond({"code": "migration_import_unavailable"}, 503)
    elif scenario == "importing_wrong_count":
        respond({"status": "complete", "objects": {"imported": 8}}, 200)
    elif scenario in ("self_int", "self_term", "cleanup_failure"):
        respond({"code": "migration_ha_unsupported", "message": "migration import is not supported on HA nodes"}, 503)
    else:
        respond({"message": f"unexpected scenario {scenario}"}, 500)
elif parsed.path == "/1/indexes" and method == "GET":
    if scenario.startswith("importing_verified") or scale_scenario:
        items = []
        for active_file in state.glob("active_*"):
            name = active_file.name.removeprefix("active_")
            entries = 1 if name.endswith("_conflict") else (active_corpus_size() if scale_scenario else 7)
            items.append({"name": name, "entries": entries})
        respond({"items": items}, 200)
    elif scenario.startswith("importing_replicas"):
        items = [{"name": target, "entries": 3}]
        if scenario != "importing_replicas_missing_sidecar":
            items += [
                {"name": f"{target}_relevance", "entries": 3},
                {"name": f"{target}_standard_rank", "entries": 3},
            ]
        if scenario == "importing_replicas_duplicate_list":
            items.append({"name": f"{target}_relevance", "entries": 3})
        respond({"items": items}, 200)
    elif scenario == "malformed_indexes_json":
        respond("{not-json", 200)
    elif scenario == "unavailable_lists_target":
        respond({"items": [{"name": target, "entries": 0}]}, 200)
    elif scenario == "importing_ok":
        respond({"items": [{"name": target, "entries": 7}]}, 200)
    elif scenario == "importing_omits_target":
        respond({"items": []}, 200)
    elif scenario == "importing_empty_target":
        respond({"items": [{"name": target, "entries": 0}]}, 200)
    elif scenario == "importing_duplicates_target":
        respond({"items": [{"name": target, "entries": 7}, {"name": target, "entries": 7}]}, 200)
    elif scenario == "importing_wrong_count":
        respond({"items": [{"name": target, "entries": 7}]}, 200)
    else:
        respond({"items": []}, 200)
elif scenario.startswith("importing_replicas") and parsed.path.endswith("/query") and method == "POST":
    index_name = urllib.parse.unquote(parsed.path.split("/")[3])
    ids = replica_ids_for(index_name)
    respond({"hits": [{"objectID": object_id, "name": "Replica Fixture", "category": "replica"} for object_id in ids], "nbHits": 3}, 200)
elif (scenario.startswith("importing_verified") or scale_scenario) and parsed.path.endswith("/query") and method == "POST":
    query_payload = json.loads(body or "{}")
    query = query_payload.get("query", "")
    if scale_scenario:
        manifest = json.loads((state / "scale-manifest.json").read_text(encoding="utf-8"))
        probes = manifest["probes"]
        if query == "" and query_payload.get("hitsPerPage") == 0 and "facets" not in query_payload:
            total = 19999 if scenario == "scale_bad_total_count" else active_corpus_size()
            respond({"hits": [], "nbHits": total}, 200)
        elif query == "" and "facets" in query_payload:
            facets = manifest["aggregate_expectations"]["facets"]
            if scenario == "scale_short_facet_cardinality":
                facets = {**facets, "color": {"black": facets["color"]["black"] + 1}}
            respond({"hits": [], "nbHits": active_corpus_size(), "facets": facets}, 200)
        elif query == "" and query_payload.get("hitsPerPage", 0) > 0:
            page_size = query_payload["hitsPerPage"]
            page = query_payload.get("page", 0)
            corpus_size = active_corpus_size()
            object_ids = [f"scale-{number:06d}" for number in range(1, corpus_size + 1)]
            if scenario == "scale_duplicate_object_ids":
                object_ids[-2] = object_ids[0]
            if scenario == "scale_missing_final_page":
                object_ids = object_ids[:-1]
            start = page * page_size
            ids = object_ids[start:start + page_size]
            respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": corpus_size}, 200)
        elif query == manifest["known_answers_query"]:
            hits = [{**hit, "_highlightResult": {}} for hit in manifest["known_answers"]]
            respond({"hits": hits, "nbHits": len(hits)}, 200)
        elif query == probes["settings"]["request"]["query"]:
            ids = probes["settings"]["expected_object_ids"]
            respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": len(ids)}, 200)
        elif query == probes["synonym"]["request"]["query"]:
            ids = probes["synonym"]["expected_object_ids"]
            respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": len(ids)}, 200)
        elif query == probes["promotion"]["request"]["query"]:
            ids = [probes["promotion"]["expected_first_object_id"], probes["promotion"]["competitor_object_id"]]
            rules = [{"objectID": probes["promotion"]["expected_rule_id"]}]
            respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": len(ids), "appliedRules": rules}, 200)
        elif query == probes["hiding"]["request"]["query"]:
            ids = probes["hiding"]["expected_object_ids"]
            rules = [{"objectID": probes["hiding"]["expected_rule_id"]}]
            respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": len(ids), "appliedRules": rules}, 200)
        else:
            respond({"message": f"unexpected scale query {query}"}, 500)
    elif query == "Known answer":
        known_hits = [
            {"objectID": "known-1", "name": "Alpha Known", "description": "Known answer alpha", "price": 10, "_highlightResult": {}},
            {"objectID": "known-2", "name": "Beta Known", "description": "Known answer beta", "price": 20, "_highlightResult": {}},
        ]
        if scenario == "importing_verified_bad_known_answers":
            known_hits[0]["price"] = 999
        respond({"hits": known_hits, "nbHits": 2}, 200)
    elif query == "settings-proof":
        ids = ["known-1", "known-2"] if scenario == "importing_verified_bad_settings" else ["known-2", "known-1"]
        respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": 2}, 200)
    elif query == "trainer":
        ids = [] if scenario == "importing_verified_bad_synonym" else ["known-2"]
        respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": len(ids)}, 200)
    elif query == "trail":
        ids = ["competitor", "known-1"] if scenario == "importing_verified_bad_promotion" else ["known-1", "competitor"]
        respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": 2, "appliedRules": [{"objectID": "rule-promote"}]}, 200)
    elif query == "rain":
        ids = ["known-2", "known-1"] if scenario == "importing_verified_bad_hiding" else ["known-2"]
        respond({"hits": [{"objectID": object_id} for object_id in ids], "nbHits": len(ids), "appliedRules": [{"objectID": "rule-hide"}]}, 200)
    else:
        respond({"message": f"unexpected query {query}"}, 500)
elif scale_scenario and method == "GET" and parsed.path.endswith("/settings"):
    respond({"attributesForFaceting": ["category", "color"]}, 200)
elif (scenario.startswith("importing_verified") or scale_scenario) and method == "PUT" and parsed.path.endswith("/sentinel-object"):
    index_name = urllib.parse.unquote(parsed.path.split("/")[3])
    activate(index_name)
    (state / "sentinel.json").write_text(body, encoding="utf-8")
    respond({"taskID": 1, "objectID": "sentinel-object"}, 200)
elif (scenario.startswith("importing_verified") or scale_scenario) and method == "GET" and parsed.path.endswith("/sentinel-object"):
    sentinel = json.loads((state / "sentinel.json").read_text(encoding="utf-8"))
    if (state / "sentinel_mutated").exists():
        sentinel["sentinel"] = "mutated"
    respond(sentinel, 200)
elif scenario.startswith("importing_replicas") and method == "POST" and parsed.path.endswith("/batch"):
    index_name = urllib.parse.unquote(parsed.path.split("/")[3])
    activate(index_name)
    append("seeded_indices.log", index_name)
    respond({"taskID": 11}, 200)
elif scenario.startswith("importing_replicas") and method == "PUT" and parsed.path.endswith("/settings"):
    index_name = urllib.parse.unquote(parsed.path.split("/")[3])
    activate(index_name)
    append("settings_indices.log", index_name)
    respond({"taskID": 12}, 200)
elif scenario.startswith("importing_replicas") and method == "GET" and "/task/" in parsed.path:
    respond({"status": "published"}, 200)
elif scenario.startswith("importing_replicas") and method == "DELETE" and parsed.path.startswith("/1/indexes/"):
    index_name = urllib.parse.unquote(parsed.path.split("/")[3])
    append("deleted_indices.log", index_name)
    deactivate(index_name)
    respond({"taskID": 13, "deletedAt": "2026-07-17T00:00:00Z"}, 200)
elif (scenario.startswith("importing_verified") or scale_scenario) and method == "DELETE" and parsed.path.startswith("/1/indexes/"):
    index_name = urllib.parse.unquote(parsed.path.split("/")[3])
    if scenario != "importing_verified_cleanup_residue" or index_name.endswith("_conflict"):
        deactivate(index_name)
    respond({"taskID": 2, "deletedAt": "2026-07-17T00:00:00Z"}, 200)
else:
    respond({"message": "unexpected request", "method": method, "path": parsed.path}, 500)
PY
  chmod +x "$runtime/bin/curl"

  cat >"$runtime/bin/ps" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ge 4 ] && [ "${1:-}" = "-o" ] && [ "${2:-}" = "rss=" ] && [ "${3:-}" = "-p" ]; then
  if [ -n "${MIGRATION_IMPORT_CONTRACT_STUB_DIR:-}" ] && [ -f "$MIGRATION_IMPORT_CONTRACT_STUB_DIR/current_rss_kb" ]; then
    cat "$MIGRATION_IMPORT_CONTRACT_STUB_DIR/current_rss_kb"
    exit 0
  fi
fi
exec /bin/ps "$@"
SH
  chmod +x "$runtime/bin/ps"

  cat >"$runtime/bin/algolia_corpus_fixture.sh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
state="$MIGRATION_IMPORT_CONTRACT_STUB_DIR"
mode="${1:-}"
shift || true
mkdir -p "$state"
case "$mode" in
  prepare)
    corpus_size=""
    work_dir=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --corpus-size) corpus_size="${2:-}"; shift 2 ;;
        --secret-file) shift 2 ;;
        --work-dir) work_dir="${2:-}"; shift 2 ;;
        *) echo "unexpected fixture argument: $1" >&2; exit 2 ;;
      esac
    done
    [ -n "$work_dir" ] || exit 2
    source_index="fj_scale_stub_source"
    target_index="fj_scale_stub_target"
    ledger="$work_dir/algolia-scale-ledger.json"
    printf '%s\n' "$corpus_size" >"$state/fixture_corpus_size"
    printf '%s\n' "$source_index" >"$state/fixture_source_index"
    printf '%s\n' "$target_index" >"$state/fixture_target_index"
    printf '%s\n' "$ledger" >"$state/fixture_ledger_path"
    printf '{"algolia_sources":[{"name":"%s","owned":true}]}\n' "$source_index" >"$ledger"
    "$MIGRATION_IMPORT_CONTRACT_TEST_GENERATOR" manifest --corpus-size "$corpus_size" >"$state/scale-manifest.json"
    if [ "${MIGRATION_IMPORT_CONTRACT_SCENARIO:-}" = "scale_init_manifest_failure" ]; then
      rm -rf "$work_dir/logs"
    fi
    jq -n --arg source "$source_index" --arg target "$target_index" --arg ledger "$ledger" \
      '{source_index:$source,target_index:$target,ledger_path:$ledger}'
    ;;
  source-count)
    index=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --index) index="${2:-}"; shift 2 ;;
        --secret-file|--work-dir) shift 2 ;;
        *) echo "unexpected fixture argument: $1" >&2; exit 2 ;;
      esac
    done
    printf '%s\n' "$index" >"$state/fixture_source_count_index"
    cat "$state/fixture_corpus_size"
    ;;
  cleanup)
    ledger=""
    while [ "$#" -gt 0 ]; do
      case "$1" in
        --ledger) ledger="${2:-}"; shift 2 ;;
        --secret-file) shift 2 ;;
        *) echo "unexpected fixture argument: $1" >&2; exit 2 ;;
      esac
    done
    printf '%s\n' "$ledger" >"$state/fixture_cleanup_ledger"
    [ "${MIGRATION_IMPORT_CONTRACT_SCENARIO:-}" != "scale_cleanup_failure" ] || exit 1
    ;;
  *)
    echo "unexpected fixture mode: $mode" >&2
    exit 2
    ;;
esac
SH
  chmod +x "$runtime/bin/algolia_corpus_fixture.sh"
}

secret_file_for() {
  local runtime="$1"
  mkdir -p "$runtime"
  printf 'ALGOLIA_APP_ID=APPID_CANARY\nALGOLIA_ADMIN_KEY=ADMIN_SECRET_CANARY\n' >"$runtime/secret.env"
  printf '%s\n' "$runtime/secret.env"
}

verification_manifest_for() {
  local runtime="$1"
  mkdir -p "$runtime"
  cat >"$runtime/verification-manifest.json" <<'JSON'
{
  "source_count": 7,
  "synonym_count": 1,
  "rule_count": 2,
  "known_answers_query": "Known answer",
  "known_answers": [
    {"objectID":"known-1","name":"Alpha Known","description":"Known answer alpha","price":10},
    {"objectID":"known-2","name":"Beta Known","description":"Known answer beta","price":20}
  ],
  "probes": {
    "settings": {"request":{"query":"settings-proof","hitsPerPage":10},"expected_object_ids":["known-2","known-1"]},
    "synonym": {"request":{"query":"trainer","hitsPerPage":10},"expected_object_ids":["known-2"]},
    "promotion": {"request":{"query":"trail","hitsPerPage":10},"expected_first_object_id":"known-1","competitor_object_id":"competitor","expected_rule_id":"rule-promote"},
    "hiding": {"request":{"query":"rain","hitsPerPage":10},"hidden_object_id":"known-1","expected_object_ids":["known-2"],"expected_rule_id":"rule-hide"}
  }
}
JSON
  printf '%s\n' "$runtime/verification-manifest.json"
}

run_oracle_with_stub() {
  local scenario="$1" out="$2" runtime="$3"
  shift 3
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="$scenario" \
    MIGRATION_IMPORT_CONTRACT_TEST_GENERATOR="$SCRIPT_DIR/common/generate_algolia_corpus.sh" \
    FJ_SCALE_FIXTURE_BIN="$runtime/bin/algolia_corpus_fixture.sh" \
    bash "$ORACLE" "$@" >"$out" 2>&1
  local rc=$?
  set -e
  if [ -f "$runtime/state/server_pid" ]; then
    OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  fi
  printf '%s' "$rc"
}

extract_evidence_path() {
  sed -n 's/^INFO: preserved sanitized migration import evidence at //p' "$1" | tail -1
}

evidence_has_contract_files() {
  local evidence="$1"
  [ -d "$evidence" ] \
    && [ -f "$evidence/logs/flapjack-server.log" ] \
    && [ -f "$evidence/logs/migration-response.raw" ] \
    && [ -f "$evidence/logs/list-indices.raw" ] \
    && [ -f "$evidence/receipt.json" ] \
    && [ -d "$evidence/migration_exports/jobs" ]
}

evidence_receipt_has_run_metadata() {
  local evidence="$1"
  jq -e '
    .head != null
    and .started_at != null
    and .completed_at != null
    and (.elapsed_seconds | type) == "number"
    and (.host.name | type) == "string"
    and (.host.uname | type) == "string"
    and (.runtime.shell | type) == "string"
    and (.runtime.bash_version | type) == "string"
    and (.runtime.working_directory | type) == "string"
    and .binary.path != null
    and .binary.build_info.revisionKnown == true
    and (.command | index("engine/tests/migration_import_contract.sh")) != null
  ' "$evidence/receipt.json" >/dev/null
}

evidence_scale_job_archives_are_relative_and_resolve() {
  local evidence="$1" archive
  while IFS= read -r archive; do
    case "$archive" in
      ""|/*) return 1 ;;
    esac
    [ -f "$evidence/$archive/manifest.json" ] || return 1
    [ -f "$evidence/$archive/completed_object_ids" ] || return 1
  done < <(jq -r '.scale.conditions_observed[].trials[].job_archive' "$evidence/receipt.json")
}

evidence_scale_sampled_archives_resolve() {
  local evidence="$1" archive
  while IFS= read -r archive; do
    [ -f "$evidence/$archive/manifest.sampled.json" ] || return 1
    [ -f "$evidence/$archive/completed_object_ids.sampled" ] || return 1
  done < <(jq -r '.scale.conditions_observed[].trials[].job_archive' "$evidence/receipt.json")
}

evidence_scale_deleted_archives_preserve_authentic_state() {
  local evidence="$1" archive
  while IFS= read -r archive; do
    jq -e '.lifecycle == "Deleted"' "$evidence/$archive/manifest.json" >/dev/null || return 1
    jq -e '
      .completed_objects.generation > 0
      and .completed_objects.count > 0
      and .completed_objects.length > 0
    ' "$evidence/$archive/manifest.sampled.json" >/dev/null || return 1
  done < <(jq -r '.scale.conditions_observed[].trials[].job_archive' "$evidence/receipt.json")
}

# The hot path is sample_scale_trial's poll loop, which re-runs every 10ms; the
# artifact-capture helper it calls is part of that path. Sampling inherently needs
# file_size_bytes and ps per poll, so the guard forbids only manifest *validation*
# (jq / scale_manifest_matches), which must stay in the post-run commit step.
scale_sampler_poll_loop_body() {
  awk '
    /^sample_scale_trial\(\) \{/ { in_fn = 1 }
    in_fn && /while \[ -f "\$marker" \]; do/ { in_loop = 1; next }
    in_loop && /^  done$/ { exit }
    in_loop { print }
  ' "$ORACLE"
  sed -n '/^capture_scale_trial_artifacts() {/,/^}/p' "$ORACLE"
}

scale_sampler_hot_path_is_cheap() {
  local body
  body="$(scale_sampler_poll_loop_body)"
  # A body the awk failed to locate would vacuously pass the grep below.
  grep -Fq 'capture_scale_trial_artifacts' <<<"$body" || return 1
  ! grep -Eq '(^|[^[:alnum:]_])(jq|scale_manifest_matches)([^[:alnum:]_]|$)' <<<"$body"
}

normalize_scenario_ids() {
  awk 'NF {print $1}' | sort -u
}

observed_scenario_ids() {
  printf '%s' "$TEST_RESULTS" | awk -F '\t' '$2 != "" {print $2}'
}

scenario_inventory_matches() {
  local expected_ids="$1" observed_ids="$2" expected_file observed_file
  expected_file="$WORK_DIR/expected-scenario-ids.txt"
  observed_file="$WORK_DIR/observed-scenario-ids.txt"
  printf '%s\n' "$expected_ids" | normalize_scenario_ids >"$expected_file"
  printf '%s\n' "$observed_ids" | normalize_scenario_ids >"$observed_file"
  diff -u "$expected_file" "$observed_file"
}

expected_scenario_count() {
  printf '%s\n' "$EXPECTED_SCENARIO_IDS" | normalize_scenario_ids | wc -l | tr -d ' '
}

assert_scenario_inventory_rejects_missing_stage3_id() {
  local expected_without_stage3 out
  out="$WORK_DIR/inventory-missing-stage3.diff"
  expected_without_stage3="$(printf '%s\n' "$EXPECTED_SCENARIO_IDS" | grep -Fxv 'scale_repository_receipt')"
  if ! scenario_inventory_matches "$expected_without_stage3" "$EXPECTED_SCENARIO_IDS" >"$out" 2>&1 \
    && grep -Fq 'scale_repository_receipt' "$out"; then
    pass 'scenario inventory rejects a missing Stage 3 scenario ID'
  else
    fail 'scenario inventory rejects a missing Stage 3 scenario ID' "diff=$(cat "$out" 2>/dev/null || true)"
  fi
}

assert_terminal_scenario_inventory() {
  local expected_count observed_count diff_out
  expected_count="$(expected_scenario_count)"
  observed_count="$(observed_scenario_ids | normalize_scenario_ids | wc -l | tr -d ' ')"
  diff_out="$WORK_DIR/scenario-inventory.diff"
  if scenario_inventory_matches "$EXPECTED_SCENARIO_IDS" "$(observed_scenario_ids)" >"$diff_out" 2>&1 \
    && [ "$observed_count" = "$expected_count" ] \
    && [ "$TESTS_PASSED" = "$expected_count" ] \
    && [ "$TESTS_FAILED" = "0" ] \
    && [ "$TESTS_SKIPPED" = "0" ]; then
    printf 'Scenario inventory: expected=%s observed=%s pass=%s fail=%s skip=%s\n' \
      "$expected_count" "$observed_count" "$TESTS_PASSED" "$TESTS_FAILED" "$TESTS_SKIPPED"
    return 0
  fi
  printf '  [FAIL] terminal scenario inventory exact set\n'
  printf '    expected=%s observed=%s pass=%s fail=%s skip=%s\n' \
    "$expected_count" "$observed_count" "$TESTS_PASSED" "$TESTS_FAILED" "$TESTS_SKIPPED"
  if [ -s "$diff_out" ]; then
    sed 's/^/    /' "$diff_out"
  fi
  return 1
}

assert_repository_scale_evidence_contract() {
  local repo_dir receipt_rel receipt_path evidence_dir tracked_receipts=()
  repo_dir="$(cd "$SCRIPT_DIR/../.." && pwd)"
  while IFS= read -r receipt_rel; do
    tracked_receipts+=("$receipt_rel")
  done < <(git -C "$repo_dir" ls-files 'build/scale-evidence/*/receipt.json')
  if [ "${#tracked_receipts[@]}" -ne 1 ]; then
    fail 'repository tracks exactly one selected two-point scale receipt' "tracked=${tracked_receipts[*]:-}"
    return
  fi

  receipt_rel="${tracked_receipts[0]}"
  receipt_path="$repo_dir/$receipt_rel"
  evidence_dir="$(dirname "$receipt_path")"
  if jq -e --arg head "$SCALE_EVIDENCE_HEAD" '
      . as $receipt
      |
      .status == "pass"
      and .head == $head
      and .binary.build_info.revision == $head
      and .binary.build_info.dirty == false
      and .scale.mode == "two-point"
      and .scale.conditions == [2000,20000]
      and .scale.trials_per_condition >= 3
      and .scale.two_point_ratio_status == "pass"
      and (.scale.two_point_observed_rewrite_ratio | type) == "number"
      and (.owned_resources.targets | all(type == "string" and length > 0))
      and (.owned_resources.algolia_sources | length) == 2
      and ([.scale.conditions_observed[].n] | sort) == [2000,20000]
      and all(.scale.conditions_observed[];
        (.source_index | type) == "string"
        and (.source_index | length) > 0
        and (.trials | length) == $receipt.scale.trials_per_condition)
      and all(.scale.conditions_observed[].trials[];
        (.job_archive | type) == "string"
        and (.job_archive | startswith("/") | not)
        and .distinct_sizes_observed >= ([(.expected_page_count / 2 | ceil), 2] | max)
        and .manifest.completed_objects.generation == .expected_page_count
        and .manifest.completed_objects.count == .condition_n
        and .manifest.completed_objects.length == .final_sidecar_bytes)
    ' "$receipt_path" >/dev/null \
    && evidence_scale_job_archives_are_relative_and_resolve "$evidence_dir"; then
    pass 'repository selected scale receipt is clean-head and archive-resolving'
  else
    fail 'repository selected scale receipt is clean-head and archive-resolving' "receipt=$receipt_rel"
  fi
}

assert_generator_contract() {
  local docs manifest scale_manifest invalid_out
  docs="$WORK_DIR/generator-documents.ndjson"
  manifest="$WORK_DIR/generator-manifest.json"
  scale_manifest="$WORK_DIR/generator-scale-manifest.json"
  invalid_out="$WORK_DIR/generator-invalid.out"
  if "$SCRIPT_DIR/common/generate_algolia_corpus.sh" documents --corpus-size 2 >"$docs" \
    && "$SCRIPT_DIR/common/generate_algolia_corpus.sh" manifest --corpus-size 2 >"$manifest" \
    && "$SCRIPT_DIR/common/generate_algolia_corpus.sh" manifest --corpus-size 20000 >"$scale_manifest" \
    && [ "$(wc -l <"$docs" | tr -d ' ')" = "2" ] \
    && jq -e '
      . == [
        {objectID:"scale-000001",name:"Alpha Scale Jacket",description:"Known scale answer alpha settings proof trail rain",category:"jackets",color:"red",price:101,popularity:100},
        {objectID:"scale-000002",name:"Beta Scale Trainer",description:"Known scale answer beta settings proof trail rain",category:"shoes",color:"blue",price:82,popularity:110}
      ]
    ' <(jq -s '.' "$docs") >/dev/null \
    && jq -e --slurpfile documents <(jq -s '.' "$docs") '
      .probes as $probes
      | (.source_configuration.rules[] | select(.objectID == $probes.promotion.expected_rule_id)) as $promotion
      | (.source_configuration.rules[] | select(.objectID == $probes.hiding.expected_rule_id)) as $hiding
      | .source_count == 2
      and .known_answers == $documents[0]
      and (.source_configuration.settings | has("typoTolerance") | not)
      and .source_configuration.settings.paginationLimitedTo == 2
      and .aggregate_expectations == {
        final_object_id:"scale-000002",
        facets:{category:{jackets:1,shoes:1},color:{black:0,blue:1,green:0,red:1}}
      }
      and .source_configuration.settings.customRanking == ["desc(popularity)"]
      and .source_configuration.synonyms == [{objectID:"synonym-trainer",type:"synonym",synonyms:["trainer","sneaker"]}]
      and .probes.settings == {request:{query:"settings proof",hitsPerPage:2},expected_object_ids:["scale-000002","scale-000001"]}
      and .probes.synonym == {request:{query:"sneaker",hitsPerPage:2},expected_object_ids:["scale-000002"]}
      and $promotion.conditions == [{pattern:$probes.promotion.request.query,anchoring:"is"}]
      and $promotion.consequence.promote == [{objectID:$probes.promotion.expected_first_object_id,position:0}]
      and .probes.promotion.competitor_object_id == "scale-000002"
      and $hiding.conditions == [{pattern:$probes.hiding.request.query,anchoring:"is"}]
      and $hiding.consequence.hide == [{objectID:$probes.hiding.hidden_object_id}]
      and .probes.hiding.expected_object_ids == ["scale-000002"]
    ' "$manifest" >/dev/null \
    && jq -e '
      .source_count == 20000
      and (.known_answers | length) == 2
      and .source_configuration.settings.paginationLimitedTo == 20000
      and .aggregate_expectations == {
        final_object_id:"scale-020000",
        facets:{
          category:{jackets:10000,shoes:10000},
          color:{black:13332,blue:1,green:6666,red:1}
        }
      }
    ' "$scale_manifest" >/dev/null; then
    pass 'generator keeps documents, setup, and live scale probes internally consistent'
  else
    fail 'generator keeps documents, setup, and live scale probes internally consistent'
  fi

  if ! "$SCRIPT_DIR/common/generate_algolia_corpus.sh" documents --corpus-size 0 >"$invalid_out" 2>&1; then
    pass 'generator rejects non-positive explicit size'
  else
    fail 'generator rejects non-positive explicit size' "output=$(cat "$invalid_out")"
  fi

  if ! "$SCRIPT_DIR/common/generate_algolia_corpus.sh" documents --corpus-size 1 >"$invalid_out" 2>&1 \
    && grep -Fq -- "--corpus-size must be at least 2" "$invalid_out" \
    && ! "$SCRIPT_DIR/common/generate_algolia_corpus.sh" manifest --corpus-size 1 >>"$invalid_out" 2>&1 \
    && grep -Fq "scale probe coverage" "$invalid_out"; then
    pass 'generator rejects corpus sizes that cannot satisfy scale probe references'
  else
    fail 'generator rejects corpus sizes that cannot satisfy scale probe references' "output=$(cat "$invalid_out")"
  fi
}

assert_fixture_source_count_contract() {
  local runtime out secret work rc
  runtime="$WORK_DIR/fixture_source_count"
  out="$runtime.out"
  work="$runtime/work"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_prepare_waits_tasks" \
    bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" source-count \
      --index source_products --secret-file "$secret" --work-dir "$work" >"$out" 2>&1
  rc=$?
  set -e
  if [ "$rc" = "0" ] \
    && [ "$(cat "$out")" = "2000" ] \
    && grep -Fxq 'POST /1/indexes/source_products/query' "$runtime/state/request_order.log" \
    && grep -Fxq '{"query":"","hitsPerPage":0}' "$runtime/state/request_bodies.log"; then
    pass 'fixture source-count emits only the live empty-query nbHits value'
  else
    fail 'fixture source-count emits only the live empty-query nbHits value' "rc=$rc output=$(cat "$out")"
  fi
}

assert_fixture_reuse_contract() {
  local runtime out secret work metadata ledger
  runtime="$WORK_DIR/fixture_reuse"
  out="$runtime.out"
  work="$runtime/work"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    FJ_SCALE_REUSE_FIXTURE="existing_source" \
    bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" prepare \
      --corpus-size 2 --secret-file "$secret" --work-dir "$work" >"$out" 2>&1
  local rc=$?
  set -e
  metadata="$(cat "$out" 2>/dev/null || true)"
  ledger="$(printf '%s\n' "$metadata" | jq -r '.ledger_path // empty' 2>/dev/null || true)"
  if [ "$rc" = "0" ] \
    && [ "$ledger" = "$work/algolia-scale-ledger.json" ] \
    && jq -e '.source_index == "existing_source"' "$out" >/dev/null \
    && jq -e '.algolia_sources == [{name:"existing_source",owned:false}]' "$ledger" >/dev/null \
    && [ ! -f "$runtime/state/request_order.log" ]; then
    PATH="$runtime/bin:$PATH" MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
      bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" cleanup --ledger "$ledger" --secret-file "$secret" >>"$out" 2>&1
    if [ ! -f "$runtime/state/request_order.log" ]; then
      pass 'fixture reuse records unowned source and refuses cleanup deletion'
      return
    fi
  fi
  fail 'fixture reuse records unowned source and refuses cleanup deletion' "rc=$rc output=$(cat "$out")"
}

assert_fixture_prefix_preflight_cleans_scale_residue() {
  local runtime out secret work ledger
  runtime="$WORK_DIR/fixture_prefix_preflight"
  out="$runtime.out"
  work="$runtime/work"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_prefix_preflight" \
    bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" prepare \
      --corpus-size 2000 --secret-file "$secret" --work-dir "$work" >"$out" 2>&1
  local rc=$?
  set -e
  ledger="$(jq -r '.ledger_path // empty' "$out" 2>/dev/null || true)"
  if [ "$rc" = "0" ] \
    && [ -f "$ledger" ] \
    && cmp -s <(printf '%s\n' \
      'GET /1/indexes' \
      'DELETE /1/indexes/fj_scale_stale_source' \
      'GET /1/indexes/fj_scale_stale_source' \
      'DELETE /1/indexes/fj_scale_stale_target' \
      'GET /1/indexes/fj_scale_stale_target') <(head -n 5 "$runtime/state/request_order.log"); then
    PATH="$runtime/bin:$PATH" \
      MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
      MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_prefix_preflight" \
      bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" cleanup --ledger "$ledger" --secret-file "$secret" >>"$out" 2>&1
    pass 'fixture preflight deletes stale fj_scale indices before fresh seeding'
  else
    fail 'fixture preflight deletes stale fj_scale indices before fresh seeding' "rc=$rc output=$(cat "$out") order=$(cat "$runtime/state/request_order.log" 2>/dev/null || true)"
  fi
}

assert_fixture_prepare_waits_for_algolia_tasks() {
  local runtime out secret work ledger
  runtime="$WORK_DIR/fixture_prepare_waits_tasks"
  out="$runtime.out"
  work="$runtime/work"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_prepare_waits_tasks" \
    bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" prepare \
      --corpus-size 2000 --secret-file "$secret" --work-dir "$work" >"$out" 2>&1
  local rc=$?
  set -e
  ledger="$(jq -r '.ledger_path // empty' "$out" 2>/dev/null || true)"
  if [ "$rc" = "0" ] \
    && [ -f "$ledger" ] \
    && cmp -s <(printf '101\n102\n103\n200\n200\n') "$runtime/state/waited_tasks.log"; then
    PATH="$runtime/bin:$PATH" \
      MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
      MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_prepare_waits_tasks" \
      bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" cleanup --ledger "$ledger" --secret-file "$secret" >>"$out" 2>&1
    pass 'fixture waits for settings, synonyms, rules, and document batch tasks before prepare returns'
  else
    fail 'fixture waits for settings, synonyms, rules, and document batch tasks before prepare returns' "rc=$rc output=$(cat "$out") waited=$(cat "$runtime/state/waited_tasks.log" 2>/dev/null || true)"
  fi
}

assert_fixture_selftest_failure_cleans_source() {
  local runtime out secret source
  runtime="$WORK_DIR/fixture_selftest_count_failure"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_selftest_count_failure" \
    bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" selftest \
      --corpus-size 2000 --secret-file "$secret" >"$out" 2>&1
  local rc=$?
  set -e
  source="$(cat "$runtime/state/deleted_indices.log" 2>/dev/null | head -1)"
  if [ "$rc" != "0" ] && case "$source" in fj_scale_source_*) true ;; *) false ;; esac; then
    pass 'fixture selftest cleans its prepared source when count verification fails'
  else
    fail 'fixture selftest cleans its prepared source when count verification fails' "rc=$rc deleted=$(cat "$runtime/state/deleted_indices.log" 2>/dev/null || true) output=$(cat "$out")"
  fi
}

assert_fixture_selftest_rejects_reuse_fixture() {
  local runtime out secret
  runtime="$WORK_DIR/fixture_selftest_reuse"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="fixture_prepare_waits_tasks" \
    FJ_SCALE_REUSE_FIXTURE="fj_scale_existing_source" \
    bash "$SCRIPT_DIR/common/algolia_corpus_fixture.sh" selftest \
      --corpus-size 2000 --secret-file "$secret" >"$out" 2>&1
  local rc=$?
  set -e
  if [ "$rc" != "0" ] \
    && grep -Fq "FJ_SCALE_REUSE_FIXTURE is not allowed in selftest mode" "$out" \
    && [ ! -f "$runtime/state/request_order.log" ]; then
    pass 'fixture selftest rejects reuse fixture before seeding'
  else
    fail 'fixture selftest rejects reuse fixture before seeding' "rc=$rc output=$(cat "$out") requests=$(cat "$runtime/state/request_order.log" 2>/dev/null || true)"
  fi
}

workflow_job_body() {
  local workflow="$1" job="$2"
  awk -v marker="  ${job}:" '
    $0 == marker { in_job = 1; print; next }
    in_job && $0 ~ /^  [A-Za-z0-9_-]+:/ { exit }
    in_job { print }
  ' "$workflow"
}

text_contains() {
  local text="$1" needle="$2"
  grep -Fq "$needle" <<<"$text"
}

remove_scale_secret_env_from_workflow() {
  local source="$1" target="$2"
  awk '
    $0 == "  migration-scale-contract:" { in_scale = 1 }
    in_scale && $0 ~ /^  [A-Za-z0-9_-]+:/ && $0 != "  migration-scale-contract:" { in_scale = 0 }
    in_scale && $0 == "        env:" { skip = 1; next }
    skip && $0 == "        run: |" { skip = 0 }
    skip { next }
    { print }
  ' "$source" >"$target"
}

remove_scale_artifact_download_from_workflow() {
  local source="$1" target="$2"
  awk '
    $0 == "  migration-scale-contract:" { in_scale = 1 }
    in_scale && $0 ~ /^  [A-Za-z0-9_-]+:/ && $0 != "  migration-scale-contract:" { in_scale = 0 }
    in_scale && $0 == "      - name: Download server binary" { skip = 1; next }
    skip && ($0 ~ /^      - name: / || $0 ~ /^      #/) { skip = 0 }
    skip { next }
    { print }
  ' "$source" >"$target"
}

assert_scale_workflow_mutation_rejected() {
  local label="$1" mutation="$2" mutated_workflow original_workflow
  local before_run before_passed before_failed before_skipped before_results
  mutated_workflow="$WORK_DIR/${mutation}.yml"
  "$mutation" "$NIGHTLY_WORKFLOW" "$mutated_workflow"

  before_run="$TESTS_RUN"
  before_passed="$TESTS_PASSED"
  before_failed="$TESTS_FAILED"
  before_skipped="$TESTS_SKIPPED"
  before_results="$TEST_RESULTS"
  original_workflow="$NIGHTLY_WORKFLOW"
  NIGHTLY_WORKFLOW="$mutated_workflow"
  assert_nightly_importing_contract >/dev/null
  local rejected="false"
  [ "$TESTS_FAILED" -gt "$before_failed" ] && rejected="true"
  TESTS_RUN="$before_run"
  TESTS_PASSED="$before_passed"
  TESTS_FAILED="$before_failed"
  TESTS_SKIPPED="$before_skipped"
  TEST_RESULTS="$before_results"
  NIGHTLY_WORKFLOW="$original_workflow"

  if [ "$rejected" = "true" ]; then
    pass "$label"
  else
    fail "$label"
  fi
}

assert_nightly_importing_contract() {
  local job_count oracle_step_count scale_job_body scale_secret_write_count
  if [ ! -f "$NIGHTLY_WORKFLOW" ]; then
    fail 'nightly workflow exists for scheduled importing oracle' "$NIGHTLY_WORKFLOW"
    return
  fi

  job_count="$(grep -Ec '^  migration-import-contract:' "$NIGHTLY_WORKFLOW")"
  oracle_step_count="$(grep -Ec 'bash engine/tests/migration_import_contract\.sh --expect-mode importing' "$NIGHTLY_WORKFLOW")"
  scale_job_body="$(workflow_job_body "$NIGHTLY_WORKFLOW" migration-scale-contract)"

  [ "$job_count" = "1" ] \
    && pass 'nightly has exactly one migration import contract job' \
    || fail 'nightly has exactly one migration import contract job' "job_count=$job_count"
  [ "$oracle_step_count" = "1" ] \
    && pass 'nightly invokes importing oracle exactly once' \
    || fail 'nightly invokes importing oracle exactly once' "oracle_step_count=$oracle_step_count"
  grep -Fq "if: needs.check-repo.outputs.is-public-repo == 'true'" "$NIGHTLY_WORKFLOW" \
    && pass 'nightly importing oracle is public-mirror gated' \
    || fail 'nightly importing oracle is public-mirror gated'
  grep -Fq 'cargo build --release --locked --package flapjack-server --features vector-search' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly builds release server binary for importing oracle' \
    || fail 'nightly builds release server binary for importing oracle'
  grep -Fq 'FLAPJACK_BIN="$PWD/engine/target/release/flapjack"' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly sets FLAPJACK_BIN to release binary' \
    || fail 'nightly sets FLAPJACK_BIN to release binary'
  ! grep -Fq 'bash engine/tests/migration_import_contract.sh --expect-mode unavailable' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly importing oracle never uses unavailable mode' \
    || fail 'nightly importing oracle never uses unavailable mode'
  grep -Fq 'ALGOLIA_APP_ID: ${{ secrets.ALGOLIA_APP_ID }}' "$NIGHTLY_WORKFLOW" \
    && grep -Fq 'ALGOLIA_ADMIN_KEY: ${{ secrets.ALGOLIA_ADMIN_KEY }}' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly passes Algolia credentials from repo secrets' \
    || fail 'nightly passes Algolia credentials from repo secrets'
  grep -Fq 'SEEDED_OBJECT_COUNT=2' "$NIGHTLY_WORKFLOW" \
    && grep -Fq 'objectID' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly seeds a non-empty source fixture' \
    || fail 'nightly seeds a non-empty source fixture'
  grep -Fxq '          SEEDED_OBJECT_COUNT=2' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly scheduled import pins SEEDED_OBJECT_COUNT=2' \
    || fail 'nightly scheduled import pins SEEDED_OBJECT_COUNT=2'
  grep -Fq 'source_count' "$NIGHTLY_WORKFLOW" \
    && grep -Fq 'SEEDED_OBJECT_COUNT' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly verifies source fixture count before oracle' \
    || fail 'nightly verifies source fixture count before oracle'
  grep -Fq 'objects.imported=${SEEDED_OBJECT_COUNT}' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly asserts oracle reported seeded object count' \
    || fail 'nightly asserts oracle reported seeded object count'
  grep -Fq 'algolia_request DELETE "/1/indexes/${SOURCE_INDEX}"' "$NIGHTLY_WORKFLOW" \
    && ! grep -Fq 'deleteBy' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly cleanup deletes exactly created source index' \
    || fail 'nightly cleanup deletes exactly created source index'
  grep -Fq '/tmp/flapjack_migration_import_contract_evidence_*' "$NIGHTLY_WORKFLOW" \
    && pass 'nightly uploads preserved oracle evidence' \
    || fail 'nightly uploads preserved oracle evidence'

  job_count="$(grep -Ec '^  migration-scale-contract:' "$NIGHTLY_WORKFLOW" || true)"
  [ "$job_count" = "1" ] \
    && pass 'nightly has exactly one migration scale contract job' \
    || fail 'nightly has exactly one migration scale contract job' "job_count=$job_count"
  grep -Fq 'run_migration_scale_contract:' "$NIGHTLY_WORKFLOW" \
    && grep -Fq 'type: boolean' "$NIGHTLY_WORKFLOW" \
    && text_contains "$scale_job_body" "if: needs.check-repo.outputs.is-public-repo == 'true' && github.event_name == 'workflow_dispatch' && inputs.run_migration_scale_contract == true" \
    && pass 'nightly scale contract is public-mirror and dispatch-input gated' \
    || fail 'nightly scale contract is public-mirror and dispatch-input gated'
  text_contains "$scale_job_body" 'bash engine/tests/migration_import_contract.sh --expect-mode scale --two-point' \
    && pass 'nightly scale contract invokes the two-point scale oracle' \
    || fail 'nightly scale contract invokes the two-point scale oracle'
  text_contains "$scale_job_body" 'timeout --kill-after=60s 5400s bash engine/tests/migration_import_contract.sh --expect-mode scale --two-point' \
    && pass 'nightly scale contract is bounded to 5,400 seconds' \
    || fail 'nightly scale contract is bounded to 5,400 seconds'
  text_contains "$scale_job_body" 'MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$RUNNER_TEMP/migration-scale-contract-evidence"' \
    && pass 'nightly scale contract preserves evidence under RUNNER_TEMP' \
    || fail 'nightly scale contract preserves evidence under RUNNER_TEMP'
  text_contains "$scale_job_body" 'receipts=( "$MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT"/**/receipt.json )' \
    && text_contains "$scale_job_body" '[ "${#receipts[@]}" = "1" ]' \
    && pass 'nightly scale contract resolves exactly one receipt' \
    || fail 'nightly scale contract resolves exactly one receipt'
  text_contains "$scale_job_body" '.status == "pass"' \
    && text_contains "$scale_job_body" '.scale.mode == "two-point"' \
    && text_contains "$scale_job_body" '.scale.conditions == [2000,20000]' \
    && text_contains "$scale_job_body" 'all(.checks[]; .status != "fail")' \
    && pass 'nightly scale contract validates receipt contents' \
    || fail 'nightly scale contract validates receipt contents'
  text_contains "$scale_job_body" 'ALGOLIA_APP_ID: ${{ secrets.ALGOLIA_APP_ID }}' \
    && text_contains "$scale_job_body" 'ALGOLIA_ADMIN_KEY: ${{ secrets.ALGOLIA_ADMIN_KEY }}' \
    && pass 'nightly scale contract sources Algolia credentials only from repo secrets' \
    || fail 'nightly scale contract sources Algolia credentials only from repo secrets'
  text_contains "$scale_job_body" 'uses: actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093' \
    && text_contains "$scale_job_body" 'name: flapjack-server' \
    && pass 'nightly scale contract reuses build-server artifact' \
    || fail 'nightly scale contract reuses build-server artifact'
  scale_secret_write_count="$(grep -Fc '} >"$SECRET_FILE"' <<<"$scale_job_body" || true)"
  [ "$scale_secret_write_count" = "1" ] \
    && pass 'nightly scale contract writes the temporary secret file exactly once' \
    || fail 'nightly scale contract writes the temporary secret file exactly once' "count=$scale_secret_write_count"
  text_contains "$scale_job_body" 'name: migration-scale-contract-evidence' \
    && text_contains "$scale_job_body" 'migration-scale-contract-receipt.json' \
    && text_contains "$scale_job_body" 'migration-scale-contract-evidence' \
    && pass 'nightly scale contract uploads receipt and spool evidence' \
    || fail 'nightly scale contract uploads receipt and spool evidence'
  text_contains "$scale_job_body" 'dispatch-only isolation avoids repeated 20,000-document vendor work on the fast nightly path' \
    && pass 'nightly scale contract documents dispatch-only isolation' \
    || fail 'nightly scale contract documents dispatch-only isolation'
}

assert_testing_docs_scale_proof_contract() {
  local docs="$SCRIPT_DIR/../docs2/1_STRATEGY/TESTING.md"
  if [ -f "$docs" ] \
    && grep -Fq '### Migration scale proof' "$docs" \
    && grep -Fq 'unset FJ_SCALE_REUSE_FIXTURE' "$docs" \
    && grep -Fq '(cd engine && MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$PWD/../build/scale-evidence" gtimeout --kill-after=60s 5400s bash tests/migration_import_contract.sh --expect-mode scale --two-point --secret-file /Users/stuart/repos/gridl-dev/flapjack_dev/engine/.secret/.env.secret)' "$docs" \
    && grep -Fq 'jq -e' "$docs" \
    && grep -Fq 'receipt.json' "$docs" \
    && grep -Fq 'newly preserved receipt' "$docs" \
    && grep -Fq 'opt-in on public mirrors' "$docs" \
    && grep -Fq 'local command is authoritative acceptance for this dev lane' "$docs" \
    && grep -Fq 'no dev-repo Actions run should be triggered' "$docs"; then
    pass 'testing docs describe the local migration scale proof'
  else
    fail 'testing docs describe the local migration scale proof'
  fi
}

assert_debbie_public_sync_surface() {
  local debbie="$SCRIPT_DIR/../../.debbie.toml"
  if [ -f "$debbie" ] \
    && grep -Fxq 'path = "engine/tests/"' "$debbie" \
    && grep -Fxq 'path = "engine/docs2/1_STRATEGY/"' "$debbie" \
    && grep -Fxq 'path = ".github/"' "$debbie"; then
    pass 'debbie sync surface publishes migration test, docs, and workflow assets'
  else
    fail 'debbie sync surface publishes migration test, docs, and workflow assets'
  fi
}

assert_success_scenario() {
  local label="$1" scenario="$2" mode="$3" runtime out rc secret args data_dir
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  if [ "$mode" = "importing" ]; then
    secret="$(secret_file_for "$runtime")"
    args=(--expect-mode importing --secret-file "$secret" --source-index source_products --target-index target_products)
  else
    args=(--expect-mode unavailable)
  fi
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" "${args[@]}")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  if [ "$rc" = "0" ] \
    && [ -f "$runtime/state/binary_ran" ] \
    && [ "$(cat "$runtime/state/binary_ran")" = "$runtime/fake-flapjack" ] \
    && cmp -s <(printf 'POST /1/migrate-from-algolia\nGET /1/indexes\n') "$runtime/state/request_order.log" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && { [ "$mode" != "unavailable" ] || [ "$(cat "$runtime/state/node_id" 2>/dev/null)" = "migration-import-contract" ]; } \
    && { [ "$mode" != "unavailable" ] || [ "$(cat "$runtime/state/peers" 2>/dev/null)" = "migration-peer=http://10.0.0.2:7700" ]; } \
    && [ -z "$(extract_evidence_path "$out")" ]; then
    pass "$label"
  else
    fail "$label" "rc=$rc output=$(cat "$out")"
  fi
}

assert_success_evidence_scenario() {
  local runtime out rc secret args evidence data_dir evidence_root
  runtime="$WORK_DIR/importing_success_evidence"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  args=(--expect-mode importing --secret-file "$secret" --source-index source_products --target-index target_products)
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="importing_ok" \
    MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root" \
    bash "$ORACLE" "${args[@]}" >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  if [ "$rc" = "0" ] \
    && case "$evidence" in "$evidence_root"/*) true ;; *) false ;; esac \
    && evidence_has_contract_files "$evidence" \
    && evidence_receipt_has_run_metadata "$evidence" \
    && jq -e '.status == "pass" and .counts.source_count == 7 and .counts.target_count == 7' "$evidence/receipt.json" >/dev/null \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null; then
    rm -rf "$evidence_root"
    pass 'opt-in success evidence preserves receipt metadata and counts'
  else
    [ -z "$evidence_root" ] || rm -rf "$evidence_root"
    fail 'opt-in success evidence preserves receipt metadata and counts' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_verified_success_scenario() {
  local runtime out rc secret manifest evidence evidence_root
  runtime="$WORK_DIR/importing_verified_ok"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  manifest="$(verification_manifest_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="importing_verified_ok" \
    MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root" \
    bash "$ORACLE" --expect-mode importing --secret-file "$secret" \
      --source-index source_products --target-index target_products \
      --verification-manifest "$manifest" >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && evidence_has_contract_files "$evidence" \
    && jq -e '
      .status == "pass"
      and .counts == {source_count:7,target_count:7,synonym_count:1,rule_count:2}
      and (.checks | map(.name) | contains([
        "known_answers", "settings_effective", "synonym_effective",
        "rule_promotion_effective", "rule_hiding_effective",
        "conflict_target_immutable", "invalid_key_target_absent", "target_cleanup"
      ]))
    ' "$evidence/receipt.json" >/dev/null \
    && [ -f "$evidence/logs/source-manifest.json" ] \
    && [ ! -e "$runtime/state/active_target_products" ] \
    && [ ! -e "$runtime/state/active_target_products_conflict" ] \
    && [ ! -e "$runtime/state/active_target_products_invalid_key" ]; then
    rm -rf "$evidence_root"
    pass 'verified importing proves content, behavior, negative arms, and target cleanup'
  else
    [ -z "$evidence_root" ] || rm -rf "$evidence_root"
    fail 'verified importing proves content, behavior, negative arms, and target cleanup' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_success_scenario() {
  local runtime out rc secret evidence evidence_root
  runtime="$WORK_DIR/scale_ok"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="scale_ok" \
    MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root" \
    MIGRATION_IMPORT_CONTRACT_TEST_GENERATOR="$SCRIPT_DIR/common/generate_algolia_corpus.sh" \
    FJ_SCALE_FIXTURE_BIN="$runtime/bin/algolia_corpus_fixture.sh" \
    bash "$ORACLE" --expect-mode scale --secret-file "$secret" >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && [ "$(cat "$runtime/state/fixture_corpus_size" 2>/dev/null)" = "20000" ] \
    && [ "$(cat "$runtime/state/fixture_target_index" 2>/dev/null)" = "fj_scale_stub_target" ] \
    && [ -f "$runtime/state/fixture_cleanup_ledger" ] \
    && evidence_has_contract_files "$evidence" \
    && jq -e '
      .status == "pass"
      and .mode == "scale"
      and .source_index == "fj_scale_stub_source"
      and .target_index == "fj_scale_stub_target"
      and .scale.corpus_size == 20000
      and .scale.mode == "single-size"
      and .owned_resources.algolia_sources == ["fj_scale_stub_source"]
      and .counts.source_count == 20000
      and .counts.target_count == 20000
      and (.checks | map(.name) | contains([
        "scale_source_count", "scale_target_total", "scale_object_id_coverage",
        "scale_facets", "known_answers", "target_cleanup"
      ]))
    ' "$evidence/receipt.json" >/dev/null \
    && [ "$(cat "$runtime/state/fixture_source_count_index" 2>/dev/null)" = "fj_scale_stub_source" ] \
    && [ -f "$evidence/logs/source-manifest.json" ]; then
    rm -rf "$evidence_root"
    pass 'scale mode prepares fixture, assigns manifest before gate, records ledger, and cleans up'
  else
    [ -z "$evidence_root" ] || rm -rf "$evidence_root"
    fail 'scale mode prepares fixture, assigns manifest before gate, records ledger, and cleans up' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_two_point_success_scenario() {
  local scenario="${1:-scale_ok}"
  local label="${2:-scale two-point mode records three complete trials at 2,000 and 20,000}"
  local runtime out rc secret evidence evidence_root
  runtime="$WORK_DIR/scale_two_point_${scenario}"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="$scenario" \
    MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root" \
    MIGRATION_IMPORT_CONTRACT_TEST_GENERATOR="$SCRIPT_DIR/common/generate_algolia_corpus.sh" \
    FJ_SCALE_FIXTURE_BIN="$runtime/bin/algolia_corpus_fixture.sh" \
    bash "$ORACLE" --expect-mode scale --two-point --secret-file "$secret" >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && evidence_has_contract_files "$evidence" \
    && jq -e '
      .status == "pass"
      and .scale.mode == "two-point"
      and .scale.conditions == [2000,20000]
      and .scale.trials_per_condition == 3
      and .scale.two_point_observed_rewrite_ratio == 70
      and .scale.two_point_ratio_status == "pass"
      and ([.scale.conditions_observed[].n] | sort) == [2000,20000]
      and (.owned_resources.targets | all(type == "string" and length > 0))
      and (.owned_resources.algolia_sources == ["fj_scale_stub_source"])
      and all(.scale.conditions_observed[]; .source_index == "fj_scale_stub_source")
      and all(.scale.conditions_observed[]; (.trials | length) == 3)
      and all(.scale.conditions_observed[].trials[];
        .distinct_sizes_observed >= .minimum_distinct_sizes_required
        and .minimum_distinct_sizes_required <= .expected_page_count)
      and all(.scale.conditions_observed[].trials[]; .manifest.completed_objects.generation == .expected_page_count)
      and all(.scale.conditions_observed[].trials[];
        .manifest.completed_objects.count == .condition_n
        and .manifest.completed_objects.length == .final_sidecar_bytes
      )
      and (.checks | map(select(.name | startswith("scale_trial_"))) | length) == 6
      and (.checks[] | select(.name == "scale_rewrite_growth_ceiling" and .status == "pass"))
    ' "$evidence/receipt.json" >/dev/null \
    && evidence_scale_job_archives_are_relative_and_resolve "$evidence" \
    && evidence_scale_sampled_archives_resolve "$evidence" \
    && { [ "$scenario" != "scale_manifest_deleted_snapshot" ] \
      || evidence_scale_deleted_archives_preserve_authentic_state "$evidence"; }; then
    rm -rf "$evidence_root"
    pass "$label"
  else
    [ -z "$evidence_root" ] || rm -rf "$evidence_root"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_two_point_failure_scenario() {
  local label="$1" scenario="$2" expected_text="$3" expected_check="$4" runtime out rc secret evidence
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && grep -Fq "$expected_text" "$out" \
    && jq -e --arg check "$expected_check" '.checks[] | select(.name == $check and .status == "fail")' "$evidence/receipt.json" >/dev/null \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence expected=$expected_text output=$(cat "$out")"
  fi
}

assert_scale_two_point_wall_clock_failure() {
  local runtime out rc secret evidence
  runtime="$WORK_DIR/scale_wall_clock_over_budget"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  export MIGRATION_IMPORT_CONTRACT_REQUEST_BUDGET_MS=50
  rc="$(run_oracle_with_stub scale_wall_clock_over_budget "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  unset MIGRATION_IMPORT_CONTRACT_REQUEST_BUDGET_MS
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && grep -Fq "scale trial exceeded request wall-clock budget" "$out" \
    && jq -e '.checks[] | select(.name == "scale_wall_clock_budget" and .status == "fail")' "$evidence/receipt.json" >/dev/null; then
    rm -rf "$evidence"
    pass 'scale two-point rejects a trial over the wall-clock budget'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'scale two-point rejects a trial over the wall-clock budget' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_two_point_rejects_excessive_request_budget() {
  local runtime out rc secret
  runtime="$WORK_DIR/scale_two_point_request_budget_too_high"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  export MIGRATION_IMPORT_CONTRACT_REQUEST_BUDGET_MS=900001
  rc="$(run_oracle_with_stub scale_ok "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  unset MIGRATION_IMPORT_CONTRACT_REQUEST_BUDGET_MS
  if [ "$rc" != "0" ] \
    && grep -Fq 'two-point request budget must be an integer from 1 through 900000 milliseconds' "$out"; then
    pass 'scale two-point rejects inherited request budgets above the pinned ceiling'
  else
    fail 'scale two-point rejects inherited request budgets above the pinned ceiling' "rc=$rc output=$(cat "$out")"
  fi
}

assert_scale_two_point_rejects_under_minimum_trial_count() {
  local runtime out rc secret
  runtime="$WORK_DIR/scale_two_point_trial_count_too_low"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  export MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT=1
  rc="$(run_oracle_with_stub scale_ok "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  unset MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT
  if [ "$rc" != "0" ] \
    && grep -Fq 'two-point trial count must be an integer of at least 3' "$out"; then
    pass 'scale two-point rejects a trial count below three'
  else
    fail 'scale two-point rejects a trial count below three' "rc=$rc output=$(cat "$out")"
  fi
}

assert_scale_two_point_receipt_reports_accepted_trial_count() {
  local runtime out rc secret evidence evidence_root
  runtime="$WORK_DIR/scale_two_point_trial_count_four"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  # A clean pass only preserves evidence when an explicit root is set, so the
  # receipt this assertion inspects would not exist without one.
  export MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root"
  export MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT=4
  rc="$(run_oracle_with_stub scale_ok "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  unset MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT
  unset MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && jq -e '
      .scale.trials_per_condition == 4
      and all(.scale.conditions_observed[]; (.trials | length) == 4)
      and (.checks | map(select(.name | startswith("scale_trial_"))) | length) == 8
    ' "$evidence/receipt.json" >/dev/null; then
    rm -rf "$evidence_root"
    pass 'scale two-point receipt reports the accepted trial count'
  else
    rm -rf "$evidence_root"
    fail 'scale two-point receipt reports the accepted trial count' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_two_point_growth_ceiling_failure() {
  local runtime out rc secret evidence
  runtime="$WORK_DIR/scale_growth_breach"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  export MIGRATION_IMPORT_CONTRACT_REWRITE_GROWTH_CEILING=1000
  rc="$(run_oracle_with_stub scale_growth_breach "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  unset MIGRATION_IMPORT_CONTRACT_REWRITE_GROWTH_CEILING
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && grep -Fq 'observed ratio exceeded ceiling' "$out" \
    && jq -e '
      .scale.two_point_rewrite_growth_ceiling == 75
      and .scale.two_point_ratio_status == "breach"
      and (.checks[] | select(.name == "scale_rewrite_growth_ceiling" and .status == "fail"))
    ' "$evidence/receipt.json" >/dev/null; then
    rm -rf "$evidence"
    pass 'scale two-point fails rewrite growth ceiling breach'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'scale two-point fails rewrite growth ceiling breach' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_two_point_even_trial_medians() {
  local runtime out rc secret evidence evidence_root
  runtime="$WORK_DIR/scale_median_four_trials"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  export MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root"
  export MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT=4
  rc="$(run_oracle_with_stub scale_median_four_trials "$out" "$runtime" \
    --expect-mode scale --two-point --secret-file "$secret")"
  unset MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT
  unset MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && jq -e '
      def median: sort as $v
        | if ($v | length) % 2 == 1 then $v[(length / 2 | floor)]
          else (($v[(length / 2) - 1] + $v[(length / 2)]) / 2)
          end;
      .scale.conditions_observed
      | all(.[]; (.trials | length) == 4
        and .summary.wall_clock_milliseconds.median == ([.trials[].wall_clock_milliseconds] | median)
        and .summary.peak_rss_kb.median == ([.trials[].peak_rss_kb] | median))
    ' "$evidence/receipt.json" >/dev/null; then
    rm -rf "$evidence_root"
    pass 'scale two-point computes mathematical medians for four accepted trials'
  else
    rm -rf "$evidence_root"
    fail 'scale two-point computes mathematical medians for four accepted trials' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_scale_failure_scenario() {
  local label="$1" scenario="$2" expected_text="$3" expected_extra="${4:-}" runtime out rc secret evidence
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" \
    --expect-mode scale --secret-file "$secret")"
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && grep -Fq "$expected_text" "$out" \
    && { [ -z "$expected_extra" ] || grep -Fq "$expected_extra" "$out"; } \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence expected=$expected_text output=$(cat "$out")"
  fi
}

assert_scale_init_failure_cleanup() {
  local runtime out rc secret evidence
  runtime="$WORK_DIR/scale_init_manifest_failure"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub scale_init_manifest_failure "$out" "$runtime" \
    --expect-mode scale --secret-file "$secret")"
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && [ "$(cat "$runtime/state/fixture_cleanup_ledger" 2>/dev/null)" = "$(cat "$runtime/state/fixture_ledger_path" 2>/dev/null)" ]; then
    [ -z "$evidence" ] || rm -rf "$evidence"
    pass 'scale init failure cleans the prepared Algolia fixture'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'scale init failure cleans the prepared Algolia fixture' "rc=$rc output=$(cat "$out")"
  fi
}

assert_scale_cleanup_failure_receipt() {
  local runtime out rc secret evidence
  runtime="$WORK_DIR/scale_cleanup_failure"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="scale_cleanup_failure" \
    MIGRATION_IMPORT_CONTRACT_TEST_GENERATOR="$SCRIPT_DIR/common/generate_algolia_corpus.sh" \
    FJ_SCALE_FIXTURE_BIN="$runtime/bin/algolia_corpus_fixture.sh" \
    bash "$ORACLE" --expect-mode scale --secret-file "$secret" >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && jq -e '
      .status == "failed"
      and (.checks[] | select(.name == "scale_source_cleanup" and .status == "fail"))
    ' "$evidence/receipt.json" >/dev/null; then
    rm -rf "$evidence"
    pass 'scale cleanup failure downgrades the persisted receipt'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'scale cleanup failure downgrades the persisted receipt' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_verified_failure_scenario() {
  local label="$1" scenario="$2" runtime out rc secret manifest evidence
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  manifest="$(verification_manifest_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" \
    --expect-mode importing --secret-file "$secret" --source-index source_products \
    --target-index target_products --verification-manifest "$manifest")"
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_replica_success_scenario() {
  local runtime out rc secret evidence evidence_root
  runtime="$WORK_DIR/importing_replicas_ok"
  out="$runtime.out"
  evidence_root="$runtime/evidence"
  secret="$(secret_file_for "$runtime")"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="importing_replicas_ok" \
    MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$evidence_root" \
    bash "$ORACLE" --expect-mode importing --scenario replicas --secret-file "$secret" \
      --source-index fj_replica_source_products --target-index fj_replica_target_products >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" = "0" ] \
    && evidence_has_contract_files "$evidence" \
    && jq -e '
      .status == "pass"
      and .scenario == "replicas"
      and (.owned_resources.targets | index("fj_replica_source_products_relevance")) != null
      and (.owned_resources.targets | index("virtual(fj_replica_source_products_relevance)")) == null
      and .replica_sequences == {
        expected: {
          primary: ["replica-001","replica-002","replica-003"],
          virtual: ["replica-002","replica-003","replica-001"],
          standard: ["replica-003","replica-001","replica-002"]
        },
        observed: {
          primary: ["replica-001","replica-002","replica-003"],
          virtual: ["replica-002","replica-003","replica-001"],
          standard: ["replica-003","replica-001","replica-002"]
        }
      }
      and (.checks | map(.name) | contains([
        "replica_primary_order", "replica_virtual_order", "replica_standard_order",
        "replica_public_list", "replica_hit_sets", "replica_virtual_sidecar",
        "replica_standard_sidecar", "replica_cleanup"
      ]))
    ' "$evidence/receipt.json" >/dev/null \
    && jq -e '[.warnings[].code] == ["PersistedNoBehaviorSetting", "ReplicaExhaustiveSortApproximated", "ReplicaRelevancyStrictnessSemanticMismatch", "ReadOnlySourceField"]' \
      "$evidence/logs/migration-response.json" >/dev/null \
    && jq -e '. == {"primary":"fj_replica_target_products","customRanking":["asc(price)"],"relevancyStrictness":80}' \
      "$evidence/logs/replica_virtual-settings-proof.json" >/dev/null \
    && jq -e '. == {"primary":"fj_replica_target_products","ranking":["typo","geo","words","filters","proximity","attribute","exact"],"customRanking":["desc(standard_rank)"]}' \
      "$evidence/logs/replica_standard-settings-proof.json" >/dev/null \
    && jq -s -e '[.[] | select(.replicas? != null)] == [{"customRanking":["desc(primary_rank)"],"replicas":["virtual(fj_replica_source_products_relevance)","fj_replica_source_products_standard_rank"]}]' \
      "$runtime/state/request_bodies.log" >/dev/null \
    && cmp -s <(printf 'fj_replica_source_products\nfj_replica_source_products_relevance\nfj_replica_source_products_standard_rank\n') "$runtime/state/settings_indices.log" \
    && cmp -s <(printf 'fj_replica_source_products\nfj_replica_source_products_relevance\nfj_replica_source_products_standard_rank\n') "$runtime/state/deleted_indices.log" \
    && ! grep -Fq 'virtual(' "$runtime/state/source_api_indices.log"; then
    rm -rf "$evidence_root"
    pass 'replica scenario proves public order, sidecars, receipt, and exact cleanup'
  else
    [ -z "$evidence_root" ] || rm -rf "$evidence_root"
    fail 'replica scenario proves public order, sidecars, receipt, and exact cleanup' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_replica_failure_scenario() {
  local label="$1" scenario="$2" expected_diagnostic="${3:-}" runtime out rc secret evidence
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" \
    --expect-mode importing --scenario replicas --secret-file "$secret" \
    --source-index fj_replica_source_products --target-index fj_replica_target_products)"
  evidence="$(extract_evidence_path "$out")"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && { [ -z "$expected_diagnostic" ] || grep -Fq "$expected_diagnostic" "$out"; } \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_failure_scenario() {
  local label="$1" scenario="$2" mode="$3" runtime out rc secret args evidence data_dir server_pid
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  if [ "$mode" = "importing" ]; then
    secret="$(secret_file_for "$runtime")"
    args=(--expect-mode importing --secret-file "$secret" --source-index source_products --target-index target_products)
  else
    args=(--expect-mode unavailable)
  fi
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" "${args[@]}")"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  server_pid="$(cat "$runtime/state/server_pid" 2>/dev/null || true)"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && { [ -z "$server_pid" ] || ! kill -0 "$server_pid" 2>/dev/null; } \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out" "$evidence"/* "$evidence"/logs/* 2>/dev/null \
    && { [ "$scenario" != "importing_returns_503" ] || jq -e '.checks[] | select(.name == "target_absent_after_failed_import" and .status == "pass")' "$evidence/receipt.json" >/dev/null; }; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_argument_contract() {
  local label="$1" expected_rc_relation="$2" runtime out rc
  shift 2
  runtime="$WORK_DIR/args-${label//[^A-Za-z0-9_]/_}"
  out="$runtime.out"
  rc="$(run_oracle_with_stub unavailable_ok "$out" "$runtime" "$@")"
  case "$expected_rc_relation" in
    zero)
      [ "$rc" = "0" ] && pass "$label" || fail "$label" "rc=$rc output=$(cat "$out")"
      ;;
    nonzero)
      if [ "$rc" != "0" ] && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
        pass "$label"
      else
        fail "$label" "rc=$rc output=$(cat "$out")"
      fi
      ;;
  esac
}

assert_non_scale_corpus_size_rejected() {
  local mode="$1" runtime out rc secret args
  runtime="$WORK_DIR/args-${mode}_explicit_default_corpus_size"
  out="$runtime.out"
  if [ "$mode" = "importing" ]; then
    secret="$(secret_file_for "$runtime")"
    args=(--expect-mode importing --secret-file "$secret" --source-index source --target-index target --corpus-size 20000)
  else
    args=(--expect-mode unavailable --corpus-size 20000)
  fi
  rc="$(run_oracle_with_stub unavailable_ok "$out" "$runtime" "${args[@]}")"
  if [ "$rc" != "0" ] \
    && grep -Fq -- "--corpus-size is not allowed in ${mode} mode" "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass "${mode} refuses explicit default corpus-size"
  else
    fail "${mode} refuses explicit default corpus-size" "rc=$rc output=$(cat "$out")"
  fi
}

assert_scale_argument_contract() {
  local label="$1" expected_rc_relation="$2" runtime out rc secret
  shift 2
  runtime="$WORK_DIR/args-${label//[^A-Za-z0-9_]/_}"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub scale_ok "$out" "$runtime" \
    --expect-mode scale --secret-file "$secret" "$@")"
  case "$expected_rc_relation" in
    zero)
      [ "$rc" = "0" ] && pass "$label" || fail "$label" "rc=$rc output=$(cat "$out")"
      ;;
    nonzero)
      if [ "$rc" != "0" ] && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
        pass "$label"
      else
        fail "$label" "rc=$rc output=$(cat "$out")"
      fi
      ;;
  esac
}

assert_replica_argument_acceptance() {
  local runtime out rc secret
  runtime="$WORK_DIR/args-scenario-replicas"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub importing_replicas_ok "$out" "$runtime" \
    --expect-mode importing --scenario replicas --secret-file "$secret" \
    --source-index fj_replica_source --target-index fj_replica_target)"
  if [ "$rc" = "0" ]; then
    pass 'scenario replicas is accepted for importing mode'
  else
    fail 'scenario replicas is accepted for importing mode' "rc=$rc output=$(cat "$out")"
  fi
}

assert_cancel_argument_acceptance() {
  local runtime out rc secret
  runtime="$WORK_DIR/args-scenario-cancel"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub cancel_ok "$out" "$runtime" \
    --expect-mode importing --scenario cancel --secret-file "$secret" \
    --source-index fj_cancel_source --target-index fj_cancel_target)"
  if [ "$rc" = "0" ]; then
    pass 'scenario cancel is accepted for importing mode'
  else
    fail 'scenario cancel is accepted for importing mode' "rc=$rc output=$(cat "$out")"
  fi
}

assert_signal_scenario() {
  local label="$1" scenario="$2" expected_rc="$3" runtime out rc evidence data_dir server_pid
  runtime="$WORK_DIR/$scenario"
  out="$runtime.out"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" --expect-mode unavailable)"
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  server_pid="$(cat "$runtime/state/server_pid" 2>/dev/null || true)"
  if [ "$rc" = "$expected_rc" ] \
    && evidence_has_contract_files "$evidence" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ] \
    && { [ -z "$server_pid" ] || ! kill -0 "$server_pid" 2>/dev/null; }; then
    rm -rf "$evidence"
    pass "$label"
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail "$label" "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_cleanup_failure_scenario() {
  local runtime out rc evidence data_dir
  runtime="$WORK_DIR/cleanup_failure"
  out="$runtime.out"
  write_fake_runtime "$runtime"
  set +e
  PATH="$runtime/bin:$PATH" \
    FLAPJACK_BIN="$runtime/fake-flapjack" \
    MIGRATION_IMPORT_CONTRACT_STUB_DIR="$runtime/state" \
    MIGRATION_IMPORT_CONTRACT_SCENARIO="cleanup_failure" \
    MIGRATION_IMPORT_CONTRACT_SIMULATE_CLEANUP_FAILURE=1 \
    bash "$ORACLE" --expect-mode unavailable >"$out" 2>&1
  rc=$?
  set -e
  [ ! -f "$runtime/state/server_pid" ] || OWNED_PIDS+=("$(cat "$runtime/state/server_pid")")
  evidence="$(extract_evidence_path "$out")"
  data_dir="$(cat "$runtime/state/data_dir" 2>/dev/null || true)"
  if [ "$rc" != "0" ] \
    && evidence_has_contract_files "$evidence" \
    && [ -n "$data_dir" ] \
    && [ ! -e "$data_dir" ]; then
    rm -rf "$evidence"
    pass 'simulated cleanup failure preserves evidence and exits nonzero'
  else
    [ -z "$evidence" ] || rm -rf "$evidence"
    fail 'simulated cleanup failure preserves evidence and exits nonzero' "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

# The async scenario must submit to the async route, walk a monotonic phase
# sequence, and refuse to believe a terminal success until the destination is
# actually present with the exact seeded content.
assert_async_positive_control() {
  local runtime out rc secret evidence target vendor_request_log
  runtime="$WORK_DIR/async-ok"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$runtime/evidence" \
    run_oracle_with_stub async_ok "$out" "$runtime" --scenario async_job --secret-file "$secret")"
  evidence="$(extract_evidence_path "$out")"
  target="$(cat "$runtime/state/target_index" 2>/dev/null || true)"
  vendor_request_log="$runtime/state/vendor_request_order.log"
  if [ "$rc" = "0" ] \
    && [ -n "$evidence" ] \
    && [ -n "$target" ] \
    && jq -e '
      (.mode == "async_job")
      and (.source_index | startswith("fj_async_"))
      and (.target_index | startswith("fj_async_"))
      and ([.checks[] | select(.status != "pass")] | length == 0)
      and ([.checks[] | select(.name == "async_source_seeded")] | length == 1)
      and ([.checks[] | select(.name == "async_submission")] | length == 1)
      and ([.checks[] | select(.name == "async_target_documents")] | length == 1)
      and ([.checks[] | select(.name == "async_fixture_cleanup")] | length == 1)
      and ([.checks[] | select(.name == "async_export_progress")] | length == 0)
      and ([.checks[] | select(.name == "async_phase_sequence") | .detail]
            == ["exporting preparing staging activating"])
    ' "$evidence/receipt.json" >/dev/null \
    && jq -e '.completed == 3 and .total == 3' \
      "$evidence/logs/async-export-progress.json" >/dev/null \
    && ! grep -Fq "DELETE /1/indexes/${target}" "$vendor_request_log" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    rm -rf "$evidence"
    pass 'async job control submits, walks monotonic phases, and verifies the target'
  else
    fail 'async job control submits, walks monotonic phases, and verifies the target' \
      "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_async_failure_scenario() {
  local label="$1" scenario="$2" expected_message="$3" runtime out rc secret
  runtime="$WORK_DIR/async-${scenario}"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" --scenario async_job --secret-file "$secret")"
  if [ "$rc" != "0" ] \
    && grep -Fq -- "$expected_message" "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass "$label"
  else
    fail "$label" "rc=$rc output=$(cat "$out")"
  fi
}

# Prefix ownership is not proof of abandonment: a concurrent async run's index is
# also fj_async_ prefixed. This proves the sweep deletes only this run's exact
# names and provably stale leftovers.
assert_async_preflight_sweep_scope() {
  local runtime out rc secret deleted
  runtime="$WORK_DIR/async-preflight-sweep"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub async_preflight_sweep "$out" "$runtime" \
    --scenario async_job --secret-file "$secret" \
    --source-index fj_async_meta_source --target-index fj_async_meta_target)"
  deleted="$runtime/state/deleted_indices.log"
  if [ "$rc" = "0" ] \
    && [ -f "$deleted" ] \
    && grep -Fxq 'fj_async_meta_source' "$deleted" \
    && grep -Fxq 'fj_async_meta_target' "$deleted" \
    && grep -Fxq 'fj_async_stale_leftover' "$deleted" \
    && ! grep -Fxq 'fj_async_recent_leftover' "$deleted" \
    && ! grep -Fxq 'fj_async_no_timestamp' "$deleted" \
    && ! grep -Fxq 'fj_scale_stale_source' "$deleted" \
    && ! grep -Fxq 'keep_me' "$deleted" \
    && grep -Fq 'skipped=fj_async_recent_leftover fj_async_no_timestamp' "$out"; then
    pass 'async preflight sweeps owned and stale names but skips unowned recent leftovers'
  else
    fail 'async preflight sweeps owned and stale names but skips unowned recent leftovers' \
      "rc=$rc deleted=$(cat "$deleted" 2>/dev/null || true) output=$(cat "$out")"
  fi
}

# A stale target must not survive a failed preflight DELETE and reach migration
# submission. Content is deliberately not asserted here: the stub authors that
# state before the driver runs, so comparing it with another test constant would
# not exercise any behavior in the driver.
assert_async_delete_failure() {
  local label="$1" scenario="$2" runtime out rc secret target request_log
  runtime="$WORK_DIR/${scenario}"
  out="$runtime.out"
  target="fj_async_meta_target"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" \
    --scenario async_job --secret-file "$secret" \
    --source-index fj_async_meta_source --target-index "$target")"
  request_log="$runtime/state/request_order.log"
  if [ "$rc" != "0" ] \
    && grep -Fq "async preflight failed to delete ${target}" "$out" \
    && [ -f "$runtime/state/vendor_active_${target}" ] \
    && ! grep -Fq 'POST /1/migrations/algolia' "$request_log" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass "$label"
  else
    fail "$label" \
      "rc=$rc target_present=$([ -f "$runtime/state/vendor_active_${target}" ] && printf yes || printf no) output=$(cat "$out")"
  fi
}

assert_async_cleanup_uses_write_host_truth() {
  local runtime out rc secret source deleted vendor_request_log
  runtime="$WORK_DIR/async-cleanup-stale-dsn-listing"
  out="$runtime.out"
  source="fj_async_meta_source"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub async_cleanup_stale_dsn_listing "$out" "$runtime" \
    --scenario async_job --secret-file "$secret" \
    --source-index "$source" --target-index fj_async_meta_target)"
  deleted="$runtime/state/deleted_indices.log"
  vendor_request_log="$runtime/state/vendor_request_order.log"
  if [ "$rc" = "0" ] \
    && [ ! -f "$runtime/state/vendor_active_${source}" ] \
    && grep -Fxq "$source" "$deleted" \
    && grep -Fq "DELETE /1/indexes/${source}" "$vendor_request_log" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass 'async cleanup deletes owned source when DSN listing is stale'
  else
    fail 'async cleanup deletes owned source when DSN listing is stale' \
      "rc=$rc source_present=$([ -f "$runtime/state/vendor_active_${source}" ] && printf yes || printf no) deleted=$(cat "$deleted" 2>/dev/null || true) output=$(cat "$out")"
  fi
}

# Proves --scenario is parsed before parse_args' unknown-argument catch-all: an
# unparsed flag would report "unknown argument" instead of the async-mode message.
assert_async_argument_message() {
  local label="$1" expected_message="$2" runtime out rc
  shift 2
  runtime="$WORK_DIR/args-${label//[^A-Za-z0-9_]/_}"
  out="$runtime.out"
  rc="$(run_oracle_with_stub async_ok "$out" "$runtime" "$@")"
  if [ "$rc" != "0" ] \
    && grep -Fq -- "$expected_message" "$out" \
    && ! grep -Fq -- 'unknown argument: --scenario' "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass "$label"
  else
    fail "$label" "rc=$rc output=$(cat "$out")"
  fi
}

# A flag and its environment variable that disagree is a naming ambiguity, not a
# precedence question: the driver must refuse rather than silently pick one.
assert_async_index_conflict_rejected() {
  local runtime out rc secret
  runtime="$WORK_DIR/args-async_index_conflict"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(FJ_ASYNC_SOURCE_INDEX=fj_async_from_environment \
    run_oracle_with_stub async_ok "$out" "$runtime" \
    --scenario async_job --secret-file "$secret" --source-index fj_async_from_flag)"
  if [ "$rc" != "0" ] \
    && grep -Fq -- '--source-index and FJ_ASYNC_SOURCE_INDEX disagree' "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass 'async index flag and environment conflict fails'
  else
    fail 'async index flag and environment conflict fails' "rc=$rc output=$(cat "$out")"
  fi
}

assert_cancel_positive_control() {
  local runtime out rc secret evidence deleted
  runtime="$WORK_DIR/cancel-ok"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$runtime/evidence" \
    run_oracle_with_stub cancel_ok "$out" "$runtime" \
      --expect-mode importing --scenario cancel --secret-file "$secret" \
      --source-index fj_cancel_meta_source --target-index fj_cancel_meta_target)"
  evidence="$(extract_evidence_path "$out")"
  deleted="$runtime/state/deleted_indices.log"
  if [ "$rc" = "0" ] \
    && [ -n "$evidence" ] \
    && jq -e '
      (.mode == "importing")
      and (.scenario == "cancel")
      and (.source_index == "fj_cancel_meta_source")
      and (.target_index == "fj_cancel_meta_target")
      and (.cancel.corpus_size == 2500)
      and (.cancel.browse_page_size == 1000)
      and (.cancel.precommit.terminal_status.disposition == "cancelled")
      and (.cancel.postcommit.terminal_status.disposition == "succeeded")
      and ([.checks[] | select(.name == "cancel_too_late" and .status == "pass")] | length == 1)
      and ([.checks[] | select(.name == "cancel_precommit_target_unchanged" and .status == "pass")] | length == 1)
      and ([.checks[] | select(.name == "cancel_postcommit_target_documents" and .status == "pass")] | length == 1)
      and ([.checks[] | select(.status != "pass")] | length == 0)
      and (.counts.source_count == 2500 and .counts.target_count == 2500)
    ' "$evidence/receipt.json" >/dev/null \
    && [ -f "$deleted" ] \
    && grep -Fxq 'fj_cancel_meta_source' "$deleted" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    rm -rf "$evidence"
    pass 'cancel scenario proves pre-commit cancel and post-commit cancel_too_late'
  else
    fail 'cancel scenario proves pre-commit cancel and post-commit cancel_too_late' \
      "rc=$rc evidence=$evidence output=$(cat "$out")"
  fi
}

assert_cancel_preflight_sweep_scope() {
  local runtime out rc secret deleted
  runtime="$WORK_DIR/cancel-preflight-sweep"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub cancel_preflight_sweep "$out" "$runtime" \
    --expect-mode importing --scenario cancel --secret-file "$secret" \
    --source-index fj_cancel_meta_source --target-index fj_cancel_meta_target)"
  deleted="$runtime/state/deleted_indices.log"
  if [ "$rc" = "0" ] \
    && [ -f "$deleted" ] \
    && grep -Fxq 'fj_cancel_stale_leftover' "$deleted" \
    && ! grep -Fxq 'fj_cancel_recent_leftover' "$deleted" \
    && ! grep -Fxq 'fj_async_stale_leftover' "$deleted" \
    && grep -Fq 'skipped=fj_cancel_recent_leftover' "$out"; then
    pass 'cancel preflight sweeps owned and stale names but skips other prefixes'
  else
    fail 'cancel preflight sweeps owned and stale names but skips other prefixes' \
      "rc=$rc deleted=$(cat "$deleted" 2>/dev/null || true) output=$(cat "$out")"
  fi
}

assert_cancel_failure_scenario() {
  local label="$1" scenario="$2" expected_message="$3" runtime out rc secret
  runtime="$WORK_DIR/${scenario}"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub "$scenario" "$out" "$runtime" \
    --expect-mode importing --scenario cancel --secret-file "$secret" \
    --source-index fj_cancel_meta_source --target-index fj_cancel_meta_target)"
  if [ "$rc" != "0" ] \
    && grep -Fq "$expected_message" "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass "$label"
  else
    fail "$label" "rc=$rc output=$(cat "$out")"
  fi
}

assert_cancel_sentinel_field_order() {
  local runtime out rc secret
  runtime="$WORK_DIR/cancel-sentinel-field-order"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub cancel_sentinel_field_order "$out" "$runtime" \
    --expect-mode importing --scenario cancel --secret-file "$secret" \
    --source-index fj_cancel_meta_source --target-index fj_cancel_meta_target)"
  if [ "$rc" = "0" ] && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass 'cancel scenario accepts unchanged sentinel with reordered JSON fields'
  else
    fail 'cancel scenario accepts unchanged sentinel with reordered JSON fields' \
      "rc=$rc output=$(cat "$out")"
  fi
}

assert_cancel_postcommit_pagination() {
  local runtime out rc secret request_log
  runtime="$WORK_DIR/cancel-postcommit-pagination"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(run_oracle_with_stub cancel_ok "$out" "$runtime" \
    --expect-mode importing --scenario cancel --secret-file "$secret" \
    --source-index fj_cancel_meta_source --target-index fj_cancel_meta_target)"
  request_log="$runtime/state/request_bodies.log"
  if [ "$rc" = "0" ] \
    && jq -s -e '
      ([.[] | select(.browse == true and .hitsPerPage == 1000) | .ordinal] | sort) == [0, 1, 2]
      and ([.[] | select(.browse == true and (has("cursor") | not))] | length) == 1
      and ([.[] | select(.browse == true and (.cursor | type) == "string")] | length) == 2
      and all(.[]; (.hitsPerPage // 0) <= 1000)
    ' "$request_log" >/dev/null \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass 'cancel postcommit verification paginates target reads'
  else
    fail 'cancel postcommit verification paginates target reads' \
      "rc=$rc requests=$(cat "$request_log" 2>/dev/null || true) output=$(cat "$out")"
  fi
}

assert_cancel_index_conflict_rejected() {
  local runtime out rc secret
  runtime="$WORK_DIR/args-cancel_index_conflict"
  out="$runtime.out"
  secret="$(secret_file_for "$runtime")"
  rc="$(FJ_CANCEL_SOURCE_INDEX=fj_cancel_from_environment \
    run_oracle_with_stub cancel_ok "$out" "$runtime" \
    --expect-mode importing --scenario cancel --secret-file "$secret" \
    --source-index fj_cancel_from_flag --target-index fj_cancel_target)"
  if [ "$rc" != "0" ] \
    && grep -Fq -- '--source-index and FJ_CANCEL_SOURCE_INDEX disagree' "$out" \
    && ! grep -Eq 'ADMIN_SECRET_CANARY|APPID_CANARY' "$out"; then
    pass 'cancel index flag and environment conflict fails'
  else
    fail 'cancel index flag and environment conflict fails' "rc=$rc output=$(cat "$out")"
  fi
}

assert_static_contract() {
  if [ -f "$ORACLE" ]; then
    pass 'oracle file exists'
  else
    fail 'oracle file exists' "$ORACLE"
    return
  fi
  [ -x "$ORACLE" ] && pass 'oracle is executable' || fail 'oracle is executable'
  grep -Fq 'set -euo pipefail' "$ORACLE" && pass 'oracle enables strict mode' || fail 'oracle enables strict mode'
  grep -Fq 'load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY' "$ORACLE" \
    && pass 'oracle loads only required Algolia secrets in importing mode' \
    || fail 'oracle loads only required Algolia secrets in importing mode'
  grep -Fq '/1/indexes"' "$ORACLE" \
    && ! grep -Fq '/1/indexes/${TARGET_INDEX}' "$ORACLE" \
    && pass 'oracle uses list-indices metadata surface, not single-index search route' \
    || fail 'oracle uses list-indices metadata surface, not single-index search route'
  scale_sampler_hot_path_is_cheap \
    && pass 'scale sampler hot path avoids manifest validation and size subprocesses' \
    || fail 'scale sampler hot path avoids manifest validation and size subprocesses'
  assert_nightly_importing_contract
  assert_scale_workflow_mutation_rejected \
    'nightly scale contract rejects missing scoped credential environment' \
    remove_scale_secret_env_from_workflow
  assert_scale_workflow_mutation_rejected \
    'nightly scale contract rejects missing scoped server artifact download' \
    remove_scale_artifact_download_from_workflow
}

main() {
  echo 'migration_import_contract oracle meta-test'
  assert_static_contract
  assert_generator_contract
  assert_fixture_source_count_contract
  assert_fixture_reuse_contract
  assert_fixture_prefix_preflight_cleans_scale_residue
  assert_fixture_prepare_waits_for_algolia_tasks
  assert_fixture_selftest_failure_cleans_source
  assert_fixture_selftest_rejects_reuse_fixture

  assert_success_scenario 'unavailable positive control passes' unavailable_ok unavailable
  assert_success_scenario 'importing positive control passes' importing_ok importing
  assert_success_evidence_scenario
  assert_verified_success_scenario
  assert_scale_success_scenario
  assert_scale_two_point_success_scenario
  assert_scale_two_point_success_scenario scale_manifest_deleted_snapshot \
    'scale two-point preserves completed manifest after deleted snapshot'
  assert_scale_init_failure_cleanup
  assert_scale_cleanup_failure_receipt

  assert_scale_failure_scenario 'scale rejects target total drift' scale_bad_total_count 'scale target total did not equal live source count'
  assert_scale_failure_scenario 'scale rejects duplicate objectIDs' scale_duplicate_object_ids 'scale target returned duplicate objectID values'
  assert_scale_failure_scenario 'scale rejects a missing final-page object' scale_missing_final_page 'scale target did not contain expected final objectID'
  assert_scale_failure_scenario 'scale rejects facet cardinality drift' scale_short_facet_cardinality \
    'scale target facets did not exactly match expected counts' \
    'target_attributesForFaceting=["category","color"]'
  assert_scale_two_point_failure_scenario 'scale two-point fails timed-out migration requests' \
    scale_request_timeout 'scale migration request timed out or failed transport' scale_request_budget
  assert_scale_two_point_wall_clock_failure
  assert_scale_two_point_failure_scenario 'scale two-point fails incomplete sidecar samples' \
    scale_sidecar_incomplete 'scale trial sidecar sample count did not match expected page count' scale_sidecar_samples_complete
  assert_scale_two_point_failure_scenario 'scale two-point rejects large-only sidecar undersampling' \
    scale_large_sidecar_undersampled 'scale trial sidecar sample count did not match expected page count' scale_sidecar_samples_complete
  assert_scale_two_point_failure_scenario 'scale two-point fails manifest generation drift' \
    scale_manifest_generation_drift 'scale trial spool manifest counters did not match observed evidence' scale_spool_manifest
  assert_scale_two_point_failure_scenario 'scale two-point fails manifest count drift' \
    scale_manifest_count_drift 'scale trial spool manifest counters did not match observed evidence' scale_spool_manifest
  assert_scale_two_point_failure_scenario 'scale two-point fails manifest length drift' \
    scale_manifest_length_drift 'scale trial spool manifest counters did not match observed evidence' scale_spool_manifest
  assert_scale_two_point_failure_scenario 'scale two-point fails multiple live job directories' \
    scale_multiple_jobs 'scale trial sampler reported an error' scale_sampler_completeness
  assert_scale_two_point_growth_ceiling_failure
  assert_scale_two_point_rejects_under_minimum_trial_count
  assert_scale_two_point_rejects_excessive_request_budget
  assert_scale_two_point_receipt_reports_accepted_trial_count
  assert_scale_two_point_even_trial_medians
  assert_repository_scale_evidence_contract
  assert_scenario_inventory_rejects_missing_stage3_id
  assert_replica_success_scenario

  assert_verified_failure_scenario 'verified importing rejects response count drift' importing_verified_bad_counts
  assert_verified_failure_scenario 'verified importing rejects known-answer field drift' importing_verified_bad_known_answers
  assert_verified_failure_scenario 'verified importing rejects ineffective settings' importing_verified_bad_settings
  assert_verified_failure_scenario 'verified importing rejects ineffective synonym' importing_verified_bad_synonym
  assert_verified_failure_scenario 'verified importing rejects ineffective promotion' importing_verified_bad_promotion
  assert_verified_failure_scenario 'verified importing rejects ineffective hiding' importing_verified_bad_hiding
  assert_verified_failure_scenario 'verified importing rejects conflict mutation' importing_verified_conflict_mutates
  assert_verified_failure_scenario 'verified importing rejects invalid-key target creation' importing_verified_invalid_creates
  assert_verified_failure_scenario 'verified importing rejects cleanup residue' importing_verified_cleanup_residue
  assert_replica_failure_scenario 'replica scenario rejects no replica checks' importing_replicas_no_checks \
    'VACUOUS: replica scenario recorded zero replica checks'
  assert_replica_failure_scenario 'replica scenario rejects skipped replica checks' importing_replicas_skipped_check \
    'replica scenario receipt checks were vacuous or had invalid statuses'
  assert_replica_failure_scenario 'replica scenario rejects check names without replica' importing_replicas_bad_check_name \
    'replica scenario receipt checks were vacuous or had invalid statuses'
  assert_replica_failure_scenario 'replica scenario rejects missing warnings field' importing_replicas_missing_warnings
  assert_replica_failure_scenario 'replica scenario rejects empty warnings list' importing_replicas_empty_warnings
  assert_replica_failure_scenario 'replica scenario rejects sidecar warning' importing_replicas_sidecar_warning
  assert_replica_failure_scenario 'replica scenario rejects missing migrated replica listing' importing_replicas_missing_sidecar
  assert_replica_failure_scenario 'replica scenario rejects virtual replica primary-order leakage' importing_replicas_primary_order_leak
  assert_replica_failure_scenario 'replica scenario rejects dropped standard desc ranking' importing_replicas_standard_desc_dropped
  assert_replica_failure_scenario 'replica scenario rejects unnormalized standard sidecar ranking' importing_replicas_standard_ranking_unnormalized
  assert_replica_failure_scenario 'replica scenario rejects mistranslated sidecar ranking settings' importing_replicas_wrong_sidecar_ranking
  assert_replica_failure_scenario 'replica scenario rejects physical replica meta' importing_replicas_physical_replica_data
  assert_replica_failure_scenario 'replica scenario rejects physical replica corpus' importing_replicas_physical_replica_corpus

  assert_failure_scenario 'unavailable returning 2xx fails closed' unavailable_returns_2xx unavailable
  assert_failure_scenario 'unavailable wrong 503 code fails closed' unavailable_wrong_code unavailable
  assert_failure_scenario 'unavailable listed target fails closed' unavailable_lists_target unavailable
  assert_failure_scenario 'importing returning 503 fails closed' importing_returns_503 importing
  assert_failure_scenario 'importing omitted target fails closed' importing_omits_target importing
  assert_failure_scenario 'importing empty target fails closed' importing_empty_target importing
  assert_failure_scenario 'importing duplicated target fails closed' importing_duplicates_target importing
  assert_failure_scenario 'importing count mismatch fails closed' importing_wrong_count importing
  assert_failure_scenario 'malformed migration response fails closed' malformed_migration_json unavailable
  assert_failure_scenario 'malformed list-indices response fails closed' malformed_indexes_json unavailable

  assert_async_positive_control
  assert_async_preflight_sweep_scope
  assert_async_delete_failure 'async preflight rejects a failed exact-target deletion' \
    async_delete_http_failure
  assert_async_delete_failure 'async preflight rejects a malformed deletion success' \
    async_delete_malformed_success
  assert_async_cleanup_uses_write_host_truth
  assert_async_failure_scenario 'async terminal success without a present target fails closed' \
    async_success_target_absent 'async scenario expected exactly one target index listing'
  assert_async_failure_scenario 'async backward phase movement fails closed' \
    async_phase_regression 'async migration phase regressed to exporting'
  assert_cancel_positive_control
  assert_cancel_preflight_sweep_scope
  assert_cancel_failure_scenario 'cancel scenario rejects a zero-document source' \
    cancel_zero_source 'cancel source fixture held 0 documents'
  assert_cancel_failure_scenario 'cancel scenario rejects cleanup residue' \
    cancel_cleanup_residue 'cancel fixture cleanup failed or left residue'
  assert_cancel_failure_scenario 'cancel scenario rejects spool residue' \
    cancel_spool_residue 'cancel_precommit leaked migration spool artifacts'
  assert_cancel_sentinel_field_order
  assert_cancel_postcommit_pagination

  assert_argument_contract 'missing expect-mode fails' nonzero
  assert_argument_contract 'unknown expect-mode fails' nonzero --expect-mode future
  assert_replica_argument_acceptance
  assert_argument_contract 'unknown scenario fails' nonzero --expect-mode importing --scenario future --secret-file "$WORK_DIR/secret.env" --source-index source --target-index target
  assert_argument_contract 'unavailable refuses scenario' nonzero --expect-mode unavailable --scenario replicas
  assert_argument_contract 'unavailable refuses secret-file' nonzero --expect-mode unavailable --secret-file "$WORK_DIR/secret.env"
  assert_argument_contract 'unavailable refuses source-index' nonzero --expect-mode unavailable --source-index source
  assert_argument_contract 'unavailable refuses target-index' nonzero --expect-mode unavailable --target-index target
  assert_non_scale_corpus_size_rejected unavailable
  assert_argument_contract 'importing requires secret-file' nonzero --expect-mode importing --source-index source --target-index target
  assert_argument_contract 'importing requires source-index' nonzero --expect-mode importing --secret-file "$WORK_DIR/secret.env" --target-index target
  assert_argument_contract 'importing requires target-index' nonzero --expect-mode importing --secret-file "$WORK_DIR/secret.env" --source-index source
  assert_non_scale_corpus_size_rejected importing
  assert_argument_contract 'verification manifest is importing-only' nonzero --expect-mode unavailable --verification-manifest "$WORK_DIR/manifest.json"
  assert_argument_contract 'verification manifest requires absolute path' nonzero --expect-mode importing --secret-file "$WORK_DIR/secret.env" --source-index source --target-index target --verification-manifest relative.json
  assert_argument_contract 'importing requires absolute secret-file path' nonzero --expect-mode importing --secret-file relative.env --source-index source --target-index target
  assert_argument_contract 'missing importing secret file is sanitized' nonzero --expect-mode importing --secret-file "$WORK_DIR/missing.env" --source-index source --target-index target
  assert_async_argument_message 'async job scenario requires secret-file' \
    '--secret-file is required in async_job scenario' --scenario async_job
  assert_async_argument_message 'async unknown scenario fails' \
    '--scenario must be async_job' --scenario future
  assert_async_index_conflict_rejected
  assert_cancel_argument_acceptance
  assert_cancel_index_conflict_rejected
  assert_argument_contract 'scale requires secret-file' nonzero --expect-mode scale
  assert_argument_contract 'scale requires absolute secret-file path' nonzero --expect-mode scale --secret-file relative.env
  assert_scale_argument_contract 'scale accepts explicit corpus size' zero --corpus-size 20000
  assert_scale_argument_contract 'scale two-point rejects explicit corpus size' nonzero --two-point --corpus-size 20000
  assert_scale_argument_contract 'scale refuses source-index' nonzero --source-index source
  assert_scale_argument_contract 'scale refuses target-index' nonzero --target-index target
  assert_scale_argument_contract 'scale refuses verification-manifest' nonzero --verification-manifest "$WORK_DIR/manifest.json"
  assert_scale_argument_contract 'scale rejects too-small corpus' nonzero --corpus-size 19999
  assert_argument_contract 'replica scenario source index rejects path traversal' nonzero --expect-mode importing --scenario replicas --secret-file "$WORK_DIR/secret.env" --source-index 'fj_replica_../escape' --target-index fj_replica_target
  assert_argument_contract 'replica scenario target index rejects path traversal' nonzero --expect-mode importing --scenario replicas --secret-file "$WORK_DIR/secret.env" --source-index fj_replica_source --target-index 'fj_replica_target/escape'

  assert_signal_scenario 'INT preserves evidence, stops server, and returns 130' self_int 130
  assert_signal_scenario 'TERM preserves evidence, stops server, and returns 143' self_term 143
  assert_cleanup_failure_scenario
  assert_testing_docs_scale_proof_contract
  assert_debbie_public_sync_surface

  assert_terminal_scenario_inventory || return 1

  printf '\nResults: %d/%d passed (%d skipped)\n' "$TESTS_PASSED" "$TESTS_RUN" "$TESTS_SKIPPED"
  if [ "$TESTS_FAILED" -gt 0 ]; then
    printf '%d test(s) failed\n' "$TESTS_FAILED"
    return 1
  fi
  echo 'All tests passed'
}

main "$@"
