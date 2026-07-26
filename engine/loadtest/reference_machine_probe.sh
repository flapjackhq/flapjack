#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
EVALUATOR="$SCRIPT_DIR/lib/reference_machine.mjs"

DATA_DIR=""
BINARY=""
GIT_SHA=""
OUTPUT=""
SNAPSHOT_FILE=""

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

usage() {
  cat <<'EOF'
Usage: reference_machine_probe.sh --data-dir <path> --binary <path> --git-sha <sha> --output <path>
       reference_machine_probe.sh --snapshot-file <path> --binary <path> --git-sha <sha> --output <path>
EOF
}

portable_sha256() {
  local file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    fail "sha256sum or shasum is required"
  fi
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --data-dir)
      DATA_DIR="${2:-}"
      shift 2
      ;;
    --binary)
      BINARY="${2:-}"
      shift 2
      ;;
    --git-sha)
      GIT_SHA="${2:-}"
      shift 2
      ;;
    --output)
      OUTPUT="${2:-}"
      shift 2
      ;;
    --snapshot-file)
      SNAPSHOT_FILE="${2:-}"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      fail "unknown argument: $1"
      ;;
  esac
done

[[ -x "$BINARY" ]] || fail "binary is missing or not executable: ${BINARY}"
[[ "$GIT_SHA" =~ ^[0-9a-f]{40}$ ]] || fail "--git-sha must be a full 40-character lowercase SHA"
[[ -n "$OUTPUT" ]] || fail "--output is required"
if [[ -n "$SNAPSHOT_FILE" ]]; then
  [[ -z "$DATA_DIR" ]] || fail "--snapshot-file and --data-dir are mutually exclusive"
  [[ -s "$SNAPSHOT_FILE" ]] || fail "snapshot file is missing or empty: ${SNAPSHOT_FILE}"
else
  [[ -d "$DATA_DIR" ]] || fail "data directory is missing: ${DATA_DIR}"
fi

probe_tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/flapjack_reference_probe.XXXXXX")"
trap 'rm -rf "$probe_tmp_dir"' EXIT
snapshot_path="$probe_tmp_dir/snapshot.json"
assessment_path="$probe_tmp_dir/assessment.json"

if [[ -n "$SNAPSHOT_FILE" ]]; then
  cp "$SNAPSHOT_FILE" "$snapshot_path"
else
  for command_name in curl jq findmnt lsblk lscpu free uname nvme node; do
    command -v "$command_name" >/dev/null 2>&1 || fail "required command is missing: ${command_name}"
  done

  imds_token="$(
    curl -fsS --connect-timeout 2 --max-time 5 \
      -X PUT \
      -H 'X-aws-ec2-metadata-token-ttl-seconds: 300' \
      http://169.254.169.254/latest/api/token
  )" || fail "IMDSv2 token request failed"
  [[ -n "$imds_token" ]] || fail "IMDSv2 returned an empty token"

  curl -fsS --connect-timeout 2 --max-time 5 \
    -H "X-aws-ec2-metadata-token: ${imds_token}" \
    http://169.254.169.254/latest/dynamic/instance-identity/document \
    > "$probe_tmp_dir/identity.json"
  findmnt -J -T "$DATA_DIR" -o SOURCE,TARGET,FSTYPE > "$probe_tmp_dir/findmnt.json"
  lsblk -J -b -o NAME,KNAME,PKNAME,TYPE,SIZE,MODEL,MOUNTPOINTS > "$probe_tmp_dir/lsblk.json"
  uname -a > "$probe_tmp_dir/uname.txt"
  lscpu > "$probe_tmp_dir/lscpu.txt"
  free -b > "$probe_tmp_dir/free.txt"
  nvme list > "$probe_tmp_dir/nvme.txt"

  jq -n \
    --arg data_dir "$DATA_DIR" \
    --slurpfile identity "$probe_tmp_dir/identity.json" \
    --slurpfile findmnt "$probe_tmp_dir/findmnt.json" \
    --slurpfile lsblk "$probe_tmp_dir/lsblk.json" \
    --rawfile uname "$probe_tmp_dir/uname.txt" \
    --rawfile lscpu "$probe_tmp_dir/lscpu.txt" \
    --rawfile free "$probe_tmp_dir/free.txt" \
    --rawfile nvme "$probe_tmp_dir/nvme.txt" \
    '{
      dataDir: $data_dir,
      identityDocument: $identity[0],
      findmnt: $findmnt[0],
      lsblk: $lsblk[0],
      system: {
        uname: $uname,
        lscpu: $lscpu,
        free: $free,
        nvme: $nvme
      }
    }' > "$snapshot_path"
fi

evaluator_exit=0
node "$EVALUATOR" --input-file "$snapshot_path" > "$assessment_path" || evaluator_exit=$?
binary_sha256="$(portable_sha256 "$BINARY")"
output_tmp="${OUTPUT}.tmp.$$"
mkdir -p "$(dirname "$OUTPUT")"
jq -n \
  --arg captured_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  --arg git_sha "$GIT_SHA" \
  --arg binary_path "$BINARY" \
  --arg binary_sha256 "$binary_sha256" \
  --slurpfile assessment "$assessment_path" \
  --slurpfile snapshot "$snapshot_path" \
  '{
    schemaVersion: 1,
    capturedAt: $captured_at,
    gitSha: $git_sha,
    binaryPath: $binary_path,
    binarySha256: $binary_sha256,
    assessment: $assessment[0],
    snapshot: $snapshot[0]
  }' > "$output_tmp"
jq -e '
  .schemaVersion == 1 and
  (.gitSha | test("^[0-9a-f]{40}$")) and
  (.binarySha256 | test("^[0-9a-f]{64}$")) and
  (.assessment.verdict == "GO" or .assessment.verdict == "INVALID")
' "$output_tmp" >/dev/null || fail "reference receipt failed its own schema check"
mv "$output_tmp" "$OUTPUT"

if [[ "$evaluator_exit" -ne 0 ]]; then
  fail "reference-machine locality verdict is INVALID; evidence saved to ${OUTPUT}"
fi
echo "PASS: reference machine and local instance-store NVMe verified; receipt=${OUTPUT}"
