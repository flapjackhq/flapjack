<!-- [scrai:start] -->
## analytics

| File | Summary |
| --- | --- |
| collector.rs | Stub summary for collector.rs. |
| config.rs | Stub summary for config.rs. |
| hll.rs | Minimal HyperLogLog implementation for distributed unique user counting.



Precision p=14: 16,384 registers, ~0.8% error, 16KB per sketch.

Uses SHA-256 for hashing (already a dependency via `sha2` crate). |
| manifest.rs | Stub summary for manifest.rs. |
| merge.rs | Tests for the analytics merge module, covering each merge strategy (top-K, rates, weighted averages, histograms, category counts, HLL user counts, currency revenue, overview) and verifying correct endpoint-to-strategy routing. |
| mod.rs | Search analytics engine powered by DataFusion + Parquet.



Tracks search events automatically and click/conversion events via the Insights API.

Data is stored in Parquet files with Hive-style date partitioning and queried

using DataFusion SQL for efficient analytics aggregation. |
| retention.rs | Stub summary for retention.rs. |
| schema.rs | Analytics event types (search and insight), their Arrow/Parquet schemas, and Algolia-spec validation logic. |
| seed.rs | Generate realistic demo analytics data for onboarding.



Writes Parquet files directly to the analytics directory,

producing 30 days of realistic search + click + conversion events. |
| types.rs | Shared types for cluster analytics fan-out and merge. |
| writer.rs | Stub summary for writer.rs. |

| Directory | Summary |
| --- | --- |
| query | The query subdirectory contains analytics query handlers for Flapjack's observability layer, including click-through rate and position analytics, search event metrics, filter usage, and user behavior tracking. |
<!-- [scrai:end] -->
