# 2026-08-02 S3 List Failure Propagation Receipt

## Scope

Stage 2 changed `engine/src/index/s3.rs` so S3 snapshot listing checks every
ListObjectsV2 HTTP response status before parsing the XML body. Rejected list
responses now return `FlapjackError::S3("S3 list: HTTP <status>")` instead of a
downstream `ListBucketResult` parse error.

## Closure Contract Discovery

Command:

```bash
cd engine && L=$(timeout 600 cargo test -p flapjack --lib -- --list 2>/dev/null); echo "$L" | grep -c 'upload_snapshot_fails_loudly_when_bucket_rejects_the_put'; echo "$L" | grep -c 'delete_snapshot_fails_loudly_when_bucket_rejects_delete'; echo "$L" | grep -c 'list_snapshots_fails_loudly_when_bucket_rejects_list'
```

Output:

```text
1
1
1
```

## Focused S3 Test Result

Command:

```bash
cd engine && timeout 600 cargo test -p flapjack --lib -- index::s3::tests
```

Final result:

```text
test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 2194 filtered out; finished in 0.01s
```

Non-zero S3 test count: 14.

## Full Validation

```text
PASS cd engine && timeout 600 cargo test -p flapjack --lib -- index::s3::tests
PASS cd engine && timeout 1800 cargo test -p flapjack --lib --no-fail-fast
PASS cd engine && cargo clippy -p flapjack
PASS cd engine && cargo fmt --check
PASS FLAPJACK_API=http://127.0.0.1:33110 bash engine/examples/s3-snapshot/test_snapshots.sh
```

Full library result:

```text
test result: ok. 2200 passed; 0 failed; 8 ignored; 0 measured; 0 filtered out; finished in 173.00s
```

## MinIO Harness Result

The standard host port `127.0.0.1:7700` was occupied by an unrelated process.
The proof therefore used a unique Compose project and a temporary, untracked
Compose override. Docker first selected free localhost ports `33107`, `33108`,
and `33110`; the final run pinned those temporary ports so the Flapjack endpoint
remained stable when the harness force-stopped and restarted its container.

Effective temporary override:

```yaml
services:
  minio:
    ports: !override
      - "127.0.0.1:33107:9000"
      - "127.0.0.1:33108:9001"
  flapjack:
    ports: !override
      - "127.0.0.1:33110:7700"
```

Actual green command sequence from the repository root:

```bash
export COMPOSE_PROJECT_NAME=fj_s3_list_stable_aug02_1913
export COMPOSE_FILE="$(pwd)/engine/examples/s3-snapshot/docker-compose.yml:/tmp/flapjack_s3_list_stage2_stable.uJ8o4L/compose.override.yml"
docker compose up -d --build
FLAPJACK_API=http://127.0.0.1:33110 bash engine/examples/s3-snapshot/test_snapshots.sh
docker compose down -v --remove-orphans
```

Before startup, `docker compose config --format json` was parsed and asserted to
contain exactly those three localhost-only publications. The run preserved the
foreign `127.0.0.1:7700` listener, made no repository changes, and removed the
temporary override during cleanup.

Observed terminal evidence:

```text
PASS: Snapshot status: uploaded
PASS: Listed 1 snapshot(s)
PASS: Restore status: restored
PASS: All docs restored: 3
PASS: Restore by key: restored
PASS: Scheduled backup ran: 1 -> 2 snapshots
PASS: Auto-restore startup path triggered
PASS: Auto-restore: search returns 1 hit
=== ALL TESTS PASSED ===
MINIO_HARNESS_EXIT=0
MINIO_STABLE_FINAL_EXIT=0
```

The first all-`:0` isolation probe also passed upload, list, both explicit
restore paths, and scheduled backup. On the forced container restart Docker
reassigned Flapjack's host port from `33110` to `33111`, so the unchanged
harness continued polling the old endpoint. Container logs showed auto-restore
had succeeded and the replacement endpoint was healthy. Pinning Docker's
selected free ports in the second temporary override removed that environment
artifact; no shipped harness change was needed.

## S3 Status Sweep

Commands:

```bash
grep -n "\.await" engine/src/index/s3.rs
grep -n "pub async fn" engine/src/index/s3.rs
```

Public S3 functions counted: `upload_snapshot`, `download_snapshot`,
`download_latest_snapshot`, `list_snapshots`, `delete_snapshot`, and
`enforce_retention`.

Sweep result: n=6, status-checked direct responses=4, additional instances found: 0.

Direct response handlers with status checks:

- `upload_snapshot`: checks `put_object_builder(...).execute().await` status.
- `download_snapshot`: checks `get_object(...).await` status.
- `list_snapshots`: delegates direct ListObjectsV2 response handling to
  `list_snapshot_page`, which checks status before XML parsing.
- `delete_snapshot`: checks `delete_object(...).await` status.

Delegating public functions:

- `download_latest_snapshot`: delegates to `list_snapshots` and `download_snapshot`.
- `enforce_retention`: delegates to `list_snapshots` and `delete_snapshot`.

## Parser Dependency Decision

`quick-xml` was already present in `Cargo.lock` through `rust-s3`, but the
`flapjack` crate could not import a transitive dependency directly. Stage 2 added
`quick-xml = { version = "0.38", features = ["serialize"], optional = true }`
as a direct dependency and wired it into the existing `s3-snapshots` feature.
No new XML parser or ad hoc XML parsing was introduced.

## ROADMAP CORRECTION REQUIRED

Proposed later-ledger closed `DUR-2` row text:

```text
DUR-2 | Closed | S3 snapshot listing now checks ListObjectsV2 HTTP status before XML parsing, propagates bucket list rejections as `S3 list: HTTP <status>`, preserves pagination, and is covered by `list_snapshots_fails_loudly_when_bucket_rejects_list` plus the green MinIO end-to-end proof in `engine/docs2/4_EVIDENCE/2026_08_02_aug02_11am_3_s3_list_failure_propagation_receipt.md`.
```
