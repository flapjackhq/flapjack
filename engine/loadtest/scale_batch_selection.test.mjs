import assert from "node:assert/strict";
import test from "node:test";

import { selectReferenceBatchSize } from "./lib/scale_batch_selection.mjs";

function metrics(batchSize, docsPerSecond, overrides = {}) {
  return {
    profile: "compact",
    targetCount: 250000,
    finalCount: 250000,
    batchSize,
    docsPerSecond,
    sentinels: "PASS",
    importLatencyWindows: {
      first: { count: 10, p50: 100, p95: 120 },
      middle: { count: 10, p50: 150, p95: 180 },
      last: { count: 10, p50: 200, p95: 240 },
      lastToFirstP50Ratio: 2,
    },
    ...overrides,
  };
}

test("selectReferenceBatchSize chooses 10k at the exact throughput boundary", () => {
  const decision = selectReferenceBatchSize({
    control: metrics(1000, 2000),
    candidate: metrics(10000, 2000),
  });

  assert.equal(decision.verdict, "GO");
  assert.equal(decision.selectedBatchSize, 10000);
  assert.equal(decision.controlDocsPerSecond, 2000);
  assert.equal(decision.candidateDocsPerSecond, 2000);
  assert.equal(decision.candidateToControlThroughputRatio, 1);
  assert.equal(decision.reason, "candidate_not_slower");
});

test("selectReferenceBatchSize keeps 1k when 10k is slower", () => {
  const decision = selectReferenceBatchSize({
    control: metrics(1000, 2500),
    candidate: metrics(10000, 2000),
  });

  assert.equal(decision.verdict, "GO");
  assert.equal(decision.selectedBatchSize, 1000);
  assert.equal(decision.candidateToControlThroughputRatio, 0.8);
  assert.equal(decision.reason, "candidate_slower");
});

test("selectReferenceBatchSize rejects inexact or sentinel-failed specimens", () => {
  for (const candidate of [
    metrics(10000, 2000, { finalCount: 249999 }),
    metrics(10000, 2000, { sentinels: "FAIL" }),
    metrics(10000, 2000, { docsPerSecond: null }),
    metrics(10000, 2000, { importLatencyWindows: null }),
  ]) {
    assert.throws(
      () => selectReferenceBatchSize({ control: metrics(1000, 2000), candidate }),
      /candidate metrics are invalid/,
    );
  }
});

test("selectReferenceBatchSize rejects wrong batch identities and target", () => {
  assert.throws(
    () => selectReferenceBatchSize({
      control: metrics(2000, 2000),
      candidate: metrics(10000, 3000),
    }),
    /control metrics are invalid/,
  );
  assert.throws(
    () => selectReferenceBatchSize({
      control: metrics(1000, 2000),
      candidate: metrics(10000, 3000, { targetCount: 100000 }),
    }),
    /candidate metrics are invalid/,
  );
});

