#!/usr/bin/env bash

# Focused lifecycle regression for the managed-startup HTTP fixture seam.
# The sourcing harness supplies pass/fail accounting and the shared probe API.

run_http_fixture_readiness_cleanup_regression() {
  local regression_root="$1"
  local lifecycle_root="$regression_root/readiness-failure"
  local pid_path="$lifecycle_root/server.pid"
  local child_pid
  local probe_rc=0

  mkdir -m 700 "$lifecycle_root"
  cat > "$lifecycle_root/fake-server" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$$" > "$HTTP_PROBE_LIFECYCLE_PID_PATH"
trap 'exit 0' TERM
while true; do sleep 1; done
SH
  cat > "$lifecycle_root/fail-readiness" <<'SH'
#!/usr/bin/env bash
for _ in $(seq 1 100); do
  [ -f "$HTTP_PROBE_LIFECYCLE_PID_PATH" ] && exit 1
  sleep 0.01
done
exit 2
SH
  chmod +x "$lifecycle_root/fake-server" "$lifecycle_root/fail-readiness"

  HTTP_PROBE_LIFECYCLE_PID_PATH="$pid_path" bash -c '
    set -uo pipefail
    source "$1"
    TMP_ROOT="$2"
    BIN="$2/fake-server"
    WAIT_FOR_FLAPJACK="$2/fail-readiness"
    SERVER_PID=""
    BASE=""
    HTTP_PROBE_FIXTURE_ENVIRONMENT_JSON="$(jq -cn \
      --arg path "$HTTP_PROBE_LIFECYCLE_PID_PATH" \
      '\''{HTTP_PROBE_LIFECYCLE_PID_PATH:$path}'\'')"
    HTTP_PROBE_FIXTURE_WORKING_DIRECTORY="$2"
    HTTP_PROBE_FIXTURE_DATA_DIRECTORY=""
    http_probe_start_server
  ' bash "$SCRIPT_DIR/common/http_probe_lib.sh" "$lifecycle_root" \
    >/dev/null 2>&1 || probe_rc=$?

  if [ ! -f "$pid_path" ]; then
    fail "fixture HTTP readiness failure must launch its exact child before probing"
    return
  fi
  child_pid="$(<"$pid_path")"
  if kill -0 "$child_pid" 2>/dev/null; then
    kill "$child_pid" 2>/dev/null || true
    fail "fixture HTTP readiness failure stops and waits for its exact child (probe_rc=$probe_rc)"
  elif [ "$probe_rc" -ne 0 ]; then
    pass "fixture HTTP readiness failure stops and waits for its exact child"
  else
    fail "fixture HTTP readiness failure must return nonzero"
  fi
}

write_http_fixture_failure_programs() {
  local lifecycle_root="$1"

  cat > "$lifecycle_root/fake-server" <<'SH'
#!/usr/bin/env bash
printf '%s\n' "$$" > "$HTTP_PROBE_LIFECYCLE_PID_PATH"
if [ "$HTTP_PROBE_LIFECYCLE_FAILURE_MODE" = startup ]; then
  exit 17
fi
printf 'Local: http://127.0.0.1:1\n'
trap 'exit 0' TERM
while true; do sleep 1; done
SH
  cat > "$lifecycle_root/wait-readiness" <<'SH'
#!/usr/bin/env bash
for _ in $(seq 1 100); do
  [ -f "$HTTP_PROBE_LIFECYCLE_PID_PATH" ] && break
  sleep 0.01
done
[ "$HTTP_PROBE_LIFECYCLE_FAILURE_MODE" != startup ]
SH
  cat > "$lifecycle_root/curl" <<'SH'
#!/usr/bin/env bash
printf '%s\n' '{"replication_enabled":true,"peer_count":0}'
SH
  chmod +x "$lifecycle_root/fake-server" "$lifecycle_root/wait-readiness" \
    "$lifecycle_root/curl"
}

