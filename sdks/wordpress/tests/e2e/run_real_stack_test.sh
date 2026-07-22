#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
export REAL_STACK_TEST_MODE=1

# The dynamic script directory keeps this test cwd-independent.
# shellcheck disable=SC1091
source "$script_dir/run_real_stack.sh"

test_dir=$(mktemp -d)
cleanup_test() {
  if test -n "${package_server_pid:-}"; then
    kill "$package_server_pid" 2>/dev/null || true
    wait "$package_server_pid" 2>/dev/null || true
  fi
  perl -MFile::Path=remove_tree -e 'remove_tree($ARGV[0])' "$test_dir"
}
trap cleanup_test EXIT

mkdir -p "$test_dir/custom_target/release"
touch "$test_dir/custom_target/release/flapjack"
chmod +x "$test_dir/custom_target/release/flapjack"

resolved_binary=$(resolve_flapjack_binary "$test_dir/custom_target" release)
test "$resolved_binary" = "$test_dir/custom_target/release/flapjack"

engine_dir="$test_dir/engine"
mkdir -p "$engine_dir/relative_target/debug"
touch "$engine_dir/relative_target/debug/flapjack"
chmod +x "$engine_dir/relative_target/debug/flapjack"
export CARGO_TARGET_DIR=relative_target
resolved_target_dir=$(resolve_cargo_target_dir)
test "$resolved_target_dir" = "$engine_dir/relative_target"
resolved_binary=$(resolve_flapjack_binary "$resolved_target_dir" debug)
test "$resolved_binary" = "$engine_dir/relative_target/debug/flapjack"
unset CARGO_TARGET_DIR

wp_env_override="$test_dir/wp_env_user.cjs"
write_wp_env_user_override "$wp_env_override"
NODE_OPTIONS="--require=$wp_env_override" node -e '
  const user = require("node:os").userInfo();
  if (user.username !== "wp_env" || user.uid !== 1000 || user.gid !== 1000) {
    process.exit(1);
  }
'

generated_config="$test_dir/.wp-env.realstack.json"
package_path="$test_dir/flapjack-search.zip"
prepare_wp_env_config "$script_dir/../../.wp-env.json" "$generated_config" "$package_path" 9123
python3 - "$generated_config" "$package_path" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as config_file:
    config = json.load(config_file)

assert config["plugins"][0] == sys.argv[2]
assert "." not in config["plugins"]
assert config.get("mappings") == {}
assert config["port"] == 9123
assert config["testsEnvironment"] is False
PY

wordpress_dir="$test_dir/wordpress"
runner_dir="$test_dir/runner"
plugin_package=''
mkdir -p "$wordpress_dir" "$runner_dir"
# Invoked indirectly by prepare_packaged_plugin.
# shellcheck disable=SC2329
npm() {
  test "$*" = 'run package'
  mkdir -p "$wordpress_dir/dist"
  printf 'package fixture\n' >"$wordpress_dir/dist/flapjack-search.zip"
}
prepare_packaged_plugin
unset -f npm
case "$plugin_package" in
  http://127.0.0.1:*/flapjack-search.zip) ;;
  *)
    echo "prepare_packaged_plugin must expose the packaged ZIP as a local ZIP URL, got: $plugin_package" >&2
    exit 1
    ;;
esac
test -f "$runner_dir/flapjack-search.zip"
curl -fsS "$plugin_package" >/dev/null
test ! -e "$runner_dir/flapjack-search"

# Invoked indirectly by wp_cli_secret_option_update.
# shellcheck disable=SC2329
run_wp_env() {
  printf '%s\n' "$*"
  cat >/dev/null
}

secret_command_output=$(wp_cli_secret_option_update flapjack_api_key 'DO-NOT-PRINT')
case "$secret_command_output" in
  *DO-NOT-PRINT*)
    echo 'wp_cli_secret_option_update exposed the secret in command arguments' >&2
    exit 1
    ;;
esac

search_json='{"hits":[{"objectID":"41","post_title":"Stage One Post"},{"objectID":"42","post_title":"Stage One Product","post_type":"product","sku":"STAGE1-SKU","price":19.95,"stock_status":"instock"}],"nbHits":2}'
assert_search_response "$search_json" 41 42 'STAGE1-SKU' '19.95'

