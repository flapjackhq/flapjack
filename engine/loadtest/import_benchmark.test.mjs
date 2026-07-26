import assert from "node:assert/strict";
import { mkdir, readdir, rm, symlink, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  listBatchFiles,
  summarizeBatchLatencies,
  buildResultArtifact,
} from "./import_benchmark.mjs";
import { generateDataset } from "./generate_dataset.mjs";

async function withTempDir(prefix, fn) {
  const tempDir = path.join(os.tmpdir(), `${prefix}${Date.now()}-${Math.random().toString(36).slice(2)}`);
  await mkdir(tempDir, { recursive: true });
  try {
    await fn(tempDir);
  } finally {
    await rm(tempDir, { recursive: true, force: true });
  }
}

// --- listBatchFiles ---

test("listBatchFiles returns sorted batch file paths from a directory", async () => {
  await withTempDir("flapjack-list-batch-", async (dir) => {
    await writeFile(path.join(dir, "batch_003.json"), "{}");
    await writeFile(path.join(dir, "batch_001.json"), "{}");
    await writeFile(path.join(dir, "batch_002.json"), "{}");
    // Non-batch files should be ignored
    await writeFile(path.join(dir, "manifest.json"), "{}");
    await writeFile(path.join(dir, "README.md"), "");

    const files = await listBatchFiles(dir);
    assert.equal(files.length, 3);
    assert.equal(path.basename(files[0]), "batch_001.json");
    assert.equal(path.basename(files[1]), "batch_002.json");
    assert.equal(path.basename(files[2]), "batch_003.json");
    // Should return absolute paths
    assert.ok(path.isAbsolute(files[0]));
  });
});

test("listBatchFiles matches real Stage 1 generateDataset output contract", async () => {
  await withTempDir("flapjack-list-generated-", async (dir) => {
    const outputDir = path.join(dir, "data");
    const summary = await generateDataset({
      count: 125,
      batchSize: 25,
      outputDir,
      printSummary: false,
    });

    const discoveredFiles = await listBatchFiles(outputDir);
    const discoveredNames = discoveredFiles.map((file) => path.basename(file));
    const directoryEntries = await readdir(outputDir);
    const expectedBatchNames = directoryEntries.filter((entry) => /^batch_\d+\.json$/.test(entry)).sort();

    assert.equal(summary.batchCount, 5);
    assert.equal(discoveredNames.length, summary.batchCount);
    assert.deepEqual(discoveredNames, expectedBatchNames);
    assert.equal(discoveredNames[0], "batch_001.json");
    assert.equal(discoveredNames[summary.batchCount - 1], "batch_005.json");
  });
});

test("listBatchFiles throws on missing directory", async () => {
  await assert.rejects(
    () => listBatchFiles("/tmp/nonexistent-flapjack-dir-" + Date.now()),
    (error) => error.code === "ENOENT",
  );
});

test("listBatchFiles returns empty array when no batch files exist", async () => {
  await withTempDir("flapjack-list-empty-", async (dir) => {
    await writeFile(path.join(dir, "other.json"), "{}");
    const files = await listBatchFiles(dir);
    assert.equal(files.length, 0);
  });
});

test("listBatchFiles rejects symlinked batch files", async () => {
  await withTempDir("flapjack-list-symlink-", async (dir) => {
    const outsideFile = path.join(dir, "outside.json");
    const symlinkedBatch = path.join(dir, "batch_001.json");
    await writeFile(outsideFile, "{}");
    await symlink(outsideFile, symlinkedBatch);

    await assert.rejects(
      () => listBatchFiles(dir),
      /refusing symlinked batch file/,
    );
  });
});

// --- summarizeBatchLatencies ---

test("summarizeBatchLatencies computes avg/p95/p99 from latency array", () => {
  // 100 values: 1..100
  const latencies = Array.from({ length: 100 }, (_, i) => i + 1);
  const summary = summarizeBatchLatencies(latencies);

  assert.equal(summary.count, 100);
  assert.equal(summary.avg, 50.5);
  assert.equal(summary.min, 1);
  assert.equal(summary.max, 100);
  // p95 = value at 95th percentile index (nearest-rank): ceil(0.95 * 100) = 95th element = 95
  assert.equal(summary.p95, 95);
  // p99 = value at 99th percentile index: ceil(0.99 * 100) = 99th element = 99
  assert.equal(summary.p99, 99);
});

