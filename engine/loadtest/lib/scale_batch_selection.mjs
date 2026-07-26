#!/usr/bin/env node

import { readFile } from "node:fs/promises";
import { pathToFileURL } from "node:url";

const CONTROL_BATCH_SIZE = 1000;
const CANDIDATE_BATCH_SIZE = 10000;
const PROBE_TARGET = 250000;

function isPositiveFinite(value) {
  return Number.isFinite(value) && value > 0;
}

function hasValidLatencyWindows(windows) {
  return (
    windows !== null &&
    typeof windows === "object" &&
    ["first", "middle", "last"].every(
      (name) =>
        windows[name] !== null &&
        typeof windows[name] === "object" &&
        Number.isInteger(windows[name].count) &&
        windows[name].count > 0 &&
        isPositiveFinite(windows[name].p50) &&
        isPositiveFinite(windows[name].p95),
    ) &&
    Number.isFinite(windows.lastToFirstP50Ratio) &&
    windows.lastToFirstP50Ratio > 0
  );
}

function validateMetrics(label, value, expectedBatchSize) {
  const valid =
    value !== null &&
    typeof value === "object" &&
    value.profile === "compact" &&
    value.targetCount === PROBE_TARGET &&
    value.finalCount === PROBE_TARGET &&
    value.batchSize === expectedBatchSize &&
    value.sentinels === "PASS" &&
    isPositiveFinite(value.docsPerSecond) &&
    hasValidLatencyWindows(value.importLatencyWindows);

  if (!valid) {
    throw new Error(`${label} metrics are invalid`);
  }
}

/**
 * Apply the frozen reference A/B rule to two exact 250k specimens.
 */
export function selectReferenceBatchSize({ control, candidate }) {
  validateMetrics("control", control, CONTROL_BATCH_SIZE);
  validateMetrics("candidate", candidate, CANDIDATE_BATCH_SIZE);

  const throughputRatio =
    Math.round((candidate.docsPerSecond / control.docsPerSecond) * 1_000_000) / 1_000_000;
  const candidateNotSlower = candidate.docsPerSecond >= control.docsPerSecond;

  return {
    verdict: "GO",
    selectedBatchSize: candidateNotSlower ? CANDIDATE_BATCH_SIZE : CONTROL_BATCH_SIZE,
    reason: candidateNotSlower ? "candidate_not_slower" : "candidate_slower",
    targetCount: PROBE_TARGET,
    controlDocsPerSecond: control.docsPerSecond,
    candidateDocsPerSecond: candidate.docsPerSecond,
    candidateToControlThroughputRatio: throughputRatio,
    controlLastToFirstP50Ratio: control.importLatencyWindows.lastToFirstP50Ratio,
    candidateLastToFirstP50Ratio: candidate.importLatencyWindows.lastToFirstP50Ratio,
  };
}

function parseArgs(argv) {
  const parsed = {};
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === "--control-metrics") {
      parsed.controlPath = argv[++index];
    } else if (argument === "--candidate-metrics") {
      parsed.candidatePath = argv[++index];
    } else {
      throw new Error(`unknown argument: ${argument}`);
    }
  }
  if (!parsed.controlPath || !parsed.candidatePath) {
    throw new Error("--control-metrics and --candidate-metrics are required");
  }
  return parsed;
}

async function runCli() {
  const { controlPath, candidatePath } = parseArgs(process.argv.slice(2));
  const [control, candidate] = await Promise.all(
    [controlPath, candidatePath].map(async (path) => JSON.parse(await readFile(path, "utf8"))),
  );
  const decision = selectReferenceBatchSize({ control, candidate });
  process.stdout.write(`${JSON.stringify(decision, null, 2)}\n`);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  runCli().catch((error) => {
    process.stderr.write(`INVALID: ${error.message}\n`);
    process.exitCode = 1;
  });
}

