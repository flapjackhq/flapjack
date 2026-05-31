# Flapjack Loadtest Evidence

## PL-12 v2 HA Peer-Failed Amplification Contract (May 29, 2026)

### Scenario Owner

- Probe: [`engine/_dev/s/manual-tests/ha-peer-failed-amp-probe.sh`](../_dev/s/manual-tests/ha-peer-failed-amp-probe.sh)
- Acceptance contract: [`engine/loadtest/tests/ha_peer_failed_amplification_acceptance.sh`](tests/ha_peer_failed_amplification_acceptance.sh)
- Stage 1 calibration evidence: [`docs/research/pl12_stage1_baseline.md`](../../docs/research/pl12_stage1_baseline.md)
- Stage 3 no-tune decision: [`docs/research/pl12v2_stage2_tune_plan.md`](../../docs/research/pl12v2_stage2_tune_plan.md)

### Final Contract Posture

- Ratio threshold retired as gate: the prior `(peer_down+1)/(baseline+1)` metric degenerates when the steady-state baseline is near zero, so it is no longer used to pass/fail the acceptance.
- Absolute peer-down bound calibrated to `MAX_PEER_DOWN_LITERAL=94` via the high-variance fallback (`CV > 0.30`, `ceil(max(max_observed * 2, 50))`) from the Stage 2 post-read-index-fix sample (`max_observed=47`, `aggregate_cv_raw_peer_down_count=0.425083`).
- Acceptance gate: `MIN_PEER_DOWN <= raw_peer_down_count <= MAX_PEER_DOWN`.
- Circuit-breaker threshold kept at default (`DEFAULT_FAILURE_THRESHOLD=3`) per Stage-3 like-for-like cap-hit evidence — `raw_peer_down_count` is not a monotonic proxy for queue-cap pressure, so there is no causal basis to retune from this sample.

## PL-13 Single-doc Durable Write Throughput (May 28, 2026)

### Scenario Owner

- Scenario: `engine/loadtest/scenarios/pl13_single_doc_durable.js`
- Full method and command transcript: `engine/docs/research/pl13_stage3_throughput_delta.md`
- Evidence directories:
  - `engine/loadtest/results/20260528T205829Z-pl13-single-doc-baseline/`
  - `engine/loadtest/results/20260528T205829Z-pl13-single-doc-durable/`

### Measured Pre/Post Delta

| Condition | Server SHA | http_reqs/s | write latency avg (ms) | write latency p90 (ms) | write latency p95 (ms) | write_http_4xx_rate | write_http_unexpected_4xx_rate | write_http_5xx_rate | WRITE_RESPONSE_CHECKS successes |
|---|---|---:|---:|---:|---:|---:|---:|---:|---|
| Pre-PL-13 baseline (`main`) | `2b44fa8a9292fc60673f1455212cadd5e88269fe` | `638.793141` | `1.492544` | `3.5925` | `3.7563` | `0.00%` | `0.00%` | `0.00%` | `250 / 250 / 250` |
| PL-13 durable HEAD | `6a922a0a6be2e158fc670699ef0026533cce54ea` | `5.290291` | `188.638336` | `202.2203` | `203.4953` | `0.00%` | `0.00%` | `0.00%` | `250 / 250 / 250` |

### Slowdown Classification

- Throughput slowdown factor (`baseline / durable`): `120.7482x`.
- This is greater than the Stage 3 alert threshold (`10x`), so this magnitude must be carried forward in the stage handoff.
- Final direct HEAD closeout rerun evidence: `engine/loadtest/results/20260528T210217Z-pl13-single-doc-final-head/` (`http_reqs/s=5.427263`, `write_http_unexpected_4xx_rate=0.00%`, `write_http_5xx_rate=0.00%`).

## PL-10 Stage 6 Dual-Scenario Revalidation (May 28, 2026)

### Run Metadata

- Run timestamp (UTC): 2026-05-28T06:25:47Z
- Canonical classification artifact: `engine/docs/research/pl10_stage6_dual_scenario_classification.md`
- Results directory: `engine/loadtest/results/20260528T062547Z-pl10-stage6-dual-scenario/`
- Head SHA: `fea67c90fd4dce88c53c470129ff65f132226889`
- Environment assumptions: release binary target at `127.0.0.1:17700`, `--no-auth`, seeded loadtest data, local single-node run

