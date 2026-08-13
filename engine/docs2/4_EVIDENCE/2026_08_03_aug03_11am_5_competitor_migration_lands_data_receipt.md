# Competitor Migration Landed-Data Receipt

This receipt records the exact served-provider proof that real Meilisearch and
Typesense documents migrate into Flapjack and remain searchable with asserted
values. The single source for the fixtures, image pins, identity markers, and
expected landed values is
`engine/tests/source_migration_provider_parity_http_probe.sh` at HEAD.

## Probe-Owned Fixtures And Expected Values

The probe boots a real `flapjack-server`, a real Meilisearch container, and a
real Typesense container. Every value below is asserted by `jq -e` equality in
both the all-documents check and the targeted-search check for that provider
collection.

### Meilisearch `configured_pk`

The migrated Flapjack index is `configured_pk`. The expected total is `2` hits.

```json
[
  {
    "objectID": "MEILI-001",
    "sku": "MEILI-001",
    "title": "Espresso Tamper",
    "price": 24.5,
    "stock": 7
  },
  {
    "objectID": "MEILI-002",
    "sku": "MEILI-002",
    "title": "Pour Over Kettle",
    "price": 39.75,
    "stock": 3
  }
]
```

Targeted searches:

```text
query="Espresso Tamper" -> nbHits=1, MEILI-001 projection exactly as above
query="Pour Over Kettle" -> nbHits=1, MEILI-002 projection exactly as above
```

### Typesense Categories

The migrated Flapjack index is `fj_ts_migration_categories`. The expected total
is `1` hit.

```json
{
  "objectID": "cat_1",
  "id": "cat_1",
  "name": "Coffee",
  "priority": 1,
  "active": true,
  "labels": ["coffee"]
}
```

Targeted search:

```text
query="Coffee" -> nbHits=1, cat_1 projection exactly as above
```

### Typesense Products

The migrated Flapjack index is `fj_ts_migration_products`. The expected total is
`2` hits.

```json
[
  {
    "objectID": "prod_1",
    "id": "prod_1",
    "title": "Espresso",
    "sku": "ESP-001",
    "price": 12.5,
    "inventory": 8,
    "available": true,
    "tags": ["coffee"],
    "category_id": "cat_1"
  },
  {
    "objectID": "prod_2",
    "id": "prod_2",
    "title": "Latte",
    "sku": "LAT-002",
    "price": 9.5,
    "inventory": 5,
    "available": true,
    "tags": ["coffee", "milk"],
    "category_id": "cat_1"
  }
]
```

Targeted searches:

```text
query="Espresso" -> nbHits=1, prod_1 projection exactly as above
query="Latte" -> nbHits=1, prod_2 projection exactly as above
```

## Identity Mapping

The code owners match the probe markers:

- `engine/flapjack-http/src/handlers/migration/meilisearch_source_reader.rs`
  maps the Meilisearch index's declared `primaryKey` to the stable export ID.
  For this fixture, the declared primary key is `sku`, so `MEILI-001` and
  `MEILI-002` become `objectID` values. The machine-checked marker is
  `LANDED_DATA meilisearch=PASS configured_pk=2 identity=sku_to_objectID`.
- `engine/flapjack-http/src/handlers/migration/typesense_source_reader.rs`
  maps the Typesense document `id` field to the stable export ID. For this
  fixture, `cat_1`, `prod_1`, and `prod_2` become `objectID` values. The
  machine-checked marker is
  `LANDED_DATA typesense=PASS categories=1 products=2 identity=id_to_objectID`.

## Pinned Vendor Images

The exercised vendor images are digest pinned in the probe:

```text
MEILI_IMAGE=getmeili/meilisearch@sha256:9694a59df43ee3f54b3fda9c5de381a3ee9852678e3e31cadf37d6bddea7fc1b
TYPESENSE_IMAGE_REF=typesense/typesense:30.2
TYPESENSE_IMAGE_DIGEST=sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110
TYPESENSE_IMAGE=typesense/typesense:30.2@sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110
```

## Stage 1 RED Evidence

Stage 1 command:

```bash
bash engine/tests/source_migration_provider_parity_http_probe.sh
```

Durable result of the initial hardened landed-data probe:

```text
PROBE_EXIT=1
Meilisearch submit HTTP 400: Meilisearch Cloud endpoint is not allowed
LEFTOVER_EXIT=0
```

That RED aborted inside `run_served_migration` at the Meilisearch submit call,
before `probe_served_migrated_data` reached any landed-data `jq -e` assertion.
It proved the submit route was broken; it did not prove a landed-value
assertion could fail.

