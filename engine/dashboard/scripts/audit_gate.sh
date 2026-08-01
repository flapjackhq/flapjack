#!/bin/bash

if ! command -v npm >/dev/null 2>&1; then
  echo "Dashboard audit gate requires npm, but npm is unavailable" >&2
  exit 1
fi

if ! command -v node >/dev/null 2>&1; then
  echo "Dashboard audit gate requires node to validate npm audit output" >&2
  exit 1
fi

AUDIT_TARGET="${1:-}"
if [ -z "$AUDIT_TARGET" ]; then
  REPO_ROOT="$(git rev-parse --show-toplevel 2>/dev/null)" || {
    echo "Dashboard audit gate could not resolve the repository root" >&2
    exit 1
  }
  AUDIT_TARGET="$REPO_ROOT/engine/dashboard"
fi

AUDIT_JSON="$(mktemp "${TMPDIR:-/tmp}/flapjack_npm_audit.XXXXXX")" || {
  echo "Dashboard audit gate could not create its audit output file" >&2
  exit 1
}
trap 'rm -f "$AUDIT_JSON"' EXIT

(
  cd "$AUDIT_TARGET" &&
    npm audit --omit=dev --audit-level=high --json
) >"$AUDIT_JSON"
AUDIT_STATUS=$?

node - "$AUDIT_JSON" <<'NODE'
const fs = require("fs");

try {
  const report = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
  const counts = report.metadata?.vulnerabilities;
  const severities = ["low", "moderate", "high", "critical"];

  if (!counts) {
    throw new Error("missing metadata.vulnerabilities");
  }

  for (const severity of severities) {
    if (!Number.isInteger(counts[severity]) || counts[severity] < 0) {
      throw new Error(`invalid ${severity} count`);
    }
  }

  const total = severities.reduce((sum, severity) => sum + counts[severity], 0);
  console.log(
    `Audit denominator: low=${counts.low} moderate=${counts.moderate} high=${counts.high} critical=${counts.critical} total=${total}`,
  );
  if (counts.high > 0 || counts.critical > 0) {
    process.exitCode = 1;
  }
} catch (error) {
  console.error(`Dashboard audit gate could not validate npm audit JSON: ${error.message}`);
  process.exit(1);
}
NODE
PARSE_STATUS=$?

if [ "$PARSE_STATUS" -ne 0 ]; then
  if [ "$AUDIT_STATUS" -ne 0 ]; then
    exit "$AUDIT_STATUS"
  fi
  exit 1
fi

exit "$AUDIT_STATUS"
