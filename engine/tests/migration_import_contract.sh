#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
ENGINE_DIR="$REPO_DIR/engine"
WAIT_HELPER="$SCRIPT_DIR/common/wait_for_flapjack.sh"
SECRET_HELPER="$SCRIPT_DIR/common/load_named_secrets.sh"
GENERATOR="$SCRIPT_DIR/common/generate_algolia_corpus.sh"
SCALE_FIXTURE="${FJ_SCALE_FIXTURE_BIN:-$SCRIPT_DIR/common/algolia_corpus_fixture.sh}"
VENDOR_CLIENT="$SCRIPT_DIR/common/algolia_vendor_client.sh"
# shellcheck source=engine/tests/common/algolia_vendor_client.sh disable=SC1091
source "$VENDOR_CLIENT"

EXPECT_MODE=""
SCENARIO=""
SECRET_FILE=""
SOURCE_INDEX=""
TARGET_INDEX=""
VERIFICATION_MANIFEST=""
CORPUS_SIZE="20000"
CORPUS_SIZE_SUPPLIED=0
SCALE_CORPUS_FLOOR=20000
TWO_POINT=0
SCALE_TRIAL_COUNT_FLOOR=3
SCALE_TRIAL_COUNT="${MIGRATION_IMPORT_CONTRACT_TRIAL_COUNT:-$SCALE_TRIAL_COUNT_FLOOR}"
SCALE_BROWSE_PAGE_SIZE=1000
SCALE_REQUEST_BUDGET_MAX_MILLISECONDS=900000
SCALE_REQUEST_BUDGET_MILLISECONDS="${MIGRATION_IMPORT_CONTRACT_REQUEST_BUDGET_MS:-$SCALE_REQUEST_BUDGET_MAX_MILLISECONDS}"
SCALE_REQUEST_BUDGET_SECONDS=900
SCALE_REWRITE_GROWTH_CEILING=75
SOURCE_APP_ID=""
SOURCE_API_KEY=""

# The async scenario owns disposable Algolia fixtures. Every index it creates or
# is willing to delete must carry this prefix, so a sweep can never reach the
# fj_scale_, fj_replica_, or fj_cancel_ fixtures owned by sibling drivers.
ASYNC_INDEX_PREFIX="fj_async_"
# A leftover fj_async_ index younger than this may belong to a concurrent run, so
# the sweep skips it instead of deleting another worker's live fixture.
ASYNC_STALE_AGE_SECONDS=86400
ASYNC_POLL_ATTEMPTS=240
ASYNC_POLL_INTERVAL_SECONDS=0.5
ASYNC_PHASE_ORDER="submitted exporting preparing staging activating"
ASYNC_OWNED_ALGOLIA_INDICES=()
ASYNC_FIXTURE_CLEANED=0
ASYNC_JOB_ID=""
ASYNC_PHASE_SEQUENCE=""
ASYNC_PHASE_RANK=0
ASYNC_RESOLVED_INDEX=""

# The cancel scenario owns disposable live fixtures under its own prefix so its
# preflight and cleanup can never touch async, scale, replica, or caller-owned names.
CANCEL_INDEX_PREFIX="fj_cancel_"
CANCEL_SOURCE_COUNT=2500
CANCEL_BROWSE_PAGE_SIZE=1000
CANCEL_STALE_AGE_SECONDS=86400
CANCEL_POLL_ATTEMPTS=240
CANCEL_POLL_INTERVAL_SECONDS=0.5
CANCEL_OWNED_ALGOLIA_INDICES=()
CANCEL_FIXTURE_CLEANED=0
CANCEL_RESOLVED_INDEX=""
CANCEL_PRECOMMIT_JOB_ID=""
CANCEL_POSTCOMMIT_JOB_ID=""
CANCEL_PRECOMMIT_TARGET=""
CANCEL_POSTCOMMIT_TARGET=""
CANCEL_PRECOMMIT_BARRIER_DIR=""
CANCEL_POSTCOMMIT_BARRIER_DIR=""
CANCEL_PRECOMMIT_SENTINEL=""
CANCEL_PRECOMMIT_LISTING=""

WORK_DIR=""
DATA_DIR=""
LOG_DIR=""
RECEIPT=""
SERVER_PID=""
SERVER_LOG=""
BASE_URL=""
ADMIN_KEY=""
BIN_PATH=""
RUN_PREFIX=""
PASS_COMPLETE=0
INTERRUPTED_EXIT_CODE=0
CLEANUP_FAILED=0
EVIDENCE_DIR=""
EVIDENCE_ANNOUNCED=0
EVIDENCE_COPY_FAILED=0
VERIFICATION_TARGETS_CLEANED=0
SCALE_FIXTURE_LEDGER=""
SCALE_FIXTURE_CLEANED=0
CURRENT_TRIAL_DIR=""
CURRENT_TRIAL_RECORD=""
CURRENT_TRIAL_CONDITION=""
CURRENT_TRIAL_NUMBER=""
REPLICA_SOURCE_FIXTURE_CLEANED=0
RUN_STARTED_EPOCH=""
RUN_STARTED_AT=""
ORIGINAL_ARGS=()

usage() {
  cat <<'EOF'
Usage:
  migration_import_contract.sh --expect-mode unavailable
  migration_import_contract.sh --expect-mode importing [--scenario replicas] --secret-file <absolute-path> --source-index <name> --target-index <name> [--verification-manifest <absolute-path>]
  migration_import_contract.sh --expect-mode scale --secret-file <absolute-path> [--corpus-size <N>]
  migration_import_contract.sh --expect-mode scale --two-point --secret-file <absolute-path>
  migration_import_contract.sh --scenario async_job --secret-file <absolute-path> [--source-index <fj_async_ name>] [--target-index <fj_async_ name>]
  migration_import_contract.sh --expect-mode importing --scenario cancel --secret-file <absolute-path> [--source-index <fj_cancel_ name>] [--target-index <fj_cancel_ name>]
EOF
}

