# Competitor Headroom Scale Contract

**Frozen:** 2026-07-26
**Track:** Track A competitor headroom — 64M operational specimen

This contract was frozen before collecting any candidate measurements. It does not revise the
July 25 or July 26 scale-ceiling results.

## Purpose

- Establish a larger, correctness-checked single-machine text-search specimen than the strongest
  current official operational document-count examples published by Typesense and Meilisearch.
- Preserve the existing exact-count, rank-1 sentinel, latency, locality, runtime, and evidence
  gates.
- Prevent a measured operational result from being misrepresented as proof against a theoretical
  address-space limit.

## Official comparison baseline

Sources were retrieved on 2026-07-26.

| Product | Official statement | Comparable classification | Source |
|---|---:|---|---|
| Typesense | Measured search benchmark at 28,000,000 books, 4 vCPU, 46 concurrent searches/s, 28 ms average processing time | operational search specimen | https://typesense.org/docs/overview/benchmarks.html |
| Meilisearch | Official importer handles datasets from thousands to 40+ million documents | operational import claim; not a search-latency specimen | https://www.meilisearch.com/docs/getting_started/integrations/meilisearch_importer |
| Meilisearch | Maximum documents per index approximately 4.3 billion; maximum index about 80 TiB, with less than 2 TiB recommended | structural limit; not evidence of safe operation at that count | https://www.meilisearch.com/docs/resources/help/faq |

The largest comparable operational count is 40,000,000. “Significantly more” is frozen as at least
1.5 times that count: 60,000,000. The candidate rung is 64,000,000.

Passing 64M permits only this statement:

> Flapjack has a saved 64M single-machine text-search specimen under the stated gates, larger than
> the cited Typesense measured specimen and Meilisearch importer claim.

It does not permit “Flapjack supports more documents than Meilisearch” because 64M does not exceed
Meilisearch’s approximately 4.3B structural per-index limit. A literal-limit win requires a
separate contract at or above 6.45B documents, an architecture and storage plan that can execute it,
and the same correctness and latency evidence. Projection, multiple unrelated indexes, or wording
changes cannot satisfy that requirement.

## Out of scope

- Weakening the frozen latency bars to reach a marketing number.
- Treating successful import without exact count, rank-1 search, and latency as safe handling.
- Comparing Flapjack’s compact profile to a competitor’s structural maximum as if record shapes
  and operating conditions were equivalent.
- Sharding, multi-node distribution, vectors, HA, or a 6.45B literal-limit campaign.

## Frozen success gates

A rung is green only when all of these pass:

- Exact live document count equals the cumulative target.
- Every sentinel query returns its expected object ranked first.
- Name/prefix p95 is at most 50 ms.
- Blended-all-query-types p95 is at most 100 ms.
- Each of the seven query types has 30 measured requests.
- Import, liveness, capacity, runtime, locality, evidence, and negative-control gates are
  determinate and green.

Missing, partial, projected, non-numeric, or unparseable evidence is never green. The July 26
document-count and batch-log repairs remain required.

## Reference ladders

Run compact and standard independently from fresh indexes through:

1. 1,000,000
2. 2,000,000
3. 4,000,000
4. 8,000,000
5. 16,000,000
6. 32,000,000
7. 64,000,000

Use the previously selected legal batch size of 10,000. Stop a profile at its first completed
latency failure or any correctness, liveness, capacity, runtime, or evidence failure. A failure in
one profile does not suppress the other profile.

The 64M operational-headroom verdict is `PASS` only if at least one profile reaches an exact green
64M rung. Report profiles independently; never imply the other profile passed.

## Capacity calibration

The static capacity estimates are frozen from the saved July 26 exact 1M reference specimens with a
1.5 safety factor:

| Profile | Source bytes/record | Index bytes/record | RSS bytes/record |
|---|---:|---:|---:|
| compact | 512 | 2,457 | 951 |
| standard | 2,048 | 6,003 | 1,635 |

Index calibration is `ceil(indexBytes / 1M * 1.5)` and RSS calibration is
`ceil(rssBytes / 1M * 1.5)`. The existing 50 GiB disk and 16 GiB memory reserves remain.

Every completed rung must compare observed cumulative index and RSS bytes per record to these
frozen allowances. Exceeding either allowance is a terminal capacity-calibration failure before
the next rung. This protects against a stale linear estimate.

## Reference locality and runtime

Reference claims require an AWS `i4i.4xlarge` whose data directory is proven to be XFS on local
`Amazon EC2 NVMe Instance Storage`. Capture the full machine, OS, git, and binary fingerprint.

Before each long tranche, use completed same-locality evidence to project its runtime. Do not
dispatch a tranche projected above 12 hours. Projections cannot establish a green rung.

## Publication and teardown

- Preserve every attempted rung with strict manifests.
- Copy evidence off instance-store using encrypted, versioned durable storage.
- Independently download, hash, extract, and verify the durable copy.
- Publish failed crossings and capacity stops as prominently as green results.
- Do not stop or terminate the instance while required evidence exists only on instance-store.
