# Decision 0008: MIG-5 Overwrite Mutation-Fence Design
<!-- markdownlint-disable MD013 -->

Date: 2026-07-22
Status: Proposed

## Purpose & Scope

This ADR is the normative future-implementation contract for enabling MIG-5 migration with
`overwrite=true` on one Flapjack node. The required safety property is:

> **no acknowledged write is lost behind the replacement generation.**

The decision extends the existing node-local publication mechanism with a target-scoped mutation
fence, durable incarnation epoch, exclusive promotion, and target-scoped write-queue drain. It does
not create a second swap or recovery owner.

This stage is documentation-only. It changes no runtime behavior, enables no API or CLI mode, and
edits no Rust or test file. The shipped create-only and HA-refusal behavior described below remains
in force until a later implementation and automated contract tests satisfy this ADR.

## Current Behavior Baseline

The shared admission owner is
`engine/flapjack-http/src/handlers/migration/mod.rs::admit_migration_request`, which delegates to
`admit_migration_payload` (`:475-501`). That function validates the request first, returns HTTP 400
with `overwrite=true is not supported by Algolia migration import` when `payload.overwrite` is true,
then independently checks `peer_count() > 0` and returns `migration_ha_unsupported`; only an admitted
request can proceed to source-reader construction (`:504-510`). This ordering matters: overwrite is
currently rejected before HA admission and before any remote source is opened.

The refusal has tight executable evidence in
`engine/flapjack-http/src/handlers/migration/import_contract_tests.rs`:

- `async_import_overwrite_true_is_refused_before_job_creation` (`:796-822`) requires HTTP 400,
  proves the source factory was not invoked, and proves no migration artifacts were created.
- `migrate_overwrite_true_is_refused_before_admission` (`:1263-1300`) configures an HA peer as an
  ordering witness, requires the overwrite-specific HTTP 400 body rather than the HA 503, and again
  proves the source factory was not invoked and no artifacts were created.

The admitted import path is also create-only. In
`engine/flapjack-http/src/handlers/migration/import.rs`, `prepare_import_publication` calls
`PreStagedPublication::prepare` (`:365-397`) to allocate a transaction namespace, and the eventual
activation calls `activate_create_only` (`:319-327`). The underlying contract in
`engine/src/index/manager/publication/executor.rs::PreStagedPublication::activate_create_only`
(`:146-157`) atomically reserves an unused target name and refuses an existing target. That
reservation prevents two create-only transactions from both winning a name. It is not a mutation
fence over a live target: it neither closes the live target's write admission nor drains its queue.

## Reusable publication seams

`engine/src/index/manager/publication.rs` is the canonical node-local publication owner. The future
overwrite path must extend, not bypass, these seams:

- `PublicationTransactionId` (`:56-72`) owns the transaction identity, and
  `PublicationGenerationEvidence` (`:160-170`) owns caller-supplied generation evidence.
- `PublicationPaths` (`:97-129`) owns the target, staging, backup, journal, and quarantine paths under
  the existing publication namespace.
- `PublicationJournal` (`:246-290`) owns durable transition, generation, digest, path, and disposition
  evidence. Its future schema must also carry the mutation-fence epoch and drain watermark.
- `NODE_LOCAL_GUARANTEE` (`:11-12`) explicitly limits the machinery to one node.
- The module re-exports the one activation owner, `activate_publication` (`:27-31`). Its implementation
  in `engine/src/index/manager/publication/executor.rs:494-628` durably syncs staging, writes the
  prepared journal, backs up the target, promotes staging, writes the committed journal, and cleans
  residue. Recovery must continue to use this journal and its repair namespace.

The existing lifecycle caller demonstrates the required orchestration order.
`engine/src/index/manager/lifecycle.rs::move_index_with_publication` (`:283-381`) uses
`copy_dir_recursive` to populate staging (`:315`), calls journaled `activate_publication`
(`:368-375`), requires a committed outcome, and only then removes source artifacts and the source
tree (`:376-380`). The overwrite path must reuse that staged/journaled/cleanup sequence.

Durable target children already have an explicit move policy. The `oplog` row in
`engine/src/index/manager/publication/policy.rs:84-95` is
`ArtifactDisposition::Preserve`: `oplog` and `committed_seq` travel inside the promoted tenant tree,
and recovery resumes from `committed_seq`. The replacement must therefore enter activation with the
correct oplog and committed watermark already inside staging; activation must not synthesize a
second replay or commit owner.

