# Decision 0007: PL-10 v1.1 Write Path Design
<!-- markdownlint-disable MD013 -->

Date: 2026-07-20
Status: Accepted

## Measured baseline

Stage 1 produced a machine-local lower bound, not a saturation ceiling. The retained evidence says
the saturation hypothesis was falsified because all `5,592` single-document write requests were
accepted and `write_http_4xx_rate` was `0.00%`; the accepted rate was `5592 / 120 = 46.600000`
writes/second, but absent backpressure makes that number a lower bound under the prescribed local
load, not a ceiling. Source: `engine/loadtest/results/20260720T170944Z-pl10-v11-single-writer-ceiling/README.md`
and raw stdout at `engine/loadtest/results/20260720T170944Z-pl10-v11-single-writer-ceiling/write_soak.stdout.txt`.

Concrete evidence preserved for Stage 3:

- Arithmetic: `5592 / 120 = 46.600000` accepted single-document writes/second.
- Contract state: `write_http_4xx_rate=0.00%`, `write_http_5xx_rate=0.00%`, and
  `write_http_unexpected_4xx_rate=0.00%`.
- Interpretation: the clean run is valid forward-progress and public-error-contract evidence, but
  it is not saturation evidence because no `QueueFull`/4xx backpressure occurred.

Decision gap: this ADR cannot claim a numeric saturation ceiling from Stage 1. The exact missing
evidence is the first same-machine offered-load point where `write_http_4xx_rate > 0.00%` while
`write_http_5xx_rate == 0.00%` and `write_http_unexpected_4xx_rate == 0.00%`. The smallest owner-file
change that would unblock a numeric ceiling claim is a loadtest-harness extension under the existing
write-soak owners (`engine/loadtest/scenarios/write-soak.js`, `engine/loadtest/lib/config.js`, and
`engine/loadtest/lib/throughput.js`) that can sweep offered load without changing
`SOAK_WRITE_THRESHOLDS` or runtime write behavior. Until that exists, alternatives below use
`46.600000` only as a clean lower bound, proxy denominator, or conditional gate.

## Current writer constraint and reusable seams

Current ownership is already separated into admission, per-tenant queue processing, writer-slot
acquisition, commit finalization, public retry contracts, and replication/search reuse seams.
PL-10 v1.1 should extend those owners instead of creating a parallel write path.

The enqueue owner is `engine/src/index/manager/write.rs`. `get_or_create_write_queue` owns
per-tenant queue creation and passes the tenant id, index, task map, base path, optional oplog,
facet cache, and vector context into `create_write_queue`. Durable conflict versions are opened
from the tenant path by the admission and finalization owners instead of being copied into queue
context. The add path documents that it sends
`WriteOp` values to the queue and returns `QueueFull` on backpressure
(`engine/src/index/manager/write.rs:81`); the actual nonblocking admission call is `try_send`
(`engine/src/index/manager/write.rs:114`) and the public error is `FlapjackError::QueueFull`
(`engine/src/index/manager/write.rs:125`). Delete follows the same pattern
(`engine/src/index/manager/write.rs:131`, `engine/src/index/manager/write.rs:147`,
`engine/src/index/manager/write.rs:158`), and compact also goes through the same queue seam
(`engine/src/index/manager/write.rs:233`, `engine/src/index/manager/write.rs:246`,
`engine/src/index/manager/write.rs:257`). Durable HTTP waits are separate from admission:
`wait_for_write_durable` is the bounded durability wait owner (`engine/src/index/manager/write.rs:329`),
`add_documents_durable` waits after enqueue (`engine/src/index/manager/write.rs:343`), and
`delete_documents_durable` does the same for deletes (`engine/src/index/manager/write.rs:363`).

The writer-slot owner is the write queue plus `Index`. `WriteQueueContext` carries the tenant queue
context (`engine/src/index/write_queue/mod.rs:118`). `create_write_queue` returns the channel sender
and spawned task handle (`engine/src/index/write_queue/mod.rs:236`,
`engine/src/index/write_queue/mod.rs:253`) and starts `process_writes`
(`engine/src/index/write_queue/mod.rs:269`). `acquire_writer_for_queue` retries `Index::writer()`
contention (`engine/src/index/write_queue/mod.rs:281`, `engine/src/index/write_queue/mod.rs:292`),
special-cases `TooManyConcurrentWrites` (`engine/src/index/write_queue/mod.rs:297`), and returns
that same error after the retry deadline (`engine/src/index/write_queue/mod.rs:308`). The lower-level
slot is the global memory budget: `Index::writer_with_size` validates the buffer, acquires a writer
guard from the budget, then constructs Tantivy's writer (`engine/src/index/mod.rs:616`,
`engine/src/index/mod.rs:619`, `engine/src/index/mod.rs:620`).

