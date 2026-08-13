# Decision 0010: OQ4 Cross-Node Idempotency Deduplication
<!-- markdownlint-disable MD013 -->

Date: 2026-07-22
Status: Accepted

## Purpose & Scope

This ADR is the single normative design owner for ADR 0005 OQ4: deduplicating a retried write when failover sends it to a different Flapjack node. This is a design-only decision. It changes no runtime behavior and does not reopen the shipped node-local contract in `engine/docs2/3_IMPLEMENTATION/decisions/active/0005_nginx_restart_window_write_recovery.md`: `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`, the `application_id + index_segment + idempotency_key` scope, the `x-flapjack-idempotency-key` header, and the `lookup_scoped` / `store_scoped` API remain owned by `engine/flapjack-http/src/idempotency.rs`.

The current seams do not provide the cross-node guarantee. `IdempotencyCache::canonical_db_path` selects a node-local file, and `lookup_scoped` and `store_scoped` operate on that local store (`engine/flapjack-http/src/idempotency.rs:352-448`). Object handlers look up before mutation but store the replay response only after a successful mutation (`engine/flapjack-http/src/handlers/objects/mod.rs:299-347`, `:927-1012`). Normal peer fan-out is expressly fire-and-forget (`engine/flapjack-replication/src/manager.rs:333-356`); the awaited `replicate_ops_to_peer` and pull catch-up seam transport mutation-log entries (`engine/flapjack-replication/src/manager.rs:358-409`), while `ReplicateOpsRequest` contains `tenant_id` and `Vec<OpLogEntry>` (`engine/flapjack-replication/src/types.rs:5-10`). These are reuse evidence, not synchronous cross-node single-execution proof.

Every option is judged first on correctness, then on two project lenses scored from 1 (poor) to 5 (best): bounded operational complexity and low replication-topology coupling. An option that cannot satisfy the invariant is ineligible regardless of score.

## The Failover Window

The promised idempotency window begins when any serving node durably accepts the first claim for a scoped key and lasts for the configured idempotency TTL measured from terminal result persistence. A completed result may be removed only after that TTL. An unresolved `Pending` or `Indeterminate` record must not age into permission to execute again merely because the normal replay TTL elapsed; it remains fail-closed until reconciliation proves a terminal result or an operator-authorized resolution policy handles it.

The contract assumes a named coordination cohort in one durable membership epoch. Before serving coordinated writes, a node knows the cohort and epoch. Membership changes use an explicit old/new-epoch transition that preserves intersecting decision quorums; a node with stale, absent, or conflicting membership cannot coordinate a write. A majority of the epoch's cohort must be durably reachable. This sacrifices write availability during a partition rather than allowing both sides to execute. Current runtime peer membership alone does not meet this assumption because its mutation fan-out has no claim quorum or membership-epoch contract.

The window includes overlapping requests, not only a retry after node loss. All requests with the same `(application_id, index_segment, idempotency_key)` compete for one record. The first durable claim wins. A concurrent request observing `Pending` or unable to prove the winning claim conservatively receives a retryable conflict/unavailable response and performs no mutation. A request observing `Completed` replays the stored status, byte-equivalent body, and task ID.

## Single-Execution Invariant

For one scoped key during the promised window, at most one underlying mutation may execute across the coordination cohort, and every request arriving after completion must replay the same HTTP status, response body, and task ID. The required state machine is:

1. **Claim/reservation:** before entering any mutation owner, persist `Pending(key, claim_id, membership_epoch, fence, created_at)` on a decision quorum. Quorum compare-and-set permits one claim from `Absent`; intersecting quorums and monotonically ordered fences reject competitors.
2. **Mutation commit:** only the holder of the quorum-proven current claim and fence may invoke the underlying write. Every mutation entry point must call the same coordinator boundary; handlers must not implement their own election policy.
3. **Result persistence:** after the mutation outcome and task ID are known, persist `Completed(key, claim_id, status, body, task_id, completed_at)` on a decision quorum. A terminal failure that proves no mutation executed may be recorded distinctly and handled by the future public contract; uncertainty is never translated to `Absent`.
4. **Peer durability and public acknowledgment:** a success response is legal only after the completed record satisfies the option's durability rule. Local persistence or initiating fire-and-forget `ReplicationManager::replicate_ops` is insufficient. If quorum persistence cannot be proved, return a conservative retryable error and retain `Pending` or `Indeterminate`.
5. **Node loss and retry:** after loss before mutation, another node sees the durable pending claim and does not execute. After loss during or after mutation but before completed-result quorum, another node treats the record as `Indeterminate` and does not execute. After completed-result quorum, another node replays the stored result even if the original node is gone.
6. **TTL expiry:** delete only a completed record whose terminal-result TTL elapsed. Expiry of a pending/indeterminate record cannot prove the mutation did not commit and therefore cannot authorize another execution.

