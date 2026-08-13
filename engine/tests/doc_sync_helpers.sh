#!/usr/bin/env bash
# Shared helpers for markdown validation against the .debbie sync surface.

doc_sync_init() {
  DOC_SYNC_REPO_DIR="$1"
  DOC_SYNC_CONFIG_FILE="$DOC_SYNC_REPO_DIR/.debbie.toml"
  DOC_SYNC_FILES_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-sync-files.XXXXXX")
  DOC_SYNC_DIRS_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-sync-dirs.XXXXXX")
  DOC_SYNC_EXCLUDES_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-sync-excludes.XXXXXX")
  DOC_SYNC_REMAPS_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-sync-remaps.XXXXXX")
  DOC_SYNC_PARSED_LOG=$(mktemp "${TMPDIR:-/tmp}/flapjack-sync-parsed.XXXXXX")
}

doc_sync_cleanup() {
  rm -f "${DOC_SYNC_FILES_LOG:-}" "${DOC_SYNC_DIRS_LOG:-}" "${DOC_SYNC_EXCLUDES_LOG:-}" "${DOC_SYNC_REMAPS_LOG:-}" "${DOC_SYNC_PARSED_LOG:-}"
}

doc_sync_normalize_repo_path() {
  local raw_path="$1"
  raw_path="${raw_path#./}"

  local IFS='/'
  local -a parts
  local -a stack=()
  read -r -a parts <<< "$raw_path"

  local part
  for part in "${parts[@]}"; do
    case "$part" in
      ""|".")
        ;;
      "..")
        if [ "${#stack[@]}" -gt 0 ]; then
          unset 'stack[${#stack[@]}-1]'
        fi
        ;;
      *)
        stack+=("$part")
        ;;
    esac
  done

  if [ "${#stack[@]}" -eq 0 ]; then
    printf ''
    return 0
  fi

  local output="${stack[0]}"
  local idx
  for ((idx = 1; idx < ${#stack[@]}; idx++)); do
    output+="/${stack[$idx]}"
  done

  printf '%s' "$output"
}

doc_sync_python_with_tomllib() {
  local -a candidates=()
  if [ -n "${DOC_SYNC_PYTHON:-}" ]; then
    candidates+=("$DOC_SYNC_PYTHON")
  else
    candidates+=(python3 python3.14 python3.13 python3.12 python3.11)
  fi

  local -a tried=()
  local candidate
  for candidate in "${candidates[@]}"; do
    if ! command -v "$candidate" >/dev/null 2>&1; then
      tried+=("$candidate: not found")
      continue
    fi
    if "$candidate" -c 'import tomllib' >/dev/null 2>&1; then
      printf '%s\n' "$candidate"
      return 0
    fi
    tried+=("$candidate: cannot import tomllib")
  done

  printf 'ERROR: doc_sync_python_tomllib_unavailable; tried: %s\n' "${tried[*]}" >&2
  return 1
}

doc_sync_emit_sync_surface() {
  local parser_python
  parser_python="$(doc_sync_python_with_tomllib)" || return 1

  "$parser_python" - "$DOC_SYNC_CONFIG_FILE" <<'PY'
import sys
import tomllib

config_path = sys.argv[1]
with open(config_path, "rb") as handle:
    raw = tomllib.load(handle)

sync = raw.get("sync", {})
for path in sync.get("files", []):
    if path:
        print(f"FILE\t{path}")

for item in sync.get("dirs", []):
    path = item.get("path", "")
    if not path:
        continue
    print(f"DIR\t{path}")
    for pattern in item.get("exclude", []):
        if pattern:
            print(f"EXCLUDE\t{path}\t{pattern}")

for item in sync.get("remap", []):
    src = item.get("from", "")
    dest = item.get("to", "")
    if src and dest:
        print(f"REMAP\t{src}\t{dest}")
PY
}

doc_sync_collect_sync_surface() {
  if [ ! -f "$DOC_SYNC_CONFIG_FILE" ]; then
    printf "\033[0;31mMissing .debbie.toml at %s\033[0m\n" "$DOC_SYNC_CONFIG_FILE"
    return 1
  fi

  doc_sync_emit_sync_surface > "$DOC_SYNC_PARSED_LOG" || return 1

  : > "$DOC_SYNC_FILES_LOG"
  : > "$DOC_SYNC_DIRS_LOG"
  : > "$DOC_SYNC_EXCLUDES_LOG"
  : > "$DOC_SYNC_REMAPS_LOG"

  while IFS=$'\t' read -r kind col1 col2; do
    case "$kind" in
      FILE)
        if [ -n "$col1" ]; then
          printf '%s\n' "$(doc_sync_normalize_repo_path "$col1")" >> "$DOC_SYNC_FILES_LOG"
        fi
        ;;
      DIR)
        if [ -n "$col1" ]; then
          printf '%s\n' "$(doc_sync_normalize_repo_path "${col1%/}")" >> "$DOC_SYNC_DIRS_LOG"
        fi
        ;;
      EXCLUDE)
        if [ -n "$col1" ] && [ -n "${col2:-}" ]; then
          printf '%s\t%s\n' "$(doc_sync_normalize_repo_path "${col1%/}")" "$col2" >> "$DOC_SYNC_EXCLUDES_LOG"
        fi
        ;;
      REMAP)
        if [ -n "$col1" ] && [ -n "${col2:-}" ]; then
          printf '%s\t%s\n' "$(doc_sync_normalize_repo_path "$col1")" "$(doc_sync_normalize_repo_path "$col2")" >> "$DOC_SYNC_REMAPS_LOG"
        fi
        ;;
    esac
  done < "$DOC_SYNC_PARSED_LOG"

  sort -u -o "$DOC_SYNC_FILES_LOG" "$DOC_SYNC_FILES_LOG"
  sort -u -o "$DOC_SYNC_DIRS_LOG" "$DOC_SYNC_DIRS_LOG"
  sort -u -o "$DOC_SYNC_EXCLUDES_LOG" "$DOC_SYNC_EXCLUDES_LOG"
  sort -u -o "$DOC_SYNC_REMAPS_LOG" "$DOC_SYNC_REMAPS_LOG"
}

