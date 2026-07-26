import assert from "node:assert/strict";
import test from "node:test";

import {
  DEFAULT_IMPORT_BUDGET_SECONDS,
  REQUIRED_PROBE_TARGETS,
  evaluateScaleProjection,
} from "./lib/scale_projection.mjs";

function locality() {
  return {
    verdict: "GO",
    reference: {
      instanceType: "i4i.4xlarge",
      backingModel: "Amazon EC2 NVMe Instance Storage",
    },
  };
}

function probe(startingCount, targetCount, docsPerSecond) {
  return {
    profile: "compact",
    runPurpose: "throughput_probe",
    startingCount,
    targetCount,
    trancheSize: targetCount - startingCount,
    finalCount: targetCount,
    docsPerSecond,
    sentinels: "PASS",
  };
}

function specimen(rates = [1000, 1000, 1000]) {
  return {
    profile: "compact",
    locality: locality(),
    probes: [
      probe(0, 1_000_000, rates[0]),
      probe(1_000_000, 4_000_000, rates[1]),
      probe(4_000_000, 8_000_000, rates[2]),
    ],
    targets: [32_000_000, 64_000_000],
    budgetSeconds: 43_200,
  };
}

test("projector applies the frozen probe targets and import budget", () => {
  assert.deepEqual(REQUIRED_PROBE_TARGETS, [1_000_000, 4_000_000, 8_000_000]);
  assert.equal(DEFAULT_IMPORT_BUDGET_SECONDS, 43_200);
});

test("constant throughput uses the slowest observed rate without inventing degradation", () => {
  const result = evaluateScaleProjection(specimen());

  assert.equal(result.verdict, "NO_GO");
  assert.equal(result.degradationDetected, false);
  assert.equal(result.degradationExponent, 0);
  assert.deepEqual(result.projections, [
    {
      targetCount: 32_000_000,
      remainingRecords: 24_000_000,
      projectedTargetDocsPerSecond: 1000,
      projectedSeconds: 24_000,
      verdict: "GO",
    },
    {
      targetCount: 64_000_000,
      remainingRecords: 56_000_000,
      projectedTargetDocsPerSecond: 1000,
      projectedSeconds: 56_000,
      verdict: "NO_GO",
    },
  ]);
});

test("degrading throughput uses the worst log/log exponent conservatively", () => {
  const result = evaluateScaleProjection(specimen([1000, 500, 250]));

  assert.equal(result.verdict, "NO_GO");
  assert.equal(result.degradationDetected, true);
  assert.equal(result.degradationExponent, 1);
  assert.equal(result.projections[0].projectedTargetDocsPerSecond, 62.5);
  assert.equal(result.projections[0].projectedSeconds, 384_000);
  assert.equal(result.projections[0].verdict, "NO_GO");
  assert.equal(result.projections[1].projectedTargetDocsPerSecond, 31.25);
  assert.equal(result.projections[1].projectedSeconds, 1_792_000);
});

test("projector fails closed on non-reference locality or incomplete probe evidence", () => {
  const wrongLocality = specimen();
  wrongLocality.locality.reference.backingModel = "Amazon Elastic Block Store";
  assert.deepEqual(evaluateScaleProjection(wrongLocality), {
    verdict: "INVALID",
    reasons: ["locality"],
  });

  const missingProbe = specimen();
  missingProbe.probes.pop();
  assert.deepEqual(evaluateScaleProjection(missingProbe), {
    verdict: "INVALID",
    reasons: ["probeTargets"],
  });

  const badCount = specimen();
  badCount.probes[1].finalCount = 3_999_999;
  assert.deepEqual(evaluateScaleProjection(badCount), {
    verdict: "INVALID",
    reasons: ["probeEvidence"],
  });
});
