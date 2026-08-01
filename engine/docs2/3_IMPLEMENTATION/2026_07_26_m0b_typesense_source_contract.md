# M0B Typesense source contract

**Retrieval and probe date:** 2026-07-26

**Selected documentation/API version:** Typesense 30.2

**Configured image reference:** `typesense/typesense:30.2`

**Resolved image identity:** `typesense/typesense@sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110`

**Resolved image ID:** `sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110`

## Purpose and evidence boundary

This note freezes the Typesense source facts that the Stage 2 local
known-answer test (KAT) may depend on. It separates:

1. **Documented facts** from the official Typesense 30.2 API documentation.
2. **Local observations** from one disposable container with the immutable
   identity above.
3. **Stage 1 conclusions** about migration fidelity and refusal behavior.

The `latest` API documentation resolved to **v30.2** on the retrieval date, so
the planned 30.2 version bound did not need to change. The bound is deliberately
narrow: a future Typesense release must be re-qualified rather than assumed
compatible.

### Out of scope

- No production adapter, remote Typesense Cloud request, deploy, or
  infrastructure change.
- No Algolia-shaped `records`, `settings`, or error placeholders for Typesense
  facts.
- No Stage 2 fixture, expected record, expected schema, normalizer, runner, or
  canonical warning code.
- No migration of API-key values, vectors, references, or analytics.
- No claim that one successful export is a transactionally consistent
  snapshot.

The existing production owners were inspected and left unchanged:
`MigrationSourceReader` in
`engine/flapjack-http/src/handlers/migration/source_reader.rs`,
`export_algolia_source_for_import` in `export.rs`, `SpoolStore` in `spool.rs`,
`MigrationJobRunner` in `job_runner.rs`, and `admit_migration_request` in
`mod.rs`. Stage 1 does not rename, generalize, or modify any of them.

## Official-source facts

Every row in this table is a documented Typesense claim. Local behavior and
migration judgments appear in later sections.

