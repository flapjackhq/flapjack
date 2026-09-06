# Testing Guide

there is NO manual QA for this project!! keep that in mind.  we need more automated tests than you think.  especially real life simulated human non-mocked real-server e2e tests


its mandatory that we get to a point where we can tell teh CEO that everything works perfectly without needing manual QA

## Multi-Instance + Port Routing (New)

For parallel local work across multiple clones on one machine, tests must use clone-local routing:

```bash
# from repo root
cp flapjack.local.conf.example flapjack.local.conf
# set unique ports per clone:
#   FJ_BACKEND_PORT
#   FJ_DASHBOARD_PORT
# optional:
#   FJ_TEST_ADMIN_KEY
```

Important:
- Dashboard Playwright + Vite config read this repo-local file.
- The canonical `./s/test` runner reads this repo-local file.
- `flapjack.local.conf` can use either `KEY=value` or `export KEY=value` syntax (inline `# comments` are supported).
- Do not run multiple active test suites against the same backend port or same `data_dir`.
- If running multiple explicit server processes, use unique instance/data-dir per process.

## Quick Reference

| Type | Count | Time | Command |
|------|-------|------|---------|
| Rust unit (lib) | 2839 passed / 2839 total | ~50s | `./s/test --unit` |
| Rust integration | 797 passed / 797 total (7 skipped) | ~42s | `./s/test --integ` |
| Server binary | 25 | ~10s | `cargo test -p flapjack-server` |
| Rust smoke | 10 | ~3s | `cargo nextest run --profile smoke` |
| Dashboard unit | 542 passed / 542 total | ~8s | `cd dashboard && npm run test:unit:run` |
| Dashboard browser smoke | 12 executed (10 specs + seed/cleanup) | ~5s | `cd dashboard && npm run test:e2e-ui:smoke` |
| Dashboard browser full | 332 executed (330 specs + seed/cleanup) | ~2 min | `cd dashboard && npm run test:e2e-ui:full` |
| Official JS/Python clients + real clients (no Algolia) | API suites + Python journey + 3 rendered clients + 6 protocol smokes | ~25s | `./s/test --sdk` |
| Official JS client Algolia migration | 24 | ~3s | `./s/test --sdk-algolia` (test_algolia_migration.js) |
| Official JS client Algolia validation | 18 (+1 skip) | ~16s | `./s/test --sdk-algolia` (algolia_validation.js) |
| CLI smoke | 17 checks | ~10s | `./s/test --e2e` (real binary, curl-based) |

### Unified test runner

```bash
./s/test                  # default: unit + integ + server (~2.5 min)
./s/test --unit           # just cargo test --lib (~15s)
./s/test --integ          # integration tests via nextest (~2 min)
./s/test --server         # server binary tests (~10s)
./s/test --smoke          # nextest smoke profile (~3s)
./s/test --sdk            # official JS/Python + browser clients + protocol smokes
./s/test --sdk-algolia    # Algolia migration/validation; needs credentials (~15s)
./s/test --dashboard      # vitest unit + playwright smoke (~2.5 min)
./s/test --dashboard-full # + playwright full e2e (~5 min)
./s/test --e2e            # build + server + official JS/Python + browser + CLI
./s/test --all            # everything except --sdk-algolia (~20 min)
./s/test --ci             # unit + integ + server + dashboard (~5 min)
./s/test --list           # print all flags and exit
```

### Supported entrypoint

`./s/test` is the only maintained test-runner entrypoint.

---

## Run ALL Tests

```bash
# From engine/ directory:
cargo test --lib -p flapjack -p flapjack-replication && cargo nextest run -P ci -p flapjack-http --lib && cargo nextest run --no-fail-fast && cargo test -p flapjack-server
```

1. **Library tests** via `./s/test --unit`: 2,277 passing flapjack core tests (8 ignored), 137 passing replication tests, and 2,434 passing process-isolated HTTP tests (5 skipped) in the August 14 proof.
2. **Integration tests** via `./s/test --integ`: 951 selected by the default profile in the August 14 proof, with 7 skipped; `--no-fail-fast` reports the complete failure set.
3. **25 server binary tests** via `cargo test -p flapjack-server` (~10s wall-clock, 5 unit + 20 integration spawning real binary)

