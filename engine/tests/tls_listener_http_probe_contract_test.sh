#!/usr/bin/env bash

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PROBE="$REPO_ROOT/engine/tests/tls_listener_http_probe.sh"

CHECKS_RUN=0
CHECKS_FAILED=0

pass() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  printf '  [PASS] %s\n' "$1"
}

fail() {
  CHECKS_RUN=$((CHECKS_RUN + 1))
  CHECKS_FAILED=$((CHECKS_FAILED + 1))
  printf '  [FAIL] %s\n' "$1" >&2
  if [ -n "${2:-}" ]; then
    printf '         %s\n' "$2" >&2
  fi
}

require_grep() {
  local description="$1" pattern="$2"
  if grep -Eq -- "$pattern" "$PROBE"; then
    pass "$description"
  else
    fail "$description" "missing pattern: $pattern"
  fi
}

require_literal() {
  local description="$1" text="$2"
  if grep -Fq -- "$text" "$PROBE"; then
    pass "$description"
  else
    fail "$description" "missing text: $text"
  fi
}

main() {
  if [ -f "$PROBE" ]; then
    pass 'tls listener probe exists'
  else
    fail 'tls listener probe exists' "$PROBE"
    printf 'SUMMARY checks_run=%s checks_failed=%s\n' "$CHECKS_RUN" "$CHECKS_FAILED"
    return 1
  fi

  if [ -x "$PROBE" ]; then
    pass 'tls listener probe is executable'
  else
    fail 'tls listener probe is executable'
  fi

  require_grep 'probe uses bash shebang' '^#!/usr/bin/env bash$'
  require_grep 'probe enables strict shell mode' '^set -euo pipefail$'
  require_literal 'probe checks all required tools' 'awk bash cargo curl grep head mkdir mktemp openssl sed seq sleep tail tee'
  require_literal 'probe writes durable tls evidence' 'tests/results/tls_listener_evidence'
  require_literal 'probe preserves full cargo build output' 'cargo-build-full.log'
  require_literal 'probe sanitizes temporary evidence paths' '<tmp-root>'
  require_literal 'probe keeps required cargo build display shape' 'cargo build -p flapjack-server 2>&1 | tail -30'
  require_literal 'probe accepts FLAPJACK_BIN override' 'FLAPJACK_BIN'
  require_literal 'probe generates localhost and ip SAN certificate' 'subjectAltName=DNS:localhost,IP:127.0.0.1'
  require_literal 'probe uses portable certificate metadata helper' 'write_certificate_metadata'
  if grep -Eq -- 'openssl x509 .* -ext ' "$PROBE"; then
    fail 'probe avoids openssl x509 -ext portability trap' 'openssl x509 -ext is unsupported by /usr/bin/openssl on macOS'
  else
    pass 'probe avoids openssl x509 -ext portability trap'
  fi
  require_literal 'probe passes TLS certificate flag to binary' '--ssl-cert-path'
  require_literal 'probe passes TLS key flag to binary' '--ssl-key-path'
  require_literal 'probe parses https startup banner' 'Local: https://127.0.0.1:<port>'
  require_literal 'probe waits for verified https readiness' '--cacert "$CERT_PATH"'
  require_grep 'probe captures shown served certificate chain' 'openssl s_client .* -showcerts'
  require_literal 'probe proves plaintext is rejected on tls port' 'http://127.0.0.1:${tls_port}/health'
  require_literal 'probe reuses plaintext readiness helper' 'common/wait_for_flapjack.sh'
  require_literal 'probe checks plaintext startup banner' 'Local: http://127.0.0.1:<port>'
  require_literal 'probe verifies plaintext api json fields' '"status":"ok"'
  require_literal 'probe records final status' 'status.txt'
  require_literal 'probe cleanup targets exact server pid' 'kill "$TLS_SERVER_PID"'

  printf 'SUMMARY checks_run=%s checks_failed=%s\n' "$CHECKS_RUN" "$CHECKS_FAILED"
  [ "$CHECKS_FAILED" -eq 0 ]
}

main "$@"
