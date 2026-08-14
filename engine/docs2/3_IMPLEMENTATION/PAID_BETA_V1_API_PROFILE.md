# Paid Beta v1 API profile

`paid_beta_v1` is an opt-in runtime API profile for FJCloud. It restricts
customer search-key traffic at the Flapjack listener while retaining the
existing authenticated admin, dashboard-session, and replication-peer control
plane. The default profile remains `full`.

## Service configuration and identity

Set this exact environment value on the Flapjack service:

```text
FLAPJACK_API_PROFILE=paid_beta_v1
```

The profile refuses to start with `FLAPJACK_NO_AUTH=1`. A present but empty or
unknown `FLAPJACK_API_PROFILE` value also refuses startup. A missing value is the
backward-compatible `full` profile.

FJCloud acceptance must require these public health fields before serving PBV1
traffic:

```json
{
  "build": {
    "apiProfile": "paid_beta_v1",
    "supportedApiProfiles": ["full", "paid_beta_v1"]
  }
}
```

## Customer data-plane contract

The only published customer route is literal `POST /1/indexes/*/queries` with:

- `x-algolia-application-id: flapjack`
- `Authorization: Bearer <search-key>`
- a direct, non-secured key whose ACL list is exactly `search` and `browse`
- a key scoped to exactly one physical index

The JSON body is closed and non-empty:

```json
{
  "requests": [
    {
      "indexName": "tenant_123_products",
      "params": {
        "query": "ridge",
        "page": 0,
        "hitsPerPage": 20,
        "facets": ["color"],
        "facetFilters": [["color:blue"]],
        "filters": "published = true"
      }
    }
  ]
}
```

Only `query` (string), `page` (integer at least zero), `hitsPerPage` (integer at
least one), `facets` (string array), `facetFilters` (array), and `filters`
(string) are accepted. Each entry must contain only `indexName` and `params`.
All entries must name the same physical index and it must exactly match the
key's single index scope. The complete batch is validated before search begins.

All other customer paths return 404 before handler dispatch, including index
CRUD, document operations, settings, keys, insights, recommendations,
personalization, experiments, analytics, migration, restore, metrics, dashboard,
and API documentation. A wrong method on the published literal path returns
405. Invalid body shape, unknown fields or parameters, empty batches, and mixed
indices return the engine's standard 400 JSON error envelope.

Invalid, missing, malformed, expired, revoked, secured, wrong-application, or
wrong-index credentials return exactly:

```json
{"message":"Invalid Application-ID or API key","status":403}
```

An authenticated direct key missing the `search` ACL returns exactly:

```json
{"message":"Method not allowed with this API key","status":403}
```

## FJCloud integration

FJCloud should provision the direct key with ACLs exactly `["search",
"browse"]` and an `indexes` array containing only the tenant-scoped physical
index. Keep FJCloud admin and replication credentials on their existing header
transport; the PBV1 Bearer transport is customer-only and does not grant a new
administrative bypass.

AMI and service acceptance should start Flapjack with the profile environment,
assert the health identity above, then run the direct contract probes using a
provisioned tenant index and search key. Staging still needs to capture these
runtime probes; source tests do not constitute an AMI or deployment attestation.
