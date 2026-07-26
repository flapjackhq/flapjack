import assert from "node:assert/strict";
import test from "node:test";

import {
  EXPECTED_INSTANCE_STORE_MODEL,
  EXPECTED_INSTANCE_TYPE,
  evaluateReferenceMachine,
} from "./lib/reference_machine.mjs";

function validSpecimen() {
  return {
    dataDir: "/srv/flapjack-scale/server_data",
    identityDocument: {
      instanceId: "i-0123456789abcdef0",
      instanceType: "i4i.4xlarge",
      region: "us-east-1",
    },
    findmnt: {
      filesystems: [
        {
          source: "/dev/nvme1n1",
          target: "/srv/flapjack-scale",
          fstype: "xfs",
        },
      ],
    },
    lsblk: {
      blockdevices: [
        {
          name: "nvme0n1",
          kname: "nvme0n1",
          type: "disk",
          size: 100_000_000_000,
          model: "Amazon Elastic Block Store",
          mountpoints: [null],
        },
        {
          name: "nvme1n1",
          kname: "nvme1n1",
          type: "disk",
          size: 3_750_000_000_000,
          model: "Amazon EC2 NVMe Instance Storage",
          mountpoints: ["/srv/flapjack-scale"],
        },
      ],
    },
  };
}

test("reference evaluator accepts the exact i4i local-NVMe specimen", () => {
  assert.equal(EXPECTED_INSTANCE_TYPE, "i4i.4xlarge");
  assert.equal(EXPECTED_INSTANCE_STORE_MODEL, "Amazon EC2 NVMe Instance Storage");

  const result = evaluateReferenceMachine(validSpecimen());

  assert.equal(result.verdict, "GO");
  assert.deepEqual(result.reasons, []);
  assert.deepEqual(result.reference, {
    instanceId: "i-0123456789abcdef0",
    instanceType: "i4i.4xlarge",
    region: "us-east-1",
    dataDir: "/srv/flapjack-scale/server_data",
    mountTarget: "/srv/flapjack-scale",
    mountSource: "/dev/nvme1n1",
    filesystem: "xfs",
    backingDevice: "nvme1n1",
    backingModel: "Amazon EC2 NVMe Instance Storage",
    backingSizeBytes: 3_750_000_000_000,
  });
});

test("reference evaluator rejects EBS and the root filesystem", () => {
  const ebs = validSpecimen();
  ebs.findmnt.filesystems[0] = {
    source: "/dev/nvme0n1p1",
    target: "/",
    fstype: "ext4",
  };
  ebs.lsblk.blockdevices[0].children = [
    {
      name: "nvme0n1p1",
      kname: "nvme0n1p1",
      pkname: "nvme0n1",
      type: "part",
      size: 99_000_000_000,
      model: null,
      mountpoints: ["/"],
    },
  ];

  const result = evaluateReferenceMachine(ebs);

  assert.equal(result.verdict, "INVALID");
  assert.ok(result.reasons.includes("rootFilesystem"));
  assert.ok(result.reasons.includes("backingModel"));
  assert.ok(result.reasons.includes("backingSizeBytes"));
});

test("reference evaluator rejects ambiguous or unsupported mounted device topology", () => {
  const raid = validSpecimen();
  raid.findmnt.filesystems[0].source = "/dev/md0";
  raid.lsblk.blockdevices.push({
    name: "md0",
    kname: "md0",
    type: "raid0",
    size: 3_750_000_000_000,
    model: null,
    mountpoints: ["/srv/flapjack-scale"],
  });

  const result = evaluateReferenceMachine(raid);

  assert.equal(result.verdict, "INVALID");
  assert.ok(result.reasons.includes("backingDeviceType"));
});

test("reference evaluator fails closed on missing identity, mount, or unknown model", () => {
  const missingIdentity = validSpecimen();
  delete missingIdentity.identityDocument.instanceType;
  assert.deepEqual(evaluateReferenceMachine(missingIdentity).reasons, ["instanceType"]);

  const missingMount = validSpecimen();
  missingMount.findmnt.filesystems = [];
  assert.deepEqual(evaluateReferenceMachine(missingMount).reasons, ["mount"]);

  const unknownModel = validSpecimen();
  unknownModel.lsblk.blockdevices[1].model = null;
  assert.deepEqual(evaluateReferenceMachine(unknownModel).reasons, ["backingModel"]);
});
