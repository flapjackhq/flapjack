#!/usr/bin/env bash

set -euo pipefail

SCRIPT_PATH="${BASH_SOURCE[0]}"
if [[ "$SCRIPT_PATH" == */* ]]; then
  SCRIPT_PARENT="${SCRIPT_PATH%/*}"
else
  SCRIPT_PARENT="."
fi
SCRIPT_DIR="$(cd "$SCRIPT_PARENT" && pwd)"
ENGINE_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
WAIT_FOR_FLAPJACK="$ENGINE_DIR/tests/common/wait_for_flapjack.sh"

EXPECTED_CSP="default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'"
EXPECTED_X_CONTENT_TYPE_OPTIONS="nosniff"
EXPECTED_REFERRER_POLICY="no-referrer"
EXPECTED_PERMISSIONS_POLICY="camera=(), microphone=(), geolocation=()"
EXPECTED_SWAGGER_SCRIPT_TAGS='<script src="./swagger-ui-bundle.js" charset="UTF-8">|<script src="./swagger-ui-standalone-preset.js" charset="UTF-8">|<script src="./swagger-initializer.js" charset="UTF-8">'
EXPECTED_SWAGGER_INLINE_SCRIPT_PRESENT="false"

BIN=""
TMP_ROOT=""
SERVER_PID=""
BASE=""
CHECKS_RUN=0
CHECKS_FAILED=0
SURFACES_EXERCISED=0
INDETERMINATE=0
SELF_TESTS_RUN=0
SELF_TESTS_FAILED=0

required_tools() {
  local missing=0 tool
  for tool in awk cargo cat curl env grep head mkdir mktemp perl rm sed tail tr; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      printf 'INDETERMINATE required_tool expected=present actual=missing:%s\n' "$tool" >&2
      missing=1
    fi
  done
  if [ "$missing" -ne 0 ]; then
    INDETERMINATE=1
    exit 1
  fi
  if [ ! -x "$WAIT_FOR_FLAPJACK" ]; then
    printf 'INDETERMINATE wait_helper expected=executable actual=%s\n' "$WAIT_FOR_FLAPJACK" >&2
    INDETERMINATE=1
    exit 1
  fi
}

target_dir() {
  if [ -z "${CARGO_TARGET_DIR:-}" ]; then
    printf '%s\n' "$ENGINE_DIR/target"
  elif [ "${CARGO_TARGET_DIR#/}" != "$CARGO_TARGET_DIR" ]; then
    printf '%s\n' "$CARGO_TARGET_DIR"
  else
    printf '%s\n' "$ENGINE_DIR/$CARGO_TARGET_DIR"
  fi
}

mark_indeterminate() {
  INDETERMINATE=1
  printf 'INDETERMINATE %s expected=%s actual=%s\n' "$1" "$2" "$3" >&2
}

record_result() {
  local status="$1" surface="$2" check="$3" expected="$4" actual="$5"
  CHECKS_RUN=$((CHECKS_RUN + 1))
  if [ "$status" = "PASS" ]; then
    printf '[PASS] %s %s expected=%s actual=%s\n' "$surface" "$check" "$expected" "$actual"
  else
    CHECKS_FAILED=$((CHECKS_FAILED + 1))
    printf '[FAIL] %s %s expected=%s actual=%s\n' "$surface" "$check" "$expected" "$actual"
  fi
}

record_equals() {
  local surface="$1" check="$2" expected="$3" actual="$4"
  if [ "$actual" = "$expected" ]; then
    record_result PASS "$surface" "$check" "$expected" "$actual"
  else
    record_result FAIL "$surface" "$check" "$expected" "$actual"
  fi
}

header_value() {
  local hdr="$1" name="$2"
  awk -F':' -v needle="$name" '
    {
      sub(/\r$/, "", $0)
    }
    tolower($1) == needle {
      value = substr($0, index($0, ":") + 1)
      sub(/^[[:space:]]+/, "", value)
      print value
      exit
    }
  ' "$hdr"
}

header_present() {
  local hdr="$1" name="$2"
  awk -F':' -v needle="$name" '
    tolower($1) == needle { found = 1 }
    END { exit(found ? 0 : 1) }
  ' "$hdr"
}

record_header_absent() {
  local surface="$1" check="$2" hdr="$3" name="$4" actual
  if header_present "$hdr" "$name"; then
    actual="$(header_value "$hdr" "$name")"
    record_result FAIL "$surface" "$check" absent "${actual:-<empty>}"
  else
    record_result PASS "$surface" "$check" absent absent
  fi
}

script_opening_tags() {
  { grep -o '<script[^>]*>' "$1" || true; } | tr '\n' '|' | sed 's/|$//'
}

script_inline_present() {
  local tags="$1"
  if [ -z "$tags" ]; then
    printf 'false\n'
    return 0
  fi
  printf '%s\n' "$tags" | tr '|' '\n' | awk '
    BEGIN { inline = "false" }
    tolower($0) !~ / src=/ { inline = "true" }
    END { print inline }
  '
}

inline_script_authorization_result() {
  local body="$1" csp="$2"
  perl -MDigest::SHA=sha256_base64,sha384_base64,sha512_base64 -e '
    use strict;
    use warnings;

    my ($body_path, $csp) = @ARGV;
    open my $body_fh, "<", $body_path or die "cannot read body: $!";
    local $/;
    my $html = <$body_fh>;

    my $script_src;
    for my $directive (split /;/, $csp) {
      if ($directive =~ /^\s*script-src\s+(.+?)\s*$/i) {
        $script_src = $1;
        last;
      }
    }
    if (!defined $script_src) {
      print "unauthorized:script-src_missing\n";
      exit 1;
    }

    my %sources;
    for my $source (split /\s+/, $script_src) {
      $source =~ s/^\x27|\x27$//g;
      $sources{$source} = 1 if length $source;
    }

    my $inline_count = 0;
    while ($html =~ m{<script\b([^>]*)>(.*?)</script\s*>}gis) {
      my ($attributes, $script) = ($1, $2);
      next if $attributes =~ /\bsrc\s*=/i;
      $inline_count++;

      my $authorized = 0;
      my $nonce;
      if ($attributes =~ /\bnonce\s*=\s*(["\x27])([^"\x27]*)\1/i) {
        $nonce = $2;
      } elsif ($attributes =~ /\bnonce\s*=\s*([^\s>]+)/i) {
        $nonce = $1;
      }
      $authorized = 1 if defined $nonce && $sources{"nonce-$nonce"};

      for my $algorithm (qw(sha256 sha384 sha512)) {
        last if $authorized;
        my $digest = $algorithm eq "sha256" ? sha256_base64($script)
          : $algorithm eq "sha384" ? sha384_base64($script)
          : sha512_base64($script);
        $digest =~ s/=+$//;
        for my $source (keys %sources) {
          next unless $source =~ /^\Q$algorithm\E-(.+)$/;
          my $expected = $1;
          $expected =~ s/=+$//;
          if ($digest eq $expected) {
            $authorized = 1;
            last;
          }
        }
      }

      if (!$authorized) {
        print "unauthorized:inline_script_$inline_count\n";
        exit 1;
      }
    }

    if (!$inline_count) {
      print "unauthorized:no_inline_script_bodies\n";
      exit 1;
    }
    print "authorized:inline_scripts=$inline_count\n";
  ' "$body" "$csp"
}

external_scripts_self_compatible() {
  local body="$1" csp="$2"
  perl -e '
    use strict;
    use warnings;

    my ($body_path, $csp) = @ARGV;
    my ($script_src) = map { /^\s*script-src\s+(.+?)\s*$/i ? $1 : () } split /;/, $csp;
    exit 1 unless defined $script_src && $script_src =~ /(?:^|\s)\x27self\x27(?:\s|$)/;

    open my $body_fh, "<", $body_path or die "cannot read body: $!";
    local $/;
    my $html = <$body_fh>;
    my $count = 0;
    while ($html =~ m{<script\b([^>]*)>}gis) {
      my $attributes = $1;
      my $src;
      if ($attributes =~ /\bsrc\s*=\s*(["\x27])([^"\x27]*)\1/i) {
        $src = $2;
      } elsif ($attributes =~ /\bsrc\s*=\s*([^\s>]+)/i) {
        $src = $1;
      } else {
        exit 1;
      }
      exit 1 if $src =~ m{^(?:[a-z][a-z0-9+.-]*:|//)}i;
      $count++;
    }
    exit($count ? 0 : 1);
  ' "$body" "$csp"
}

build_current_checkout_binary() {
  local build_log="$TMP_ROOT/build.log"
  if ! (cd "$ENGINE_DIR" && cargo build -p flapjack-server >"$build_log" 2>&1); then
    tail -30 "$build_log" >&2 || true
    mark_indeterminate build "cargo build -p flapjack-server succeeds" failed
    exit 1
  fi

  BIN="$(target_dir)/debug/flapjack"
  if [ ! -x "$BIN" ]; then
    mark_indeterminate binary "executable at $BIN" missing
    exit 1
  fi
}

stop_server() {
  if [ -n "$SERVER_PID" ] && kill -0 "$SERVER_PID" 2>/dev/null; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
  SERVER_PID=""
  BASE=""
}

cleanup() {
  local script_exit_code=$?
  stop_server

  if [ -n "$TMP_ROOT" ] && [ -d "$TMP_ROOT" ]; then
    if [ "$CHECKS_FAILED" -gt 0 ] || [ "$INDETERMINATE" -ne 0 ] || [ "$script_exit_code" -ne 0 ]; then
      printf 'INFO: preserved security header evidence at %s\n' "$TMP_ROOT" >&2
    else
      rm -rf "$TMP_ROOT"
    fi
  fi
}
trap cleanup EXIT

start_server() {
  local data_dir="$TMP_ROOT/data"
  local log_path="$TMP_ROOT/server.log"
  mkdir -p "$data_dir"

  env \
    -u FLAPJACK_ALLOW_NO_AUTH_PUBLIC_BIND \
    -u FLAPJACK_BIND_ADDR \
    -u FLAPJACK_CONTENT_SECURITY_POLICY \
    -u FLAPJACK_DISABLE_DASHBOARD \
    -u FLAPJACK_ENV \
    -u FLAPJACK_NO_AUTH \
    -u FLAPJACK_NODE_ID \
    -u FLAPJACK_PEERS \
    -u FLAPJACK_PORT \
    FLAPJACK_ADMIN_KEY="security-headers-http-probe-admin-key" \
    FLAPJACK_DATA_DIR="$data_dir" \
    "$BIN" --auto-port >"$log_path" 2>&1 &
  SERVER_PID=$!

  if ! "$WAIT_FOR_FLAPJACK" \
    --pid "$SERVER_PID" \
    --host 127.0.0.1 \
    --port auto \
    --log-path "$log_path" \
    --retries 80 \
    --interval-seconds 0.5; then
    mark_indeterminate readiness "healthy loopback auto-port server" failed
    exit 1
  fi

  if [ ! -s "$log_path" ]; then
    mark_indeterminate server_log non_empty empty
    exit 1
  fi

  local port
  port="$(sed -n 's/.*Local:.*http:\/\/127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' "$log_path" | head -1)"
  if [ -z "$port" ]; then
    mark_indeterminate auto_port "Local: http://127.0.0.1:<port>" missing
    exit 1
  fi

  BASE="http://127.0.0.1:${port}"
  printf 'INFO: probing %s\n' "$BASE"
}

fetch_surface() {
  local surface="$1" path="$2" hdr="$3" body="$4" http_code
  if ! http_code="$(curl -sS -D "$hdr" -o "$body" -w '%{http_code}' "$BASE$path")"; then
    mark_indeterminate "${surface}_curl" "transport success" failed
    return 1
  fi
  if [ ! -s "$hdr" ]; then
    mark_indeterminate "${surface}_headers" non_empty empty
    return 1
  fi
  if ! printf '%s' "$http_code" | grep -Eq '^[0-9][0-9][0-9]$'; then
    mark_indeterminate "${surface}_http_code" "three digit status" "$http_code"
    return 1
  fi
  printf '%s\n' "$http_code"
}

check_frame_protection() {
  local surface="$1" csp="$2" x_frame_options="$3"
  if printf '%s' "$csp" | grep -Fq "frame-ancestors 'none'"; then
    record_result PASS "$surface" frame_protection "CSP frame-ancestors 'none' or X-Frame-Options DENY" "CSP frame-ancestors 'none'"
  elif printf '%s' "$x_frame_options" | tr '[:lower:]' '[:upper:]' | grep -Fxq "DENY"; then
    record_result PASS "$surface" frame_protection "CSP frame-ancestors 'none' or X-Frame-Options DENY" "X-Frame-Options DENY"
  else
    record_result FAIL "$surface" frame_protection "CSP frame-ancestors 'none' or X-Frame-Options DENY" "csp=${csp:-missing};x-frame-options=${x_frame_options:-missing}"
  fi
}

check_security_headers() {
  local surface="$1" hdr="$2" csp xcto referrer permissions xfo
  csp="$(header_value "$hdr" content-security-policy)"
  xcto="$(header_value "$hdr" x-content-type-options)"
  referrer="$(header_value "$hdr" referrer-policy)"
  permissions="$(header_value "$hdr" permissions-policy)"
  xfo="$(header_value "$hdr" x-frame-options)"
  record_equals "$surface" content_security_policy "$EXPECTED_CSP" "$csp"
  record_equals "$surface" x_content_type_options "$EXPECTED_X_CONTENT_TYPE_OPTIONS" "$xcto"
  record_equals "$surface" referrer_policy "$EXPECTED_REFERRER_POLICY" "$referrer"
  record_equals "$surface" permissions_policy "$EXPECTED_PERMISSIONS_POLICY" "$permissions"
  check_frame_protection "$surface" "$csp" "$xfo"
  record_header_absent "$surface" hsts_absent "$hdr" strict-transport-security
}

check_swagger_body() {
  local body="$1" csp="$2" tags inline_present authorization

  if grep -Fq '<title>Swagger UI</title>' "$body" && grep -Fq 'swagger-ui-bundle.js' "$body"; then
    record_result PASS swagger swagger_document_body "Swagger UI HTML" "Swagger UI HTML"
  else
    record_result FAIL swagger swagger_document_body "Swagger UI HTML" "not Swagger UI HTML"
    mark_indeterminate swagger_document_body "served Swagger UI HTML" malformed_or_placeholder
  fi

  tags="$(script_opening_tags "$body")"
  record_equals swagger swagger_script_tags "$EXPECTED_SWAGGER_SCRIPT_TAGS" "$tags"

  inline_present="$(script_inline_present "$tags")"
  printf 'SWAGGER_INLINE_SCRIPT_PRESENT=%s\n' "$inline_present"
  record_equals swagger swagger_inline_script_present "$EXPECTED_SWAGGER_INLINE_SCRIPT_PRESENT" "$inline_present"

  if [ -z "$csp" ]; then
    printf 'INFO: swagger csp_script_compatibility skipped because CSP is missing; missing CSP is recorded by content_security_policy\n'
  elif [ "$inline_present" = "true" ]; then
    if authorization="$(inline_script_authorization_result "$body" "$csp")"; then
      record_result PASS swagger csp_script_compatibility "each inline script's actual nonce or digest is authorized by script-src" "$authorization"
    else
      record_result FAIL swagger csp_script_compatibility "each inline script's actual nonce or digest is authorized by script-src" "$authorization"
    fi
  elif external_scripts_self_compatible "$body" "$csp"; then
    record_result PASS swagger csp_script_compatibility "all external scripts are same-origin and allowed by script-src 'self'" "authorized external script sources"
  else
    record_result FAIL swagger csp_script_compatibility "all external scripts are same-origin and allowed by script-src 'self'" "$csp"
  fi
}

check_surface() {
  local surface="$1" path="$2"
  local hdr="$TMP_ROOT/${surface}.headers" body="$TMP_ROOT/${surface}.body"
  local http_code csp

  http_code="$(fetch_surface "$surface" "$path" "$hdr" "$body")"
  record_equals "$surface" http_status 200 "$http_code"
  SURFACES_EXERCISED=$((SURFACES_EXERCISED + 1))

  check_security_headers "$surface" "$hdr"

  if [ "$surface" = "swagger" ]; then
    csp="$(header_value "$hdr" content-security-policy)"
    check_swagger_body "$body" "$csp"
  fi
}

self_test_result() {
  local status="$1" name="$2" detail="$3"
  SELF_TESTS_RUN=$((SELF_TESTS_RUN + 1))
  if [ "$status" = "PASS" ]; then
    printf '[PASS] self_test %s %s\n' "$name" "$detail"
  else
    SELF_TESTS_FAILED=$((SELF_TESTS_FAILED + 1))
    printf '[FAIL] self_test %s %s\n' "$name" "$detail" >&2
  fi
}

self_test_equals() {
  local name="$1" expected="$2" actual="$3"
  if [ "$actual" = "$expected" ]; then
    self_test_result PASS "$name" "expected=$expected actual=$actual"
  else
    self_test_result FAIL "$name" "expected=$expected actual=$actual"
  fi
}

self_test_contains() {
  local name="$1" haystack="$2" needle="$3"
  if printf '%s\n' "$haystack" | grep -Fq "$needle"; then
    self_test_result PASS "$name" "contains=$needle"
  else
    self_test_result FAIL "$name" "missing=$needle"
  fi
}

run_parser_self_tests() {
  local fixture_dir="$TMP_ROOT/self_test_fixtures"
  local empty_html="$fixture_dir/empty.html"
  local inline_html="$fixture_dir/inline.html"
  local multiple_inline_html="$fixture_dir/multiple_inline.html"
  local nonce_html="$fixture_dir/nonce.html"
  local external_html="$fixture_dir/external.html"
  local header_fixture="$fixture_dir/headers.txt"
  local result_log="$fixture_dir/result.log"
  local tags output authorization
  local saved_checks_run="$CHECKS_RUN"
  local saved_checks_failed="$CHECKS_FAILED"
  local saved_indeterminate="$INDETERMINATE"
  local known_sha256="bhHHL3z2vDgxUt0W3dWQOrprscmda2Y5pLsLg4GF+pI="
  mkdir -p "$fixture_dir"

  printf '%s\n' '<html><title>placeholder</title></html>' >"$empty_html"
  if tags="$(script_opening_tags "$empty_html")"; then
    self_test_equals zero_script_tags_empty "" "$tags"
  else
    self_test_result FAIL zero_script_tags_nonfatal "script_opening_tags returned non-zero"
  fi

  CHECKS_RUN=0
  CHECKS_FAILED=0
  INDETERMINATE=0
  {
    check_swagger_body "$empty_html" ""
    printf 'SUMMARY checks_run=%s checks_failed=%s indeterminate=%s\n' \
      "$CHECKS_RUN" "$CHECKS_FAILED" "$INDETERMINATE"
  } >"$result_log" 2>&1
  output="$(<"$result_log")"
  CHECKS_RUN="$saved_checks_run"
  CHECKS_FAILED="$saved_checks_failed"
  INDETERMINATE="$saved_indeterminate"
  self_test_contains malformed_swagger_classified "$output" \
    'INDETERMINATE swagger_document_body expected=served Swagger UI HTML actual=malformed_or_placeholder'
  self_test_contains malformed_swagger_reaches_summary "$output" \
    'SUMMARY checks_run=3 checks_failed=2 indeterminate=1'

  printf '%s\n' \
    'HTTP/1.1 200 OK' \
    "content-security-policy: $EXPECTED_CSP" \
    "x-content-type-options: $EXPECTED_X_CONTENT_TYPE_OPTIONS" \
    "referrer-policy: $EXPECTED_REFERRER_POLICY" \
    "permissions-policy: $EXPECTED_PERMISSIONS_POLICY" \
    'strict-transport-security:' \
    >"$header_fixture"
  CHECKS_RUN=0
  CHECKS_FAILED=0
  {
    check_security_headers fixture "$header_fixture"
    printf 'SUMMARY checks_run=%s checks_failed=%s\n' "$CHECKS_RUN" "$CHECKS_FAILED"
  } >"$result_log" 2>&1
  output="$(<"$result_log")"
  CHECKS_RUN="$saved_checks_run"
  CHECKS_FAILED="$saved_checks_failed"
  self_test_contains empty_hsts_is_present_failure "$output" \
    '[FAIL] fixture hsts_absent expected=absent actual=<empty>'
  self_test_contains empty_hsts_has_single_failure "$output" \
    'SUMMARY checks_run=6 checks_failed=1'

  printf '%s' '<script>alert(1)</script>' >"$inline_html"
  if authorization="$(inline_script_authorization_result "$inline_html" "default-src 'self' 'sha256-$known_sha256'; script-src 'self'" 2>&1)"; then
    self_test_result FAIL hash_outside_script_src_rejected "unexpected=$authorization"
  else
    self_test_equals hash_outside_script_src_rejected unauthorized:inline_script_1 "$authorization"
  fi
  if authorization="$(inline_script_authorization_result "$inline_html" "script-src 'self' 'sha256-AAAAAAAA'" 2>&1)"; then
    self_test_result FAIL wrong_inline_digest_rejected "unexpected=$authorization"
  else
    self_test_equals wrong_inline_digest_rejected unauthorized:inline_script_1 "$authorization"
  fi
  if authorization="$(inline_script_authorization_result "$inline_html" "script-src 'self' 'sha256-$known_sha256'" 2>&1)"; then
    self_test_equals actual_inline_digest_authorized authorized:inline_scripts=1 "$authorization"
  else
    self_test_result FAIL actual_inline_digest_authorized "unexpected=$authorization"
  fi
  printf '%s' '<script>alert(1)</script><script>alert(2)</script>' >"$multiple_inline_html"
  if authorization="$(inline_script_authorization_result "$multiple_inline_html" "script-src 'sha256-$known_sha256'" 2>&1)"; then
    self_test_result FAIL every_inline_script_requires_authorization "unexpected=$authorization"
  else
    self_test_equals every_inline_script_requires_authorization unauthorized:inline_script_2 "$authorization"
  fi

  printf '%s' '<script nonce="right-nonce">alert(1)</script>' >"$nonce_html"
  if authorization="$(inline_script_authorization_result "$nonce_html" "default-src 'nonce-right-nonce'; script-src 'self'" 2>&1)"; then
    self_test_result FAIL nonce_outside_script_src_rejected "unexpected=$authorization"
  else
    self_test_equals nonce_outside_script_src_rejected unauthorized:inline_script_1 "$authorization"
  fi
  if authorization="$(inline_script_authorization_result "$nonce_html" "script-src 'self' 'nonce-right-nonce'" 2>&1)"; then
    self_test_equals actual_inline_nonce_authorized authorized:inline_scripts=1 "$authorization"
  else
    self_test_result FAIL actual_inline_nonce_authorized "unexpected=$authorization"
  fi

  printf '%s' '<script src="./swagger-ui-bundle.js"></script>' >"$external_html"
  if external_scripts_self_compatible "$external_html" "default-src 'none'; script-src 'self'"; then
    self_test_result PASS same_origin_external_script_authorized "relative src accepted by script-src self"
  else
    self_test_result FAIL same_origin_external_script_authorized "relative src rejected"
  fi
  printf '%s' '<script src="https://example.invalid/swagger.js"></script>' >"$external_html"
  if external_scripts_self_compatible "$external_html" "default-src 'none'; script-src 'self'"; then
    self_test_result FAIL cross_origin_external_script_rejected "cross-origin src accepted"
  else
    self_test_result PASS cross_origin_external_script_rejected "cross-origin src rejected"
  fi
}

run_ambient_environment_self_test() {
  local surface path status body
  FLAPJACK_DISABLE_DASHBOARD=1
  export FLAPJACK_DISABLE_DASHBOARD
  start_server

  for surface in dashboard swagger; do
    if [ "$surface" = "dashboard" ]; then
      path="/dashboard/"
    else
      path="/swagger-ui/"
    fi
    body="$TMP_ROOT/self_test_${surface}.body"
    if status="$(curl -sS -o "$body" -w '%{http_code}' "$BASE$path")"; then
      self_test_equals "ambient_disable_${surface}_status" 200 "$status"
    else
      self_test_result FAIL "ambient_disable_${surface}_transport" "curl failed"
    fi
  done
  stop_server
}

run_self_tests() {
  required_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj_security_headers_self_test.XXXXXX")"
  run_parser_self_tests
  build_current_checkout_binary
  run_ambient_environment_self_test
  printf 'SELF_TEST_SUMMARY tests_run=%s tests_failed=%s\n' "$SELF_TESTS_RUN" "$SELF_TESTS_FAILED"
  if [ "$SELF_TESTS_FAILED" -ne 0 ]; then
    exit 1
  fi
}

main() {
  required_tools
  TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj_security_headers.XXXXXX")"
  build_current_checkout_binary
  start_server

  check_surface health /health
  check_surface dashboard /dashboard/
  check_surface swagger /swagger-ui/

  record_equals summary surfaces_exercised 3 "$SURFACES_EXERCISED"
  if [ "$SURFACES_EXERCISED" -ne 3 ]; then
    INDETERMINATE=1
  fi

  printf 'SUMMARY checks_run=%s checks_failed=%s surfaces_exercised=%s indeterminate=%s\n' \
    "$CHECKS_RUN" "$CHECKS_FAILED" "$SURFACES_EXERCISED" "$INDETERMINATE"

  if [ "$INDETERMINATE" -ne 0 ] || [ "$CHECKS_FAILED" -ne 0 ]; then
    exit 1
  fi
}

case "${1:-}" in
  --self-test)
    run_self_tests
    ;;
  "")
    main
    ;;
  *)
    printf 'Usage: %s [--self-test]\n' "$0" >&2
    exit 2
    ;;
esac
