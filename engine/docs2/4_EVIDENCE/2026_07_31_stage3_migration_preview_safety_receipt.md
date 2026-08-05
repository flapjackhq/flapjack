# 2026-07-31 Stage 3 Migration Preview Safety Receipt

## Purpose And Scope

This receipt records the local Stage 3 test audit for migration preview. It proves the Algolia fixture preview is byte-identical across durable state and records the real-source provider evidence without widening the claim. It does not authorize or perform deployment, release, sync, AWS, or Fjcloud work.

The Stage 3 test, harness, and initial receipt are committed at `fc6ba208487e4293a2a5bc52beae15f98c68cf8b`. A verification audit reran the fail-capable checks from clean starting HEAD `9e0ef4ee9dbb200d7345d258d14c76798c40b1e3`.

## Durable-State Proof

`handlers::migration::preview_tests::preview_does_not_write_durable_state_byte_identity` seeds `shop` and `bystander` through `IndexManager`, then adds nonempty sentinels under `migration_exports/jobs`, the `migration_exports` root, and `.publication`. Its deterministic recursive snapshot records every relative path, directory/file kind, and SHA-256 digest for file bytes.

The exact pre-preview specimen count was `51`. The post-preview snapshot had the same 51 paths, kinds, and file digests. This covers the full temporary data root, including both destination indexes and all named job, spool-adjacent, publication, and root-metadata surfaces.

Owner anchors at this HEAD:

- `mod.rs::preview_source_migration`, lines 1203-1246, reads the source snapshot, collects replica settings, and stops after `translation_session.rs::translate_spool_report` (defined at lines 128-142).
- Publication admission starts separately at `mod.rs::submit_source_migration_impl`, lines 1249-1270, through `MigrationJobRunner::submit_source_import_for_owner` at line 1264.
- Durable spool construction is owned by `import.rs::spool_for_manager`, lines 397-400.
- Publication/staging begins in `import.rs::import_accepted_export_inner`, lines 492-506, at `BulkBuildService::prepare_publication`.

### RED mutation specimen

The fail-capable check was proved by temporarily adding one mutation immediately after `translation::translate_spool_report` in `preview_source_migration`: when the test-only `FJ_STAGE3_RED_MUTATION_ROOT` was present, production wrote the exact bytes `red mutation` to `migration_exports/jobs/preview_mutation_sentinel` beneath that root. The test temporarily supplied the root through the same environment variable. The corrected focused command failed as intended:

```text
$ bash -lc 'cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests::preview_does_not_write_durable_state_byte_identity 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"'
assertion `left == right` failed: durable specimen count changed
  left: 52
 right: 51
exit=101
```

Both temporary hooks were reverted. The same command then passed with `1 passed`, `0 failed`, and `exit=0`; a separate `--nocapture` run emitted `durable_state_specimens=51`.

## Provider Method Table

| Provider | Existing source owner | Preview method and observation | Disposition |
| --- | --- | --- | --- |
| Algolia | `algolia_source_export_live.sh` and the existing scripted source reader | Fixture route proof returned the translation owner's exact report and counts `indexes=1`, `records=3`. The live owner requires `--secret-file`; no documented secret-file input was supplied, and this stage is local-only. | Fixture-proven only. No real-Algolia preview claim. |
| Meilisearch | `meilisearch_source_contract_kat.sh::{start_live_source,seed_live_source,cleanup_live}` and `mod.rs::meilisearch_source_reader` | `--preview-live` seeded the pinned local source, completed its existing source-capture assertions, and invoked the ignored production-route probe before cleanup. The route returned HTTP 400 before capture because the production constructor refused the loopback endpoint. | Real source lifecycle reached; production preview parity remains open. |
| Typesense | `typesense_migration_contract.sh` and `mod.rs::ensure_source_provider_supported` | Pinned real-source contract KAT passed. The focused route assertion returned HTTP 400 with exact code `source_provider_unsupported` and the canonical message. | Honest unsupported-runtime proof; no Typesense preview adapter claim. |

