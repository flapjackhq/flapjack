#!/usr/bin/env bash
#
# Live proof that the published operations OpenAPI schemas in docs2/openapi.json
# describe what a real flapjack server actually returns on its operational reads.
#
# The proof exercises three shipped topologies against the SAME published
# contract:
#   * standalone  -- `cargo run` of the committed HEAD binary, no S3, no peers
#   * configured-S3 -- engine/examples/s3-snapshot/docker-compose.yml + MinIO
#   * HA          -- engine/examples/replication/docker-compose.yml, two nodes
#
# Every assertion below reads a body collected from a running server; no fixture
# stands in for a live response. The harness is split into two strict modes so
# the expensive build/pull work is separated from the verified prepared run:
#
#   --prepare        build the committed HEAD once, pull external MinIO images,
#                    build one reusable engine image, and record metadata that
#                    pins exactly what was prepared.
#   --run-prepared   refuse to compile/pull/build, prove it is running the
#                    prepared HEAD/image, start each topology, collect real
#                    operation responses, and validate them against the
#                    published contract with jq and Schemathesis.

set -euo pipefail

# ── Paths derived from the script location (never worktree-absolute) ──────────
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_DIR="$(cd "$ENGINE_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$SCRIPT_DIR/common/wait_for_flapjack.sh"

readonly OPENAPI_DOC="$ENGINE_DIR/docs2/openapi.json"
# flapjack-http/build.rs declares `rerun-if-changed=../dashboard/dist/assets`.
# The dashboard build produces that directory; a dev checkout that has not built
# the dashboard leaves it absent, which Cargo treats as a perpetually-stale
# build input, so `cargo run` recompiles flapjack-http/flapjack-server on every
# invocation. That would make the standalone no-op guarantee below unsatisfiable.
# The directory is gitignored (engine/dashboard/.gitignore), so materializing an
# empty marker before the host build removes the false staleness without dirtying
# the tree or changing any compiled behavior.
readonly DASHBOARD_ASSETS_MARKER="$ENGINE_DIR/dashboard/dist/assets"
readonly RESULTS_DIR="$ENGINE_DIR/tests/results/ops_contract_live"
readonly SUMMARY_FILE="$ENGINE_DIR/tests/results/ops_contract_live.txt"
readonly METADATA_FILE="$RESULTS_DIR/metadata.json"
readonly RUN_LOG="$RESULTS_DIR/run_log.txt"

# Reusable engine image identity. The label lets --run-prepared prove the local
# image was built for exactly the prepared HEAD without rebuilding.
readonly IMAGE_LABEL_KEY="org.flapjack.ops_contract_live.head"
readonly MINIO_SERVER_IMAGE="minio/minio:latest"
readonly MINIO_MC_IMAGE="minio/mc:latest"

# Contract facts asserted below (single source: the shipped handlers/schemas).
readonly SNAPSHOT_BUCKET="flapjack-snapshots"
readonly SCHEMATHESIS_PIN="schemathesis==4.1.4"
readonly OPS_PATH_REGEX='^/(health|internal/status|internal/cluster/status|internal/snapshots/capability)$'

# Unique-per-process Compose project names so concurrent hosts never collide and
# cleanup only ever targets projects this exact process created.
readonly S3_PROJECT="ops_contract_live_s3_$$"
readonly HA_PROJECT="ops_contract_live_ha_$$"

# ── Mutable run state (populated during --run-prepared) ──────────────────────
STANDALONE_PID=""
STANDALONE_DATA_DIR=""
STANDALONE_BASE_URL=""
S3_BASE_URL=""
HA_BASE_URL=""
S3_OVERRIDE=""
HA_OVERRIDE=""
S3_STARTED=0
HA_STARTED=0
CHECKS_RUN=0
CHECKS_FAILED=0
RUN_SUCCEEDED=0
INTERRUPTED_EXIT_CODE=0

# ── Small logging helpers ────────────────────────────────────────────────────
log() { printf '%s\n' "$*"; }