| Fact | Typesense 30.2 documentation |
| --- | --- |
| Version selection | The current API index identifies itself as v30.2 and describes v30 synonym, curation, and analytics changes. The `latest` API index also resolved to v30.2 on 2026-07-26. <https://typesense.org/docs/30.2/api/> |
| Collection discovery | `GET /collections` lists collection summaries newest first; `offset` and `limit` paginate the listing, and `exclude_fields=fields` can omit field definitions. `GET /collections/:collection` retrieves one collection. <https://typesense.org/docs/30.2/api/collections.html> |
| Collection schema parameters | A collection requires `name` and `fields`. Collection-level behavior includes `enable_nested_fields`, `token_separators`, `symbols_to_index`, `default_sorting_field`, `synonym_sets`, and `curation_sets`. `default_sorting_field` names an `int32` or `float` field used when search has no explicit `sort_by`. <https://typesense.org/docs/30.2/api/collections.html> |
| Field flags | Every field requires `name` and `type`. `facet` defaults false; `optional` defaults false and permits empty, null, or missing values when true; `index` defaults true; `store` defaults true; and `sort` defaults true for numeric fields and false otherwise. <https://typesense.org/docs/30.2/api/collections.html> |
| Field types and nested fields | Documented field types include scalar and array strings, `int32`, `int64`, floats, booleans, geopoints, geopolygons, `object`, `object[]`, `string*`, `image`, and `auto`. Object indexing requires collection-level `enable_nested_fields`; specific children use dot notation. <https://typesense.org/docs/30.2/api/collections.html> |
| Vector fields | A nonzero `num_dim` makes a `float[]` field a vector; `vec_dist` defaults to cosine and can use inner product. Typesense also supports generated embeddings and HNSW parameters. <https://typesense.org/docs/30.2/api/vector-search.html> |
| Reference fields | A field's `reference` names a field in another collection. Scalar or array string/integer fields express one or multiple references, and stored internal reference IDs depend on indexing order. <https://typesense.org/docs/30.2/api/joins.html> |
| Document identity | A string `id` is the document identifier. If it is absent, Typesense generates an identifier. `id` is special and need not appear in the collection schema. <https://typesense.org/docs/30.2/api/documents.html> |
| Document export | `GET /collections/:collection/documents/export` returns JSONL. The documented projection/filter parameters are `filter_by`, `include_fields`, and `exclude_fields`. <https://typesense.org/docs/30.2/api/documents.html> |
| Export error behavior | The v30.2 behavior notes say an export does not stop streaming when loading a document from disk fails: the error is logged and also returned in the response stream. <https://typesense.org/docs/30.2/api/> |
| Aliases | An alias is a virtual collection name. `PUT /aliases/:alias` creates or updates it; `GET /aliases/:alias` retrieves one; and `GET /aliases` lists all mappings. <https://typesense.org/docs/30.2/api/collection-alias.html> |
| Global synonym sets | v30 uses top-level `/synonym_sets` and `/synonym_sets/:name/items` resources. Sets can be linked through a collection's `synonym_sets` field or supplied to search. <https://typesense.org/docs/30.2/api/synonyms.html> |
| Legacy collection synonyms | v30 automatically migrates old `/collections/{collection}/synonyms/*` data to `/synonym_sets/*`, naming migrated sets with the `*_synonyms_index` suffix. Keys with `synonyms:*` do not authorize new routes; new keys require `synonym_sets:*`. <https://typesense.org/docs/30.2/api/synonyms.html> |
| Global curation sets | v30 uses top-level `/curation_sets` and `/curation_sets/:name/items` resources. Sets link through a collection's `curation_sets` field or can be supplied to search. <https://typesense.org/docs/30.2/api/curation.html> |
| Legacy collection overrides | v30 automatically migrates old `/collections/{collection}/overrides/*` data to `/curation_sets/*`, naming migrated sets with the `*_curations_index` suffix. Keys with `overrides:*` do not authorize new routes; new keys require `curation_sets:*`. <https://typesense.org/docs/30.2/api/curation.html> |
| Search pagination | Search supports `page`/`per_page` and `offset`/`limit`; pages start at 1, `per_page` defaults to 10, and at most 250 hits or groups can be fetched per page. <https://typesense.org/docs/30.2/api/search.html> |
| Health and version | `GET /health` reports node health and may include `resource_error`; `GET /debug` reports `version` and node `state` (1 leader, 4 follower). The official 30.2 API does not document a `GET /version` endpoint. <https://typesense.org/docs/30.2/api/cluster-operations.html> |
| Snapshots and capture markers | `POST /operations/snapshot` creates a point-in-time server-side snapshot at a server path. The document-export API does not document a snapshot ID, collection content revision, or conditional-read marker. <https://typesense.org/docs/30.2/api/cluster-operations.html> |
| Read-key actions | The capture endpoints have distinct actions: `collections:list/get`, `documents:export`, `aliases:list/get`, `synonym_sets:list/get`, `synonym_sets/items:list/get`, `curation_sets:list/get`, `curation_sets/items:list/get`, and `debug:list`. Set and item scopes are separate. <https://typesense.org/docs/30.2/api/api-keys.html> |
| API-key containment limit | A key's `collections` entries accept regular expressions, but collection scope applies only to collection endpoints. It does not restrict global resources such as synonym sets or keys. A key value is returned only when the key is created. <https://typesense.org/docs/30.2/api/api-keys.html> |
| Analytics | Typesense can aggregate queries and interaction events and exposes analytics-rule APIs; self-hosted analytics requires separate server configuration. v30 changed the analytics-rule structure. <https://typesense.org/docs/30.2/api/analytics-query-suggestions.html> |
| Remote Typesense Cloud | Typesense Cloud supplies per-node hostnames and uses HTTPS on port 443, while self-hosted node addresses are supplied by the operator. The data API remains authenticated with a Typesense API key. <https://typesense.org/docs/30.2/api/authentication.html> |
| Redirects | The versioned API index and the resource pages above define exact request paths but specify no redirect-following contract. Redirect handling therefore cannot be inferred from the documentation. <https://typesense.org/docs/30.2/api/> |

## Local public-API probe

### Isolation, identity, and cleanup

The probe used:

- Exact container: `fj_m0b_typesense_30_2_probe`
- Network: container port 8108 published only as
  `127.0.0.1:18108`
- Disposable host root:
  `/tmp/flapjack_typesense_m0b_probe.NmGuH5`
- Exact data directory:
  `/tmp/flapjack_typesense_m0b_probe.NmGuH5/data`
- Exact storage teardown target:
  `/tmp/flapjack_typesense_m0b_probe.NmGuH5`