The real missing seam is target-scoped draining. `IndexManager::unload` in
`engine/src/index/manager/lifecycle.rs:107-111` removes the target's queue sender and runtime state
but does not remove or await its entry in `write_task_handles`. By contrast, `graceful_shutdown`
(`:448-465`) clears every sender, then removes and awaits every write-task handle. MIG-5 needs the
same shape for exactly one target, with an admission fence around it. It must not assume `unload`
already drains, and it must not add a second publication mechanism.

Canonical ownership is therefore:

- incarnation epoch, exclusive promotion state, generation evidence, watermark evidence, journal,
  and recovery: `engine/src/index/manager/publication.rs` and its existing submodules;
- mutation admission tickets, target queue closure, target handle await, runtime unload, and reload:
  the existing `IndexManager` write/lifecycle seams, principally
  `engine/src/index/manager/write.rs` and `engine/src/index/manager/lifecycle.rs`;
- oplog sequence and committed-sequence persistence:
  `engine/src/index/oplog.rs` and
  `engine/src/index/write_queue/finalization.rs`;
- HTTP request validation and error translation only:
  `engine/flapjack-http/src/handlers/migration/mod.rs`.

## Mutation-fence design

The implementation must expose one ordered state machine for each target. Let `E_old` be the live
incarnation epoch, `E_new = E_old + 1` the replacement epoch, and `W` the drained old generation's
verified committed oplog sequence.

1. **Stage and validate while live.** Allocate the replacement with the existing
   `PublicationTransactionId` and `PublicationPaths`. Populate and validate `paths.staging` while
   `paths.target` remains the searchable and writable `E_old` generation. Record the old
   generation's staging-baseline sequence so the staged tree can prove a contiguous baseline plus
   mutation delta through `W`; a missing retained delta is an abort condition, never permission to
   promote stale content.
2. **Acquire the target's publication fence and advance the epoch.** The publication owner acquires
   its exclusive per-target fence, verifies the expected `E_old`, advances to `E_new`, and durably
   persists that epoch in its existing per-target publication namespace before cutover can continue.
   The epoch record and the later journal evidence are owned by `publication.rs`; neither belongs in
   the migration handler.
3. **Close old-epoch admission.** Every mutating `IndexManager` admission captures the current target
   epoch and validates it at the enqueue linearization point while holding a target admission ticket.
   Acquiring the exclusive fence closes issuance of `E_old` tickets. A request that merely observed
   `E_old` but did not enqueue before closure fails its epoch check and cannot later receive a success
   ACK. Requests already enqueued under valid `E_old` tickets are not stale: they may finish during
   the drain and are included in `W`.
4. **Give blocked callers a truthful result.** The first implementation must fail fast before task
   creation with the existing retryable `IndexPaused` contract (HTTP 503 and `Retry-After: 1`, owned
   by `engine/src/error.rs:209,780,839-842`). It must not return a task ID or success for work
   that was not admitted. A later bounded-wait implementation is compatible only if it waits behind
   the same fence, re-reads `E_new`, and admits against the replacement after reopen; waiting may not
   preserve an `E_old` ticket.
5. **Drain exactly the target.** Remove/drop that target's existing write-queue sender, remove its
   `write_task_handles` entry, and await the handle. Await completion before clearing the old runtime
   state. This is a target-scoped extension of `graceful_shutdown`, not a call to global shutdown.
   After the await, no `E_old` mutation can still transition to succeeded.
6. **Capture and reproduce `W`.** Read `OpLog::current_seq` only after the target handle has completed,
   strictly verify the old tree's `COMMITTED_SEQ_FILE`, and define their proven committed value as
   `W`. Carry forward or replay every old-generation operation required by the replacement baseline
   through `W` into staging. Commit those effects to the staged Tantivy tree, preserve the staged
   oplog, and strictly persist and re-read staged `committed_seq = W`. If the baseline is not proven,
   the oplog prefix is not contiguous, any replay fails, or either committed-sequence check differs
   from `W`, abort before activation and retain the live generation.
7. **Journal once, then promote once.** Extend the existing `PublicationJournal` evidence with
   `E_old`, `E_new`, the generation, staging baseline, and `W`. Durably journal that evidence and call
   the existing `activate_publication`; its target-to-backup and staging-to-target transitions remain
   the sole promotion mechanism. The exclusive per-target fence remains held throughout journaled
   activation and repair.
8. **Load, verify, and reopen.** Load the promoted target, verify its publication generation, epoch,
   and strict `committed_seq = W`, then reopen admission only for tickets carrying `E_new`. Only after
   those checks may cleanup finish and the fence release. Search handles already opened on the old
   immutable generation may finish, but new target loads are held across the swap so callers never
   open an unverified filesystem generation.

