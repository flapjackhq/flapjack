# Flapjack Product Status & Roadmap

Single maintained status ledger for Flapjack. Shipped feature status, current production-readiness state, and post-launch work are owned in this document.

**Last updated: 2026-06-05 (v1.0.8 current-release truth-up; keep lane-state routing in `ROADMAP.md`, with detailed measured evidence retained in private stage artifacts.)**

- 2026-05-31 stage note: `FLAPJACK_WRITE_QUEUE_BATCH_SIZE` is now runtime-configurable with default-preserving behavior (`32` fallback). See [`3_IMPLEMENTATION/OPS_CONFIGURATION.md`](3_IMPLEMENTATION/OPS_CONFIGURATION.md) for full operator semantics.

- **Backend API:** 197/197 complete (as of 2026-03-13). The full parity verification is retained in the dev repo's internal audit history.
- **Dashboard UI:** 22 user-facing routes are shipped, backed by 21 lazy-loaded page components in `dashboard/src/App.tsx`, plus the `*` not-found catch-all. No scaffolded stubs remain.
- **E2E Browser Tests:** 340+ tests across 46 Playwright spec files in total: 42 browser specs (41 specs in `tests/e2e-ui/` [4 smoke + 37 full] plus root-level `tests/result-helpers.spec.ts`) and 4 API-contract specs in `tests/e2e-api/`. Exact-HEAD wrapper verification passed on 2026-03-26 at commit `aa7dd7db61d7e274cdf946ac6dd7d7435c4dcdf4`, with all 14 wrapper sections green.
- **Tour Video Walkthroughs:** 24/24 per-feature specs now have archived MP4 artifacts. The former vector/chat blockers (05/06) were closed on 2026-03-30 with dedicated tour specs plus default-build/vector+AI runtime wiring. Per-feature tours provide end-to-end workflow proof for core dashboard capabilities.
- **Load & Stress Testing:** k6 suite in `engine/loadtest/` — smoke, search throughput, write throughput, mixed workload, spike, memory-pressure, plus the long-running `mixed-soak` / `write-soak` scenarios and `soak_proof.sh` restart harness. PL-10's post-fix 60-minute Stage 3 mixed-soak gate (run date 2026-05-27) is classified `failure`, while the public write contract remained intact (no write `5xx`, no unexpected write `4xx`). Keep detailed lane status in [`ROADMAP.md`](../../ROADMAP.md), with measured verdict/evidence paths retained in private stage artifacts. Large-dataset benchmarking (100k docs): deterministic generator (`generate_dataset.mjs`), import throughput (`import_benchmark.sh`), search latency by query type (`search_benchmark.sh`), k6 concurrent load (`benchmark_k6.sh`), and dashboard large-index perf test (`large-index-perf.spec.ts`).
- **Architecture decisions:** `3_IMPLEMENTATION/decisions/active/`

## Public Sync Lineage Ledger (Canonical)

This is the detailed public-sync lineage ledger. Current release truth is owned by [`PRIORITIES.md`](../../PRIORITIES.md) and [`CHANGELOG.md`](../../CHANGELOG.md); as of the 2026-06-05 doc truth-up, v1.0.8 released 2026-06-05 is the latest paid-beta ship in the v1.0 line.

May 22 OSS polish wave status facts (lanes A-F) and the v1.0.3 public-beta cut remain canonical historical lineage in the existing rows below; they are preserved as lineage, not as the current release baseline.

| Proof Item | Value |
|---|---|
| Canonical dev source for historical v1.0.3 wave | Commit `1111b` (v1.0.3 release cut lane) plus merged PL-14 fix surface `dbd78016` |
| Staging publication | `ci.yml` green at HEAD for the v1.0.3 wave (see current production lane-state owner in `ROADMAP.md`) |
| Prod publication | `ci.yml` green at HEAD for the v1.0.3 wave with release artifacts published on 2026-05-30 |
| Release closeout channels | `github_release`, `binaries`, `ghcr_VERSION`, `ghcr_latest`, `mirror_ci`, `nightlies` |
| Preserved cleanup lineage in prod history | Historical mar31/apr08/apr15 publication lineage remains preserved below; v1.0.3 is an additive release wave, not a history rewrite |
| Non-pruned stale-file removals in public clones | No new stale-file removal axis introduced in v1.0.3; prior mar31/apr08/apr15 cleanup lineage remains authoritative |
| Tracked-file audit outcome | v1.0.3 closure retained public-sync boundaries; no new blocker leaks surfaced in the published release wave |
| Validator boundary contract | Public status/readiness detail ownership remains split: `FEATURES.md` for canonical snapshot, `PRIORITIES.md` + `ROADMAP.md` for live lane-state |
| Local public validation before push | v1.0.3 release closure validated release assets/channels (`gh release`, GHCR multi-arch, nightly/staging/prod CI green lineage) per `PRIORITIES.md` |
| Known residual (out of scope for this wave) | v1.0.3 intentionally excludes post-tag `[Unreleased]` CLI flag/dispatch fixes that ship in the next release |

---

## Griddle Launch Status (as of 2026-03-24)

The original internal launch checklist is retained in the dev repo; the public outcome is summarized below.

| Checklist Item | Status | Notes |
|---|---|---|
| GL-1 Replication peer auth | ✅ Done | `PeerClient` sends `x-algolia-api-key` + `x-algolia-application-id`; `/internal/*` remains admin-gated. |
| GL-2 Replication catch-up on startup | ✅ Done | `startup_catchup.rs` — fetches missed ops from primary on boot before serving. |
| GL-3 `restrictSources` enforcement | ✅ Done | CIDR/IP allow-list on API keys, fail-closed. Merged. |
| GL-4 Metering agent integration | ✅ Done | `/internal/storage` and `/metrics` both require admin-key auth. `POST /internal/rotate-admin-key` supports runtime key rotation without restart. |
| GL-5 Dictionaries multi-tenant fix | ✅ Done | Per-tenant stop words/plurals/compounds wired in backend and shipped in dashboard UI. |
| GL-6 Dashboard feature completeness | ✅ Done | Dashboard route inventory shipped: 22 user-facing routes backed by 21 lazy-loaded page components, plus the not-found catch-all. Tour video acceptance suite now covers all 24 per-feature specs (01-24) with archived MP4 artifacts. Legacy root walkthrough covers all 21 pages at smoke level. |
| GL-7 Griddle integration docs | ✅ Done | Canonical integration docs are maintained in `../../README.md` (quickstart + API flow), `3_IMPLEMENTATION/DEPLOYMENT.md` (deployment paths), and `../examples/ha-cluster/README.md` (HA proof). |
| GL-8 Engine polish | ✅ Done | Stage 2 and Stage 3 follow-ups remain implemented: recommend env-var handling, virtual-replica validation/enforcement, and auth `restrictSources`/ACL hardening with tests. |
| GL-9 OpenAPI spec completeness | ✅ Done | Stage 4/5 annotation work verified via both `openapi_export_tests` and `openapi::tests` for recommend/personalization/experiments. |

### What "done" requires before CEO sign-off

1. ~~**GL-4** lands~~ — ✅ Done (admin key rotation live, metering endpoints auth-gated)
2. ~~**Chat/RAG dashboard page** — only remaining UI stub (backend ✅, UI shell still scaffolded)~~ — ✅ Done (`/index/:name/chat` shipped)
3. ~~**Dashboard full E2E-UI stability** (per `BROWSER_TESTING_STANDARDS_2.md`) — latest standalone full run baseline is 318/320 passing; fix the two failing Overview analytics specs.~~ — ✅ Done (latest clean-head standalone browser full proof passed 320/320 at `10cc160`)
4. `./s/test --all` green (~20 min full suite) after the dashboard full-suite stability pass. — ✅ Done: exact-HEAD wrapper verification passed on 2026-03-26 at commit `aa7dd7db61d7e274cdf946ac6dd7d7435c4dcdf4`, with all 14 sections green.

## Open-Source Launch Readiness (as of 2026-04-01)

