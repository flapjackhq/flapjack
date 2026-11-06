# Architecture

This document is the maintained high-level map of the shipped Flapjack workspace.
It focuses on current structure and ownership boundaries, not historical design
notes.

For product/readiness status, see [../FEATURES.md](../FEATURES.md).
For frontend-specific design, see the bundled dashboard sources in `engine/dashboard/`.
For deployment/runbook entry points, see [./DEPLOYMENT.md](./DEPLOYMENT.md).

## Workspace layout

Flapjack is organized as a Rust workspace with a bundled dashboard:

- `engine/src/` — core search engine library
- `engine/flapjack-http/` — Axum HTTP layer and request handlers
- `engine/flapjack-server/` — binary entrypoint and CLI/env bootstrap
- `engine/flapjack-replication/` — replication peer coordination
- `engine/flapjack-ssl/` — TLS / ACME support
- `engine/dashboard/` — bundled dashboard frontend
- `engine/examples/` — runnable proof surfaces for specific topologies

## Core library

The core library in `engine/src/` owns the data model and search behavior:

- `index/` — index lifecycle, settings, write queue, persistence, snapshots
- `query/` — parsing, typo tolerance, filters, geo, highlighting
- `analytics/` — events, rollups, query analytics
- `vector/` — vector search and embedder support
- `query_suggestions/` — query-suggestion generation
- `tokenizer/` — custom tokenization pipeline

This crate should remain usable outside the bundled HTTP server where practical.

## Storage ownership

Index names cannot begin with `.` or `_`. Independent stores use those reserved
namespaces; discovery must never infer ownership from a directory's contents.
Application IDs (dictionary API), index IDs (core/index extensions), and the
server-global personalization store are different identities.

| Artifact | Identity and canonical location | Readers and mutation owner | Copy, delete and recovery |
|---|---|---|---|
| Documents, settings, rules, synonyms, oplog, vectors | Index ID: `<data>/<index>/` | `IndexManager`, write queue, index config owners | Existing fenced index publication/copy/delete and native snapshot/import; damaged indexes remain errors. |
| Application dictionaries/settings | Authenticated application ID: `<data>/.applications/<app>/.dictionaries/` | Dictionary HTTP handlers and authenticated single/batch search and query-browse via `DictionaryManager` / `DictionaryStore` | Atomic JSON persistence with application-local mutation ordering through one shared manager; index creation/copy/delete/import must not touch them. Native index snapshots do **not** prove application-state recovery. See transfer boundary below. |
| Personalization strategy and profiles | Server-global: `<data>/.personalization/personalization_strategy.json` and `profiles/<user>.json` | `PersonalizationProfileStore`; strategy/profile HTTP, search personalization, insights computation and GDPR deletion | Atomic persistence and existing per-user ordering; independent of index lifetime. Preserve current server-global semantics and product gates; not a new multi-app guarantee or index snapshot artifact. |
| Publication transaction / quarantine evidence | Index ID: existing `.publication` / `.publication_quarantine` | Existing publication owner | Existing fencing, rollback/recovery and strict snapshot import; no new registry. |

Authenticated HTTP dictionaries follow Algolia application ownership, including
searches of virtual replicas. Replica-local dictionary lookup was an implicit
index-ID override, not an application-authenticated API. The dictionary module
explicitly documents application scope, not index scope; no independent product
contract for index dictionary overrides was found. Remove that accidental override.
Core callers select an application explicitly, or use the existing default
application constant; HTTP callers use the authenticated application. No new route or personalization scope is introduced.

Prior `<data>/<id>/.dictionaries`, `<data>/personalization/profiles`, and root
`personalization_strategy.json` files are unsupported ambiguous layouts. Admission and
storage access must diagnose them without moving/deleting data, falling back to
empty state, or skipping a damaged index. No migration or dual-read fallback.
A valid index named `personalization` without the obsolete profile artifact remains
legal. Reserved roots do not detect total disk loss; expected-index inventory
remains the existing topology/readiness owner's responsibility.
The strategy filename is also a legal index name: a directory with that name
undergoes normal index validation; it is never treated as a strategy file.

### Application-state transfer boundary

The existing native snapshot (`internal_snapshot` / `index::snapshot`) and FJCloud
`restore_native_snapshot` transfer one index with tenant/transaction/hash/watermark
evidence. They have no application inventory, application-to-index binding or
application-state revision. Dictionary mutations do not advance the index oplog.
A same-name directory previously hitchhiking in an index archive is not a valid
shared application recovery contract. Personalization is similarly server-global.

