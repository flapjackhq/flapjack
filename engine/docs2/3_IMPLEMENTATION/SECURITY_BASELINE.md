# Security Baseline

This document is the canonical public hardening baseline for the open-source
Flapjack repo. It is intentionally scoped: it describes what is verified today,
what operators should strongly prefer in production, and what still remains
outside the current proof bar.

- For deployment and operator runbooks, see [OPERATIONS.md](./OPERATIONS.md).
- For env vars and defaults, see [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).
- For shipped/readiness status, see [../FEATURES.md](../FEATURES.md).

## What is verified today

Full OWASP top-10 pass complete (2026-05-25); consolidated category-by-category
proof and RED baselines live in
`engine/docs2/4_EVIDENCE/SECURITY_BASELINE_AUDIT.md` under
`## OWASP Top-10 Audit (2021)`.

### Production auth floor

- `FLAPJACK_NO_AUTH=1` is blocked in production mode.
- `FLAPJACK_ADMIN_KEY` is required in production mode.
- short production admin keys are rejected at startup.

Proof surfaces:

- `cargo test -p flapjack-server --test env_mode_test`
- `engine/flapjack-http/src/startup.rs`

### TLS transport

- Flapjack can serve optional in-binary TLS when operators provide both
  `--ssl-cert-path` / `FLAPJACK_SSL_CERT_PATH` and `--ssl-key-path` /
  `FLAPJACK_SSL_KEY_PATH`.
- with both TLS paths unset, the server binds plaintext HTTP by default.
- TLS certificate and key material are loaded once at startup; file replacement
  requires a restart.
- startup fails before serving if either side is missing, unreadable, malformed,
  or does not form a usable certificate/key pair.
- reverse proxy or load-balancer TLS remains the preferred production topology.
- the ACME manager configured by `FLAPJACK_SSL_EMAIL`, `FLAPJACK_PUBLIC_IP`, and
  `FLAPJACK_ACME_DIRECTORY` is not wired to rotate the in-binary listener.

Proof surfaces:

- `engine/flapjack-http/src/server.rs::serve`
- `engine/flapjack-http/src/tls_serve.rs::load_tls_config`
- `engine/flapjack-server/src/main.rs`
- `engine/tests/tls_listener_http_probe.sh`
- `engine/flapjack-server/tests/tls_startup_test.rs`

### Admin-only operational surfaces

- `/metrics` requires admin-key auth.
- admin-key rotation is available at `POST /internal/rotate-admin-key`.
- rotated keys invalidate the previous admin key immediately for metrics access.

Proof surfaces:

- `cargo test -p flapjack-server --test admin_key_test`
- `cargo test -p flapjack-http router::tests::metrics_returns_200_with_admin_key_only -- --exact`

### Canonical auth failures

- malformed or invalid credentials return the same canonical
  `{"message":"Invalid Application-ID or API key","status":403}` shape
- malformed secured keys do not leak decode/parse internals in the response
- secured-key credential parsing is hardened against malformed input; invalid
  credentials are rejected without destabilizing request handling.

Proof surfaces:

- `engine/flapjack-http/src/auth/mod.rs`
- `engine/tests/credential_parser_http_probe.sh` — served-boundary probe against
  a real running binary, not just the library function

### Route authorization default

- a request path matching no ACL rule is **denied**, not passed through to its
  handler. Adding a route without an ACL entry therefore fails closed.
- the index-collection `HEAD` route uses the same list-indexes ACL as the
  corresponding collection read.

Proof surfaces:

- `engine/flapjack-http/src/auth/route_acl.rs::required_acl_for_route`
- `engine/flapjack-http/src/auth_tests/middleware_tests.rs::route_acl_denies_unmapped_route_by_default`
- `engine/flapjack-http/src/auth_tests/middleware_tests.rs::head_indexes_collection_honors_list_indexes_acl`
- `engine/tests/authorization_boundary_http_probe.sh`

### Admin credential transport

- admin-ACL routes accept the API key **only** in the `x-algolia-api-key`
  request header. The query-string credential form is refused on those routes.
- search-scoped keys keep query-string support, so browser and InstantSearch
  clients are unaffected.

Proof surfaces:

- `engine/flapjack-http/src/auth/middleware.rs`
- `engine/flapjack-http/src/auth_tests/middleware_tests.rs::admin_api_key_in_query_string_is_rejected_for_key_routes`
- `engine/flapjack-http/src/auth_tests/middleware_tests.rs::search_api_key_in_query_string_still_allows_search_route`
- `engine/tests/authorization_boundary_http_probe.sh`