| Item | Status | Session | Notes |
|------|--------|---------|-------|
| Post-merge regression validation | ✅ Done | mar22_1 | Full suite green; coverage verified |
| End-to-end API smoke test (`integration_smoke.sh`) | ✅ Done | mar22_pm_3 | 513-line test covering 13 API categories |
| HA + Docker deployment verified | ✅ Done | mar22_3 | Single-node, HA cluster, replication, S3 snapshot all tested |
| Docs accuracy audit | ✅ Done | mar22_pm_2 | Full mechanical audit, dead links removed |
| Performance benchmarks published | ✅ Done | mar22_2 | k6 baselines published in [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md). |
| `query-suggestions.spec.ts` full-suite status | ✅ Exact-HEAD wrapper green | mar23_pm_1 + mar24_pm_1 + mar25_pm_12 | Exact-HEAD wrapper verification passed at commit `aa7dd7db`, with all 14 wrapper sections green. |
| CI org names fixed + smoke wired into CI | ✅ Done | mar23_pm_2 | Legacy org tokens → correct org names; `sleep 3` → `wait_for_flapjack.sh`; `integration-smoke` job added |
| Systemd VPS deployment path | ✅ Done | mar23_pm_3 + mar23_pm_5 + stage_03 + stage_04 | Templates/docs landed in mar23; live VPS end-to-end verification completed on 2026-03-26. The validated contract included Linux ELF install, `EnvironmentFile=/etc/flapjack/env`, `systemctl enable --now`, public `/health` + `/health/ready`, manual restart, and SIGKILL recovery. |
| README quickstart accuracy | ✅ Done | mar23_pm_3 | `/health/ready` docs fixed; quickstart curl commands updated with task-wait loop |
| README screenshots (dash_overview, dash_search, dash_migrate_alg) | ✅ Done | mar23_pm_4 + mar24_pm_1 | Automation landed in mar23_pm_4; refreshed tracked PNGs were merged on 2026-03-25 alongside the screenshot-gated test flow. |
| Repo URL + path hygiene | ✅ Done | mar24_pm_2 | README/show-HN/tour/deployment docs now point to the public repo or `<repo-root>` placeholders where appropriate, and deploy/sync helper scripts now resolve `origin` or repo root instead of hardcoded local paths. |
| Test stability fixes (Recommendations + analytics-deep) | ✅ Done | mar23_pm_6 | Recommendations.test.tsx network-noise isolation; analytics-deep.spec.ts flexible assertion replacing California hardcode; bundler dedup verification script |
| Post-merge regression validation (pm_1) | ✅ Done | mar26_pm_1 | Full suite green after am_1+am_2 merge. Green wrapper proof at `aa7dd7db`. |
| Debbie sync config hardening | ✅ Done | mar26_pm_2 | Blacklist → whitelist `.debbie.toml`. Prevents leaking 60+ internal files to public repos. |
| README & Show HN polish | ✅ Done | mar26_pm_3 | Stale claims fixed, Docker quickstart added, engine/README public-ready, FEATURES.md counts corrected. |
| Staging push + CI gate-closing | ✅ Done | mar27_noon + mar27_pm | Fixed debbie whitelist gaps, post-sync hook pattern, staging CI across 6+ rounds. Gate-closing staging run `23671792399` on commit `745a059` completed `success`. |
| Algolia compat hardening + deterministic parity | ✅ Done | mar27_night + mar27_master | Mutation parity matrix, runtime/OpenAPI/artifact coupling tests, staged mirror guards, dashboard readiness contracts, SDK/HTTP contract reinforcement. Canonical matrix in `engine/flapjack-http/src/mutation_parity.rs`. |
| Launch docs truth-sync + proof pack | ✅ Done | mar27_pm | PRIORITIES.md, ROADMAP.md, HIGHEST_PRIORITY.md all reconciled with live launch state. Launch proof pack created. Public surface validated (README smoke 6/6, doc-link validation, live URL checks). |
| Confidence completeness: Stage 3 soak/failure | ✅ Done | mar28_stage3_6 | 2h mixed/write soak artifacts, restart-during-active-writes proof, nontrivial crash/restart recovery proof. Bounded latency, zero 5xx, exact post-restart count preservation. |
| Confidence completeness: Stage 4-6 ops/security | ✅ Done | mar28_stage3_6 | Upgrade smoke test, canonical OPERATIONS.md runbooks, SECURITY_BASELINE.md hardening doc, security proof surfaces green. |
| OSS policy docs + version 1.0.0 | ✅ Done | mar28_pm_1 | SECURITY.md, CHANGELOG.md, CONTRIBUTING.md created and added to debbie sync whitelist. All workspace crates bumped to 1.0.0. Version consistency test added. Dev release script `lib/version.sh` helper created. |
| OpenTelemetry distributed tracing | ✅ Done | mar28_pm_2 | Feature-gated OTEL OTLP gRPC export (`--features otel`). `otel.rs` module with `try_init_otel_layer()`, wired into subscriber and graceful shutdown. Zero overhead when disabled. |
| TODO stub cleanup + HA soak hardening | ✅ Done | mar28_pm_3 | Replaced ~601 auto-generated `TODO: Document` stubs with real doc comments across all crates. Added a dev-repo HA soak test harness. Doc-regression tests for server/startup. |
| Codebase quality cleanup (Round 2) | ✅ Done | mar29 | Fixed 15 error-leaking 500 sites across settings/snapshot/query_suggestions (settings+rules+synonyms+query_suggestions migrated to `HandlerError`; snapshot's local helper sanitized). Removed `cognitive_complexity` suppressions in `startup_catchup.rs`/`server.rs`. Decomposed `execute_search_query` (CC=26 → 8 extracted helpers, orchestrator now ~130 lines). Updated `engine/CLAUDE.md` with HandlerError and suppression guidance. |
| HA multi-node soak harness + CI integration | ✅ Done | mar29_pm_1 | Delivered a dev-repo HA soak harness, Rust integration coverage (`engine/tests/test_ha_soak_harness.rs`), and topology/soak shell acceptance tests (`engine/loadtest/tests/ha_topology_acceptance.sh`, `engine/loadtest/tests/ha_soak_acceptance.sh`). |
| File size guardrail enforcement | ✅ Done | mar29_pm_2 | Extracted 13 inline test modules (>500 test lines each) to standalone `*_tests.rs` files. Split 2 production files (`search_helpers.rs`, `promote.rs`). All files now under 800-line guardrail. Pre-commit hook installed via `engine/scripts/install-pre-commit-hook.sh`. |
| Debbie sync pipeline (wave 2) | ✅ Done | mar30_pm_1 | Full debbie sync pipeline to staging and prod repos. OpenAPI test dedup and helper extraction (`openapi_test_helpers.rs`). Experiment handler refactoring (extracted `require_experiment_store`, `resolve_store_and_experiment_id`, `should_promote_variant_settings` helpers). Soak proof consistency harness improvements. Fixed debbie sync excludes for HA soak harness test and SDK lock files. |
| Cognitive complexity reduction | ✅ Done | mar30_pm_2 | Decomposed 5 high-complexity hotspots: `merge_settings_payload` (CC=35), `SearchRequest::validate` (CC=29), `compute_exact_vs_prefix_bucket` (CC=26), `build_results_response` (CC=22), `browse_index` (CC=21). Each refactored into domain-grouped private helpers. Added settings characterization tests (`settings_tests.rs`). Moved `SearchCompat` trait methods to default implementations. |
| Full regression gate + targeted fixes | ✅ Done | mar30_pm_5 | Ran the full post-merge regression gate across Rust, dashboard, browser, SDK, and Go surfaces. The real regression fix was FastEmbed test nondeterminism caused by concurrent ONNX/model cache initialization; affected tests are now serialized. Proof artifacts were captured in `engine/state/`, and the committed OpenAPI export was re-synced after restoring real browse/experiment endpoint summaries in current `main`. |
| Public doc sync surface hardening | ✅ Done | mar30_pm_6 | The public-doc contract is now explicit in `.debbie.toml`: `ROADMAP.md`, `PRIORITIES.md`, `engine/LIB.md`, `engine/docs2/FEATURES.md`, `engine/loadtest/BENCHMARKS.md`, and the public `engine/docs2/1_STRATEGY/` + `3_IMPLEMENTATION/` trees are all whitelisted intentionally. Added `engine/tests/doc_sync_helpers.sh`, `engine/tests/validate_sync_surface.sh`, widened `engine/tests/validate_doc_links.sh`, and scrubbed non-public path references from the synced doc graph, including dev-only multi-instance script references in `engine/README.md`. |
| HA convergence contract + runbook truth sync | ✅ Done | mar31_am_2 | Boundary path executed. Added `engine/docs2/4_EVIDENCE/HA_CONVERGENCE_ANALYSIS.md`, aligned `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md` and `engine/examples/ha-cluster/README.md` with the proven async-replication boundary, and tightened `engine/loadtest/tests/ha_soak_acceptance.sh`. |
| Debbie sync wave 3 | ✅ Done | mar31_am_1 | Published the latest post-launch hardening to staging commit `6166055` (CI run `23818440499`) and prod commit `b7841a0` (CI run `23819698304`). Carried the HA boundary truth surfaces, public doc sync contract, regression-gate follow-through, and refreshed committed OpenAPI export. |
| Nightly CI + sync hygiene | ✅ Done | mar31_pm_1 | Restored nightly Rust CI parity by stubbing the dashboard dist asset, added `CHANGELOG.md`/`CONTRIBUTING.md`/`SECURITY.md` to the public sync whitelist, and clarified root README vector/hybrid platform caveats by target. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Operations runbook hardening | ✅ Done | mar31_pm_2 | `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md` now carries proof-backed startup/readiness/replication/admin-key/snapshot failure runbooks, stronger ownership links to deployment/security/config docs, corrected `flapjack --data-dir <path> reset-admin-key` syntax, and tightened proof citations. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Security baseline docs + test coverage | ✅ Done | mar31_pm_3 | `engine/docs2/3_IMPLEMENTATION/SECURITY_BASELINE.md` and `engine/docs2/4_EVIDENCE/SECURITY_BASELINE_AUDIT.md` now capture the shipped HTTP hardening surface (CORS, body limits, trusted proxies, per-key rate limiting) with focused proof references, and non-strict startup catchup now warns-and-continues on write-queue timeout. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Security baseline follow-through | ✅ Done | mar31_pm_4 | Closed the scoped HTTP-hardening proof gaps with invalid-key non-consumption and `FLAPJACK_MAX_BODY_MB` `413` tests, aligned the security docs/audit, refreshed the committed OpenAPI export, and tightened helper-script safety around sync destinations. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Runbook parity + admin-key truth sync | ✅ Done | mar31_pm_5 | Standardized `flapjack --data-dir <path> reset-admin-key` across startup output, dashboard auth help, `engine/docs/AUTH_DESIGN.md`, and `OPERATIONS.md`, including shell-safe quoting for spaced paths. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Experiment handler merge guardrails | ✅ Done | mar31_pm_6 | Routed `/2/abtests/{id}/results` through the shared resolver seam, added direct results-endpoint proof for store-unavailable plus numeric/UUID resolution, and aligned experiment OpenAPI `500` docs with resolver behavior. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Targeted cleanup follow-through | ✅ Done | apr08 | Extracted dashboard experiment normalization/results typing into `engine/dashboard/src/lib/experiment-normalization.ts` and removed a stale server cognitive-complexity suppression. Published in the completed public lineage; see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). |
| Analytics retention hardening + rollup foundation | ✅ Retention done / 🔶 rollup foundation only | apr07_pm_2 | Analytics retention cleanup is deterministic and still defaults to 90 days via `FLAPJACK_ANALYTICS_RETENTION_DAYS`; durable rollup design, known-answer query contracts, schema/config helpers, and `RollupManifest` foundation are merged. Rollup writer, query-planner rollup reads, and retention deletion gated by certified rollup coverage remain unimplemented. |
| Test hygiene, SDK contract CI, and query safety audit | ✅ Done with deferrals | apr07_pm_3 | Public CI now has an SDK contract gate, dashboard/browser tests were tightened to reduce false positives, stale test-script shims were removed in favor of `engine/s/test`, and confirmed query/request crash paths now return typed/sanitized errors. The run still deferred OpenAPI snapshot follow-up verification and did not execute the separate search-HA ownership plan. |
| OSS polish wave: lane A repo cutover | ✅ Done (superseded by flapjackhq transfer) | may22_5pm_2 | Original may22 cutover landed at `griddlehq/flapjack`; superseded 2026-05-24 by may24_816am L1 which transferred the engine repo from `griddlehq/flapjack` → `flapjackhq/flapjack` for owner-identity consistency with the SDK family. Old URL auto-redirects. install.flapjack.foo now points at `flapjackhq/flapjack/main/engine/install.sh`. Historical detail retained in private stage evidence. |
| OSS polish wave: lane B nightly CI | ✅ Done with deferred-op | may22_5pm_3 | `engine/rust-toolchain.toml` pinned to 1.95.0 and added to `.debbie.toml` `[sync].files`. Nightly workflow JS/Go/server-backed SDK jobs realigned with CI owner parity (shared server artifact, JS workspace flow, Go unit/e2e split). Stages 1–3 complete; Stages 4–5 (3 consecutive green nightly runs on `griddlehq/flapjack`) marked `@work:deferred-op` and never dispatched after cutover. |
| OSS polish wave: lane C README polish | ✅ Done | may22_5pm_4 | 9-stage audit covering root + engine READMEs, CONTRIBUTING audit-and-extend, SECURITY disclosure-path audit, `engine/LIB.md` architecture refresh, CHANGELOG v1.0.0 fidelity, SDK README cleanup, root clutter sweep, and consolidated outbound-link gate. |
| OSS polish wave: lane D docs + demo | ✅ Done | may22_5pm_5 | docs.flapjack.foo + flapjack-demo + apex `flapjack-website` redeployed via Cloudflare Pages. Stage 1 audit froze the docs owner; Stage 2 corrected launch-link sources in `website/index.html` + `engine/demo-dualclient`; Stage 3–4 deployed docs/demo/apex from selected owners; Stage 5 ran the sitemap-first URL audit with ≥10-URL + zero-non-2xx/3xx gate. |
| OSS polish wave: lane E SDK release | ✅ Done (publishes landed across canonical owners) | may22_5pm_6 | Stage 1 topology audit correctly stopped this lane from publishing from the dev mirror; canonical owners later shipped: PyPI `flapjack-search==1.0.0` (may23 Lane 7), Go `github.com/flapjackhq/flapjack-search-go/v4@v4.0.0` (may23 Lane 9), npm `flapjack-search@1.0.0` + 6 scoped packages (2026-05-24 via OIDC Trusted Publishing from `flapjackhq/flapjack-search-javascript` `release.yml`; sigstore provenance log entries `1626399336`–`1626400381`). |
| OSS polish wave: lane F e2e validation | ✅ Done (all 3 gaps closed) | may22_5pm_7 | 8-stage cross-platform validation evidence is retained in private stage artifacts. Final closure: (Stage 1 GHCR arm64) v1.0.1 multi-arch shipped to `ghcr.io/griddlehq/flapjack` in may23 Lane 5 and retagged to `ghcr.io/flapjackhq/flapjack` on 2026-05-24 (`docker manifest inspect ghcr.io/flapjackhq/flapjack:1.0.1` shows linux/amd64+linux/arm64); (Stage 5 faceting) `pass=12 fail=0` broader + `pass=6 fail=0` focused; (Stage 6 SDK registry) Python 1.0.0 (may23 L7), Go v4.0.0 (may23 L9), npm 1.0.0 + 6 scoped packages (2026-05-24 OIDC). |
| may24_816am batch: flapjackhq transfer + RF planning baselines + npm 1.0.0 | ✅ Done | may24_816am | (L1 `d1e30589`) engine repo transferred `griddlehq/flapjack` → `flapjackhq/flapjack`; (L3 `3bdcdffe`) Wave 2 RF-2 analytics rollup build note retained in private stage evidence; (L4 `1283eb7e`) Wave 3 RF-1 HA design detail retained in private ADR evidence + red ha_contracts test baseline (`engine/tests/ha_contracts/c[1-5]_*.rs`); (wrap-up `a2a9feca`) owner-drift sweep; (post-wrap 2026-05-24) GHCR multi-arch retagged to flapjackhq namespace via `docker buildx imagetools create`, npm published via OIDC Trusted Publishing on `flapjackhq/flapjack-search-javascript` (provenance entries `1626399336`–`1626400381`). Wave 2 + Wave 3 dispatched against this baseline. |