- Non-production bootstrap key passed only to the local container
- Preserved sanitized evidence:
  `/tmp/flapjack_typesense_m0b_evidence.6oJIXp`

The command shapes were:

```bash
docker pull typesense/typesense:30.2
docker image inspect --format '{{json .RepoDigests}} {{.Id}}' \
  typesense/typesense:30.2
docker run --name fj_m0b_typesense_30_2_probe \
  --publish 127.0.0.1:18108:8108 \
  --volume "${DISPOSABLE_DATA_DIR}:/data" \
  typesense/typesense:30.2 \
  --data-dir=/data --api-key="${DISPOSABLE_BOOTSTRAP_KEY}"

curl --fail-with-body \
  -H "X-TYPESENSE-API-KEY: ${SCOPED_CAPTURE_KEY}" \
  "http://127.0.0.1:18108/collections"

docker rm -f fj_m0b_typesense_30_2_probe
```

Before semantic requests, `docker image inspect` resolved both the repository
digest and image ID to the identity at the top of this note. Cleanup used the
exact container and exact disposable root only. The final receipt reported zero
matching containers and an absent disposable storage root. Raw database files,
scratch imports, and scratch exports were not committed.

Because two probe-harness assertions stopped early, sanitized diagnostics were
preserved before cleanup:

- The first request helper used `path` as a zsh local, shadowing zsh's special
  command-search array. It failed before sending a semantic request. The
  corrected helper renamed the variable.
- The first bulk-import accounting assertion equated newline separators with
  JSON values. It saw 30,000 valid result objects and 29,999 separators because
  the response had no terminal newline. The corrected probe counted parsed JSON
  values.

These were harness errors, not Typesense results. They reinforce the Stage 2
requirement to count parsed JSON values rather than `wc -l`.

### Transcript summary: observed behavior

The table below reports local observations only. Statuses are literal HTTP
results from the pinned container; payload values belong to the future Stage 2
machine oracle and are intentionally not copied here.

| Probe | Observed result |
| --- | --- |
| Collection discovery | Create returned 201. List, paginated list, and schema retrieval returned 200. The list was newest-first and honored `offset`/`limit`. |
| Typed schema | Returned schemas retained `optional`, `facet`, `index`, `store`, `sort`, `num_dim`, `vec_dist`, `reference`, nested-field, tokenizer, set-link, and default-sort information. Auto-detected nested child fields were also returned. |
| Required fields and `id` | Omitting a required schema field returned 400. A string `id` was retained; omitting `id` produced a generated string; a numeric `id` returned 400. |
| Stored versus indexed fields | A `store:false` value was absent from retrieval and export. An `index:false` value remained stored and exported. This is source behavior, not adapter-side loss. |
| References | A reference-bearing exported document contained Typesense-generated reference metadata in addition to its source reference field. Stage 2 must not silently treat generated metadata as an ordinary application field. |
| Export framing | A three-value export returned 200, contained two newline separators, and ended with `}` (decimal byte 125), not a newline. Filter and field-projection parameters took effect. |
| Export errors | A missing collection returned 404; an invalid filter returned 400. The successful stream carried no snapshot or content-revision response header. |
| Alias discovery | Alias creation, list, and exact retrieval returned 200 and exposed the alias-to-collection mapping. |
| Synonym sets | Set creation, set list/get, item list/get, and synonym-affected search returned 200. |
| Legacy synonyms | Both collection-level legacy write and list routes returned 404 on a fresh 30.2 container. |
| Curation sets | Set creation, set list/get, and item list/get returned 200. |
| Legacy overrides | Both collection-level legacy write and list routes returned 404 on a fresh 30.2 container. |
| Search behavior | With no explicit sort, the configured default sort determined the observed order. A configured token separator changed matching, and the linked synonym set expanded matching. |
| Search pagination | `page`/`per_page` and `offset`/`limit` returned deterministic adjacent subsets in the fixture. |
| Health and version | `/health` returned 200 without a key. `/debug` returned 401 without a key and 200 with `debug:list`, reporting version 30.2 and leader state 1. `/version` returned 404. |
| Redirects | `/collections/` returned 200 directly with no `Location` header; `/` and `/version` returned 404. No redirect was observed. Clients must use exact documented routes and must not make redirect-following part of the source contract. |
| Least-privilege positive control | A read key using the actions listed below returned 200 for collection/schema/export, aliases, global set/set-item, and debug reads. |
| Least-privilege negative control | A key containing only `documents:export` exported successfully but received 401 for collection listing. The read key received 401 for `/keys`. |
| Global-resource scope | A read key whose collection regex matched only the probe collections could still list an unrelated global synonym set, confirming that collection regex is not global-set isolation. |