run_http_fixture_failure_cleanup_case() {
  local regression_root="$1"
  local failure_mode="$2"
  local lifecycle_root="$regression_root/$failure_mode-failure"
  local runtime_root="$lifecycle_root/runtime"
  local pid_path="$lifecycle_root/server.pid"
  local assertion_path="$lifecycle_root/assertion-observed"
  local child_pid
  local probe_rc=0

  mkdir -m 700 "$lifecycle_root"
  write_http_fixture_failure_programs "$lifecycle_root"

  HTTP_PROBE_LIFECYCLE_PID_PATH="$pid_path" \
  HTTP_PROBE_LIFECYCLE_ASSERTION_PATH="$assertion_path" \
  HTTP_PROBE_LIFECYCLE_FAILURE_MODE="$failure_mode" \
  PATH="$lifecycle_root:$PATH" bash -c '
    set -uo pipefail
    source "$1"
    source "$2"
    pass() { :; }
    fail() { : > "$HTTP_PROBE_LIFECYCLE_ASSERTION_PATH"; }
    ENGINE_DIR="$3"
    BIN="$4/fake-server"
    WAIT_FOR_FLAPJACK="$4/wait-readiness"
    SERVER_PID=""
    BASE=""
    consumed="$(jq -cn \
      --arg work "$4/work" \
      --arg pid_path "$HTTP_PROBE_LIFECYCLE_PID_PATH" \
      --arg mode "$HTTP_PROBE_LIFECYCLE_FAILURE_MODE" \
      '\''{working_directory:$work,runtime_environment:{
        FLAPJACK_ADMIN_KEY:"synthetic-cleanup-key",
        HTTP_PROBE_LIFECYCLE_PID_PATH:$pid_path,
        HTTP_PROBE_LIFECYCLE_FAILURE_MODE:$mode
      }}'\'')"
    mkdir -m 700 "$4/work"
    if [ "$HTTP_PROBE_LIFECYCLE_FAILURE_MODE" = signal ]; then
      generated_status_summary() { kill -TERM "$$"; return 1; }
    fi
    run_generated_http_fixture_case cleanup-control "$consumed" false 0 "$5"
  ' bash "$SCRIPT_DIR/common/http_probe_lib.sh" \
    "$SCRIPT_DIR/common/managed_fleet_ha_http_fixture_test.sh" \
    "$ENGINE_DIR" "$lifecycle_root" "$runtime_root" >/dev/null 2>&1 || probe_rc=$?

  child_pid="$(<"$pid_path")"
  if kill -0 "$child_pid" 2>/dev/null; then
    kill "$child_pid" 2>/dev/null || true
    wait "$child_pid" 2>/dev/null || true
    fail "fixture HTTP $failure_mode failure stops and waits for its exact child (probe_rc=$probe_rc)"
  elif [ -d "$runtime_root" ]; then
    fail "fixture HTTP $failure_mode failure removes its task-owned runtime directory"
  elif [ "$failure_mode" != assertion ] || [ -f "$assertion_path" ]; then
    pass "fixture HTTP $failure_mode failure stops its exact child and removes private artifacts"
  else
    fail "fixture HTTP assertion cleanup control must reach the failed assertion"
  fi
}

run_http_fixture_failure_cleanup_regressions() {
  local regression_root="$1"

  run_http_fixture_failure_cleanup_case "$regression_root" startup
  run_http_fixture_failure_cleanup_case "$regression_root" assertion
  run_http_fixture_failure_cleanup_case "$regression_root" signal
}

write_generated_status_curl_config() {
  local config_path="$1"

  (umask 077; : > "$config_path")
  printf 'header = "X-Algolia-Application-ID: flapjack"\n' >> "$config_path"
  printf 'header = "X-Algolia-API-Key: %s"\n' "$ADMIN_KEY" >> "$config_path"
}

generated_status_summary() {
  local config_path="$1"

  curl --disable --noproxy '*' --max-time 10 --fail --silent --show-error \
    --config "$config_path" "$BASE/internal/status" \
    | jq -c '{replication_enabled,peer_count}'
}

cleanup_generated_http_fixture_runtime() {
  local runtime_root="$1"
  local cleanup_rc=0

  http_probe_stop_server || cleanup_rc=$?
  SERVER_PID=""
  unset FLAPJACK_REPLICATION_API_KEY
  rm -rf -- "$runtime_root"
  return "$cleanup_rc"
}

abort_generated_http_fixture_runtime() {
  local signal_name="$1"
  local runtime_root="$2"
  local signal_exit_code=1

  cleanup_generated_http_fixture_runtime "$runtime_root" || true
  case "$signal_name" in
    HUP) signal_exit_code=129 ;;
    INT) signal_exit_code=130 ;;
    TERM) signal_exit_code=143 ;;
  esac
  exit "$signal_exit_code"
}

restore_generated_http_fixture_traps() {
  local saved_hup="$1"
  local saved_int="$2"
  local saved_term="$3"

  trap - HUP INT TERM
  [ -z "$saved_hup" ] || eval "$saved_hup"
  [ -z "$saved_int" ] || eval "$saved_int"
  [ -z "$saved_term" ] || eval "$saved_term"
}

write_generated_http_baseline_decoy() {
  local case_name="$1"
  local runtime_root="$2"

  [ "$case_name" = baseline ] || return 0
  mkdir -p "$runtime_root/data"
  printf '%s\n' '{"node_id":"launcher-decoy","bind_addr":"127.0.0.1:7700","advertise_addr":"https://launcher-decoy.example.invalid:8443","peers":[]}' \
    > "$runtime_root/data/node.json"
}

configure_generated_http_fixture_runtime() {
  local runtime_root="$1"
  local consumed="$2"
  local explicit_data_directory="$3"

  TMP_ROOT="$runtime_root"
  SERVER_PID=""
  BASE=""
  APP_ID="flapjack"
  ADMIN_KEY="$(printf '%s\n' "$consumed" | jq -er '.runtime_environment.FLAPJACK_ADMIN_KEY')"
  HTTP_PROBE_FIXTURE_ENVIRONMENT_JSON="$(printf '%s\n' "$consumed" | jq -c '.runtime_environment')"
  HTTP_PROBE_FIXTURE_WORKING_DIRECTORY="$(printf '%s\n' "$consumed" | jq -er '.working_directory')"
  HTTP_PROBE_FIXTURE_DATA_DIRECTORY="$explicit_data_directory"
  export FLAPJACK_REPLICATION_API_KEY="legacy-launcher-decoy-key"
}

