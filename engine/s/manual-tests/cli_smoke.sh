#!/bin/bash
# cli_smoke.sh — End-to-end smoke test against the real flapjack binary.
#
# Tests the actual binary, port binding, HTTP responses — NOT the in-process
# Axum used by Rust integration tests.
#
# Usage:
#   ./cli_smoke.sh                          # builds + starts server + runs tests
#   FJ_ALREADY_RUNNING=true ./cli_smoke.sh  # use already-running server on :7700
#   FJ_ALREADY_RUNNING=true FJ_API_BASE=http://127.0.0.1:7710 ./cli_smoke.sh
#
# Set FLAPJACK_ADMIN_KEY to override the default test key.

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/../../.." && pwd)"

API="${FJ_API_BASE:-http://localhost:7700}"
KEY="${FLAPJACK_ADMIN_KEY:-}"
HDR=(-H "Content-Type: application/json" -H "x-algolia-api-key: $KEY" -H "x-algolia-application-id: flapjack")
IDX="cli_smoke_$$_$(date +%s)"

PASSED=0
FAILED=0
FJ_PID=""
FJ_TMP=""

pass() { echo "  ✅ $1"; PASSED=$((PASSED + 1)); }
fail() { echo "  ❌ $1: $2"; FAILED=$((FAILED + 1)); }

generate_admin_key() {
  local random_hex=""
  random_hex="$(od -An -N16 -tx1 /dev/urandom 2>/dev/null | tr -d ' \n')"
  [ -n "$random_hex" ] || {
    echo "ERROR: failed to generate a random admin key from /dev/urandom" >&2
    exit 1
  }
  printf 'fj_cli_smoke_%s\n' "$random_hex"
}

# TODO: Document cleanup.
cleanup() {
  local script_exit_code=$?
  # Delete test index
  curl -sf -X DELETE "$API/1/indexes/$IDX" "${HDR[@]}" > /dev/null 2>&1 || true
  # Stop server if we started it
  if [ -n "$FJ_PID" ]; then
    kill "$FJ_PID" 2>/dev/null || true
    wait "$FJ_PID" 2>/dev/null || true
  fi
  if [ -n "$FJ_TMP" ]; then
    if [ "$FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      local failure_snapshot="/tmp/flapjack_cli_smoke_failure_${$}_$(date +%s)"
      cp -R "$FJ_TMP" "$failure_snapshot"
      echo "INFO: preserved cli smoke failure data at $failure_snapshot"
    else
      rm -rf "$FJ_TMP"
    fi
  fi
}
trap cleanup EXIT

# ── Start server (unless already running) ─────────────────────────────────────

if [ "${FJ_ALREADY_RUNNING:-}" != "true" ]; then
  if [ -z "$KEY" ]; then
    KEY="$(generate_admin_key)"
    HDR=(-H "Content-Type: application/json" -H "x-algolia-api-key: $KEY" -H "x-algolia-application-id: flapjack")
  fi
  echo "=== Building flapjack binary ==="
  cd "$ENGINE_DIR"
  mkdir -p dashboard/dist && [ -f dashboard/dist/index.html ] || echo '<html></html>' > dashboard/dist/index.html
  cargo build --package flapjack-server 2>&1 | tail -3
  echo ""

  FJ_TMP="/tmp/fj-cli-smoke-$$"
  mkdir -p "$FJ_TMP"

  echo "=== Starting server ==="
  FLAPJACK_ADMIN_KEY="$KEY" "$ENGINE_DIR/target/debug/flapjack" --data-dir "$FJ_TMP" &
  FJ_PID=$!

  for i in $(seq 1 15); do
    if ! kill -0 "$FJ_PID" >/dev/null 2>&1; then
      echo "❌ Server exited during startup (possible port conflict on $API)"
      wait "$FJ_PID" 2>/dev/null || true
      exit 1
    fi
    if curl -sf "$API/health" >/dev/null 2>&1; then break; fi
    sleep 1
  done
  if ! kill -0 "$FJ_PID" >/dev/null 2>&1; then
    echo "❌ Server exited before tests began (possible port conflict on $API)"
    wait "$FJ_PID" 2>/dev/null || true
    exit 1
  fi
  if ! curl -sf "$API/health" >/dev/null 2>&1; then
    echo "❌ Server failed to start"
    exit 1
  fi
  echo "  Server running (PID $FJ_PID)"
  echo ""
fi

# ── Tests ─────────────────────────────────────────────────────────────────────

echo "=== CLI Smoke Tests (index: $IDX) ==="
echo ""

# 1. Health check
echo "--- Health ---"
STATUS=$(curl -sf -o /dev/null -w '%{http_code}' "$API/health")
[ "$STATUS" = "200" ] && pass "GET /health → 200" || fail "GET /health" "expected 200, got $STATUS"

# 1b. Dashboard check — HTML body from RustEmbed or dev stub
echo "--- Dashboard ---"
DASH_BODY=$(curl -sf "$API/dashboard" 2>/dev/null || echo "")
DASH_STATUS=$(curl -s -o /dev/null -w '%{http_code}' "$API/dashboard")
if [ "$DASH_STATUS" = "200" ] && echo "$DASH_BODY" | grep -q '<html'; then
  pass "GET /dashboard → 200 with <html"
else
  fail "GET /dashboard" "expected 200 with <html body, got status=$DASH_STATUS"
fi

# 2. Create index by adding docs
echo "--- Add documents ---"
RESP=$(curl -sf -X POST "$API/1/indexes/$IDX/batch" "${HDR[@]}" \
  -d '{"requests":[
    {"action":"addObject","body":{"objectID":"1","name":"Gaming Laptop","category":"electronics","price":1299}},
    {"action":"addObject","body":{"objectID":"2","name":"Wireless Mouse","category":"electronics","price":49}},
    {"action":"addObject","body":{"objectID":"3","name":"Mechanical Keyboard","category":"peripherals","price":159}},
    {"action":"addObject","body":{"objectID":"4","name":"Office Desk","category":"furniture","price":399}}
  ]}')