test("summarizeBatchLatencies handles single-element array", () => {
  const summary = summarizeBatchLatencies([42.5]);
  assert.equal(summary.count, 1);
  assert.equal(summary.avg, 42.5);
  assert.equal(summary.min, 42.5);
  assert.equal(summary.max, 42.5);
  assert.equal(summary.p95, 42.5);
  assert.equal(summary.p99, 42.5);
});

test("summarizeBatchLatencies handles empty array", () => {
  const summary = summarizeBatchLatencies([]);
  assert.equal(summary.count, 0);
  assert.equal(summary.avg, 0);
  assert.equal(summary.min, 0);
  assert.equal(summary.max, 0);
  assert.equal(summary.p95, 0);
  assert.equal(summary.p99, 0);
});

test("summarizeBatchLatencies rounds avg to 1 decimal place", () => {
  const summary = summarizeBatchLatencies([1, 2, 3]);
  assert.equal(summary.avg, 2);

  const summary2 = summarizeBatchLatencies([1, 2, 4]);
  // avg = 7/3 = 2.333... -> 2.3
  assert.equal(summary2.avg, 2.3);
});

// --- buildResultArtifact ---

test("buildResultArtifact produces valid schema with all required fields", () => {
  const latencies = [100, 200, 150, 180, 120];
  const artifact = buildResultArtifact({
    totalDocs: 5000,
    batchCount: 5,
    errorCount: 1,
    latenciesMs: latencies,
    wallClockMs: 800,
    indexName: "benchmark_100k",
    settingsSource: "engine/dashboard/tour/product-seed-data.mjs::seedSettings",
  });

  // Top-level required fields
  assert.equal(artifact.totalDocs, 5000);
  assert.equal(artifact.batchCount, 5);
  assert.equal(artifact.errorCount, 1);
  assert.equal(artifact.wallClockMs, 800);
  assert.equal(artifact.indexName, "benchmark_100k");
  assert.equal(artifact.settingsSource, "engine/dashboard/tour/product-seed-data.mjs::seedSettings");
  assert.equal(typeof artifact.timestamp, "string");
  // ISO 8601 format
  assert.ok(/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}/.test(artifact.timestamp));

  // Latency stats
  assert.equal(artifact.latency.count, 5);
  assert.equal(typeof artifact.latency.avg, "number");
  assert.equal(typeof artifact.latency.p95, "number");
  assert.equal(typeof artifact.latency.p99, "number");
  assert.equal(artifact.latency.min, 100);
  assert.equal(artifact.latency.max, 200);
});

test("buildResultArtifact with zero errors and empty latencies", () => {
  const artifact = buildResultArtifact({
    totalDocs: 0,
    batchCount: 0,
    errorCount: 0,
    latenciesMs: [],
    wallClockMs: 0,
    indexName: "benchmark_100k",
    settingsSource: "seedSettings",
  });

  assert.equal(artifact.errorCount, 0);
  assert.equal(artifact.latency.count, 0);
  assert.equal(artifact.latency.avg, 0);
});

// --- result artifact schema stability ---

test("buildResultArtifact schema has exactly the expected keys", () => {
  const artifact = buildResultArtifact({
    totalDocs: 100,
    batchCount: 1,
    errorCount: 0,
    latenciesMs: [50],
    wallClockMs: 60,
    indexName: "test_idx",
    settingsSource: "test",
  });

  const expectedTopKeys = [
    "timestamp",
    "indexName",
    "settingsSource",
    "totalDocs",
    "batchCount",
    "errorCount",
    "wallClockMs",
    "latency",
  ].sort();
  assert.deepEqual(Object.keys(artifact).sort(), expectedTopKeys);

  const expectedLatencyKeys = ["count", "avg", "min", "max", "p95", "p99"].sort();
  assert.deepEqual(Object.keys(artifact.latency).sort(), expectedLatencyKeys);
});
