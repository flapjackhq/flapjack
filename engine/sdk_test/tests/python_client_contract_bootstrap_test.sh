#!/bin/bash

set -euo pipefail

SDK_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENTRYPOINT="$SDK_TEST_DIR/python_client_contract_test.sh"
REQUIREMENTS="$SDK_TEST_DIR/requirements-python-client.txt"
CACHE_PARENT="$SDK_TEST_DIR/.cache"
mkdir -p "$CACHE_PARENT"
SUITE_ROOT="$(mktemp -d "$CACHE_PARENT/python-client-bootstrap-test.XXXXXX")"
FAKE_BIN="$SUITE_ROOT/bin"
FAKE_BASE_INTERPRETER="$FAKE_BIN/fake-python"
FAKE_ENV_INTERPRETER_TEMPLATE="$SUITE_ROOT/fake-environment-python"
LAST_OUTPUT=""
LAST_STATUS=0

cleanup() {
    rm -rf "$SUITE_ROOT"
}
trap cleanup EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_contains() {
    local haystack="$1"
    local needle="$2"
    [[ "$haystack" == *"$needle"* ]] || fail "expected output to contain '$needle'; got: $haystack"
}

assert_file_contains() {
    local path="$1"
    local needle="$2"
    grep -F -- "$needle" "$path" >/dev/null || fail "expected $path to contain '$needle'"
}

assert_failed() {
    local description="$1"
    (( LAST_STATUS != 0 )) || fail "$description unexpectedly succeeded"
}

assert_status() {
    local expected="$1"
    local description="$2"
    [[ "$LAST_STATUS" -eq "$expected" ]] || fail "$description returned $LAST_STATUS, expected $expected; output: $LAST_OUTPUT"
}

new_case_dir() {
    mktemp -d "$SUITE_ROOT/case.XXXXXX"
}

write_fake_environment_interpreter() {
    cat >"$FAKE_ENV_INTERPRETER_TEMPLATE" <<'FAKE_ENVIRONMENT'
#!/bin/bash
set -u

environment_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
printf 'environment|' >>"$FAKE_CALL_LOG"
printf '%q ' "$@" >>"$FAKE_CALL_LOG"
printf '\n' >>"$FAKE_CALL_LOG"

if [[ "${1:-}" == "-c" ]]; then
    [[ ! -f "$environment_dir/incompatible" ]]
    exit
fi

if [[ "${1:-}" == "-m" && "${2:-}" == "pip" ]]; then
    exit "${FAKE_PIP_EXIT:-0}"
fi

exit "${FAKE_EXEC_EXIT:-0}"
FAKE_ENVIRONMENT
    chmod +x "$FAKE_ENV_INTERPRETER_TEMPLATE"
}

write_fake_base_interpreter() {
    cat >"$FAKE_BASE_INTERPRETER" <<'FAKE_BASE'
#!/bin/bash
set -u

printf 'base|' >>"$FAKE_CALL_LOG"
printf '%q ' "$@" >>"$FAKE_CALL_LOG"
printf '\n' >>"$FAKE_CALL_LOG"

if [[ "${1:-}" == "-c" ]]; then
    case "${FAKE_BASE_KIND:-valid}" in
        valid) exit 0 ;;
        non-cpython|wrong-version) exit 1 ;;
        *) exit 64 ;;
    esac
fi

if [[ "${1:-}" == "-m" && "${2:-}" == "venv" ]]; then
    environment_dir="$3"
    mkdir -p "$environment_dir/bin"
    cp "$FAKE_ENV_INTERPRETER_TEMPLATE" "$environment_dir/bin/python"
    chmod +x "$environment_dir/bin/python"
    exit 0
fi

exit 65
FAKE_BASE
    chmod +x "$FAKE_BASE_INTERPRETER"
}

create_cached_environment() {
    local environment_dir="$1"
    local validity="${2:-valid}"
    mkdir -p "$environment_dir/bin"
    cp "$FAKE_ENV_INTERPRETER_TEMPLATE" "$environment_dir/bin/python"
    chmod +x "$environment_dir/bin/python"
    if [[ "$validity" == "incompatible" ]]; then
        touch "$environment_dir/incompatible"
    fi
}

run_entrypoint() {
    local interpreter="$1"
    local cache_root="$2"
    local call_log="$3"
    shift 3

    set +e
    LAST_OUTPUT="$(env \
        PATH="$FAKE_BIN:/usr/bin:/bin" \
        FLAPJACK_SDK_PYTHON="$interpreter" \
        FLAPJACK_SDK_PYTHON_CLIENT_CACHE="$cache_root" \
        FAKE_CALL_LOG="$call_log" \
        FAKE_ENV_INTERPRETER_TEMPLATE="$FAKE_ENV_INTERPRETER_TEMPLATE" \
        "$@" \
        bash "$ENTRYPOINT" 2>&1)"
    LAST_STATUS=$?
    set -e
}

