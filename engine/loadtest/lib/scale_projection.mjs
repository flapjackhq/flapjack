#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const REQUIRED_PROBE_TARGETS = Object.freeze([1_000_000, 4_000_000, 8_000_000]);
export const REQUIRED_PROJECTION_TARGETS = Object.freeze([32_000_000, 64_000_000]);
export const DEFAULT_IMPORT_BUDGET_SECONDS = 12 * 60 * 60;
export const PROBE_WORKLOADS = Object.freeze(["import", "bulk_build"]);

const EXPECTED_INSTANCE_TYPE = "i4i.4xlarge";
const EXPECTED_BACKING_MODEL = "Amazon EC2 NVMe Instance Storage";

function invalid(reason) {
  return {
    verdict: "INVALID",
    reasons: [reason],
  };
}

function sameNumbers(actual, expected) {
  return (
    Array.isArray(actual) &&
    actual.length === expected.length &&
    actual.every((value, index) => value === expected[index])
  );
}

function round(value) {
  return Number(value.toFixed(6));
}

export function evaluateScaleProjection(input) {
  if (
    input?.locality?.verdict !== "GO" ||
    input?.locality?.reference?.instanceType !== EXPECTED_INSTANCE_TYPE ||
    input?.locality?.reference?.backingModel !== EXPECTED_BACKING_MODEL
  ) {
    return invalid("locality");
  }
  if (input?.profile !== "compact" && input?.profile !== "standard") {
    return invalid("profile");
  }
  if (!Array.isArray(input?.probes)) {
    return invalid("probeTargets");
  }
  const workload = input?.workload ?? "import";
  if (!PROBE_WORKLOADS.includes(workload)) {
    return invalid("workload");
  }
  const probeTargets = input.probes.map((probe) => probe?.targetCount);
  if (!sameNumbers(probeTargets, REQUIRED_PROBE_TARGETS)) {
    return invalid("probeTargets");
  }
  if (!sameNumbers(input?.targets, REQUIRED_PROJECTION_TARGETS)) {
    return invalid("projectionTargets");
  }
  if (input?.budgetSeconds !== DEFAULT_IMPORT_BUDGET_SECONDS) {
    return invalid("budgetSeconds");
  }

  let expectedStart = 0;
  for (const probe of input.probes) {
    const expectedProbeStart = workload === "bulk_build" ? 0 : expectedStart;
    const evidenceIsExact =
      probe?.profile === input.profile &&
      probeRunPurposeMatchesWorkload(probe?.runPurpose, workload) &&
      (probe?.workload ?? "import") === workload &&
      probe?.startingCount === expectedProbeStart &&
      probe?.trancheSize === probe.targetCount - probe.startingCount &&
      probe?.finalCount === probe.targetCount &&
      typeof probe?.docsPerSecond === "number" &&
      Number.isFinite(probe.docsPerSecond) &&
      probe.docsPerSecond > 0 &&
      typeof probe?.importWallClockMs === "number" &&
      Number.isFinite(probe.importWallClockMs) &&
      probe.importWallClockMs > 0 &&
      probe?.sentinels === "PASS";
    if (!evidenceIsExact) {
      return invalid("probeEvidence");
    }
    expectedStart = probe.targetCount;
  }

  const pairExponents = [];
  for (let index = 1; index < input.probes.length; index += 1) {
    const previous = input.probes[index - 1];
    const current = input.probes[index];
    if (current.docsPerSecond >= previous.docsPerSecond) {
      pairExponents.push(0);
      continue;
    }
    pairExponents.push(
      Math.log(previous.docsPerSecond / current.docsPerSecond) /
        Math.log(current.targetCount / previous.targetCount),
    );
  }
  const degradationExponent = Math.max(0, ...pairExponents);
  // Using the slowest observed rate as the 8M baseline prevents a late recovery
  // from erasing an earlier degradation signal.
  const baseDocsPerSecond = Math.min(...input.probes.map((probe) => probe.docsPerSecond));
  const baseCount = REQUIRED_PROBE_TARGETS.at(-1);
  const observedSeconds =
    workload === "bulk_build"
      ? input.probes.at(-1).importWallClockMs / 1000
      : input.probes.reduce((sum, probe) => sum + probe.importWallClockMs, 0) / 1000;

  const projections = input.targets.map((targetCount) => {
    const projectedTargetDocsPerSecond =
      baseDocsPerSecond * (targetCount / baseCount) ** -degradationExponent;
    const remainingRecords = workload === "bulk_build" ? targetCount : targetCount - baseCount;
    // Charge the whole remaining tranche at its projected terminal rate. This is
    // deliberately more conservative than integrating the faster early portion.
    const projectedRemainingSeconds = remainingRecords / projectedTargetDocsPerSecond;
    const projectedSeconds =
      workload === "bulk_build"
        ? projectedRemainingSeconds
        : observedSeconds + projectedRemainingSeconds;
    return {
      targetCount,
      remainingRecords,
      projectedTargetDocsPerSecond: round(projectedTargetDocsPerSecond),
      projectedRemainingSeconds: round(projectedRemainingSeconds),
      projectedTotalSeconds: round(projectedSeconds),
      projectedSeconds: round(projectedSeconds),
      verdict: projectedSeconds <= input.budgetSeconds ? "GO" : "NO_GO",
    };
  });

  return {
    verdict: projections.every((projection) => projection.verdict === "GO")
      ? "GO"
      : "NO_GO",
    reasons: projections
      .filter((projection) => projection.verdict === "NO_GO")
      .map((projection) => `target:${projection.targetCount}`),
    profile: input.profile,
    workload,
    budgetSeconds: input.budgetSeconds,
    probeTargets,
    baseCount,
    baseDocsPerSecond: round(baseDocsPerSecond),
    observedSeconds: round(observedSeconds),
    degradationDetected: degradationExponent > 0,
    degradationExponent: round(degradationExponent),
    projections,
  };
}

function probeRunPurposeMatchesWorkload(runPurpose, workload) {
  if (workload === "import") {
    return runPurpose === "throughput_probe";
  }
  return runPurpose === "bulk_build_throughput_probe";
}

function runCli() {
  const [flag, inputPath] = process.argv.slice(2);
  if (flag === "--help" || flag === "-h") {
    process.stdout.write("Usage: scale_projection.mjs --input-file <path>\n");
    return;
  }
  if (flag !== "--input-file" || !inputPath) {
    throw new Error("Usage: scale_projection.mjs --input-file <path>");
  }

  const result = evaluateScaleProjection(JSON.parse(fs.readFileSync(inputPath, "utf8")));
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
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
