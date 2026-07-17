#!/usr/bin/env bash
#
# no_vendor_host_in_shipped_sdk_test.sh — Companion contract for the shipped-SDK
# vendor-host scanner (engine/tests/no_vendor_host_in_shipped_sdk.sh).
#
# This test NEVER reimplements the host regex or the exclusion rules. Every
# fixture is a repo-shaped tree (engine/tests/ + sdks/) into which the real
# production scanner is copied, so the only matching/filtering owner exercised
# is the production script itself. The test drives that script with controlled
# inputs and asserts exit status plus reported filenames.
#
# It proves:
#   - every matching arm fails (positive fixtures, exit 1, correct file named);
#   - a missing sdks/ tree fails loudly (no fail-open);
#   - a trailing comment does not mask executable code;
#   - every deliberate exclusion passes (negative fixtures, exit 0);
#   - the real scanner reports a zero-finding GREEN mirror from any CWD;
#   - the real `engine/_dev/s/test` runner selects the gate under --ci / --all
#     and continues to downstream work after the scanner passes.
#
# Usage:
#   bash engine/tests/no_vendor_host_in_shipped_sdk_test.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
PROD_SCANNER="$SCRIPT_DIR/no_vendor_host_in_shipped_sdk.sh"
REAL_RUNNER="$REPO_DIR/engine/_dev/s/test"

