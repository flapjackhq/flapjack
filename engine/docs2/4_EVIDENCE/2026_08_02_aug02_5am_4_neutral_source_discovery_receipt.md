# Neutral Source Discovery Stage 3 Receipt

## Purpose

Record automated evidence that the provider-neutral source-discovery contract is
published and served for Algolia, Meilisearch, and Typesense. This receipt covers
tests and probes only; it does not authorize product-code or generated-OpenAPI
changes.

Test changes were committed at `5f41cde6194643ab60217e1dc062f81b4a792f18`.
The production-owner Typesense offset closure was committed at
`0554accb07999b550db7026b334f2217544a5e41`.

## Contract owners

- `engine/flapjack-http/src/router.rs::register_source_migration_routes` mounts
  `POST /1/migrations/{provider}/list-indexes` for each public provider.
- `engine/flapjack-http/src/handlers/migration/mod.rs` publishes the literal
  OpenAPI paths `/1/migrations/algolia/list-indexes`,
  `/1/migrations/meilisearch/list-indexes`, and
  `/1/migrations/typesense/list-indexes` through
  `define_source_migration_openapi_lifecycle!`.
- The existing MIG-12 provider-neutral source seam remains
  `SourceExportError`, `SourceExportRecord`, `SourceConfigurationArtifact`, and
  `MigrationSourceReader` in `source_reader.rs`. Discovery returns the existing
  `ListSourceIndexesResponse` / `SourceIndexSummary` bundle from `mod.rs`; Stage 3
  introduced no parallel client or response type.

## Route and OpenAPI proof

The following bounded commands passed on the Stage 3 tree:

```text
(cd engine && timeout 900 cargo test -p flapjack-http -- source_discovery_route)
test router_tests::source_discovery_route_is_mounted_for_every_public_provider ... ok
test router_tests::source_discovery_route_preserves_job_status_param_route ... ok
test result: ok. 2 passed; 0 failed

(cd engine && timeout 900 cargo test -p flapjack-http -- published_migration_paths)
test router_tests::published_migration_paths_are_all_mounted ... ok
test result: ok. 1 passed; 0 failed
```

The served probe additionally downloaded the live OpenAPI document and asserted
all three discovery operation IDs, the `migration` tag, and the provider-specific
request schema references. Deleting the Typesense discovery path and cross-wiring
the Meilisearch request schema to the Typesense schema both made the predicate go
red.

## Typesense producer known answers

`(cd engine && timeout 1800 bash tests/typesense_migration_contract_test.sh)`
passed its positive control and all mutation cases: `Results: 54/54 passed`.
The live `typesense/typesense:30.2` producer fixture proved:

- exact name set:
  `{"fj_ts_migration_categories","fj_ts_migration_products"}`;
- exact newest-first order:
  `["fj_ts_migration_products","fj_ts_migration_categories"]`;
- `?limit=1` returns only `fj_ts_migration_products`;
- `?offset=1&limit=1` returns only `fj_ts_migration_categories`;
- `?offset=1` without `limit` also returns only
  `fj_ts_migration_categories` with HTTP 200;
- `?offset=2&limit=1` returns HTTP 400 with the exact body
  `{"message":"Invalid offset param."}`.

The meta-KAT independently rejects mutations to the name set, order, and slice,
and requires the four discovery response artifacts to survive failure cleanup.
The existing least-privilege `/collections` success and export-only-key 401
checks remain in `permission_controls()`.

## Served cross-provider proof

`(cd engine && timeout 900 bash tests/source_migration_provider_parity_http_probe.sh)`
passed against probe-owned loopback upstreams and the real Flapjack binary:

```text
DISCOVERY algolia={"indexes":[{"name":"algolia_products","primaryKey":null,"entries":7,"documentCount":null,"createdAt":null,"updatedAt":"2026-08-02T05:00:00Z","defaultSortingField":null}]}
DISCOVERY meilisearch={"indexes":[{"name":"configured_pk","primaryKey":"sku","entries":null,"documentCount":null,"createdAt":"2026-08-03T11:51:11.143340469Z","updatedAt":"2026-08-03T11:51:11.147244801Z","defaultSortingField":null}],"total":1,"offset":0,"limit":10}
DISCOVERY typesense={"indexes":[{"name":"fj_ts_migration_products","primaryKey":null,"entries":null,"documentCount":2,"createdAt":1785757875,"updatedAt":null,"defaultSortingField":"price"},{"name":"fj_ts_migration_categories","primaryKey":null,"entries":null,"documentCount":1,"createdAt":1785757874,"updatedAt":null,"defaultSortingField":"priority"}]}
DISCOVERY localhost_refusal=PASS providers=meilisearch,typesense
PASS: served discovery response and refusal bodies contain no source or admin credentials
SOURCE_MIGRATION_HTTP_PROBE=PASS providers=3 lifecycle=submit,status,cancel,ack discovery=list-indexes
```

The server process received
`FJ_ENABLE_MEILISEARCH_PREVIEW_LOOPBACK=1` and
`FJ_ENABLE_TYPESENSE_PREVIEW_LOOPBACK=1`; literal `127.0.0.1` endpoints were
admitted, while equivalent `localhost` endpoints returned the exact sanitized
400 responses. Default-off behavior remains pinned by
`meilisearch_client_tests.rs::discovery_loopback_constructor_requires_explicit_opt_in_before_resolution`
and
`typesense_client_tests.rs::discovery_loopback_constructor_requires_explicit_opt_in_before_resolution`.

## Admission ownership split

The reusable outbound admission matrix remains owned by
`engine/src/security_tests.rs`: public/private/loopback/link-local/metadata IP
classification, hostname resolution behavior, strict vendor DNS validation, and
Typesense private-host rejection. Discovery-specific tests remain beside the
Stage 1/2 clients and router: explicit opt-in, literal-loopback-only admission,
hostname refusal, credential sanitization, and route mounting. Stage 3 exercises
those policies through the served binary without duplicating the ACL matrix.

## Regression and offset closure

`(cd engine && timeout 900 bash tests/meilisearch_source_contract_kat_test.sh)`
passed `42/42`. The receipt non-empty and `ROADMAP.md` exclusion gates also
passed.

Typesense 30.2 accepts an in-range offset without a limit, but does not represent
an exhausted collection window as an empty list: offset equal to the collection
count is rejected. The production owner now pins those windows in
`engine/flapjack-http/src/handlers/migration/typesense_client.rs`:

- `list_collections_accepts_offset_without_limit_known_answer` proves
  `offset=1` without `limit` is forwarded and decoded as the exact older
  collection slice.
- `list_collections_surfaces_exhausted_offset_rejection` proves
  `offset=2&limit=1` is forwarded to Typesense and surfaces as the shared safe
  upstream error instead of a fabricated empty list.

`router_tests.rs::list_source_indexes_returns_typesense_known_answer` now uses
the same public behavior: `POST /1/migrations/typesense/list-indexes?offset=1`
returns the older collection, while
`POST /1/migrations/typesense/list-indexes?offset=2&limit=1` returns HTTP 502
with `{"message":"Typesense request failed","status":502}`.

Additional bounded validation for commit
`0554accb07999b550db7026b334f2217544a5e41` passed:

```text
cd engine && timeout 900 cargo fmt --check
cd engine && timeout 900 cargo check -p flapjack-http
cd engine && timeout 900 cargo clippy -p flapjack-http
cd engine && timeout 900 cargo test -p flapjack-http -- list_collections_
cd engine && timeout 900 cargo test -p flapjack-http -- list_source_indexes_returns_typesense_known_answer
cd engine && timeout 1800 bash tests/typesense_migration_contract_test.sh
cd engine && timeout 900 bash tests/source_migration_provider_parity_http_probe.sh
cd engine && timeout 900 cargo test -p flapjack-http -- source_discovery_route
cd engine && timeout 900 cargo test -p flapjack-http -- published_migration_paths
```
