# Decision 0009: MIG-7 HA-Converging Import Design
<!-- markdownlint-disable MD013 -->

Date: 2026-07-22
Status: Accepted

## MIG-7

MIG-7 remains refused in the shipped API until HA import has a durable convergence protocol.
The current guard is `engine/flapjack-http/src/handlers/migration/mod.rs::admit_migration_request`:
when a replication manager has peers, the request returns `migration_ha_unsupported` and names
MIG-7 as the required costed convergence protocol (`engine/flapjack-http/src/handlers/migration/mod.rs:35`,
`engine/flapjack-http/src/handlers/migration/mod.rs:495`).

This ADR is research/design only. It does not change Rust code, tests, scripts, or runtime behavior.

Out of scope:

- Shipping HA import in this stage
- Changing migration admission, publication, snapshot, or replication code
- Reopening MIG-5 overwrite semantics
- Duplicating HA benchmark numbers or prior ADR conclusions from canonical evidence owners

## Current node-local publish/import contract

The verified baseline is preserve-not-reset:

- `move_index` calls `move_index_with_publication` in `engine/src/index/manager/lifecycle.rs:237`.
- `move_index_with_publication` validates and unloads source/destination, copies the source tenant
  directory into publication staging, calls journaled `activate_publication`, then removes staged
  source artifacts and the source tenant directory (`engine/src/index/manager/lifecycle.rs:291`,
  `engine/src/index/manager/lifecycle.rs:315`, `engine/src/index/manager/lifecycle.rs:368`,
  `engine/src/index/manager/lifecycle.rs:377`, `engine/src/index/manager/lifecycle.rs:380`).
- Publication paths, journals, handoffs, tombstones, and public surface contracts are explicitly
  node-local. The owner string says the contract is for one node only and "cannot make HA peers
  converge" (`engine/src/index/manager/publication.rs:11`, `engine/src/index/manager/publication.rs:97`,
  `engine/src/index/manager/publication.rs:246`, `engine/src/index/manager/publication.rs:407`,
  `engine/src/index/manager/publication.rs:426`, `engine/src/index/manager/publication.rs:512`).
- Oplog artifacts are preserved as tenant children. The artifact policy marks `oplog` as
  `ArtifactDisposition::Preserve`, resolves `OPLOG_DIR and COMMITTED_SEQ_FILE`, and repairs by
  "replay from committed_seq" (`engine/src/index/manager/publication/policy.rs:84`).
- Oplog sequence state is local and durable. `OpLog` owns `current_seq: AtomicU64`,
  `COMMITTED_SEQ_FILE`, `read_committed_seq`, `scan_existing`, `fetch_add`, and `read_since`
  (`engine/src/index/oplog.rs:10`, `engine/src/index/oplog.rs:30`, `engine/src/index/oplog.rs:42`,
  `engine/src/index/oplog.rs:59`, `engine/src/index/oplog.rs:101`,
  `engine/src/index/oplog.rs:174`, `engine/src/index/oplog.rs:259`).

Therefore, `move_index` / `move_index_with_publication` copies source to publication staging, calls
journaled `activate_publication`, then removes the source, while oplog artifacts are
`ArtifactDisposition::Preserve` and recovery replays from `committed_seq`.

## Why HA peers cannot converge today

The gap is not a sequence reset. The gap is that publication is node-local and no cross-node import
epoch exists:

- A node can atomically publish its local tenant directory, but the publication journal does not name
  a cluster epoch, a peer adoption set, or a cross-node promotion owner
  (`engine/src/index/manager/publication.rs:11`, `engine/src/index/manager/publication.rs:248`).
- Oplog recovery can replay local entries after `committed_seq`, but `engine/src/index/oplog.rs`
  does not define an import-generation or convergence-epoch namespace that HA peers can compare
  (`engine/src/index/oplog.rs:42`, `engine/src/index/oplog.rs:259`).
