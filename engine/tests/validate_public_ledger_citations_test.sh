#!/usr/bin/env bash

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
VALIDATOR="$SCRIPT_DIR/validate_public_ledger_citations.sh"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/flapjack-public-citations-test.XXXXXX")"

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
    sed -n '1,160p' "$output_file" >&2
    fail "$context: missing expected text: $expected"
  }
}

assert_not_contains() {
  local output_file="$1"
  local unexpected="$2"
  local context="$3"
  if grep -Fq -- "$unexpected" "$output_file"; then
    printf '%s\n' "--- $context output ---" >&2
    sed -n '1,160p' "$output_file" >&2
    fail "$context: found unexpected text: $unexpected"
  fi
}

new_fixture_repo() {
  local name="$1"
  local repo="$WORK_DIR/$name"
  mkdir -p "$repo/engine/docs2/4_EVIDENCE" "$repo/engine/docs2/evidence/history" "$repo/docs" "$repo/chats" "$repo/chatting" "$repo/implemented"
  printf '%s' "$repo"
}

write_sync_files() {
  local repo="$1"
  shift

  {
    printf '[sync]\nfiles = [\n'
    local path
    for path in "$@"; do
      printf '    "%s",\n' "$path"
    done
    printf ']\n'
  } > "$repo/.debbie.toml"
}

write_sync_remap_and_exclude_fixture_config() {
  local repo="$1"

  cat > "$repo/.debbie.toml" <<'EOF'
[sync]
files = [
  "ROADMAP.md", # inline comment must not hide this file
]

[[sync.dirs]]
path = "public"
exclude = [
  "/anchored",
  "unanchored",
  "trailing/",
  "nested/exact",
]

[[sync.remap]]
from = "private/remap_source.md"
to = "public/remapped.md"
EOF
}

run_validator() {
  local repo="$1"
  local output_file="$2"
  local status=0
  PUBLIC_LEDGER_REPO_DIR="$repo" bash "$VALIDATOR" > "$output_file" 2>&1 || status=$?
  printf '%s' "$status"
}

run_validator_args() {
  local repo="$1"
  local output_file="$2"
  shift 2
  local status=0
  PUBLIC_LEDGER_REPO_DIR="$repo" bash "$VALIDATOR" "$@" > "$output_file" 2>&1 || status=$?
  printf '%s' "$status"
}

init_mirror_fixture_repo() {
  local repo="$1"
  git -C "$repo" init -q
  git -C "$repo" config user.email "public-ledger-test@example.invalid"
  git -C "$repo" config user.name "Public Ledger Test"
}

track_mirror_paths() {
  local repo="$1"
  shift
  git -C "$repo" add -- "$@"
}

test_worktree_existence_does_not_imply_public_resolvability() {
  local repo
  repo="$(new_fixture_repo omitted_from_sync)"
  local receipt="engine/docs2/4_EVIDENCE/live_receipt.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf 'Proof: [%s](%s)\n' "$receipt" "$receipt" > "$repo/ROADMAP.md"
  write_sync_files "$repo" ROADMAP.md

  local output="$repo/unsynced.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'worktree-only citation'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=0 references_unresolvable=1 status=FAIL' 'worktree-only citation'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$receipt" 'worktree-only citation reason'

  write_sync_files "$repo" ROADMAP.md "$receipt"
  output="$repo/synced.log"
  status="$(run_validator "$repo" "$output")"

  assert_status 0 "$status" 'allowlisted citation'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'allowlisted citation'
}