The single-batch commit sequence is also owned in one place. `flush_pending_batch` acquires one
writer and delegates the entire pending vector to `commit_batch`
(`engine/src/index/write_queue/mod.rs:339`, `engine/src/index/write_queue/mod.rs:350`,
`engine/src/index/write_queue/mod.rs:351`). `commit_batch` stages all queued ops into that writer
(`engine/src/index/write_queue/mod.rs:600`, `engine/src/index/write_queue/mod.rs:642`) and calls
`commit_writer_with_panic_guard` once per flush (`engine/src/index/write_queue/mod.rs:666`).
Document staging calls `writer.add_document` (`engine/src/index/write_queue/finalization.rs:10`,
`engine/src/index/write_queue/finalization.rs:16`), and the final commit path wraps
`writer.commit()` (`engine/src/index/write_queue/finalization.rs:56`,
`engine/src/index/write_queue/finalization.rs:72`).

Public backpressure/error contracts must stay distinct. The error enum has a writer-slot contention
variant (`engine/src/error.rs:45`) and a queue-admission variant (`engine/src/error.rs:63`). The
status map sends `TooManyConcurrentWrites` to 503 (`engine/src/error.rs:181`,
`engine/src/error.rs:191`) and `QueueFull` to 429 (`engine/src/error.rs:197`). API messages likewise
separate the two conditions (`engine/src/error.rs:739`, `engine/src/error.rs:753`,
`engine/src/error.rs:770`), and both transient responses carry `Retry-After: 1`
(`engine/src/error.rs:844`, `engine/src/error.rs:849`). The single-object HTTP handler documents
the pre-enqueue contract directly: `QueueFull` returns 429 with no task id, while durable commit
failures can still report the accepted task id (`engine/flapjack-http/src/handlers/objects/mod.rs:485`).

Durability and state side effects also constrain the design. Oplog receipts are prepared with the
committed Tantivy batch. After Tantivy commit,
`engine/src/index/write_queue/finalization.rs::finalize_committed_batch` applies every receipt plus
finalized task identity in one `VersionStore` SQLite transaction, persists the oplog watermark,
then reloads and invalidates search/facet state.
`engine/src/index/version_store.rs::VersionStore` is the sole durable owner of per-document
conflict tuples for primary and replicated writes. The oplog batch writer remains the owner of
sequence allocation and flushing. `IndexManager::tenant_doc_count` remains backed only by the
published Tantivy searcher and does not consult conflict metadata.

### Alternative 1 — Shard fan-in through query-time resolution

Reuse first: Flapjack already has a query-time indirection shape in virtual replicas. A resolved
search target contains a physical `data_index` plus optional settings override
(`engine/flapjack-http/src/handlers/replicas.rs:69`). `resolve_search_target` decides whether the
requested index should query its own data or the primary's data with an override
(`engine/flapjack-http/src/handlers/replicas.rs:74`, `engine/flapjack-http/src/handlers/replicas.rs:90`,
`engine/flapjack-http/src/handlers/replicas.rs:115`). Search execution already calls that resolver
before the core engine search (`engine/flapjack-http/src/handlers/search/single_interleaving.rs:41`,
`engine/flapjack-http/src/handlers/search/single_interleaving.rs:49`) and queries
`resolved_target.data_index` (`engine/flapjack-http/src/handlers/search/single_interleaving.rs:60`).
The top-level single-search orchestration already resolves request context before the blocking search
work (`engine/flapjack-http/src/handlers/search/single_execution.rs:52`).

Costing:

- Capacity claim: conditional sustained-capacity increase. If a logical index is split across `N`
  physical shard indexes and each shard retains its own existing `write_queue`, the theoretical
  enqueue/commit parallelism is up to `N * 46.600000` accepted single-document writes/sec as a lower
  bound proxy, not a ceiling. The valid claim is only "can exceed one unsaturated queue's observed
  lower-bound rate after a saturation/control experiment"; evidence grade: conditional. This
  alternative is not recommended as the first slice until search fan-in correctness is specified.
