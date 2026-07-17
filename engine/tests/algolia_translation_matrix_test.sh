#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TMP_DIR="$(mktemp -d)"

TRANSLATION_RS="flapjack-http/src/handlers/migration/translation.rs"
TRANSLATION_TESTS_RS="flapjack-http/src/handlers/migration/translation_tests.rs"
TRANSLATION_REPORT_RS="flapjack-http/src/handlers/migration/translation_report.rs"
SOURCE_SNAPSHOT_RS="flapjack-http/src/handlers/migration/source_snapshot.rs"

MUTATED_FILES=(
  "$TRANSLATION_RS"
  "$TRANSLATION_TESTS_RS"
  "$TRANSLATION_REPORT_RS"
  "$SOURCE_SNAPSHOT_RS"
)

CASE_NAMES=(
  denominator_row_removed
  exact_settings_row_rejected
  transformed_row_weakened
  warning_code_dropped
  topology_rejection_weakened
  closed_object_fields_loosened
  synonym_matcher_type_changed
  rule_schema_promote_multiple_weakened
  invalid_object_id_code_changed
  duplicate_object_id_code_changed
  malformed_document_mapping_changed
  typed_failure_suppressed
  scope_gap_rank_changed
)

CASE_TESTS=(
  matrix_denominator_is_explicit_stage3_oracle
  exact_document_and_settings_rows_persist_payload_values
  transformed_settings_distinct_and_numeric_attributes_to_index_persist
  warned_allow_compression_setting_persists_and_reports_warning
  hard_rejected_settings_emit_canonical_codes_and_paths
  closed_unknown_fields_reject_settings_rules_and_synonyms
  supported_synonym_payloads_resolve_to_schema_rows
  every_rule_schema_matcher_has_an_owner_path_case
  invalid_object_id_report_preserves_resource_coordinates
  duplicate_object_id_report_is_scoped_per_resource
  malformed_payload_reports_cover_settings_document_rule_and_synonym_paths
  typed_failures_are_aggregated_without_duplicate_invalid_id_entries
  scope_gap_entries_have_deterministic_order
)

cleanup() {
  restore_all
  rm -rf "$TMP_DIR"
}

restore_all() {
  for file in "${MUTATED_FILES[@]}"; do
    if [[ -f "$TMP_DIR/$file" ]]; then
      mkdir -p "$ROOT_DIR/$(dirname "$file")"
      cp "$TMP_DIR/$file" "$ROOT_DIR/$file"
    fi
  done
}

hash_file() {
  shasum -a 256 "$ROOT_DIR/$1" | awk '{print $1}'
}

assert_hashes_match_start() {
  local file
  for file in "${MUTATED_FILES[@]}"; do
    local current_hash
    current_hash="$(hash_file "$file")"
    local start_hash
    start_hash="$(cat "$TMP_DIR/$file.sha256")"
    if [[ "$current_hash" != "$start_hash" ]]; then
      echo "MUTATION_LEAK: $file hash changed between cases" >&2
      exit 1
    fi
  done
}

replace_once() {
  local file="$1"
  local needle="$2"
  local replacement="$3"
  python3 - "$ROOT_DIR/$file" "$needle" "$replacement" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
needle = sys.argv[2]
replacement = sys.argv[3]
text = path.read_text(encoding="utf-8")
count = text.count(needle)
if count != 1:
    raise SystemExit(f"expected one match in {path}, found {count}: {needle!r}")
path.write_text(text.replace(needle, replacement, 1), encoding="utf-8")
PY
}

snapshot_files() {
  local file
  for file in "${MUTATED_FILES[@]}"; do
    mkdir -p "$TMP_DIR/$(dirname "$file")"
    cp "$ROOT_DIR/$file" "$TMP_DIR/$file"
    hash_file "$file" > "$TMP_DIR/$file.sha256"
  done
}

structural_self_check() {
  if [[ "${#CASE_NAMES[@]}" -eq 0 ]]; then
    echo "VACUOUS: no mutation cases" >&2
    exit 1
  fi
  if [[ "${#CASE_NAMES[@]}" -ne "${#CASE_TESTS[@]}" ]]; then
    echo "case/test mapping length mismatch" >&2
    exit 1
  fi

  local sorted_count unique_count test_name
  sorted_count="$(printf '%s\n' "${CASE_NAMES[@]}" | sort | wc -l | tr -d ' ')"
  unique_count="$(printf '%s\n' "${CASE_NAMES[@]}" | sort -u | wc -l | tr -d ' ')"
  if [[ "$sorted_count" != "$unique_count" ]]; then
    echo "duplicate mutation case names" >&2
    exit 1
  fi

  for test_name in "${CASE_TESTS[@]}"; do
    if [[ "$test_name" != handlers::migration::translation::* ]]; then
      echo "mutation target is outside handlers::migration::translation: $test_name" >&2
      exit 1
    fi
    if ! grep -q "fn ${test_name##*::}(" "$ROOT_DIR/$TRANSLATION_TESTS_RS"; then
      echo "mutation target test not found in translation_tests.rs: $test_name" >&2
      exit 1
    fi
  done
}

