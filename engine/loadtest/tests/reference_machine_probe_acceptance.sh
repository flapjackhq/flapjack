#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROBE="$LOADTEST_DIR/reference_machine_probe.sh"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

test_root="$(mktemp -d)"
trap 'rm -rf "$test_root"' EXIT
binary="$test_root/flapjack"
snapshot="$test_root/snapshot.json"
receipt="$test_root/receipt.json"
printf '#!/usr/bin/env bash\nexit 0\n' > "$binary"
chmod +x "$binary"

jq -n '{
  dataDir: "/srv/flapjack-scale/server_data",
  identityDocument: {
    instanceId: "i-0123456789abcdef0",
    instanceType: "i4i.4xlarge",
    region: "us-east-1"
  },
  findmnt: {
    filesystems: [{
      source: "/dev/nvme1n1",
      target: "/srv/flapjack-scale",
      fstype: "xfs"
    }]
  },
  lsblk: {
    blockdevices: [{
      name: "nvme1n1",
      kname: "nvme1n1",
      type: "disk",
      size: 3750000000000,
      model: "Amazon EC2 NVMe Instance Storage",
      mountpoints: ["/srv/flapjack-scale"]
    }]
  },
  system: {
    uname: "fixture",
    lscpu: "fixture",
    free: "fixture",
    nvme: "fixture"
  }
}' > "$snapshot"

bash "$PROBE" \
  --snapshot-file "$snapshot" \
  --binary "$binary" \
  --git-sha 0123456789abcdef0123456789abcdef01234567 \
  --output "$receipt"
jq -e '
  .assessment.verdict == "GO" and
  .assessment.reference.instanceType == "i4i.4xlarge" and
  .assessment.reference.backingModel == "Amazon EC2 NVMe Instance Storage" and
  (.binarySha256 | test("^[0-9a-f]{64}$"))
' "$receipt" >/dev/null || fail "known-good fixture receipt is incomplete"

jq '.lsblk.blockdevices[0].model = "Amazon Elastic Block Store"' \
  "$snapshot" > "$test_root/ebs_snapshot.json"
if bash "$PROBE" \
  --snapshot-file "$test_root/ebs_snapshot.json" \
  --binary "$binary" \
  --git-sha 0123456789abcdef0123456789abcdef01234567 \
  --output "$test_root/ebs_receipt.json"; then
  fail "EBS negative control unexpectedly passed"
fi
jq -e '
  .assessment.verdict == "INVALID" and
  (.assessment.reasons | index("backingModel"))
' "$test_root/ebs_receipt.json" >/dev/null || {
  fail "EBS negative control failed without preserving the exact locality reason"
}

echo "PASS: reference-machine probe accepts local NVMe and rejects EBS with durable evidence"