die() {
  printf 'ERROR: %s\n' "$1" >&2
  exit "${2:-1}"
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

http_body() { algolia_vendor_response_body; }
http_code() { algolia_vendor_response_code; }

http_success_code() {
  local code="$1"
  [[ "$code" =~ ^[0-9]+$ ]] && [ "$code" -ge 200 ] && [ "$code" -le 299 ]
}

parse_args() {
  while [ "$#" -gt 0 ]; do
    case "$1" in
      --expect-mode)
        EXPECT_MODE="${2:-}"
        shift 2
        ;;
      --scenario)
        SCENARIO="${2:-}"
        shift 2
        ;;
      --secret-file)
        SECRET_FILE="${2:-}"
        shift 2
        ;;
      --source-index)
        SOURCE_INDEX="${2:-}"
        shift 2
        ;;
      --target-index)
        TARGET_INDEX="${2:-}"
        shift 2
        ;;
      --verification-manifest)
        VERIFICATION_MANIFEST="${2:-}"
        shift 2
        ;;
      --corpus-size)
        CORPUS_SIZE="${2:-}"
        CORPUS_SIZE_SUPPLIED=1
        shift 2
        ;;
      --two-point)
        TWO_POINT=1
        shift
        ;;
      --help|-h)
        usage
        exit 0
        ;;
      *)
        usage >&2
        die "unknown argument: $1" 2
        ;;
    esac
  done

  # --scenario is the async selector. Normalize it once into EXPECT_MODE so the
  # rest of the driver keeps a single dispatch source of truth instead of two.
  case "$SCENARIO" in
    "") ;;
    # The replica and cancel scenarios stay --expect-mode importing selectors; their
    # mode-specific validation lives in the importing) arm below.
    replicas|cancel) ;;
    async_job)
      [ -z "$EXPECT_MODE" ] || die "--expect-mode is not allowed with --scenario async_job" 2
      EXPECT_MODE="async_job"
      ;;
    *)
      usage >&2
      die "--scenario must be async_job" 2
      ;;
  esac

  case "$EXPECT_MODE" in
    unavailable)
      [ -z "$SCENARIO" ] || die "--scenario is not allowed in unavailable mode" 2
      [ -z "$SECRET_FILE" ] || die "--secret-file is not allowed in unavailable mode" 2
      [ -z "$SOURCE_INDEX" ] || die "--source-index is not allowed in unavailable mode" 2
      [ -z "$TARGET_INDEX" ] || die "--target-index is not allowed in unavailable mode" 2
      [ -z "$VERIFICATION_MANIFEST" ] || die "--verification-manifest is not allowed in unavailable mode" 2
      [ "$CORPUS_SIZE_SUPPLIED" -eq 0 ] || die "--corpus-size is not allowed in unavailable mode" 2
      [ "$TWO_POINT" -eq 0 ] || die "--two-point is only allowed in scale mode" 2
      ;;
    importing)
      case "$SCENARIO" in
        ""|replicas) ;;
        cancel)
          resolve_cancel_index_names
          ;;
        *) die "--scenario must be replicas or cancel when provided" 2 ;;
      esac
      [ -n "$SECRET_FILE" ] || die "--secret-file is required in importing mode" 2
      [ -n "$SOURCE_INDEX" ] || die "--source-index is required in importing mode" 2
      [ -n "$TARGET_INDEX" ] || die "--target-index is required in importing mode" 2
      [ "$CORPUS_SIZE_SUPPLIED" -eq 0 ] || die "--corpus-size is not allowed in importing mode" 2
      [ "$TWO_POINT" -eq 0 ] || die "--two-point is only allowed in scale mode" 2
      case "$SECRET_FILE" in
        /*) ;;
        *) die "--secret-file must be an absolute path in importing mode" 2 ;;
      esac
      if [ -n "$VERIFICATION_MANIFEST" ]; then
        [ -z "$SCENARIO" ] || die "--verification-manifest is not supported with --scenario" 2
        case "$VERIFICATION_MANIFEST" in
          /*) ;;
          *) die "--verification-manifest must be an absolute path in importing mode" 2 ;;
        esac
      fi
      [ "$SCENARIO" != "cancel" ] || require_cancel_names
      ;;
    scale)
      [ -n "$SECRET_FILE" ] || die "--secret-file is required in scale mode" 2
      case "$SECRET_FILE" in
        /*) ;;
        *) die "--secret-file must be an absolute path in scale mode" 2 ;;
      esac
      # Scale mode owns fixture creation internally; importing mode remains caller-driven.
      [ -z "$SOURCE_INDEX" ] || die "--source-index is not allowed in scale mode" 2
      [ -z "$TARGET_INDEX" ] || die "--target-index is not allowed in scale mode" 2
      [ -z "$VERIFICATION_MANIFEST" ] || die "--verification-manifest is not allowed in scale mode" 2
      [ "$TWO_POINT" -eq 0 ] || [ "$CORPUS_SIZE_SUPPLIED" -eq 0 ] \
        || die "--corpus-size conflicts with --two-point" 2
      [[ "$CORPUS_SIZE" =~ ^[1-9][0-9]*$ ]] || die "--corpus-size must be a positive integer in scale mode" 2
      [ "$TWO_POINT" -eq 1 ] || [ "$CORPUS_SIZE" -ge "$SCALE_CORPUS_FLOOR" ] \
        || die "--corpus-size must be at least ${SCALE_CORPUS_FLOOR} in scale mode" 2
      # Repeated sampling is the point of two-point mode: a run that samples fewer
      # than three trials per condition cannot support the median/spread claims the
      # receipt makes, so reject it before any live vendor work happens.
      if [ "$TWO_POINT" -eq 1 ]; then
        { [[ "$SCALE_TRIAL_COUNT" =~ ^[0-9]+$ ]] && [ "$SCALE_TRIAL_COUNT" -ge "$SCALE_TRIAL_COUNT_FLOOR" ]; } \
          || die "two-point trial count must be an integer of at least ${SCALE_TRIAL_COUNT_FLOOR}" 2
        { [[ "$SCALE_REQUEST_BUDGET_MILLISECONDS" =~ ^[1-9][0-9]*$ ]] \
          && [ "$SCALE_REQUEST_BUDGET_MILLISECONDS" -le "$SCALE_REQUEST_BUDGET_MAX_MILLISECONDS" ]; } \
          || die "two-point request budget must be an integer from 1 through ${SCALE_REQUEST_BUDGET_MAX_MILLISECONDS} milliseconds" 2
      fi
      SCALE_REQUEST_BUDGET_SECONDS=$(((SCALE_REQUEST_BUDGET_MILLISECONDS + 999) / 1000))
      ;;
    async_job)
      [ -n "$SECRET_FILE" ] || die "--secret-file is required in async_job scenario" 2
      case "$SECRET_FILE" in
        /*) ;;
        *) die "--secret-file must be an absolute path in async_job scenario" 2 ;;
      esac
      [ -z "$VERIFICATION_MANIFEST" ] || die "--verification-manifest is not allowed in async_job scenario" 2
      [ "$CORPUS_SIZE_SUPPLIED" -eq 0 ] || die "--corpus-size is not allowed in async_job scenario" 2
      [ "$TWO_POINT" -eq 0 ] || die "--two-point is only allowed in scale mode" 2
      resolve_async_index_names
      ;;
    "")
      usage >&2
      die "--expect-mode is required" 2
      ;;
    *)
      usage >&2
      die "--expect-mode must be unavailable, importing, or scale" 2
      ;;
  esac
}

replica_source_relevance_index() { printf '%s_relevance' "$SOURCE_INDEX"; }
replica_source_relevance_topology_entry() { printf 'virtual(%s)' "$(replica_source_relevance_index)"; }
replica_source_standard_index() { printf '%s_standard_rank' "$SOURCE_INDEX"; }
replica_target_relevance_index() { printf '%s_relevance' "$TARGET_INDEX"; }
replica_target_standard_index() { printf '%s_standard_rank' "$TARGET_INDEX"; }

require_replica_name() {
  local label="$1" name="$2"
  [[ "$name" =~ ^fj_replica_[A-Za-z0-9_-]+$ ]] \
    || die "replica scenario ${label} must match ^fj_replica_[A-Za-z0-9_-]+$: ${name}" 2
}

require_replica_names() {
  [ "$SCENARIO" = "replicas" ] || return 0
  require_replica_name "source index" "$SOURCE_INDEX"
  require_replica_name "target index" "$TARGET_INDEX"
  require_replica_name "source relevance replica" "$(replica_source_relevance_index)"
  require_replica_name "source standard replica" "$(replica_source_standard_index)"
  require_replica_name "target relevance replica" "$(replica_target_relevance_index)"
  require_replica_name "target standard replica" "$(replica_target_standard_index)"
}

require_cancel_name() {
  local label="$1" name="$2"
  [[ "$name" =~ ^fj_cancel_[A-Za-z0-9_-]+$ ]] \
    || die "cancel scenario ${label} must match ^fj_cancel_[A-Za-z0-9_-]+$: ${name}" 2
}

require_cancel_names() {
  [ "$SCENARIO" = "cancel" ] || return 0
  require_cancel_name "source index" "$SOURCE_INDEX"
  require_cancel_name "target index" "$TARGET_INDEX"
  [ "$SOURCE_INDEX" != "$TARGET_INDEX" ] \
    || die "cancel source and target index names must differ" 2
}

resolve_cancel_index_name() {
  local role="$1" flag_value="$2" env_name="$3" env_value="${!3:-}" resolved
  if [ -n "$flag_value" ] && [ -n "$env_value" ] && [ "$flag_value" != "$env_value" ]; then
    die "--${role}-index and ${env_name} disagree" 2
  fi
  resolved="${flag_value:-$env_value}"
  if [ -z "$resolved" ]; then
    resolved="${CANCEL_INDEX_PREFIX}${role}_$(date +%s)_$$_$((RANDOM % 100000))"
  fi
  case "$resolved" in
    "${CANCEL_INDEX_PREFIX}"*) ;;
    *) die "cancel ${role} index must start with ${CANCEL_INDEX_PREFIX}" 2 ;;
  esac
  CANCEL_RESOLVED_INDEX="$resolved"
}

resolve_cancel_index_names() {
  resolve_cancel_index_name source "$SOURCE_INDEX" FJ_CANCEL_SOURCE_INDEX
  SOURCE_INDEX="$CANCEL_RESOLVED_INDEX"
  resolve_cancel_index_name target "$TARGET_INDEX" FJ_CANCEL_TARGET_INDEX
  TARGET_INDEX="$CANCEL_RESOLVED_INDEX"
  [ "$SOURCE_INDEX" != "$TARGET_INDEX" ] || die "cancel source and target index names must differ" 2
}

load_credentials() {
  if [ "$EXPECT_MODE" = "unavailable" ]; then
    SOURCE_APP_ID="stub_app_id"
    SOURCE_API_KEY="stub_source_key"
    return
  fi

  # shellcheck source=engine/tests/common/load_named_secrets.sh disable=SC1091
  source "$SECRET_HELPER"
  local loader_output
  loader_output="$(mktemp)"
  if ! load_named_secrets "$SECRET_FILE" ALGOLIA_APP_ID ALGOLIA_ADMIN_KEY >"$loader_output" 2>&1; then
    rm -f "$loader_output"
    die "required Algolia credentials could not be loaded"
  fi
  rm -f "$loader_output"
  SOURCE_APP_ID="$ALGOLIA_APP_ID"
  SOURCE_API_KEY="$ALGOLIA_ADMIN_KEY"
}

prepare_scale_fixture() {
  local metadata manifest_path
  [ -x "$SCALE_FIXTURE" ] || die "scale fixture script is not executable"
  [ -x "$GENERATOR" ] || die "scale corpus generator is not executable"
  metadata="$("$SCALE_FIXTURE" prepare --corpus-size "$CORPUS_SIZE" --secret-file "$SECRET_FILE" --work-dir "$WORK_DIR")" \
    || die "scale fixture prepare failed"
  SOURCE_INDEX="$(printf '%s\n' "$metadata" | jq -er '.source_index')"
  TARGET_INDEX="$(printf '%s\n' "$metadata" | jq -er '.target_index')"
  SCALE_FIXTURE_LEDGER="$(printf '%s\n' "$metadata" | jq -er '.ledger_path')"
  [ -f "$SCALE_FIXTURE_LEDGER" ] || die "scale fixture ledger was not created"
  manifest_path="$WORK_DIR/scale-verification-manifest.json"
  "$GENERATOR" manifest --corpus-size "$CORPUS_SIZE" >"$manifest_path"
  VERIFICATION_MANIFEST="$manifest_path"
}

init_run() {
  require_tool curl
  require_tool jq
  require_tool od
  require_tool sed
  require_tool tr

  RUN_STARTED_EPOCH="$(date +%s)"
  RUN_STARTED_AT="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  WORK_DIR="$(mktemp -d)"
  trap cleanup EXIT
  trap 'INTERRUPTED_EXIT_CODE=130; exit 130' INT
  trap 'INTERRUPTED_EXIT_CODE=143; exit 143' TERM
  DATA_DIR="$WORK_DIR/flapjack-data"
  LOG_DIR="$WORK_DIR/logs"
  RECEIPT="$WORK_DIR/receipt.json"
  mkdir -p "$DATA_DIR" "$LOG_DIR"
  SERVER_LOG="$LOG_DIR/flapjack-server.log"
  : >"$LOG_DIR/migration-response.raw"
  : >"$LOG_DIR/list-indices.raw"

  if [ "$EXPECT_MODE" = "scale" ] && [ "$TWO_POINT" -eq 0 ]; then
    prepare_scale_fixture
  fi

  if [ -n "$VERIFICATION_MANIFEST" ]; then
    [ -f "$VERIFICATION_MANIFEST" ] || die "verification manifest does not exist"
    jq -e '
      (.source_count | type == "number" and . > 0 and floor == .)
      and (.synonym_count | type == "number" and . >= 0 and floor == .)
      and (.rule_count | type == "number" and . >= 0 and floor == .)
      and (.known_answers_query | type == "string" and length > 0)
      and (.known_answers | type == "array" and length > 0)
      and (all(.known_answers[]; type == "object" and (.objectID | type == "string" and length > 0)))
      and (.probes.settings.request | type == "object")
      and (.probes.settings.expected_object_ids | type == "array" and length > 0)
      and (.probes.synonym.request | type == "object")
      and (.probes.synonym.expected_object_ids | type == "array" and length > 0)
      and (.probes.promotion.request | type == "object")
      and (.probes.promotion.expected_first_object_id | type == "string" and length > 0)
      and (.probes.promotion.competitor_object_id | type == "string" and length > 0)
      and (.probes.promotion.expected_rule_id | type == "string" and length > 0)
      and (.probes.hiding.request | type == "object")
      and (.probes.hiding.hidden_object_id | type == "string" and length > 0)
      and (.probes.hiding.expected_object_ids | type == "array")
      and (.probes.hiding.expected_rule_id | type == "string" and length > 0)
    ' "$VERIFICATION_MANIFEST" >/dev/null || die "verification manifest is malformed"
    if [ "$EXPECT_MODE" = "scale" ]; then
      jq -e '
        (.aggregate_expectations.final_object_id | type == "string" and length > 0)
        and (.aggregate_expectations.facets.category | type == "object" and length > 0)
        and (.aggregate_expectations.facets.color | type == "object" and length > 0)
        and (all(.aggregate_expectations.facets[][]; type == "number" and . >= 0 and floor == .))
      ' "$VERIFICATION_MANIFEST" >/dev/null || die "scale verification manifest aggregate expectations are malformed"
    fi
    cp "$VERIFICATION_MANIFEST" "$LOG_DIR/source-manifest.json"
  fi

  local random_hex
  random_hex="$(od -An -N8 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || die "failed to generate run entropy"
  RUN_PREFIX="fj_migration_import_${random_hex}"
  ADMIN_KEY="fj_import_contract_$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"

  if [ "$EXPECT_MODE" = "unavailable" ]; then
    SOURCE_INDEX="${RUN_PREFIX}_source"
    TARGET_INDEX="${RUN_PREFIX}_target"
  fi
  require_replica_names

  local command_json host_name host_uname runtime_shell runtime_bash_version runtime_working_directory
  command_json="$({
    printf '%s\n' "bash" "engine/tests/migration_import_contract.sh"
    printf '%s\n' "${ORIGINAL_ARGS[@]}"
  } | jq -R . | jq -s .)"
  host_name="$(hostname 2>/dev/null || printf 'unknown')"
  host_uname="$(uname -a 2>/dev/null || printf 'unknown')"
  runtime_shell="${BASH:-bash}"
  runtime_bash_version="${BASH_VERSION:-unknown}"
  runtime_working_directory="$(pwd)"

  jq -n \
    --arg mode "$EXPECT_MODE" \
    --arg source "$SOURCE_INDEX" \
    --arg target "$TARGET_INDEX" \
    --arg scenario "$SCENARIO" \
    --arg source_relevance "$(replica_source_relevance_index)" \
    --arg source_standard "$(replica_source_standard_index)" \
    --arg target_relevance "$(replica_target_relevance_index)" \
    --arg target_standard "$(replica_target_standard_index)" \
    --arg head "$(git -C "$REPO_DIR" rev-parse HEAD 2>/dev/null || true)" \
    --arg started_at "$RUN_STARTED_AT" \
    --arg host_name "$host_name" \
    --arg host_uname "$host_uname" \
    --arg runtime_shell "$runtime_shell" \
    --arg runtime_bash_version "$runtime_bash_version" \
    --arg runtime_working_directory "$runtime_working_directory" \
    --argjson command "$command_json" \
    '{
      mode:$mode,
      scenario:$scenario,
      source_index:$source,
      target_index:$target,
      head:$head,
      started_at:$started_at,
      host:{name:$host_name, uname:$host_uname},
      runtime:{
        shell:$runtime_shell,
        bash_version:$runtime_bash_version,
        working_directory:$runtime_working_directory
      },
      command:$command,
      owned_resources:{targets:(if $target == "" then [] else [$target] end)},
    checks:[]
  }
  | if $scenario == "replicas" then
      .owned_resources.targets += [$target_relevance, $target_standard, $source, $source_relevance, $source_standard]
    else
      .
    end
' >"$RECEIPT"

  if [ -n "$VERIFICATION_MANIFEST" ]; then
    local next conflict_target invalid_target
    conflict_target="${TARGET_INDEX}_conflict"
    invalid_target="${TARGET_INDEX}_invalid_key"
    next="$(mktemp)"
    jq --arg conflict "$conflict_target" --arg invalid "$invalid_target" \
      '.owned_resources.targets += [$conflict, $invalid]' "$RECEIPT" >"$next"
    mv "$next" "$RECEIPT"
  fi

  if [ "$EXPECT_MODE" = "scale" ]; then
    local next
    next="$(mktemp)"
    if [ "$TWO_POINT" -eq 1 ]; then
      jq --argjson page_size "$SCALE_BROWSE_PAGE_SIZE" \
        --argjson budget_ms "$SCALE_REQUEST_BUDGET_MILLISECONDS" \
        --argjson ceiling "$SCALE_REWRITE_GROWTH_CEILING" \
        --argjson trials "$SCALE_TRIAL_COUNT" '
        .scale = {
          mode:"two-point",
          conditions:[2000,20000],
          trials_per_condition:$trials,
          browse_page_size:$page_size,
          request_budget_milliseconds:$budget_ms,
          completed_object_ids_rewrite_growth_ceiling:$ceiling,
          conditions_observed:[]
        }
      ' "$RECEIPT" >"$next"
    else
      jq --argjson corpus_size "$CORPUS_SIZE" --slurpfile ledger "$SCALE_FIXTURE_LEDGER" '
        .scale = {mode:"single-size", corpus_size:$corpus_size}
        | .owned_resources.algolia_sources = [
            $ledger[0].algolia_sources[]? | select(.owned == true) | .name
          ]
      ' "$RECEIPT" >"$next"
    fi
    mv "$next" "$RECEIPT"
  fi

  if [ "$SCENARIO" = "cancel" ]; then
    local next post_target
    post_target="$(cancel_postcommit_target_name)"
    next="$(mktemp)"
    jq --arg source "$SOURCE_INDEX" --arg target "$TARGET_INDEX" --arg post_target "$post_target" '
      .owned_resources.algolia_sources = [$source]
      | .owned_resources.targets = ((.owned_resources.targets + [$target, $post_target]) | unique)
      | .cancel = {
          corpus_size:null,
          browse_page_size:null,
          swept_algolia_indices:[],
          precommit:{},
          postcommit:{}
        }
    ' "$RECEIPT" >"$next"
    mv "$next" "$RECEIPT"
  fi

  if [ "$EXPECT_MODE" = "async_job" ]; then
    prepare_async_fixture
  fi
}

record_check() {
  local name="$1" status="$2" detail="${3:-}"
  local next
  next="$(mktemp)"
  jq --arg name "$name" --arg status "$status" --arg detail "$detail" \
    '.checks += [{name:$name,status:$status,detail:$detail}]' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

build_or_resolve_binary() {
  if [ -n "${FLAPJACK_BIN:-}" ]; then
    [ -x "$FLAPJACK_BIN" ] || die "FLAPJACK_BIN is not executable"
    BIN_PATH="$FLAPJACK_BIN"
    record_binary_identity
    return
  fi

  if (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$LOG_DIR/build.log" 2>&1); then
    BIN_PATH="$(cd "$ENGINE_DIR" && cd "${CARGO_TARGET_DIR:-target}" && pwd)/debug/flapjack"
  else
    die "cargo build -p flapjack-server failed"
  fi
  [ -x "$BIN_PATH" ] || die "expected flapjack binary was not built"
  record_binary_identity
}

record_binary_identity() {
  local next
  next="$(mktemp)"
  if "$BIN_PATH" build-info --json >"$LOG_DIR/build-info.json" 2>"$LOG_DIR/build-info.stderr"; then
    jq --arg bin "$BIN_PATH" --slurpfile build "$LOG_DIR/build-info.json" \
      '.binary = {path:$bin, build_info:$build[0]}' "$RECEIPT" >"$next"
  else
    jq --arg bin "$BIN_PATH" \
      '.binary = {path:$bin, build_info:null, build_info_available:false}' "$RECEIPT" >"$next"
  fi
  mv "$next" "$RECEIPT"
}

start_server() {
  if [ "$SCENARIO" = "cancel" ]; then
    CANCEL_PRECOMMIT_BARRIER_DIR="$WORK_DIR/cancel-precommit-barrier"
    CANCEL_POSTCOMMIT_BARRIER_DIR="$WORK_DIR/cancel-postcommit-barrier"
    mkdir -p "$CANCEL_PRECOMMIT_BARRIER_DIR" "$CANCEL_POSTCOMMIT_BARRIER_DIR"
    FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_SOURCE="$SOURCE_INDEX" \
      FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_PRE_ACTIVATION_BARRIER_DIR="$CANCEL_PRECOMMIT_BARRIER_DIR" \
      FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_SOURCE="$SOURCE_INDEX" \
      FLAPJACK_ALGOLIA_LIVE_TEST_IMPORT_POST_COMMIT_BARRIER_DIR="$CANCEL_POSTCOMMIT_BARRIER_DIR" \
      FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
      FLAPJACK_DATA_DIR="$DATA_DIR" \
      "$BIN_PATH" --auto-port >"$SERVER_LOG" 2>&1 &
  elif [ "$EXPECT_MODE" = "unavailable" ]; then
    FLAPJACK_NODE_ID="migration-import-contract" \
      FLAPJACK_PEERS="migration-peer=http://10.0.0.2:7700" \
      FLAPJACK_STARTUP_CATCHUP_STRICT=0 \
      FLAPJACK_STARTUP_CATCHUP_TIMEOUT_SECS=2 \
      FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
      FLAPJACK_DATA_DIR="$DATA_DIR" \
      "$BIN_PATH" --auto-port >"$SERVER_LOG" 2>&1 &
  else
    FLAPJACK_ADMIN_KEY="$ADMIN_KEY" \
      FLAPJACK_DATA_DIR="$DATA_DIR" \
      "$BIN_PATH" --auto-port >"$SERVER_LOG" 2>&1 &
  fi
  SERVER_PID=$!

  "$WAIT_HELPER" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$SERVER_LOG" --retries 80 --interval-seconds 0.5
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$SERVER_LOG" | head -1)"
  [ -n "$port" ] || die "server became ready but no auto-port was found"
  BASE_URL="http://127.0.0.1:${port}"
  record_check "local_server" "pass" "started"
}

flapjack_request() {
  local method="$1" path="$2" body="$3" out="$4" status body_file=""
  set +e
  if [ -n "$body" ]; then
    body_file="$(mktemp "$WORK_DIR/curl-body.XXXXXX")" || {
      set -e
      die "failed to create curl request body file"
    }
    chmod 600 "$body_file" 2>/dev/null || true
    printf '%s' "$body" >"$body_file" || {
      rm -f "$body_file"
      set -e
      die "failed to write curl request body file"
    }
  fi
  {
    printf 'silent\n'
    printf 'show-error\n'
    printf 'request = "%s"\n' "$method"
    printf 'url = "%s%s"\n' "$BASE_URL" "$path"
    printf 'header = "x-algolia-application-id: flapjack"\n'
    printf 'header = "x-algolia-api-key: %s"\n' "$ADMIN_KEY"
    printf 'header = "content-type: application/json"\n'
    if [ -n "$body_file" ]; then
      printf 'data-binary = "@%s"\n' "$body_file"
    fi
  } | curl --max-time "$SCALE_REQUEST_BUDGET_SECONDS" -w '\n%{http_code}' --config - >"$out"
  status=$?
  [ -z "$body_file" ] || rm -f "$body_file"
  set -e
  return "$status"
}

current_milliseconds() {
  perl -MTime::HiRes=time -e 'printf "%.0f\n", time() * 1000'
}

file_size_bytes() {
  local path="$1"
  stat -f%z "$path" 2>/dev/null || stat -c%s "$path" 2>/dev/null
}

job_dir_count() {
  local jobs_dir="$DATA_DIR/migration_exports/jobs"
  [ -d "$jobs_dir" ] || {
    printf '0\n'
    return
  }
  find "$jobs_dir" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d ' '
}

single_job_dir() {
  local jobs_dir="$DATA_DIR/migration_exports/jobs"
  find "$jobs_dir" -mindepth 1 -maxdepth 1 -type d
}

scale_manifest_matches() {
  local manifest_path="$1" expected_pages="$2" condition="$3" final_size="$4"
  [ -f "$manifest_path" ] || return 1
  jq -e --argjson expected_pages "$expected_pages" \
    --argjson condition "$condition" \
    --argjson final_size "$final_size" '
    .completed_objects.generation == $expected_pages
    and .completed_objects.count == $condition
    and .completed_objects.length == $final_size
  ' "$manifest_path" >/dev/null
}

minimum_distinct_sidecar_samples() {
  local expected_pages="$1"
  if [ "$expected_pages" -le 2 ]; then
    printf '%s\n' "$expected_pages"
  else
    printf '%s\n' $((expected_pages / 2 + 1))
  fi
}

capture_scale_trial_artifacts() {
  local job_dir="$1" candidate_dir="$2" observed_size="$3"
  local sidecar="$job_dir/completed_object_ids" live_manifest="$job_dir/manifest.json"
  local sampled_sidecar="$candidate_dir/completed_object_ids" sampled_size=""
  local latest_manifest="$candidate_dir/manifest.0.json"
  [ -f "$sidecar" ] || return 0

  [ ! -f "$candidate_dir/observed_size" ] || IFS= read -r sampled_size <"$candidate_dir/observed_size"
  if [ "$observed_size" != "$sampled_size" ]; then
    cp "$sidecar" "$sampled_sidecar.candidate" 2>/dev/null || return 0
    mv "$sampled_sidecar.candidate" "$sampled_sidecar"
    printf '%s\n' "$observed_size" >"$candidate_dir/observed_size"
    rm -f "$candidate_dir"/manifest.[012].json
  fi

  [ -f "$live_manifest" ] || return 0
  [ ! -f "$latest_manifest" ] || [ "$live_manifest" -nt "$latest_manifest" ] || return 0
  cp -p "$live_manifest" "$candidate_dir/manifest.candidate" 2>/dev/null || return 0
  rm -f "$candidate_dir/manifest.2.json"
  [ ! -f "$candidate_dir/manifest.1.json" ] \
    || mv "$candidate_dir/manifest.1.json" "$candidate_dir/manifest.2.json"
  [ ! -f "$latest_manifest" ] || mv "$latest_manifest" "$candidate_dir/manifest.1.json"
  mv "$candidate_dir/manifest.candidate" "$latest_manifest"
}

commit_scale_trial_artifact_pair() {
  local candidate_dir="$1" pair_dir="$2" expected_pages="$3" condition="$4"
  local sampled_sidecar="$candidate_dir/completed_object_ids" final_size manifest pair_candidate
  [ -f "$sampled_sidecar" ] || return 1
  final_size="$(file_size_bytes "$sampled_sidecar" || true)"
  [[ "$final_size" =~ ^[0-9]+$ ]] || return 1

  for manifest in "$candidate_dir"/manifest.[012].json; do
    [ -f "$manifest" ] || continue
    scale_manifest_matches "$manifest" "$expected_pages" "$condition" "$final_size" || continue
    pair_candidate="${pair_dir}.candidate.$$"
    [ ! -e "$pair_dir" ] || return 1
    mkdir "$pair_candidate" || return 1
    if cp "$sampled_sidecar" "$pair_candidate/completed_object_ids" \
      && cp "$manifest" "$pair_candidate/manifest.json" \
      && mv "$pair_candidate" "$pair_dir"; then
      return 0
    fi
    rm -r "$pair_candidate" 2>/dev/null || true
    return 1
  done
  return 1
}

sample_scale_trial() {
  local marker="$1" out="$2" jobs_dir="$DATA_DIR/migration_exports/jobs"
  local peak_rss=0 rss job_dir="" sidecar="" size last_size="" error="" interval_ms=10
  local sizes_file="${out}.sizes"
  local candidate_dir="${out}.candidates"
  mkdir "$candidate_dir"
  : >"$sizes_file"
  while [ -f "$marker" ]; do
    if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
      rss="$(ps -o rss= -p "$SERVER_PID" 2>/dev/null | tr -d ' ' || true)"
      if [[ "$rss" =~ ^[0-9]+$ ]] && [ "$rss" -gt "$peak_rss" ]; then
        peak_rss="$rss"
      fi
    fi

    if [ -d "$jobs_dir" ]; then
      local count
      count="$(job_dir_count)"
      if [ "$count" -gt 1 ]; then
        error="multiple job directories observed"
        break
      fi
      if [ "$count" = "1" ]; then
        job_dir="$(single_job_dir)"
        sidecar="$job_dir/completed_object_ids"
        if [ -f "$sidecar" ]; then
          size="$(file_size_bytes "$sidecar" || true)"
          if [[ "$size" =~ ^[0-9]+$ ]] && [ "$size" -gt 0 ]; then
            if [ "$size" != "$last_size" ]; then
              printf '%s\n' "$size" >>"$sizes_file"
              last_size="$size"
            fi
            capture_scale_trial_artifacts "$job_dir" "$candidate_dir" "$size"
          fi
        fi
      fi
    fi
    sleep 0.01
  done
  if [ -z "$error" ] && [ -d "$jobs_dir" ] && [ "$(job_dir_count)" = "1" ]; then
    job_dir="$(single_job_dir)"
    sidecar="$job_dir/completed_object_ids"
    if [ -f "$sidecar" ]; then
      size="$(file_size_bytes "$sidecar" || true)"
      if [[ "$size" =~ ^[0-9]+$ ]] && [ "$size" -gt 0 ]; then
        [ "$size" = "$last_size" ] || printf '%s\n' "$size" >>"$sizes_file"
        capture_scale_trial_artifacts "$job_dir" "$candidate_dir" "$size"
      fi
    fi
  fi
  jq -n --arg job_dir "$job_dir" --arg error "$error" --argjson peak_rss "$peak_rss" \
    --argjson interval_ms "$interval_ms" --slurpfile sizes <(jq -R 'tonumber' "$sizes_file" | jq -s '.') '
    {
      sampler_interval_milliseconds:$interval_ms,
      sampler_error:(if $error == "" then null else $error end),
      peak_rss_kb:$peak_rss,
      job_dir:$job_dir,
      sidecar_sizes:$sizes[0]
    }
  ' >"$out"
}

execute_migration_request() {
  local body="$1" out="$2"
  if [ "$EXPECT_MODE" = "scale" ] && [ "$TWO_POINT" -eq 1 ]; then
    measured_scale_migration_request "$body" "$out"
  else
    flapjack_request POST "/1/migrate-from-algolia" "$body" "$out"
  fi
}

measured_scale_migration_request() {
  local body="$1" out="$2" jobs_dir="$DATA_DIR/migration_exports/jobs"
  local before_count marker sample_out sampler_pid started_ms ended_ms rc elapsed_ms
  mkdir -p "$jobs_dir" "$CURRENT_TRIAL_DIR"
  before_count="$(job_dir_count)"
  [ "$before_count" = "0" ] || die "scale trial ${CURRENT_TRIAL_CONDITION}/${CURRENT_TRIAL_NUMBER} found unarchived migration job before request"

  marker="$CURRENT_TRIAL_DIR/sampler.running"
  sample_out="$CURRENT_TRIAL_DIR/sampler.json"
  : >"$marker"
  sample_scale_trial "$marker" "$sample_out" &
  sampler_pid=$!
  started_ms="$(current_milliseconds)"
  set +e
  flapjack_request POST "/1/migrate-from-algolia" "$body" "$out"
  rc=$?
  set -e
  ended_ms="$(current_milliseconds)"
  rm -f "$marker"
  wait "$sampler_pid" || die "scale trial sampler failed"
  elapsed_ms=$((ended_ms - started_ms))
  build_scale_trial_record "$sample_out" "$elapsed_ms" "$rc"
  return "$rc"
}

build_scale_trial_record() {
  local sample_out="$1" elapsed_ms="$2" curl_rc="$3"
  local expected_pages minimum_distinct_samples archive_dir job_dir final_size distinct_count total_rewritten manifest_path next job_archive_ref sampled_artifacts
  expected_pages=$(((CORPUS_SIZE + SCALE_BROWSE_PAGE_SIZE - 1) / SCALE_BROWSE_PAGE_SIZE))
  minimum_distinct_samples="$(minimum_distinct_sidecar_samples "$expected_pages")"
  [ "$curl_rc" = "0" ] || fail_scale_check "scale_request_budget" "scale migration request timed out or failed transport"
  jq -e '.sampler_error == null' "$sample_out" >/dev/null \
    || fail_scale_check "scale_sampler_completeness" "scale trial sampler reported an error"
  job_dir="$(jq -r '.job_dir // ""' "$sample_out")"
  [ -n "$job_dir" ] && [ -d "$job_dir" ] || die "scale trial ${CURRENT_TRIAL_CONDITION}/${CURRENT_TRIAL_NUMBER} did not create exactly one migration job"
  sampled_artifacts="${sample_out}.artifacts"
  commit_scale_trial_artifact_pair "${sample_out}.candidates" "$sampled_artifacts" \
    "$expected_pages" "$CORPUS_SIZE" || true
  archive_dir="$CURRENT_TRIAL_DIR/job"
  mv "$job_dir" "$archive_dir"
  if [ -d "$sampled_artifacts" ]; then
    cp "$sampled_artifacts/completed_object_ids" "$archive_dir/completed_object_ids.sampled" \
      || die "failed to archive sampled scale sidecar"
    cp "$sampled_artifacts/manifest.json" "$archive_dir/manifest.sampled.json" \
      || die "failed to archive sampled scale manifest"
  fi
  final_size=0
  [ -f "$sampled_artifacts/completed_object_ids" ] \
    && final_size="$(file_size_bytes "$sampled_artifacts/completed_object_ids")"
  distinct_count="$(jq '.sidecar_sizes | length' "$sample_out")"
  total_rewritten="$(jq '[.sidecar_sizes[]] | add // 0' "$sample_out")"
  manifest_path="$sampled_artifacts/manifest.json"
  if [ ! -f "$manifest_path" ]; then
    manifest_path="$CURRENT_TRIAL_DIR/missing-manifest.json"
    printf 'null\n' >"$manifest_path"
  fi
  next="${CURRENT_TRIAL_RECORD:-$CURRENT_TRIAL_DIR/trial-record.json}"
  job_archive_ref="logs/scale-trials/${CURRENT_TRIAL_CONDITION}/trial-${CURRENT_TRIAL_NUMBER}/job"
  local staged
  staged="$(mktemp)"
  jq -n --argjson condition "$CURRENT_TRIAL_CONDITION" --argjson trial "$CURRENT_TRIAL_NUMBER" \
    --arg target "$TARGET_INDEX" --arg job_archive "$job_archive_ref" \
    --argjson elapsed_ms "$elapsed_ms" --argjson budget_ms "$SCALE_REQUEST_BUDGET_MILLISECONDS" \
    --argjson curl_rc "$curl_rc" --argjson expected_pages "$expected_pages" \
    --argjson minimum_distinct_samples "$minimum_distinct_samples" \
    --argjson final_size "$final_size" --slurpfile sample "$sample_out" \
    --slurpfile manifest "$manifest_path" '
    {
      condition_n:$condition,
      trial:$trial,
      target_index:$target,
      job_archive:$job_archive,
      wall_clock_milliseconds:$elapsed_ms,
      request_budget_milliseconds:$budget_ms,
      request_exit_code:$curl_rc,
      peak_rss_kb:$sample[0].peak_rss_kb,
      sampler_interval_milliseconds:$sample[0].sampler_interval_milliseconds,
      sampler_error:$sample[0].sampler_error,
      sidecar_sizes_observed:$sample[0].sidecar_sizes,
      distinct_sizes_observed:($sample[0].sidecar_sizes | length),
      expected_page_count:$expected_pages,
      minimum_distinct_sizes_required:$minimum_distinct_samples,
      observed_sidecar_bytes_rewritten:([$sample[0].sidecar_sizes[]] | add // 0),
      final_sidecar_bytes:$final_size,
      manifest:(if ($manifest | length) == 0 then null else $manifest[0] end)
    }
  ' >"$staged"
  mv "$staged" "$next"
  CURRENT_TRIAL_RECORD="$next"

  [ "$elapsed_ms" -le "$SCALE_REQUEST_BUDGET_MILLISECONDS" ] \
    || fail_scale_check "scale_wall_clock_budget" "scale trial exceeded request wall-clock budget"
  [ "$distinct_count" -ge "$minimum_distinct_samples" ] \
    || fail_scale_check "scale_sidecar_samples_complete" "scale trial sidecar sample count did not match expected page count"
  [ "$total_rewritten" -gt 0 ] || fail_scale_check "scale_sidecar_samples_complete" "scale trial observed zero sidecar bytes rewritten"
  jq -e --argjson expected_pages "$expected_pages" \
    --argjson condition "$CURRENT_TRIAL_CONDITION" \
    --argjson final_size "$final_size" '
    .manifest.completed_objects.generation == $expected_pages
    and .manifest.completed_objects.count == $condition
    and .manifest.completed_objects.length == $final_size
  ' "$CURRENT_TRIAL_RECORD" >/dev/null \
    || fail_scale_check "scale_spool_manifest" "scale trial spool manifest counters did not match observed evidence"
}

source_algolia_request() {
  local method="$1" path="$2" body="$3" out="$4" status body_file=""
  set +e
  if [ -n "$body" ]; then
    body_file="$(mktemp "$WORK_DIR/source-curl-body.XXXXXX")" || {
      set -e
      die "failed to create source curl request body file"
    }
    chmod 600 "$body_file" 2>/dev/null || true
    printf '%s' "$body" >"$body_file" || {
      rm -f "$body_file"
      set -e
      die "failed to write source curl request body file"
    }
  fi
  {
    printf 'silent\n'
    printf 'show-error\n'
    printf 'request = "%s"\n' "$method"
    printf 'url = "https://%s.algolia.net%s"\n' "$SOURCE_APP_ID" "$path"
    printf 'header = "x-algolia-application-id: %s"\n' "$SOURCE_APP_ID"
    printf 'header = "x-algolia-api-key: %s"\n' "$SOURCE_API_KEY"
    printf 'header = "content-type: application/json"\n'
    if [ -n "$body_file" ]; then
      printf 'data-binary = "@%s"\n' "$body_file"
    fi
  } | curl -w '\n%{http_code}' --config - >"$out"
  status=$?
  [ -z "$body_file" ] || rm -f "$body_file"
  set -e
  return "$status"
}

source_algolia_json_request() {
  local method="$1" path="$2" body="$3" label="$4" code payload
  source_algolia_request "$method" "$path" "$body" "$LOG_DIR/${label}.raw" \
    || die "${label} source request transport failed"
  code="$(http_code <"$LOG_DIR/${label}.raw")"
  payload="$(http_body <"$LOG_DIR/${label}.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/${label}.json"
  http_success_code "$code" || die "${label} source request returned HTTP ${code}"
  jq -e 'type == "object"' "$LOG_DIR/${label}.json" >/dev/null \
    || die "${label} source response was malformed"
}

source_algolia_index_path() {
  local index_name="$1"
  encoded_index_path "$index_name"
}

wait_source_algolia_task() {
  local index_name="$1" task_id="$2" label="$3" remaining=40 status=""
  while [ "$remaining" -gt 0 ]; do
    source_algolia_json_request GET "$(source_algolia_index_path "$index_name")/task/${task_id}" "" "${label}-task"
    status="$(jq -r '.status // empty' "$LOG_DIR/${label}-task.json")"
    [ "$status" = "published" ] && return 0
    sleep 0.25
    remaining=$((remaining - 1))
  done
  die "${label} source task did not publish"
}

source_algolia_mutation() {
  local method="$1" index_name="$2" suffix="$3" body="$4" label="$5" task_id
  source_algolia_json_request "$method" "$(source_algolia_index_path "$index_name")${suffix}" "$body" "$label"
  task_id="$(jq -er '.taskID' "$LOG_DIR/${label}.json")" || die "${label} source response was missing taskID"
  wait_source_algolia_task "$index_name" "$task_id" "$label"
}

seed_replica_source_fixture() {
  local source_relevance source_relevance_topology_entry source_standard records primary_settings relevance_settings standard_settings
  [ "$SCENARIO" = "replicas" ] || return 0
  require_replica_names
  source_relevance="$(replica_source_relevance_index)"
  source_relevance_topology_entry="$(replica_source_relevance_topology_entry)"
  source_standard="$(replica_source_standard_index)"
  records='{"requests":[{"action":"addObject","body":{"objectID":"replica-001","name":"Replica Fixture","category":"replica","description":"same searchable text","primary_rank":300,"price":30,"standard_rank":20}},{"action":"addObject","body":{"objectID":"replica-002","name":"Replica Fixture","category":"replica","description":"same searchable text","primary_rank":200,"price":10,"standard_rank":10}},{"action":"addObject","body":{"objectID":"replica-003","name":"Replica Fixture","category":"replica","description":"same searchable text","primary_rank":100,"price":20,"standard_rank":30}}]}'
  primary_settings="$(jq -cn --arg relevance "$source_relevance_topology_entry" --arg standard "$source_standard" \
    '{customRanking:["desc(primary_rank)"], replicas:[$relevance,$standard]}')"
  relevance_settings='{"customRanking":["asc(price)"],"relevancyStrictness":80}'
  standard_settings='{"ranking":["desc(standard_rank)","typo","geo","words","filters","proximity","attribute","exact","custom"],"customRanking":[],"relevancyStrictness":100}'

  source_algolia_mutation POST "$SOURCE_INDEX" "/batch" "$records" "replica-source-primary-batch"
  source_algolia_mutation PUT "$SOURCE_INDEX" "/settings" "$primary_settings" "replica-source-primary-settings"
  source_algolia_mutation PUT "$source_relevance" "/settings" "$relevance_settings" "replica-source-relevance-settings"
  source_algolia_mutation PUT "$source_standard" "/settings" "$standard_settings" "replica-source-standard-settings"
  record_check "replica_source_fixture" "pass" "seeded three records and two source replicas"
}

migration_payload() {
  local key="${1:-$SOURCE_API_KEY}" target="${2:-$TARGET_INDEX}"
  local app_json key_json source_json target_json
  app_json="$(printf '%s' "$SOURCE_APP_ID" | jq -Rs .)"
  key_json="$(printf '%s' "$key" | jq -Rs .)"
  source_json="$(printf '%s' "$SOURCE_INDEX" | jq -Rs .)"
  target_json="$(printf '%s' "$target" | jq -Rs .)"
  printf '{"appId":%s,"apiKey":%s,"sourceIndex":%s,"targetIndex":%s}\n' \
    "$app_json" "$key_json" "$source_json" "$target_json"
}

encoded_index_path() {
  local index_name="$1"
  printf '/1/indexes/%s' "$(algolia_vendor_url_encode "$index_name")"
}

query_index() {
  local index_name="$1" label="$2" request="$3" path code payload
  path="$(encoded_index_path "$index_name")/query"
  flapjack_request POST "$path" "$request" "$LOG_DIR/${label}.raw" \
    || die "${label} query transport failed"
  code="$(http_code <"$LOG_DIR/${label}.raw")"
  payload="$(http_body <"$LOG_DIR/${label}.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/${label}.json"
  http_success_code "$code" || die "${label} query returned HTTP ${code}"
  jq -e 'type == "object" and (.hits | type == "array")' "$LOG_DIR/${label}.json" >/dev/null \
    || die "${label} query response was malformed"
}

browse_index() {
  local index_name="$1" label="$2" request="$3" path code payload
  path="$(encoded_index_path "$index_name")/browse"
  flapjack_request POST "$path" "$request" "$LOG_DIR/${label}.raw" \
    || die "${label} browse transport failed"
  code="$(http_code <"$LOG_DIR/${label}.raw")"
  payload="$(http_body <"$LOG_DIR/${label}.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/${label}.json"
  http_success_code "$code" || die "${label} browse returned HTTP ${code}"
  jq -e '
    type == "object"
    and (.hits | type == "array")
    and ((.cursor == null) or (.cursor | type == "string"))
  ' "$LOG_DIR/${label}.json" >/dev/null \
    || die "${label} browse response was malformed"
}

read_live_scale_source_count() {
  local count
  count="$("$SCALE_FIXTURE" source-count --index "$SOURCE_INDEX" \
    --secret-file "$SECRET_FILE" --work-dir "$WORK_DIR")" \
    || die "scale source count query failed"
  [[ "$count" =~ ^[0-9]+$ ]] || die "scale source count helper returned a malformed count"
  printf '%s\n' "$count"
}

# Diagnostic-only read of the migrated target's attributesForFaceting. Never aborts the run:
# it is called from a failure path that is already about to exit, so a transport or parse
# problem here must degrade to a marker string rather than mask the real facet mismatch.
read_target_attributes_for_faceting() {
  local path code
  path="$(encoded_index_path "$TARGET_INDEX")/settings"
  if ! flapjack_request GET "$path" "" "$LOG_DIR/scale-facets-settings.raw"; then
    printf '%s\n' '"<settings transport failed>"'
    return 0
  fi
  code="$(http_code <"$LOG_DIR/scale-facets-settings.raw")"
  http_body <"$LOG_DIR/scale-facets-settings.raw" >"$LOG_DIR/scale-facets-settings.json"
  if ! http_success_code "$code"; then
    printf '"<settings HTTP %s>"\n' "$code"
    return 0
  fi
  jq -c '.attributesForFaceting // "<absent>"' "$LOG_DIR/scale-facets-settings.json" 2>/dev/null \
    || printf '%s\n' '"<settings unparseable>"'
}

fail_scale_check() {
  local name="$1" detail="$2"
  record_check "$name" "fail" "$detail"
  die "$detail"
}

assert_scale_aggregates() {
  local source_count="$1" request target_total expected_final
  local page_size=1000 page=0 page_count label ids_file fetched_count duplicate_count

  request='{"query":"","hitsPerPage":0}'
  query_index "$TARGET_INDEX" "scale-target-total" "$request"
  target_total="$(jq -er '
    if (.nbHits | type) == "number" and .nbHits >= 0 and (.nbHits | floor) == .nbHits
    then .nbHits else empty end
  ' "$LOG_DIR/scale-target-total.json")" || fail_scale_check "scale_target_total" "scale target total response was malformed"
  [ "$target_total" = "$source_count" ] \
    || fail_scale_check "scale_target_total" "scale target total did not equal live source count"
  record_check "scale_target_total" "pass" "nbHits=${target_total}"

  ids_file="$LOG_DIR/scale-object-ids.txt"
  : >"$ids_file"
  page_count=$(((source_count + page_size - 1) / page_size))
  while [ "$page" -lt "$page_count" ]; do
    printf -v label 'scale-object-ids-page-%06d' "$page"
    request="$(jq -cn --argjson page "$page" --argjson page_size "$page_size" \
      '{query:"",page:$page,hitsPerPage:$page_size,attributesToRetrieve:["objectID"],attributesToHighlight:[],attributesToSnippet:[]}')"
    query_index "$TARGET_INDEX" "$label" "$request"
    jq -e --argjson source_count "$source_count" '
      .nbHits == $source_count
      and all(.hits[]; (.objectID | type) == "string" and (.objectID | length) > 0)
    ' "$LOG_DIR/${label}.json" >/dev/null \
      || fail_scale_check "scale_object_id_coverage" "scale target objectID page response was malformed"
    jq -r '.hits[].objectID' "$LOG_DIR/${label}.json" >>"$ids_file"
    page=$((page + 1))
  done

  expected_final="$(jq -r '.aggregate_expectations.final_object_id' "$VERIFICATION_MANIFEST")"
  grep -Fxq "$expected_final" "$ids_file" \
    || fail_scale_check "scale_object_id_coverage" "scale target did not contain expected final objectID"
  duplicate_count="$(sort "$ids_file" | uniq -d | wc -l | tr -d ' ')"
  [ "$duplicate_count" = "0" ] \
    || fail_scale_check "scale_object_id_coverage" "scale target returned duplicate objectID values"
  fetched_count="$(wc -l <"$ids_file" | tr -d ' ')"
  [ "$fetched_count" = "$source_count" ] \
    || fail_scale_check "scale_object_id_coverage" "scale target paged objectID count did not equal live source count"
  record_check "scale_object_id_coverage" "pass" "${fetched_count} unique objectIDs; final=${expected_final}"

  request='{"query":"","hitsPerPage":0,"facets":["category","color"],"maxValuesPerFacet":100}'
  query_index "$TARGET_INDEX" "scale-facets" "$request"
  if ! jq -e --argjson source_count "$source_count" --slurpfile manifest "$VERIFICATION_MANIFEST" '
    .nbHits == $source_count
    and .facets == $manifest[0].aggregate_expectations.facets
  ' "$LOG_DIR/scale-facets.json" >/dev/null; then
    # A bare "did not match" here costs a whole 20,000-document live rerun to diagnose.
    # Capture the observed facet map alongside the target's migrated attributesForFaceting so
    # the receipt distinguishes "faceting setting never migrated" from "setting migrated but
    # documents were indexed without it" without re-seeding the corpus.
    fail_scale_check "scale_facets" \
      "scale target facets did not exactly match expected counts; expected=$(jq -c '.aggregate_expectations.facets' "$VERIFICATION_MANIFEST") observed=$(jq -c '{nbHits, facets}' "$LOG_DIR/scale-facets.json") target_attributesForFaceting=$(read_target_attributes_for_faceting)"
  fi
  record_check "scale_facets" "pass" "category and color counts exactly matched"
}

target_listing_count() {
  local target="$1" label="$2" code payload
  flapjack_request GET "/1/indexes" "" "$LOG_DIR/${label}.raw" \
    || return 2
  code="$(http_code <"$LOG_DIR/${label}.raw")"
  payload="$(http_body <"$LOG_DIR/${label}.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/${label}.json"
  http_success_code "$code" || return 2
  jq -er --arg target "$target" \
    '[.items[]? | select(.name == $target)] | if length <= 1 then length else error("duplicate target") end' \
    "$LOG_DIR/${label}.json"
}

assert_target_list_exactly_once() {
  local label="$1"
  flapjack_request GET "/1/indexes" "" "$LOG_DIR/${label}.raw" \
    || die "${label} list-indices request transport failed"
  local code payload
  code="$(http_code <"$LOG_DIR/${label}.raw")"
  payload="$(http_body <"$LOG_DIR/${label}.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/${label}.json"
  http_success_code "$code" || die "${label} GET /1/indexes returned HTTP ${code}"
  jq -e \
    --arg primary "$TARGET_INDEX" \
    --arg relevance "$(replica_target_relevance_index)" \
    --arg standard "$(replica_target_standard_index)" '
      def count_name($name): [.items[]? | select(.name == $name)] | length;
      count_name($primary) == 1
      and count_name($relevance) == 1
      and count_name($standard) == 1
    ' "$LOG_DIR/${label}.json" >/dev/null || die "replica scenario target list did not expose primary and replicas exactly once"
  record_check "replica_public_list" "pass" "primary and two replicas listed exactly once"
}

delete_verified_target() {
  local target="$1" label="$2" listed code payload remaining=40
  listed="$(target_listing_count "$target" "cleanup-${label}-before")" || return 1
  if [ "$listed" = "0" ]; then
    return 0
  fi

  flapjack_request DELETE "$(encoded_index_path "$target")" "" "$LOG_DIR/cleanup-${label}-delete.raw" \
    || return 1
  code="$(http_code <"$LOG_DIR/cleanup-${label}-delete.raw")"
  payload="$(http_body <"$LOG_DIR/cleanup-${label}-delete.raw")"
  printf '%s\n' "$payload" >"$LOG_DIR/cleanup-${label}-delete.json"
  http_success_code "$code" || return 1

  while [ "$remaining" -gt 0 ]; do
    listed="$(target_listing_count "$target" "cleanup-${label}-poll")" || return 1
    [ "$listed" = "0" ] && return 0
    sleep 0.25
    remaining=$((remaining - 1))
  done
  return 1
}

cleanup_verified_targets() {
  [ -n "$VERIFICATION_MANIFEST" ] || return 0
  [ "$VERIFICATION_TARGETS_CLEANED" -eq 0 ] || return 0
  [ -n "$BASE_URL" ] || return 1
  [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null || return 1

  local failed=0
  delete_verified_target "$TARGET_INDEX" "primary" || failed=1
  delete_verified_target "${TARGET_INDEX}_conflict" "conflict" || failed=1
  delete_verified_target "${TARGET_INDEX}_invalid_key" "invalid-key" || failed=1
  [ "$failed" -eq 0 ] || return 1
  VERIFICATION_TARGETS_CLEANED=1
}

cleanup_scale_fixture() {
  [ "$EXPECT_MODE" = "scale" ] || return 0
  [ "$SCALE_FIXTURE_CLEANED" -eq 0 ] || return 0
  [ -n "$SCALE_FIXTURE_LEDGER" ] || return 0
  [ -f "$SCALE_FIXTURE_LEDGER" ] || return 1
  "$SCALE_FIXTURE" cleanup --ledger "$SCALE_FIXTURE_LEDGER" --secret-file "$SECRET_FILE" || return 1
  SCALE_FIXTURE_CLEANED=1
}

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
}

# Asks the vendor itself how many documents the fixture holds. Without this a
# zero-document source would let the whole contract pass by absence.
async_assert_source_seeded() {
  local expected observed out
  expected="$(async_fixture_document_count)"
  [ "$expected" -gt 0 ] || die "async fixture document set is empty"
  out="$LOG_DIR/async-source-count.json"
  async_vendor_json write POST \
    "/1/indexes/$(algolia_vendor_url_encode "$SOURCE_INDEX")/query" \
    '{"query":"","hitsPerPage":0}' "$out"
  observed="$(jq -er '
    if (.nbHits | type) == "number" and (.nbHits | floor) == .nbHits then .nbHits else empty end
  ' "$out")" || die "async source count response was malformed"
  [ "$observed" = "$expected" ] \
    || die "async source fixture held ${observed} documents, expected ${expected}"
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
  record_check "cancel_vendor_reachable" "pass" "GET https://${ALGOLIA_APP_ID}-dsn.algolia.net/1/indexes returned 200"

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

assert_async_job() {
  submit_async_migration "$(migration_payload)"
  poll_async_job_until_terminal
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

median_of_numbers() {
  jq -s 'sort as $v
    | if ($v | length) % 2 == 1 then $v[(length / 2 | floor)]
      else (($v[(length / 2) - 1] + $v[(length / 2)]) / 2)
      end'
}

append_scale_trial_record() {
  local condition="$1" trial_record="$2" next
  next="$(mktemp)"
  jq --argjson condition "$condition" --arg source "$SOURCE_INDEX" --slurpfile trial "$trial_record" '
    .scale.conditions_observed |= (
      if any(.[]; .n == $condition) then
        map(if .n == $condition then .source_index = $source | .trials += [$trial[0]] else . end)
      else
        . + [{n:$condition,source_index:$source,trials:[$trial[0]]}]
      end
    )
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

summarize_scale_condition() {
  local condition="$1" next
  next="$(mktemp)"
  jq --argjson condition "$condition" '
    def median:
      sort as $v
      | if ($v | length) % 2 == 1 then $v[(length / 2 | floor)]
        else (($v[(length / 2) - 1] + $v[(length / 2)]) / 2)
        end;
    .scale.conditions_observed |= map(
      if .n == $condition then
        .summary = {
          trial_count:(.trials | length),
          wall_clock_milliseconds:{
            min:([.trials[].wall_clock_milliseconds] | min),
            median:([.trials[].wall_clock_milliseconds] | median),
            max:([.trials[].wall_clock_milliseconds] | max)
          },
          peak_rss_kb:{
            min:([.trials[].peak_rss_kb] | min),
            median:([.trials[].peak_rss_kb] | median),
            max:([.trials[].peak_rss_kb] | max)
          },
          observed_sidecar_bytes_rewritten:([.trials[].observed_sidecar_bytes_rewritten] | add),
          final_sidecar_bytes:([.trials[].final_sidecar_bytes] | max),
          distinct_sizes_observed:([.trials[].distinct_sizes_observed] | min),
          expected_page_count:([.trials[].expected_page_count] | max)
        }
      else . end
    )
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

record_two_point_ratio() {
  local next ratio_status
  next="$(mktemp)"
  jq --argjson ceiling "$SCALE_REWRITE_GROWTH_CEILING" '
    (.scale.conditions_observed | sort_by(.n)) as $conditions
    | ($conditions[0].summary.observed_sidecar_bytes_rewritten) as $small
    | ($conditions[1].summary.observed_sidecar_bytes_rewritten) as $large
    | .scale.two_point_observed_rewrite_ratio = (if $small == 0 then null else ($large / $small) end)
    | .scale.two_point_rewrite_growth_ceiling = $ceiling
    | .scale.two_point_ratio_status = (
        if $small == 0 then "fail"
        elif ($large / $small) > $ceiling then "breach"
        else "pass" end
      )
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
  ratio_status="$(jq -r '.scale.two_point_ratio_status' "$RECEIPT")"
  case "$ratio_status" in
    pass)
      record_check "scale_rewrite_growth_ceiling" "pass" "observed ratio within ceiling"
      ;;
    breach)
      record_check "scale_rewrite_growth_ceiling" "fail" "observed ratio exceeded ceiling"
      die "observed ratio exceeded ceiling"
      ;;
    *)
      record_check "scale_rewrite_growth_ceiling" "fail" "observed ratio unavailable"
      die "observed ratio unavailable"
      ;;
  esac
}

run_scale_condition_trials() {
  local condition="$1" trial base_target
  CORPUS_SIZE="$condition"
  prepare_scale_fixture
  update_scale_owned_sources
  base_target="$TARGET_INDEX"
  trial=1
  while [ "$trial" -le "$SCALE_TRIAL_COUNT" ]; do
    TARGET_INDEX="${base_target}_trial_${trial}"
    VERIFICATION_MANIFEST="$WORK_DIR/scale-verification-manifest-${condition}.json"
    "$GENERATOR" manifest --corpus-size "$CORPUS_SIZE" >"$VERIFICATION_MANIFEST"
    cp "$VERIFICATION_MANIFEST" "$LOG_DIR/source-manifest-${condition}-trial-${trial}.json"
    CURRENT_TRIAL_CONDITION="$condition"
    CURRENT_TRIAL_NUMBER="$trial"
    CURRENT_TRIAL_DIR="$LOG_DIR/scale-trials/${condition}/trial-${trial}"
    mkdir -p "$CURRENT_TRIAL_DIR"
    VERIFICATION_TARGETS_CLEANED=0
    register_scale_trial_targets
    CURRENT_TRIAL_RECORD="$CURRENT_TRIAL_DIR/trial-record.json"
    assert_importing
    archive_unmeasured_jobs "postcheck"
    append_scale_trial_record "$condition" "$CURRENT_TRIAL_RECORD"
    record_check "scale_trial_${condition}_${trial}" "pass" "target=${TARGET_INDEX}"
    trial=$((trial + 1))
  done
  summarize_scale_condition "$condition"
  cleanup_scale_fixture || die "scale fixture cleanup failed or left residue"
  record_check "scale_source_cleanup_${condition}" "pass" "all owned Algolia source indices absent"
  SCALE_FIXTURE_CLEANED=0
  SCALE_FIXTURE_LEDGER=""
}

archive_unmeasured_jobs() {
  local label="$1" jobs_dir="$DATA_DIR/migration_exports/jobs" destination job name count
  [ -d "$jobs_dir" ] || return 0
  count="$(job_dir_count)"
  [ "$count" -eq 0 ] && return 0
  destination="$CURRENT_TRIAL_DIR/${label}-jobs"
  mkdir -p "$destination"
  while [ "$(job_dir_count)" -gt 0 ]; do
    job="$(single_job_dir | head -1)"
    [ -n "$job" ] || break
    name="$(basename "$job")"
    mv "$job" "$destination/$name"
  done
}

register_scale_trial_targets() {
  local next conflict_target invalid_target
  conflict_target="${TARGET_INDEX}_conflict"
  invalid_target="${TARGET_INDEX}_invalid_key"
  next="$(mktemp)"
  jq --arg target "$TARGET_INDEX" --arg conflict "$conflict_target" --arg invalid "$invalid_target" '
    .target_index = $target
    | .owned_resources.targets = ((.owned_resources.targets + [$target, $conflict, $invalid]) | map(select(length > 0)) | unique)
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

update_scale_owned_sources() {
  local next
  next="$(mktemp)"
  jq --slurpfile ledger "$SCALE_FIXTURE_LEDGER" '
    .owned_resources.algolia_sources = (
      ((.owned_resources.algolia_sources // []) + [
        $ledger[0].algolia_sources[]? | select(.owned == true) | .name
      ]) | unique
    )
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

assert_scale_two_point() {
  run_scale_condition_trials 2000
  run_scale_condition_trials 20000
  record_two_point_ratio
}

record_replica_sequences() {
  local primary="$1" relevance="$2" standard="$3" next
  next="$(mktemp)"
  jq \
    --argjson expected_primary '["replica-001","replica-002","replica-003"]' \
    --argjson expected_virtual '["replica-002","replica-003","replica-001"]' \
    --argjson expected_standard '["replica-003","replica-001","replica-002"]' \
    --argjson observed_primary "$primary" \
    --argjson observed_virtual "$relevance" \
    --argjson observed_standard "$standard" \
    '
      .replica_sequences = {
        expected:{primary:$expected_primary, virtual:$expected_virtual, standard:$expected_standard},
        observed:{primary:$observed_primary, virtual:$observed_virtual, standard:$observed_standard}
      }
    ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

assert_replica_warnings() {
  jq -e '
    (.warnings | type) == "array"
    and ([.warnings[].code] | index("ReplicaExhaustiveSortApproximated")) != null
    and ([.warnings[].code] | index("ReplicaRelevancyStrictnessSemanticMismatch")) != null
    and all(.warnings[];
      type == "object"
      and (.code | type) == "string"
      and (.code as $code
        | ([
            "ReplicaExhaustiveSortApproximated",
            "ReplicaRelevancyStrictnessSemanticMismatch",
            "PersistedNoBehaviorSetting",
            "ReadOnlySourceField"
          ] | index($code)) != null)
    )
  ' "$LOG_DIR/migration-response.json" >/dev/null \
    || die "replica scenario migration warnings contained a sidecar/materialization failure or lacked the documented replica warnings"
  record_check "replica_migration_warnings" "pass" "only documented benign warnings observed"
}

assert_replica_order() {
  local index_name="$1" label="$2" expected="$3" observed
  query_index "$index_name" "$label" '{"query":"","hitsPerPage":3}'
  observed="$(jq -c '[.hits[].objectID]' "$LOG_DIR/${label}.json")"
  [ "$observed" = "$expected" ] || die "${label} order mismatch: expected ${expected}, observed ${observed}"
  record_check "${label//-/_}" "pass" "$observed"
  printf '%s\n' "$observed"
}

assert_replica_hit_sets_equal() {
  jq -n -e \
    --slurpfile primary "$LOG_DIR/replica-primary-order.json" \
    --slurpfile relevance "$LOG_DIR/replica-virtual-order.json" \
    --slurpfile standard "$LOG_DIR/replica-standard-order.json" '
      def ids($doc): $doc[0].hits | map(.objectID) | sort;
      ids($primary) == ids($relevance) and ids($primary) == ids($standard)
    ' >/dev/null || die "replica hit sets did not equal the primary hit set"
  record_check "replica_hit_sets" "pass" "primary and replicas returned identical objectID sets"
}

assert_virtual_sidecar_structure() {
  local index_name="$1" label="$2" jq_filter="$3" index_dir find_log
  index_dir="$DATA_DIR/$index_name"
  find_log="$LOG_DIR/${label}-sidecar-find.txt"
  [ -f "$index_dir/settings.json" ] || die "${label} sidecar settings.json was missing"
  # Preserve the actual sidecar settings in evidence BEFORE asserting on them, so a
  # mismatch is diagnosable after the data dir is torn down.
  cp "$index_dir/settings.json" "$LOG_DIR/${label}-settings-actual.json" 2>/dev/null || true
  find "$index_dir" -maxdepth 2 -print | sort >"$find_log"
  jq -e --arg primary "$TARGET_INDEX" "$jq_filter" "$index_dir/settings.json" >"$LOG_DIR/${label}-settings-proof.json" \
    || die "${label} sidecar settings did not match translated replica settings"
  [ ! -e "$index_dir/meta.json" ] || die "${label} sidecar had physical meta.json"
  if find "$index_dir" -mindepth 1 ! -name settings.json -print -quit | grep -q .; then
    die "${label} sidecar had Tantivy or document-store artifacts"
  fi
  record_check "${label}_sidecar" "pass" "settings-only virtual sidecar"
}

assert_replica_check_receipt_guard() {
  local count
  count="$(jq -er '[.checks[]? | select(.name | contains("replica"))] | length' "$RECEIPT")" \
    || die "VACUOUS: replica check receipt could not be read"
  [ "$count" -gt 0 ] || die "VACUOUS: replica scenario recorded zero replica checks"
  jq -e '
    all(.checks[]? | select(.name | contains("replica")); .status == "pass" or .status == "fail")
    and all(.checks[]? | select(.name | test("sidecar|order|hit_sets|public_list|warnings|fixture|cleanup")); .name | contains("replica"))
  ' "$RECEIPT" >/dev/null || die "replica scenario receipt checks were vacuous or had invalid statuses"
}

assert_replica_scenario_import() {
  local primary_ids relevance_ids standard_ids
  assert_replica_warnings
  assert_target_list_exactly_once "replica-list-indices"
  primary_ids="$(assert_replica_order "$TARGET_INDEX" "replica-primary-order" '["replica-001","replica-002","replica-003"]')"
  relevance_ids="$(assert_replica_order "$(replica_target_relevance_index)" "replica-virtual-order" '["replica-002","replica-003","replica-001"]')"
  standard_ids="$(assert_replica_order "$(replica_target_standard_index)" "replica-standard-order" '["replica-003","replica-001","replica-002"]')"
  assert_replica_hit_sets_equal
  # Sidecar settings.json is a full IndexSettings serialization (defaults included),
  # so assert the load-bearing translated fields exactly rather than whole-object
  # equality against a hand-written minimal object (live evidence 2026-07-19).
  assert_virtual_sidecar_structure "$(replica_target_relevance_index)" "replica_virtual" \
    'if (.primary == $primary)
      and (.customRanking == ["asc(price)"])
      and (.relevancyStrictness == 80)
    then . else empty end'
  # Translation consumes the trailing "custom" ranking token (it enables appending
  # customRanking) and normalizes default-equivalent relevancyStrictness (100) away —
  # both pinned by translation_tests.rs; assert the product contract, not the fake's.
  assert_virtual_sidecar_structure "$(replica_target_standard_index)" "replica_standard" \
    'if (.primary == $primary)
      and (.ranking == ["typo","geo","words","filters","proximity","attribute","exact"])
      and (.customRanking == ["desc(standard_rank)"])
      and (.relevancyStrictness == null)
    then . else empty end'
  record_replica_sequences "$primary_ids" "$relevance_ids" "$standard_ids"
  assert_replica_check_receipt_guard
}

update_counts() {
  local source_count="$1" target_count="$2" next
  next="$(mktemp)"
  jq --argjson source_count "$source_count" --arg target_count "$target_count" '
    .counts.source_count = $source_count
    | if $target_count == "" then . else .counts.target_count = ($target_count | tonumber) end
  ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

update_verified_counts() {
  local source_count="$1" target_count="$2" synonym_count="$3" rule_count="$4" next
  next="$(mktemp)"
  jq --argjson source_count "$source_count" --argjson target_count "$target_count" \
    --argjson synonym_count "$synonym_count" --argjson rule_count "$rule_count" '
      .counts = {
        source_count:$source_count,
        target_count:$target_count,
        synonym_count:$synonym_count,
        rule_count:$rule_count
      }
    ' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

finish_receipt() {
  local status="$1" completed_at elapsed next
  completed_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  if [ -n "$RUN_STARTED_EPOCH" ]; then
    elapsed=$(( $(date +%s) - RUN_STARTED_EPOCH ))
  else
    elapsed=0
  fi
  next="$(mktemp)"
  jq --arg status "$status" --arg completed_at "$completed_at" --argjson elapsed "$elapsed" \
    '.status = $status | .completed_at = $completed_at | .elapsed_seconds = $elapsed' "$RECEIPT" >"$next"
  mv "$next" "$RECEIPT"
}

copy_evidence_path() {
  local source="$1" target="$2"
  cp -R "$source" "$target" 2>/dev/null || EVIDENCE_COPY_FAILED=1
}

preserve_run_evidence() {
  local announce="${1:-1}"
  if [ -z "$EVIDENCE_DIR" ]; then
    if [ -n "${MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT:-}" ]; then
      mkdir -p "$MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT" 2>/dev/null || EVIDENCE_COPY_FAILED=1
      EVIDENCE_DIR="${MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT%/}/$(date +%Y%m%dT%H%M%SZ)_${RUN_PREFIX:-migration_import_$$}"
    else
      EVIDENCE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack_migration_import_contract_evidence.XXXXXX")"
    fi
    mkdir -p "$EVIDENCE_DIR"
    chmod 700 "$EVIDENCE_DIR" 2>/dev/null || true
    [ -n "$LOG_DIR" ] && [ -d "$LOG_DIR" ] && copy_evidence_path "$LOG_DIR" "$EVIDENCE_DIR/logs"
    [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ] && cp "$RECEIPT" "$EVIDENCE_DIR/receipt.json" 2>/dev/null || EVIDENCE_COPY_FAILED=1
    if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR/migration_exports/jobs" ]; then
      mkdir -p "$EVIDENCE_DIR/migration_exports"
      copy_evidence_path "$DATA_DIR/migration_exports/jobs" "$EVIDENCE_DIR/migration_exports/jobs"
    fi
  fi
  if [ "$announce" -eq 1 ] && [ "$EVIDENCE_ANNOUNCED" -eq 0 ]; then
    printf 'INFO: preserved sanitized migration import evidence at %s\n' "$EVIDENCE_DIR" >&2
    EVIDENCE_ANNOUNCED=1
  fi
}

refresh_run_evidence() {
  [ -n "$EVIDENCE_DIR" ] && [ -d "$EVIDENCE_DIR" ] || return 0
  if [ -n "$LOG_DIR" ] && [ -d "$LOG_DIR" ]; then
    rm -rf "$EVIDENCE_DIR/logs" 2>/dev/null || EVIDENCE_COPY_FAILED=1
    copy_evidence_path "$LOG_DIR" "$EVIDENCE_DIR/logs"
  fi
  if [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ]; then
    cp "$RECEIPT" "$EVIDENCE_DIR/receipt.json" 2>/dev/null || EVIDENCE_COPY_FAILED=1
  fi
  if [ -n "$DATA_DIR" ] && [ -d "$DATA_DIR/migration_exports/jobs" ]; then
    rm -rf "$EVIDENCE_DIR/migration_exports/jobs" 2>/dev/null || EVIDENCE_COPY_FAILED=1
    mkdir -p "$EVIDENCE_DIR/migration_exports"
    copy_evidence_path "$DATA_DIR/migration_exports/jobs" "$EVIDENCE_DIR/migration_exports/jobs"
  fi
}

current_mode_fixture_needs_cleanup() {
  case "$EXPECT_MODE" in
    scale) [ "$SCALE_FIXTURE_CLEANED" -eq 0 ] ;;
    async_job) [ "$ASYNC_FIXTURE_CLEANED" -eq 0 ] ;;
    importing) [ "$SCENARIO" = "cancel" ] && [ "$CANCEL_FIXTURE_CLEANED" -eq 0 ] ;;
    *) return 1 ;;
  esac
}

# Owns the common receipt and error semantics for scenario fixtures. Scenario
# cleanup functions still own their resources; this helper prevents their trap
# orchestration from drifting apart.
cleanup_current_mode_fixture() {
  local cleanup_function check_name pass_detail fail_detail error_message
  case "$EXPECT_MODE" in
    scale)
      cleanup_function="cleanup_scale_fixture"
      check_name="scale_source_cleanup"
      pass_detail="all owned Algolia source indices absent"
      fail_detail="fixture cleanup failed or residue remained"
      error_message="scale fixture cleanup failed or left residue"
      ;;
    async_job)
      cleanup_function="cleanup_async_scenario"
      check_name="async_fixture_cleanup"
      pass_detail="all registered async indices absent"
      fail_detail="async cleanup failed or residue remained"
      error_message="async fixture cleanup failed or left residue"
      ;;
    importing)
      [ "$SCENARIO" = "cancel" ] || return 0
      cleanup_function="cleanup_cancel_scenario"
      check_name="cancel_fixture_cleanup"
      pass_detail="all registered fj_cancel_ Algolia indices absent"
      fail_detail="cancel cleanup failed or residue remained"
      error_message="cancel fixture cleanup failed or left residue"
      ;;
    *) return 0 ;;
  esac

  if "$cleanup_function"; then
    [ -f "$RECEIPT" ] && record_check "$check_name" "pass" "$pass_detail"
    return 0
  fi

  CLEANUP_FAILED=1
  [ -f "$RECEIPT" ] && record_check "$check_name" "fail" "$fail_detail"
  printf 'ERROR: %s\n' "$error_message" >&2
  return 1
}

cleanup() {
  local script_exit_code=$?
  local effective_exit_code="$script_exit_code"
  [ "$INTERRUPTED_EXIT_CODE" -eq 0 ] || effective_exit_code="$INTERRUPTED_EXIT_CODE"
  trap - EXIT INT TERM
  set +e

  if [ "${MIGRATION_IMPORT_CONTRACT_SIMULATE_CLEANUP_FAILURE:-0}" = "1" ]; then
    CLEANUP_FAILED=1
    printf 'ERROR: simulated cleanup failure\n' >&2
  fi

  if [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ] && [ "$PASS_COMPLETE" -ne 1 ]; then
    finish_receipt "failed"
  fi

  if [ "$PASS_COMPLETE" -ne 1 ] || [ "$effective_exit_code" -ne 0 ] || [ "$CLEANUP_FAILED" -ne 0 ] || [ -n "${MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT:-}" ]; then
    preserve_run_evidence 1
  fi

  if [ "$SCENARIO" = "replicas" ] && [ "$REPLICA_SOURCE_FIXTURE_CLEANED" -eq 0 ] && [ -n "$RECEIPT" ] && [ -f "$RECEIPT" ]; then
    if ! cleanup_replica_source_fixture; then
      CLEANUP_FAILED=1
      printf 'ERROR: replica source fixture cleanup failed\n' >&2
      [ -f "$RECEIPT" ] && finish_receipt "failed"
    fi
    refresh_run_evidence
  fi

  if [ -n "$VERIFICATION_MANIFEST" ] && [ "$VERIFICATION_TARGETS_CLEANED" -eq 0 ]; then
    if cleanup_verified_targets; then
      [ -f "$RECEIPT" ] && record_check "failure_target_cleanup" "pass" "all ledgered target names absent"
    else
      CLEANUP_FAILED=1
      [ -f "$RECEIPT" ] && record_check "failure_target_cleanup" "fail" "cleanup failed or exact-name residue remained"
      printf 'ERROR: verified target cleanup failed or left residue\n' >&2
    fi
    [ -f "$RECEIPT" ] && finish_receipt "failed"
    refresh_run_evidence
  fi

  if current_mode_fixture_needs_cleanup; then
    cleanup_current_mode_fixture || true
    if [ -f "$RECEIPT" ] && { [ "$PASS_COMPLETE" -ne 1 ] || [ "$CLEANUP_FAILED" -ne 0 ]; }; then
      finish_receipt "failed"
    fi
    if [ "$CLEANUP_FAILED" -ne 0 ]; then
      preserve_run_evidence 1
    fi
    refresh_run_evidence
  fi

  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi

  [ -z "$WORK_DIR" ] || rm -rf "$WORK_DIR" 2>/dev/null || CLEANUP_FAILED=1

  refresh_run_evidence

  if { [ "$CLEANUP_FAILED" -ne 0 ] || [ "$EVIDENCE_COPY_FAILED" -ne 0 ]; } && [ "$effective_exit_code" -eq 0 ]; then
    exit 1
  fi
  if [ "$effective_exit_code" -ne "$script_exit_code" ]; then
    exit "$effective_exit_code"
  fi
}

main() {
  ORIGINAL_ARGS=("$@")
  parse_args "$@"
  load_credentials
  init_run
  build_or_resolve_binary
  start_server

  case "$EXPECT_MODE" in
    unavailable) assert_unavailable ;;
    importing)
      if [ "$SCENARIO" = "cancel" ]; then
        assert_cancel_scenario
      else
        seed_replica_source_fixture
        assert_importing
      fi
      ;;
    async_job) assert_async_job ;;
    scale)
      if [ "$TWO_POINT" -eq 1 ]; then
        assert_scale_two_point
      else
        assert_importing
      fi
      ;;
  esac

  if { [ "$EXPECT_MODE" = "scale" ] && [ "$TWO_POINT" -eq 0 ]; } \
    || [ "$EXPECT_MODE" = "async_job" ] \
    || { [ "$EXPECT_MODE" = "importing" ] && [ "$SCENARIO" = "cancel" ]; }; then
    cleanup_current_mode_fixture || return 1
  fi

  if [ "$SCENARIO" = "replicas" ]; then
    cleanup_replica_source_fixture || die "replica source fixture cleanup failed"
  fi

  PASS_COMPLETE=1
  record_check "contract_complete" "pass" "$EXPECT_MODE"
  finish_receipt "pass"
  jq -c '{status, mode, scenario, source_index, target_index, head, host, runtime, binary, owned_resources, scale, counts, replica_sequences, elapsed_seconds, checks}' "$RECEIPT"
}

main "$@"
