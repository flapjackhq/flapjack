#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
wordpress_dir=$(cd "$script_dir/../.." && pwd)
sdks_dir=$(cd "$wordpress_dir/.." && pwd)
repo_root=$(cd "$sdks_dir/.." && pwd)
engine_dir="$repo_root/engine"

runner_dir=''
data_dir=''
server_pid=''
server_log=''
wp_env_config=''
wp_env_override=''
wp_env_override_config=''
plugin_package=''
package_server_pid=''
wp_env_owned=0
run_succeeded=0

fail() {
  echo "real-stack: $*" >&2
  return 1
}

require_command() {
  command -v "$1" >/dev/null 2>&1 || fail "required command not found: $1"
}

remove_directory() {
  local path=$1
  test -n "$path" || return 0
  perl -MFile::Path=remove_tree -e 'remove_tree($ARGV[0])' "$path"
}

resolve_flapjack_binary() {
  local target_dir=$1
  local profile=$2
  local binary="$target_dir/$profile/flapjack"
  test -x "$binary" || fail "built Flapjack binary is missing: $binary"
  printf '%s\n' "$binary"
}

resolve_cargo_target_dir() {
  local configured_target_dir=${CARGO_TARGET_DIR:-}
  if test -z "$configured_target_dir"; then
    printf '%s\n' "$engine_dir/target"
    return 0
  fi
  case "$configured_target_dir" in
    /*) printf '%s\n' "$configured_target_dir" ;;
    *) printf '%s\n' "$engine_dir/$configured_target_dir" ;;
  esac
}

write_wp_env_user_override() {
  local destination=$1
  cat >"$destination" <<'JS'
const os = require('node:os');
os.userInfo = () => ({
  username: 'wp_env',
  uid: 1000,
  gid: 1000,
  shell: '/bin/sh',
  homedir: '/home/wp_env',
});
JS
}

# TODO: Document prepare_wp_env_config.
prepare_wp_env_config() {
  local source_config=$1
  local destination=$2
  local plugin_path=$3
  local port=$4
  python3 - "$source_config" "$destination" "$plugin_path" "$port" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source_file:
    config = json.load(source_file)

config["plugins"][0] = sys.argv[3]
config["port"] = int(sys.argv[4])
config["testsEnvironment"] = False

with open(sys.argv[2], "w", encoding="utf-8") as destination_file:
    json.dump(config, destination_file, indent=2)
    destination_file.write("\n")
PY
}

# TODO: Document assert_exact_object_ids.
assert_exact_object_ids() {
  local response=$1
  shift
  SEARCH_RESPONSE="$response" python3 - "$@" <<'PY'
import json
import os
import sys

response = json.loads(os.environ["SEARCH_RESPONSE"])
actual_ids = sorted(str(hit.get("objectID")) for hit in response.get("hits", []))
expected_ids = sorted(sys.argv[1:])

assert actual_ids == expected_ids, f"expected objectIDs {expected_ids}, got {actual_ids}"
assert response.get("nbHits") == len(expected_ids), response
PY
}

# TODO: Document assert_search_response.
assert_search_response() {
  local response=$1
  local post_id=$2
  local product_id=$3
  local expected_sku=$4
  local expected_price=$5

  assert_exact_object_ids "$response" "$post_id" "$product_id" || return 1
  SEARCH_RESPONSE="$response" python3 - "$post_id" "$product_id" "$expected_sku" "$expected_price" <<'PY'
import json
import os
import sys

response = json.loads(os.environ["SEARCH_RESPONSE"])
hits = {str(hit.get("objectID")): hit for hit in response.get("hits", [])}
post = hits.get(sys.argv[1])
product = hits.get(sys.argv[2])

assert post is not None, f"missing post objectID {sys.argv[1]}"
assert post.get("post_title") == "Stage One Post", post
assert product is not None, f"missing product objectID {sys.argv[2]}"
assert product.get("post_title") == "Stage One Product", product
assert product.get("post_type") == "product", product
assert product.get("sku") == sys.argv[3], product
assert abs(float(product.get("price")) - float(sys.argv[4])) < 0.001, product
assert product.get("stock_status") == "instock", product
PY
}

# TODO: Document assert_woocommerce_settings.
assert_woocommerce_settings() {
  local response=$1
  SETTINGS_RESPONSE="$response" python3 - <<'PY'
import json
import os

settings = json.loads(os.environ["SETTINGS_RESPONSE"])
expected_searchable = {"sku", "variation_skus"}
expected_facets = {
    "taxonomy_product_cat",
    "filterOnly(price)",
    "filterOnly(on_sale)",
    "filterOnly(in_stock)",
    "filterOnly(product_type)",
    "searchable(attribute_pa_color)",
    "searchable(attribute_pa_size)",
}
expected_ranking = {"desc(total_sales)", "desc(average_rating)"}

assert expected_searchable.issubset(settings.get("searchableAttributes", [])), settings
assert expected_facets.issubset(settings.get("attributesForFaceting", [])), settings
assert expected_ranking.issubset(settings.get("customRanking", [])), settings
PY
}

# TODO: Document assert_latest_failure.
assert_latest_failure() {
  local response=$1
  local expected_post_id=$2
  local forbidden_secret=$3
  STATUS_RESPONSE="$response" FORBIDDEN_SECRET="$forbidden_secret" python3 - "$expected_post_id" <<'PY'
import json
import os
import sys

status = json.loads(os.environ["STATUS_RESPONSE"])
failure = status.get("last_failure")
assert isinstance(failure, dict), status
assert failure.get("operation") == "index_post", failure
assert failure.get("source") == "post_sync", failure
assert failure.get("post_id") == int(sys.argv[1]), failure
assert failure.get("index_name") == "stage_one_wordpress", failure
assert isinstance(failure.get("occurred_at"), int), failure
assert failure.get("message"), failure
assert os.environ["FORBIDDEN_SECRET"] not in json.dumps(failure), failure
PY
}

available_port() {
  python3 - <<'PY'
import socket

with socket.socket() as sock:
    sock.bind(("127.0.0.1", 0))
    print(sock.getsockname()[1])
PY
}

run_wp_env() {
  (
    cd "$wordpress_dir"
    NODE_OPTIONS="--require=$wp_env_override" npx wp-env "$@"
  )
}

wp_env_stack_exists() {
  local install_path containers
  if ! install_path=$(run_wp_env install-path 2>/dev/null | awk '/^\// { path=$0 } END { print path }'); then
    fail 'GAP: could not inspect the current wp-env install path'
    return 2
  fi
  test -n "$install_path" || return 1
  test -f "$install_path/docker-compose.yml" || return 1
  if ! containers=$(docker compose -f "$install_path/docker-compose.yml" ps -a -q 2>/dev/null); then
    fail 'GAP: could not inspect the current wp-env stack'
    return 2
  fi
  test -n "$containers"
}

destroy_owned_wp_env_stack() {
  printf 'y\n' | run_wp_env destroy
}

preserve_failure_evidence() {
  local evidence_dir
  evidence_dir=$(mktemp -d "${TMPDIR:-/tmp}/flapjack_wordpress_real_stack_evidence.XXXXXX")
  if test -f "$server_log"; then
    sed -E 's/fj_admin_[A-Za-z0-9_-]+/[REDACTED_ADMIN_KEY]/g' "$server_log" >"$evidence_dir/flapjack.log"
  fi
  if test "$wp_env_owned" -eq 1; then
    run_wp_env logs --no-watch >"$evidence_dir/wp_env.log" 2>&1 || true
  fi
  echo "real-stack: failure evidence preserved at $evidence_dir" >&2
}

# TODO: Document cleanup.
cleanup() {
  local exit_code=$?
  set +e
  if test "$run_succeeded" -ne 1; then
    preserve_failure_evidence
  fi
  if test -n "$server_pid"; then
    kill "$server_pid" 2>/dev/null
    wait "$server_pid" 2>/dev/null
  fi
  if test -n "$package_server_pid"; then
    kill "$package_server_pid" 2>/dev/null
    wait "$package_server_pid" 2>/dev/null
  fi
  if test "$wp_env_owned" -eq 1; then
    destroy_owned_wp_env_stack >/dev/null 2>&1
  fi
  if test -n "$wp_env_override_config"; then
    perl -e 'unlink $ARGV[0] if -e $ARGV[0]' "$wp_env_override_config"
  fi
  remove_directory "$data_dir"
  remove_directory "$runner_dir"
  trap - EXIT
  exit "$exit_code"
}

ensure_wp_env_dependencies() {
  (
    cd "$wordpress_dir"
    npm install --ignore-scripts --no-audit --no-fund
  )
  test -x "$wordpress_dir/node_modules/.bin/wp-env" || fail '@wordpress/env was not installed from package.json'
}

# TODO: Document start_package_artifact_server.
start_package_artifact_server() {
  local package_server_port=$1
  (
    cd "$runner_dir"
    python3 -m http.server "$package_server_port" --bind 127.0.0.1 >/dev/null 2>&1
  ) &
  package_server_pid=$!

  local _
  for _ in $(seq 1 30); do
    curl -fsS "http://127.0.0.1:$package_server_port/flapjack-search.zip" >/dev/null 2>&1 && return 0
    kill -0 "$package_server_pid" 2>/dev/null || fail 'package artifact server exited before serving the ZIP'
    sleep 1
  done
  curl -fsS "http://127.0.0.1:$package_server_port/flapjack-search.zip" >/dev/null
}

prepare_packaged_plugin() {
  (
    cd "$wordpress_dir"
    npm run package
  )
  local produced_package="$wordpress_dir/dist/flapjack-search.zip"
  local package_server_port
  local runner_package="$runner_dir/flapjack-search.zip"
  test -f "$produced_package" || fail "package artifact is missing: $produced_package"
  cp "$produced_package" "$runner_package"
  package_server_port=$(available_port)
  start_package_artifact_server "$package_server_port"
  plugin_package="http://127.0.0.1:$package_server_port/flapjack-search.zip"
  test ! -e "$runner_dir/flapjack-search" || fail 'source-mapped plugin directory must not exist'
}

# TODO: Document build_and_start_flapjack.
build_and_start_flapjack() {
  local target_dir profile flapjack_binary flapjack_port
  profile=${CARGO_PROFILE:-debug}
  (
    cd "$engine_dir"
    cargo build --package flapjack-server
  )
  target_dir=$(resolve_cargo_target_dir)
  flapjack_binary=$(resolve_flapjack_binary "$target_dir" "$profile")
  flapjack_port=$(available_port)
  data_dir=$(mktemp -d "${TMPDIR:-/tmp}/flapjack_wordpress_real_stack_data.XXXXXX")
  server_log="$runner_dir/flapjack.log"
  FLAPJACK_ENV=development "$flapjack_binary" \
    --data-dir "$data_dir" --bind-addr "0.0.0.0:$flapjack_port" >"$server_log" 2>&1 &
  server_pid=$!

  local _
  for _ in $(seq 1 60); do
    curl -fsS "http://127.0.0.1:$flapjack_port/health/ready" >/dev/null 2>&1 && break
    kill -0 "$server_pid" 2>/dev/null || fail 'Flapjack exited before becoming ready'
    sleep 1
  done
  curl -fsS "http://127.0.0.1:$flapjack_port/health/ready" >/dev/null
  test -s "$data_dir/.admin_key" || fail 'Flapjack did not create its ephemeral admin key'
  FLAPJACK_PORT=$flapjack_port
  FLAPJACK_ADMIN_KEY=$(<"$data_dir/.admin_key")
}

assert_php_container_reachability() {
  docker run --rm --add-host=host.docker.internal:host-gateway php:8.1-cli \
    php -r "exit(file_get_contents('http://host.docker.internal:$FLAPJACK_PORT/health/ready') === false);"
}

# TODO: Document start_owned_wp_env_stack.
start_owned_wp_env_stack() {
  local stack_status=0
  wp_env_stack_exists || stack_status=$?
  if test "$stack_status" -eq 0; then
    fail 'GAP: the current wp-env stack already exists; it was left untouched'
    return 1
  fi
  if test "$stack_status" -ne 1; then
    return "$stack_status"
  fi
  wp_env_override_config="$wordpress_dir/.wp-env.override.json"
  if test -e "$wp_env_override_config"; then
    fail "GAP: wp-env override already exists and was left untouched: $wp_env_override_config"
    return 1
  fi
  local wordpress_port
  wordpress_port=$(available_port)
  wp_env_config="$wordpress_dir/.wp-env.json"
  prepare_wp_env_config "$wp_env_config" "$wp_env_override_config" "$plugin_package" "$wordpress_port"
  wp_env_owned=1
  run_wp_env start
  run_wp_env run cli php -r "exit(file_get_contents('http://host.docker.internal:$FLAPJACK_PORT/health/ready') === false);"
  WORDPRESS_PORT=$wordpress_port
}

wp_cli() {
  run_wp_env run cli wp "$@"
}

wp_cli_secret_option_update() {
  local option_name=$1
  local option_value=$2
  # The positional parameters and command substitution expand inside the container shell.
  # shellcheck disable=SC2016
  run_wp_env run cli sh -eu -c 'secret=$(cat); wp option update "$1" "$secret"' sh "$option_name" <<<"$option_value"
}

configure_wordpress() {
  local woocommerce_plugin
  wp_cli plugin is-active flapjack-search || wp_cli plugin activate flapjack-search
  woocommerce_plugin=$(wp_cli plugin list --field=name --skip-plugins --skip-themes | awk '/^woocommerce/ { name=$0 } END { print name }')
  test -n "$woocommerce_plugin" || fail 'WooCommerce is missing from the wp-env plugin registry'
  wp_cli plugin is-active "$woocommerce_plugin" || wp_cli plugin activate "$woocommerce_plugin"
  wp_cli eval 'exit(post_type_exists("product") ? 0 : 1);'
  wp_cli option update flapjack_host "http://host.docker.internal:$FLAPJACK_PORT"
  wp_cli option update flapjack_app_id test_app
  wp_cli_secret_option_update flapjack_api_key "$FLAPJACK_ADMIN_KEY"
  wp_cli_secret_option_update flapjack_search_api_key "$FLAPJACK_ADMIN_KEY"
  wp_cli option update flapjack_index_name stage_one_wordpress
  wp_cli option update flapjack_post_types '["post","product"]' --format=json
}

extract_created_id() {
  awk '/^[0-9]+$/ { id=$0 } END { if (!id) exit 1; print id }'
}

delete_existing_indexable_content() {
  local existing_ids
  local -a existing_id_list
  existing_ids=$(wp_cli post list --post_type=post,page,product --post_status=any --format=ids)
  if test -n "$existing_ids"; then
    read -r -a existing_id_list <<<"$existing_ids"
    wp_cli post delete "${existing_id_list[@]}" --force
  fi
}

# TODO: Document create_wordpress_fixtures.
create_wordpress_fixtures() {
  local post_output product_output delete_output unpublish_output
  delete_existing_indexable_content
  post_output=$(wp_cli post create --post_type=post --post_status=publish \
    --post_title='Stage One Post' --post_content='Deterministic real-stack post.' --porcelain)
  POST_ID=$(printf '%s\n' "$post_output" | extract_created_id)
  product_output=$(wp_cli post create --post_type=product --post_status=publish \
    --post_title='Stage One Product' --post_content='Deterministic WooCommerce product.' --porcelain)
  PRODUCT_ID=$(printf '%s\n' "$product_output" | extract_created_id)
  wp_cli post meta update "$PRODUCT_ID" _sku STAGE1-SKU
  wp_cli post meta update "$PRODUCT_ID" _regular_price 19.95
  wp_cli post meta update "$PRODUCT_ID" _price 19.95
  wp_cli post meta update "$PRODUCT_ID" _stock_status instock

  delete_output=$(wp_cli post create --post_type=post --post_status=publish \
    --post_title='Lifecycle Delete Candidate' --post_content='Removed through a WordPress delete hook.' --porcelain)
  DELETE_POST_ID=$(printf '%s\n' "$delete_output" | extract_created_id)
  unpublish_output=$(wp_cli post create --post_type=post --post_status=publish \
    --post_title='Lifecycle Unpublish Candidate' --post_content='Removed through a WordPress status transition.' --porcelain)
  UNPUBLISH_POST_ID=$(printf '%s\n' "$unpublish_output" | extract_created_id)
}

# TODO: Document query_flapjack.
query_flapjack() {
  local query=${1:-}
  local request_body
  request_body=$(SEARCH_QUERY="$query" python3 - <<'PY'
import json
import os

print(json.dumps({"query": os.environ["SEARCH_QUERY"], "hitsPerPage": 100}))
PY
)
  curl_flapjack_admin \
    -H 'Content-Type: application/json' \
    --data "$request_body" \
    "http://127.0.0.1:$FLAPJACK_PORT/1/indexes/stage_one_wordpress/query"
}

curl_flapjack_admin() {
  printf 'X-Algolia-API-Key: %s\n' "$FLAPJACK_ADMIN_KEY" | curl -fsS \
    -H 'X-Algolia-Application-Id: test_app' \
    -H @- \
    "$@"
}

# TODO: Document prove_real_wordpress_indexing.
prove_real_wordpress_indexing() {
  wp_cli flapjack test
  wp_cli flapjack reindex

  local direct_response rest_response settings_response all_response status_response failure_output
  direct_response=$(query_flapjack 'Stage One')
  assert_search_response "$direct_response" "$POST_ID" "$PRODUCT_ID" STAGE1-SKU 19.95
  rest_response=$(curl -fsS \
    "http://127.0.0.1:$WORDPRESS_PORT/wp-json/flapjack-search/v1/search?q=Stage%20One&per_page=20")
  assert_search_response "$rest_response" "$POST_ID" "$PRODUCT_ID" STAGE1-SKU 19.95

  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID" "$DELETE_POST_ID" "$UNPUBLISH_POST_ID"

  settings_response=$(curl_flapjack_admin \
    "http://127.0.0.1:$FLAPJACK_PORT/1/indexes/stage_one_wordpress/settings")
  assert_woocommerce_settings "$settings_response"

  # The dollar-prefixed expressions are PHP variables, not shell expansions.
  # shellcheck disable=SC2016
  wp_cli eval '$factory = new \Flapjack\WordPress\ClientFactory(); $factory->get_client()->saveObjects($factory->get_index_name(), [["objectID" => "stale-shell-object", "post_title" => "Stale shell object", "post_type" => "post"]]);'
  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID" "$DELETE_POST_ID" "$UNPUBLISH_POST_ID" stale-shell-object

  wp_cli flapjack reindex
  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID" "$DELETE_POST_ID" "$UNPUBLISH_POST_ID"

  wp_cli post delete "$DELETE_POST_ID" --force
  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID" "$UNPUBLISH_POST_ID"

  wp_cli post update "$UNPUBLISH_POST_ID" --post_status=draft
  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID"

  wp_cli_secret_option_update flapjack_api_key invalid-rebuild-key
  if wp_cli flapjack reindex; then
    fail 'reindex unexpectedly succeeded with an invalid API key'
  fi
  wp_cli_secret_option_update flapjack_api_key "$FLAPJACK_ADMIN_KEY"
  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID"

  wp_cli option update flapjack_host 'http://127.0.0.1:1'
  failure_output=$(wp_cli post create --post_type=post --post_status=publish \
    --post_title='Failure Visibility Candidate' --post_content='This sync must fail durably.' --porcelain)
  FAILURE_POST_ID=$(printf '%s\n' "$failure_output" | extract_created_id)
  wp_cli option update flapjack_host "http://host.docker.internal:$FLAPJACK_PORT"
  # The dollar-prefixed expressions are PHP variables, not shell expansions.
  # shellcheck disable=SC2016
  status_response=$(wp_cli --user=1 eval '$response = rest_do_request("/flapjack-search/v1/status"); echo wp_json_encode($response->get_data());')
  assert_latest_failure "$status_response" "$FAILURE_POST_ID" "$FLAPJACK_ADMIN_KEY"

  all_response=$(query_flapjack)
  assert_exact_object_ids "$all_response" "$POST_ID" "$PRODUCT_ID"

  PACKAGED_INSTALL_CONTRACT_SKIP_REBUILD=1 bash "$wordpress_dir/tests/packaged_install_contract.sh"
}

run_php_realstack_suite() {
  FLAPJACK_TEST_API_KEY="$FLAPJACK_ADMIN_KEY" docker run --rm \
    --add-host=host.docker.internal:host-gateway \
    -v "$wordpress_dir:/plugin:ro" \
    -w /plugin \
    -e "FLAPJACK_TEST_HOST=http://host.docker.internal:$FLAPJACK_PORT" \
    -e FLAPJACK_TEST_APP_ID=test_app \
    -e FLAPJACK_TEST_API_KEY \
    php:8.1-cli vendor/bin/phpunit --testsuite realstack --fail-on-skipped
}

# TODO: Document main.
main() {
  require_command cargo
  require_command curl
  require_command docker
  require_command node
  require_command npm
  require_command npx
  require_command perl
  require_command python3
  require_command zip

  runner_dir=$(mktemp -d "$repo_root/.realstack_runner.XXXXXX")
  wp_env_override="$runner_dir/wp_env_user.cjs"
  write_wp_env_user_override "$wp_env_override"
  trap cleanup EXIT

  ensure_wp_env_dependencies
  build_and_start_flapjack
  assert_php_container_reachability
  prepare_packaged_plugin
  start_owned_wp_env_stack
  configure_wordpress
  create_wordpress_fixtures
  prove_real_wordpress_indexing
  run_php_realstack_suite
  run_succeeded=1
  echo 'real-stack: PASS'
}

if test "${REAL_STACK_TEST_MODE:-0}" != 1; then
  main "$@"
fi
