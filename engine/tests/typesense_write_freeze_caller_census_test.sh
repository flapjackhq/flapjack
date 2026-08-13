#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
TMP="$(mktemp -d "${TMPDIR:-/tmp}/typesense_write_freeze_census.XXXXXX")"
cleanup() {
  rm -rf "$TMP"
}
trap cleanup EXIT

fixture_files=(
  engine/tests/typesense_write_freeze_caller_census.sh
  engine/tests/source_migration_provider_parity_http_probe.sh
  engine/tests/migration_release_loopback_contract.sh
  engine/flapjack-http/src/handlers/migration/mod.rs
  engine/flapjack-http/src/handlers/migration/typesense_source_reader.rs
  engine/flapjack-http/src/handlers/migration/async_status_tests.rs
  engine/flapjack-http/src/handlers/migration/preview_tests/typesense.rs
  engine/flapjack-server/src/migrate.rs
  engine/flapjack-server/tests/migrate_cli_test.rs
  engine/dashboard/src/pages/migrateHelpers.ts
  engine/dashboard/src/pages/migrateHelpers.test.ts
  engine/docs2/openapi.json
  engine/demo-dualclient/public/openapi.json
  engine/docs2/3_IMPLEMENTATION/OPERATIONS.md
  engine/docs2/3_IMPLEMENTATION/2026_07_26_m0b_typesense_source_contract.md
)
for relative in "${fixture_files[@]}"; do
  mkdir -p "$TMP/$(dirname "$relative")"
  cp "$ROOT/$relative" "$TMP/$relative"
done

bash "$TMP/engine/tests/typesense_write_freeze_caller_census.sh" "$TMP" >/dev/null
sed -i.bak '/"sourceWriteFrozen": true/d' \
  "$TMP/engine/flapjack-http/src/handlers/migration/async_status_tests.rs"

set +e
bash "$TMP/engine/tests/typesense_write_freeze_caller_census.sh" "$TMP" \
  >"$TMP/mutation.log" 2>&1
mutation_rc=$?
set -e
if test "$mutation_rc" -eq 0; then
  echo "CALLER_CENSUS_MUTATION=RED permissive_stub_accepted_missing_producer"
  exit 1
fi
grep -Fq \
  'CALLER_CENSUS_FAIL rust_json_missing_attestation engine/flapjack-http/src/handlers/migration/async_status_tests.rs::typesense_submit_payload_with_key' \
  "$TMP/mutation.log"

cp "$ROOT/engine/flapjack-http/src/handlers/migration/async_status_tests.rs" \
  "$TMP/engine/flapjack-http/src/handlers/migration/async_status_tests.rs"
sed -i.bak 's/,\\"sourceWriteFrozen\\":true//' \
  "$TMP/engine/tests/migration_release_loopback_contract.sh"

set +e
bash "$TMP/engine/tests/typesense_write_freeze_caller_census.sh" "$TMP" \
  >"$TMP/release_mutation.log" 2>&1
release_mutation_rc=$?
set -e
if test "$release_mutation_rc" -eq 0; then
  echo "CALLER_CENSUS_MUTATION=RED permissive_stub_accepted_release_submit_without_attestation"
  exit 1
fi
grep -Fq 'CALLER_CENSUS_FAIL raw_json_missing release_typesense_categories_submit' \
  "$TMP/release_mutation.log"
grep -Fq 'CALLER_CENSUS_FAIL raw_json_missing release_typesense_products_submit' \
  "$TMP/release_mutation.log"

echo "CALLER_CENSUS_MUTATION=PASS"
