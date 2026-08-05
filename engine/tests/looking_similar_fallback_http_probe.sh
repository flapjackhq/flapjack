#!/usr/bin/env bash

# Running-system contract probe for the looking-similar term fallback.
#
# Catches a shipped default-feature build (no `vector-search`) whose
# `/1/indexes/<index>/recommendations` endpoint returns no useful looking-similar
# hits -- the `Ok(Vec::new())` fallback -- or ranks/scores them wrongly on the wire.
# Rust unit coverage exercises `compute_looking_similar` directly and therefore
# cannot catch a regression in the HTTP envelope, the handler dispatch, the
# `_score` annotation, or the default-feature build shape itself.
#
# Every assertion runs against a freshly built, lane-owned server started by this
# script on its own auto-assigned port. There is no skip-success path: a missing
# tool, a failed build, or an unseeded fixture exits non-zero.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"
HTTP_PROBE_LIB="$ENGINE_DIR/tests/common/http_probe_lib.sh"

# shellcheck source=common/http_probe_lib.sh
source "$HTTP_PROBE_LIB"

ADMIN_KEY="ls-fallback-probe-admin-key"
APP_ID="ls-fallback-probe-app"
INDEX_NAME="ls_fallback_probe"
MISSING_INDEX_NAME="ls_fallback_probe_absent"
FIXTURE_OBJECT_IDS="seed strict_winner five_terms three_terms two_terms one_term zero_overlap no_terms"
EXPECTED_CHECKS=8

# Inverted by the companion self-test to prove wrong wire ordering and scoring
# turn this probe red at the R1_ORDERING and R2_SCORE labels specifically.
EXPECTED_TOP_OBJECT_ID="${FJ_PROBE_EXPECTED_TOP_OBJECT_ID:-strict_winner}"
EXPECTED_TOP_SCORE="${FJ_PROBE_EXPECTED_TOP_SCORE:-100}"

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
TESTS_RUN=0
TESTS_FAILED=0

cleanup() {
  local script_exit_code=$?
  if ! http_probe_cleanup "$script_exit_code" "looking-similar fallback"; then
    script_exit_code=1
  fi
  trap - EXIT
  exit "$script_exit_code"
}

# Default features only: the fallback under test is the path taken when the
# `vector-search` feature is absent from the shipped binary.
resolve_binary() { http_probe_resolve_default_binary; }
start_server() { http_probe_start_server; }
curl_json() { http_probe_curl_json "$@"; }
curl_get() { http_probe_curl_get "$@"; }
record_check() { http_probe_record_check "$@"; }
require_status() { http_probe_require_status "$@"; }

# Reduces one recommendations response to a single comparable fact line. A
# malformed or non-JSON envelope reduces to a `malformed:` marker so a 4xx or a
# reshaped body fails its labelled assertion instead of crashing the probe.
recommend_summary() {
  local mode="$1" body_path="$2" status="$3"
  python3 - "$mode" "$body_path" "$status" <<'PY'
import json
import sys

mode, path, status = sys.argv[1], sys.argv[2], sys.argv[3]

try:
    with open(path) as handle:
        hits = json.load(handle)["results"][0]["hits"]
except Exception as error:  # noqa: BLE001 - any shape deviation is a probe failure
    print(f"{status}|malformed:{type(error).__name__}")
    raise SystemExit(0)

object_ids = [hit.get("objectID") for hit in hits]
scores = [hit.get("_score") for hit in hits]
top_object_ids = object_ids[:5]
joined_top_object_ids = ",".join(str(object_id) for object_id in top_object_ids)

def score_malformed_marker():
    if any(score is None for score in scores):
        return "MissingScore"
    if any(type(score) is not int for score in scores):
        return "InvalidScoreType"
    return None

if mode == "order":
    print("|".join([
        status,
        joined_top_object_ids,
        "min_hits_ok" if len(hits) >= 4 else f"min_hits_{len(hits)}",
        "no_seed" if "seed" not in object_ids else "has_seed",
        "no_zero_overlap" if "zero_overlap" not in object_ids else "has_zero_overlap",
    ]))
elif mode == "score":
    malformed_marker = score_malformed_marker()
    if malformed_marker is not None:
        print(f"{status}|malformed:{malformed_marker}")
        raise SystemExit(0)
    non_increasing = all(
        scores[index] >= scores[index + 1] for index in range(len(scores) - 1)
    )
    in_range = bool(scores) and all(0 <= score <= 100 for score in scores)
    print("|".join([
        str(scores[0]) if scores else "none",
        "non_increasing" if non_increasing else "increasing",
        "in_range" if in_range else "out_of_range",
    ]))
elif mode == "second_score":
    print(scores[1] if len(scores) > 1 else "none")
elif mode == "top":
    print("|".join([status, joined_top_object_ids, str(scores[0]) if scores else "none"]))
elif mode == "cap":
    print("|".join([status, joined_top_object_ids, str(scores[1]) if len(scores) > 1 else "none"]))
elif mode == "empty":
    print("|".join([status, "empty" if not hits else f"hits={joined_top_object_ids}"]))
else:
    raise SystemExit(f"unknown recommend_summary mode: {mode}")
PY
}

