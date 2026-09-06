#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SDK_TEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$(cd "$SDK_TEST_DIR/.." && pwd)"
RUNNER="$ENGINE_DIR/s/test"
TMP_DIR="$(mktemp -d)"
BIN_DIR="$TMP_DIR/bin"
OUTPUT_FILE="$TMP_DIR/output.log"
export STUB_NPM_LOG="$TMP_DIR/npm.log" STUB_NODE_LOG="$TMP_DIR/node.log"
source "$SCRIPT_DIR/sdk_runner_contract_support.sh"

cleanup() {
  if [ -f "$TMP_DIR/private-forwarder" ]; then
    rm -f "$RUNNER"
    mv "$TMP_DIR/private-forwarder" "$RUNNER"
    rm -f "$ENGINE_DIR/s/lib" "$ENGINE_DIR/s/manual-tests"
  fi
  restore_runner_scripts
  local project
  for project in sdk_test dashboard console; do
    [ -f "$TMP_DIR/$project.prepared" ] || continue
    rm -rf "$ENGINE_DIR/$project/node_modules"
    if [ -e "$TMP_DIR/$project.node_modules" ]; then
      mv "$TMP_DIR/$project.node_modules" "$ENGINE_DIR/$project/node_modules"
    fi
  done
  if [ -f "$TMP_DIR/dist.created" ]; then
    rm -rf "$ENGINE_DIR/dashboard/dist"
  elif [ -f "$TMP_DIR/index.created" ]; then
    rm -f "$ENGINE_DIR/dashboard/dist/index.html"
  fi
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

prepare_modules() {
  local project="$1"
  local modules="$ENGINE_DIR/$project/node_modules"
  if [ -e "$modules" ]; then
    mv "$modules" "$TMP_DIR/$project.node_modules"
  fi
  touch "$TMP_DIR/$project.prepared"
  mkdir -p "$modules"
  cksum "$ENGINE_DIR/$project/package-lock.json" | awk '{print $1 ":" $2}' > "$modules/.flapjack-package-lock.cksum"
}

mkdir -p "$BIN_DIR"
for project in sdk_test dashboard console; do prepare_modules "$project"; done
if [ ! -d "$ENGINE_DIR/dashboard/dist" ]; then
  touch "$TMP_DIR/dist.created"
elif [ ! -f "$ENGINE_DIR/dashboard/dist/index.html" ]; then
  touch "$TMP_DIR/index.created"
fi
setup_python_contract_stub
stub_protocol_smokes
if [ -f "$ENGINE_DIR/_dev/s/manual-tests/cli_smoke.sh" ]; then
  stub_runner_script "$ENGINE_DIR/_dev/s/manual-tests/cli_smoke.sh"
else
  stub_runner_script "$ENGINE_DIR/s/manual-tests/cli_smoke.sh"
fi
configure_synthetic_runner_origin

cat > "$BIN_DIR/node" <<'STUB'
#!/bin/bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_NODE_LOG"
case "${1:-}" in
  --version) echo v20.10.0 ;;
  -e)
    case "$2" in
      *server.address*) echo "$FJ_BACKEND_PORT" ;;
      *randomUUID*) echo synthetic-console-instance ;;
      *require.resolve*) exit 0 ;;
      *) exit 65 ;;
    esac ;;
  test.js|contract_tests.js|full_compat_tests.js|instantsearch_contract_tests.js) ;;
  scripts/playwright-webserver.mjs) ;;
  *) exit 66 ;;
esac
STUB
cat > "$BIN_DIR/npm" <<'STUB'
#!/bin/bash
printf 'cwd=%s args=%s\n' "$PWD" "$*" >> "$STUB_NPM_LOG"
case "$*" in
  'run test:real_clients'|'run test:unit:run'|'run test:e2e-ui:smoke'|'run test:e2e-ui:full') exit 0 ;;
  --prefix*' run test:unit:run'|--prefix*' run check'|--prefix*' run build'|--prefix*' run lint:browser-tests:unmocked'|--prefix*' run test:browser:unmocked') exit 0 ;;
  *) exit 69 ;;
