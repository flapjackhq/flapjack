#!/usr/bin/env bash
# The assigned globals and stubs are consumed indirectly by functions from the
# sourced durability owner, which ShellCheck cannot follow through this path.
# shellcheck disable=SC1091,SC2034,SC2329
set -euo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$TEST_DIR/../.." && pwd)"
# shellcheck source=../../examples/ha-cluster/test_oom_kill_durability.sh
source "$ENGINE_DIR/examples/ha-cluster/test_oom_kill_durability.sh"
SELF_TEST_TMP_DIR=""

assert_file_content() {
  local actual
  actual="$(cat "$1")"
  [[ "$actual" == "$2" ]] || fail "expected $1 to contain '$2', got '$actual'"
}

admission_record_object_ids_present() {
  local data_root="$1" index_name="$2" object_id
  shift 2
  local admission_dir="$data_root/$index_name/write_admission"
  [[ -d "$admission_dir" ]] || return 1
  for object_id in "$@"; do
    grep -R -F -q -- "$object_id" "$admission_dir" || return 1
  done
}

assert_admission_classification_contract() {
  local failed_payload failed_response failed_headers
  mkdir -p "$SELF_TEST_TMP_DIR/self_idx/write_admission"
  ACK_ORACLE_PATH="$SELF_TEST_TMP_DIR/ack.txt"
  RECOVERED_ACK_PATH="$SELF_TEST_TMP_DIR/recovered.txt"
  NO_ACK_PATH="$SELF_TEST_TMP_DIR/no_ack.jsonl"
  printf 'b\na\na\n' >"$ACK_ORACLE_PATH"
  printf 'a\nb\n' >"$RECOVERED_ACK_PATH"
  sort -u "$ACK_ORACLE_PATH" >"$SELF_TEST_TMP_DIR/ack_sorted.txt"
  sort -u "$RECOVERED_ACK_PATH" >"$SELF_TEST_TMP_DIR/recovered_sorted.txt"
  compare_sorted_unique_files "$SELF_TEST_TMP_DIR/ack_sorted.txt" "$SELF_TEST_TMP_DIR/recovered_sorted.txt"
  assert_file_content "$SELF_TEST_TMP_DIR/ack_sorted.txt" $'a\nb'
  [[ "$(unique_count "$ACK_ORACLE_PATH")" == "2" ]] || fail "unique_count should deduplicate IDs"

  printf '{"record":{"actions":[{"addObject":{"id":"queue-fill-a"}}]}}\n' >"$SELF_TEST_TMP_DIR/self_idx/write_admission/1.json"
  printf '{"record":{"actions":[{"addObject":{"id":"queue-fill-b"}}]}}\n' >"$SELF_TEST_TMP_DIR/self_idx/write_admission/2.json"
  admission_record_object_ids_present "$SELF_TEST_TMP_DIR" self_idx queue-fill-a queue-fill-b

  failed_payload="$SELF_TEST_TMP_DIR/failed_request.json"
  failed_response="$SELF_TEST_TMP_DIR/failed_response.json"
  failed_headers="$SELF_TEST_TMP_DIR/failed_headers.txt"
  printf '{"requests":[{"action":"addObject","body":{"objectID":"failed-a"}},{"action":"addObject","body":{"objectID":"failed-b"}}]}\n' >"$failed_payload"
  printf '{"taskID":1,"objectIDs":["failed-a"]}\n' >"$failed_response"
  if classify_acknowledged_response narrowed_2xx 200 "$failed_payload" "$failed_response"; then
    fail "a narrowed 2xx objectID response must not become the acknowledged oracle"
  fi
  [[ "$(unique_count "$ACK_ORACLE_PATH")" == "2" ]] || fail "mismatched response must not alter the acknowledged oracle"
  [[ "$(no_ack_object_id_count)" == "2" ]] || fail "mismatched response must retain every requested objectID as no-ack"
  : >"$NO_ACK_PATH"
  http_request() { HTTP_STATUS="503"; return 1; }
  if submit_acknowledged_batch failed_http "$failed_payload" "$failed_response" "$failed_headers"; then
    fail "unexpected HTTP status should fail the batch submission"
  fi
  [[ "$(no_ack_object_id_count)" == "2" ]] || fail "failed HTTP request should retain both attempted object IDs"

  http_request() { HTTP_STATUS="000"; return 7; }
  if submit_acknowledged_batch failed_transport "$failed_payload" "$failed_response" "$failed_headers"; then
    fail "transport failure should fail the batch submission"
  fi
  [[ "$(no_ack_object_id_count)" == "4" ]] || fail "transport failure should retain both attempted object IDs"

  http_request() { HTTP_STATUS="200"; return 0; }
  record_acknowledged_response() { return 1; }
  if submit_acknowledged_batch ambiguous_2xx "$failed_payload" "$failed_response" "$failed_headers"; then
    fail "unclassifiable 2xx response should fail the batch submission"
  fi
  [[ "$(no_ack_object_id_count)" == "6" ]] || fail "ambiguous 2xx should retain both attempted object IDs"
}

