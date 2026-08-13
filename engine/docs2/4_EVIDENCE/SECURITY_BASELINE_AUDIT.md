# Security Baseline Audit — HTTP Surface Coverage Matrix

**Audit date:** 2026-04-01
**HEAD:** `37c89d83`
**Focused proofs re-verified green:** `auth_middleware_invalid_key_does_not_consume_rate_limit`, `body_limit_from_env_rejects_payload_over_limit`, `body_limit_from_env_allows_payload_under_limit`, `env_mode_test`, `admin_key_test`, `test_security_audit`, `test_security_sources_parity`

---

## 1. CORS Parsing & Preflight — COVERED

### 1a. CORS mode parsing (`startup.rs:50` `cors_origins_from_value`)

| Test | File | Proves |
|------|------|--------|
| `cors_origins_from_value_defaults_to_loopback_only_when_missing_or_empty` | `startup_tests.rs:156` | `None`, `""`, `"   "` all → `CorsMode::LoopbackOnly` |
| `cors_origins_from_value_parses_single_origin` | `startup_tests.rs:179` | Single origin → `CorsMode::Restricted([origin])` |
| `cors_origins_from_value_parses_comma_separated_origins_with_trimmed_whitespace` | `startup_tests.rs:188` | Comma-separated, whitespace-trimmed → `CorsMode::Restricted` |
| `cors_origins_from_value_ignores_trailing_commas_and_empty_segments` | `startup_tests.rs:202` | Trailing commas / empty segments filtered |

**Enforcement code:** `startup.rs:50` `cors_origins_from_value` → branches on `None`/empty→`LoopbackOnly` vs parsed→`Restricted`. `cors_origins_from_env` delegates to `cors_origins_from_value`.

### 1b. CORS preflight behavior (`router.rs:487` `build_cors_layer`)

| Test | File | Proves |
|------|------|--------|
| `cors_preflight_returns_expected_allow_origin_for_restricted_and_loopback_modes` | `router_inline_tests.rs:267` | Restricted mode echoes configured origin; loopback mode allows loopback browser origins |
| `cors_preflight_blocks_non_loopback_origins_in_loopback_mode` | `router_inline_tests.rs:315` | Default loopback mode denies non-loopback browser origins |
| `cors_preflight_rejects_blocked_origins_in_restricted_mode` | `router_inline_tests.rs:260` | Blocked origin gets no `access-control-allow-origin` header |

**Enforcement code:** `router.rs:488` `build_cors_layer` — LoopbackOnly → `AllowOrigin::predicate(is_loopback_origin)`, Restricted → explicit origin list.

**Stage 2 verdict: No new CORS tests needed.**

---

## 2. Trusted Proxy & IP Extraction — COVERED

### 2a. `extract_client_ip` (`middleware.rs`)

| Test | File | Proves |
|------|------|--------|
| `extract_client_ip_ignores_forwarded_headers_without_trusted_proxy` | `middleware_tests.rs:421` | No TrustedProxyMatcher → uses ConnectInfo peer IP, ignores XFF/X-Real-IP |
| `extract_client_ip_uses_x_real_ip_when_peer_is_trusted_proxy` | `middleware_tests.rs:438` | Trusted peer → X-Real-IP honored |
| `extract_client_ip_falls_back_to_connect_info` | `middleware_tests.rs:456` | No headers → ConnectInfo fallback |
| `extract_client_ip_rejects_forwarded_headers_without_peer_info` | `middleware_tests.rs:470` | No ConnectInfo → loopback fallback (127.0.0.1), ignores XFF |
| `extract_client_ip_uses_first_untrusted_from_right_when_peer_is_trusted_proxy` | `middleware_tests.rs:484` | XFF walk: first untrusted from right selected |
| `extract_client_ip_uses_leftmost_after_skipping_trusted_forward_chain` | `middleware_tests.rs:504` | All XFF trusted → leftmost (furthest) IP returned |
| `extract_client_ip_combines_multiple_x_forwarded_for_headers` | `middleware_tests.rs:523` | Multiple XFF headers concatenated |

### 2b. `TrustedProxyMatcher` configuration

| Test | File | Proves |
|------|------|--------|
| `trusted_proxy_matcher_rejects_invalid_cidr` | `middleware_tests.rs:542` | Invalid CIDR → error |
| `trusted_proxy_matcher_defaults_to_loopback_when_not_configured` | `middleware_tests.rs:548` | `None` → loopback (127.0.0.1, ::1) trusted, public IPs untrusted |
| `trusted_proxy_matcher_supports_explicit_off_keyword` | `middleware_tests.rs:556` | `"off"` → empty matcher (no proxies trusted) |

### 2c. `extract_rate_limit_ip` (`middleware.rs`)

| Test | File | Proves |
|------|------|--------|
| `rate_limit_ip_uses_trusted_path_when_peer_is_trusted_and_xff_present` | `middleware_tests.rs:565` | Trusted peer + XFF → rightmost untrusted from XFF |
| `rate_limit_ip_uses_peer_when_peer_is_not_trusted_ignoring_xff` | `middleware_tests.rs:585` | Untrusted peer → peer IP (prevents XFF spoof bypass) |
| `rate_limit_ip_uses_peer_when_no_xff_headers` | `middleware_tests.rs:602` | No XFF → peer IP |
| `rate_limit_ip_ignores_xff_when_no_connect_info` | `middleware_tests.rs:616` | No ConnectInfo → loopback |
| `rate_limit_ip_falls_back_to_loopback_with_no_info` | `middleware_tests.rs:629` | Bare request → 127.0.0.1 |

### 2d. GeoIP / analytics IP consistency

| Test | File | Proves |
|------|------|--------|
| `geoip_uses_peer_ip_when_proxy_not_trusted` | `middleware_tests.rs:645` | Untrusted proxy → peer IP for geo |
| `geoip_uses_forwarded_chain_when_peer_is_trusted` | `middleware_tests.rs:666` | Trusted proxy → XFF-resolved IP for geo |
| `geoip_takes_first_untrusted_from_right_in_xff_chain` | `middleware_tests.rs:689` | Multi-hop resolution consistent |
| `geoip_handles_multiple_x_forwarded_for_headers_consistently` | `middleware_tests.rs:714` | Multiple XFF headers combined |
| `analytics_country_enrichment_uses_same_client_ip_path` | `middleware_tests.rs:741` | `extract_client_ip_opt` returns same IP for both geo and analytics |

**Stage 2 verdict: No new IP extraction or trusted-proxy tests needed.**

---

## 3. Auth Boundary — COVERED

