#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CTL="$SCRIPT_DIR/source_provider_fixture_ctl.sh"
TMP="$(mktemp -d /tmp/fj_source_provider_fixture_ctl_test_XXXXXX)"
FAKE_BIN="$TMP/bin"
DOCKER_LOG="$TMP/docker.log"
ORDER_LOG="$TMP/order.log"
PRIVILEGED_LOG="$TMP/privileged.log"
REJECTED_DOCKER_LOG="$TMP/rejected_docker.log"
FAKE_DOCKER_STATE="$TMP/docker_state"
REAL_DOCKER_BIN="${SOURCE_PROVIDER_CTL_TEST_REAL_DOCKER_BIN:-$(command -v docker || true)}"
RECORDED_FIXTURE_DIRS=()
PROTECTED_FIXTURE_DIRS=()

# Contract under test:
# source_provider_fixture_ctl.sh::stop_provider validates the provider, container
# name, owner token, and fixture directory before any mutation. Typesense
# cleanup repairs its verified /data bind mount before removing the owned
# container, then removes the exact fixture directory and reports success only
# after it is absent.
#
# Typesense creates the Linux-shaped failure:
# lib/source_provider_fixtures.sh::start_typesense creates $TMP/typesense_data
# and bind-mounts it as /data, so the red specimen below includes that subtree.

cleanup() {
  local script_exit_code=$?
  local dir path protected_dir requires_container_cleanup cleanup_failed repair_errors
  trap - EXIT
  cleanup_failed=0
  # bash 3.2 (macOS /bin/bash) treats "${arr[@]}" on an empty array as unbound
  # under set -u, so guard every expansion of the two recording arrays.
  for dir in ${RECORDED_FIXTURE_DIRS[@]+"${RECORDED_FIXTURE_DIRS[@]}"}; do
    if [ -d "$dir" ]; then
      repair_errors=""
      for path in "$dir" "$dir/typesense_data" "$dir/typesense_data/protected"; do
        if [ -e "$path" ] && ! chmod u+rwx "$path" 2>/dev/null; then
          repair_errors="${repair_errors} chmod_path_failed=${path}"
        fi
      done
      if ! chmod -R u+rwx "$dir" 2>/dev/null; then
        repair_errors="${repair_errors} recursive_chmod_failed=${dir}"
      fi
      requires_container_cleanup=0
      for protected_dir in ${PROTECTED_FIXTURE_DIRS[@]+"${PROTECTED_FIXTURE_DIRS[@]}"}; do
        if [ "$dir" = "$protected_dir" ]; then
          requires_container_cleanup=1
          break
        fi
      done
      if [ "$requires_container_cleanup" -eq 1 ] && [ -n "$REAL_DOCKER_BIN" ]; then
        if ! "$REAL_DOCKER_BIN" run --rm --volume "$dir:/fixture" alpine:3 \
          sh -c 'chmod -R u+rwx,go+rwx /fixture 2>/dev/null || true' >/dev/null 2>&1; then
          repair_errors="${repair_errors} docker_permission_repair_failed=${dir}"
        fi
      fi
      if ! rm -rf -- "$dir" 2>/dev/null; then
        repair_errors="${repair_errors} rm_failed=${dir}"
      fi
      if [ -d "$dir" ]; then
        cleanup_failed=1
        printf 'cleanup failed: fixture dir residue remains dir=%s%s\n' "$dir" "$repair_errors" >&2
      fi
    fi
  done
  rm -rf -- "$TMP"
  if [ "$script_exit_code" -ne 0 ]; then
    exit "$script_exit_code"
  fi
  if [ "$cleanup_failed" -ne 0 ]; then
    exit 1
  fi
  exit 0
}
trap cleanup EXIT

mkdir -p "$FAKE_BIN" "$FAKE_DOCKER_STATE"
cat >"$FAKE_BIN/docker" <<'FAKE_DOCKER'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${SOURCE_PROVIDER_CTL_TEST_DOCKER_LOG:?}"

state_dir="${SOURCE_PROVIDER_CTL_TEST_DOCKER_STATE:?}"

# Only the ownership lookup (ps/inspect) and a targeted `rm -f` are permitted.
# Anything else — notably a container-assisted `docker run --volume` cleanup —
# is recorded as rejected so the caller's assertions can fail on it.
reject_unpermitted_docker() {
  printf '%s\n' "$*" >>"${SOURCE_PROVIDER_CTL_TEST_REJECTED_LOG:?}"
  printf 'fake docker: unpermitted invocation: %s\n' "$*" >&2
  exit 97
}

