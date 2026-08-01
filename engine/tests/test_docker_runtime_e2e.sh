#!/bin/bash
# test_docker_runtime_e2e.sh — Published-image Docker runtime e2e.
#
# Validates a published ghcr.io/flapjackhq/flapjack image end to end:
# container start, architecture assertion, /health value-level contract,
# .admin_key persistence, batch ingest, task publication polling (README
# workflow), search retrieval, and missing-Application-Id 403 auth contract.
#
# Accepts env overrides: IMAGE, PLATFORM, EXPECTED_VERSION, CONTAINER_NAME,
# HOST_PORT. Defaults to ghcr.io/flapjackhq/flapjack:1.0.0 on linux/amd64.
#
# Usage:
#   bash engine/tests/test_docker_runtime_e2e.sh
#   IMAGE=ghcr.io/flapjackhq/flapjack:1.0.1 PLATFORM=linux/arm64 \
#     EXPECTED_VERSION=1.0.1 bash engine/tests/test_docker_runtime_e2e.sh

set -uo pipefail

IMAGE="${IMAGE:-ghcr.io/flapjackhq/flapjack:1.0.0}"
CONTAINER_NAME="${CONTAINER_NAME:-flapjack_stage4_e2e}"
HOST_PORT="${HOST_PORT:-17700}"
BASE_URL="http://127.0.0.1:${HOST_PORT}"
PLATFORM="${PLATFORM:-linux/amd64}"
EXPECTED_VERSION="${EXPECTED_VERSION:-1.0.0}"
EXPECTED_RUNTIME_UID="${EXPECTED_RUNTIME_UID:-10001}"
EXPECTED_RUNTIME_GID="${EXPECTED_RUNTIME_GID:-10001}"

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0
FIRST_FAILURE=""
TMP_WORK_DIR=""
CONTAINER_STARTED="false"
RUNTIME_UID=""
RUNTIME_GID=""

# Docker volume names are daemon-global. Keep both fixtures in the same
# caller-controlled namespace as the containers so concurrent harness runs
# with distinct CONTAINER_NAME values cannot remove each other's fixtures.
MAIN_DATA_VOLUME="${CONTAINER_NAME}_vol_main"
WRITABLE_VOLUME="${CONTAINER_NAME}_vol_writable"
UNWRITABLE_VOLUME="${CONTAINER_NAME}_vol_unwritable"
WRITABLE_CONTAINER_NAME="${CONTAINER_NAME}_vol_writable"
UNWRITABLE_CONTAINER_NAME="${CONTAINER_NAME}_vol_unwritable"
WRITABLE_MARKER="flapjack-sec-g11-writable-v1"
UNWRITABLE_MARKER="flapjack-sec-g11-unwritable-v1"

timestamp() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }

redact_sensitive_output() {
  # Expected failure logs are committed as RED evidence. Never persist the
  # generated admin credential printed during first boot.
  sed -E 's/fj_admin_[[:alnum:]_-]+/[REDACTED_ADMIN_KEY]/g'
}

pass() {
  TESTS_PASSED=$((TESTS_PASSED + 1))
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "[%s] PASS: %s\n" "$(timestamp)" "$1"
}

fail() {
  TESTS_FAILED=$((TESTS_FAILED + 1))
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "[%s] FAIL: %s\n" "$(timestamp)" "$1"
  if [ -n "${2:-}" ]; then
    printf "  expected: %s\n" "$2"
  fi
  if [ -n "${3:-}" ]; then
    printf "  actual:   %s\n" "$3"
  fi
  if [ -z "$FIRST_FAILURE" ]; then
    FIRST_FAILURE="$1"
  fi
}

