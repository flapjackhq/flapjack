#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
SDK_TEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
ENGINE_DIR="$(cd "$SDK_TEST_DIR/.." && pwd)"
RUNNER="$ENGINE_DIR/s/test"

TMP_DIR="$(mktemp -d)"
BIN_DIR="$TMP_DIR/bin"
OUTPUT_FILE="$TMP_DIR/output.log"
NPM_LOG="$TMP_DIR/npm.log"
NODE_LOG="$TMP_DIR/node.log"

SDK_NODE_MODULES="$ENGINE_DIR/sdk_test/node_modules"
SDK_NODE_MODULES_BACKUP="$TMP_DIR/sdk_node_modules.backup"
SDK_NODE_MODULES_WAS_PRESENT=false
SDK_LOCKFILE="$ENGINE_DIR/sdk_test/package-lock.json"

resolve_cli_smoke_script() {
  local canonical_path="$ENGINE_DIR/s/manual-tests/cli_smoke.sh"
  local dev_path="$ENGINE_DIR/_dev/s/manual-tests/cli_smoke.sh"

  if [ -f "$canonical_path" ]; then
    echo "$canonical_path"
    return 0
  fi

  if [ -f "$dev_path" ]; then
    echo "$dev_path"
    return 0
  fi

  echo "Unable to locate cli_smoke.sh under canonical or dev runner paths" >&2
  exit 1
}

CLI_SMOKE_SCRIPT="$(resolve_cli_smoke_script)"
CLI_SMOKE_MODE_BEFORE=""

file_mode() {
  local target="$1"
  if stat -f '%Lp' "$target" >/dev/null 2>&1; then
    stat -f '%Lp' "$target"
  else
    stat -c '%a' "$target"
  fi
}

cleanup() {
  if [ -n "$CLI_SMOKE_MODE_BEFORE" ]; then
    chmod "$CLI_SMOKE_MODE_BEFORE" "$CLI_SMOKE_SCRIPT" || true
  fi

  rm -rf "$SDK_NODE_MODULES"

  if [ "$SDK_NODE_MODULES_WAS_PRESENT" = "true" ]; then
    mv "$SDK_NODE_MODULES_BACKUP" "$SDK_NODE_MODULES"
  fi

  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

mkdir -p "$BIN_DIR"
: > "$NPM_LOG"
: > "$NODE_LOG"

if [ -e "$SDK_NODE_MODULES" ]; then
  SDK_NODE_MODULES_WAS_PRESENT=true
  mv "$SDK_NODE_MODULES" "$SDK_NODE_MODULES_BACKUP"
fi

mkdir -p "$SDK_NODE_MODULES/algoliasearch" "$SDK_NODE_MODULES/dotenv"
printf '%s\n' "$(cksum "$SDK_LOCKFILE" | awk '{print $1 ":" $2}')" > "$SDK_NODE_MODULES/.flapjack-package-lock.cksum"

CLI_SMOKE_MODE_BEFORE="$(file_mode "$CLI_SMOKE_SCRIPT")"
chmod a-x "$CLI_SMOKE_SCRIPT"

cat > "$BIN_DIR/curl" <<'WRAP'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == *"/health"* ]]; then
  echo '{"status":"ok"}'
  exit 0
fi
if [[ "$*" == *"/1/keys"* ]]; then
  exit 0
fi
exit 0
WRAP
chmod +x "$BIN_DIR/curl"

cat > "$BIN_DIR/npm" <<'WRAP'
#!/usr/bin/env bash
set -euo pipefail
printf 'cwd=%s args=%s\n' "$PWD" "$*" >> "$STUB_NPM_LOG"
if [[ "${1:-}" == "ci" ]]; then
  exit 0
fi
exit 0
WRAP
chmod +x "$BIN_DIR/npm"

cat > "$BIN_DIR/node" <<'WRAP'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$STUB_NODE_LOG"
if [[ "${1:-}" == "--version" ]]; then
  echo "v20.10.0"
  exit 0
fi
if [[ "${1:-}" == "-e" ]]; then
  if [[ -d "$PWD/node_modules/algoliasearch" && -d "$PWD/node_modules/dotenv" ]]; then
    exit 0
  fi
  exit 42
fi
case "${1:-}" in
  test.js|contract_tests.js|full_compat_tests.js|instantsearch_contract_tests.js)
    exit 0
    ;;
esac
exit 0
WRAP
chmod +x "$BIN_DIR/node"

PATH="$BIN_DIR:$PATH" \
STUB_NPM_LOG="$NPM_LOG" \
STUB_NODE_LOG="$NODE_LOG" \
"$RUNNER" --sdk --e2e >"$OUTPUT_FILE" 2>&1

if grep -Fq "args=ci" "$NPM_LOG"; then
  echo "Did not expect npm ci on valid sdk_test node_modules cache hit during --sdk --e2e"
  cat "$NPM_LOG"
  cat "$OUTPUT_FILE"
  exit 1
fi

for script_name in test.js contract_tests.js full_compat_tests.js instantsearch_contract_tests.js; do
  run_count=$(grep -cx "$script_name" "$NODE_LOG" || true)
  if [ "$run_count" != "1" ]; then
    echo "Expected $script_name to run exactly once under --sdk --e2e, got $run_count"
    cat "$NODE_LOG"
    cat "$OUTPUT_FILE"
    exit 1
  fi
done

real_client_count=$(grep -Fc "args=run test:real_clients" "$NPM_LOG" || true)
if [ "$real_client_count" != "1" ]; then
  echo "Expected real-client browser conformance to run exactly once under --sdk --e2e, got $real_client_count"
  cat "$NPM_LOG"
  cat "$OUTPUT_FILE"
  exit 1
fi

if grep -Fq "SDK: JS test.js" "$OUTPUT_FILE"; then
  echo "Expected SDK-only JS suite block to be skipped when --e2e is enabled"
  cat "$OUTPUT_FILE"
  exit 1
fi

if grep -Fq "SDK: PHP protocol smoke test" "$OUTPUT_FILE"; then
  echo "Expected SDK protocol smoke tests to be skipped when --e2e is enabled"
  cat "$OUTPUT_FILE"
  exit 1
fi

if ! grep -Fq "E2E: JS test.js" "$OUTPUT_FILE"; then
  echo "Expected E2E JS suite to run under --sdk --e2e"
  cat "$OUTPUT_FILE"
  exit 1
fi

echo "PASS: s/test --sdk --e2e runs core JS suite once and skips SDK-only duplicate path"