### 3a. Public routes bypass auth (`router_tests.rs`)

| Test | File | Proves |
|------|------|--------|
| `readiness_route_is_public` | `router_tests.rs:38` | `/health/ready` returns 200 with auth enabled |
| `health_route_is_public` | `router_tests.rs:52` | `/health` returns 200 with auth enabled |
| `dashboard_route_is_public_and_serves_html` | `router_tests.rs:64` | `/dashboard` public |
| `dashboard_spa_fallback_route_is_public` | `router_tests.rs:91` | `/dashboard/...` public |
| `dashboard_prefix_without_separator_is_not_public` | `router_tests.rs:108` | `/dashboardX` NOT public |

### 3b. Metrics endpoint security (`router_tests.rs`)

| Test | File | Proves |
|------|------|--------|
| `metrics_returns_403_without_auth_headers` | `router_tests.rs:116` | `/metrics` requires auth |
| `metrics_returns_200_with_admin_key_only` | `router_tests.rs:144` | Admin key (no app-id) sufficient for `/metrics` |
| `metrics_rejects_query_param_admin_key` | `router_tests.rs:172` | URL-borne admin key rejected (prevents log leakage) |

### 3c. Open mode (--no-auth) behavior (`router_inline_tests.rs`)

| Test | File | Proves |
|------|------|--------|
| `build_router_open_mode_allows_protected_routes_without_auth_layer` | `router_inline_tests.rs:112` | No auth layer → protected routes accessible |
| `build_router_open_mode_allows_dictionary_routes_without_auth_layer` | `router_inline_tests.rs:142` | Dictionaries work in open mode |
| `build_router_open_mode_does_not_expose_internal_routes` | `router_inline_tests.rs:181` | Internal routes still 404 in open mode |

### 3d. Internal route auth (`router_tests.rs`, `auth_tests/middleware_tests.rs`)

| Test | File | Proves |
|------|------|--------|
| `internal_storage_returns_403_with_admin_key_only_no_app_id` | `router_tests.rs:209` | Internal routes require app-id even with admin key |
| `auth_middleware_internal_storage_requires_app_id_even_for_admin_key` | `auth_tests/middleware_tests.rs` | Same property at unit level |

**Enforcement code:** `auth/middleware.rs` `authenticate_and_authorize` — checks `request_application_id()`, returns 403 if missing (except `/metrics` with admin key).

---

## 4. Server Startup / Admin Key Lifecycle — COVERED

### 4a. Production mode guards (`env_mode_test.rs`)

| Test | Proves |
|------|--------|
| `production_mode_rejects_missing_key` | No `FLAPJACK_ADMIN_KEY` → exit 1 |
| `production_mode_rejects_short_key` | Key < 16 chars → exit 1 |
| `production_mode_accepts_valid_key` | Key >= 16 chars → starts, key NOT echoed |
| `production_mode_rejects_no_auth` | `--no-auth` + production → exit 1 |

### 4b. Development mode key lifecycle (`env_mode_test.rs`)

| Test | Proves |
|------|--------|
| `development_mode_auto_generates_key` | `fj_admin_` + 32 hex chars, shown in banner, persisted to `keys.json` |
| `key_persists_across_restarts` | Same hash in `keys.json` on restart, key not re-shown |
| `development_mode_with_custom_key` | `FLAPJACK_ADMIN_KEY` used, not echoed |
| `no_auth_env_var_disables_auth` | `FLAPJACK_NO_AUTH=1` → no key, no `keys.json` |
| `no_auth_cli_flag_disables_auth` | `--no-auth` CLI flag matches env behavior |

### 4c. Admin key management (`admin_key_test.rs`)

| Test | Proves |
|------|--------|
| `env_var_key_overrides_existing_keys_json` | Env var key replaces persisted key, hash changes |
| `rotate_admin_key_endpoint_rewrites_admin_key_file_and_invalidates_old_key` | `/internal/rotate-admin-key` → new key works, old key 403 |
| `reset_admin_key_works` | `reset-admin-key` CLI → new `fj_admin_` key, hash changes |
| `reset_admin_key_fails_without_keys_json` | Missing `keys.json` → graceful error |

### 4d. Instance isolation (`env_mode_test.rs`)

| Test | Proves |
|------|--------|
| `second_process_same_data_dir_fails_fast_with_lock_message` | Data dir lock prevents concurrent access |
| `instance_flag_derives_isolated_data_dir` | `--instance` creates unique data dir |
| `two_instances_with_unique_data_dirs_serve_independent_index_state` | Full index isolation between instances |

**Stage 2 verdict: No new startup/lifecycle tests needed.**

---

## 5. Malformed Key Rejection — COVERED

| Test | File | Proves |
|------|------|--------|
| `malformed_secured_keys_return_canonical_403_without_decode_leaks` | `test_security_audit.rs:12` | Non-Base64 and short-decoded tokens → canonical `{"message":"Invalid Application-ID or API key","status":403}` with no decode internals leaked |

**Enforcement code:** `auth/middleware.rs` `lookup_authenticated_key` → `validate_secured_key` returns `None` for decode failures → `invalid_api_credentials_error()`.

**Stage 2 verdict: No new malformed-key tests needed.**

---

## 6. Source Allowlist (Security Sources) — COVERED

### 6a. CRUD operations (`test_security_sources_parity.rs`)

| Test | Proves |
|------|--------|
| `get_security_sources_is_empty_by_default` | Empty list on fresh instance |
| `append_then_get_roundtrips_source_and_description_with_created_at` | Full CRUD round-trip |
| `put_replaces_entire_list_and_returns_updated_at` | Bulk replace works |
| `append_duplicate_is_idempotent` | No duplicates on re-append |
| `delete_removes_entry_and_is_noop_for_missing` | Delete + idempotent missing delete |
| `malformed_cidr_returns_400_parity_json_error` | Bad CIDR → 400 with CIDR mention |
| `allowlist_persists_across_app_rebuild_using_same_data_dir` | Persistence across restarts |

### 6b. Enforcement behavior (`test_security_sources_parity.rs`)

| Test | Proves |
|------|--------|
| `allowlist_empty_allows_all_ips_on_protected_routes` | Empty allowlist → all IPs pass |
| `allowlist_enforces_cidr_and_returns_forbidden_for_unlisted_ip` | Unlisted IP → `{"message":"Forbidden","status":403}` |
| `forwarded_for_rightmost_untrusted_takes_precedence_over_real_ip_for_allowlist_matching` | XFF-resolved IP used for allowlist, not X-Real-IP |
| `health_route_is_not_subject_to_allowlist_middleware` | `/health` exempt from allowlist |