[[ -f "$ENTRYPOINT" ]] || fail "missing contract entrypoint: $ENTRYPOINT"
[[ -f "$REQUIREMENTS" ]] || fail "missing contract requirements: $REQUIREMENTS"

mkdir -p "$FAKE_BIN"
write_fake_environment_interpreter
write_fake_base_interpreter

missing_case="$(new_case_dir)"
run_entrypoint "missing-python" "$missing_case/cache" "$missing_case/calls.log"
assert_failed "missing interpreter case"
assert_contains "$LAST_OUTPUT" "Unable to find CPython 3.12 interpreter"

non_cpython_case="$(new_case_dir)"
run_entrypoint "$FAKE_BASE_INTERPRETER" "$non_cpython_case/cache" "$non_cpython_case/calls.log" FAKE_BASE_KIND=non-cpython
assert_failed "non-CPython case"
assert_contains "$LAST_OUTPUT" "must be CPython 3.12"
[[ ! -e "$non_cpython_case/cache" ]] || fail "non-CPython interpreter created the cache"

wrong_version_case="$(new_case_dir)"
run_entrypoint "$FAKE_BASE_INTERPRETER" "$wrong_version_case/cache" "$wrong_version_case/calls.log" FAKE_BASE_KIND=wrong-version
assert_failed "wrong-version case"
assert_contains "$LAST_OUTPUT" "must be CPython 3.12"
[[ ! -e "$wrong_version_case/cache" ]] || fail "wrong-version interpreter created the cache"

reuse_case="$(new_case_dir)"
reuse_environment="$reuse_case/cache/python-client-contract"
reuse_log="$reuse_case/calls.log"
create_cached_environment "$reuse_environment"
run_entrypoint "$FAKE_BASE_INTERPRETER" "$reuse_case/cache" "$reuse_log"
assert_status 0 "compatible cached environment case"
if grep -F -- '-m venv' "$reuse_log" >/dev/null; then
    fail "compatible cached environment was recreated"
fi
assert_file_contains "$reuse_log" "environment|-m pip install -r $REQUIREMENTS"
assert_file_contains "$reuse_log" "environment|$SDK_TEST_DIR/python_client_contract_test.py"

broken_case="$(new_case_dir)"
broken_environment="$broken_case/cache/python-client-contract"
broken_log="$broken_case/calls.log"
mkdir -p "$broken_environment" "$broken_case/cache/unrelated-environment"
touch "$broken_environment/stale" "$broken_case/cache/unrelated-environment/sentinel"
run_entrypoint "$FAKE_BASE_INTERPRETER" "$broken_case/cache" "$broken_log"
assert_status 0 "broken cached environment case"
assert_file_contains "$broken_log" "base|-m venv $broken_environment"
[[ ! -e "$broken_environment/stale" ]] || fail "broken task environment was not recreated"
[[ -f "$broken_case/cache/unrelated-environment/sentinel" ]] || fail "unrelated cache contents were removed"

incompatible_case="$(new_case_dir)"
incompatible_environment="$incompatible_case/cache/python-client-contract"
incompatible_log="$incompatible_case/calls.log"
create_cached_environment "$incompatible_environment" incompatible
run_entrypoint "$FAKE_BASE_INTERPRETER" "$incompatible_case/cache" "$incompatible_log"
assert_status 0 "incompatible cached environment case"
assert_file_contains "$incompatible_log" "base|-m venv $incompatible_environment"
[[ ! -e "$incompatible_environment/incompatible" ]] || fail "incompatible task environment was not recreated"

install_failure_case="$(new_case_dir)"
install_failure_log="$install_failure_case/calls.log"
run_entrypoint "$FAKE_BASE_INTERPRETER" "$install_failure_case/cache" "$install_failure_log" FAKE_PIP_EXIT=23
assert_status 23 "installation failure case"
if grep -F -- "$SDK_TEST_DIR/python_client_contract_test.py" "$install_failure_log" >/dev/null; then
    fail "contract ran after installation failed"
fi

execution_failure_case="$(new_case_dir)"
execution_failure_log="$execution_failure_case/calls.log"
run_entrypoint "$FAKE_BASE_INTERPRETER" "$execution_failure_case/cache" "$execution_failure_log" FAKE_EXEC_EXIT=37
assert_status 37 "contract execution failure case"
assert_file_contains "$execution_failure_log" "environment|-m pip install -r $REQUIREMENTS"
assert_file_contains "$execution_failure_log" "environment|$SDK_TEST_DIR/python_client_contract_test.py"

echo "Python client contract bootstrap tests passed"
