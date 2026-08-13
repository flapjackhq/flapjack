#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONTRACT="$SCRIPT_DIR/migration_release_loopback_contract.sh"

# Load the contract definitions without executing its production entry point.
# The stubs below make this a fast startup-order contract with no build, ports,
# or containers, while retaining the real main() and fixture-label owner.
# shellcheck disable=SC1090
source "$CONTRACT"

if (
  export LOG=""
  # shellcheck disable=SC2030
  export CONTRACT_PASSES=$((RELEASE_LOOPBACK_EXPECTED_ARM_COUNT - 1))
  # shellcheck disable=SC2218
  assert_expected_contract_arm_count
) >/dev/null 2>&1; then
  printf 'expected missing contract arm to be indeterminate\n' >&2
  exit 1
else
  arm_count_rc=$?
  [ "$arm_count_rc" -eq 2 ] || {
    printf 'expected missing contract arm rc=2, got rc=%s\n' "$arm_count_rc" >&2
    exit 1
  }
fi
# shellcheck disable=SC2031
export CONTRACT_PASSES=$RELEASE_LOOPBACK_EXPECTED_ARM_COUNT
# shellcheck disable=SC2218
assert_expected_contract_arm_count
export CONTRACT_PASSES=0

cleanup_test_tmp() {
  if [ -n "${TMP:-}" ] && [ -d "$TMP" ]; then
    case "$TMP" in
      */fj_migration_release_loopback.*) rm -rf -- "$TMP" ;;
      *) printf 'refusing to remove unexpected test temp dir: %s\n' "$TMP" >&2; return 1 ;;
    esac
  fi
}
trap cleanup_test_tmp EXIT

require_tools() { :; }
build_dns_canary() { :; }
start_request_canary() { :; }
build_release_binary() { :; }
assert_server_dns_canary_positive_control() { :; }
assert_unreachable_request_canary_is_indeterminate() { :; }
start_release_server() { :; }
assert_meilisearch_discovery() { :; }
assert_typesense_discovery() { :; }
assert_meilisearch_migration() { :; }
assert_typesense_migration() { :; }
run_refusal_matrix() { :; }
assert_no_canary_activity() { :; }
assert_expected_contract_arm_count() { :; }

STARTUP_CALLS=0
start_discovery_upstreams() {
  env | grep -Fqx "SOURCE_PROVIDER_OWNER_TOKEN=${SOURCE_PROVIDER_OWNER_TOKEN:-}" \
    || fail_red 'source_provider_owner_token_not_exported_before_upstreams'
  [ -n "${SOURCE_PROVIDER_OWNER_TOKEN:-}" ] \
    || fail_red 'source_provider_owner_token_empty_before_upstreams'
  source_provider_container_name_matches meilisearch "$MEILI_CONTAINER" \
    || fail_red "meilisearch_container_name_not_owned name=${MEILI_CONTAINER}"
  source_provider_container_name_matches typesense "$TYPESENSE_CONTAINER" \
    || fail_red "typesense_container_name_not_owned name=${TYPESENSE_CONTAINER}"
  source_provider_docker_labels meilisearch
  source_provider_docker_labels typesense
  STARTUP_CALLS=$((STARTUP_CALLS + 1))
}

main >/dev/null
[ "$STARTUP_CALLS" -eq 1 ]
printf 'MIGRATION_RELEASE_LOOPBACK_STARTUP_CONTRACT=PASS owner_token=exported_before_upstreams\n'