cleanup() {
  local script_exit_code=$?
  # This is the single teardown owner for the main container and both
  # namespaced upgrade fixtures, including leftovers from interrupted runs.
  docker rm -f \
    "$CONTAINER_NAME" \
    "$WRITABLE_CONTAINER_NAME" \
    "$UNWRITABLE_CONTAINER_NAME" >/dev/null 2>&1 || true
  CONTAINER_STARTED="false"
  if [ -n "$TMP_WORK_DIR" ] && [ -d "$TMP_WORK_DIR" ]; then
    if [ "$TESTS_FAILED" -gt 0 ] || [ "$script_exit_code" -ne 0 ]; then
      local failure_snapshot="/tmp/flapjack_docker_runtime_e2e_failure_${$}_$(date +%s)"
      mkdir -p "$failure_snapshot"
      cp -R "$TMP_WORK_DIR" "$failure_snapshot/host_work_dir" 2>/dev/null || true
      if docker volume inspect "$MAIN_DATA_VOLUME" >/dev/null 2>&1; then
        mkdir -p "$failure_snapshot/main_data_volume"
        docker run --rm \
          -v "${MAIN_DATA_VOLUME}:/from:ro" \
          -v "${failure_snapshot}:/to" \
          --entrypoint /bin/sh \
          "$IMAGE" -c 'cp -a /from/. /to/main_data_volume/' >/dev/null 2>&1 || true
      fi
      printf "INFO: preserving docker e2e artifacts for triage: %s\n" "$failure_snapshot"
    else
      rm -rf "$TMP_WORK_DIR" 2>/dev/null || true
    fi
  fi
  docker volume rm -f \
    "$MAIN_DATA_VOLUME" \
    "$WRITABLE_VOLUME" \
    "$UNWRITABLE_VOLUME" >/dev/null 2>&1 || true
}
trap cleanup EXIT

printf "=== Flapjack Docker Runtime E2E ===\n"
printf "Started: %s\n" "$(timestamp)"
printf "Image: %s\n" "$IMAGE"
printf "Platform: %s\n" "$PLATFORM"
printf "Expected version: %s\n" "$EXPECTED_VERSION"
printf "Container: %s\n" "$CONTAINER_NAME"
printf "Host port: %s -> 7700\n\n" "$HOST_PORT"

# ── Pre-flight: docker available and any stale container removed ─────────────

if ! command -v docker >/dev/null 2>&1; then
  fail "docker-available" "docker on PATH" "docker not found"
  printf "\nAborting: docker is required.\n"
  exit 1
fi
pass "docker-available"

# Remove every stale container and named volume through the same cleanup owner
# used by the EXIT trap, so repeated and interrupted runs start deterministically.
cleanup

# Probe the image's runtime identity once. The same observed UID:GID owns
# writable fixtures below, avoiding test-only values that could drift from the
# image contract.
RUNTIME_IDENTITY=$(docker run --rm \
  --platform "$PLATFORM" \
  --entrypoint /bin/sh \
  "$IMAGE" \
  -c 'printf "%s:%s" "$(id -u)" "$(id -g)"' \
  2>/tmp/flapjack_runtime_identity.err) && RUNTIME_IDENTITY_EXIT=0 || RUNTIME_IDENTITY_EXIT=$?
RUNTIME_UID="${RUNTIME_IDENTITY%%:*}"
RUNTIME_GID="${RUNTIME_IDENTITY#*:}"

if [ "$RUNTIME_IDENTITY_EXIT" -eq 0 ] && [ -n "$RUNTIME_UID" ] && [ "$RUNTIME_UID" != "0" ]; then
  pass "non-root-uid"
else
  fail "non-root-uid" "non-empty UID other than 0" \
    "uid=${RUNTIME_UID:-<empty>} gid=${RUNTIME_GID:-<empty>} exit=$RUNTIME_IDENTITY_EXIT"
fi

if [ "$RUNTIME_IDENTITY_EXIT" -eq 0 ] && [ "$RUNTIME_UID" = "$EXPECTED_RUNTIME_UID" ]; then
  pass "runtime-uid-pinned (${EXPECTED_RUNTIME_UID})"
else
  fail "runtime-uid-pinned" "$EXPECTED_RUNTIME_UID" \
    "${RUNTIME_UID:-<empty>} (gid=${RUNTIME_GID:-<empty>} exit=$RUNTIME_IDENTITY_EXIT)"
fi

if [ "$RUNTIME_IDENTITY_EXIT" -eq 0 ] && [ "$RUNTIME_GID" = "$EXPECTED_RUNTIME_GID" ]; then
  pass "runtime-gid-pinned (${EXPECTED_RUNTIME_GID})"
