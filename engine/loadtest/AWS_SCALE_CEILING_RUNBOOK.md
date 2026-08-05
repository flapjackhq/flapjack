# AWS Scale-Ceiling Reference Runbook

**Owner:** engine / loadtest  
**Applies to:** Track A reference-machine provisioning, execution, pause, and teardown

## Purpose

Run the frozen scale-ceiling contract on an AWS `i4i.4xlarge` without mistaking EBS performance for
local NVMe performance or losing the only copy of checkpoint and rung evidence.

This runbook does not authorize changing the frozen latency bars, tuning Flapjack during a run, or
promoting a projected rung to Guaranteed.

## Unattended execution

Use the one-shot runner when a caller can supply the setup, measurement, and evidence locations up
front. All eight flags are required and have no defaults:

```bash
FLAPJACK_AWS_SECRET_FILE="/Users/stuart/repos/gridl-dev/flapjack_dev/engine/.secret/.env.secret" \
bash engine/loadtest/remote/run_remote.sh \
  --run-id "<unique-run-id>" \
  --instance-type "<caller-selected-instance-type>" \
  --git-sha "$(git rev-parse HEAD)" \
  --setup-cmd '<caller-owned-setup-command>' \
  --measure-cmd '<caller-owned-measure-command>' \
  --evidence-remote '/home/ec2-user/flapjack-evidence' \
  --evidence-local '<local-evidence-directory>' \
  --budget-usd '<non-zero-budget>'
```

## Caller-owned values that have already burned a paid run

Three of the three reference-scale attempts on 2026-08-03 failed on caller-owned
configuration, not on the runner, the probe, or the product. Two are now refused
locally before launch; the third is a package fact this section records so it is
copied rather than re-derived. Read this list before composing an invocation.

- **`--evidence-remote` must be under `/home/ec2-user`, `/tmp`, or `/var/tmp`.**
  `bootstrap_instance` creates it with a plain `mkdir -p` as `ec2-user`, *before*
  `--setup-cmd` runs, so a setup command cannot pre-create or chown it. A run that
  passed `/srv/flapjack-scale` cleared every syntax check and then died on
  `mkdir: cannot create directory '/srv/flapjack-evidence': Permission denied`
  with the instance already billable. `validate_arguments` now refuses this for
  free; the refusal is pinned by
  `engine/loadtest/tests/remote_runner_billing_safety_selftest.sh`.