test_sync_surface_remap_and_exclude_semantics_match_debbie() {
  local repo
  repo="$(new_fixture_repo remap_excludes)"
  mkdir -p \
    "$repo/private" \
    "$repo/public/anchored" \
    "$repo/public/nested/anchored" \
    "$repo/public/section/unanchored" \
    "$repo/public/trailing" \
    "$repo/public/nested/exact" \
    "$repo/public/nested/other" \
    "$repo/public/other/nested/exact"

  local remap_dest="public/remapped.md"
  local remap_source="private/remap_source.md"
  local visible="public/visible.md"
  local anchored_excluded="public/anchored/root.md"
  local anchored_nested_visible="public/nested/anchored/root.md"
  local unanchored_excluded="public/section/unanchored/file.md"
  local unanchored_suffix_visible="public/section/unanchored.md"
  local trailing_excluded="public/trailing/file.md"
  local trailing_suffix_visible="public/trailing.md"
  local slash_excluded="public/nested/exact/file.md"
  local slash_nested_visible="public/other/nested/exact/file.md"

  for path in \
    "$remap_dest" \
    "$remap_source" \
    "$visible" \
    "$anchored_excluded" \
    "$anchored_nested_visible" \
    "$unanchored_excluded" \
    "$unanchored_suffix_visible" \
    "$trailing_excluded" \
    "$trailing_suffix_visible" \
    "$slash_excluded" \
    "$slash_nested_visible"; do
    printf 'body for %s\n' "$path" > "$repo/$path"
  done

  printf '%s\n' \
    "Remap destination: [$remap_dest]($remap_dest)." \
    "Remap source: [$remap_source]($remap_source)." \
    "Visible dir file: [$visible]($visible)." \
    "Anchored exclude: [$anchored_excluded]($anchored_excluded)." \
    "Anchored nested control: [$anchored_nested_visible]($anchored_nested_visible)." \
    "Unanchored exclude: [$unanchored_excluded]($unanchored_excluded)." \
    "Unanchored suffix control: [$unanchored_suffix_visible]($unanchored_suffix_visible)." \
    "Trailing slash exclude: [$trailing_excluded]($trailing_excluded)." \
    "Trailing suffix control: [$trailing_suffix_visible]($trailing_suffix_visible)." \
    "Slash pattern exclude: [$slash_excluded]($slash_excluded)." \
    "Slash pattern nested control: [$slash_nested_visible]($slash_nested_visible)." \
    > "$repo/ROADMAP.md"
  write_sync_remap_and_exclude_fixture_config "$repo"

  local output="$repo/remap_excludes.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'remap and exclude semantics'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=11 references_resolvable=6 references_unresolvable=5 status=FAIL' 'remap and exclude denominator'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$remap_dest" 'remap destination owned'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$remap_source" 'remap source private'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$visible" 'ordinary dir path owned'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$anchored_excluded" 'anchored exclude applies at sync root'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$anchored_nested_visible" 'anchored exclude does not match nested component'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$unanchored_excluded" 'unanchored exclude matches component'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$unanchored_suffix_visible" 'unanchored exclude does not match suffix'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$trailing_excluded" 'trailing slash exclude matches directory component'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$trailing_suffix_visible" 'trailing slash exclude does not match suffix'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$slash_excluded" 'slash exclude matches exact subtree'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$slash_nested_visible" 'slash exclude does not float across arbitrary parents'
}

test_vacuous_ledger_is_distinct_from_invalid_sync_configuration() {
  local repo
  repo="$(new_fixture_repo vacuous)"
  local receipt="engine/docs2/4_EVIDENCE/real_receipt.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf 'No repository citations here.\n' > "$repo/PROJECT_OVERVIEW.md"
  printf '[Receipt](%s)\n' "$receipt" > "$repo/ROADMAP.md"
  write_sync_files "$repo" PROJECT_OVERVIEW.md ROADMAP.md "$receipt"

  local output="$repo/vacuous.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 0 "$status" 'vacuous synced ledger'
  assert_contains "$output" 'ledger=PROJECT_OVERVIEW.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'vacuous synced ledger'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'non-vacuous sibling ledger'

  write_sync_files "$repo"
  output="$repo/empty_surface.log"
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'empty parsed sync surface'
  assert_contains "$output" 'parsed sync surface is empty' 'empty parsed sync surface'
  assert_not_contains "$output" 'status=VACUOUS' 'empty parsed sync surface'

  rm "$repo/.debbie.toml"
  output="$repo/missing_config.log"
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'missing sync config'
  assert_contains "$output" 'Missing .debbie.toml' 'missing sync config'
  assert_not_contains "$output" 'status=VACUOUS' 'missing sync config'
}