### Mutation during export

A collection with 30,001 documents was exported through the public JSONL
endpoint with client-side rate limiting. After 139,264 response bytes had been
received and while the exact export process was still running, the probe
updated one existing document through the public document API.

The completed response:

- returned 200;
- parsed as 30,001 JSON values;
- contained the pre-update form of the mutated document; and
- ended with `}`, not a newline.

The collection's exposed `created_at` and `num_documents` values were identical
before and after the same-count update. `/debug` exposes node role and server
version, not a collection content revision. No export header supplied a
snapshot identity.

**Stage 1 conclusion:** the observation is compatible with an internal
point-in-time view for that run, but it is not proof of a documented snapshot
contract. The available markers cannot detect a same-count update. A successful
export, an unchanged count, or an unchanged creation timestamp must never be
accepted as proof of quiescence.

## Least-privilege capture actions

Stage 2's local capture key should contain exactly:

```text
collections:list
collections:get
documents:export
aliases:list
aliases:get
synonym_sets:list
synonym_sets:get
synonym_sets/items:list
synonym_sets/items:get
curation_sets:list
curation_sets:get
curation_sets/items:list
curation_sets/items:get
debug:list
```

`GET /health` needs no key in the pinned local runtime. The key does not need
`keys:*`, mutation, search, import, legacy `synonyms:*`, legacy `overrides:*`,
analytics, metrics, snapshot, or other operations.

This action list is necessary but not sufficient containment. The collection
regex limits collection endpoints, while global synonym/curation reads are
cluster-wide. An operator must explicitly consent to that global read scope.
The capture process must never retrieve, serialize, log, or migrate API-key
values. The bootstrap key may create the ephemeral local test key only; it is
not a capture credential.

## Capability classification matrix

These are **Stage 1 migration-fidelity conclusions**, not claims made by
Typesense. `exact` means Stage 2 can preserve the relevant source meaning in
its provider-attributed bundle without a lossy policy choice.
`translated_with_warning` means capture is available but any Flapjack-facing
representation needs explicit translation or an operator-visible warning.
`unsupported` means this lane must refuse or retain an explicit unsupported
finding; it must not invent an Algolia-shaped substitute.

