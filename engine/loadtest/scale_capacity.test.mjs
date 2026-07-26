import assert from "node:assert/strict";
import test from "node:test";

import { evaluateScaleCapacity } from "./lib/scale_capacity.mjs";

const BASE_INPUT = {
  profile: "compact",
  startingCount: 1_000,
  targetCount: 5_000,
  diskFreeBytes: 1_000_000,
  memoryCapacityBytes: 500_000,
  sourceBytesPerRecord: 10,
  indexBytesPerRecord: 20,
  rssBytesPerRecord: 30,
  diskReserveBytes: 40_000,
  memoryReserveBytes: 50_000,
};

test("capacity evaluator returns hand-calculated GO requirements", () => {
  const result = evaluateScaleCapacity(BASE_INPUT);

  assert.equal(result.verdict, "GO");
  assert.equal(result.sourceBytes, 40_000);
  assert.equal(result.steadyIndexBytes, 100_000);
  assert.equal(result.mergeAllowanceBytes, 200_000);
  assert.equal(result.requiredDiskBytes, 380_000);
  assert.equal(result.requiredMemoryBytes, 200_000);
  assert.deepEqual(result.reasons, []);
});

test("capacity evaluator returns NO_GO when disk headroom is insufficient", () => {
  const result = evaluateScaleCapacity({
    ...BASE_INPUT,
    diskFreeBytes: 379_999,
  });

  assert.equal(result.verdict, "NO_GO");
  assert.deepEqual(result.reasons, ["disk"]);
  assert.equal(result.requiredDiskBytes, 380_000);
});

test("capacity evaluator fails closed on missing or non-positive evidence", () => {
  const missingMemory = evaluateScaleCapacity({
    ...BASE_INPUT,
    memoryCapacityBytes: undefined,
  });
  assert.equal(missingMemory.verdict, "INVALID");
  assert.ok(missingMemory.reasons.includes("memoryCapacityBytes"));

  const zeroIndexEstimate = evaluateScaleCapacity({
    ...BASE_INPUT,
    indexBytesPerRecord: 0,
  });
  assert.equal(zeroIndexEstimate.verdict, "INVALID");
  assert.ok(zeroIndexEstimate.reasons.includes("indexBytesPerRecord"));
});