- Arithmetic: for `N = 2`, the lower-bound proxy is `2 * 46.600000 = 93.200000` accepted writes/sec
  if both shard queues receive balanced traffic and neither hits a shared writer-memory or disk
  bottleneck. Because the Stage 1 run did not saturate one queue, this arithmetic is a capacity
  floor for a balanced two-shard candidate, not an uplift denominator.
- Durability and API compatibility: each physical shard must still call existing
  `IndexManager::add/delete/compact` paths, so task state, oplog append, conflict-version
  finalization, and 429/503 mapping remain with current owners. A logical write that touches one
  shard can return one Algolia task id;
  a multi-shard batch needs either one logical task wrapper or documented physical task fan-out, both
  with task-polling compatibility tests.
- Search latency, memory, and operational complexity: every logical search pays fan-out and merge
  cost. The existing batch/federation path already builds query metadata and calls
  `merge_federated_results` (`engine/flapjack-http/src/handlers/search/batch.rs:121`,
  `engine/flapjack-http/src/handlers/search/batch.rs:166`), while the merge owner uses weighted RRF,
  per-query weights, per-index de-duplication, and `estimated_total_hits` summing
  (`engine/flapjack-http/src/federation.rs:66`, `engine/flapjack-http/src/federation.rs:76`,
  `engine/flapjack-http/src/federation.rs:114`). Transparent logical shards need global Algolia
  semantics for ranking, pagination, facets, distinct, optional filters, and analytics. The current
  federation owner explicitly rejects facet merging when requested
  (`engine/flapjack-http/src/handlers/search/batch.rs:99`), so it does not yet fit transparent
  shards.
- Incremental first slice: extend `resolve_search_target` from one `data_index` to an enum that can
  represent one-to-many shard fan-in, then reuse `SearchInvocation::run` for each physical shard and
  route the combined hits through a new mode on the existing federation merge owner. The smallest
  extension seam is in `engine/flapjack-http/src/federation.rs`, adding a shard-merge mode that
  handles globally comparable scores/facets rather than creating a second merge implementation.
- Existing owners extended: `engine/flapjack-http/src/handlers/replicas.rs::resolve_search_target`,
  `engine/flapjack-http/src/handlers/search/single_interleaving.rs::SearchInvocation::run`,
  `engine/flapjack-http/src/handlers/search/batch.rs`, and
  `engine/flapjack-http/src/federation.rs::merge_federated_results`.

### Alternative 2 — Persistent admission and replay in the write queue

Reuse first: the current write queue already owns validation, batching, commit, task status, oplog,
facet-cache invalidation, durable conflict-version publication, and vector persistence. Extend
that boundary with durable admission and replay rather than adding a front queue. Admitted
`WriteOp` values must continue through the
existing queue processor rather than staging documents directly into Tantivy.

Costing:

- Capacity claim: bounded-lag/backpressure improvement, not a proven sustained-ceiling increase.
  With the current durable HTTP contract, `add_documents_durable` still waits after enqueue
  (`engine/src/index/manager/write.rs:353`, `engine/src/index/manager/write.rs:358`,
  `engine/src/index/manager/write.rs:359`), so adding persistent admission to the same commit owner
  cannot honestly claim more synchronous committed writes/sec than the single queue can flush. The
  viable product claim is: at a fixed offered load above `46.600000` writes/sec, the server can
  durably admit writes to a local log, apply bounded backpressure before unbounded memory growth, and
  drain to Tantivy with a measured backlog-age ceiling. Evidence grade: proxy until the fixed-load
  bounded-lag experiment runs.
- Arithmetic: Stage 1 proves only `accepted_rate >= 46.600000`. A first bounded-lag candidate should
  choose an offered load above that lower bound, for example `2 * 46.600000 = 93.200000` attempted
  writes/sec for the same 120-second window, and compare control versus candidate on backlog age,
  accepted count, and clean error contract. The denominator is attempted offered load and backlog
  duration, never a claimed uplift over a missing ceiling.
