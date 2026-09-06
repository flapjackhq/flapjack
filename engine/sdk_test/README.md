# SDK & Migration Tests

Tests of Flapjack's Algolia API compatibility using official clients against Flapjack.
Only the explicitly labeled Algolia comparison and migration scripts contact Algolia.

**Prerequisites:** Flapjack running on `FLAPJACK_URL` (default `http://localhost:7700`).
The Algolia comparison/migration scripts additionally require `ALGOLIA_APP_ID` and
`ALGOLIA_ADMIN_KEY` in `.secret/.env.secret`.

## Official synchronous Python client

`python_client_contract_test.sh` bootstraps the test environment and runs
`python_client_contract_test.py` using the official `algoliasearch` client. The bounded
journey creates a unique index, applies settings, batches records, waits for tasks,
checks exact search hits and facet values, then deletes the index and closes the client.
It uses only the configured Flapjack origin.
`FLAPJACK_ADMIN_KEY` is required. Plain HTTP is accepted only for a loopback
origin; use HTTPS for any remote test server so the key is not sent in cleartext.

Install CPython 3.12 first; the bootstrap selects `python3.12` or the executable in
`FLAPJACK_SDK_PYTHON`. From `engine/sdk_test`, with a server running:

```bash
FLAPJACK_URL=http://localhost:7700 FLAPJACK_ADMIN_KEY=your-local-test-key \
  bash python_client_contract_test.sh
```

The bootstrap manages its own virtual environment under `.cache/`; the test-only
`requirements-python-client.txt` owns the official package pin. Both Python and the
browser adapter consume `fixtures/official_client_contract.json` as their shared oracle.

From `engine/`, `./s/test --sdk`, `./s/test --e2e`, `./s/test --all`, and
`./s/test --sdk --e2e` run the labeled official Python proof exactly once against the
managed server. `--sdk` also retains the separate curl-based `python_smoke_test.sh`;
E2E modes subsume SDK client coverage and omit the protocol smokes.

The existing `sdk-contract` CI job selects Python 3.12 and invokes the same shell
entrypoint. Its recurring regressions are `npm run test:runner-shell`,
`npm run test:python-client:unit` (no server needed), and the existing
`npm run test:real_clients:wiring`.

## Real InstantSearch clients

`browser_tests_unmocked/` renders the official vanilla, React, and Vue InstantSearch
packages in Chromium against the real Flapjack server through `algoliasearch/lite`. One
shared fixture gives every interaction a different expected result, proving exact initial
hits, query refinement, facet refinement, and pagination for every client. The browser
receives a temporary search-only key restricted to the fixture index; the administrative
key remains in the setup runner.

```bash
npm run test:real_clients
```

`./s/test --sdk` owns normal local execution. Public mirror CI runs the same browser suite
in the `SDK contract tests` job.

## Critical Tests

### `test_algolia_migration.js` — Algolia Migration (MOST IMPORTANT)

Proves a real customer can migrate from Algolia to Flapjack. Tests both migration paths:

- **Manual migration** (Phase 3/4): Export settings/synonyms/objects from Algolia, import into Flapjack via individual API calls, compare search results.
- **One-click migration** (Phase 3b/4b): exercises the `POST /1/migrate-from-algolia` endpoint. The create-only path is available on `main`: it exports a source index, imports into a fresh target index, and verifies the migrated search result. Existing-target overwrite, async status/cancel/resume, and HA-converging import remain deferred. See the canonical status in [`FEATURES.md`](../docs2/FEATURES.md#algolia-migration-1migrate-from-algolia--create-only-shipped).

```bash
node test_algolia_migration.js           # run full migration test
node test_algolia_migration.js --verbose # with detailed output
```

### `algolia_validation.js` — SDK Compatibility

Compares live Algolia responses against Flapjack using cached golden files. 15 test cases across 4 suites covering search, highlighting, filters, facets, and pagination.

```bash
node algolia_validation.js               # all tests with cache
node algolia_validation.js highlighting  # specific suite
node algolia_validation.js --no-cache    # force fresh API hits
node algolia_validation.js --verbose     # show detailed diffs
node algolia_validation.js --cleanup     # delete test indices
```

### `contract_tests.js` — API Contract Tests

Validates Flapjack API endpoint contracts (request/response shapes, status codes).

## Other Files

| File | Purpose |
|------|---------|
| `test_algolia_multi_pin.js` | Tests rules with multiple pin/hide operations |
| `test_exhaustive_fields.js` | Tests field type handling edge cases |
| `test_v4_simple.js` | Basic SDK v4 compatibility |
| `race_test.js` | Concurrent write/read race condition testing |
| `debug_search.js` | Manual search debugging utility |
| `audit_algolia_defaults.js` | Audits Algolia default settings |
| `TEST_COVERAGE.md` | Validation test coverage matrix |

## One-Click Migration Endpoint

```
POST /1/migrate-from-algolia
{
  "appId": "YOUR_ALGOLIA_APP_ID",
  "apiKey": "YOUR_ALGOLIA_ADMIN_KEY",
  "sourceIndex": "products",
  "targetIndex": "products"   // optional, defaults to sourceIndex
}
```

**Create-only migration is available on `main`.** This endpoint migrates an Algolia index (settings, synonyms, rules, objects) into a fresh Flapjack target index. Existing-target overwrite returns 409 until `MIG-5` lands; async status/cancel/resume waits on `MIG-6`; HA-converging import remains refused under `MIG-7`. See the canonical status in [`FEATURES.md`](../docs2/FEATURES.md#algolia-migration-1migrate-from-algolia--create-only-shipped).
