# Flapjack Shipped Capabilities

This document owns current shipped product capabilities and their limitations.
It deliberately does not own strategy, live work, release history, or test policy:

- [`PROJECT_OVERVIEW.md`](../../PROJECT_OVERVIEW.md) owns mission and strategic priority.
- The private development repository's Beads ledger (`.beads/`) is the only live-work ledger.
- [`CHANGELOG.md`](../../CHANGELOG.md) owns release history.
- [`1_STRATEGY/TESTING.md`](1_STRATEGY/TESTING.md) owns test policy and current test commands.

Historical launch, rollout, and completed-work narratives remain available in git
history and evidence receipts; repeating them here made current capability lookup
ambiguous and allowed stale status prose to compete with its canonical owners.

## Shipped Feature Status

All shipped capability status lives in the feature tables below. Other documents should link here instead of duplicating feature inventories.

## Search

| Feature | Status | Notes |
|---|---|---|
| Full-text search (BM25 scoring) | ✅ | |
| Typo tolerance | ✅ | strsim, configurable minWordLength |
| Prefix search | ✅ | edge-ngram tokenizer (custom Tantivy fork), queryType: prefixLast/prefixAll/prefixNone |
| Exact phrase / word search | ✅ | `_json_exact` field for non-prefix tokens |
| Faceted search | ✅ | Hierarchical facets, facet counts, facet stats |
| Numeric + string filters | ✅ | Both Algolia syntaxes: `field:value` and `field OP number`, ranges |
| Geo search | ✅ | aroundLatLng, aroundRadius, insideBoundingBox, insidePolygon |
| Synonyms | ✅ | Regular, one-way, and alternative correction mappings |
| Query rules | ✅ | Conditions (query, filters, context) + consequences (pin, hide, filter, boost, redirect, userData) |
| Distinct (deduplication) | ✅ | Variant grouping by attribute |
| Multi-index search | ✅ | Parallel and federated queries across indices in one request (`federation` + weighted merge contract shipped). |
| Highlight / snippet | ✅ | |
| Smart sorting | ✅ | text-first top-100 + filter-only global sort + empty-query objectID lex desc |
| Custom ranking | ✅ | Multiple criteria, asc/desc |
| Optional filters (soft boost) | ✅ | |
| Sum of filters scoring | ✅ | |
| Decompounding | ✅ | Feature-flagged (`decompound`) |
| CJK tokenization | ✅ | |
| Language-specific stemming | ✅ | |

## High Availability

