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
    npm audit --package-lock-only --omit=dev --json
) >"$AUDIT_JSON"

node - "$AUDIT_JSON" <<'NODE'
const fs = require("fs");

try {
  const report = JSON.parse(fs.readFileSync(process.argv[2], "utf8"));
  if (report.auditReportVersion !== 2) {
    throw new Error(`unsupported auditReportVersion: ${String(report.auditReportVersion)}`);
  }
  const counts = report.metadata?.vulnerabilities;
  const vulnerabilities = report.vulnerabilities;
  const severities = ["info", "low", "moderate", "high", "critical"];
  const severityRank = new Map(severities.map((severity, index) => [severity, index]));
  // This is the live exception owner. Package binding prevents an accepted
  // GHSA from masking a malformed audit entry for an unrelated dependency.
  const acceptedModerateAdvisories = new Map([
    ["GHSA-wrjc-x8rr-h8h6", {
      packageName: "react-router",
      rationale: "The maintained React Router v6 line has no fixed release.",
      reachability: "The navigation APIs exist, but no attacker-supplied path reaches them; destinations are hard-coded or URL-encoded same-dashboard paths.",
      upgradeConstraint: "Upstream patches react-router in 7.18.0; npm recommends the semver-major react-router-dom 7.18.2 migration.",
      removalTrigger: "Remove after migration to React Router >=7.18.0, or when a compatible v6 release is outside the affected range and the audit is clean.",
    }],
    ["GHSA-337j-9hxr-rhxg", {
      packageName: "react-router",
      rationale: "The maintained React Router v6 line has no fixed release.",
      reachability: "Unreachable: upstream excludes declarative BrowserRouter mode, and the dashboard has no SSR hydration or deserializeErrors().",
      upgradeConstraint: "Upstream patches react-router in 7.18.0; npm recommends the semver-major react-router-dom 7.18.2 migration.",
      removalTrigger: "Remove after migration to React Router >=7.18.0, or when a compatible v6 release is outside the affected range and the audit is clean.",
    }],
    ["GHSA-jjmj-jmhj-qwj2", {
      packageName: "react-router-dom",
      rationale: "No fixed React Router v6 package is currently published.",
      reachability: "The application has no open redirect accepting an attacker-supplied target, which upstream names as the exploit precondition.",
      upgradeConstraint: "Upstream lists no patched react-router-dom v6; npm recommends the semver-major 7.18.2 migration.",
      removalTrigger: "Remove when a patched v6 release beyond 6.30.4 is published and audits clean, or after migration to React Router >=7.18.0.",
    }],
  ]);
  const requiredDispositionFields = [
    "packageName",
    "rationale",
    "reachability",
    "upgradeConstraint",
    "removalTrigger",
  ];

  if (!counts) {
    throw new Error("missing metadata.vulnerabilities");
  }
  if (!vulnerabilities || typeof vulnerabilities !== "object" || Array.isArray(vulnerabilities)) {
    throw new Error("missing vulnerabilities");
  }

  for (const severity of severities) {
    if (!Number.isInteger(counts[severity]) || counts[severity] < 0) {
      throw new Error(`invalid ${severity} count`);
    }
  }
  if (!Number.isInteger(counts.total) || counts.total < 0) {
    throw new Error("invalid total count");
  }
  const entryCounts = Object.fromEntries(severities.map((severity) => [severity, 0]));
  for (const [packageName, vulnerability] of Object.entries(vulnerabilities)) {
    if (!vulnerability || typeof vulnerability !== "object" || Array.isArray(vulnerability)) {
      throw new Error(`invalid vulnerability entry for ${packageName}`);
    }
    if (vulnerability.name !== packageName) {
      throw new Error(`invalid vulnerability name for ${packageName}`);
    }
    if (!severities.includes(vulnerability.severity)) {
      throw new Error(`invalid vulnerability severity for ${packageName}`);
    }
    entryCounts[vulnerability.severity] += 1;
  }
  // npm metadata counts affected package nodes. Reconciling every severity
  // prevents contradictory metadata from hiding a package-level finding.
  for (const severity of severities) {
    if (entryCounts[severity] !== counts[severity]) {
      throw new Error(
        `${severity} package count mismatch: metadata=${counts[severity]} entries=${entryCounts[severity]}`,
      );
    }
  }
  const total = severities.reduce((sum, severity) => sum + counts[severity], 0);
  if (counts.total !== total) {
    throw new Error(`total vulnerability count mismatch: metadata=${counts.total} entries=${total}`);
  }
  for (const [advisoryId, disposition] of acceptedModerateAdvisories) {
    for (const field of requiredDispositionFields) {
      if (typeof disposition[field] !== "string" || disposition[field].length === 0) {
        throw new Error(`accepted advisory ${advisoryId} is missing ${field}`);
      }
    }
  }

  const unresolvedModeratePackages = new Set();
  const invalidModerateAdvisories = new Set();
  const acceptedAdvisoryPackageMismatches = new Set();

  function advisoryId(via) {
    const match = typeof via.url === "string"
      ? via.url.match(/\/advisories\/(GHSA-[0-9a-z-]+)(?:[/?#]|$)/i)
      : null;
    return match?.[1] ?? null;
  }

  function validateViaEntries(packageName, vulnerability) {
    if (!Array.isArray(vulnerability.via)) {
      throw new Error(`invalid via list for ${packageName}`);
    }

    for (const via of vulnerability.via) {
      if (typeof via === "string") {
        if (via.length === 0 || !Object.hasOwn(vulnerabilities, via)) {
          throw new Error(`unresolved via reference for ${packageName}: ${via || "<empty>"}`);
        }
        const referencedSeverity = vulnerabilities[via].severity;
        if (severityRank.get(referencedSeverity) > severityRank.get(vulnerability.severity)) {
          throw new Error(
            `via severity exceeds package severity for ${packageName}: package=${vulnerability.severity} via=${referencedSeverity}`,
          );
        }
        continue;
      }

      if (!via || typeof via !== "object" || Array.isArray(via)) {
        throw new Error(`invalid via entry for ${packageName}`);
      }
      if (!severities.includes(via.severity)) {
        throw new Error(`invalid via advisory severity for ${packageName}: ${String(via.severity)}`);
      }
      if (typeof via.url !== "string" || via.url.length === 0) {
        throw new Error(`invalid via advisory for ${packageName}`);
      }
      if (severityRank.get(via.severity) > severityRank.get(vulnerability.severity)) {
        throw new Error(
          `via severity exceeds package severity for ${packageName}: package=${vulnerability.severity} via=${via.severity}`,
        );
      }
    }
  }

  for (const [packageName, vulnerability] of Object.entries(vulnerabilities)) {
    validateViaEntries(packageName, vulnerability);
  }

  function hasConcreteAdvisory(packageName, visiting = new Set()) {
    if (visiting.has(packageName)) {
      return false;
    }

    const nextVisiting = new Set(visiting).add(packageName);
    return vulnerabilities[packageName].via.some((via) => (
      typeof via === "string" ? hasConcreteAdvisory(via, nextVisiting) : true
    ));
  }

  for (const packageName of Object.keys(vulnerabilities)) {
    if (!hasConcreteAdvisory(packageName)) {
      throw new Error(`unresolved via chain for ${packageName}`);
    }
  }

  function moderateAdvisoriesForPackage(packageName, visiting = new Set()) {
    if (visiting.has(packageName)) {
      return new Set();
    }

    const vulnerability = vulnerabilities[packageName];
    if (!vulnerability || !Array.isArray(vulnerability.via)) {
      unresolvedModeratePackages.add(packageName);
      return new Set();
    }

    const nextVisiting = new Set(visiting).add(packageName);
    const ids = new Set();
    for (const via of vulnerability.via) {
      if (typeof via === "string") {
        // npm represents an indirect finding as the affected package name;
        // follow that edge until the report's concrete advisory object.
        for (const inheritedId of moderateAdvisoriesForPackage(via, nextVisiting)) {
          ids.add(inheritedId);
        }
      } else if (via && via.severity === "moderate") {
        const id = advisoryId(via);
        if (id) {
          ids.add(id);
          const disposition = acceptedModerateAdvisories.get(id);
          if (disposition && disposition.packageName !== packageName) {
            acceptedAdvisoryPackageMismatches.add(
              `${id} expected=${disposition.packageName} actual=${packageName}`,
            );
          }
        } else {
          invalidModerateAdvisories.add(String(via.source ?? `${packageName}:unknown`));
        }
      }
    }
    return ids;
  }

  const foundModerateAdvisories = new Set();
  for (const [packageName, vulnerability] of Object.entries(vulnerabilities)) {
    const ids = moderateAdvisoriesForPackage(packageName);
    if (vulnerability.severity === "moderate" && ids.size === 0) {
      unresolvedModeratePackages.add(packageName);
    }
    for (const id of ids) {
      foundModerateAdvisories.add(id);
    }
  }

  const acceptedFound = [...foundModerateAdvisories]
    .filter((id) => acceptedModerateAdvisories.has(id))
    .sort();
  const unrecognized = [...foundModerateAdvisories]
    .filter((id) => !acceptedModerateAdvisories.has(id))
    .sort();

  console.log(
    `Audit denominator: info=${counts.info} low=${counts.low} moderate=${counts.moderate} high=${counts.high} critical=${counts.critical} total=${total}`,
  );
  if (acceptedFound.length > 0) {
    console.log(`Accepted moderate advisories: ${acceptedFound.join(", ")}`);
  }
  if (unrecognized.length > 0) {
    console.error(`Unrecognized moderate advisories: ${unrecognized.join(", ")}`);
  }
  if (invalidModerateAdvisories.size > 0) {
    console.error(
      `Moderate advisories without exact GHSA IDs: ${[...invalidModerateAdvisories].sort().join(", ")}`,
    );
  }
  if (acceptedAdvisoryPackageMismatches.size > 0) {
    console.error(
      `Accepted advisory package mismatch: ${[...acceptedAdvisoryPackageMismatches].sort().join(", ")}`,
    );
  }
  if (unresolvedModeratePackages.size > 0) {
    console.error(
      `Moderate packages without resolvable advisory IDs: ${[...unresolvedModeratePackages].sort().join(", ")}`,
    );
  }
  if (
    counts.high > 0 ||
    counts.critical > 0 ||
    unrecognized.length > 0 ||
    invalidModerateAdvisories.size > 0 ||
    acceptedAdvisoryPackageMismatches.size > 0 ||
    unresolvedModeratePackages.size > 0
  ) {
    process.exitCode = 1;
  }
} catch (error) {
  console.error(`Dashboard audit gate could not validate npm audit JSON: ${error.message}`);
  process.exit(1);
}
NODE
PARSE_STATUS=$?

# npm exits nonzero for every reported vulnerability. The validated JSON policy
# above decides whether exact accepted moderates may pass.
exit "$PARSE_STATUS"
