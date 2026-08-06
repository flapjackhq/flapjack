#!/bin/bash
# Start the stable flapjack binary for dashboard development.
# This binary is decoupled from ongoing server code changes.
# Uses port 7700 by default (override with FLAPJACK_BIND_ADDR).
#
# Usage: ./scripts/start-stable-server.sh
# Rebuild: npm run update-server
#   package.json owns the build flags — including --features vector-search-local,
#   which is what makes GET /health report vectorSearch/vectorSearchLocal true and
#   keeps the dashboard's vector e2e specs from skipping themselves.

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
source "$SCRIPT_DIR/../../_dev/s/lib/secret-env.sh"
BIN="$SCRIPT_DIR/../bin/flapjack-stable"
DEFAULT_DASHBOARD_ADMIN_KEY="fj_devtestadminkey000000"

if [ ! -f "$BIN" ]; then
  echo "Stable binary not found at $BIN"
  echo "Build it with: npm run update-server"
  exit 1
fi

export FLAPJACK_DATA_DIR="${FLAPJACK_DATA_DIR:-/tmp/flapjack-dashboard-clean}"
export FLAPJACK_BIND_ADDR="${FLAPJACK_BIND_ADDR:-127.0.0.1:7700}"

umask 077
mkdir -p "$FLAPJACK_DATA_DIR"
chmod 700 "$FLAPJACK_DATA_DIR"

if [ -z "${FLAPJACK_ADMIN_KEY:-}" ]; then
  export FLAPJACK_ADMIN_KEY="$DEFAULT_DASHBOARD_ADMIN_KEY"
  GENERATED_ADMIN_KEY=1
else
  GENERATED_ADMIN_KEY=0
fi

ADMIN_KEY_FILE="$FLAPJACK_DATA_DIR/.admin-key"
if [ "$GENERATED_ADMIN_KEY" -eq 1 ]; then
  printf '%s\n' "$FLAPJACK_ADMIN_KEY" > "$ADMIN_KEY_FILE"
  chmod 600 "$ADMIN_KEY_FILE"
fi

load_flapjack_runtime_env_from_secret "$REPO_ROOT"

echo "Starting stable flapjack on $FLAPJACK_BIND_ADDR"
if [ "$GENERATED_ADMIN_KEY" -eq 1 ]; then
  echo "  Admin key file: $ADMIN_KEY_FILE"
  echo "  (Loaded default dashboard dev admin key)"
else
  echo "  Admin key: [hidden; provided via FLAPJACK_ADMIN_KEY]"
fi
echo "  Data dir:  $FLAPJACK_DATA_DIR"
if [ -n "${FLAPJACK_AI_API_KEY:-}" ]; then
  echo "  AI provider: ${FLAPJACK_AI_BASE_URL:-https://api.openai.com/v1} (${FLAPJACK_AI_MODEL:-gpt-4o-mini})"
else
  echo "  AI provider: not configured"
fi
echo ""

exec "$BIN"