test_prose_paths_anchor_and_private_surfaces_are_checked() {
  local repo
  repo="$(new_fixture_repo anchoring)"
  local root_receipt="engine/docs2/4_EVIDENCE/root_receipt.md"
  local legacy_receipt="engine/docs2/evidence/history/summary.md"
  local feature_receipt="engine/docs2/4_EVIDENCE/feature_receipt.md"

  printf 'root receipt\n' > "$repo/$root_receipt"
  printf 'legacy receipt\n' > "$repo/$legacy_receipt"
  printf 'feature receipt\n' > "$repo/$feature_receipt"
  printf 'Private source cites docs/nested.md but must not be scanned.\n' > "$repo/docs/private_note.md"
  printf 'private chat\n' > "$repo/chats/private_thread.md"
  printf 'private closeout\n' > "$repo/chatting/private_closeout.md"
  printf 'private implementation\n' > "$repo/implemented/history.md"

  printf '%s\n' \
    "Root proof: \`$root_receipt\` and \`$legacy_receipt\`." \
    'Private context: docs/private_note.md, chats/private_thread.md, and chatting/private_closeout.md.' \
    'Skipped shapes: https://example.test/docs/remote.md, #docs/private_note.md, docs/**/*.md, and docs/<name>.md.' \
    > "$repo/ROADMAP.md"
  printf '%s\n' \
    "Feature proof: \`4_EVIDENCE/feature_receipt.md\`." \
    'Private history: implemented/history.md.' \
    > "$repo/engine/docs2/FEATURES.md"

  write_sync_files "$repo" ROADMAP.md engine/docs2/FEATURES.md "$root_receipt" "$legacy_receipt" "$feature_receipt"

  local output="$repo/anchoring.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'anchoring and private surfaces'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=5 references_resolvable=2 references_unresolvable=3 status=FAIL' 'root ledger anchoring'
  assert_contains "$output" 'ledger=engine/docs2/FEATURES.md references_examined=2 references_resolvable=1 references_unresolvable=1 status=FAIL' 'nested ledger anchoring'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$root_receipt" 'root evidence resolution detail'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$legacy_receipt" 'legacy evidence resolution detail'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=docs/private_note.md' 'private docs extraction'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=chats/private_thread.md' 'private chats extraction'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=chatting/private_closeout.md' 'private chatting extraction'
  assert_contains "$output" "ledger=engine/docs2/FEATURES.md result=RESOLVABLE path=$feature_receipt" 'feature evidence resolution detail'
  assert_contains "$output" 'ledger=engine/docs2/FEATURES.md result=UNRESOLVABLE reason=outside_sync_surface path=implemented/history.md' 'private implemented extraction'
  assert_not_contains "$output" 'remote.md' 'URL exclusion'
  assert_not_contains "$output" 'name>.md' 'template exclusion'
  assert_not_contains "$output" 'ledger=docs/private_note.md' 'private source ledger exclusion'
}

test_fragment_and_query_citations_counted_by_base_path() {
  local repo
  repo="$(new_fixture_repo fragment_query)"
  mkdir -p "$repo/engine/docs2/3_IMPLEMENTATION"
  local md_positive="engine/docs2/4_EVIDENCE/md_anchor_positive.md"
  local prose_positive="engine/docs2/4_EVIDENCE/prose_query_positive.md"
  local md_negative="engine/docs2/3_IMPLEMENTATION/OPERATIONS.md"
  printf 'md positive\n' > "$repo/$md_positive"
  printf 'prose positive\n' > "$repo/$prose_positive"
  printf 'operations\n' > "$repo/$md_negative"
  printf 'private note\n' > "$repo/docs/private_note.md"

  # Markdown link carrying a #fragment, synced -> counted by its base repository path.
  printf '[Receipt](%s#section-two)\n' "$md_positive" > "$repo/PROJECT_OVERVIEW.md"
  # A markdown #fragment (unsynced), a prose ?query (synced), and a prose #fragment (unsynced).
  printf '%s\n' \
    "Runbook: [Ops](${md_negative}#migration-jobs)." \
    "Evidence: \`${prose_positive}?rev=2\`." \
    'Private: docs/private_note.md#details.' \
    > "$repo/ROADMAP.md"
  write_sync_files "$repo" PROJECT_OVERVIEW.md ROADMAP.md "$md_positive" "$prose_positive"

  local output="$repo/fragment_query.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'fragment/query citations'
  assert_contains "$output" 'ledger=PROJECT_OVERVIEW.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'markdown fragment synced positive denominator'
  assert_contains "$output" "ledger=PROJECT_OVERVIEW.md result=RESOLVABLE path=$md_positive" 'markdown fragment resolves to base path'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=3 references_resolvable=1 references_unresolvable=2 status=FAIL' 'fragment/query denominator'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$prose_positive" 'prose query resolves to base path'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$md_negative" 'markdown fragment unsynced negative by base path'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=docs/private_note.md' 'prose fragment unsynced negative by base path'
  assert_not_contains "$output" '#section-two' 'markdown fragment stripped from base path'
  assert_not_contains "$output" '?rev=2' 'prose query stripped from base path'
  assert_not_contains "$output" '#migration-jobs' 'markdown negative fragment stripped from base path'
  assert_not_contains "$output" '#details' 'prose negative fragment stripped from base path'
}

