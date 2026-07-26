# Load Test Runner

## Prerequisites

- k6
- Rust toolchain (cargo)
- curl
- jq
- node

## Configuration Contract

All load-test scripts use `engine/loadtest/lib/config.js::sharedLoadtestConfig` as the only environment contract.

Set these variables when overriding defaults:

- `FLAPJACK_LOADTEST_BASE_URL`
- `FLAPJACK_LOADTEST_APP_ID`
- `FLAPJACK_LOADTEST_API_KEY`
- `FLAPJACK_LOADTEST_READ_INDEX`
- `FLAPJACK_LOADTEST_WRITE_INDEX`
- `FLAPJACK_LOADTEST_SOAK_DURATION`
- `FLAPJACK_LOADTEST_WRITE_TARGET_RPS`
- `FLAPJACK_LOADTEST_TASK_MAX_ATTEMPTS`
- `FLAPJACK_LOADTEST_TASK_POLL_INTERVAL_SECONDS`

## Single-Machine Scale Ceiling

[`SCALE_CEILING_CONTRACT.md`](SCALE_CEILING_CONTRACT.md) owns the frozen profiles, rungs, latency
bars, and stop conditions. [`AWS_SCALE_CEILING_RUNBOOK.md`](AWS_SCALE_CEILING_RUNBOOK.md) owns the
reference-machine locality and evidence-preservation procedure.

The dated
[`SCALE_CEILING_CONTRACT_2026_07_26.md`](SCALE_CEILING_CONTRACT_2026_07_26.md) owns the independent
follow-up that replaces full-search liveness with the usage gauge, compares legal 1k and 10k HTTP
batches at 250k, and localizes the compact and standard crossings on intermediate rungs. It does
not revise the original contract or its July 25 result.

[`COMPETITOR_HEADROOM_CONTRACT_2026_07_26.md`](COMPETITOR_HEADROOM_CONTRACT_2026_07_26.md) owns the
subsequent 1M → 64M campaign. It freezes 60M as the operational comparison threshold, keeps that
claim separate from Meilisearch's structural per-index limit, requires exactly 30 measured requests
for all seven query types, and adds a post-rung observed-capacity gate before every checkpoint.

The ladder starts its own loopback-only release server, grows one index incrementally, and fails
closed on insufficient capacity, a flat document count, an inexact final count, missing rank-1
sentinels, invalid latency evidence, or an unsafe resume checkpoint:

```bash
bash scale_ladder.sh \
  --profile compact \
  --rungs 1000000,2000000,4000000,8000000,16000000,32000000,64000000 \
  --batch-size 10000 \
  --data-dir /srv/flapjack-scale/compact_ladder_data \
  --results-dir /durable/compact_ladder \
  --server-binary ../target/release/flapjack
```

For the dated competitor-headroom campaign, pass its frozen 1.5x calibration values:

```bash
# compact
SCALE_SOURCE_BYTES_PER_RECORD=512 \
SCALE_INDEX_BYTES_PER_RECORD=2457 \
SCALE_RSS_BYTES_PER_RECORD=951 \
bash scale_ladder.sh ...

# standard
SCALE_SOURCE_BYTES_PER_RECORD=2048 \
SCALE_INDEX_BYTES_PER_RECORD=6003 \
SCALE_RSS_BYTES_PER_RECORD=1635 \
bash scale_ladder.sh ...
```

The same values drive both the preflight projection and `capacity_observation.json`; a completed
rung whose observed cumulative bytes per record exceeds either allowance cannot be checkpointed.

Before a paid ladder, run the purpose-tagged `1M,4M,8M` compact probe with
`--throughput-probe`, then evaluate its three exact `metrics.json` files with
`lib/scale_projection.mjs`. A throughput probe may continue past a latency failure to answer the
import-runtime question, but its checkpoint cannot be resumed as a reference ladder checkpoint.
Only a reference-ladder rung that passes both frozen latency bars may establish Guaranteed.

After every terminal run, create and verify an exact evidence manifest:

```bash
node lib/evidence_manifest.mjs create \
  --root /durable/compact_ladder \
  --manifest /durable/compact_ladder/evidence_manifest.json
node lib/evidence_manifest.mjs verify \
  --root /durable/compact_ladder \
  --manifest /durable/compact_ladder/evidence_manifest.json
```

Copy and independently re-verify that directory before stopping or terminating a machine whose
index is on instance-store storage. Raw result directories are intentionally gitignored; the honest
curve and implementation fingerprints belong in [`BENCHMARKS.md`](BENCHMARKS.md).

## Full Suite Orchestration

From `engine/loadtest`:

```bash
./run.sh
```

The runner executes one normal pass and one pressure pass, and stores artifacts under `engine/loadtest/results/<timestamp>/`.