| Feature | Status | Notes |
|---|---|---|
| Dead-node auto-heal | ✅ Bounded / default-off | Opt-in with `FLAPJACK_AUTOHEAL_ENABLED=true`. The engine evicts at most one sustained-unreachable peer after the fixed three-observation threshold only when the local quorum guard remains satisfied, records refusals/evictions/readmissions in `${FLAPJACK_DATA_DIR}/autoheal_decisions.jsonl`, readmits returning healthy candidates with startup catch-up before authoritative reads, and exposes `autoheal_enabled` plus `autoheal_peers` on admin-only `/internal/cluster/status`. See [Dead-node auto-heal](3_IMPLEMENTATION/OPERATIONS.md#scenario-dead-node-auto-heal) and [Replication configuration](3_IMPLEMENTATION/OPS_CONFIGURATION.md#replication). Excludes consensus, majority writes, arbitrary partition healing, simultaneous majority-loss recovery, UI workflow, and CRD/controller lifecycle management. |

## Indexing & Records

| Feature | Status | Notes |
|---|---|---|
| Schemaless JSON upload | ✅ | Dual-field schema (search + filter), nested objects via dot notation |
| `flapjack ingest` beta | ✅ | Streams JSON arrays, NDJSON files, and stdin-backed NDJSON/JSON into the authenticated `/1/indexes/{indexName}/batch` path. Upsert is the only durable mode; explicit `_action:"delete"` records delete by `objectID`. Source-side omissions do not delete target-only records. |
| Atomic bulk-replace job API | ✅ Node-local | Admin-authenticated `POST /1/migrations/bulk-replace?indexName=...` streams NDJSON into the durable migration spool and publishes one replacement generation atomically. Durable status and cooperative cancellation use `/1/migrations/bulk-replace/{jobID}`. Admission returns `503 migration_ha_unsupported` whenever replication peers are configured. |
| Single record CRUD | ✅ | |
| Batch operations | ✅ | Up to 1000 ops, hybrid batching (10 ops or 100ms) |
| Browse (full index scan) | ✅ | Cursor-based pagination |
| deleteByQuery | ✅ | |
| partialUpdateObjects | ✅ | |
| Index copy / move / clear | ✅ | |
| Replicas | ✅ | Virtual + standard replicas |
| Task status API | ✅ | Async task tracking |
| Fail-closed durable acknowledgement | ✅ | A write is never acknowledged before it is recoverable, and a write rejected to the client never becomes visible after restart. The direct oplog-append I/O failure class is proven by `engine/src/index/write_queue_tests.rs::oplog_append_io_failure_before_acknowledgement_is_fail_closed`, which flushes and syncs a partial task-tagged row inside `OpLog::append_operations_with_task_id` *before* `current_seq` advances and then requires one of exactly two honest outcomes: client failure with no replayable state after restart, or an honest durable acknowledgement with the documents present. Compensation stays single-owned by `compensate_failed_commit_batch` → `compensate_uncommitted_tasks` → `OpLog::retract_tasks_from`; no second rollback owner was added (2026-08-02). Receipt: [`4_EVIDENCE/2026_08_02_aug02_11am_2_durable_ack_fail_closed_receipt.md`](4_EVIDENCE/2026_08_02_aug02_11am_2_durable_ack_fail_closed_receipt.md). |

### `flapjack ingest` Beta Bounds

- Inputs: JSON array files, NDJSON files, or `--source -` for stdin. The parser keeps memory bounded by `--batch-size` and reports `queue_high_watermark`.
- Credentials: exactly one of `--api-key-env`, `--api-key-file`, or `--api-key-stdin` is accepted. `--api-key` is intentionally not a CLI option so secrets are not exposed through help text, shell history, or process listings.
- Writes: the CLI sends bounded batch envelopes to the same authenticated batch endpoint used by normal clients. Upsert is default and preserves target-only records. Deletes happen only when a source record carries the configured action field with `delete` or `deleteObject`.
- Retries: one serialized envelope owns one `x-flapjack-idempotency-key` across retry attempts. The beta retries transport failures plus HTTP `429` and `503`, caps `Retry-After`, and reports `retries`, `last_retry_after_ms`, `confirmed_committed`, and `outcome_unknown`.
- Recovery: when the JSON report shows `outcome_unknown > 0`, rerun the same source with the same idempotent object IDs after checking the destination. Do not treat unknown envelopes as confirmed writes.
- Replace mode: `--mode replace` normalizes the source into bounded temporary storage, streams it to the admin-authenticated bulk-replace job API, polls durable status to a terminal disposition, and reports the server-confirmed committed count. It is node-local only; peer-routed and older-server refusals remain typed `replace_not_supported` failures with zero confirmed mutations.
- Bulk replace tuning: staged bulk builds use bulk-only writer and document-checkpoint knobs (`FLAPJACK_BULK_BUILD_WRITER_BUFFER_SIZE`, `FLAPJACK_BULK_BUILD_DOCUMENT_CHECKPOINT_INTERVAL`) without changing the online write queue defaults. Both are frozen at the behavior-preserving baseline (20,000,000-byte writer buffer, 1,000-document checkpoint) with a recorded local-locality gate measurement; raising the bulk-only budget requires a reference-locality (`i4i.4xlarge` NVMe) sweep that is a paid AWS scale run deferred to the named successor "paid reference ladder" batch.

## Index Settings

| Feature | Status | Notes |
|---|---|---|
| searchableAttributes | ✅ | Ordered, with optional unordered flag |
| attributesForFaceting | ✅ | filterOnly, searchable variants |
| ranking (built-in criteria) | ✅ | typo, geo, words, filters, proximity, attribute, exact, custom |
| customRanking | ✅ | |
| attributesToRetrieve | ✅ | |
| attributesToHighlight / Snippet | ✅ | |
| queryType / removeWordsIfNoResults | ✅ | |
| typoTolerance settings | ✅ | |
| minWordSizeFor1/2Typos | ✅ | |
| ignorePlurals / removeStopWords | ✅ | |
| Pagination settings (hitsPerPage, paginationLimitedTo) | ✅ | |
| numericAttributesForFiltering | ✅ | |
| unretrievableAttributes | ✅ | |
| disableTypoToleranceOnAttributes | ✅ | |
| All remaining Algolia settings | ✅ | Full parity per §10 of parity report |

## Analytics & Insights

| Feature | Status | Notes |
|---|---|---|
| Search query logs | ✅ | |
| Analytics API (top queries, no-results, no-clicks) | ✅ | |
| Events / Insights API | ✅ | click, conversion, view events with position tracking. **Authorization enforced since 2026-08-10 (`561cce36b`, closed as `SEC-EVENTS-1` / `PR-17`):** both insights handlers take the key/app-id extensions and authorize every event target through the shared analytics index-access enforcement before recording any item; mixed-index batches fail atomically, and a search-only or index-restricted key posting outside its scope receives `403`. **Bounded ingress closed 2026-08-12 as `SEC-EVENTS-2`:** the configured allowance accepts two requests, the first excess returns exact 429, and the rejected event is absent from debug and analytics. Synchronized executable owners: `engine/tests/test_tenant_isolation.rs` and `engine/tests/events_rate_limit_http_probe.sh`; the whole target is 11 passed, 0 failed. The existing shared limiter was sufficient; no production limiter/code repair was required. Published in v1.0.12. |
| Event Debugger | ✅ | Per-index event stream inspection, tenant-scoped since 2026-08-10 (`561cce36b`): `GET /1/events/debug` requires the `analytics` ACL, reads are filtered to indexes the caller's key may access, and filtered reads cannot expose another tenant's events or `user_token`; a search-only key receives `403`. Same close: `SEC-EVENTS-1` / `PR-17`. |
| A/B Testing (experiments) | ✅ | Traffic split, variant tracking, winner selection. List filtering uses exact `indexName` matching separately from `indexPrefix`/`indexSuffix`; owner: `engine/flapjack-http/src/handlers/experiments/mod.rs::list_experiments`. |
| Usage metering | ✅ | Per-key, per-index operation counts |
| Analytics retention cleanup | ✅ | Partition-based retention cleanup is configurable with `FLAPJACK_ANALYTICS_RETENTION_DAYS`, defaults to 90 days, skips malformed/non-partition paths, and is covered by deterministic cutoff tests. |
| Durable analytics rollup storage | ✅ | Rollup writer + query planner fallback + certified-coverage retention gate are shipped. Proof: `engine/src/analytics/writer.rs` (rollup writer), `engine/src/analytics/query/mod.rs` (rollup planner with raw fallback), `engine/src/analytics/retention.rs` + `engine/src/analytics/manifest.rs` (certified-coverage delete gate), `engine/loadtest/soak_proof.sh` (soak evidence flow). Rollout design and test-citation details are retained in private stage evidence. |

## Personalization & AI

| Feature | Status | Notes |
|---|---|---|
| Personalization API | ✅ | Event scoring, user profile building, personalizationImpact |
| Personalization in search | ✅ | Profile applied at query time |
| Recommendations API | ✅ | `related-products`, `bought-together`, and `trending` ship unconditionally. `looking-similar` works on every published target, using vector similarity when vector search and an embedder are available and content/term similarity otherwise. The shipped fallback needs no model download or new runtime dependency; it replaced the default-feature empty response on 2026-08-04 while preserving legitimate empty vector answers instead of silently changing strategies. |
| AI Search / RAG endpoint | ✅ | Chat-style query with LLM reranking |
| Re-ranking (enableReRanking) | ✅ | |
| Vector search | ✅ | usearch + fastembed, compile-time feature flag with runtime capability detection via `/health`. Dashboard is capability-aware. See [VECTOR_SEARCH_QUICKSTART.md](3_IMPLEMENTATION/VECTOR_SEARCH_QUICKSTART.md) for setup |

## API Keys & Security

| Feature | Status | Notes |
|---|---|---|
| API Keys | ✅ | Create, list, update, delete |
| ACL (Access Control Lists) | ✅ | search, browse, addObject, deleteObject, etc. |
| Key restrictions | ✅ | maxHitsPerQuery, queryParameters, indexRestrictions, referers, description, and `restrictSources` are enforced. |
| Rate limiting per key | ✅ | |
| Security Sources / Vault | ✅ | Secrets injection for external sources |
| Secured API keys (signed) | ✅ | Malformed/non-UTF-8-boundary secured keys are rejected as `400`, not a parser panic (2026-07-31). |
| Route authorization default | ✅ | Fail-closed: a path matching no ACL rule is denied rather than allowed through (`RouteAcl::Unmapped`, 2026-07-31). |
| Insights-route tenant authorization | ✅ | **Fixed 2026-08-10 at `561cce36b`; filed and closed the same day as `SEC-EVENTS-1` / `PR-17`.** The original defect: `extract_index_name` matched only `/1/indexes/<name>/…`, so `/1/events` was never authorized, and both insights handlers took only `State(collector)`, so neither could enforce anything even in principle. The repair follows the exit as named: both handlers take the key/app-id extensions and call the shared analytics index-access enforcement before any side effect, `/1/events/debug` is raised above `search` to the `analytics` ACL with the debug buffer filtered by tenant, and the pinning test `acl_events_search` was re-pointed rather than extended. Current combined-tree regression proof: `(cd engine && timeout 1200 cargo test --no-fail-fast -p flapjack --test test_tenant_isolation)` → 11 passed, 0 failed. |
| Admin credential transport | ✅ | Admin-ACL routes accept the key only in the `x-algolia-api-key` header; the query-string form is refused so admin keys stay out of logs, shell history, and proxy access logs. Search-scoped keys keep query-string support for browser clients (2026-07-31). |
| Analytics client-IP minimization | ✅ | Persisted analytics coarsen the client IP before write (IPv4 → /24, IPv6 → /48); the full address is never stored (2026-07-31). |
| Container runtime posture | ✅ | The image runs as non-root `flapjack:flapjack` at fixed UID/GID `10001:10001`, and refuses to start with an actionable non-zero exit when `/data` is not writable (2026-07-31). |
| Dashboard dependency supply chain | ✅ | CI gates the bundled dashboard on a high-and-above production `npm audit`, with a deliberately-vulnerable fixture proving the gate can fail (2026-07-31). |
| Server-side TLS | ✅ | Static PEM startup plus ACME-backed hot rotation are shipped. Startup fails closed for unreadable, malformed, incomplete, or mismatched material. A valid renewed generation updates the next TLS handshake without rebinding the listener or restarting the process; malformed publication keeps serving the last valid certificate. Plaintext HTTP-01 challenges remain reachable while other plaintext API requests stay rejected. Receipts: `4_EVIDENCE/2026_08_03_aug03_11am_3_acme_material_lifecycle_receipt.md` and `2026_08_03_aug03_11am_7_tls_hot_reload_receipt.md`. |
| Security audit event coverage | ✅ | Eleven audited actions — `authenticate`, `create_key`, `update_key`, `delete_key`, `restore_key`, `generate_secured_key`, `delete_index`, `set_settings`, `import_snapshot`, `restore_snapshot_from_s3`, `rotate_admin_key` — over two outcomes (`success`, `failure`), each carrying actor / action / target / outcome. Targets are mapped through a bounded route-template vocabulary, so no key material, header value, or query payload reaches an event. Emission is consolidated in one owner, `engine/flapjack-http/src/security_audit.rs` (2026-08-01). Per `SD-006` the engine emits a structured stream and does **not** own durable retention — this does not close fjcloud's audit-trail control. |
| Snapshot server-side encryption (S3) | ✅ | S3 snapshot uploads set server-side encryption rather than relying on bucket defaults: `AES256` when `FLAPJACK_S3_SSE` is unset, or `aws:kms` with an optional `FLAPJACK_S3_SSE_KMS_KEY_ID`; any other value is a startup-time error. The response SSE header is verified rather than assumed. Source: `engine/src/index/s3.rs`; probe: `engine/tests/s3_sse_http_probe.sh` (2026-08-01). |
| Snapshot at-rest encryption (local export) | ✅ | `export_to_bytes` / `import_from_bytes` support optional AES-256-GCM-SIV encryption through `FLAPJACK_SNAPSHOT_KEY_FILE`; the pinned four-case producer/consumer symmetry probe passed. The unused plaintext `export_to_tarball` / `import_from_tarball` helpers were deleted in `98b4790838dc9c090a0dd3cc9b054a858bef3ffc`, closing `SEC-G5`; the canonical security control register owns the terminal disposition. |
| S3 failure propagation | ✅ | Upload, delete, **and list** reject non-success HTTP responses, and retention call sites propagate or log those failures. `list_snapshots` checks the ListObjectsV2 status before parsing the body and returns `S3("S3 list: HTTP <status>")` instead of a downstream XML parse error (2026-08-02). All three focused regressions exist — `upload_snapshot_fails_loudly_when_bucket_rejects_the_put`, `delete_snapshot_fails_loudly_when_bucket_rejects_delete`, `list_snapshots_fails_loudly_when_bucket_rejects_list` — and `cargo test -p flapjack --lib -- index::s3::tests` reported `14 passed`. Closes `ROADMAP.md` row `DUR-2`. Source: `engine/src/index/s3.rs`; receipt: [`4_EVIDENCE/2026_08_02_aug02_11am_3_s3_list_failure_propagation_receipt.md`](4_EVIDENCE/2026_08_02_aug02_11am_3_s3_list_failure_propagation_receipt.md). |
| Replication peer authentication | ⚠️ | A distinct configured peer credential serves replication and is provably refused on `add_cluster_peer`, `remove_cluster_peer`, and `rotate_admin_key` by `engine/tests/replication_peer_auth_http_probe.sh`; receipt: the reviewed private replication peer auth receipt. **Both `SEC-G9` residuals closed 2026-08-02.** The credential is no longer optional: `startup.rs::validate_replication_peer_credential` refuses to start a node that configures replication peers without `FLAPJACK_REPLICATION_API_KEY`. Cleartext peer transport is refused by default: `flapjack-replication/src/config.rs::NodeConfig::validate_credentialed_peer_transport` rejects an `http://` peer origin that would carry a credential — across static, persisted, bootstrap, and runtime `POST /internal/cluster/peers` paths — unless `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` is set explicitly. Background rollup fan-out (`analytics_cluster.rs::push_rollup_to_peers`) now authenticates with the peer credential instead of running unauthenticated. Receipts: [`4_EVIDENCE/2026_08_02_aug02_11am_4_replication_peer_identity_receipt.md`](4_EVIDENCE/2026_08_02_aug02_11am_4_replication_peer_identity_receipt.md), [`4_EVIDENCE/2026_08_03_aug03_5am_0_runtime_peer_transport_regression_receipt.md`](4_EVIDENCE/2026_08_03_aug03_5am_0_runtime_peer_transport_regression_receipt.md). Operator upgrade path: [`3_IMPLEMENTATION/OPERATIONS.md`](3_IMPLEMENTATION/OPERATIONS.md) rolling-upgrade runbook. |
| Dashboard session credential storage | ✅ | **Shipped 2026-08-03, closing `ROADMAP.md` row `SEC-G3`.** The console no longer keeps admin credentials where a same-origin script can read them. It exchanges the key once at `POST /1/dashboard/session` for a server-owned `HttpOnly; SameSite=Strict; Path=/` cookie (`Secure` when served over TLS); `DELETE /1/dashboard/session` revokes server-side. `engine/dashboard/src/hooks/useAuth.ts` persists **only** `appId` through its zustand `partialize`, with a `migrate` that drops legacy persisted key material on upgrade, so an authenticated reload survives without the key ever returning to browser storage. The durable store is `engine/flapjack-http/src/auth/session.rs`: it mints, validates, revokes, and survives restart, persisting only a keyed fingerprint plus salted HMAC-SHA256 verifiers in a `0o600` `dashboard_sessions.json`, and the plaintext token and the admin key each appear **zero** times in the persisted bytes (17/17 focused tests green). Header-key auth is unchanged for SDKs, InstantSearch, and HTTP probes — only the browser console switched. **Fail-capability is proven at both tiers, not asserted:** with `HttpOnly` deliberately removed, `auth::tests::session_transport_tests` went `4 passed / 4 failed` and the browser probe failed at `session_auth.spec.ts:44`; restored, engine `8/8` and browser `3/3`. User contract: [`docs/screen_specs/login.md`](../../docs/screen_specs/login.md). Receipts: [`4_EVIDENCE/2026_08_03_aug03_5am_1_dashboard_session_store_foundation_receipt.md`](4_EVIDENCE/2026_08_03_aug03_5am_1_dashboard_session_store_foundation_receipt.md), [`4_EVIDENCE/2026_08_02_aug02_11am_8_dashboard_session_auth_receipt.md`](4_EVIDENCE/2026_08_02_aug02_11am_8_dashboard_session_auth_receipt.md). The private SD-009 security decision is superseded by this work. |

## Dictionaries

| Feature | Status | Notes |
|---|---|---|
| Stop words dictionary | ✅ | Per-language |
| Plurals dictionary | ✅ | |
| Compounds dictionary | ✅ | |
| Custom entries | ✅ | |

## Infrastructure

| Feature | Status | Notes |
|---|---|---|
| Multi-tenant isolation | ✅ | Per-tenant memory limits (31 MB buffer, 40 concurrent writers) |
| Oplog replication + startup catch-up | ✅ | Peer oplog replication with pre-serve catch-up (`run_pre_serve_catchup`) |
| S3 snapshots | ✅ | Single-node snapshot APIs ship with scheduled backups and empty-dir auto-restore, verified by the MinIO harness in `engine/examples/s3-snapshot/`. Non-success list-response fidelity closed 2026-08-02 under `ROADMAP.md` row `DUR-2`: upload, delete, and list all reject non-success HTTP responses, so the capability symbol now does claim complete failure propagation. Detail owner: the **S3 failure propagation** row in the Security section above. |
| Published operations APIs | ✅ | Engine-owned consumer contract is published in [`operations_consumer_contract.md`](operations_consumer_contract.md) for `/health`, `/internal/status`, `/internal/cluster/status`, and `/internal/snapshots/capability`. Snapshot capability reports `not_configured` or `configured_unverified`; `configured_unverified` means config exists, not that credentials, bucket existence, or reachability were verified. |
| SSL / TLS | ✅ | In-binary TLS covers operator-supplied PEM startup and ACME-backed material rotation. ACME issuance persists the private key, atomically publishes fullchain/key pairs under the managed material directory, and hot-reloads changed generations in the running rustls listener without a process restart. Plaintext HTTP-01 challenge requests remain reachable on the TLS listener while other plaintext API requests remain rejected. Verified by the Pebble DNS/IP known-answer and served-rotation receipts. |
| OpenAPI spec | ✅ | Auto-generated via utoipa; includes recommend, personalization, and experiments routes with coverage in both `openapi_export_tests` and `openapi::tests`. |
| Memory safety | ✅ | OOM-proof: BufferSizeExceeded → 429, DocumentTooLarge → drop |
| Health endpoint | ✅ | Liveness endpoint (`/health`). |
| Readiness probe (`/health/ready`) | ✅ | Operational readiness probe: returns `{"ready":true}` (200) when no visible tenant directories exist or the first tenant probes successfully; returns canonical 503 when tenant discovery or probing fails. `_`-prefixed and `.`-prefixed directories (e.g. `_usage/`, `analytics/`) are excluded from tenant probing. Source: `engine/flapjack-http/src/handlers/readiness.rs`, `engine/flapjack-http/src/tenant_dirs.rs`. |
| Request latency histograms | ✅ | `request_duration_seconds` Prometheus histogram labeled by bounded `method` + normalized `route` + `status_class`, collected by global middleware and appended to `/metrics`. Source: `engine/flapjack-http/src/latency_middleware.rs`, `engine/flapjack-http/src/handlers/metrics.rs`. |
| Error response parity | ✅ | HTTP status codes match Algolia exactly |

## Operational / Observability

Env-var details for operational behavior are canonical in
[`3_IMPLEMENTATION/OPS_CONFIGURATION.md`](3_IMPLEMENTATION/OPS_CONFIGURATION.md).

| Feature | Status | Notes |
|---|---|---|
| Request ID propagation (Stage 1) | ✅ | Every response includes `x-request-id`, and the same value is attached to the active request span in middleware. Always on (no feature flag/env var). |
| JSON structured logging (Stage 2) | ✅ | Controlled by `FLAPJACK_LOG_FORMAT=json` (`text` default). |
| Configurable CORS origins (Stage 4) | ✅ | `FLAPJACK_ALLOWED_ORIGINS` controls restrictive allowlists; empty/unset defaults to loopback-only browser access. |
| Graceful shutdown timeout (Stage 5) | ✅ | `FLAPJACK_SHUTDOWN_TIMEOUT_SECS` controls write-queue drain deadline before forced-exit warning. |
| Startup dependency summary (Stage 6) | ✅ | Emits a structured `[startup] Dependency status summary` event in both text and JSON logging modes. |

## Client compatibility

Flapjack supports the official Algolia clients only. The Flapjack-specific SDKs were retired from this repository in 2026-09; sources remain in git history. Do not install the retired packages. Historical releases sent Flapjack API keys to Algolia hosts: PyPI `flapjack-search` (`GHSA-jhcc-64c6-pfq2`), RubyGems `flapjack-search` (`GHSA-q67x-w5fw-5mw2`), and Go v4.0.0 (`GHSA-jc2w-7wq6-r5w7`). The historical receipt records the Go v4.0.1 correction; it does not establish completion of the retirement operations. Registry deprecation and removal actions and the public history tag `sdks-retired-2026-09` remain pending operator work. Historical remediation receipt: [`4_EVIDENCE/2026_08_08_aug08_9pm_2_sdk_receipt.md`](4_EVIDENCE/2026_08_08_aug08_9pm_2_sdk_receipt.md).

| Client | Status | Verification |
|---|---|---|
| JavaScript / TypeScript (algoliasearch v5) | ✅ | 32 contract + 13 full-compat tests |
| SDK contract CI gate | Configured | `sdk-contract` retains official JS/browser coverage and adds Python 3.12 setup, `python_client_contract_test.sh`, runner/bootstrap regressions, and Python units. The local `./s/test --sdk`, `--e2e`, `--all`, and `--sdk --e2e` modes run official Python once. This wiring does not claim landed public-main acceptance. |
| PHP | Protocol smoke only | curl protocol smoke (`engine/sdk_test/php_smoke_test.sh`) |
| Python | Bounded official-client proof + protocol smoke | `engine/sdk_test/python_client_contract_test.sh` runs the synchronous official client on CPython 3.12: settings, batch, task waits, exact shared-fixture search/facet results, and deletion/closure. `requirements-python-client.txt` owns the test-only pin. The separate curl smoke (`python_smoke_test.sh`) remains in local `--sdk`. |
| Ruby | Protocol smoke only | curl protocol smoke (`engine/sdk_test/ruby_smoke_test.sh`) |
| Go | Protocol smoke only | curl protocol smoke (`engine/sdk_test/go_smoke_test.sh`) |
| Java | Protocol smoke only | curl protocol smoke (`engine/sdk_test/java_smoke_test.sh`) |
| Swift | Protocol smoke only | curl protocol smoke (`engine/sdk_test/swift_smoke_test.sh`) |
| InstantSearch.js 4.111.0 | ✅ | Official package rendered in Chromium through `algoliasearch/lite` and an index-scoped search key; distinct exact query, facet, and pagination results |
| React InstantSearch 7.44.0 | ✅ | Official React 18 package rendered in Chromium with the same scoped-key proof |
| Vue InstantSearch 4.29.2 | ✅ | Official Vue 3 package rendered in Chromium with the same scoped-key proof |
| Angular InstantSearch | ⚠️ | No rendered-client proof |
| InstantSearch Android | ⚠️ | Java curl protocol smoke (`engine/sdk_test/java_smoke_test.sh`); no rendered-client proof |
| InstantSearch iOS | ⚠️ | Swift curl protocol smoke (`engine/sdk_test/swift_smoke_test.sh`); no rendered-client proof |
| Autocomplete.js | ⚠️ | Multi-index API contract only; no rendered-client proof |

The recurring real-client owner is
`engine/sdk_test/browser_tests_unmocked/real_client_conformance.spec.mjs`; evidence and
scope limits are recorded in
[`4_EVIDENCE/2026_08_12_real_instantsearch_clients_receipt.md`](4_EVIDENCE/2026_08_12_real_instantsearch_clients_receipt.md).

## Source migration — PROVIDER-NEUTRAL CORE + ALGOLIA RESUME SHIPPED

**Current capability:** node-local discovery, preview, and authenticated async import support Algolia, Meilisearch, and Typesense. Interrupted-job resume remains Algolia-only, and HA import is refused because staged publication is node-local and has no convergence epoch.

**Operator CLI:** `flapjack migrate` uses one provider-neutral internal adapter/capture seam and one shared submit/status/cancel/acknowledge lifecycle across the public Algolia, Meilisearch, and Typesense route families. Resume remains Algolia-only. The served provider-parity probe proves local landed-data fidelity through real digest-pinned Meilisearch and Typesense containers: Meilisearch `configured_pk` lands two searchable documents with exact `sku`-to-`objectID` projections, and Typesense categories/products land one category plus two products with exact `id`-to-`objectID` projections. Receipt: `engine/docs2/4_EVIDENCE/2026_08_03_aug03_11am_5_competitor_migration_lands_data_receipt.md`; landed merge: `2c05776c7b9d8f60bae89c34ad819ece084fa2e4`. See the [`flapjack migrate` operator configuration](3_IMPLEMENTATION/OPS_CONFIGURATION.md#flapjack-migrate) for provider connections, secret sources, output, and exit behavior.

| Leg | Status | Owner |
|---|---|---|
| Source index discovery (provider-neutral) | ✅ Shipped 2026-08-03 | `POST /1/migrations/{provider}/list-indexes` is mounted and published in OpenAPI for all three public providers (`algolia`, `meilisearch`, `typesense`) by `engine/flapjack-http/src/router.rs::register_source_migration_routes` and `handlers/migration/mod.rs::define_source_migration_openapi_lifecycle!`, returning the shared `ListSourceIndexesResponse` / `SourceIndexSummary` bundle. No parallel client or response type was introduced. Receipt: [`4_EVIDENCE/2026_08_02_aug02_5am_4_neutral_source_discovery_receipt.md`](4_EVIDENCE/2026_08_02_aug02_5am_4_neutral_source_discovery_receipt.md) |
| Source preview (provider-neutral) | ✅ Shipped 2026-08-03 | `POST /1/migrations/{provider}/preview` is mounted and published in OpenAPI for all three public providers by the same router and lifecycle-macro owners. Typesense has a served `200` proof with its provider-specific request schema and settings translation report, recorded in a reviewed private Typesense preview-and-translation receipt. |
| Meilisearch source adapter | ✅ Shipped | `engine/flapjack-http/src/handlers/migration/{meilisearch_client,meilisearch_source_reader,meilisearch_settings}.rs`; shared lifecycle owner `handlers/migration/mod.rs::define_source_migration_openapi_lifecycle!` |
| Typesense source adapter | ✅ Shipped | `engine/flapjack-http/src/handlers/migration/{typesense_client,typesense_source_reader,typesense_settings}.rs`; reviewed private M2ET adapter receipt |
| Typesense export stream traversal | ✅ Shipped 2026-08-11 | `TYPESENSE_EXPORT_STREAM_CONTRACT` in `engine/tests/typesense_migration_contract.sh` proves a 137-document export traverses the complete stream with exact IDs, one export request, no query pagination, and no discovery export request. |
| Typesense write-freeze admission | ✅ Shipped 2026-08-12 | `TYPESENSE_WRITE_FREEZE_CONTRACT` in `engine/tests/typesense_migration_contract.sh` proves preview and submit refuse missing or false `sourceWriteFrozen` before source traffic, accept the explicit attestation, and keep Typesense resume unsupported without source requests. |
| Typesense dashboard-to-source join | ✅ Browser-proven 2026-08-12 | `migrate-typesense.spec.ts` uses the real `e2e-ui` browser/server project and a real pinned Typesense source, asserts the attestation starts unchecked with preview and submit disabled, then drives discovery → preview → submit → terminal success → Browse and verifies the seeded `prod_1` Espresso record. This is path-specific joined proof; it does not create or revive a portfolio-wide `JOIN-1` numerator. |
| Source export: Algolia → durable on-disk spool (checkpointed, resumable) | ✅ Shipped | `engine/flapjack-http/src/handlers/migration/{algolia_client,source_reader,export,spool}.rs` |
| Translation: spool → Flapjack documents/settings/synonyms/rules | ✅ Shipped | `engine/flapjack-http/src/handlers/migration/translation.rs` |
| Import: translated content → target index via staged publication | ✅ Shipped for create-only plus synchronous and async overwrite | `engine/flapjack-http/src/handlers/migration/import.rs`; `engine/flapjack-http/src/handlers/migration/mod.rs` |
| Staged publication primitive (crash-safe, node-local) | ✅ Shipped | `engine/src/index/manager/publication.rs` |
| Interrupted-job resume (pre-publication export) | ✅ Shipped — Algolia only | `POST /1/migrations/{provider}/{job_id}/resume`; `engine/flapjack-http/src/handlers/migration/{spool_lifecycle,export,job_runner,mod}.rs`; restart proof `engine/flapjack-server/tests/crash_durability_test.rs::interrupted_async_migration_resumes_exactly_once_after_process_restart` |
| Dashboard `Migrate` page | ✅ All three providers reachable, dry-run before any write, and **all three browser-proven** as of 2026-08-07 (`86b143724`) — 21 passed targeted, 411 passed / 5 skipped full `e2e-ui`, and `migrate-{meilisearch,typesense}.spec.ts` executed on Linux CI in staging nightly `31176417863`. Synchronous create-only mutation; no console job-status or resume surface. **Published in v1.0.12:** the installable dashboard reaches all three providers and exposes the dry-run. | `engine/dashboard/src/pages/{Migrate.tsx,MigrateSections.tsx,migrateHelpers.ts}`; specs `engine/dashboard/tests/e2e-ui/full/migrate-{algolia,meilisearch,typesense}.spec.ts`. Re-measure rather than cite: `cd engine/dashboard && npm run test:e2e-ui`. Screen contract: the private migrate screen contract. |
| **Backend ↔ frontend joined end-to-end** | No portfolio-wide score is claimed for the retiring React dashboard. Provider-specific browser proofs are listed above, while the validated manifest remains the console port map. | Manifest owner: `engine/dashboard/tests/e2e-ui/join_proof_manifest.json`; validator: `engine/dashboard/scripts/check_join_manifest.mjs`. The console replacement policy is owned by [`PROJECT_OVERVIEW.md`](../../PROJECT_OVERVIEW.md). |

Replica translation detects topology from the source primary, fetches every named replica's own settings, and carries the derived virtual topology plus translated per-replica settings in the create-only migration bundle. Materialization then creates each derived replica as a settings-only virtual sidecar (no physical copy, by design) whose sort order resolves at query time. This contract is live-proven: on 2026-07-19 a real Algolia application with one `virtual(...)` relevance replica and one standard replica migrated end-to-end with a passing machine-verified receipt (jul18_11am batch) covering fixture seeding, import, sort-order proofs on the primary and both replica indexes, sidecar structure, and exact source cleanup. The remaining fidelity limits surface as documented migration warnings: standard-replica exhaustive sorting is approximated as a virtual replica, and Algolia `relevancyStrictness` semantics differ from Flapjack's deterministic ranking.

Migration warnings expose the remaining replica fidelity limits:

- Algolia standard-replica exhaustive sorting is approximated by blended Flapjack virtual ranking.
- `asc()` and `desc()` tokens in replica `ranking` are lifted ahead of replica `customRanking`; unknown ranking tokens are ignored with warnings.
- Matching-critical fields that diverge from the primary cannot be reproduced independently by a virtual replica.
- Algolia and Flapjack use different `relevancyStrictness` scales, and `nbSortedHits` may differ for deterministic queries.

## Dashboard UI

`dashboard/src/App.tsx` defines 24 derived user-facing route patterns from 24 raw `path=` attributes and two attribute-less index routes, backed by 22 lazy page components. No stub pages remain.
The route inventory spans overview, search/browse, settings, analytics, relevancy controls, security tooling, and migration workflows with no placeholder pages.

Playwright webserver startup now reclaims stale startup leases before acquiring the shared server slot. The repair shipped at `89df8543d`; `engine/dashboard/scripts/playwright-webserver.mjs::acquireStartupLease` is the behavior owner.

**Caveat:** a shipped route does not imply portfolio-wide backend-to-frontend proof. The provider-specific proofs above and the validated port manifest are the bounded claims; no aggregate score is claimed for the React dashboard scheduled for replacement.

| Status | Features |
|---|---|
| ✅ Built | Overview, Search & Browse (including Hybrid Search mode), Settings (all tabs, including Vector Search settings), Analytics (7 tabs), Synonyms, Rules, Merchandising Studio, API Keys (with `restrictSources`), Search Logs, Query Suggestions, Personalization, Recommendations, Experiments, Event Debugger, Metrics, System, Migrate, Dictionaries, Security Sources, Chat/RAG |
