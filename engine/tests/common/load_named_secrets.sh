#!/usr/bin/env bash

set -euo pipefail

# Read a single KEY=value entry from an env-style file without executing it as shell.
read_secret_env_value() {
  local env_file="$1"
  local key="$2"
  local line=""
  local current_line=""
  local current_key=""
  local value=""

  [ -f "$env_file" ] || return 1
  [ -n "$key" ] || return 1
  [[ "$key" != *=* ]] || return 1

  # Compare parsed keys literally so caller-controlled metacharacters do not
  # alter which secret entry is selected.
  while IFS= read -r current_line || [ -n "$current_line" ]; do
    case "$current_line" in
      *=*)
        current_key="${current_line%%=*}"
        if [ "$current_key" = "$key" ]; then
          line="$current_line"
        fi
        ;;
    esac
  done < "$env_file"
  [ -n "$line" ] || return 1

  value="${line#*=}"
  if [[ "$value" == \"*\" && "$value" == *\" ]]; then
    value="${value:1:${#value}-2}"
  elif [[ "$value" == \'*\' && "$value" == *\' ]]; then
    value="${value:1:${#value}-2}"
  fi

  printf '%s\n' "$value"
}

export_if_unset() {
  local var_name="$1"
  local value="$2"

  [ -n "$value" ] || return 0
  if ! [[ "$var_name" =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]]; then
    echo "ERROR: invalid environment variable name '$var_name'" >&2
    return 1
  fi
  [ -z "${!var_name:-}" ] || return 0

  export "$var_name=$value"
}

load_named_secrets() {
  local env_file=""
  local key=""
  local value=""

  if [ "$#" -lt 2 ]; then
    echo "Usage: load_named_secrets <secret-file> KEY1 [KEY2 ...]" >&2
    return 2
  fi

  env_file="$1"
  shift

  if [ ! -f "$env_file" ]; then
    echo "ERROR: secret file not found: $env_file" >&2
    return 1
  fi

  for key in "$@"; do
    if ! value="$(read_secret_env_value "$env_file" "$key")" || [ -z "$value" ]; then
      echo "ERROR: missing required secret key '$key' in $env_file" >&2
      return 1
    fi
    export_if_unset "$key" "$value"
  done
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  load_named_secrets "$@"
fi