---

## Shipped Feature Status

All shipped capability status lives in the feature tables below (Search, Indexing, Analytics, etc.) through the Production-Readiness tiers. `ROADMAP.md` and `engine/README.md` must link here instead of duplicating feature/readiness inventories.

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

## Indexing & Records

| Feature | Status | Notes |
|---|---|---|
| Schemaless JSON upload | ✅ | Dual-field schema (search + filter), nested objects via dot notation |
| Single record CRUD | ✅ | |
| Batch operations | ✅ | Up to 1000 ops, hybrid batching (10 ops or 100ms) |
| Browse (full index scan) | ✅ | Cursor-based pagination |
| deleteByQuery | ✅ | |
| partialUpdateObjects | ✅ | |
| Index copy / move / clear | ✅ | |
| Replicas | ✅ | Virtual + standard replicas |
| Task status API | ✅ | Async task tracking |

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
| Events / Insights API | ✅ | click, conversion, view events with position tracking |
| Event Debugger | ✅ | Per-index event stream inspection |
| A/B Testing (experiments) | ✅ | Traffic split, variant tracking, winner selection |
| Usage metering | ✅ | Per-key, per-index operation counts |
| Analytics retention cleanup | ✅ | Partition-based retention cleanup is configurable with `FLAPJACK_ANALYTICS_RETENTION_DAYS`, defaults to 90 days, skips malformed/non-partition paths, and is covered by deterministic cutoff tests. |
| Durable analytics rollup storage | ✅ | Rollup writer + query planner fallback + certified-coverage retention gate are shipped. Proof: `engine/src/analytics/writer.rs` (rollup writer), `engine/src/analytics/query/mod.rs` (rollup planner with raw fallback), `engine/src/analytics/retention.rs` + `engine/src/analytics/manifest.rs` (certified-coverage delete gate), `engine/loadtest/soak_proof.sh` (soak evidence flow). Rollout design and test-citation details are retained in private stage evidence. |

## Personalization & AI

| Feature | Status | Notes |
|---|---|---|
| Personalization API | ✅ | Event scoring, user profile building, personalizationImpact |
| Personalization in search | ✅ | Profile applied at query time |
| Recommendations API | ✅ | related-products, bought-together, trending, looking-similar |
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
| Secured API keys (signed) | ✅ | |

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
| S3 snapshots | ✅ | Single-node snapshot APIs with scheduled backups and empty-dir auto-restore. Verified via MinIO harness in `engine/examples/s3-snapshot/`. |
| SSL / TLS | ✅ | Let's Encrypt ACME automation |
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

## SDK & Widget Compatibility

| Client | Status | Verification |
|---|---|---|
| JavaScript / TypeScript (algoliasearch v5) | ✅ | 32 contract + 13 full-compat tests |
| SDK contract CI gate | ✅ | Public CI runs `engine/sdk_test/contract_tests.js` against a built Flapjack server, protecting Algolia-compatible client behavior outside local-only scripts. |
| PHP | ✅ | Smoke test |
| Python | ✅ | Smoke test |
| Ruby | ✅ | Smoke test |
| Go | ✅ | Smoke test |
| Java | ✅ | Smoke test |
| Swift | ✅ | Smoke test |
| InstantSearch.js v5 | ✅ | 15 instantsearch contract tests |
| React InstantSearch | ✅ | Via instantsearch.js proxy |
| Vue InstantSearch | ✅ | Via instantsearch.js proxy |
| Angular InstantSearch | ✅ | Via instantsearch.js proxy |
| InstantSearch Android | ✅ | Via Kotlin client + Java smoke |
| InstantSearch iOS | ✅ | Via Swift client + Swift smoke |
| Autocomplete.js | ✅ | |

## Dashboard UI

22 user-facing routes are shipped, backed by 21 lazy-loaded page components, plus the `*` not-found catch-all. No stub pages remain.
The route inventory spans overview, search/browse, settings, analytics, relevancy controls, security tooling, and migration workflows with no placeholder pages.

| Status | Features |
|---|---|
| ✅ Built | Overview, Search & Browse (including Hybrid Search mode), Settings (all tabs, including Vector Search settings), Analytics (7 tabs), Synonyms, Rules, Merchandising Studio, API Keys (with `restrictSources`), Search Logs, Query Suggestions, Personalization, Recommendations, Experiments, Event Debugger, Metrics, System, Migrate, Dictionaries, Security Sources, Chat/RAG |

## Testing & Quality Assurance

### E2E Browser Tests (Playwright)

340+ tests across 46 Playwright spec files in total — 42 browser specs (41 specs in `tests/e2e-ui/` [4 smoke + 37 full] plus root-level `tests/result-helpers.spec.ts`) and 4 API-contract specs in `tests/e2e-api/`. Present-tense full-suite green status is based on exact-HEAD wrapper verification at commit `aa7dd7db61d7e274cdf946ac6dd7d7435c4dcdf4`: `cd engine && ./s/test --all` completed green with all 14 sections passed and exit 0. The Apr 15 test-hygiene merge tightened `.or()` and browser-API usage to reduce false positives, but it did not replace that full-suite proof with a newer exact-HEAD full wrapper run.
Coverage includes smoke and full-browser flows across index creation, search, faceting, settings, analytics, dictionaries, security sources, API keys, and migration.

