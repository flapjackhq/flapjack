#!/bin/sh
# tests/test_install.sh — Unit and integration tests for install.sh
#
# Usage:
#   ./tests/test_install.sh           # Run all tests
#   GITHUB_TOKEN=xxx ./tests/test_install.sh  # Include private-repo download tests
#
# Tests are split into:
#   1. Unit tests (no network) — validate platform detection, PATH logic, etc.
#   2. Integration tests (network) — validate actual downloads (requires GITHUB_TOKEN for private repos)

set -eu

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
INSTALL_SCRIPT="${REPO_DIR}/install.sh"
DEBBIE_CONFIG="${REPO_DIR}/../.debbie.toml"

# ── Test Helpers ─────────────────────────────────────────────────────────────

TESTS_RUN=0
TESTS_PASSED=0
TESTS_FAILED=0

pass() {
  TESTS_PASSED=$((TESTS_PASSED + 1))
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "  \033[0;32m✓\033[0m %s\n" "$1"
}

fail() {
  TESTS_FAILED=$((TESTS_FAILED + 1))
  TESTS_RUN=$((TESTS_RUN + 1))
  printf "  \033[0;31m✗\033[0m %s\n" "$1"
  if [ -n "${2:-}" ]; then
    printf "    %s\n" "$2"
  fi
}

section() {
  printf "\n\033[1m%s\033[0m\n" "$1"
}

installer_target_triples() {
  sed -n 's/.*target="\([A-Za-z0-9._-]*\)".*/\1/p' "$1" | LC_ALL=C sort -u
}

# ── Unit Tests ───────────────────────────────────────────────────────────────

section "Install Script Syntax & Structure"

# Test 1: Script is valid POSIX shell
if sh -n "$INSTALL_SCRIPT" 2>/dev/null; then
  pass "install.sh passes POSIX shell syntax check"
else
  fail "install.sh has shell syntax errors"
fi

# Test 2: Script starts with proper shebang
first_line=$(head -1 "$INSTALL_SCRIPT")
if [ "$first_line" = "#!/bin/sh" ]; then
  pass "Shebang is #!/bin/sh (POSIX compatible)"
else
  fail "Shebang should be #!/bin/sh, got: $first_line"
fi

# Test 3: set -eu is present (fail-fast)
if grep -q '^set -eu' "$INSTALL_SCRIPT"; then
  pass "set -eu present (fail-fast mode)"
else
  fail "set -eu not found — script won't fail on errors"
fi

# Test 4: Script is executable
if [ -x "$INSTALL_SCRIPT" ]; then
  pass "install.sh is executable"
else
  fail "install.sh is not executable"
fi

# ── Configuration Defaults ───────────────────────────────────────────────────
section "Configuration Defaults"

# Test 5: REPO default matches the environment (staging vs prod)
# In staging CI: expect gridl-staging/flapjack
# In prod CI: expect flapjackhq/flapjack
# Locally: accept either (dev repo has prod default, but staging sync rewrites it)
if [ -n "${GITHUB_REPOSITORY:-}" ]; then
  case "$GITHUB_REPOSITORY" in
    gridl-staging/flapjack)
      if grep -q 'REPO=.*gridl-staging/flapjack' "$INSTALL_SCRIPT"; then
        pass "Default REPO is gridl-staging/flapjack (staging environment)"
      else
        fail "Default REPO should be gridl-staging/flapjack in staging environment"
      fi
      ;;
    flapjackhq/flapjack)
      if grep -q 'REPO=.*flapjackhq/flapjack' "$INSTALL_SCRIPT" && ! grep -q 'gridl-staging' "$INSTALL_SCRIPT"; then
        pass "Default REPO is flapjackhq/flapjack (production environment)"
      else
        fail "Default REPO should be flapjackhq/flapjack in production environment"
      fi
      ;;
    *)
      fail "Unexpected GITHUB_REPOSITORY: $GITHUB_REPOSITORY"
      ;;
  esac
else
  # Local dev: accept either repo (dev has prod default, staging sync rewrites it)
  if grep -q 'REPO=.*flapjackhq/flapjack\|gridl-staging/flapjack' "$INSTALL_SCRIPT"; then
    pass "Default REPO is set (local dev environment)"
  else
    fail "Default REPO should be flapjackhq/flapjack or gridl-staging/flapjack"
  fi
fi

# Test 6: BINARY_NAME is flapjack
if grep -q 'BINARY_NAME="flapjack"' "$INSTALL_SCRIPT"; then
  pass "BINARY_NAME is flapjack"
else
  fail "BINARY_NAME is not flapjack"
fi

# Test 7: Install dir defaults to ~/.flapjack/bin
if grep -q 'INSTALL_DIR=.*HOME/.flapjack.*/bin' "$INSTALL_SCRIPT"; then
  pass "Default install dir is ~/.flapjack/bin"