### Exact Commands

- Server: `/Users/stuart/parallel_development/flapjack_dev/may27_8pm_1_pl10v2_write_throughput/flapjack_dev/engine/target/release/flapjack --no-auth --bind-addr 127.0.0.1:17700 --data-dir /Users/stuart/parallel_development/flapjack_dev/may27_8pm_1_pl10v2_write_throughput/flapjack_dev/engine/loadtest/results/20260528T062547Z-pl10-stage6-dual-scenario/server_data`
- Seed: `FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:17700 bash engine/loadtest/seed-loadtest-data.sh`
- Mixed soak (60m): `cd engine/loadtest && FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:17700 FLAPJACK_LOADTEST_SOAK_DURATION=60m k6 run scenarios/mixed-soak.js`
- Realistic batch soak (30m): `cd engine/loadtest && FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:17700 FLAPJACK_LOADTEST_SOAK_DURATION=30m k6 run scenarios/realistic_batch_soak.js`

### Measured Stage 6 Throughput + Classification

| Scenario | Duration | Throughput (http_reqs/s) | Successful writes | Write failure % | write_http_5xx_rate | write_http_unexpected_4xx_rate | Contract-health verdict |
|---|---:|---:|---:|---:|---:|---:|---|
| `mixed-soak.js` | `60m` | `921.538969/s` | `196274` | `85.04%` | `0.00%` | `0.00%` | PASS |
| `realistic_batch_soak.js` | `30m` | `831.652577/s` | `26688` | `98.21%` | `0.00%` | `0.00%` | PASS |

### Threshold Ownership

Threshold and contract semantics are owned by `engine/loadtest/lib/throughput.js`:

- `SOAK_WRITE_THRESHOLDS` governs sustained-write soak classification.
- Stage 6 contract-health PASS requires `write_http_5xx_rate=0.00%` and `write_http_unexpected_4xx_rate=0.00%` from retained stdout metrics.
- High `write_http_4xx_rate` is expected under saturation (dominantly `429`) and does not by itself fail Stage 6 contract health.

## PL-10 Stage 4 Regression Envelope (May 31, 2026)

### Purpose

Stage 4 regression verification after Stage 2 (env-driven write-queue batch-size resolver) and Stage 3 (PL-10 acceptance harness). Confirms adjacent behavior is still correct at current HEAD.

### Run Metadata

- Run timestamp (UTC): 2026-05-31T20:06:32Z
- Results directory: `engine/loadtest/results/20260531T200632Z-pl10-saturation-acceptance/`
- Head SHA: `7d3007ea5afc8aa9369b666b64f60b0d593b4aa1`
- Harness owner: `engine/loadtest/tests/pl10_saturation_acceptance.sh`

### Regression Commands and Results

| Command | Result | Count |
|---|---|---:|
| `cargo test -p flapjack --lib -- index::write_queue::tests` | PASS | 14 tests |
| `cargo test -p flapjack --lib -- index::manager::tests::wait_for_write_durable` | PASS | 2 tests |
| `cargo test -p flapjack-http --test ack_on_durable_integration` | PASS | 7 tests |
| `cargo test -p flapjack-replication` | PASS | 39 tests |
| `cargo test -p flapjack-server` | PASS | 8 tests |
| `bash engine/loadtest/tests/throughput_acceptance.sh` | PASS | — |
| `bash engine/loadtest/tests/benchmark_k6_errors.sh` | PASS | 7 checks |
| `bash engine/loadtest/tests/pl10_saturation_acceptance.sh` | PASS | — |
| `cargo fmt --all -- --check` | PASS | — |
| `cargo clippy -p flapjack -p flapjack-http -p flapjack-replication -p flapjack-server --all-targets -- -D warnings` | PASS | — |
| `./s/test --unit --integ --server` | PASS | 1044 integ + unit + server |

### PL-10 Acceptance Metrics (Stage 4 rerun)

| Metric | Value |
|---|---|
| `http_reqs` | 180,573 (1491.3/s) |
| `successful_writes` | 47,880 |
| `write_http_5xx_rate` | 0.00% |
| `write_http_unexpected_4xx_rate` | 0.00% |
| `write_http_4xx_rate` | 0.00% |
| write latency avg | 502.93ms |
| write latency p95 | 893.85ms |
| search latency avg | 22.23ms |
| search latency p95 | 68.65ms |

