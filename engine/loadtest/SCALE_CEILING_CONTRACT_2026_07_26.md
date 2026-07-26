# Single-Machine Scale-Ceiling Follow-up Contract

**Frozen:** 2026-07-26  
**Track:** Track A follow-up — count liveness, import amplification, and ceiling localization

This is a new contract. It does not revise or overwrite the July 25 result or
`SCALE_CEILING_CONTRACT.md`. Candidate measurements collected under this contract must be
published separately from the immutable July 25 baseline.

## Purpose

- Replace the ladder's full-search liveness probe with the existing live document-count gauge.
- Remove per-object INFO amplification while retaining one useful batch-level event.
- Measure whether larger legal HTTP batches amortize per-commit work without changing correctness.
- Localize the frozen latency crossing for both compact and standard records.

## Out of scope

- Changing the July 25 Guaranteed result without a new green reference specimen.
- Sharding, multi-node distribution, vectors, or a different reference machine.
- Skipping upsert semantics or weakening exact-count, sentinel, latency, locality, or evidence gates.
- Treating laptop timing as reference-machine evidence.

## Frozen correctness and latency gates

A rung is green only when all of these pass:

- Exact live document count equals the cumulative target.
- Every sentinel query returns the expected object ranked first.
- Name/prefix p95 is at most 50 ms.
- Blended-all-query-types p95 is at most 100 ms.
- Import, search, liveness, capacity, and evidence guards all return determinate PASS/GO results.

Each query type uses 30 measured requests. Percentiles use the existing nearest-rank owner. Missing,
non-numeric, partial, projected, or unparseable evidence never counts as green.

## Liveness source

Long imports must poll `GET /1/usage/documents_count/{indexName}` and read the final
`documents_count[].v` point. That endpoint is the existing single-index, in-memory Tantivy segment
count owner. The liveness helper must:

- keep the five-second HTTP request bound and 60-second flat-count bound;
- reject a missing index, missing/empty series, a non-integer value, or a regressing count;
- prove with a negative control that a blocked full-search route cannot block the count probe; and
- retain the final independent exact-count and sentinel checks.

The full empty-query search used by the July 25 ladder is diagnostic baseline evidence, not a count
API, and must not remain in the liveness path.

## Batch logging contract

The batch handler may emit at most one INFO summary event per single-index batch request. The event
records the index and operation count. It must not emit one event per object. Error and warning
events remain unchanged.

## Reference A/B probe

Before the ladder, run two fresh 250,000-record compact imports on the reference machine and the
same candidate binary:

1. 1,000 records per HTTP batch.
2. 10,000 records per HTTP batch.

Both probes use unique generated object IDs and must pass exact count and sentinels. Record total
docs/s plus first, middle, and last import-latency deciles (count, p50, p95) and
`last_p50 / first_p50`. Select 10,000 for the ladder only if its total docs/s is at least the
1,000-record control and all correctness gates pass; otherwise select 1,000. This selection rule is
frozen before the A/B data exists.

The 10,000-record candidate is within the server's existing maximum batch size. No server limit is
raised for this track.

## Reference ladders

Run each profile independently through these cumulative rungs:

1. 10,000
2. 50,000
3. 100,000
4. 250,000
5. 500,000
6. 1,000,000

Profiles retain their July 25 definitions:

- `compact`: 160–240 serialized bytes, approximately 200 bytes.
- `standard`: the existing approximately 1 KB, 13-field product record.

The index grows incrementally within a profile. Stop that profile after its first completed latency
failure or any correctness/liveness/capacity/evidence failure. A compact failure does not suppress
the standard run. Record import time, docs/s, import deciles, disk bytes, RSS, per-query-type
p50/p95/p99, exact count, sentinel result, and batch size for every attempted rung.

## Reference locality and runtime

Reference claims require an AWS `i4i.4xlarge` with the index directory proven on local
`Amazon EC2 NVMe Instance Storage`, following `AWS_SCALE_CEILING_RUNBOOK.md`. Capture the full
hardware, OS, git, and binary fingerprint.

Before any long job, run a timed probe in that same locality and project the next tranche. Do not
dispatch a tranche projected above 12 hours. Laptop work is plumbing and capacity evidence only.

## Publication and teardown

- Preserve strict manifests and copy every attempted result off instance-store before teardown.
- Independently download and verify the durable copy.
- Publish the candidate curve beside, not over, the July 25 curve.
- Promote Guaranteed only to the largest newly saved green reference rung.
- Do not stop, hibernate, or terminate while required evidence exists only on instance-store.

