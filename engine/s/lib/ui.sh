#!/bin/bash
# ui.sh — Shared UI utilities for Flapjack CLI scripts.
# Source this file: source "$(dirname "$0")/lib/ui.sh"
#
# Provides:
#   Colors (with NO_COLOR support), branded headers, step/status formatting,
#   spinners, timers, and log file management.

# ── Brand ────────────────────────────────────────────────────────────────────
FJ="🥞"

# ── Colors ───────────────────────────────────────────────────────────────────
# Respects NO_COLOR standard (https://no-color.org) and non-interactive shells.
if [ -n "${NO_COLOR:-}" ] || [ ! -t 1 ] || [ "${TERM:-}" = "dumb" ]; then
  RED='' GREEN='' YELLOW='' BLUE='' CYAN='' MAGENTA='' DIM='' BOLD='' NC=''
  _HAS_COLOR=false
else
  RED='\033[0;31m'
  GREEN='\033[0;32m'
  YELLOW='\033[1;33m'
  BLUE='\033[0;34m'
  CYAN='\033[0;36m'
  MAGENTA='\033[0;35m'
  DIM='\033[2m'
  BOLD='\033[1m'
  NC='\033[0m'
  _HAS_COLOR=true
fi

# ── Output Functions ─────────────────────────────────────────────────────────

# Branded banner — use at the top of every pipeline/script
# Usage: banner "Staging Pipeline" "v0.0.20-beta"
banner() {
  local title="$1"
  local subtitle="${2:-}"
  echo ""
  if [ -n "$subtitle" ]; then
    echo -e "  ${BOLD}${FJ} ${title}${NC}  ${DIM}${subtitle}${NC}"
  else
    echo -e "  ${BOLD}${FJ} ${title}${NC}"
  fi
  echo ""
}

# Numbered step header
# Usage: step 1 "Running local tests"
step() {
  local num="$1"
  local label="$2"
  echo -e "  ${BLUE}${BOLD}[$num]${NC} ${BOLD}${label}${NC}"
}

# Phase header (for multi-phase pipelines)
# Usage: phase 1 "Staging Pipeline" "local tests → sync → push"
phase() {
  local num="$1"
  local label="$2"
  local desc="${3:-}"
  echo ""
  divider
  if [ -n "$desc" ]; then
    echo -e "  ${BOLD}Phase ${num}: ${label}${NC}  ${DIM}${desc}${NC}"
  else
    echo -e "  ${BOLD}Phase ${num}: ${label}${NC}"
  fi
  echo ""
}

# Status messages
info()    { echo -e "  ${CYAN}→${NC} $*"; }
success() { echo -e "  ${GREEN}✓${NC} $*"; }
warn()    { echo -e "  ${YELLOW}!${NC} $*"; }
error()   { echo -e "  ${RED}✗${NC} $*"; }

# Dimmed secondary info (for paths, commands, etc.)
dim() { echo -e "    ${DIM}$*${NC}"; }

# Horizontal divider
divider() { echo -e "  ${DIM}──────────────────────────────────────────────────────${NC}"; }

# Completion banner
# Usage: done_banner "Pipeline complete!" "Laptop can shut down."
done_banner() {
  local title="$1"
  local subtitle="${2:-}"
  echo ""
  divider
  echo ""
  if [ -n "$subtitle" ]; then
    echo -e "  ${GREEN}${BOLD}${FJ} ${title}${NC}"
    echo -e "  ${DIM}${subtitle}${NC}"
  else
    echo -e "  ${GREEN}${BOLD}${FJ} ${title}${NC}"
  fi
  echo ""
}

# Failure banner
# Usage: fail_banner "Pipeline failed!" "Tests did not pass."
fail_banner() {
  local title="$1"
  local subtitle="${2:-}"
  echo ""
  echo -e "  ${RED}${BOLD}${FJ} ${title}${NC}"
  [ -n "$subtitle" ] && echo -e "  ${DIM}${subtitle}${NC}"
  echo ""
}

# Next-steps block — suggest commands after completion
# Usage: next_steps "gh run watch --repo ..." "./s/pipeline-prod.sh 0.0.20-beta"
next_steps() {
  echo -e "  ${BOLD}Next:${NC}"
  for cmd in "$@"; do
    echo -e "    ${DIM}\$${NC} ${CYAN}${cmd}${NC}"
  done
}

# Links block — show relevant URLs
# Usage: link "Staging CI" "https://github.com/..."
link() {
  local label="$1"
  local url="$2"
  echo -e "    ${DIM}${label}:${NC} ${CYAN}${url}${NC}"
}

# Key-value display (aligned)
# Usage: kv "Version" "0.0.20-beta"
kv() {
  local key="$1"
  local val="$2"
  printf "  ${DIM}%-14s${NC} %s\n" "$key:" "$val"
}

# ── Spinner ──────────────────────────────────────────────────────────────────
# Animated spinner for short-running background operations.
# Usage:
#   spin_start "Pushing to staging..."
#   git push origin main >> "$LOG_FILE" 2>&1
#   spin_stop success "Pushed to staging"

