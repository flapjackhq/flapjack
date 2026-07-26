import assert from "node:assert/strict";
import test from "node:test";

import {
  LARGEST_OPERATIONAL_CLAIM,
  LITERAL_LIMIT_CLAIM,
  OPERATIONAL_TARGET,
  evaluateCompetitorHeadroom,
} from "./lib/competitor_headroom.mjs";

function profile(targetCount, verdict = "PASS") {
  return {
    targetCount,
    finalCount: targetCount,
    rungVerdict: verdict,
    sentinels: "PASS",
  };
}

test("frozen competitor counts and 1.5x operational target are exact", () => {
  assert.equal(LARGEST_OPERATIONAL_CLAIM, 40_000_000);
  assert.equal(OPERATIONAL_TARGET, 60_000_000);
  assert.equal(LITERAL_LIMIT_CLAIM, 4_300_000_000);
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
