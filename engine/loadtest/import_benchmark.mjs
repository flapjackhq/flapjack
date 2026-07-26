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
 * Summarize an array of latency values (in ms) with nearest-rank percentiles.
 */
export function summarizeBatchLatencies(latenciesMs) {
  if (latenciesMs.length === 0) {
    return { count: 0, avg: 0, min: 0, max: 0, p50: 0, p95: 0, p99: 0 };
  }

  const sorted = [...latenciesMs].sort((a, b) => a - b);
  const sum = sorted.reduce((acc, v) => acc + v, 0);
  const avg = Math.round((sum / sorted.length) * 10) / 10;

  return {
    count: sorted.length,
    avg,
    min: sorted[0],
    max: sorted[sorted.length - 1],
    p50: nearestRankPercentile(sorted, 50),
    p95: nearestRankPercentile(sorted, 95),
    p99: nearestRankPercentile(sorted, 99),
  };
}

/**
 * Compare equally sized first, middle, and last deciles without sorting away
 * run order. Small specimens use one sample per window so the diagnostic never
 * becomes silently empty.
 */
export function buildLatencyWindows(latenciesMs) {
  if (latenciesMs.length === 0) {
    throw new Error("latency windows require at least one successful batch latency");
  }

  const windowSize = Math.max(1, Math.floor(latenciesMs.length / 10));
  const middleStart = Math.floor((latenciesMs.length - windowSize) / 2);
  const firstValues = latenciesMs.slice(0, windowSize);
  const middleValues = latenciesMs.slice(middleStart, middleStart + windowSize);
  const lastValues = latenciesMs.slice(latenciesMs.length - windowSize);
  const first = summarizeBatchLatencies(firstValues);
  const middle = summarizeBatchLatencies(middleValues);
  const last = summarizeBatchLatencies(lastValues);

  return {
    windowSize,
    first,
    middle,
    last,
    lastToFirstP50Ratio:
      first.p50 > 0
        ? Math.round((last.p50 / first.p50) * 1000) / 1000
        : null,
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
    latencyWindows: latenciesMs.length > 0 ? buildLatencyWindows(latenciesMs) : null,
  };
}
