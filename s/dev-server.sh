#!/bin/bash
# Development server launcher
# Usage:
#   ./s/dev-server.sh start            - builds and starts debug version
#   ./s/dev-server.sh start --release  - builds and starts release version
#   ./s/dev-server.sh stop             - stops the server
#   ./s/dev-server.sh restart          - restarts the server
#   ./s/dev-server.sh logs             - tail the server logs

set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
DATA_DIR="$REPO_ROOT/dev-data"
PID_FILE="$DATA_DIR/server.pid"
LOG_FILE="$DATA_DIR/server.log"

# Parse flags
USE_RELEASE=false
for arg in "$@"; do
    if [ "$arg" = "--release" ]; then
        USE_RELEASE=true
    fi
done

start() {
    if [ -f "$PID_FILE" ]; then
        PID=$(cat "$PID_FILE")
        if ps -p $PID > /dev/null 2>&1; then
            echo "Server already running (PID $PID)"
            exit 1
        fi
        rm "$PID_FILE"
    fi

    mkdir -p "$DATA_DIR"

    cd "$REPO_ROOT"

    if [ "$USE_RELEASE" = true ]; then
        echo "Building release version..."
        cargo build --release -p flapjack-server
        BUILD_TYPE="release"
    else
        echo "Building debug version..."
        cargo build -p flapjack-server
        BUILD_TYPE="debug"
    fi

    echo "Starting server ($BUILD_TYPE build, data=$DATA_DIR)..."
    RUST_LOG=warn FLAPJACK_DATA_DIR="$DATA_DIR" ./target/$BUILD_TYPE/flapjack-server > "$LOG_FILE" 2>&1 &
    echo $! > "$PID_FILE"

    for i in {1..10}; do
        if curl -s http://localhost:7700/health > /dev/null 2>&1; then
            echo "Server started (PID $(cat $PID_FILE))"
            echo "Logs: $LOG_FILE"
            exit 0
        fi
        sleep 0.5
    done

    echo "Server failed to start. Check $LOG_FILE"
    exit 1
}

stop() {
    if [ ! -f "$PID_FILE" ]; then
        echo "Server not running"
        return 0
    fi

    PID=$(cat "$PID_FILE")
    if ps -p $PID > /dev/null 2>&1; then
        kill $PID
        rm "$PID_FILE"
        echo "Server stopped"
    else
        echo "Server not running (stale PID file)"
        rm "$PID_FILE"
    fi
}

restart() {
    stop
    sleep 1
    start
}

logs() {
    if [ ! -f "$LOG_FILE" ]; then
        echo "No logs found"
        exit 1
    fi
    tail -f "$LOG_FILE"
}

# Remove --release flag from command arguments
COMMAND=""
for arg in "$@"; do
    if [ "$arg" != "--release" ]; then
        COMMAND="$arg"
    fi
done

case "${COMMAND:-}" in
    start) start ;;
    stop) stop ;;
    restart) restart ;;
    logs) logs ;;
    *)
        echo "Usage: $0 {start|stop|restart|logs} [--release]"
        exit 1
        ;;
esac
