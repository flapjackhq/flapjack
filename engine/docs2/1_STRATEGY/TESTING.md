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
| JS SDK (no Algolia) | 32 | ~8s | `./s/test --sdk` (test.js + contract_tests.js) |
| JS SDK Algolia migration | 24 | ~3s | `./s/test --sdk-algolia` (test_algolia_migration.js) |
| JS SDK Algolia validation | 18 (+1 skip) | ~16s | `./s/test --sdk-algolia` (algolia_validation.js) |
| Go SDK | ~3 | ~5s | `cd ../sdks/go && go test ./flapjack/...` |
| CLI smoke | 17 checks | ~10s | `./s/test --e2e` (real binary, curl-based) |

### Unified test runner

```bash
./s/test                  # default: unit + integ + server (~2.5 min)
./s/test --unit           # just cargo test --lib (~15s)
./s/test --integ          # integration tests via nextest (~2 min)
./s/test --server         # server binary tests (~10s)
./s/test --smoke          # nextest smoke profile (~3s)
./s/test --sdk            # JS SDK: test.js + contract_tests.js (~8s)
./s/test --sdk-algolia    # JS SDK: needs Algolia creds (~15s)
./s/test --dashboard      # vitest unit + playwright smoke (~2.5 min)
./s/test --dashboard-full # + playwright full e2e (~5 min)
./s/test --e2e            # build binary + server + sdk + cli smoke (~30s)
./s/test --go             # Go SDK unit tests (~5s)
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
cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication && cargo nextest run && cargo test -p flapjack-server
```

1. **2839 inline unit tests** via `./s/test --unit` (1530 flapjack + 1276 flapjack-http + 33 flapjack-replication, ~49s wall-clock in latest run)
2. **797 integration tests** via `./s/test --integ` (~42s wall-clock in latest run; 7 skipped in summary output)
3. **25 server binary tests** via `cargo test -p flapjack-server` (~10s wall-clock, 5 unit + 20 integration spawning real binary)

**Total: ~101s wall-clock, ~3661 tests (plus 7 skipped integration cases).**

### Why the lib/nextest split?

Nextest spawns a separate OS process per test. Each process pays ~1.5s to load the flapjack binary (tantivy + arrow + parquet). For inline unit tests that take microseconds, this is pure waste:

| Method | 2839 lib tests | Per-test |
|--------|----------------|----------|
| `./s/test --unit` (threads) | **~49s** | 0.017s |
| `cargo nextest run` (processes) | **much slower for micro-tests due process startup overhead** | varies |

The nextest config (`kind(lib)` filter) automatically excludes lib tests. Always run `cargo test --lib` alongside nextest.

---

## Test Organization

### Inline unit tests (`#[cfg(test)]` in source files)
2839 tests in the latest baseline run. Test pure functions in isolation. Run via `./s/test --unit`.
Latest crate split: 1530 in flapjack, 1276 in flapjack-http, 33 in flapjack-replication.
Note: `flapjack-server` has no lib target — do not include it in the count.

### Integration tests (`engine/tests/*.rs`)
797 tests in the latest baseline run (default profile summary), with 7 skipped. Test HTTP endpoints, auth, replication, cross-crate integration. Run via `./s/test --integ` (which uses nextest under the hood). Filtered by name suffix:

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

### SDK tests
- **JS SDK tests** (`sdk_test/`): Uses `algoliasearch` npm package against running server. Run via `./s/test --sdk` (test.js + contract_tests.js) or `./s/test --sdk-algolia` (needs Algolia creds).
- **SDK bootstrap decoupling:** `engine/sdk_test` is self-contained with its own `package.json`; bootstrap now runs local `npm ci` in `sdk_test` instead of depending on `dashboard/node_modules`.
- **Go SDK** (`sdks/go/`): Unit tests, run via `./s/test --go`.
- **Other SDKs** (PHP, Python, Ruby, Java, C#): CI only, require their runtimes. A legacy aggregate runner covers Go + PHP only.
- **Note:** The legacy aggregate runner does NOT run JS SDK tests. Use `./s/test --sdk` for that.

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
cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication  # ~15s — unit tests
cargo nextest run                                 # ~2 min — integration tests
```

### Parallel clone execution (same machine)
Use different `flapjack.local.conf` values in each clone so ports do not collide.
Each clone should run its own backend and dashboard test stack.

### Pre-commit
```bash
cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication && cargo nextest run && cargo test -p flapjack-server
```

### CI
```bash
cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication && cargo nextest run && cargo test -p flapjack-server  # every commit
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

## SDK Tests (`sdk_test/`)

JavaScript tests using the official `algoliasearch` npm package against a running server.

### Automated (wired into `./s/test --sdk`)

| Script | Purpose | Tests | Algolia? |
|--------|---------|-------|----------|
| `test.js` | Basic SDK ops (settings, batch, search, filters, facets) | 8 | No |
| `contract_tests.js` | Full API contract validation (CRUD, browse, delete-by, multi-index) | 24 | No |

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

All automated SDK tests require a running server (`./s/test --sdk` handles this automatically).

## Manual Tests

| Script | Requires | Tests | Automated? |
|--------|----------|-------|------------|
| `cli_smoke.sh` | Server (auto-starts) | 17 curl-based checks: health, CRUD, search, settings, filters, facets, multi-index | **Yes** — via `./s/test --e2e` |
| `test_s3.sh` | Server + AWS creds + S3 | Snapshot upload, list, delete+restore, retention | No |
| `test_s3_backup_restore.sh` | Server + AWS creds + S3 | Full DR pipeline | No |
| `perf_search.sh` | Server with data | 11 query scenarios, timing benchmarks | No |
| HA confidence test | Docker (3-node cluster) | 124+ assertions: replication, failover, catch-up, document identity verification, search result + ranking symmetry, analytics fan-out, rate merges, HLL dedup, partial failure, LWW, concurrent writes, double failover (+10 latency-specific with `--latency`) | No |

---

## Strategy Overview

| Layer | Command | Tests | Time | Purpose |
|-------|---------|-------|------|---------|
| Inline unit | `./s/test --unit` | 2839 | ~50s | Pure function correctness |
| Integration | `./s/test --integ` | 797 (7 skipped) | ~42s | HTTP, auth, replication, geo |
| Server binary | `cargo test -p flapjack-server` | 25 | ~10s | Startup modes, key management, multi-instance |
| Dashboard unit | `npm run test:unit:run` | 542 | ~8s | React components, hooks, config parser |
| Dashboard browser smoke | `npm run test:e2e-ui:smoke` | 12 executed (10 specs + seed/cleanup) | ~5s | Critical user paths |
| JS SDK | `./s/test --sdk` | 32 | ~8s | API contract validation |
| CLI smoke | `./s/test --e2e` | 17 | ~10s | Real binary curl tests |
| SDK migration | `node test_algolia_migration.js` | 24 | ~3s | Algolia drop-in proof |
| SDK validation | `node algolia_validation.js` | 18 (+1 skip) | ~16s | Response accuracy vs Algolia |

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
