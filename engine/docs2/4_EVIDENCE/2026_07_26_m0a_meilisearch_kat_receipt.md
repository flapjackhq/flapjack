# M0A Meilisearch KAT Receipt

## PURPOSE

Record the compact verification receipt for the M0A Meilisearch source-contract
gate. Source research remains owned by
`engine/docs2/4_EVIDENCE/2026_07_26_jul26_am_12_meilisearch_source_contract.md`;
machine oracle values remain owned by
`engine/tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/expected_bundle.json`.

Verification HEAD: `9b44e66c34e11663f62adb8e98e7649899859d03`.
Verification date: 2026-07-26.

## Source Identity

- Meilisearch image:
  `getmeili/meilisearch@sha256:9694a59df43ee3f54b3fda9c5de381a3ee9852678e3e31cadf37d6bddea7fc1b`
- Version endpoint oracle:
  `{"commitSha":"2ecfd54ca7f3dd1c76d304efce86c9b8af9f82aa","commitDate":"unknown","pkgVersion":"1.50.0"}`
- Live command:
  `cd engine && bash tests/meilisearch_source_contract_kat.sh --live`
- Live runner binds only loopback `127.0.0.1`, uses the disposable container
  name `flapjack_stage2_meilisearch_source_contract`, and reads the fixture
  bundle at
  `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/expected_bundle.json`.

## Fixture Hash

Command:

```bash
cd engine && git ls-files -z tests/fixtures/2026_07_26_m0a_meilisearch_source_contract | sort -z | xargs -0 shasum -a 256 | shasum -a 256
```

Output:

```text
03ff26051f12ae9fa93b2678a140c38a416c0b7e6e33b2ae7d546f9922a4ab8f  -
```

Tracked fixture inputs:

- `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/ambiguous_primary_key_documents.json`
- `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/configured_primary_key_documents.json`
- `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/configured_primary_key_settings.json`
- `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/expected_bundle.json`
- `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/inferred_primary_key_documents.json`
- `tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/restricted_key_action_probes.json`

## Validation Evidence

All commands below were run or accepted through the required validation-cache
helper at `/Users/stuart/repos/gridl/mike_dev/matt_root/matt/validation_cache.py`.

| Command | Exit | Evidence |
| --- | ---: | --- |
| `cd engine && bash tests/meilisearch_source_contract_kat.sh --live && bash tests/meilisearch_source_contract_kat.sh --live` | 0 | Two fresh live runs returned `{"result":"PASS"}` with sorted stable IDs `SKU-001`, `SKU-002`, `SKU-003`, POST document fetch parsing, bounded task polling limit `120`, and cleanup fields. |
| `cd engine && jq -e . tests/fixtures/2026_07_26_m0a_meilisearch_source_contract/*.json >/dev/null` | 0 | All fixture JSON parsed. |
| `cd engine && bash -n tests/meilisearch_source_contract_kat.sh tests/meilisearch_source_contract_kat_test.sh` | 0 | Shell syntax passed. |
| `cd engine && shellcheck tests/meilisearch_source_contract_kat.sh tests/meilisearch_source_contract_kat_test.sh` | 0 | ShellCheck passed. |
| `cd engine && bash tests/meilisearch_source_contract_kat_test.sh` | 0 | `PASS denominator=39/39`. |
| `cd engine && bash tests/migration_import_contract_test.sh` | 0 | `Scenario inventory: expected=161 observed=161 pass=161 fail=0 skip=0`; `Results: 161/161 passed (0 skipped)`. |
| Direct residue and secret grep command from the Stage 3 checklist | 0 | Exact KAT container absent from `docker ps -a`; expected temp dirs absent under `/tmp` and the repo; generated key pattern absent from tracked KAT/fixture files. |
| `cd engine && git diff --check` | 0 | No whitespace errors. |

Before the passing live run, a stale
`/tmp/jul26_local_performance_lease` directory named PID `25763`; `kill -0
25763` returned no such process, and only that exact stale lease directory was
removed. No foreign container was stopped, restarted, killed, or reused.

## Contract Matrix

Supported exact rows:

- Index discovery.
- Configured primary key.
- Inferred primary key.
- Single document fetch.
- Settings inventory.
- Display/search/filter/sort/ranking settings.
- Synonyms.
- Vector/semantic settings when no embedder is configured.
- Task success/failure/quiescence.
- Least-privilege action probes.
- Version identity.
- Health.
- Dumps.
- Snapshots.
- Public mutation markers.

Translated with warning:

- Ambiguous primary-key inference:
  `meili_primary_key_ambiguous_candidates`.
- Document enumeration order:
  `meili_document_order_not_contractual`.
- Pagination/export bounds:
  `meili_search_pagination_bound_not_document_export_bound`.
- Normalized setting values:
  `meili_setting_value_normalized`.
- Trailing-slash behavior:
  `meili_trailing_slash_redirect_unknown`.

Unsupported by this M0A gate:

- Experimental chat, prefix search, search cutoff, and localized-field migration
  semantics.
- Large dump/snapshot staging disk sizing.

## Minimum Read Actions

The fixture requires and verifies these scoped actions:

- `indexes.get`
- `documents.get`
- `settings.get`
- `tasks.get`
- `version`
- `stats.get`
- `search`
- `dumps.create`
- `snapshots.create`

For each action, the KAT exercises a positive restricted-key probe and a
negative probe that removes only that action and expects HTTP 403 with
`invalid_api_key`.

## Quiescence And Cleanup

Task polling is bounded by `taskPollLimit=120`. The gate accepts only terminal
task states from `succeeded`, `failed`, and `canceled`, and the final task sample
must contain no enqueued or processing task.

Cleanup proof is two-layered: each live KAT run prints cleanup ownership for the
exact container and temp directory, and the direct residue check proves the
container name is absent from `docker ps -a`, temp directories are absent under
both `/tmp` and the repo, and generated raw key values are not present in tracked
KAT or fixture files.

## Red Mutations

The meta-test denominator is 39/39. It includes the positive control plus these
named mutations or failure modes that must turn the KAT red:

- `missing_port_probe_tool`
- `port_probe_operational_failure`
- `docker_inventory_failure`
- `partial_container_launch_cleanup`
- `indeterminate_container_inspection`
- `image_digest_mismatch`
- `expected_primary_keys_drift`
- `pagination_offsets_drift`
- `pagination_limit_drift`
- `empty_task_captures`
- `wrong_record_value`
- `wrong_record_count`
- `configured_primary_key_drift`
- `inferred_primary_key_drift`
- `ambiguous_primary_key_metadata_drift`
- `ambiguous_primary_key_acceptance`
- `ambiguous_task_response_uid_drift`
- `dropped_stable_id`
- `duplicate_stable_id`
- `changed_settings`
- `changed_synonyms`
- `nonterminal_task_acceptance`
- `dump_task_uid_drift`
- `dump_task_status_drift`
- `dump_task_type_drift`
- `snapshot_task_uid_drift`
- `snapshot_task_status_drift`
- `snapshot_task_type_drift`
- `source_mutation_during_capture`
- `missing_required_read_action`
- `restricted_probe_path_drift`
- `restricted_probe_body_drift`
- `warning_identifier_drift`
- `credential_leakage`
- `truncated_pagination`
- `cleanup_residue`
- `search_limit_as_export`
- `http_status_only_correctness`