An index archive must not overwrite reserved sibling roots. This correction can
prove index-owned configuration survives copy/restore and unrelated application
state is unchanged. Shared application recovery uses the separate managed
configuration contract below: FJCloud selects authenticated customer bindings
from its existing index/replica catalog and delivers the committed generation.
Do not derive application ownership or freshness from coincident names, index
watermarks or a broadcast. Managed FJCloud customer calls must retain the
authenticated customer UUID as their canonical application ID under a node-admin
credential. PBV5 batch admission permits this only for direct node-admin calls;
customer keys and dashboard sessions keep their existing profile restrictions.
Correct identity and local mutation ordering do not establish application-state
delivery, a required configuration revision, or recovery readiness.

### Managed dictionary recovery contract

FJCloud owns the authenticated customer UUID, durable desired application
configuration and customer-scoped mutation task. The engine dictionary owner
validates commands and publishes one immutable local generation containing all
three dictionaries and settings. Preparation does not activate or write files;
the control plane must commit the canonical generation before delivery. Index
snapshots remain index-only; replacement obtains the application generation from
the durable authority, never from an arbitrary peer or a same-named index.

The local generation remains under `.applications/<app>/.dictionaries/` and uses
the existing atomic-write persistence owner. A request captures one generation
through preprocessing, fallback and interleaving. Index deletion cannot remove
it, and a generation for another application cannot satisfy the request.
Explicit empty configuration is valid; missing, malformed, stale or conflicting
required state is unavailable. Restoring older control-plane state must not
silently overwrite a newer local generation. Ambiguous legacy layouts retain
clear diagnostics and require an explicit recovery decision.

Durable acceptance returns a pending task, not successful fleet completion.
Pending delivery allows coherent previous configuration to continue serving.
Completion requires the existing routing/lease owners to prevent admission of
older configuration on every still-authorized serving path; admitted in-flight
requests may finish. Normal searches consult local configuration only. Managed
task status survives API restart and index deletion/movement, and is scoped to
the authenticated customer. This is not a new global Algolia dictionary API.

The internal prepare/install/read endpoints require the exact node-admin key;
a dashboard session or peer credential cannot use them. An installed receipt
reads the actual generation file; absence is unavailable even for revision zero.
Management reads obey the same required floor as search. A valid installed older
generation can remain eligible while a newer generation is pending. Publication
uses the existing mutation fence and atomic write/directory-sync path, including
an identical retry after an uncertain durability acknowledgement.

## HTTP layer

`engine/flapjack-http/` owns:

- router construction
- middleware
- auth and API key enforcement
- health/readiness endpoints
- Algolia-compatible request/response handling

Representative files:

- `flapjack-http/src/server.rs`
- `flapjack-http/src/router.rs`
- `flapjack-http/src/handlers/`
- `flapjack-http/src/auth/`

## Binary entrypoint

`engine/flapjack-server/src/main.rs` is the runtime bootstrap layer:

- CLI parsing
- env-var resolution for server startup
- local-dev instance isolation helpers
- `reset-admin-key` and uninstall commands

The binary should stay thin. Business logic belongs in the core library or HTTP
crate, not in `main.rs`.

## Dashboard

`engine/dashboard/` is the bundled TypeScript/React dashboard frontend served by
the Flapjack binary under `/dashboard`.

Current route inventory is canonical in:

- `engine/dashboard/src/App.tsx`
- `engine/dashboard/tests/E2E_UI_COVERAGE_CHECKLIST.md`

The dashboard is part of the launch surface and is validated by Playwright
coverage in `engine/dashboard/tests/`.

## Operational proof surfaces

Deployment claims should be anchored to runnable examples or validation scripts,
not prose alone:

- `engine/examples/systemd/` — reusable Linux/systemd deployment templates
- `engine/examples/ha-cluster/` — nginx-routed 3-node HA compose proof
- `engine/examples/replication/` — 2-node replication + analytics fan-out proof
- `engine/examples/s3-snapshot/` — MinIO-backed snapshot proof
- `./s/test` — canonical unified test runner

## Design constraints

- Keep Algolia compatibility stable at the wire level.
- Prefer a single source of truth per operational concern.
- Avoid duplicating status/readiness claims outside `engine/docs2/FEATURES.md`.
- Keep deployment guidance grounded in verified examples.
