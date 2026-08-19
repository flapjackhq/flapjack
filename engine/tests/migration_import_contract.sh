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
SCALE_SAMPLER_INTERVAL_SECONDS="${MIGRATION_IMPORT_CONTRACT_SCALE_SAMPLER_INTERVAL_SECONDS:-0.01}"
READY_POLL_INTERVAL_SECONDS="${MIGRATION_IMPORT_CONTRACT_READY_POLL_INTERVAL_SECONDS:-0.5}"
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

# TODO: Document parse_args.
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

# TODO: Document load_credentials.
load_credentials() {
  if [ "$EXPECT_MODE" = "unavailable" ]; then
    SOURCE_APP_ID="stub_app_id"
    SOURCE_API_KEY="stub_source_key"
    return
  fi

  # shellcheck source=engine/tests/common/load_named_secrets.sh disable=SC1091
  source "$SECRET_HELPER"
  local loaded_app_id loaded_admin_key
  if ! loaded_app_id="$(read_secret_env_value "$SECRET_FILE" ALGOLIA_APP_ID)" || [ -z "$loaded_app_id" ]; then
    die "required Algolia credentials could not be loaded"
  fi
  if ! loaded_admin_key="$(read_secret_env_value "$SECRET_FILE" ALGOLIA_ADMIN_KEY)" || [ -z "$loaded_admin_key" ]; then
    die "required Algolia credentials could not be loaded"
  fi
  export ALGOLIA_APP_ID="$loaded_app_id"
  export ALGOLIA_ADMIN_KEY="$loaded_admin_key"
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

# TODO: Document init_run.
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

# TODO: Document build_or_resolve_binary.
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

# TODO: Document start_server.
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
    # Peers with no peer credential: startup requires
    # FLAPJACK_REPLICATION_API_KEY unless this override is set.
    FLAPJACK_NODE_ID="migration-import-contract" \
      FLAPJACK_PEERS="migration-peer=https://10.0.0.2:7700" \
      FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1 \
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

  "$WAIT_HELPER" --pid "$SERVER_PID" --host 127.0.0.1 --port auto --log-path "$SERVER_LOG" \
    --retries 80 --interval-seconds "$READY_POLL_INTERVAL_SECONDS"
  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$SERVER_LOG" | head -1)"
  [ -n "$port" ] || die "server became ready but no auto-port was found"
  BASE_URL="http://127.0.0.1:${port}"
  record_check "local_server" "pass" "started"
}

# TODO: Document flapjack_request.
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

# TODO: Document capture_scale_trial_artifacts.
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

# TODO: Document commit_scale_trial_artifact_pair.
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

# TODO: Document sample_scale_trial.
sample_scale_trial() {
  local marker="$1" out="$2" jobs_dir="$DATA_DIR/migration_exports/jobs"
  local peak_rss=0 rss job_dir="" sidecar="" size last_size="" error="" interval_ms
  local sizes_file="${out}.sizes"
  local candidate_dir="${out}.candidates"
  interval_ms="$(awk -v interval="$SCALE_SAMPLER_INTERVAL_SECONDS" 'BEGIN { printf "%d", interval * 1000 }')"
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
    sleep "$SCALE_SAMPLER_INTERVAL_SECONDS"
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

# TODO: Document measured_scale_migration_request.
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

# TODO: Document build_scale_trial_record.
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

# TODO: Document source_algolia_request.
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

# TODO: Document seed_replica_source_fixture.
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

# TODO: Document browse_index.
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

# TODO: Document assert_scale_aggregates.
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

# TODO: Document assert_target_list_exactly_once.
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

# TODO: Document delete_verified_target.
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
# Async, cancel, replica, unavailable, and verified-import scenarios share
# this runner's transport and receipt owners but live in a concern-named module
# so the stable executable entrypoint stays navigable.
source "$SCRIPT_DIR/migration_import_scenarios.sh"

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

# TODO: Document summarize_scale_condition.
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

# TODO: Document record_two_point_ratio.
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

# TODO: Document run_scale_condition_trials.
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

# TODO: Document record_replica_sequences.
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

# TODO: Document assert_replica_warnings.
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

# TODO: Document assert_virtual_sidecar_structure.
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

# TODO: Document assert_replica_scenario_import.
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

# TODO: Document preserve_run_evidence.
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

# TODO: Document cleanup.
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

# TODO: Document main.
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