- **Do not put `curl` in the `--setup-cmd` package list on AL2023.** The AMI ships
  `curl-minimal`, and `dnf install curl` fails on the conflict, taking the whole
  setup with it. See
  [Package changes for curl and libcurl](https://docs.aws.amazon.com/linux/al2023/ug/curl-minimal.html).
  `curl` is already present; request only what is genuinely missing.
- **Run the loadtest gates on the exact SHA you are about to dispatch.** A run
  reached the instance, passed its locality probe, and then imported zero records
  because the dispatched SHA carried a seed-data pointer at a deleted path. The
  guard that catches this — `engine/loadtest/tests/foundation_acceptance.sh` —
  already existed and was already correct; nobody ran it against the dispatch SHA.
  `bash engine/loadtest/tests/foundation_acceptance.sh` is free and takes seconds.
- **Run the zero-cost runner selftest too.** `bash
  engine/loadtest/tests/remote_runner_billing_safety_selftest.sh` exercises the
  billing, teardown, and validation paths with every AWS call stubbed. It must
  print `PASS: remote runner billing-safety selftest` as its last line; a run that
  stops earlier exits non-zero by design.

The runner delegates rather than creating parallel policy owners:

- `aws_credential_preflight.sh` owns identity classification and fails the run before EC2 access.
- `lib/evidence_manifest.mjs create|verify` owns evidence integrity. The runner creates the manifest
  remotely and verifies the transferred tree locally.
- `reference_machine_probe.sh` is staged on the instance for the caller's `--measure-cmd`; the
  runner does not execute it. A reference-machine measurement must invoke the staged probe from the
  caller-owned command.
- The instance-store lifecycle and evidence-before-teardown rules below remain authoritative. The
  unattended entry point changes their execution mechanism, not their policy.

The runner captures and verifies evidence after the remote workload. If that forward-path capture
fails, or if the driver is interrupted during measurement, teardown reaps the active remote command
and makes one bounded capture-and-verify attempt before terminating the instance. A missing or
unverified copy keeps the run red. It does not leave a billable instance running indefinitely:
tag-scoped resource release retains a reserved part of the teardown deadline and still proceeds.
This bounded failure rule is the unattended interpretation of evidence-before-teardown; callers that
need recovery after a failed copy must also stream or checkpoint evidence to separately durable
storage during their own measurement command.

Startup cleanup and teardown are fail-closed and tag-scoped. Every destructive AWS query requires
`flapjack-batch=aug02_11am`. An instance is sweepable only when it also carries a
`flapjack-run-id` whose on-disk marker is dead; selection never uses instance type, `Name`, launch
age, or broad account state. Marker liveness is re-read immediately before deletion, so a live
concurrent run under the same batch tag is spared.

The unattended launch requires IMDSv2, encrypts its disposable gp3 root volume, restricts SSH to
the caller's current `/32`, and retains the first observed SSH host key in its private per-run work
directory so a later key substitution fails closed.

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

The numbered sections are the manual entry point. Automated consumers should start with
[Unattended execution](#unattended-execution) and use the sections below as the shared policy.

## 1. Verify cloud identity and discover live state

Use the secret file from the source repository, not a worktree copy:

```bash
TRACK_A_SECRET_FILE="/Users/stuart/repos/gridl-dev/flapjack_dev/engine/.secret/.env.secret"
bash engine/loadtest/aws_credential_preflight.sh "$TRACK_A_SECRET_FILE"
```

Run the preflight FIRST and treat a non-zero exit as a hard stop. Do not fall back to a bare
`source` + `aws sts get-caller-identity`, and do not run `aws login`.

**Why this replaced the bare source (2026-07-27).** The secret files across every repo used bare
`NAME=value` assignments with no `export` — 370 of them, zero exported. `source` therefore set SHELL
variables that never reached the `aws` child process, so the CLI silently resolved
`~/.aws/config`'s `[default]` browser-token profile instead and reported
`Your session has expired. Please reauthenticate using 'aws login'`. That message named neither real
fault: the environment was not being passed at all, and underneath it the long-lived `AKIA` keys were
themselves invalid. One fault masked the other and a paid measurement lane failed its identity gate
without ever provisioning. The preflight separates the cases and exits `1` (not exported), `2` (keys
rejected — plumbing fine), `3` (a browser/SSO session is still winning over the environment), `4`
(secret file missing), `5` (no `aws` CLI).

Prefer noninteractive short-lived role credentials from a dedicated least-privilege workload
identity, passed through a credential source accepted by the preflight. A browser/SSO session is
not usable for unattended work: it expires and its refresh is interactive. If the current
preflight-compatible environment cannot issue temporary credentials, treat a dedicated IAM user's
long-lived access key as a legacy fallback: scope it only to the required EC2 and S3 actions, rotate
it on a fixed schedule, and revoke it when the measurement lane is idle.

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

For the automated create-transfer-verify path, use [Unattended execution](#unattended-execution).
The durability requirements in this section apply to both entry points.

After every completed rung:

1. Confirm `metrics.json`, `dataset_cleanup.json`, `checkpoint.json`, and `run_receipt.json` parse.
2. Create and locally verify the strict manifest:
   `node engine/loadtest/lib/evidence_manifest.mjs create --root <results> --manifest
   <results>/evidence_manifest.json`, followed by the corresponding `verify` command.
3. Copy results to S3 or a separately mounted EBS volume.
4. Download the durable copy to a separate directory and run the same `verify` command there.

For a failed or interrupted rung, preserve the current generated dataset, server log, failure
evidence, results, checkpoint, and server data before teardown. Missing or unverified copied
evidence fails the run. The unattended runner makes a bounded final recovery attempt and then
continues mandatory resource release; it never converts a failed copy into an unbounded billing
exception.

## 5. Pause and teardown

The unattended runner owns its tag-scoped teardown; do not add manual account-wide cleanup around
it. The lifecycle outcomes below remain the policy for caller-owned measurement state.

- Process-only pause: stop after a green rung with the harness checkpoint option; keep the instance
  running and verify the live count plus sentinels again on resume.
- Reboot: allowed only when operationally necessary; instance-store data should remain, but resume
  still must pass the exact live count, saved metrics, and sentinel checks.
- Stop/hibernate: destructive to the local NVMe run state. Use only after the durable-copy manifest
  passes, and never describe the stopped instance as directly resumable.
- Manual terminate: allowed only after all attempted-rung evidence has passed durable checksum
  verification. The unattended runner's failure-path exception is defined above: after its bounded
  recovery attempt, mandatory tag-scoped release still runs and the overall result remains failed.

Record the final EC2 state and durable evidence location in the run receipt. Guaranteed remains the
largest rung whose complete green evidence was copied and verified.