Wall-clock time is host-dependent; use the exact command receipts rather than the obsolete historical aggregate estimate.

### Why the lib/nextest split?

Nextest spawns a separate OS process per test. Each process pays startup overhead to load the flapjack binary (tantivy + arrow + parquet). The large `flapjack` core therefore stays in-process, while the `flapjack-http` library uses process-per-test isolation because it owns process-global migration state:

| Method | Ownership | Isolation |
|--------|-----------|-----------|
| `cargo test --lib -p flapjack -p flapjack-replication` | **Fast in-process core** | host-dependent |
| `cargo nextest run -P ci -p flapjack-http --lib` | **Isolated HTTP library tests** | host-dependent |

The default nextest integration profile excludes `kind(lib)`, so it does not duplicate either library owner. Always run both library commands alongside integration nextest.

---

## Test Organization

### Inline unit tests (`#[cfg(test)]` in source files)
Run via `./s/test --unit`. The August 14 proof executed 2,277 flapjack core tests and 137 replication tests in-process, then 2,434 flapjack-http tests under process-per-test nextest isolation; 8 core tests were ignored and 5 HTTP tests were skipped under their explicit tier owners.
Note: `flapjack-server` has no lib target — do not include it in the count.

### Integration tests (`engine/tests/*.rs`)
951 tests were selected in the August 14 default-profile proof, with 7 skipped. Test HTTP endpoints, auth, replication, cross-crate integration. Run via `./s/test --integ` (which uses nextest with `--no-fail-fast`). Filtered by name suffix:

- **No suffix**: Default. Run every time.
- **`_slow` suffix**: Excluded from default profile. Performance benchmarks, stress tests.
- **`_very_slow` suffix**: Nightly/pre-release only.

### Server binary tests (`flapjack-server/tests/` + `src/main.rs`)
25 tests total: 5 inline unit tests in `main.rs` (port derivation, instance name validation, auto-port flag logic) + 20 integration tests in `env_mode_test.rs` (spawn real `flapjack` binary, test startup modes: production key validation, development auto-keygen, key persistence across restarts, key rotation via env var, `reset-admin-key`, `--no-auth` mode, multi-instance isolation, auto-port, process lock). Run via `cargo test -p flapjack-server`. Integration tests NOT included in nextest (requires compiled binary).

### Dashboard tests
- **Component tests** (`dashboard/src/**/*.test.tsx` + `local-instance-config.test.ts`): Vitest + React Testing Library, 542 tests, ~8s
- **Browser smoke** (`dashboard/tests/e2e-ui/smoke/`): Playwright, real server, 10 spec tests per run (12 executed including seed/cleanup), every push
- **Browser full** (`dashboard/tests/e2e-ui/full/`): Playwright, 330 spec tests across 32 full-browser spec files; `npm run test:e2e-ui:full -- --list` currently enumerates 332 executed tests in 34 files because the `e2e-ui` project depends on `seed` and tears down with `cleanup`
- **API shape** (`dashboard/tests/e2e-api/`): Playwright HTTP-only, main only

**CRITICAL: All browser tests must follow `dashboard/BROWSER_TESTING_STANDARDS_2.md`.** This covers locator rules, arrange/act/assert separation, ESLint enforcement, and banned patterns. Read it before writing or modifying any UI test.

Dashboard testing is covered by the browser smoke/full suites and the HTTP-only API shape suite listed above, with standards enforced by `dashboard/BROWSER_TESTING_STANDARDS_2.md`.

