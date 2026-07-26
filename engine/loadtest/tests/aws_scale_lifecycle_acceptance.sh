#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LOADTEST_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RUNBOOK="$LOADTEST_DIR/AWS_SCALE_CEILING_RUNBOOK.md"
CONTRACT="$LOADTEST_DIR/SCALE_CEILING_CONTRACT.md"

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

[[ -s "$RUNBOOK" ]] || fail "AWS scale-ceiling runbook is missing or empty"

grep -Fq 'aws sts get-caller-identity' "$RUNBOOK" || {
  fail "runbook does not require fresh AWS identity verification"
}
grep -Fq 'describe-instances' "$RUNBOOK" || {
  fail "runbook does not resolve instance state and address from fresh AWS CLI output"
}
tr '\n' ' ' < "$RUNBOOK" | grep -Eq 'local +instance-store +NVMe' || {
  fail "runbook does not require local NVMe locality proof"
}
grep -Fq 'Stop, hibernate, and terminate erase instance-store data' "$RUNBOOK" || {
  fail "runbook does not state the destructive instance-store lifecycle rule"
}
grep -Fq 'A reboot preserves instance-store data' "$RUNBOOK" || {
  fail "runbook does not distinguish reboot from stop"
}
grep -Eq 'S3 or EBS' "$RUNBOOK" || {
  fail "runbook does not name a durable off-instance evidence destination"
}
grep -Fq 'verify the copied checksums before any stop or termination' "$RUNBOOK" || {
  fail "runbook does not require verified durable evidence before destructive lifecycle actions"
}
grep -Fq 'AWS_SCALE_CEILING_RUNBOOK.md' "$CONTRACT" || {
  fail "frozen contract does not route operators to the lifecycle owner"
}

if grep -Eiq 'stop (the )?instance (to|for|when) paus|pause by stopping' "$RUNBOOK"; then
  fail "runbook recommends a destructive stop as a pause mechanism"
fi

echo "PASS: AWS scale-ceiling lifecycle instructions fail closed around instance-store data"
