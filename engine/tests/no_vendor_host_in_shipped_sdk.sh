#!/usr/bin/env bash
#
# no_vendor_host_in_shipped_sdk.sh — Source contract for the shipped SDK mirror.
#
# Flapjack ships a self-hosted, registry-free search engine. Any executable
# vendor host literal (an Algolia API endpoint baked into shipped SDK code)
# would make a generated client dial the vendor's infrastructure instead of the
# operator's Flapjack deployment. This scanner is the single canonical owner of
# what counts as such a literal: it walks every SDK tree under sdks/ and fails
# when executable vendor-host literals remain.
#
# It is deliberately a NEGATIVE gate: a *match* is a *failure*. We therefore
# invert grep's normal semantics — surviving matches are printed and force a
# nonzero exit, and a clean mirror exits 0. There is no allow-list of counts
# baked in here; the pass rule is simply "zero surviving matches", so the gate
# stays correct as SDK trees are added or hosts are fixed.
#
# Usage:
#   bash engine/tests/no_vendor_host_in_shipped_sdk.sh
#
# Exit status:
#   0  no executable vendor-host literals found in shipped sdks/
#   1  at least one executable vendor-host literal found, OR sdks/ is missing

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
SDKS_DIR="$REPO_DIR/sdks"

TAG="[no-vendor-host]"

# Fail loudly rather than fail-open: a missing sdks/ tree means the contract
# cannot be enforced, which we treat as a failure, not a silent pass.
if [ ! -d "$SDKS_DIR" ]; then
  printf '%s ERROR: shipped SDK mirror not found at %s\n' "$TAG" "$SDKS_DIR" >&2
  exit 1
fi

# ── The one shared matching/filtering rule ────────────────────────────────────
#
# Applied to every candidate "path:lineno:content" line. A line is an offending
# executable vendor-host literal unless one of the deliberate exclusions fires.
# Each exclusion is safe because it removes a NON-executable reference, and none
# of them inspect trailing content — so executable code that merely carries a
# trailing comment (e.g. `.host("algolia.com") // overridden later`) is still
# caught.
#
# Exclusions:
#   test paths  — vendor strings inside test/spec trees never ship as runtime
#                 endpoints; excluded by directory and by filename suffix.
#   x-algolia-* — the `X-Algolia-*` compatibility headers keep the vendor's
#                 wire name on purpose; a header token is not a host.
#   doc URL/prose — a vendor reference inside an `http(s)://` URL or a
#                 `www.algolia*` string is generated documentation/prose, not an
#                 endpoint the client dials. Executable API host literals are
#                 bare hostnames (e.g. "insights.algolia.io"), never full
#                 website URLs, so dropping scheme/`www.` references cannot hide
#                 a real endpoint.
#   comment-only — a line whose first non-whitespace token is a supported
#                 comment marker (# // /* * */ <!--) is commentary. Only the
#                 FIRST token is inspected, so trailing comments do not mask
#                 executable code.
#
# shellcheck disable=SC2016  # awk program is single-quoted on purpose (no shell expansion)
HOST_FILTER_AWK='
function is_test_path(p) {
  return (p ~ /\/(__tests__|test|tests|spec)\//) ||
         (p ~ /\.test\.[^\/]*$/)  || (p ~ /_test\.[^\/]*$/) ||
         (p ~ /Test\.[^\/]*$/)    || (p ~ /Spec\.[^\/]*$/)
}
{
  # Split "path:lineno:content" on the first two colons only; SDK paths never
  # contain a colon, but content routinely does.
  ci = index($0, ":");                if (ci == 0) next
  path = substr($0, 1, ci - 1);       rest = substr($0, ci + 1)
  cj = index(rest, ":");              if (cj == 0) next
  lineno  = substr(rest, 1, cj - 1)
  content = substr(rest, cj + 1)

  if (is_test_path(path)) next

  low = tolower(content)
  if (low !~ /algolia\.(net|com|io)/ && low !~ /algolianet\.com/) next   # no host
  if (low ~ /x-algolia/)   next                                          # compat header
  if (low ~ /https?:\/\//) next                                          # doc URL
  if (low ~ /www\.algolia/) next                                         # doc prose
  t = content; sub(/^[ \t]+/, "", t)                                     # comment-only?
  if (t ~ /^#/ || t ~ /^\/\// || t ~ /^\/\*/ || t ~ /^\*\// || t ~ /^\*/ || t ~ /^<!--/) next

  rel = substr(path, length(repo) + 2)   # strip "<repo>/" so output is caller-independent
  print rel ":" lineno ":" content
}
'

printf '%s scanning shipped SDK mirror: %s\n' "$TAG" "$SDKS_DIR"

# One case-insensitive candidate pass over the whole mirror, then the single
# shared filter above. `|| true` because grep exits 1 on zero matches, which is
# the clean (passing) case here.
candidates="$(grep -rInI -iE 'algolia\.(net|com|io)|algolianet\.com' "$SDKS_DIR" 2>/dev/null || true)"
offending_lines="$(printf '%s\n' "$candidates" \
  | awk -v repo="$REPO_DIR" "$HOST_FILTER_AWK")"

# Unique offending files (a file with several offending lines counts once).
offending_files="$(printf '%s\n' "$offending_lines" | awk -F: 'NF && $1 != "" {print $1}' | sort -u)"
total="$(printf '%s\n' "$offending_files" | grep -c . || true)"

if [ "$total" -gt 0 ]; then
  echo "$TAG offending executable vendor-host literals (Stage 2 edit list):"
  printf '%s\n' "$offending_lines" | sed "s/^/  /"
  echo ""
fi

echo "$TAG --- offending files per SDK tree ---"
for tree_dir in "$SDKS_DIR"/*/; do
  tree="$(basename "$tree_dir")"
  tree_count="$(printf '%s\n' "$offending_files" | grep -c "^sdks/$tree/" || true)"
  printf '%s   %s: %s\n' "$TAG" "$tree" "$tree_count"
done
echo "$TAG TOTAL offending files: $total"

if [ "$total" -gt 0 ]; then
  echo "$TAG FAIL: executable vendor-host literals present in shipped sdks/" >&2
  exit 1
fi

echo "$TAG OK: no executable vendor-host literals in shipped sdks/"
exit 0