### Client compatibility tests
- **Official JavaScript client tests** (`sdk_test/`): Uses the `algoliasearch` npm package against a running server. Run via `./s/test --sdk` (test.js + contract_tests.js) or `./s/test --sdk-algolia` (needs Algolia credentials).
- **Official Python client proof** (`sdk_test/python_client_contract_test.sh`): CPython 3.12 synchronous settings/batch/task/search/facet/delete journey with exact shared-fixture assertions. Runs once in `--sdk`, `--e2e`, `--all`, or `--sdk --e2e`; the existing `sdk-contract` CI job selects Python 3.12 and invokes the same bootstrap. `requirements-python-client.txt` owns its test-only package pin; `FLAPJACK_SDK_PYTHON` selects the interpreter.
- **Real client browser tests** (`sdk_test/browser_tests_unmocked/`): Chromium renders the official InstantSearch.js, React InstantSearch, and Vue InstantSearch packages against the real server. The three clients share one dataset and exact query/facet/pagination assertions. Run via `./s/test --sdk` or `npm run test:real_clients` with a server already running.
- **Test bootstrap:** `engine/sdk_test` is self-contained with its own `package.json`; bootstrap runs local `npm ci` instead of depending on `dashboard/node_modules`.
- **Protocol smokes:** PHP, Python, Ruby, Go, Java, and Swift retain separate curl-based request/response checks under `sdk_test/`; `./s/test --sdk` runs all six. E2E modes omit them. The Python smoke is distinct from the official-client proof.

---

## Workflows

### Smoke (~3s)
```bash
cargo smoke
# or: s/smoke.sh  |  cargo nextest run --profile smoke
```
Runs `test_smoke.rs` — 10 tests covering every workspace crate (library, parser, auth, HTTP, replication, SSL, memory, CORS).

### Development (TDD)
```bash
cargo test --lib -p flapjack -p flapjack-replication  # fast in-process core
cargo nextest run -P ci -p flapjack-http --lib       # process-isolated HTTP library
cargo nextest run --no-fail-fast                     # process-isolated integration tests
```

### Parallel clone execution (same machine)
Use different `flapjack.local.conf` values in each clone so ports do not collide.
Each clone should run its own backend and dashboard test stack.

### Pre-commit
```bash
cargo test --lib -p flapjack -p flapjack-replication && cargo nextest run -P ci -p flapjack-http --lib && cargo nextest run --no-fail-fast && cargo test -p flapjack-server
```

### CI
```bash
cargo test --lib -p flapjack -p flapjack-replication && cargo nextest run -P ci -p flapjack-http --lib && cargo nextest run --no-fail-fast && cargo test -p flapjack-server  # every commit
cargo nextest run -P slow                                            # PR/merge queue
cargo nextest run -P very_slow                                       # nightly
cargo test --lib -p flapjack -p flapjack-http && cargo nextest run -P ci && cargo test -p flapjack-server  # release
```

### Selective
```bash
cargo nextest run --test test_smoke          # specific file
cargo nextest run filter                      # pattern match
cargo nextest list                            # list without running
cargo test --lib -p flapjack -- filter::tests # specific inline module
```

### Migration scale proof
Prerequisites: `gtimeout`, `jq`, a built local Flapjack binary or local build support, and the source-repo secret file at `/Users/stuart/repos/gridl-dev/flapjack_dev/engine/.secret/.env.secret`.

```bash
unset FJ_SCALE_REUSE_FIXTURE
(cd engine && MIGRATION_IMPORT_CONTRACT_EVIDENCE_ROOT="$PWD/../build/scale-evidence" gtimeout --kill-after=60s 5400s bash tests/migration_import_contract.sh --expect-mode scale --two-point --secret-file /Users/stuart/repos/gridl-dev/flapjack_dev/engine/.secret/.env.secret)
```

Validate the newly preserved receipt, not stdout:

```bash
receipt="$(ls -1t build/scale-evidence/*/receipt.json | head -1)"
jq -e '.status == "pass" and .scale.mode == "two-point" and .scale.conditions == [2000,20000] and (.checks | length) > 0 and all(.checks[]; .status != "fail")' "$receipt"
```

The public mirror job is opt-in on public mirrors. The local command is authoritative acceptance for this dev lane, and no dev-repo Actions run should be triggered.

### Multi-Language Fast Suite
Use `--lib` for focused multi-language feature checks so we only execute inline/unit-style targets and avoid spawning unrelated integration binaries with zero tests.