- Durability and API compatibility: the preferred design extends the current `write_queue` owner with
  persistent admission records rather than introducing a second front queue. Admission must be one
  explicit state transition: reserve bounded queue capacity before creating a replayable record. A
  failed reservation keeps the existing 429 `QueueFull` response, creates no task id, and leaves no
  durable record. After reservation, allocate the task id, durably append the replayable record, and
  deliver it through the reserved slot; an append failure releases the reservation and leaves no
  replayable record, while any later delivery or commit failure is post-admission, retains the task
  id, and must never be remapped to 429. Ordering remains the channel/log order owned by
  `engine/src/index/write_queue/mod.rs::create_write_queue`; task lifecycle remains in the `TaskInfo`
  map created by `engine/src/index/manager/write.rs::add_documents_inner`; the persisted task id is
  the replay identity and explicit idempotency keys continue to use the existing idempotency cache
  boundary; durable acknowledgment remains `wait_for_write_durable` until an explicit async
  accepted-to-log API is separately designed.
  Commit, oplog, conflict-version, facet-cache, and vector side effects stay in `commit_batch` and
  `finalize_committed_batch`.
- Search latency, memory, and operational complexity: this path has no query fan-in cost and no
  replication topology requirement. Its memory cost is bounded in-memory staging plus disk-backed log
  metadata; operational cost is local disk monitoring and replay recovery. It does not reduce Tantivy
  commit time and will expose lag under overload, so it must surface 429 `QueueFull` before memory
  growth and keep 503 `TooManyConcurrentWrites` for writer-slot failure.
- Incremental first slice: add persistent admission/replay inside the existing write queue boundary:
  reserve bounded queue capacity, persist `WriteOp` metadata and its task id as the admission point,
  deliver through the reserved slot, replay admitted uncommitted records into the same queue on
  startup, and mark records committed only after `finalize_committed_batch`. Recovery must reconcile
  a crash between Tantivy commit and the admission-log completion marker by stable task id so it does
  not repeat commit, oplog, conflict-version, facet-cache, or vector side effects. An incomplete
  trailing append may be discarded only when it was never made replayable; checksum or structural
  corruption of a
  complete replayable record must fail startup before the server accepts writes rather than silently
  dropping the record. Preserve the existing durable wait path. Do not add a separate queue unless a
  future ADR names one owner each for ordering, task lifecycle, replay/idempotency, and durable
  acknowledgment and proves it does not duplicate validation, commit, oplog, or conflict-version
  logic.
- Existing owners extended: `engine/src/index/manager/write.rs::add_documents_inner`,
  `engine/src/index/manager/write.rs::wait_for_write_durable`,
  `engine/src/index/write_queue/mod.rs::create_write_queue`,
  `engine/src/index/write_queue/mod.rs::commit_batch`,
  `engine/src/index/write_queue/finalization.rs::finalize_committed_batch`, and
  `engine/src/index/oplog.rs::append_batch`.
- Rejection: an implementation that only accepts more writes into an unbounded volatile buffer is
  rejected. Burst absorption without bounded lag, durable replay, and explicit backpressure neither
  raises sustained capacity nor meets the bounded-lag goal.

### Alternative 3 — Horizontal scaling through replication ownership

Reuse first: the replication crate already owns peer topology, delivery status, peer HTTP calls, and
catch-up DTOs. `ReplicationManager` tracks per-peer delivery cursors
(`engine/flapjack-replication/src/manager.rs:16`) and is the orchestration owner
(`engine/flapjack-replication/src/manager.rs:39`). It builds `ReplicateOpsRequest` payloads for
peer delivery (`engine/flapjack-replication/src/manager.rs:152`), exposes fire-and-forget
multi-peer replication (`engine/flapjack-replication/src/manager.rs:227`), spawns per-peer sends
(`engine/flapjack-replication/src/manager.rs:243`), exposes a direct peer replication method
(`engine/flapjack-replication/src/manager.rs:251`), supports catch-up
(`engine/flapjack-replication/src/manager.rs:291`), has strict pre-serve catch-up
(`engine/flapjack-replication/src/manager.rs:301`), and centralizes merge complexity in the
catch-up implementation (`engine/flapjack-replication/src/manager.rs:402`). Peer HTTP seams already
cover `/internal/replicate`, `/internal/ops`, and snapshot fetch
(`engine/flapjack-replication/src/peer.rs:74`, `engine/flapjack-replication/src/peer.rs:115`,
`engine/flapjack-replication/src/peer.rs:156`). DTOs already model replicate request, replicate
response, and catch-up response payloads (`engine/flapjack-replication/src/types.rs:5`,
`engine/flapjack-replication/src/types.rs:12`, `engine/flapjack-replication/src/types.rs:26`).

Costing:

