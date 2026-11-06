#!/usr/bin/env python3
"""Record current red required/scheduled Flapjack CI in its private Beads ledger."""
import hashlib
import json
import subprocess
from datetime import datetime
from zoneinfo import ZoneInfo


SOURCES = (
    ('gridl-dev/flapjack_dev', 'private-ci.yml'),
    ('flapjackhq/flapjack', 'ci.yml'),
    ('flapjackhq/flapjack', 'nightly.yml'),
    ('flapjackhq/flapjack', 'union.yml'),
)
RED = {'failure', 'timed_out', 'action_required', 'startup_failure'}


def read_json(args):
    output = subprocess.check_output(args, text=True, timeout=90)
    # bd may prefix its JSON with a v2 protocol NOTE. Parse a complete JSON value.
    for offset, char in enumerate(output):
        if char in '[{':
            try:
                value, end = json.JSONDecoder().raw_decode(output[offset:])
                if not output[offset + end:].strip():
                    return value
            except json.JSONDecodeError:
                pass
    raise ValueError(f'{args[0]} did not return JSON')


def current_red(runs, active_heads, main_sha):
    latest = {}
    for run in sorted(runs, key=lambda r: r['createdAt'], reverse=True):
        if run['status'] != 'completed' or run['conclusion'] == 'cancelled':
            continue
        if run['headSha'] != main_sha and run['headSha'] not in active_heads:
            continue
        latest.setdefault(run['headBranch'], run)
    return [run for run in latest.values() if run['conclusion'] in RED]


def label_for(repo, workflow, branch):
    key = f'{repo}/{workflow}/{branch}'
    return 'ci-red-' + hashlib.sha256(key.encode()).hexdigest()[:12]


def main():
    today = datetime.now(ZoneInfo('America/New_York')).date().isoformat()
    issues = read_json(['bd', 'list', '--all', '--label', 'ci-red', '-n', '0', '--json'])
    existing = {label for issue in issues if issue.get('status') != 'closed'
                for label in issue.get('labels', [])}
    repos = {}
    for repo, workflow in SOURCES:
        if repo not in repos:
            prs = read_json(['gh', 'pr', 'list', '--repo', repo, '--state', 'open',
                             '--limit', '100', '--json', 'headRefOid'])
            main_sha = subprocess.check_output(
                ['gh', 'api', f'repos/{repo}/commits/main', '--jq', '.sha'], text=True, timeout=90,
            ).strip()
            repos[repo] = ({pr['headRefOid'] for pr in prs}, main_sha)
        runs = read_json(['gh', 'run', 'list', '--repo', repo, '--workflow', workflow,
                          '--limit', '100', '--json',
                          'databaseId,name,conclusion,headBranch,headSha,url,event,status,createdAt'])
        for run in current_red(runs, *repos[repo]):
            label = label_for(repo, workflow, run['headBranch'])
            if label in existing:
                continue
            subprocess.run([
                'bd', 'create', '--type', 'bug', '--priority', '1',
                '--title', f"[BUG {today}] Red {run['name']}: {repo}@{run['headBranch']}",
                '--labels', f'ci,ci-red,{label}', '--external-ref', run['url'],
                '--description', f"{run['name']} concluded {run['conclusion']} on {run['headSha']}. "
                f"Evidence: {run['url']}. Reproduce the failing step, fix its cause without "
                'weakening coverage, and verify green on the affected ref. Detected by hourly private CI reporting.',
            ], check=True, timeout=90)
            existing.add(label)


if __name__ == '__main__':
    main()
