#!/bin/sh

# TODO: Document extract_readme_curl_block.
require_readme_path() {
  if [ -z "${README_PATH:-}" ]; then
    echo "ERROR: README_PATH must be set before extracting README curl blocks" >&2
    return 1
  fi
  if [ ! -f "$README_PATH" ]; then
    echo "ERROR: README_PATH does not exist: $README_PATH" >&2
    return 1
  fi
}

extract_readme_curl_block() {
  path="$1"
  require_readme_path || return 1

  # Pull the exact multi-line curl block from README so smoke tests fail when docs drift.
  awk -v path="$path" '
    $0 ~ /^curl / && index($0, path) {
      capture = 1
    }
    capture {
      print
      line = $0
      quote_count += gsub(/\047/, "", line)
      if ($0 !~ /\\$/ && quote_count % 2 == 0) {
        found = 1
        exit
      }
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$README_PATH"
}

run_readme_curl() {
  local path="$1"
  local command_block=""
  if [ -z "${BASE:-}" ]; then
    echo "ERROR: BASE must be set before executing README curl blocks" >&2
    return 1
  fi
  if [ -z "${ADMIN_KEY:-}" ]; then
    echo "ERROR: ADMIN_KEY must be set before executing README curl blocks" >&2
    return 1
  fi

  command_block=$(extract_readme_curl_block "$path")
  # Treat README curl blocks as data, not executable shell, so docs drift checks
  # cannot smuggle extra shell commands into CI via command substitution, pipes,
  # or separators. Also reject curl flags that can read local files, write
  # artifacts, or target a host other than the expected README endpoint.
  COMMAND_BLOCK="$command_block" API_KEY="$ADMIN_KEY" BASE="$BASE" REQUEST_PATH="$path" python3 - <<'PY'
import os
import shlex
import subprocess
import sys
from urllib.parse import urlsplit

command_block = os.environ["COMMAND_BLOCK"]
base = os.environ["BASE"]
api_key = os.environ["API_KEY"]
request_path = os.environ["REQUEST_PATH"]

expanded = (
    command_block
    .replace("${API_KEY}", api_key)
    .replace("$API_KEY", api_key)
    .replace("http://localhost:7700", base)
)
expanded = expanded.replace("\\\n", " ")

for forbidden in ("`", "$(", ";", "&&", "||", "|", "<", ">"):
    if forbidden in expanded:
        raise SystemExit(f"Unsupported shell control token in README curl block: {forbidden}")

try:
    args = shlex.split(expanded, posix=True)
except ValueError as exc:
    raise SystemExit(f"Could not parse README curl block safely: {exc}") from exc

if not args or args[0] != "curl":
    raise SystemExit("README curl block must start with curl")

expected_url = f"{base.rstrip('/')}{request_path}"
sanitized_args = []
url_arg = None
i = 1
while i < len(args):
    arg = args[i]

    if arg in ("-s", "-S", "-sS", "-Ss", "--silent", "--show-error"):
        i += 1
        continue

    if arg in ("-X", "--request", "-H", "--header", "-d", "--data", "--data-raw", "--data-binary"):
        if i + 1 >= len(args):
            raise SystemExit(f"README curl block option {arg} is missing its value")
        value = args[i + 1]
        if arg in ("-d", "--data", "--data-raw", "--data-binary") and value.startswith("@"):
            raise SystemExit(
                f"README curl block option {arg} must use inline data, not local-file reads"
            )
        if arg in ("-H", "--header") and ("\n" in value or "\r" in value):
            raise SystemExit("README curl block headers must be single-line values")
        sanitized_args.extend((arg, value))
        i += 2
        continue

    if arg.startswith("-"):
        raise SystemExit(f"Unsupported curl option in README curl block: {arg}")

    if url_arg is not None:
        raise SystemExit("README curl block must target exactly one URL")
    url_arg = arg
    i += 1

if url_arg is None:
    raise SystemExit("README curl block must include a request URL")

parsed_url = urlsplit(url_arg)
if parsed_url.scheme not in ("http", "https"):
    raise SystemExit(f"README curl block URL must use http(s), got: {url_arg}")
if url_arg != expected_url:
    raise SystemExit(
        f"README curl block URL must be exactly {expected_url}, got: {url_arg}"
    )
sanitized_args.append(url_arg)

result = subprocess.run(
    ["curl", "-sS", "-w", r"\n%{http_code}", *sanitized_args],
    check=False,
    capture_output=True,
    text=True,
)
sys.stdout.write(result.stdout)
sys.stderr.write(result.stderr)
raise SystemExit(result.returncode)
PY
}
