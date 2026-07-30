# Scale Remediation Contract

**Frozen:** 2026-07-26
**Contract ID:** `flapjack-scale-remediation-64m-standard-2026-07-26`
**Profile:** standard
**Certification target:** 64,000,000 records

## Relationship to the historical contract

This contract supersedes nothing. It governs only the repaired scale campaign.
`COMPETITOR_HEADROOM_CONTRACT_2026_07_26.md` remains the sole authority for the completed
July 26 campaign and its recorded verdicts. Historical receipts are not reclassified by this
contract and cannot satisfy it.

## Purpose

- Certify an exact, green 64M standard-profile specimen under independent latency-family,
  correctness, capacity, locality, evidence, and dispatch gates.
- Keep every executable threshold in its existing code owner.
- Fail closed where the load-test harness does not yet enforce a named requirement.

## Measured entry condition

The repaired campaign starts from a measured deficit, not a passing baseline. Under these gates,
the historical standard 2M rung is **RED** because facet p95 was `315.727 ms` against the
independent `100 ms` gate. A green historical 1M rung is not progress past that measured failure.

## Frozen gates

| Gate | Value | Enforcement owner |
|---|---:|---|
| Name/prefix search p95 | ≤ 50 ms | `NAME_PREFIX_P95_LIMIT_MS` in `engine/loadtest/lib/scale_rung_verdict.mjs:7-21` |
| Per-query-family p95 | ≤ 100 ms | `PER_QUERY_TYPE_P95_LIMIT_MS` in `engine/loadtest/lib/scale_rung_verdict.mjs:7-21` |
| Blended search p95 | ≤ 100 ms | `BLENDED_P95_LIMIT_MS` in `engine/loadtest/lib/scale_rung_verdict.mjs:7-21` |
| Measured search requests | = 30 × 7 | `SEARCH_SAMPLES_PER_TYPE` and `REQUIRED_QUERY_TYPES` in `engine/loadtest/lib/scale_rung_verdict.mjs:7-21` |
| Forward latency projection | GO through 64,000,000 records | `FINAL_CERTIFICATION_TARGET` at `engine/loadtest/lib/scale_latency_projection.mjs:14` and `evaluateScaleLatencyProjection()` |
| Forward import-runtime projection | ≤ 12 h | `engine/loadtest/lib/scale_projection.mjs` |
| Capacity allowances per record | As frozen | `engine/loadtest/lib/scale_capacity_observation.mjs` plus `engine/loadtest/COMPETITOR_HEADROOM_CONTRACT_2026_07_26.md:84-99` |
| Count/health p99 during import | ≤ 250 ms | `liveness_distribution()` in `engine/loadtest/lib/loadtest_shell_helpers.sh`, called by `evaluate_rung_liveness()` in `engine/loadtest/scale_ladder.sh`, with contract coverage in `engine/loadtest/tests/scale_ladder_liveness_gate_acceptance.sh` |
| Five-second count timeouts during import | zero | `FLAPJACK_LOADTEST_LIVENESS_TIMEOUT_SECONDS:-5` in `sample_liveness_endpoint()` in `engine/loadtest/lib/loadtest_shell_helpers.sh` owns the sampling timeout; `liveness_distribution()` owns zero-timeout accounting, called by `evaluate_rung_liveness()` in `engine/loadtest/scale_ladder.sh`, with contract coverage in `engine/loadtest/tests/scale_ladder_liveness_gate_acceptance.sh` |

Each completed rung must still provide an exact final count, passing rank-1 sentinels, valid
same-locality evidence, durable evidence, and green capacity observation. Missing, partial,
projected, non-numeric, or unparseable evidence is not green.

## Projection and dispatch

Runtime and latency are peer dispatch gates. A rung must not be paid for when either projector
crosses its frozen bound:

- `scale_latency_projection.mjs` exits `0` for `GO`, `1` for `REFUSE`, and `2` for
  `INVALID` or otherwise unusable input.
- `scale_projection.mjs` uses a different CLI convention: callers read refusal from the emitted
  `.verdict`, because its `NO_GO` result is not a non-zero exit-code contract. Its non-zero status
  is reserved for invalid input or CLI failure.

Projection authorizes only whether another measurement may be dispatched. It never converts a
projected target into a completed green rung.

## Receipt qualification

A remediation receipt qualifies only when it carries this exact contract ID, names the
`standard` profile, targets at least 64,000,000 records, and passes every existing exact-green
headroom requirement. Compact-profile receipts and receipts governed by the historical contract
remain outside this remediation regime.