else
  fail "runtime-gid-pinned" "$EXPECTED_RUNTIME_GID" \
    "${RUNTIME_GID:-<empty>} (uid=${RUNTIME_UID:-<empty>} exit=$RUNTIME_IDENTITY_EXIT)"
fi

if [ "$RUNTIME_IDENTITY_EXIT" -ne 0 ] || [ -z "$RUNTIME_UID" ] || [ -z "$RUNTIME_GID" ]; then
  printf "Runtime identity probe stderr:\n"
  cat /tmp/flapjack_runtime_identity.err 2>/dev/null || true
  printf "\nAborting: image runtime identity could not be observed.\n"
  exit 1
fi

# ── Step 1: Start published image ────────────────────────────────────────────

printf '\n%s\n' "--- Step 1: Start published image ---"
TMP_WORK_DIR=$(mktemp -d)
printf "Host work dir: %s\n" "$TMP_WORK_DIR"
printf "Main data volume: %s\n" "$MAIN_DATA_VOLUME"
# The main container uses a named volume instead of a host bind mount because
# macOS/Colima ownership translation can reject writes from the pinned runtime
# UID even after host-side chmod. Prepare the volume with the observed runtime
# identity so the start probe measures Flapjack, not the host filesystem.
docker volume create "$MAIN_DATA_VOLUME" >/dev/null 2>&1 && \
  docker run --rm \
    --platform "$PLATFORM" \
    --user 0 \
    --entrypoint /bin/sh \
    -v "${MAIN_DATA_VOLUME}:/data" \
    "$IMAGE" \
    -c 'chown -R "$1:$2" /data && chmod 0775 /data' \
    sh "$RUNTIME_UID" "$RUNTIME_GID" \
    >/tmp/flapjack_main_prepare.out 2>&1 && MAIN_VOLUME_PREP_EXIT=0 || MAIN_VOLUME_PREP_EXIT=$?

if [ "$MAIN_VOLUME_PREP_EXIT" -eq 0 ]; then
  pass "main-data-volume-prepared"
else
  fail "main-data-volume-prepared" \
    "named /data volume prepared for ${RUNTIME_UID}:${RUNTIME_GID}" \
    "exit $MAIN_VOLUME_PREP_EXIT"
  cat /tmp/flapjack_main_prepare.out 2>/dev/null || true
  exit 1
fi

docker run -d --rm \
  --platform "$PLATFORM" \
  --name "$CONTAINER_NAME" \
  -p "${HOST_PORT}:7700" \
  -v "${MAIN_DATA_VOLUME}:/data" \
  "$IMAGE" > /tmp/docker_run.out 2>&1 && RUN_EXIT=0 || RUN_EXIT=$?

printf "docker run exit: %d\n" "$RUN_EXIT"
printf "docker run output: "
cat /tmp/docker_run.out 2>/dev/null || true
printf "\n"

if [ "$RUN_EXIT" -eq 0 ]; then
  CONTAINER_STARTED="true"
  pass "container-started"
else
  fail "container-started" "exit 0" "exit $RUN_EXIT"
  printf "\nAborting: container failed to start.\n"
  exit 1
fi

# ── Step 2: Poll /health until HTTP 200 or timeout ───────────────────────────

printf '\n%s\n' "--- Step 2: Health check ---"
HEALTH_OK="false"
HEALTH_BODY=""
for _i in $(seq 1 60); do
  HEALTH_HTTP_CODE=$(curl -s -o /tmp/flapjack_health.out -w "%{http_code}" "${BASE_URL}/health" 2>/dev/null) && HEALTH_EXIT=0 || HEALTH_EXIT=$?; HEALTH_BODY=$(cat /tmp/flapjack_health.out 2>/dev/null)
  if [ "$HEALTH_EXIT" -eq 0 ] && [ "$HEALTH_HTTP_CODE" = "200" ]; then
    HEALTH_OK="true"
    break
  fi
  sleep 0.5
done

if [ "$HEALTH_OK" = "true" ]; then
  pass "server-starts"
  printf "  /health response: %s\n" "$HEALTH_BODY"
else
  fail "server-starts" "/health HTTP 200 within 30s" "timeout"
  printf "Container logs:\n"
  docker logs "$CONTAINER_NAME" 2>&1 | redact_sensitive_output | tail -40 || true
  printf "\nAborting: server did not become healthy.\n"
  exit 1
