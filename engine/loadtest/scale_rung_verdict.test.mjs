import assert from "node:assert/strict";
import test from "node:test";

import {
  BLENDED_P95_LIMIT_MS,
  NAME_PREFIX_P95_LIMIT_MS,
  evaluateScaleRung,
} from "./lib/scale_rung_verdict.mjs";

function specimen(nameP95, blendedP95) {
  const summary = { count: 30, p50: 10, p95: 20, p99: 30 };
  return {
    queryTypes: {
      text: { ...summary, p95: nameP95 },
      typo: summary,
      multi_word: summary,
      facet: summary,
      filter: summary,
      geo: summary,
      highlight: summary,
    },
    overall: { count: 210, p50: 20, p95: blendedP95, p99: 30 },
  };
}

test("rung verdict passes the exact frozen latency boundaries", () => {
  assert.equal(NAME_PREFIX_P95_LIMIT_MS, 50);
  assert.equal(BLENDED_P95_LIMIT_MS, 100);

  const result = evaluateScaleRung(specimen(50, 100));

  assert.equal(result.verdict, "PASS");
  assert.deepEqual(result.reasons, []);
  assert.deepEqual(result.observed, {
    namePrefixP95Ms: 50,
    blendedP95Ms: 100,
  });
});

test("rung verdict reports each breached frozen boundary", () => {
  const nameFailure = evaluateScaleRung(specimen(51, 100));
  assert.equal(nameFailure.verdict, "FAIL");
  assert.deepEqual(nameFailure.reasons, ["namePrefixP95"]);

  const blendedFailure = evaluateScaleRung(specimen(50, 101));
  assert.equal(blendedFailure.verdict, "FAIL");
  assert.deepEqual(blendedFailure.reasons, ["blendedP95"]);

  const bothFail = evaluateScaleRung(specimen(51, 101));
  assert.equal(bothFail.verdict, "FAIL");
  assert.deepEqual(bothFail.reasons, ["namePrefixP95", "blendedP95"]);
});

test("rung verdict fails closed on missing or non-finite latency evidence", () => {
  const missing = evaluateScaleRung({ queryTypes: {}, overall: { p95: 10 } });
  assert.equal(missing.verdict, "INVALID");
  assert.deepEqual(missing.reasons, ["namePrefixP95"]);

  const nonNumeric = evaluateScaleRung(specimen("5", 10));
  assert.equal(nonNumeric.verdict, "INVALID");
  assert.deepEqual(nonNumeric.reasons, ["namePrefixP95"]);

  const nonFinite = evaluateScaleRung(specimen(5, Number.POSITIVE_INFINITY));
  assert.equal(nonFinite.verdict, "INVALID");
  assert.deepEqual(nonFinite.reasons, ["blendedP95"]);
});

test("rung verdict requires exactly 30 measured requests for all seven query types", () => {
  const missingType = specimen(5, 10);
  delete missingType.queryTypes.highlight;
  assert.deepEqual(evaluateScaleRung(missingType), {
    verdict: "INVALID",
    reasons: ["queryTypes.highlight.count", "overall.count"],
  });

  const shortType = specimen(5, 10);
  shortType.queryTypes.geo = { ...shortType.queryTypes.geo, count: 29 };
  shortType.overall.count = 209;
  assert.deepEqual(evaluateScaleRung(shortType), {
    verdict: "INVALID",
    reasons: ["queryTypes.geo.count", "overall.count"],
  });
});