else
  fail "Default install dir not found"
fi

# Tests 8-9 verify private-repo debbie sync surface; they only run in the dev
# mirror where .debbie.toml lives. The public mirror intentionally does not
# carry .debbie.toml, so skip these assertions when the file is absent rather
# than coupling public install-behavior tests to private sync config.
if [ -f "$DEBBIE_CONFIG" ]; then
  # Test 8: Legacy prod token is retained for rewrite compatibility
  if grep -q 'github_legacy = "gridlhq/flapjack"' "$DEBBIE_CONFIG"; then
    pass "Legacy prod token retained under identity.prod.github_legacy"
  else
    fail "Missing identity.prod.github_legacy legacy token for rewrite compatibility"
  fi

  # Test 9: Rewrite scope remains single-source for install/workflow identity rewrites
  if grep -q '"engine/install.sh"' "$DEBBIE_CONFIG" \
    && grep -q '".github/workflows/docker.yml"' "$DEBBIE_CONFIG" \
    && grep -q '".github/workflows/release.yml"' "$DEBBIE_CONFIG"; then
    pass "Rewrite scope includes install/workflow identity-owned files"
  else
    fail "Rewrite scope missing install/workflow identity-owned files"
  fi
fi

# ── Platform Detection ───────────────────────────────────────────────────────
section "Platform Detection"

# Test 10: All four Rust target triples are present
for target in "x86_64-unknown-linux-musl" "aarch64-unknown-linux-musl" "x86_64-apple-darwin" "aarch64-apple-darwin"; do
  if grep -q "$target" "$INSTALL_SCRIPT"; then
    pass "Target triple present: $target"
  else
    fail "Target triple missing: $target"
  fi
done

# Test 11: Rosetta 2 detection exists
if grep -q "sysctl.proc_translated" "$INSTALL_SCRIPT"; then
  pass "Rosetta 2 detection present"
else
  fail "Rosetta 2 detection missing"
fi

# Test 12: Windows detection with helpful error
if grep -q "MINGW\|MSYS\|CYGWIN" "$INSTALL_SCRIPT"; then
  pass "Windows detection present (with error message)"
else
  fail "Windows detection missing"
fi

# ── Download Tool Detection ──────────────────────────────────────────────────
section "Download Tool Detection"

# Test 13: curl support
if grep -q 'command -v curl' "$INSTALL_SCRIPT"; then
  pass "curl detection present"
else
  fail "curl detection missing"
fi

# Test 14: wget fallback
if grep -q 'command -v wget' "$INSTALL_SCRIPT"; then
  pass "wget fallback present"
else
  fail "wget fallback missing"
fi

# ── Version Resolution ───────────────────────────────────────────────────────

section "Version Resolution"

# Test 15: FLAPJACK_VERSION env var support
if grep -q 'FLAPJACK_VERSION' "$INSTALL_SCRIPT"; then
  pass "FLAPJACK_VERSION env var support"
else
  fail "FLAPJACK_VERSION env var not supported"
fi

# Test 16: CLI argument version pinning
if grep -q '${1:-}' "$INSTALL_SCRIPT" || grep -q '"$1"' "$INSTALL_SCRIPT"; then
  pass "CLI argument version pinning supported"
else
  fail "CLI argument version pinning not found"
fi

# Test 17: GitHub API latest release detection
if grep -q 'api.github.com/repos.*releases/latest' "$INSTALL_SCRIPT"; then
  pass "GitHub API latest release detection"
else
  fail "GitHub API latest release detection missing"
fi

# ── Security Features ────────────────────────────────────────────────────────
section "Security Features"

# Test 18: SHA256 checksum verification (with -c flag for actual file verification)
if grep -q 'shasum -a 256 -c' "$INSTALL_SCRIPT" && grep -q 'sha256sum -c' "$INSTALL_SCRIPT"; then
  pass "SHA256 checksum verification (shasum -c + sha256sum -c)"
else
  fail "SHA256 checksum verification incomplete (missing -c flag)"
fi

# Test 19: Checksum failure exits with error code
if grep -A 3 'Checksum verification FAILED' "$INSTALL_SCRIPT" | grep -q 'exit'; then
  pass "Checksum failure causes exit"
else
  fail "Checksum failure message found but no exit statement"
fi

# Test 20: GITHUB_TOKEN support for private repos
if grep -q 'GITHUB_TOKEN' "$INSTALL_SCRIPT"; then
  pass "GITHUB_TOKEN support for private repos"
else
  fail "GITHUB_TOKEN support missing"
fi

