#!/usr/bin/env node

import crypto from "node:crypto";
import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

function hashFile(filePath) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(filePath));
  return hash.digest("hex");
}

function manifestRelativePath(root, manifestPath) {
  const relative = path.relative(root, manifestPath);
  if (relative === "" || relative.startsWith("..") || path.isAbsolute(relative)) {
    throw new Error("manifest must be a file inside the evidence root");
  }
  return relative.split(path.sep).join("/");
}

function canonicalManifestPath(manifestPath) {
  const absolutePath = path.resolve(manifestPath);
  return path.join(fs.realpathSync(path.dirname(absolutePath)), path.basename(absolutePath));
}

function listEvidenceFiles(root, excludedRelativePath) {
  const files = [];

  function walk(directory, relativeDirectory) {
    const entries = fs
      .readdirSync(directory, { withFileTypes: true })
      .sort((left, right) => left.name.localeCompare(right.name));
    for (const entry of entries) {
      const absolutePath = path.join(directory, entry.name);
      const relativePath = path
        .join(relativeDirectory, entry.name)
        .split(path.sep)
        .join("/");
      if (relativePath === excludedRelativePath) {
        continue;
      }
      if (entry.isSymbolicLink()) {
        throw new Error(`symlink is not allowed: ${relativePath}`);
      }
      if (entry.isDirectory()) {
        walk(absolutePath, relativePath);
      } else if (entry.isFile()) {
        const stat = fs.statSync(absolutePath);
        files.push({
          path: relativePath,
          bytes: stat.size,
          sha256: hashFile(absolutePath),
        });
      } else {
        throw new Error(`unsupported evidence entry: ${relativePath}`);
      }
    }
  }

  walk(root, "");
  return files.sort((left, right) => left.path.localeCompare(right.path));
}

export function createEvidenceManifest(rootPath, manifestPath) {
  const root = fs.realpathSync(rootPath);
  if (!fs.statSync(root).isDirectory()) {
    throw new Error("evidence root is not a directory");
  }
  const absoluteManifest = canonicalManifestPath(manifestPath);
  const relativeManifest = manifestRelativePath(root, absoluteManifest);
  const files = listEvidenceFiles(root, relativeManifest);
  if (files.length === 0) {
    throw new Error("evidence root contains no files");
  }

  const manifest = {
    schemaVersion: 1,
    createdAt: new Date().toISOString(),
    algorithm: "sha256",
    rootLabel: path.basename(root),
    files,
  };
  const temporaryPath = `${absoluteManifest}.tmp.${process.pid}`;
  fs.writeFileSync(temporaryPath, `${JSON.stringify(manifest, null, 2)}\n`, {
    mode: 0o600,
  });
  fs.renameSync(temporaryPath, absoluteManifest);
  return manifest;
}

function validateManifest(manifest) {
  if (
    manifest?.schemaVersion !== 1 ||
    manifest?.algorithm !== "sha256" ||
    !Array.isArray(manifest?.files) ||
    manifest.files.length === 0
  ) {
    throw new Error("manifest schema is invalid");
  }
  const seen = new Set();
  for (const entry of manifest.files) {
    const safePath =
      typeof entry?.path === "string" &&
      entry.path.length > 0 &&
      !path.isAbsolute(entry.path) &&
      !entry.path.split("/").includes("..");
    if (
      !safePath ||
      !Number.isSafeInteger(entry?.bytes) ||
      entry.bytes < 0 ||
      typeof entry?.sha256 !== "string" ||
      !/^[0-9a-f]{64}$/.test(entry.sha256) ||
      seen.has(entry.path)
    ) {
      throw new Error("manifest file entry is invalid");
    }
    seen.add(entry.path);
  }
}

export function verifyEvidenceManifest(rootPath, manifestPath) {
  const root = fs.realpathSync(rootPath);
  const absoluteManifest = canonicalManifestPath(manifestPath);
  const relativeManifest = manifestRelativePath(root, absoluteManifest);
  const manifest = JSON.parse(fs.readFileSync(absoluteManifest, "utf8"));
  validateManifest(manifest);
  const actualFiles = listEvidenceFiles(root, relativeManifest);
  const expectedPaths = manifest.files.map((entry) => entry.path);
  const actualPaths = actualFiles.map((entry) => entry.path);
  if (JSON.stringify(expectedPaths) !== JSON.stringify(actualPaths)) {
    throw new Error(
      `file set mismatch: expected=${JSON.stringify(expectedPaths)} actual=${JSON.stringify(actualPaths)}`,
    );
  }

  for (let index = 0; index < manifest.files.length; index += 1) {
    const expected = manifest.files[index];
    const actual = actualFiles[index];
    if (actual.bytes !== expected.bytes) {
      throw new Error(
        `size mismatch: ${expected.path} expected=${expected.bytes} actual=${actual.bytes}`,
      );
    }
    if (actual.sha256 !== expected.sha256) {
      throw new Error(`sha256 mismatch: ${expected.path}`);
    }
  }

  return {
    verdict: "PASS",
    fileCount: manifest.files.length,
    totalBytes: manifest.files.reduce((total, entry) => total + entry.bytes, 0),
  };
}

function parseCliArgs(args) {
  const [command, rootFlag, root, manifestFlag, manifest] = args;
  if (
    !["create", "verify"].includes(command) ||
    rootFlag !== "--root" ||
    !root ||
    manifestFlag !== "--manifest" ||
    !manifest
  ) {
    throw new Error(
      "Usage: evidence_manifest.mjs <create|verify> --root <dir> --manifest <path>",
    );
  }
  return { command, root, manifest };
}

function runCli() {
  if (process.argv[2] === "--help" || process.argv[2] === "-h") {
    process.stdout.write(
      "Usage: evidence_manifest.mjs <create|verify> --root <dir> --manifest <path>\n",
    );
    return;
  }
  const { command, root, manifest } = parseCliArgs(process.argv.slice(2));
  const result =
    command === "create"
      ? createEvidenceManifest(root, manifest)
      : verifyEvidenceManifest(root, manifest);
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
}

const currentFilePath = fileURLToPath(import.meta.url);
const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (currentFilePath === invokedPath) {
  try {
    runCli();
  } catch (error) {
    console.error(`FAIL: ${error.message}`);
    process.exit(1);
  }
}