test_mode_argument_parser_contract() {
  local repo
  repo="$(new_fixture_repo mode_parser)"
  local receipt="engine/docs2/4_EVIDENCE/mode_receipt.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf '[Receipt](%s)\n' "$receipt" > "$repo/ROADMAP.md"
  write_sync_files "$repo" ROADMAP.md "$receipt"

  # Explicit --mode dev-predictive: exactly one label, emitted before per-ledger summaries.
  local output="$repo/mode_explicit.log"
  local status
  status="$(run_validator_args "$repo" "$output" --mode dev-predictive)"
  assert_status 0 "$status" 'explicit dev-predictive mode'
  local label_count
  label_count=$(grep -c '^ownership_mode=dev-predictive$' "$output")
  [ "$label_count" -eq 1 ] || fail "explicit dev-predictive mode: expected exactly one ownership_mode label, got $label_count"
  local label_line summary_line
  label_line=$(grep -n '^ownership_mode=dev-predictive$' "$output" | head -1 | cut -d: -f1)
  summary_line=$(grep -n '^ledger=' "$output" | head -1 | cut -d: -f1)
  [ "$label_line" -lt "$summary_line" ] || fail 'explicit dev-predictive mode: label must precede ledger summaries'

  # Bare run defaults to dev-predictive with the same label.
  output="$repo/mode_default.log"
  status="$(run_validator_args "$repo" "$output")"
  assert_status 0 "$status" 'default mode'
  label_count=$(grep -c '^ownership_mode=dev-predictive$' "$output")
  [ "$label_count" -eq 1 ] || fail "default mode: expected exactly one ownership_mode label, got $label_count"

  # Unrecognized --mode value: exit 2, named usage, no ledger summaries.
  output="$repo/mode_bad_value.log"
  status="$(run_validator_args "$repo" "$output" --mode nonsense)"
  assert_status 2 "$status" 'unrecognized mode value'
  assert_contains "$output" 'usage: validate_public_ledger_citations.sh' 'unrecognized mode value usage'
  assert_not_contains "$output" 'ledger=' 'unrecognized mode value emits no ledger summaries'

  # Bare --mode with no value: exit 2, named usage, no ledger summaries.
  output="$repo/mode_missing_value.log"
  status="$(run_validator_args "$repo" "$output" --mode)"
  assert_status 2 "$status" 'missing mode value'
  assert_contains "$output" 'usage: validate_public_ledger_citations.sh' 'missing mode value usage'
  assert_not_contains "$output" 'ledger=' 'missing mode value emits no ledger summaries'

  # Unrecognized positional: exit 2, named usage, no ledger summaries.
  output="$repo/mode_positional.log"
  status="$(run_validator_args "$repo" "$output" bogus)"
  assert_status 2 "$status" 'unrecognized positional'
  assert_contains "$output" 'usage: validate_public_ledger_citations.sh' 'unrecognized positional usage'
  assert_not_contains "$output" 'ledger=' 'unrecognized positional emits no ledger summaries'
}