```bash
# Language parsing + aliases (queryLanguages/indexLanguages foundation)
cargo test -p flapjack --lib -- language::tests::

# Multi-language stopwords (includes pt-br alias routing)
cargo test -p flapjack --lib -- query::stopwords::tests::
cargo test -p flapjack --lib -- integ_tests::test_query::multilang_stopwords::

# Multi-language plurals
cargo test -p flapjack --lib -- query::plurals::tests::
cargo test -p flapjack --lib -- integ_tests::test_query::multilang_plurals::

# Decompound behavior + decompoundQuery toggle
cargo test -p flapjack --lib -- query::decompound::tests::
cargo test -p flapjack --lib -- integ_tests::test_query::decompound::decompound_query_false_disables_compound_splitting

# indexLanguages persistence + tokenizer wiring
cargo test -p flapjack --lib -- index::settings::tests::test_index_languages
cargo test -p flapjack --lib -- integ_tests::test_query::index_languages_roundtrip::
cargo test -p flapjack --lib -- integ_tests::test_query::index_languages_tokenizer_wiring::

# decompoundQuery HTTP DTO
cargo test -p flapjack-http -- decompound
```

### Dictionaries Focused Suite
Run focused dictionary tests only. Do not run a full project test sweep for dictionary work.

```bash
# Dictionary manager/storage/query-pipeline unit tests
cargo test -p flapjack -- dictionaries

# Dictionary HTTP handler validation tests (no spawned server)
cargo test -p flapjack-http -- handlers::dictionaries::tests

# Search pipeline dictionary wiring (IndexManager layer)
cargo test -p flapjack --lib -- index::manager::tests::test_custom_

# Dictionaries HTTP integration binary compile check (compile-only)
cargo test -p flapjack --test test_dictionaries --no-run

# Dictionaries HTTP integration execution (requires environment that permits binding localhost sockets)
cargo nextest run -p flapjack --test test_dictionaries
```

---

## Guidelines

### CRITICAL: Always Use IndexManager for Tests

Every write/search test MUST use IndexManager, never Index directly.

- Matches production code path (HTTP API → IndexManager)
- Guarantees reader visibility (auto-reloads before search)
- Validates async write queue behavior

```rust
#[tokio::test]
async fn test_write_then_search() -> Result<()> {
    let temp_dir = TempDir::new()?;
    let manager = IndexManager::new(temp_dir.path());
    manager.create_tenant("tenant", schema)?;
    manager.add_documents("tenant", docs).await?;
    let results = manager.search("tenant", "query", None, None, 10)?;
    assert_eq!(results.total, expected_count);
    Ok(())
}
```

**Anti-pattern** (causes 0-result failures):
```rust
let index = Index::create(path, schema)?;
let mut writer = index.writer()?;
writer.commit()?;
let reader = index.reader();  // No reload, stale
```

If you MUST test `Index` directly, call `reader.reload()` after commit.

### Prefer IndexManager Over spawn_server

Only use `spawn_server()` for behavior implemented in flapjack-http (CORS, auth headers, response format, geo). Everything else should call IndexManager directly — server startup adds ~5s per test.

### Integration test isolation

**All integration tests run via nextest** (process-per-test isolation). Each test
calls `spawn_server()` directly to get its own isolated server instance.

**Do NOT run with `cargo test --test <file>`** — use `cargo nextest run --test <file>`
or `./s/test --integ`.

### Adding New Tests

- **Pure logic** → `#[cfg(test)]` in the same source file. Essentially free to run.
- **IndexManager integration** → add to an existing file in `engine/tests/` or `src/integ_tests/`. Do NOT create new test binaries — each one adds a link step.
- **HTTP integration** → add to the relevant existing test file in `engine/tests/`.

### Test Harness Routes

`spawn_server()` builds its own Axum router separate from production. When adding new HTTP routes, you **must also add them to `tests/common/mod.rs`** or integration tests will get 404.

### No Blind Sleeps

Use task-polling helpers from `tests/common/mod.rs`:
- `wait_for_task(client, addr, task_id)` — polls `/1/tasks/{id}` at 10ms intervals
- `wait_for_response_task(client, addr, resp)` — extracts taskID from response, then polls
- Server startup uses health endpoint polling, not `sleep()`