At the preview probe's lifecycle position, `validate_controlled_mutation` has already added `SKU-004`. The intended Meilisearch preview response for `configured_pk`, therefore, derives `sourceCounts.records=4` from the fixture owner's `.documents.countAfter`; `sourceCounts.indexes=1`. The expected report still uses this exact ordered code list from the existing translation owner:

```text
ProductNotMigrated (five entries)
MeilisearchDocumentOrderNotContractual
MeilisearchSearchPaginationNotExportBound
MeilisearchSettingNotMigrated (five entries)
MeilisearchSettingValueNormalized
```

Those response values were not observed through the production route, so they are not recorded as passing evidence.

## Exact Command Evidence

```text
$ bash -lc 'cd engine && timeout 600 bash tests/meilisearch_source_contract_kat.sh --preview-live 2>&1 | tail -60; echo "exit=${PIPESTATUS[0]}"'
running 1 test
assertion `left == right` failed: preview error body: {"message":"Meilisearch Cloud endpoint is not allowed","status":400}
  left: 400
 right: 200
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2053 filtered out
exit=101

$ bash -lc 'cd engine && timeout 600 env FJ_TYPESENSE_WRITE_FREEZE_ATTESTED=1 bash tests/typesense_migration_contract.sh 2>&1 | tail -60; echo "exit=${PIPESTATUS[0]}"'
PASS: Typesense migration source contract KAT verified
image=typesense/typesense:30.2@sha256:610f2d34b1f93d00762869da2c67736775e5798d19a2c8b91b014b8a0cc1e110 expected_bundle=tests/fixtures/2026_07_26_m0b_typesense_migration/expected_bundle.json
exit=0

$ bash -lc 'cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests::typesense_preview_returns_exact_unsupported_provider_error 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"'
running 1 test
test handlers::migration::preview_tests::typesense_preview_returns_exact_unsupported_provider_error ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 2053 filtered out
exit=0

$ bash -lc 'cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"'
running 4 tests
test handlers::migration::preview_tests::meilisearch_live_preview_reports_exact_seeded_counts_and_codes ... ignored
test handlers::migration::preview_tests::typesense_preview_returns_exact_unsupported_provider_error ... ok
test handlers::migration::preview_tests::preview_http_report_matches_translation_owner_and_exact_source_counts ... ok
test handlers::migration::preview_tests::preview_does_not_write_durable_state_byte_identity ... ok
test result: ok. 3 passed; 0 failed; 1 ignored; 2050 filtered out
exit=0
```

The checklist's literal filters `migration::preview::preview_does_not_write_durable_state_byte_identity` and `migration::preview` each exited 0 while selecting zero tests. They are not passing evidence. The real test module is `handlers::migration::preview_tests`; the `--lib` commands above both select the intended library test binary and show nonzero passing-test counts.

The exact residue guard after each live-source command reported only:

```text
stage2reviewseed6549-meilisearch-1
stage2reviewseed6549-typesense-1
```

Docker metadata identifies both as pre-existing Compose containers created on 2026-07-29 by a different workspace. The Stage 3 harness-owned `flapjack_stage2_meilisearch_source_contract` container was absent after failure and was not cleaned a second time. No unrelated container was modified.

## Broader Local Gate

The checklist command `cd engine && ./s/test --quick` returned `quick exit=1` because the canonical runner rejects the unknown `--quick` flag. The supported default was run under the required 600-second bound instead:

```text
$ cd engine && timeout 600 ./s/test; echo "default exit=$?"
Rust library lanes: PASS
Integration setup failure: target/debug/flapjack was absent
default exit=100

$ cd engine && cargo build -p flapjack-server 2>&1 | tail -30
Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 40s

$ cd engine && timeout 600 ./s/test; echo "default exit=$?"
Rust library lanes: PASS
Integration: 311 passed before fail-fast
legacy_quickstart_routes_are_removed_even_with_valid_auth: expected 404, observed 403
default exit=100
```

The first default-run failure is a pre-existing runner-order prerequisite: `_dev/s/test` runs integration at lines 467-474, while its server binary lane is later at lines 478-483; `test_analytics_ip_e2e.rs:88-94` explicitly requires the debug binary. Building the documented binary allowed that test to pass.