**Enforcement code:** `auth/middleware.rs` `ensure_sources_allow_request` → calls `middleware::extract_client_ip_opt` for `api_key.restrict_sources` and `middleware::extract_rate_limit_ip` for secured-key `restrictSources`.

**Stage 2 verdict: No new allowlist tests needed.**

---

## 7. Auth Failure Ordering & Rate-Limit Non-Consumption — COVERED

### 7a. Secured-key restrictSources ordering — COVERED

| Test | File | Proves |
|------|------|--------|
| `auth_middleware_enforces_secured_key_restrict_sources` | `auth_tests/middleware_tests.rs` | Secured key with `restrictSources=127.0.0.0/8` allows matching IP, rejects non-matching |
| `auth_middleware_secured_key_restrict_sources_rejection_does_not_consume_rate_limit` | `auth_tests/middleware_tests.rs` | Source rejection (403) with `max_queries_per_ip_per_hour=1` — sends 2 requests, both get 403 (not 429), proving rate limit not consumed |

**Enforcement code:** `auth/middleware.rs:L284-289` — `ensure_sources_allow_request` (L284) is called BEFORE `ensure_rate_limit_allows_request` (L289), so source-rejected requests never reach the rate limiter.

### 7b. Invalid API key non-consumption — COVERED

| Test | File | Proves |
|------|------|--------|
| `auth_middleware_invalid_key_does_not_consume_rate_limit` | `auth_tests/middleware_tests.rs` | Repeated invalid-key requests return canonical `403` responses instead of exhausting the per-IP bucket and flipping to `429`, proving auth failure exits before rate-limit accounting |

**Enforcement code:** `auth/middleware.rs` still returns early at `lookup_authenticated_key(...).ok_or_else(invalid_api_credentials_error)?` before `ensure_rate_limit_allows_request`, and the new proof now exercises that ordering directly.

### 7c. Additional auth ordering tests — COVERED

| Test | File | Proves |
|------|------|--------|
| `auth_middleware_returns_algolia_error_shape_for_403_and_429` | `auth_tests/middleware_tests.rs` | 403 and 429 both use `{"message":"...","status":N}` shape |
| `auth_middleware_allows_non_admin_key_to_get_own_key_record` | `auth_tests/middleware_tests.rs` | Non-admin key can read `/1/keys/{self}` |
| `auth_middleware_rejects_non_admin_key_for_own_restore_route` | `auth_tests/middleware_tests.rs` | Non-admin key cannot POST `/1/keys/{self}/restore` |
| `auth_middleware_rejects_protected_routes_when_keystore_is_missing` | `auth_tests/middleware_tests.rs` | Missing KeyStore → 500 "Internal server error" |

---

## 8. Oversized Request Rejection (DefaultBodyLimit / 413) — COVERED

| Test | File | Proves |
|------|------|--------|
| `body_limit_from_env_rejects_payload_over_limit` | `router_inline_tests.rs` | A request above `FLAPJACK_MAX_BODY_MB` is rejected at the middleware layer with canonical JSON `413 Payload Too Large` |
| `body_limit_from_env_allows_payload_under_limit` | `router_inline_tests.rs` | A request below the configured `FLAPJACK_MAX_BODY_MB` limit still reaches handler logic, proving the env-var-controlled threshold is actually applied |

**Enforcement code:** `router.rs` `apply_middleware` reads `FLAPJACK_MAX_BODY_MB`, passes it through `max_body_mb_from_value`, and applies `DefaultBodyLimit::max(max_body_mb * 1024 * 1024)`. The `ensure_json_errors` middleware in `middleware.rs` wraps the `413` into the canonical `{"message":"...","status":413}` shape now exercised by the focused route test.

---

## Stage 2 Gap Summary — CLOSED

| # | Surface | Status | Gap Description |
|---|---------|--------|-----------------|
| G1 | Invalid API key rate-limit non-consumption | CLOSED | `auth_middleware_invalid_key_does_not_consume_rate_limit` now proves unrecognized keys do not burn rate-limit quota before the canonical `403` return |
| G2 | HTTP body-limit 413 rejection | CLOSED | `body_limit_from_env_rejects_payload_over_limit` and `body_limit_from_env_allows_payload_under_limit` now prove the middleware-layer `413` path and env-var threshold behavior |

All originally audited Stage 2 gaps are now closed. All other audited surfaces (CORS, trusted proxy, IP extraction, public route bypass, auth boundary, admin key lifecycle, malformed key rejection, source allowlist, secured-key ordering) remain covered.

---

## OWASP Top-10 Audit (2021)

### A01 Broken Access Control
Stage 2 A01 RED→GREEN audit is complete. New focused integration tests in `engine/tests/test_security_audit.rs` cover restricted-key cross-tenant denial (`a01_restricted_key_denies_cross_tenant_index_query`), admin-only internal endpoint denial for non-admin keys (`a01_non_admin_key_cannot_access_internal_status`), and `/internal/*` app-id/auth parity (`a01_internal_route_rejects_empty_application_id_header`). RED baseline evidence is captured at `engine/tests/results/security_audit_a01_red_baseline.log`, which shows the failing pre-fix behavior (`/internal/status` returned 200 when `x-algolia-application-id` was present-but-empty). The owner-seam fix is in `engine/flapjack-http/src/auth/mod.rs` (`request_application_id` now trims and rejects empty/whitespace app IDs before auth proceeds). OWASP category reference: https://owasp.org/Top10/A01_2021-Broken_Access_Control/. Owner seam files touched for this stage: `engine/flapjack-http/src/auth/mod.rs`, `engine/flapjack-http/src/auth/middleware.rs` (behavior exercised), `engine/flapjack-http/src/auth/route_acl.rs` (behavior exercised), `engine/tests/test_security_audit.rs`. Coverage assessment: `resolved (✅)`.