### Threshold Ownership

Thresholds and contract semantics are owned by `engine/loadtest/lib/throughput.js` (Stage 1/3 contract wording). Stage 4 reuses existing contract definitions — no threshold redefinition.

## PL-10 Stage 3 Saturation Acceptance (May 31, 2026)

### Run Metadata

- Run timestamp (UTC): 2026-05-31T19:49:01Z
- Results directory: `engine/loadtest/results/20260531T194901Z-pl10-saturation-acceptance/`
- Head SHA: `fa3d78812aff842dd91f3a7aa751c3a37fee90c2`
- Scenario owner: `engine/loadtest/scenarios/mixed-soak.js`
- Harness owner: `engine/loadtest/tests/pl10_saturation_acceptance.sh`

### Stage 1 Contract Wording Applied

Under `mixed-soak.js` with `FLAPJACK_LOADTEST_WRITE_VUS=200` and `FLAPJACK_LOADTEST_SOAK_DURATION=2m`, require:

- `successful_writes >= 45000`
- while preserving write contract: `write_http_5xx_rate == 0.00%` and `write_http_unexpected_4xx_rate == 0.00%`
- baseline-unset must fail the same saturation target

### Acceptance Verdict

| Case | `successful_writes` | Saturation target (`>=45000`) | `write_http_5xx_rate` | `write_http_unexpected_4xx_rate` | Verdict |
|---|---:|---:|---:|---:|---|
| Baseline unset | `27048` | FAIL | `0.00%` | `0.00%` | Contract preserved, saturation target miss |
| Tuned `FLAPJACK_WRITE_QUEUE_BATCH_SIZE=64` | `54783` | PASS | `0.00%` | `0.00%` | Contract preserved, saturation target pass |

## Stage 3 Soak Proof (Mar 28, 2026)

### Run Metadata

- Run date (UTC): 2026-03-28T05:06:35Z
- Run date (local): 2026-03-28 01:06:35 EDT
- Results: stored locally under `engine/loadtest/results/` (gitignored, not included in the repository)
- Mixed-soak command: `cd engine/loadtest && FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:7701 FLAPJACK_LOADTEST_SOAK_DURATION=2h bash soak_proof.sh --scenario mixed-soak`
- Write-soak command: `cd engine/loadtest && FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:7702 FLAPJACK_LOADTEST_SOAK_DURATION=2h bash soak_proof.sh --scenario write-soak`
- Release binary: `engine/target/release/flapjack`
- Important note: these two soak proofs were run concurrently on the same host. Treat them as sustained-behavior evidence, not isolated max-throughput claims.

### Hardware and OS

- CPU: Apple M4 Max
- RAM: 36.00 GiB (`38654705664` bytes)
- OS: macOS 26.0.1 (Build 25A362)
- Kernel: Darwin 25.0.0 (arm64)

### What These Soaks Prove

- the missing multi-hour artifact gap is now closed
- both 2h runs completed and wrote restart-verified proof artifacts
- both runs stayed free of `5xx` responses and unexpected non-`429` `4xx` responses
- memory pressure never rose above `0`
- post-soak and post-restart counts matched exactly on the same data dirs

### What These Soaks Do Not Prove

- cluster-wide performance guarantees
- hardware-independent SLOs
- that this specific VU count / duration is the only overload profile worth testing (different concurrency or payload shapes may surface different behavior)

### Soak Summary

| Scenario | k6 exit | Latency drift (5-minute windows) | Overload outcome | Memory / pressure | Restart + consistency |
|---|---:|---|---|---|---|
| `mixed-soak` | `99` | search `p95=16-18ms`, `p99=24-28ms`; write `p95=13-15ms`, `p99=21-25ms` | `429=99.21%`, accepted writes=`50750`, `5xx=0`, unexpected non-`429` `4xx=0` | RSS `92784 -> 542432 KB` (max `600496`); heap `25412792 -> 146221104` (max `168398752`); pressure `0` | read count stayed `1000`; write count stayed `50750`; seeded `MacBook` hits stayed `40` |
| `write-soak` | `99` | write `p95=10-11ms`, `p99=15-18ms` | `429=99.81%`, accepted writes=`50410`, `5xx=0`, unexpected non-`429` `4xx=0` | RSS `92160 -> 135856 KB` (max `158448`); heap `26248640 -> 128030864` (max `142691584`); pressure `0` | read count stayed `1000`; write count stayed `50410`; seeded `MacBook` hits stayed `40` |