The second failure is unrelated to migration preview. The exact focused test reproduced with the same `expected 404, observed 403` assertion and `exit=101` at the unmodified Stage 2 base `99fafff8bc26a585bed02f9f3f668dfc650c2480`, so it is pre-existing rather than a Stage 3 regression. Per regression quarantine, Stage 3 did not modify that test or routing behavior.

## Diagnosed Gaps And Conditional Disposition

Gap spec:

1. `MeilisearchClient::new` at `meilisearch_client.rs:85-106` requires the strict HTTPS `*.meilisearch.io` vendor target, while the canonical pinned lifecycle owner exposes HTTP loopback. Therefore the required local real-source route cannot reach `source_reader::read_source_snapshot` without a production-owned secure integration seam or a credentialed cloud fixture.
2. Even after reachability is repaired, `source_reader.rs::normalize_meilisearch_settings` at lines 746-759 collects provider translation warnings into `warnings` and serializes only `normalized`; it discards the warnings. The preview report therefore cannot currently expose the eight fixture-specific Meilisearch warning entries owned by `meilisearch_settings.rs`.

Biased proxy offer: the existing live KAT is a strong proxy for pinned-source startup, seeding, pagination, settings/synonym capture, task polling, and cleanup. Its bias is explicit: it calls Meilisearch directly and therefore bypasses the production route constructor and preview report assembly. The scripted preview test is a strong proxy for the no-write route contract, but it uses the existing test reader factory and does not prove the real Meilisearch adapter.

Conditional disposition: local durable-state safety is proven. Real-source Meilisearch preview parity is **not complete**. A later product stage must add an owner-approved secure local integration seam (or use a documented credentialed cloud fixture), preserve the strict runtime SSRF boundary, propagate existing Meilisearch translation warnings into the canonical preview report, and rerun this exact probe. Stage 3 does not add a second source reader or change production behavior.

## Verification Audit Rerun

The validation cache was consulted before every command with the clean whole-repository tree at starting HEAD `9e0ef4ee9dbb200d7345d258d14c76798c40b1e3`. The focused preview suite, `git diff --check`, and clean status were valid cache hits for that exact tree and were not redundantly rerun. The cached suite result was `3 passed`, `0 failed`, `1 ignored`; the ignored test is the env-gated real-source Meilisearch probe.

Fresh command evidence:

```text
$ cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests::preview_does_not_write_durable_state_byte_identity 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"
test handlers::migration::preview_tests::preview_does_not_write_durable_state_byte_identity ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 2053 filtered out
exit=0

$ cd engine && timeout 600 bash tests/meilisearch_source_contract_kat.sh --preview-live 2>&1 | tail -60; echo "exit=${PIPESTATUS[0]}"
assertion `left == right` failed: preview error body: {"message":"Meilisearch Cloud endpoint is not allowed","status":400}
test result: FAILED. 0 passed; 1 failed; 0 ignored; 2053 filtered out
exit=101

$ cd engine && timeout 600 env FJ_TYPESENSE_WRITE_FREEZE_ATTESTED=1 bash tests/typesense_migration_contract.sh 2>&1 | tail -60; echo "exit=${PIPESTATUS[0]}"
PASS: Typesense migration source contract KAT verified
exit=0

$ cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests::typesense_preview_returns_exact_unsupported_provider_error 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"
test handlers::migration::preview_tests::typesense_preview_returns_exact_unsupported_provider_error ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 2053 filtered out
exit=0
```

The exact residue guard again reported only the two pre-existing `stage2reviewseed6549` containers listed above. No Stage 3 command-owned container remained.

The corrected broad gate was attempted twice under the checklist's exact 600-second bound. The cold run finished every Rust library lane and compiled the integration profile, then timed out before integration execution. The warm rerun again passed every completed library test and began nextest; the wrapper terminated it at `295/842` tests (`277 passed`, `18 SIGTERM`, `547 not run`) with `default exit=124`. No assertion failed. Repeating the same non-code failure established that the canonical full runner exceeds the 600-second stage budget in this execution locality, so no third retry was launched.

## Review Remediation Audit