---

## Configuration

`.config/nextest.toml`:
```toml
[profile.default]
default-filter = "not (test(/_slow$/) or test(/_very_slow$/) or kind(lib))"

[profile.slow]
default-filter = "test(/_slow$/) and not test(/_very_slow$/)"

[profile.very_slow]
default-filter = "test(/_very_slow$/)"

[profile.smoke]
default-filter = "binary(test_smoke)"

[profile.ci]
default-filter = "all()"
```

---

## Client compatibility tests (`sdk_test/`)

JavaScript and synchronous Python tests using the official `algoliasearch` clients against Flapjack. The Python journey and browser clients share `fixtures/official_client_contract.json`.

### Automated (wired into `./s/test --sdk`)

| Script | Purpose | Tests | Algolia? |
|--------|---------|-------|----------|
| `test.js` | Basic SDK ops (settings, batch, search, filters, facets) | 8 | No |
| `python_client_contract_test.sh` | Official synchronous Python settings/batch/tasks, exact search/facets, cleanup | 1 journey | No |
| `python_smoke_test.sh` | Separate curl-based Python protocol smoke | 1 smoke | No |
| `contract_tests.js` | Full API contract validation (CRUD, browse, delete-by, multi-index) | 24 | No |
| `browser_tests_unmocked/real_client_conformance.spec.mjs` | Official vanilla, React, and Vue rendered clients | 3 | No |

### Requires Algolia creds (`./s/test --sdk-algolia`)

| Script | Purpose | Tests | Algolia? |
|--------|---------|-------|----------|
| `test_algolia_migration.js` | Full migration from Algolia to Flapjack (35 tests, 10 phases) | 35 | Yes |
| `algolia_validation.js` | Response-for-response comparison vs live Algolia (4 suites) | 15 | Yes |

### Debug utilities (archived to `sdk_test/_archive/`, not automated)

| Script | Notes |
|--------|-------|
| `race_test.js` | 17-line quick check, not a test suite |
| `test_exhaustive_fields.js` | Debug utility, needs Algolia, no assertions |
| `test_algolia_multi_pin.js` | Debug/exploratory, needs Algolia |

The live client journeys require a running server (`./s/test --sdk` handles this automatically).
The `sdk-contract` CI job runs the official Python shell entrypoint after pinned Python 3.12 setup, alongside the existing JS/browser gates. Its fast regressions run without a server: `npm run test:runner-shell` includes the Python bootstrap tests, `npm run test:python-client:unit` owns the Python unit selection, and `npm run test:real_clients:wiring` checks fixture and job-scoped wiring. This describes the configured gates; landed public-main acceptance remains a separate release check.

## Manual Tests

| Script | Requires | Tests | Automated? |
|--------|----------|-------|------------|
| `cli_smoke.sh` | Server (auto-starts) | 17 curl-based checks: health, CRUD, search, settings, filters, facets, multi-index | **Yes** — via `./s/test --e2e` |
| `test_s3.sh` | Server + AWS creds + S3 | Snapshot upload, list, delete+restore, retention | No |
| `test_s3_backup_restore.sh` | Server + AWS creds + S3 | Full DR pipeline | No |
| `perf_search.sh` | Server with data | 11 query scenarios, timing benchmarks | No |
| HA confidence test | Docker (3-node cluster) | 124+ assertions: replication, failover, catch-up, document identity verification, search result + ranking symmetry, analytics fan-out, rate merges, HLL dedup, partial failure, LWW, concurrent writes, double failover (+10 latency-specific with `--latency`) | No |

---

## CI Baselines and Execution Tiers

The workflow YAML remains execution truth. `engine/tests/ci_test_tiers.json` is the machine-readable ownership manifest: `test_ci_test_tiers.py` fails public CI when a workflow job, risk class, or intentional Rust ignore has no explicit minimum tier. This table is the operator-facing summary rather than a second executable definition.

