#!/bin/bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SELECTED_PYTHON="${FLAPJACK_SDK_PYTHON:-python3.12}"
CACHE_ROOT="${FLAPJACK_SDK_PYTHON_CLIENT_CACHE:-$SCRIPT_DIR/.cache}"
ENVIRONMENT_DIR="$CACHE_ROOT/python-client-contract"
REQUIREMENTS_FILE="$SCRIPT_DIR/requirements-python-client.txt"
CONTRACT_PROGRAM="$SCRIPT_DIR/python_client_contract_test.py"

require_cpython_312() {
    local interpreter="$1"
    "$interpreter" -c '
import platform
import sys

raise SystemExit(
    0
    if platform.python_implementation() == "CPython"
    and sys.version_info[:2] == (3, 12)
    else 1
)
'
}

BASE_PYTHON="$(command -v "$SELECTED_PYTHON" || true)"
if [[ -z "$BASE_PYTHON" ]]; then
    echo "Unable to find CPython 3.12 interpreter '$SELECTED_PYTHON'. Set FLAPJACK_SDK_PYTHON to a CPython 3.12 executable." >&2
    exit 1
fi

if ! require_cpython_312 "$BASE_PYTHON"; then
    echo "Interpreter '$BASE_PYTHON' must be CPython 3.12. Set FLAPJACK_SDK_PYTHON to a compatible executable." >&2
    exit 1
fi

ENVIRONMENT_PYTHON="$ENVIRONMENT_DIR/bin/python"
if [[ ! -x "$ENVIRONMENT_PYTHON" ]] || ! require_cpython_312 "$ENVIRONMENT_PYTHON"; then
    rm -rf -- "$ENVIRONMENT_DIR"
    mkdir -p "$CACHE_ROOT"
    "$BASE_PYTHON" -m venv "$ENVIRONMENT_DIR"
fi

"$ENVIRONMENT_PYTHON" -m pip install -r "$REQUIREMENTS_FILE"
"$ENVIRONMENT_PYTHON" "$CONTRACT_PROGRAM"