### Analytics client-IP minimization

- persisted analytics coarsen the client IP before the Parquet write: IPv4 to
  /24 and IPv6 to /48. The full address is never written to disk (2026-07-31).
- unparseable addresses are stored as null rather than as raw text.
- existing Parquet files stay readable; historical data is not rewritten.

Proof surfaces:

- `engine/src/analytics/writer.rs::truncate_user_ip_for_analytics`
- `engine/tests/test_analytics_ip_e2e.rs::served_search_persists_only_minimized_client_ip`

### Container runtime posture

- images built from `engine/Dockerfile` run as non-root `flapjack:flapjack` at
  fixed UID/GID `10001:10001`. The numeric identity is pinned so base-image
  upgrades cannot silently shift ownership of persisted volumes.
- a pre-existing `/data` volume that is not writable by that identity causes an
  actionable non-zero exit at startup rather than a later runtime failure.

Proof surfaces:

- `engine/Dockerfile`, `engine/entrypoint.sh`
- `engine/tests/test_docker_runtime_e2e.sh`

### Response security headers

- API, embedded-dashboard, Swagger, and generated-error responses set a strict
  content security policy, MIME-sniffing protection, a no-referrer policy, and
  a restrictive permissions policy.
- dashboard credential storage remains outside this verified claim; response
  headers do not establish that browser-held credentials are safely stored.

Proof surfaces:

- `engine/flapjack-http/src/router.rs::insert_security_headers`
- `engine/flapjack-http/src/router_inline_tests.rs::health_surface_has_expected_security_headers`
- `engine/flapjack-http/src/router_inline_tests.rs::dashboard_surface_has_expected_security_headers`
- `engine/flapjack-http/src/router_inline_tests.rs::swagger_surface_has_expected_security_headers`
- `engine/tests/security_headers_http_probe.sh`

### Request resource bounds

- admitted requests share a configurable execution timeout and one global
  concurrency limit; an excess request waits while an admitted request owns
  the available slot.
- panics crossing the router boundary are converted into canonical JSON error
  responses carrying the request ID.

Proof surfaces:

- `engine/flapjack-http/src/router.rs::apply_middleware`
- `engine/flapjack-http/src/middleware.rs::ensure_json_errors`
- `engine/flapjack-http/src/router_inline_tests.rs::fault_sleep_times_out_with_canonical_json_error`
- `engine/flapjack-http/src/router_inline_tests.rs::fault_panic_returns_canonical_json_error_with_request_id`
- `engine/flapjack-http/src/router_inline_tests.rs::global_concurrency_limit_queues_health_while_fault_sleep_owns_slot`
- `engine/tests/resource_bounds_http_probe.sh`

### Dashboard dependency audit policy

- the production dashboard dependency audit gate fails on high and critical
  advisories and fails closed when its audit result cannot be validated.
- production moderate advisories remain below this gate; this bounded control
  does not claim that the remaining dashboard dependency risk is closed.

Proof surfaces:

- `.github/workflows/ci.yml`
- `engine/dashboard/scripts/audit_gate.sh`
- `engine/dashboard/scripts/audit_gate_fixture_test.sh`

### `restrictSources` coverage

- `restrictSources` persists through key storage and serialization
- allow-list enforcement is fail-closed on protected routes
- forwarded-header handling only trusts known proxy CIDRs

Proof surfaces:

- `cargo test -p flapjack --test test_security_sources_parity`
- `engine/flapjack-http/src/auth_tests/key_store_tests.rs`
- `engine/flapjack-http/src/middleware.rs`

### CORS origin enforcement

- `FLAPJACK_ALLOWED_ORIGINS` defaults to loopback-only browser access when
  missing/empty, and switches to restricted mode when valid origins are
  provided.
- CORS preflight reflects allowed origins in restricted mode.
- blocked origins in restricted mode do not receive
  `access-control-allow-origin`.
- non-loopback origins are blocked by default in loopback-only mode.

Proof surfaces:

- `engine/flapjack-http/src/startup_tests.rs::cors_origins_from_value_defaults_to_loopback_only_when_missing_or_empty`
- `engine/flapjack-http/src/startup_tests.rs::cors_origins_from_value_parses_single_origin`
- `engine/flapjack-http/src/startup_tests.rs::cors_origins_from_value_parses_comma_separated_origins_with_trimmed_whitespace`
- `engine/flapjack-http/src/startup_tests.rs::cors_origins_from_value_ignores_trailing_commas_and_empty_segments`
- `engine/flapjack-http/src/router_inline_tests.rs::cors_preflight_returns_expected_allow_origin_for_restricted_and_loopback_modes`
- `engine/flapjack-http/src/router_inline_tests.rs::cors_preflight_blocks_non_loopback_origins_in_loopback_mode`
- `engine/flapjack-http/src/router_inline_tests.rs::cors_preflight_rejects_blocked_origins_in_restricted_mode`

### Request body-size limit

- `FLAPJACK_MAX_BODY_MB` parsing accepts valid integer values and falls back to
  `100` for invalid values.
- oversized JSON payloads are rejected with `413 Payload Too Large`.
- payloads under the configured limit are accepted.
- for variable types and defaults, see the
  [OPS_CONFIGURATION.md Limits table](./OPS_CONFIGURATION.md#limits).

Proof surfaces:

- `engine/flapjack-http/src/router_inline_tests.rs::max_body_mb_from_value_parses_valid_integer`
- `engine/flapjack-http/src/router_inline_tests.rs::max_body_mb_from_value_defaults_to_100_for_invalid_values`
- `engine/flapjack-http/src/router_inline_tests.rs::body_limit_from_env_rejects_payload_over_limit`
- `engine/flapjack-http/src/router_inline_tests.rs::body_limit_from_env_allows_payload_under_limit`

### Trusted proxy and client IP resolution

- with no `FLAPJACK_TRUSTED_PROXY_CIDRS` setting, trusted proxies default to
  loopback ranges only.
- explicit `off` disables trusted forwarded-header parsing.
- when the peer is untrusted, `X-Forwarded-For` is ignored and socket peer IP
  is used.
- when the peer is trusted, client IP resolution uses the rightmost untrusted
  address from the forwarded chain.

Proof surfaces:

- `engine/flapjack-http/src/middleware_tests.rs::trusted_proxy_matcher_defaults_to_loopback_when_not_configured`
- `engine/flapjack-http/src/middleware_tests.rs::trusted_proxy_matcher_supports_explicit_off_keyword`
- `engine/flapjack-http/src/middleware_tests.rs::extract_client_ip_ignores_forwarded_headers_without_trusted_proxy`
- `engine/flapjack-http/src/middleware_tests.rs::extract_client_ip_uses_first_untrusted_from_right_when_peer_is_trusted_proxy`

### Per-key per-IP rate limiting

- request-rate counters are enforced per API key and per resolved client IP.
- exhausting one IP bucket for a key does not consume the bucket for a different
  IP.
- `429` responses use the canonical Algolia-shaped JSON envelope.
- `restrictSources` rejections return `403` before rate-limit consumption.

Proof surfaces:

- `engine/flapjack-http/src/handlers/wire_format_tests.rs::x_forwarded_for_used_for_rate_limiting`
- `engine/flapjack-http/src/auth_tests/middleware_tests.rs::auth_middleware_returns_algolia_error_shape_for_403_and_429`
- `engine/flapjack-http/src/auth_tests/middleware_tests.rs::auth_middleware_secured_key_restrict_sources_rejection_does_not_consume_rate_limit`

### Request correlation and error consistency

- every response carries `x-request-id`
- JSON log mode preserves the request ID in span context
- non-JSON errors are normalized into the canonical JSON envelope

Proof surfaces:

- `engine/flapjack-http/src/middleware.rs`
- `engine/flapjack-http/src/router_tests.rs`
- `engine/flapjack-http/src/startup.rs`

### HTTP hardening

#### Request body size limits

- `FLAPJACK_MAX_BODY_MB` is enforced in `engine/flapjack-http/src/router.rs`
  by `apply_middleware` with `DefaultBodyLimit::max(...)`.
- oversized requests are returned as HTTP 413 and normalized into the
  canonical JSON error envelope by `ensure_json_errors` in
  `engine/flapjack-http/src/middleware.rs`.
- defaults and env-var typing are documented in
  [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).

Proof surfaces:

- `cargo test -p flapjack-http --lib -- oversized_body_returns_413_json_error`
- `engine/flapjack-http/src/router.rs`

#### CORS controls

- `FLAPJACK_ALLOWED_ORIGINS` is parsed by
  `engine/flapjack-http/src/startup.rs` (`cors_origins_from_value`) into
  loopback-only vs restricted mode.
- CORS policy is enforced in `engine/flapjack-http/src/router.rs`
  (`build_cors_layer`).
- defaults and env-var typing are documented in
  [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).

Proof surfaces:

- `engine/docs2/4_EVIDENCE/SECURITY_BASELINE_AUDIT.md` (sections `1a` and `1b`)
- `cargo test -p flapjack-http --lib -- cors_preflight_rejects_blocked_origins_in_restricted_mode`

#### Per-key rate limiting

- `maxQueriesPerIPPerHour` is an API key setting enforced in
  `engine/flapjack-http/src/auth/middleware.rs`
  (`ensure_rate_limit_allows_request`).
- invalid keys return early at `lookup_authenticated_key`, so invalid-key
  requests do not consume rate-limit buckets.
- `maxQueriesPerIPPerHour` is configured via the keys API (not an env var).

Proof surfaces:

- `cargo test -p flapjack-http --lib -- auth_middleware_invalid_key_does_not_consume_rate_limit`
- `cargo test -p flapjack-http --lib -- auth_middleware_secured_key_restrict_sources_rejection_does_not_consume_rate_limit`
- `engine/flapjack-http/src/auth/middleware.rs`

#### Trusted proxy handling

- `FLAPJACK_TRUSTED_PROXY_CIDRS` configures `TrustedProxyMatcher` in
  `engine/flapjack-http/src/middleware.rs`.
- `extract_rate_limit_ip` and `extract_client_ip` only honor forwarded headers
  when the immediate peer is trusted.
- `off` or `none` disables proxy trust entirely.
- defaults and env-var typing are documented in
  [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).

Proof surfaces:

- `engine/docs2/4_EVIDENCE/SECURITY_BASELINE_AUDIT.md` (section `2`)
- `cargo test -p flapjack-http --lib -- trusted_proxy_matcher_supports_explicit_off_keyword`
- `engine/flapjack-http/src/middleware.rs`

## Public deployment safety review

The public docs should normalize the following production posture:

- run with `FLAPJACK_ENV=production`
- provide a strong admin key
- do not use `--no-auth` or `FLAPJACK_NO_AUTH=1` outside local loopback-only dev
- keep `/internal/*` and `/metrics` behind both auth and network controls
- prefer reverse-proxy or load-balancer TLS in production; use the in-binary
  `--ssl-cert-path` / `--ssl-key-path` listener only when static certificate
  files and restart-based rotation are acceptable
- keep secrets in an env file or secret store, not in tracked config

Current public docs are aligned with that baseline:

- `README.md` keeps no-auth examples explicitly local
- `engine/examples/systemd/README.md` requires a production env file and admin key
- `engine/docs2/3_IMPLEMENTATION/DEPLOYMENT.md` points production operators to the systemd path and env-file pattern

## Strong operator recommendations

These are not launch blockers for the OSS repo, but they are the recommended
production baseline:

- terminate TLS at a trusted proxy or load balancer when practical; the optional
  in-binary listener is a static certificate/key-file mode, not an ACME rotation
  lifecycle
- restrict `/internal/*` and `/metrics` with firewall/VPC rules even though they are auth-gated
- use dedicated service accounts and least-privilege filesystem ownership
- back up the data dir before upgrades and before risky maintenance
- keep trusted proxy CIDRs explicit instead of broadly trusting forwarded headers
- use `FLAPJACK_LOG_FORMAT=json` when central log collection is in place
- set `FLAPJACK_MAX_BODY_MB` to the smallest value your workload allows
- restrict `FLAPJACK_ALLOWED_ORIGINS` to your frontend domain(s) in production
- set `maxQueriesPerIPPerHour` on search-only API keys to limit abuse

## Not yet proven deeply enough

These areas remain outside the current OSS hardening proof bar:

- formal pentest or hostile-network review
- SaaS/multi-tenant isolation review beyond the current targeted tests
- SSO, SCIM, audit-log, and enterprise IAM controls
- distributed tracing / full cross-node forensic tooling
- HTTP hardening basics (body limits, CORS, per-key rate limiting, trusted
  proxy handling) are documented and test-backed above, but deeper proof is
  still missing for sustained rate-limit bucket exhaustion behavior,
  distributed CORS origin validation across replicas, and CIDR-based proxy
  trust in multi-hop cloud load-balancer topologies

## Scope boundary

Use this document for bounded security claims. Do not turn it into a wish list.
If a new hardening measure is only recommended or only planned, label it that
way instead of presenting it as shipped protection.