health_summary() {
  local body_path="$1" status="$2"
  python3 - "$body_path" "$status" <<'PY'
import json
import sys

path, status = sys.argv[1], sys.argv[2]

try:
    with open(path) as handle:
        vector_search = json.load(handle)["capabilities"]["vectorSearch"]
except Exception as error:  # noqa: BLE001 - any shape deviation is a probe failure
    print(f"{status}|malformed:{type(error).__name__}")
    raise SystemExit(0)

print(f"{status}|{json.dumps(vector_search)}")
PY
}

json_field() {
  local path="$1" field="$2"
  python3 - "$path" "$field" <<'PY'
import json
import sys

path, field = sys.argv[1], sys.argv[2]
with open(path) as handle:
    value = json.load(handle)
for part in field.split("."):
    value = value[part]
print(value)
PY
}

searchable_object_ids() {
  local body_path="$1"
  python3 - "$body_path" <<'PY'
import json
import sys

with open(sys.argv[1]) as handle:
    hits = json.load(handle).get("hits", [])
print(" ".join(sorted(str(hit.get("objectID")) for hit in hits)))
PY
}

# Proves the running build is the default-feature shape the fallback exists for.
# Runs before any fixture work so a vector-enabled binary cannot silently pass.
check_health_capability() {
  local body_path="$TMP_ROOT/health.json" status actual
  status="$(curl_get "$BASE/health" "$body_path")"
  actual="$(health_summary "$body_path" "$status")"
  record_check "HEALTH_VECTOR_CAPABILITY" "200|false" "$actual"

  if [ "$actual" = "200|false" ]; then
    printf 'CAPABILITY_VECTOR_SEARCH=false\n'
  fi
}

# `zephyrium` is a rare term shared only by the seed and `strict_winner`, so it
# breaks the top-score tie that a plain shared-vocabulary fixture would produce.
seed_fixtures() {
  local body_path="$TMP_ROOT/settings.json" status task_id

  status="$(curl_json PUT "$BASE/1/indexes/$INDEX_NAME/settings" \
    '{"searchableAttributes":["name"]}' "$body_path")"
  require_status "persist searchableAttributes" "200" "$status" "$body_path"
  task_id="$(json_field "$body_path" taskID)"
  wait_for_task "$task_id"

  body_path="$TMP_ROOT/batch.json"
  status="$(curl_json POST "$BASE/1/indexes/$INDEX_NAME/batch" '{"requests":[
    {"action":"addObject","body":{"objectID":"seed","name":"Wireless Bluetooth Headphones with active noise cancelling zephyrium"}},
    {"action":"addObject","body":{"objectID":"strict_winner","name":"Wireless Bluetooth Headphones with active noise cancelling zephyrium travel"}},
    {"action":"addObject","body":{"objectID":"five_terms","name":"Wireless Bluetooth Headphones noise cancelling"}},
    {"action":"addObject","body":{"objectID":"three_terms","name":"Wireless Bluetooth Headphones"}},
    {"action":"addObject","body":{"objectID":"two_terms","name":"Wireless Headphones"}},
    {"action":"addObject","body":{"objectID":"one_term","name":"Bluetooth speaker"}},
    {"action":"addObject","body":{"objectID":"zero_overlap","name":"Ceramic coffee grinder"}},
    {"action":"addObject","body":{"objectID":"no_terms","sku":"A1"}}
  ]}' "$body_path")"
  require_status "seed probe fixtures" "200" "$status" "$body_path"

  task_id="$(json_field "$body_path" taskID)"
  wait_for_task "$task_id"
  wait_for_fixtures_searchable
}

wait_for_task() {
  local task_id="$1" body_path="$TMP_ROOT/task.json" status
  for _attempt in $(seq 1 80); do
    status="$(curl_get "$BASE/1/indexes/$INDEX_NAME/task/$task_id" "$body_path")"
    if [ "$status" = "200" ] && [ "$(json_field "$body_path" status)" = "published" ]; then
      return 0
    fi
    sleep 0.25
  done

  printf 'ERROR: seed task %s did not publish\n' "$task_id" >&2
  cat "$body_path" >&2 || true
  exit 1
}

