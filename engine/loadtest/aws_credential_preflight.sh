#!/usr/bin/env bash
# AWS credential preflight — distinguishes the failure modes that look identical.
#
# Why this exists: on 2026-07-27 a paid measurement lane failed its AWS identity
# gate and reported `InvalidClientTokenId`. The real situation was TWO stacked
# faults, and the reported error named neither:
#
#   1. The secret file used bare `NAME=value` assignments with no `export`, so
#      `source .env.secret` set SHELL variables that never reached the `aws`
#      child process. Every repo's secret file had this (370 assignments, zero
#      exports). The CLI silently fell back to ~/.aws/config's browser-token
#      profile, whose session had expired — producing a misleading
#      "Your session has expired. Please reauthenticate using 'aws login'".
#   2. Underneath that, the long-lived IAM keys were themselves invalid.
#
# Fault 1 masked fault 2 for hours. This script separates them and prints the
# exact next action, so nobody re-diagnoses it from scratch.
#
# Usage:  bash aws_credential_preflight.sh [path-to-.env.secret]
# Exit:   0 usable · 1 not exported · 2 keys invalid · 3 expired session
#         4 secret file missing/unreadable · 5 aws CLI missing
#
# Never prints a secret value. Only names, prefixes, and lengths.

# NOTE: deliberately NOT `set -u`. This script sources an operator-maintained
# secret file whose lines may legitimately reference positional parameters
# (the flapjack engine secret has such a line), and `set -u` turns that into a
# spurious "unbound variable" failure that looks like a credential problem.
set -o pipefail

SECRET_FILE="${1:-${FLAPJACK_AWS_SECRET_FILE:-}}"
if [[ -z "$SECRET_FILE" ]]; then
  # Default to this repo's engine secret, resolved relative to the script so it
  # works from a worktree. Do NOT hardcode a machine-absolute path here.
  SECRET_FILE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/.secret/.env.secret"
fi

command -v aws >/dev/null 2>&1 || { echo "FAIL(5): aws CLI not on PATH"; exit 5; }

if [[ ! -s "$SECRET_FILE" ]]; then
  echo "FAIL(4): secret file missing or empty: $SECRET_FILE"
  echo "  Next: point at the SOURCE repo's secret file, never a worktree copy."
  exit 4
fi

# --- Fault 1: are the AWS vars actually exported? -----------------------------
# A bare `NAME=value` line is invisible to child processes. This is the check
# that was missing, and it is the reason the CLI fell through to a stale profile.
bare_aws=$(grep -cE '^[[:space:]]*AWS_(ACCESS_KEY_ID|SECRET_ACCESS_KEY|SESSION_TOKEN|DEFAULT_REGION|REGION)=' "$SECRET_FILE" || true)
if (( bare_aws > 0 )); then
  echo "FAIL(1): $bare_aws AWS assignment(s) in $SECRET_FILE lack 'export'."
  echo "  Sourcing this file sets shell vars that NEVER reach the aws process,"
  echo "  so the CLI silently falls back to ~/.aws/config and reports a"
  echo "  misleading expired-session error."
  echo "  Fix (idempotent, values untouched):"
  echo "    perl -i -pe 's/^([A-Za-z_][A-Za-z0-9_]*=)/export \$1/ unless /^\\s*(#|export\\s)/' \"$SECRET_FILE\""
  exit 1
fi

# shellcheck disable=SC1090
source "$SECRET_FILE"

# Prove the value reached a child process, not just this shell.
visible=$(python3 -c 'import os,sys; sys.stdout.write("1" if os.environ.get("AWS_ACCESS_KEY_ID") else "0")' 2>/dev/null || echo 0)
if [[ "$visible" != "1" ]]; then
  echo "FAIL(1): AWS_ACCESS_KEY_ID is not visible to child processes after sourcing."
  exit 1
fi

key_prefix="${AWS_ACCESS_KEY_ID:0:4}"
echo "credential shape: prefix=$key_prefix len=${#AWS_ACCESS_KEY_ID} region=${AWS_DEFAULT_REGION:-${AWS_REGION:-<unset>}}"
case "$key_prefix" in
  AKIA) echo "  AKIA = long-lived IAM user key (does not expire on its own)";;
  ASIA) echo "  ASIA = temporary STS credentials (WILL expire; expect churn)";;
  *)    echo "  WARNING: unrecognised access-key prefix";;
esac

# --- Faults 2/3: does the credential actually authenticate? -------------------
out=$(aws sts get-caller-identity --output json 2>&1)
rc=$?
if (( rc == 0 )); then
  acct=$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("Account","?"))' 2>/dev/null || echo '?')
  arn=$(printf '%s' "$out" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("Arn","?"))' 2>/dev/null || echo '?')
  echo "OK: authenticated. account=$acct arn=$arn"
  exit 0
fi

if printf '%s' "$out" | grep -q 'InvalidClientTokenId\|SignatureDoesNotMatch\|AuthFailure'; then
  echo "FAIL(2): credentials ARE being read, but AWS rejects them (key deleted, deactivated, or rotated)."
  echo "  This is the good failure mode — the plumbing works, the key does not."
  echo "  Next: mint a replacement long-lived IAM access key and write it into"
  echo "        $SECRET_FILE as 'export AWS_ACCESS_KEY_ID=...' / 'export AWS_SECRET_ACCESS_KEY=...',"
  echo "        then re-run this script. Prefer a dedicated IAM user scoped to"
  echo "        EC2 + S3 for scale runs over reusing a broad admin key."
  exit 2
fi

if printf '%s' "$out" | grep -qi 'session has expired\|aws login\|ExpiredToken\|TokenRefreshRequired'; then
  echo "FAIL(3): a browser/SSO session is being used and has expired."
  echo "  If the secret file's keys were exported correctly you should NOT see this;"
  echo "  seeing it means the CLI is still resolving ~/.aws/config ahead of the"
  echo "  environment. Check for AWS_PROFILE being set, or a [default] profile"
  echo "  with 'login_session'/'sso_session'."
  exit 3
fi

echo "FAIL: unclassified AWS error — treat as investigate, not as healthy:"
printf '  %s\n' "$out" | head -5
exit 2
