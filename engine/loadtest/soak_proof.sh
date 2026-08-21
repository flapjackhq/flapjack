#!/usr/bin/env bash
# shellcheck disable=SC1091,SC2016,SC2034
set -euo pipefail

# PL-9 contract: when FLAPJACK_LOADTEST_ROLLUP_WINDOW_MS is set, this soak
# passes it through to the engine as FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS and
# uses the same window width for the soak-marker boundary assertion. The soak
# defaults to 60000ms windows so analytics proof runs finish in minutes; the
# engine still falls back to hour-aligned production behavior when the env var
# is unset outside the soak harness.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
LOADTEST_HELPERS="$SCRIPT_DIR/lib/loadtest_shell_helpers.sh"
SOAK_HELPERS="$SCRIPT_DIR/lib/loadtest_soak_helpers.sh"
SEED_SCRIPT="$SCRIPT_DIR/seed-loadtest-data.sh"
SERVER_BINARY="${FLAPJACK_LOADTEST_SERVER_BINARY:-$ENGINE_DIR/target/release/flapjack}"
RESULTS_BASE_DIR="$SCRIPT_DIR/results"

RESULTS_DIR=""
RUNNER_TMP_DIR=""
SERVER_DATA_DIR=""
SERVER_LOG_PATH=""
SERVER_PID=""
SAMPLER_PID=""
LIVENESS_SAMPLER_PID=""
SAMPLE_PATH=""
LIVENESS_SAMPLE_PATH=""
SUMMARY_PATH=""
K6_STDOUT_PATH=""
K6_JSON_PATH=""
K6_API_ADDR=""
SCENARIO_NAME=""
SCENARIO_EXIT_CODE=0
INTERRUPTED_EXIT_CODE=0
FLAPJACK_BIND_ADDR=""
SOAK_START_EPOCH_MS=0
SOAK_MARKER_HOUR_END_BOUNDARY_MS=0
SOAK_MARKER_USER_TOKEN=""
ANALYTICS_INGEST_TARGET_RATE="${FLAPJACK_LOADTEST_ANALYTICS_TARGET_RATE:-4_events_per_second}"
ANALYTICS_ACCEPTED_EVENTS="0"
ANALYTICS_DROPPED_EVENTS="0"
ANALYTICS_ROLLUP_GENERATION_P99_MS="0"
ANALYTICS_ROLLUP_LAG_VERDICT="NOT_RUN"
ANALYTICS_ZERO_DROPPED_VERDICT="NOT_RUN"
RETENTION_GATE_VERDICT="NOT_RUN"
RETENTION_GATE_PROBE_PARTITION_PATH="N/A"
MANIFEST_EVIDENCE_PATHS="N/A"
PARQUET_EVIDENCE_PATHS="N/A"
ANALYTICS_PROBE_FAILURE_REASON=""
WRITE_ATTEMPTED_REQUESTS="0"
WRITE_ACCEPTED_200_COUNT="0"
WRITE_QUEUE_FULL_429_COUNT="0"
WRITE_UNEXPECTED_4XX_COUNT="0"
WRITE_5XX_COUNT="0"
WRITE_DIRTY_ERROR_COUNT="0"
WRITE_DROPPED_ITERATIONS="0"
WRITE_ACCEPTED_P95_MS="N/A"
WRITE_EXPECTED_ATTEMPTS="N/A"
WRITE_STATUS_SUM_VERDICT="NOT_RUN"
WRITE_ATTEMPT_COUNT_VERDICT="NOT_RUN"
WRITE_DROPPED_ITERATIONS_VERDICT="NOT_RUN"
WRITE_DIRTY_ERROR_VERDICT="NOT_RUN"
WRITE_BACKPRESSURE_VERDICT="NOT_RUN"
WRITE_CONTROL_ACCEPTANCE_VERDICT="NOT_RUN"
WRITE_CANDIDATE_ACCEPTANCE_VERDICT="NOT_RUN"
WRITE_CANDIDATE_LATENCY_VERDICT="NOT_RUN"
WRITE_ADMISSION_SAMPLE_VERDICT="NOT_RUN"
WRITE_ADMISSION_AGE_VERDICT="NOT_RUN"
WRITE_ADMISSION_DRAIN_VERDICT="NOT_RUN"
WRITE_OVERALL_VERDICT="NOT_RUN"
WRITE_PEAK_ADMISSION_RECORD_COUNT="0"
WRITE_MAX_OLDEST_ADMISSION_AGE_MS="0"
WRITE_ADMISSION_DRAIN_DURATION_SECONDS="N/A"
WRITE_ADMISSION_DRAIN_RECORD_COUNT="N/A"
WRITE_MEMORY_SAMPLE_VERDICT="NOT_RUN"
WRITE_RSS_START_KB="N/A"
WRITE_RSS_PEAK_KB="N/A"
WRITE_RSS_END_KB="N/A"
WRITE_HEAP_START_BYTES="N/A"
WRITE_HEAP_PEAK_BYTES="N/A"
WRITE_HEAP_END_BYTES="N/A"
WRITE_LIVENESS_VERDICT="NOT_RUN"
WRITE_HEALTH_LIVENESS="NOT_RUN"
WRITE_COUNT_LIVENESS="NOT_RUN"

usage() {
  cat <<'EOF'
Usage:
  bash engine/loadtest/soak_proof.sh --scenario <mixed-soak|write-soak|analytics-soak>

Environment overrides come from engine/loadtest/lib/config.js via
sharedLoadtestConfig. The most relevant override for manual proof runs is:

  FLAPJACK_LOADTEST_SOAK_DURATION=2h
EOF
}

fail() {
  echo "FAIL: $1" >&2
  exit 1
}

resolve_loadtest_rollup_window_ms() {
  local window_ms="${FLAPJACK_LOADTEST_ROLLUP_WINDOW_MS:-60000}"
  if [[ ! "$window_ms" =~ ^[1-9][0-9]*$ ]]; then
    fail "FLAPJACK_LOADTEST_ROLLUP_WINDOW_MS must be a positive integer (got: ${window_ms})"
  fi
  printf '%s' "$window_ms"
}

create_results_dir() {
  RESULTS_DIR="$(create_loadtest_results_dir "$RESULTS_BASE_DIR" "$SCENARIO_NAME")"
  mkdir -p "$RESULTS_DIR"
  SAMPLE_PATH="$RESULTS_DIR/memory_samples.csv"
  LIVENESS_SAMPLE_PATH="$RESULTS_DIR/liveness_samples.tsv"
  SUMMARY_PATH="$RESULTS_DIR/summary.md"
  K6_STDOUT_PATH="$RESULTS_DIR/${SCENARIO_NAME}.stdout.txt"
  K6_JSON_PATH="$RESULTS_DIR/${SCENARIO_NAME}.json.gz"
}

derive_k6_api_addr() {
  local host="${FLAPJACK_BIND_ADDR%:*}"
  local port="${FLAPJACK_BIND_ADDR##*:}"
  local k6_port=$((port + 10000))

  if (( k6_port > 65535 )); then
    fail "derived k6 API port ${k6_port} from bind addr ${FLAPJACK_BIND_ADDR} exceeds 65535"
  fi

  K6_API_ADDR="${host}:${k6_port}"
}

initialize_soak_marker_user_token() {
  SOAK_MARKER_USER_TOKEN="$(
    node -e 'process.stdout.write(require("node:crypto").randomUUID())'
  )"
  [[ -n "$SOAK_MARKER_USER_TOKEN" ]] || fail "failed to initialize soak marker user token"
  export FLAPJACK_LOADTEST_SOAK_MARKER_USER_TOKEN="$SOAK_MARKER_USER_TOKEN"
}

build_or_reuse_binary() {
  if [[ -x "$SERVER_BINARY" ]]; then
    echo "INFO: reusing existing release binary at $SERVER_BINARY"
    return 0
  fi

  echo "INFO: building release flapjack server binary"
  (
    cd "$ENGINE_DIR"
    cargo build --release -p flapjack-server
  )

  [[ -x "$SERVER_BINARY" ]] || fail "build completed without producing $SERVER_BINARY"
}

# TODO: Document start_server.
start_server() {
  mkdir -p "$SERVER_DATA_DIR"
  SERVER_LOG_PATH="$RESULTS_DIR/server.log"

  # PL-9: rename the loadtest-scoped knob to the engine's test-only override
  # so the soak and rollup writer derive boundaries from the same width.
  FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS="$(resolve_loadtest_rollup_window_ms)"
  export FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS
  export FLAPJACK_ROLLUP_INTERVAL_SECS="${FLAPJACK_LOADTEST_ROLLUP_INTERVAL_SECS:-10}"
  "$SERVER_BINARY" --no-auth --bind-addr "$FLAPJACK_BIND_ADDR" --data-dir "$SERVER_DATA_DIR" \
    >"$SERVER_LOG_PATH" 2>&1 &
  SERVER_PID=$!

  sleep 0.1
  if ! kill -0 "$SERVER_PID" 2>/dev/null; then
    wait "$SERVER_PID" 2>/dev/null || true
    fail "server exited during startup; see $SERVER_LOG_PATH"
  fi
}