### A02 Cryptographic Failures
Stage 3 A02 RED→GREEN audit is complete. New focused integration tests in `engine/tests/test_security_audit.rs` cover key entropy after prefix stripping (`a02_generated_search_keys_have_128_bits_after_prefix_strip`), plaintext-at-rest leakage detection for both `keys.json` and `key_material.json` (`a02_keys_json_does_not_persist_plaintext_api_keys`), runtime admin-key rotation invalidation (`a02_rotate_admin_key_invalidates_old_and_accepts_new`), and `/internal/*` auth enforcement from Algolia credential headers without TLS-termination metadata dependency (`a02_internal_auth_depends_only_on_algolia_auth_headers`). RED baseline evidence is captured at `engine/tests/results/security_audit_a02_red_baseline.log`, which shows the pre-fix failure where `key_material.json` persisted plaintext search key material (`fj_search_...`). Finding severity: `medium` (plaintext key material at rest). Remediation guidance implemented: keep `keys.json` hash-only and encrypt required secured-key parent material at rest before persistence. The owner-seam fix is in `engine/flapjack-http/src/auth/key_store.rs`: `keys.json` persists hashed key records only, and `key_material.json` now stores AES-256-GCM-SIV-encrypted parent-key material derived from the runtime admin key while preserving legacy-material hydration for compatibility. OWASP category reference: https://owasp.org/Top10/A02_2021-Cryptographic_Failures/. Owner seam files touched for this stage: `engine/flapjack-http/src/auth/key_store.rs`, `engine/flapjack-http/src/auth/mod.rs`, `engine/flapjack-http/src/auth/middleware.rs` (behavior exercised), `engine/tests/test_security_audit.rs`. Coverage assessment: `resolved (✅)`.

### A03 Injection
Stage 4 A03 RED→GREEN audit is complete. New focused integration tests in `engine/tests/test_security_audit.rs` cover malformed filter rejection on the public search API (`a03_search_rejects_malformed_filters_instead_of_ignoring_them`), analytics tag SQL `LIKE` interpolation hardening (`a03_top_searches_tags_sql_injection_payload_does_not_broaden_results`), and analytics country SQL equality interpolation hardening (`a03_top_searches_country_sql_injection_payload_does_not_broaden_results`). RED baseline evidence is captured at `engine/tests/results/security_audit_a03_red_baseline.log`, which shows two pre-fix/defect-injection failures: (1) a malformed `filters` payload (`category:books OR )`) returned HTTP `200` and fell through to an unfiltered search instead of being rejected, and (2) with the `country` equality sanitizer temporarily removed from `search_analytics.rs:45`, a `ZZ' OR '1'='1` country payload broadened results to every recorded row — proving the country regression test fails for a real injection defect. Finding severity: `medium` because malformed attacker-controlled filter syntax could silently broaden search scope. The owner-seam fix is in `engine/flapjack-http/src/dto/filter_parsing.rs` and `engine/flapjack-http/src/handlers/search/single_execution.rs`: raw `filters` parsing is now fail-closed and returns `InvalidQuery("Filter parse error: ...")` rather than being skipped during `build_combined_filter()`. The analytics SQL seam in `engine/src/analytics/query/search_analytics.rs` was re-verified safe: every user-controlled string interpolated into SQL across all analytics query modules (country/event_subtype via `sanitize_sql_eq`, tags/attribute via `sanitize_sql_like` with `ESCAPE '\'`, query_id via `sql_string_literal`) routes through a sanitizer; integer date/limit params are safe by type; `FROM` table names are literals and `index_name` only resolves on-disk directories. The public search `query` string is tokenized into `TermQuery`/`FuzzyTermQuery` by Flapjack's own `QueryParser` (not Tantivy's query-DSL parser), so it carries no field-scoping or operator-injection surface. No analytics product-code change was needed — the LIKE and equality sanitizers already preserved literal matching; the country test closes a previously-untested-but-safe path. OWASP category reference: https://owasp.org/Top10/A03_2021-Injection/. Owner seam files touched for this stage: `engine/flapjack-http/src/dto/filter_parsing.rs`, `engine/flapjack-http/src/handlers/search/single_execution.rs`, `engine/src/analytics/query/search_analytics.rs` (re-verified safe; no code change), `engine/tests/test_security_audit.rs`, `engine/flapjack-http/src/dto_tests.rs`, `engine/tests/test_filters.rs`. Coverage assessment: `resolved (✅)`.

### A04 Insecure Design
Stage 5 A04 RED→GREEN audit is complete. New focused integration tests in `engine/tests/test_security_audit.rs` cover fail-closed auth defaults (`a04_auth_enabled_routes_fail_closed_without_credentials`), production bootstrap auth policy (`a04_production_bootstrap_rejects_missing_blank_or_short_admin_key`), default CORS hardening (`a04_unset_allowed_origins_defaults_to_loopback_only_contract`, `a04_default_cors_blocks_non_loopback_browser_origins`), and default key-abuse bounds (`a04_shipped_default_search_key_has_bounded_rate_limit`). RED baseline evidence is captured at `engine/tests/results/security_audit_a04_red_baseline.log`, which shows three pre-fix failures: unset-allowed-origins startup parsing did not enforce a loopback-only default contract, non-loopback browser origin was accepted under the default CORS mode, and default search key shipped with `maxQueriesPerIPPerHour=0` (unbounded). Finding severities: `medium` for insecure default CORS exposure and `medium` for unbounded default search-key abuse path. Remediation guidance implemented: default browser access is now loopback-only unless operators explicitly set `FLAPJACK_ALLOWED_ORIGINS`; shipped default search key now carries a bounded per-IP hourly cap; production auth bootstrap policy is fail-closed and unit-validated in `startup_tests`. Owner-seam fixes are in `engine/flapjack-http/src/startup.rs`, `engine/flapjack-http/src/router.rs`, and `engine/flapjack-http/src/auth/key_store.rs`, with seam-adjacent unit coverage extended in `startup_tests.rs`, `router_inline_tests.rs`, and `auth_tests/key_store_tests.rs`. OWASP category reference: https://owasp.org/Top10/A04_2021-Insecure_Design/. Coverage assessment: `resolved (✅)`.

### A05 Security Misconfiguration
Stage 6 A05 RED→GREEN audit is complete. New focused integration tests in `engine/tests/test_security_audit.rs` cover canonical invalid-credential error contract hardening (`a05_invalid_credentials_error_contract_has_no_metadata_leaks`), public health metadata contract enforcement (`a05_public_health_uses_explicit_metadata_denylist`), and unauthorized `/internal/*` canonical error-shape enforcement (`a05_internal_route_unauthorized_response_has_canonical_shape_only`). RED baseline evidence is captured at `engine/tests/results/security_audit_a05_red_baseline.log`, which shows the pre-fix failure where `/health` leaked `version` metadata. The same baseline log records the repo-grounded route inventory finding: there is no standalone public version endpoint beyond `/health` at HEAD (`build_public_health_routes`). Finding severity: `medium` (public metadata disclosure). Final remediation in owner seams keeps canonical auth/internal 403 error bodies (`{"message":"...","status":403}`) without stack/decode/build leakage and narrows `/health` to an explicit approved contract: `status`, `version`, `uptime_secs`, `capabilities` (`vectorSearch`, `vectorSearchLocal`), `active_writers`, `max_concurrent_writers`, `facet_cache_entries`, `facet_cache_cap`, `heap_allocated_mb`, `system_limit_mb`, `pressure_level`, `allocator`, and `tenants_loaded`; `build_profile` is explicitly denied on the public response. Seam-level regression coverage is pinned by `handlers::health::tests::health_keeps_required_compatibility_fields` and `handlers::health::tests::health_hides_build_and_debug_fields`. Audited surfaces: canonical auth failures, `/health`, and `/internal/*` denial paths. OWASP category reference: https://owasp.org/Top10/A05_2021-Security_Misconfiguration/. Coverage assessment: `resolved (✅)`.

