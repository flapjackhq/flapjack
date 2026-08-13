# Source metadata branch range

Purpose: freeze the exact two-commit denominator from
`codex/aug05_fjcloud_stage2_v1010_current_count` before the Typesense export
contract is changed.

The denominator was produced by
`git rev-list origin/main..codex/aug05_fjcloud_stage2_v1010_current_count | sort`
after the preserved branch-inventory preflight confirmed the branch remained
stranded at `b767c6dc58ee14831a6c44a82c773284f637c422` and retained the designated
successor `chats/icg/aug10_8pm_6_source_metadata_branch_integration.md`.

COMMIT_DISPOSITION sha=b6ed926ba4b247b0f0bd30c7d15a6abbe27cdf6b verdict=superseded owner=engine/tests/meilisearch_source_contract_kat_test.sh
COMMIT_DISPOSITION sha=b767c6dc58ee14831a6c44a82c773284f637c422 verdict=port owner=engine/flapjack-http/src/handlers/migration/typesense_client.rs
RED_PROOF sha=b767c6dc58ee14831a6c44a82c773284f637c422 rc=1 denominator=137 token=typesense_paginated_export_incompatible log_sha256=059be23081c952f48fb7284359dce212e4b7bfcf5385b2dcc547cbcd477e081c
RED_EXCERPT sha=b767c6dc58ee14831a6c44a82c773284f637c422 token=typesense_paginated_export_incompatible text=typesense_paginated_export_incompatible: running pinned live contract
LIVE_EXPORT_RED_COMMAND FJ_TYPESENSE_RUN_PRODUCTION_EXPORT_RED=1 bash engine/tests/typesense_migration_contract.sh
MUTATION_PROOF target=engine/tests/typesense_migration_contract.sh::run_production_export_stream_contract case=silent_default_off_skip red=57/58 restored_green=58/58 assertion=skipped_live_export_arm_announces_itself
MUTATION_PROOF target=engine/flapjack-http/src/handlers/migration/typesense_client_tests.rs::LiveTypesenseTransport case=live_fork_message_literal_drift red=1_failed restored_green=62_passed_1_ignored assertion=live_contract_transport_tracks_the_production_reqwest_transport
MUTATION_PROOF sha=b767c6dc58ee14831a6c44a82c773284f637c422 target=engine/flapjack-http/src/handlers/migration/source_identity_partitions.rs::PartitionLoad::sort case=no_op_partition_sort red=5_failed_18_passed restored_green=23_passed suites=source_identity_partitions_tests,source_snapshot_tests assertions=duplicate_validator_is_exact_across_hash_partitions,duplicate_validator_reports_first_and_second_for_cross_page_duplicate,duplicate_validator_selects_lowest_partition_then_sorted_object_id,newline_in_object_id_survives_spool_round_trip,source_snapshot_canonical_hashes_counts_and_membership_independent_of_item_order
MUTATION_PROOF target=engine/flapjack-http/src/handlers/migration/source_snapshot.rs::aggregate_source_item_hashes case=removed_resource_sort red=1_failed restored_green=1_passed assertion=source_snapshot_resource_hashes_are_independent_of_rule_synonym_and_replica_order
MUTATION_PROOF target=engine/tests/typesense_migration_contract.sh::run_production_export_stream_contract case=restored_default_off red=60/61 restored_green=61/61 assertion=production_export_stream_live_contract_runs_by_default

The Meilisearch count behavior is already owned by landed commit
`9df4cc76ae161a5af28e869e99a6f2f9a40a8ae5`. The Typesense commit is not
applied directly: Stage 1 captures its transport incompatibility as a red
contract, and Stage 2 owns the production transport repair through the named
client owner. The partition-sort mutation proof supersedes any identity-order
coverage claim from the stale branch by naming the canonical identity suites
that actually turn red under the defect.

## Focused test-owner audit

- `typesense_client_tests.rs` already owns malformed JSON, in-stream error
  objects, non-object payloads, response-byte limits, item-limit rejection,
  counted traversal, source capture, and restricted-access probes. The
  item-limit assertion was tightened to require one exact request and a
  sanitized limit error.
- The ignored live contract owns the missing 137-value, exact-ID,
  no-terminal-newline, single-export-request, query-free export, and
  discovery-export-free assertions through the production client path.
- `source_identity_partitions_tests` and `source_snapshot_tests` own the
  mutation-sensitive key/document-order independence guard for canonical source
  identity. A no-op `PartitionLoad::sort` mutation leaves `source_reader_tests`
  green at its fixture sizes, so source-reader coverage is not credited with
  that invariant.
- `source_reader_tests.rs::source_reader_document_identity_changes_for_insert_delete_and_in_place_update`
  owns same-count value sensitivity.
- `source_reader_tests.rs::typesense_reader_rejects_invalid_ids_with_sanitized_errors`
  already exercises missing, non-string, and duplicate IDs through the
  Typesense adapter, so no parallel adapter assertion was added.
- `source_snapshot_tests.rs::source_snapshot_resource_hashes_are_independent_of_rule_synonym_and_replica_order`
  owns rules, synonyms, and replica-settings resource hash independence from
  capture order. Removing the `aggregate_source_item_hashes` stable-ID sort now
  turns that focused test red.