case "${1:-}" in
  ps)
    if [ "${SOURCE_PROVIDER_CTL_TEST_DOCKER_PS_FAIL:-}" = "1" ]; then
      printf 'fake docker: ps failed\n' >&2
      exit 1
    fi
    name_filter=""
    include_stopped=0
    while [ "$#" -gt 0 ]; do
      case "$1" in
        -a)
          include_stopped=1
          shift
          ;;
        --filter)
          name_filter="${2:-}"
          shift 2
          ;;
        *)
          shift
          ;;
      esac
    done
    name="${name_filter#name=^/}"
    name="${name%$}"
    # Real `docker ps` hides exited containers unless -a is passed, so a stopped
    # fixture container must stay invisible to the running-only lookup.
    if [ -n "$name" ] && [ -f "$state_dir/$name.provider" ] \
      && { [ "$include_stopped" -eq 1 ] || [ ! -f "$state_dir/$name.stopped" ]; }; then
      printf '%s\n' "$name"
    fi
    ;;
  inspect)
    format="${3:-}"
    name="${4:-}"
    case "$format" in
      *flapjack.source_provider_fixture.token*)
        cat "$state_dir/$name.token" 2>/dev/null || true
        ;;
      *flapjack.source_provider_fixture.provider*)
        cat "$state_dir/$name.provider" 2>/dev/null || true
        ;;
      *flapjack.source_provider_fixture*)
        [ -f "$state_dir/$name.provider" ] && printf '1\n'
        ;;
    esac
    ;;
  rm)
    [ "${2:-}" = "-f" ] || reject_unpermitted_docker "$@"
    name="${3:-}"
    if [ -n "${SOURCE_PROVIDER_CTL_TEST_ORDER_LOG:-}" ]; then
      printf 'docker rm -f %s\n' "$name" >>"$SOURCE_PROVIDER_CTL_TEST_ORDER_LOG"
    fi
    if [ "${SOURCE_PROVIDER_CTL_TEST_FAIL_RM_FOR:-}" = "$name" ]; then
      exit 1
    fi
    rm -f -- "$state_dir/$name.provider" "$state_dir/$name.token" "$state_dir/$name.stopped"
    ;;
  exec)
    name="${2:-}"
    shift 2
    if [ -n "${SOURCE_PROVIDER_CTL_TEST_ORDER_LOG:-}" ]; then
      printf 'docker exec %s %s\n' "$name" "$*" >>"$SOURCE_PROVIDER_CTL_TEST_ORDER_LOG"
    fi
    [ -f "$state_dir/$name.provider" ] || reject_unpermitted_docker exec "$name" "$@"
    if [ -f "$state_dir/$name.stopped" ]; then
      printf 'fake docker: exec on a stopped container: %s\n' "$name" >&2
      exit 1
    fi
    case "$*" in
      "sh -c chown -R "*" /data && chmod -R u+rwX /data")
        if [ -n "${SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR:-}" ]; then
          if [ -f "$SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR/.requires_container_repair" ] \
            && [ -n "${SOURCE_PROVIDER_CTL_TEST_REAL_DOCKER_BIN:-}" ]; then
            "$SOURCE_PROVIDER_CTL_TEST_REAL_DOCKER_BIN" run --rm \
              --volume "$SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR/typesense_data:/data" \
              alpine:3 "$@"
          else
            chmod -R u+rwX "$SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR/typesense_data" 2>/dev/null || true
          fi
        fi
        ;;
      *)
        reject_unpermitted_docker exec "$name" "$@"
        ;;
    esac
    ;;
  *)
    reject_unpermitted_docker "$@"
    ;;
esac
FAKE_DOCKER
chmod +x "$FAKE_BIN/docker"
cat >"$FAKE_BIN/sudo" <<'FAKE_SUDO'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"${SOURCE_PROVIDER_CTL_TEST_PRIVILEGED_LOG:?}"
printf 'fake sudo: privileged cleanup is not permitted: %s\n' "$*" >&2
exit 97
FAKE_SUDO
chmod +x "$FAKE_BIN/sudo"
cat >"$FAKE_BIN/rm" <<'FAKE_RM'
#!/usr/bin/env bash
set -euo pipefail
if [ -n "${SOURCE_PROVIDER_CTL_TEST_ORDER_LOG:-}" ] \
  && [ -n "${SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR:-}" ]; then
  for arg in "$@"; do
    if [ "$arg" = "$SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR" ]; then
      printf 'rm -rf %s\n' "$arg" >>"$SOURCE_PROVIDER_CTL_TEST_ORDER_LOG"
      if [ "${SOURCE_PROVIDER_CTL_TEST_TARGET_RM_FAIL:-}" = "1" ]; then
        exit 1
      fi
      break
    fi
  done
fi
exec /bin/rm "$@"
FAKE_RM
chmod +x "$FAKE_BIN/rm"

# Single owner of the faked-binary environment; both expectation wrappers below
# report through RUN_CTL_OUTPUT/RUN_CTL_STATUS.
run_ctl() {
  set +e
  RUN_CTL_OUTPUT="$(PATH="$FAKE_BIN:$PATH" \
    SOURCE_PROVIDER_CTL_TEST_DOCKER_LOG="$DOCKER_LOG" \
    SOURCE_PROVIDER_CTL_TEST_DOCKER_STATE="$FAKE_DOCKER_STATE" \
    SOURCE_PROVIDER_CTL_TEST_PRIVILEGED_LOG="$PRIVILEGED_LOG" \
    SOURCE_PROVIDER_CTL_TEST_REAL_DOCKER_BIN="$REAL_DOCKER_BIN" \
    SOURCE_PROVIDER_CTL_TEST_REJECTED_LOG="$REJECTED_DOCKER_LOG" \
    "$@" 2>&1)"
  RUN_CTL_STATUS=$?
  set -e
}

