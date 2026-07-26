import { lstat, readdir, realpath } from "node:fs/promises";
import path from "node:path";

const BATCH_FILE_NAME_PATTERN = /^batch_\d+\.json$/;

function isPathWithinDirectory(candidatePath, directoryPath) {
  const relativePath = path.relative(directoryPath, candidatePath);
  return relativePath === "" || (!relativePath.startsWith("..") && !path.isAbsolute(relativePath));
}

/**
 * Discover and return sorted absolute paths of batch_NNN.json files in dataDir.
 */
export async function listBatchFiles(dataDir) {
  const canonicalDataDir = await realpath(dataDir);
  const entries = await readdir(canonicalDataDir);
  const batchFiles = entries.filter((entry) => BATCH_FILE_NAME_PATTERN.test(entry)).sort();
  const resolvedBatchFiles = [];

  for (const fileName of batchFiles) {
    const batchPath = path.join(canonicalDataDir, fileName);
    const batchStats = await lstat(batchPath);
    if (batchStats.isSymbolicLink()) {
      throw new Error(`refusing symlinked batch file: ${batchPath}`);
    }
    if (!batchStats.isFile()) {
      throw new Error(`refusing non-file batch entry: ${batchPath}`);
    }

    const resolvedBatchPath = await realpath(batchPath);
    if (!isPathWithinDirectory(resolvedBatchPath, canonicalDataDir)) {
      throw new Error(`batch file resolves outside dataset directory: ${batchPath}`);
    }
    resolvedBatchFiles.push(resolvedBatchPath);
  }

  return resolvedBatchFiles;
}

/**
 * Compute percentile using nearest-rank method.
 * sorted must be a pre-sorted ascending array.
 */
function nearestRankPercentile(sorted, percentile) {
  if (sorted.length === 0) return 0;
  const rank = Math.ceil((percentile / 100) * sorted.length);
  return sorted[Math.min(rank, sorted.length) - 1];
}

/**
 * Summarize an array of latency values (in ms) into count, avg, min, max, p95, p99.
 */
export function summarizeBatchLatencies(latenciesMs) {
  if (latenciesMs.length === 0) {
    return { count: 0, avg: 0, min: 0, max: 0, p95: 0, p99: 0 };
  }

  const sorted = [...latenciesMs].sort((a, b) => a - b);
  const sum = sorted.reduce((acc, v) => acc + v, 0);
  const avg = Math.round((sum / sorted.length) * 10) / 10;

  return {
    count: sorted.length,
    avg,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    p95: nearestRankPercentile(sorted, 95),
    p99: nearestRankPercentile(sorted, 99),
  };
}

/**
 * Build a machine-readable result artifact from benchmark run data.
 */
export function buildResultArtifact({
  totalDocs,
  batchCount,
  errorCount,
  latenciesMs,
  wallClockMs,
  indexName,
  settingsSource,
}) {
  return {
    timestamp: new Date().toISOString(),
    indexName,
    settingsSource,
    totalDocs,
    batchCount,
    errorCount,
    wallClockMs,
    latency: summarizeBatchLatencies(latenciesMs),
  };
}
