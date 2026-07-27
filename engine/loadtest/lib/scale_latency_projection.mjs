#!/usr/bin/env node

import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  BLENDED_P95_LIMIT_MS,
  NAME_PREFIX_P95_LIMIT_MS,
  PER_QUERY_TYPE_P95_LIMIT_MS,
  REQUIRED_QUERY_TYPES,
  SEARCH_SAMPLES_PER_TYPE,
} from "./scale_rung_verdict.mjs";

export const FINAL_CERTIFICATION_TARGET = 64_000_000;

const EXPECTED_INSTANCE_TYPE = "i4i.4xlarge";
const EXPECTED_BACKING_MODEL = "Amazon EC2 NVMe Instance Storage";

function invalid(reason) {
  return {
    verdict: "INVALID",
    reasons: [reason],
  };
}

function round(value) {
  return Number(value.toFixed(6));
}

function fitLine(points) {
  const count = points.length;
  const meanX = points.reduce((sum, point) => sum + point.x, 0) / count;
  const meanY = points.reduce((sum, point) => sum + point.y, 0) / count;
  const numerator = points.reduce((sum, point) => sum + (point.x - meanX) * (point.y - meanY), 0);
  const denominator = points.reduce((sum, point) => sum + (point.x - meanX) ** 2, 0);
  return {
    a: meanY - (numerator / denominator) * meanX,
    b: numerator / denominator,
  };
}

function validP95(value) {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function validTarget(value) {
  return Number.isSafeInteger(value) && value > 0;
}

function validLocality(locality) {
  return (
    locality?.verdict === "GO" &&
    locality?.reference?.instanceType === EXPECTED_INSTANCE_TYPE &&
    locality?.reference?.backingModel === EXPECTED_BACKING_MODEL
  );
}

function validateCompletedRungs(completedRungs) {
  if (!Array.isArray(completedRungs) || completedRungs.length < 2) {
    return "completedRungs";
  }

  let previousTarget = 0;
  for (const rung of completedRungs) {
    if (!validTarget(rung?.targetCount) || rung.targetCount <= previousTarget) {
      return "completedRungs.targetCount";
    }
    previousTarget = rung.targetCount;

    for (const queryType of REQUIRED_QUERY_TYPES) {
      const summary = rung?.queryTypes?.[queryType];
      if (summary?.count !== SEARCH_SAMPLES_PER_TYPE || !validP95(summary?.p95)) {
        return `completedRungs.queryTypes.${queryType}`;
      }
    }
    const expectedOverallCount = REQUIRED_QUERY_TYPES.length * SEARCH_SAMPLES_PER_TYPE;
    if (rung?.overall?.count !== expectedOverallCount || !validP95(rung?.overall?.p95)) {
      return "completedRungs.overall";
    }
  }
  return null;
}

function validateContract(input) {
  if (input?.profile !== "compact" && input?.profile !== "standard") {
    return "profile";
  }
  const completedRungError = validateCompletedRungs(input?.completedRungs);
  if (completedRungError !== null) {
    return completedRungError;
  }
  const largestCompletedTarget = input.completedRungs.at(-1).targetCount;
  if (!validTarget(input?.nextTarget) || input.nextTarget <= largestCompletedTarget) {
    return "nextTarget";
  }
  return null;
}

function familyLimit(familyKey) {
  if (familyKey === "text") {
    return NAME_PREFIX_P95_LIMIT_MS;
  }
  if (familyKey === "blended") {
    return BLENDED_P95_LIMIT_MS;
  }
  return PER_QUERY_TYPE_P95_LIMIT_MS;
}

function familyReason(targetCount, familyKey) {
  if (familyKey === "blended") {
    return `target:${targetCount}:blended`;
  }
  return `target:${targetCount}:queryTypes.${familyKey}`;
}

function observedFamilyValue(rung, familyKey) {
  if (familyKey === "blended") {
    return rung.overall.p95;
  }
  return rung.queryTypes[familyKey].p95;
}

function projectFamily(completedRungs, familyKey, targetCount) {
  const points = completedRungs.map((rung) => ({
    x: rung.targetCount,
    y: observedFamilyValue(rung, familyKey),
  }));
  const observedP95Ms = points.map((point) => point.y);
  const maxObservedP95Ms = Math.max(...observedP95Ms);
  const line = fitLine(points);
  // These two safeguards jointly enforce "never project an improvement" and make
  // an already-breached measured family refuse every forward dispatch.
  const rawProjectedP95Ms = line.b <= 0 ? maxObservedP95Ms : line.a + line.b * targetCount;
  const projectedP95Ms = round(Math.max(maxObservedP95Ms, rawProjectedP95Ms));
  const limitMs = familyLimit(familyKey);
  const reason = familyReason(targetCount, familyKey);

  return {
    limitMs,
    maxObservedP95Ms: round(maxObservedP95Ms),
    projectedP95Ms,
    verdict: projectedP95Ms > limitMs ? "REFUSE" : "GO",
    reason,
  };
}

function projectionTargets(nextTarget) {
  return [...new Set([nextTarget, FINAL_CERTIFICATION_TARGET])];
}

export function evaluateScaleLatencyProjection(input) {
  if (!validLocality(input?.locality)) {
    return invalid("locality");
  }

  const contractError = validateContract(input);
  if (contractError !== null) {
    return invalid(contractError);
  }

  const targets = projectionTargets(input.nextTarget);
  const familyKeys = [...REQUIRED_QUERY_TYPES, "blended"];
  const projections = targets.map((targetCount) => {
    const families = Object.fromEntries(
      familyKeys.map((familyKey) => [
        familyKey,
        projectFamily(input.completedRungs, familyKey, targetCount),
      ]),
    );
    // REFUSE is the semantic peer of scale_projection.mjs's NO_GO.
    const verdict = Object.values(families).some((family) => family.verdict === "REFUSE")
      ? "REFUSE"
      : "GO";
    return { targetCount, families, verdict };
  });
  const reasons = projections.flatMap((projection) =>
    Object.entries(projection.families)
      .filter(([, family]) => family.verdict === "REFUSE")
      .map(([, family]) => family.reason),
  );

  return {
    verdict: reasons.length > 0 ? "REFUSE" : "GO",
    reasons,
    profile: input.profile,
    completedTargets: input.completedRungs.map((rung) => rung.targetCount),
    projectionTargets: targets,
    projections,
  };
}

function parseArgs(argv) {
  const inputIndex = argv.indexOf("--input-json");
  if (inputIndex === -1 || argv[inputIndex + 1] === undefined) {
    throw new Error("usage: scale_latency_projection.mjs --input-json '<json>'");
  }
  return JSON.parse(argv[inputIndex + 1]);
}

const currentFilePath = fileURLToPath(import.meta.url);
const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (currentFilePath === invokedPath) {
  try {
    const result = evaluateScaleLatencyProjection(parseArgs(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    if (result.verdict === "REFUSE") {
      process.exitCode = 1;
    }
    if (result.verdict === "INVALID") {
      process.exitCode = 2;
    }
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 2;
  }
}
