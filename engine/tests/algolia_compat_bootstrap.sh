#!/usr/bin/env bash
# Shared bootstrap owner seam for Stage 3/5 Algolia compatibility wrappers.

ALGOLIA_COMPAT_SERVER_PID=""
ALGOLIA_COMPAT_DATA_DIR=""
ALGOLIA_COMPAT_BUILD_LOG=""

cleanup_algolia_compat_harness() {
  if [ -n "${ALGOLIA_COMPAT_SERVER_PID:-}" ] && kill -0 "${ALGOLIA_COMPAT_SERVER_PID}" 2>/dev/null; then
    kill "${ALGOLIA_COMPAT_SERVER_PID}" 2>/dev/null || true
    wait "${ALGOLIA_COMPAT_SERVER_PID}" 2>/dev/null || true
  fi
  if [ -n "${ALGOLIA_COMPAT_BUILD_LOG:-}" ] && [ -f "${ALGOLIA_COMPAT_BUILD_LOG}" ]; then
    rm -f "${ALGOLIA_COMPAT_BUILD_LOG}"
  fi
  if [ -n "${ALGOLIA_COMPAT_DATA_DIR:-}" ] && [ -d "${ALGOLIA_COMPAT_DATA_DIR}" ]; then
    rm -rf "${ALGOLIA_COMPAT_DATA_DIR}"
  fi
}

timestamp_algolia_compat() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

# TODO: Document resolve_algolia_compat_binary.
resolve_algolia_compat_binary() {
  local engine_dir="$1"
  local bin=""

  if [ -n "${FLAPJACK_BIN:-}" ]; then
    if [ ! -x "$FLAPJACK_BIN" ]; then
      echo "ERROR: FLAPJACK_BIN=$FLAPJACK_BIN is not executable"
      return 1
    fi
    bin="$FLAPJACK_BIN"
    echo "Using pre-built binary: $bin" >&2
    printf '%s\n' "$bin"
    return 0
  fi

  echo "Building flapjack-server release binary..." >&2
  ALGOLIA_COMPAT_BUILD_LOG="$(mktemp)"
  if (cd "$engine_dir" && cargo build -p flapjack-server --release >"$ALGOLIA_COMPAT_BUILD_LOG" 2>&1); then
    tail -5 "$ALGOLIA_COMPAT_BUILD_LOG" >&2
  else
    tail -20 "$ALGOLIA_COMPAT_BUILD_LOG" >&2 || true
    echo "ERROR: cargo build -p flapjack-server --release failed" >&2
    return 1
  fi

  bin="$engine_dir/target/release/flapjack"
  if [ ! -x "$bin" ]; then
    echo "ERROR: build succeeded but binary missing at $bin"
    return 1
  fi

  printf '%s\n' "$bin"
}

assert_algolia_compat_port_free() {
  local port="$1"
  if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
    echo "ERROR: port $port already has a listening process"
    return 1
  fi
}

# TODO: Document wait_for_algolia_compat_health.
wait_for_algolia_compat_health() {
  local base_url="$1"
  local server_log="$2"
  local health_ok="false"

  for _i in $(seq 1 60); do
    if ! kill -0 "$ALGOLIA_COMPAT_SERVER_PID" 2>/dev/null; then
      echo "ERROR: server exited before readiness"
      cat "$server_log" 2>/dev/null || true
      return 1
    fi
    if curl -sf "$base_url/health" >/dev/null 2>&1; then
      health_ok="true"
      break
    fi
    sleep 0.5
  done

  if [ "$health_ok" != "true" ]; then
    echo "ERROR: /health was not ready within 30s at $base_url/health"
    cat "$server_log" 2>/dev/null || true
    return 1
  fi
}

# TODO: Document ensure_algolia_compat_sdk_dependencies.
ensure_algolia_compat_sdk_dependencies() {
  local sdk_dir="$1"

  if [ ! -d "$sdk_dir/node_modules" ]; then
    echo "sdk_test/node_modules missing; running npm ci"
    (cd "$sdk_dir" && npm ci)
    return
  fi

  if (cd "$sdk_dir" && node -e "require.resolve('algoliasearch'); require.resolve('dotenv')" >/dev/null 2>&1); then
    echo "sdk_test dependencies present; reusing existing node_modules"
  else
    echo "sdk_test dependency probe failed; running npm ci"
    (cd "$sdk_dir" && npm ci)
  fi
}