Any error before journaled activation aborts the staged transaction and leaves the old target as the
live generation. Because the durable epoch may already be `E_new`, recovery reopens that unchanged
old tree under `E_new`; an epoch identifies the admission incarnation, not a claim that the tree's
content was promoted. No handler-local mutex, handler-local epoch file, or handler-local swap journal
is permitted.

## Crash-safety invariant

The proof uses the shipped write ordering rather than treating `W` as an arbitrary counter.
`engine/src/index/oplog.rs` owns `COMMITTED_SEQ_FILE`, `read_committed_seq`, and
`write_committed_seq` (`:10-11,38-57`), while `OpLog::current_seq` reports the in-memory appended
sequence (`:138-140`). After a successful Tantivy commit,
`engine/src/index/write_queue/finalization.rs::persist_oplog_commit_state` reads `current_seq` and
writes the tenant's committed sequence (`:220-238`). The queue marks tasks succeeded only after
`finalize_committed_batch` returns (`engine/src/index/write_queue/mod.rs:666-687`), and the durable
HTTP wait does not return success until that task is terminal
(`engine/src/index/manager/write.rs:329-375`).

The future fence strengthens this existing evidence at promotion time. The current
`read_committed_seq` maps missing, unreadable, and malformed files to `0`, and current finalization
logs a committed-sequence write error instead of returning it. Those convenience behaviors are not
sufficient evidence for MIG-5. Fence code must distinguish a real, parseable zero from missing or
malformed evidence, must make its own persistence/verification failure fatal to promotion, and must
prove the staged source contains the committed effects through `W` before it calls activation.

The invariant is falsifiable as follows:

- At admission closure, every request is in exactly one class: it was enqueued with a valid `E_old`
  ticket, or it receives no success ACK and is retryable/waiting.
- Awaiting the target queue makes the first class terminal. Every success ACK in that class follows a
  Tantivy commit and is durably represented at or below verified `W`.
- Promotion is forbidden until staging contains the replacement baseline plus every required
  operation through `W`, and until staging strictly reads back `committed_seq = W`.
- The oplog `ArtifactDisposition::Preserve` policy makes the promoted destination inherit that staged
  oplog and `committed_seq = W`. Recovery/replay therefore resumes after `W`, not behind it.
- The exclusive fence prevents any mutation from being acknowledged against the superseded epoch
  after the drain, throughout promotion, and until admission reopens on `E_new`.

An automated implementation test falsifies the promise if it can obtain a successful pre-fence ACK
whose document/delete effect is absent after promotion, obtain a successful stale-epoch ACK after
the drain, promote with incomplete sequence evidence, or reopen with a committed sequence other than
`W`.

Crash outcomes are bounded by the same owner:

- **Before drain completes:** no activation journal may be committed. Startup discards or rolls back
  the unactivated staging transaction, finishes recovery of the still-live old tree, advances/reuses
  only proven epoch evidence, and reopens it. A request without a durable success ACK may be retried;
  an ACKed commit remains in the old tree.
- **After drain but before journaled activation:** the old target is still authoritative and contains
  every ACKed effect through `W`. Recovery may abort staging and reopen the old tree. It may continue
  toward promotion only when epoch, baseline, contiguous oplog, staged effects, and `W` all validate;
  absent or inconsistent evidence fails closed.
- **During target-backup/staging promotion:** the prepared journal includes epoch, generation, digest,
  and `W`. Existing publication repair may complete the staging-to-target transition or roll back the
  backup according to proven tree/digest evidence. Both eligible trees must have been proven safe for
  the acknowledged prefix through `W`; unreadable, missing, or contradictory epoch/watermark evidence
  is quarantined and the target remains closed.
- **After committed journal but before cleanup or reopen:** recovery requires the promoted target to
  match the journaled generation and `committed_seq = W`, completes residue cleanup, loads that target,
  and only then reopens `E_new`. It must not reopen the backup or accept old-epoch work.

Thus every successful write is either in the drained prefix installed in the replacement, or occurs
after reopen in the replacement epoch. There is no success-ACK interval in which a write can land on
the superseded generation behind the publication swap.

## Alternative 1

**Per-target mutation fence extending `publication.rs` and `lifecycle.rs` (recommended).**

- **Correctness and ACK behavior:** one target's epoch tickets establish a precise admission/drain
  boundary. Old-epoch admitted writes finish into `W`; later attempts receive existing retryable
  `IndexPaused` behavior without a false ACK.
- **Crash recovery:** epoch, generation, digest, and watermark join the existing target-specific
  journal and repair namespace. There is still one activation and recovery owner.
- **Availability:** unrelated indexes remain searchable and writable. The target remains searchable
  on its old immutable handle through most staging/drain work, then has a bounded load/swap window;
  target writes pause only from fence acquisition through verified reopen.