search_with_extra_json='{"hits":[{"objectID":"41","post_title":"Stage One Post"},{"objectID":"42","post_title":"Stage One Product","post_type":"product","sku":"STAGE1-SKU","price":19.95,"stock_status":"instock"},{"objectID":"99","post_title":"Unexpected stale post"}],"nbHits":3}'
if assert_search_response "$search_with_extra_json" 41 42 'STAGE1-SKU' '19.95' 2>/dev/null; then
  echo 'assert_search_response accepted an unexpected objectID' >&2
  exit 1
fi

if assert_search_response "$search_json" 41 42 'WRONG-SKU' '19.95' 2>/dev/null; then
  echo 'assert_search_response accepted the wrong SKU' >&2
  exit 1
fi

assert_exact_object_ids "$search_json" 41 42
if assert_exact_object_ids "$search_json" 41 99 2>/dev/null; then
  echo 'assert_exact_object_ids accepted the wrong objectID set' >&2
  exit 1
fi

settings_json='{"searchableAttributes":["post_title","sku","variation_skus","post_content"],"attributesForFaceting":["filterOnly(post_type)","taxonomy_product_cat","filterOnly(price)","filterOnly(on_sale)","filterOnly(in_stock)","filterOnly(product_type)","searchable(attribute_pa_color)","searchable(attribute_pa_size)"],"customRanking":["desc(post_date)","desc(total_sales)","desc(average_rating)"]}'
assert_woocommerce_settings "$settings_json"

failure_json='{"last_failure":{"operation":"index_post","source":"post_sync","post_id":77,"index_name":"stage_one_wordpress","message":"Connection refused","occurred_at":1700000000}}'
assert_latest_failure "$failure_json" 77 'DO-NOT-LEAK'

fixture_cleanup_log="$test_dir/fixture_cleanup.log"
# Invoked indirectly by delete_existing_indexable_content.
# shellcheck disable=SC2329
wp_cli() {
  printf '%s\n' "$*" >>"$fixture_cleanup_log"
  if test "$*" = 'post list --post_type=post,page,product --post_status=any --format=ids'; then
    printf '%s\n' '1 2'
  fi
}
delete_existing_indexable_content
grep -qx 'post list --post_type=post,page,product --post_status=any --format=ids' "$fixture_cleanup_log"
grep -qx 'post delete 1 2 --force' "$fixture_cleanup_log"

: >"$fixture_cleanup_log"
# Invoked indirectly by delete_existing_indexable_content.
# shellcheck disable=SC2329
wp_cli() {
  printf '%s\n' "$*" >>"$fixture_cleanup_log"
}
delete_existing_indexable_content
grep -qx 'post list --post_type=post,page,product --post_status=any --format=ids' "$fixture_cleanup_log"
if grep -q 'post delete' "$fixture_cleanup_log"; then
  echo 'delete_existing_indexable_content deleted without candidate IDs' >&2
  exit 1
fi

mkdir -p "$test_dir/existing_wp_env"
touch "$test_dir/existing_wp_env/docker-compose.yml"
fake_bin="$test_dir/bin"
mkdir -p "$fake_bin"
cat >"$fake_bin/docker" <<'SH'
#!/usr/bin/env sh
if [ "$1" = "compose" ]; then
  printf '%s\n' existing-container
  exit 0
fi
exit 1
SH
chmod +x "$fake_bin/docker"
# Invoked indirectly by start_owned_wp_env_stack.
# shellcheck disable=SC2329
run_wp_env() {
  if test "$1" = install-path; then
    printf '%s\n' "$test_dir/existing_wp_env"
    return 0
  fi
  echo "unexpected wp-env command: $*" >&2
  return 1
}
PATH="$fake_bin:$PATH"
wp_env_owned=0
wp_env_override_config="$test_dir/should-not-exist.json"
if start_owned_wp_env_stack 2>/dev/null; then
  echo 'start_owned_wp_env_stack took ownership of an existing stopped stack' >&2
  exit 1
