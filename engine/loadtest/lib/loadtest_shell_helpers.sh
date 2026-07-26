#!/usr/bin/env bash

LOADTEST_HELPER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_AUTH_HEADERS=()

require_loadtest_command() {
  local binary="$1"
  if ! command -v "$binary" >/dev/null 2>&1; then
    echo "FAIL: required command not found: $binary"
    exit 1
  fi
}

require_loadtest_commands() {
  local binary
  for binary in "$@"; do
    require_loadtest_command "$binary"
  done
}

derive_bind_addr_from_base_url() {
  local base_url="$1"
  local parsed_bind_addr

  parsed_bind_addr="$(
    node -e '
const input = process.argv[1];
try {
  const url = new URL(input);
  const loopbackHosts = new Set(["127.0.0.1", "localhost", "::1"]);
  if (!loopbackHosts.has(url.hostname)) {
    console.error(`refusing to start a no-auth loadtest server on non-loopback host: ${url.hostname}`);
    process.exit(42);
  }
  const fallbackPort = url.protocol === "https:" ? "443" : "80";
  const port = url.port || fallbackPort;
  process.stdout.write(`${url.hostname}:${port}`);
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
' "$base_url"
  )" || {
    echo "FAIL: unable to parse FLAPJACK_LOADTEST_BASE_URL: $base_url"
    exit 1
  }

  printf '%s' "$parsed_bind_addr"
}

