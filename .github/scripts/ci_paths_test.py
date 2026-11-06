import os
import subprocess
import tempfile
import unittest
from pathlib import Path

from ci_paths import changed_paths, needs_rust


class RustRoutingTest(unittest.TestCase):
    def test_only_docs_or_sdks_skip_rust(self):
        self.assertFalse(needs_rust(['README.md', 'docs/design/plan.md', 'sdks/javascript/a.js']))
        self.assertFalse(needs_rust(['engine/sdk_test/contract_tests.js', 'engine/docs2/FEATURES.md']))
        self.assertFalse(needs_rust(['engine/README.md', 'engine/LIB.md', '.github/workflows/README.md']))
        for path in ('engine/Cargo.lock', 'engine/src/lib.rs', '.github/workflows/ci.yml',
                     'engine/docs2/openapi.json', 'engine/tests/fixtures/note.md', '.debbie.toml'):
            with self.subTest(path=path):
                self.assertTrue(needs_rust(['README.md', path]))

    def test_unknown_or_empty_diff_runs_rust(self):
        self.assertTrue(needs_rust([]))
        self.assertTrue(needs_rust(changed_paths(None, 'HEAD')))
        self.assertTrue(needs_rust(changed_paths('0' * 40, 'HEAD')))
        self.assertTrue(needs_rust(changed_paths('missing-commit', 'HEAD')))

    def test_new_mirror_candidate_routes_real_docs_and_metadata_diff(self):
        helper = Path(__file__).resolve().with_name('ci_paths.py')
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(['git', '-C', directory, *args], text=True).strip()
            git('init', '-q')
            git('config', 'user.email', 'test@example.invalid')
            git('config', 'user.name', 'CI routing test')
            Path(directory, 'README.md').write_text('before')
            git('add', '.')
            git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
            git('update-ref', 'refs/remotes/origin/main', base)
            Path(directory, 'README.md').write_text('after')
            manifest = Path(directory, '.debbie/sync_manifest.json')
            manifest.parent.mkdir()
            provenance = ('{"schema_version":1,"dev_repo":"gridl-dev/flapjack_dev",'
                          '"synced_at":"2026-11-05T00:00:00Z","dev_sha":"' + 'a' * 40 + '"}')
            manifest.write_text(provenance)
            git('add', '.')
            git('commit', '-qm', 'docs candidate')
            head = git('rev-parse', 'HEAD')
            previous = os.getcwd()
            try:
                os.chdir(directory)
                paths = changed_paths('0' * 40, head, candidate_ref='refs/heads/public-candidate/docs')
                self.assertEqual(set(paths), {'README.md', '.debbie/sync_manifest.json'})
                self.assertFalse(needs_rust(paths, mirror=True))
                self.assertTrue(needs_rust(paths))
                self.assertTrue(needs_rust(['.debbie/sync_manifest.json'], mirror=True))
                self.assertTrue(needs_rust(paths + ['engine/src/lib.rs'], mirror=True))
                result = subprocess.run(['python3', str(helper)], capture_output=True, text=True,
                    env=dict(os.environ, BASE_SHA='0' * 40, HEAD_SHA=head,
                        GITHUB_REPOSITORY='flapjackhq/flapjack', GITHUB_EVENT_NAME='push',
                        GITHUB_REF='refs/heads/public-candidate/docs',
                        GITHUB_OUTPUT=str(Path(directory, 'step-output'))))
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(result.stdout, 'rust-needed=false\n')
                manifest.write_text('{}')
                self.assertTrue(needs_rust(paths, mirror=True))
                manifest.unlink()
                self.assertTrue(needs_rust(paths, mirror=True))
                manifest.write_text(provenance)
                git('update-ref', '-d', 'refs/remotes/origin/main')
                self.assertTrue(needs_rust(changed_paths('0' * 40, head,
                    candidate_ref='refs/heads/public-candidate/docs'), mirror=True))
            finally:
                os.chdir(previous)

    def test_deleted_and_renamed_rust_paths_stay_in_denominator(self):
        with tempfile.TemporaryDirectory() as directory:
            def git(*args):
                return subprocess.check_output(['git', '-C', directory, *args], text=True).strip()
            git('init', '-q')
            git('config', 'user.email', 'test@example.invalid')
            git('config', 'user.name', 'CI routing test')
            Path(directory, 'lib.rs').write_text('fn main() {}')
            git('add', '.')
            git('commit', '-qm', 'base')
            base = git('rev-parse', 'HEAD')
            Path(directory, 'lib.rs').rename(Path(directory, 'README.md'))
            git('add', '-A')
            git('commit', '-qm', 'rename')
            previous = os.getcwd()
            try:
                os.chdir(directory)
                paths = changed_paths(base, 'HEAD')
                self.assertEqual(set(paths), {'lib.rs', 'README.md'})
                self.assertTrue(needs_rust(paths))
            finally:
                os.chdir(previous)

