# 2026-08-02 aug02_11am_4 replication peer identity receipt

## Purpose

Close the documentation and evidence gate for the Stage 2 replication
peer-identity enforcement changes.

## Goals

- Record that configured replication now requires an outbound peer identity by
  default.
- Record that credentialed `http://` peer origins are refused by default.
- Record that `analytics_cluster.rs::push_rollup_to_peers` now authenticates
  background rollup fan-out with the configured peer credential.
- Replace stale operator wording in
  `engine/docs2/3_IMPLEMENTATION/OPS_CONFIGURATION.md` and add the rolling
  upgrade runbook in `engine/docs2/3_IMPLEMENTATION/OPERATIONS.md`.

## Out Of Scope

- Rust implementation changes.
- Shell fixture changes.
- Edits to `ROADMAP.md`, `PROJECT_OVERVIEW.md`, `engine/docs2/FEATURES.md`,
  `CHANGELOG.md`, or `docs/security/DECISIONS.md`.
- Closing the `RUSTSEC-2025-0134` advisory.

## Implementation Owners

- Startup refusal for configured replication without
  `FLAPJACK_REPLICATION_API_KEY`:
  `engine/flapjack-http/src/startup.rs::validate_replication_peer_credential`.
- Static, persisted, bootstrap, and runtime peer transport refusal:
  `engine/flapjack-replication/src/config.rs::NodeConfig::validate_peer_transport`.
- Caller-authenticated analytics query transport refusal:
  `engine/flapjack-http/src/analytics_cluster.rs::validate_authenticated_query_peer_transport`.
- Bootstrap peer retention after transport filtering:
  `engine/flapjack-replication/src/config.rs::NodeConfig::retain_transport_safe_bootstrap_peer`.
- Runtime add-peer use of the shared transport policy:
  `engine/flapjack-http/src/handlers/internal.rs::add_cluster_peer`.
- Outbound analytics peer identity:
  `engine/flapjack-http/src/analytics_cluster.rs::AnalyticsClusterClient::with_peer_identity`,
  `::query_peers`, and `::push_rollup_to_peers`.
- Effective outbound credential selection:
  `engine/flapjack-http/src/server.rs::initialize_server_infrastructure`.

## Measured Transport Premise

`https://` peer origins were already supported before this lane.
`engine/flapjack-replication/src/config.rs::NodeConfig::normalize_peer_addr`
accepts `http` and `https`, `engine/flapjack-replication/Cargo.toml` builds
`reqwest` with `rustls-tls`, and config tests construct
`https://peer-a.example.com:7700`.

The closed defect is default refusal of credentialed cleartext peer origins,
not adding HTTPS support.

## Validation Evidence

Recorded at code HEAD `b7506538d0101af1ac9dfed044ebd885fa4f60a3`. Stage 3
made no Rust or shell fixture changes; repo changes were limited to operator
documentation, this receipt, and the public-doc sync-surface configuration.

- `cd engine && timeout 1200 cargo test -p flapjack-replication --lib -- cleartext_`
  passed: 6 passed, 0 failed. This covers default refusal and escape logging
  for static peers, `FLAPJACK_BOOTSTRAP_PEER`, and persisted `node.json`,
  including the Stage 2 review fix in `retain_transport_safe_bootstrap_peer`.
- `cd engine && timeout 1200 cargo test -p flapjack-http --lib -- startup_refuses_`
  passed: 2 passed, 0 failed. This covers startup refusal for configured
  replication intent without `FLAPJACK_REPLICATION_API_KEY`.
- `cd engine && timeout 1200 cargo test -p flapjack-http --lib -- add_cluster_peer_cleartext`
  passed: 1 passed, 0 failed. This covers the runtime membership cleartext
  escape path.
- `cd engine && timeout 1200 cargo test -p flapjack-http --lib -- add_cluster_peer_rejects_cleartext_transport_when_peer_key_is_configured`
  passed: 1 passed, 0 failed. This covers the Stage 2 review fix in
  `engine/flapjack-http/src/handlers/internal.rs::add_cluster_peer`, where the
  runtime membership endpoint shares the same default cleartext refusal as
  startup-loaded peers.
