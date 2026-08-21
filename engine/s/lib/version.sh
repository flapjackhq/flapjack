#!/usr/bin/env bash
# version.sh — Shared version helper for the public release launcher.
# Sourced by release.sh.
#
# Exports:
#   latest_release_version REPO    — resolve latest GitHub release tag to semver
#   auto_bump_version REPO CHANNEL [CURRENT_VERSION] — derive next prerelease version
#   fix_cargo_path_deps REPO_ROOT  — strip unreachable path deps from synced Cargo.toml files

# Resolve the latest release version from a GitHub repo.
# Uses the gh CLI to query the most recent release tag, strips the leading "v",
# and falls back to "0.0.0" when the repo has no releases.
#
# Usage: latest_release_version "owner/repo"
latest_release_version() {
  local repo="$1"
  local tag
  tag=$(gh release view --repo "$repo" --json tagName -q .tagName 2>/dev/null || echo "")
  if [ -z "$tag" ]; then
    echo "0.0.0"
    return 0
  fi
  # Strip leading "v" prefix (e.g. "v1.0.3" -> "1.0.3")
  echo "${tag#v}"
}

# Derive the next prerelease version by bumping the patch number and appending
# a channel suffix.
#
# When called with 2 args, resolves the current version from the latest release.
# When called with 3 args, uses the provided current version directly.
#
# Usage:
#   auto_bump_version "owner/repo" "beta"              → e.g. "1.0.4-beta"
#   auto_bump_version "owner/repo" "beta" "1.0.3"      → "1.0.4-beta"
auto_bump_version() {
  local repo="$1"
  local channel="$2"
  local current="${3:-}"

  # Resolve from latest release if no explicit version provided
  if [ -z "$current" ]; then
    current=$(latest_release_version "$repo")
  fi

  # Strip any existing prerelease suffix for clean semver parsing
  # e.g. "1.0.3-beta" -> "1.0.3"
  local base_version
  base_version="${current%%-*}"

  # Parse major.minor.patch
  local major minor patch
  IFS='.' read -r major minor patch <<< "$base_version"
  major="${major:-0}"
  minor="${minor:-0}"
  patch="${patch:-0}"

  # Bump patch and append channel suffix
  echo "${major}.${minor}.$(( patch + 1 ))-${channel}"
}

# Validate and fix Cargo.toml path dependencies in a synced repo.
#
# After debbie syncs dev files to a public repo, some workspace path deps may
# reference directories that weren't copied. This function checks each path dep
# under engine/ and attempts to strip unreachable ones. Returns non-zero if any
# path dep cannot be resolved or safely removed.
#
# Usage: fix_cargo_path_deps "/path/to/synced/repo"
fix_cargo_path_deps() {
  local repo_root="$1"
  local engine_dir="$repo_root/engine"
  local had_unfixable=0

  if [ ! -d "$engine_dir" ]; then
    echo "fix_cargo_path_deps: no engine/ directory in $repo_root" >&2
    return 1
  fi

  local cargo_file
  while IFS= read -r cargo_file; do
    local cargo_dir
    cargo_dir="$(dirname "$cargo_file")"

    # Read each line; detect inline-table dependency path entries.
    # Lines like: crate-name = { path = "...", version = "1.0" }
    # Skip standalone `path = "src/main.rs"` in [lib]/[[bin]] sections.
    local tmp_file
    tmp_file=$(mktemp)
    local modified=0

    while IFS= read -r line; do
      # Skip non-dependency lines (standalone path = "src/main.rs" in [lib]/[[bin]] sections, etc.)
      if ! [[ "$line" =~ ^[a-zA-Z][a-zA-Z0-9_-]*[[:space:]]*=[[:space:]]*\{.*path[[:space:]]*=[[:space:]]*\" ]]; then
        printf '%s\n' "$line" >> "$tmp_file"
        continue
      fi

      local dep_path
      dep_path=$(echo "$line" | sed -n 's/.*path[[:space:]]*=[[:space:]]*"\([^"]*\)".*/\1/p')

      # Keep lines where path target exists or dep_path couldn't be extracted
      if [ -z "$dep_path" ] || [ -d "$cargo_dir/$dep_path" ]; then
        printf '%s\n' "$line" >> "$tmp_file"
        continue
      fi

      # Path target missing — strip the path attribute
      local fixed
      fixed=$(echo "$line" | sed -E 's/,[[:space:]]*path[[:space:]]*=[[:space:]]*"[^"]*"//g')   # path after other attrs
      fixed=$(echo "$fixed" | sed -E 's/path[[:space:]]*=[[:space:]]*"[^"]*",[[:space:]]*//g')   # path before other attrs
      fixed=$(echo "$fixed" | sed -E 's/path[[:space:]]*=[[:space:]]*"[^"]*"//g')                # path as only attr

      # Cargo requires at least one resolvable source: version, workspace = true, or git.
      local inner
      inner=$(echo "$fixed" | sed -n 's/.*{\(.*\)}.*/\1/p' | tr -d '[:space:]')
      if [ -n "$inner" ] && echo "$inner" | grep -qE '(version=|workspace=|git=)'; then
        printf '%s\n' "$fixed" >> "$tmp_file"
        modified=1
        echo "  fixed: removed unreachable path dep '$dep_path' in $cargo_file" >&2
      else
        printf '%s\n' "$line" >> "$tmp_file"
        echo "  WARNING: cannot resolve path dep '$dep_path' in $cargo_file (no fallback source)" >&2
        had_unfixable=1
      fi
    done < "$cargo_file"

    if [ "$modified" -eq 1 ]; then
      cp "$tmp_file" "$cargo_file"
    fi
    rm -f "$tmp_file"
  done < <(find "$engine_dir" -name "Cargo.toml" -type f 2>/dev/null)

  return $had_unfixable
}