echo "$RESP" | grep -q "taskID" && pass "POST batch addObject" || fail "POST batch" "no taskID in response"

sleep 2  # wait for indexing

# 3. Search — text query
echo "--- Search ---"
RESP=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":"laptop"}')
HITS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['nbHits'])" 2>/dev/null || echo "ERR")
[ "$HITS" = "1" ] && pass "Search 'laptop' → 1 hit" || fail "Search 'laptop'" "expected 1 hit, got $HITS"

# 4. Search — empty query (all docs)
RESP=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":""}')
HITS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['nbHits'])" 2>/dev/null || echo "ERR")
[ "$HITS" = "4" ] && pass "Search '' → 4 hits" || fail "Search ''" "expected 4 hits, got $HITS"

# 5. Get object
echo "--- Get Object ---"
RESP=$(curl -sf "$API/1/indexes/$IDX/1" "${HDR[@]}")
NAME=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['name'])" 2>/dev/null || echo "ERR")
[ "$NAME" = "Gaming Laptop" ] && pass "GET object/1 → Gaming Laptop" || fail "GET object/1" "got $NAME"

# 6. Get object — 404
STATUS=$(curl -s -o /dev/null -w '%{http_code}' "$API/1/indexes/$IDX/nonexistent" "${HDR[@]}")
[ "$STATUS" = "404" ] && pass "GET object/nonexistent → 404" || fail "GET object/nonexistent" "expected 404, got $STATUS"

# 7. Settings — set
echo "--- Settings ---"
curl -sf -X PUT "$API/1/indexes/$IDX/settings" "${HDR[@]}" \
  -d '{"attributesForFaceting":["category"],"searchableAttributes":["name"]}' > /dev/null
pass "PUT settings"

sleep 1

# 8. Settings — get
RESP=$(curl -sf "$API/1/indexes/$IDX/settings" "${HDR[@]}")
echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); assert 'category' in d.get('attributesForFaceting',[])" 2>/dev/null \
  && pass "GET settings → has category facet" || fail "GET settings" "attributesForFaceting missing category"

