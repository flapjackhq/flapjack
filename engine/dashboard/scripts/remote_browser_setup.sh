#!/usr/bin/env bash
#
# remote_browser_setup.sh — the single, re-runnable setup entrypoint that L6's
# remote smoke proof hands to `run_remote.sh --setup-cmd`.
#
# It prepares one freshly-provisioned AL2023 EC2 instance to run
# `npm run test:e2e-ui:smoke` (the caller-owned --measure-cmd) against a real
# flapjack backend and a real Chromium. Setup and measure are SEPARATE SSH
# sessions (`run_remote_workload` calls `run_remote_stage` twice), so anything
# this script starts that the measure stage relies on must survive the setup
# session's logout.
#
# --- Contracts this script depends on and does NOT restate -------------------
#   engine/dashboard/package.json            npm ci / update-server / build owners
#   engine/dashboard/scripts/start-stable-server.sh  backend binary + bind + key owner
#   engine/_dev/s/lib/secret-env.sh          runtime-env-from-secret owner (no-op w/o secret)
#   engine/dashboard/local-instance-config.ts  host/port/admin-key defaults (127.0.0.1,
#                                            backend 7700, dashboard 5177, fj_devtestadminkey000000)
#   engine/rust-toolchain.toml               Rust channel pin rustup must honour
#   engine/loadtest/AWS_SCALE_CEILING_RUNBOOK.md §2  NVMe instance-store identification
#
# --- Two load-bearing decisions, stated here so review can see them ----------
#
# SERVER LIFECYCLE OWNER: this script. Playwright's webServer config only starts
#   the Vite dashboard (`npm run dev`); it never starts the flapjack backend. The
#   backend must therefore be up BEFORE the measure stage and must outlive this
#   SSH session. We start it with `setsid` (new session, detached from the SSH
#   TTY) and then poll /health, rather than foregrounding start-stable-server.sh
#   (it ends in `exec` and would hang setup forever) or backgrounding it with a
#   bare `&` (which dies when the setup SSH session closes).
#
# PLAYWRIGHT DEPENDENCY PATH: native-first, container-fallback. `npx playwright
#   install --with-deps` only knows Debian/Ubuntu package families, so on AL2023
#   the dependency half is EXPECTED to fail. We install the browser download and
#   the Chromium runtime libraries explicitly via `dnf`, then prove the browser
#   can actually launch. A launch failure is the PRE-IDENTIFIED trigger for the
#   containerized arm (official mcr.microsoft.com/playwright image), not a
#   surprise — see prepare_container_fallback().
#
# The script asserts every precondition up front and dies naming the specific
# missing one, so a review never has to read a half-finished build to learn why.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DASHBOARD_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
REPO_ROOT="$(cd "$DASHBOARD_DIR/../.." && pwd)"

# local-instance-config.ts owns these; duplicated here only because a bash setup
# script cannot import a TS module. Keep in sync with that owner.
BACKEND_HEALTH_URL="http://127.0.0.1:7700/health"
BACKEND_HEALTH_TIMEOUT_S=120

# Heavy build artifacts (Rust target/, node_modules, the Chromium download) do
# not fit an AMI-default AL2023 root volume. Below this many GiB free on the
# filesystem holding the repo, we relocate those artifacts onto a mounted NVMe
# instance store (i4i family) before building.
#
# Measured, not assumed — capability probe `l6-probe-e818f6518` (2026-08-02,
# i4i.2xlarge, AL2023 ami-0006118602dfc1c09, us-east-1):
#   /dev/nvme0n1p1  8.0G total, 6.1G available, mounted on /
#   nvme1n1         1.7T, TYPE=disk, no mountpoint, no fstype, no partitions
#   node v18.20.8;  git, cargo and docker all absent
# So the relocation branch below is the expected path on this instance type, and
# the instance store the probe saw is exactly what find_unmounted_instance_store
# is written to match.
MIN_BUILD_GIB_FREE=20
INSTANCE_STORE_MOUNT="/mnt/flapjack-build"

# Playwright 1.58 requires Node >= 18; AL2023's `nodejs` dnf package is 18.x.
MIN_NODE_MAJOR=18