| Capability | Classification | Stage 1 conclusion | Official source |
| --- | --- | --- | --- |
| `collections` | `exact` | Capture collection names and the raw collection response through list/get. | <https://typesense.org/docs/30.2/api/collections.html> |
| `schema_fields` | `translated_with_warning` | Preserve the full provider schema, field order, all flags, collection tokenization, nested settings, linked sets, and default sort. A target schema mapping is a later policy decision. | <https://typesense.org/docs/30.2/api/collections.html> |
| `documents` | `exact` | Preserve exported stored documents and string IDs. Values removed by source-side `store:false` are not recoverable and must not be fabricated. | <https://typesense.org/docs/30.2/api/documents.html> |
| `aliases` | `translated_with_warning` | Capture alias name and target exactly, but do not assume a target-side alias lifecycle or silently turn an alias into a duplicate collection. | <https://typesense.org/docs/30.2/api/collection-alias.html> |
| `synonym_sets` | `translated_with_warning` | Capture top-level sets, items, and collection links exactly; later translation must preserve set identity and sharing semantics. | <https://typesense.org/docs/30.2/api/synonyms.html> |
| `legacy_synonyms` | `unsupported` | The selected fresh v30.2 source exposes the new set API; legacy routes returned 404. Older-source discovery requires a separately versioned contract. | <https://typesense.org/docs/30.2/api/synonyms.html> |
| `curation_sets` | `translated_with_warning` | Capture top-level sets, items, and collection links exactly; later translation must preserve rule semantics and set sharing. | <https://typesense.org/docs/30.2/api/curation.html> |
| `legacy_overrides` | `unsupported` | The selected fresh v30.2 source exposes the new curation API; legacy routes returned 404. Older-source discovery requires a separately versioned contract. | <https://typesense.org/docs/30.2/api/curation.html> |
| `export_stream` | `translated_with_warning` | Parse JSON values incrementally, accept a valid final value without terminal newline, count values rather than separators, and reject or surface in-stream error objects. | <https://typesense.org/docs/30.2/api/documents.html> |
| `pagination` | `exact` | Use documented list/search pagination only where needed for discovery or KAT assertions; document export itself is a stream, not paged capture. | <https://typesense.org/docs/30.2/api/search.html> |
| `health_version` | `exact` | Use unauthenticated `/health` for readiness and authenticated `/debug` for the supported-version gate; do not call a fabricated `/version` route. | <https://typesense.org/docs/30.2/api/cluster-operations.html> |
| `quiescence` | `translated_with_warning` | Public export exposes no stable capture marker. Require an external write freeze/attestation and refuse capture when it cannot be established; pre/post count and creation time are only diagnostics. | <https://typesense.org/docs/30.2/api/cluster-operations.html> |
| `api_keys` | `unsupported` | API-key material is not migration data. Accept a separately supplied least-privilege capture key and never export or reproduce key values. | <https://typesense.org/docs/30.2/api/api-keys.html> |
| `vectors` | `unsupported` | Preserve an explicit unsupported finding for vector schema/data; do not imply equivalent distance, embedding, or index behavior. | <https://typesense.org/docs/30.2/api/vector-search.html> |
| `references` | `unsupported` | Preserve an explicit unsupported finding and source schema. Do not migrate generated internal reference metadata or claim target join equivalence. | <https://typesense.org/docs/30.2/api/joins.html> |
| `analytics` | `unsupported` | Do not capture analytics rules/events or translate them into Flapjack click analytics in this lane. | <https://typesense.org/docs/30.2/api/analytics-query-suggestions.html> |
| `remote_typesense_cloud` | `unsupported` | Stage 1 proves loopback self-hosted behavior only. Remote TLS, hostname, credentials, egress, and Cloud operational behavior require a separately authorized probe. | <https://typesense.org/docs/30.2/api/authentication.html> |

## Unsupported and warning mappings

Stage 2 owns the canonical machine-readable warning codes. It should derive
codes for the following meanings without copying these prose labels as a
second oracle:

- Provider schema requires translation: preserve every source field flag,
  nested setting, tokenization setting, linked set, and default sort.
- Alias requires policy: preserve the mapping; do not create a duplicate
  collection implicitly.
- Global synonym and curation sets require translation while retaining
  collection links and shared-set identity.
- Export may omit its terminal newline and may contain an error object inside
  an otherwise successful stream.
- Source quiescence is unproven unless the caller supplies an accepted
  write-freeze attestation; count and timestamps do not close this warning.
- Legacy v29 synonym/override discovery is outside the 30.2 contract.
- API keys, vectors, references, analytics, and remote Cloud capture are
  unsupported and must remain explicit.

## Stage 3 publication receipt

Stage 3 re-verified this note against HEAD
`669ace4a30071a6bb856b530022f7c26a3398dd2` on 2026-07-26. The local KAT
support bound remains **Typesense 30.2 only**. The configured image reference is
`typesense/typesense:30.2`, and each fresh run resolved the pinned identity
`sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110`.

The canonical machine oracle remains
`engine/tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json`.
Its verified SHA-256 is
`2ce9667cdb32212fcc3f17edbb8e2b395c6581b2ba273b635e864f6182f1029c`.
Typesense KAT: `Results: 44/44 passed`.
This note intentionally does not copy the fixture's document records, source
schemas, provider evidence objects, warning-code arrays, unsupported-code
arrays, or generated bundle payload.

From `engine/`, the green runner command used by consumers should be:

```bash
RUN_ID="m0c_m1_typesense_contract_probe_$(date +%s)_$$"
TMPDIR=/tmp FJ_TYPESENSE_WRITE_FREEZE_ATTESTED=1 FJ_TYPESENSE_RUN_ID="$RUN_ID" FJ_TYPESENSE_EVIDENCE_DIR="/tmp/fj_${RUN_ID}_evidence" bash tests/typesense_migration_contract.sh
```

From `engine/`, verify fixture identity with:

```bash
shasum -a 256 tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
```