## Stage 2 Arm B Fix Path

Stage 2 took `[alt]` Arm B. The diagnosed cause was that served Meilisearch and
Typesense submit used production source-reader constructors directly, while
discovery and preview already had explicit debug-only loopback-aware admission
seams. The fix stayed inside
`engine/flapjack-http/src/handlers/migration/**`: submit now uses the shared
provider source-reader owners with production admission first and a debug-only,
explicitly gated loopback fallback for local Docker probes.

The green submit path then exposed a Meilisearch 1.50 settings translation
defect: default ranking rule `attributeRank` and `wordPosition` did not map to
Algolia's single `attribute` criterion correctly. Stage 2 fixed the ranking-rule
translation at the migration settings seam and kept the fail-closed behavior for
unknown ranking rules.

Final Stage 2 gate at tip:

```text
PROBE_EXIT=0
FMT_EXIT=0
CLIPPY_EXIT=0
UNIT_EXIT=0
LEFTOVER_EXIT=0
LANDED_DATA meilisearch=PASS configured_pk=2 identity=sku_to_objectID
LANDED_DATA typesense=PASS categories=1 products=2 identity=id_to_objectID
SOURCE_MIGRATION_HTTP_PROBE=PASS providers=3 lifecycle=submit,status,cancel,ack,preview discovery=list-indexes landed_data=meilisearch,typesense
Full lib: 2353 passed; 0 failed; 2 ignored
```

Focused unit red/green evidence for the ranking-rule fix:

```text
Command: cargo test -p flapjack-http --lib --no-fail-fast -- handlers::migration::meilisearch_contract_tests
RED_EXIT=101, 11 passed / 1 failed
Failure: meilisearch_custom_ranking_rules_keep_attribute_at_first_split_member_position
assertion left == right failed: ranking rules ["words","wordPosition","typo","attributeRank","exactness"] translated to the wrong Algolia order
GREEN_EXIT=0, 12 passed / 0 failed
```

## Negative Controls

Stage 2 performed both required scratch mutations and reverted them.

### Typesense Espresso Price

Mutation:

```text
engine/tests/source_migration_provider_parity_http_probe.sh:633
price:12.5 -> price:12.6 in the typesense_product_espresso assertion
```

Outcome:

```text
NEGCTL PROBE_EXIT=1
SOURCE_MIGRATION_HTTP_PROBE=RED typesense_product_espresso_mismatch body={"hits":[{"objectID":"prod_1","id":"prod_1","sku":"ESP-001",...,"price":12.5,...}],"nbHits":1,...}
```

Revert confirmation:

```text
Mutation reverted; git diff --quiet on the probe was clean; rerun PROBE_EXIT=0.
```

### Typesense Product Inventory

Mutation:

```text
prod_1.inventory:8 -> 9 in the Typesense all-products expectation
```

Outcome:

```text
RED_PROBE_EXIT=1
SOURCE_MIGRATION_HTTP_PROBE=RED typesense_product_all_documents_mismatch
```

The recorded body showed the true landed value `inventory":8`.

Revert confirmation:

```text
Mutation reverted; corrected probe returned GREEN_PROBE_EXIT=0 with LANDED_DATA typesense=PASS categories=1 products=2 identity=id_to_objectID.
```

## Limitations

This probe proves landed document and search value fidelity through a real
Meilisearch server, a real Typesense server, and a real `flapjack-server`. It
does not prove settings, synonyms, curation/rules, permissions, remote Cloud
enablement, resume support, HA import, or any other provider-specific
configuration dimension except where Stage 2 evidence directly measured it. The
Meilisearch 1.50 ranking-rule translation was directly measured by focused unit
contracts and the green full probe; broader settings fidelity remains outside
this receipt's claim.

## Final HEAD Probe Rerun

Final Stage 3 validation was run after writing this receipt and is recorded in
`/tmp/l5_final.txt`.

```text
RECEIPT_EXIT=0
LEDGER_UNTOUCHED_EXIT=0
PROBE_RERUN_EXIT=0
LEFTOVER_EXIT=0
LANDED_DATA meilisearch=PASS configured_pk=2 identity=sku_to_objectID
LANDED_DATA typesense=PASS categories=1 products=2 identity=id_to_objectID
SOURCE_MIGRATION_HTTP_PROBE=PASS providers=3 lifecycle=submit,status,cancel,ack,preview discovery=list-indexes landed_data=meilisearch,typesense
```
