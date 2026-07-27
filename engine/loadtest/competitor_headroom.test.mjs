import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  HEADROOM_CONTRACT_ID,
  LARGEST_OPERATIONAL_CLAIM,
  LITERAL_LIMIT_CLAIM,
  OPERATIONAL_TARGET,
  REMEDIATION_CONTRACT_ID,
  REMEDIATION_REQUIRED_TARGET,
  evaluateCompetitorHeadroom,
} from "./lib/competitor_headroom.mjs";
import { FINAL_CERTIFICATION_TARGET } from "./lib/scale_latency_projection.mjs";

function profile(targetCount, verdict = "PASS") {
  return {
    targetCount,
    finalCount: targetCount,
    rungVerdict: verdict,
    sentinels: "PASS",
  };
}

function compact64mLegacyResult() {
  return {
    greenProfiles: ["compact"],
    operational: {
      verdict: "PASS",
      requiredTarget: 60_000_000,
      comparisonCount: 40_000_000,
      profile: "compact",
      targetCount: 64_000_000,
      marginOverLargestClaim: 1.6,
    },
    literalLimit: {
      verdict: "NOT_PROVEN",
      requiredTarget: 6_450_000_000,
      comparisonCount: 4_300_000_000,
    },
  };
}

function importWithoutCliPath(relativeModulePath) {
  const testDir = path.dirname(fileURLToPath(import.meta.url));
  const moduleUrl = pathToFileURL(path.join(testDir, relativeModulePath)).href;
  return spawnSync(
    process.execPath,
    [
      "--input-type=module",
      "-e",
      `import(${JSON.stringify(moduleUrl)}).then(() => process.stdout.write("ok\\n")).catch((error) => { process.stderr.write(String(error.message ?? error)); process.exit(1); })`,
    ],
    { encoding: "utf8" },
  );
}

test("frozen competitor counts and 1.5x operational target are exact", () => {
  assert.equal(LARGEST_OPERATIONAL_CLAIM, 40_000_000);
  assert.equal(OPERATIONAL_TARGET, 60_000_000);
  assert.equal(LITERAL_LIMIT_CLAIM, 4_300_000_000);
});

test("loadtest library entrypoints stay importable without a CLI argv path", () => {
  for (const relativeModulePath of [
    "./lib/competitor_headroom.mjs",
    "./lib/scale_latency_projection.mjs",
  ]) {
    const result = importWithoutCliPath(relativeModulePath);
    assert.equal(result.status, 0, `${relativeModulePath}: ${result.stderr}`);
    assert.equal(result.stdout, "ok\n");
  }
});

test("an exact green 64M compact specimen passes only operational headroom", () => {
  const result = evaluateCompetitorHeadroom({
    compact: profile(64_000_000),
    standard: profile(4_000_000, "FAIL"),
  });

  assert.equal(result.operational.verdict, "PASS");
  assert.equal(result.operational.profile, "compact");
  assert.equal(result.operational.marginOverLargestClaim, 1.6);
  assert.equal(result.literalLimit.verdict, "NOT_PROVEN");
  assert.deepEqual(result.greenProfiles, ["compact"]);
});

test("59,999,999 and inexact or sentinel-failed specimens cannot pass", () => {
  for (const specimen of [
    profile(59_999_999),
    { ...profile(64_000_000), finalCount: 63_999_999 },
    { ...profile(64_000_000), sentinels: "FAIL" },
  ]) {
    const result = evaluateCompetitorHeadroom({ compact: specimen });
    assert.equal(result.operational.verdict, "NOT_PROVEN");
    assert.equal(result.literalLimit.verdict, "NOT_PROVEN");
  }
});

test("profiles remain independent and only an exact 6.45B specimen beats the literal limit", () => {
  const result = evaluateCompetitorHeadroom({
    compact: profile(64_000_000),
    standard: profile(6_450_000_000),
  });

  assert.deepEqual(result.greenProfiles, ["compact", "standard"]);
  assert.equal(result.operational.verdict, "PASS");
  assert.equal(result.literalLimit.verdict, "PASS");
  assert.equal(result.literalLimit.profile, "standard");
});

test(
  "remediation_contract_id_is_distinct_and_old_receipts_cannot_satisfy_new_gates",
  async (t) => {
    assert.notEqual(REMEDIATION_CONTRACT_ID, HEADROOM_CONTRACT_ID);
    assert.equal(REMEDIATION_CONTRACT_ID.includes(HEADROOM_CONTRACT_ID), false);
    assert.equal(HEADROOM_CONTRACT_ID.includes(REMEDIATION_CONTRACT_ID), false);
    assert.equal(REMEDIATION_REQUIRED_TARGET, FINAL_CERTIFICATION_TARGET);

    await t.test("old contract receipts do not satisfy remediation", () => {
      const result = evaluateCompetitorHeadroom(
        {
          standard: {
            ...profile(REMEDIATION_REQUIRED_TARGET),
            contractId: HEADROOM_CONTRACT_ID,
          },
        },
        { contractId: REMEDIATION_CONTRACT_ID },
      );

      assert.equal(result.operational.verdict, "NOT_PROVEN");
      assert.deepEqual(result.greenProfiles, []);
      assert.equal(result.contractId, REMEDIATION_CONTRACT_ID);
    });

    await t.test("standard remediation receipts satisfy remediation", () => {
      const result = evaluateCompetitorHeadroom(
        {
          standard: {
            ...profile(REMEDIATION_REQUIRED_TARGET),
            contractId: REMEDIATION_CONTRACT_ID,
          },
        },
        { contractId: REMEDIATION_CONTRACT_ID },
      );

      assert.equal(result.operational.verdict, "PASS");
      assert.equal(result.contractId, REMEDIATION_CONTRACT_ID);
    });

    await t.test("compact receipts do not satisfy standard remediation", () => {
      const result = evaluateCompetitorHeadroom(
        {
          compact: {
            ...profile(REMEDIATION_REQUIRED_TARGET),
            contractId: REMEDIATION_CONTRACT_ID,
          },
        },
        { contractId: REMEDIATION_CONTRACT_ID },
      );

      assert.equal(result.operational.verdict, "NOT_PROVEN");
      assert.deepEqual(result.greenProfiles, []);
      assert.equal(result.contractId, REMEDIATION_CONTRACT_ID);
    });

    await t.test("legacy one-argument output remains byte-shape compatible", () => {
      assert.deepEqual(
        evaluateCompetitorHeadroom({ compact: profile(64_000_000) }),
        compact64mLegacyResult(),
      );
    });

    await t.test("unknown requested contract IDs fail closed", () => {
      assert.throws(
        () => evaluateCompetitorHeadroom({}, { contractId: "unknown-contract" }),
        RangeError,
      );
    });
  },
);

test("explicit historical contract preserves qualification and records its regime", () => {
  assert.deepEqual(
    evaluateCompetitorHeadroom(
      { compact: profile(64_000_000) },
      { contractId: HEADROOM_CONTRACT_ID },
    ),
    {
      ...compact64mLegacyResult(),
      contractId: HEADROOM_CONTRACT_ID,
    },
  );
});

test("remediation target is an inclusive 64M boundary", () => {
  const belowTarget = evaluateCompetitorHeadroom(
    {
      standard: {
        ...profile(REMEDIATION_REQUIRED_TARGET - 1),
        contractId: REMEDIATION_CONTRACT_ID,
      },
    },
    { contractId: REMEDIATION_CONTRACT_ID },
  );

  assert.deepEqual(belowTarget.greenProfiles, []);
  assert.equal(belowTarget.operational.verdict, "NOT_PROVEN");
});