# TODO: Document run_algolia_compat_npm_script.
run_algolia_compat_npm_script() {
  local sdk_dir="$1"
  local npm_script="$2"
  local server_log="$3"
  local node_exit=0
  
  echo "Node command: (cd $sdk_dir && FLAPJACK_URL=$FLAPJACK_URL FLAPJACK_ADMIN_KEY=<redacted> npm run $npm_script)"

  set +e
  (cd "$sdk_dir" && npm run "$npm_script")
  node_exit=$?
  set -e

  if [ "$node_exit" -ne 0 ]; then
    echo "--- Server log tail (failure) ---"
    tail -80 "$server_log" 2>/dev/null || true
    echo "VERDICT: FAIL"
    return "$node_exit"
  fi

  echo "--- Server log tail ---"
  tail -40 "$server_log" 2>/dev/null || true
  echo "VERDICT: PASS"
}

initialize_algolia_compat_harness_state() {
  ALGOLIA_COMPAT_SERVER_PID=""
  ALGOLIA_COMPAT_DATA_DIR=""
  ALGOLIA_COMPAT_BUILD_LOG=""
  trap cleanup_algolia_compat_harness EXIT
}

# TODO: Document prepare_algolia_compat_server_env.
prepare_algolia_compat_server_env() {
  local default_bind_addr="$1"
  local admin_key_prefix="$2"

  local bind_addr="" port="" admin_key=""

  bind_addr="${FLAPJACK_BIND_ADDR:-$default_bind_addr}"
  port="${bind_addr##*:}"
  assert_algolia_compat_port_free "$port" || return 1

  ALGOLIA_COMPAT_DATA_DIR="$(mktemp -d)"
  admin_key="${admin_key_prefix}_$(date +%s)"

  export FLAPJACK_ADMIN_KEY="$admin_key"
  export FLAPJACK_BIND_ADDR="$bind_addr"
  export FLAPJACK_DATA_DIR="$ALGOLIA_COMPAT_DATA_DIR"
  export FLAPJACK_URL="http://${bind_addr}"
}

# TODO: Document start_algolia_compat_server.
start_algolia_compat_server() {
  local bin="$1"
  local server_log_name="$2"
  local server_log_ref="$3"
  
  local resolved_log_path=""
  if [[ ! "$server_log_ref" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "ERROR: invalid server-log variable name '$server_log_ref'" >&2
    return 1
  fi
  resolved_log_path="$ALGOLIA_COMPAT_DATA_DIR/$server_log_name"
  # Create the log path before background launch so callers can inspect/tail it immediately.
  : >"$resolved_log_path"
  "$bin" >"$resolved_log_path" 2>&1 &
  ALGOLIA_COMPAT_SERVER_PID=$!
  
  printf -v "$server_log_ref" '%s' "$resolved_log_path"
}

print_algolia_compat_server_details() {
  local server_log="$1"
  echo "Server PID: $ALGOLIA_COMPAT_SERVER_PID"
  echo "Server log: $server_log"
}

# TODO: Document run_algolia_compat_harness.
run_algolia_compat_harness() {
  local stage_label="$1"
  local default_bind_addr="$2"
  local admin_key_prefix="$3"
  local server_log_name="$4"
  local npm_script="$5"
  local print_direct_rerun_env="${6:-false}"

  local script_dir="" engine_dir="" sdk_dir=""
  local server_log="" bin=""

  script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  engine_dir="$(cd "$script_dir/.." && pwd)"
  sdk_dir="$engine_dir/sdk_test"

  initialize_algolia_compat_harness_state

  echo "=== $stage_label ==="
  echo "Started: $(timestamp_algolia_compat)"

  bin="$(resolve_algolia_compat_binary "$engine_dir")" || return 1
  prepare_algolia_compat_server_env "$default_bind_addr" "$admin_key_prefix" || return 1

  start_algolia_compat_server "$bin" "$server_log_name" server_log
  print_algolia_compat_server_details "$server_log"

  wait_for_algolia_compat_health "$FLAPJACK_URL" "$server_log" || return 1
  echo "Health ready: $FLAPJACK_URL/health"

  if [ "$print_direct_rerun_env" = "true" ]; then
    echo "Direct rerun env: FLAPJACK_URL=$FLAPJACK_URL FLAPJACK_ADMIN_KEY=<redacted>"
  fi

  ensure_algolia_compat_sdk_dependencies "$sdk_dir" || return 1

  run_algolia_compat_npm_script "$sdk_dir" "$npm_script" "$server_log"
}