This closes the dangerous crash gap by refusing to turn uncertainty into a second mutation. It does not promise that every accepted claim returns success: a crash after mutation commit but before result durability can leave the client without a replayable success until reconciliation. The safety promise is at-most-once execution; availability under uncertainty is deliberately lower.

## Design Options

### Option A — Quorum-Durable In-Binary Coordination Record

#### Flow and ordering

- Add a cluster idempotency record, separate from `OpLogEntry`, and propagate it through an awaited extension of the existing authenticated peer channel.
- A `ClusterIdempotencyCoordinator` obtains a majority compare-and-set reservation in the fixed membership epoch before calling the mutation owner.
- The coordinator alone converts the claim to `Completed` and waits for a majority durable acknowledgment before permitting the handler to return success.
- Intersecting majority quorums ensure overlapping claims cannot both acquire a valid fence; handlers receiving `Pending` wait only for a bounded interval and then return a retryable response.
- The node-local `IdempotencyCache` may remain a fast replay cache, but it is not evidence that cluster durability completed.

#### Failure and durability semantics

- A network minority cannot claim or acknowledge and therefore cannot execute; this is fail-closed partition behavior.
- Loss before claim quorum means no accepted claim and no mutation. Loss after claim quorum but before mutation leaves a recoverable `Pending` record and no new execution.
- Loss after mutation starts but before completion quorum leaves `Indeterminate`; retries reject/defer until reconciliation, even beyond normal TTL.
- Loss after completion quorum permits any majority-connected successor to read and replay the exact stored result.
- Normal `ReplicationManager::replicate_ops` remains fire-and-forget and cannot satisfy any reservation or acknowledgment precondition; the new awaited record protocol must have its own durable acknowledgments and recovery scan.

#### Operational tradeoffs

- Adds no external service and can reuse peer authentication, addressing, retry/circuit-breaker primitives, and delivery observability.
- Adds one small durable record family, quorum latency before mutation and before success acknowledgment, plus compaction for completed TTLs and reconciliation for indeterminate claims.
- Write availability requires a majority and a coherent membership epoch. Topology changes are therefore visible to the protocol, but ordinary object-mutation `OpLogEntry` sequencing remains unchanged.
- Recovery is bounded for completed records; indeterminate records intentionally require positive reconciliation rather than unsafe timeout release.

#### Ownership and lens score

- Existing owner reused: `engine/flapjack-http/src/idempotency.rs` remains the sole node-local cache and scoped-key owner.
- Existing owner reused: `flapjack-replication` remains the peer transport, membership snapshot, and delivery-cursor owner.
- New justified boundary: `ClusterIdempotencyCoordinator` owns cluster claim/result policy so object handlers do not duplicate state transitions and mutation `OpLogEntry` does not acquire a second, unrelated coordination meaning.
- Bounded operational complexity: **3/5** — material protocol and durable-state work, but no separately operated dependency.
- Low replication-topology coupling: **3/5** — depends on membership epochs and quorums, but keeps coordination records independent of mutation-log sequence topology.
- Correctness eligibility: **pass**, only with quorum reservation before mutation, quorum completion before success acknowledgment, and fail-closed indeterminate handling.

### Option B — Authoritative Owner with Fenced Routing

#### Flow and ordering

- Map each scoped key to one authoritative node in a membership epoch; every other node forwards the request or returns a redirect/retryable response without mutating.
- The authority durably reserves locally before mutation and persists the completed response before acknowledgment.
- Failover requires a membership authority to assign a higher fencing epoch and to prove the former owner can no longer enter the mutation path before the successor serves the key.
- Overlapping requests serialize at the authority. A successor with no conclusive terminal record marks the claim indeterminate rather than re-executing.
- Result transfer to the successor must complete before the routing owner declares failover ready.