_SPINNER_PID=""

# Pick spinner frames: braille dots have known rendering issues in VS Code
# terminal (incorrect advance width), so fall back to ASCII there.
if [ "${TERM_PROGRAM:-}" = "vscode" ]; then
  _SPINNER_FRAMES=('|' '/' '-' '\')
else
  _SPINNER_FRAMES=('⠋' '⠙' '⠹' '⠸' '⠼' '⠴' '⠦' '⠧' '⠇' '⠏')
fi
_SPINNER_FRAME_COUNT=${#_SPINNER_FRAMES[@]}

# TODO: Document spin_start.
spin_start() {
  local msg="$1"
  # No spinner in non-interactive mode
  if [ "$_HAS_COLOR" = "false" ]; then
    echo "  → $msg"
    return
  fi
  set +m 2>/dev/null || true  # Suppress job control messages
  (
    local i=0
    while true; do
      printf "\r  \033[0;36m%s\033[0m %s" "${_SPINNER_FRAMES[$((i % _SPINNER_FRAME_COUNT))]}" "$msg"
      i=$((i + 1))
      sleep 0.08
    done
  ) &
  _SPINNER_PID=$!
}

# TODO: Document spin_stop.
spin_stop() {
  local status="${1:-success}"
  local msg="${2:-}"
  if [ -n "$_SPINNER_PID" ]; then
    kill "$_SPINNER_PID" 2>/dev/null || true
    wait "$_SPINNER_PID" 2>/dev/null || true
    _SPINNER_PID=""
    printf "\r\033[2K"  # Clear the spinner line
  fi
  if [ -n "$msg" ]; then
    case "$status" in
      success) success "$msg" ;;
      error)   error "$msg" ;;
      warn)    warn "$msg" ;;
      info)    info "$msg" ;;
    esac
  fi
}

# Convenience: run a command with spinner, show success/failure
# Usage: run_with_spinner "Pushing to remote" git push origin main
run_with_spinner() {
  local label="$1"
  shift
  spin_start "$label"
  if "$@" > /dev/null 2>&1; then
    spin_stop success "$label"
    return 0
  else
    spin_stop error "$label"
    return 1
  fi
}

# Same as above but logs output to file
# Usage: run_logged "Building release" "$LOG_FILE" cargo build --release
run_logged() {
  local label="$1"
  local logfile="$2"
  shift 2
  spin_start "$label"
  if "$@" >> "$logfile" 2>&1; then
    spin_stop success "$label"
    return 0
  else
    spin_stop error "${label} — see log for details"
    dim "Log: ${logfile}"
    return 1
  fi
}

# Default trap: ensure spinner is cleaned up on exit, Ctrl+C, or SIGTERM.
# Scripts that set their own EXIT trap MUST call spin_stop in their cleanup.
_ui_cleanup() {
  if [ -n "${_SPINNER_PID:-}" ]; then
    kill "$_SPINNER_PID" 2>/dev/null || true
    wait "$_SPINNER_PID" 2>/dev/null || true
    _SPINNER_PID=""
    printf "\r\033[2K" 2>/dev/null || true
  fi
}
trap '_ui_cleanup' EXIT
trap '_ui_cleanup; exit 130' INT
trap '_ui_cleanup; exit 143' TERM

# ── Timer ────────────────────────────────────────────────────────────────────
_TIMER_START=""

timer_start() { _TIMER_START=$(date +%s); }

timer_elapsed() {
  if [ -z "$_TIMER_START" ]; then echo "0s"; return; fi
  local elapsed=$(( $(date +%s) - _TIMER_START ))
  if [ "$elapsed" -ge 60 ]; then
    echo "$((elapsed / 60))m $((elapsed % 60))s"
  else
    echo "${elapsed}s"
  fi
}

# ── Log File Management ─────────────────────────────────────────────────────
_UI_LOG_DIR="${FLAPJACK_LOG_DIR:-${HOME}/.flapjack/logs}"
_UI_LOG_FILE=""

# Initialize a log file for this run
# Usage: log_init "pipeline-staging"
log_init() {
  local name="$1"
  mkdir -p "$_UI_LOG_DIR"
  _UI_LOG_FILE="${_UI_LOG_DIR}/${name}_$(date +%Y%m%d_%H%M%S).log"
  dim "Log: ${_UI_LOG_FILE}"

  # Clean up old logs (keep last 20)
  local count
  count=$(find "$_UI_LOG_DIR" -maxdepth 1 -name "${name}_*.log" 2>/dev/null | wc -l | tr -d ' ')
  if [ "$count" -gt 20 ]; then
    ls -1t "${_UI_LOG_DIR}/${name}_"*.log 2>/dev/null | tail -n +21 | while IFS= read -r f; do rm -f "$f"; done
  fi
}

log_file() { echo "$_UI_LOG_FILE"; }

# ── Abort Helper ─────────────────────────────────────────────────────────────
# Usage: abort "Tests failed"
abort() {
  spin_stop 2>/dev/null  # Clean up any running spinner
  error "$1"
  exit 1
}