# Durable evidence line: goes to stdout (which the canonical run tees into
# ops_contract_live.txt) and to the harness-owned run log under RESULTS_DIR.
note() {
  local line="$1"
  log "$line"
  if [ -d "$RESULTS_DIR" ]; then
    printf '[%s] %s\n' "$(date -u +%FT%TZ)" "$line" >>"$RUN_LOG" 2>/dev/null || true
  fi
}

die() {
  note "ERROR: $1"
  exit 1
}

pass() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  note "  [PASS] $1"
}

fail() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  note "  [FAIL] $1"
  if [ -n "${2:-}" ]; then
    note "         $2"
  fi
}

usage() {
  cat <<'EOF'
Usage:
  ops_contract_live.sh --prepare        Build/pull all reusable inputs and record
                                        prepared metadata for the committed HEAD.
  ops_contract_live.sh --run-prepared   Refuse to build/pull, prove the prepared
                                        state, start the topologies, collect real
                                        operation responses, and validate them
                                        against docs2/openapi.json.
  ops_contract_live.sh --help           Show this message.

Exactly one mode is required. Run --prepare first (after committing a clean
tree), then --run-prepared.
EOF
}

# ── Shared preflight ─────────────────────────────────────────────────────────

require_tools() {
  local missing=0 tool
  for tool in bash cargo curl docker git jq timeout uvx; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      log "ERROR: required tool not found: $tool" >&2
      missing=1
    fi
  done
  if ! docker compose version >/dev/null 2>&1; then
    log "ERROR: 'docker compose' (Compose v2+) is required" >&2
    missing=1
  fi
  [ "$missing" -eq 0 ] || exit 1
  [ -x "$WAIT_FOR_FLAPJACK" ] || die "missing readiness helper: $WAIT_FOR_FLAPJACK"
}

# Compose must support the `!override` YAML tag so example port lists are
# replaced (not appended to); plain sequence merge would retain fixed ports.
require_compose_override_support() {
  local probe_base probe_over out
  probe_base="$(mktemp)"
  probe_over="$(mktemp)"
  printf 'services:\n  probe:\n    image: %s\n    ports:\n      - "5999:5999"\n' \
    "$MINIO_MC_IMAGE" >"$probe_base"
  printf 'services:\n  probe:\n    ports: !override\n      - "127.0.0.1:0:5999"\n' >"$probe_over"
  out="$(docker compose -f "$probe_base" -f "$probe_over" config --format json 2>/dev/null || true)"
  rm -f "$probe_base" "$probe_over"
  if ! printf '%s' "$out" \
    | jq -e '.services.probe.ports | length == 1 and (.[0].published == "0")' >/dev/null 2>&1; then
    die "docker compose does not honor the '!override' tag required by this harness"
  fi
}