### Interpretation

The Stage 3 soak proof is now materially stronger than the earlier short-run baseline:

- sustained multi-hour traffic no longer needs to be inferred from short benchmarks
- restart-after-soak behavior is now proven on the same data dir, not assumed
- the write path remained bounded and fail-closed under prolonged overload instead of degrading into `5xx` or inconsistent post-restart state

#### Why the 2026-03-28 runs exited `99` and what changed

Both 2h runs exited `99` because they used the short-baseline `WRITE_THRESHOLDS`, which cap `http_req_failed{type:write}` at `rate<0.99` and require `rate>0.01` forward progress. Under sustained 12-VU overload for 2h, 99.2-99.8% of writes correctly received `429` backpressure, breaching those bounds.

This was a **threshold classification problem**, not an engine defect. The engine behavior was correct at every point: bounded latency, zero `5xx`, zero unexpected `4xx`, exact post-restart count preservation.

**Resolution (2026-03-28):** Soak scenarios now use `SOAK_WRITE_THRESHOLDS` (defined in `lib/throughput.js`), which:

- Allow `http_req_failed{type:write}` up to `rate<1.0` (sustained saturation is expected)
- Allow `write_http_4xx_rate` up to `rate<1.0` (all `4xx` under soak are expected `429`s)
- Still require zero unexpected `4xx` (`rate<0.005`) and zero `5xx` (`rate<0.005`)
- Still require bounded latency (`p95<1000ms`, `p99<2000ms`)
- Still require forward progress (`rate>0.001` — at least some writes must succeed)

The short-baseline `WRITE_THRESHOLDS` remain unchanged for `write-throughput.js` and `mixed-workload.js`, which test normal (non-saturated) operation.

The canonical threshold definitions live in `engine/loadtest/lib/throughput.js`.

### Soak Evidence Sources

- `engine/loadtest/soak_proof.sh`
- Run artifacts stored locally under `engine/loadtest/results/` (gitignored)

---

## HA Soak Proof (Mar 30, 2026)

### Run Metadata

- Run date (UTC): 2026-03-30
- Harness: dev-repo HA soak harness
- Validation command: dev-repo HA soak harness (default 2h)
- Short smoke: dev-repo HA soak harness with `FLAPJACK_LOADTEST_SOAK_DURATION=2m` and `FLAPJACK_HA_SOAK_RESTART_INTERVAL_SECONDS=30`
- Compose target: `engine/examples/ha-cluster/docker-compose.yml`
- Load scenario: `engine/loadtest/scenarios/mixed-soak.js` (15 read VUs + 4 write VUs)
- Results: stored locally under `engine/loadtest/results/` (gitignored)

### Hardware and OS

- CPU: Apple M4 Max
- RAM: 36.00 GiB (`38654705664` bytes)
- OS: macOS 26.0.1 (Build 25A362)
- Kernel: Darwin 25.0.0 (arm64)

### Configuration

| Parameter | Default | Source |
|---|---|---|
| Soak duration | `2h` | `FLAPJACK_LOADTEST_SOAK_DURATION` |
| Restart interval | `180s` | `FLAPJACK_HA_SOAK_RESTART_INTERVAL_SECONDS` |
| Node rotation | `node-a → node-b → node-c → repeat` | Hardcoded in harness |
| Load balancer | `http://127.0.0.1:7800` (nginx) | `FLAPJACK_LOADTEST_BASE_URL` |
| Convergence timeout | `120s` | `FLAPJACK_HA_SOAK_CONVERGENCE_TIMEOUT_SECONDS` |
| k6 per-request JSON | Disabled | Cluster evidence comes from CSV/log artifacts, not per-request metrics |

### Harness Classification

| Field | Value |
|---|---|
| Final classification | `warning-findings` |
| Convergence result | `diverged` |
| k6 exit code | `99` |
| Restart count | `39` |