Coverage hardened by three MAR18 workstreams (merged 2026-03-18):

| Workstream | Scope | Record |
|---|---|---|
| A — CRUD & Data Management | Documents, settings, rules, merchandising, API keys, dictionaries, security sources (7 stages, 19/19 checks) | Internal workstream checklist retained in the dev repo |
| B — Intelligence & Analytics | Analytics, query suggestions, experiments, personalization, recommendations (5 stages, 18/20 checks) | Internal workstream checklist retained in the dev repo |
| C — System, Devtools, Edge Cases | System/metrics/migration reconciliation, devtools, navigation dedup, adversarial search, shared constants (6 stages, 16/16 checks) | Internal workstream checklist retained in the dev repo |

Quality standards: zero ESLint violations, zero CSS class selectors, zero sleeps, zero conditional assertions, content verification (not just visibility), deterministic seed data with cleanup.

### Tour Video Walkthroughs (Playtour)

MP4 video walkthroughs proving each dashboard feature works end-to-end. Each video = living documentation + regression detection.
Tour phases document infrastructure setup, CRUD workflows, intelligence features, system/developer workflows, and edge-case coverage. The backend-wiring audit confirmed route handlers were connected end-to-end for the audited pages.

| Phase | Status | Specs |
|---|---|---|
| 0 — Infrastructure | 8/8 done | Seed data, helpers, config, constants, multi-spec runner verified |
| 1 — Core Search | 6/6 done | 01-overview, 02-search-basic, 03-search-facets, 04-search-synonyms, 05-search-vector-hybrid, 06-chat-rag |
| 2 — Data Management | 5/5 done | 07-documents-crud, 08-settings, 09-synonyms-crud, 10-rules-crud, 11-merchandising |
| 3 — Intelligence | 5/5 done | 12-analytics, 13-query-suggestions, 14-experiments, 15-personalization, 16-recommendations |
| 4 — Developer & System | 5/5 done | 17-api-keys, 18-system-health, 19-dictionaries, 20-security-sources, 21-api-logs-events |
| 5 — Edge Cases | 3/3 done | 22-edge-cases, 23-navigation-ux, 24-migrate |
| 6 — Review & Polish | 4/4 done | Watched videos, filed UX issues, re-recorded `18-system-health.mp4`, created `TOUR_INDEX.md`. |

**2026-03-30 refresh:** `05-search-vector-hybrid.spec.ts` and `06-chat-rag.spec.ts` now have archived MP4 artifacts, closing the former vector/chat tour gap.

### Load & Stress Testing (k6)

k6 test suite in `engine/loadtest/` covering concurrent production traffic patterns. See [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md).

| Scenario | File | What it measures |
|---|---|---|
| Smoke | `scenarios/smoke.js` | Health, basic search/write, gate before heavier runs |
| Search throughput | `scenarios/search-throughput.js` | Concurrent read performance, p95/p99 latency under ramp |
| Write throughput | `scenarios/write-throughput.js` | Batch write concurrency, task creation rate, error rates |
| Mixed workload | `scenarios/mixed-workload.js` | Concurrent reads + writes, tagged metrics per workload |
| Spike | `scenarios/spike.js` | Traffic burst recovery, error rates during sudden load jump |
| Memory pressure | `scenarios/memory-pressure.js` | Validates memory_middleware.rs behavior at Normal/Elevated/Critical pressure levels |

### Large-Dataset Benchmarking (100k docs)

Added by mar22_2. Deterministic 100k-doc product dataset generator with import throughput, search latency, and concurrent load benchmarks. See [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md) for results.

| Tool | File | What it measures |
|---|---|---|
| Dataset generator | `generate_dataset.mjs` | Deterministic 100k product docs from 25 base products, batched JSONL output |
| Import benchmark | `import_benchmark.sh` / `import_benchmark.mjs` | Batch ingest throughput, per-batch latency (avg/p95/p99), error rate |
| Search benchmark | `search_benchmark.sh` / `search_benchmark.mjs` | Latency by query type (prefix, typo, multi-word, facet, geo, filter, highlight) |
| k6 concurrent load | `benchmark_k6.sh` | Full k6 suite against 100k-doc index |
| Dashboard perf | `tests/e2e-ui/full/large-index-perf.spec.ts` | Page load and search responsiveness with 100k-doc index |

### Regression Guard Scripts

CI-runnable scripts that verify documentation accuracy and API completeness against a live server.

| Script | Purpose |
|---|---|
| `engine/tests/readme_api_smoke.sh` | Starts a clean server, executes every API curl example from the root README, asserts correct responses |
| `engine/tests/validate_doc_links.sh` | Checks all internal markdown links in the current public routing docs (`README.md`, `PRIORITIES.md`, `ROADMAP.md`, `engine/README.md`, `engine/docs/HIGHEST_LEVEL.md`, `engine/docs2/FEATURES.md`, and `engine/docs2/1_STRATEGY/HIGHEST_PRIORITY.md`) resolve to real files |
| `engine/tests/integration_smoke.sh` | Comprehensive 513-line API integration test: 13 categories (health, index CRUD, doc CRUD, search variants, settings, synonyms, rules, analytics, API keys, dashboard, multi-index, browse, task status). Added by mar22_pm_3. |
| `engine/tests/upgrade_smoke.sh` | Starts an older binary on a temp data dir, seeds data, then upgrades that same dir to a newer binary and re-verifies health/readiness/search/write/dashboard |

---

## Current Production-Readiness State

Production-readiness checklist organized by priority tier. Tier 1 items were launch blockers, Tier 2 items are required for production confidence, Tier 3 items can be iterated on post-launch.

As of the 2026-06-05 doc truth-up, v1.0.8 released 2026-06-05 is the current shipped paid-beta baseline. This section remains the canonical readiness snapshot while ongoing lane-state and post-ship sequencing are intentionally routed to [`ROADMAP.md`](../../ROADMAP.md) and [`PRIORITIES.md`](../../PRIORITIES.md) to avoid duplicate live-status prose in this owner.

**Last updated: 2026-06-05**

### Tier 1 — Launch Blockers

These must be complete before any customer-facing deployment or open-source release.

| # | Work Item | Status | Description |
|---|-----------|--------|-------------|
| PR-1 | `./s/test --all` green | ✅ Done (2026-03-26) | Exact-HEAD wrapper verification passed at commit `aa7dd7db61d7e274cdf946ac6dd7d7435c4dcdf4`, with all 14 sections green. A second independent wrapper proof also completed green on 2026-03-26. |
| PR-2 | Load & stress testing | ✅ Done (2026-03-19) | k6 suite in `engine/loadtest/` — 6 scenarios covering search throughput, write throughput, mixed workload, spike, and memory-pressure validation. Branch: `batman/mar19_2_load_stress_testing`. |
| PR-3 | Tour video completion (Phases 2–5) | ✅ Done (2026-03-20); refreshed 2026-03-30 | Original closure shipped 22/24 archived MP4 artifacts. The former vector/chat blockers (05/06) were later closed with dedicated specs plus default-build/vector+AI runtime wiring, bringing archived per-feature MP4 coverage to 24/24. Branch: `batman/mar19_3_tour_videos_phases_2_5`. |
| PR-4 | UI/UX audit, polish & Tour Phase 6 | ✅ Done (2026-03-21) | Two parts that must happen together: (1) **Tour Phase 6** — watch all 22 recorded tour videos and identify every moment of confusion, awkward flow, or unclear labeling; (2) **Fix + re-record** — address identified issues across the shipped dashboard route set, re-record final polished videos, create index video. Scope includes error message quality (are failure states helpful and actionable?), empty states, loading states, workflow coherence, information hierarchy. Known issues: (a) API Keys layout "feels chaotic", (b) System > Index Health "too much info at once", (c) sidebar index list clutter. The tour videos are the no-manual-QA equivalent of a human QA pass — watching them *is* the human-perspective walkthrough. |
| PR-9 | Security audit | ✅ Done (2026-03-21) | Stage 1 closed targeted evidence gaps: malformed-request rejection without panic plus sanitized invalid-credential bodies (`engine/tests/test_security_audit.rs`), restricted-key cross-index denial using shared `key_allows_index()` (`engine/tests/test_tenant_isolation.rs`, `engine/flapjack-http/src/handlers/search/batch.rs`), and API-key entropy coverage near `generate_hex_key()` (`engine/flapjack-http/src/auth_tests/key_store_tests.rs`, `engine/flapjack-http/src/auth/key_store.rs`). Full OWASP top-10 pass complete (2026-05-25). |
| PR-14 | First-run experience audit | ✅ Done (2026-03-21) | Follow the root quickstart in `../../README.md` from a blank machine with fresh eyes. Time how long it takes to go from binary download → first index → first working search. Document every friction point, confusing error, or missing step. Fix and update docs until the experience is under 5 minutes with zero head-scratching. This is the single highest-impact thing for open-source adoption — a frustrated developer who can't get started in 5 minutes closes the tab. |

### Tier 2 — Production Confidence

Required for sleeping well at night when enterprise customers run production workloads.