class PublicAggregateTest(unittest.TestCase):
    def run_gate(self, results, rust='true', repository='flapjackhq/flapjack', ref='refs/heads/public-candidate/test'):
        import json
        import textwrap
        workflow = Path(__file__).resolve().parents[2] / '.github/workflows/ci.yml'
        block = workflow.read_text().split('  public-candidate-gate:\n', 1)[1]
        script = textwrap.dedent(block.split("          python3 - <<'PY'\n", 1)[1].rsplit('          PY', 1)[0])
        env = dict(os.environ, NEEDS_JSON=json.dumps(results), RUST_NEEDED=rust,
                   ACTUAL_REPOSITORY=repository, ACTUAL_REF=ref)
        return subprocess.run(['python3', '-c', script], env=env, capture_output=True, text=True).returncode

    def test_success_and_explicit_docs_skip(self):
        self.assertEqual(self.run_gate({'check-repo': {'result': 'success'}}), 0)
        self.assertEqual(self.run_gate({'rust-tests-all': {'result': 'skipped'}}, rust='false'), 0)

    def test_failures_cannot_be_hidden_by_docs_routing(self):
        for name, result, rust in [('rust-tests-all', 'skipped', 'true'),
                                   ('rust-tests-all', 'failure', 'false'),
                                   ('rust-tests-all', 'cancelled', 'false'),
                                   ('sdk-contract', 'skipped', 'false')]:
            with self.subTest(name=name, result=result, rust=rust):
                self.assertNotEqual(self.run_gate({name: {'result': result}}, rust=rust), 0)

    def test_foreign_repository_and_untrusted_branch_refuse(self):
        self.assertNotEqual(self.run_gate({}, repository='gridl-dev/flapjack_dev'), 0)
        self.assertNotEqual(self.run_gate({}, ref='refs/heads/random'), 0)

class UpgradeValidationStatusTest(unittest.TestCase):
    def invoke_status(self, result, sha='a' * 40):
        import json
        import textwrap
        workflow = Path(__file__).resolve().parents[2] / '.github/workflows/ci.yml'
        script = textwrap.dedent(workflow.read_text().split('  validation-status:\n', 1)[1].split('        run: |\n', 1)[1])
        with tempfile.TemporaryDirectory() as directory:
            capture = Path(directory, 'args.json')
            gh = Path(directory, 'gh')
            gh.write_text('#!/usr/bin/env python3\nimport json,os,sys\nopen(os.environ["CAPTURE"],"w").write(json.dumps(sys.argv[1:]))\n')
            gh.chmod(0o755)
            env = dict(os.environ, PATH=directory + os.pathsep + os.environ['PATH'],
                       CAPTURE=str(capture), SOURCE_SHA=sha, SUITE_RESULT=result,
                       GITHUB_REPOSITORY='owner/repo', RUN_URL='https://github.com/owner/repo/actions/runs/7')
            process = subprocess.run(['bash', '-e', '-c', script], env=env, capture_output=True)
            return process.returncode, json.loads(capture.read_text()) if capture.exists() else None

    def test_only_successful_full_suite_reports_success(self):
        for result in ('success', 'failure', 'cancelled', 'skipped'):
            with self.subTest(result=result):
                code, args = self.invoke_status(result)
                self.assertEqual(code, 0)
                expected = 'success' if result == 'success' else 'failure'
                self.assertIn('state=' + expected, args)
                self.assertIn('repos/owner/repo/statuses/' + 'a' * 40, args)

    def test_mutable_or_malformed_revision_cannot_receive_status(self):
        for sha in ('main', 'a' * 39, 'a' * 41, 'A' * 40, 'a; echo broken'):
            with self.subTest(sha=sha):
                code, args = self.invoke_status('success', sha)
                self.assertNotEqual(code, 0)
                self.assertIsNone(args)


if __name__ == '__main__':
    unittest.main()