- **Memory and latency:** no global backlog is created. Per-target fence state and admission tickets
  are bounded metadata, and only the promoted target pays drain and retry latency.
- **Owner complexity:** this requires per-target admission plumbing, an epoch check at each mutating
  enqueue seam, and a target-specific handle await. That complexity is localized to the owners that
  already manage publication and write queues.

## Alternative 2

**Node-wide write quiescence reusing the same publication seam (not recommended).**

- **Correctness and ACK behavior:** clearing every queue and awaiting every handle produces a simple
  global boundary. All new writes must wait or receive the same existing retryable response; the
  target's `W` is then captured before the existing activation.
- **Crash recovery:** publication still uses the target's existing journal, so recovery correctness is
  comparable to Alternative 1. The global quiescence itself adds no durable recovery evidence beyond
  the target epoch/journal contract.
- **Availability:** searches can generally continue, but writes to every unrelated tenant stall for a
  single target's import. One slow or stuck queue extends the outage for the whole node.
- **Memory and latency:** callers retrying across all tenants create wider burst pressure after reopen,
  and p95/p99 write latency inherits the slowest queue drain. This is an unacceptable operational
  blast radius for a multi-tenant server.
- **Owner complexity:** implementation is initially simpler because `graceful_shutdown` demonstrates
  the drain shape, but lifecycle coordination and restart behavior become node-global even though
  publication and recovery remain target-specific. The simpler proof does not justify the latency and
  availability cost.

## Recommendation

Implement Alternative 1: a per-target mutation fence that extends the existing publication and
`IndexManager` lifecycle/write owners. It meets the ACK invariant while preserving unrelated-index
availability, bounded memory, and the established journal/repair namespace.

Optimistic post-promotion replay is rejected. It permits a write to receive a success ACK against the
old generation after the replacement has crossed the point where its replay set was captured, which
is exactly the promotion/write-ACK race this ADR must exclude. A migration-handler-local mutex or
swap journal is also rejected: it cannot cover non-migration mutation entry points and duplicates
epoch, promotion, and recovery ownership already belonging to the publication and index-manager
seams.

Any future proposal for a different mechanism must name exactly one owner each for incarnation epoch,
promotion/recovery, and admission/drain, and must prove that none duplicates the existing publication
journal, activation, write queue, or lifecycle machinery.

## Interaction with MIG-7

MIG-5 does not relax the independent HA safety boundary. The
`engine/flapjack-http/src/handlers/migration/mod.rs::admit_migration_payload` check
`peer_count() > 0` must continue to return `migration_ha_unsupported` even after single-node
overwrite admission is implemented. The overwrite refusal may eventually be removed after this
contract ships; the HA refusal must remain.

`NODE_LOCAL_GUARANTEE` in `engine/src/index/manager/publication.rs:11-12` means the epoch, drain,
journal, activation, and repair proof in ADR 0008 applies to one node only. It supplies no peer
agreement, distributed epoch, or convergence protocol. HA-converging import remains refused and is
owned independently by MIG-7 and its reserved ADR 0009 lane.

The downstream CLI consumer is
`engine/flapjack-server/src/ingest.rs::run_ingest` (`:217-228`). It currently returns
`replace_not_supported: --mode replace requires the MIG-5 mutation-fence/publication contract` for
`--mode replace`. A later implementation may remove that refusal only after the same API publication,
epoch, drain, ACK, and recovery contracts are proven; ADR 0008 itself does not unblock the flag at
runtime.

## Non-goals

- implementing the fence, epoch record, target drain, replay, journal schema, or recovery changes;
- enabling `overwrite=true` or `--mode replace`, or changing any public response in this stage;
- changing HA admission, peer convergence, replication, or MIG-7/ADR 0009 ownership;
- adding or changing any `.rs` file, test, load test, API schema, or deployment artifact;
- performance tuning, queue sizing, retention-policy changes, deployment, secrets, network probes,
  or external-system verification;
- creating a second publication, journal, replay, or HTTP-handler-local coordination owner.

## Deferred Items

Future implementation must add the per-target admission/epoch plumbing, target-scoped drain, strict
watermark evidence, staged delta application, publication-journal schema extension, fail-closed
startup repair, and automated failpoint/contract tests described above. It must also measure the
bounded target write pause and prove unrelated-index writes remain available. API and CLI enablement
come only after those tests pass.

Serialization details and concrete Rust type names for the epoch/ticket/watermark extensions are
deferred to that implementation, but their owners and observable contract are fixed by this ADR.
Normative design open questions: none. MIG-7's distributed-convergence questions remain outside this
decision and must not be answered by changing ADR 0008's node-local guarantee.
<!-- markdownlint-enable MD013 -->
