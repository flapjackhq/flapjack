#!/usr/bin/env bash
# test_release_feature_gating.sh — Ensure release no-default-features gating is transitive.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
SERVER_CARGO="$REPO_ROOT/engine/flapjack-server/Cargo.toml"
HTTP_CARGO="$REPO_ROOT/engine/flapjack-http/Cargo.toml"
REPLICATION_CARGO="$REPO_ROOT/engine/flapjack-replication/Cargo.toml"

TESTS_RUN=0
TESTS_FAILED=0

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "  [PASS] %s\n" "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf "  [FAIL] %s\n" "$1"
  if [ -n "${2:-}" ]; then
    printf "    %s\n" "$2"
  fi
}

printf "\nRelease Feature Gating\n"

if rg -q 'flapjack = \{ path = "\.\.", default-features = false, features = \[' "$SERVER_CARGO" \
  && ! rg -q 'flapjack = \{[^\n]*vector-search-local' "$SERVER_CARGO"; then
  pass "flapjack-server disables flapjack dependency defaults"
else
  fail "flapjack-server disables flapjack dependency defaults" "expected default-features = false on flapjack dependency"
fi

if rg -q 'flapjack-http = \{ path = "\.\./flapjack-http", default-features = false \}' "$SERVER_CARGO"; then
  pass "flapjack-server disables flapjack-http dependency defaults"
else
  fail "flapjack-server disables flapjack-http dependency defaults" "expected default-features = false on flapjack-http dependency"
fi

if rg -q 'flapjack = \{ path = "\.\.", default-features = false, features = \[' "$HTTP_CARGO" \
  && ! rg -q 'flapjack = \{[^\n]*vector-search-local' "$HTTP_CARGO"; then
  pass "flapjack-http disables flapjack dependency defaults"
else
  fail "flapjack-http disables flapjack dependency defaults" "expected default-features = false on flapjack dependency"
fi

if rg -q 'flapjack = \{ path = "\.\.", default-features = false, features = \["analytics"\] \}' "$REPLICATION_CARGO"; then
  pass "flapjack-replication disables flapjack dependency defaults"
else
  fail "flapjack-replication disables flapjack dependency defaults" "expected default-features = false on flapjack dependency"
fi

TREE_OUTPUT="$(cd "$REPO_ROOT/engine" && cargo tree -p flapjack-server --target all --no-default-features)"
if printf "%s\n" "$TREE_OUTPUT" | grep -Eq "ort-sys|fastembed|openssl-sys"; then
  fail "no-default-features graph excludes ORT/openssl path" "dependency graph still contains ort-sys/fastembed/openssl-sys"
else
  pass "no-default-features graph excludes ORT/openssl path"
fi

VECTOR_TREE_OUTPUT="$(cd "$REPO_ROOT/engine" && cargo tree -p flapjack-server --target all --no-default-features --features vector-search)"
if printf "%s\n" "$VECTOR_TREE_OUTPUT" | grep -Eq '(^| )numkong v'; then
  fail "release vector-search graph excludes NumKong" "dependency graph still contains NumKong"
else
  pass "release vector-search graph excludes NumKong"
fi

printf "\nTotal: %d  Failed: %d\n" "$TESTS_RUN" "$TESTS_FAILED"
if [ "$TESTS_FAILED" -gt 0 ]; then
  exit 1
fi
