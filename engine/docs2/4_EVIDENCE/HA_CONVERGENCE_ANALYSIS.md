# HA Convergence Analysis: Theory Selection and Contract Decision

> **Superseded (2026-05-26) — historical record, preserved unchanged.** This is a research
> deliverable scoped to the **Mar 30, 2026** soak; its theory analysis and "architectural
> boundary" conclusion are intentionally retained as a record of that run and are not
> rewritten. Superseding context after the L1 anti-entropy fix (`066549d5`) is routed
> through ADR
> [`0004`](../3_IMPLEMENTATION/decisions/active/0004_ha_convergence_reversal.md); for current
> canonical evidence see the **HA Soak Proof (May 26, 2026)** section of
> [`engine/loadtest/BENCHMARKS.md`](../../loadtest/BENCHMARKS.md).

Research deliverable for Stage 2 of the HA convergence contract sync.
Canonical evidence source: `engine/loadtest/BENCHMARKS.md` (HA Soak Proof, Mar 30, 2026).

## 1. Retained Facts (from BENCHMARKS.md)

| Field | Value |
|---|---|
| Final classification | `warning-findings` |
| Convergence result | `diverged` |
| Restart count | 39 (across 3 nodes, 2h) |
| Restart interval | 180s |
| Write routing | nginx round-robin to node-a, node-b, node-c |
| Per-node final doc counts | 65,323 / 67,309 / 66,724 |
| Max divergence | ~1,986 docs (~3% of highest count) |
| k6 exit code | 99 (threshold breach under sustained overload) |
| All nodes healthy throughout | Yes |

## 2. Theory Mapping

### Theory A: nginx write routing drops writes during node restarts

**Claim:** nginx fails to reroute writes when a target node is restarting, causing
permanent write loss.

**Evidence against:** nginx.conf configures `proxy_next_upstream error timeout http_502
http_503 http_504` with `proxy_next_upstream_tries 3`. When a node is fully down
(connection refused), nginx retries on another upstream. The write reaches a live node.

**Partial truth:** There is a brief window during node shutdown where nginx has already
routed a request but the node dies mid-processing. The write may be committed to the
dying node's oplog but the HTTP response is lost, causing nginx to retry on another node
(potential duplicate with different auto-generated IDs). However, this window is narrow
and doesn't account for the full ~3% divergence.

**Verdict:** Contributing factor, not root cause.

### Theory B: FLAPJACK_STARTUP_CATCHUP_STRICT=0 allows stale serving

**Claim:** Restarted nodes serve traffic before fully catching up, permanently missing ops.

**Evidence:** All three nodes in docker-compose.yml set
`FLAPJACK_STARTUP_CATCHUP_STRICT: "0"`. With strict=0, `run_pre_serve_catchup`
(startup_catchup.rs:40-68) logs a warning and continues even when catch-up fails or
times out. The node starts serving with potentially stale data.

**Partial truth:** Non-strict mode means the node starts serving earlier, but periodic
sync (`spawn_periodic_sync`, default 60s interval) continues pulling from peers. The
initial staleness should be temporary. The persistent divergence must come from ops
that no peer has available to serve during catchup.

**Verdict:** Contributing factor (widens the window), not root cause.

### Theory C: Fire-and-forget replication drops ops permanently

**Claim:** `replicate_ops` (manager.rs:109-154) spawns fire-and-forget tokio tasks per
peer with a single retry after 2s. When both attempts fail (peer is down), the ops are
silently dropped from the replication pipeline.

**Evidence:** The code:
```rust
tokio::spawn(async move {
    let result = peer.replicate_ops(req.clone()).await;
    let result = match result {
        Ok(resp) => Ok(resp),
        Err(e) => {
            tokio::time::sleep(Duration::from_secs(2)).await;
            peer.replicate_ops(req).await
        }
    };
    match result {
        Ok(resp) => { /* update cursor */ }
        Err(e) => {
            tracing::warn!("[REPL] peer {} failed after retry, ops dropped: {}", ...);
        }
    }
});
```

Dropped ops are logged but not tracked or re-queued. The only recovery path is
pull-based catchup, which requires the writing node to still have the ops in its oplog
when the peer eventually runs catchup.

**Why pull-based catchup doesn't fully recover:**
- `catch_up_from_peer_with_metadata` tries peers in order and stops at the first success
- With 3 nodes and round-robin writes, node-C catching up from node-A won't get ops
  that were written directly to node-B but not yet replicated to node-A
- Periodic sync (60s default) eventually reaches node-B too, but under continuous 180s
  restart rotation, the timing windows compound

**Verdict:** Proximate mechanism of the divergence. This is a design choice (async
replication for write latency), not a bug.

### Theory D: Architectural boundary — async replication cannot guarantee convergence under sustained rolling restarts

**Claim:** The ~3% divergence is an inherent property of the system's async replication
model combined with the nginx-routed example topology under the tested stress pattern.

**Evidence chain:**
1. nginx round-robin distributes writes across all 3 nodes
2. Each node commits locally and fires async replication to peers (Theory C mechanism)
3. When a peer is down (restarting), replication drops after 1 retry
4. The dropped ops exist only in the writing node's oplog
5. Pull-based catchup (startup + periodic) recovers most but not all ops because:
   - It contacts one peer at a time, not all peers
   - Under continuous restart rotation, the "one peer" may itself be missing ops
   - The 60s periodic sync interval vs 180s restart interval means some windows
     are not fully covered