fi

# Value-level /health assertions: status, version, vector-search capability.
# These catch wrong-image / wrong-build regressions, not just "did it boot".
if printf '%s' "$HEALTH_BODY" | grep -q '"status":"ok"'; then
  pass "health-status-ok"
else
  fail "health-status-ok" '"status":"ok" in /health body' "$HEALTH_BODY"
fi

if printf '%s' "$HEALTH_BODY" | grep -q "\"version\":\"${EXPECTED_VERSION}\""; then
  pass "health-version-match (${EXPECTED_VERSION})"
else
  fail "health-version-match" "\"version\":\"${EXPECTED_VERSION}\" in /health body" "$HEALTH_BODY"
fi

# README documents vector search as a Docker-runtime capability; assert the
# image actually carries it (the rust:1 trixie runtime build path enables it,
# unlike the musl release build path).
if printf '%s' "$HEALTH_BODY" | grep -q '"vectorSearch":true'; then
  pass "health-capability-vector-search"
else
  fail "health-capability-vector-search" '"vectorSearch":true in /health.capabilities' "$HEALTH_BODY"
fi

# ── Step 2b: Architecture assertion ──────────────────────────────────────────

printf '\n%s\n' "--- Step 2b: Architecture assertion ---"
ACTUAL_PLATFORM=$(docker image inspect --format '{{.Os}}/{{.Architecture}}' "$IMAGE" 2>/dev/null) && INSPECT_EXIT=0 || INSPECT_EXIT=$?

if [ "$INSPECT_EXIT" -eq 0 ] && [ "$ACTUAL_PLATFORM" = "$PLATFORM" ]; then
  pass "image-architecture-match ($ACTUAL_PLATFORM)"
else
  fail "image-architecture-match" "$PLATFORM" "${ACTUAL_PLATFORM:-inspect failed (exit $INSPECT_EXIT)}"
fi

# ── Step 2c: Pre-existing /data upgrade fixtures ─────────────────────────────

printf '\n%s\n' "--- Step 2c: Pre-existing /data upgrade fixtures ---"
# Operators upgrading from older Flapjack images already have root-created
# /data volumes. A correctly prepared writable volume must keep serving without
# losing data, while an unwritable root-owned volume must refuse loudly rather
# than start and later fail persistence. Refusing every volume is also a
# data-availability incident, so both cases are load-bearing.

docker volume create "$WRITABLE_VOLUME" >/dev/null 2>&1 && \
  docker run --rm \
    --platform "$PLATFORM" \
    --user 0 \
      --entrypoint /bin/sh \
      -v "${WRITABLE_VOLUME}:/data" \
      "$IMAGE" \
      -c 'printf "%s" "$1" > /data/secg11_marker && chown -R "$2:$3" /data' \
      sh "$WRITABLE_MARKER" "$RUNTIME_UID" "$RUNTIME_GID" \
      >/tmp/flapjack_writable_prepare.out 2>&1 && WRITABLE_PREP_EXIT=0 || WRITABLE_PREP_EXIT=$?

if [ "$WRITABLE_PREP_EXIT" -eq 0 ]; then
  docker run -d \
    --platform "$PLATFORM" \
    --name "$WRITABLE_CONTAINER_NAME" \
    -v "${WRITABLE_VOLUME}:/data" \
    "$IMAGE" >/tmp/flapjack_writable_start.out 2>&1 && WRITABLE_START_EXIT=0 || WRITABLE_START_EXIT=$?
else
  WRITABLE_START_EXIT=1
fi

WRITABLE_HEALTH_OK="false"
WRITABLE_MARKER_ACTUAL=""
if [ "$WRITABLE_START_EXIT" -eq 0 ]; then
  for _i in $(seq 1 60); do
    if docker exec "$WRITABLE_CONTAINER_NAME" \
      curl -fsS http://127.0.0.1:7700/health >/dev/null 2>&1; then
      WRITABLE_HEALTH_OK="true"
      break
    fi
    sleep 0.5
  done
  WRITABLE_MARKER_ACTUAL=$(docker exec "$WRITABLE_CONTAINER_NAME" \
    cat /data/secg11_marker 2>/dev/null) || true