The harness classifies via `classify_soak_result()`: `PASS` requires convergence reached AND k6 exit 0; `warning-findings` indicates either divergence or non-zero k6 exit without hard failure. This run received `warning-findings` because document counts diverged across the three nodes and k6 thresholds breached under sustained overload.

### What This Soak Demonstrates

- The 3-node nginx-routed compose topology survives 2h of continuous write+search traffic while nodes restart in rotation (39 restarts across 3 nodes)
- Each restarted node returns to healthy state and resumes serving traffic (pre-serve catch-up via `run_pre_serve_catchup`)
- nginx `proxy_next_upstream` reroutes around failed nodes within 1-2 requests
- The harness automatically records restart timestamps, node health, per-node document counts, and cluster status at each restart and post-soak
- All three nodes remained healthy (`ok`) throughout the entire 2h run

### What This Soak Does Not Prove

- **Document convergence** — per-node counts diverged (see finding below)
- Leader election or automatic promotion (this compose topology has none)
- Load-balancer redundancy (nginx is a single point of failure in this example)
- Hardware-independent SLOs
- That all HA failure modes are covered (only single-node restart rotation is tested)

### Per-Node Consistency Finding

Document counts remained diverged across nodes at the end of the post-soak convergence window. The retained proof establishes a real topology/runtime gap, but it does **not** isolate a single root cause yet. The current evidence and code paths point to an interaction between nginx-routed restart windows and the current async replication/catch-up behavior:

- Writes can fail at the nginx layer while a target node is restarting (connection refused or timeout), so some client-visible write attempts are not committed.
- Replication to peers is asynchronous, and catch-up only replays operations that already exist in some peer oplog.
- The remaining follow-up is therefore a real product/topology decision: harden the example topology, add stronger client retry/write guidance, improve replication/catch-up behavior, or document the limitation as a boundary.

**Final post-soak document counts (`loadtest_write` index):**

| node-a | node-b | node-c |
|---:|---:|---:|
| 65,323 | 67,309 | 66,724 |

The divergence magnitude scales with write rate × restart duration × number of restarts. Over 39 restarts across 2h, the max divergence was ~1,986 docs (~3% of the highest count).

### Artifact Pack

Evidence artifacts (stored locally, gitignored):

| File | Contents |
|---|---|
| `summary.md` | Machine-generated run metadata and canonical classification fields |
| `cluster_samples.csv` | Timestamped per-node health and document counts at each restart and post-soak |
| `restart_events.csv` | Restart start/healthy timestamps per node |
| `cluster_status_snapshots.log` | Full cluster status JSON at each sample point |
| `mixed-soak.stdout.txt` | Full k6 progress output and final summary |

### Evidence Sources

- dev-repo HA soak harness
- `engine/loadtest/lib/loadtest_soak_helpers.sh`
- `engine/loadtest/scenarios/mixed-soak.js`
- `engine/loadtest/lib/config.js` (sharedLoadtestConfig)
- `engine/loadtest/lib/throughput.js` (SOAK_WRITE_THRESHOLDS)
- `engine/loadtest/tests/ha_soak_acceptance.sh`
- Local 2h soak run artifacts (gitignored)

---

## HA Soak Proof (May 26, 2026)

### Context

Diagnostic 2h soak following the L1 anti-entropy / strict bootstrap peer-coverage fix (`066549d5`). Goal: prove L1 closes the convergence boundary the Mar 30 soak surfaced. Run uses the script fix from `528235bf` (preserves per-node `/data` snapshots on non-converged runs).

### Run Metadata

- Run date (UTC): 2026-05-26 (soak started 19:16:18Z, completed ~21:25Z)
- Harness: dev-repo HA soak harness
- Validation command: `engine/_dev/s/manual-tests/ha-soak-test.sh`
- Compose target: `engine/examples/ha-cluster/docker-compose.yml`
- Load scenario: `engine/loadtest/scenarios/mixed-soak.js`
- Results: `engine/loadtest/results/20260526T191618Z-ha-soak/`

### Harness Classification

