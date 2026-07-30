#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_HELPERS="$SCRIPT_DIR/lib/loadtest_shell_helpers.sh"

# Shared contract defaults come from lib/config.js (loadtest_read, loadtest_write).
# Stage 1 route targets:
# - /1/indexes/:indexName/settings
# - /1/indexes/:indexName/batch

if [[ ! -f "$LOADTEST_HELPERS" ]]; then
  echo "FAIL: missing $LOADTEST_HELPERS"
  exit 1
fi

# shellcheck source=lib/loadtest_shell_helpers.sh
source "$LOADTEST_HELPERS"

require_loadtest_commands curl jq node
load_shared_loadtest_config
initialize_loadtest_auth_headers
ENCODED_READ_INDEX="$(loadtest_encode_path_component "$FLAPJACK_READ_INDEX")"
ENCODED_WRITE_INDEX="$(loadtest_encode_path_component "$FLAPJACK_WRITE_INDEX")"

seed_read_documents() {
  local docs_file
  local total_docs
  local batch_size=100
  local offset=0

  docs_file="$(mktemp)"

  (
    cd "$SCRIPT_DIR"
    node -e '
import("../dashboard/tour/product-seed-data.mjs").then(({ baseProducts }) => {

function round(value) {
  return Math.round(value * 10000) / 10000;
}

const total = 1000;
const docs = [];
for (let i = 0; i < total; i += 1) {
  const template = baseProducts[i % baseProducts.length];
  const variant = Math.floor(i / baseProducts.length) + 1;
  const geoJitter = (i % 9) - 4;

  // Spread inherits seeded fields from base product: brand, category, subcategory, color
  docs.push({
    ...template,
    objectID: `loadtest-read-${String(i + 1).padStart(5, "0")}`,
    name: `${template.name} Variant ${variant}`,
    description: `${template.description} Deterministic variant ${variant}.`,
    price: Number((template.price + (i % 17) * 2.5).toFixed(2)),
    rating: Number(Math.min(5, Math.max(1, template.rating - (i % 5) * 0.1)).toFixed(1)),
    reviewCount: template.reviewCount + (i % 800),
    inStock: (i % 11) !== 0,
    tags: [...new Set([...(template.tags || []), `series-${i % 20}`])],
    releaseYear: 2022 + (i % 5),
    _geo: {
      lat: round(template._geo.lat + geoJitter * 0.001),
      lng: round(template._geo.lng - geoJitter * 0.001),
    },
  });
}

process.stdout.write(JSON.stringify(docs));
}).catch((error) => {
  console.error(error);
  process.exit(1);
});
' >"$docs_file"
  )

  total_docs="$(jq 'length' "$docs_file")"
  if (( total_docs < 1000 )); then
    echo "FAIL: generated document count (${total_docs}) is below 1000."
    rm -f "$docs_file"
    exit 1
  fi

  while (( offset < total_docs )); do
    local batch_payload
    local batch_response
    local task_id

    batch_payload="$(
      jq -c --argjson offset "$offset" --argjson size "$batch_size" '
        { requests: [ .[$offset:($offset + $size)][] | { action: "addObject", body: . } ] }
      ' "$docs_file"
    )"

    batch_response="$(loadtest_http_request POST "/1/indexes/${ENCODED_READ_INDEX}/batch" "$batch_payload" "200")"
    task_id="$(extract_loadtest_numeric_task_id "$batch_response")"
    wait_for_loadtest_task_published "$task_id"

    offset=$((offset + batch_size))
  done

  rm -f "$docs_file"
}

verify_post_seed_state() {
  local post_search_response
  local get_search_response
  local write_index_response
  local metrics_response
  local post_hits
  local get_hits
  local write_hits

  post_search_response="$(loadtest_http_request POST "/1/indexes/${ENCODED_READ_INDEX}/query" '{"query":"MacBook","hitsPerPage":5}' "200")"
  post_hits="$(jq -r '(.hits // []) | length' <<<"$post_search_response")"
  if (( post_hits < 1 )); then
    echo "FAIL: POST search verification returned no hits on read index."
    exit 1
  fi

  get_search_response="$(loadtest_http_request GET "/1/indexes/${ENCODED_READ_INDEX}/query?query=MacBook&hitsPerPage=5" "" "200")"
  get_hits="$(jq -r '(.hits // []) | length' <<<"$get_search_response")"
  if (( get_hits < 1 )); then
    echo "FAIL: GET search verification returned no hits on read index."
    exit 1
  fi

  write_index_response="$(loadtest_http_request POST "/1/indexes/${ENCODED_WRITE_INDEX}/query" '{"query":"","hitsPerPage":5}' "200")"
  write_hits="$(jq -r '(.hits // []) | length' <<<"$write_index_response")"
  if (( write_hits != 0 )); then
    echo "FAIL: write index baseline is not clean (expected 0 hits, got ${write_hits})."
    exit 1
  fi

  metrics_response="$(loadtest_http_request GET "/metrics" "" "200")"
  if ! grep -q "flapjack_" <<<"$metrics_response"; then
    echo "FAIL: /metrics verification failed; expected flapjack_* metric names."
    exit 1
  fi
}

load_dashboard_seed_settings

echo "Seeding loadtest data with shared contract from engine/loadtest/lib/config.js"
echo "Base URL: $FLAPJACK_BASE_URL"
echo "Read index: $FLAPJACK_READ_INDEX"
echo "Write index: $FLAPJACK_WRITE_INDEX"

reset_loadtest_index "$FLAPJACK_READ_INDEX"
reset_loadtest_index "$FLAPJACK_WRITE_INDEX"

apply_loadtest_index_settings "$FLAPJACK_READ_INDEX"
apply_loadtest_index_settings "$FLAPJACK_WRITE_INDEX"

seed_read_documents
verify_post_seed_state

echo "Loadtest seed complete: read index populated and verified, write index clean, metrics available."