fi

if [ "$WRITABLE_PREP_EXIT" -eq 0 ] && \
  [ "$WRITABLE_START_EXIT" -eq 0 ] && \
  [ "$WRITABLE_HEALTH_OK" = "true" ] && \
  [ "$WRITABLE_MARKER_ACTUAL" = "$WRITABLE_MARKER" ]; then
  pass "data-volume-writable-starts"
else
  fail "data-volume-writable-starts" \
    "prepared volume reaches /health and preserves marker '$WRITABLE_MARKER'" \
    "prepare=$WRITABLE_PREP_EXIT start=$WRITABLE_START_EXIT health=$WRITABLE_HEALTH_OK marker=${WRITABLE_MARKER_ACTUAL:-<missing>}"
  printf "Writable fixture prepare/start output:\n"
  cat /tmp/flapjack_writable_prepare.out /tmp/flapjack_writable_start.out 2>/dev/null || true
  printf "Writable fixture container logs:\n"
  docker logs "$WRITABLE_CONTAINER_NAME" 2>&1 | redact_sensitive_output || true
fi
docker rm -f "$WRITABLE_CONTAINER_NAME" >/dev/null 2>&1 || true

docker volume create "$UNWRITABLE_VOLUME" >/dev/null 2>&1 && \
  docker run --rm \
    --platform "$PLATFORM" \
    --user 0 \
    --entrypoint /bin/sh \
    -v "${UNWRITABLE_VOLUME}:/data" \
    "$IMAGE" \
    -c 'printf "%s" "$1" > /data/secg11_marker && chown -R 0:0 /data && chmod 0755 /data && chmod 0644 /data/secg11_marker' \
    sh "$UNWRITABLE_MARKER" \
    >/tmp/flapjack_unwritable_prepare.out 2>&1 && UNWRITABLE_PREP_EXIT=0 || UNWRITABLE_PREP_EXIT=$?

if [ "$UNWRITABLE_PREP_EXIT" -eq 0 ]; then
  docker run -d \
    --platform "$PLATFORM" \
    --name "$UNWRITABLE_CONTAINER_NAME" \
    -v "${UNWRITABLE_VOLUME}:/data" \
    "$IMAGE" >/tmp/flapjack_unwritable_start.out 2>&1 && UNWRITABLE_START_EXIT=0 || UNWRITABLE_START_EXIT=$?
else
  UNWRITABLE_START_EXIT=1
fi

UNWRITABLE_STOPPED="false"
UNWRITABLE_EXIT_CODE=""
if [ "$UNWRITABLE_START_EXIT" -eq 0 ]; then
  for _i in $(seq 1 60); do
    UNWRITABLE_RUNNING=$(docker inspect --format '{{.State.Running}}' \
      "$UNWRITABLE_CONTAINER_NAME" 2>/dev/null) || UNWRITABLE_RUNNING=""
    if [ "$UNWRITABLE_RUNNING" = "false" ]; then
      UNWRITABLE_STOPPED="true"
      UNWRITABLE_EXIT_CODE=$(docker inspect --format '{{.State.ExitCode}}' \
        "$UNWRITABLE_CONTAINER_NAME" 2>/dev/null) || UNWRITABLE_EXIT_CODE=""
      break
    fi
    sleep 0.5
  done
fi
UNWRITABLE_LOGS=$(docker logs "$UNWRITABLE_CONTAINER_NAME" 2>&1 | redact_sensitive_output) || true

if [ "$UNWRITABLE_PREP_EXIT" -eq 0 ] && \
  [ "$UNWRITABLE_START_EXIT" -eq 0 ] && \
  [ "$UNWRITABLE_STOPPED" = "true" ] && \
  [ -n "$UNWRITABLE_EXIT_CODE" ] && \
  [ "$UNWRITABLE_EXIT_CODE" -ne 0 ] 2>/dev/null && \
  printf '%s' "$UNWRITABLE_LOGS" | grep -qE 'docker run .*chown .* /data'; then
  pass "data-volume-unwritable-refuses"
