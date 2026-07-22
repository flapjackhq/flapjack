#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage:
  wait_for_flapjack.sh --pid <pid> --log-path <path> [--health-url <url> | --host <host> --port <port|auto>] [--retries <n>] [--interval-seconds <seconds>]

Examples:
  wait_for_flapjack.sh --pid 1234 --health-url http://localhost:7700/health --log-path /tmp/server.log
  wait_for_flapjack.sh --pid 1234 --host 127.0.0.1 --port auto --log-path /tmp/server.log
EOF
}

extract_port_from_log() {
  local log_path="$1"
  sed -n -E 's/.*Local:.*http:\/\/(\[::\]|0\.0\.0\.0|127\.0\.0\.1):([0-9]+).*/\2/p' "$log_path" | head -1
}

print_failure_with_log() {
  local message="$1"
  local log_path="$2"
  echo "ERROR: $message" >&2
  if [ -f "$log_path" ]; then
    cat "$log_path" >&2 || true
  fi
}

main() {
  local pid="" log_path="" health_url="" host="127.0.0.1" port="" retries="60" interval_seconds="0.5"

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --pid)
        pid="${2:-}"
        shift 2
        ;;
      --log-path)
        log_path="${2:-}"
        shift 2
        ;;
      --health-url)
        health_url="${2:-}"
        shift 2
        ;;
      --host)
        host="${2:-}"
        shift 2
        ;;
      --port)
        port="${2:-}"
        shift 2
        ;;
      --retries)
        retries="${2:-}"
        shift 2
        ;;
      --interval-seconds)
        interval_seconds="${2:-}"
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

  if [ -z "$pid" ] || [ -z "$log_path" ]; then
    echo 'ERROR: --pid and --log-path are required' >&2
    usage >&2
    return 1
  fi

  if [ -z "$health_url" ] && [ -z "$port" ]; then
    echo 'ERROR: provide --health-url or --port' >&2
    usage >&2
    return 1
  fi

  if [ -n "$health_url" ] && [ -n "$port" ]; then
    echo 'ERROR: provide only one of --health-url or --port' >&2
    usage >&2
    return 1
  fi

  local resolved_health_url="" detected_port=""
  for _i in $(seq 1 "$retries"); do
    if ! kill -0 "$pid" 2>/dev/null; then
      print_failure_with_log "server process ${pid} exited before becoming ready" "$log_path"
      return 1
    fi

    resolved_health_url="$health_url"
    if [ -z "$resolved_health_url" ]; then
      if [ "$port" = "auto" ]; then
        detected_port="$(extract_port_from_log "$log_path")"
        if [ -n "$detected_port" ]; then
          resolved_health_url="http://${host}:${detected_port}/health"
        fi
      else
        resolved_health_url="http://${host}:${port}/health"
      fi
    fi

    if [ -n "$resolved_health_url" ]; then
      health_status="$(curl -sS -o /dev/null -w '%{http_code}' "$resolved_health_url" 2>/dev/null || true)"
      if [ "$health_status" = "200" ]; then
        return 0
      fi
    fi

    sleep "$interval_seconds"
  done

  local timeout_seconds
  timeout_seconds="$(awk -v retries="$retries" -v interval="$interval_seconds" 'BEGIN { printf "%.1f", retries * interval }')"
  print_failure_with_log "server did not become ready within ${timeout_seconds}s" "$log_path"
  return 1
}

main "$@"
