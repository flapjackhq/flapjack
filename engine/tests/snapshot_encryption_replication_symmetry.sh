#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ENGINE_DIR="$(cd -- "${SCRIPT_DIR}/.." && pwd)"
EVIDENCE_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/fj_snapshot_symmetry.XXXXXX")"
passed_cases=0
executed_cases=0
preserve_evidence=1

cleanup() {
    local exit_code=$?
    if [[ "${preserve_evidence}" -eq 0 && "${exit_code}" -eq 0 ]]; then
        rm -rf -- "${EVIDENCE_ROOT}"
    else
        printf 'Evidence preserved at %s\n' "${EVIDENCE_ROOT}" >&2
    fi
}
trap cleanup EXIT

indeterminate() {
    printf 'INDETERMINATE %s expected=%s actual=%s\n' "$1" "$2" "$3" >&2
    exit 1
}

run_cargo() {
    env -u FLAPJACK_SNAPSHOT_KEY_FILE cargo "$@"
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || indeterminate "missing tool" "$1" "absent"
}

require_tool cargo
require_tool grep

[[ -d "${EVIDENCE_ROOT}" && -w "${EVIDENCE_ROOT}" ]] \
    || indeterminate "unusable fixture root" "writable directory" "${EVIDENCE_ROOT}"

cd "${ENGINE_DIR}"

compile_log="${EVIDENCE_ROOT}/compile.log"
if ! run_cargo test -p flapjack --test test_snapshot_encryption_contract --no-run \
    >"${compile_log}" 2>&1; then
    indeterminate "compile failure" "cargo test --no-run exit 0" "nonzero; log=${compile_log}"
fi

test_binary="$(grep -Eo 'target/[^ ]*/deps/test_snapshot_encryption_contract-[^ )]+' "${compile_log}" | tail -n 1 || true)"
[[ -n "${test_binary}" ]] \
    || indeterminate "test binary listing" "test_snapshot_encryption_contract binary" "absent; log=${compile_log}"

declare -a tests=(
    "encrypted_magic_does_not_collide_with_gzip"
    "encrypted_export_round_trips_with_the_key"
    "encrypted_export_is_not_readable_without_the_key"
    "plaintext_import_succeeds_when_key_is_supplied"
)
declare -a cases=(
    "default_off_plaintext"
    "same_key_both_ends"
    "key_only_on_producer"
    "plaintext_into_key_configured_consumer"
)
declare -a expected=(
    "plaintext export/import succeeds without FLAPJACK_SNAPSHOT_KEY_FILE"
    "encrypted export/import succeeds with identical key"
    "encrypted import without key fails and leaves destination empty"
    "plaintext import succeeds when consumer has a key"
)

list_log="${EVIDENCE_ROOT}/list.log"
"${test_binary}" --list >"${list_log}" 2>&1 \
    || indeterminate "test listing" "test binary --list exit 0" "nonzero; log=${list_log}"

for test_name in "${tests[@]}"; do
    count="$(grep -Ec "^${test_name}: test$" "${list_log}" || true)"
    [[ "${count}" == "1" ]] \
        || indeterminate "case cardinality ${test_name}" "1" "${count}; log=${list_log}"
done

for i in "${!tests[@]}"; do
    test_name="${tests[$i]}"
    case_name="${cases[$i]}"
    log_path="${EVIDENCE_ROOT}/${case_name}.log"
    executed_cases=$((executed_cases + 1))
    if run_cargo test -p flapjack --test test_snapshot_encryption_contract -- "${test_name}" --exact \
        >"${log_path}" 2>&1; then
        printf '[PASS] %s\n' "${case_name}"
        passed_cases=$((passed_cases + 1))
    else
        printf '[FAIL] %s expected=%s actual=nonzero exit; log=%s\n' \
            "${case_name}" "${expected[$i]}" "${log_path}"
    fi
done

[[ "${executed_cases}" -eq 4 ]] \
    || indeterminate "executed case count" "4" "${executed_cases}"

if [[ "${passed_cases}" -ne 4 ]]; then
    exit 1
fi

preserve_evidence=0