- Snapshot install is pull-based. Startup catch-up downloads bytes through
  `ReplicationManager::download_snapshot_from_peer` and installs them with
  `install_snapshot_bytes` (`engine/flapjack-http/src/startup_catchup.rs:277`,
  `engine/flapjack-http/src/startup_catchup.rs:283`, `engine/flapjack-http/src/startup_catchup.rs:377`).
- The replication peer client pulls `/internal/snapshot/{tenant}` from another node
  (`engine/flapjack-replication/src/manager.rs:756`, `engine/flapjack-replication/src/peer.rs:160`).
- Public snapshot import and S3 restore also install snapshot bytes locally; they do not fan out a
  committed epoch to peers (`engine/flapjack-http/src/handlers/snapshot.rs:214`,
  `engine/flapjack-http/src/handlers/snapshot.rs:315`).

Current HA admission refusal is correct because an accepted import could publish on one node while
other peers continue serving the prior generation or install a different later snapshot without one
cluster-wide ordering rule.

## Designed convergence protocol

Future MIG-7 implementation should add exactly one durable convergence epoch owner, one exclusive
cross-node promotion rule, and one peer adoption path.

Durable convergence epoch owner:

- Add a future `MigrationPublicationEpoch` record owned by the migration/publication boundary, not
  by ad hoc peer handlers. The logical owner should extend `engine/src/index/manager/publication.rs`
  because publication already owns transaction id, target, generation evidence, digest, journal
  phase, and terminal disposition for a promoted tenant.
- The record must be persisted before any node advertises the imported target as converged. It must
  include at least target index, source migration job id, leader node id, monotonically comparable
  epoch id, published tenant digest, source committed sequence, and required peer adoption set.
- The epoch must be carried inside the installed tenant snapshot or adjacent publication metadata so
  peer adoption can verify exactly which generation was installed.

Exclusive cross-node promotion rule:

- Only the node that owns the admitted migration job may promote an epoch from prepared to committed.
- A peer must never independently run `move_index` for the same HA import epoch. A peer either keeps
  its previous target generation or adopts the leader's committed epoch through the pull snapshot
  path.
- A conflicting prepared epoch for the same target must lose to the already committed epoch and must
  be repaired or quarantined by the publication repair owner before the target is loadable.
- Migration completion must require adoption evidence for the configured peer set recorded in the
  epoch. Until that evidence is durable, the job is still in progress or failed; it is not a
  successful HA import.

Peer adoption path:

- Reuse the existing pull owners. Peers fetch from the committed leader with
  `engine/flapjack-replication/src/peer.rs::get_snapshot`, orchestrated by
  `engine/flapjack-replication/src/manager.rs::download_snapshot_from_peer`, and install through
  `engine/flapjack-http/src/startup_catchup.rs::install_snapshot_bytes`.
- Do not introduce a peer-push install endpoint for MIG-7. A push installer would duplicate the
  existing validation, extraction, publication repair, staging, backup, and rename boundary already
  concentrated in `install_snapshot_bytes`.
- Adoption evidence must compare epoch id and digest after install. A peer that cannot prove the
  installed tenant matches the committed leader epoch must remain non-adopted.

## Crash and failover safety invariant

For target `T` and epoch `E`, a node may serve the imported generation only after it has durable local
proof of one of these states:

- It is the promotion owner and its journal proves `E` reached committed publication.
- It is a peer and its adoption record proves it pulled and installed a snapshot whose epoch id and
  digest match the committed promotion owner.

A crash before promotion commit must leave every node on the prior generation or in repair. A crash
after promotion commit but before full peer adoption must preserve the leader's committed epoch and
allow peers to resume adoption by pulling the same snapshot. Because the tenant oplog and
`committed_seq` are preserved artifacts, recovery replays only entries after the preserved committed
sequence instead of reinitializing sequence state.

Open questions for implementation:

- Where should adoption receipts live so startup repair can distinguish "old generation still safe"
  from "committed epoch needs adoption retry" without adding a second publication owner?