run_generated_http_fixture_case() {
  local case_name="$1"
  local consumed="$2"
  local expected_replication_enabled="$3"
  local expected_control_exit="$4"
  local runtime_root="$5"
  local explicit_data_directory="${6:-}"
  local curl_config="$runtime_root/status.curl-config"
  local summary=""
  local control_exit=0
  local curl_config_mode
  local saved_hup saved_int saved_term

  mkdir -m 700 "$runtime_root"
  configure_generated_http_fixture_runtime \
    "$runtime_root" "$consumed" "$explicit_data_directory"
  saved_hup="$(trap -p HUP)"
  saved_int="$(trap -p INT)"
  saved_term="$(trap -p TERM)"
  trap 'abort_generated_http_fixture_runtime HUP "$runtime_root"' HUP
  trap 'abort_generated_http_fixture_runtime INT "$runtime_root"' INT
  trap 'abort_generated_http_fixture_runtime TERM "$runtime_root"' TERM

  write_generated_http_baseline_decoy "$case_name" "$runtime_root"

  if ! http_probe_start_server; then
    cleanup_generated_http_fixture_runtime "$runtime_root" || true
    restore_generated_http_fixture_traps "$saved_hup" "$saved_int" "$saved_term"
    fail "generated $case_name server must reach authenticated readiness"
    return
  fi
  write_generated_status_curl_config "$curl_config"
  curl_config_mode="$(python3 -c 'import os, stat, sys; print(oct(stat.S_IMODE(os.stat(sys.argv[1]).st_mode)))' "$curl_config")"
  summary="$(generated_status_summary "$curl_config")" || summary="request_failed"
  if curl --disable --noproxy '*' --max-time 10 --fail --silent --show-error \
      --config "$curl_config" "$BASE/internal/status" \
      | jq -e '(.replication_enabled | type) == "boolean" and .replication_enabled == false' \
      >/dev/null; then
    control_exit=0
  else
    control_exit=$?
  fi
  cleanup_generated_http_fixture_runtime "$runtime_root" || control_exit=125
  restore_generated_http_fixture_traps "$saved_hup" "$saved_int" "$saved_term"

  if printf '%s\n' "$summary" | jq -e \
      --argjson enabled "$expected_replication_enabled" \
      '.replication_enabled == $enabled and .peer_count == 0' >/dev/null \
      && [ "$control_exit" -eq "$expected_control_exit" ] \
      && [ "$curl_config_mode" = 0o600 ]; then
    pass "generated $case_name authenticated status replication_enabled=$expected_replication_enabled peer_count=0 control_exit=$expected_control_exit curl_config_mode=0600"
  else
    fail "generated $case_name authenticated status expected enabled=$expected_replication_enabled peer_count=0 control_exit=$expected_control_exit mode=0600 observed=$summary control_exit=$control_exit mode=$curl_config_mode"
  fi
}

run_generated_http_fixture_regressions() {
  local regression_root="$1"
  local baseline_artifact="$2"
  local producer_sha="$3"
  local baseline_consumed="$4"
  local advertise_artifact="$regression_root/http-advertise.json"
  local advertise_consumed
  local explicit_data_directory

  TMP_ROOT="$regression_root/http-build"
  mkdir -m 700 "$TMP_ROOT"
  http_probe_resolve_default_binary
  run_generated_http_fixture_case baseline "$baseline_consumed" false 0 \
    "$regression_root/http-baseline-runtime"

  cp -p "$baseline_artifact" "$advertise_artifact"
  python3 - "$advertise_artifact" <<'PY'
import json
import sys

path = sys.argv[1]
with open(path, encoding="utf-8") as source:
    artifact = json.load(source)
artifact["test_mutation"] = "advertise"
artifact["service_env"] += (
    "FLAPJACK_ADVERTISE_ADDR=https://node-managed-startup-proof.example.invalid\n"
    "FLAPJACK_REPLICATION_API_KEY=managed-startup-proof-replication-key\n"
)
with open(path, "w", encoding="utf-8") as output:
    json.dump(artifact, output, sort_keys=True, separators=(",", ":"))
PY
  advertise_consumed="$(managed_fleet_ha_consume_generated_artifact \
    "$advertise_artifact" "$producer_sha" advertise "$regression_root/http-advertise-mapped")"
  run_generated_http_fixture_case advertise "$advertise_consumed" true 1 \
    "$regression_root/http-advertise-runtime"

  explicit_data_directory="$(dirname "$(printf '%s\n' "$advertise_consumed" \
    | jq -er '.working_directory')")/explicit/data"
  mkdir -p "$explicit_data_directory"
  printf '%s\n' '{"node_id":"explicit-runtime-node","bind_addr":"127.0.0.1:7700","peers":[]}' \
    > "$explicit_data_directory/node.json"
  run_generated_http_fixture_case explicit-directory "$advertise_consumed" false 0 \
    "$regression_root/http-explicit-runtime" "$explicit_data_directory"
}