| # | Work Item | Status | Description |
|---|-----------|--------|-------------|
| PR-5 | Accessibility (axe-core + WCAG) | ✅ Done (2026-03-21) | `@axe-core/playwright` integrated into Playwright suite (`accessibility.spec.ts`). Automated WCAG violation detection covers all dashboard routes for missing labels, broken ARIA, and contrast issues. Known Radix-tab ID suppressions documented inline. |
| PR-6 | Deep health check | ✅ Done (2026-03-21) | `/health/ready` ships as an operational readiness probe (`engine/flapjack-http/src/handlers/readiness.rs`) with canonical 503 failure envelope and 200 on healthy/empty-node states. Bug fixed 2026-03-23: `_usage/` excluded from tenant probing in `tenant_dirs.rs`. Future depth additions (S3 accessibility, replication connectivity, index-file readability) tracked separately if needed. |
| PR-7 | Latency histograms + performance baseline | ✅ Done (2026-03-21) | Stage 3 shipped request-latency histogram instrumentation (`engine/flapjack-http/src/latency_middleware.rs`) and `/metrics` exposition integration (`engine/flapjack-http/src/handlers/metrics.rs`). Stage 4 published the benchmark baseline in [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md); `engine/loadtest/run.sh` exits with code 99 only for threshold breaches while completing all scenarios. Benchmark figures remain owned by `BENCHMARKS.md`. |
| PR-8 | Error recovery + data durability | ✅ Done (2026-03-21) | Delivered targeted integration tests: (a) crash-during-indexing → restart → zero data loss (`crash_durability_test.rs`), (b) restart-during-active-writes → acknowledged writes survive (`restart_during_writes_test.rs`), (c) replication peer catch-up reconnection, (d) S3 backup/restore round-trip, (e) multi-tenant isolation under adversarial load (`test_tenant_isolation.rs`). 2h soak artifacts prove bounded latency and exact post-restart count preservation. |
| PR-10 | Chaos / resilience testing | 🟡 Folded into PR-8 (2026-03-21) | Originally planned as a distinct fault-injection harness with `engine/scripts/chaos_test.sh` + `engine/tests/test_resilience_isolation.rs`. Neither file was created; the resilience surface is in practice covered by PR-8's targeted integration tests (`crash_durability_test.rs`, `restart_during_writes_test.rs`, replication catch-up, S3 round-trip, `test_tenant_isolation.rs`) plus the private 2h HA soak harness evidence. A standalone adversarial chaos suite (kill server mid-write, fill disk mid-write, partition replica from primary, OOM-kill and restart) remains a post-launch hardening item — not currently a release blocker, but worth re-opening for enterprise-tier customers. |

### Post-Launch Work

Important for long-term operational maturity. Can iterate after initial release.

