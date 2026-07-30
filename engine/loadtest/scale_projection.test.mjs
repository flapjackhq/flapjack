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
    importWallClockMs: ((targetCount - startingCount) / docsPerSecond) * 1000,
    sentinels: "PASS",
  };
}

function bulkProbe(targetCount, docsPerSecond) {
  return {
    ...probe(0, targetCount, docsPerSecond),
    profile: "standard",
    workload: "bulk_build",
    runPurpose: "bulk_build_throughput_probe",
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

function bulkSpecimen(rates = [1000, 1000, 1000]) {
  return {
    ...specimen(rates),
    profile: "standard",
    workload: "bulk_build",
    probes: [
      bulkProbe(1_000_000, rates[0]),
      bulkProbe(4_000_000, rates[1]),
      bulkProbe(8_000_000, rates[2]),
    ],
  };
}

test("projector applies the frozen probe targets and import budget", () => {
  assert.deepEqual(REQUIRED_PROBE_TARGETS, [1_000_000, 4_000_000, 8_000_000]);
  assert.equal(DEFAULT_IMPORT_BUDGET_SECONDS, 43_200);
});

test("constant throughput uses the slowest observed rate without inventing degradation", () => {
  const result = evaluateScaleProjection(specimen());

  assert.equal(result.verdict, "NO_GO");
  assert.equal(result.workload, "import");
  assert.equal(result.degradationDetected, false);
  assert.equal(result.degradationExponent, 0);
  assert.deepEqual(result.projections, [
    {
      targetCount: 32_000_000,
      remainingRecords: 24_000_000,
      projectedTargetDocsPerSecond: 1000,
      projectedRemainingSeconds: 24_000,
      projectedTotalSeconds: 32_000,
      projectedSeconds: 32_000,
      verdict: "GO",
    },
    {
      targetCount: 64_000_000,
      remainingRecords: 56_000_000,
      projectedTargetDocsPerSecond: 1000,
      projectedRemainingSeconds: 56_000,
      projectedTotalSeconds: 64_000,
      projectedSeconds: 64_000,
      verdict: "NO_GO",
    },
  ]);
});

test("bulk_probe_projects_total_runtime_from_reference_locality", () => {
  const result = evaluateScaleProjection(bulkSpecimen([2000, 2000, 2000]));

  assert.equal(result.workload, "bulk_build");
  assert.equal(result.profile, "standard");
  assert.equal(result.baseDocsPerSecond, 2000);
  assert.equal(result.observedSeconds, 4000);
  assert.deepEqual(
    result.projections.map(({ projectedRemainingSeconds, projectedTotalSeconds }) => ({
      projectedRemainingSeconds,
      projectedTotalSeconds,
    })),
    [
      { projectedRemainingSeconds: 16_000, projectedTotalSeconds: 16_000 },
      { projectedRemainingSeconds: 32_000, projectedTotalSeconds: 32_000 },
    ],
  );
});

test("bulk-build probes reject non-reference locality and wrong-purpose evidence", () => {
  const ebs = bulkSpecimen();
  ebs.locality.reference.backingModel = "Amazon Elastic Block Store";
  assert.deepEqual(evaluateScaleProjection(ebs), {
    verdict: "INVALID",
    reasons: ["locality"],
  });

  const wrongPurpose = bulkSpecimen();
  wrongPurpose.probes[0].runPurpose = "throughput_probe";
  assert.deepEqual(evaluateScaleProjection(wrongPurpose), {
    verdict: "INVALID",
    reasons: ["probeEvidence"],
  });
});

test("degrading throughput uses the worst log/log exponent conservatively", () => {
  const result = evaluateScaleProjection(specimen([1000, 500, 250]));

  assert.equal(result.verdict, "NO_GO");
  assert.equal(result.degradationDetected, true);
  assert.equal(result.degradationExponent, 1);
  assert.equal(result.projections[0].projectedTargetDocsPerSecond, 62.5);
  assert.equal(result.projections[0].projectedRemainingSeconds, 384_000);
  assert.equal(result.projections[0].projectedSeconds, 407_000);
  assert.equal(result.projections[0].verdict, "NO_GO");
  assert.equal(result.projections[1].projectedTargetDocsPerSecond, 31.25);
  assert.equal(result.projections[1].projectedRemainingSeconds, 1_792_000);
  assert.equal(result.projections[1].projectedSeconds, 1_815_000);
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