# TODO: Document stop_server.
stop_server() {
  if [[ -z "$SERVER_PID" ]]; then
    return 0
  fi

  if kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true

    for _ in $(seq 1 50); do
      if ! kill -0 "$SERVER_PID" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done

    if kill -0 "$SERVER_PID" 2>/dev/null; then
      kill -9 "$SERVER_PID" 2>/dev/null || true
    fi
  fi

  wait "$SERVER_PID" 2>/dev/null || true
  SERVER_PID=""
}

# TODO: Document wait_for_health.
wait_for_health() {
  local health_url="$FLAPJACK_BASE_URL/health"
  local max_attempts=300
  local attempt
  local health_status_code

  for ((attempt = 1; attempt <= max_attempts; attempt += 1)); do
    if [[ -n "$SERVER_PID" ]] && ! kill -0 "$SERVER_PID" 2>/dev/null; then
      wait "$SERVER_PID" 2>/dev/null || true
      fail "server exited while waiting for /health"
    fi

    health_status_code="$(
      curl -sS -o /dev/null -w '%{http_code}' --max-time 1 "$health_url" 2>/dev/null || true
    )"
    if [[ "$health_status_code" == "200" ]]; then
      return 0
    fi

    sleep 0.1
  done

  fail "timed out waiting for $health_url"
}

# TODO: Document metric_value.
metric_value() {
  local metric_name="$1"
  local label_fragment="${2:-}"
  local metrics

  metrics="$(loadtest_http_request GET "/metrics" "" "200")"
  if [[ -n "$label_fragment" ]]; then
    awk -v metric_name="$metric_name" -v label_fragment="$label_fragment" '
      index($0, metric_name "{") == 1 && index($0, label_fragment) > 0 {
        print $NF
        exit
      }
    ' <<<"$metrics"
  else
    awk -v metric_name="$metric_name" '
      $1 == metric_name {
        print $2
        exit
      }
    ' <<<"$metrics"
  fi
}

metric_must_be_number() {
  local metric_name="$1"
  local metric_value="$2"

  if [[ "$metric_value" =~ ^-?[0-9]+([.][0-9]+)?$ ]]; then
    return 0
  fi

  mark_analytics_probe_failure "metric ${metric_name} is not numeric: ${metric_value:-<empty>}"
  return 1
}

mark_analytics_probe_failure() {
  local reason="$1"
  ANALYTICS_PROBE_FAILURE_REASON="$reason"
  ANALYTICS_ROLLUP_LAG_VERDICT="FAIL (${reason})"
  ANALYTICS_ZERO_DROPPED_VERDICT="FAIL (${reason})"
}

scenario_requires_analytics_proof() {
  [[ "$SCENARIO_NAME" == "mixed-soak" || "$SCENARIO_NAME" == "analytics-soak" ]]
}

