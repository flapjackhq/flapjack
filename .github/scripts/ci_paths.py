#!/usr/bin/env python3
"""Conservative Rust test routing for private PRs and trusted mirror pushes."""
import json
import os
import subprocess
import sys
from pathlib import Path, PurePosixPath


def valid_mirror_provenance():
    try:
        value = json.loads(Path('.debbie/sync_manifest.json').read_text(encoding='utf-8'))
    except (OSError, UnicodeError, json.JSONDecodeError):
        return False
    return (isinstance(value, dict)
            and set(value) == {'schema_version', 'dev_sha', 'dev_repo', 'synced_at'}
            and type(value['schema_version']) is int and value['schema_version'] == 1
            and isinstance(value['dev_sha'], str) and len(value['dev_sha']) == 40
            and all(c in '0123456789abcdef' for c in value['dev_sha'])
            and value['dev_repo'] == 'gridl-dev/flapjack_dev'
            and isinstance(value['synced_at'], str) and bool(value['synced_at']))


def needs_rust(paths, *, mirror=False):
    if mirror and '.debbie/sync_manifest.json' in paths:
        if not valid_mirror_provenance():
            return True
        paths = [path for path in paths if path != '.debbie/sync_manifest.json']
    if not paths:
        return True
    for path in paths:
        p = PurePosixPath(path)
        if path.startswith(('sdks/', 'engine/sdk_test/')):
            continue
        if p.suffix == '.md' and (
            len(p.parts) == 1 or path.startswith(('docs/', 'engine/docs2/'))
            or path in {'engine/README.md', 'engine/LIB.md', '.github/workflows/README.md'}
        ):
            continue
        return True
    return False


def changed_paths(base, head, *, candidate_ref=None):
    if head and base == '0' * 40 and (candidate_ref or '').startswith('refs/heads/public-candidate/'):
        # A newly published candidate has no previous push SHA. Its known merge
        # base with public main is the complete candidate diff, including deletions.
        result = subprocess.run(
            ['git', 'merge-base', head, 'refs/remotes/origin/main'],
            capture_output=True, text=True,
        )
        base = result.stdout.strip() if result.returncode == 0 else None
    if not base or set(base) == {'0'} or not head:
        return []
    result = subprocess.run(
        ['git', 'diff', '--name-only', '--no-renames', '-z', base, head],
        capture_output=True,
    )
    if result.returncode:
        return []
    return [p.decode('utf-8', errors='replace') for p in result.stdout.split(b'\0') if p]


if __name__ == '__main__':
    mirror = (os.environ.get('GITHUB_REPOSITORY') == 'flapjackhq/flapjack'
              and os.environ.get('GITHUB_EVENT_NAME') == 'push')
    paths = changed_paths(os.environ.get('BASE_SHA'), os.environ.get('HEAD_SHA'),
                          candidate_ref=os.environ.get('GITHUB_REF') if mirror else None)
    needed = needs_rust(paths, mirror=mirror)
    value = f'rust-needed={str(needed).lower()}\n'
    if os.environ.get('GITHUB_OUTPUT'):
        with open(os.environ['GITHUB_OUTPUT'], 'a') as out:
            out.write(value)
    sys.stdout.write(value)