#### Failure and durability semantics

- A healthy authority gives one-node latency and a simple serialization point.
- A partitioned non-owner cannot execute. A partitioned old owner also must be fenced from mutation; routing preference without an enforced fence is not sufficient.
- Loss before durable transfer blocks the key until the old owner recovers or reconciliation proves the outcome. Loss after transfer permits replay by the successor.
- Public success requires the result to be durable on the authority and on the designated failover target, or in an equivalent fenced handoff record, before acknowledgment.
- TTL cleanup occurs at the authority and follows ownership transfer; pending/indeterminate records cannot be discarded by timeout.

#### Operational tradeoffs

- Fast in the stable case and stores one authoritative copy plus a failover copy instead of writing a majority on every state transition.
- Availability depends on routing, lease/fence issuance, and clean ownership transfer. The current in-memory peer list and fire-and-forget mutation fan-out do not supply those controls.
- Rebalancing or peer churn moves key authority and replay state, directly coupling idempotency availability to topology transitions.
- Building a trustworthy lease or fencing authority approaches a new coordination system even though no external datastore is named.

#### Ownership and lens score

- `engine/flapjack-http/src/idempotency.rs` still owns the node-local cache, while a new authoritative-routing coordinator owns key placement, fences, handoff, and terminal-result transfer.
- `flapjack-replication` can transport handoff records but cannot own HTTP idempotency policy or infer fencing from `OpLogEntry` cursors.
- Bounded operational complexity: **3/5** — efficient steady state, offset by a mandatory fence/lease and ownership-transfer control plane.
- Low replication-topology coupling: **1/5** — correctness and availability change directly with ownership maps, rebalancing, and node fencing.
- Correctness eligibility: **pass only if** the former authority is mutation-fenced before promotion and success is durable on the failover path; simple consistent-hash routing is ineligible.

### Option C — External Shared Store or Consensus Service

#### Flow and ordering

- Use a linearizable external store or consensus service as the authoritative owner of claim compare-and-set, fencing token, terminal result, and TTL metadata.
- Every node performs an external `Absent -> Pending` compare-and-set before mutation; only the returned current fence may enter the mutation path.
- After mutation, the node writes the exact completed result with a conditional update and waits for the service's durable acknowledgment before responding success.
- Overlapping requests read one linearizable record: completed requests replay, while pending/indeterminate requests reject or wait for a bounded interval.
- Flapjack peer replication carries mutations as today but is not part of the idempotency safety proof.

#### Failure and durability semantics

- Store unavailability or loss of quorum stops coordinated writes before mutation; a partitioned Flapjack node cannot make a local claim.
- A crash after claim but before mutation leaves pending. A crash after mutation but before result persistence leaves indeterminate and blocks re-execution.
- A completed durable record is replayable from any node independent of which Flapjack peers survive.
- The service must provide linearizable conditional writes, fencing, durable reads, and retention semantics; an eventually consistent cache is ineligible.
- TTL applies only to completed records. Indeterminate cleanup requires reconciliation or an explicit operator policy.

#### Operational tradeoffs

- Provides the clearest arbitration primitive and decouples idempotency records from Flapjack replication topology.
- Adds a mandatory highly available service, credentials, monitoring, backups, capacity planning, upgrade procedures, and another network round trip on every coordinated write state transition.
- Flapjack write availability becomes bounded by the external service's availability and latency.
- Recovery tooling can query one authoritative record set, but the service becomes an additional production data owner.

#### Ownership and lens score

- `engine/flapjack-http/src/idempotency.rs` remains the node-local cache owner; a new coordinator adapter owns external claim/result calls, and the external system owns cluster durability.
- `flapjack-replication` remains mutation transport/cursor owner and is intentionally outside the claim proof.
- Bounded operational complexity: **1/5** — strongest primitive at the cost of a new mandatory HA dependency and its lifecycle.
- Low replication-topology coupling: **5/5** — claim correctness is independent of Flapjack peer layout.
- Correctness eligibility: **pass**, provided the service contract is linearizable and failures are fail-closed; a non-linearizable shared cache fails.

