import { pathToFileURL } from "node:url";

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

export function evaluateCompetitorHeadroom(profileResults) {
  const supplied =
    profileResults !== null && typeof profileResults === "object" ? profileResults : {};
  const orderedNames = [
    ...PROFILE_ORDER,
    ...Object.keys(supplied)
      .filter((name) => !PROFILE_ORDER.includes(name))
      .sort(),
  ];
  const green = orderedNames
    .filter((name) => isExactGreenProfile(supplied[name]))
    .map((name) => ({ name, specimen: supplied[name] }));

  return {
    greenProfiles: green.map(({ name }) => name),
    operational: verdictFor(green, OPERATIONAL_TARGET, LARGEST_OPERATIONAL_CLAIM),
    literalLimit: verdictFor(green, LITERAL_LIMIT_TARGET, LITERAL_LIMIT_CLAIM),
  };
}

function parseArgs(argv) {
  const inputIndex = argv.indexOf("--input-json");
  if (inputIndex === -1 || argv[inputIndex + 1] === undefined) {
    throw new Error("usage: competitor_headroom.mjs --input-json '<json>'");
  }
  return JSON.parse(argv[inputIndex + 1]);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    process.stdout.write(`${JSON.stringify(evaluateCompetitorHeadroom(parseArgs(process.argv.slice(2))), null, 2)}\n`);
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 2;
  }
}