6. Over 39 restarts across 2h, the cumulative effect is ~3% divergence

**This is NOT a single fixable config/code bug.** Making the divergence zero would
require one or more of:
- Synchronous replication (kills write latency)
- Write-ahead delivery log with guaranteed at-least-once delivery
- Client-side retry with dedup (changes the API contract)
- Anti-entropy protocol (merkle trees, read repair)

Each of these is a significant architectural change, not a Stage 3 patch.

**Verdict: Selected theory.** The divergence is a product boundary, not a defect.

## 3. Code/Config Seam Trace

| Seam | File | Function/Config | Role in Divergence |
|---|---|---|---|
| Write routing | `engine/examples/ha-cluster/nginx.conf:63` | `proxy_next_upstream error timeout http_502 http_503 http_504` | Routes around dead nodes; doesn't prevent mid-flight write loss during shutdown |
| Strict mode | `engine/examples/ha-cluster/docker-compose.yml:25,45,65` | `FLAPJACK_STARTUP_CATCHUP_STRICT: "0"` | Allows serving before full catchup (widens window) |
| Push replication | `engine/flapjack-replication/src/manager.rs:109-154` | `replicate_ops()` | Fire-and-forget with 1 retry; drops ops on double failure |
| Pull catchup (startup) | `engine/flapjack-http/src/startup_catchup.rs:40-68` | `run_pre_serve_catchup()` | Contacts peers once; non-strict mode ignores failures |
| Pull catchup (periodic) | `engine/flapjack-http/src/startup_catchup.rs:163-178` | `spawn_periodic_sync()` | 60s interval; contacts one peer per tenant per cycle |
| Catchup logic | `engine/flapjack-http/src/startup_catchup.rs:187-235` | `catchup_all_tenants()` | Iterates tenants, pulls ops from first responsive peer |
| Single-peer fetch | `engine/flapjack-replication/src/manager.rs:169-210` | `catch_up_from_peer_with_metadata()` | Tries peers in order, returns after first success; may miss ops only on other peers |

## 4. Test Surface Selection

**Selected surface:** `engine/loadtest/tests/ha_soak_acceptance.sh`

**Rationale:**
- Already validates harness structural properties (function existence, env defaults,
  artifact naming, classification output format)
- Can be extended with a doc-truth assertion that verifies `engine/examples/ha-cluster/README.md`
  acknowledges the known divergence boundary
- Deterministic (grep-based structural check, no timing dependency)
- Directly tests the "contract gap": docs don't match evidence

**Rejected alternatives:**
- `test_replication.rs`: The divergence requires 3-node round-robin write routing with
  timing-dependent replication drops. A unit test would be non-deterministic or would
  need an unrealistically complex fixture. The existing 2-node test
  (`test_restart_catches_up_before_serving`) correctly proves what it claims.
- `test_ha.sh` / `ha_topology_acceptance.sh`: These test the fast topology proof (single
  restart, small doc set). The soak divergence only manifests under sustained load with
  many restarts. Extending these would blur their scope.

## 5. Go/No-Go Decision

**GO for boundary path (Theory D).**

**Decision:** The Mar 30 soak divergence is an architectural boundary, not a fixable
config/runtime defect. Stage 3 should sharpen documentation to acknowledge this boundary
explicitly, not attempt to fix the replication model.

**Contract:** `engine/examples/ha-cluster/README.md` must acknowledge the known soak
divergence boundary and reference `engine/loadtest/BENCHMARKS.md` for evidence. At
current HEAD, the README describes catch-up behavior optimistically without mentioning
the limitation. The RED check in `ha_soak_acceptance.sh` will assert this acknowledgment
exists.

**Improvement path blocked because:** Current HEAD cannot be made RED by a narrowly
scoped runtime behavior change. The divergence is structural — it requires architectural
changes (sync replication, delivery guarantees, or anti-entropy protocols) that are
outside the scope of this task.

## 6. Open Questions

1. **Periodic sync effectiveness:** The 60s periodic sync should eventually converge all
   nodes if all oplogs are retained. The persistent divergence suggests either: (a) some
   ops are genuinely lost (oplog truncation before sync), or (b) the 2h soak with 180s
   restart intervals doesn't allow enough sync cycles for full recovery. A longer
   post-soak convergence window (>120s) might reduce the divergence. This is a tuning
   question, not a design question.

2. **Acceptable divergence threshold:** The ~3% divergence over 39 restarts in 2h may be
   acceptable for many use cases. If a threshold (e.g., <5% divergence is "expected
   behavior") were formalized, the harness could classify results as PASS/WARNING/FAIL
   with finer granularity. This is a product decision for a future iteration.

3. **Client retry guidance:** If clients retry writes that timed out at the LB layer,
   they may create duplicate documents (different auto-generated IDs). Adding an
   idempotency key or client-side dedup guidance would reduce this vector. This is
   outside Stage 2-3 scope but relevant for production HA documentation.