- `cd engine && timeout 1200 cargo test -p flapjack-http --lib -- push_rollup_to_peers_uses_query_peer_auth_header_contract`
  passed: 1 passed, 0 failed. This covers
  `analytics_cluster.rs::push_rollup_to_peers` sending the same peer API key
  and application ID header contract as query fan-out.
- `cd engine && bash tests/replication_peer_auth_http_probe.sh` passed:
  `PASS: replication peer authorization HTTP probe completed (10 checks)`.
  The served probe confirmed peer-key replication access, peer-key refusal on
  add/remove/rotate-admin-key, query-string peer-key refusal, random-key
  refusal, public-route refusal, admin-key compatibility on peer routes, and
  replication convergence on node B.

## Residuals

- `RUSTSEC-2025-0134` remains open and is owned by the active repair lane
  `95067`; this receipt does not change `Cargo.toml`, `Cargo.lock`, or
  `tls_serve.rs`.

## Posthoc Review Fixes

- `server_init.rs::merge_bootstrap_membership` now applies the canonical peer
  transport policy to every member learned from bootstrap status, preventing
  an HTTPS bootstrap node from injecting a credentialed cleartext peer.
- `server.rs::initialize_server_infrastructure` no longer substitutes the admin
  key when the explicit unauthenticated replication override is active.
- `startup.rs::normalize_replication_api_key` rejects peer keys that cannot be
  represented as HTTP header values instead of accepting a broken identity
  that makes every outbound request fail later.
- `cd engine && timeout 600 cargo test -p flapjack-replication --no-fail-fast`
  passed: 137 passed, 0 failed.
- The three focused `flapjack-http` regressions
  `bootstrap_membership_rejects_credentialed_cleartext_peer`,
  `serve_startup_without_peer_key_does_not_send_admin_key_to_peers`, and
  `replication_peer_api_key_rejects_invalid_header_characters` each passed.
- Final posthoc review closed a fail-open integration gap in
  `NodeConfig::load_for_server_startup`: transport-policy rejections now reach
  `load_server_config` as errors instead of disappearing from the filtered
  topology and allowing a configured HA node to serve standalone or as an
  empty seed. The focused regressions
  `startup_rejects_cleartext_static_peer_instead_of_serving_standalone` and
  `startup_rejects_cleartext_bootstrap_peer_instead_of_becoming_a_seed` passed.
- Final cross-stage review closed a remaining credential leak under
  `FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1`: authenticated analytics
  query fan-out still forwards the caller's API key, so static, bootstrap, and
  runtime-learned `http://` peers are refused without the separate cleartext
  override. No-auth mode remains covered because analytics forwards any
  caller-supplied key rather than suppressing headers. Focused startup,
  runtime-membership, bootstrap-membership, and no-auth regressions passed.
- `cd engine && cargo check -p flapjack-http`,
  `cargo clippy -p flapjack-http`, `cargo fmt --check`, and
  `git diff --check` passed. The no-fail-fast HTTP package run exercised all
  2,196 library tests; 2,192 passed, while two unchanged bulk-replace tests and
  one unchanged timing-sensitive integration test failed outside this lane's
  changed-file scope.

## ROADMAP CORRECTION REQUIRED

Use this replacement text for the `SEC-W4` row:

`Updated 2026-08-02 - the remaining SEC-G9 peer-identity residuals closed for configured replication: startup now refuses replication intent without FLAPJACK_REPLICATION_API_KEY unless FLAPJACK_ALLOW_UNAUTHENTICATED_REPLICATION_PEERS=1 is set, credentialed http:// peers are refused by default unless FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1 is set, bootstrap-learned membership shares that transport policy, analytics_cluster.rs::push_rollup_to_peers authenticates outbound rollup fan-out with the configured peer credential, and outbound replication never substitutes the admin key for a missing peer key. Correct the old "encrypt peer transport" / "support https peers" wording: https:// peer origins were already accepted; this lane closed default refusal of credentialed cleartext. Remaining SEC-W4 residuals are RUSTSEC-2025-0134 under owner lane 95067 and any separately tracked moderate-advisory supply-chain disposition.`