HOST_RE='algolia\.(net|com|io)|algolianet\.com'

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_PASSED=$((TESTS_PASSED + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  TESTS_RUN=$((TESTS_RUN + 1))
  TESTS_FAILED=$((TESTS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1"
  if [ -n "${2:-}" ]; then
    printf '    %s\n' "$2"
  fi
}

WORK_DIR="$(mktemp -d)"
cleanup() { rm -rf "$WORK_DIR"; }
trap cleanup EXIT

# Build a repo-shaped fixture with the production scanner copied into
# engine/tests/. The copy — never a reimplementation — is the object under test.
# mktemp gives each fixture a unique dir (this runs in a $(...) subshell, so a
# shared counter would not persist back to the parent).
new_fixture_repo() {
  local repo
  repo="$(mktemp -d "$WORK_DIR/repo_XXXXXX")"
  mkdir -p "$repo/engine/tests"
  cp "$PROD_SCANNER" "$repo/engine/tests/no_vendor_host_in_shipped_sdk.sh"
  printf '%s' "$repo"
}

write_file() {
  local repo="$1" rel="$2" content="$3"
  mkdir -p "$repo/$(dirname "$rel")"
  printf '%s\n' "$content" >"$repo/$rel"
}

# Run the copied scanner inside a fixture; capture output, echo exit status.
run_fixture() {
  local repo="$1"
  set +e
  bash "$repo/engine/tests/no_vendor_host_in_shipped_sdk.sh" >"$repo/scan.out" 2>&1
  local rc=$?
  set -e
  printf '%s' "$rc"
}

# ── Positive arms: an executable vendor-host literal must fail and be named ────

assert_positive() {
  local label="$1" rel="$2" content="$3"
  local repo rc
  repo="$(new_fixture_repo)"
  write_file "$repo" "$rel" "$content"
  rc="$(run_fixture "$repo")"
  if [ "$rc" = "1" ] && grep -Fq "$rel" "$repo/scan.out"; then
    pass "$label"
  else
    fail "$label" "rc=$rc (want 1, file named) out=$(cat "$repo/scan.out")"
  fi
}

# ── Negative arms: a deliberately excluded reference must pass ─────────────────

assert_negative() {
  local label="$1" rel="$2" content="$3"
  local repo rc
  repo="$(new_fixture_repo)"
  write_file "$repo" "$rel" "$content"
  # Guard against a vacuous pass: the fixture MUST actually contain the vendor
  # host pattern, otherwise "exit 0" proves nothing about the exclusion.
  if ! grep -Eqi "$HOST_RE" "$repo/$rel"; then
    fail "$label" "fixture does not contain the vendor host pattern (vacuous)"
    return
  fi
  rc="$(run_fixture "$repo")"
  if [ "$rc" = "0" ] && grep -Fq 'TOTAL offending files: 0' "$repo/scan.out"; then
    pass "$label"
  else
    fail "$label" "rc=$rc (want 0) out=$(cat "$repo/scan.out")"
  fi
}

echo "no_vendor_host_in_shipped_sdk_test — positive arms (executable host literals)"

# Cover every offending language plus each host-literal shape:
# {app_id}-dsn.algolia.net, literal -dsn.algolia.net, and .algolianet.com.
assert_positive "python config host ({app_id}-dsn.algolia.net)" \
  "sdks/python/flapjacksearch/recommend/config.py" \
  '                    url="{}-dsn.algolia.net".format(self.app_id),'

assert_positive "ruby api client host (.algolianet.com)" \
  "sdks/ruby/lib/flapjack/api/analytics_client.rb" \
  '          Transport::StatefulHost.new("{}-1.algolianet.com".sub("{}", app_id)),'

assert_positive "php api client host (literal -dsn.algolia.net)" \
  "sdks/php/lib/Api/InsightsClient.php" \
  "                \$host = 'myapp-dsn.algolia.net';"

assert_positive "java api client host (analytics.{region}.algolia.com)" \
  "sdks/java/flapjacksearch/src/main/java/com/flapjackhq/api/AnalyticsClient.java" \
  '    String url = "analytics.{region}.algolia.com".replace("{region}", region);'

assert_positive "java internal HttpRequester default host" \
  "sdks/java/flapjacksearch/src/main/java/com/flapjackhq/internal/HttpRequester.java" \
  '      .host("algolia.com")'

assert_positive "go ingestion client host (data.{region}.algolia.com)" \
  "sdks/go/flapjack/ingestion/client.go" \
  '		transport.NewStatefulHost("https", strings.ReplaceAll("data.{region}.algolia.com", "{region}", string(r)), call.IsReadWrite),'

# shellcheck disable=SC2016  # literal SDK source line; ${appId} must not expand
assert_positive "javascript searchClient host (search.algolia.net)" \
  "sdks/javascript/packages/client-search/src/searchClient.ts" \
  '  const host = `${appId}-dsn.algolia.net`;'

assert_positive "otherwise-clean SDK tree still fails (swift insights.algolia.io)" \
  "sdks/swift/Sources/Core/Hosts.swift" \
  '    let host = "insights.algolia.io"'

# Fail-open guard: a missing sdks/ tree must fail loudly, not pass silently.
missing_repo="$(new_fixture_repo)"
missing_rc="$(run_fixture "$missing_repo")"
if [ "$missing_rc" = "1" ] && grep -Fq "not found" "$missing_repo/scan.out"; then
  pass "missing sdks/ tree fails with diagnostic (no fail-open)"
else
  fail "missing sdks/ tree fails with diagnostic (no fail-open)" \
    "rc=$missing_rc out=$(cat "$missing_repo/scan.out")"
fi

# Trailing-comment guard: executable code with a trailing comment stays caught.
assert_positive "executable host with trailing comment stays caught" \
  "sdks/java/flapjacksearch/src/main/java/com/flapjackhq/internal/RetryHost.java" \
  '      .host("algolia.com") // will be overridden by the retry strategy'

echo ""
echo "no_vendor_host_in_shipped_sdk_test — negative arms (deliberate exclusions)"

# x-algolia-* compatibility header token is not an endpoint.
assert_negative "x-algolia compatibility header line excluded" \
  "sdks/python/flapjacksearch/http/headers.py" \
  '    DEFAULT_HEADERS = {"X-Algolia-Host": "search.algolia.net"}'

# Documentation URL / prose (scheme URL or www. reference), not an endpoint.
assert_negative "doc URL in documentation file excluded" \
  "sdks/python/README.md" \
  'See the API reference at https://www.algolia.com/doc/rest-api/search for details.'

assert_negative "doc prose (www.algolia.com) in generated docstring excluded" \
  "sdks/python/flapjacksearch/insights/models/clicked_object_ids.py" \
  '    """ For more information, see www.algolia.com/doc/guides/sending-events. """'

# Comment-only lines: every supported first-token comment marker.
assert_negative "comment marker # excluded" \
  "sdks/python/flapjacksearch/notes_hash.py" \
  '# fallback host insights.algolia.io'
assert_negative "comment marker // excluded" \
  "sdks/go/flapjack/notes_slash.go" \
  '// fallback host insights.algolia.io'
assert_negative "comment marker /* excluded" \
  "sdks/java/notes_block_open.java" \
  '/* fallback host insights.algolia.io */'
assert_negative "comment marker * excluded" \
  "sdks/java/notes_block_cont.java" \
  ' * fallback host insights.algolia.io'
assert_negative "comment marker */ excluded" \
  "sdks/java/notes_block_close.java" \
  ' */ trailing note insights.algolia.io'
assert_negative "comment marker <!-- excluded" \
  "sdks/wordpress/notes.html" \
  '<!-- fallback host insights.algolia.io -->'

# Excluded test directories and filename suffixes.
assert_negative "excluded path /__tests__/" \
  "sdks/javascript/packages/client-search/__tests__/hosts.js" \
  'const h = "search.algolia.net";'
assert_negative "excluded path /test/" \
  "sdks/java/flapjacksearch/src/test/java/com/flapjackhq/HostFixture.java" \
  'String h = "search.algolia.net";'
assert_negative "excluded path /tests/" \
  "sdks/python/tests/hosts.py" \
  'h = "search.algolia.net"'
assert_negative "excluded path /spec/" \
  "sdks/ruby/spec/hosts_fixture.rb" \
  'h = "search.algolia.net"'
assert_negative "excluded suffix *.test.*" \
  "sdks/javascript/packages/client-search/hosts.test.ts" \
  'const h = "search.algolia.net";'
assert_negative "excluded suffix *_test.*" \
  "sdks/go/flapjack/hosts_test.go" \
  'h := "search.algolia.net"'
assert_negative "excluded suffix *Test.*" \
  "sdks/java/flapjacksearch/src/main/java/com/flapjackhq/HostFixtureTest.java" \
  'String h = "search.algolia.net";'
assert_negative "excluded suffix *Spec.*" \
  "sdks/kotlin/src/main/kotlin/com/flapjackhq/HostFixtureSpec.kt" \
  'val h = "search.algolia.net"'

echo ""
echo "no_vendor_host_in_shipped_sdk_test — real scanner, GREEN baseline, location independence"

# Run the REAL production scanner against the REAL mirror from two CWDs. Output
# is repo-relative / absolute-sdks-path, so it is caller-independent; no
# normalization beyond the working directory is applied.
set +e
(cd "$REPO_DIR" && bash "$PROD_SCANNER") >"$WORK_DIR/real_root.out" 2>&1
rc_root=$?
(cd "$REPO_DIR/engine" && bash "$PROD_SCANNER") >"$WORK_DIR/real_eng.out" 2>&1
rc_eng=$?
set -e

if [ "$rc_root" = "0" ] && [ "$rc_eng" = "0" ] \
  && diff -q "$WORK_DIR/real_root.out" "$WORK_DIR/real_eng.out" >/dev/null; then
  pass "real scanner: identical zero-finding output from repo root and engine/"
else
  fail "real scanner: identical zero-finding output from repo root and engine/" \
    "rc_root=$rc_root rc_eng=$rc_eng"
fi

assert_green_count() {
  local label="$1" needle="$2"
  if grep -Fq "$needle" "$WORK_DIR/real_root.out" && grep -Fq "$needle" "$WORK_DIR/real_eng.out"; then
    pass "$label"
  else
    fail "$label" "missing: $needle"
  fi
}
assert_green_count "GREEN baseline: python 0" '[no-vendor-host]   python: 0'
assert_green_count "GREEN baseline: ruby 0" '[no-vendor-host]   ruby: 0'
assert_green_count "GREEN baseline: php 0" '[no-vendor-host]   php: 0'
assert_green_count "GREEN baseline: java 0" '[no-vendor-host]   java: 0'
assert_green_count "GREEN baseline: go 0" '[no-vendor-host]   go: 0'
assert_green_count "GREEN baseline: javascript 0" '[no-vendor-host]   javascript: 0'
assert_green_count "GREEN baseline: 0 files total" '[no-vendor-host] TOTAL offending files: 0'
assert_green_count "GREEN baseline: OK line" '[no-vendor-host] OK: no executable vendor-host literals in shipped sdks/'

echo ""
echo "no_vendor_host_in_shipped_sdk_test — runner-contract (real engine/_dev/s/test)"

# Drive the REAL runner. Only downstream heavy commands are shadowed on PATH with
# markers; the runner still uses its own lib/ and the real scanner. Cargo exits
# with a sentinel failure after recording the first downstream command, proving
# the scanner passed and the runner continued without running the full suite.
assert_runner_runs_gate_then_continues() {
  local label="$1" mode="$2"
  local shadow="$WORK_DIR/shadow_${mode#--}"
  local marker="$shadow/tool_ran.log"
  mkdir -p "$shadow"
  cat >"$shadow/cargo" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "cargo" "\$*" >>"$marker"
exit 73
EOF
  chmod +x "$shadow/cargo"
  local t
  for t in npm npx node playwright; do
    cat >"$shadow/$t" <<EOF
#!/usr/bin/env bash
printf '%s %s\n' "$t" "\$*" >>"$marker"
exit 0
EOF
    chmod +x "$shadow/$t"
  done
  set +e
  PATH="$shadow:$PATH" bash "$REAL_RUNNER" "$mode" >"$shadow/run.out" 2>&1
  local rc=$?
  set -e
  if [ "$rc" = "73" ] \
    && grep -Fq 'SDK: no executable vendor-host literals in shipped sdks/' "$shadow/run.out" \
    && grep -Fq 'TOTAL offending files: 0' "$shadow/run.out" \
    && grep -Fq 'OK: no executable vendor-host literals in shipped sdks/' "$shadow/run.out" \
    && grep -Fq 'cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication' "$marker"; then
    pass "$label"
  else
    fail "$label" \
      "rc=$rc marker=$([ -f "$marker" ] && tr '\n' ';' <"$marker" || echo none)"
  fi
}

assert_runner_runs_gate_then_continues "runner --ci selects gate and continues after pass" --ci
assert_runner_runs_gate_then_continues "runner --all selects gate and continues after pass" --all

echo ""
printf 'Ran %d, passed %d, failed %d\n' "$TESTS_RUN" "$TESTS_PASSED" "$TESTS_FAILED"
[ "$TESTS_FAILED" -eq 0 ]