test_mirror_mode_tracked_checkout_contracts() {
  local repo
  repo="$(new_fixture_repo mirror_mode)"
  init_mirror_fixture_repo "$repo"

  local tracked_target="engine/docs2/4_EVIDENCE/mirror_receipt.md"
  local untracked_target="engine/docs2/4_EVIDENCE/untracked_receipt.md"
  local deleted_target="engine/docs2/4_EVIDENCE/deleted_receipt.md"
  printf 'mirror receipt\n' > "$repo/$tracked_target"
  printf 'untracked receipt\n' > "$repo/$untracked_target"
  printf 'deleted receipt\n' > "$repo/$deleted_target"
  printf '%s\n' \
    "Tracked proof: [$tracked_target]($tracked_target)." \
    "Untracked proof: [$untracked_target]($untracked_target)." \
    "Deleted proof: [$deleted_target]($deleted_target)." \
    'Above root: [Above](../../ROADMAP.md).' \
    > "$repo/ROADMAP.md"
  track_mirror_paths "$repo" ROADMAP.md "$tracked_target" "$deleted_target"

  local missing_config_output="$repo/missing_config_dev_predictive.log"
  local missing_config_status
  missing_config_status="$(run_validator_args "$repo" "$missing_config_output" --mode dev-predictive)"
  assert_status 1 "$missing_config_status" 'mirror fixture dev-predictive missing config red'
  assert_contains "$missing_config_output" 'Missing .debbie.toml' 'mirror fixture dev-predictive missing config red'

  rm "$repo/$deleted_target"

  local shim_dir="$repo/shim"
  mkdir -p "$shim_dir"
  printf '#!/usr/bin/env bash\nexit 99\n' > "$shim_dir/python3"
  chmod +x "$shim_dir/python3"

  local output="$repo/mirror.log"
  local mirror_status=0
  DOC_SYNC_PYTHON="$repo/no_such_python" PATH="$shim_dir:/bin:/usr/bin" PUBLIC_LEDGER_REPO_DIR="$repo" \
    bash "$VALIDATOR" --mode mirror > "$output" 2>&1 || mirror_status=$?

  assert_status 1 "$mirror_status" 'mirror mode tracked checkout'
  assert_contains "$output" 'ownership_mode=mirror' 'mirror mode label'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=4 references_resolvable=1 references_unresolvable=3 status=FAIL' 'mirror denominator'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$tracked_target" 'mirror tracked target resolves'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=untracked path=$untracked_target" 'mirror untracked target reason'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=tracked_file_missing path=$deleted_target" 'mirror tracked deleted target reason'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=above_repository_root path=../../ROADMAP.md' 'mirror above-root reason'
  assert_not_contains "$output" 'doc_sync_python_tomllib_unavailable' 'mirror mode does not require Python'

  local python_repo
  python_repo="$(new_fixture_repo dev_predictive_python)"
  printf 'No citations here.\n' > "$python_repo/ROADMAP.md"
  write_sync_files "$python_repo" ROADMAP.md
  local dev_python_output="$python_repo/dev_predictive_python.log"
  local dev_python_status=0
  DOC_SYNC_PYTHON="$repo/no_such_python" PATH="$shim_dir:/bin:/usr/bin" PUBLIC_LEDGER_REPO_DIR="$python_repo" \
    bash "$VALIDATOR" --mode dev-predictive > "$dev_python_output" 2>&1 || dev_python_status=$?
  assert_status 1 "$dev_python_status" 'dev-predictive requires tomllib Python'
  assert_contains "$dev_python_output" 'doc_sync_python_tomllib_unavailable' 'dev-predictive named interpreter error'

  local non_git
  non_git="$(new_fixture_repo mirror_non_git)"
  printf 'No git here.\n' > "$non_git/ROADMAP.md"
  local non_git_output="$non_git/non_git.log"
  local non_git_status
  non_git_status="$(run_validator_args "$non_git" "$non_git_output" --mode mirror)"
  assert_status 1 "$non_git_status" 'mirror non-git setup failure'
  assert_contains "$non_git_output" 'ERROR: mirror mode requires REPO_DIR to be a git checkout' 'mirror non-git setup failure'
  assert_not_contains "$non_git_output" 'status=VACUOUS' 'mirror non-git never vacuous'

  local empty_repo
  empty_repo="$(new_fixture_repo mirror_empty)"
  init_mirror_fixture_repo "$empty_repo"
  local empty_output="$empty_repo/empty.log"
  local empty_status
  empty_status="$(run_validator_args "$empty_repo" "$empty_output" --mode mirror)"
  assert_status 1 "$empty_status" 'mirror empty tracked set'
  assert_contains "$empty_output" 'ERROR: mirror mode tracked file set is empty' 'mirror empty tracked set'
  assert_not_contains "$empty_output" 'status=VACUOUS' 'mirror empty tracked set never vacuous'
}

test_mirror_mode_inventory_sort_failure_fails_closed() {
  local repo
  repo="$(new_fixture_repo mirror_sort_failure)"
  init_mirror_fixture_repo "$repo"

  local receipt="engine/docs2/4_EVIDENCE/mirror_sort_receipt.md"
  printf 'mirror receipt\n' > "$repo/$receipt"
  printf '[Receipt](%s)\n' "$receipt" > "$repo/ROADMAP.md"
  track_mirror_paths "$repo" ROADMAP.md "$receipt"

  local shim_dir="$repo/shim"
  local counter_file="$repo/sort_count"
  local real_sort
  real_sort="$(command -v sort)"
  mkdir -p "$shim_dir"
  cat > "$shim_dir/sort" <<'EOF'
#!/usr/bin/env bash
count=0
[ ! -f "$PUBLIC_LEDGER_SORT_COUNTER" ] || read -r count < "$PUBLIC_LEDGER_SORT_COUNTER"
printf '%d\n' "$((count + 1))" > "$PUBLIC_LEDGER_SORT_COUNTER"
"$PUBLIC_LEDGER_REAL_SORT" "$@"
sort_status=$?
[ "$count" -ne 0 ] || exit 17
exit "$sort_status"
EOF
  chmod +x "$shim_dir/sort"

  local output="$repo/sort_failure.log"
  local validator_status=0
  PUBLIC_LEDGER_SORT_COUNTER="$counter_file" PUBLIC_LEDGER_REAL_SORT="$real_sort" \
    PATH="$shim_dir:$PATH" PUBLIC_LEDGER_REPO_DIR="$repo" \
    bash "$VALIDATOR" --mode mirror > "$output" 2>&1 || validator_status=$?

  assert_status 1 "$validator_status" 'mirror inventory sort failure'
  assert_contains "$output" 'ERROR: mirror mode failed to normalize tracked file inventory' 'mirror inventory sort failure'
  assert_not_contains "$output" 'status=PASS' 'mirror inventory sort failure cannot report a passing ledger'
}