- Capacity claim: conditional operator-scale increase when independent tenants or logical shards are
  routed to different authoritative writer nodes. With `M` active writer nodes and balanced ownership,
  the lower-bound proxy is `M * 46.600000` accepted writes/sec across the cluster. This is not a
  single-node or single-index ceiling claim, and it is invalid without a placement/routing owner and a
  same-workload multi-node measurement. Evidence grade: conditional.
- Arithmetic: for `M = 3`, the lower-bound proxy is `3 * 46.600000 = 139.800000` accepted writes/sec
  only if each node receives an independent shard/tenant stream and the client or router sends writes
  to the authoritative node. Replication fan-out after commit does not itself increase the accepting
  node's local commit rate.
- Durability and API compatibility: `ReplicationManager` is the peer delivery/catch-up owner, not the
  public routing owner. Its `replicate_ops` method is fire-and-forget
  (`engine/flapjack-replication/src/manager.rs:227`, `engine/flapjack-replication/src/manager.rs:243`),
  while `replicate_ops_to_peer` can await one peer and update cursor state
  (`engine/flapjack-replication/src/manager.rs:251`). Strict startup catch-up already requires every
  configured peer to answer (`engine/flapjack-replication/src/manager.rs:301`), and catch-up merge
  state is centralized in one owner (`engine/flapjack-replication/src/manager.rs:403`). The public
  Algolia write path still needs a separate authoritative-write routing boundary so accepted task ids,
  conflict-precedence rules and durable ACK semantics do not drift by node.
- Search latency, memory, and operational complexity: this is the highest operational-complexity
  option. It requires placement metadata, stale-route handling, failover, write-authority fencing, and
  observability for lag per peer. It does not add query fan-in unless logical shards also need
  transparent search across nodes, in which case Alternative 1's merge cost is inherited.
- Incremental first slice: add an HTTP/config owner for tenant or shard placement and authoritative
  write routing before changing replication. Current HTTP startup/config surfaces load node identity,
  bind address, and peers through `NodeConfig` (`engine/flapjack-replication/src/config.rs:7`,
  `engine/flapjack-replication/src/config.rs:19`) and store the replication manager in `AppState`
  (`engine/flapjack-http/src/handlers/mod.rs:52`), but no existing owner maps tenant/shard to an
  authoritative write node. That missing boundary is a required cost and should live in HTTP/server
  configuration, not in `flapjack-replication`.
- Existing owners extended: `engine/flapjack-replication/src/manager.rs::ReplicationManager`,
  `replicate_ops`, `replicate_ops_to_peer`, `catch_up_from_peer_with_metadata_strict`,
  `engine/flapjack-replication/src/peer.rs`, and `engine/flapjack-replication/src/types.rs` for peer
  transport/DTOs only.
- Rejection: using replication alone as "horizontal write scaling" is rejected. It moves already
  committed operations and can improve durability/availability, but without authoritative placement
  and routing it cannot raise sustained accepted capacity or prove bounded lag.

## Recommendation

Recommend Alternative 2 first: durable admission/replay inside the existing `write_queue` ownership
boundary, accepted only as a bounded-lag/backpressure slice rather than a numeric sustained-ceiling
uplift. It is the first implementable path because it extends the current write owners directly,
does not require a new query-merge contract, does not require multi-node routing/failover, preserves
the current Algolia-compatible task and error shapes, and keeps durable commit acknowledgment behind
`wait_for_write_durable`.

The sequence is:

1. Implement persistent admission/replay in the existing write-queue path, keeping current synchronous
   durable HTTP ACK semantics. This can ship only if the bounded-lag/backpressure acceptance below
   passes.
2. Run a prerequisite saturation sweep if the next decision needs a throughput-uplift denominator.
   The exact gap is the first offered-load point where the current control produces expected
   backpressure (`write_http_4xx_rate > 0.00%`) without dirty errors. The smallest owner-file change
   remains a parameterized offered-load sweep in the current loadtest harness.
3. Park transparent shard fan-in until a shard-aware extension of the existing federation merge owner
   can prove ranking, pagination, facet, distinct, optional-filter, and analytics semantics.
4. Park horizontal/operator scaling until HTTP/server configuration owns tenant/shard placement and
   authoritative-write routing separately from `flapjack-replication`.

First-slice acceptance:

- Provenance only: Stage 1 evidence is
  `engine/loadtest/results/20260720T170944Z-pl10-v11-single-writer-ceiling/README.md` plus raw stdout
  at `engine/loadtest/results/20260720T170944Z-pl10-v11-single-writer-ceiling/write_soak.stdout.txt`;
  it supplies `5592 / 120 = 46.600000` accepted writes/sec as a lower bound and must not be used as
  an uplift denominator.
