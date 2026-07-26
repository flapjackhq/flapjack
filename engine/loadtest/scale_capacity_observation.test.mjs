import assert from "node:assert/strict";
import test from "node:test";

import { evaluateCapacityObservation } from "./lib/scale_capacity_observation.mjs";

const BASE = {
  profile: "compact",
  targetCount: 1_000,
  indexBytes: 2_456_001,
  rssBytes: 950_001,
  indexBytesPerRecordAllowance: 2_457,
  rssBytesPerRecordAllowance: 951,
};

test("capacity observation passes the exact rounded-up allowance boundary", () => {
  const result = evaluateCapacityObservation(BASE);

  assert.equal(result.verdict, "PASS");
  assert.equal(result.observedIndexBytesPerRecord, 2_457);
  assert.equal(result.observedRssBytesPerRecord, 951);
  assert.deepEqual(result.reasons, []);
});

test("capacity observation fails when either observed allowance is exceeded", () => {
  const indexFailure = evaluateCapacityObservation({
    ...BASE,
    indexBytes: 2_457_001,
  });
  assert.equal(indexFailure.verdict, "FAIL");
  assert.deepEqual(indexFailure.reasons, ["indexBytesPerRecord"]);

  const rssFailure = evaluateCapacityObservation({
    ...BASE,
    rssBytes: 951_001,
  });
  assert.equal(rssFailure.verdict, "FAIL");
  assert.deepEqual(rssFailure.reasons, ["rssBytesPerRecord"]);
});

test("capacity observation fails closed on missing, zero, or unsafe evidence", () => {
  for (const mutation of [
    { targetCount: 0 },
    { indexBytes: undefined },
    { rssBytes: Number.MAX_SAFE_INTEGER + 1 },
    { indexBytesPerRecordAllowance: -1 },
  ]) {
    const result = evaluateCapacityObservation({ ...BASE, ...mutation });
    assert.equal(result.verdict, "INVALID");
    assert.ok(result.reasons.length > 0);
  }
});