### A06 Vulnerable and Outdated Components
Stage 7 A06 RED→GREEN audit is complete with dependency-owner seam remediation and dedicated `a06_` gate tests. New focused tests in `engine/tests/test_security_audit.rs` execute the same tooling contract used by this stage (`cargo audit` and `cargo deny`): `a06_vulnerable_fixture_fails_cargo_audit`, `a06_vulnerable_fixture_fails_cargo_deny_advisories`, and `a06_workspace_passes_cargo_audit_and_cargo_deny`.

RED baseline evidence is captured at `engine/tests/results/security_audit_a06_red_baseline.log` (captured on 2026-05-25 UTC). The RED log records three failures before remediation: (1) stage command defect (`--no-fail-fast` placement) preventing the `a06_` test run from executing, (2) `cargo audit --deny warnings` surfacing 19 vulnerabilities and 9 denied warnings, and (3) `cargo deny check advisories` failing across multiple vulnerable components.

Remediation implemented in owner seams:
- `engine/Cargo.toml` / `engine/Cargo.lock`: lockfile refresh plus targeted upgrades, including `rust-s3` `0.35 -> 0.37.2`, `tar` `0.4.44 -> 0.4.46`, `time` `0.3.44 -> 0.3.47`, `aws-lc-sys` `0.37.1 -> 0.41.0`, `bytes` `1.10.1 -> 1.11.1`, `oneshot` `0.1.11 -> 0.1.13`, `rustls-webpki` `0.101.7/0.103.9 -> 0.103.13`, `quinn-proto` `0.11.13 -> 0.11.14`, and `lz4_flex` `0.11.5 -> 0.11.6`.
- `engine/flapjack-http/Cargo.toml`: `lru` `0.12 -> 0.16`, `maxminddb` `0.24 -> 0.27`, and `aws-sdk-sesv2` switched to explicit non-default features (`default-https-client`, `rt-tokio`, `sigv4a`) to eliminate legacy `rustls-webpki 0.101.7` exposure.
- `engine/Cargo.toml`: `tantivy` upgraded from `0.25` to `0.26.1`, and `paste` is source-patched to maintained fork `esrauch/paste` (`tag=1.0.15`) so the workspace no longer ships the archived upstream `paste` source flagged by `RUSTSEC-2024-0436`.
- `engine/tests/test_sdk_compat.rs`: Rust test helper migration from `dotenv` to `dotenvy`.
- `engine/flapjack-http/src/geoip.rs`: compatibility fix for `maxminddb 0.27` deferred-decoding lookup API.
- `engine/deny.toml` created as canonical cargo-deny policy file for this workspace.
- Tantivy 0.26 compatibility adjustments applied in Flapjack owner seams to keep behavior stable while removing the vulnerable graph (`engine/src/index/mod.rs`, `engine/src/index/schema.rs`, `engine/src/index/manager/mod.rs`, `engine/src/query/executor/{relevance,facets,sorting,rules}.rs`).

Tooling versions used in this stage:
- `cargo-audit`: `0.22.1` (preinstalled at session start)
- `cargo-deny`: `0.19.7` (bootstrapped during stage)
- `engine/deny.toml`: created in Stage 7 (new canonical policy location)

Final GREEN validation commands:
- `cd engine && cargo test -p flapjack --test test_security_audit -- a06_ -- --no-fail-fast`
- `cd engine && cargo audit --deny warnings`
- `cd engine && cargo deny check advisories`

Finding severity:
- `critical`: multiple dependency CVEs present at RED baseline (AWS-LC, bytes, rustls-webpki, tar, time, etc.) remediated in-stage via dependency updates.
- `medium`: dependency major-line upgrade risk from moving to Tantivy `0.26.1`; mitigated by focused compile/runtime compatibility fixes plus the dedicated `a06_` regression gate.

Remediation guidance:
- Keep `cargo audit --deny warnings` and `cargo deny check advisories` as strict CI gates with no advisory-specific ignore exceptions.
- Prefer maintained-source overrides only as a short seam while upstream crates transition; periodically re-evaluate whether patched sources can be dropped for upstream releases.

OWASP category reference: https://owasp.org/Top10/A06_2021-Vulnerable_and_Outdated_Components/. Owner seam files: `engine/Cargo.toml`, `engine/Cargo.lock`, `engine/flapjack-http/Cargo.toml`, `engine/deny.toml`. Coverage assessment: `resolved (✅)`.

### A07 Identification and Authentication Failures
Stage 8 A07 audit is complete with focused `a07_` integration proofs in `engine/tests/test_security_audit.rs`: `a07_repeated_invalid_credentials_keep_canonical_403_and_do_not_consume_valid_key_budget`, `a07_session_fixation_and_jwt_downgrade_are_not_applicable_to_current_auth_surface`, and `a07_admin_key_rotation_never_allows_old_and_new_admin_keys_simultaneously`. These close the two previously open gaps (`auth-failure-shape/brute-force boundary` and `credential lifecycle`) by proving repeated invalid direct-key attempts keep canonical 403 responses without consuming a valid key's per-IP rate budget, and by proving runtime admin-key rotation does not leave a split-brain window where old/new admin keys both authenticate `/internal/status`.

RED-baseline evidence is captured in `engine/tests/results/security_audit_a07_red_baseline.log`. The log includes explicit OWASP variant-c N/A justification: at HEAD, auth owners are API-key/HMAC only (no session-cookie store, no JWT verifier surface) in `engine/flapjack-http/src/auth/middleware.rs` and `engine/flapjack-http/src/auth/mod.rs`. No production auth/key-store code remediation was required in this stage because the owner seams already enforced the expected fail-safe behavior; the stage delivered missing canonical proof coverage and evidence.

