//! Stage 1 RED contract for the replication peer-vs-admin boundary.
//!
//! This module locks the *closed* `/internal/*` denominator mounted by
//! `router.rs::build_internal_routes` and pins each route to a peer-allowed or
//! admin-only decision. `route_acl.rs` stays the single owner of route→tier
//! mapping; this table is its exhaustive contract, not a second mapper.
//!
//! The peer-allowed rows assert the tier Stage 2 introduced. The admin-only rows
//! and the unmatched fall-through are GREEN and must stay that way — a
//! permissive fall-through is how a future administrative route silently becomes
//! peer-reachable.

use super::{required_acl_for_route, RouteAcl};
use axum::http::Method;
use std::collections::BTreeSet;

use crate::auth::tests::peer_boundary_route_contract::{
    admin_only_routes, peer_allowed_routes, InternalRouteTier, ADMIN_ONLY_ROUTE_COUNT,
    INTERNAL_ROUTE_CONTRACT, MOUNTED_INTERNAL_ROUTE_COUNT, PEER_ALLOWED_ROUTE_COUNT,
};

/// Router source, read at compile time so the denominator cannot drift away
/// from the live router without this test failing.
const ROUTER_SOURCE: &str = include_str!("../router.rs");

/// The one public route in `build_internal_routes`. Excluded from the internal
/// denominator on purpose: it is served before any credential is required.
const ACME_CHALLENGE_SPECIMEN: &str = "/.well-known/acme-challenge/stage1-token-specimen";

/// Collect every `"/internal/..."` string literal mounted in `router.rs`.
fn mounted_internal_route_literals() -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (index, _) in ROUTER_SOURCE.match_indices("\"/internal/") {
        let after_quote = &ROUTER_SOURCE[index + 1..];
        let Some(end) = after_quote.find('"') else {
            continue;
        };
        found.insert(after_quote[..end].to_string());
    }
    found
}

fn mounted_internal_method_patterns() -> BTreeSet<(String, String)> {
    let mut found = BTreeSet::new();
    for row in INTERNAL_ROUTE_CONTRACT {
        let route_literal = format!("\"{}\"", row.mounted_pattern);
        let Some(route_start) = ROUTER_SOURCE.find(&route_literal) else {
            panic!(
                "router.rs::build_internal_routes no longer mounts {}",
                row.mounted_pattern
            );
        };
        let after_route = &ROUTER_SOURCE[route_start + route_literal.len()..];
        let Some(next_route_offset) = after_route.find(".route(") else {
            panic!(
                "router.rs::build_internal_routes has no next route after {}",
                row.mounted_pattern
            );
        };
        let route_block = &after_route[..next_route_offset];
        for method_name in ["get", "post", "delete"] {
            let needle = format!("{method_name}(");
            if route_block.contains(&needle) {
                found.insert((
                    method_name.to_ascii_uppercase(),
                    row.mounted_pattern.to_string(),
                ));
            }
        }
    }
    found
}

/// The denominator is only meaningful if it matches the live router. This fails
/// the moment someone mounts a new `/internal/*` route without deciding its
/// tier, which is exactly the drift that turns a closed rule set into an open one.
#[test]
fn internal_route_table_covers_every_route_mounted_by_the_router() {
    let contract = INTERNAL_ROUTE_CONTRACT;
    let table_patterns: BTreeSet<String> = contract
        .iter()
        .map(|row| row.mounted_pattern.to_string())
        .collect();
    let router_patterns = mounted_internal_route_literals();

    assert_eq!(
        table_patterns, router_patterns,
        "closed internal-route denominator drifted from router.rs::build_internal_routes"
    );
    assert_eq!(
        contract.len(),
        MOUNTED_INTERNAL_ROUTE_COUNT,
        "denominator changed: expected {MOUNTED_INTERNAL_ROUTE_COUNT} decided internal routes"
    );
    assert_eq!(
        contract
            .iter()
            .filter(|row| row.tier == InternalRouteTier::PeerAllowed)
            .count(),
        PEER_ALLOWED_ROUTE_COUNT,
        "peer-allowed subset size changed"
    );
    assert_eq!(
        contract
            .iter()
            .filter(|row| row.tier == InternalRouteTier::AdminOnly)
            .count(),
        ADMIN_ONLY_ROUTE_COUNT,
        "admin-only subset size changed"
    );
    assert!(
        contract.iter().all(|row| !row.rationale.is_empty()),
        "every decided route must carry a one-line rationale"
    );
}

