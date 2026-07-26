# Single-Machine Scale-Ceiling Contract

**Frozen:** 2026-07-25  
**Track:** Track A — single-machine text-search record ceiling

This contract defines the pass/fail bar before any reference-machine measurement is collected.
Laptop dry-run results are plumbing evidence and cannot change this contract or establish a ceiling.

## Ceiling definition

For each record profile, the single-machine ceiling is the largest completed ladder rung where both
of these latency requirements pass:

- Name/prefix search p95 ≤ 50 ms.
- Blended-all-query-types search p95 ≤ 100 ms.

A rung is not completed unless its exact document-count check and every sentinel rank-1 check pass.
Projected, partial, missing, or unparseable evidence never counts as green.

## Record profiles

- `compact`: approximately 200 serialized bytes per record, with a unique name and two or three
  short facet dimensions. The accepted size range is 160–240 bytes (200 bytes ± 20%).
- `standard`: the existing approximately 1 KB, 13-field product record. `standard` remains the
  generator default and its existing record output must remain byte-for-byte compatible.

Track A measures text search only. Vector-bearing records are out of scope.

## Ladder

Each profile uses the same cumulative record-count rungs:

1. 1,000,000
2. 4,000,000
3. 16,000,000
4. 32,000,000
5. 64,000,000

The index grows incrementally within a run. A rung result must record whether it was incremental or
a dedicated fresh import.

## Stop conditions

Any one of these conditions ends the ladder for that profile:

- Either frozen latency requirement is exceeded at a completed rung.
- Memory-pressure write shedding remains active and prevents the next exact-count rung from
  completing.
- The reference-locality throughput probe projects that the next rung will exceed the default
  12-hour per-run import budget.
- A liveness, exact-count, sentinel, evidence, or storage-locality guard fails.

The first failed rung is evidence but is not guaranteed capacity. The largest earlier green rung is
the measured ceiling.

## Reference machine

Reference results are valid only on an AWS `i4i.4xlarge` whose index directory is proven to reside on
local instance-store NVMe. Results from EBS, a laptop, or any other locality cannot be substituted or
used for the 32M/64M runtime projection.

Provisioning, storage-locality proof, evidence preservation, and instance lifecycle are governed by
`engine/loadtest/AWS_SCALE_CEILING_RUNBOOK.md`. Stop, hibernate, and terminate erase the required
instance-store state; checkpoint/resume does not waive that rule.

Every reference run must capture:

- EC2 instance type
- vCPU count
- RAM capacity
- `lsblk` output
- Index-directory mount and backing block device
- Kernel version
- CPU model
- Git commit SHA
- Release binary SHA-256

## Evidence and publication

Every attempted rung must preserve import time, docs/s, bytes on disk, RSS, per-query-type
p50/p95/p99, exact final count, and sentinel verdict before teardown. Missing evidence fails closed.

The published Guaranteed number is the largest saved green rung. Higher projected or attempted
numbers must be labeled Target or Stretch and cannot be promoted without a saved green result and
the complete reference-machine fingerprint.

Any future change to these frozen rules requires a newly dated contract version; reference results
must name the exact contract version they used.