wait_for_fixtures_searchable() {
  local body_path="$TMP_ROOT/searchable.json" expected status
  expected="$(printf '%s\n' $FIXTURE_OBJECT_IDS | sort | tr '\n' ' ' | sed 's/ $//')"

  for _attempt in $(seq 1 80); do
    status="$(curl_json POST "$BASE/1/indexes/$INDEX_NAME/query" \
      '{"query":"","hitsPerPage":50}' "$body_path")"
    if [ "$status" = "200" ] && [ "$(searchable_object_ids "$body_path")" = "$expected" ]; then
      return 0
    fi
    sleep 0.25
  done

  printf 'ERROR: probe fixtures did not become searchable\n' >&2
  cat "$body_path" >&2 || true
  exit 1
}

# The router captures the concrete index segment through its
# `/1/indexes/:_wildcard/recommendations` route; `indexName` in the body selects
# the index that is actually queried.
post_recommendation() {
  local label="$1" index_name="$2" body="$3"
  local request_path="$TMP_ROOT/${label}_request.json"
  local body_path="$TMP_ROOT/${label}_response.json"

  printf '%s\n' "$body" >"$request_path"
  curl_json POST "$BASE/1/indexes/$index_name/recommendations" "$body" "$body_path"
}

looking_similar_body() {
  local index_name="$1" object_id="$2" threshold="$3" extra="$4"
  printf '{"requests":[{"indexName":"%s","model":"looking-similar","objectID":"%s","threshold":%s%s}]}' \
    "$index_name" "$object_id" "$threshold" "$extra"
}

run_recommendation_checks() {
  local status second_score

  status="$(post_recommendation r1 "$INDEX_NAME" \
    "$(looking_similar_body "$INDEX_NAME" seed 0 '')")"
  record_check "R1_ORDERING" \
    "200|${EXPECTED_TOP_OBJECT_ID},five_terms,three_terms,two_terms,one_term|min_hits_ok|no_seed|no_zero_overlap" \
    "$(recommend_summary order "$TMP_ROOT/r1_response.json" "$status")"

  record_check "R2_SCORE" \
    "${EXPECTED_TOP_SCORE}|non_increasing|in_range" \
    "$(recommend_summary score "$TMP_ROOT/r1_response.json" "$status")"
  second_score="$(recommend_summary second_score "$TMP_ROOT/r1_response.json" "$status")"

  status="$(post_recommendation r3 "$INDEX_NAME" \
    "$(looking_similar_body "$INDEX_NAME" seed 100 '')")"
  record_check "R3_THRESHOLD" "200|strict_winner|100" \
    "$(recommend_summary top "$TMP_ROOT/r3_response.json" "$status")"

  status="$(post_recommendation r4 "$INDEX_NAME" \
    "$(looking_similar_body "$INDEX_NAME" seed 0 ',"maxRecommendations":2')")"
  record_check "R4_CAP" "200|strict_winner,five_terms|${second_score}" \
    "$(recommend_summary cap "$TMP_ROOT/r4_response.json" "$status")"

  status="$(post_recommendation r5 "$INDEX_NAME" \
    "$(looking_similar_body "$INDEX_NAME" absent_seed_object 0 '')")"
  record_check "R5_UNKNOWN_SEED" "200|empty" \
    "$(recommend_summary empty "$TMP_ROOT/r5_response.json" "$status")"

  status="$(post_recommendation r6 "$MISSING_INDEX_NAME" \
    "$(looking_similar_body "$MISSING_INDEX_NAME" seed 0 '')")"
  record_check "R6_MISSING_INDEX" "200|empty" \
    "$(recommend_summary empty "$TMP_ROOT/r6_response.json" "$status")"

  status="$(post_recommendation r7 "$INDEX_NAME" \
    "$(looking_similar_body "$INDEX_NAME" no_terms 0 '')")"
  record_check "R7_NO_USABLE_TERMS" "200|empty" \
    "$(recommend_summary empty "$TMP_ROOT/r7_response.json" "$status")"
}

main() {
  trap cleanup EXIT
  http_probe_require_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj-looking-similar-probe.XXXXXX")"
  resolve_binary
  start_server

  check_health_capability
  seed_fixtures
  run_recommendation_checks

  if [ "$TESTS_RUN" -ne "$EXPECTED_CHECKS" ]; then
    printf 'ERROR: expected %s checks, ran %s\n' "$EXPECTED_CHECKS" "$TESTS_RUN" >&2
    exit 1
  fi
  if [ "$TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main "$@"
fi