/// The contract is method-sensitive. A route moved from GET to POST (or the
/// reverse) changes what replicas and operators can actually call even if the
/// path string stays the same, so pattern-only coverage is not enough.
#[test]
fn internal_route_table_covers_mounted_methods() {
    let table_method_patterns: BTreeSet<(String, String)> = INTERNAL_ROUTE_CONTRACT
        .iter()
        .map(|row| {
            (
                row.method.as_str().to_string(),
                row.mounted_pattern.to_string(),
            )
        })
        .collect();

    assert_eq!(
        table_method_patterns,
        mounted_internal_method_patterns(),
        "closed internal-route denominator drifted in method or path"
    );
}

/// Dynamic rows must assert against served paths. A row whose specimen still
/// contains `:` would pass while the real request path fails classification.
#[test]
fn dynamic_route_rows_assert_against_concrete_served_paths() {
    let mut dynamic_rows = 0;
    for row in INTERNAL_ROUTE_CONTRACT {
        assert!(
            !row.specimen_path.contains(':'),
            "specimen for {} must be a served path, not an Axum pattern: {}",
            row.mounted_pattern,
            row.specimen_path
        );
        if row.mounted_pattern.contains(':') {
            dynamic_rows += 1;
            assert_ne!(
                row.specimen_path, row.mounted_pattern,
                "dynamic route {} must be probed with a substituted path segment",
                row.mounted_pattern
            );
        }
    }
    assert_eq!(
        dynamic_rows, 9,
        "expected 9 dynamic internal patterns (:tenantId, :node_id, :indexName x4, :run_id x3)"
    );
}

#[test]
fn peer_allowed_internal_routes_are_classified_peer_or_admin() {
    for row in peer_allowed_routes() {
        assert_eq!(
            required_acl_for_route(&row.method, row.specimen_path),
            RouteAcl::PeerOrAdmin,
            "{} {} must require the peer-or-admin tier ({})",
            row.method,
            row.specimen_path,
            row.rationale
        );
    }
}

/// GREEN and must stay green. If any of these ever stops being admin-only, a
/// peer credential gains administrative authority and the lane's deliverable —
/// the refusal — is gone.
#[test]
fn admin_only_internal_routes_stay_admin_only() {
    for row in admin_only_routes() {
        assert_eq!(
            required_acl_for_route(&row.method, row.specimen_path),
            RouteAcl::Required("admin"),
            "{} {} must remain admin-only ({})",
            row.method,
            row.specimen_path,
            row.rationale
        );
    }
}

/// The terminal decision for anything unmatched under `/internal/`. A future
/// route that nobody classified must default to the stricter tier.
#[test]
fn unmatched_internal_paths_fall_through_to_admin_only() {
    for path in [
        "/internal/",
        "/internal/not-a-real-route",
        "/internal/cluster",
        "/internal/cluster/peers/bogus-peer/extra",
        "/internal/snapshot",
        "/internal/fault",
    ] {
        for method in [Method::GET, Method::POST, Method::DELETE, Method::PUT] {
            assert_eq!(
                required_acl_for_route(&method, path),
                RouteAcl::Required("admin"),
                "unmatched internal path {method} {path} must default to admin-only"
            );
        }
    }
}

/// The single public route in `build_internal_routes`, excluded from the
/// internal denominator on purpose.
#[test]
fn acme_challenge_is_the_only_public_route_in_the_internal_router() {
    assert_eq!(
        required_acl_for_route(&Method::GET, ACME_CHALLENGE_SPECIMEN),
        RouteAcl::Public,
        "ACME HTTP-01 challenge must stay public"
    );
    for row in INTERNAL_ROUTE_CONTRACT {
        assert!(
            !row.specimen_path.starts_with("/.well-known/"),
            "the public ACME route must not appear in the internal denominator"
        );
        assert_ne!(
            required_acl_for_route(&row.method, row.specimen_path),
            RouteAcl::Public,
            "{} {} must never be public",
            row.method,
            row.specimen_path
        );
    }
}

/// Client-facing Algolia-compatible auth is out of this lane's scope. Pinning it
/// here makes an accidental widening of the peer tier into the public API
/// visible in the same run as the internal assertions.
#[test]
fn client_facing_index_routes_keep_their_existing_acls() {
    assert_eq!(
        required_acl_for_route(&Method::GET, "/1/indexes"),
        RouteAcl::Required("listIndexes")
    );
    assert_eq!(
        required_acl_for_route(&Method::POST, "/1/indexes/baseline_index/query"),
        RouteAcl::Required("search")
    );
}