# Test 21: GitHub API asset download (for private repos)
if grep -q 'application/octet-stream' "$INSTALL_SCRIPT"; then
  pass "GitHub API asset download (Accept: application/octet-stream)"
else
  fail "GitHub API asset download not implemented"
fi

# Test 21b: Missing checksum file fails closed
if grep -q 'No checksum file available' "$INSTALL_SCRIPT" && grep -q 'refusing unverifiable install' "$INSTALL_SCRIPT"; then
  pass "Missing checksum file is a hard failure"
else
  fail "Missing checksum file does not fail closed"
fi

# Test 21c: Missing checksum tool fails closed
if grep -q 'No checksum tool found' "$INSTALL_SCRIPT" && grep -q 'refusing unverifiable install' "$INSTALL_SCRIPT"; then
  pass "Missing checksum tool is a hard failure"
else
  fail "Missing checksum tool does not fail closed"
fi

# ── PATH Management ──────────────────────────────────────────────────────────

section "PATH Management"

# Test 22: Bash profile update
if grep -q '.bashrc' "$INSTALL_SCRIPT" && grep -q '.bash_profile' "$INSTALL_SCRIPT"; then
  pass "Bash profile update (.bashrc/.bash_profile)"
else
  fail "Bash profile update incomplete"
fi

# Test 23: Zsh profile update
if grep -q '.zshrc' "$INSTALL_SCRIPT"; then
  pass "Zsh profile update (.zshrc)"
else
  fail "Zsh profile update missing"
fi

# Test 24: Fish config update
if grep -q 'config.fish' "$INSTALL_SCRIPT"; then
  pass "Fish config update"
else
  fail "Fish config update missing"
fi

# Test 25: NO_MODIFY_PATH support
if grep -q 'NO_MODIFY_PATH' "$INSTALL_SCRIPT"; then
  pass "NO_MODIFY_PATH env var supported"
else
  fail "NO_MODIFY_PATH not supported"
fi

# Test 26: Idempotent PATH update (won't add duplicate)
if grep -q 'grep -qF.*INSTALL_DIR' "$INSTALL_SCRIPT"; then
  pass "Idempotent PATH update (checks for existing entry)"
else
  fail "PATH update may not be idempotent"
fi

# Test 27: Permission-denied handling for shell profiles
if grep -q 'permission denied' "$INSTALL_SCRIPT"; then
  pass "Permission-denied handling for shell profiles"
else
  fail "No permission-denied handling for shell profiles"
fi

# ── Environment Variable Overrides ───────────────────────────────────────────

section "Environment Variable Overrides"

for var in FLAPJACK_INSTALL FLAPJACK_REPO FLAPJACK_VERSION GITHUB_TOKEN NO_MODIFY_PATH; do
  if grep -q "$var" "$INSTALL_SCRIPT"; then
    pass "Env var override: $var"
  else
    fail "Env var override missing: $var"
  fi
done

# ── Functional PATH Setup Tests (no network, sandboxed HOME) ────────────────

section "PATH Setup (functional)"

# Helper: extract setup_path and dependencies from install.sh, run in a sandboxed HOME
run_setup_path() {
  _fake_home="$1"
  _install_dir="$2"

  # Run a minimal version of the installer's setup_path in a sandboxed env.
  # We source the relevant functions then call setup_path().
  HOME="$_fake_home" INSTALL_DIR="$_install_dir" NO_MODIFY_PATH=0 \
    sh -c '
    HOME='"'$_fake_home'"'
    INSTALL_DIR='"'$_install_dir'"'
    setup_colors() { RED="" GREEN="" YELLOW="" BLUE="" BOLD="" NC=""; }
    info()  { printf "info  %s\n" "$1"; }
    warn()  { printf "warn  %s\n" "$1"; }

    '"$(sed -n '/^setup_path()/,/^}/p' "$INSTALL_SCRIPT")"'

    setup_colors
    setup_path
  ' 2>&1
}

# Test: Updates .bashrc when it exists
_td=$(mktemp -d)
mkdir -p "$_td"
touch "$_td/.bashrc"
run_setup_path "$_td" "/fake/bin" > /dev/null
if grep -qF "/fake/bin" "$_td/.bashrc" 2>/dev/null; then
  pass "PATH setup: updates .bashrc when it exists"
else
  fail "PATH setup: did not update .bashrc"
fi
rm -rf "$_td"

# Test: Updates .zshrc when it exists
_td=$(mktemp -d)
touch "$_td/.zshrc"
run_setup_path "$_td" "/fake/bin" > /dev/null
if grep -qF "/fake/bin" "$_td/.zshrc" 2>/dev/null; then
  pass "PATH setup: updates .zshrc when it exists"
else
  fail "PATH setup: did not update .zshrc"
fi
rm -rf "$_td"

