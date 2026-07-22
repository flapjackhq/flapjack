#!/usr/bin/env bash
# Shared Algolia vendor HTTP transport for live test drivers.
#
# Source this file to get read/write host routing, URL encoding, a
# status-code-aware request helper, and task-wait polling. It owns transport
# only: index naming, fixture content, sweep policy, and cleanup ownership stay
# with the driver that sources it, so there is exactly one owner per concern.
#
# Requires ALGOLIA_APP_ID and ALGOLIA_ADMIN_KEY in the environment plus curl and jq.
#
# Credentials travel through a curl config on stdin rather than argv so the
# admin key never appears in the process table.

ALGOLIA_VENDOR_REQUEST_TIMEOUT_SECONDS="${ALGOLIA_VENDOR_REQUEST_TIMEOUT_SECONDS:-120}"
ALGOLIA_VENDOR_TASK_POLL_ATTEMPTS="${ALGOLIA_VENDOR_TASK_POLL_ATTEMPTS:-120}"
ALGOLIA_VENDOR_TASK_POLL_INTERVAL_SECONDS="${ALGOLIA_VENDOR_TASK_POLL_INTERVAL_SECONDS:-0.5}"

# Reads go to the DSN host; every mutation and task wait must go to the write
# host, where writes are immediately visible.
algolia_vendor_base() {
  case "$1" in
    read) printf 'https://%s-dsn.algolia.net' "$ALGOLIA_APP_ID" ;;
    write) printf 'https://%s.algolia.net' "$ALGOLIA_APP_ID" ;;
    *) return 1 ;;
  esac
}

algolia_vendor_url_encode() {
  jq -nr --arg value "$1" '$value | @uri'
}

# Curl callers in the migration driver use the same payload/status framing as
# vendor requests. Keep the split here so CR and empty-body handling cannot
# drift between the sourced transport and its caller.
algolia_vendor_response_body() { sed '$d'; }
algolia_vendor_response_code() { tail -1; }

# Performs one vendor request, writes the response payload to <out>, and prints
# the HTTP status code on stdout. Returns nonzero only on transport failure, so
# callers can distinguish "no answer" from "an answer we dislike".
algolia_vendor_request() {
  local mode="$1" method="$2" path="$3" body="$4" out="$5"
  local base body_file="" response status

  base="$(algolia_vendor_base "$mode")" || return 1
  if [ -n "$body" ]; then
    body_file="$(mktemp)" || return 1
    chmod 600 "$body_file" 2>/dev/null || true
    printf '%s' "$body" >"$body_file" || {
      rm -f "$body_file"
      return 1
    }
  fi

  set +e
  response="$({
    printf 'silent\n'
    printf 'show-error\n'
    printf 'request = "%s"\n' "$method"
    printf 'url = "%s%s"\n' "$base" "$path"
    printf 'header = "x-algolia-application-id: %s"\n' "$ALGOLIA_APP_ID"
    printf 'header = "x-algolia-api-key: %s"\n' "$ALGOLIA_ADMIN_KEY"
    printf 'header = "content-type: application/json"\n'
    [ -z "$body_file" ] || printf 'data-binary = "@%s"\n' "$body_file"
  } | curl --max-time "$ALGOLIA_VENDOR_REQUEST_TIMEOUT_SECONDS" -w '\n%{http_code}' --config -)"
  status=$?
  set -e

  [ -z "$body_file" ] || rm -f "$body_file"
  [ "$status" -eq 0 ] || return "$status"
  printf '%s\n' "$response" | algolia_vendor_response_body >"$out"
  printf '%s\n' "$response" | algolia_vendor_response_code
}

# Blocks until the named index task is published. Polls the write host so a
# freshly issued mutation is never reported against a stale replica.
algolia_vendor_wait_task() {
  local index="$1" task_id="$2" out="$3"
  local path attempt=0 code task_status
  path="/1/indexes/$(algolia_vendor_url_encode "$index")/task/${task_id}"
  while [ "$attempt" -lt "$ALGOLIA_VENDOR_TASK_POLL_ATTEMPTS" ]; do
    code="$(algolia_vendor_request write GET "$path" "" "$out")" || return 1
    [ "$code" = "200" ] || return 1
    task_status="$(jq -er '.status | strings' "$out" 2>/dev/null)" || return 1
    case "$task_status" in
      published) return 0 ;;
      notPublished) ;;
      *) return 1 ;;
    esac
    attempt=$((attempt + 1))
    sleep "$ALGOLIA_VENDOR_TASK_POLL_INTERVAL_SECONDS"
  done
  return 1
}