- Should the first implementation require all configured peers, or should it define a quorum plus
  explicit degraded-read behavior?
- What timeout and retry contract should the async migration status API expose while peers adopt?

## Alternatives

### Alternative 1: Keep the HA refusal until this protocol is implemented

This is the current behavior and remains the recommendation for v1. It protects Algolia-compatible
clients from a successful import response that only describes one node.

### Alternative 2: Let local `move_index` succeed and rely on oplog catch-up

Rejected. The local oplog is preserved, but there is no convergence epoch for peers to compare. Oplog
catch-up can replay operations after a sequence point; it does not prove that every peer has adopted
the same imported directory generation.

### Alternative 3: Push snapshots from the leader into peers

Rejected. This would create a second install boundary separate from
`engine/flapjack-http/src/startup_catchup.rs::install_snapshot_bytes` and would duplicate the
existing pull snapshot owners in `engine/flapjack-replication/src/peer.rs` and
`engine/flapjack-replication/src/manager.rs`.

### Alternative 4: Run `move_index` independently on each peer

Rejected. Independent peer promotion has no exclusive cross-node ordering rule. Two nodes could
publish different staged imports for the same target and both would have locally valid journals.

## Recommendation

Keep `admit_migration_request` refusing HA migration imports until MIG-7 implements the convergence
protocol above. When reopened, implement the smallest slice that:

- persists one cluster-visible `MigrationPublicationEpoch`
- allows only the admitted migration owner to commit that epoch
- has every peer adopt by pulling and installing the committed leader snapshot through existing
  snapshot owners
- records durable adoption evidence before migration status reports success
- tests crash points before commit, after commit before peer adoption, and during peer install

## Non-goals

- MIG-5 overwrite import into an existing target. That needs its own mutation fence and incarnation
  policy in the reserved ADR 0008 boundary.
- Cross-node idempotency deduplication for write failover. ADR 0005 owns that open question.
- Changing HA soak evidence language. ADR 0004, `engine/loadtest/BENCHMARKS.md`, and `ROADMAP.md`
  own HA evidence and public narrative routing.
- Changing snapshot import or S3 restore behavior outside HA migration import.

## Deferred items

- Epoch storage format and metadata file name.
- Adoption receipt storage and repair behavior.
- Migration status fields for peer adoption progress.
- Validation matrix for crash injection and peer failover.
- Operator-facing runbook updates after implementation exists.

## Boundary with sibling ADRs 0008 (MIG-5) and 0010 (OQ4)

ADR 0009 covers only HA-converging create-only migration import. ADR 0008 and ADR 0010 are reserved
numbers in this checkout; there are no active ADR files to link for those numbers.

- ADR 0008 (MIG-5) should own overwrite import into an existing target and the mutation fence needed
  to protect acknowledged writes behind a replacement generation.
- ADR 0010 (OQ4) should own cross-node failover idempotency deduplication if ADR 0005's deferred
  OQ4 becomes an implementation lane.
- ADR 0009 reuses evidence owners by path: `engine/docs2/3_IMPLEMENTATION/decisions/active/0004_ha_convergence_reversal.md`,
  `engine/docs2/3_IMPLEMENTATION/decisions/active/0005_nginx_restart_window_write_recovery.md`,
  `engine/loadtest/BENCHMARKS.md`, and `ROADMAP.md`.

## External-system verification

No external system was contacted for this ADR. Evidence came from checked-in owner files and
fixed-string local probes against the Stage 1 guard anchors.

Required local validation for this stage:

```bash
git diff --check -- ROADMAP.md engine/docs2/3_IMPLEMENTATION/decisions/active/0009_mig7_ha_converging_import_design.md
bash "${TMPDIR:-/tmp}/check_adr_0009.sh"
git status --short
git diff --name-only "$(git merge-base origin/main HEAD 2>/dev/null || git merge-base main HEAD)" HEAD
```