# Test: Updates fish config.fish when dir exists
_td=$(mktemp -d)
mkdir -p "$_td/.config/fish"
touch "$_td/.config/fish/config.fish"
run_setup_path "$_td" "/fake/bin" > /dev/null
if grep -qF "/fake/bin" "$_td/.config/fish/config.fish" 2>/dev/null; then
  pass "PATH setup: updates fish config.fish when dir exists"
else
  fail "PATH setup: did not update fish config.fish"
fi
rm -rf "$_td"

# Test: Updates ALL profiles when multiple exist (the key bug fix)
_td=$(mktemp -d)
touch "$_td/.bashrc"
touch "$_td/.zshrc"
mkdir -p "$_td/.config/fish"
touch "$_td/.config/fish/config.fish"
run_setup_path "$_td" "/fake/bin" > /dev/null
_all_updated=true
grep -qF "/fake/bin" "$_td/.bashrc" 2>/dev/null || _all_updated=false
grep -qF "/fake/bin" "$_td/.zshrc" 2>/dev/null || _all_updated=false
grep -qF "/fake/bin" "$_td/.config/fish/config.fish" 2>/dev/null || _all_updated=false
if [ "$_all_updated" = "true" ]; then
  pass "PATH setup: updates ALL shell profiles (bash + zsh + fish)"
else
  fail "PATH setup: did not update all profiles" \
    "bash=$(grep -c '/fake/bin' "$_td/.bashrc" 2>/dev/null) zsh=$(grep -c '/fake/bin' "$_td/.zshrc" 2>/dev/null) fish=$(grep -c '/fake/bin' "$_td/.config/fish/config.fish" 2>/dev/null)"
fi
rm -rf "$_td"

# Test: Fish gets fish-specific syntax (set -gx), not export
_td=$(mktemp -d)
mkdir -p "$_td/.config/fish"
touch "$_td/.config/fish/config.fish"
run_setup_path "$_td" "/fake/bin" > /dev/null
if grep -q "set -gx PATH" "$_td/.config/fish/config.fish" 2>/dev/null; then
  pass "PATH setup: fish config uses 'set -gx PATH' syntax"
else
  fail "PATH setup: fish config should use 'set -gx PATH', not 'export'"
fi
rm -rf "$_td"

# Test: Idempotent — running twice doesn't duplicate
_td=$(mktemp -d)
touch "$_td/.bashrc"
touch "$_td/.zshrc"
run_setup_path "$_td" "/fake/bin" > /dev/null
run_setup_path "$_td" "/fake/bin" > /dev/null
_bash_count=$(grep -c '/fake/bin' "$_td/.bashrc" 2>/dev/null || echo 0)
_zsh_count=$(grep -c '/fake/bin' "$_td/.zshrc" 2>/dev/null || echo 0)
if [ "$_bash_count" -eq 1 ] && [ "$_zsh_count" -eq 1 ]; then
  pass "PATH setup: idempotent (no duplicate entries on second run)"
else
  fail "PATH setup: duplicate entries on second run (bash=$_bash_count zsh=$_zsh_count)"
fi
rm -rf "$_td"

# Test: Permission-denied on one profile doesn't block others
_td=$(mktemp -d)
touch "$_td/.zshrc"
chmod 444 "$_td/.zshrc"  # read-only → permission denied
mkdir -p "$_td/.config/fish"
touch "$_td/.config/fish/config.fish"
run_setup_path "$_td" "/fake/bin" > /dev/null
if grep -qF "/fake/bin" "$_td/.config/fish/config.fish" 2>/dev/null; then
  pass "PATH setup: permission-denied on .zshrc still updates fish config"
else
  fail "PATH setup: permission-denied on .zshrc blocked fish config update"
fi
chmod 644 "$_td/.zshrc"  # restore for cleanup
rm -rf "$_td"

# Test: NO_MODIFY_PATH=1 skips all profile updates
_td=$(mktemp -d)
touch "$_td/.bashrc"
touch "$_td/.zshrc"
HOME="$_td" INSTALL_DIR="/fake/bin" NO_MODIFY_PATH=1 \
  sh -c '
  HOME='"'$_td'"'
  INSTALL_DIR="/fake/bin"
  NO_MODIFY_PATH=1
  setup_colors() { RED="" GREEN="" YELLOW="" BLUE="" BOLD="" NC=""; }
  info()  { :; }
  warn()  { :; }
  '"$(sed -n '/^setup_path()/,/^}/p' "$INSTALL_SCRIPT")"'
  setup_colors
  setup_path
' 2>/dev/null
if ! grep -qF "/fake/bin" "$_td/.bashrc" 2>/dev/null && ! grep -qF "/fake/bin" "$_td/.zshrc" 2>/dev/null; then
  pass "PATH setup: NO_MODIFY_PATH=1 skips all profile updates"
