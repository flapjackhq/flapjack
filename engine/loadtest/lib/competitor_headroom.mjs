import path from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

import { FINAL_CERTIFICATION_TARGET } from "./scale_latency_projection.mjs";

export const HEADROOM_CONTRACT_ID = "flapjack-competitor-headroom-2026-07-26";
export const REMEDIATION_CONTRACT_ID =
  "flapjack-scale-remediation-64m-standard-2026-07-26";
export const REMEDIATION_REQUIRED_PROFILE = "standard";
export const REMEDIATION_REQUIRED_TARGET = FINAL_CERTIFICATION_TARGET;

export const LARGEST_OPERATIONAL_CLAIM = 40_000_000;
export const OPERATIONAL_TARGET = 60_000_000;
export const LITERAL_LIMIT_CLAIM = 4_300_000_000;
export const LITERAL_LIMIT_TARGET = 6_450_000_000;

const PROFILE_ORDER = ["compact", "standard"];

function isExactGreenProfile(profile) {
  return (
    profile !== null &&
    typeof profile === "object" &&
    Number.isSafeInteger(profile.targetCount) &&
    profile.targetCount > 0 &&
    Number.isSafeInteger(profile.finalCount) &&
    profile.finalCount === profile.targetCount &&
    profile.rungVerdict === "PASS" &&
    profile.sentinels === "PASS"
  );
}

function qualifyingProfile(profiles, minimumTarget) {
  return profiles
    .filter((entry) => entry.specimen.targetCount >= minimumTarget)
    .sort(
      (left, right) =>
        right.specimen.targetCount - left.specimen.targetCount ||
        PROFILE_ORDER.indexOf(left.name) - PROFILE_ORDER.indexOf(right.name),
    )[0];
}

function verdictFor(profiles, minimumTarget, comparisonCount) {
  const qualifying = qualifyingProfile(profiles, minimumTarget);
  if (qualifying === undefined) {
    return {
      verdict: "NOT_PROVEN",
      requiredTarget: minimumTarget,
      comparisonCount,
    };
  }

  return {
    verdict: "PASS",
    requiredTarget: minimumTarget,
    comparisonCount,
    profile: qualifying.name,
    targetCount: qualifying.specimen.targetCount,
    marginOverLargestClaim:
      Math.round((qualifying.specimen.targetCount / comparisonCount) * 1_000_000) / 1_000_000,
  };
}

export function evaluateCompetitorHeadroom(profileResults, options = {}) {
  const requestedContractId = options?.contractId;
  if (
    requestedContractId !== undefined &&
    requestedContractId !== HEADROOM_CONTRACT_ID &&
    requestedContractId !== REMEDIATION_CONTRACT_ID
  ) {
    // Unknown regimes must not silently fall back to historical receipt qualification.
    throw new RangeError(`unknown competitor headroom contract ID: ${requestedContractId}`);
  }

  const supplied =
    profileResults !== null && typeof profileResults === "object" ? profileResults : {};
  const orderedNames = [
    ...PROFILE_ORDER,
    ...Object.keys(supplied)
      .filter((name) => !PROFILE_ORDER.includes(name))
      .sort(),
  ];
  const eligibleNames =
    requestedContractId === REMEDIATION_CONTRACT_ID
      ? orderedNames.filter(
          (name) =>
            name === REMEDIATION_REQUIRED_PROFILE &&
            supplied[name]?.contractId === REMEDIATION_CONTRACT_ID &&
            supplied[name]?.targetCount >= REMEDIATION_REQUIRED_TARGET,
        )
      : orderedNames;
  const green = eligibleNames
    .filter((name) => isExactGreenProfile(supplied[name]))
    .map((name) => ({ name, specimen: supplied[name] }));

  const result = {
    greenProfiles: green.map(({ name }) => name),
    operational: verdictFor(green, OPERATIONAL_TARGET, LARGEST_OPERATIONAL_CLAIM),
    literalLimit: verdictFor(green, LITERAL_LIMIT_TARGET, LITERAL_LIMIT_CLAIM),
  };
  // The omitted-ID path is frozen for byte-shape compatibility with historical consumers.
  return requestedContractId === undefined
    ? result
    : { ...result, contractId: requestedContractId };
}

function parseArgs(argv) {
  const inputIndex = argv.indexOf("--input-json");
  if (inputIndex === -1 || argv[inputIndex + 1] === undefined) {
    throw new Error("usage: competitor_headroom.mjs --input-json '<json>'");
  }
  return JSON.parse(argv[inputIndex + 1]);
}

const currentFilePath = fileURLToPath(import.meta.url);
const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (currentFilePath === invokedPath) {
  try {
    process.stdout.write(`${JSON.stringify(evaluateCompetitorHeadroom(parseArgs(process.argv.slice(2))), null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 2;
  }
}