fi
test "$wp_env_owned" -eq 0
test ! -e "$wp_env_override_config"

cat >"$fake_bin/docker" <<'SH'
#!/usr/bin/env sh
exit 2
SH
chmod +x "$fake_bin/docker"
wp_env_owned=0
wp_env_override_config="$test_dir/compose-inspection-failure.json"
if compose_inspection_error=$(start_owned_wp_env_stack 2>&1); then
  echo 'start_owned_wp_env_stack continued after docker compose inspection failed' >&2
  exit 1
fi
case "$compose_inspection_error" in
  *'could not inspect the current wp-env stack'*) ;;
  *)
    echo "start_owned_wp_env_stack hid the compose inspection failure: $compose_inspection_error" >&2
    exit 1
    ;;
esac
test "$wp_env_owned" -eq 0
test ! -e "$wp_env_override_config"

# Invoked indirectly by start_owned_wp_env_stack.
# shellcheck disable=SC2329
run_wp_env() {
  if test "$1" = install-path; then
    return 2
  fi
  echo "unexpected wp-env command: $*" >&2
  return 1
}
wp_env_owned=0
wp_env_override_config="$test_dir/install-path-inspection-failure.json"
if install_path_error=$(start_owned_wp_env_stack 2>&1); then
  echo 'start_owned_wp_env_stack continued after wp-env install-path failed' >&2
  exit 1
fi
case "$install_path_error" in
  *'could not inspect the current wp-env install path'*) ;;
  *)
    echo "start_owned_wp_env_stack hid the install-path failure: $install_path_error" >&2
    exit 1
    ;;
esac
test "$wp_env_owned" -eq 0
test ! -e "$wp_env_override_config"

admin_key='DO-NOT-EXPOSE-IN-ARGV'
curl_argv_log="$test_dir/curl_argv.log"
curl_stdin_log="$test_dir/curl_stdin.log"
# Invoked indirectly by query_flapjack.
# shellcheck disable=SC2329
curl() {
  printf '%s\n' "$@" >"$curl_argv_log"
  cat >"$curl_stdin_log"
  printf '%s\n' '{"hits":[],"nbHits":0}'
}
# Used by the sourced query_flapjack function.
# shellcheck disable=SC2034
FLAPJACK_ADMIN_KEY=$admin_key
# shellcheck disable=SC2034
FLAPJACK_PORT=9124
query_flapjack >/dev/null
unset -f curl
if grep -Fq "$admin_key" "$curl_argv_log"; then
  echo 'query_flapjack exposed the admin key in curl arguments' >&2
  exit 1
fi
grep -Fqx "X-Algolia-API-Key: $admin_key" "$curl_stdin_log"

python_argv_log="$test_dir/python_argv.log"
python_env_log="$test_dir/python_env.log"
# Invoked indirectly by assert_latest_failure.
# shellcheck disable=SC2329
python3() {
  printf '%s\n' "$@" >"$python_argv_log"
  printf '%s\n' "${FORBIDDEN_SECRET:-}" >"$python_env_log"
  cat >/dev/null
}
assert_latest_failure "$failure_json" 77 "$admin_key"
unset -f python3
if grep -Fq "$admin_key" "$python_argv_log"; then
  echo 'assert_latest_failure exposed the admin key in Python arguments' >&2
  exit 1
fi
grep -Fqx "$admin_key" "$python_env_log"

docker_argv_log="$test_dir/docker_argv.log"
docker_env_log="$test_dir/docker_env.log"
# Invoked indirectly by run_php_realstack_suite.
# shellcheck disable=SC2329
docker() {
  printf '%s\n' "$@" >"$docker_argv_log"
  printf '%s\n' "${FLAPJACK_TEST_API_KEY:-}" >"$docker_env_log"
}
run_php_realstack_suite
unset -f docker
if grep -Fq "$admin_key" "$docker_argv_log"; then
  echo 'run_php_realstack_suite exposed the admin key in Docker arguments' >&2
  exit 1
fi
grep -Fqx "$admin_key" "$docker_env_log"

echo 'run_real_stack shell contract: PASS'