| Field | Value |
|---|---|
| Final classification | `warning-findings` |
| Convergence result | `diverged` (per script's strict-equality check) |
| k6 exit code | non-zero (write threshold breached under saturation) |
| Restart count | 36 |
| Per-node final docs | node-a=335,929 / node-b=337,184 / node-c=334,209 |
| Spread (max − min) / max | **0.88%** (vs Mar 30's ~3%) |
| Steady-state | All 24 polls in the 120s post-load convergence window held identical numbers — cluster reached steady state at 0.88%, not "still catching up" |
| Segment integrity | **11/11, 11/11, 9/9** segments in `meta.json` matched files on disk across all 3 nodes (vs the 2026-05-25 18:46Z fluke where meta referenced absent segments) |

### What This Soak Demonstrates

- **L1 anti-entropy + strict bootstrap peer-coverage works as designed.** The `c1_ownership` and `c3_replica_freshness` contracts hold; segment integrity recovers cleanly under sustained restart pressure.
- **Convergence improved 3.4× over Mar 30** — steady-state spread is 0.88% (vs ~3%).
- **No node-zero failure mode.** All three nodes ended healthy with non-trivial doc counts. The 2026-05-25 18:46Z node-a→0-docs fluke (preserved evidence at `docs/research/2026_05_26_ha_soak_segment_inconsistency.md`) did not reproduce.

### What This Soak Does Not Prove

- **Strict zero-spread convergence.** The residual ~1% spread reflects writes lost at the nginx routing layer during restart windows (`engine/_dev/s/manual-tests/ha-soak-test.sh:244-247` documents this explicitly: writes lost during restarts are unrecoverable under the current nginx-routed topology). Closing that boundary requires client-side retry / write-buffering and is tracked as roadmap **PL-8**.
- **Write-queue saturation under sustained cross-replication.** In the 21:00-21:10Z window, node-b logged 24,356 and node-c logged 22,050 "Write-queue-full / peer-failed" lines; the 1000-op queue cap saturated simultaneously on both peers. node-b transiently stalled at 291,816 docs for 3 samples before recovering. Symptom of the saturation, not a segment-integrity bug. Resolution path is part of PL-8.

### Boundary Interpretation (canonical)

The Mar 30 entry above described this as a "Known boundary" between async replication and nginx-routed restarts. **That framing remains substantially correct for the current topology.** L1 narrowed the boundary materially (3% → ~1% steady-state spread; clean segment recovery; no node-zero failures). The remaining ~1% is the nginx-write-loss residual, not an engine bug; PL-8 tracks the work to close it.

### Artifact Pack

| File | Contents |
|---|---|
| `summary.md` | Machine-generated run metadata + classification |
| `cluster_samples.csv` | Per-node health and doc counts at each sample |
| `restart_events.csv` | Restart start/healthy timestamps per node |
| `cluster_status_snapshots.log` | Full cluster status JSON snapshots |
| `mixed-soak.stdout.txt` | k6 progress + final summary |
| `node_data/{node-a,node-b,node-c}/` | Preserved on-disk state per the `528235bf` script fix |

### Evidence Sources

- dev-repo HA soak harness (`engine/_dev/s/manual-tests/ha-soak-test.sh`)
- L1 fix commit `066549d5` (strict bootstrap peer coverage; touches `engine/flapjack-replication/src/manager.rs` + `engine/flapjack-http/src/startup_catchup.rs`)
- Script fix commit `528235bf` (preserves per-node `/data` snapshots on non-converged runs)
- Partial diagnostic evidence from the 2026-05-25 18:46Z run preserved at `docs/research/2026_05_26_ha_soak_segment_inconsistency.md`
- Local 2h soak run artifacts (gitignored, but `node_data/` snapshots are local-only)

---

## Short-Run Baseline (Stage 4)

## Run Metadata

- Run date (UTC): 2026-03-21T02:54:26Z
- Run date (local): 2026-03-20 22:54:26 EDT
- Results: stored locally under `engine/loadtest/results/` (gitignored)
- Runner command: `cd engine/loadtest && FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:7701 ./run.sh`
- Release build command: `cd engine && cargo build --release -p flapjack-server`
- Release binary: `engine/target/release/flapjack` (executable)

## Hardware and OS

- CPU: Apple M4 Max
- RAM: 36.00 GiB (`38654705664` bytes)
- OS: macOS 26.0.1 (Build 25A362)
- Kernel: Darwin 25.0.0 (arm64)

## Scenario Threshold Contracts

The 2026-03-21 run below was executed under the original Stage 4 write contract.
Current HEAD relaxes the write-side thresholds so expected single-node `429`
backpressure does not fail the baseline by itself; a fresh benchmark rerun is
still required if you want PASS/FAIL labels regenerated under the updated
contract.

- `smoke.js`: no explicit thresholds; hard failures come from in-script assertions (`check` + `fail`).
- `search-throughput.js`: spreads `SEARCH_THRESHOLDS` from `lib/throughput.js`.
  - `http_req_duration{type:search}`: `p(95)<500`, `p(99)<1000`
  - `http_req_failed{type:search}`: `rate<0.01`
  - `http_reqs{type:search}`: `rate>5`
  - `checks{search returns 200,hits array}`: `rate==1`
- `write-throughput.js`: spreads `WRITE_THRESHOLDS` from `lib/throughput.js`.
  - `http_req_duration{type:write}`: `p(95)<1000`, `p(99)<2000`
  - `http_req_failed{type:write}`: `rate<0.99`
  - `write_http_4xx_rate`: `rate<0.99`
  - `write_http_unexpected_4xx_rate`: `rate<0.005`
  - `write_http_5xx_rate`: `rate<0.005`
  - `checks{write returns 200, numeric taskID, objectIDs array}`: `rate>0.01`
- `mixed-workload.js`: applies both `SEARCH_THRESHOLDS` and `WRITE_THRESHOLDS`.
- `spike.js`: applies `SEARCH_THRESHOLDS`.
- `memory-pressure.js`: no explicit k6 threshold map; pass/fail is assertion-driven in scenario code.
- `write-soak.js`: spreads `SOAK_WRITE_THRESHOLDS` from `lib/throughput.js` (relaxed for sustained overload).
  - `http_req_duration{type:write}`: `p(95)<1000`, `p(99)<2000`
  - `http_req_failed{type:write}`: `rate<1.0` (sustained saturation expected)
  - `write_http_4xx_rate`: `rate<1.0`
  - `write_http_unexpected_4xx_rate`: `rate<0.005`
  - `write_http_5xx_rate`: `rate<0.005`
  - `checks{write returns 200, numeric taskID, objectIDs array}`: `rate>0.001`
- `mixed-soak.js`: applies `SEARCH_THRESHOLDS` and `SOAK_WRITE_THRESHOLDS`.

## Current Interpretation At HEAD

What the current evidence does prove:

- search-heavy traffic on the documented single-node baseline stays fast with low error rates
- write overload is backpressure-limited rather than crash-prone on this setup
- memory-pressure mode assertions pass under the documented restart harness
- large-dataset import and search numbers provide a useful same-machine baseline
- the committed repo now also has 2h soak artifacts plus restart-verified summaries for prolonged mixed/write overload behavior

What it does not prove:

- cluster-wide performance guarantees
- hardware-independent SLOs
- that every overload pattern is safe just because one k6 profile passed

For deliberate single-node write overload, the current contract treats `429 Too Many Requests`
as acceptable intentional backpressure. The failure boundary is not "zero `429`s";
the failure boundary is:

- unexpected non-`429` client errors above the documented threshold
- `5xx` responses above the documented threshold
- latency breaching the documented write thresholds
- total loss of forward progress, reflected by successful write-task publication dropping below the documented minimum

The canonical threshold source is `engine/loadtest/lib/throughput.js` — `WRITE_THRESHOLDS` for short baselines and `SOAK_WRITE_THRESHOLDS` for multi-hour sustained overload scenarios.

## Benchmark Summary (From k6 stdout)

| Scenario | Status | p50 latency | p95 latency | p99 latency | Throughput (http_reqs/s) | Notes |
|---|---|---:|---:|---:|---:|---|
| smoke | PASS | 618us | 34.56ms | n/a (not printed) | 67.055935 | 9/9 checks passed |
| search-throughput | PASS | 3.63ms | 10.74ms | 17.87ms | 2567.89946 | all thresholds passed |
| write-throughput | FAIL | 4.02ms | 6.53ms | 9.11ms | 1685.898731 | write backpressure (`429`) breached failure-rate thresholds |
| mixed-workload | FAIL | 3.8ms | 8.9ms | n/a (type-specific p99: search 12.55ms, write 14.28ms) | 2940.152034 | write-side thresholds breached (`http_req_failed{type:write}`, `write_http_4xx_rate`) |
| spike | PASS | 7.43ms | 22.34ms | 31.4ms | 2390.306952 | all thresholds passed |
| memory-pressure | PASS | 560us | 3.07ms | n/a (not printed) | 601.757131 | pressure-mode assertions all passed |

### Threshold Breach Detail

- `run.sh` completed all six scenarios and exited with `99` because two scenarios breached thresholds (`write-throughput`, `mixed-workload`).
- `write-throughput` k6 summary:
  - `http_req_failed{type:write}`: 98.38%
  - `write_http_4xx_rate`: 98.38%
  - `write_http_5xx_rate`: 0.00%
  - raw status counts (`write-throughput.json`, `http_reqs`): `429=116105`, `200=1910`
- `mixed-workload` k6 summary:
  - `http_req_failed{type:write}`: 96.22%
  - `write_http_4xx_rate`: 96.22%
  - `http_req_failed{type:search}`: 0.00%
  - raw status counts (`mixed-workload.json`, `http_reqs`): `429=48889`, `200=156925`

## Caveats

- This is a single-node baseline run on one developer machine.
- Results are environment-specific (hardware, OS, local runtime conditions).
- This is useful operational evidence, not a product performance guarantee.
- The short-run baseline below is not the same thing as the Stage 3 soak proof above.
- These numbers are not product performance guarantees.
- Raw run artifacts under `engine/loadtest/results/<timestamp>/` are gitignored (`*` with `!.gitignore`), so this document is the committed summary artifact.

## Reproduction

Loadtest scripts and configuration are in the `engine/loadtest/` directory. This document serves as the committed benchmark summary.

## Evidence Sources

- `engine/loadtest/run.sh`
- `engine/loadtest/lib/loadtest_shell_helpers.sh`
- `engine/loadtest/lib/throughput.js`
- `engine/loadtest/scenarios/smoke.js`
- `engine/loadtest/scenarios/search-throughput.js`
- `engine/loadtest/scenarios/write-throughput.js`
- `engine/loadtest/scenarios/mixed-workload.js`
- `engine/loadtest/scenarios/spike.js`
- `engine/loadtest/scenarios/memory-pressure.js`
- Run artifacts stored locally under `engine/loadtest/results/` (gitignored)

---

# Large-Dataset Baseline

## Run Metadata
- Baseline generated at (UTC): 2026-03-23T18:03:59.735Z
- Import benchmark timestamp: 2026-03-23T15:52:55.271Z
- Search benchmark timestamp: 2026-03-23T15:53:18.574Z

## Hardware and OS
- CPU: Apple M4 Max
- RAM: 36.00 GiB
- OS: macOS 26.0.1 (Build 25A362)
- Kernel: Darwin 25.0.0 arm64

## Import Throughput
| Metric | Value |
| --- | --- |
| Index | benchmark_100k |
| Total docs | 100000 |
| Batches | 100 |
| Wall clock (ms) | 48402 |
| Avg batch latency (ms) | 397.6 |
| P95 batch latency (ms) | 507 |
| P99 batch latency (ms) | 578 |

## Search Latency
| Metric | Value |
| --- | --- |
| Index | benchmark_100k |
| Doc count | 100000 |
| Wall clock (ms) | 7036 |
| Overall avg (ms) | 85.7 |
| Overall p95 (ms) | 128 |
| Overall p99 (ms) | 195 |

## k6 Concurrent Load
| Scenario | Status | p95 (ms) | p99 (ms) |
| --- | --- | ---: | ---: |
| smoke | PASS | 69.04 | 69.04 |
| search-throughput | PASS | 12.10 | 19.65 |
| write-throughput | PASS | 5.84 | 8.62 |
| mixed-workload | PASS | 9.70 | 14.73 |
| spike | PASS | 33.12 | 59.65 |
| memory-pressure | PASS | 2.76 | 2.76 |

## Dashboard Timings
- not available

## Reproduction
- Dataset size: 100000
- Import batch size: 1000
- k6 search concurrency (VUs): 20
- Build mode: release
- Import command: `bash engine/loadtest/import_benchmark.sh`
- Search command: `bash engine/loadtest/search_benchmark.sh`
- k6 command: `bash engine/loadtest/run.sh`

## Evidence Sources
- Run artifacts (import, search, k6 scenarios) stored locally under `engine/loadtest/results/` (gitignored)
