#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
TYPESENSE_FIXTURE="$ENGINE_DIR/tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json"

die_indeterminate() {
  printf 'source provider fixture failed: %s\n' "$1" >&2
  exit 2
}

mark_cleanup_failure() {
  die_indeterminate "$1"
}

# shellcheck source=lib/source_provider_fixtures.sh
source "$SCRIPT_DIR/lib/source_provider_fixtures.sh"

usage() {
  printf 'usage: %s {up|down} {meilisearch|typesense}\n' "$0" >&2
  exit 2
}

ACTIVE_PROVIDER=""
ACTIVE_CONTAINER=""
TMP=""

cleanup_failed_start() {
  local script_exit_code=$?
  trap - EXIT
  if [ "$script_exit_code" -ne 0 ]; then
    [ -z "$ACTIVE_CONTAINER" ] || remove_owned_container "$ACTIVE_PROVIDER" "$ACTIVE_CONTAINER"
    if source_provider_fixture_dir_matches "$ACTIVE_PROVIDER" "$TMP" && [ -d "$TMP" ]; then
      rm -rf -- "$TMP"
    fi
  fi
  exit "$script_exit_code"
}
trap cleanup_failed_start EXIT

emit_fixture_json() {
  local provider="$1" port="$2" api_key="$3" source_name="$4" container="$5" ownership_token="$6"
  shift 6
  jq -cn \
    --arg provider "$provider" \
    --argjson port "$port" \
    --arg apiKey "$api_key" \
    --arg sourceName "$source_name" \
    --arg container "$container" \
    --arg fixtureDir "$TMP" \
    --arg ownershipToken "$ownership_token" \
    --argjson seededIds "$(printf '%s\n' "$@" | jq -R . | jq -s .)" \
    '{provider:$provider,port:$port,apiKey:$apiKey,sourceName:$sourceName,container:$container,fixtureDir:$fixtureDir,ownershipToken:$ownershipToken,seededDocumentCount:($seededIds|length),seededIds:$seededIds}'
}

emit_cleanup_receipt() {
  local provider="$1" container="$2" ownership_token="$3"
  printf 'SOURCE_PROVIDER_CLEANUP_RECEIPT=' >&2
  jq -cn \
    --arg provider "$provider" \
    --arg container "$container" \
    --arg fixtureDir "$TMP" \
    --arg ownershipToken "$ownership_token" \
    '{provider:$provider,container:$container,fixtureDir:$fixtureDir,ownershipToken:$ownershipToken}' >&2
}

new_ownership_token() {
  local provider="$1"
  printf '%s_fixture_%s_%s\n' "$provider" "$$" "$(date +%s%N 2>/dev/null || date +%s)"
}

start_provider() {
  local provider="$1"
  case "$provider" in
    meilisearch|typesense) ;;
    *) usage ;;
  esac
  TMP="$(mktemp -d "/tmp/fj_source_provider_fixture_${provider}_XXXXXX")"
  SOURCE_PROVIDER_OWNER_TOKEN="$(new_ownership_token "$provider")"
  export SOURCE_PROVIDER_OWNER_TOKEN
  case "$provider" in
    meilisearch)
      MEILI_CONTAINER="fj_source_migration_provider_parity_meili_$$"
      ACTIVE_PROVIDER="meilisearch"
      ACTIVE_CONTAINER="$MEILI_CONTAINER"
      MEILI_PORT=""
      start_meilisearch
      emit_cleanup_receipt meilisearch "$MEILI_CONTAINER" "$SOURCE_PROVIDER_OWNER_TOKEN"
      emit_fixture_json meilisearch "$MEILI_PORT" "$MEILI_KEY" configured_pk "$MEILI_CONTAINER" "$SOURCE_PROVIDER_OWNER_TOKEN" MEILI-001 MEILI-002
      ACTIVE_CONTAINER=""
      ;;
    typesense)
      TYPESENSE_CONTAINER="fj_source_migration_provider_parity_typesense_$$"
      ACTIVE_PROVIDER="typesense"
      ACTIVE_CONTAINER="$TYPESENSE_CONTAINER"
      TYPESENSE_PORT=""
      start_typesense
      emit_cleanup_receipt typesense "$TYPESENSE_CONTAINER" "$SOURCE_PROVIDER_OWNER_TOKEN"
      emit_fixture_json typesense "$TYPESENSE_PORT" "$TYPESENSE_KEY" "$TYPESENSE_PRODUCTS" "$TYPESENSE_CONTAINER" "$SOURCE_PROVIDER_OWNER_TOKEN" prod_1 prod_2
      ACTIVE_CONTAINER=""
      ;;
  esac
}

stop_provider() {
  local provider="$1" container="${SOURCE_PROVIDER_CONTAINER:-}" fixture_dir="${SOURCE_PROVIDER_FIXTURE_DIR:-}" ownership_token="${SOURCE_PROVIDER_OWNER_TOKEN:-}"
  case "$provider" in
    meilisearch|typesense) ;;
    *) usage ;;
  esac
  [ -n "$container" ] || die_indeterminate 'SOURCE_PROVIDER_CONTAINER is required for down'
  source_provider_container_name_matches "$provider" "$container" \
    || die_indeterminate "SOURCE_PROVIDER_CONTAINER is not an owned ${provider} fixture container"
  [ -n "$ownership_token" ] || die_indeterminate 'SOURCE_PROVIDER_OWNER_TOKEN is required for down'
  [ -z "$fixture_dir" ] || source_provider_fixture_dir_matches "$provider" "$fixture_dir" \
    || die_indeterminate "SOURCE_PROVIDER_FIXTURE_DIR is not an owned ${provider} fixture directory"
  export SOURCE_PROVIDER_OWNER_TOKEN="$ownership_token"
  if container_exists "$container" && ! container_has_fixture_ownership "$provider" "$container"; then
    mark_cleanup_failure "${provider}_container_unowned_label name=${container}"
  fi
  case "$provider" in
    typesense)
      repair_typesense_data_permissions "$container" "$fixture_dir"
      ensure_stopped_typesense_data_host_removable "$container" "$fixture_dir"
      ;;
    meilisearch) ;;
  esac
  case "$provider" in
    meilisearch|typesense) remove_owned_container "$provider" "$container" ;;
  esac
  if [ -n "$fixture_dir" ] && [ -d "$fixture_dir" ]; then
    rm -rf -- "$fixture_dir" \
      || mark_cleanup_failure "${provider}_fixture_dir_rm_failed dir=${fixture_dir}"
  fi
  if [ -n "$fixture_dir" ] && [ -d "$fixture_dir" ]; then
    mark_cleanup_failure "${provider}_fixture_dir_residue dir=${fixture_dir}"
  fi
  jq -cn --arg provider "$provider" --arg container "$container" \
    '{provider:$provider,container:$container,removed:true}'
}

[ "$#" -eq 2 ] || usage
case "$1" in
  up) start_provider "$2" ;;
  down) stop_provider "$2" ;;
  *) usage ;;
esac
