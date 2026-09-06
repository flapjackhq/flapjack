<!-- [scrai:start] -->
## experiments

| File | Summary |
| --- | --- |
| assignment.rs | Deterministic A/B experiment assignment using MurmurHash3_x64_128 bucketing, with a priority cascade from user token to session ID to query ID for stable variant allocation. |
| config.rs | Stub summary for engine/src/experiments/config.rs. |
| interleaving.rs | Team-draft interleaving algorithm for A/B search experiments, combining two ranked result lists into a single interleaved list with deterministic team assignment and click attribution. |
| stats.rs | Statistical testing utilities for A/B experiments: delta-method and Welch z/t-tests, Bayesian beta-binomial comparison, CUPED variance reduction, interleaving preference scoring, SRM detection, guard-rail alerting, and sample-size estimation. |
| store.rs | Persistent, file-backed store for A/B experiment lifecycle management with atomic writes, numeric ID mapping, and single-active-per-index enforcement. |

| Directory | Summary |
| --- | --- |
| metrics | The metrics module aggregates per-user search and click event data from Parquet files to compute arm-level statistics for A/B testing experiments, supporting delta method z-tests, Welch's t-tests, and interleaving preference scoring. |
<!-- [scrai:end] -->