# Reindex documents once after settings update so facet distribution is populated.
curl -sf -X POST "$API/1/indexes/$IDX/batch" "${HDR[@]}" \
  -d '{"requests":[
    {"action":"addObject","body":{"objectID":"1","name":"Gaming Laptop","category":"electronics","price":1299}},
    {"action":"addObject","body":{"objectID":"2","name":"Wireless Mouse","category":"electronics","price":49}},
    {"action":"addObject","body":{"objectID":"3","name":"Mechanical Keyboard","category":"peripherals","price":159}},
    {"action":"addObject","body":{"objectID":"4","name":"Office Desk","category":"furniture","price":399}}
  ]}' > /dev/null
sleep 1

# 9. Filter search
echo "--- Filters ---"
RESP=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":"","filters":"category:electronics"}')
HITS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['nbHits'])" 2>/dev/null || echo "ERR")
[ "$HITS" = "2" ] && pass "Filter category:electronics → 2 hits" || fail "Filter category:electronics" "expected 2, got $HITS"

# 10. Numeric filter
RESP=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":"","filters":"price >= 200"}')
HITS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['nbHits'])" 2>/dev/null || echo "ERR")
[ "$HITS" = "2" ] && pass "Filter price >= 200 → 2 hits" || fail "Filter price >= 200" "expected 2, got $HITS"

# 11. Facets
echo "--- Facets ---"
RESP=$(curl -sf -X POST "$API/1/indexes/*/queries" "${HDR[@]}" \
  -d "{\"requests\":[{\"indexName\":\"$IDX\",\"query\":\"\",\"facets\":[\"category\"]}]}")
ELEC=$(echo "$RESP" | python3 -c "import sys,json; d=json.load(sys.stdin); results=d.get('results') or []; first=results[0] if results else {}; facets=first.get('facets') if isinstance(first, dict) else {}; category=facets.get('category') if isinstance(facets, dict) else {}; print(category.get('electronics','ERR'))" 2>/dev/null || echo "ERR")
[ "$ELEC" = "2" ] && pass "Facets → electronics:2" || fail "Facets" "expected electronics:2, got $ELEC"

# 12. Multi-index search
echo "--- Multi-index search ---"
RESP=$(curl -sf -X POST "$API/1/indexes/*/queries" "${HDR[@]}" \
  -d "{\"requests\":[{\"indexName\":\"$IDX\",\"query\":\"laptop\"},{\"indexName\":\"$IDX\",\"query\":\"desk\"}]}")
NRESULTS=$(echo "$RESP" | python3 -c "import sys,json; print(len(json.load(sys.stdin)['results']))" 2>/dev/null || echo "ERR")
[ "$NRESULTS" = "2" ] && pass "Multi-index → 2 result sets" || fail "Multi-index" "expected 2 result sets, got $NRESULTS"

# 13. Update object
echo "--- Update/delete ---"
curl -sf -X PUT "$API/1/indexes/$IDX/1" "${HDR[@]}" \
  -d '{"name":"Gaming Laptop Pro","price":1499}' > /dev/null
sleep 1
RESP=$(curl -sf "$API/1/indexes/$IDX/1" "${HDR[@]}")
NAME=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['name'])" 2>/dev/null || echo "ERR")
[ "$NAME" = "Gaming Laptop Pro" ] && pass "PUT object/1 → updated" || fail "PUT object/1" "got $NAME"

# 14. Delete object
curl -sf -X DELETE "$API/1/indexes/$IDX/4" "${HDR[@]}" > /dev/null
sleep 1
STATUS=$(curl -s -o /dev/null -w '%{http_code}' "$API/1/indexes/$IDX/4" "${HDR[@]}")
[ "$STATUS" = "404" ] && pass "DELETE object/4 → gone" || fail "DELETE object/4" "expected 404, got $STATUS"

# 15. List indices
echo "--- Index management ---"
RESP=$(curl -sf "$API/1/indexes" "${HDR[@]}")
echo "$RESP" | python3 -c "import sys,json; items=json.load(sys.stdin)['items']; assert any(i['name']=='$IDX' for i in items)" 2>/dev/null \
  && pass "GET /1/indexes → lists test index" || fail "GET /1/indexes" "test index not found"

# 16. Clear index
curl -sf -X POST "$API/1/indexes/$IDX/clear" "${HDR[@]}" > /dev/null
sleep 1
RESP=$(curl -sf -X POST "$API/1/indexes/$IDX/query" "${HDR[@]}" \
  -d '{"query":""}')