# Pre-soak contract guard: confirm every metric capture_analytics_proof
# will later read is already present at value-line form on the freshly
# started server. Without this, a stale binary that predates
# register_analytics_gauges (or any of its rollup-metric extensions)
# would only surface at the post-soak probe — wasting the full soak
# duration before failing. Anchored: the 2026-05-25 RF-2 L2 soak runs
# (20260525T184045Z / T190336Z / T193540Z) each failed with
# "missing flapjack_analytics_events_accepted_total metric" after a 2h
# k6 cycle because the on-disk binary was v1.0.0, built before
# f963f21d introduced register_analytics_gauges. The metric names below
# must match the inline metric_value calls in capture_analytics_proof
# and the register_gauge calls in
# engine/flapjack-http/src/handlers/metrics.rs::register_analytics_gauges.
assert_analytics_metric_contract() {
  local metric
  local value
  local missing=()
  for metric in \
      flapjack_analytics_events_accepted_total \
      flapjack_analytics_events_dropped_total \
      flapjack_analytics_rollup_windows_generated_total \
      flapjack_analytics_rollup_events_generated_total \
      flapjack_analytics_rollup_latest_nonempty_window_end_ms \
      flapjack_analytics_soak_marker_first_event_timestamp_ms \
      flapjack_analytics_rollup_generation_latency_ms_p99; do
    value="$(metric_value "$metric")"
    if [[ -z "$value" ]]; then
      missing+=("$metric")
    fi
  done

  if (( ${#missing[@]} > 0 )); then
    fail "pre-soak contract: /metrics is missing $(printf '%s ' "${missing[@]}")— likely a stale $SERVER_BINARY (built before these gauges were registered). Rebuild with: rm -f $SERVER_BINARY && (cd $ENGINE_DIR && cargo build --release -p flapjack-server)"
  fi
}

# TODO: Document capture_analytics_proof.
capture_analytics_proof() {
  local accepted_events
  local dropped_events
  local rollup_windows_generated
  local rollup_events_generated
  local rollup_latest_nonempty_window_end_ms
  local soak_marker_first_event_timestamp_ms
  local rollup_p99_ms

  accepted_events="$(metric_value "flapjack_analytics_events_accepted_total")"
  dropped_events="$(metric_value "flapjack_analytics_events_dropped_total")"
  rollup_windows_generated="$(metric_value "flapjack_analytics_rollup_windows_generated_total")"
  rollup_events_generated="$(metric_value "flapjack_analytics_rollup_events_generated_total")"
  rollup_latest_nonempty_window_end_ms="$(metric_value "flapjack_analytics_rollup_latest_nonempty_window_end_ms")"
  soak_marker_first_event_timestamp_ms="$(metric_value "flapjack_analytics_soak_marker_first_event_timestamp_ms")"
  rollup_p99_ms="$(metric_value "flapjack_analytics_rollup_generation_latency_ms_p99")"

  [[ -n "$accepted_events" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_events_accepted_total metric"; return 1; }
  [[ -n "$dropped_events" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_events_dropped_total metric"; return 1; }
  [[ -n "$rollup_windows_generated" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_rollup_windows_generated_total metric"; return 1; }
  [[ -n "$rollup_events_generated" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_rollup_events_generated_total metric"; return 1; }
  [[ -n "$rollup_latest_nonempty_window_end_ms" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_rollup_latest_nonempty_window_end_ms metric"; return 1; }
  [[ -n "$soak_marker_first_event_timestamp_ms" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_soak_marker_first_event_timestamp_ms metric"; return 1; }
  [[ -n "$rollup_p99_ms" ]] || { mark_analytics_probe_failure "missing flapjack_analytics_rollup_generation_latency_ms_p99 metric"; return 1; }
  metric_must_be_number "flapjack_analytics_events_accepted_total" "$accepted_events" || return 1
  metric_must_be_number "flapjack_analytics_events_dropped_total" "$dropped_events" || return 1
  metric_must_be_number "flapjack_analytics_rollup_windows_generated_total" "$rollup_windows_generated" || return 1
  metric_must_be_number "flapjack_analytics_rollup_events_generated_total" "$rollup_events_generated" || return 1
  metric_must_be_number "flapjack_analytics_rollup_latest_nonempty_window_end_ms" "$rollup_latest_nonempty_window_end_ms" || return 1
  metric_must_be_number "flapjack_analytics_soak_marker_first_event_timestamp_ms" "$soak_marker_first_event_timestamp_ms" || return 1
  metric_must_be_number "flapjack_analytics_rollup_generation_latency_ms_p99" "$rollup_p99_ms" || return 1
  if ! awk -v metric="$soak_marker_first_event_timestamp_ms" 'BEGIN {exit !(metric > 0)}'; then
    mark_analytics_probe_failure "server soak marker metric reports no observed soak marker event"
    return 1
  fi
  # PL-9: keep the soak-marker boundary derivation aligned with the same
  # override width the engine uses, or the proof would assert an hour boundary
  # that minute-granularity rollups never produce.
  SOAK_MARKER_WINDOW_MS="$(resolve_loadtest_rollup_window_ms)"
  SOAK_MARKER_HOUR_END_BOUNDARY_MS="$(
    awk -v metric="$soak_marker_first_event_timestamp_ms" -v window="$SOAK_MARKER_WINDOW_MS" \
      'BEGIN {printf "%.0f", (int(metric / window) + 1) * window}'
  )"
  if [[ "$SOAK_MARKER_HOUR_END_BOUNDARY_MS" -le 0 ]]; then
    mark_analytics_probe_failure "failed to derive soak marker boundary from first marker event timestamp"
    return 1
  fi
  if ! awk -v metric="$rollup_windows_generated" 'BEGIN {exit !(metric > 0)}'; then
    mark_analytics_probe_failure "rollup generation metric reports zero windows generated"
    return 1
  fi
  if ! awk -v metric="$rollup_events_generated" 'BEGIN {exit !(metric > 0)}'; then
    mark_analytics_probe_failure "rollup generation metric reports zero rolled-up events for soak windows"
    return 1
  fi
  if ! awk -v observed="$rollup_latest_nonempty_window_end_ms" -v boundary="$SOAK_MARKER_HOUR_END_BOUNDARY_MS" 'BEGIN {exit !(observed >= boundary)}'; then
    mark_analytics_probe_failure "latest non-empty rollup window end (${rollup_latest_nonempty_window_end_ms}) precedes soak marker boundary (${SOAK_MARKER_HOUR_END_BOUNDARY_MS})"
    return 1
  fi

  ANALYTICS_ACCEPTED_EVENTS="$accepted_events"
  ANALYTICS_DROPPED_EVENTS="$dropped_events"
  ANALYTICS_ROLLUP_GENERATION_P99_MS="$rollup_p99_ms"

  if awk -v metric="$ANALYTICS_ROLLUP_GENERATION_P99_MS" 'BEGIN {exit !(metric < 5000)}'; then
    ANALYTICS_ROLLUP_LAG_VERDICT="PASS (windows=${rollup_windows_generated} events=${rollup_events_generated} latest_nonempty_window_end_ms=${rollup_latest_nonempty_window_end_ms} soak_marker_event_ms=${soak_marker_first_event_timestamp_ms} soak_marker_boundary_ms=${SOAK_MARKER_HOUR_END_BOUNDARY_MS} p99=${ANALYTICS_ROLLUP_GENERATION_P99_MS}ms)"
  else
    ANALYTICS_ROLLUP_LAG_VERDICT="FAIL (windows=${rollup_windows_generated} events=${rollup_events_generated} latest_nonempty_window_end_ms=${rollup_latest_nonempty_window_end_ms} soak_marker_event_ms=${soak_marker_first_event_timestamp_ms} soak_marker_boundary_ms=${SOAK_MARKER_HOUR_END_BOUNDARY_MS} p99=${ANALYTICS_ROLLUP_GENERATION_P99_MS}ms)"
    mark_analytics_probe_failure "rollup lag verdict failed: expected p99 < 5000ms, got ${ANALYTICS_ROLLUP_GENERATION_P99_MS}ms"
    return 1
  fi

  if awk -v metric="$ANALYTICS_DROPPED_EVENTS" 'BEGIN {exit !(metric == 0)}'; then
    ANALYTICS_ZERO_DROPPED_VERDICT="PASS (dropped=${ANALYTICS_DROPPED_EVENTS})"
  else
    ANALYTICS_ZERO_DROPPED_VERDICT="FAIL (dropped=${ANALYTICS_DROPPED_EVENTS})"
    mark_analytics_probe_failure "zero dropped events verdict failed: expected 0, got ${ANALYTICS_DROPPED_EVENTS}"
    return 1
  fi

  return 0
}

# TODO: Document discover_analytics_evidence_paths.
discover_analytics_evidence_paths() {
  local analytics_dir="$SERVER_DATA_DIR/analytics"
  local manifests=()
  local parquet_files=()
  local path
  if [[ ! -d "$analytics_dir" ]]; then
    MANIFEST_EVIDENCE_PATHS="N/A (analytics dir missing)"
    PARQUET_EVIDENCE_PATHS="N/A (analytics dir missing)"
    return 0
  fi

  shopt -s globstar nullglob
  for path in "$analytics_dir"/**/manifest.json; do
    manifests+=("$path")
  done
  for path in "$analytics_dir"/**/*.parquet; do
    parquet_files+=("$path")
  done
  shopt -u globstar nullglob

  if ((${#manifests[@]} > 0)); then
    MANIFEST_EVIDENCE_PATHS="$(printf '%s\n' "${manifests[@]}" | sort | tr '\n' ';' | sed 's/;$//')"
  else
    MANIFEST_EVIDENCE_PATHS="N/A"
  fi

  if ((${#parquet_files[@]} > 0)); then
    PARQUET_EVIDENCE_PATHS="$(printf '%s\n' "${parquet_files[@]}" | sort | head -n 10 | tr '\n' ';' | sed 's/;$//')"
  else
    PARQUET_EVIDENCE_PATHS="N/A"
  fi
}

# TODO: Document run_retention_gate_probe.
run_retention_gate_probe() {
  local probe_output
  local probe_status=0
  local analytics_dir="$SERVER_DATA_DIR/analytics"

  probe_output="$(
    cd "$ENGINE_DIR"
    cargo run -q -p flapjack-http --bin analytics_retention_probe -- "$analytics_dir"
  )" || probe_status=$?

  if [[ "$probe_status" -ne 0 ]]; then
    RETENTION_GATE_VERDICT="FAIL"
    ANALYTICS_PROBE_FAILURE_REASON="retention gate probe failed against $analytics_dir"
    return 1
  fi

  RETENTION_GATE_VERDICT="$(
    awk -F= '$1 == "retention_gate_verdict" { print $2 }' <<<"$probe_output" | tr -d '[:space:]'
  )"
  RETENTION_GATE_PROBE_PARTITION_PATH="$(
    awk -F= '$1 == "probe_partition_path" { print $2 }' <<<"$probe_output" | tr -d '\r'
  )"

  if [[ "$RETENTION_GATE_VERDICT" != "pass" ]]; then
    RETENTION_GATE_VERDICT="FAIL (${RETENTION_GATE_VERDICT:-missing})"
    ANALYTICS_PROBE_FAILURE_REASON="retention gate probe returned non-pass verdict: ${RETENTION_GATE_VERDICT:-missing}"
    return 1
  fi
  RETENTION_GATE_VERDICT="PASS"
  return 0
}

# TODO: Document write_summary_on_analytics_probe_failure.
write_summary_on_analytics_probe_failure() {
  local failure_reason="$1"
  discover_analytics_evidence_paths
  ANALYTICS_ACCEPTED_EVENTS="${ANALYTICS_ACCEPTED_EVENTS:-0}"
  ANALYTICS_DROPPED_EVENTS="${ANALYTICS_DROPPED_EVENTS:-0}"
  ANALYTICS_ROLLUP_GENERATION_P99_MS="${ANALYTICS_ROLLUP_GENERATION_P99_MS:-0}"
  RETENTION_GATE_PROBE_PARTITION_PATH="${RETENTION_GATE_PROBE_PARTITION_PATH:-N/A}"
  MANIFEST_EVIDENCE_PATHS="${MANIFEST_EVIDENCE_PATHS:-N/A}"
  PARQUET_EVIDENCE_PATHS="${PARQUET_EVIDENCE_PATHS:-N/A}"

  cat >"$SUMMARY_PATH" <<EOF
# Soak Proof Summary

- Scenario: \`${SCENARIO_NAME}\`
- Soak duration: \`${FLAPJACK_SOAK_DURATION}\`
- Base URL: \`${FLAPJACK_BASE_URL}\`
- Bind address: \`${FLAPJACK_BIND_ADDR}\`
- k6 REST API address: \`${K6_API_ADDR}\`
- Server binary: \`${SERVER_BINARY}\`
- k6 exit code: \`${SCENARIO_EXIT_CODE}\`
- failure reason: \`${failure_reason}\`
- Artifacts:
  - k6 stdout: \`${K6_STDOUT_PATH}\`
  - k6 JSON: \`${K6_JSON_PATH}\`
  - memory samples: \`${SAMPLE_PATH}\`
  - server log: \`${SERVER_LOG_PATH}\`
  - retention probe partition: \`${RETENTION_GATE_PROBE_PARTITION_PATH}\`

## Analytics Proof

- analytics ingest target rate: \`${ANALYTICS_INGEST_TARGET_RATE}\`
- observed accepted-event totals: \`${ANALYTICS_ACCEPTED_EVENTS}\`
- observed dropped-event totals: \`${ANALYTICS_DROPPED_EVENTS}\`
- rollup lag verdict (p99 < 5s): \`${ANALYTICS_ROLLUP_LAG_VERDICT}\`
- zero dropped events verdict: \`${ANALYTICS_ZERO_DROPPED_VERDICT}\`
- retention gate verdict: \`${RETENTION_GATE_VERDICT}\`
- manifest evidence paths: \`${MANIFEST_EVIDENCE_PATHS}\`
- parquet evidence paths: \`${PARQUET_EVIDENCE_PATHS}\`
EOF
}

sample_write_admission_state() {
  local admission_dir="$SERVER_DATA_DIR/$FLAPJACK_WRITE_INDEX/write_admission"

  node - "$admission_dir" <<'NODE'
const fs = require("fs");
const path = require("path");

const admissionDir = process.argv[2];
function invalidSample(recordCount, entry, reason) {
  const safeReason = String(reason).replace(/[^A-Za-z0-9_.:-]/g, "_");
  process.stdout.write(`${recordCount},,invalid:${entry}:${safeReason}`);
  process.exit(0);
}

if (!fs.existsSync(admissionDir)) {
  process.stdout.write("0,,empty");
  process.exit(0);
}

let entries;
try {
  entries = fs.readdirSync(admissionDir).filter((entry) => entry.endsWith(".json"));
} catch (error) {
  process.stdout.write(`0,,invalid:${error.message}`);
  process.exit(0);
}

if (entries.length === 0) {
  process.stdout.write("0,,empty");
  process.exit(0);
}

const nowMs = Date.now();
let oldestAgeMs = 0;
let recordCount = 0;
for (const entry of entries) {
  const recordPath = path.join(admissionDir, entry);
  let envelope;
  try {
    envelope = JSON.parse(fs.readFileSync(recordPath, "utf8"));
  } catch (error) {
    if (error && error.code === "ENOENT") {
      continue;
    }
    invalidSample(entries.length, entry, error.message);
  }
  const record = envelope.record;
  if (!record || typeof record !== "object" || Array.isArray(record)) {
    invalidSample(entries.length, entry, "missing_record");
  }
  if (!Number.isFinite(record.created_at_ms) || record.created_at_ms <= 0) {
    invalidSample(entries.length, entry, "missing_created_at_ms");
  }
  recordCount += 1;
  oldestAgeMs = Math.max(oldestAgeMs, Math.max(0, nowMs - record.created_at_ms));
}

if (recordCount === 0) {
  process.stdout.write("0,,empty");
  process.exit(0);
}

process.stdout.write(`${recordCount},${Math.round(oldestAgeMs)},ok`);
NODE
}

write_sample_header() {
  echo "timestamp_utc,rss_kb,heap_bytes,pressure_level,admission_record_count,oldest_admission_age_ms,admission_sample_status"
}

append_current_sample() {
  local timestamp_utc
  local rss_kb
  local heap_bytes
  local pressure_level
  local admission_sample

  timestamp_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  rss_kb="$(ps -o rss= -p "$SERVER_PID" | tr -d ' ' || true)"
  heap_bytes="$(metric_value "flapjack_memory_heap_bytes" || true)"
  pressure_level="$(metric_value "flapjack_memory_pressure_level" || true)"
  admission_sample="$(sample_write_admission_state)"

  echo "${timestamp_utc},${rss_kb:-},${heap_bytes:-},${pressure_level:-},${admission_sample}"
}

start_sampler() {
  local sample_interval_seconds="${FLAPJACK_LOADTEST_SAMPLE_INTERVAL_SECONDS:-30}"

  {
    write_sample_header
    while [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; do
      append_current_sample
      sleep "$sample_interval_seconds"
    done
  } >"$SAMPLE_PATH" &

  SAMPLER_PID=$!
}

stop_sampler() {
  if [[ -z "$SAMPLER_PID" ]]; then
    return 0
  fi

  if kill -0 "$SAMPLER_PID" 2>/dev/null; then
    kill "$SAMPLER_PID" 2>/dev/null || true
  fi
  wait "$SAMPLER_PID" 2>/dev/null || true
  SAMPLER_PID=""
}

# TODO: Document run_soak_scenario.
run_soak_scenario() {
  local scenario_to_run="$SCENARIO_NAME"
  if [[ "$SCENARIO_NAME" == "analytics-soak" ]]; then
    scenario_to_run="mixed-soak"
  fi
  SOAK_START_EPOCH_MS="$(node -e 'process.stdout.write(String(Date.now()))')"

  echo "INFO: running ${SCENARIO_NAME} for ${FLAPJACK_SOAK_DURATION}"
  run_loadtest_scenario_with_artifacts \
    "$SCRIPT_DIR" \
    "$scenario_to_run" \
    "$K6_API_ADDR" \
    "$K6_JSON_PATH" \
    "$K6_STDOUT_PATH" || SCENARIO_EXIT_CODE=$?

  case "$SCENARIO_EXIT_CODE" in
    0)
      return 0
      ;;
    99)
      echo "WARN: ${SCENARIO_NAME} breached thresholds (exit code 99)"
      return 0
      ;;
    *)
      fail "${SCENARIO_NAME} failed with hard error (k6 exit code ${SCENARIO_EXIT_CODE})"
      ;;
  esac
}

poll_write_admission_drain() {
  local timeout_seconds="${FLAPJACK_LOADTEST_WRITE_ADMISSION_DRAIN_TIMEOUT_SECONDS:-30}"
  local started_epoch
  local elapsed
  local sample
  local record_count
  local sample_status

  WRITE_ADMISSION_DRAIN_DURATION_SECONDS="N/A"
  WRITE_ADMISSION_DRAIN_RECORD_COUNT="N/A"
  if [[ "$SCENARIO_NAME" != "write-soak" ]]; then
    return 0
  fi

  started_epoch="$(date +%s)"
  while true; do
    if [[ -n "$SERVER_PID" ]] && kill -0 "$SERVER_PID" 2>/dev/null; then
      append_current_sample >>"$SAMPLE_PATH"
    fi
    sample="$(sample_write_admission_state)"
    IFS=, read -r record_count _ sample_status <<<"$sample"
    elapsed="$(($(date +%s) - started_epoch))"

    if [[ "$sample_status" == invalid:* ]]; then
      WRITE_ADMISSION_DRAIN_DURATION_SECONDS="$elapsed"
      WRITE_ADMISSION_DRAIN_RECORD_COUNT="$record_count"
      return 1
    fi
    if [[ "$record_count" == "0" ]]; then
      WRITE_ADMISSION_DRAIN_DURATION_SECONDS="$elapsed"
      WRITE_ADMISSION_DRAIN_RECORD_COUNT="0"
      return 0
    fi
    if (( elapsed >= timeout_seconds )); then
      WRITE_ADMISSION_DRAIN_DURATION_SECONDS="$elapsed"
      WRITE_ADMISSION_DRAIN_RECORD_COUNT="$record_count"
      return 1
    fi
    sleep 1
  done
}

search_hit_count() {
  local index_name="$1"
  local payload="$2"
  local encoded_index_name
  local response

  encoded_index_name="$(loadtest_encode_path_component "$index_name")"
  response="$(loadtest_http_request POST "/1/indexes/${encoded_index_name}/query" "$payload" "200")"
  jq -r '.nbHits' <<<"$response"
}

# TODO: Document capture_stable_index_snapshot.
capture_stable_index_snapshot() {
  local index_name="$1"
  local payload="$2"
  local min_doc_count="$3"
  local stable_polls_required="${4:-2}"
  local max_attempts="${5:-180}"
  local poll_interval_seconds="${6:-1}"
  local previous_signature=""
  local stable_polls=0
  local attempt
  local last_pending_tasks="unknown"
  local last_doc_count="unknown"
  local last_hit_count="unknown"

  for ((attempt = 1; attempt <= max_attempts; attempt += 1)); do
    local index_item
    local pending_tasks
    local doc_count
    local hit_count
    local signature

    index_item="$(loadtest_get_index_item_json "$index_name")"
    pending_tasks="$(jq -r '.numberOfPendingTasks // 0' <<<"$index_item")"
    doc_count="$(jq -r '.entries // 0' <<<"$index_item")"
    hit_count="$(search_hit_count "$index_name" "$payload")"
    signature="${doc_count}:${hit_count}"
    last_pending_tasks="$pending_tasks"
    last_doc_count="$doc_count"
    last_hit_count="$hit_count"

    if [[ "$pending_tasks" == "0" && "$doc_count" -ge "$min_doc_count" && "$hit_count" -ge "$min_doc_count" ]]; then
      if [[ "$signature" == "$previous_signature" ]]; then
        stable_polls=$((stable_polls + 1))
      else
        stable_polls=1
        previous_signature="$signature"
      fi

      if [[ "$stable_polls" -ge "$stable_polls_required" ]]; then
        printf '%s,%s' "$doc_count" "$hit_count"
        return 0
      fi
    else
      previous_signature=""
      stable_polls=0
    fi

    sleep "$poll_interval_seconds"
  done

  fail "index ${index_name} did not reach a stable snapshot after ${max_attempts} polls (pending=${last_pending_tasks} doc_count=${last_doc_count} hit_count=${last_hit_count})"
}

consistency_key() {
  local phase="$1"
  local metric="$2"
  printf '%s_%s' "$phase" "$metric"
}

append_consistency_value() {
  local phase="$1"
  local metric="$2"
  local value="$3"
  printf '%s=%s\n' "$(consistency_key "$phase" "$metric")" "$value" >>"$RESULTS_DIR/consistency.env"
}

phase_consistency_value() {
  local phase="$1"
  local metric="$2"
  read_consistency_value "$(consistency_key "$phase" "$metric")"
}

load_phase_consistency_snapshot() {
  local phase="$1"
  local read_doc_count
  local write_doc_count
  local macbook_hits
  local write_index_hits

  read_doc_count="$(phase_consistency_value "$phase" "read_doc_count")"
  write_doc_count="$(phase_consistency_value "$phase" "write_doc_count")"
  macbook_hits="$(phase_consistency_value "$phase" "macbook_hits")"
  write_index_hits="$(phase_consistency_value "$phase" "write_index_hits")"
  printf '%s,%s,%s,%s' "$read_doc_count" "$write_doc_count" "$macbook_hits" "$write_index_hits"
}

# TODO: Document capture_consistency_snapshot.
capture_consistency_snapshot() {
  local phase="$1"
  local read_doc_count
  local write_doc_count
  local macbook_hits
  local write_index_hits
  local read_snapshot
  local write_snapshot

  read_snapshot="$(capture_stable_index_snapshot "$FLAPJACK_READ_INDEX" '{"query":"MacBook","hitsPerPage":5}' 1)"
  write_snapshot="$(capture_stable_index_snapshot "$FLAPJACK_WRITE_INDEX" '{"query":"","hitsPerPage":1}' 1)"
  IFS=, read -r read_doc_count macbook_hits <<<"$read_snapshot"
  IFS=, read -r write_doc_count write_index_hits <<<"$write_snapshot"

  append_consistency_value "$phase" "read_doc_count" "$read_doc_count"
  append_consistency_value "$phase" "write_doc_count" "$write_doc_count"
  append_consistency_value "$phase" "macbook_hits" "$macbook_hits"
  append_consistency_value "$phase" "write_index_hits" "$write_index_hits"

  [[ "$read_doc_count" == "1000" ]] || fail "expected read index doc count 1000 during ${phase}, got ${read_doc_count}"
  [[ "$macbook_hits" -ge 1 ]] || fail "expected seeded MacBook hits during ${phase}, got ${macbook_hits}"
  [[ "$write_doc_count" -ge 1 ]] || fail "expected write index forward progress during ${phase}, got ${write_doc_count}"
  [[ "$write_index_hits" -ge 1 ]] || \
    fail "expected write index query hits during ${phase}, got ${write_index_hits}"
}

# TODO: Document read_consistency_value.
read_consistency_value() {
  local key="$1"

  awk -F= -v key="$key" '
    $1 == key {
      print substr($0, index($0, "=") + 1)
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$RESULTS_DIR/consistency.env"
}

k6_json_stream_command() {
  if [[ "$K6_JSON_PATH" == *.gz ]]; then
    printf 'gzip -cd %q' "$K6_JSON_PATH"
  else
    printf 'cat %q' "$K6_JSON_PATH"
  fi
}

parse_duration_seconds() {
  local raw_duration="$1"
  node - "$raw_duration" <<'NODE'
const raw = process.argv[2] || "";
const match = raw.match(/^([1-9][0-9]*)(ms|s|m|h)$/);
if (!match) {
  process.exit(1);
}
const value = Number(match[1]);
const unit = match[2];
const multipliers = { ms: 0.001, s: 1, m: 60, h: 3600 };
process.stdout.write(String(value * multipliers[unit]));
NODE
}

load_write_k6_metrics() {
  local parsed_metrics
  [[ -f "$K6_JSON_PATH" ]] || return 1

  parsed_metrics="$(
    node - "$K6_JSON_PATH" <<'NODE'
const fs = require("fs");
const zlib = require("zlib");

const k6JsonPath = process.argv[2];
const rawBytes = fs.readFileSync(k6JsonPath);
const rawText = k6JsonPath.endsWith(".gz") ? zlib.gunzipSync(rawBytes).toString("utf8") : rawBytes.toString("utf8");
const input = rawText.trim().split(/\n+/).filter(Boolean);
const values = {
  attempted: 0,
  accepted: 0,
  queueFull: 0,
  unexpected4xx: 0,
  server5xx: 0,
  dropped: 0,
};
const acceptedDurations = [];

for (const line of input) {
  let event;
  try {
    event = JSON.parse(line);
  } catch (error) {
    console.error(`invalid k6 JSON line: ${error.message}`);
    process.exit(2);
  }
  if (event.type !== "Point") {
    continue;
  }
  const metric = event.metric;
  const value = Number(event.data && event.data.value);
  const tags = (event.data && event.data.tags) || {};
  if (!Number.isFinite(value)) {
    console.error(`non-numeric k6 point for ${metric}`);
    process.exit(2);
  }
  if (metric === "http_reqs" && tags.type === "write") {
    values.attempted += value;
  } else if (metric === "write_http_accepted_200_count") {
    values.accepted += value;
  } else if (metric === "write_http_queue_full_429_count") {
    values.queueFull += value;
  } else if (metric === "write_http_unexpected_4xx_rate") {
    values.unexpected4xx += value;
  } else if (metric === "write_http_5xx_rate") {
    values.server5xx += value;
  } else if (metric === "dropped_iterations") {
    values.dropped += value;
  } else if (metric === "http_req_duration" && tags.type === "write" && tags.status === "200") {
    acceptedDurations.push(value);
  }
}

function percentile(values, percentileValue) {
  if (values.length === 0) {
    return "N/A";
  }
  const sorted = [...values].sort((left, right) => left - right);
  const index = Math.max(0, Math.ceil((percentileValue / 100) * sorted.length) - 1);
  return sorted[index].toFixed(3).replace(/\.?0+$/, "");
}

for (const [key, value] of Object.entries(values)) {
  console.log(`${key}=${Math.round(value)}`);
}
console.log(`acceptedP95=${percentile(acceptedDurations, 95)}`);
NODE
  )" || return 1

  WRITE_ATTEMPTED_REQUESTS="$(awk -F= '$1 == "attempted" { print $2 }' <<<"$parsed_metrics")"
  WRITE_ACCEPTED_200_COUNT="$(awk -F= '$1 == "accepted" { print $2 }' <<<"$parsed_metrics")"
  WRITE_QUEUE_FULL_429_COUNT="$(awk -F= '$1 == "queueFull" { print $2 }' <<<"$parsed_metrics")"
  WRITE_UNEXPECTED_4XX_COUNT="$(awk -F= '$1 == "unexpected4xx" { print $2 }' <<<"$parsed_metrics")"
  WRITE_5XX_COUNT="$(awk -F= '$1 == "server5xx" { print $2 }' <<<"$parsed_metrics")"
  WRITE_DROPPED_ITERATIONS="$(awk -F= '$1 == "dropped" { print $2 }' <<<"$parsed_metrics")"
  WRITE_ACCEPTED_P95_MS="$(awk -F= '$1 == "acceptedP95" { print $2 }' <<<"$parsed_metrics")"
  WRITE_DIRTY_ERROR_COUNT="$((WRITE_UNEXPECTED_4XX_COUNT + WRITE_5XX_COUNT))"
}

load_write_sample_metrics() {
  [[ -f "$SAMPLE_PATH" ]] || return 1

  local parsed_samples
  parsed_samples="$(
    awk -F, '
      NR == 1 {
        expected = "timestamp_utc,rss_kb,heap_bytes,pressure_level,admission_record_count,oldest_admission_age_ms,admission_sample_status"
        if ($0 != expected) {
          print "invalid=unexpected_header"
          failed = 1
          exit
        }
        next
      }
      NF != 7 {
        print "invalid=wrong_column_count"
        failed = 1
        exit
      }
      $2 !~ /^[0-9]+$/ || $3 !~ /^[0-9]+$/ || $5 !~ /^[0-9]+$/ {
        print "invalid=non_numeric_sample"
        failed = 1
        exit
      }
      $7 ~ /^invalid:/ || $7 == "" {
        print "invalid=bad_admission_sample"
        failed = 1
        exit
      }
      $6 != "" && $6 !~ /^[0-9]+$/ {
        print "invalid=non_numeric_oldest_age"
        failed = 1
        exit
      }
      {
        count += 1
        if (count == 1) {
          rss_start = $2
          heap_start = $3
          rss_peak = $2
          heap_peak = $3
        }
        rss_end = $2
        heap_end = $3
        if ($2 > rss_peak) rss_peak = $2
        if ($3 > heap_peak) heap_peak = $3
        if ($5 > peak_records) peak_records = $5
        if ($6 != "" && $6 > max_oldest_age) max_oldest_age = $6
      }
      END {
        if (failed) {
          exit
        }
        if (count == 0) {
          print "invalid=no_samples"
          exit
        }
        print "rss_start=" rss_start
        print "rss_peak=" rss_peak
        print "rss_end=" rss_end
        print "heap_start=" heap_start
        print "heap_peak=" heap_peak
        print "heap_end=" heap_end
        print "peak_records=" (peak_records + 0)
        print "max_oldest_age=" (max_oldest_age + 0)
      }
    ' "$SAMPLE_PATH"
  )"

  if grep -q '^invalid=' <<<"$parsed_samples"; then
    local invalid_reason
    invalid_reason="$(awk -F= '$1 == "invalid" { print $2 }' <<<"$parsed_samples")"
    WRITE_ADMISSION_SAMPLE_VERDICT="FAIL (${invalid_reason})"
    WRITE_MEMORY_SAMPLE_VERDICT="$WRITE_ADMISSION_SAMPLE_VERDICT"
    return 1
  fi

  WRITE_RSS_START_KB="$(awk -F= '$1 == "rss_start" { print $2 }' <<<"$parsed_samples")"
  WRITE_RSS_PEAK_KB="$(awk -F= '$1 == "rss_peak" { print $2 }' <<<"$parsed_samples")"
  WRITE_RSS_END_KB="$(awk -F= '$1 == "rss_end" { print $2 }' <<<"$parsed_samples")"
  WRITE_HEAP_START_BYTES="$(awk -F= '$1 == "heap_start" { print $2 }' <<<"$parsed_samples")"
  WRITE_HEAP_PEAK_BYTES="$(awk -F= '$1 == "heap_peak" { print $2 }' <<<"$parsed_samples")"
  WRITE_HEAP_END_BYTES="$(awk -F= '$1 == "heap_end" { print $2 }' <<<"$parsed_samples")"
  WRITE_PEAK_ADMISSION_RECORD_COUNT="$(awk -F= '$1 == "peak_records" { print $2 }' <<<"$parsed_samples")"
  WRITE_MAX_OLDEST_ADMISSION_AGE_MS="$(awk -F= '$1 == "max_oldest_age" { print $2 }' <<<"$parsed_samples")"
  WRITE_ADMISSION_SAMPLE_VERDICT="PASS"
  WRITE_MEMORY_SAMPLE_VERDICT="PASS"
}

read_summary_backtick_value() {
  local summary_path="$1"
  local label="$2"
  awk -v label="$label" '
    index($0, "- " label ": `") == 1 {
      value = $0
      sub("^- " label ": `", "", value)
      sub("`$", "", value)
      print value
      found = 1
      exit
    }
    END { if (!found) exit 1 }
  ' "$summary_path"
}

classify_write_soak_metrics() {
  local failures=0
  local status_sum
  local duration_seconds
  local control_summary="${FLAPJACK_LOADTEST_WRITE_CONTROL_SUMMARY:-}"
  local condition="${FLAPJACK_LOADTEST_WRITE_CONDITION:-standalone}"
  local accepted_floor="${FLAPJACK_LOADTEST_WRITE_ACCEPTED_FLOOR:-}"
  local enforced_expected_attempts="${FLAPJACK_LOADTEST_WRITE_EXPECTED_ATTEMPTS:-}"

  status_sum="$((WRITE_ACCEPTED_200_COUNT + WRITE_QUEUE_FULL_429_COUNT + WRITE_UNEXPECTED_4XX_COUNT + WRITE_5XX_COUNT))"
  if [[ "$status_sum" == "$WRITE_ATTEMPTED_REQUESTS" ]]; then
    WRITE_STATUS_SUM_VERDICT="PASS"
  else
    WRITE_STATUS_SUM_VERDICT="FAIL (status_sum=${status_sum})"
    failures=$((failures + 1))
  fi

  if [[ -n "${FLAPJACK_LOADTEST_WRITE_TARGET_RPS:-}" ]]; then
    duration_seconds="$(parse_duration_seconds "$FLAPJACK_SOAK_DURATION" || true)"
    if [[ -n "$duration_seconds" ]]; then
      WRITE_EXPECTED_ATTEMPTS="$(awk -v rate="$FLAPJACK_LOADTEST_WRITE_TARGET_RPS" -v seconds="$duration_seconds" 'BEGIN { printf "%.0f", rate * seconds }')"
    fi
  fi
  if [[ -n "$enforced_expected_attempts" ]]; then
    WRITE_EXPECTED_ATTEMPTS="$enforced_expected_attempts"
    if [[ "$WRITE_ATTEMPTED_REQUESTS" == "$WRITE_EXPECTED_ATTEMPTS" ]]; then
      WRITE_ATTEMPT_COUNT_VERDICT="PASS"
    else
      WRITE_ATTEMPT_COUNT_VERDICT="FAIL (expected=${WRITE_EXPECTED_ATTEMPTS})"
      failures=$((failures + 1))
    fi
  elif [[ -n "${FLAPJACK_LOADTEST_WRITE_TARGET_RPS:-}" ]]; then
    WRITE_ATTEMPT_COUNT_VERDICT="PASS (not enforced)"
  else
    WRITE_ATTEMPT_COUNT_VERDICT="PASS (no fixed target)"
  fi

  if [[ "$WRITE_DROPPED_ITERATIONS" == "0" ]]; then
    WRITE_DROPPED_ITERATIONS_VERDICT="PASS"
  else
    WRITE_DROPPED_ITERATIONS_VERDICT="FAIL"
    failures=$((failures + 1))
  fi
  if [[ "$WRITE_DIRTY_ERROR_COUNT" == "0" ]]; then
    WRITE_DIRTY_ERROR_VERDICT="PASS"
  else
    WRITE_DIRTY_ERROR_VERDICT="FAIL"
    failures=$((failures + 1))
  fi
  if awk -v age="$WRITE_MAX_OLDEST_ADMISSION_AGE_MS" 'BEGIN { exit !(age < 30000) }'; then
    WRITE_ADMISSION_AGE_VERDICT="PASS"
  else
    WRITE_ADMISSION_AGE_VERDICT="FAIL"
    failures=$((failures + 1))
  fi
  if [[ "$WRITE_ADMISSION_DRAIN_RECORD_COUNT" == "0" ]] && \
      awk -v seconds="$WRITE_ADMISSION_DRAIN_DURATION_SECONDS" 'BEGIN { exit !(seconds <= 30) }'; then
    WRITE_ADMISSION_DRAIN_VERDICT="PASS"
  else
    WRITE_ADMISSION_DRAIN_VERDICT="FAIL"
    failures=$((failures + 1))
  fi

  if [[ "$condition" == "candidate" ]]; then
    if (( WRITE_QUEUE_FULL_429_COUNT > 0 )); then
      WRITE_BACKPRESSURE_VERDICT="PASS"
    else
      WRITE_BACKPRESSURE_VERDICT="FAIL"
    fi
  else
    WRITE_BACKPRESSURE_VERDICT="N/A (${condition})"
  fi

  if [[ -n "$accepted_floor" ]]; then
    if (( WRITE_ACCEPTED_200_COUNT >= accepted_floor )); then
      WRITE_CONTROL_ACCEPTANCE_VERDICT="PASS"
    else
      WRITE_CONTROL_ACCEPTANCE_VERDICT="FAIL (floor=${accepted_floor})"
      failures=$((failures + 1))
    fi
  fi

  if [[ -n "$control_summary" ]]; then
    local control_accepted
    local control_p95
    control_accepted="$(read_summary_backtick_value "$control_summary" "write accepted 200 count")"
    control_p95="$(read_summary_backtick_value "$control_summary" "write accepted 200 p95 ms")"
    if (( WRITE_ACCEPTED_200_COUNT >= control_accepted )); then
      WRITE_CANDIDATE_ACCEPTANCE_VERDICT="PASS"
    else
      WRITE_CANDIDATE_ACCEPTANCE_VERDICT="FAIL (control=${control_accepted})"
      failures=$((failures + 1))
    fi
    if awk -v candidate="$WRITE_ACCEPTED_P95_MS" -v control="$control_p95" 'BEGIN { exit !(candidate <= control * 1.10) }'; then
      WRITE_CANDIDATE_LATENCY_VERDICT="PASS"
    else
      WRITE_CANDIDATE_LATENCY_VERDICT="FAIL (control=${control_p95})"
      failures=$((failures + 1))
    fi
  fi

  if (( failures == 0 )); then
    if [[ "$condition" == "candidate" && "$WRITE_BACKPRESSURE_VERDICT" == "PASS" ]]; then
      WRITE_OVERALL_VERDICT="CONFIRMED_BOUNDED_LAG"
    elif [[ "$condition" == "candidate" ]]; then
      WRITE_OVERALL_VERDICT="INCONCLUSIVE_LOAD_NOT_SATURATING"
    else
      WRITE_OVERALL_VERDICT="PASS"
    fi
  else
    WRITE_OVERALL_VERDICT="FALSIFIED_UNBOUNDED_OR_REGRESSED"
  fi
}

capture_write_liveness_proof() {
  local distribution_output

  if [[ "$SCENARIO_NAME" != "write-soak" ]]; then
    return 0
  fi
  if ! declare -F liveness_distribution >/dev/null; then
    # Acceptance fixtures source this script without running main, so load the
    # canonical evaluator here rather than duplicating its distribution math.
    # shellcheck source=lib/loadtest_shell_helpers.sh
    source "$LOADTEST_HELPERS"
  fi

  if distribution_output="$(
    LIVENESS_MIN_SAMPLES_PER_ENDPOINT=100 \
      liveness_distribution "$LIVENESS_SAMPLE_PATH" 250 5000
  )"; then
    WRITE_LIVENESS_VERDICT="PASS"
  else
    WRITE_LIVENESS_VERDICT="FAIL"
  fi
  WRITE_HEALTH_LIVENESS="$(
    awk '/^endpoint=health / { print; exit }' <<<"$distribution_output"
  )"
  WRITE_COUNT_LIVENESS="$(
    awk '/^endpoint=count / { print; exit }' <<<"$distribution_output"
  )"
  WRITE_HEALTH_LIVENESS="${WRITE_HEALTH_LIVENESS:-missing}"
  WRITE_COUNT_LIVENESS="${WRITE_COUNT_LIVENESS:-missing}"
  [[ "$WRITE_LIVENESS_VERDICT" == "PASS" ]]
}

capture_write_soak_proof() {
  if [[ "$SCENARIO_NAME" != "write-soak" ]]; then
    return 0
  fi

  load_write_k6_metrics || {
    WRITE_OVERALL_VERDICT="FALSIFIED_UNBOUNDED_OR_REGRESSED"
    return 1
  }
  load_write_sample_metrics || {
    WRITE_OVERALL_VERDICT="FALSIFIED_UNBOUNDED_OR_REGRESSED"
    return 1
  }
  classify_write_soak_metrics
  capture_write_liveness_proof || {
    WRITE_OVERALL_VERDICT="FALSIFIED_UNBOUNDED_OR_REGRESSED"
    return 1
  }
  [[ "$WRITE_OVERALL_VERDICT" != "FALSIFIED_UNBOUNDED_OR_REGRESSED" ]]
}

# TODO: Document write_summary.
write_summary() {
  local write_summary_status=0
  local sample_count
  local rss_start
  local rss_end
  local rss_min
  local rss_max
  local heap_start
  local heap_end
  local heap_min
  local heap_max
  local max_pressure_level
  local post_soak_read_doc_count
  local post_soak_write_doc_count
  local post_soak_macbook_hits
  local post_soak_write_index_hits
  local post_restart_read_doc_count
  local post_restart_write_doc_count
  local post_restart_macbook_hits
  local post_restart_write_index_hits
  capture_write_soak_proof || write_summary_status=$?
  post_soak_read_doc_count="$(phase_consistency_value "post_soak" "read_doc_count")"
  post_soak_write_doc_count="$(phase_consistency_value "post_soak" "write_doc_count")"
  post_soak_macbook_hits="$(phase_consistency_value "post_soak" "macbook_hits")"
  post_soak_write_index_hits="$(phase_consistency_value "post_soak" "write_index_hits")"
  post_restart_read_doc_count="$(phase_consistency_value "post_restart" "read_doc_count")"
  post_restart_write_doc_count="$(phase_consistency_value "post_restart" "write_doc_count")"
  post_restart_macbook_hits="$(phase_consistency_value "post_restart" "macbook_hits")"
  post_restart_write_index_hits="$(phase_consistency_value "post_restart" "write_index_hits")"
  sample_count="$(awk 'NR > 1 { count += 1 } END { print count + 0 }' "$SAMPLE_PATH")"
  rss_start="$(awk -F, 'NR == 2 { print $2 }' "$SAMPLE_PATH")"
  rss_end="$(awk -F, 'END { print $2 }' "$SAMPLE_PATH")"
  rss_min="$(awk -F, 'NR == 2 { min = $2 } NR > 2 && $2 < min { min = $2 } END { print min }' "$SAMPLE_PATH")"
  rss_max="$(awk -F, 'NR == 2 { max = $2 } NR > 2 && $2 > max { max = $2 } END { print max }' "$SAMPLE_PATH")"
  heap_start="$(awk -F, 'NR == 2 { print $3 }' "$SAMPLE_PATH")"
  heap_end="$(awk -F, 'END { print $3 }' "$SAMPLE_PATH")"
  heap_min="$(awk -F, 'NR == 2 { min = $3 } NR > 2 && $3 < min { min = $3 } END { print min }' "$SAMPLE_PATH")"
  heap_max="$(awk -F, 'NR == 2 { max = $3 } NR > 2 && $3 > max { max = $3 } END { print max }' "$SAMPLE_PATH")"
  max_pressure_level="$(awk -F, 'NR == 2 { max = $4 } NR > 2 && $4 > max { max = $4 } END { print max }' "$SAMPLE_PATH")"

  cat >"$SUMMARY_PATH" <<EOF
# Soak Proof Summary

- Scenario: \`${SCENARIO_NAME}\`
- Soak duration: \`${FLAPJACK_SOAK_DURATION}\`
- Base URL: \`${FLAPJACK_BASE_URL}\`
- Bind address: \`${FLAPJACK_BIND_ADDR}\`
- k6 REST API address: \`${K6_API_ADDR}\`
- Server binary: \`${SERVER_BINARY}\`
- k6 exit code: \`${SCENARIO_EXIT_CODE}\`
- Threshold interpretation:
  - \`0\` means the soak stayed inside the current k6 threshold contract
  - \`99\` means thresholds were breached but the soak still completed and post-soak checks ran
- Artifacts:
  - k6 stdout: \`${K6_STDOUT_PATH}\`
  - k6 JSON: \`${K6_JSON_PATH}\`
  - memory samples: \`${SAMPLE_PATH}\`
  - liveness samples: \`${LIVENESS_SAMPLE_PATH}\`
  - server log: \`${SERVER_LOG_PATH}\`
  - retention probe partition: \`${RETENTION_GATE_PROBE_PARTITION_PATH}\`

## Analytics Proof

- analytics ingest target rate: \`${ANALYTICS_INGEST_TARGET_RATE}\`
- observed accepted-event totals: \`${ANALYTICS_ACCEPTED_EVENTS}\`
- observed dropped-event totals: \`${ANALYTICS_DROPPED_EVENTS}\`
- rollup lag verdict (p99 < 5s): \`${ANALYTICS_ROLLUP_LAG_VERDICT}\`
- zero dropped events verdict: \`${ANALYTICS_ZERO_DROPPED_VERDICT}\`
- retention gate verdict: \`${RETENTION_GATE_VERDICT}\`
- manifest evidence paths: \`${MANIFEST_EVIDENCE_PATHS}\`
- parquet evidence paths: \`${PARQUET_EVIDENCE_PATHS}\`

## Memory Samples

- sample count: \`${sample_count}\`
- RSS KB: start=\`${rss_start}\`, end=\`${rss_end}\`, min=\`${rss_min}\`, max=\`${rss_max}\`
- Heap bytes: start=\`${heap_start}\`, end=\`${heap_end}\`, min=\`${heap_min}\`, max=\`${heap_max}\`
- max pressure level: \`${max_pressure_level}\`
- latency drift over time lives in the k6 JSON artifact; this summary keeps the canonical file path instead of duplicating k6 math in shell

## Write Soak Proof

- write expected attempts: \`${WRITE_EXPECTED_ATTEMPTS}\`
- write attempted requests: \`${WRITE_ATTEMPTED_REQUESTS}\`
- write dropped iterations: \`${WRITE_DROPPED_ITERATIONS}\`
- write accepted 200 count: \`${WRITE_ACCEPTED_200_COUNT}\`
- write QueueFull 429 count: \`${WRITE_QUEUE_FULL_429_COUNT}\`
- write unexpected 4xx count: \`${WRITE_UNEXPECTED_4XX_COUNT}\`
- write 5xx count: \`${WRITE_5XX_COUNT}\`
- write dirty-error count: \`${WRITE_DIRTY_ERROR_COUNT}\`
- write accepted 200 p95 ms: \`${WRITE_ACCEPTED_P95_MS}\`
- write status-sum verdict: \`${WRITE_STATUS_SUM_VERDICT}\`
- write attempt-count verdict: \`${WRITE_ATTEMPT_COUNT_VERDICT}\`
- write dropped-iterations verdict: \`${WRITE_DROPPED_ITERATIONS_VERDICT}\`
- write dirty-error verdict: \`${WRITE_DIRTY_ERROR_VERDICT}\`
- write candidate backpressure verdict: \`${WRITE_BACKPRESSURE_VERDICT}\`
- write control acceptance verdict: \`${WRITE_CONTROL_ACCEPTANCE_VERDICT}\`
- write candidate acceptance verdict: \`${WRITE_CANDIDATE_ACCEPTANCE_VERDICT}\`
- write candidate accepted-p95 verdict: \`${WRITE_CANDIDATE_LATENCY_VERDICT}\`
- write admission sample verdict: \`${WRITE_ADMISSION_SAMPLE_VERDICT}\`
- write admission max oldest age ms: \`${WRITE_MAX_OLDEST_ADMISSION_AGE_MS}\`
- write admission age verdict: \`${WRITE_ADMISSION_AGE_VERDICT}\`
- write admission peak record count: \`${WRITE_PEAK_ADMISSION_RECORD_COUNT}\`
- write admission drain duration seconds: \`${WRITE_ADMISSION_DRAIN_DURATION_SECONDS}\`
- write admission drain record count: \`${WRITE_ADMISSION_DRAIN_RECORD_COUNT}\`
- write admission drain verdict: \`${WRITE_ADMISSION_DRAIN_VERDICT}\`
- write RSS KB diagnostics: start=\`${WRITE_RSS_START_KB}\`, peak=\`${WRITE_RSS_PEAK_KB}\`, end=\`${WRITE_RSS_END_KB}\`
- write heap bytes diagnostics: start=\`${WRITE_HEAP_START_BYTES}\`, peak=\`${WRITE_HEAP_PEAK_BYTES}\`, end=\`${WRITE_HEAP_END_BYTES}\`
- write memory sample verdict: \`${WRITE_MEMORY_SAMPLE_VERDICT}\`
- write health liveness: \`${WRITE_HEALTH_LIVENESS}\`
- write count liveness: \`${WRITE_COUNT_LIVENESS}\`
- write liveness verdict: \`${WRITE_LIVENESS_VERDICT}\`
- write overall verdict: \`${WRITE_OVERALL_VERDICT}\`

## Consistency Checks

- post-soak read doc count: \`${post_soak_read_doc_count}\`
- post-soak write doc count: \`${post_soak_write_doc_count}\`
- post-soak seeded MacBook hits: \`${post_soak_macbook_hits}\`
- post-soak write-index hits: \`${post_soak_write_index_hits}\`
- post-restart read doc count: \`${post_restart_read_doc_count}\`
- post-restart write doc count: \`${post_restart_write_doc_count}\`
- post-restart seeded MacBook hits: \`${post_restart_macbook_hits}\`
- post-restart write-index hits: \`${post_restart_write_index_hits}\`
EOF

  return "$write_summary_status"
}

# TODO: Document verify_restart_preserved_counts.
assert_restart_preserved_value() {
  local metric="$1"
  local label="$2"
  local post_soak_value
  local post_restart_value

  post_soak_value="$(phase_consistency_value "post_soak" "$metric")"
  post_restart_value="$(phase_consistency_value "post_restart" "$metric")"
  [[ "$post_restart_value" == "$post_soak_value" ]] || \
    fail "restart changed ${label}: post_soak=${post_soak_value} post_restart=${post_restart_value}"
}

verify_restart_preserved_counts() {
  assert_restart_preserved_value "read_doc_count" "read index doc count"
  assert_restart_preserved_value "write_doc_count" "write index doc count"
  assert_restart_preserved_value "macbook_hits" "read index hit count"
  assert_restart_preserved_value "write_index_hits" "write index hit count"
}

# TODO: Document cleanup.
cleanup() {
  local script_exit_code=$?
  local effective_exit_code="$script_exit_code"
  if [[ "$INTERRUPTED_EXIT_CODE" -ne 0 ]]; then
    effective_exit_code="$INTERRUPTED_EXIT_CODE"
  fi
  if declare -F stop_liveness_sampler >/dev/null; then
    stop_liveness_sampler LIVENESS_SAMPLER_PID
  fi
  stop_sampler
  stop_server

  if [[ -n "$RUNNER_TMP_DIR" && -d "$RUNNER_TMP_DIR" ]]; then
    if [[ "$SCENARIO_EXIT_CODE" -eq 0 && "$effective_exit_code" -eq 0 ]]; then
      rm -rf "$RUNNER_TMP_DIR"
      RUNNER_TMP_DIR=""
      return 0
    fi

    if [[ -n "$RESULTS_DIR" ]]; then
      mkdir -p "$RESULTS_DIR"
      if [[ -d "$SERVER_DATA_DIR" ]]; then
        rm -rf "$RESULTS_DIR/failure_server_data"
        cp -R "$SERVER_DATA_DIR" "$RESULTS_DIR/failure_server_data"
      fi
      if [[ -d "$SERVER_DATA_DIR/analytics" ]]; then
        rm -rf "$RESULTS_DIR/failure_analytics"
        cp -R "$SERVER_DATA_DIR/analytics" "$RESULTS_DIR/failure_analytics"
      fi
      rm -rf "$RESULTS_DIR/failure_runner_state"
      cp -R "$RUNNER_TMP_DIR" "$RESULTS_DIR/failure_runner_state"
      echo "INFO: preserving non-pass soak runner state at $RUNNER_TMP_DIR"
      echo "INFO: failure analytics path: $RESULTS_DIR/failure_analytics"
    fi
  fi
}

# TODO: Document main.
main() {
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --scenario)
        SCENARIO_NAME="${2:-}"
        shift 2
        ;;
      --help|-h)
        usage
        return 0
        ;;
      *)
        echo "ERROR: unknown argument: $1" >&2
        usage >&2
        return 1
        ;;
    esac
  done

  [[ "$SCENARIO_NAME" == "mixed-soak" || "$SCENARIO_NAME" == "write-soak" || "$SCENARIO_NAME" == "analytics-soak" ]] || \
    fail "--scenario must be one of: mixed-soak, write-soak, analytics-soak"
  [[ -f "$LOADTEST_HELPERS" ]] || fail "missing $LOADTEST_HELPERS"
  [[ -f "$SOAK_HELPERS" ]] || fail "missing $SOAK_HELPERS"
  [[ -x "$SEED_SCRIPT" ]] || fail "missing executable $SEED_SCRIPT"

  # shellcheck source=lib/loadtest_shell_helpers.sh
  source "$LOADTEST_HELPERS"
  # shellcheck source=lib/loadtest_soak_helpers.sh
  source "$SOAK_HELPERS"

  require_loadtest_commands cargo curl jq k6 node ps
  load_shared_loadtest_config
  initialize_loadtest_auth_headers
  load_dashboard_seed_settings "$SCRIPT_DIR"

  create_results_dir
  FLAPJACK_BIND_ADDR="$(derive_bind_addr_from_base_url "$FLAPJACK_BASE_URL")"
  derive_k6_api_addr
  build_or_reuse_binary
  initialize_soak_marker_user_token

  RUNNER_TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-loadtest-soak.XXXXXX")"
  SERVER_DATA_DIR="$RUNNER_TMP_DIR/data"

  start_server
  wait_for_health
  if scenario_requires_analytics_proof; then
    assert_analytics_metric_contract
  fi
  "$SEED_SCRIPT"

  start_sampler
  if [[ "$SCENARIO_NAME" == "write-soak" ]]; then
    start_liveness_sampler \
      LIVENESS_SAMPLER_PID \
      "$FLAPJACK_BASE_URL" \
      "$FLAPJACK_WRITE_INDEX" \
      "$LIVENESS_SAMPLE_PATH" \
      "$SERVER_PID"
  fi
  run_soak_scenario
  stop_liveness_sampler LIVENESS_SAMPLER_PID
  stop_sampler
  poll_write_admission_drain || true
  if scenario_requires_analytics_proof; then
    if ! capture_analytics_proof; then
      write_summary_on_analytics_probe_failure "${ANALYTICS_PROBE_FAILURE_REASON:-analytics proof capture failed}"
      fail "${ANALYTICS_PROBE_FAILURE_REASON:-analytics proof capture failed}"
    fi
    if ! run_retention_gate_probe; then
      write_summary_on_analytics_probe_failure "${ANALYTICS_PROBE_FAILURE_REASON:-retention gate probe failed}"
      fail "${ANALYTICS_PROBE_FAILURE_REASON:-retention gate probe failed}"
    fi
    discover_analytics_evidence_paths
  fi

  : >"$RESULTS_DIR/consistency.env"
  capture_consistency_snapshot "post_soak"

  stop_server
  start_server
  wait_for_health
  capture_consistency_snapshot "post_restart"
  verify_restart_preserved_counts
  write_summary

  echo "INFO: soak proof artifacts written to $RESULTS_DIR"

  if [[ "$SCENARIO_EXIT_CODE" -eq 99 ]]; then
    exit 99
  fi

  echo "PASS: soak proof completed for ${SCENARIO_NAME}"
}

if [[ "${FLAPJACK_SOAK_PROOF_SKIP_MAIN:-0}" != "1" ]]; then
  trap cleanup EXIT
  trap 'INTERRUPTED_EXIT_CODE=130; cleanup; exit 130' INT
  trap 'INTERRUPTED_EXIT_CODE=143; cleanup; exit 143' TERM

  main "$@"
fi
