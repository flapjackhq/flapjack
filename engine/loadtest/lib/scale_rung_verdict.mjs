#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const NAME_PREFIX_P95_LIMIT_MS = 50;
export const BLENDED_P95_LIMIT_MS = 100;
export const SEARCH_SAMPLES_PER_TYPE = 30;
export const REQUIRED_QUERY_TYPES = Object.freeze([
  "text",
  "typo",
  "multi_word",
  "facet",
  "filter",
  "geo",
  "highlight",
]);

function validLatency(value) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

export function evaluateScaleRung(searchArtifact) {
  const namePrefixP95Ms = searchArtifact?.queryTypes?.text?.p95;
  const blendedP95Ms = searchArtifact?.overall?.p95;
  const invalidFields = [];

  if (!validLatency(namePrefixP95Ms)) {
    invalidFields.push("namePrefixP95");
  }
  if (!validLatency(blendedP95Ms)) {
    invalidFields.push("blendedP95");
  }
  if (invalidFields.length > 0) {
    return {
      verdict: "INVALID",
      reasons: invalidFields,
    };
  }

  let summedQueryCount = 0;
  for (const queryType of REQUIRED_QUERY_TYPES) {
    if (searchArtifact?.queryTypes?.[queryType]?.count !== SEARCH_SAMPLES_PER_TYPE) {
      invalidFields.push(`queryTypes.${queryType}.count`);
    } else {
      summedQueryCount += searchArtifact.queryTypes[queryType].count;
    }
  }
  const expectedOverallCount = REQUIRED_QUERY_TYPES.length * SEARCH_SAMPLES_PER_TYPE;
  if (
    searchArtifact?.overall?.count !== expectedOverallCount ||
    summedQueryCount !== expectedOverallCount ||
    searchArtifact?.overall?.count !== summedQueryCount
  ) {
    invalidFields.push("overall.count");
  }
  if (invalidFields.length > 0) {
    return {
      verdict: "INVALID",
      reasons: invalidFields,
    };
  }

  const reasons = [];
  if (namePrefixP95Ms > NAME_PREFIX_P95_LIMIT_MS) {
    reasons.push("namePrefixP95");
  }
  if (blendedP95Ms > BLENDED_P95_LIMIT_MS) {
    reasons.push("blendedP95");
  }

  return {
    verdict: reasons.length === 0 ? "PASS" : "FAIL",
    reasons,
    limits: {
      namePrefixP95Ms: NAME_PREFIX_P95_LIMIT_MS,
      blendedP95Ms: BLENDED_P95_LIMIT_MS,
    },
    observed: {
      namePrefixP95Ms,
      blendedP95Ms,
    },
  };
}

function runCli() {
  const [flag, artifactPath] = process.argv.slice(2);
  if (flag === "--help" || flag === "-h") {
    process.stdout.write("Usage: scale_rung_verdict.mjs --search-artifact <path>\n");
    return;
  }
  if (flag !== "--search-artifact" || !artifactPath) {
    throw new Error("Usage: scale_rung_verdict.mjs --search-artifact <path>");
  }

  const artifact = JSON.parse(fs.readFileSync(artifactPath, "utf8"));
  const result = evaluateScaleRung(artifact);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  // FAIL is a valid measured outcome. INVALID is a broken evidence contract.
  if (result.verdict === "INVALID") {
    process.exitCode = 1;
  }
}

const currentFilePath = fileURLToPath(import.meta.url);
const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (currentFilePath === invokedPath) {
  try {
    runCli();
  } catch (error) {
    console.error(`FAIL: ${error.message}`);
    process.exit(1);
  }
}
