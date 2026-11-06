import unittest
from report_ci_red import current_red, label_for


def run(sha='current', conclusion='failure', date='2026-11-05T10:00:00Z', branch='main', status='completed'):
    return dict(headSha=sha, conclusion=conclusion, createdAt=date, headBranch=branch, status=status)


class RedReportingTest(unittest.TestCase):
    def test_latest_green_replaces_prior_failure(self):
        self.assertEqual(current_red([run(), run(conclusion='success', date='2026-11-05T11:00:00Z')], set(), 'current'), [])

    def test_cancelled_run_does_not_hide_red(self):
        red = run()
        self.assertEqual(current_red([red, run(conclusion='cancelled', date='2026-11-05T11:00:00Z')], set(), 'current'), [red])

    def test_current_main_and_open_pr_only(self):
        red_pr = run(sha='pr', branch='work')
        self.assertEqual(current_red([run(sha='old'), red_pr, run(sha='closed', branch='closed')], {'pr'}, 'current'), [red_pr])

    def test_new_running_attempt_does_not_erase_completed_failure(self):
        red = run()
        self.assertEqual(current_red([red, run(status='in_progress', date='2026-11-05T11:00:00Z')], set(), 'current'), [red])

    def test_dedup_key_is_stable_and_separates_branch_and_workflow(self):
        self.assertEqual(label_for('repo', 'nightly', 'main'), label_for('repo', 'nightly', 'main'))
        self.assertNotEqual(label_for('repo', 'nightly', 'main'), label_for('repo', 'union', 'main'))
        self.assertNotEqual(label_for('repo', 'CI', 'one'), label_for('repo', 'CI', 'two'))

class BeadsReportingTest(unittest.TestCase):
    def invoke(self, issues, failing_create=False):
        import subprocess
        from unittest import mock
        import report_ci_red as reporter
        repo, workflow = 'owner/repo', 'ci.yml'
        failed = dict(run(), name='CI', databaseId=7, url='https://github.com/owner/repo/actions/runs/7', event='push')
        def read(args):
            if args[0] == 'bd':
                return issues
            if args[1] == 'pr':
                return []
            return [failed]
        with mock.patch.object(reporter, 'SOURCES', ((repo, workflow),)), \
             mock.patch.object(reporter, 'read_json', side_effect=read), \
             mock.patch.object(reporter.subprocess, 'check_output', return_value='current\n'), \
             mock.patch.object(reporter.subprocess, 'run') as create:
            if failing_create:
                create.side_effect = subprocess.CalledProcessError(1, 'bd')
            reporter.main()
            return create

    def test_new_red_creates_one_bead_with_evidence(self):
        create = self.invoke([])
        self.assertEqual(create.call_count, 1)
        args = create.call_args.args[0]
        self.assertEqual(args[:2], ['bd', 'create'])
        self.assertEqual(args[args.index('--external-ref') + 1], 'https://github.com/owner/repo/actions/runs/7')

    def test_existing_open_bead_deduplicates_but_closed_bead_allows_recurrence(self):
        label = label_for('owner/repo', 'ci.yml', 'main')
        self.assertFalse(self.invoke([dict(labels=[label], status='open')]).called)
        self.assertTrue(self.invoke([dict(labels=[label], status='closed')]).called)

    def test_beads_write_failure_is_not_hidden(self):
        import subprocess
        with self.assertRaises(subprocess.CalledProcessError):
            self.invoke([], failing_create=True)
