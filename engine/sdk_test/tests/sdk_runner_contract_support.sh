#!/usr/bin/env bash
# Shared command-boundary fixtures for the real SDK runner regressions.

setup_python_contract_stub() {
  export FLAPJACK_SDK_PYTHON_CLIENT_CACHE="$TMP_DIR/python-cache"
  export FLAPJACK_SDK_PYTHON="$BIN_DIR/python-contract"
  export STUB_PYTHON_LOG="$TMP_DIR/python.log"
  mkdir -p "$FLAPJACK_SDK_PYTHON_CLIENT_CACHE/python-client-contract/bin"
  : > "$STUB_PYTHON_LOG"
  cat > "$FLAPJACK_SDK_PYTHON" <<'STUB'
#!/bin/bash
set -euo pipefail
case "${1:-}" in
  -c) exit 0 ;;
  -m)
    [[ "$2" == pip && "$3" == install && "$4" == -r ]] || exit 65
    exit 0 ;;
  */python_client_contract_test.py)
    printf 'cwd=%s url=%s key=%s program=%s\n' "$PWD" "$FLAPJACK_URL" "$FLAPJACK_ADMIN_KEY" "$1" >> "$STUB_PYTHON_LOG"
    exit "${STUB_PYTHON_EXIT:-0}" ;;
  *) exit 66 ;;
esac
STUB
  chmod +x "$FLAPJACK_SDK_PYTHON"
  cp "$FLAPJACK_SDK_PYTHON" "$FLAPJACK_SDK_PYTHON_CLIENT_CACHE/python-client-contract/bin/python"
}

stub_runner_script() {
  local script_path="$1"
  local backup="$TMP_DIR/scripts/${script_path#"$ENGINE_DIR/"}"
  mkdir -p "$(dirname "$backup")"
  cp -p "$script_path" "$backup"
  cat > "$script_path" <<'STUB'
#!/bin/bash
exit 0
STUB
}

restore_runner_scripts() {
  if [ -d "$TMP_DIR/scripts" ]; then
    cp -pR "$TMP_DIR/scripts/." "$ENGINE_DIR/"
  fi
}

stub_protocol_smokes() {
  local language
  for language in php python ruby go java swift; do
    stub_runner_script "$SDK_TEST_DIR/${language}_smoke_test.sh"
  done
}

assert_python_contract_execution() {
  local expected="cwd=$SDK_TEST_DIR url=$FLAPJACK_BACKEND_URL key=$FJ_TEST_ADMIN_KEY program=$SDK_TEST_DIR/python_client_contract_test.py"
  local count
  count=$(grep -Fxc "$expected" "$STUB_PYTHON_LOG" || true)
  if [ "$count" != 1 ] || [ "$(wc -l < "$STUB_PYTHON_LOG" | tr -d ' ')" != 1 ]; then
    echo "Expected exactly one official Python execution with SDK cwd and configured URL/key; got $count"
    cat "$STUB_PYTHON_LOG" "$OUTPUT_FILE"
    exit 1
  fi
}

configure_synthetic_runner_origin() {
  export FJ_HOST=127.0.0.1 FJ_BACKEND_PORT=19773
  export FLAPJACK_BACKEND_URL="http://$FJ_HOST:$FJ_BACKEND_PORT"
  export FJ_TEST_ADMIN_KEY=synthetic-runner-contract-key
}

run_sdk_until_python_failure() {
  setup_python_contract_stub
  stub_protocol_smokes
  configure_synthetic_runner_origin
  : > "$NPM_LOG"
  : > "$NODE_LOG"
  local status=0
  PATH="$BIN_DIR:$PATH" STUB_NPM_LOG="$NPM_LOG" STUB_NODE_LOG="$NODE_LOG" \
    STUB_JS_EXIT=0 STUB_PYTHON_EXIT=87 "$RUNNER" --sdk >"$OUTPUT_FILE" 2>&1 || status=$?
  assert_python_contract_execution
  if [ "$status" != 87 ]; then
    echo "Expected official Python exit 87 to fail the runner; got $status"
    cat "$OUTPUT_FILE"
    exit 1
  fi
}