assert_recovery_contract_helpers() {
  printf 'oom-000\n' >"$ACK_ORACLE_PATH"
  printf 'oom-000\n' >"$RECOVERED_ACK_PATH"
  printf '{"label":"ambiguous","http_status":200,"objectIDs":["recovered-no-ack"],"response":""}\n' >"$NO_ACK_PATH"
  RECOVERED_NO_ACK_PATH="$SELF_TEST_TMP_DIR/recovered_no_ack.txt"
  printf 'recovered-no-ack\n' >"$RECOVERED_NO_ACK_PATH"
  printf 'oom-000\nrecovered-no-ack\n' >"$SELF_TEST_TMP_DIR/recovered_all_ids.txt"
  printf '{"nbHits":2,"hits":[]}\n' >"$SELF_TEST_TMP_DIR/recovery_all_query.json"
  ACKNOWLEDGED_COUNT="1"
  EXPLICIT_REJECTED_COUNT="1"
  RECOVERED_REJECTED_COUNT="0"
  query_count() {
    case "$1" in
      alpha | oom-000) printf '1\n' ;;
      *) fail "unexpected self-test recovery query: $1" ;;
    esac
  }
  assert_recovery_contract

  TOTAL_DOCS=5
  BATCH_SIZE=4
  [[ "$(batch_document_count 4)" == "1" ]] || fail "final batch should stop at TOTAL_DOCS"
  printf 'oom-000\noom-001\noom-002\n' >"$ACK_ORACLE_PATH"
  [[ "$(acknowledged_alpha_count)" == "2" ]] || fail "alpha count should derive from acknowledged IDs"
  [[ "$(acknowledged_sample_id)" == "oom-002" ]] || fail "sample should derive from acknowledged IDs"
}

assert_paginated_recovery_query() {
  : >"$ACK_ORACLE_PATH"
  for id_number in $(seq -f '%04.0f' 1 1002); do
    printf 'boundary-%s\n' "$id_number" >>"$ACK_ORACLE_PATH"
  done
  RECOVERY_BASE_URL="http://recovery.test"
  ENCODED_INDEX="self_idx"
  curl() {
    local output_path="" body="" argument
    while (($#)); do
      argument="$1"
      shift
      case "$argument" in
        -o)
          output_path="$1"
          shift
          ;;
        --data-binary)
          body="$1"
          shift
          ;;
      esac
    done
    python3 - "$output_path" "$body" <<'PY'
import json
import sys
output_path, body = sys.argv[1], sys.argv[2]
request = json.loads(body)
hits_per_page = int(request["hitsPerPage"])
page = int(request.get("page", 0))
start = page * hits_per_page + 1
stop = min(start + hits_per_page, 1003)
hits = [{"objectID": f"boundary-{number:04d}"} for number in range(start, stop)]
with open(output_path, "w", encoding="utf-8") as handle:
    json.dump({"nbHits": 1002, "hits": hits}, handle, separators=(",", ":"))
print("200", end="")
PY
  }
  query_recovered_ids
  [[ "$(unique_count "$RECOVERED_ACK_PATH")" == "1002" ]] || fail "recovery query should request enough hits for every acknowledged object ID"
}

assert_network_subnet_contract() {
  local docker_calls_path="$SELF_TEST_TMP_DIR/docker_calls.txt"
  NETWORK_NAME="oom-self-test-network"
  DATA_VOLUME="oom-self-test-data"
  NEGATIVE_DATA_VOLUME="oom-self-test-negative-data"
  NEGATIVE_EMPTY_RESTART="0"
  FLAPJACK_OOM_NETWORK_SUBNET="10.251.255.240/28"
  docker() {
    printf '%s\n' "$*" >>"$docker_calls_path"
  }
  create_run_docker_resources
  [[ "$(sed -n '1p' "$docker_calls_path")" == \
    "network create --subnet 10.251.255.240/28 $NETWORK_NAME" ]] \
    || fail "explicit OOM subnet must be passed to docker network create"

  : >"$docker_calls_path"
  unset FLAPJACK_OOM_NETWORK_SUBNET
  create_run_docker_resources
  [[ "$(sed -n '1p' "$docker_calls_path")" == "network create $NETWORK_NAME" ]] \
    || fail "default OOM network creation must continue to use Docker IPAM"
}

assert_workload_configuration_contract() {
  local injection_marker validation_output
  injection_marker="$SELF_TEST_TMP_DIR/oom_fill_injection_marker"
  TOTAL_DOCS=12
  BATCH_SIZE=4
  OOM_FILL_MIB="1; touch $injection_marker"
  if validation_output="$(validate_workload_configuration 2>&1)"; then
    fail "shell metacharacters in FLAPJACK_OOM_FILL_MIB must be rejected"
  fi
  [[ "$validation_output" == "FAIL: FLAPJACK_OOM_FILL_MIB must be a positive integer" ]] \
    || fail "invalid FLAPJACK_OOM_FILL_MIB must fail at its validation boundary"
  [[ ! -e "$injection_marker" ]] || fail "invalid FLAPJACK_OOM_FILL_MIB executed command text"

  OOM_FILL_MIB=768
  validate_workload_configuration
}

run_tests() {
  require_command python3
  require_command jq
  SELF_TEST_TMP_DIR="$(mktemp -d)"
  trap 'rm -rf "$SELF_TEST_TMP_DIR"' EXIT
  RESULTS_DIR="$SELF_TEST_TMP_DIR"

  assert_admission_classification_contract
  assert_recovery_contract_helpers
  assert_paginated_recovery_query
  assert_network_subnet_contract
  assert_workload_configuration_contract
  echo "PASS: OOM durability helper self-test"
}

run_tests