Literal read-only M0C/M1 probes from `engine/`:

```bash
jq -e '.contract.least_privilege_actions | index("collections:list") and index("debug:list") and length == 14' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
jq -e '.contract.warning_codes | index("typesense_schema_requires_translation") and index("typesense_export_stream_not_newline_counted") and length == 5' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
jq -e '.contract.unsupported_codes | index("typesense_api_keys_unsupported") and index("typesense_vectors_unsupported") and index("typesense_references_unsupported") and index("typesense_analytics_unsupported") and length == 5' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
jq -e '.source.aliases[] | select(.name == "fj_ts_migration_catalog" and .collection_name == "fj_ts_migration_products")' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
jq -e '.source.collections | map(.name) | sort == ["fj_ts_migration_categories", "fj_ts_migration_products"]' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
jq -e '.source.collections[] | select(.name == "fj_ts_migration_products") | .fields[] | select(.name == "metadata.color" and .facet == true)' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
jq -e '.source.provider_evidence.debug.version == "30.2" and .source.provider_evidence.health.ok == true and .contract.capture_requires_write_freeze == true' tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
```

The least-privilege capture actions are the fourteen actions listed in
`Least-privilege capture actions` and exposed by
`.contract.least_privilege_actions` in the fixture. The warning matrix remains
the five codes in the fixture's `.contract.warning_codes`, derived from the
provider-attributed warning meanings in `Unsupported and warning mappings`; the
unsupported matrix remains the five codes in the fixture's
`.contract.unsupported_codes`: API-key material, vectors, references,
analytics, and the write-freeze refusal for unproven source quiescence. Remote
Typesense Cloud stays `unsupported` in the capability matrix above and is
intentionally absent from the local-capture fixture codes. The unresolved
architecture choices remain the write-freeze owner, reference-metadata
handling, target policies for aliases/shared sets, and separately authorized
remote Typesense Cloud qualification.

## Stage 2 handoff

1. Keep all seed values and expected normalized values in the single
   machine-owned fixture assigned by the canonical stages plan:
   `engine/tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json`.
   This note must not become a second expected-value owner.
2. Use only the public APIs and the pinned image identity above. Capture raw,
   provider-attributed collection schemas, stored documents, aliases, global
   sets/items, and version evidence before normalization.
3. Parse export/import JSONL by JSON values, not `wc -l`; support a final JSON
   value without a newline and account for every byte/value. Fail on
   truncation, malformed JSON, or an in-stream error object.
4. Require an explicit write-freeze/attestation input. If it is absent or
   indeterminate, return the quiescence warning/refusal instead of treating a
   successful export or unchanged count as stable.
5. Use the exact read actions above and test both positive and negative
   permission controls. Treat global set visibility as cluster-wide, regardless
   of the key's collection regex.
6. Detect reference schema before interpreting exported generated reference
   metadata. Preserve the unsupported finding rather than silently importing
   those keys as application fields.
7. Reuse the existing test discipline:
   `engine/tests/algolia_source_export_live.sh` preserves evidence before
   teardown and tracks exact owned resource names;
   `engine/tests/algolia_source_export_live_test.sh` verifies cleanup and
   evidence behavior; `engine/tests/migration_import_contract.sh` produces a
   deterministic manifest; and
   `engine/tests/migration_import_contract_test.sh` uses exact full-field,
   hand-calculated known answers and deliberate negative mutations.
8. A 2026-07-26 search found no landed `*meilisearch*` runner/fixture and no
   provider-neutral migration normalizer/verifier under `engine/tests/`.
   Therefore there is no reusable M0A Stage 1 dependency yet. Re-check before
   Stage 2; do not create a speculative shared boundary.
9. Continue to leave the production migration owners named in the first
   section unchanged. This KAT is test-only evidence for a later architecture
   decision.

## Open questions

- What concrete operator attestation or maintenance-mode seam will own the
  write freeze for a future production adapter?
- Should a future source bundle retain Typesense-generated reference metadata
  solely as quarantined evidence, or omit it after recording the unsupported
  reference finding?
- What target-side policies, if any, will represent aliases, shared synonym
  sets, and curation sets without losing provider semantics?
- Which separately authorized stage will qualify remote Typesense Cloud
  redirects, TLS, topology, and rate/availability behavior?