run_expect_failure() {
  run_ctl "$@"
  [ "$RUN_CTL_STATUS" -ne 0 ] || {
    printf 'expected failure, got success: %s\noutput=%s\n' "$*" "$RUN_CTL_OUTPUT" >&2
    exit 1
  }
  printf '%s\n' "$RUN_CTL_OUTPUT"
}

run_expect_indeterminate() {
  run_ctl "$@"
  [ "$RUN_CTL_STATUS" -eq 2 ] || {
    printf 'expected indeterminate exit 2, got %s: %s\noutput=%s\n' \
      "$RUN_CTL_STATUS" "$*" "$RUN_CTL_OUTPUT" >&2
    exit 1
  }
  printf '%s\n' "$RUN_CTL_OUTPUT"
}

reset_fake_docker() {
  : >"$DOCKER_LOG"
  : >"$ORDER_LOG"
  : >"$PRIVILEGED_LOG"
  : >"$REJECTED_DOCKER_LOG"
  rm -rf -- "$FAKE_DOCKER_STATE"
  mkdir -p "$FAKE_DOCKER_STATE"
}

assert_output_contains() {
  local output="$1" expected="$2"
  grep -F "$expected" <<<"$output" >/dev/null || {
    printf 'expected output to contain %s, saw:\n%s\n' "$expected" "$output" >&2
    exit 1
  }
}

assert_docker_not_called() {
  [ ! -s "$DOCKER_LOG" ] || {
    printf 'docker should not have been called, saw:\n' >&2
    cat "$DOCKER_LOG" >&2
    exit 1
  }
}

count_fixture_directories() {
  local pattern="$1" matches
  matches="$(compgen -G "$pattern" || true)"
  if [ -z "$matches" ]; then
    printf '0\n'
  else
    printf '%s\n' "$matches" | wc -l | tr -d ' '
  fi
}

record_fixture_dir() {
  local dir="$1"
  RECORDED_FIXTURE_DIRS+=("$dir")
}

new_typesense_fixture_dir() {
  local dir
  dir="$(mktemp -d /tmp/fj_source_provider_fixture_typesense_ctltest_XXXXXX)"
  assert_owned_typesense_fixture_dir "$dir"
  printf '%s\n' "$dir"
}

assert_owned_typesense_fixture_dir() {
  local dir="$1"
  [[ "$dir" == /tmp/fj_source_provider_fixture_typesense_* ]] || {
    printf 'fixture dir is not lane-owned: %s\n' "$dir" >&2
    exit 1
  }
}

assert_typesense_container_name() {
  local name="$1"
  [[ "$name" =~ ^fj_source_migration_provider_parity_typesense_[0-9]+$ ]] || {
    printf 'container is not lane-owned: %s\n' "$name" >&2
    exit 1
  }
}

register_fake_container() {
  local name="$1" provider="$2" token="$3"
  assert_typesense_container_name "$name"
  printf '%s\n' "$provider" >"$FAKE_DOCKER_STATE/$name.provider"
  printf '%s\n' "$token" >"$FAKE_DOCKER_STATE/$name.token"
}

mark_fake_container_stopped() {
  local name="$1"
  [ -f "$FAKE_DOCKER_STATE/$name.provider" ] || {
    printf 'cannot stop an unregistered fake container: %s\n' "$name" >&2
    exit 1
  }
  : >"$FAKE_DOCKER_STATE/$name.stopped"
}

assert_no_docker_exec_event() {
  grep -q '^exec ' "$DOCKER_LOG" && {
    printf 'cleanup must not exec a container that is not running, saw:\n' >&2
    cat "$DOCKER_LOG" >&2
    exit 1
  }
  return 0
}

assert_docker_rm_attempted() {
  local name="$1"
  grep -Fx "rm -f $name" "$DOCKER_LOG" >/dev/null || {
    printf 'expected docker rm -f %s, saw:\n' "$name" >&2
    cat "$DOCKER_LOG" >&2
    exit 1
  }
}

assert_docker_rm_not_attempted() {
  local name="$1"
  ! grep -Fx "rm -f $name" "$DOCKER_LOG" >/dev/null || {
    printf 'did not expect docker rm -f %s, saw:\n' "$name" >&2
    cat "$DOCKER_LOG" >&2
    exit 1
  }
}

assert_no_privileged_cleanup() {
  [ ! -s "$PRIVILEGED_LOG" ] || {
    printf 'cleanup must never escalate privileges, saw:\n' >&2
    cat "$PRIVILEGED_LOG" >&2
    exit 1
  }
}

assert_only_ownership_typesense_repair_and_targeted_rm_docker_events() {
  local container="$1" line
  assert_no_privileged_cleanup
  [ ! -s "$REJECTED_DOCKER_LOG" ] || {
    printf 'cleanup issued an unpermitted docker invocation, saw:\n' >&2
    cat "$REJECTED_DOCKER_LOG" >&2
    exit 1
  }
  while IFS= read -r line; do
    case "$line" in
      "ps "*"name=^/${container}"*) ;;
      "inspect --format "*" ${container}") ;;
      "exec ${container} sh -c chown -R "*" /data && chmod -R u+rwX /data") ;;
      "rm -f ${container}") ;;
      *)
        printf 'unpermitted docker event for %s: %s\n' "$container" "$line" >&2
        cat "$DOCKER_LOG" >&2
        exit 1
        ;;
    esac
  done <"$DOCKER_LOG"
}