- Harness: a future implementation stage should run the existing local write soak harness with an
  offered-load override owned by `engine/loadtest/scenarios/write-soak.js` and
  `engine/loadtest/lib/config.js`, and classify metrics with `engine/loadtest/lib/throughput.js`.
  The unattended command shape is:
  `cd engine/loadtest && FLAPJACK_LOADTEST_BASE_URL=http://127.0.0.1:<port> FLAPJACK_LOADTEST_SOAK_DURATION=2m FLAPJACK_LOADTEST_WRITE_TARGET_RPS=94 run_loadtest_scenario_with_artifacts . write-soak "" "" <results-dir>/write_soak.stdout.txt`.
- Paired-run validity: control and candidate must run on the same machine, same release profile,
  same workload, same seeded data shape, same `FLAPJACK_WRITE_QUEUE_BATCH_SIZE` setting, fresh server
  data directories, fresh process per run, and a 120-second measured interval after a successful
  preflight.
- Measured metric and threshold: at `FLAPJACK_LOADTEST_WRITE_TARGET_RPS=94` or the nearest harness
  representable fixed offered load above `93.200000`, candidate p95 durable-write acknowledgment lag
  must be no worse than control by more than 10%, candidate maximum replay backlog age must stay
  below 30 seconds by the end of the 120-second window, and candidate accepted request count must be
  at least the control accepted count. Expected 429 backpressure is valid only alongside those
  latency, backlog, and accepted-count requirements; it is not an exception to forward progress.
- Minimum valid request count: each paired run must attempt at least `5592` write requests and report
  the write metric denominator explicitly. The control must accept at least `5592` requests, reusing
  the measured lower bound as a non-vacuity floor rather than an uplift denominator; the candidate
  must accept at least as many as that valid control.
- Error contract: both runs must have `write_http_5xx_rate == 0.00%` and
  `write_http_unexpected_4xx_rate == 0.00%`; candidate expected backpressure may appear only as
  429 `QueueFull`, never as 503 `TooManyConcurrentWrites` for queue admission.
- Admission/recovery contract: deterministic automated failpoint tests must stop the process after
  durable admission but before queue delivery, after delivery but before Tantivy commit, and after
  Tantivy commit but before the admission-log completion marker. After restart, every replayable task
  must reach one terminal outcome under its original task id, already committed writes must not repeat
  oplog/conflict-version or other finalization side effects, and a request that received 429 must
  remain absent after drain and restart. A separate known-answer corruption fixture must prove
  that a damaged
  complete replayable record prevents startup from serving writes; missing or indeterminate replay
  evidence is a failure, not a skipped test.
- Conditional disposition: durable queue-first ships if it preserves zero dirty errors and bounded
  lag at the fixed offered load and passes the admission/recovery contract; it parks if it only
  buffers bursts without bounded drain; a new saturation experiment is required before any ADR or
  release note claims a higher sustained writes/sec ceiling.

## Non-goals preserved

This stage is documentation-only internal analysis. It does not edit runtime behavior, engine
source, `ROADMAP.md`, loadtest thresholds, `SOAK_WRITE_THRESHOLDS`,
`FLAPJACK_WRITE_QUEUE_BATCH_SIZE`, replication code, or index-manager code.

Durability is preserved as a design constraint: accepted writes must keep flowing through the current
commit/finalization owners unless a future stage deliberately changes the public write ACK contract
and adds tests that can fail for lost committed data.

Algolia API compatibility is preserved as a design constraint: task ids, write response shapes,
read-only virtual replica behavior, and retryable error contracts remain public surface. In
particular, `QueueFull` 429 and `TooManyConcurrentWrites` 503 must remain distinct client signals.

Direct Tantivy writes outside `write_queue` are not part of this decision. Future work must not
duplicate validation, commit, oplog, or conflict-version ownership outside the existing write
queue/finalization owners. It must not collapse 429 `QueueFull` into 503
`TooManyConcurrentWrites`, and it must not duplicate replication peer delivery, cursor, DTO, or
catch-up logic outside `flapjack-replication`.

No external-system verification applies here. The only external state referenced is the local Stage
1 evidence directory already committed in this repo; this ADR does not deploy, publish, contact
cloud infrastructure, or require secrets.
