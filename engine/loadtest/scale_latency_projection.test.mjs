import assert from "node:assert/strict";
import fs from "node:fs";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { REMEDIATION_CONTRACT_ID } from "./lib/competitor_headroom.mjs";
import {
  BLENDED_P95_LIMIT_MS,
  NAME_PREFIX_P95_LIMIT_MS,
  PER_QUERY_TYPE_P95_LIMIT_MS,
  REQUIRED_QUERY_TYPES,
  SEARCH_SAMPLES_PER_TYPE,
} from "./lib/scale_rung_verdict.mjs";
import {
  FINAL_CERTIFICATION_TARGET,
  evaluateScaleLatencyProjection,
} from "./lib/scale_latency_projection.mjs";

function locality(overrides = {}) {
  return {
    verdict: "GO",
    reference: {
      instanceType: "i4i.4xlarge",
      backingModel: "Amazon EC2 NVMe Instance Storage",
    },
    ...overrides,
  };
}

function family(p95) {
  return {
    count: SEARCH_SAMPLES_PER_TYPE,
    p50: p95 / 2,
    p95,
    p99: p95 * 1.1,
  };
}

function queryTypes(overrides = {}) {
  const summaries = Object.fromEntries(
    REQUIRED_QUERY_TYPES.map((queryType) => [queryType, family(20)]),
  );
  for (const [queryType, p95] of Object.entries(overrides)) {
    summaries[queryType] = family(p95);
  }
  return summaries;
}

function overall(p95) {
  return {
    count: REQUIRED_QUERY_TYPES.length * SEARCH_SAMPLES_PER_TYPE,
    p50: p95 / 2,
    p95,
    p99: p95 * 1.1,
  };
}

function completedRung(targetCount, queryTypeP95, blendedP95) {
  return {
    targetCount,
    queryTypes: queryTypes(queryTypeP95),
    overall: overall(blendedP95),
  };
}

function specimen(overrides = {}) {
  return {
    profile: "standard",
    locality: locality(),
    completedRungs: [
      completedRung(1_000_000, { text: 10, facet: 20 }, 18),
      completedRung(2_000_000, { text: 10.2, facet: 20.5 }, 18.4),
    ],
    nextTarget: 4_000_000,
    ...overrides,
  };
}

function projectionAt(result, targetCount) {
  return result.projections.find((projection) => projection.targetCount === targetCount);
}

test("latency_projection_refuses_next_rung_when_facet_trend_crosses_gate", () => {
  const recordedAndSynthesized = specimen({
    completedRungs: [
      completedRung(
        1_000_000,
        {
          text: 29.313,
          facet: 95.0,
          multi_word: 60.0,
          typo: 20.0,
          filter: 20.0,
          geo: 20.0,
          highlight: 20.0,
        },
        52.277,
      ),
      completedRung(
        2_000_000,
        {
          text: 44.182,
          facet: 315.727,
          multi_word: 118.989,
          typo: 20.0,
          filter: 20.0,
          geo: 20.0,
          highlight: 20.0,
        },
        83.83,
      ),
    ],
  });

  const refusal = evaluateScaleLatencyProjection(recordedAndSynthesized);

  assert.equal(refusal.verdict, "REFUSE");
  assert.ok(refusal.reasons.includes("target:4000000:queryTypes.facet"));
  assert.equal(projectionAt(refusal, 4_000_000).families.facet.verdict, "REFUSE");
  assert.equal(projectionAt(refusal, 4_000_000).families.facet.projectedP95Ms, 757.181);

  const improvingFacet = specimen({
    completedRungs: [
      completedRung(1_000_000, { text: 29.313, facet: 700.0 }, 52.277),
      completedRung(2_000_000, { text: 44.182, facet: 315.727 }, 83.83),
    ],
  });

  const conservativeRefusal = evaluateScaleLatencyProjection(improvingFacet);

  assert.equal(conservativeRefusal.verdict, "REFUSE");
  assert.ok(conservativeRefusal.reasons.includes("target:4000000:queryTypes.facet"));
  assert.equal(projectionAt(conservativeRefusal, 4_000_000).families.facet.verdict, "REFUSE");
  assert.equal(
    projectionAt(conservativeRefusal, 4_000_000).families.facet.reason,
    "target:4000000:queryTypes.facet",
  );
  assert.equal(projectionAt(conservativeRefusal, 4_000_000).families.facet.projectedP95Ms, 700);
  assert.equal(projectionAt(conservativeRefusal, 64_000_000).families.facet.projectedP95Ms, 700);
});