derive_denominator() {
  local output denominator
  output="$(
    cd "$ROOT_DIR"
    cargo test -p flapjack-http handlers::migration::translation::tests::matrix_denominator_is_explicit_stage3_oracle -- --nocapture 2>&1
  )"
  denominator="$(printf '%s\n' "$output" | sed -n 's/.*DENOMINATOR=\([0-9][0-9]*\).*/\1/p' | tail -1)"
  if [[ -z "$denominator" || "$denominator" == "0" ]]; then
    echo "VACUOUS: matrix denominator was zero or missing" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
  echo "DENOMINATOR=$denominator"
}

mutate_case() {
  case "$1" in
    denominator_row_removed)
      replace_once "$TRANSLATION_RS" '    exact_settings("attributesForFaceting"),' ''
      ;;
    exact_settings_row_rejected)
      replace_once "$TRANSLATION_RS" '    exact_settings("attributesForFaceting"),' '    rejected_topology_settings("attributesForFaceting"),'
      ;;
    transformed_row_weakened)
      replace_once "$TRANSLATION_RS" '    transformed_settings("numericAttributesToIndex"),' '    exact_settings("numericAttributesToIndex"),'
      ;;
    warning_code_dropped)
      replace_once "$TRANSLATION_RS" '            warning_code: Some(WarningCode::PersistedNoBehaviorSetting),' '            warning_code: None,'
      ;;
    topology_rejection_weakened)
      replace_once "$TRANSLATION_RS" '    rejected_topology_settings("replicas"),' '    exact_settings("replicas"),'
      ;;
    closed_object_fields_loosened)
      replace_once "$TRANSLATION_RS" '                .all(|key| self.allowed.contains(&key.as_str()))' '                .all(|_key| true)'
      ;;
    synonym_matcher_type_changed)
      replace_once "$TRANSLATION_RS" '                "onewaysynonym",' '                "oneway",'
      ;;
    rule_schema_promote_multiple_weakened)
      replace_once "$TRANSLATION_RS" '                closed_fields(&["objectIDs", "position"], &["objectIDs", "position"]),' '                closed_fields(&["objectID", "position"], &["objectID", "position"]),'
      ;;
    invalid_object_id_code_changed)
      replace_once "$TRANSLATION_REPORT_RS" '        SourceSnapshotSchemaViolationKind::InvalidObjectId => ReportCode::InvalidObjectId,' '        SourceSnapshotSchemaViolationKind::InvalidObjectId => ReportCode::DuplicateObjectId,'
      ;;
    duplicate_object_id_code_changed)
      replace_once "$TRANSLATION_REPORT_RS" '        SourceSnapshotSchemaViolationKind::DuplicateObjectId => ReportCode::DuplicateObjectId,' '        SourceSnapshotSchemaViolationKind::DuplicateObjectId => ReportCode::InvalidObjectId,'
      ;;
    malformed_document_mapping_changed)
      replace_once "$TRANSLATION_REPORT_RS" '        SourceSnapshotResource::Document => ReportCode::MalformedDocumentPayload,' '        SourceSnapshotResource::Document => ReportCode::InvalidObjectId,'
      ;;
    typed_failure_suppressed)
      replace_once "$TRANSLATION_RS" '        push_unique_entry(entries, typed_failure_entry(failure));' '        let _ = failure;'
      ;;
    scope_gap_rank_changed)
      replace_once "$TRANSLATION_REPORT_RS" '        ReportResource::Analytics => 0,' '        ReportResource::Analytics => 9,'
      ;;
    *)
      echo "unknown mutation case: $1" >&2
      exit 1
      ;;
  esac
}

run_mutation_case() {
  local case_name="$1"
  local test_name="$2"
  local output status

  restore_all
  assert_hashes_match_start
  mutate_case "$case_name"

  set +e
  output="$(
    cd "$ROOT_DIR"
    cargo test -p flapjack-http "$test_name" 2>&1
  )"
  status=$?
  set -e

  if [[ "$status" -eq 0 ]]; then
    echo "ORACLE_NOT_LOAD_BEARING: $case_name left $test_name green" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
  if printf '%s\n' "$output" | grep -q "could not compile"; then
    echo "ORACLE_COMPILE_FAILURE: $case_name failed by compilation" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi
  if ! printf '%s\n' "$output" | grep -q "test ${test_name} .* FAILED"; then
    echo "ORACLE_WRONG_FAILURE: $case_name did not fail expected test $test_name" >&2
    printf '%s\n' "$output" >&2
    exit 1
  fi

  restore_all
  assert_hashes_match_start
  echo "PASS $case_name -> $test_name"
}

main() {
  cd "$ROOT_DIR"
  snapshot_files
  trap cleanup EXIT

  for i in "${!CASE_TESTS[@]}"; do
    CASE_TESTS[$i]="handlers::migration::translation::tests::${CASE_TESTS[$i]}"
  done

  structural_self_check
  derive_denominator

  for i in "${!CASE_NAMES[@]}"; do
    run_mutation_case "${CASE_NAMES[$i]}" "${CASE_TESTS[$i]}"
  done

  restore_all
  assert_hashes_match_start
}

main "$@"
