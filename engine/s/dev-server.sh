#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
source "$SCRIPT_DIR/lib/ui.sh"
source "$SCRIPT_DIR/lib/secret-env.sh"

REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
DATA_DIR="$REPO_ROOT/engine/_dev/dev-data"

# Parse flags
USE_RELEASE=false
for arg in "$@"; do
    if [ "$arg" = "--release" ]; then
        USE_RELEASE=true
    fi
done

# TODO: Document build_dashboard.
build_dashboard() {
    local DASH_DIR="$REPO_ROOT/engine/dashboard"
    local DIST="$DASH_DIR/dist/index.html"

    if [ -f "$DIST" ] && [ -z "$(find "$DASH_DIR/src" -newer "$DIST" -print -quit 2>/dev/null)" ]; then
        dim "Dashboard up to date — skipping build"
        return 0
    fi

    spin_start "Building dashboard"
    cd "$DASH_DIR"
    if [ ! -d node_modules ]; then
        npm ci --silent > /dev/null 2>&1
    fi
    npx vite build --logLevel error > /dev/null 2>&1
    cd "$REPO_ROOT/engine"
    spin_stop success "Dashboard built"
}

# TODO: Document start.
start() {
    local BUILD_MODE="debug"
    [ "$USE_RELEASE" = true ] && BUILD_MODE="release"

    banner "Dev Server" "${BUILD_MODE} mode"

    # Kill anything already on the port
    EXISTING=$(lsof -ti :7700 -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$EXISTING" ]; then
        warn "Stopping existing server ${DIM}(PID $EXISTING)${NC}"
        kill $EXISTING 2>/dev/null || true
        sleep 1
    fi

    mkdir -p "$DATA_DIR"
    cd "$REPO_ROOT/engine"

    build_dashboard

    if [ "$USE_RELEASE" = true ]; then
        info "Building release binary..."
        cargo build --release -p flapjack-server
    else
        info "Building debug binary..."
        cargo build -p flapjack-server
    fi
    success "Build complete"
    echo ""

    load_flapjack_runtime_env_from_secret "$REPO_ROOT"

    # Run the binary — its output IS the output
    env RUST_LOG=warn FLAPJACK_DATA_DIR="$DATA_DIR" ${FLAPJACK_ADMIN_KEY:+FLAPJACK_ADMIN_KEY="$FLAPJACK_ADMIN_KEY"} ./target/$BUILD_MODE/flapjack
}

stop() {
    EXISTING=$(lsof -ti :7700 -sTCP:LISTEN 2>/dev/null || true)
    if [ -n "$EXISTING" ]; then
        kill $EXISTING 2>/dev/null || true
        success "Server stopped ${DIM}(PID $EXISTING)${NC}"
    else
        dim "Server not running"
    fi
}

restart() {
    stop
    sleep 1
    start
}

# Remove --release flag from command arguments
COMMAND=""
for arg in "$@"; do
    if [ "$arg" != "--release" ]; then
        COMMAND="$arg"
    fi
done

case "${COMMAND:-start}" in
    start) start ;;
    stop) stop ;;
    restart) restart ;;
    *)
        echo -e "  ${FJ} ${BOLD}Usage:${NC} $0 {start|stop|restart} [--release]"
        exit 1
        ;;
esac
