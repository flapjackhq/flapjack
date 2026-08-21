#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/../.." && pwd)"
VALIDATOR="$SCRIPT_DIR/validate_sync_surface.sh"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-sync-surface-test.XXXXXX")"

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

fail() {
  printf 'FAIL: %s\n' "$1" >&2
  exit 1
}

assert_status() {
  local expected="$1"
  local actual="$2"
  local context="$3"
  [ "$actual" -eq "$expected" ] || fail "$context: expected exit $expected, got $actual"
}

assert_contains() {
  local output_file="$1"
  local expected="$2"
  local context="$3"
  grep -Fq -- "$expected" "$output_file" || {
    printf '%s\n' "--- $context output ---" >&2
    sed -n '1,180p' "$output_file" >&2
    fail "$context: missing expected text: $expected"
  }
}

assert_not_contains() {
  local output_file="$1"
  local unexpected="$2"
  local context="$3"
  if grep -Fq -- "$unexpected" "$output_file"; then
    printf '%s\n' "--- $context output ---" >&2
    sed -n '1,180p' "$output_file" >&2
    fail "$context: found unexpected text: $unexpected"
  fi
}

assert_pbv1_profile_doc_is_publishable() {
  local doc_path="engine/docs2/3_IMPLEMENTATION/PAID_BETA_V1_API_PROFILE.md"
  [ -f "$REPO_DIR/$doc_path" ] || fail "PBV1 profile doc is missing: $doc_path"
  if git -C "$REPO_DIR" check-ignore --no-index -q "$doc_path"; then
    fail "PBV1 profile doc is ignored and would be omitted from a fresh public mirror: $doc_path"
  fi
}

run_validator() {
  local repo="$1"
  local output_file="$2"
  local validator="${3:-$VALIDATOR}"
  local status=0
  SYNC_SURFACE_REPO_DIR="$repo" bash "$validator" > "$output_file" 2>&1 || status=$?
  printf '%s' "$status"
}

write_parser_failure_validator() {
  local validator="$1"
  ln -s "$SCRIPT_DIR/doc_sync_helpers.sh" "$(dirname "$validator")/doc_sync_helpers.sh"
  awk '
    /^configured_private_beads_paths\(\) \{/ {
      print
      print "  return 7"
      replacing = 1
      replacements++
      next
    }
    replacing && /^}/ {
      print
      replacing = 0
      next
    }
    !replacing { print }
    END {
      if (replacing || replacements != 1) {
        exit 1
      }
    }
  ' "$VALIDATOR" > "$validator"
}

write_clean_fixture() {
  local repo="$1"
  mkdir -p \
    "$repo/.github/workflows" \
    "$repo/.beads" \
    "$repo/engine/_dev/s/manual-tests" \
    "$repo/engine/docs2" \
    "$repo/engine/flapjack-http/src" \
    "$repo/engine/loadtest" \
    "$repo/engine/sdk_test" \
    "$repo/integrations/laravel-scout/src" \
    "$repo/integrations/laravel-scout/tests"

  cat > "$repo/.debbie.toml" <<'EOF'
[sync]
# Private Beads paths mentioned in comments are not sync entries: ".beads/README.md".
files = [
  "PROJECT_OVERVIEW.md",
  "ROADMAP.md",
  "README.md",
  "engine/README.md",
  "engine/LIB.md",
  "engine/docs2/FEATURES.md",
  "engine/loadtest/BENCHMARKS.md",
  "engine/docs2/operations_consumer_contract.md",
  "engine/rust-toolchain.toml",
]

[[sync.remap]]
from = "engine/_dev/s/test"
to = "engine/s/test"

[[sync.remap]]
from = "engine/_dev/s/lib/ui.sh"
to = "engine/s/lib/ui.sh"

[[sync.remap]]
from = "engine/_dev/s/lib/local-instance.sh"
to = "engine/s/lib/local-instance.sh"

[[sync.remap]]
from = "engine/_dev/s/manual-tests/cli_smoke.sh"
to = "engine/s/manual-tests/cli_smoke.sh"
EOF

  local path
  for path in \
    PROJECT_OVERVIEW.md \
    ROADMAP.md \
    README.md \
    CHANGELOG.md \
    .gitignore \
    .github/workflows/README.md \
    .beads/config.yaml \
    .beads/metadata.json \
    engine/README.md \
    engine/LIB.md \
    engine/Cargo.toml \
    engine/docs2/FEATURES.md \
    engine/docs2/operations_consumer_contract.md \
    engine/flapjack-http/src/openapi.rs \
    engine/loadtest/BENCHMARKS.md \
    engine/rust-toolchain.toml \
    engine/sdk_test/README.md
  do
    printf 'Fixture content for %s.\n' "$path" > "$repo/$path"
  done
}

assert_clean_fixture() {
  local repo="$1"
  local context="$2"
  local output="$repo/${context}.log"
  local status
  status="$(run_validator "$repo" "$output")"
  assert_status 0 "$status" "$context"
  assert_contains "$output" 'All checked link targets are within .debbie sync surface' "$context"
  assert_not_contains "$output" 'debbie sync prod --dry-run' "$context"
}

restore_and_assert_clean() {
  local pristine="$1"
  local repo="$2"
  local mutated_file="$3"
  local context="$4"

  cp "$pristine/$mutated_file" "$repo/$mutated_file"
  cmp -s "$pristine/$mutated_file" "$repo/$mutated_file" || fail "$context: restore mismatch for $mutated_file"
  assert_clean_fixture "$repo" "${context}_restored"
}

remove_line() {
  local file="$1"
  local needle="$2"
  grep -Fv -- "$needle" "$file" > "$file.tmp"
  mv "$file.tmp" "$file"
}