test("latency_projection_is_invalid_on_missing_or_foreign_locality_samples", () => {
  const cases = [
    [
      "EBS backing model",
      () =>
        specimen({
          locality: locality({
            reference: {
              instanceType: "i4i.4xlarge",
              backingModel: "Amazon Elastic Block Store",
            },
          }),
        }),
    ],
    [
      "wrong instance type",
      () =>
        specimen({
          locality: locality({
            reference: {
              instanceType: "m7i.4xlarge",
              backingModel: "Amazon EC2 NVMe Instance Storage",
            },
          }),
        }),
    ],
    ["locality not GO", () => specimen({ locality: locality({ verdict: "NO_GO" }) })],
    ["missing completedRungs", () => ({ ...specimen(), completedRungs: undefined })],
    ["one completed rung", () => specimen({ completedRungs: [completedRung(1_000_000, {}, 18)] })],
    [
      "duplicate targetCount",
      () =>
        specimen({
          completedRungs: [completedRung(1_000_000, {}, 18), completedRung(1_000_000, {}, 19)],
        }),
    ],
    [
      "decreasing targetCount",
      () =>
        specimen({
          completedRungs: [completedRung(2_000_000, {}, 18), completedRung(1_000_000, {}, 19)],
        }),
    ],
    [
      "non-positive targetCount",
      () =>
        specimen({
          completedRungs: [completedRung(0, {}, 18), completedRung(1_000_000, {}, 19)],
        }),
    ],
    ["bad family count", () => {
      const invalid = specimen();
      invalid.completedRungs[0].queryTypes.facet.count = 29;
      return invalid;
    }],
    ["missing family p95", () => {
      const invalid = specimen();
      delete invalid.completedRungs[0].queryTypes.facet.p95;
      return invalid;
    }],
    ["infinite family p95", () => {
      const invalid = specimen();
      invalid.completedRungs[0].queryTypes.facet.p95 = Number.POSITIVE_INFINITY;
      return invalid;
    }],
    ["bad overall count", () => {
      const invalid = specimen();
      invalid.completedRungs[0].overall.count = 209;
      return invalid;
    }],
    ["missing overall p95", () => {
      const invalid = specimen();
      delete invalid.completedRungs[0].overall.p95;
      return invalid;
    }],
    ["infinite overall p95", () => {
      const invalid = specimen();
      invalid.completedRungs[0].overall.p95 = Number.POSITIVE_INFINITY;
      return invalid;
    }],
    ["non-forward nextTarget", () => specimen({ nextTarget: 2_000_000 })],
    ["invalid profile", () => specimen({ profile: "wide" })],
  ];

  for (const [name, buildInvalid] of cases) {
    const result = evaluateScaleLatencyProjection(buildInvalid());
    assert.equal(result.verdict, "INVALID", name);
    assert.ok(result.reasons.length > 0, name);
  }
});

test("latency_projection_green_case_returns_exact_projected_values", () => {
  const result = evaluateScaleLatencyProjection(
    specimen({
      completedRungs: [
        completedRung(1_000_000, { text: 10, facet: 20 }, 18),
        completedRung(2_000_000, { text: 10.2, facet: 20.5 }, 18.4),
      ],
    }),
  );

  assert.equal(result.verdict, "GO");
  assert.equal(projectionAt(result, 4_000_000).families.facet.projectedP95Ms, 21.5);
  assert.equal(projectionAt(result, 64_000_000).families.facet.projectedP95Ms, 51.5);
  assert.equal(projectionAt(result, 4_000_000).families.text.projectedP95Ms, 10.6);
  assert.equal(projectionAt(result, 64_000_000).families.text.projectedP95Ms, 22.6);
  assert.equal(projectionAt(result, 4_000_000).families.blended.projectedP95Ms, 19.2);
  assert.equal(projectionAt(result, 64_000_000).families.blended.projectedP95Ms, 43.2);
  for (const queryType of ["typo", "multi_word", "filter", "geo", "highlight"]) {
    assert.equal(projectionAt(result, 4_000_000).families[queryType].projectedP95Ms, 20);
    assert.equal(projectionAt(result, 64_000_000).families[queryType].projectedP95Ms, 20);
  }
});

test("latency_projection_reuses_the_rung_verdict_gate_constants", () => {
  const result = evaluateScaleLatencyProjection(specimen());

  assert.equal(projectionAt(result, 4_000_000).families.text.limitMs, NAME_PREFIX_P95_LIMIT_MS);
  assert.equal(projectionAt(result, 4_000_000).families.facet.limitMs, PER_QUERY_TYPE_P95_LIMIT_MS);
  assert.equal(projectionAt(result, 4_000_000).families.blended.limitMs, BLENDED_P95_LIMIT_MS);
});

test("remediation_contract_document_matches_exported_gate_constants", () => {
  const contractPath = path.join(
    path.dirname(fileURLToPath(import.meta.url)),
    "SCALE_REMEDIATION_CONTRACT_2026_07_26.md",
  );
  const contract = fs.readFileSync(contractPath, "utf8");

  assert.ok(contract.includes(`**Contract ID:** \`${REMEDIATION_CONTRACT_ID}\``));
  assert.ok(
    contract.includes(
      `| Name/prefix search p95 | ≤ ${NAME_PREFIX_P95_LIMIT_MS} ms | \`NAME_PREFIX_P95_LIMIT_MS\` in \`engine/loadtest/lib/scale_rung_verdict.mjs:7-21\` |`,
    ),
  );
  assert.ok(
    contract.includes(
      `| Per-query-family p95 | ≤ ${PER_QUERY_TYPE_P95_LIMIT_MS} ms | \`PER_QUERY_TYPE_P95_LIMIT_MS\` in \`engine/loadtest/lib/scale_rung_verdict.mjs:7-21\` |`,
    ),
  );
  assert.ok(
    contract.includes(
      `| Blended search p95 | ≤ ${BLENDED_P95_LIMIT_MS} ms | \`BLENDED_P95_LIMIT_MS\` in \`engine/loadtest/lib/scale_rung_verdict.mjs:7-21\` |`,
    ),
  );
  assert.ok(
    contract.includes(
      `| Measured search requests | = ${SEARCH_SAMPLES_PER_TYPE} × ${REQUIRED_QUERY_TYPES.length} | \`SEARCH_SAMPLES_PER_TYPE\` and \`REQUIRED_QUERY_TYPES\` in \`engine/loadtest/lib/scale_rung_verdict.mjs:7-21\` |`,
    ),
  );
  assert.ok(
    contract.includes(
      `| Forward latency projection | GO through ${FINAL_CERTIFICATION_TARGET.toLocaleString("en-US")} records | \`FINAL_CERTIFICATION_TARGET\` at \`engine/loadtest/lib/scale_latency_projection.mjs:14\` and \`evaluateScaleLatencyProjection()\` |`,
    ),
  );
});