# Only the canonical summary and files below the detailed-results directory may
# differ from committed HEAD. Match parsed paths, not porcelain display lines:
# rename entries contain two paths, and Git may quote unusual filenames.
is_harness_owned_result_path() {
  local path="$1"
  [ "$path" = "engine/tests/results/ops_contract_live.txt" ] \
    || [[ "$path" == engine/tests/results/ops_contract_live/* ]]
}

list_dirty_paths() {
  git -C "$REPO_DIR" diff --no-renames --name-only -z \
    && git -C "$REPO_DIR" diff --cached --no-renames --name-only -z \
    && git -C "$REPO_DIR" ls-files --others --exclude-standard -z
}

# Reject anything that could change the engine/Docker build. Only the two
# harness-owned result paths are allowed to be dirty.
assert_clean_build_tree() {
  local path dirty_paths_file
  local -a unexpected_paths=()
  dirty_paths_file="$(mktemp)"
  if ! list_dirty_paths >"$dirty_paths_file"; then
    rm -f "$dirty_paths_file"
    die "could not determine whether the build tree is clean"
  fi
  while IFS= read -r -d '' path; do
    if ! is_harness_owned_result_path "$path"; then
      unexpected_paths+=("$path")
    fi
  done <"$dirty_paths_file"
  rm -f "$dirty_paths_file"

  if [ "${#unexpected_paths[@]}" -ne 0 ]; then
    note "unexpected working-tree changes that can affect the build:"
    for path in "${unexpected_paths[@]}"; do
      note "  $path"
    done
    die "refusing to proceed with a dirty build tree"
  fi
}

# ── --prepare ────────────────────────────────────────────────────────────────

image_tag_for_head() {
  printf 'flapjack-ops-contract-live:%s\n' "${1:0:12}"
}

cmd_prepare() {
  require_tools
  require_compose_override_support
  assert_clean_build_tree

  mkdir -p "$RESULTS_DIR"
  : >"$RUN_LOG"

  local head image_tag
  head="$(git -C "$REPO_DIR" rev-parse HEAD)"
  image_tag="$(image_tag_for_head "$head")"
  note "prepare: committed HEAD $head"

  # Materialize the dashboard-assets freshness marker BEFORE building so the host
  # build fingerprint captures the existing directory; without this the standalone
  # cargo run in --run-prepared would always recompile (see the const comment).
  if [ ! -d "$DASHBOARD_ASSETS_MARKER" ]; then
    mkdir -p "$DASHBOARD_ASSETS_MARKER"
    note "prepare: created gitignored dashboard assets marker at $DASHBOARD_ASSETS_MARKER"
  fi

  note "prepare: building standalone flapjack binary (cargo build)"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server --bin flapjack \
    >"$RESULTS_DIR/prepare_cargo_build.log" 2>&1); then
    tail -40 "$RESULTS_DIR/prepare_cargo_build.log" >&2 || true
    die "cargo build -p flapjack-server --bin flapjack failed"
  fi

  note "prepare: pulling external MinIO images"
  docker pull "$MINIO_SERVER_IMAGE" >>"$RESULTS_DIR/prepare_pull.log" 2>&1 \
    || die "failed to pull $MINIO_SERVER_IMAGE"
  docker pull "$MINIO_MC_IMAGE" >>"$RESULTS_DIR/prepare_pull.log" 2>&1 \
    || die "failed to pull $MINIO_MC_IMAGE"

  note "prepare: building reusable engine image $image_tag"
  if ! docker build -f "$ENGINE_DIR/Dockerfile" \
    -t "$image_tag" \
    --label "$IMAGE_LABEL_KEY=$head" \
    "$REPO_DIR" >"$RESULTS_DIR/prepare_docker_build.log" 2>&1; then
    tail -40 "$RESULTS_DIR/prepare_docker_build.log" >&2 || true
    die "docker build of the reusable engine image failed"
  fi

  note "prepare: warming Schemathesis ($SCHEMATHESIS_PIN) into the uv cache"
  NO_COLOR=1 uvx --from "$SCHEMATHESIS_PIN" schemathesis --version \
    >"$RESULTS_DIR/prepare_schemathesis.log" 2>&1 \
    || die "failed to warm Schemathesis $SCHEMATHESIS_PIN via uvx"

  write_prepared_metadata "$head" "$image_tag"
  note "prepare: complete; metadata written to $METADATA_FILE"
}

write_prepared_metadata() {
  local head="$1" image_tag="$2"
  local image_id image_label minio_server_id minio_mc_id
  image_id="$(docker image inspect --format '{{.Id}}' "$image_tag")" \
    || die "prepared engine image $image_tag not found after build"
  image_label="$(docker image inspect \
    --format "{{index .Config.Labels \"$IMAGE_LABEL_KEY\"}}" "$image_tag")"
  [ "$image_label" = "$head" ] \
    || die "prepared image label ($image_label) does not match HEAD ($head)"
  minio_server_id="$(docker image inspect --format '{{.Id}}' "$MINIO_SERVER_IMAGE")" \
    || die "MinIO server image not present after pull"
  minio_mc_id="$(docker image inspect --format '{{.Id}}' "$MINIO_MC_IMAGE")" \
    || die "MinIO mc image not present after pull"

  jq -n \
    --arg prepared_head "$head" \
    --arg prepared_at "$(date -u +%FT%TZ)" \
    --arg image_tag "$image_tag" \
    --arg image_id "$image_id" \
    --arg image_source_label "$image_label" \
    --arg minio_server_image "$MINIO_SERVER_IMAGE" \
    --arg minio_server_image_id "$minio_server_id" \
    --arg minio_mc_image "$MINIO_MC_IMAGE" \
    --arg minio_mc_image_id "$minio_mc_id" \
    '{prepared_head:$prepared_head, prepared_at:$prepared_at,
      image_tag:$image_tag, image_id:$image_id,
      image_source_label:$image_source_label,
      minio_server_image:$minio_server_image,
      minio_server_image_id:$minio_server_image_id,
      minio_mc_image:$minio_mc_image,
      minio_mc_image_id:$minio_mc_image_id}' >"$METADATA_FILE"
}

meta_field() {
  jq -r --arg k "$1" '.[$k] // ""' "$METADATA_FILE"
}

# ── --run-prepared preflight ────────────────────────────────────────────────

preflight_run_prepared() {
  require_tools
  require_compose_override_support

  [ -f "$METADATA_FILE" ] || die "no prepared metadata at $METADATA_FILE; run --prepare first"
  # The standalone cargo run stays a no-op only while this marker exists (see the
  # const comment). If it was removed, re-preparation is required so the marker's
  # mtime precedes the built artifacts; do NOT recreate it here (a fresh mtime
  # would itself invalidate the prepared build).
  [ -d "$DASHBOARD_ASSETS_MARKER" ] \
    || die "dashboard assets freshness marker missing at $DASHBOARD_ASSETS_MARKER; re-run --prepare"
  [ -x "$WAIT_FOR_FLAPJACK" ] || die "missing executable readiness helper: $WAIT_FOR_FLAPJACK"
  [ -f "$OPENAPI_DOC" ] || die "missing published contract: $OPENAPI_DOC"

  local prepared_head current_head image_tag image_id image_label
  prepared_head="$(meta_field prepared_head)"
  image_tag="$(meta_field image_tag)"
  image_id="$(meta_field image_id)"

  current_head="$(git -C "$REPO_DIR" rev-parse HEAD)"
  [ -n "$prepared_head" ] || die "prepared metadata is missing prepared_head"
  [ "$current_head" = "$prepared_head" ] \
    || die "HEAD changed since prepare (prepared $prepared_head, now $current_head); re-run --prepare"

  # Dirty build inputs (outside the two allowed harness-owned result paths).
  assert_clean_build_tree

  # Prepared engine image must exist with the exact prepared ID and HEAD label.
  [ -n "$image_id" ] || die "prepared metadata is missing image_id"
  local live_image_id
  live_image_id="$(docker image inspect --format '{{.Id}}' "$image_tag" 2>/dev/null || true)"
  [ -n "$live_image_id" ] || die "prepared engine image $image_tag is absent; re-run --prepare"
  [ "$live_image_id" = "$image_id" ] \
    || die "prepared engine image ID drifted (prepared $image_id, live $live_image_id)"
  image_label="$(docker image inspect \
    --format "{{index .Config.Labels \"$IMAGE_LABEL_KEY\"}}" "$image_tag" 2>/dev/null || true)"
  [ "$image_label" = "$prepared_head" ] \
    || die "prepared image source label ($image_label) != prepared HEAD ($prepared_head)"

  # External MinIO images must be present locally so Compose never pulls.
  assert_prepared_image_present "$(meta_field minio_server_image)" "$(meta_field minio_server_image_id)"
  assert_prepared_image_present "$(meta_field minio_mc_image)" "$(meta_field minio_mc_image_id)"

  note "run-prepared: preflight ok for HEAD $current_head using image $image_tag"
}

assert_prepared_image_present() {
  local image_ref="$1" expected_id="$2" live_id
  [ -n "$image_ref" ] || die "prepared metadata is missing an external image reference"
  [ -n "$expected_id" ] || die "prepared metadata is missing an external image ID for $image_ref"
  live_id="$(docker image inspect --format '{{.Id}}' "$image_ref" 2>/dev/null || true)"
  [ -n "$live_id" ] || die "prepared external image $image_ref absent; re-run --prepare (no pulls in run-prepared)"
  [ "$live_id" = "$expected_id" ] \
    || die "external image $image_ref drifted (prepared $expected_id, live $live_id)"
}

# ── Topology startup ─────────────────────────────────────────────────────────

start_standalone() {
  local server_log="$RESULTS_DIR/standalone_server.log"
  STANDALONE_DATA_DIR="$(mktemp -d)"
  note "standalone: starting committed-HEAD binary via cargo run (no S3, no peers)"

  ( cd "$ENGINE_DIR" && \
    env -u FLAPJACK_S3_BUCKET -u FLAPJACK_S3_ENDPOINT -u FLAPJACK_S3_REGION \
      -u FLAPJACK_PEERS -u FLAPJACK_NODE_ID \
      FLAPJACK_NO_AUTH=1 \
      FLAPJACK_DATA_DIR="$STANDALONE_DATA_DIR" \
      cargo run -p flapjack-server --bin flapjack -- --auto-port ) \
    >"$server_log" 2>&1 &
  STANDALONE_PID=$!

  "$WAIT_FOR_FLAPJACK" \
    --pid "$STANDALONE_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$server_log" \
    --retries 120 \
    --interval-seconds 0.5 \
    || die "standalone server did not become ready"

  # Prepared runs must not trigger any compilation.
  if grep -Eq '^[[:space:]]*Compiling ' "$server_log"; then
    die "cargo run compiled during --run-prepared; the prepared build is stale"
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9]*\).*/\1/p' "$server_log" | head -1)"
  [ -n "$port" ] || die "standalone server ready but no --auto-port found in $server_log"
  STANDALONE_BASE_URL="http://127.0.0.1:${port}"
  note "standalone: STANDALONE_BASE_URL=$STANDALONE_BASE_URL"
}

# Assert the resolved Compose config exposes only ephemeral loopback host ports.
assert_only_ephemeral_loopback_ports() {
  local project="$1" base="$2" override="$3"
  local config bad
  config="$(docker compose -p "$project" -f "$base" -f "$override" config --format json 2>/dev/null)" \
    || die "docker compose config failed for project $project"
  printf '%s' "$config" >"$RESULTS_DIR/${project}_resolved_config.json"
  bad="$(printf '%s' "$config" | jq -r '
    [ .services // {} | to_entries[] | .value.ports // [] | .[]
      | select((.published // "0") != "0" or (.host_ip // "") != "127.0.0.1") ]
    | length')"
  [ "$bad" = "0" ] \
    || die "resolved Compose config for $project retains fixed or non-loopback host bindings"
}

# Wait for /health on a Compose-published loopback port to return 200.
wait_for_http_health() {
  local base_url="$1" label="$2" attempt
  for attempt in $(seq 1 120); do
    if [ "$(curl -sS -o /dev/null -w '%{http_code}' "${base_url}/health" 2>/dev/null || true)" = "200" ]; then
      return 0
    fi
    sleep 1
  done
  die "$label did not report /health 200 at $base_url"
}

compose_published_port() {
  local project="$1" base="$2" override="$3" service="$4" mapping
  mapping="$(docker compose -p "$project" -f "$base" -f "$override" port "$service" 7700 2>/dev/null || true)"
  # docker compose port prints host:port; take the port field.
  printf '%s\n' "${mapping##*:}"
}

start_s3_branch() {
  local base="$ENGINE_DIR/examples/s3-snapshot/docker-compose.yml"
  local image_tag; image_tag="$(meta_field image_tag)"
  S3_OVERRIDE="$(mktemp)"
  cat >"$S3_OVERRIDE" <<EOF
services:
  minio:
    ports: !override
      - "127.0.0.1:0:9000"
      - "127.0.0.1:0:9001"
  flapjack:
    image: "$image_tag"
    environment:
      FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND: "1"
    ports: !override
      - "127.0.0.1:0:7700"
EOF
  assert_only_ephemeral_loopback_ports "$S3_PROJECT" "$base" "$S3_OVERRIDE"

  note "configured-S3: starting Compose project $S3_PROJECT (--no-build --pull never)"
  S3_STARTED=1
  docker compose -p "$S3_PROJECT" -f "$base" -f "$S3_OVERRIDE" \
    up -d --no-build --pull never >"$RESULTS_DIR/s3_up.log" 2>&1 \
    || die "configured-S3 Compose up failed"

  local port; port="$(compose_published_port "$S3_PROJECT" "$base" "$S3_OVERRIDE" flapjack)"
  [ -n "$port" ] || die "could not discover configured-S3 flapjack port"
  S3_BASE_URL="http://127.0.0.1:${port}"
  wait_for_http_health "$S3_BASE_URL" "configured-S3 flapjack"
  note "configured-S3: S3_BASE_URL=$S3_BASE_URL"
}

start_ha_branch() {
  local base="$ENGINE_DIR/examples/replication/docker-compose.yml"
  local image_tag; image_tag="$(meta_field image_tag)"
  HA_OVERRIDE="$(mktemp)"
  cat >"$HA_OVERRIDE" <<EOF
services:
  node-a:
    image: "$image_tag"
    environment:
      FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND: "1"
      FLAPJACK_STARTUP_CATCHUP_STRICT: "0"
    ports: !override
      - "127.0.0.1:0:7700"
  node-b:
    image: "$image_tag"
    environment:
      FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND: "1"
      FLAPJACK_STARTUP_CATCHUP_STRICT: "0"
    ports: !override
      - "127.0.0.1:0:7700"
EOF
  assert_only_ephemeral_loopback_ports "$HA_PROJECT" "$base" "$HA_OVERRIDE"

  note "HA: starting Compose project $HA_PROJECT (--no-build --pull never)"
  HA_STARTED=1
  docker compose -p "$HA_PROJECT" -f "$base" -f "$HA_OVERRIDE" \
    up -d --no-build --pull never >"$RESULTS_DIR/ha_up.log" 2>&1 \
    || die "HA Compose up failed"

  local port_a port_b
  port_a="$(compose_published_port "$HA_PROJECT" "$base" "$HA_OVERRIDE" node-a)"
  port_b="$(compose_published_port "$HA_PROJECT" "$base" "$HA_OVERRIDE" node-b)"
  [ -n "$port_a" ] || die "could not discover HA node-a port"
  [ -n "$port_b" ] || die "could not discover HA node-b port"
  HA_BASE_URL="http://127.0.0.1:${port_a}"
  local node_b_url="http://127.0.0.1:${port_b}"
  wait_for_http_health "$HA_BASE_URL" "HA node-a"
  wait_for_http_health "$node_b_url" "HA node-b"
  note "HA: HA_BASE_URL=$HA_BASE_URL node-b=$node_b_url"
}

# ── Response collection ──────────────────────────────────────────────────────

fetch_operation() {
  local base_url="$1" path="$2" out_file="$3" code
  code="$(curl -sS -o "$out_file" -w '%{http_code}' "${base_url}${path}" 2>/dev/null || true)"
  [ "$code" = "200" ] || { note "GET ${path} returned HTTP ${code:-<none>}"; return 1; }
  jq -e . "$out_file" >/dev/null 2>&1 || { note "GET ${path} body was not valid JSON"; return 1; }
  return 0
}

# Collect all four operation reads for one running mode into a mode directory.
collect_operations() {
  local mode="$1" base_url="$2" collected=0 path name
  local mode_dir="$RESULTS_DIR/$mode"
  mkdir -p "$mode_dir"
  for path in /health /internal/status /internal/cluster/status /internal/snapshots/capability; do
    name="$(printf '%s' "$path" | tr '/' '_' | sed 's/^_//')"
    if fetch_operation "$base_url" "$path" "$mode_dir/${name}.json"; then
      collected=$((collected + 1))
    else
      die "$mode: failed to collect $path from $base_url"
    fi
  done
  [ "$collected" -eq 4 ] || die "$mode: expected 4 operations, collected $collected"
  note "$mode: collected $collected operations from $base_url"
}

# ── jq assertions against the live bodies ────────────────────────────────────

assert_jq() {
  local label="$1" file="$2" filter="$3"
  if jq -e "$filter" "$file" >/dev/null 2>&1; then
    pass "$label"
  else
    fail "$label" "filter <$filter> failed against $(jq -c . "$file" 2>/dev/null || cat "$file")"
  fi
}

readonly HEALTH_KEYS_JSON='["status","version","build","uptime_secs","capabilities","active_writers","max_concurrent_writers","facet_cache_entries","facet_cache_cap","heap_allocated_mb","system_limit_mb","pressure_level","allocator","tenants_loaded"]'

assert_standalone_contract() {
  local d="$RESULTS_DIR/standalone"
  assert_jq "standalone snapshot capability is not_configured with null bucket" \
    "$d/internal_snapshots_capability.json" \
    '.state == "not_configured" and .bucket == null'
  assert_jq "standalone /health exposes the exact top-level key allowlist" \
    "$d/health.json" \
    "(keys | sort) == ($HEALTH_KEYS_JSON | sort)"
  assert_jq "standalone /internal/status reports fallback replication values" \
    "$d/internal_status.json" \
    '.node_id == "unknown" and .replication_enabled == false and .peer_count == 0 and (.ssl_renewal == null)'
  assert_jq "standalone /internal/cluster/status is the standalone branch" \
    "$d/internal_cluster_status.json" \
    '.replication_enabled == false and .peers == []'
}

assert_s3_contract() {
  local d="$RESULTS_DIR/configured_s3"
  assert_jq "configured-S3 snapshot capability is configured_unverified for the S3 bucket" \
    "$d/internal_snapshots_capability.json" \
    ".state == \"configured_unverified\" and .bucket == \"$SNAPSHOT_BUCKET\""
}

assert_ha_contract() {
  local d="$RESULTS_DIR/ha"
  assert_jq "HA /internal/cluster/status is the HA branch with consistent peer counts" \
    "$d/internal_cluster_status.json" \
    '.replication_enabled == true
       and (.peers_total | type == "number")
       and (.peers_healthy | type == "number")
       and .peers_healthy >= 0
       and .peers_healthy <= .peers_total
       and .peers_total == (.peers | length)
       and (.peers | length) >= 1'
}

wait_for_ha_peer() {
  local cluster_status_file="$RESULTS_DIR/ha/internal_cluster_status.json"
  local attempt
  mkdir -p "$(dirname "$cluster_status_file")"
  for ((attempt = 1; attempt <= 30; attempt++)); do
    if fetch_operation "$HA_BASE_URL" /internal/cluster/status "$cluster_status_file" \
      && jq -e '(.peers | length) >= 1' "$cluster_status_file" >/dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  die "HA node did not report a peer within 30 attempts"
}

# ── Schemathesis conformance against every live base URL ─────────────────────

run_schemathesis() {
  local mode="$1" base_url="$2"
  local out="$RESULTS_DIR/${mode}_schemathesis.txt"
  note "$mode: running Schemathesis conformance against $base_url"
  if NO_COLOR=1 uvx --from "$SCHEMATHESIS_PIN" schemathesis run "$OPENAPI_DOC" \
    --url "$base_url" \
    --include-path-regex "$OPS_PATH_REGEX" \
    --phases fuzzing \
    --mode positive \
    --max-examples 1 \
    --checks status_code_conformance,content_type_conformance,response_schema_conformance \
    >"$out" 2>&1; then
    pass "$mode: Schemathesis conformance passed"
  else
    fail "$mode: Schemathesis conformance failed" "see $out"
  fi
}

# ── Cleanup: preserve evidence, then tear down only what we created ──────────

cleanup() {
  local script_exit_code=$?
  # Capture container logs into durable evidence BEFORE any teardown.
  if [ "$S3_STARTED" -eq 1 ]; then
    docker compose -p "$S3_PROJECT" logs --no-color >"$RESULTS_DIR/s3_compose.log" 2>&1 || true
  fi
  if [ "$HA_STARTED" -eq 1 ]; then
    docker compose -p "$HA_PROJECT" logs --no-color >"$RESULTS_DIR/ha_compose.log" 2>&1 || true
  fi

  if [ "$RUN_SUCCEEDED" -eq 1 ] && [ "$CHECKS_FAILED" -eq 0 ] && [ "$script_exit_code" -eq 0 ]; then
    note "cleanup: run succeeded ($CHECKS_RUN checks, 0 failures)"
  else
    note "cleanup: run did NOT fully succeed (exit=$script_exit_code, interrupted=$INTERRUPTED_EXIT_CODE, checks=$CHECKS_RUN, failed=$CHECKS_FAILED); evidence preserved under $RESULTS_DIR"
  fi

  # Stop only the exact local PID this script started.
  if [ -n "$STANDALONE_PID" ] && kill -0 "$STANDALONE_PID" 2>/dev/null; then
    kill "$STANDALONE_PID" 2>/dev/null || true
    wait "$STANDALONE_PID" 2>/dev/null || true
  fi
  # Tear down only the exact unique Compose projects this script created.
  if [ "$S3_STARTED" -eq 1 ]; then
    docker compose -p "$S3_PROJECT" down -v --remove-orphans >>"$RESULTS_DIR/s3_down.log" 2>&1 || true
  fi
  if [ "$HA_STARTED" -eq 1 ]; then
    docker compose -p "$HA_PROJECT" down -v --remove-orphans >>"$RESULTS_DIR/ha_down.log" 2>&1 || true
  fi

  [ -n "$S3_OVERRIDE" ] && rm -f "$S3_OVERRIDE"
  [ -n "$HA_OVERRIDE" ] && rm -f "$HA_OVERRIDE"
  if [ -n "$STANDALONE_DATA_DIR" ] && [ -d "$STANDALONE_DATA_DIR" ]; then
    rm -rf "$STANDALONE_DATA_DIR"
  fi
}

handle_signal() {
  local exit_code="$1" signal_name="$2"
  INTERRUPTED_EXIT_CODE="$exit_code"
  note "INTERRUPTED: received $signal_name; success is forbidden"
  trap - INT TERM
  exit "$exit_code"
}

# ── --run-prepared ───────────────────────────────────────────────────────────

cmd_run_prepared() {
  mkdir -p "$RESULTS_DIR"
  : >"$RUN_LOG"
  trap cleanup EXIT
  trap 'handle_signal 130 INT' INT
  trap 'handle_signal 143 TERM' TERM
  note "run-prepared: starting live operations-contract proof"
  note "run-prepared: canonical summary target is $SUMMARY_FILE"

  preflight_run_prepared

  # Standalone topology.
  start_standalone
  export STANDALONE_BASE_URL
  collect_operations standalone "$STANDALONE_BASE_URL"
  assert_standalone_contract
  run_schemathesis standalone "$STANDALONE_BASE_URL"

  # Configured-S3 topology.
  start_s3_branch
  export S3_BASE_URL
  collect_operations configured_s3 "$S3_BASE_URL"
  assert_s3_contract
  run_schemathesis configured_s3 "$S3_BASE_URL"

  # HA topology.
  start_ha_branch
  export HA_BASE_URL
  # Peer rows can lag node startup slightly; retry the cluster read until seen.
  wait_for_ha_peer
  collect_operations ha "$HA_BASE_URL"
  assert_ha_contract
  run_schemathesis ha "$HA_BASE_URL"

  note "run-prepared: $CHECKS_RUN checks run, $CHECKS_FAILED failed"
  if [ "$CHECKS_FAILED" -ne 0 ]; then
    die "operations-contract proof failed: $CHECKS_FAILED check(s) did not pass"
  fi
  RUN_SUCCEEDED=1
  note "SUCCESS: live operations OpenAPI contract validated across standalone, configured-S3, and HA"
}

# ── Entry point ──────────────────────────────────────────────────────────────

main() {
  if [ "$#" -ne 1 ]; then
    usage >&2
    exit 1
  fi
  case "$1" in
    --prepare) cmd_prepare ;;
    --run-prepared) cmd_run_prepared ;;
    --help|-h) usage ;;
    *)
      log "ERROR: unknown argument: $1" >&2
      usage >&2
      exit 1
      ;;
  esac
}

main "$@"