## Scenario Commands

Run from `engine/loadtest`:

```bash
k6 run scenarios/smoke.js
k6 run scenarios/search-throughput.js
k6 run scenarios/write-throughput.js
k6 run scenarios/mixed-workload.js
k6 run scenarios/spike.js
k6 run scenarios/mixed-soak.js
k6 run scenarios/write-soak.js
k6 inspect scenarios/memory-pressure.js
```

These direct `k6 run` commands assume `FLAPJACK_LOADTEST_BASE_URL` already points at a running, seeded server. Use `./seed-loadtest-data.sh` after starting the server to populate the read index and reset the write index before direct scenario runs. Use `./run.sh` when you want the fully managed normal pass, isolated write-index resets, and the pressure-pass restart flow.

## Scenario Intent

- `smoke.js`: validates health, read query, and write task publication.
- `search-throughput.js`: exercises read-index query throughput.
- `write-throughput.js`: exercises write-index batch throughput.
- `mixed-workload.js`: runs concurrent read and write pressure.
- `spike.js`: applies short burst traffic and recovery.
- `mixed-soak.js`: steady mixed read/write soak profile for longer confidence runs (default `sharedLoadtestConfig.soakDuration = 4h`).
- `write-soak.js`: write-heavy overload soak profile that should observe intentional `429` backpressure (default `sharedLoadtestConfig.soakDuration = 4h`).
- `memory-pressure.js`: validates behavior when the runner restarts with alternate memory settings.

## Results and Interpretation

For each scenario, the runner writes:

- k6 stdout summary: `<scenario>.stdout.txt`
- k6 JSON output: `<scenario>.json` for `run.sh`, gzipped `<scenario>.json.gz` for `soak_proof.sh`

Use stdout summaries for quick pass/fail checks and JSON outputs for detailed trend analysis and tooling.

## Current Overload Contract

The canonical write-overload contract lives in
`engine/loadtest/lib/throughput.js::WRITE_THRESHOLDS`.

For the single-node loadtest scenarios, sustained write-side `429 Too Many Requests`
responses are expected and acceptable under deliberate overload. They are treated as
intentional backpressure, not as a correctness failure by themselves.

The current write-path pass criteria are:

- keep write latency bounded (`p95<1000ms`, `p99<2000ms`)
- keep unexpected non-`429` client errors rare (`write_http_unexpected_4xx_rate<0.005`)
- keep server errors rare (`write_http_5xx_rate<0.005`)
- preserve forward progress instead of total saturation (`write returns 200` checks `rate>0.01`)

This suite is a short-run baseline, not a multi-hour soak test. For the current
evidence summary and its limits, see [BENCHMARKS.md](BENCHMARKS.md).

## Soak Scenario Designs

These scenarios are checked in for Stage 3 confidence-completeness work but are
not part of the default `./run.sh` baseline because they are intentionally long-running.

Suggested commands:

```bash
k6 run scenarios/mixed-soak.js
k6 run scenarios/write-soak.js
bash engine/loadtest/soak_proof.sh --scenario mixed-soak
bash engine/loadtest/soak_proof.sh --scenario write-soak
```

`soak_proof.sh` owns the repeatable Stage 3 proof flow for one soak scenario:

- starts a fresh local release binary on the configured base URL
- reseeds the read/write indices with the shared loadtest contract
- captures k6 stdout + JSON plus periodic RSS/heap samples
- gives each soak run its own k6 REST API address so parallel proofs do not fight over `localhost:6565`
- restarts the same server on the same data dir after the soak
- records post-soak and post-restart index-consistency checks

Override `FLAPJACK_LOADTEST_SOAK_DURATION` when you want a shorter or longer run
without editing the scenario files.

Set `FLAPJACK_LOADTEST_WRITE_TARGET_RPS` to a positive integer for
`scenarios/write-soak.js` when a fixed offered write rate is required. When this
override is unset, `write-soak.js` keeps its legacy 12-VU constant-VU profile.

Suggested evidence to capture alongside those runs:

- k6 stdout + JSON summaries for latency drift over time
- periodic server RSS or equivalent memory sampling from the host
- one server restart after the soak to confirm clean recovery
- one post-soak search consistency check on both the read and write indices

## Memory-Pressure Restart Contract

`run.sh` owns the pressure-mode restart flow. It stops the normal server, applies pressure-mode `FLAPJACK_MEMORY_LIMIT_MB`, `FLAPJACK_MEMORY_HIGH_WATERMARK`, and `FLAPJACK_MEMORY_CRITICAL` via one helper, waits for `/health`, reseeds data with `./seed-loadtest-data.sh`, verifies `/health` again, then runs only `scenarios/memory-pressure.js`.
