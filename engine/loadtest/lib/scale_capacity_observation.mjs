import { pathToFileURL } from "node:url";

const POSITIVE_SAFE_INTEGER_FIELDS = [
  "targetCount",
  "indexBytes",
  "rssBytes",
  "indexBytesPerRecordAllowance",
  "rssBytesPerRecordAllowance",
];

export function evaluateCapacityObservation(observation) {
  if (observation === null || typeof observation !== "object") {
    return {
      verdict: "INVALID",
      reasons: ["observation"],
    };
  }

  const invalidFields = POSITIVE_SAFE_INTEGER_FIELDS.filter(
    (field) => !Number.isSafeInteger(observation[field]) || observation[field] <= 0,
  );
  if (typeof observation.profile !== "string" || observation.profile.length === 0) {
    invalidFields.unshift("profile");
  }
  if (invalidFields.length > 0) {
    return {
      profile: observation.profile,
      targetCount: observation.targetCount,
      verdict: "INVALID",
      reasons: invalidFields,
    };
  }

  const observedIndexBytesPerRecord = Math.ceil(
    observation.indexBytes / observation.targetCount,
  );
  const observedRssBytesPerRecord = Math.ceil(observation.rssBytes / observation.targetCount);
  const reasons = [];
  if (observedIndexBytesPerRecord > observation.indexBytesPerRecordAllowance) {
    reasons.push("indexBytesPerRecord");
  }
  if (observedRssBytesPerRecord > observation.rssBytesPerRecordAllowance) {
    reasons.push("rssBytesPerRecord");
  }

  return {
    profile: observation.profile,
    targetCount: observation.targetCount,
    indexBytes: observation.indexBytes,
    rssBytes: observation.rssBytes,
    indexBytesPerRecordAllowance: observation.indexBytesPerRecordAllowance,
    rssBytesPerRecordAllowance: observation.rssBytesPerRecordAllowance,
    observedIndexBytesPerRecord,
    observedRssBytesPerRecord,
    verdict: reasons.length === 0 ? "PASS" : "FAIL",
    reasons,
  };
}

function parseArgs(argv) {
  const inputIndex = argv.indexOf("--input-json");
  if (inputIndex === -1 || argv[inputIndex + 1] === undefined) {
    throw new Error("usage: scale_capacity_observation.mjs --input-json '<json>'");
  }
  return JSON.parse(argv[inputIndex + 1]);
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const result = evaluateCapacityObservation(parseArgs(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    if (result.verdict !== "PASS") {
      process.exitCode = 1;
    }
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 2;
  }
}
