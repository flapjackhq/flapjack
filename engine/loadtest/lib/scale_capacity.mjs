#!/usr/bin/env node

import { fileURLToPath } from "node:url";
import path from "node:path";

const POSITIVE_INTEGER_FIELDS = Object.freeze([
  "targetCount",
  "diskFreeBytes",
  "memoryCapacityBytes",
  "sourceBytesPerRecord",
  "indexBytesPerRecord",
  "rssBytesPerRecord",
  "diskReserveBytes",
  "memoryReserveBytes",
]);

function isPositiveSafeInteger(value) {
  return Number.isSafeInteger(value) && value > 0;
}

export function evaluateScaleCapacity(input) {
  const invalidFields = [];
  if (input?.profile !== "compact" && input?.profile !== "standard") {
    invalidFields.push("profile");
  }
  if (!Number.isSafeInteger(input?.startingCount) || input.startingCount < 0) {
    invalidFields.push("startingCount");
  }
  for (const fieldName of POSITIVE_INTEGER_FIELDS) {
    if (!isPositiveSafeInteger(input?.[fieldName])) {
      invalidFields.push(fieldName);
    }
  }
  if (
    Number.isSafeInteger(input?.startingCount) &&
    Number.isSafeInteger(input?.targetCount) &&
    input.targetCount <= input.startingCount
  ) {
    invalidFields.push("targetCount");
  }

  if (invalidFields.length > 0) {
    return {
      verdict: "INVALID",
      reasons: [...new Set(invalidFields)],
    };
  }

  const trancheCount = input.targetCount - input.startingCount;
  const sourceBytes = trancheCount * input.sourceBytesPerRecord;
  const steadyIndexBytes = input.targetCount * input.indexBytesPerRecord;
  const mergeAllowanceBytes = steadyIndexBytes * 2;
  const requiredDiskBytes =
    sourceBytes +
    steadyIndexBytes +
    mergeAllowanceBytes +
    input.diskReserveBytes;
  const requiredMemoryBytes =
    input.targetCount * input.rssBytesPerRecord +
    input.memoryReserveBytes;

  if (
    ![
      sourceBytes,
      steadyIndexBytes,
      mergeAllowanceBytes,
      requiredDiskBytes,
      requiredMemoryBytes,
    ].every(Number.isSafeInteger)
  ) {
    return {
      verdict: "INVALID",
      reasons: ["arithmeticOverflow"],
    };
  }

  const reasons = [];
  if (requiredDiskBytes > input.diskFreeBytes) {
    reasons.push("disk");
  }
  if (requiredMemoryBytes > input.memoryCapacityBytes) {
    reasons.push("memory");
  }

  return {
    verdict: reasons.length === 0 ? "GO" : "NO_GO",
    reasons,
    profile: input.profile,
    startingCount: input.startingCount,
    targetCount: input.targetCount,
    trancheCount,
    diskFreeBytes: input.diskFreeBytes,
    memoryCapacityBytes: input.memoryCapacityBytes,
    sourceBytesPerRecord: input.sourceBytesPerRecord,
    indexBytesPerRecord: input.indexBytesPerRecord,
    rssBytesPerRecord: input.rssBytesPerRecord,
    diskReserveBytes: input.diskReserveBytes,
    memoryReserveBytes: input.memoryReserveBytes,
    sourceBytes,
    steadyIndexBytes,
    mergeAllowanceBytes,
    requiredDiskBytes,
    requiredMemoryBytes,
  };
}

function runCli() {
  const [flag, rawInput] = process.argv.slice(2);
  if (flag === "--help" || flag === "-h") {
    process.stdout.write("Usage: scale_capacity.mjs --input-json <json>\n");
    return;
  }
  if (flag !== "--input-json" || !rawInput) {
    throw new Error("Usage: scale_capacity.mjs --input-json <json>");
  }

  const result = evaluateScaleCapacity(JSON.parse(rawInput));
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (result.verdict !== "GO") {
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
