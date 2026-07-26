import assert from "node:assert/strict";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import {
  createEvidenceManifest,
  verifyEvidenceManifest,
} from "./lib/evidence_manifest.mjs";

function fixture() {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "scale-evidence-"));
  fs.mkdirSync(path.join(root, "rung_1000000"));
  fs.writeFileSync(path.join(root, "checkpoint.json"), '{"lastCompletedRung":1000000}\n');
  fs.writeFileSync(path.join(root, "rung_1000000", "metrics.json"), '{"finalCount":1000000}\n');
  return root;
}

test("manifest create and verify pins exact relative paths, sizes, and hashes", () => {
  const root = fixture();
  const manifestPath = path.join(root, "evidence_manifest.json");

  const manifest = createEvidenceManifest(root, manifestPath);
  assert.deepEqual(
    manifest.files.map((entry) => entry.path),
    ["checkpoint.json", "rung_1000000/metrics.json"],
  );
  assert.ok(manifest.files.every((entry) => entry.bytes > 0));
  assert.ok(manifest.files.every((entry) => /^[0-9a-f]{64}$/.test(entry.sha256)));

  const result = verifyEvidenceManifest(root, manifestPath);
  assert.deepEqual(result, {
    verdict: "PASS",
    fileCount: 2,
    totalBytes: 53,
  });
});

test("verification rejects mutated, missing, and extra evidence files", () => {
  const mutatedRoot = fixture();
  const mutatedManifest = path.join(mutatedRoot, "evidence_manifest.json");
  createEvidenceManifest(mutatedRoot, mutatedManifest);
  fs.appendFileSync(path.join(mutatedRoot, "checkpoint.json"), " ");
  assert.throws(
    () => verifyEvidenceManifest(mutatedRoot, mutatedManifest),
    /size mismatch: checkpoint\.json/,
  );

  const missingRoot = fixture();
  const missingManifest = path.join(missingRoot, "evidence_manifest.json");
  createEvidenceManifest(missingRoot, missingManifest);
  fs.unlinkSync(path.join(missingRoot, "rung_1000000", "metrics.json"));
  assert.throws(
    () => verifyEvidenceManifest(missingRoot, missingManifest),
    /file set mismatch/,
  );

  const extraRoot = fixture();
  const extraManifest = path.join(extraRoot, "evidence_manifest.json");
  createEvidenceManifest(extraRoot, extraManifest);
  fs.writeFileSync(path.join(extraRoot, "unexpected.txt"), "not in manifest");
  assert.throws(
    () => verifyEvidenceManifest(extraRoot, extraManifest),
    /file set mismatch/,
  );
});

test("manifest creation refuses symlinked evidence", () => {
  const root = fixture();
  fs.symlinkSync(path.join(root, "checkpoint.json"), path.join(root, "linked_checkpoint.json"));

  assert.throws(
    () => createEvidenceManifest(root, path.join(root, "evidence_manifest.json")),
    /symlink is not allowed: linked_checkpoint\.json/,
  );
});