assert_no_docker_event_after_targeted_rm() {
  local container="$1"
  awk -v rm_event="rm -f ${container}" '
    $0 == rm_event { seen_rm = 1; next }
    seen_rm == 1 { exit 1 }
  ' "$DOCKER_LOG" || {
    printf 'unexpected docker event after failed rm for %s, saw:\n' "$container" >&2
    cat "$DOCKER_LOG" >&2
    exit 1
  }
}

assert_container_removed_before_fixture_rm() {
  local name="$1" dir="$2" docker_line rm_line
  docker_line="$(grep -nFx "docker rm -f $name" "$ORDER_LOG" | cut -d: -f1 | head -n 1 || true)"
  rm_line="$(grep -nFx "rm -rf $dir" "$ORDER_LOG" | cut -d: -f1 | head -n 1 || true)"
  [ -n "$docker_line" ] || {
    printf 'expected ordered docker rm -f %s event, saw:\n' "$name" >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
  [ -n "$rm_line" ] || {
    printf 'expected ordered rm -rf %s event, saw:\n' "$dir" >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
  [ "$docker_line" -lt "$rm_line" ] || {
    printf 'expected docker rm before fixture rm, saw:\n' >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
}

assert_typesense_repair_before_container_rm_before_fixture_rm() {
  local name="$1" dir="$2" repair_line docker_line rm_line
  repair_line="$(grep -nF "docker exec $name sh -c chown -R " "$ORDER_LOG" | cut -d: -f1 | head -n 1 || true)"
  docker_line="$(grep -nFx "docker rm -f $name" "$ORDER_LOG" | cut -d: -f1 | head -n 1 || true)"
  rm_line="$(grep -nFx "rm -rf $dir" "$ORDER_LOG" | cut -d: -f1 | head -n 1 || true)"
  [ -n "$repair_line" ] || {
    printf 'expected ordered docker exec permission repair for %s, saw:\n' "$name" >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
  [ -n "$docker_line" ] || {
    printf 'expected ordered docker rm -f %s event, saw:\n' "$name" >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
  [ -n "$rm_line" ] || {
    printf 'expected ordered rm -rf %s event, saw:\n' "$dir" >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
  [ "$repair_line" -lt "$docker_line" ] && [ "$docker_line" -lt "$rm_line" ] || {
    printf 'expected repair before docker rm before fixture rm, saw:\n' >&2
    cat "$ORDER_LOG" >&2
    exit 1
  }
}

assert_no_container_residue() {
  local name="$1"
  [ ! -e "$FAKE_DOCKER_STATE/$name.provider" ] && [ ! -e "$FAKE_DOCKER_STATE/$name.token" ] || {
    printf 'expected fake container residue to be absent for %s\n' "$name" >&2
    ls "$FAKE_DOCKER_STATE" >&2
    exit 1
  }
}

assert_container_residue() {
  local name="$1"
  [ -e "$FAKE_DOCKER_STATE/$name.provider" ] && [ -e "$FAKE_DOCKER_STATE/$name.token" ] || {
    printf 'expected fake container residue to remain for %s\n' "$name" >&2
    ls "$FAKE_DOCKER_STATE" >&2
    exit 1
  }
}

assert_fixture_dir_absent() {
  local dir="$1"
  [ ! -d "$dir" ] || {
    printf 'expected fixture dir to be absent: %s\n' "$dir" >&2
    exit 1
  }
}

assert_success_receipt() {
  local output="$1" provider="$2" container="$3"
  jq -e --arg provider "$provider" --arg container "$container" \
    '. == {provider:$provider,container:$container,removed:true}' \
    <<<"$output" >/dev/null || {
      printf 'unexpected cleanup success receipt, saw:\n%s\n' "$output" >&2
      exit 1
    }
}

assert_fixture_dir_exists() {
  local dir="$1"
  [ -d "$dir" ] || {
    printf 'expected fixture dir to remain: %s\n' "$dir" >&2
    exit 1
  }
}

assert_path_exists() {
  local path="$1"
  [ -e "$path" ] || {
    printf 'expected fixture path to remain: %s\n' "$path" >&2
    exit 1
  }
}

assert_protected_marker_exists() {
  local fixture_dir="$1" marker output status
  marker="$fixture_dir/typesense_data/protected/marker"
  if [ -e "$marker" ]; then
    return 0
  fi
  if [ -f "$fixture_dir/.requires_container_repair" ] && [ -n "$REAL_DOCKER_BIN" ]; then
    set +e
    output="$("$REAL_DOCKER_BIN" run --rm --volume "$fixture_dir/typesense_data:/data" alpine:3 \
      sh -c 'test -f /data/protected/marker' 2>&1)"
    status=$?
    set -e
    [ "$status" -eq 0 ] || {
      printf 'expected protected marker to remain in fixture dir %s: %s\n' "$fixture_dir" "$output" >&2
      exit 1
    }
    return 0
  fi
  printf 'expected fixture path to remain: %s\n' "$marker" >&2
  exit 1
}

assert_non_owned_typesense_fixture_dir() {
  local dir="$1"
  [[ "$dir" == "$TMP"/* ]] && [[ "$dir" != /tmp/fj_source_provider_fixture_typesense_* ]] || {
    printf 'fixture dir is not a test-owned negative control: %s\n' "$dir" >&2
    exit 1
  }
}

create_protected_typesense_data() {
  local fixture_dir="$1" setup_output setup_status verify_output verify_status
  assert_owned_typesense_fixture_dir "$fixture_dir"
  PROTECTED_FIXTURE_DIRS+=("$fixture_dir")
  mkdir -p "$fixture_dir/typesense_data"
  if [ -n "$REAL_DOCKER_BIN" ]; then
    set +e
    setup_output="$("$REAL_DOCKER_BIN" run --rm --volume "$fixture_dir/typesense_data:/data" alpine:3 \
      sh -c 'mkdir -p /data/protected && touch /data/protected/marker && chmod 700 /data/protected' 2>&1)"
    setup_status=$?
    set -e
    if [ "$setup_status" -ne 0 ]; then
      printf 'indeterminate: real docker protected-subtree setup failed; daemon unavailable, alpine:3 unavailable/pull failed, or container command failed: %s\n' "$setup_output" >&2
      exit 2
    fi
    if [ "${SOURCE_PROVIDER_CTL_TEST_FORCE_RM_PROBE_FAILURE:-}" = "1" ] \
      || ! rm -rf -- "$fixture_dir" 2>"$TMP/protected_rm_probe.err"; then
      set +e
      verify_output="$("$REAL_DOCKER_BIN" run --rm --volume "$fixture_dir/typesense_data:/data" alpine:3 \
        sh -c 'test -f /data/protected/marker' 2>&1)"
      verify_status=$?
      set -e
      if [ "$verify_status" -ne 0 ]; then
        printf 'indeterminate: real docker protected-subtree marker verification failed: %s\n' "$verify_output" >&2
        exit 2
      fi
      : >"$fixture_dir/.requires_container_repair"
      return
    fi
    mkdir -p "$fixture_dir/typesense_data"
  fi
  mkdir -p "$fixture_dir/typesense_data/protected"
  touch "$fixture_dir/typesense_data/protected/marker"
  chmod 500 "$fixture_dir/typesense_data/protected"
  chmod 500 "$fixture_dir/typesense_data"
  if rm -rf -- "$fixture_dir" 2>"$TMP/protected_rm_probe.err"; then
    printf 'indeterminate: host cannot create a protected typesense_data subtree\n' >&2
    exit 2
  fi
}

run_real_docker_setup_failure_case() {
  local fixture fake_docker
  fake_docker="$TMP/real_docker_setup_failure"
  cat >"$fake_docker" <<'FAKE_REAL_DOCKER'
#!/usr/bin/env bash
printf 'Cannot connect to the Docker daemon\n' >&2
exit 1
FAKE_REAL_DOCKER
  chmod +x "$fake_docker"
  REAL_DOCKER_BIN="$fake_docker"
  fixture="$(new_typesense_fixture_dir)"
  record_fixture_dir "$fixture"
  create_protected_typesense_data "$fixture"
}

run_real_docker_marker_verification_failure_case() {
  local fixture fake_docker
  fake_docker="$TMP/real_docker_marker_failure"
  cat >"$fake_docker" <<'FAKE_REAL_DOCKER'
#!/usr/bin/env bash
case "$*" in
  *'test -f /data/protected/marker'*)
    printf 'marker missing\n' >&2
    exit 1
    ;;
  *)
    exit 0
    ;;
esac
FAKE_REAL_DOCKER
  chmod +x "$fake_docker"
  REAL_DOCKER_BIN="$fake_docker"
  fixture="$(new_typesense_fixture_dir)"
  record_fixture_dir "$fixture"
  SOURCE_PROVIDER_CTL_TEST_FORCE_RM_PROBE_FAILURE=1 create_protected_typesense_data "$fixture"
}

run_real_docker_precondition_negative_controls() {
  local output status

  set +e
  output="$(SOURCE_PROVIDER_CTL_TEST_CASE=real_docker_setup_failure bash "$0" 2>&1)"
  status=$?
  set -e
  [ "$status" -eq 2 ] || {
    printf 'expected real Docker setup failure to exit 2, got %s\noutput=%s\n' "$status" "$output" >&2
    exit 1
  }
  assert_output_contains "$output" 'indeterminate: real docker protected-subtree setup failed'
  assert_output_contains "$output" 'daemon unavailable'

  set +e
  output="$(SOURCE_PROVIDER_CTL_TEST_CASE=real_docker_marker_failure bash "$0" 2>&1)"
  status=$?
  set -e
  [ "$status" -eq 2 ] || {
    printf 'expected real Docker marker verification failure to exit 2, got %s\noutput=%s\n' "$status" "$output" >&2
    exit 1
  }
  assert_output_contains "$output" 'indeterminate: real docker protected-subtree marker verification failed'
}

run_cleanup_residue_case() {
  local fixture="${SOURCE_PROVIDER_CTL_TEST_RESIDUE_DIR:?}"
  assert_owned_typesense_fixture_dir "$fixture"
  record_fixture_dir "$fixture"
  mkdir -p "$fixture/typesense_data"
  cat >"$FAKE_BIN/rm" <<'FAKE_RM'
#!/usr/bin/env bash
set -euo pipefail
if [ "${SOURCE_PROVIDER_CTL_TEST_RESIDUE_RM_FAIL:-}" = "1" ]; then
  for arg in "$@"; do
    if [ "$arg" = "${SOURCE_PROVIDER_CTL_TEST_RESIDUE_DIR:-}" ]; then
      exit 1
    fi
  done
fi
exec /bin/rm "$@"
FAKE_RM
  chmod +x "$FAKE_BIN/rm"
  export SOURCE_PROVIDER_CTL_TEST_RESIDUE_RM_FAIL=1
  export PATH="$FAKE_BIN:$PATH"
  exit 0
}

run_cleanup_preserves_failure_case() {
  exit 7
}

run_cleanup_exit_status_controls() {
  local residue_dir output status

  residue_dir="$(new_typesense_fixture_dir)"
  record_fixture_dir "$residue_dir"
  set +e
  output="$(SOURCE_PROVIDER_CTL_TEST_CASE=cleanup_residue \
    SOURCE_PROVIDER_CTL_TEST_RESIDUE_DIR="$residue_dir" \
    bash "$0" 2>&1)"
  status=$?
  set -e
  [ "$status" -ne 0 ] || {
    printf 'expected cleanup residue subprocess to fail, got success\noutput=%s\n' "$output" >&2
    exit 1
  }
  assert_output_contains "$output" "cleanup failed: fixture dir residue remains dir=$residue_dir"
  assert_fixture_dir_exists "$residue_dir"
  rm -rf -- "$residue_dir"
  [ ! -d "$residue_dir" ] || {
    printf 'failed to remove cleanup residue control dir: %s\n' "$residue_dir" >&2
    exit 1
  }

  set +e
  output="$(SOURCE_PROVIDER_CTL_TEST_CASE=cleanup_preserves_failure bash "$0" 2>&1)"
  status=$?
  set -e
  [ "$status" -eq 7 ] || {
    printf 'expected cleanup to preserve incoming status 7, got %s\noutput=%s\n' "$status" "$output" >&2
    exit 1
  }
}

run_non_owned_fixture_dir_negative_control() {
  local container token fixture output marker
  reset_fake_docker
  container="fj_source_migration_provider_parity_typesense_10004"
  token="typesense_fixture_test_token_non_owned"
  fixture="$TMP/non_owned_typesense_fixture"
  marker="$fixture/typesense_data/marker"
  assert_non_owned_typesense_fixture_dir "$fixture"
  mkdir -p "$fixture/typesense_data"
  touch "$marker"
  record_fixture_dir "$fixture"
  register_fake_container "$container" typesense "$token"
  run_ctl env \
    SOURCE_PROVIDER_CONTAINER="$container" \
    SOURCE_PROVIDER_FIXTURE_DIR="$fixture" \
    SOURCE_PROVIDER_OWNER_TOKEN="$token" \
    bash "$CTL" down typesense
  output="$RUN_CTL_OUTPUT"
  # Isolated Stage 1 red: stop_provider still accepts a non-owned fixture dir,
  # removes the owned container, and reports removed:true. Report the status
  # first so the pinned class is named instead of a downstream assertion.
  [ "$RUN_CTL_STATUS" -eq 2 ] || {
    printf 'source_provider_fixture_ctl_test=RED case=non_owned_fixture_dir class=non_owned_fixture_dir_accepted status=%s\noutput=%s\n' \
      "$RUN_CTL_STATUS" "$output" >&2
    exit 1
  }
  assert_output_contains "$output" 'SOURCE_PROVIDER_FIXTURE_DIR is not an owned typesense fixture directory'
  assert_docker_not_called
  assert_no_privileged_cleanup
  assert_fixture_dir_exists "$fixture"
  assert_path_exists "$marker"
  printf 'source_provider_fixture_ctl_test=PASS case=non_owned_fixture_dir\n'
}

if [ "${SOURCE_PROVIDER_CTL_TEST_CASE:-}" = "non_owned_fixture_dir" ]; then
  run_non_owned_fixture_dir_negative_control
  exit 0
fi

if [ "${SOURCE_PROVIDER_CTL_TEST_CASE:-}" = "cleanup_residue" ]; then
  run_cleanup_residue_case
fi

if [ "${SOURCE_PROVIDER_CTL_TEST_CASE:-}" = "cleanup_preserves_failure" ]; then
  run_cleanup_preserves_failure_case
fi

if [ "${SOURCE_PROVIDER_CTL_TEST_CASE:-}" = "real_docker_setup_failure" ]; then
  run_real_docker_setup_failure_case
  exit 0
fi

if [ "${SOURCE_PROVIDER_CTL_TEST_CASE:-}" = "real_docker_marker_failure" ]; then
  run_real_docker_marker_verification_failure_case
  exit 0
fi

run_real_docker_precondition_negative_controls
run_cleanup_exit_status_controls

reset_fake_docker
output="$(run_expect_indeterminate env SOURCE_PROVIDER_CONTAINER=unrelated SOURCE_PROVIDER_FIXTURE_DIR=/tmp/fj_source_provider_fixture_meilisearch_abc bash "$CTL" down meilisearch)"
assert_output_contains "$output" 'SOURCE_PROVIDER_CONTAINER is not an owned meilisearch fixture container'
assert_docker_not_called
assert_no_privileged_cleanup

unsupported_provider="unknown_$$"
unsupported_fixture_pattern="/tmp/fj_source_provider_fixture_${unsupported_provider}_*"
before_count="$(count_fixture_directories "$unsupported_fixture_pattern")"
[ "$before_count" = 0 ] || {
  printf 'unsupported-provider proof requires a clean baseline: count=%s\n' "$before_count" >&2
  exit 1
}
run_expect_indeterminate bash "$CTL" up "$unsupported_provider" | grep -F 'usage:' >/dev/null
after_count="$(count_fixture_directories "$unsupported_fixture_pattern")"
[ "$before_count" = "$after_count" ] || {
  printf 'unsupported provider leaked a temp fixture directory: before=%s after=%s\n' "$before_count" "$after_count" >&2
  exit 1
}

reset_fake_docker
output="$(run_expect_indeterminate bash "$CTL" down "$unsupported_provider")"
assert_output_contains "$output" 'usage:'
assert_docker_not_called
assert_no_privileged_cleanup

reset_fake_docker
missing_token_container="fj_source_migration_provider_parity_typesense_10001"
missing_token_fixture="$(new_typesense_fixture_dir)"
record_fixture_dir "$missing_token_fixture"
output="$(run_expect_indeterminate env \
  SOURCE_PROVIDER_CONTAINER="$missing_token_container" \
  SOURCE_PROVIDER_FIXTURE_DIR="$missing_token_fixture" \
  bash "$CTL" down typesense)"
assert_output_contains "$output" 'SOURCE_PROVIDER_OWNER_TOKEN is required for down'
assert_docker_not_called
assert_no_privileged_cleanup
assert_fixture_dir_exists "$missing_token_fixture"

reset_fake_docker
ps_fail_container="fj_source_migration_provider_parity_typesense_10006"
ps_fail_token="typesense_fixture_test_token_ps_fail"
ps_fail_fixture="$(new_typesense_fixture_dir)"
record_fixture_dir "$ps_fail_fixture"
mkdir -p "$ps_fail_fixture/typesense_data"
touch "$ps_fail_fixture/typesense_data/marker"
register_fake_container "$ps_fail_container" typesense "$ps_fail_token"
output="$(SOURCE_PROVIDER_CTL_TEST_DOCKER_PS_FAIL=1 run_expect_indeterminate env \
  SOURCE_PROVIDER_CONTAINER="$ps_fail_container" \
  SOURCE_PROVIDER_FIXTURE_DIR="$ps_fail_fixture" \
  SOURCE_PROVIDER_OWNER_TOKEN="$ps_fail_token" \
  bash "$CTL" down typesense)"
assert_output_contains "$output" "docker_ps_failed name=$ps_fail_container"
assert_docker_rm_not_attempted "$ps_fail_container"
assert_container_residue "$ps_fail_container"
assert_fixture_dir_exists "$ps_fail_fixture"
assert_path_exists "$ps_fail_fixture/typesense_data/marker"
assert_no_privileged_cleanup

reset_fake_docker
rm_fail_container="fj_source_migration_provider_parity_typesense_10002"
rm_fail_token="typesense_fixture_test_token_rm_fail"
rm_fail_fixture="$(new_typesense_fixture_dir)"
record_fixture_dir "$rm_fail_fixture"
mkdir -p "$rm_fail_fixture/typesense_data"
touch "$rm_fail_fixture/typesense_data/marker"
register_fake_container "$rm_fail_container" typesense "$rm_fail_token"
output="$(SOURCE_PROVIDER_CTL_TEST_FAIL_RM_FOR="$rm_fail_container" run_expect_indeterminate env \
  SOURCE_PROVIDER_CONTAINER="$rm_fail_container" \
  SOURCE_PROVIDER_FIXTURE_DIR="$rm_fail_fixture" \
  SOURCE_PROVIDER_OWNER_TOKEN="$rm_fail_token" \
  bash "$CTL" down typesense)"
assert_output_contains "$output" "typesense_container_rm_failed name=$rm_fail_container"
assert_docker_rm_attempted "$rm_fail_container"
assert_only_ownership_typesense_repair_and_targeted_rm_docker_events "$rm_fail_container"
assert_no_docker_event_after_targeted_rm "$rm_fail_container"
assert_fixture_dir_exists "$rm_fail_fixture"
assert_path_exists "$rm_fail_fixture/typesense_data/marker"

# Keep the focused selector above while exercising this guard in the default run.
run_non_owned_fixture_dir_negative_control

reset_fake_docker
protected_container="fj_source_migration_provider_parity_typesense_10003"
protected_token="typesense_fixture_test_token_protected"
protected_fixture="$(new_typesense_fixture_dir)"
record_fixture_dir "$protected_fixture"
create_protected_typesense_data "$protected_fixture"
register_fake_container "$protected_container" typesense "$protected_token"
run_ctl env \
  SOURCE_PROVIDER_CTL_TEST_ORDER_LOG="$ORDER_LOG" \
  SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR="$protected_fixture" \
  SOURCE_PROVIDER_CONTAINER="$protected_container" \
  SOURCE_PROVIDER_FIXTURE_DIR="$protected_fixture" \
  SOURCE_PROVIDER_OWNER_TOKEN="$protected_token" \
  bash "$CTL" down typesense
[ "$RUN_CTL_STATUS" -eq 0 ] || {
  printf 'expected protected Typesense cleanup to succeed, got %s\noutput=%s\n' "$RUN_CTL_STATUS" "$RUN_CTL_OUTPUT" >&2
  exit 1
}
assert_success_receipt "$RUN_CTL_OUTPUT" typesense "$protected_container"
assert_docker_rm_attempted "$protected_container"
assert_only_ownership_typesense_repair_and_targeted_rm_docker_events "$protected_container"
assert_typesense_repair_before_container_rm_before_fixture_rm "$protected_container" "$protected_fixture"
assert_no_container_residue "$protected_container"
assert_fixture_dir_absent "$protected_fixture"

reset_fake_docker
stopped_protected_container="fj_source_migration_provider_parity_typesense_10007"
stopped_protected_token="typesense_fixture_test_token_stopped_protected"
stopped_protected_fixture="$(new_typesense_fixture_dir)"
record_fixture_dir "$stopped_protected_fixture"
create_protected_typesense_data "$stopped_protected_fixture"
register_fake_container "$stopped_protected_container" typesense "$stopped_protected_token"
mark_fake_container_stopped "$stopped_protected_container"
output="$(SOURCE_PROVIDER_CTL_TEST_TARGET_RM_FAIL=1 run_expect_indeterminate env \
  SOURCE_PROVIDER_CTL_TEST_ORDER_LOG="$ORDER_LOG" \
  SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR="$stopped_protected_fixture" \
  SOURCE_PROVIDER_CONTAINER="$stopped_protected_container" \
  SOURCE_PROVIDER_FIXTURE_DIR="$stopped_protected_fixture" \
  SOURCE_PROVIDER_OWNER_TOKEN="$stopped_protected_token" \
  bash "$CTL" down typesense)"
assert_output_contains "$output" "typesense_stopped_container_data_unremovable name=$stopped_protected_container"
assert_no_docker_exec_event
assert_docker_rm_not_attempted "$stopped_protected_container"
assert_container_residue "$stopped_protected_container"
assert_fixture_dir_exists "$stopped_protected_fixture"
assert_protected_marker_exists "$stopped_protected_fixture"
assert_no_privileged_cleanup

# A crashed fixture container still exists to `docker ps -a`, but `docker exec`
# cannot reach it. Cleanup must skip the permission repair instead of turning a
# removable fixture directory into an indeterminate teardown.
reset_fake_docker
stopped_container="fj_source_migration_provider_parity_typesense_10005"
stopped_token="typesense_fixture_test_token_stopped"
stopped_fixture="$(new_typesense_fixture_dir)"
record_fixture_dir "$stopped_fixture"
mkdir -p "$stopped_fixture/typesense_data"
touch "$stopped_fixture/typesense_data/marker"
register_fake_container "$stopped_container" typesense "$stopped_token"
mark_fake_container_stopped "$stopped_container"
run_ctl env \
  SOURCE_PROVIDER_CTL_TEST_ORDER_LOG="$ORDER_LOG" \
  SOURCE_PROVIDER_CTL_TEST_TARGET_RM_DIR="$stopped_fixture" \
  SOURCE_PROVIDER_CONTAINER="$stopped_container" \
  SOURCE_PROVIDER_FIXTURE_DIR="$stopped_fixture" \
  SOURCE_PROVIDER_OWNER_TOKEN="$stopped_token" \
  bash "$CTL" down typesense
[ "$RUN_CTL_STATUS" -eq 0 ] || {
  printf 'expected stopped-container Typesense cleanup to succeed, got %s\noutput=%s\n' \
    "$RUN_CTL_STATUS" "$RUN_CTL_OUTPUT" >&2
  exit 1
}
assert_success_receipt "$RUN_CTL_OUTPUT" typesense "$stopped_container"
assert_no_docker_exec_event
assert_docker_rm_attempted "$stopped_container"
assert_only_ownership_typesense_repair_and_targeted_rm_docker_events "$stopped_container"
assert_container_removed_before_fixture_rm "$stopped_container" "$stopped_fixture"
assert_no_container_residue "$stopped_container"
assert_fixture_dir_absent "$stopped_fixture"

printf 'source_provider_fixture_ctl_test=PASS staging_run=31176417863 class=typesense_data_rm_failed\n'
