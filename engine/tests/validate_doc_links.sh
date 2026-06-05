#!/usr/bin/env bash
# validate_doc_links.sh — Check that public doc graph links resolve to real files.
#
# Uses .debbie.toml as the source of truth for the Stage 2 public doc graph, then
# verifies that every relative markdown link in those canonical docs resolves on
# disk.
#
# Usage:
#   bash engine/tests/validate_doc_links.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
README_PATH="$REPO_DIR/README.md"
ENGINE_README_PATH="$REPO_DIR/engine/README.md"

# shellcheck source=engine/tests/doc_sync_helpers.sh
source "$SCRIPT_DIR/doc_sync_helpers.sh"

FAILURE_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-link-failures.XXXXXX")
COUNT_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-link-counts.XXXXXX")
DOCS_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-link-docs.XXXXXX")
README_FAILURE_LOG=""
if [ -n "${BUNDLE_DIR:-}" ]; then
  mkdir -p "$BUNDLE_DIR"
  README_FAILURE_LOG="$BUNDLE_DIR/readme_link_failures.txt"
  : > "$README_FAILURE_LOG"
fi

cleanup() {
  doc_sync_cleanup
  rm -f "$FAILURE_LOG" "$COUNT_LOG" "$DOCS_LOG"
}
trap cleanup EXIT

doc_sync_init "$REPO_DIR"

readme_public_guard_urls() {
  grep -hEo "https?://[^][) >\"']+" "$README_PATH" "$ENGINE_README_PATH" \
    | grep -Ev '^https?://(localhost|127\.0\.0\.1)(:|/|$)' \
    | awk '!seen[$0]++' || true
}

# Reject README URLs that target private, loopback, link-local, or credentialed
# endpoints before curl ever leaves the machine. The README URL guard runs in CI
# against repo-controlled content, so doc changes must not be able to repurpose
# it as an SSRF probe into runner-local networks or metadata services.
is_safe_public_url() {
  local url="$1"

  python3 - "$url" <<'PY'
import ipaddress
import socket
import sys
from urllib.parse import urlsplit

url = sys.argv[1]
parts = urlsplit(url)

if parts.scheme not in {"http", "https"}:
    raise SystemExit(1)
if not parts.hostname:
    raise SystemExit(1)
if parts.username is not None or parts.password is not None:
    raise SystemExit(1)

host = parts.hostname
try:
    literal_ip = ipaddress.ip_address(host)
except ValueError:
    literal_ip = None

blocked_flags = (
    "is_private",
    "is_loopback",
    "is_link_local",
    "is_multicast",
    "is_reserved",
    "is_unspecified",
)

def is_blocked(addr):
    return any(getattr(addr, flag) for flag in blocked_flags)

if literal_ip is not None:
    raise SystemExit(1 if is_blocked(literal_ip) else 0)

try:
    infos = socket.getaddrinfo(host, None, proto=socket.IPPROTO_TCP)
except socket.gaierror:
    # Let the normal curl probe surface a DNS failure later; this guard only
    # blocks addresses we can positively classify as unsafe.
    raise SystemExit(0)

for info in infos:
    addr_text = info[4][0]
    try:
        addr = ipaddress.ip_address(addr_text)
    except ValueError:
        continue
    if is_blocked(addr):
        raise SystemExit(1)

raise SystemExit(0)
PY
}

probe_http_status() {
  local url="$1"
  shift

  curl -L -s -o /dev/null -w "%{http_code}" --max-time 30 "$@" "$url"
}

printf "\033[1mDoc Link Validation\033[0m\n"
doc_sync_collect_sync_surface
doc_sync_collect_validation_docs "$DOCS_LOG" "engine/docs/HIGHEST_LEVEL.md"

while IFS= read -r doc; do
  [ -n "$doc" ] || continue

  while IFS=$'\t' read -r src_doc line_num target resolved_path; do
    [ -n "$resolved_path" ] || continue

    if [ ! -e "$REPO_DIR/$resolved_path" ]; then
      printf "  \033[0;31m✗\033[0m %s:%s → %s (not found)\n" "$src_doc" "$line_num" "$target"
      printf "FAIL\n" >> "$FAILURE_LOG"
    fi
    printf "CHECK\n" >> "$COUNT_LOG"
  done < <(doc_sync_extract_relative_markdown_links "$doc")
done < "$DOCS_LOG"

# Stage 9 durable onboarding guard: highest-value public URLs from README.md.
README_GUARD_URLS=$(readme_public_guard_urls)
if [ -z "$README_GUARD_URLS" ]; then
  printf "  \033[0;31m✗\033[0m README public URL guard could not extract any onboarding URLs\n"
  if [ -n "$README_FAILURE_LOG" ]; then
    printf "missing-url\tREADME public URL guard could not extract any onboarding URLs\n" >> "$README_FAILURE_LOG"
  fi
  printf "FAIL\n" >> "$FAILURE_LOG"
fi

while IFS= read -r url; do
  [ -n "$url" ] || continue
  http_code=""
  retry_code=""
  http_probe_failed=0
  retry_probe_failed=0

  if ! is_safe_public_url "$url"; then
    printf "  \033[0;31m✗\033[0m external URL rejected by safety guard: %s\n" "$url"
    if [ -n "$README_FAILURE_LOG" ]; then
      printf "unsafe-url\tunsafe-url\t%s\n" "$url" >> "$README_FAILURE_LOG"
    fi
    printf "FAIL\n" >> "$FAILURE_LOG"
    printf "CHECK\n" >> "$COUNT_LOG"
    continue
  fi

  if ! http_code=$(probe_http_status "$url"); then
    http_probe_failed=1
  fi

  if [ "$http_probe_failed" -eq 1 ] || [ "$http_code" -ge 400 ]; then
    # Retry once with a browser-like UA for providers that filter default curl.
    if ! retry_code=$(probe_http_status "$url" -H "User-Agent: Mozilla/5.0"); then
      retry_probe_failed=1
    fi

    if [ "$retry_probe_failed" -eq 1 ] || [ "$retry_code" -ge 400 ]; then
      if [ "$http_probe_failed" -eq 1 ] || [ "$retry_probe_failed" -eq 1 ]; then
        printf "  \033[0;31m✗\033[0m external URL probe failed (%s/%s): %s\n" \
          "${http_code:-curl-error}" "${retry_code:-curl-error}" "$url"
      else
        printf "  \033[0;31m✗\033[0m external URL failed (%s/%s): %s\n" "$http_code" "$retry_code" "$url"
      fi
      if [ -n "$README_FAILURE_LOG" ]; then
        printf "%s\t%s\t%s\n" "${http_code:-curl-error}" "${retry_code:-curl-error}" "$url" >> "$README_FAILURE_LOG"
      fi
      printf "FAIL\n" >> "$FAILURE_LOG"
    fi
  fi
  printf "CHECK\n" >> "$COUNT_LOG"
done <<EOF
$README_GUARD_URLS
EOF

CHECKED=$(doc_sync_count_log_lines "$COUNT_LOG")
FAILURES=$(doc_sync_count_log_lines "$FAILURE_LOG")
DOCS_CHECKED=$(doc_sync_count_log_lines "$DOCS_LOG")

printf "\n\033[1mChecked %d links across %d public doc graph docs\033[0m\n" "$CHECKED" "$DOCS_CHECKED"

if [ "$FAILURES" -gt 0 ]; then
  printf "\033[0;31m%d broken link(s) found\033[0m\n" "$FAILURES"
  exit 1
fi

printf "\033[0;32mAll links valid\033[0m\n"
exit 0