| Surface | Minimum tier | Executable owner |
|---------|--------------|------------------|
| Rust core search/index and durability | Candidate | `ci.yml` → `rust-tests-fast`; prebuild then bounded `cargo test -p flapjack --lib` |
| Authentication, index boundary, API compatibility, startup/wiring | Public fast | `ci.yml` → `integration-smoke` |
| Rust HTTP, vector, server, TLS, and replication | Public complete | `ci.yml` → `rust-tests-all`, using nextest process isolation |
| Source quality and test-harness contracts | Public fast | `ci.yml` → `fmt`, `clippy`, and `release-contracts` |
| Dashboard and client compatibility | Public fast | `ci.yml` dashboard jobs and the `sdk-contract` official-client gate |
| Installer and migration oracles | Nightly | `nightly.yml` installer and migration jobs |
| Comprehensive default-feature Rust/dashboard/integration | Nightly | `nightly.yml` comprehensive job families |
| Process-global leak detection | Union | `union.yml` → `rust-in-process-union`; never substitute nextest |
| Packaging, images, and release preflights | Release | manually dispatched `release.yml` and development-image `docker.yml`; never part of candidate feedback |
| Twenty long-running or live-system ignored Rust evidence harnesses | Manual evidence | exact source/name allowlist in `ci_test_tiers.json` |

### Historical baseline from the former pre-candidate topology

These successful first-attempt runs are retained only as performance history from the former public-staging topology. Current acceptance runs on the rendered `public-candidate/**` branch in the sole public product repository and must pass `Public candidate gate`; recurring workflows run on public main. Durations below are run/job wall clock from GitHub timestamps, so compare jobs only within this historical workflow family.

| Run | Workflow | Rust fast | Rust all | Integration smoke | Release contracts |
|-----|----------|-----------|----------|-------------------|-------------------|
| `31682605708` | 24m27s | 24m17s | 22m18s | 18s | 35s |
| `31674400217` | 26m02s | 25m55s | 23m43s | 15s | 38s |
| `31664714337` | 25m41s | 25m34s | 24m07s | 18s | 40s |
| `31658983027` | 26m14s | 25m59s | 23m34s | 19s | 33s |
| `31657261952` | 26m12s | 25m58s | 25m38s | 19s | 35s |
| `31245623596` | 25m51s | 25m40s | 23m10s | 19s | 27s |

Median Rust-fast wall clock was 25m47s. In run `31682605708`, the vector prebuild compiled for 3m11s with step-local `RUSTFLAGS=-C debuginfo=0 -C strip=debuginfo`; nextest then returned to the job profile `-C debuginfo=0`, recompiled for 9m53s, and executed 962 tests in only 62.397s. The complete job showed the same fingerprint mismatch: a 2m53s prebuild, a 9m04s nextest compile, then 5,847 tests in 159.700s. The repaired workflow keeps compilation before the timeout but gives prebuild and nextest one job-level compilation identity.

### Candidate contract and local measurements

The candidate deliberately excludes the two unsafe broad library unions. The vector-feature `flapjack + flapjack-http` library specimen ran 2,434 tests but failed from cross-test vector-save observation interference; the exact test passed alone. The default-feature workspace library specimen ran 2,433 HTTP tests but failed from migration process-state interference; that exact test also passed alone. Both surfaces retain nextest and union owners.

On an uncontended Apple Silicon host with an absent worktree target directory, `cargo test -p flapjack --lib --no-run` took 161.22s. Reusing that artifact, the bounded candidate took 145.64s and passed 2,277 tests with the eight manifest-classified manual-evidence ignores. A focused durability filter completed in 6.89s. Cold compilation and warm behavior are reported separately; the build exports the exact Cargo test executable and the behavioral step runs that artifact directly under a 300-second cap, so Cargo cannot recompile inside the behavior budget and individual test results stream immediately.

---

## Strategy Overview