test_mirror_mode_newline_filename_injection_fails_closed() {
  local repo
  repo="$(new_fixture_repo mirror_newline_filename)"
  init_mirror_fixture_repo "$repo"

  local target="engine/docs2/4_EVIDENCE/untracked_receipt.md"
  mkdir -p "$repo/engine/docs2/4_EVIDENCE"
  printf 'untracked target\n' > "$repo/$target"
  printf '[Receipt](%s)\n' "$target" > "$repo/ROADMAP.md"

  local spoof_ledger=$'spoof\nROADMAP.md'
  local spoof_target=$'spoof\nengine/docs2/4_EVIDENCE/untracked_receipt.md'
  mkdir -p "$repo/"$'spoof\nengine/docs2/4_EVIDENCE'
  printf 'tracked spoof\n' > "$repo/$spoof_ledger"
  printf 'tracked spoof\n' > "$repo/$spoof_target"
  track_mirror_paths "$repo" "$spoof_ledger" "$spoof_target"

  local output="$repo/newline_filename.log"
  local status
  status="$(run_validator_args "$repo" "$output" --mode mirror)"

  assert_status 1 "$status" 'mirror newline filename injection'
  assert_contains "$output" 'ERROR: mirror mode tracked filename contains unsupported line break' 'mirror newline filename injection'
  assert_not_contains "$output" 'status=PASS' 'mirror newline filename injection cannot report a passing ledger'
}

test_unreadable_synced_ledger_fails_closed() {
  local repo
  repo="$(new_fixture_repo unreadable_ledger)"
  printf 'This ledger should not be readable.\n' > "$repo/ROADMAP.md"
  chmod 000 "$repo/ROADMAP.md"
  write_sync_files "$repo" ROADMAP.md

  local output="$repo/unreadable.log"
  local status
  status="$(run_validator "$repo" "$output")"
  chmod 644 "$repo/ROADMAP.md"

  assert_status 1 "$status" 'unreadable synced ledger'
  assert_contains "$output" 'ERROR: failed to extract references from synced ledger: ROADMAP.md' 'unreadable synced ledger'
  assert_not_contains "$output" 'status=VACUOUS' 'unreadable synced ledger'
}

test_nul_byte_in_synced_ledger_fails_closed() {
  local repo
  repo="$(new_fixture_repo nul_byte_ledger)"
  local receipt="engine/docs2/4_EVIDENCE/nul_positive.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf '[Receipt](%s)\n' "$receipt" > "$repo/PROJECT_OVERVIEW.md"
  printf 'Hidden private citation: [Private](docs/private_note.md)\000\n' > "$repo/ROADMAP.md"
  write_sync_files "$repo" PROJECT_OVERVIEW.md ROADMAP.md "$receipt"

  local output="$repo/nul_byte.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'NUL-byte synced ledger'
  assert_contains "$output" 'ERROR: synced public ledger contains a NUL byte: ROADMAP.md' 'NUL-byte synced ledger'
  assert_not_contains "$output" 'ledger=ROADMAP.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'NUL-byte synced ledger'
}

test_scheme_qualified_urls_are_skipped_case_insensitively() {
  local repo
  repo="$(new_fixture_repo scheme_urls)"
  local receipt="engine/docs2/4_EVIDENCE/scheme_positive.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf '[Receipt](%s)\n' "$receipt" > "$repo/PROJECT_OVERVIEW.md"
  printf '%s\n' \
    'Uppercase URL: [Remote](HTTPS://example.test/docs/private_note.md).' \
    'FTP URL: [Archive](ftp://example.test/docs/archive.md).' \
    > "$repo/ROADMAP.md"
  write_sync_files "$repo" PROJECT_OVERVIEW.md ROADMAP.md "$receipt"

  local output="$repo/scheme_urls.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 0 "$status" 'scheme-qualified URLs'
  assert_contains "$output" 'ledger=PROJECT_OVERVIEW.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'scheme-qualified positive control'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'scheme-qualified URLs'
  assert_not_contains "$output" 'HTTPS:' 'uppercase URL exclusion'
  assert_not_contains "$output" 'ftp:' 'FTP URL exclusion'
}

