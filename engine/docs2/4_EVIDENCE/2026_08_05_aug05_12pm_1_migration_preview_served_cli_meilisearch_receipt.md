# Served CLI Meilisearch Migration Preview Receipt

## PURPOSE

Record the release-profile proof that the shipped `flapjack migrate preview`
CLI reaches a running `flapjack-server`, reads a real loopback Meilisearch
source, renders human and JSON output, and preserves mixed translation-report
severities. The live setup and machine oracle remain owned by
`engine/tests/meilisearch_source_contract_kat.sh` and
`engine/tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/expected_bundle.json`.

Verification date: 2026-08-05.
Verification HEAD: `a45353d2822344ee3cd1cbc1ad73cf447abb8414`.

## Command And Exit Evidence

The checklist command ran inside a wall-clock bound:

```bash
cd engine && timeout 2400 bash tests/meilisearch_source_contract_kat.sh --preview-live > /tmp/s4_preview_live.log 2>&1
```

- `MIG20_LIVE_EXIT=0`
- Human CLI exit code: `9`
- JSON CLI exit code: `9`
- Build profile: `release`
- Release binary for both server and CLI: `target/release/flapjack`
- Meilisearch source URL: `http://127.0.0.1:17747`
- Flapjack server URL: `http://127.0.0.1:65457`
- Credentials: generated at runtime, passed through named environment
  variables, absent from combined CLI/server output, and not recorded here.
- Durable failure-evidence path: none. The probe passed; the KAT staged
  sanitized evidence before teardown, proved exact-name cleanup and secret
  absence, then removed the staged directory on the success path. A nonzero
  exit preserves `/tmp/flapjack_stage2_meilisearch_source_contract_failure_<pid>`.

Exit `9` is the expected CLI classification for a completed preview containing
a hard rejection. The enclosing live KAT exits `0` because it requires and
verifies that exact outcome.

## Hand-Calculated Expectations

The fixture owns `.documents.countAfter = 4`. The CLI request selects the one
fixture-owned `configured_pk` index, so the expected source counts are exactly
one index and four records. The seeded
`typoTolerance.disableOnNumbers = true` setting must yield at least one
`HardRejection`; the provider-wide Meilisearch limitations must yield at least
one `Warning`.

Independent counting of the eight JSON `report.entries` gives:

| Field | Expected from entries | Actual summary |
| --- | ---: | ---: |
| `totalEntries` | 8 | 8 |
| `hardRejections` | 1 | 1 |
| `warnings` | 2 | 2 |
| `scopeGaps` | 5 | 5 |
| `sourceCounts.indexes` | 1 | 1 |
| `sourceCounts.records` | 4 | 4 |

Report digest:
`4984d0de540d20f0b8c73e2894f59e79cedec67e9fa5c21185362783b92a8fc9`.

The exercised report-entry-kind denominator is `3/3`: `HardRejection`,
`Warning`, and `ScopeGap` all occur in the live report.

## Literal Human CLI Output

```text
total_entries=8 hard_rejections=1 warnings=2 scope_gaps=5 source_indexes=1 source_records=4 report_digest=4984d0de540d20f0b8c73e2894f59e79cedec67e9fa5c21185362783b92a8fc9
severity=ScopeGap code=ProductNotMigrated resource=Analytics jsonPath=$
severity=ScopeGap code=ProductNotMigrated resource=ApiKeys jsonPath=$
severity=ScopeGap code=ProductNotMigrated resource=Events jsonPath=$
severity=ScopeGap code=ProductNotMigrated resource=Experiments jsonPath=$
severity=ScopeGap code=ProductNotMigrated resource=Recommend jsonPath=$
severity=Warning code=MeilisearchDocumentOrderNotContractual resource=Settings jsonPath=$.documents
severity=Warning code=MeilisearchSearchPaginationNotExportBound resource=Settings jsonPath=$.pagination
severity=HardRejection code=MalformedSettingsPayload resource=Settings jsonPath=$.typoTolerance.disableOnNumbers
```

## JSON CLI Output Summary

```json
{
  "sourceCounts": {"indexes": 1, "records": 4},
  "summary": {
    "totalEntries": 8,
    "hardRejections": 1,
    "warnings": 2,
    "scopeGaps": 5
  },
  "reportDigest": "4984d0de540d20f0b8c73e2894f59e79cedec67e9fa5c21185362783b92a8fc9",
  "entrySeverityCounts": {
    "HardRejection": 1,
    "Warning": 2,
    "ScopeGap": 5
  }
}
```

## Cleanup Proof

The PASS receipt named the exact disposable container
`flapjack_stage2_meilisearch_source_contract` and temp directory
`tests/flapjack_stage2_meilisearch_source_contract_tmp`. After the run, the
container name was absent from `docker ps` and the temp directory did not
exist. The KAT also killed and waited for its exact server PID before removing
only the guarded `flapjack_server` data directory.