else
  fail "data-volume-unwritable-refuses" \
    "non-zero exit within 30s and operator-actionable docker run ... chown ... /data log" \
    "prepare=$UNWRITABLE_PREP_EXIT start=$UNWRITABLE_START_EXIT stopped=$UNWRITABLE_STOPPED exit=${UNWRITABLE_EXIT_CODE:-<running-or-missing>}"
  printf "Unwritable fixture prepare/start output:\n"
  cat /tmp/flapjack_unwritable_prepare.out /tmp/flapjack_unwritable_start.out 2>/dev/null || true
  printf "Unwritable fixture container logs:\n%s\n" "$UNWRITABLE_LOGS"
fi
docker rm -f "$UNWRITABLE_CONTAINER_NAME" >/dev/null 2>&1 || true

# ── Step 3: Admin key discovery via docker exec on /data/.admin_key ──────────

printf '\n%s\n' "--- Step 3: Admin key discovery ---"
ADMIN_KEY=$(docker exec "$CONTAINER_NAME" cat /data/.admin_key 2>/dev/null) && EXEC_EXIT=0 || EXEC_EXIT=$?

if [ "$EXEC_EXIT" -ne 0 ]; then
  fail "admin-key-file-exists" "/data/.admin_key readable via docker exec" "docker exec exit $EXEC_EXIT"
  printf "\nAborting: cannot read admin key from container.\n"
  exit 1
fi
pass "admin-key-file-exists"

if [ -n "$ADMIN_KEY" ]; then
  pass "admin-key-non-empty"
  printf "  admin key length: %d chars\n" "${#ADMIN_KEY}"
else
  fail "admin-key-non-empty" "non-empty admin key value" "empty"
  printf "\nAborting: empty admin key.\n"
  exit 1
fi

# ── Step 4: Batch ingest using Stage 2/3 canonical fixture ───────────────────

printf '\n%s\n' "--- Step 4: Batch ingest ---"
# Verbatim BATCH_BODY from engine/tests/test_linux_e2e.sh:134
# (matches engine/tests/test_macos_e2e.sh:200).
BATCH_BODY='{"requests":[{"action":"addObject","body":{"objectID":"1","title":"The Matrix","year":1999}},{"action":"addObject","body":{"objectID":"2","title":"Inception","year":2010}}]}'

BATCH_RESP_FILE="${TMP_WORK_DIR}/batch_resp.json"
BATCH_HTTP_CODE=$(curl -s -o "$BATCH_RESP_FILE" -w "%{http_code}" \
  -X POST "${BASE_URL}/1/indexes/test_movies/batch" \
  -H "Content-Type: application/json" \
  -H "X-Algolia-Application-Id: e2e-test" \
  -H "X-Algolia-API-Key: ${ADMIN_KEY}" \
  -d "$BATCH_BODY") && BATCH_EXIT=0 || BATCH_EXIT=$?

BATCH_RESP=$(cat "$BATCH_RESP_FILE" 2>/dev/null)
printf "Batch HTTP code: %s\n" "$BATCH_HTTP_CODE"
printf "Batch response: %s\n" "$BATCH_RESP"

if [ "$BATCH_EXIT" -eq 0 ] && { [ "$BATCH_HTTP_CODE" = "200" ] || [ "$BATCH_HTTP_CODE" = "201" ]; }; then
  pass "batch-accepted"
else
  fail "batch-accepted" "curl exit 0 and HTTP 200 or 201" "exit=$BATCH_EXIT http=$BATCH_HTTP_CODE"
fi

# Assert a numeric taskID is present (README task workflow contract).
TASK_ID=$(printf '%s' "$BATCH_RESP" | sed -n 's/.*"taskID":\([0-9]*\).*/\1/p' | head -1)
if [ -n "$TASK_ID" ]; then
  pass "batch-taskID-present (taskID=$TASK_ID)"
else
  fail "batch-taskID-present" '"taskID":<integer> in batch response' "$BATCH_RESP"
fi

# ── Step 5: Poll /1/tasks/$TASK_ID until "published" (README workflow) ───────

