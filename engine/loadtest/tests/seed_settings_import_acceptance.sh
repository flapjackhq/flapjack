#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LOADTEST_HELPERS="$ROOT_DIR/lib/loadtest_shell_helpers.sh"
LOADTEST_SEED_SOURCE="$ROOT_DIR/product-seed-data.mjs"

if [[ ! -f "$LOADTEST_HELPERS" ]]; then
  echo "FAIL: missing $LOADTEST_HELPERS"
  exit 1
fi
if [[ ! -f "$LOADTEST_SEED_SOURCE" ]]; then
  echo "FAIL: missing $LOADTEST_SEED_SOURCE"
  exit 1
fi

# The root is derived from BASH_SOURCE so this guard works from any checkout location.
# shellcheck disable=SC1090,SC1091
source "$LOADTEST_HELPERS"

require_loadtest_commands jq node
load_dashboard_seed_settings "$ROOT_DIR"

if ! jq -e '
  .searchableAttributes == ["name", "description", "brand", "category", "tags"] and
  (.attributesForFaceting | index("brand")) != null and
  .customRanking == ["desc(rating)", "desc(reviewCount)", "asc(price)"]
' <<<"$LOADTEST_SETTINGS_JSON" >/dev/null; then
  echo "FAIL: load_dashboard_seed_settings did not import the canonical loadtest seed settings."
  exit 1
fi

echo "PASS: loadtest seed-settings import points at product-seed-data.mjs"