HITS=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin)['nbHits'])" 2>/dev/null || echo "ERR")
[ "$HITS" = "0" ] && pass "POST clear → 0 hits" || fail "POST clear" "expected 0 hits, got $HITS"

# 17. Delete index
curl -sf -X DELETE "$API/1/indexes/$IDX" "${HDR[@]}" > /dev/null
sleep 0.5
STATUS=$(curl -s -o /dev/null -w '%{http_code}' "$API/1/indexes/$IDX/1" "${HDR[@]}")
if [ "$STATUS" = "200" ]; then
  fail "DELETE index" "index object still readable after delete"
else
  RESP=$(curl -sf "$API/1/indexes" "${HDR[@]}")
  MISSING=$(echo "$RESP" | python3 -c "import sys,json; idx='$IDX'; items=json.load(sys.stdin).get('items',[]); print('YES' if all(i.get('name')!=idx for i in items if isinstance(i,dict)) else 'NO')" 2>/dev/null || echo "ERR")
  [ "$MISSING" = "YES" ] && pass "DELETE index → gone" || fail "DELETE index" "index still present in /1/indexes list"
fi

# ── Restart persistence (self-managed server only) ────────────────────────────
# Writes a doc, stops the server, restarts on the same data dir, and confirms
# the doc survived. Gated behind FJ_ALREADY_RUNNING because external callers
# (e.g. Docker) manage their own process lifecycle.

if [ "${FJ_ALREADY_RUNNING:-}" != "true" ] && [ -n "$FJ_PID" ]; then
  echo ""
  echo "--- Restart persistence ---"

  # Use a dedicated index for the persistence check
  PERSIST_IDX="cli_persist_$$_$(date +%s)"

  # Write a document before restart
  RESP=$(curl -sf -X POST "$API/1/indexes/$PERSIST_IDX/batch" "${HDR[@]}" \
    -d '{"requests":[{"action":"addObject","body":{"objectID":"persist1","name":"Persist Test"}}]}')
  echo "$RESP" | grep -q "taskID" && pass "Persistence: wrote doc before restart" || fail "Persistence: write doc" "no taskID"
  sleep 2

  # Stop the server
  kill "$FJ_PID" 2>/dev/null || true
  wait "$FJ_PID" 2>/dev/null || true
  FJ_PID=""
  sleep 1

  # Restart on the same data dir
  FLAPJACK_ADMIN_KEY="$KEY" "$ENGINE_DIR/target/debug/flapjack" --data-dir "$FJ_TMP" &
  FJ_PID=$!

  for i in $(seq 1 15); do
    if ! kill -0 "$FJ_PID" >/dev/null 2>&1; then
      fail "Persistence: server restart" "server exited during startup"
      break
    fi
    if curl -sf "$API/health" >/dev/null 2>&1; then break; fi
    sleep 1
  done

  if kill -0 "$FJ_PID" >/dev/null 2>&1 && curl -sf "$API/health" >/dev/null 2>&1; then
    pass "Persistence: server restarted"

    # Verify the document survived
    RESP=$(curl -sf "$API/1/indexes/$PERSIST_IDX/persist1" "${HDR[@]}" 2>/dev/null || echo "")
    PNAME=$(echo "$RESP" | python3 -c "import sys,json; print(json.load(sys.stdin).get('name',''))" 2>/dev/null || echo "ERR")
    [ "$PNAME" = "Persist Test" ] && pass "Persistence: doc survived restart" || fail "Persistence: doc survived" "expected 'Persist Test', got '$PNAME'"

    # Clean up persistence index
    curl -sf -X DELETE "$API/1/indexes/$PERSIST_IDX" "${HDR[@]}" > /dev/null 2>&1 || true
  else
    fail "Persistence: server restart" "server did not come back up"
  fi
fi

# ── Summary ───────────────────────────────────────────────────────────────────

echo ""
echo "=== Results: $PASSED passed, $FAILED failed ==="
echo ""

if [ "$FAILED" -gt 0 ]; then
  exit 1
fi
exit 0