test_all_vacuous_public_ledgers_fail_zero_denominator() {
  local repo
  repo="$(new_fixture_repo all_vacuous)"
  printf 'No citations here.\n' > "$repo/PROJECT_OVERVIEW.md"
  printf 'No citations here either.\n' > "$repo/ROADMAP.md"
  printf 'No citations in this change log.\n' > "$repo/CHANGELOG.md"
  printf 'No feature citations.\n' > "$repo/engine/docs2/FEATURES.md"
  write_sync_files "$repo" PROJECT_OVERVIEW.md ROADMAP.md CHANGELOG.md engine/docs2/FEATURES.md

  local output="$repo/all_vacuous.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'all-vacuous public ledgers'
  assert_contains "$output" 'ledger=PROJECT_OVERVIEW.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'all-vacuous overview ledger'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'all-vacuous roadmap ledger'
  assert_contains "$output" 'ledger=CHANGELOG.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'all-vacuous changelog ledger'
  assert_contains "$output" 'ledger=engine/docs2/FEATURES.md references_examined=0 references_resolvable=0 references_unresolvable=0 status=VACUOUS' 'all-vacuous features ledger'
  assert_contains "$output" 'ERROR: zero public ledger references examined across synced ledgers' 'all-vacuous zero denominator error'
}

test_markdown_link_title_is_stripped_before_resolution() {
  local repo
  repo="$(new_fixture_repo markdown_title)"
  local receipt="engine/docs2/4_EVIDENCE/real.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf '[T](%s "Receipt")\n' "$receipt" > "$repo/ROADMAP.md"
  write_sync_files "$repo" ROADMAP.md "$receipt"

  local output="$repo/markdown_title.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 0 "$status" 'markdown title'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'markdown title'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$receipt" 'markdown title resolution'
  assert_not_contains "$output" 'Receipt' 'markdown title excluded from path'
  assert_not_contains "$output" 'references_examined=2' 'markdown title not double-counted'
}

test_root_absolute_markdown_links_are_counted() {
  local repo
  repo="$(new_fixture_repo root_absolute)"
  local receipt="engine/docs2/4_EVIDENCE/root_absolute.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf '[Abs](/%s)\n' "$receipt" > "$repo/ROADMAP.md"
  write_sync_files "$repo" ROADMAP.md

  local output="$repo/root_absolute.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'root-absolute markdown link'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=0 references_unresolvable=1 status=FAIL' 'root-absolute markdown link'
  assert_contains "$output" "ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=$receipt" 'root-absolute markdown link resolution'
}

test_markdown_extractor_failure_propagates() {
  local output="$WORK_DIR/markdown_extractor_failure.log"
  local status=0

  (
    # shellcheck source=engine/tests/validate_public_ledger_citations.sh
    source "$VALIDATOR"
    # These overrides are invoked indirectly by public_ledger_extract_references.
    # shellcheck disable=SC2329
    doc_sync_extract_relative_markdown_links() {
      return 23
    }
    # shellcheck disable=SC2329
    public_ledger_extract_prose_references() {
      printf 'docs/prose_succeeds.md\n'
    }

    public_ledger_extract_references ROADMAP.md
  ) > "$output" 2>&1 || status=$?

  [ "$status" -ne 0 ] || fail 'markdown extractor failure: expected nonzero exit'
}

test_path_shaped_markdown_title_is_not_prose() {
  local repo
  repo="$(new_fixture_repo path_shaped_markdown_title)"
  local receipt="engine/docs2/4_EVIDENCE/real.md"
  printf 'receipt body\n' > "$repo/$receipt"
  printf '[T](%s "docs/private_note.md")\n' "$receipt" > "$repo/ROADMAP.md"
  printf 'private note\n' > "$repo/docs/private_note.md"
  write_sync_files "$repo" ROADMAP.md "$receipt"

  local output="$repo/path_shaped_markdown_title.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 0 "$status" 'path-shaped markdown title'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'path-shaped markdown title'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$receipt" 'path-shaped markdown title target'
  assert_not_contains "$output" 'path=docs/private_note.md' 'path-shaped markdown title excluded from prose'
}

test_private_planning_pp_prose_citations_are_extracted_and_unresolvable() {
  local repo
  repo="$(new_fixture_repo private_planning_pp)"
  local receipt="engine/docs2/4_EVIDENCE/pp_positive.md"
  printf 'receipt body\n' > "$repo/$receipt"
  mkdir -p "$repo/pp/supervise_notes"
  printf 'private supervise ledger\n' > "$repo/pp/supervise.md"
  printf 'private supervise note\n' > "$repo/pp/supervise_notes/flapjack_dev.md"
  printf '%s\n' \
    "Root proof: \`$receipt\`." \
    'Provenance: `pp/supervise_notes/flapjack_dev.md`, and registered in `pp/supervise.md`.' \
    > "$repo/ROADMAP.md"
  write_sync_files "$repo" ROADMAP.md "$receipt"

  local output="$repo/private_planning_pp.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'private planning pp citations'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=3 references_resolvable=1 references_unresolvable=2 status=FAIL' 'pp denominator'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$receipt" 'pp positive control'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=pp/supervise_notes/flapjack_dev.md' 'pp nested note extraction'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=outside_sync_surface path=pp/supervise.md' 'pp root note extraction'
}