duplicate_sync_file() {
  local config="$1"
  local entry="$2"
  awk -v entry="$entry" '
    {
      print
      if ($0 ~ "\"" entry "\"") {
        print "  \"" entry "\","
      }
    }
  ' "$config" > "$config.tmp"
  mv "$config.tmp" "$config"
}

add_sync_dir() {
  local config="$1"
  local dir="$2"
  cat >> "$config" <<EOF

[[sync.dirs]]
path = "$dir"
EOF
}

add_sync_remap() {
  local config="$1"
  local from="$2"
  local to="$3"
  cat >> "$config" <<EOF

[[sync.remap]]
from = "$from"
to = "$to"
EOF
}

add_explicit_sync_file() {
  local config="$1"
  local path="$2"
  awk -v path="$path" '
    {
      if ($0 ~ /^]/) {
        print "  \"" path "\","
      }
      print
    }
  ' "$config" > "$config.tmp"
  mv "$config.tmp" "$config"
}

add_raw_sync_file() {
  local config="$1"
  local toml_string="$2"
  TOML_STRING="$toml_string" awk '
    {
      if ($0 ~ /^]/) {
        print "  " ENVIRON["TOML_STRING"] ","
      }
      print
    }
  ' "$config" > "$config.tmp"
  mv "$config.tmp" "$config"
}

add_multiline_sync_file() {
  local config="$1"
  local delimiter="$2"
  local path="$3"
  awk -v delimiter="$delimiter" -v path="$path" '
    {
      if ($0 ~ /^]/) {
        print "  " delimiter
        print path
        print delimiter ","
      }
      print
    }
  ' "$config" > "$config.tmp"
  mv "$config.tmp" "$config"
}

assert_red_arm() {
  local repo="$1"
  local context="$2"
  local expected="$3"
  local output="$repo/${context}.log"
  local status
  status="$(run_validator "$repo" "$output")"
  assert_status 1 "$status" "$context"
  assert_contains "$output" "$expected" "$context"
}

assert_parser_failure_is_red() {
  local repo="$1"
  local validator="$WORK_DIR/parser_failure_validator.sh"
  local output="$repo/parser_failure.log"
  local status

  write_parser_failure_validator "$validator"
  status="$(run_validator "$repo" "$output" "$validator")"
  assert_status 1 "$status" parser_failure
  assert_contains "$output" 'could not parse .debbie.toml for private .beads/ sync paths' parser_failure
}

main() {
  local repo="$WORK_DIR/fixture"
  local pristine="$WORK_DIR/pristine"
  mkdir -p "$repo"
  write_clean_fixture "$repo"
  cp -R "$repo" "$pristine"

  assert_pbv1_profile_doc_is_publishable
  assert_clean_fixture "$repo" clean_fixture
  assert_parser_failure_is_red "$repo"

  remove_line "$repo/.debbie.toml" '"PROJECT_OVERVIEW.md"'
  assert_red_arm "$repo" project_overview_dropped 'exactly once (found 0)'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml project_overview_dropped

  duplicate_sync_file "$repo/.debbie.toml" "PROJECT_OVERVIEW.md"
  assert_red_arm "$repo" project_overview_duplicated 'found 2'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml project_overview_duplicated

  remove_line "$repo/.debbie.toml" '"ROADMAP.md"'
  assert_red_arm "$repo" roadmap_dropped 'ROADMAP.md exactly once (found 0)'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml roadmap_dropped

  duplicate_sync_file "$repo/.debbie.toml" "ROADMAP.md"
  assert_red_arm "$repo" roadmap_duplicated 'ROADMAP.md exactly once (found 2)'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml roadmap_duplicated

  add_explicit_sync_file "$repo/.debbie.toml" "PRIORITIES.md"
  assert_red_arm "$repo" priorities_added_to_sync_files 'must not contain retired PRIORITIES.md'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml priorities_added_to_sync_files

  add_explicit_sync_file "$repo/.debbie.toml" ".beads/README.md"
  assert_red_arm "$repo" beads_file_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml beads_file_added_to_sync_surface

  add_raw_sync_file "$repo/.debbie.toml" "'.beads/README.md'"
  assert_red_arm "$repo" literal_beads_file_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml literal_beads_file_added_to_sync_surface

  add_raw_sync_file "$repo/.debbie.toml" '"\u002e\u0062\u0065\u0061\u0064\u0073/README.md"'
  assert_red_arm "$repo" escaped_beads_file_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml escaped_beads_file_added_to_sync_surface

  add_explicit_sync_file "$repo/.debbie.toml" ".BEADS/README.md"
  assert_red_arm "$repo" case_variant_beads_file_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml case_variant_beads_file_added_to_sync_surface

  add_multiline_sync_file "$repo/.debbie.toml" '"""' ".beads/README.md"
  assert_red_arm "$repo" multiline_beads_file_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml multiline_beads_file_added_to_sync_surface

  add_multiline_sync_file "$repo/.debbie.toml" "'''" ".beads/README.md"
  assert_red_arm "$repo" multiline_literal_beads_file_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml multiline_literal_beads_file_added_to_sync_surface

  add_sync_dir "$repo/.debbie.toml" ".beads/"
  assert_red_arm "$repo" beads_dir_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml beads_dir_added_to_sync_surface

  add_sync_remap "$repo/.debbie.toml" ".beads/private" "private"
  assert_red_arm "$repo" beads_remap_added_to_sync_surface '.beads/ must stay out of the public sync surface'
  restore_and_assert_clean "$pristine" "$repo" .debbie.toml beads_remap_added_to_sync_surface
}

main "$@"
