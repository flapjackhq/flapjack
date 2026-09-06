// Canonical test-only contract for the closed internal-route denominator.
//
// Both the route-ACL suite and the middleware suite import this module. Keeping
// the mounted pattern, served specimen, tier decision, and high-risk mutation
// flag in one row prevents either suite from silently omitting a classified
// route.

use axum::http::Method;

pub(crate) const MOUNTED_INTERNAL_ROUTE_COUNT: usize = 25;
pub(crate) const PEER_ALLOWED_ROUTE_COUNT: usize = 8;
pub(crate) const ADMIN_ONLY_ROUTE_COUNT: usize = 17;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InternalRouteTier {
    PeerAllowed,
    AdminOnly,
}

/// A decided row of the closed internal-route rule set.
pub(crate) struct InternalRouteRow {
    pub(crate) method: Method,
    /// The Axum pattern as mounted in `build_internal_routes`.
    pub(crate) mounted_pattern: &'static str,
    /// A concrete served path. The ACL mapper receives request paths, never
    /// Axum pattern strings, so dynamic routes need substituted specimens.
    pub(crate) specimen_path: &'static str,
    pub(crate) tier: InternalRouteTier,
    /// Marks the three administrative mutations called out by the stage's
    /// high-risk refusal contract without maintaining a second specimen list.
    pub(crate) high_risk_mutation: bool,
    pub(crate) rationale: &'static str,
}

pub(crate) const INTERNAL_ROUTE_CONTRACT: &[InternalRouteRow] = &[
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/status",
        specimen_path: "/internal/status",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "replication liveness read; carries no administrative authority",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/cluster/status",
        specimen_path: "/internal/cluster/status",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "membership read a replica needs to locate its primary",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/snapshots/capability",
        specimen_path: "/internal/snapshots/capability",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "capability probe a replica issues before requesting a snapshot",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/replicate",
        specimen_path: "/internal/replicate",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "the replication transport itself; refusing it breaks HA convergence",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/ops",
        specimen_path: "/internal/ops",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "oplog pull is the core replica read path",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/tenants",
        specimen_path: "/internal/tenants",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "replica enumerates tenants to drive per-tenant catch-up",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/snapshot/:tenantId",
        specimen_path: "/internal/snapshot/baseline_tenant",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "snapshot fetch for initial sync; read-only per tenant",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/analytics-rollup",
        specimen_path: "/internal/analytics-rollup",
        tier: InternalRouteTier::PeerAllowed,
        high_risk_mutation: false,
        rationale: "peer-to-peer analytics fan-in; part of the replication mesh",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/cluster/peers",
        specimen_path: "/internal/cluster/peers",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: true,
        rationale: "re-shapes runtime membership and can persist attacker-chosen origins",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/build-info",
        specimen_path: "/internal/build-info",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale:
            "exact build provenance is an operator attestation surface, not peer or public data",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/indexes/:indexName/count",
        specimen_path: "/internal/indexes/baseline_index/count",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "cross-tenant operational probe; no replication transport requires it",
    },
    InternalRouteRow {
        method: Method::DELETE,
        mounted_pattern: "/internal/cluster/peers/:node_id",
        specimen_path: "/internal/cluster/peers/bogus-peer",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: true,
        rationale: "membership removal can partition the cluster",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/rollup-cache",
        specimen_path: "/internal/rollup-cache",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "operator diagnostics across every tenant; no replication need",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/storage",
        specimen_path: "/internal/storage",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "whole-node storage inventory; operator surface",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/storage/:indexName",
        specimen_path: "/internal/storage/baseline_index",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "per-index storage inventory; operator surface",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/pause/:indexName",
        specimen_path: "/internal/pause/baseline_index",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "pausing an index is a write-availability control, not replication",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/resume/:indexName",
        specimen_path: "/internal/resume/baseline_index",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "resume is the paired availability control; same tier as pause",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/rotate-admin-key",
        specimen_path: "/internal/rotate-admin-key",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: true,
        rationale: "issues a new administrative credential; the worst peer escalation",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/crawler/runs",
        specimen_path: "/internal/crawler/runs",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "PBV4 crawler start is node-admin-only and remains hidden until runtime proof",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/crawler/runs/:run_id",
        specimen_path: "/internal/crawler/runs/018f3e2a-7b1c-7d45-8c90-1234567890ab",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale:
            "durable crawler outcome may expose physical publication facts only to node admin",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/crawler/runs/:run_id/cancel",
        specimen_path: "/internal/crawler/runs/018f3e2a-7b1c-7d45-8c90-1234567890ab/cancel",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "crawler cancellation controls server work and publication eligibility",
    },
    InternalRouteRow {
        method: Method::POST,
        mounted_pattern: "/internal/crawler/runs/:run_id/ack",
        specimen_path: "/internal/crawler/runs/018f3e2a-7b1c-7d45-8c90-1234567890ab/ack",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "crawler acknowledgement changes durable replay-retention state",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/recommendations/analytics",
        specimen_path: "/internal/recommendations/analytics",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "PBV5 recommendation telemetry spans tenant data and remains node-admin-only",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/fault/sleep",
        specimen_path: "/internal/fault/sleep",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "fault-injection stalls a worker; test-build operator surface only",
    },
    InternalRouteRow {
        method: Method::GET,
        mounted_pattern: "/internal/fault/panic",
        specimen_path: "/internal/fault/panic",
        tier: InternalRouteTier::AdminOnly,
        high_risk_mutation: false,
        rationale: "fault-injection aborts the process; test-build operator surface only",
    },
];

pub(crate) fn peer_allowed_routes() -> impl Iterator<Item = &'static InternalRouteRow> {
    INTERNAL_ROUTE_CONTRACT
        .iter()
        .filter(|row| row.tier == InternalRouteTier::PeerAllowed)
}

pub(crate) fn admin_only_routes() -> impl Iterator<Item = &'static InternalRouteRow> {
    INTERNAL_ROUTE_CONTRACT
        .iter()
        .filter(|row| row.tier == InternalRouteTier::AdminOnly)
}

pub(crate) fn high_risk_admin_mutations() -> impl Iterator<Item = &'static InternalRouteRow> {
    INTERNAL_ROUTE_CONTRACT
        .iter()
        .filter(|row| row.high_risk_mutation)
}