test_path_traversal_above_repo_root_is_unresolvable() {
  local repo
  repo="$(new_fixture_repo path_traversal)"
  printf '%s\n' \
    'Within root prose: docs/../ROADMAP.md' \
    'Above root prose: docs/../../ROADMAP.md' \
    > "$repo/ROADMAP.md"
  printf '%s\n' \
    '[Within root](../../ROADMAP.md)' \
    '[Above root](../../../../ROADMAP.md)' \
    > "$repo/engine/docs2/FEATURES.md"
  write_sync_files "$repo" ROADMAP.md engine/docs2/FEATURES.md

  local output="$repo/path_traversal.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 1 "$status" 'path traversal above repository root'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=2 references_resolvable=1 references_unresolvable=1 status=FAIL' 'prose traversal denominator'
  assert_contains "$output" 'ledger=ROADMAP.md result=RESOLVABLE path=ROADMAP.md' 'within-root prose traversal'
  assert_contains "$output" 'ledger=ROADMAP.md result=UNRESOLVABLE reason=above_repository_root path=docs/../../ROADMAP.md' 'above-root prose traversal'
  assert_contains "$output" 'ledger=engine/docs2/FEATURES.md references_examined=2 references_resolvable=1 references_unresolvable=1 status=FAIL' 'markdown traversal denominator'
  assert_contains "$output" 'ledger=engine/docs2/FEATURES.md result=RESOLVABLE path=ROADMAP.md' 'within-root traversal'
  assert_contains "$output" 'ledger=engine/docs2/FEATURES.md result=UNRESOLVABLE reason=above_repository_root path=engine/docs2/../../../../ROADMAP.md' 'above-root traversal'
}

test_remap_source_that_is_also_synced_is_published() {
  local repo
  repo="$(new_fixture_repo remap_source_owned)"

  local shared="shared/owned_source.md"
  mkdir -p "$repo/shared"
  printf 'body for %s\n' "$shared" > "$repo/$shared"
  printf 'Remap source also owned: [%s](%s).\n' "$shared" "$shared" > "$repo/ROADMAP.md"

  cat > "$repo/.debbie.toml" <<EOF
[sync]
files = [
  "ROADMAP.md",
  "$shared",
]

[[sync.remap]]
from = "$shared"
to = "public/owned_source.md"
EOF

  local output="$repo/remap_source_owned.log"
  local status
  status="$(run_validator "$repo" "$output")"

  assert_status 0 "$status" 'independently owned remap source publishes'
  assert_contains "$output" 'ledger=ROADMAP.md references_examined=1 references_resolvable=1 references_unresolvable=0 status=PASS' 'independently owned remap source denominator'
  assert_contains "$output" "ledger=ROADMAP.md result=RESOLVABLE path=$shared" 'remap source resolves when also an explicit files entry'
}

TEST_CASES=(
  test_worktree_existence_does_not_imply_public_resolvability
  test_sync_surface_remap_and_exclude_semantics_match_debbie
  test_remap_source_that_is_also_synced_is_published
  test_vacuous_ledger_is_distinct_from_invalid_sync_configuration
  test_prose_paths_anchor_and_private_surfaces_are_checked
  test_fragment_and_query_citations_counted_by_base_path
  test_mode_argument_parser_contract
  test_mirror_mode_tracked_checkout_contracts
  test_mirror_mode_inventory_sort_failure_fails_closed
  test_mirror_mode_newline_filename_injection_fails_closed
  test_unreadable_synced_ledger_fails_closed
  test_nul_byte_in_synced_ledger_fails_closed
  test_scheme_qualified_urls_are_skipped_case_insensitively
  test_all_vacuous_public_ledgers_fail_zero_denominator
  test_markdown_link_title_is_stripped_before_resolution
  test_root_absolute_markdown_links_are_counted
  test_markdown_extractor_failure_propagates
  test_path_shaped_markdown_title_is_not_prose
  test_private_planning_pp_prose_citations_are_extracted_and_unresolvable
  test_path_traversal_above_repo_root_is_unresolvable
)

run_test_case() {
  local case_name="$1"
  "$case_name"
}

run_all_cases() {
  local case_name
  for case_name in "${TEST_CASES[@]}"; do
    run_test_case "$case_name"
  done
}

case "${1:-}" in
  "")
    run_all_cases
    ;;
  --mirror-fixture-only)
    run_test_case test_mirror_mode_tracked_checkout_contracts
    ;;
  *)
    fail "unknown test filter: $1"
    ;;
esac

printf 'PASS: public ledger citation validator regression coverage\n'