else
  fail "PATH setup: NO_MODIFY_PATH=1 did not prevent profile modification"
fi
rm -rf "$_td"

# ── Vanity URL Tests (requires network) ──────────────────────────────────────

section "Vanity URL Tests"

# Test: install.flapjack.foo redirects to a URL that serves the install script
install_url_content=$(curl -fsSL https://install.flapjack.foo 2>/dev/null | head -1)
if [ "$install_url_content" = "#!/bin/sh" ]; then
  pass "install.flapjack.foo serves install.sh (shebang verified)"
else
  fail "install.flapjack.foo did not serve install.sh (got: $install_url_content)"
fi

# Test: install.flapjack.foo redirect target matches expected path
install_redirect=$(curl -sI https://install.flapjack.foo 2>/dev/null | grep -i '^location:' | tr -d '\r')
if echo "$install_redirect" | grep -Eq 'raw\.githubusercontent\.com/(flapjackhq|griddlehq|gridlhq)/flapjack/main/engine/install\.sh'; then
  pass "install.flapjack.foo redirects to canonical or legacy GitHub path"
else
  fail "install.flapjack.foo redirect target unexpected: $install_redirect"
fi

# ── Release API Reachability (public, no token needed) ────────────────────────

section "Release API Reachability"

# Extract the default REPO from install.sh
default_repo=$(grep 'FLAPJACK_REPO:-' "$INSTALL_SCRIPT" | sed 's/.*FLAPJACK_REPO:-//;s/}.*//;s/"//g')

# Test: GitHub API /releases/latest returns a valid tag_name
if [ -n "${GITHUB_TOKEN:-}" ]; then
  api_resp=$(curl -fsSL -H "Authorization: token ${GITHUB_TOKEN}" "https://api.github.com/repos/${default_repo}/releases/latest" 2>&1) || true
else
  api_resp=$(curl -fsSL "https://api.github.com/repos/${default_repo}/releases/latest" 2>&1) || true
fi
if echo "$api_resp" | grep -q '"tag_name"'; then
  latest_tag=$(echo "$api_resp" | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
  pass "GitHub API releases/latest returns tag: ${latest_tag}"
else
  fail "GitHub API releases/latest for ${default_repo} failed (404 or no tag_name)" \
    "Got: $(echo "$api_resp" | head -3)"
fi

# Test: At least one release asset exists for a common target
if echo "$api_resp" | grep -q 'flapjack-.*\.tar\.gz'; then
  pass "Release has .tar.gz assets"
else
  fail "No .tar.gz assets found in latest release"
fi

# Test: the published release is COMPLETE for every target install.sh can resolve.
#
# Why this is separate from the test above: that one passes if a single tarball
# exists anywhere in the release. A release that published only linux-x86_64 —
# or that lost an asset to a partial upload — would satisfy it while leaving
# every macOS and Windows user with a 404 from the documented quickstart. The
# install path is `curl -fsSL https://install.flapjack.foo | sh`, which resolves
# /releases/latest and then downloads `flapjack-<triple>.<ext>` plus its
# `.sha256`, so an incomplete release is a customer-visible outage, not a
# packaging detail.
#
# Deliberately NOT asserted here: that the tag matches the version in
# engine/Cargo.toml. This file runs in mirror CI (.github/workflows/
# test-installer.yml, nightly.yml), and a tree-vs-published equality check is red
# by construction for the whole window between the version bump and the
# release.yml publish — i.e. it would block the very release it is meant to
# protect. That assertion belongs in the release lane's post-publish
# verification, where "the tag I just cut is what customers now resolve" is the
# correct and non-blocking question.
#
# Targets are read from install.sh rather than restated, so adding another
# supported platform automatically makes its release assets mandatory here.
missing_assets=""
checked_targets=0
installer_targets=$(installer_target_triples "$INSTALL_SCRIPT")
for _triple in $installer_targets; do
  checked_targets=$((checked_targets + 1))
  for _name in "flapjack-${_triple}.tar.gz" "flapjack-${_triple}.tar.gz.sha256"; do
    echo "$api_resp" | grep -q "\"name\": *\"${_name}\"" || missing_assets="${missing_assets} ${_name}"
  done
done

if [ "$checked_targets" -eq 0 ]; then
  # Fail closed: zero targets means either install.sh stopped naming triples or
  # the API response never arrived. Passing here would be a vacuous green.
  fail "Release asset completeness could not be evaluated" \
    "No installer targets were resolved from $INSTALL_SCRIPT; refusing to report healthy"
elif [ -n "$missing_assets" ]; then
  fail "Latest release is missing published assets for ${checked_targets} installer target(s)" \
    "Missing:${missing_assets}"
else
  pass "Latest release publishes every asset install.sh can request (${checked_targets} targets, binary + sha256)"
fi

# Test: the release a customer resolves is actually installable — not a draft,
# not a prerelease. `/releases/latest` already excludes drafts, so a draft here
# means the API shape changed; prerelease is excluded by GitHub too, but both are
# asserted because the installer treats whatever it gets as shippable.
if echo "$api_resp" | grep -q '"draft": *true'; then
  fail "Latest release is a draft; the documented quickstart would resolve an unpublished release"
elif echo "$api_resp" | grep -q '"prerelease": *true'; then
  fail "Latest release is a prerelease; the documented quickstart would resolve a prerelease to new users"
else
  pass "Latest release is published and not a prerelease"
fi

# ── Integration Tests (requires network + GITHUB_TOKEN for private repos) ───

section "Integration Tests"

if [ -z "${GITHUB_TOKEN:-}" ]; then
  # Try to get token from gh CLI
  if command -v gh >/dev/null 2>&1; then
    GITHUB_TOKEN=$(gh auth token 2>/dev/null || true)
  fi
fi

if [ -z "${GITHUB_TOKEN:-}" ]; then
  printf "  \033[1;33mSkipped\033[0m (set GITHUB_TOKEN or install gh CLI for integration tests)\n"
else
  # Resolve a pinned version dynamically (use the latest release tag)
  PIN_VERSION=$(curl -fsSL -H "Authorization: token ${GITHUB_TOKEN}" \
    "https://api.github.com/repos/${default_repo}/releases/latest" 2>/dev/null \
    | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": *"//;s/".*//')
  if [ -z "$PIN_VERSION" ]; then
    PIN_VERSION="v0.0.16-beta"
  fi

  # Test 31: Full install with version pinning
  test_dir=$(mktemp -d)
  trap_cleanup() { rm -rf "$test_dir"; }
  trap trap_cleanup EXIT

  if NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$test_dir" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" "$PIN_VERSION" 2>&1 | grep -q "installed successfully"; then
    pass "Full install with version pinning (${PIN_VERSION})"
  else
    fail "Full install with version pinning failed (${PIN_VERSION})"
  fi

  # Test 32: Binary exists and is executable
  if [ -x "$test_dir/bin/flapjack" ]; then
    pass "Binary is executable at expected path"
  else
    fail "Binary not found or not executable at $test_dir/bin/flapjack"
  fi

  # Test 33: Binary runs (check help)
  if "$test_dir/bin/flapjack" --help >/dev/null 2>&1; then
    pass "Binary runs successfully (--help)"
  else
    fail "Binary failed to run --help (may be incompatible with this platform)"
  fi

  # Test 34: Latest version auto-detection
  test_dir2=$(mktemp -d)
  if NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$test_dir2" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" 2>&1 | grep -q "installed successfully"; then
    pass "Latest version auto-detection works"
  else
    fail "Latest version auto-detection failed"
  fi
  rm -rf "$test_dir2"

  # Test 35: Idempotent reinstall (run twice, check no errors)
  test_dir3=$(mktemp -d)
  output1=$(NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$test_dir3" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" "$PIN_VERSION" 2>&1 || true)
  output2=$(NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$test_dir3" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" "$PIN_VERSION" 2>&1 || true)
  if echo "$output2" | grep -q "installed successfully"; then
    pass "Idempotent reinstall works"
  else
    fail "Idempotent reinstall failed" "$output2"
  fi
  rm -rf "$test_dir3"

  # Test 36: Custom install directory
  test_dir4=$(mktemp -d)/custom/path
  if NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$(dirname "$test_dir4")" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" "$PIN_VERSION" 2>&1 | grep -q "installed successfully"; then
    pass "Custom install directory (FLAPJACK_INSTALL)"
  else
    fail "Custom install directory failed"
  fi
  rm -rf "$(dirname "$(dirname "$test_dir4")")"

  # Test 37: Invalid version fails gracefully
  test_dir5=$(mktemp -d)
  if NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$test_dir5" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" v999.999.999 2>&1 | grep -qi "error\|fail\|not found\|404"; then
    pass "Invalid version fails gracefully"
  else
    fail "Invalid version did not produce error"
  fi
  rm -rf "$test_dir5"
fi

# ── Quickstart API Tests (requires network + binary) ─────────────────────────

section "Quickstart API Tests"

if [ -z "${GITHUB_TOKEN:-}" ]; then
  printf "  \033[1;33mSkipped\033[0m (requires GITHUB_TOKEN for binary download)\n"
else
  qs_dir=$(mktemp -d)
  qs_data=$(mktemp -d)
  QS_PORT=7711
  QS_SERVER_PID=""

  qs_cleanup() {
    if [ -n "$QS_SERVER_PID" ] && kill -0 "$QS_SERVER_PID" 2>/dev/null; then
      kill "$QS_SERVER_PID" 2>/dev/null || true
      wait "$QS_SERVER_PID" 2>/dev/null || true
    fi
    rm -rf "$qs_dir" "$qs_data"
  }
  trap qs_cleanup EXIT

  # Install latest version
  qs_install_output=$(NO_MODIFY_PATH=1 FLAPJACK_INSTALL="$qs_dir" GITHUB_TOKEN="$GITHUB_TOKEN" sh "$INSTALL_SCRIPT" 2>&1)
  if echo "$qs_install_output" | grep -q "installed successfully" && [ -x "$qs_dir/bin/flapjack" ]; then
    pass "Quickstart: installed latest binary"
  else
    fail "Quickstart: failed to install binary" "$qs_install_output"
  fi

  # Mirror the documented quickstart UX (README §Quickstart):
  #   1. Start the binary with no auth env vars.
  #   2. On first boot, server auto-generates an admin key at data/.admin_key.
  #   3. Read that key and pass it as X-Algolia-API-Key in all /1/* calls.
  # No --no-auth, no FLAPJACK_ADMIN_KEY override — exercises the same path a
  # first-time user follows.
  if [ -x "$qs_dir/bin/flapjack" ]; then
    FLAPJACK_BIND_ADDR="127.0.0.1:${QS_PORT}" FLAPJACK_DATA_DIR="$qs_data" \
      "$qs_dir/bin/flapjack" >/dev/null 2>&1 &
    QS_SERVER_PID=$!

    # Wait for both the server to respond AND the auto-generated .admin_key
    # to land on disk (the key is written during initialize_key_store, before
    # /health serves, but assert both to be defensive against startup ordering
    # changes in future binaries).
    qs_ready=false
    for _i in $(seq 1 30); do
      if curl -sf "http://localhost:${QS_PORT}/health" >/dev/null 2>&1 \
        && [ -s "$qs_data/.admin_key" ]; then
        qs_ready=true
        break
      fi
      sleep 0.5
    done

    if [ "$qs_ready" = "true" ]; then
      pass "Install test: server started on port ${QS_PORT} (auto-generated admin key)"
    else
      fail "Install test: server failed to start within 15s or did not write data/.admin_key"
    fi

    # Read the auto-generated key and assemble auth headers per the documented
    # Algolia-compatible header pair.
    QS_ADMIN_KEY=""
    if [ -s "$qs_data/.admin_key" ]; then
      QS_ADMIN_KEY=$(cat "$qs_data/.admin_key")
    fi
    QS_BASE="http://localhost:${QS_PORT}/1/indexes"
    QS_AUTH="-H X-Algolia-API-Key:${QS_ADMIN_KEY} -H X-Algolia-Application-Id:flapjack"

    if [ "$qs_ready" = "true" ]; then
      # POST documents via /1/ batch endpoint
      post_resp=$(curl -s -X POST "${QS_BASE}/movies/batch" \
        $QS_AUTH \
        -H "Content-Type: application/json" \
        -d '{"requests":[{"action":"addObject","body":{"objectID":"1","title":"The Matrix","year":1999}},{"action":"addObject","body":{"objectID":"2","title":"Inception","year":2010}}]}' 2>&1)
      if echo "$post_resp" | grep -q "taskID\|objectIDs"; then
        pass "Install test: POST documents returns taskID"
      else
        fail "Install test: POST documents failed" "$post_resp"
      fi

      # Wait for indexing
      sleep 2

      # Search — exact match
      search_resp=$(curl -s -X POST "${QS_BASE}/movies/query" \
        $QS_AUTH \
        -H "Content-Type: application/json" \
        -d '{"query":"matrix"}' 2>&1)
      if echo "$search_resp" | grep -q '"The Matrix"'; then
        pass "Install test: search for 'matrix' returns The Matrix"
      else
        fail "Install test: search for 'matrix' did not return expected hit" "$search_resp"
      fi

      # Search — typo tolerance
      typo_resp=$(curl -s -X POST "${QS_BASE}/movies/query" \
        $QS_AUTH \
        -H "Content-Type: application/json" \
        -d '{"query":"matrx"}' 2>&1)
      if echo "$typo_resp" | grep -q '"The Matrix"'; then
        pass "Install test: typo-tolerant search 'matrx' returns The Matrix"
      else
        fail "Install test: typo-tolerant search 'matrx' did not return expected hit" "$typo_resp"
      fi

      # List indexes — verify movies index exists
      idx_resp=$(curl -s "${QS_BASE}" \
        $QS_AUTH 2>&1)
      if echo "$idx_resp" | grep -q "movies"; then
        pass "Install test: GET /1/indexes lists 'movies'"
      else
        fail "Install test: GET /1/indexes missing 'movies'" "$idx_resp"
      fi
    fi

    # Stop server
    if [ -n "$QS_SERVER_PID" ] && kill -0 "$QS_SERVER_PID" 2>/dev/null; then
      kill "$QS_SERVER_PID" 2>/dev/null || true
      wait "$QS_SERVER_PID" 2>/dev/null || true
    fi
    QS_SERVER_PID=""
  fi

  rm -rf "$qs_dir" "$qs_data"
fi

# ── Uninstall Tests (requires network + binary) ─────────────────────────────

section "Uninstall Tests"

if [ -z "${GITHUB_TOKEN:-}" ]; then
  printf "  \033[1;33mSkipped\033[0m (requires GITHUB_TOKEN for binary download)\n"
else
  # Install to sandboxed dir with shell configs
  uninst_home=$(mktemp -d)
  uninst_install="${uninst_home}/.flapjack"

  # Create shell config files with existing content
  printf "# existing bashrc content\n" > "$uninst_home/.bashrc"
  printf "# existing zshrc content\n" > "$uninst_home/.zshrc"
  mkdir -p "$uninst_home/.config/fish"
  printf "# existing fish config\n" > "$uninst_home/.config/fish/config.fish"

  # Install flapjack (this adds PATH entries to the shell configs)
  HOME="$uninst_home" FLAPJACK_INSTALL="$uninst_install" GITHUB_TOKEN="$GITHUB_TOKEN" \
    sh "$INSTALL_SCRIPT" 2>&1 >/dev/null

  # Verify binary was installed
  if [ -x "$uninst_install/bin/flapjack" ]; then
    pass "Uninstall: binary installed at $uninst_install/bin/flapjack"
  else
    fail "Uninstall: binary not found (cannot proceed with uninstall tests)"
  fi

  # Verify PATH entries were added to at least one config
  if grep -qF ".flapjack" "$uninst_home/.bashrc" 2>/dev/null || \
     grep -qF ".flapjack" "$uninst_home/.zshrc" 2>/dev/null || \
     grep -qF ".flapjack" "$uninst_home/.config/fish/config.fish" 2>/dev/null; then
    pass "Uninstall: PATH entries present in shell configs before uninstall"
  else
    fail "Uninstall: no PATH entries found in shell configs (install may not have modified them)"
  fi

  if [ -x "$uninst_install/bin/flapjack" ]; then
    # Run uninstall
    uninst_output=$(HOME="$uninst_home" FLAPJACK_INSTALL="$uninst_install" \
      "$uninst_install/bin/flapjack" uninstall 2>&1)

    # Check success message
    if echo "$uninst_output" | grep -q "uninstalled"; then
      pass "Uninstall: command reports success"
    else
      fail "Uninstall: no success message" "$uninst_output"
    fi

    # Verify install directory removed
    if [ ! -d "$uninst_install" ]; then
      pass "Uninstall: install directory removed"
    else
      fail "Uninstall: install directory still exists at $uninst_install"
    fi

    # Verify PATH entries cleaned from bashrc
    if ! grep -qF ".flapjack" "$uninst_home/.bashrc" 2>/dev/null; then
      pass "Uninstall: PATH entry removed from .bashrc"
    else
      fail "Uninstall: .bashrc still contains .flapjack PATH entry"
    fi

    # Verify PATH entries cleaned from zshrc
    if ! grep -qF ".flapjack" "$uninst_home/.zshrc" 2>/dev/null; then
      pass "Uninstall: PATH entry removed from .zshrc"
    else
      fail "Uninstall: .zshrc still contains .flapjack PATH entry"
    fi

    # Verify PATH entries cleaned from fish config
    if ! grep -qF ".flapjack" "$uninst_home/.config/fish/config.fish" 2>/dev/null; then
      pass "Uninstall: PATH entry removed from fish config"
    else
      fail "Uninstall: fish config still contains .flapjack PATH entry"
    fi

    # Verify existing content preserved
    if grep -q "existing bashrc content" "$uninst_home/.bashrc" 2>/dev/null; then
      pass "Uninstall: existing .bashrc content preserved"
    else
      fail "Uninstall: existing .bashrc content was lost"
    fi
  fi

  rm -rf "$uninst_home"
fi

# ── Summary ──────────────────────────────────────────────────────────────────

section "Summary"
printf "  Total: %d  Passed: \033[0;32m%d\033[0m  Failed: \033[0;31m%d\033[0m\n\n" "$TESTS_RUN" "$TESTS_PASSED" "$TESTS_FAILED"

if [ "$TESTS_FAILED" -gt 0 ]; then
  exit 1
fi