# Chromium's runtime shared libraries on AL2023. `playwright install --with-deps`
# cannot resolve these (no apt), so we name them for dnf explicitly. This is the
# native arm's dependency half; if a later launch still fails, we fall back to
# the container image, which carries its own.
CHROMIUM_DNF_DEPS=(
  nss nspr atk at-spi2-atk at-spi2-core cups-libs libdrm mesa-libgbm
  libXcomposite libXdamage libXext libXfixes libXrandr libxkbcommon
  libXScrnSaver alsa-lib pango cairo gtk3 gdk-pixbuf2 libwayland-client
)

log() { printf '[remote-setup] %s\n' "$*"; }
die() { printf '[remote-setup] FATAL: %s\n' "$*" >&2; exit 1; }

# --- Preconditions -----------------------------------------------------------
#
# Fail here, naming the one missing thing, rather than partway through a build.

assert_source_tree_present() {
  # These are the exact files the reused owners resolve at runtime. Their absence
  # is the signature of the "runner staged tools but not the repo" gap: the
  # runner has no caller source-upload seam, so a source-archive flag (or an
  # equivalent) must place the tree here first. See the L6 source-provisioning
  # gap spec in engine/docs2/4_EVIDENCE/.
  local required=(
    "$REPO_ROOT/engine/Cargo.toml"
    "$DASHBOARD_DIR/package.json"
    "$SCRIPT_DIR/start-stable-server.sh"
    "$REPO_ROOT/engine/_dev/s/lib/secret-env.sh"
  )
  local missing=()
  local path
  for path in "${required[@]}"; do
    [[ -e "$path" ]] || missing+=("$path")
  done
  (( ${#missing[@]} == 0 )) || die \
    "source tree incomplete on this instance; missing: ${missing[*]}. The runner stages tools but not the repo — provision the source at ${REPO_ROOT} before --setup-cmd."
}

assert_node_version() {
  command -v node >/dev/null 2>&1 || die "node is not installed (expected from REMOTE_PACKAGES=nodejs)"
  local major
  major="$(node --version | sed -E 's/^v([0-9]+).*/\1/')"
  [[ "$major" =~ ^[0-9]+$ ]] || die "could not parse node version '$(node --version)'"
  (( major >= MIN_NODE_MAJOR )) || die \
    "node major ${major} is below the required ${MIN_NODE_MAJOR} (Playwright 1.58 minimum)"
  log "node $(node --version) satisfies the >= ${MIN_NODE_MAJOR} requirement"
}

# GiB free on the filesystem holding a given path.
free_gib() {
  df -BG --output=avail "$1" | tail -1 | tr -dc '0-9'
}

# --- Build storage -----------------------------------------------------------
#
# If the repo's filesystem cannot hold the build, mount an NVMe instance store
# and redirect every heavy artifact onto it while preserving the paths that the
# package.json scripts own. Root disk stays small. Follows RUNBOOK §2 device
# identification.

# First whole-disk NVMe device that is unmounted and has no filesystem and no
# partitions — i.e. a raw instance store, never the mounted EBS root.
find_unmounted_instance_store() {
  local name type mount fstype
  while read -r name type mount fstype; do
    [[ "$type" == "disk" ]] || continue
    [[ -z "$mount" ]] || continue
    [[ -z "$fstype" ]] || continue
    # Skip a disk that has child partitions (the root EBS volume does).
    if lsblk -no NAME "$name" | tail -n +2 | grep -q .; then
      continue
    fi
    printf '%s\n' "$name"
    return 0
  done < <(lsblk -dpno NAME,TYPE,MOUNTPOINT,FSTYPE)
  return 1
}

relocate_build_artifacts_to() {
  local mount="$1"
  sudo mkdir -p "$mount"/{cargo-target,cargo-home,rustup-home,npm-cache,playwright,node_modules}
  sudo chown -R "$(id -u):$(id -g)" "$mount"
  export CARGO_TARGET_DIR="$mount/cargo-target"
  # The pinned toolchain is ~1.5 GiB and rustup reads these at install time, so
  # they must be exported before ensure_rust_toolchain runs — see main().
  export CARGO_HOME="$mount/cargo-home"
  export RUSTUP_HOME="$mount/rustup-home"
  export npm_config_cache="$mount/npm-cache"
  export PLAYWRIGHT_BROWSERS_PATH="$mount/playwright"
  export FLAPJACK_NODE_MODULES_RELOCATION_DIR="$mount/node_modules"
  # npm run update-server copies ../target/release/flapjack after cargo builds.
  # Keep that owner path valid even though cargo writes to CARGO_TARGET_DIR.
  rm -rf "$REPO_ROOT/engine/target"
  ln -s "$CARGO_TARGET_DIR" "$REPO_ROOT/engine/target"
  log "build artifacts relocated to ${mount} (cargo target + home, rustup home, npm cache, playwright browsers)"
}

relocate_node_modules_after_npm_ci() {
  [[ -n "${FLAPJACK_NODE_MODULES_RELOCATION_DIR:-}" ]] || return 0
  [[ -d "$DASHBOARD_DIR/node_modules" ]] \
    || die "npm ci finished but ${DASHBOARD_DIR}/node_modules is missing"
  rm -rf "$FLAPJACK_NODE_MODULES_RELOCATION_DIR"
  mv "$DASHBOARD_DIR/node_modules" "$FLAPJACK_NODE_MODULES_RELOCATION_DIR"
  ln -s "$FLAPJACK_NODE_MODULES_RELOCATION_DIR" "$DASHBOARD_DIR/node_modules"
  log "node_modules relocated to ${FLAPJACK_NODE_MODULES_RELOCATION_DIR} after npm ci"
}

provision_build_storage() {
  local free
  free="$(free_gib "$REPO_ROOT")"
  if (( free >= MIN_BUILD_GIB_FREE )); then
    log "root filesystem has ${free} GiB free (>= ${MIN_BUILD_GIB_FREE}); building in place"
    return 0
  fi
  if mountpoint -q "$INSTANCE_STORE_MOUNT"; then
    log "reusing mounted build storage at ${INSTANCE_STORE_MOUNT}"
    relocate_build_artifacts_to "$INSTANCE_STORE_MOUNT"
    return 0
  fi
  log "only ${free} GiB free on the repo filesystem; seeking an NVMe instance store"
  local device
  device="$(find_unmounted_instance_store)" || die \
    "root filesystem has ${free} GiB free, below the ${MIN_BUILD_GIB_FREE} GiB build minimum, and no unmounted NVMe instance store was found (use an i4i instance type or a larger root volume)"
  log "formatting instance store ${device} for the build"
  sudo mkfs.xfs -f "$device" >/dev/null
  sudo mount "$device" "$INSTANCE_STORE_MOUNT" 2>/dev/null || {
    sudo mkdir -p "$INSTANCE_STORE_MOUNT"
    sudo mount "$device" "$INSTANCE_STORE_MOUNT"
  }
  relocate_build_artifacts_to "$INSTANCE_STORE_MOUNT"
}

# --- Rust toolchain ----------------------------------------------------------
#
# `npm run update-server` shells out to `cargo build -p flapjack-server
# --release`, but the runner's REMOTE_PACKAGES is only "nodejs jq nvme-cli" and
# probe l6-probe-e818f6518 reported CARGO_MISSING on a fresh instance. Installing
# the toolchain here is what keeps the missing-cargo case from surfacing as a
# mid-build failure inside a package.json owner.
#
# rustup rather than AL2023's dnf `rust`: engine/rust-toolchain.toml pins channel
# 1.95.0, and rustup is the only installer that honours that pin. openssl-devel
# and pkgconf are for the openssl-sys build script in engine/Cargo.lock; gcc is
# the linker every Rust build needs.
RUST_BUILD_DNF_DEPS=(gcc make perl openssl-devel pkgconf-pkg-config)
RUSTUP_VERSION="1.29.0"
RUSTUP_TARGET="x86_64-unknown-linux-gnu"
RUSTUP_INIT_SHA256="4acc9acc76d5079515b46346a485974457b5a79893cfb01112423c89aeb5aa10"
RUSTUP_INIT_URL="https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${RUSTUP_TARGET}/rustup-init"

install_pinned_rustup() {
  [[ "$(uname -m)" == "x86_64" ]] \
    || die "the pinned rustup target ${RUSTUP_TARGET} requires x86_64, found $(uname -m)"
  command -v sha256sum >/dev/null 2>&1 \
    || die "sha256sum is required to verify the rustup installer"

  local rustup_init
  rustup_init="$(mktemp)" || die "could not create a temporary rustup installer path"
  if ! curl --proto '=https' --tlsv1.2 -fsSLo "$rustup_init" "$RUSTUP_INIT_URL"; then
    rm -f "$rustup_init"
    die "could not download pinned rustup ${RUSTUP_VERSION}"
  fi
  if ! printf '%s  %s\n' "$RUSTUP_INIT_SHA256" "$rustup_init" | sha256sum -c -; then
    rm -f "$rustup_init"
    die "rustup ${RUSTUP_VERSION} installer checksum mismatch"
  fi
  chmod 0700 "$rustup_init"
  if ! "$rustup_init" -y --profile minimal --no-modify-path; then
    rm -f "$rustup_init"
    die "rustup ${RUSTUP_VERSION} installation failed"
  fi
  rm -f "$rustup_init"
}

ensure_rust_toolchain() {
  if command -v cargo >/dev/null 2>&1; then
    log "cargo already present: $(cargo --version)"
    return 0
  fi
  log "installing Rust build dependencies via dnf (${RUST_BUILD_DNF_DEPS[*]})"
  sudo dnf install -y -q "${RUST_BUILD_DNF_DEPS[@]}" \
    || die "could not install the Rust build dependencies (${RUST_BUILD_DNF_DEPS[*]})"
  log "installing checksum-pinned rustup ${RUSTUP_VERSION}; engine/rust-toolchain.toml pins the channel it fetches"
  install_pinned_rustup
  export PATH="${CARGO_HOME:-$HOME/.cargo}/bin:$PATH"
  command -v cargo >/dev/null 2>&1 \
    || die "rustup finished but cargo is still not on PATH (CARGO_HOME=${CARGO_HOME:-$HOME/.cargo})"
  log "cargo ready: $(cargo --version)"
}

# --- Build -------------------------------------------------------------------
#
# Reuse the package.json owners; never inline a cargo/npm command bundle.

build_dashboard_and_server() {
  cd "$DASHBOARD_DIR"
  log "installing dashboard dependencies (npm ci)"
  npm ci
  relocate_node_modules_after_npm_ci
  log "building the release flapjack-server binary (npm run update-server)"
  npm run update-server
  log "building the dashboard (npm run build)"
  npm run build
}

# --- Backend -----------------------------------------------------------------
#
# Detach the backend so it outlives this SSH session (see header). Reuse
# start-stable-server.sh: it owns the binary path, the 127.0.0.1:7700 bind, the
# default dev admin key, and the runtime-env-from-secret load.

start_detached_backend() {
  local server_log="/tmp/flapjack-backend.log"
  cd "$DASHBOARD_DIR"
  log "starting the flapjack backend detached (setsid); log at ${server_log}"
  setsid bash "$SCRIPT_DIR/start-stable-server.sh" >"$server_log" 2>&1 < /dev/null &
  disown || true

  local deadline=$(( SECONDS + BACKEND_HEALTH_TIMEOUT_S ))
  while (( SECONDS < deadline )); do
    if curl -sf "$BACKEND_HEALTH_URL" >/dev/null 2>&1; then
      log "backend healthy at ${BACKEND_HEALTH_URL}"
      return 0
    fi
    sleep 2
  done
  log "backend did not become healthy within ${BACKEND_HEALTH_TIMEOUT_S}s; last log lines:"
  tail -20 "$server_log" >&2 || true
  die "flapjack backend never reported healthy at ${BACKEND_HEALTH_URL}"
}

# --- Playwright browser ------------------------------------------------------

# Native arm: dnf-installed Chromium libraries + the Playwright browser download,
# then a real launch to prove it works. Returns non-zero (not fatal) when the
# native browser cannot launch, which is the trigger for the container arm.
install_native_chromium() {
  cd "$DASHBOARD_DIR"
  log "installing Chromium runtime libraries via dnf (native arm)"
  sudo dnf install -y -q "${CHROMIUM_DNF_DEPS[@]}" || {
    log "dnf could not install the full Chromium dependency set"
    return 1
  }
  log "downloading the Playwright Chromium browser (no --with-deps: AL2023 is not apt-based)"
  npx --yes playwright install chromium || {
    log "playwright chromium download failed"
    return 1
  }
  log "verifying Chromium can actually launch (headless launch + close)"
  node -e "const{chromium}=require('@playwright/test');chromium.launch().then(b=>b.close()).then(()=>process.exit(0)).catch(e=>{console.error(e.message);process.exit(1)})" || {
    log "Chromium is installed but cannot launch on this AL2023 host"
    return 1
  }
  log "native Chromium launches successfully"
}

# Container arm: install and start Docker (absent from REMOTE_PACKAGES), pull the
# official Playwright image, and leave a marker the measure stage / verify lane
# reads to switch --measure-cmd into the container. Timing bias note: a
# containerized browser is a valid joined proof of the app, but its timing
# differs — any timing-sensitive smoke failure must be re-checked before being
# called a product defect.
prepare_container_fallback() {
  local image="mcr.microsoft.com/playwright:v1.58.2-noble"
  export FLAPJACK_PLAYWRIGHT_CONTAINER_IMAGE="$image"
  log "native Chromium unavailable; preparing containerized fallback (${image})"
  command -v docker >/dev/null 2>&1 || sudo dnf install -y -q docker || \
    die "could not install Docker for the containerized Playwright fallback"
  sudo systemctl start docker || die "could not start the Docker daemon"
  sudo docker pull "$image" >/dev/null || die "could not pull ${image}"
  # Land the marker in fetched evidence when the runner exported an evidence dir,
  # so the verify lane's disposition can cite it; otherwise /tmp.
  printf '%s\n' "$image" >"${FLAPJACK_EVIDENCE_DIR:-/tmp}/playwright_container_image.txt"
  log "containerized Playwright is ready; measure stage must run the smoke suite inside ${image}"
}

install_browser() {
  if install_native_chromium; then
    return 0
  fi
  prepare_container_fallback
}

# --- Handoff to the measure stage --------------------------------------------
#
# The measure stage is a SEPARATE SSH session (run_remote_workload calls
# run_remote_stage twice), so nothing this script exports reaches it. Anything
# the smoke run needs that is not an AL2023 default has to be written down.
# This script stays the single owner of the values; the measure command only
# sources the file.
MEASURE_ENV_FILE="$HOME/.flapjack-remote-env"

write_measure_environment() {
  local line
  : >"$MEASURE_ENV_FILE"
  # Only relocated / non-default values matter. node and npm are already on the
  # default PATH, and the measure stage does not build, so cargo is not needed.
  for line in PLAYWRIGHT_BROWSERS_PATH npm_config_cache FLAPJACK_PLAYWRIGHT_CONTAINER_IMAGE; do
    if [[ -n "${!line:-}" ]]; then
      printf 'export %s=%q\n' "$line" "${!line}" >>"$MEASURE_ENV_FILE"
    fi
  done
  log "measure-stage environment written to ${MEASURE_ENV_FILE}:"
  cat "$MEASURE_ENV_FILE"
}

# --- Main --------------------------------------------------------------------

main() {
  log "repo root: ${REPO_ROOT}"
  assert_source_tree_present
  assert_node_version
  # Storage first: it exports CARGO_HOME/RUSTUP_HOME, which rustup reads.
  provision_build_storage
  ensure_rust_toolchain
  build_dashboard_and_server
  start_detached_backend
  install_browser
  write_measure_environment
  log "setup complete; measure stage must source ${MEASURE_ENV_FILE} before npm run test:e2e-ui:smoke"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