start_loadtest_server() {
  local server_binary="$1"
  local auth_mode="$2"
  local bind_addr="$3"
  local data_dir="$4"
  local server_log_path="$5"
  local api_key="${6:-}"
  local -a extra_env=()
  if (( $# > 6 )); then
    shift 6
    extra_env=("$@")
  fi
  local -a env_prefix=()

  [[ -x "$server_binary" ]] || {
    echo "FAIL: missing executable server binary: $server_binary"
    exit 1
  }

  mkdir -p "$data_dir"
  if (( ${#extra_env[@]} > 0 )); then
    env_prefix+=("${extra_env[@]}")
  fi

  case "$auth_mode" in
    no-auth)
      if (( ${#env_prefix[@]} > 0 )); then
        env "${env_prefix[@]}" \
          "$server_binary" --no-auth --bind-addr "$bind_addr" --data-dir "$data_dir" \
          >"$server_log_path" 2>&1 &
      else
        "$server_binary" --no-auth --bind-addr "$bind_addr" --data-dir "$data_dir" \
          >"$server_log_path" 2>&1 &
      fi
      ;;
    auth-required)
      [[ -n "$api_key" ]] || {
        echo "FAIL: start_loadtest_server auth-required mode needs FLAPJACK_API_KEY"
        exit 1
      }
      if (( ${#env_prefix[@]} > 0 )); then
        env FLAPJACK_ADMIN_KEY="$api_key" "${env_prefix[@]}" \
          "$server_binary" --bind-addr "$bind_addr" --data-dir "$data_dir" \
          >"$server_log_path" 2>&1 &
      else
        env FLAPJACK_ADMIN_KEY="$api_key" \
          "$server_binary" --bind-addr "$bind_addr" --data-dir "$data_dir" \
          >"$server_log_path" 2>&1 &
      fi
      ;;
    *)
      echo "FAIL: unknown server auth mode: $auth_mode"
      exit 1
      ;;
  esac

  local server_pid=$!
  sleep 0.1
  if ! kill -0 "$server_pid" 2>/dev/null; then
    wait "$server_pid" 2>/dev/null || true
    echo "FAIL: server exited during startup; see $server_log_path"
    exit 1
  fi

  printf '%s' "$server_pid"
}

wait_for_loadtest_health() {
  local base_url="$1"
  local server_pid="$2"
  local max_attempts="${3:-300}"
  local sleep_seconds="${4:-0.1}"
  local health_url="${base_url}/health"
  local attempt
  local health_status_code

  for ((attempt = 1; attempt <= max_attempts; attempt += 1)); do
    if [[ -n "$server_pid" ]] && ! kill -0 "$server_pid" 2>/dev/null; then
      wait "$server_pid" 2>/dev/null || true
      echo "FAIL: server exited while waiting for /health"
      exit 1
    fi

    health_status_code="$(
      curl -sS -o /dev/null -w '%{http_code}' --max-time 1 "$health_url" 2>/dev/null || true
    )"
    if [[ "$health_status_code" == "200" ]]; then
      return 0
    fi

    sleep "$sleep_seconds"
  done

  echo "FAIL: timed out waiting for $health_url"
  exit 1
}

stop_loadtest_server() {
  local server_pid="$1"
  if [[ -z "$server_pid" ]]; then
    return 0
  fi

  if kill -0 "$server_pid" 2>/dev/null; then
    kill "$server_pid" 2>/dev/null || true
    for _ in $(seq 1 50); do
      if ! kill -0 "$server_pid" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    if kill -0 "$server_pid" 2>/dev/null; then
      kill -9 "$server_pid" 2>/dev/null || true
    fi
  fi

  wait "$server_pid" 2>/dev/null || true
}

load_shared_loadtest_config() {
  local config_json
  config_json="$(
    cd "$LOADTEST_HELPER_DIR"
    node -e 'import("./config.js").then(({ sharedLoadtestConfig }) => { console.log(JSON.stringify(sharedLoadtestConfig)); }).catch((error) => { console.error(error); process.exit(1); });'
  )"

  FLAPJACK_BASE_URL="$(jq -r '.baseUrl' <<<"$config_json")"
  FLAPJACK_READ_INDEX="$(jq -r '.readIndexName' <<<"$config_json")"
  FLAPJACK_WRITE_INDEX="$(jq -r '.writeIndexName' <<<"$config_json")"
  FLAPJACK_APP_ID="$(jq -r '.appId' <<<"$config_json")"
  FLAPJACK_API_KEY="$(jq -r '.apiKey' <<<"$config_json")"
  FLAPJACK_BENCHMARK_INDEX="$(jq -r '.benchmarkIndexName' <<<"$config_json")"
  FLAPJACK_SOAK_DURATION="$(jq -r '.soakDuration' <<<"$config_json")"
  FLAPJACK_TASK_MAX_ATTEMPTS="$(jq -r '.taskPollMaxAttempts' <<<"$config_json")"
  FLAPJACK_TASK_POLL_INTERVAL_SECONDS="$(jq -r '.taskPollIntervalSeconds' <<<"$config_json")"
}

initialize_loadtest_auth_headers() {
  LOADTEST_AUTH_HEADERS=()
  if [[ -n "${FLAPJACK_API_KEY:-}" ]]; then
    LOADTEST_AUTH_HEADERS=(
      -H "x-algolia-api-key: $FLAPJACK_API_KEY"
      -H "x-algolia-application-id: $FLAPJACK_APP_ID"
    )
  fi
}

loadtest_encode_path_component() {
  local raw_component="$1"
  if [[ -z "$raw_component" ]]; then
    echo "FAIL: URL path component must not be empty."
    exit 1
  fi

  jq -rn --arg component "$raw_component" '$component | @uri'
}

loadtest_http_request() {
  local method="$1"
  local path="$2"
  local payload="${3:-}"
  local expected_statuses_csv="${4:-200}"
  local response_file
  local status_code
  local response_body
  local -a curl_args

  response_file="$(mktemp)"
  curl_args=(curl -sS -o "$response_file" -w '%{http_code}' -X "$method")
  if [[ ${#LOADTEST_AUTH_HEADERS[@]} -gt 0 ]]; then
    curl_args+=("${LOADTEST_AUTH_HEADERS[@]}")
  fi
  if [[ -n "$payload" ]]; then
    curl_args+=(-H "Content-Type: application/json" --data "$payload")
  fi
  curl_args+=("$FLAPJACK_BASE_URL$path")

  status_code="$("${curl_args[@]}")"

  response_body="$(cat "$response_file")"
  rm -f "$response_file"

  if [[ ",${expected_statuses_csv}," != *",${status_code},"* ]]; then
    echo "FAIL: ${method} ${path} returned HTTP ${status_code}."
    echo "$response_body"
    exit 1
  fi

  printf '%s' "$response_body"
}

loadtest_list_indexes_response() {
  loadtest_http_request GET "/1/indexes" "" "200"
}

loadtest_get_index_item_json() {
  local index_name="$1"
  local response
  local item

  response="$(loadtest_list_indexes_response)"
  item="$(jq -cer --arg name "$index_name" '(.items // []) | map(select(.name == $name)) | .[0]' <<<"$response")" || {
    echo "FAIL: index ${index_name} not found in /1/indexes response."
    exit 1
  }

  printf '%s' "$item"
}

loadtest_get_index_doc_count() {
  local index_name="$1"
  loadtest_get_index_item_json "$index_name" | jq -r '.entries // 0'
}

loadtest_get_index_pending_task_count() {
  local index_name="$1"
  loadtest_get_index_item_json "$index_name" | jq -r '.numberOfPendingTasks // 0'
}

loadtest_index_exists() {
  local index_name="$1"
  local response

  response="$(loadtest_list_indexes_response)"
  jq -e --arg name "$index_name" \
    '(.items // []) | any(.name == $name)' \
    <<<"$response" >/dev/null
}

index_doc_count() {
  local base_url="$1"
  local index_name="$2"
  local encoded_index_name
  local response
  local -a curl_args

  encoded_index_name="$(loadtest_encode_path_component "$index_name")" || return 1
  curl_args=(curl -fsS --max-time "${COUNT_REQUEST_TIMEOUT_SECONDS:-5}")
  if [[ ${#LOADTEST_AUTH_HEADERS[@]} -gt 0 ]]; then
    curl_args+=("${LOADTEST_AUTH_HEADERS[@]}")
  fi
  curl_args+=("${base_url}/1/indexes/${encoded_index_name}?query=&hitsPerPage=0")

  response="$("${curl_args[@]}")" || {
    echo "FAIL: unable to read doc count for index ${index_name}." >&2
    return 1
  }
  jq -er '
    .nbHits
    | select(type == "number" and . >= 0 and floor == .)
  ' <<<"$response" || {
    echo "FAIL: search response for index ${index_name} has no integer nbHits." >&2
    return 1
  }
}

wait_for_count_or_stall() {
  local base_url="$1"
  local index_name="$2"
  local target_count="$3"
  local stall_seconds="${4:-60}"
  local poll_seconds="${COUNT_POLL_INTERVAL_SECONDS:-1}"
  local current_count
  local last_count
  local last_progress_ms
  local now_ms
  local stalled_ms

  [[ "$target_count" =~ ^[0-9]+$ ]] || {
    echo "FAIL: target doc count must be a non-negative integer: ${target_count}" >&2
    return 1
  }
  awk -v value="$stall_seconds" 'BEGIN { exit !(value > 0) }' || {
    echo "FAIL: stall seconds must be positive: ${stall_seconds}" >&2
    return 1
  }

  current_count="$(index_doc_count "$base_url" "$index_name")" || return 1
  [[ "$current_count" =~ ^[0-9]+$ ]] || {
    echo "FAIL: doc count is not an integer: ${current_count}" >&2
    return 1
  }
  if (( current_count >= target_count )); then
    return 0
  fi

  last_count="$current_count"
  last_progress_ms="$(node -e 'process.stdout.write(String(Date.now()))')"
  while true; do
    sleep "$poll_seconds"
    current_count="$(index_doc_count "$base_url" "$index_name")" || return 1
    [[ "$current_count" =~ ^[0-9]+$ ]] || {
      echo "FAIL: doc count is not an integer: ${current_count}" >&2
      return 1
    }

    if (( current_count < last_count )); then
      echo "FAIL: doc count regressed from ${last_count} to ${current_count}." >&2
      return 1
    fi
    if (( current_count > last_count )); then
      last_count="$current_count"
      last_progress_ms="$(node -e 'process.stdout.write(String(Date.now()))')"
      echo "INFO: index ${index_name} advanced to ${current_count}/${target_count} docs"
    fi
    if (( current_count >= target_count )); then
      return 0
    fi

    now_ms="$(node -e 'process.stdout.write(String(Date.now()))')"
    stalled_ms=$((now_ms - last_progress_ms))
    if awk -v elapsed_ms="$stalled_ms" -v limit_seconds="$stall_seconds" \
      'BEGIN { exit !(elapsed_ms >= limit_seconds * 1000) }'; then
      echo "STALL: index ${index_name} stayed at ${current_count} docs for ${stall_seconds}s; target=${target_count}." >&2
      return 1
    fi
  done
}

sentinel_search_response() {
  local base_url="$1"
  local index_name="$2"
  local token="$3"
  local encoded_index_name
  local payload
  local -a curl_args

  encoded_index_name="$(loadtest_encode_path_component "$index_name")" || return 1
  payload="$(jq -cn --arg query "$token" '{query: $query, hitsPerPage: 1}')"
  curl_args=(
    curl -fsS --max-time "${COUNT_REQUEST_TIMEOUT_SECONDS:-5}"
    -X POST -H "Content-Type: application/json" --data "$payload"
  )
  if [[ ${#LOADTEST_AUTH_HEADERS[@]} -gt 0 ]]; then
    curl_args+=("${LOADTEST_AUTH_HEADERS[@]}")
  fi
  curl_args+=("${base_url}/1/indexes/${encoded_index_name}/query")
  "${curl_args[@]}"
}

assert_sentinels_top1() {
  local base_url="$1"
  local index_name="$2"
  local rung="$3"
  local sentinel_number

  for sentinel_number in 0 1; do
    local token="xyzzysentinel${rung}${sentinel_number}"
    local expected_object_id="zzsentinel_${rung}_${sentinel_number}"
    local response
    response="$(sentinel_search_response "$base_url" "$index_name" "$token")" || {
      echo "FAIL: sentinel ${expected_object_id} search request failed." >&2
      return 1
    }
    if ! jq -e --arg object_id "$expected_object_id" '
      (.nbHits | type == "number" and . > 0) and
      (.hits | type == "array" and length > 0) and
      .hits[0].objectID == $object_id
    ' <<<"$response" >/dev/null; then
      echo "FAIL: sentinel ${expected_object_id} was not found and ranked #1." >&2
      return 1
    fi
  done
}

extract_loadtest_numeric_task_id() {
  local response_json="$1"
  jq -er '.taskID | select(type == "number")' <<<"$response_json"
}

wait_for_loadtest_task_published() {
  local task_id="$1"
  local attempt

  for ((attempt = 1; attempt <= FLAPJACK_TASK_MAX_ATTEMPTS; attempt += 1)); do
    local task_response
    local task_status
    local pending_task

    task_response="$(loadtest_http_request GET "/1/tasks/${task_id}" "" "200")"
    task_status="$(jq -r '.status // ""' <<<"$task_response")"
    pending_task="$(jq -r 'if has("pendingTask") then (.pendingTask | tostring) else "" end' <<<"$task_response")"

    if [[ "$task_status" == "published" && "$pending_task" == "false" ]]; then
      return 0
    fi

    sleep "$FLAPJACK_TASK_POLL_INTERVAL_SECONDS"
  done

  echo "FAIL: task ${task_id} did not settle to published within ${FLAPJACK_TASK_MAX_ATTEMPTS} polls."
  exit 1
}

load_dashboard_seed_settings() {
  local loadtest_root="${1:-$LOADTEST_HELPER_DIR/..}"

  LOADTEST_SETTINGS_JSON="$(
    cd "$loadtest_root"
    node -e 'import("../dashboard/tour/product-seed-data.mjs").then(({ seedSettings }) => { process.stdout.write(JSON.stringify(seedSettings)); }).catch((error) => { console.error(error); process.exit(1); });'
  )"
}

reset_loadtest_index() {
  local index_name="$1"
  local encoded_index_name
  local create_index_payload
  local delete_response

  encoded_index_name="$(loadtest_encode_path_component "$index_name")"
  create_index_payload="$(jq -cn --arg uid "$index_name" '{ uid: $uid }')"

  delete_response="$(loadtest_http_request DELETE "/1/indexes/${encoded_index_name}" "" "200,404")"
  if jq -e '.taskID | type == "number"' >/dev/null 2>&1 <<<"$delete_response"; then
    wait_for_loadtest_task_published "$(extract_loadtest_numeric_task_id "$delete_response")"
  fi

  loadtest_http_request POST "/1/indexes" "$create_index_payload" "200" >/dev/null
}

# Run a single k6 scenario with JSON + stdout artifact capture.
# Requires globals: SCRIPT_DIR, RESULTS_DIR, SCENARIO_FAILURE_COUNT.
# Requires function: fail.
# k6 exit 0 = pass, exit 99 = threshold breach (counted, non-fatal),
# any other exit = hard failure (abort).
run_k6_scenario() {
  local scenario_name="$1"
  local scenario_path="$2"
  local stdout_path="${RESULTS_DIR}/${scenario_name}.stdout.txt"
  local scenario_exit_code=0

  echo "INFO: running scenario ${scenario_name}"
  (
    cd "$SCRIPT_DIR"
    FLAPJACK_LOADTEST_BASE_URL="$FLAPJACK_BASE_URL" \
      k6 run "$scenario_path"
  ) | tee "$stdout_path" || scenario_exit_code=$?

  if [[ $scenario_exit_code -eq 0 ]]; then
    return 0
  fi
  if [[ $scenario_exit_code -eq 99 ]]; then
    echo "WARN: scenario ${scenario_name} breached thresholds (exit code 99)"
    SCENARIO_FAILURE_COUNT=$((SCENARIO_FAILURE_COUNT + 1))
    return 0
  fi

  fail "scenario ${scenario_name} failed with a hard error (k6 exit code ${scenario_exit_code})"
}

# Smoke is a hard gate — if it breaches thresholds, return 1 so the
# caller can decide how to handle it (abort, return, etc.).
run_smoke_gate() {
  local pre_count="$SCENARIO_FAILURE_COUNT"
  run_k6_scenario "smoke" "scenarios/smoke.js"
  if [[ $SCENARIO_FAILURE_COUNT -gt $pre_count ]]; then
    return 1
  fi
}

apply_loadtest_index_settings() {
  local index_name="$1"
  local encoded_index_name
  local settings_response
  local task_id

  if [[ -z "${LOADTEST_SETTINGS_JSON:-}" ]]; then
    echo "FAIL: LOADTEST_SETTINGS_JSON must be populated before apply_loadtest_index_settings."
    exit 1
  fi

  encoded_index_name="$(loadtest_encode_path_component "$index_name")"
  settings_response="$(loadtest_http_request PUT "/1/indexes/${encoded_index_name}/settings" "$LOADTEST_SETTINGS_JSON" "200,207")"
  task_id="$(extract_loadtest_numeric_task_id "$settings_response")"
  wait_for_loadtest_task_published "$task_id"
}
