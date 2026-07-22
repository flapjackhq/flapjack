#!/usr/bin/env bash
set -euo pipefail

wordpress_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
dist_dir="$wordpress_dir/dist"
created_fixture_roots=()

fail() {
	echo "packaged-install-contract: $*" >&2
	exit 1
}

cleanup_fixtures() {
	for fixture_root in "${created_fixture_roots[@]}"; do
		rm -rf "$fixture_root"
	done
}

create_local_artifact_fixture() {
	local fixture_root=$1
	local fixture_file=$2
	local root_path="$wordpress_dir/$fixture_root"
	local file_path="$wordpress_dir/$fixture_file"

	if test -e "$root_path"; then
		return
	fi

	mkdir -p "$(dirname "$file_path")"
	printf 'packaged install contract local artifact fixture\n' >"$file_path"
	created_fixture_roots+=("$root_path")
}

trap cleanup_fixtures EXIT

create_local_artifact_fixture ".env" ".env"
create_local_artifact_fixture ".wp-env" ".wp-env/state.json"
create_local_artifact_fixture ".wp-env.override.json" ".wp-env.override.json"
create_local_artifact_fixture "local-artifact.log" "local-artifact.log"
create_local_artifact_fixture "test-results" "test-results/package-artifact/result.txt"
create_local_artifact_fixture "playwright-report" "playwright-report/index.html"
create_local_artifact_fixture "playwright" "playwright/.cache/browser-state"
create_local_artifact_fixture "build" "build/package-artifact/output.txt"
create_local_artifact_fixture ".DS_Store" ".DS_Store"

archive_sha256() {
	python3 - "$1" <<'PY'
import hashlib
import sys

digest = hashlib.sha256()
with open(sys.argv[1], "rb") as archive:
    for chunk in iter(lambda: archive.read(1024 * 1024), b""):
        digest.update(chunk)
print(digest.hexdigest())
PY
}

find_package_archive() {
	shopt -s nullglob
	package_archives=("$dist_dir"/*.zip)
	shopt -u nullglob
	test "${#package_archives[@]}" -eq 1 || fail "expected exactly one ZIP in dist/, found ${#package_archives[@]}"
	package_archive=${package_archives[0]}
}

if test "${PACKAGED_INSTALL_CONTRACT_SKIP_REBUILD:-0}" != 1; then
	(
		cd "$wordpress_dir"
		npm run package
	)
	find_package_archive
	first_hash=$(archive_sha256 "$package_archive")
	(
		cd "$wordpress_dir"
		npm run package
	)
	find_package_archive
	second_hash=$(archive_sha256 "$package_archive")
	test "$first_hash" = "$second_hash" || fail 'repeated package builds produced different ZIP bytes'
else
	find_package_archive
fi

python3 - "$package_archive" <<'PY'
import stat
import sys
import zipfile

archive_path = sys.argv[1]
required_paths = {
    "flapjack-search.php",
    "assets/vendor/instantsearch.production.min.js",
    "assets/vendor/instantsearch-satellite.min.css",
    "vendor/autoload.php",
}
excluded_roots = {"tests", "node_modules", "scripts"}
excluded_files = {
    ".env",
    ".wp-env.override.json",
    ".DS_Store",
    "composer.json",
    "composer.lock",
    "local-artifact.log",
    "package.json",
    "package-lock.json",
}
excluded_generated_roots = {
    ".wp-env",
    "build",
    "playwright-report",
    "test-results",
}

with zipfile.ZipFile(archive_path) as archive:
    entries = archive.infolist()

names = [entry.filename for entry in entries]
assert names == sorted(names), "ZIP entries are not sorted deterministically"
assert all(entry.date_time == (1980, 1, 1, 0, 0, 0) for entry in entries), (
    "ZIP entries must use the fixed 1980-01-01 timestamp"
)

top_levels = {name.split("/", 1)[0] for name in names if name}
assert top_levels == {"flapjack-search"}, f"unexpected ZIP roots: {sorted(top_levels)}"
relative_names = {
    name.removeprefix("flapjack-search/")
    for name in names
    if name != "flapjack-search/"
}

missing = sorted(required_paths - relative_names)
assert not missing, f"missing required package paths: {missing}"
assert any(name.startswith("vendor/flapjackhq/flapjack-search-php/lib/") for name in relative_names), (
    "missing packaged PHP SDK lib/ contents"
)

for name in relative_names:
    path_parts = set(name.rstrip("/").split("/"))
    assert not path_parts.intersection(excluded_roots), f"development directory packaged: {name}"
    assert name not in excluded_files, f"source metadata packaged: {name}"
    assert name.split("/", 1)[0] not in excluded_generated_roots, (
        f"local/generated artifact packaged: {name}"
    )
    assert not name.startswith("playwright/.cache/"), f"Playwright cache packaged: {name}"
    assert not name.endswith(".log"), f"log artifact packaged: {name}"
    assert not name.endswith("/.DS_Store"), f"macOS metadata packaged: {name}"
    assert not name.startswith("vendor/phpunit/"), f"PHPUnit packaged: {name}"
    assert not name.startswith("vendor/yoast/"), f"Yoast test package packaged: {name}"

for entry in entries:
    mode = entry.external_attr >> 16
    assert not stat.S_ISLNK(mode), f"symlink packaged: {entry.filename}"
PY

echo 'packaged install contract: PASS'