## Recommendation

Adopt **Option A**, the quorum-durable in-binary coordination record, for the future implementation lane. Its combined project-lens score is 6/10, versus Option B's 4/10 and Option C's 6/10. Option A and C tie numerically; Option A wins the stated bounded-operational-complexity priority because it avoids making a second HA system mandatory while retaining a middle score for topology coupling. Option B's steady-state simplicity does not offset its direct dependence on correct routing, fencing, and ownership transfer.

Option A is accepted only as the complete protocol described here, not as “send the local cache entry to peers.” It closes every invariant timeline because a quorum reservation precedes mutation, intersecting quorums exclude overlapping winners, a quorum-completed record precedes public success, and every uncertain post-mutation state rejects re-execution. If a future implementation cannot supply durable membership epochs, quorum compare-and-set/fencing, and conservative indeterminate recovery, it must return to this ADR rather than weakening the guarantee or silently falling back to fire-and-forget replication.

The future contract is `ClusterIdempotencyCoordinator`: given the existing scoped key, it returns one of `Acquired(claim_id, fence)`, `Replay(record)`, or `PendingOrIndeterminate(retry_contract)`; only `Acquired` may call a mutation, and a separate completion operation returns only after quorum durability. This boundary prevents policy duplication across object handlers and avoids overloading mutation `OpLogEntry` ownership. This ADR does not implement the contract, change the local SQLite schema/API, or add infrastructure.

## Relationship to Sibling ADRs

- `engine/docs2/3_IMPLEMENTATION/decisions/active/0005_nginx_restart_window_write_recovery.md` remains the normative owner for OQ1/OQ2/OQ3: scoped keys, node-local restart persistence, and multi-index envelope replay. ADR 0010 resolves only its deferred OQ4 cross-node behavior.
- ADR 0008 is absent at this decision's execution HEAD. The existing `MIG-5` row in `ROADMAP.md` requires a durable incarnation epoch, exclusive promotion, and drain before overwrite. That mutation-fence contract constrains OQ4: a cluster idempotency fence must not authorize a mutation against an index generation that MIG-5 has fenced or replaced.
- ADR 0009 is absent at this decision's execution HEAD. The existing `MIG-7` row in `ROADMAP.md` records that publication is node-local, oplogs lack a convergence epoch, moves reset sequence space, and snapshot install is pull-only. OQ4 therefore cannot infer a coordination decision from current HA convergence or mutation-log sequence alone; its record and membership epoch remain explicit.

These are constraints, not copied sibling decisions. ADR 0010 is the sole normative owner of the cross-node claim/result policy.

## Non-goals

- Changing `${FLAPJACK_DATA_DIR}/_idempotency/cache.db`, scoped-key composition, header spelling, `lookup_scoped`, or `store_scoped`.
- Implementing `ClusterIdempotencyCoordinator`, a new schema, peer endpoint, membership epoch, or runtime feature in this design-only lane.
- Making mutation `OpLogEntry` the idempotency claim record or treating fire-and-forget fan-out as durability.
- Redesigning nginx retry policy, Algolia-compatible response shapes, task IDs, write queues, migration publication, snapshot convergence, or peer discovery.
- Promising progress during a partition or automatic re-execution after an indeterminate mutation.

## Deferred Items

- Choose the durable record encoding, on-disk location, authenticated peer API, and quorum acknowledgment payload.
- Specify membership-epoch persistence and safe joint transition using the cluster membership work available at implementation time.
- Define the exact retryable HTTP status, `Retry-After` behavior, bounded waiter policy, and observability for pending/indeterminate claims without changing Algolia compatibility accidentally.
- Define reconciliation evidence for proving `Pending -> safe terminal failure` or recovering an exact completed status/body/task ID after a post-mutation crash.
- Establish TTL/compaction limits, bounded memory/disk behavior, degraded-majority alerts, and disaster-recovery procedures.
- Add contract tests for overlapping requests, every crash point in the invariant, partitions, stale membership epochs, TTL expiry, result byte equality, and handler coverage before runtime implementation.

Open questions for this design decision: none. The deferred items are implementation choices constrained by the accepted invariant and recommendation.
<!-- markdownlint-enable MD013 -->
