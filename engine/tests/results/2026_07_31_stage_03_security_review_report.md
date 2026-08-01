# Stage 3 Parser-Class Security Review

Date: 2026-07-31

## Scope

Reviewed the Stage 1 byte-slice denominator and the Stage 3 lane-owned parser seams:

- `engine/flapjack-http/src/auth/mod.rs`
- `engine/src/vector/config.rs`
- `engine/src/query_suggestions/config.rs`
- `engine/src/recommend/mod.rs`
- `engine/src/recommend/rules.rs`

## Findings

### Fixed: userProvided vector config accepted zero dimensions

Severity: Medium

Owner: `engine/src/vector/config.rs::EmbedderConfig::validate_required_fields`

Evidence: before the fix, `userProvided` embedders with `dimensions: 0` passed configuration admission. A zero-dimension vector configuration is a degenerate contract that can reach downstream vector owners with no useful search semantics.

Remediation applied: `validate_required_fields` now rejects `dimensions == 0` with a field-specific embedding error.

Regression coverage:

- `vector::config::tests::stage3_user_provided_rejects_zero_dimensions`
- `vector::config::tests::stage3_user_provided_dimensions_reject_negative_and_overflowing_json`
- `vector::config::tests::stage3_index_settings_validate_embedders_accepts_usize_max_without_allocating`

### No unfixed in-scope security findings

The auth, query-suggestions, and recommend hostile-input additions did not expose new lane-owned panics, lossy parses, path traversal escapes, or unbounded allocations.

## Denominator Summary

- Sites found: 25 in `/tmp/sec_g1_slice_sites.txt`
- Attacker-influenced and lane-owned byte-slice sites: 2
- Newly defective byte-slice sites: 0
- Newly defective parser-class findings outside byte slicing: 1
- Fixed in this stage: 1
- Already fixed in Stage 2: 2 stale denominator hits, both in `engine/flapjack-http/src/auth/mod.rs::validate_secured_key`
- Routed to another lane: 23 byte-slice sites

Stage 3 lane-owned coverage included auth secured-key/referer/restrict-source parsing, vector config admission, query-suggestions config/status/log storage parsing, and recommend env/rules/object-ID parsing. The Stage 1 denominator still contains the old pre-fix auth line numbers, but HEAD now splits the secured-key payload at byte offset 64 before UTF-8 validation.

## Validation

- `(cd engine && timeout 900 cargo test -p flapjack --test test_security_audit --no-fail-fast)` passed: 50 tests.
- `(cd engine && cargo test -p flapjack --features vector-search --test test_security_audit -- --list | rg "a10_vector_embedders_reject_ssrf_payload_urls")` passed: gated vector security test listed.
- `(cd engine && timeout 1800 cargo test -p flapjack --features vector-search --test test_security_audit --no-fail-fast)` passed: 51 tests.
- `(cd engine && timeout 1800 cargo test -p flapjack --lib --no-fail-fast)` passed: 2152 tests.
- `(cd engine && cargo test -p flapjack --features vector-search --lib -- --list | rg "stage3_user_provided_rejects_zero_dimensions|stage3_user_provided_dimensions_reject_negative_and_overflowing_json|stage3_index_settings_validate_embedders_accepts_usize_max_without_allocating|vector::embedder::tests::test_user_provided_validate_wrong_dimensions")` passed: all required names listed.
- `(cd engine && timeout 1800 cargo test -p flapjack --features vector-search --lib --no-fail-fast)` passed: 2309 tests.
- `(cd engine && timeout 1800 cargo test -p flapjack-http --lib --no-fail-fast)` failed on pre-existing branch regression `router_tests::migration_routes_preserve_admin_contract`; the same failure reproduced with this session's touched files stashed, while unmodified `origin/main` passed the focused test.
- `(cd engine && cargo check -p flapjack)` passed.
- `(cd engine && cargo check -p flapjack --features vector-search)` passed.
- `(cd engine && cargo check -p flapjack-http)` passed.
- `(cd engine && cargo clippy -p flapjack --features vector-search)` passed.
- `(cd engine && cargo clippy -p flapjack-http)` passed.
- `(cd engine && cargo fmt --check)` passed.
- `(cd engine && cargo build -p flapjack-server)` passed.
- `bash engine/tests/credential_parser_http_probe.sh` passed: 17 served assertions.

Feature-gated parser tests not executed: none observed. The analytics-gated recommend object-ID test executed in the default library run because `analytics` is a default feature.