esac
STUB
cat > "$BIN_DIR/curl" <<'STUB'
#!/bin/bash
case "$*" in
  */health*|*/1/keys*) echo '{"status":"ok"}' ;;
  *) exit 67 ;;
esac
STUB
cat > "$BIN_DIR/cargo" <<'STUB'
#!/bin/bash
case "$1" in
  test|nextest|build) exit 0 ;;
  *) exit 68 ;;
esac
STUB
cat > "$BIN_DIR/bash" <<'STUB'
#!/bin/bash
case "$1" in
  */scripts/tests/rwork_repository_contract_test.sh|*/scripts/tests/publish_guard_test.sh|*/scripts/tests/publish_public_candidate_test.sh|*/scripts/check_migration_ssot_owners.sh) exit 0 ;;
  *) exec /bin/bash "$@" ;;
esac
STUB
chmod +x "$BIN_DIR/"*

assert_count() {
  local expected="$1" needle="$2" file="$3"
  local count
  count=$(grep -Fxc "$needle" "$file" || true)
  if [ "$count" != "$expected" ]; then
    echo "Expected $expected occurrences of $needle; got $count"
    cat "$file" "$OUTPUT_FILE"
    exit 1
  fi
}

run_mode() {
  local python_exit="$1"
  shift
  local prefix=E2E status=0 script_name
  [ "$*" != --sdk ] || prefix=SDK
  : > "$STUB_NPM_LOG"
  : > "$STUB_NODE_LOG"
  : > "$STUB_PYTHON_LOG"
  PATH="$BIN_DIR:$PATH" STUB_PYTHON_EXIT="$python_exit" \
    "$RUNNER" "$@" > "$OUTPUT_FILE" 2>&1 || status=$?
  assert_python_contract_execution
  if [ "$status" != "$python_exit" ]; then
    echo "Expected runner $* to return Python status $python_exit; got $status"
    cat "$OUTPUT_FILE"
    exit 1
  fi
  grep -Fq "$prefix: official Python client contract" "$OUTPUT_FILE"
  for script_name in test.js contract_tests.js full_compat_tests.js instantsearch_contract_tests.js; do
    assert_count 1 "$script_name" "$STUB_NODE_LOG"
  done
  assert_count 1 "cwd=$SDK_TEST_DIR args=run test:real_clients" "$STUB_NPM_LOG"
  if [ "$python_exit" = 0 ]; then
    grep -Fq 'Official Python client contract passed' "$OUTPUT_FILE"
  elif grep -Fq 'Official Python client contract passed' "$OUTPUT_FILE"; then
    echo 'Failed Python execution must not print success'
    exit 1
  fi
  if [ "$prefix" = SDK ]; then
    grep -Fq 'SDK: Python protocol smoke test' "$OUTPUT_FILE"
    grep -Fq 'Python protocol smoke test passed' "$OUTPUT_FILE"
  elif grep -Eq 'SDK: (JS|.*protocol smoke)' "$OUTPUT_FILE"; then
    echo 'E2E must subsume SDK JS/browser and exclude protocol smokes'
    exit 1
  fi
  echo "PASS: $* executes official Python exactly once (exit $python_exit), JS/browser once, and preserves protocol-smoke scope"
}

run_all_modes() {
  run_mode 0 --sdk
  run_mode 81 --sdk
  run_mode 0 --e2e
  run_mode 82 --e2e
  run_mode 0 --all
  run_mode 83 --all
  run_mode 0 --sdk --e2e
  run_mode 84 --sdk --e2e
}

run_all_modes

# Exercise the public remap using the same runner bytes, without copying it.
if [ -f "$ENGINE_DIR/_dev/s/test" ]; then
  [ ! -e "$ENGINE_DIR/s/lib" ] && [ ! -e "$ENGINE_DIR/s/manual-tests" ]
  mv "$RUNNER" "$TMP_DIR/private-forwarder"
  ln -s ../_dev/s/test "$RUNNER"
  ln -s ../_dev/s/lib "$ENGINE_DIR/s/lib"
  ln -s ../_dev/s/manual-tests "$ENGINE_DIR/s/manual-tests"
  run_all_modes
  echo 'PASS: public remap preserves engine-root resolution and SDK/E2E dispatch'
fi
