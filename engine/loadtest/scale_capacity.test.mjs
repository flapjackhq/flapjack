import assert from "node:assert/strict";
import test from "node:test";

import { evaluateScaleCapacity } from "./lib/scale_capacity.mjs";

const BASE_INPUT = {
  profile: "compact",
  startingCount: 1_000,
  targetCount: 5_000,
  dataDiskFreeBytes: 1_000_000,
  datasetDiskFreeBytes: 1_000_000,
  diskFilesystemsShared: true,
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
  assert.equal(result.requiredDataDiskBytes, 380_000);
  assert.equal(result.requiredDatasetDiskBytes, 380_000);
  assert.equal(result.requiredDiskBytes, 380_000);
  assert.equal(result.requiredMemoryBytes, 200_000);
  assert.deepEqual(result.reasons, []);
});

test("capacity evaluator returns NO_GO when disk headroom is insufficient", () => {
  const result = evaluateScaleCapacity({
    ...BASE_INPUT,
    dataDiskFreeBytes: 379_999,
    datasetDiskFreeBytes: 379_999,
  });

  assert.equal(result.verdict, "NO_GO");
  assert.deepEqual(result.reasons, ["disk"]);
  assert.deepEqual(result.diskReasons, ["data", "dataset"]);
  assert.equal(result.requiredDiskBytes, 380_000);
});

test("capacity evaluator checks separate data and dataset filesystems independently", () => {
  const datasetLimited = evaluateScaleCapacity({
    ...BASE_INPUT,
    dataDiskFreeBytes: 340_000,
    datasetDiskFreeBytes: 79_999,
    diskFilesystemsShared: false,
  });

  assert.equal(datasetLimited.verdict, "NO_GO");
  assert.deepEqual(datasetLimited.reasons, ["disk"]);
  assert.deepEqual(datasetLimited.diskReasons, ["dataset"]);
  assert.equal(datasetLimited.requiredDataDiskBytes, 340_000);
  assert.equal(datasetLimited.requiredDatasetDiskBytes, 80_000);
  assert.equal(datasetLimited.requiredDiskBytes, 420_000);

  const dataLimited = evaluateScaleCapacity({
    ...BASE_INPUT,
    dataDiskFreeBytes: 339_999,
    datasetDiskFreeBytes: 80_000,
    diskFilesystemsShared: false,
  });
  assert.equal(dataLimited.verdict, "NO_GO");
  assert.deepEqual(dataLimited.diskReasons, ["data"]);
});

test("capacity evaluator accepts exact headroom on separate filesystems", () => {
  const result = evaluateScaleCapacity({
    ...BASE_INPUT,
    dataDiskFreeBytes: 340_000,
    datasetDiskFreeBytes: 80_000,
    diskFilesystemsShared: false,
  });

  assert.equal(result.verdict, "GO");
  assert.deepEqual(result.diskReasons, []);
});

test("capacity evaluator fails closed on missing or non-positive evidence", () => {
  const missingMemory = evaluateScaleCapacity({
    ...BASE_INPUT,
    memoryCapacityBytes: undefined,
  });
  assert.equal(missingMemory.verdict, "INVALID");
  assert.ok(missingMemory.reasons.includes("memoryCapacityBytes"));

  const missingDatasetDisk = evaluateScaleCapacity({
    ...BASE_INPUT,
    datasetDiskFreeBytes: undefined,
  });
  assert.equal(missingDatasetDisk.verdict, "INVALID");
  assert.ok(missingDatasetDisk.reasons.includes("datasetDiskFreeBytes"));

  const missingDiskLayout = evaluateScaleCapacity({
    ...BASE_INPUT,
    diskFilesystemsShared: undefined,
  });
  assert.equal(missingDiskLayout.verdict, "INVALID");
  assert.ok(missingDiskLayout.reasons.includes("diskFilesystemsShared"));

  const zeroIndexEstimate = evaluateScaleCapacity({
    ...BASE_INPUT,
    indexBytesPerRecord: 0,
  });
  assert.equal(zeroIndexEstimate.verdict, "INVALID");
  assert.ok(zeroIndexEstimate.reasons.includes("indexBytesPerRecord"));
});
