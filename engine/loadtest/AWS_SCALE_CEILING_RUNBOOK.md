# AWS Scale-Ceiling Reference Runbook

**Owner:** engine / loadtest  
**Applies to:** Track A reference-machine provisioning, execution, pause, and teardown

## Purpose

Run the frozen scale-ceiling contract on an AWS `i4i.4xlarge` without mistaking EBS performance for
local NVMe performance or losing the only copy of checkpoint and rung evidence.

This runbook does not authorize changing the frozen latency bars, tuning Flapjack during a run, or
promoting a projected rung to Guaranteed.

## Instance-store lifecycle rule

Stop, hibernate, and terminate erase instance-store data. A reboot preserves instance-store data,
but it is not a substitute for copying evidence to durable storage.

The scale index, server state, generated input for an active rung, and local checkpoint must all be
treated as disposable until copied off the instance. A `checkpoint.json` and rung metrics alone are
not sufficient to resume after instance-store loss: the matching server data directory is required
too.

For an ordinary between-rung pause, use `scale_ladder.sh --stop-after-rung ...` and leave the EC2
instance running. If spend must end, copy every required result and the complete server data
directory to S3 or EBS, verify the copied checksums before any stop or termination, and treat a
later restore as a fresh fail-closed resume validation.

## 1. Verify cloud identity and discover live state

Use the secret file from the source repository, not a worktree copy:

```bash
TRACK_A_SECRET_FILE="/Users/stuart/repos/gridl-dev/flapjack_dev/engine/.secret/.env.secret"
test -s "$TRACK_A_SECRET_FILE"
source "$TRACK_A_SECRET_FILE"
aws sts get-caller-identity
```

Resolve the instance by its Name tag on every connection attempt. Never use a cached IP or a local
state file:

```bash
TRACK_A_NAME="flapjack-scale-ceiling-track-a"
aws ec2 describe-instances \
  --filters "Name=tag:Name,Values=$TRACK_A_NAME" \
  --query 'Reservations[].Instances[].{Id:InstanceId,State:State.Name,Type:InstanceType,IP:PublicIpAddress}' \
  --output table
```

If the named instance is stopped, start it and wait with the AWS CLI before resolving its new IP.
Assume any previous instance-store index is gone; do not attempt to resume it. If more than one
non-terminated instance has the Name tag, fail and resolve the ambiguity before connecting.

## 2. Prove the reference machine and storage locality

Before importing any records, save these commands and their complete output in the run results:

```bash
uname -a
lscpu
free -b
lsblk -o NAME,TYPE,SIZE,MODEL,SERIAL,FSTYPE,MOUNTPOINTS
findmnt -T /srv/flapjack-scale
sudo nvme list
```

The run is reference-valid only when all of the following automated checks pass:

- EC2 metadata/CLI reports exactly `i4i.4xlarge`.
- `findmnt` resolves the server data directory to the intended data filesystem.
- Every block device backing that filesystem is identified by `lsblk`/NVMe metadata as local
  instance-store NVMe, not `Amazon Elastic Block Store`, the root filesystem, or an unknown device.
- The saved fingerprint includes instance ID/type, vCPU, RAM, CPU model, kernel, mount, backing
  devices, git SHA, and release-binary SHA-256.

An indeterminate backing device is a failure. Do not redirect the data directory to EBS to make the
check pass; EBS results are outside the frozen reference contract.

## 3. Build and run on the instance

Build the x86_64 release binary on the reference machine, record `git rev-parse HEAD`, and record
`sha256sum` for the exact binary. Run the health/create/index/search known-answer probe before the
throughput projection or paid ladder.

Use a persistent terminal multiplexer or service supervisor for the ladder process. The harness
owns liveness, exact-count, sentinel, capacity, checkpoint, and evidence gates; do not bypass a red
gate manually.

## 4. Preserve evidence during the run

After every completed rung:

1. Confirm `metrics.json`, `dataset_cleanup.json`, `checkpoint.json`, and `run_receipt.json` parse.
2. Create and locally verify the strict manifest:
   `node engine/loadtest/lib/evidence_manifest.mjs create --root <results> --manifest
   <results>/evidence_manifest.json`, followed by the corresponding `verify` command.
3. Copy results to S3 or a separately mounted EBS volume.
4. Download the durable copy to a separate directory and run the same `verify` command there.

For a failed or interrupted rung, preserve the current generated dataset, server log, failure
evidence, results, checkpoint, and server data before teardown. Missing or unverified copied
evidence fails closed.

## 5. Pause and teardown

- Process-only pause: stop after a green rung with the harness checkpoint option; keep the instance
  running and verify the live count plus sentinels again on resume.
- Reboot: allowed only when operationally necessary; instance-store data should remain, but resume
  still must pass the exact live count, saved metrics, and sentinel checks.
- Stop/hibernate: destructive to the local NVMe run state. Use only after the durable-copy manifest
  passes, and never describe the stopped instance as directly resumable.
- Terminate: allowed only after all attempted-rung evidence has passed durable checksum
  verification.

Record the final EC2 state and durable evidence location in the run receipt. Guaranteed remains the
largest rung whose complete green evidence was copied and verified.