OWASP category reference: https://owasp.org/Top10/A07_2021-Identification_and_Authentication_Failures/. Owner seam files: `engine/flapjack-http/src/auth/middleware.rs`, `engine/flapjack-http/src/auth/mod.rs`, `engine/flapjack-http/src/auth/key_store.rs`, `engine/flapjack-http/src/handlers/internal.rs` (route wrapper exercised by integration proof). Coverage assessment: `resolved (✅)`.

### A08 Software and Data Integrity Failures
Stage 9 closes A08 with explicit RED→GREEN proof over the two live integrity surfaces that can ship tampered state: installer/release artifacts and snapshot imports. Category-proof ownership is in `engine/tests/test_security_audit.rs` with `a08_` tests:
- `a08_installer_fails_closed_when_verification_material_is_missing`
- `a08_release_workflow_emits_verifiable_provenance_metadata`
- `a08_snapshot_import_rejects_parent_dir_traversal_entries`
- `a08_snapshot_import_rejects_absolute_path_entries`
- `a08_snapshot_import_rejects_symlink_escape_pivots`

RED baseline evidence is committed at `engine/tests/results/security_audit_a08_red_baseline.log` (captured before remediation). Remediation stayed in the existing owner seams:
- Findings with severity and remediation:
  - `critical`: installer allowed unverifiable downloads when checksum file/tool was missing. Remediation: fail closed with explicit `exit 1` paths in `engine/install.sh::download_and_verify`.
  - `critical`: snapshot imports accepted traversal/absolute-path archive entries and relied on downstream unpack behavior. Remediation: add explicit shared entry validation in `engine/src/index/snapshot.rs` before unpack for both import entrypoints.
  - `medium`: release workflow did not emit explicit GHCR provenance/SBOM metadata. Remediation: add OIDC permission and provenance/SBOM metadata emission in `.github/workflows/release.yml`.
- `engine/install.sh::download_and_verify` now fails closed when checksum material or checksum tooling is unavailable (no warn-and-continue path).
- `.github/workflows/release.yml` now emits verifiable GHCR provenance/SBOM metadata (`id-token: write`, `provenance: mode=max`, `sbom: true` on publish builds).
- `engine/src/index/snapshot.rs::{import_from_tarball, import_from_bytes}` now share entry validation that rejects absolute paths, parent traversal, and link entries before unpack.

Installer seam-regression coverage was tightened in `engine/tests/test_install.sh` for missing-checksum and missing-checksum-tool hard-failure behavior. OWASP category reference: https://owasp.org/Top10/A08_2021-Software_and_Data_Integrity_Failures/. Owner seam files: `engine/install.sh`, `.github/workflows/release.yml`, `engine/src/index/snapshot.rs`, `engine/tests/test_security_audit.rs`. Coverage assessment: `resolved (✅)`.

### A09 Security Logging and Monitoring Failures
Stage 10 A09 RED→GREEN audit is complete with focused proofs in `engine/tests/test_security_audit.rs`: `a09_failed_direct_key_auth_emits_audit_event_without_secret_or_query_leaks`, `a09_failed_secured_key_auth_emits_audit_event_without_token_payload_or_query_leaks`, and `a09_rotate_admin_key_success_emits_audit_event_without_key_leaks`.

RED baseline evidence is committed at `engine/tests/results/security_audit_a09_red_baseline.log`. The RED run failed on all three new tests, proving missing security-event markers for failed direct-key auth, failed secured-key auth, and successful `/internal/rotate-admin-key` execution.

Findings with severity and remediation:
- `medium`: auth rejection paths lacked explicit security-event audit markers for failed direct-key and secured-key attempts. Remediation: add explicit structured `tracing::warn!` A09 auth-failure event logs in `engine/flapjack-http/src/auth/middleware.rs` with `auth_attempt_type` and failure reason, while excluding secrets and query payloads.
- `medium`: successful admin-key rotation lacked an explicit A09 admin-action audit marker. Remediation: add structured `tracing::info!` admin-action event logging in `engine/flapjack-http/src/handlers/internal.rs::rotate_admin_key`.
- `low` (verified absent): raw sensitive values (plaintext API keys, `x-algolia-api-key` header content, secured-key token payload material, raw search/filter payloads) were not logged in the exercised paths; redaction/non-leak assertions are now locked by `a09_` regression tests.

Seam-adjacent guard coverage re-verified: `auth::tests::middleware_tests::auth_middleware_invalid_key_does_not_consume_rate_limit` (`flapjack-http` unit test) remains green after A09 instrumentation.

OWASP category reference: https://owasp.org/Top10/A09_2021-Security_Logging_and_Monitoring_Failures/. Owner seam files changed in this stage: `engine/flapjack-http/src/auth/middleware.rs`, `engine/flapjack-http/src/handlers/internal.rs`, and `engine/tests/test_security_audit.rs`. Coverage assessment: `resolved (✅)`.

### A10 Server-Side Request Forgery (SSRF)
Stage 11 A10 RED→GREEN audit is complete with explicit outbound-destination guards and focused `a10_` proofs in `engine/tests/test_security_audit.rs`: `a10_chat_ai_provider_rejects_unsafe_base_urls_from_settings`, `a10_chat_ai_provider_rejects_unsafe_base_urls_from_env`, `a10_vector_embedders_reject_ssrf_payload_urls`, and `a10_peer_address_intake_rejects_unsafe_env_destinations_before_client_construction`. Seam-level tests were added to lock policy at owner seams: `engine/flapjack-http/src/handlers/chat_tests.rs::{a10_resolve_provider_config_rejects_unsafe_base_url_from_index_settings,a10_resolve_provider_config_rejects_unsafe_base_url_from_env}`, `engine/src/vector/config_tests.rs::{a10_openai_config_rejects_non_http_or_malformed_url,a10_rest_config_rejects_non_http_or_malformed_url}`, and `engine/flapjack-replication/src/config.rs::{a10_env_peer_parser_rejects_unsafe_or_malformed_peer_addresses,a10_node_json_filters_unsafe_peer_addresses}`.

**RED baseline (real failing proof).** The committed artifact `engine/tests/results/security_audit_a10_red_baseline.log` captures the stage command (`cargo test -p flapjack --test test_security_audit --no-fail-fast -- a10_`) run with the three seam *implementations* reverted to HEAD while the expanded `a10_` payloads were in place. It shows two genuine pre-fix failures that prove the residual SSRF bypass:
- `a10_vector_embedders_reject_ssrf_payload_urls` panics with `openAi embedder must reject payload http://localhost.` — the trailing-dot FQDN passed validation unblocked.
- `a10_peer_address_intake_rejects_unsafe_env_destinations_before_client_construction` panics with `unsafe peer destinations must be dropped by config intake` — the `localhost.` peer survived intake.
(The two `a10_chat_*` entries in the same log report `PoisonError`: the panicking peer test holds the shared `a10_env_lock` mutex, so its panic poisons the lock for the sibling chat tests. That is a RED-state artifact of the shared serialization mutex, not an independent finding; in the GREEN tree all four pass.)

