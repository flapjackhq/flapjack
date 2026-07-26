#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const EXPECTED_INSTANCE_TYPE = "i4i.4xlarge";
export const EXPECTED_INSTANCE_STORE_MODEL = "Amazon EC2 NVMe Instance Storage";
export const MINIMUM_INSTANCE_STORE_BYTES = 3_500_000_000_000;

function flattenBlockDevices(devices, output = []) {
  for (const device of devices ?? []) {
    output.push(device);
    flattenBlockDevices(device.children, output);
  }
  return output;
}

function deduplicate(values) {
  return [...new Set(values)];
}

function validAbsoluteDataPath(dataDir, mountTarget) {
  if (typeof dataDir !== "string" || !path.isAbsolute(dataDir)) {
    return false;
  }
  if (typeof mountTarget !== "string" || !path.isAbsolute(mountTarget)) {
    return false;
  }
  const relative = path.relative(mountTarget, dataDir);
  return relative === "" || (!relative.startsWith("..") && !path.isAbsolute(relative));
}

export function evaluateReferenceMachine(input) {
  const reasons = [];
  const identity = input?.identityDocument;
  if (identity?.instanceType !== EXPECTED_INSTANCE_TYPE) {
    reasons.push("instanceType");
  }
  if (typeof identity?.instanceId !== "string" || !/^i-[a-z0-9]+$/.test(identity.instanceId)) {
    reasons.push("instanceId");
  }
  if (typeof identity?.region !== "string" || identity.region.length === 0) {
    reasons.push("region");
  }

  const mounts = input?.findmnt?.filesystems;
  if (!Array.isArray(mounts) || mounts.length !== 1) {
    return {
      verdict: "INVALID",
      reasons: deduplicate([...reasons, "mount"]),
    };
  }
  const mount = mounts[0];
  if (!validAbsoluteDataPath(input?.dataDir, mount?.target)) {
    reasons.push("dataDirMount");
  }
  if (mount?.target === "/") {
    reasons.push("rootFilesystem");
  }
  if (typeof mount?.source !== "string" || !mount.source.startsWith("/dev/")) {
    reasons.push("mountSource");
  }
  if (typeof mount?.fstype !== "string" || mount.fstype.length === 0) {
    reasons.push("filesystem");
  }

  const devices = flattenBlockDevices(input?.lsblk?.blockdevices);
  const sourceName =
    typeof mount?.source === "string" ? path.basename(mount.source) : "";
  let backingDevice = devices.find(
    (device) => device?.name === sourceName || device?.kname === sourceName,
  );
  if (!backingDevice) {
    reasons.push("backingDevice");
  } else if (backingDevice.type === "part") {
    // A single partition is acceptable only when it resolves to one explicit parent disk.
    const parentName = backingDevice.pkname;
    backingDevice = devices.find(
      (device) => device?.name === parentName || device?.kname === parentName,
    );
    if (!backingDevice) {
      reasons.push("backingDevice");
    }
  }

  if (backingDevice && backingDevice.type !== "disk") {
    reasons.push("backingDeviceType");
  }
  const backingModel =
    typeof backingDevice?.model === "string" ? backingDevice.model.trim() : "";
  if (backingModel !== EXPECTED_INSTANCE_STORE_MODEL) {
    reasons.push("backingModel");
  }
  const backingSizeBytes = Number(backingDevice?.size);
  if (
    !Number.isSafeInteger(backingSizeBytes) ||
    backingSizeBytes < MINIMUM_INSTANCE_STORE_BYTES
  ) {
    reasons.push("backingSizeBytes");
  }

  const uniqueReasons = deduplicate(reasons);
  if (uniqueReasons.length > 0) {
    return {
      verdict: "INVALID",
      reasons: uniqueReasons,
    };
  }

  return {
    verdict: "GO",
    reasons: [],
    reference: {
      instanceId: identity.instanceId,
      instanceType: identity.instanceType,
      region: identity.region,
      dataDir: input.dataDir,
      mountTarget: mount.target,
      mountSource: mount.source,
      filesystem: mount.fstype,
      backingDevice: backingDevice.kname ?? backingDevice.name,
      backingModel,
      backingSizeBytes,
    },
  };
}

function runCli() {
  const [flag, inputPath] = process.argv.slice(2);
  if (flag === "--help" || flag === "-h") {
    process.stdout.write("Usage: reference_machine.mjs --input-file <path>\n");
    return;
  }
  if (flag !== "--input-file" || !inputPath) {
    throw new Error("Usage: reference_machine.mjs --input-file <path>");
  }

  const result = evaluateReferenceMachine(JSON.parse(fs.readFileSync(inputPath, "utf8")));
  process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
  if (result.verdict !== "GO") {
    process.exitCode = 1;
  }
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