| Layer | Command | Tests | Time | Purpose |
|-------|---------|-------|------|---------|
| Rust libraries | `./s/test --unit` | Core + HTTP + replication libraries | Host-dependent | Fast core runs in-process; process-global HTTP tests run under isolated nextest `ci` |
| Integration | `./s/test --integ` | 797 (7 skipped) | ~42s | HTTP, auth, replication, geo |
| Server binary | `cargo test -p flapjack-server` | 25 | ~10s | Startup modes, key management, multi-instance |
| Dashboard unit | `npm run test:unit:run` | 542 | ~8s | React components, hooks, config parser |
| Dashboard browser smoke | `npm run test:e2e-ui:smoke` | 12 executed (10 specs + seed/cleanup) | ~5s | Critical user paths |
| Official JS/Python clients + real clients | `./s/test --sdk` | API suites + Python journey + 3 rendered clients + 6 protocol smokes | ~25s | API contracts and official-client browser behavior |
| CLI smoke | `./s/test --e2e` | 17 | ~10s | Real binary curl tests |
| Client migration | `node test_algolia_migration.js` | 24 | ~3s | Algolia drop-in proof |
| Client validation | `node algolia_validation.js` | 18 (+1 skip) | ~16s | Response accuracy vs Algolia |

**Recommended workflow:**
1. `./s/test --smoke` — quick sanity check (~3s)
2. `./s/test --unit` — after any code change (~15s)
3. `./s/test` — before push (unit + integ + server, ~2 min)
4. `./s/test --sdk --e2e` — after HTTP/API changes (~15s)
5. `./s/test --all` — full validation (~20 min)

---

## Known Gaps

1. **Geo tests stuck in HTTP tier** — geo filtering/sorting logic lives in `flapjack-http/src/handlers/search.rs`. To convert to lib tests, extract geo logic into the library first.
2. **5 cross-crate test files stuck in nextest** — they import `flapjack_http` types, causing circular deps when compiled inline. Fix: extract needed types (SearchRequest, parse_filter) into flapjack crate. (43 facet tests moved to lib in session 007, 5 redundant parse_facet_params tests deduplicated in session 008, reducing nextest count from 376 → 321, then to 324 after 3 periodic sync tests added in session 31 review; current baseline integration total is 797 as of 2026-03-17.)
3. **~14 engine/src/ modules tightly coupled to I/O/async** — manager.rs, collector.rs, writer.rs, s3.rs, etc. Not unit-testable without major refactoring.
4. **flapjack-http untested pure functions** — extractable logic in insights.rs, tasks.rs, dashboard.rs, memory_middleware.rs.
5. **HA replication trigger coverage** — `add_record_auto_id`, `partial_update_object`, `delete_by_query` now call `trigger_replication()` (session 23), but no integration test verifies replication is actually triggered by these endpoints (low priority — code paths are simple and similar to the covered `put_object`/`delete_object` paths).
6. ~~**No real two-node E2E test**~~ — **ADDRESSED**: The HA confidence test runs a full 3-node Docker cluster with 124+ assertions covering replication, failover, catch-up, document identity verification (IDs + content + ranking order across nodes), analytics fan-out, rate merges, HLL dedup, LWW conflict resolution, concurrent writes, and double failover. A cross-region latency stress variant adds ~10 latency-specific assertions with tc/netem. **P0 periodic anti-entropy sync implemented (session 31)** with 3 Rust integration tests. Remaining gap: Docker-based iptables partition test (Phase 18) for full partition→heal→converge validation.
8. **QS `trigger_build` 409-while-running untested** — `POST /1/configs/:name/build` returns 409 if a build is already running. Now also covered indirectly by `update_config_while_building_returns_409`. Direct mid-build trigger_build interception remains timing-dependent, low risk.

## Known Flaky Tests

- ~~**`test_index_ops oplog_replay::replay_interleaved_adds_and_deletes`**~~ — **FIXED (session 29)**. Root cause was `IndexManager::Drop` not aborting background write tasks; detached tokio tasks ran past drop, racing with parallel tests. Fixed by implementing `Drop for IndexManager` that calls `.abort()` on all `write_task_handles`.
- ~~**`config::tests::test_load_or_default_no_file` / `test_load_or_default_invalid_json`**~~ — **FIXED (session 008)**. Root cause was an absent `ENV_MUTEX` guard: these tests read `FLAPJACK_PEERS` env var but ran concurrently with tests that set it. Fixed by adding `ENV_MUTEX` lock + explicit `remove_var` cleanup.