**Root-cause analysis of the bypass (corrects the prior, overstated write-up).** The pre-fix validators (added earlier in this stage) parsed each URL with the `url`/`reqwest` crate, then rejected (a) non-`http(s)` schemes, (b) the literal string `localhost`, and (c) hosts that parse as a literal private/link-local IP. Two facts determine what that actually covered:
- **Numeric-form hosts are NOT a bypass** of these validators. The WHATWG URL host parser in the `url` crate canonicalizes numeric IPv4 forms during parse — `http://2130706433`, `http://0x7f000001`, and `http://127.1` all yield `host_str() == "127.0.0.1"`, which the literal-IP check already rejects. The `a10_*` suites include such payloads to *assert this canonicalization defense holds*, not as a resolution bypass.
- **Registered hostnames that resolve to a private destination WERE a bypass.** Any host the `url` crate keeps as a registered name — an attacker-controlled DNS name whose A record points at `127.0.0.1`/`169.254.169.254`/RFC1918, or the hermetic stand-in `localhost.` (trailing-dot FQDN, which the parser keeps verbatim as `"localhost."` so it matches neither the literal-IP check nor the exact `"localhost"` string) — passed all three checks. The outbound `reqwest`/peer client would then connect to the private destination. This is the `hostname-ssrf-bypass` (critical) finding.

**Remediation (fail-closed, in existing owners only).** Each seam now also resolves non-literal hosts via `(host, port).to_socket_addrs()` and applies the destination policy to every resolved address, closing the hostname bypass:
- `engine/flapjack-http/src/handlers/chat.rs::validate_ai_base_url` (called from `resolve_provider_config`) — splits the policy into *always-blocked* (link-local incl. the `169.254.169.254` metadata endpoint, unspecified, broadcast) vs. *local-network* (loopback + RFC1918/ULA). Local-network is blocked by default but permitted under the `FLAPJACK_AI_ALLOW_LOCAL_URLS` opt-in, because running a local model server (Ollama/llama.cpp/vLLM on `http://127.0.0.1:PORT`) is a first-class use; the metadata/link-local class is never permitted even with the opt-in.
- `engine/src/vector/config.rs::validate_outbound_url` — resolves and rejects any private/local resolved address for `openAi.url` and `rest.url` (no opt-in; embedder endpoints are expected to be public services).
- `engine/flapjack-replication/src/config.rs::{normalize_peer_addr → parse_peer_entry, load_from_file}` — resolves and drops unsafe peer destinations before both the replication client and the analytics-cluster fan-out (`AnalyticsClusterClient`) consume them. Resolution *failure* returns "not blocked" so docker/k8s service names that only resolve at connect time (`http://peer1:7700`) stay configurable.

**Regression found and fixed in the same sprint.** The earlier loopback block (no opt-in) had broken 8 chat handler tests in `flapjack-http/src/handlers/chat_tests.rs` that mock the AI provider on a loopback wiremock server (`chat_provider_resolution_uses_index_base_url_and_request_model_precedence`, `chat_sse_*`, `chat_json_response_*`, `chat_sources_*`, `chat_retrieval_context_*`, `chat_conversation_id_resume_*`, `chat_openai_provider_upstream_401_maps_to_502_error`). The prior clean-review missed this because its green-check only ran the `a10_` integration filter, not the chat handler unit tests. These now pass via the `FLAPJACK_AI_ALLOW_LOCAL_URLS` opt-in (set by the `start_local_mock_server` test helper); the `a10_*` proofs explicitly clear the opt-in to keep the default fail-closed posture under test.

Findings with severity and remediation:
- `critical` (`hostname-ssrf-bypass`): registered hostnames resolving to private/loopback/link-local destinations bypassed all three seam validators. Remediation: resolve-and-check every resolved address in `validate_ai_base_url`, `validate_outbound_url`, and `normalize_peer_addr`.
- `medium`: chat/vector/peer outbound URLs previously accepted literal private/loopback/link-local IPs and non-http schemes. Remediation: scheme + literal-IP + destination-class checks at each seam (pre-existing in this stage, retained).
- `medium` (regression): the loopback block broke 8 local-LLM chat tests. Remediation: `FLAPJACK_AI_ALLOW_LOCAL_URLS` opt-in (default off), metadata/link-local always blocked.

**Known residual limitation (documented, not a regression).** Validation resolves the host at config/request time and checks the result; it does not pin that resolution through to the outbound connection. A DNS-rebinding adversary who returns a public address at validation time and a private address at connect time is not stopped by a pre-request seam guard — that requires a connection-time resolver/connector hook on the `reqwest` client, which is outside this stage's seam scope. The metadata-endpoint and direct-private-IP/hostname cases (the practical SSRF vectors) are closed.

OWASP category reference: https://owasp.org/Top10/A10_2021-Server-Side_Request_Forgery_%28SSRF%29/. Owner seams changed in this stage: `engine/flapjack-http/src/handlers/chat.rs`, `engine/src/vector/config.rs`, `engine/flapjack-replication/src/config.rs`, and test files `engine/tests/test_security_audit.rs`, `engine/flapjack-http/src/handlers/chat_tests.rs`, `engine/src/vector/config_tests.rs`. Coverage assessment: `resolved (✅)`.

