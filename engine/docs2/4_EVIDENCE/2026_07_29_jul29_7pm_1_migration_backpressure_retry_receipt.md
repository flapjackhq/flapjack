# Migration Backpressure Retry Receipt

## Locality

Local real-server release-binary proof on the local laptop. The probe started `engine/target/release/flapjack` and exercised the stock HTTP bulk-replace route with normal admin-key authentication.

## Binary Fingerprint

- Build evidence: `BUILD_EXIT=0`
- SHA-256: `4afa679363155f9a62570516172245e6aa60a3c358ff2fd9b86aa1bae2d865b4  engine/target/release/flapjack`

## Knobs

- Attempt 1: PROBE_DOCS=220000 PROBE_CHECKPOINT=200 PROBE_WRITER_BUFFER=15000000 PROBE_SETTLE_TIMEOUT_SECS=2400 disposition=succeeded imported=220000 list_index_entries=220000 pause_copied=no retry_marker_count=0
- Attempt 2: PROBE_DOCS=440000 PROBE_CHECKPOINT=100 PROBE_WRITER_BUFFER=15000000 PROBE_SETTLE_TIMEOUT_SECS=2400 disposition=succeeded imported=440000 list_index_entries=440000 pause_copied=yes retry_marker_count=9
  - Discovered pause artifact path: `/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T//flapjack_backpressure_retry_probe_72598_1785414422.xxviDY/attempt_2/data/.publication/backpressure_retry_probe_72598_2/snapshot_e101d9329c05429f894fbb300f58b9db/staging/write_backpressure_pause.json`
  - Captured pause artifact path in probe workspace: `/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T//flapjack_backpressure_retry_probe_72598_1785414422.xxviDY/attempt_2/captured_write_backpressure_pause.json`

## Retry Marker

- Token: `flapjack.migration.bulk_build.backpressure_retry`
- Attempt 1 marker line count: 0
- Attempt 2 marker line count: 9
  - `2026-07-30T12:45:03.510395Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=517.25µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:45:33.558681Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=992.459µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:48:03.864612Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=417.125µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:48:34.001557Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=456.417µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:49:04.082760Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=716.167µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:49:34.184885Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=472.459µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:50:04.182943Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=431.958µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:50:34.294195Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=407.583µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`
  - `2026-07-30T12:51:04.312163Z  WARN flapjack_http::handlers::migration::bulk_build: flapjack.migration.bulk_build.backpressure_retry attempt=1 elapsed=457.833µs retry_cap=900s staging_tenant=staging error=Index paused for migration: staging write backpressure: segment ceiling persisted without improvement across the bounded window`

## Captured Pause Artifacts

### Attempt 2

```json
{
  "tenant_id": "staging",
  "decision": "pause",
  "reason": "segment ceiling persisted without improvement across the bounded window",
  "selected_segment_band": [
    2,
    9
  ],
  "selected_segment_ceiling": 9,
  "window_size": 3,
  "improvement_verdict": "not_improving",
  "observations": [
    {
      "state": "determinate",
      "sampled_at_ms": 1785415834288,
      "live_segment_count": 14,
      "live_docs": 415100,
      "per_segment_doc_counts": {
        "086b30bf2b154fdea9881db75bcc33e1": 10600,
        "1db5657da3fc4a768863476f9614ea28": 100,
        "3f373d0815ce4fb89bc28ee5ab238d42": 91100,
        "9b76caee610d4e20880fc5d51d1a1a5b": 10600,
        "b2930fe45fc341a9b9a78c9031efca08": 16900,
        "b46ede7e2733491fbbba24539c062c86": 10600,
        "b4960e67259f40d795f12be570b85241": 91100,
        "be4bf66686794b5aa037d764b6a25dfc": 100,
        "cd8fb98c73b947a0bbc2b8c7e1c8d622": 100,
        "dda86879e5d84142b52435f3f8b47fec": 91100,
        "eb3962bdbe3044ada659fd4f4665a638": 100,
        "f1626e65ed2f40ce97a9a5757d5ed450": 91100,
        "fddee6f7c4684d5f949d4840bbdd3ecf": 100,
        "ff6dc96251af4200916fff0715a8e002": 1500
      },
      "managed_index_file_count": 85,
      "index_bytes": 107792411,
      "orphan_file_set_count": 0,
      "orphan_file_set_ids": []
    },
    {
      "state": "determinate",
      "sampled_at_ms": 1785415835302,
      "live_segment_count": 14,
      "live_docs": 415100,
      "per_segment_doc_counts": {
        "086b30bf2b154fdea9881db75bcc33e1": 10600,
        "1db5657da3fc4a768863476f9614ea28": 100,
        "3f373d0815ce4fb89bc28ee5ab238d42": 91100,
        "9b76caee610d4e20880fc5d51d1a1a5b": 10600,
        "b2930fe45fc341a9b9a78c9031efca08": 16900,
        "b46ede7e2733491fbbba24539c062c86": 10600,
        "b4960e67259f40d795f12be570b85241": 91100,
        "be4bf66686794b5aa037d764b6a25dfc": 100,
        "cd8fb98c73b947a0bbc2b8c7e1c8d622": 100,
        "dda86879e5d84142b52435f3f8b47fec": 91100,
        "eb3962bdbe3044ada659fd4f4665a638": 100,
        "f1626e65ed2f40ce97a9a5757d5ed450": 91100,
        "fddee6f7c4684d5f949d4840bbdd3ecf": 100,
        "ff6dc96251af4200916fff0715a8e002": 1500
      },
      "managed_index_file_count": 85,
      "index_bytes": 107792612,
      "orphan_file_set_count": 0,
      "orphan_file_set_ids": []
    },
    {
      "state": "determinate",
      "sampled_at_ms": 1785415864300,
      "live_segment_count": 17,
      "live_docs": 430800,
      "per_segment_doc_counts": {
        "086b30bf2b154fdea9881db75bcc33e1": 10600,
        "0d3cf69b24ad45e0be29b92afb48db77": 100,
        "3f373d0815ce4fb89bc28ee5ab238d42": 91100,
        "4dc0d71e72604f478aaa8a87443cad3c": 100,
        "6a3be05d229e486a9e0c7e960f744640": 100,
        "8f70992637ca499195bcc17de0553952": 100,
        "9868d04a8c5c4fd687f0af758d678464": 10600,
        "9b76caee610d4e20880fc5d51d1a1a5b": 10600,
        "a5f26a7c8f374af1b668b56cae7a726c": 100,
        "b2930fe45fc341a9b9a78c9031efca08": 16900,
        "b46ede7e2733491fbbba24539c062c86": 10600,
        "b4960e67259f40d795f12be570b85241": 91100,
        "c4de9dbb24574a6ab135c5a424607b06": 100,
        "cd74e8bde6224d07be07e4635c1ecde1": 6400,
        "dda86879e5d84142b52435f3f8b47fec": 91100,
        "e6b8e4f4768e412d8ae5c7dd92232056": 100,
        "f1626e65ed2f40ce97a9a5757d5ed450": 91100
      },
      "managed_index_file_count": 109,
      "index_bytes": 118757963,
      "orphan_file_set_count": 1,
      "orphan_file_set_ids": [
        "16e07501e5a646128b2910fb4a82d32d"
      ]
    }
  ]
}
```

## Verdict

PASS: release-binary migration met a real write-backpressure pause, emitted the retry marker, settled succeeded, and preserved the submitted document count.

## Evidence Workspace

- Preserved workspace: `/var/folders/v6/b8qh29l57ql_p7hdw2qhpqkw0000gn/T/flapjack_backpressure_retry_probe_72598_1785414422.xxviDY`