printf '\n%s\n' "--- Step 5: Task publication ---"
TASK_PUBLISHED="false"
TASK_RESP=""
if [ -n "$TASK_ID" ]; then
  for _i in $(seq 1 40); do
    TASK_RESP=$(curl -s "${BASE_URL}/1/tasks/${TASK_ID}" \
      -H "X-Algolia-Application-Id: e2e-test" \
      -H "X-Algolia-API-Key: ${ADMIN_KEY}" 2>&1) || true
    if printf '%s' "$TASK_RESP" | grep -q '"status":"published"'; then
      TASK_PUBLISHED="true"
      break
    fi
    sleep 0.5
  done
fi

printf "Final task response: %s\n" "$TASK_RESP"

if [ "$TASK_PUBLISHED" = "true" ]; then
  pass "task-published"
else
  fail "task-published" '"status":"published" within 20s' "$TASK_RESP"
fi

# ── Step 6: Query and assert nbHits>=1 and "The Matrix" present ──────────────

printf '\n%s\n' "--- Step 6: Search validation ---"
SEARCH_RESP=$(curl -s -X POST "${BASE_URL}/1/indexes/test_movies/query" \
  -H "Content-Type: application/json" \
  -H "X-Algolia-Application-Id: e2e-test" \
  -H "X-Algolia-API-Key: ${ADMIN_KEY}" \
  -d '{"query":"matrix"}' 2>&1)
printf "Search response: %s\n" "$SEARCH_RESP"

NBHITS=$(printf '%s' "$SEARCH_RESP" | grep -o '"nbHits":[0-9]*' | head -1 | cut -d: -f2)
if [ -n "$NBHITS" ] && [ "$NBHITS" -ge 1 ] 2>/dev/null; then
  pass "search-nbHits (nbHits=$NBHITS)"
else
  fail "search-nbHits" "nbHits >= 1" "nbHits=${NBHITS:-<missing>}"
fi

if printf '%s' "$SEARCH_RESP" | grep -q '"The Matrix"'; then
  pass "search-exact-match"
else
  fail "search-exact-match" 'response contains "The Matrix"' "not found in response"
fi

# ── Step 7: Auth contract — missing X-Algolia-Application-Id => 403 ──────────

printf '\n%s\n' "--- Step 7: Auth contract ---"
AUTH_RESP_FILE="${TMP_WORK_DIR}/auth_resp.json"
AUTH_HTTP_CODE=$(curl -s -o "$AUTH_RESP_FILE" -w "%{http_code}" \
  -X POST "${BASE_URL}/1/indexes/test_movies/query" \
  -H "Content-Type: application/json" \
  -H "X-Algolia-API-Key: ${ADMIN_KEY}" \
  -d '{"query":"matrix"}')

AUTH_RESP=$(cat "$AUTH_RESP_FILE" 2>/dev/null)
printf "Auth test (no Application-Id) HTTP code: %s\n" "$AUTH_HTTP_CODE"
printf "Auth test response: %s\n" "$AUTH_RESP"

if [ "$AUTH_HTTP_CODE" = "403" ]; then
  pass "auth-403-without-appid"
else
  fail "auth-403-without-appid" "HTTP 403" "HTTP $AUTH_HTTP_CODE"
fi

# Positive control: with both headers, the same query must succeed (proves the
# 403 above is specifically about the missing Application-Id header, not a
# coincidental failure).
AUTH_OK_HTTP_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
  -X POST "${BASE_URL}/1/indexes/test_movies/query" \
  -H "Content-Type: application/json" \
  -H "X-Algolia-Application-Id: e2e-test" \
  -H "X-Algolia-API-Key: ${ADMIN_KEY}" \
  -d '{"query":"matrix"}')

if [ "$AUTH_OK_HTTP_CODE" = "200" ]; then
  pass "auth-200-with-both-headers"
else
  fail "auth-200-with-both-headers" "HTTP 200" "HTTP $AUTH_OK_HTTP_CODE"
fi

# ── Summary ──────────────────────────────────────────────────────────────────

printf "\n=== Summary ===\n"
printf "Finished: %s\n" "$(timestamp)"
printf "Total: %d  Passed: %d  Failed: %d\n" "$TESTS_RUN" "$TESTS_PASSED" "$TESTS_FAILED"

if [ "$TESTS_FAILED" -gt 0 ]; then
  printf "VERDICT: FAIL (first failure: %s)\n" "$FIRST_FAILURE"
  exit 1
else
  printf "VERDICT: PASS\n"
  exit 0
fi