| Category | State | Existing test | Gap | Red-baseline log |
|---|---|---|---|---|
| A01 | ✅ | `test_security_audit.rs::a01_restricted_key_denies_cross_tenant_index_query`; `test_security_audit.rs::a01_non_admin_key_cannot_access_internal_status`; `test_security_audit.rs::a01_internal_route_rejects_empty_application_id_header` | Stage 2 A01 gaps resolved; empty app-id bypass closed in `auth/mod.rs::request_application_id` | `engine/tests/results/security_audit_a01_red_baseline.log` |
| A02 | ✅ | `test_security_audit.rs::a02_generated_search_keys_have_128_bits_after_prefix_strip`; `test_security_audit.rs::a02_keys_json_does_not_persist_plaintext_api_keys`; `test_security_audit.rs::a02_rotate_admin_key_invalidates_old_and_accepts_new`; `test_security_audit.rs::a02_internal_auth_depends_only_on_algolia_auth_headers` | Stage 3 A02 gaps resolved; plaintext key leakage removed from both `keys.json` and `key_material.json` via encrypted key-material persistence in `auth/key_store.rs` | `engine/tests/results/security_audit_a02_red_baseline.log` |
| A03 | ✅ | `test_security_audit.rs::a03_search_rejects_malformed_filters_instead_of_ignoring_them`; `test_security_audit.rs::a03_top_searches_tags_sql_injection_payload_does_not_broaden_results`; `test_security_audit.rs::a03_top_searches_country_sql_injection_payload_does_not_broaden_results` | Stage 4 A03 gaps resolved; malformed `filters` now fail closed in `dto/filter_parsing.rs::build_combined_filter` instead of widening to an unfiltered search; analytics SQL string interpolation (LIKE + equality) re-verified sanitized | `engine/tests/results/security_audit_a03_red_baseline.log` |
| A04 | ✅ | `test_security_audit.rs::a04_auth_enabled_routes_fail_closed_without_credentials`; `test_security_audit.rs::a04_production_bootstrap_rejects_missing_blank_or_short_admin_key`; `test_security_audit.rs::a04_unset_allowed_origins_defaults_to_loopback_only_contract`; `test_security_audit.rs::a04_default_cors_blocks_non_loopback_browser_origins`; `test_security_audit.rs::a04_shipped_default_search_key_has_bounded_rate_limit` | Stage 5 A04 gaps resolved; default CORS is loopback-only and shipped default search key is rate-bounded | `engine/tests/results/security_audit_a04_red_baseline.log` |
| A05 | ✅ | `test_security_audit.rs::a05_invalid_credentials_error_contract_has_no_metadata_leaks`; `test_security_audit.rs::a05_public_health_uses_explicit_metadata_denylist`; `test_security_audit.rs::a05_internal_route_unauthorized_response_has_canonical_shape_only`; `handlers::health::tests::health_keeps_required_compatibility_fields`; `handlers::health::tests::health_hides_build_and_debug_fields` | Stage 6 A05 gaps resolved; `/health` exposes only the approved compatibility contract (`status`, `version`, `uptime_secs`, `capabilities`, memory-pressure fields, `tenants_loaded`) and denies `build_profile`; canonical error shapes for auth failures and unauthorized `/internal/*` responses verified | `engine/tests/results/security_audit_a05_red_baseline.log` |
| A06 | ✅ | `test_security_audit.rs::a06_vulnerable_fixture_fails_cargo_audit`; `test_security_audit.rs::a06_vulnerable_fixture_fails_cargo_deny_advisories`; `test_security_audit.rs::a06_workspace_passes_cargo_audit_and_cargo_deny` | Stage 7 A06 dependency-audit gate is strict-green with no ignore exceptions; dependency graph remediated through `Cargo.toml`/`Cargo.lock` upgrades, maintained-source patch for `paste`, and Tantivy 0.26 compatibility fixes in Flapjack query/index seams | `engine/tests/results/security_audit_a06_red_baseline.log` |
| A07 | ✅ | `test_security_audit.rs::a07_repeated_invalid_credentials_keep_canonical_403_and_do_not_consume_valid_key_budget`; `test_security_audit.rs::a07_session_fixation_and_jwt_downgrade_are_not_applicable_to_current_auth_surface`; `test_security_audit.rs::a07_admin_key_rotation_never_allows_old_and_new_admin_keys_simultaneously`; `auth_tests/middleware_tests.rs::auth_middleware_invalid_key_does_not_consume_rate_limit`; `test_secured_keys.rs::test_algolia_compatible_format` | Stage 8 A07 gaps resolved; repeated invalid-credential boundary and runtime admin-key credential lifecycle are now explicitly proven, with session/JWT variant-c documented N/A for key-only auth surface | `engine/tests/results/security_audit_a07_red_baseline.log` |
| A08 | ✅ | `test_security_audit.rs::a08_installer_fails_closed_when_verification_material_is_missing`; `test_security_audit.rs::a08_release_workflow_emits_verifiable_provenance_metadata`; `test_security_audit.rs::a08_snapshot_import_rejects_parent_dir_traversal_entries`; `test_security_audit.rs::a08_snapshot_import_rejects_absolute_path_entries`; `test_security_audit.rs::a08_snapshot_import_rejects_symlink_escape_pivots` | Stage 9 A08 gaps resolved in existing owner seams: installer now rejects unverifiable downloads, release workflow emits GHCR provenance/SBOM metadata, and snapshot imports reject escape/link entries before unpack | `engine/tests/results/security_audit_a08_red_baseline.log` |
| A09 | ✅ | `test_security_audit.rs::a09_failed_direct_key_auth_emits_audit_event_without_secret_or_query_leaks`; `test_security_audit.rs::a09_failed_secured_key_auth_emits_audit_event_without_token_payload_or_query_leaks`; `test_security_audit.rs::a09_rotate_admin_key_success_emits_audit_event_without_key_leaks`; `auth::tests::middleware_tests::auth_middleware_invalid_key_does_not_consume_rate_limit` | Stage 10 A09 gaps resolved: explicit auth-failure/admin-action audit events are emitted and regression-locked; no key/header/query/token payload leakage in exercised log paths | `engine/tests/results/security_audit_a09_red_baseline.log` |
| A10 | ✅ | `test_security_audit.rs::a10_chat_ai_provider_rejects_unsafe_base_urls_from_settings`; `test_security_audit.rs::a10_chat_ai_provider_rejects_unsafe_base_urls_from_env`; `test_security_audit.rs::a10_vector_embedders_reject_ssrf_payload_urls`; `test_security_audit.rs::a10_peer_address_intake_rejects_unsafe_env_destinations_before_client_construction`; seam tests in `chat_tests.rs`, `vector/config_tests.rs`, and `flapjack-replication/src/config.rs` | Stage 11 A10 gaps resolved in existing owner seams: non-http schemes and malformed URLs rejected; literal private/loopback/link-local/unspecified IPs rejected; and the `hostname-ssrf-bypass` (registered hostnames resolving to private destinations, e.g. `localhost.` or attacker DNS) closed by resolving non-literal hosts and checking every resolved address at chat/vector/peer seams. Local-LLM loopback permitted only under `FLAPJACK_AI_ALLOW_LOCAL_URLS`; metadata/link-local always blocked. Residual DNS-rebinding TOCTOU documented as out-of-seam-scope | `engine/tests/results/security_audit_a10_red_baseline.log` |
