# Real InstantSearch client conformance receipt

Date: 2026-08-12
Base revision: `e66c4fee07e8a6404354660b155f617b276d2e95`

## Purpose

Prove that the official vanilla, React, and Vue InstantSearch packages render and operate
against a real Flapjack listener. API-shape tests alone are not accepted as client proof.

## Scope

- InstantSearch.js `4.111.0`
- React InstantSearch `7.44.0` on React `18.3.1`
- Vue InstantSearch `4.29.2` on Vue `3.5.41`
- `algoliasearch/lite` `5.49.2`
- Chromium using Playwright `1.62.1`
- Exact initial hits, query refinement, facet refinement, and pagination
- A temporary `search` key restricted to the fixture index

Angular, mobile InstantSearch libraries, Autocomplete UI, SSR, and widgets outside this
shared interaction set are not covered by this receipt.

## RED evidence

The initial recurring-gate contract failed with exit `1` because `@playwright/test` and the
three official client packages were absent. After the clients were mounted, the first live
browser run failed `3/3`: every client received `403 Invalid Application-ID or API key`.
The trace showed the official browser transport sending `x-algolia-application-id` in the
query string while Flapjack read that value only from a header.

The focused engine regression test then failed before the repair:

```text
cargo test -p flapjack-http --lib -- auth::tests::application_id_accepts_official_browser_query_transport
left: None
right: Some("browser-app")
```

## Repair

`auth::request_application_id` now accepts the official browser query transport, decodes
the value, and keeps a non-empty header authoritative when both forms are present. API-key
transport policy is unchanged: privileged operational routes continue to reject URL-borne
API keys.

The browser fixture configures the official `algoliasearch/lite` package through its
`hosts` option. It does not rewrite credential transport or mock server responses. The
full client and administrative key seed the fixture and create a random index-scoped
search key; only that temporary key reaches browser JavaScript.

## GREEN evidence

```text
cargo test -p flapjack-http --lib -- auth::tests::application_id_accepts_official_browser_query_transport
1 passed; 0 failed

npm run test:real_clients
vanilla ... passed
react ... passed
vue ... passed
3 passed
```

Each browser case asserted the exact visible names for:

- Initial page: `Alpha Laptop`, `Gamma Phone`
- Query `laptop`: `Alpha Laptop`, `Beta Laptop`
- Facet `Nova`: `Gamma Phone`, `Delta Phone`
- Page 2: `Beta Laptop`, `Delta Phone`

The four expected lists are pairwise different. The recurring wiring test enforces that
property, so ignoring the query, facet, or page transition makes the visible-name assertion
red. Before the fixture order was repaired, all three clients failed with the same exact
diagnostic:

`Expected ["Alpha Laptop", "Gamma Phone"]; received ["Alpha Laptop", "Beta Laptop"]`.
This replaces the earlier test-only mutation branches, which duplicated behavior across
three otherwise faithful example applications without testing query, facet, or pagination.

## Wider regression validation

```text
./engine/_dev/s/test --sdk
API suites, 3 rendered clients, and 6 protocol smokes passed in 24s

cargo test -p flapjack-http --no-fail-fast
2417 library tests passed; 0 failed; 5 ignored
all additional binary and integration targets passed

cargo clippy -p flapjack-http --all-targets -- -D warnings
PASS
```

## Recurring gate

- Local owner: `./s/test --sdk`
- Public mirror owner: `.github/workflows/ci.yml`, job `SDK contract tests`
- Structural owner: `engine/sdk_test/tests/real_client_conformance_wiring_test.mjs`

The runner allocates a loopback port, owns one unique index and one temporary scoped key,
removes both in its `finally` block, and terminates only the process group it created.
Post-run process checks found no scoped Vite, Playwright, Chromium, or Flapjack process.