The review remediation made the live harness fail-capable without changing production behavior:

- `run_preview_probe` passes fixture-owned `.documents.countAfter` to the ignored route probe, so the post-mutation record expectation is `4` rather than the pre-mutation count `3`.
- Its inner network-bound Cargo command is bounded by `timeout 600` and requires both Cargo's `1 passed` summary and the probe's `"previewProof":"PASS"` JSON. A zero-match filter can no longer produce a passing harness receipt.
- `post_preview` now delegates to `post_provider_preview`, leaving one owner for preview auth headers and request construction.
- The durable specimen count is named and documents its IndexManager, KeyStore, and sentinel constituents.
- The default runner now builds `target/debug/flapjack` inside the integration lane before `cargo nextest run`. Its new source-contract test went red before the ordering fix and green afterward; `cargo test -p flapjack --test test_analytics_ip_e2e` then passed its one served-boundary test.

The existing harness meta-suite was extended from 39 to 42 fail-capable scenarios. Its three new cases prove fixture-count propagation, zero-match rejection, and the distinct 600-second timeout diagnosis. The focused Rust preview suite passed with `3 passed`, `0 failed`, and the one env-gated live probe ignored. The live command executed exactly one probe and again stopped at the already-recorded HTTP 400 endpoint-policy gap; because that boundary occurs before source export, count `4` remains fixture-derived rather than observed through the production route. Cleanup left no command-owned container.

Exact remediation evidence:

```text
$ cd engine && timeout 600 bash tests/meilisearch_source_contract_kat_test.sh
PASS preview_fixture_count
PASS preview_zero_match
PASS preview_timeout
PASS denominator=42/42
exit=0

$ bash -lc 'cd engine && timeout 600 bash tests/meilisearch_source_contract_kat.sh --preview-live 2>&1 | tail -60; echo "exit=${PIPESTATUS[0]}"'
running 1 test
assertion `left == right` failed: preview error body: {"message":"Meilisearch Cloud endpoint is not allowed","status":400}
test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 2053 filtered out
ERROR: preview probe test failed (exit=101)
exit=1

$ cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests
test result: ok. 3 passed; 0 failed; 1 ignored; 0 measured; 2050 filtered out
exit=0

$ cd engine && bash _dev/s/tests/test_runner_integration_binary_contract.sh
PASS: integration runner builds the required flapjack binary before nextest
exit=0

$ cd engine && timeout 600 cargo test -p flapjack --test test_analytics_ip_e2e
test served_search_persists_only_minimized_client_ip ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
exit=0
```

## Absent-Surface Review Remediation

`preview_leaves_absent_target_and_unopened_publication_namespace_absent` restores the two negative preconditions that the nonempty byte-identity fixture cannot represent. After router construction, it proves both `shop` and `.publication` start nonexistent. After a successful preview response, it independently proves the target remains absent and the publication namespace remains unopened.

Both postconditions were exercised with isolated test-only negative controls. Temporarily creating only `shop` after the response failed at the target-specific assertion:

```text
$ cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests::preview_leaves_absent_target_and_unopened_publication_namespace_absent 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"
preview must not create, stage, or publish an absent target index
test result: FAILED. 0 passed; 1 failed; 0 ignored; 2054 filtered out
exit=101
```

After reverting that mutation, temporarily creating only `.publication` failed at the namespace-specific assertion:

```text
$ cd engine && timeout 600 cargo test -p flapjack-http --lib -- handlers::migration::preview_tests::preview_leaves_absent_target_and_unopened_publication_namespace_absent 2>&1 | tail -30; echo "exit=${PIPESTATUS[0]}"
preview must not prepare an unopened publication namespace
test result: FAILED. 0 passed; 1 failed; 0 ignored; 2054 filtered out
exit=101
```

Both mutations were reverted. The same focused command then passed with `1 passed`, `0 failed`, and `exit=0`. The complete preview module passed with `4 passed`, `0 failed`, one env-gated live probe ignored, and `exit=0`. The Meilisearch harness meta-suite also passed all 42 scenarios, including its preview fixture-count, zero-match, and timeout guards.