| # | Work Item | Status | Description |
|---|-----------|--------|-------------|
| PR-11 | Distributed tracing (OpenTelemetry) | ✅ Done (2026-03-28) | OTLP gRPC trace export is shipped behind the `otel` Cargo feature flag. Runtime configuration uses `OTEL_EXPORTER_OTLP_ENDPOINT`, and startup wiring now initializes OTEL when the endpoint is set. |
| PR-12 | Runbooks & incident response | ✅ Done (2026-05-25) | Closeout verified in `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md` with 19 `### Scenario` runbooks and explicit `**Test (where applicable):**` markers; Stage 2 regression coverage commands reran green at HEAD in this stage: `cargo test -p flapjack-http readiness::tests -- --test-threads=1`, `cargo test -p flapjack-http startup_catchup::tests -- --test-threads=1`, `cargo nextest run --test test_replication -E "test(test_startup_catchup_noop_without_replication) or test(test_two_node_startup_catchup_via_get_ops) or test(test_restart_catches_up_before_serving)"`, and `cargo nextest run -p flapjack-server --test admin_key_test` (evidence intentionally excludes the separately tracked ignored admin-key concurrency regression). |
| PR-16 | Disaster recovery measured backup/restore contract | ✅ Done (2026-05-25) | File-snapshot DR contract is now documented in [`engine/docs2/3_IMPLEMENTATION/DISASTER_RECOVERY.md`](3_IMPLEMENTATION/DISASTER_RECOVERY.md) from the Stage 1 measured artifact (`engine/target/dr_proof/latest/measurements.txt`): `RPO_MEASURED_MS=10`, `RTO_MEASURED_MS=221`, and exact `DOC_COUNT_AT_SNAPSHOT=DOC_COUNT_AT_RESTORE=550` parity under active-write snapshot capture. |
| PR-13 | Mobile / responsive dashboard | 🔴 Not started | Admin dashboard responsive design for tablets/phones. Low priority — desktop-first is acceptable for admin tooling. |
| PR-14 | Search HA ownership/freshness design | 🔴 Deferred / design gate required | Active health probing already exists, but automatic search write promotion remains unsafe until index ownership, generation/term, replica freshness, restart recovery, and split-brain behavior have one tested source of truth. Safe forwarding/503 behavior can proceed after that abstraction is specified. |
| PR-15 | Durable analytics rollup writer/query planner | ✅ Done (2026-05-25) | The Apr 15 design foundation is now fully closed: rollup writer, query-planner fallback, and certified-coverage retention gating are shipped. See the durable analytics row in [Analytics & Insights](#analytics--insights) for canonical code-owner and proof references. |

### Recommended Execution Order

All Tier 1 and Tier 2 engineering tracks are complete and the OSS launch gate is closed. v1.0.0 released with all 5 binary targets + Docker image. The latest public lineage now includes the Mar 31 pm1-pm6 stack plus Apr 8, Apr 15, and May 25 follow-through; exact publication proof is tracked in [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical). Current post-launch focus is search HA ownership/freshness plus continued runbook/security refinement from real incidents.

**Pre-launch (P0):**
1. ~~Post-merge regression validation~~ — ✅ Done (mar22_1). OpenAPI spec regenerated, Rust test suite validated, README API smoke and doc link validation scripts created.
2. ~~End-to-end workflow audit~~ — ✅ Done (mar22_pm_3). Comprehensive `engine/tests/integration_smoke.sh` (513 lines) exercises every major API path: health, index CRUD, document CRUD, search (all query types), settings, synonyms, rules, analytics events, API keys, dashboard, multi-index search, browse, task status. Also validated by mar22_1 (README API smoke) and mar22_2 (large-dataset behavior).
3. ~~HA topology verification~~ — 🟡 Executed with findings (mar22_3, mar29_pm_1, mar30_pm_3). 3-node HA cluster, 2-node replication/analytics fan-out, and S3 snapshot/restore were verified via Docker. The Mar 30 HA soak harness proof ran for 2h across 39 restart rotations and proved restart survivability; the May 26 diagnostic soak after the L1 anti-entropy fix (`066549d5`) refined the picture to **bounded convergence** (~0.88% steady-state spread), with the residual nginx restart-window write loss tracked as PL-8. See [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md) for the retained proof pack and interpretation, and ADR `0004` for the narrative seam.
4. ~~README + docs polish~~ — ✅ Done (mar22_pm_2). Full mechanical code-vs-docs audit across all public-facing documents. Env var completeness verified (OPS_CONFIGURATION.md canonical), feature claims verified against code, quickstart guides reconciled, cross-doc DRY violations fixed, PHASES.md deprecated. 1 code blocker was logged in an internal audit note (`/health/ready` unit test vs runtime mismatch). All validation scripts pass.
5. ~~Dashboard performance audit~~ — ✅ Done (mar22_2). Large-index perf test (`large-index-perf.spec.ts`) verifies dashboard responsiveness with 100k-doc index.
6. ~~Deployment path verification~~ — ✅ Done (mar22_3). Docker single-node, 3-node HA, 2-node replication, S3 snapshot harness all verified. Systemd stays manual follow-up.

**Post-launch (Tier 3 hardening):**
1. ~~PR-11: OpenTelemetry~~ — ✅ Done (2026-03-28). OTLP gRPC export is shipped behind the `otel` feature flag with `OTEL_EXPORTER_OTLP_ENDPOINT` configuration.
2. **PR-14: Search HA ownership/freshness design** — unblock safe read fallback/write forwarding decisions before any automatic write promotion.
3. **PR-12: Runbooks & incident response** — continue from real incidents plus any remaining proof-summary cleanup.
4. **PR-13: Mobile / responsive dashboard** — low priority, desktop-first acceptable.

### Completed Work Archive

| Date | Milestone | Details |
|------|-----------|---------|
| 2026-05-27 | may27_1: PL-9 rollup window override shipped (commits `9353fca8` + `05b4ae8b`) | `FLAPJACK_ROLLUP_WINDOW_OVERRIDE_MS` now resolves through `engine/src/analytics/mod.rs`, is consumed by `engine/flapjack-http/src/background_tasks.rs` + `engine/src/analytics/writer.rs`, and is passed through `engine/loadtest/soak_proof.sh` so the soak harness and engine use the same window width. Executable proofs: `engine/loadtest/tests/soak_proof_analytics_acceptance.sh` and `engine/loadtest/tests/analytics_soak_window_override_acceptance.sh`. |
| 2026-05-26 | may26_pm_3: PL-1 test-hang CI-side cap closeout | Canonical timeout-policy completion recorded from current HEAD acceptance proof: `.github/workflows/ci.yml` and `.github/workflows/nightly.yml` now enforce `timeout-minutes: 10` on the five direct `cargo nextest run` steps, with contract assertions in `engine/tests/ci_test_timeout_cap_acceptance.sh`. |
| 2026-03-13 | Backend API 197/197 | Full Algolia parity verified; the detailed audit history is retained in the dev repo. |
| 2026-03-13 | SDK compatibility verified | JS (32 contract + 13 compat), PHP, Python, Ruby, Go, Java, Swift smoke tests, InstantSearch.js (15 contract tests). |
| 2026-03-14 | Dashboard route inventory shipped | 22 user-facing routes backed by 21 lazy-loaded page components, with zero stubs. |
| 2026-03-14 | GL-1 through GL-9 | Griddle launch checklist complete. |
| 2026-03-18 | MAR18 Workstream A | CRUD & data management e2e hardening — 7 stages, 19/19 checks. |
| 2026-03-18 | MAR18 Workstream B | Intelligence & analytics e2e hardening — 5 stages, 18/20 checks. |
| 2026-03-18 | MAR18 Workstream C | System, devtools, edge cases e2e hardening — 6 stages, 16/16 checks. |
| 2026-03-18 | E2E test count: 340/340 | Up from 320. All passing across 36 spec files (baseline before mar19_1). |
| 2026-03-18 | Tour system bootstrap (Phase 0 + Phase 1 partial) | Infrastructure + 5/24 specs (01-04, 09). |
| 2026-03-19 | PR-1: `./s/test --all` green | Historical milestone; later superseded by exact-HEAD wrapper reruns, including the final 2026-03-26 green proof at commit `a220e66c`. |
| 2026-03-19 | PR-2: Load & stress testing | k6 suite built — 6 scenarios, `engine/loadtest/`. |
| 2026-03-19–20 | PR-3: Tour videos phases 2–5 | 17 new specs (07-24 minus 05/06), 22/24 total with MP4 artifacts. Rust source fixes: index recovery, relevance scoring. IndexTabBar refactor. |
| 2026-03-20 | mar20_2 observability/security (Stages 1–3) | Stage 1 closed targeted PR-9 security-coverage gaps (malformed-request rejection without panic, sanitized invalid-credential bodies, restricted-key cross-index denial via shared `key_allows_index()`, API-key entropy tests). Stage 2 shipped `/health/ready` with canonical readiness error contract. Stage 3 shipped request-latency histogram instrumentation and `/metrics` exposition wiring. |
| 2026-03-21 | mar20_2 observability/security (Stage 4 baseline artifact) | Published the loadtest baseline artifact in [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md); `engine/loadtest/run.sh` now reports threshold-breach runs with exit code 99 after completing all scenarios. |
| 2026-03-21 | mar21_1: Vector search first-class (Stages 1–3) | Stage 1: `capabilities` object (`vectorSearch`, `vectorSearchLocal`) in `/health` + startup banner. Stage 2: all 4 dashboard components capability-aware (Chat, SearchModeSection, EmbedderPanel, VectorStatusBadge) with e2e route-interception tests. Stage 3: `VECTOR_SEARCH_QUICKSTART.md` created and linked. Branch: `batman/mar21_1_vector_search_first_class`. |
| 2026-03-21 | mar21_2: Federated multi-index search (Stages 1–5) | Full Meilisearch-compatible federation: `federation.rs` module (419 lines) with weighted RRF merge, `FederationConfig`/`FederationMeta`/`FederatedResponse` types, wired into batch handler. OpenAPI annotations, JS SDK federation types added. Design doc: `FEDERATED_SEARCH_DESIGN.md`. Integration tests in `test_federated_search.rs`. Branch: `batman/mar21_2_federated_multi_index_search`. |
| 2026-03-21 | mar21_3: Observability & ops hardening (Stages 1–2, 4–6) | Stage 1: `x-request-id` middleware (forwarded or UUID v4 generated). Stage 2: `FLAPJACK_LOG_FORMAT=json` structured logging. Stage 4: `FLAPJACK_ALLOWED_ORIGINS` configurable CORS. Stage 5: `FLAPJACK_SHUTDOWN_TIMEOUT_SECS` graceful shutdown. Stage 6: startup dependency summary. Stage 7: `OPS_CONFIGURATION.md` env var reference. Stage 3 (OpenTelemetry OTLP export) was completed later on 2026-03-28. Branch: `batman/mar21_3_observability_ops_hardening`. |
| 2026-03-22 | mar22_1: Regression fix & validation sweep | OpenAPI spec regenerated (federation + observability routes). README API smoke script (`engine/tests/readme_api_smoke.sh`) and doc link validator (`engine/tests/validate_doc_links.sh`) created. Rust test suite and Playwright smoke confirmed green post-merge. Branch: `batman/mar22_1_regression_fix_and_validation_sweep`. |
| 2026-03-22 | mar22_2: Large-dataset performance benchmarking | Deterministic 100k-doc generator (`generate_dataset.mjs`), import throughput benchmark (`import_benchmark.sh`), search latency benchmark by query type (`search_benchmark.sh`), k6 concurrent load runner (`benchmark_k6.sh`), dashboard large-index perf test (`large-index-perf.spec.ts`). Baseline compilation script (`compile_baseline.sh`). Branch: `batman/mar22_2_large_dataset_performance_benchmarking`. |
| 2026-03-22 | mar22_3: HA & deployment verification | Docker single-node, 3-node HA cluster, 2-node replication/analytics fan-out, and S3 snapshot/restore all verified end-to-end via Docker. Dockerfile bind-address fix (`ENV FLAPJACK_BIND_ADDR=0.0.0.0:7700`). New S3 snapshot example (`engine/examples/s3-snapshot/`). HA test script tightened with in-network health probes. Deployment and HA docs reconciled to match verified proofs. S3 snapshot audit doc (`3_IMPLEMENTATION/S3_SNAPSHOT_AUDIT.md`). Branch: `batman/mar22_3_ha_and_deployment_verification`. |
| 2026-03-23 | mar22_pm_1: Full test suite regression gate | Crate-by-crate Rust test validation, fixed taskID alias collisions in index manager (`lifecycle.rs`, `write.rs`, `mod.rs`), fixed 21 dashboard test regressions (unit + e2e), refreshed openapi snapshot fixture, stabilized query-suggestions e2e, and captured wrapper proof artifacts. That run stayed non-green (`a6a12ea1`); launch authority was later superseded by the green proof at commit `a220e66c`. Branch: `batman/mar22_pm_1_full_test_suite_regression_gate`. |
| 2026-03-23 | mar22_pm_2: Documentation accuracy audit | All 6 stages complete. Mechanical code-vs-docs verification across all public-facing documents: env var completeness (OPS_CONFIGURATION.md canonical), feature comparison table verified, quickstart guides reconciled against live contracts, cross-doc DRY violations fixed, PHASES.md deprecated, terminology normalized. Tightened SSL config assertions (`flapjack-ssl/src/config.rs`). 1 code blocker was logged in an internal audit note (`/health/ready` unit test vs runtime mismatch). All validation scripts pass. Branch: `batman/mar22_pm_2_docs_accuracy_audit`. |
| 2026-03-23 | mar22_pm_3: Benchmark validation & integration smoke | New comprehensive API integration smoke test (`engine/tests/integration_smoke.sh`, 513 lines) exercising 13 API path categories. Updated benchmark baseline compilation (`compile_baseline.mjs/sh`). Large-dataset baseline: 100k docs imported in 48.4s, search p95 128ms, all 6 k6 scenarios PASS (Apple M4 Max). Results in [`engine/loadtest/BENCHMARKS.md`](../loadtest/BENCHMARKS.md). Branch: `batman/mar22_pm_3_benchmark_and_smoke`. |
| 2026-03-24 | mar23_pm_4: README screenshot automation | Created automated Playwright e2e test `readme-screenshots.spec.ts` (73 lines) covering Overview, Search, and Migrate screenshot capture with readiness contracts. Branch: `batman/mar23_pm_4_readme_screenshots`. |
| 2026-03-24 | mar23_pm_5: Systemd VPS deployment docs | Improved `engine/examples/systemd/README.md` with production guidance (env file setup, admin key requirements, `/health/ready` probe verification). This remained docs-only at the time and was later closed by the 2026-03-26 live VPS verification. Branch: `batman/mar23_pm_5_systemd_vps_smoke_test`. |
| 2026-03-24 | mar23_pm_6: Test stability & launch status reconciliation | Fixed `Recommendations.test.tsx` (network-noise isolation, test helper refactoring), `analytics-deep.spec.ts` (California hardcode → flexible assertion), `sdk_test/package.json` update, bundler dedup verification script. Launch docs reconciled with canonical wrapper proof status. Branch: `batman/mar23_pm_6_test_all_green`. |
| 2026-03-25 | mar24_pm_1: test hardening at current HEAD | Hardened `query-suggestions.spec.ts` readiness waits, `local-instance-config.ts` parsing/URL handling, Playwright worker override support, socket-churn retries in Rust integration tests, and refreshed the tracked README screenshot PNGs. Targeted validations passed at HEAD; the port-contention issue seen in earlier wrapper runs was resolved in the final green proof at commit `a220e66c`. Branch: `batman/mar24_pm_1_test_suite_green`. |
| 2026-03-25 | stage_04: exact-HEAD wrapper proof refresh | Ran the canonical wrapper at commit `23ac8a9e76c90cf2c36c447b812acdcbf0e32d4e`; executed sections `[1]-[5]` passed, and the first failing executed section was `[6]` Dashboard Playwright smoke (`127.0.0.1:53142` already in use). |
| 2026-03-25 | mar24_pm_2: repo hygiene fixes | Landed the safe hygiene subset: public-repo URL updates in README/show-HN/deployment docs, `<repo-root>` placeholders in retained docs, path-agnostic deploy/sync helper scripts, and cleanup of the duplicated `load_local_instance_config` TODO block. Destructive doc/history removals from the worktree branch were intentionally not merged into `main`. Branch: `batman/mar24_pm_2_repo_hygiene_sweep`. |
| 2026-03-25 | mar25_pm_10: exact-HEAD wrapper proof | Fixed `local-instance-config.ts` quoted-value comment-stripping bug via TDD (new test file `local-instance-config.test.ts`). Produced proof artifacts in the dev repo. Green run at commit `0dc55b39` passed all executed sections [1]-[13]. Subsequent verification run at commit `23ac8a9e` was red due to Playwright port contention (`127.0.0.1:53142` already in use), not a code defect. Updated launch-status docs. Branch: `batman/mar25_pm_10_exact_head_wrapper_proof`. |
| 2026-03-25 | mar25_pm_11: live linux systemd VPS validation | Attempted VPS validation of systemd deployment path. Fixed the EC2 SSH helper to fall back to `~/.ssh`. Refactored the internal local-instance shell helper to use safe KEY=value parsing instead of shell sourcing — adds helper functions for config loading, hostname extraction, loopback detection, and inline comment stripping. Locally verified systemd artifact consistency (`flapjack.service`/`env.example`/README alignment, `/health` + `/health/ready` route contracts). VPS host reachability timed out at that time; this failed attempt was later superseded by the 2026-03-26 successful live verification. Branch: `batman/mar25_pm_11_live_linux_systemd_vps_validation`. |
| 2026-03-25 | mar25_pm_13: live VPS systemd validation (second attempt) | Re-attempted VPS validation; SSH still timing out to `44.202.224.48`. Code review posthoc stages cleaned up analytics test doc comments (`analytics_tests.rs`: replaced TODO stubs with meaningful descriptions), refactored `hybrid.rs` (removed `#[allow(dead_code)]` suppressions, simplified `build_fused_document` and `requested_hybrid_params` with idiomatic Rust), and refactored `search_compat.rs` (extracted `search_with_legacy_options` helper, cleaned doc comments). This failed attempt was later superseded by the 2026-03-26 successful live verification. Branch: `batman/mar25_pm_13_live_vps_systemd_validation`. |
| 2026-03-25 | mar25_pm_14: Rust code quality audit + leaky test fix | Fixed the intermittent nextest leak in integration test local helpers (`engine/tests/common/state.rs` and `engine/tests/common/http.rs`): proper server shutdown and resource cleanup to eliminate leaked child processes/file descriptors. Fixed 2 clippy warnings (feature-gated `dead_code` in `hybrid.rs` — resolved by pm_13's posthoc refactor removing the suppressions entirely). Fixed 6 `cargo fmt` diffs (leading blank lines in `dictionaries.rs`, `metrics.rs`, `notifications.rs`, `rollup_broadcaster.rs`, `router_tests.rs`, `startup_catchup.rs`). Additional cleanup in `language.rs`, `decompound.rs`, `stopwords/mod.rs`, `write_queue/` modules. Nextest now reports 0 leaky, 0 failed. `cargo clippy --workspace` clean. `cargo fmt --check` clean. Branch: `batman/mar25_pm_14_rust_quality_leaky_test`. |
| 2026-03-28 | mar28: Stage 4-6 confidence-completeness ops/security pass | Added `engine/tests/upgrade_smoke.sh` and proved upgrade handoff from the gate-closing staging commit `745a059` to the current binary on the same data dir. Added canonical operator docs in [`engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`](3_IMPLEMENTATION/OPERATIONS.md) and scoped hardening guidance in [`engine/docs2/3_IMPLEMENTATION/SECURITY_BASELINE.md`](3_IMPLEMENTATION/SECURITY_BASELINE.md). Also tightened the MinIO snapshot harness to match its no-auth compose stack and fail fast when a stray local `flapjack` owns `127.0.0.1:7700`. |
| 2026-03-28 | mar28: Stage 3 soak/failure proof pack | Added `engine/flapjack-server/tests/restart_during_writes_test.rs`, extended `crash_durability_test.rs` with a nontrivial acknowledged-dataset recovery proof, and completed 2h `mixed-soak` / `write-soak` artifact capture with `engine/loadtest/soak_proof.sh`. The new soak artifacts prove bounded latency, zero `5xx`, restart-safe recovery, and exact post-soak/post-restart count preservation, while also documenting that the current single-node write-overload thresholds still breach under prolonged overload on this host. |
| 2026-03-26 | mar25_pm_12: Playwright port hardening + wrapper green proof | Fixed port contention between Playwright smoke and full e2e runs within `./s/test --all`. Added a port-release wait step in the canonical test wrapper and `--wait-port-free` mode in `engine/dashboard/scripts/playwright-webserver.mjs`. New vitest coverage for port-wait logic. JSDoc cleanup in `local-instance-config.ts`. Produced an authoritative green exact-HEAD wrapper proof at commit `a220e66c`, with all 14 sections passed. Resolves launch blocker #1 (PR-1). Branch: `batman/mar25_pm_12_playwright_port_wrapper_proof`. |
| 2026-03-26 | stage_04: VPS systemd proof reconciliation | Published a redacted maintained evidence summary in the dev repo from the live Stage 3 run/review, updated launch status ledgers to remove stale VPS blocker language, and added the explicit Linux binary prerequisite in `engine/examples/systemd/README.md`. |
| 2026-03-26 | mar26_am_1: HA cluster dashboard | New Cluster page in React dashboard (`engine/dashboard/src/pages/Cluster.tsx`) showing live peer health with status badges (healthy/stale/unhealthy/circuit_open/never_contacted), auto-refreshing every 5s via `useClusterStatus` hook. Standalone mode shows configuration guidance. Added to sidebar nav and router. Full TDD coverage (`Cluster.test.tsx`, `useClusterStatus.test.ts`). Operator spec at `engine/dashboard/tests/specs/cluster.md`. Branch: `batman/mar26_am_1_ha_cluster_dashboard`. |
| 2026-03-26 | mar26_am_2: VPS systemd end-to-end proof | Full end-to-end systemd deployment validated on Ubuntu EC2 (c7i-flex.2xlarge, us-east-1). Service account, unit file, env file, health probes, restart recovery, crash recovery all confirmed. EC2 helper script secret-path handling was corrected to use the repo-local secret directory. Resolves launch blocker #2. Branch: `batman/mar26_am_2_vps_systemd_proof`. |
| 2026-03-26 | mar26_pm_1: Post-merge regression validation | Full test suite validated green after merging am_1 (HA dashboard) and am_2 (VPS systemd). Cargo check/clippy/fmt clean, 2839+ Rust lib tests, 25 server tests, 542+ vitest, nextest 0 leaky, Playwright smoke+full, SDK/CLI all passing. Metrics handler refactored (extracted `storage_bytes_gauge_values` helper), search_compat doc comments cleaned, common.sh refactored. Green wrapper proof at `aa7dd7db`. Branch: `batman/mar26_pm_1_post_merge_regression_validation`. |
| 2026-03-26 | mar26_pm_2: Debbie config hardening | Replaced dangerous blacklist `.debbie.toml` (syncing entire repo root with 14 exclusions) with proper whitelist config using explicit `sync.files` + targeted `[[sync.dirs]]`. Would have leaked 60+ internal files to public repos. Created `.debbie/post-sync.sh` hook for Cargo.toml path dep fixup. Dry-run validated against staging repo. Branch: `batman/mar26_pm_2_debbie_config_hardening`. |
| 2026-03-26 | mar26_pm_3: README & Show HN launch polish | Fixed 4 stale Show HN claims ("English-only, no vector search, no HA" — all shipped). Root README: feature table verified, architecture tree duplicate fixed, Docker Compose quickstart added (`engine/examples/quickstart/`). engine/README cleaned for public audience. FEATURES.md spec counts corrected to 46. Branch: `batman/mar26_pm_3_readme_launch_polish`. |
| 2026-03-27 | mar26_pm_4: Dev repo test suite + Docker build | Second independent full test suite proof at HEAD; all 14 sections green. Docker build verified with container health + search smoke test. Added `search_compat` shim unit + integration tests. Branch: `batman/mar26_pm_4_dev_repo_test_suite_and_docker`. |
| 2026-03-27 | mar26_pm_5: Debbie staging sync config hardening | Fixed legacy identity values to the correct public-target mappings. Added `ROADMAP.md`, `engine/docs2/` strategy docs, `engine/examples/`, `integrations/laravel-scout/`, CI shell scripts to sync whitelist. Sanitized `FEATURES.md` and `TESTING.md` to remove private dashboard path references for public staging. Executed real debbie sync to staging clone. Stage 4 (staging push + CI) deferred. Branch: `batman/mar26_pm_5_debbie_sync_staging_ci`. |
| 2026-03-27 | mar27_noon: Staging push + CI fix | Fixed `.debbie.toml` whitelist gaps (`validate_doc_links.sh`), fixed post-sync hook to handle `branch =` tantivy deps alongside `path =`, fixed `integration_smoke.sh` executable bit. Pushed to staging repo and drove CI through 6+ rounds of fixes. Key code fixes: dashboard chat e2e specs (embedder readiness), API key create `200` alignment, A/B test create `200` alignment, stale CRUD setup expectations, crash-durability task-poll helper retry robustness, OpenAPI typed-schema corrections. Commits: `d7beff86` through `45374320`. |
| 2026-03-27 | mar27_pm: Launch gate closure + truth-sync | 5-stage launch completion: (1) Staging CI green via gate-closing run `23671792399` on commit `745a059`; (2) truth-synced PRIORITIES.md, ROADMAP.md, HIGHEST_PRIORITY.md to match live launch state; (3) public launch surface audit — README smoke 6/6, doc-link validation green, all live public URLs returning 200; (4) a launch proof pack was created in the dev repo; (5) Algolia compat sprint checklist drafted. |
| 2026-03-27 | mar27_night: Algolia compat hardening | Built deterministic parity foundation: canonical high-risk mutation inventory in `engine/flapjack-http/src/mutation_parity.rs`, behavior-level parity checks in `engine/tests/test_mutation_parity.rs`, spec-level parity checks in `flapjack-http::openapi::tests::high_risk_mutation_openapi_contracts_match_shared_matrix`. Caught and fixed additional drift: `POST /1/indexes/{indexName}` corrected to `201`, missing OpenAPI paths for auto-ID save and partial update, stronger `/2/abtests/{id}/conclude` schema. All 5 stages (mutation matrix, artifact coupling, mirror guards, dashboard readiness contracts, SDK/HTTP reinforcement) complete. |
| 2026-03-28 | mar28: Soak threshold correction + test coverage expansion | Verified staging CI run `23674270883` (33/33 green). Resolved Stage 3 soak threshold breach by introducing `SOAK_WRITE_THRESHOLDS` in `engine/loadtest/lib/throughput.js` — correctly distinguishes sustained-overload acceptance from short-baseline failure. Expanded test coverage: 4 new federation unit tests + 3 handler tests in `batch_federation.rs`, 2 new cluster status backend tests in `internal_tests.rs`, 8 new extended parity lifecycle/error tests in `tests/test_mutation_parity_extended.rs`. All tests green, clippy clean, fmt clean. |
| 2026-03-28 | mar28_pm_1: Security, versioning, and release polish | Created SECURITY.md (vulnerability disclosure policy), CHANGELOG.md (Keep a Changelog format with full feature inventory), CONTRIBUTING.md (contributor guide). All three added to `.debbie.toml` sync whitelist. Bumped all 5 workspace crates from 0.1.0 to 1.0.0. Added version consistency test (`engine/flapjack-server/tests/version_consistency_test.rs`). Created a shared version helper for dev release scripts. Branch: `batman/mar28_pm_1_security_versioning_release_polish`. |
| 2026-03-28 | mar28_pm_2: OpenTelemetry distributed tracing (PR-11) | Feature-gated OTEL OTLP gRPC export behind `--features otel` in `flapjack-http`. New `engine/flapjack-http/src/otel.rs` module with `try_init_otel_layer()` — reads `OTEL_EXPORTER_OTLP_ENDPOINT`, builds OTLP exporter when set, returns None when unset (zero overhead). Wired into tracing subscriber composition in `startup.rs` and graceful shutdown in `server.rs` (provider flush). Updated OPS_CONFIGURATION.md, FEATURES.md, ROADMAP.md, PRIORITIES.md. Both `cargo check -p flapjack-http` and `cargo check -p flapjack-http --features otel` pass clean. Branch: `batman/mar28_pm_2_opentelemetry_distributed_tracing`. |
| 2026-03-29 | mar28_pm_3: TODO stub cleanup + HA soak hardening | Replaced ~601 auto-generated `TODO: Document` stubs with real doc comments across all 4 crates (engine/src: 271, flapjack-http: 322, flapjack-server: 3, flapjack-replication: 5). Added a dev-repo HA soak test harness for 3-node restart-rotation validation. Doc-regression tests for `run_graceful_shutdown` and `load_server_config`. Module-level `//!` summaries added to key files. Branch: `batman/mar28_pm_3_code_hygiene_todo_cleanup_ha_soak`. |
| 2026-03-29 | mar29: Codebase cleanup round 2 | Fixed 15 error-leaking 500 response sites in `settings.rs` (7), `snapshot.rs` (7), `query_suggestions.rs` (1) — all migrated to `HandlerError` which auto-sanitizes internal errors. Consolidated duplicate `internal_error` helpers in `rules.rs`/`synonyms.rs` via same migration. Removed 3 `cognitive_complexity` suppressions in `startup_catchup.rs` by extracting `execute_timed_catchup()` and `handle_fetch_error()` helpers. Removed `server.rs` serve() suppression. Decomposed `execute_search_query` (CC=26, 348 NLOC) into phase helpers. Added `validate_restore_key_override` to snapshot handler. New integration test for query suggestions. Updated `engine/CLAUDE.md` with HandlerError and suppression guidance. Branch: `batman/mar29_codebase_cleanup_checklist`. |
| 2026-03-29 | mar29_pm_1: HA multi-node soak harness + CI integration | Completed HA soak confidence infrastructure for the multi-node branch: a dev-repo harness script, Rust structural integration test (`engine/tests/test_ha_soak_harness.rs`), and shell acceptance checks for soak/topology (`engine/loadtest/tests/ha_soak_acceptance.sh`, `engine/loadtest/tests/ha_topology_acceptance.sh`). Multi-node soak execution itself remains Docker-daemon dependent and was explicitly deferred when Docker is unavailable. Branch: `batman/mar29_pm_1_ha_multi_node_soak`. |
| 2026-03-31 | mar30_pm_5: Full regression gate + targeted fixes | Ran the full post-merge regression gate across Rust, dashboard, browser, SDK, and Go surfaces. The confirmed regression was FastEmbed test nondeterminism from concurrent ONNX/model cache initialization; affected local-embedder tests are now serialized in `engine/src/vector/embedder_tests.rs` and `engine/src/index/write_queue_tests.rs`. Proof artifacts were recorded in `engine/state/`. The follow-through merge on 2026-03-31 also restored real summaries for `browse_index` and the experiment CRUD/conclude handlers and regenerated `engine/docs2/openapi.json`, leaving `openapi_export_tests::committed_docs2_openapi_matches_export_output` green at current `main`. Branch: `batman/mar30_pm_5_full_regression_gate_targeted_fixes`. |
| 2026-03-31 | mar30_pm_6: Public doc sync surface hardening | Hardened the public documentation contract so canonical routing docs are explicitly synced instead of relying on stale mirror state. `.debbie.toml` now whitelists the canonical public doc graph (`ROADMAP.md`, `PRIORITIES.md`, `engine/LIB.md`, `engine/docs2/FEATURES.md`, `engine/loadtest/BENCHMARKS.md`, `engine/docs2/1_STRATEGY/`, `engine/docs2/3_IMPLEMENTATION/`). Added `engine/tests/doc_sync_helpers.sh`, a dedicated `engine/tests/validate_sync_surface.sh`, widened `engine/tests/validate_doc_links.sh`, and scrubbed non-public path references from the newly public docs, including removing dev-only `_dev/s/` multi-instance helpers from `engine/README.md`. Branch: `batman/mar30_pm_6_public_doc_sync_surface_hardening`. |
| 2026-03-31 | mar31_am_2: HA convergence contract + runbook truth sync | Added `engine/docs2/4_EVIDENCE/HA_CONVERGENCE_ANALYSIS.md`, aligned `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md` and `engine/examples/ha-cluster/README.md` with the proven async-replication boundary, and tightened `engine/loadtest/tests/ha_soak_acceptance.sh`. Branch: `batman/mar31_am_2_ha_convergence_contract_and_runbook_truth_sync`. |
| 2026-03-31 | mar31_am_1: Debbie sync wave 3 | Published the then-current post-launch hardening to staging commit `6166055` (CI run `23818440499`) and prod commit `b7841a0` (CI run `23819698304`), carrying the HA boundary truth surfaces, public doc sync contract, regression-gate follow-through, and refreshed committed OpenAPI export. Branch: `batman/mar31_am_1_green_baseline_and_wave3_public_sync`. |
| 2026-03-31 | mar31_pm_1: Nightly CI + sync hygiene | Restored nightly Rust CI parity by stubbing the dashboard dist asset, added `CHANGELOG.md` / `CONTRIBUTING.md` / `SECURITY.md` to `.debbie.toml`, and tightened root README vector/hybrid caveats so pre-built Linux/Windows binary limits match the real release matrix. This was later published in the completed public lineage (see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical)). Branch: `batman/mar31_pm_1_ci_and_sync_hygiene`. |
| 2026-03-31 | mar31_pm_2: Operations runbook hardening | Expanded `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md` with proof-backed failure-mode runbooks, ownership cross-links, corrected `reset-admin-key` CLI ordering, and tighter readiness/replication proof citations. This was later published in the completed public lineage (see [Public Sync Lineage Ledger (Canonical)](#public-sync-lineage-ledger-canonical)). Branch: `batman/mar31_pm_2_operations_runbook_hardening`. |
| 2026-03-31 | mar31_pm_3: Security baseline docs + test coverage | Added the canonical HTTP-hardening ledger in `engine/docs2/3_IMPLEMENTATION/SECURITY_BASELINE.md` and the paired audit matrix in `engine/docs2/4_EVIDENCE/SECURITY_BASELINE_AUDIT.md`, documenting CORS, trusted-proxy, rate-limit, and body-limit behavior with proof references. Added focused security proofs and aligned `startup_catchup.rs` so non-strict bootstrap logs write-queue timeout instead of failing startup. Branch: `batman/mar31_pm_3_security_baseline_docs_and_tests`. |
| 2026-03-31 | mar31_pm_4: Security baseline follow-through | Closed the remaining scoped HTTP-hardening proof gaps with invalid-key non-consumption and `FLAPJACK_MAX_BODY_MB` `413` tests, extracted `max_body_mb_from_value`, aligned the audit/doc surfaces, refreshed `engine/docs2/openapi.json`, and hardened sync helpers against symlinked destinations. Branch: `batman/mar31_pm_4_security_baseline_followthrough`. |
| 2026-03-31 | mar31_pm_5: Runbook parity + admin-key truth sync | Standardized the explicit `flapjack --data-dir <path> reset-admin-key` contract across startup output, dashboard auth help, `engine/docs/AUTH_DESIGN.md`, and `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`, including shell-quoted output for spaced paths. Branch: `batman/mar31_pm_5_runbook_parity_and_admin_key_truth_sync`. |
| 2026-03-31 | mar31_pm_6: Experiment handler merge guardrails | Routed `/2/abtests/{id}/results` through `resolve_store_and_experiment_id()`, added direct results-endpoint seam proofs for store-unavailable plus numeric/UUID resolution, removed the orphaned old OpenAPI test file, and aligned experiment OpenAPI docs with the resolver-driven `500` path. Branch: `batman/mar31_pm_6_experiment_handler_merge_guardrails`. |
| 2026-04-08 | apr08: targeted cleanup follow-through | Extracted dashboard experiment payload normalization/results typing into `engine/dashboard/src/lib/experiment-normalization.ts` with focused tests, reducing `useExperiments.ts` to hook concerns, and removed a stale `clippy::cognitive_complexity` suppression from `flapjack-http/src/server.rs` after re-verifying the crate stays clippy-clean. Commits: `44e7fa9c`, `7250f00b`. |
| 2026-04-15 | apr07_pm_2: analytics retention hardening | Hardened partition-retention behavior with deterministic cutoff tests, preserved the 90-day default, documented the actual env/config surface, added durable rollup design and known-answer query contracts, and merged rollup schema/config/manifest foundation. Rollup writer/query-planner/retention gating remain open. Branch: `batman/apr07_pm_2_analytics_phase5_retention`. |
| 2026-04-15 | apr07_pm_3: test hygiene and safety audit | Added public CI SDK contract coverage, tightened dashboard/browser false-positive patterns, added targeted plural/analytics collector tests, converted confirmed filter query crash paths to typed errors with safer formatting, sanitized experiment internal errors, removed stale test shims, and aligned canonical runner docs. OpenAPI snapshot follow-up remained deferred in the session handoff. Branch: `batman/apr07_pm_3_test_hygiene_and_safety_audit`. |