doc_sync_component_match() {
  local pattern="$1"
  local relative_path="$2"
  local IFS='/'
  local -a parts
  read -r -a parts <<< "$relative_path"

  local part
  for part in "${parts[@]}"; do
    [[ "$part" == $pattern ]] && return 0
  done
  return 1
}

doc_sync_path_or_parent_match() {
  local pattern="$1"
  local relative_path="$2"

  while [ -n "$relative_path" ]; do
    [[ "$relative_path" == $pattern ]] && return 0
    case "$relative_path" in
      */*) relative_path="${relative_path%/*}" ;;
      *) break ;;
    esac
  done

  return 1
}

doc_sync_matches_exclude_pattern() {
  local relative_path="$1"
  local pattern="$2"

  pattern="${pattern#"${pattern%%[![:space:]]*}"}"
  pattern="${pattern%"${pattern##*[![:space:]]}"}"
  if [ -z "$pattern" ]; then
    return 1
  fi

  local directory_only=false
  if [[ "$pattern" == */ ]]; then
    directory_only=true
    pattern="${pattern%/}"
  fi

  local anchored=false
  if [[ "$pattern" == /* ]]; then
    anchored=true
    pattern="${pattern#/}"
  fi

  [ -n "$pattern" ] || return 1

  if [ "$directory_only" = true ]; then
    if [[ "$pattern" == *"/"* ]] || [ "$anchored" = true ]; then
      [[ "$relative_path" == "$pattern" || "$relative_path" == "$pattern"/* ]]
      return
    fi
    doc_sync_component_match "$pattern" "$relative_path"
    return
  fi

  if [ "$anchored" = true ] || [[ "$pattern" == *"/"* ]]; then
    doc_sync_path_or_parent_match "$pattern" "$relative_path"
    return
  fi

  doc_sync_component_match "$pattern" "$relative_path"
}

doc_sync_path_is_synced() {
  local raw_path="$1"
  local normalized_path
  normalized_path="$(doc_sync_normalize_repo_path "$raw_path")"
  normalized_path="${normalized_path%/}"

  if [ -z "$normalized_path" ]; then
    return 1
  fi

  # Ownership follows Debbie's is_owned order: explicit files, then remap
  # destinations, then non-excluded dir contents. Debbie has no remap-source
  # rule, so a remap source is published only when it is independently owned
  # (also an explicit file or inside a synced dir) — never denied outright.
  if grep -Fxq -- "$normalized_path" "$DOC_SYNC_FILES_LOG"; then
    return 0
  fi

  local remap_source
  local remap_dest
  while IFS=$'\t' read -r remap_source remap_dest; do
    [ -n "${remap_dest:-}" ] || continue
    if [ "$normalized_path" = "$remap_dest" ]; then
      return 0
    fi
  done < "$DOC_SYNC_REMAPS_LOG"

  local dir_path
  while IFS= read -r dir_path; do
    [ -n "$dir_path" ] || continue

    local dir_root="${dir_path%/}"
    if [ "$normalized_path" = "$dir_root" ]; then
      return 0
    fi

    case "$normalized_path" in
      "$dir_root"/*)
        local relative_to_dir="${normalized_path#"$dir_root"/}"
        local excluded=false

        while IFS=$'\t' read -r exclude_dir exclude_pattern; do
          [ "$exclude_dir" = "$dir_root" ] || continue
          if doc_sync_matches_exclude_pattern "$relative_to_dir" "$exclude_pattern"; then
            excluded=true
            break
          fi
        done < "$DOC_SYNC_EXCLUDES_LOG"

        if [ "$excluded" = false ]; then
          return 0
        fi
        ;;
    esac
  done < "$DOC_SYNC_DIRS_LOG"

  return 1
}

doc_sync_extract_relative_markdown_links() {
  local doc_rel="$1"
  local doc_abs="$DOC_SYNC_REPO_DIR/$doc_rel"
  local doc_dir_rel
  doc_dir_rel=$(dirname -- "$doc_rel")

  while IFS= read -r match_line; do
    [ -n "$match_line" ] || continue

    local line_num
    line_num=$(printf '%s' "$match_line" | cut -d: -f1)
    local line_content
    line_content=$(printf '%s' "$match_line" | cut -d: -f2-)

    while IFS= read -r target; do
      [ -n "$target" ] || continue

      case "$target" in
        http://*|https://*|mailto:*|\#*)
          continue
          ;;
      esac

      local link_path
      link_path=$(printf '%s' "$target" | sed 's/#.*//')
      if [ -z "$link_path" ]; then
        continue
      fi

      local resolved
      if [[ "$link_path" == /* ]]; then
        resolved=$(doc_sync_normalize_repo_path "${link_path#/}")
      else
        resolved=$(doc_sync_normalize_repo_path "$doc_dir_rel/$link_path")
      fi

      if [ -z "$resolved" ]; then
        continue
      fi

      printf '%s\t%s\t%s\t%s\n' "$doc_rel" "$line_num" "$target" "$resolved"
    done < <(printf '%s\n' "$line_content" | grep -oE '\]\([^)]+\)' | sed 's/^\](//' | sed 's/)$//' || true)
  done < <(grep -n '\]([^)]*)' "$doc_abs" || true)
}

doc_sync_collect_public_doc_graph_docs() {
  local output_log="$1"
  : > "$output_log"

  local file_path
  while IFS= read -r file_path; do
    [ -n "$file_path" ] || continue
    case "$file_path" in
      README.md|PROJECT_OVERVIEW.md|ROADMAP.md|engine/README.md|engine/LIB.md|engine/docs2/FEATURES.md|engine/loadtest/BENCHMARKS.md)
        if [ -f "$DOC_SYNC_REPO_DIR/$file_path" ]; then
          printf '%s\n' "$file_path" >> "$output_log"
        fi
        ;;
    esac
  done < "$DOC_SYNC_FILES_LOG"

  local dir_path
  while IFS= read -r dir_path; do
    [ -n "$dir_path" ] || continue

    case "$dir_path" in
      engine/docs2/1_STRATEGY|engine/docs2/3_IMPLEMENTATION)
        ;;
      *)
        continue
        ;;
    esac

    local dir_abs="$DOC_SYNC_REPO_DIR/$dir_path"
    [ -d "$dir_abs" ] || continue

    local doc_abs
    while IFS= read -r doc_abs; do
      [ -n "$doc_abs" ] || continue

      local doc_rel="${doc_abs#"$DOC_SYNC_REPO_DIR"/}"
      if doc_sync_path_is_synced "$doc_rel"; then
        printf '%s\n' "$doc_rel" >> "$output_log"
      fi
    done < <(find "$dir_abs" -type f -name '*.md' -print)
  done < "$DOC_SYNC_DIRS_LOG"

  sort -u -o "$output_log" "$output_log"
}

doc_sync_collect_validation_docs() {
  local output_log="$1"
  shift || true

  doc_sync_collect_public_doc_graph_docs "$output_log"

  local extra_doc
  for extra_doc in "$@"; do
    [ -n "$extra_doc" ] || continue
    [ -f "$DOC_SYNC_REPO_DIR/$extra_doc" ] || continue
    printf '%s\n' "$extra_doc" >> "$output_log"
  done

  sort -u -o "$output_log" "$output_log"
}

doc_sync_count_log_lines() {
  local log_path="$1"
  if [ ! -f "$log_path" ]; then
    printf '0'
    return 0
  fi

  wc -l < "$log_path" | tr -d ' '
}
