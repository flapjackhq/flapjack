#!/usr/bin/env python3
"""Mutation-capable contract tests for the candidate test-binary handoff."""

import json
import os
import shutil
import signal
import subprocess
import sys
import time
import tempfile
import unittest
from pathlib import Path

from extract_candidate_binary import ContractError, extract_candidate_binary


def artifact(executable, *, name="flapjack", kind=None, test=True):
    return {
        "reason": "compiler-artifact",
        "target": {"name": name, "kind": kind or ["lib"]},
        "profile": {"test": test},
        "executable": executable,
    }


class CandidateBinaryContract(unittest.TestCase):
    def write_messages(self, directory, messages):
        path = Path(directory) / "candidate_build.json"
        path.write_text("".join(json.dumps(message) + "\n" for message in messages))
        return path

    def test_extracts_only_the_flapjack_library_test_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(
                directory,
                [
                    artifact(None, test=False),
                    artifact("/tmp/flapjack-http-test", name="flapjack-http"),
                    artifact("/tmp/flapjack-lib-test"),
                    {"reason": "build-finished", "success": True},
                ],
            )
            output = Path(directory) / "github_output"

            self.assertEqual("/tmp/flapjack-lib-test", extract_candidate_binary(messages, output))
            self.assertEqual("executable=/tmp/flapjack-lib-test\n", output.read_text())

    def test_refuses_a_missing_candidate_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(directory, [{"reason": "build-finished", "success": True}])
            with self.assertRaisesRegex(ContractError, "exactly one"):
                extract_candidate_binary(messages, Path(directory) / "github_output")

    def test_refuses_ambiguous_candidate_binaries(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(
                directory,
                [
                    artifact("/tmp/flapjack-lib-test-a"),
                    artifact("/tmp/flapjack-lib-test-b"),
                    {"reason": "build-finished", "success": True},
                ],
            )
            output = Path(directory) / "github_output"
            output.write_text("sentinel=true\n")
            with self.assertRaisesRegex(ContractError, "found 2"):
                extract_candidate_binary(messages, output)
            self.assertEqual("sentinel=true\n", output.read_text())

    def test_refuses_output_without_a_successful_build_finished_message(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(
                directory,
                [artifact("/tmp/flapjack-lib-test")],
            )
            with self.assertRaisesRegex(ContractError, "successful build-finished"):
                extract_candidate_binary(messages, Path(directory) / "github_output")

    def test_refuses_a_failed_build_finished_message(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(
                directory,
                [
                    artifact("/tmp/flapjack-lib-test"),
                    {"reason": "build-finished", "success": False},
                ],
            )
            with self.assertRaisesRegex(ContractError, "successful build-finished"):
                extract_candidate_binary(messages, Path(directory) / "github_output")

    def test_refuses_non_object_cargo_messages(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(directory, [["not", "an", "object"]])
            with self.assertRaisesRegex(ContractError, "JSON object"):
                extract_candidate_binary(messages, Path(directory) / "github_output")

    def test_refuses_malformed_compiler_artifact_metadata(self):
        with tempfile.TemporaryDirectory() as directory:
            messages = self.write_messages(
                directory,
                [
                    {
                        "reason": "compiler-artifact",
                        "target": {"name": "flapjack", "kind": None},
                        "profile": {"test": True},
                        "executable": "/tmp/flapjack-lib-test",
                    },
                    {"reason": "build-finished", "success": True},
                ],
            )
            with self.assertRaisesRegex(ContractError, "target/profile data"):
                extract_candidate_binary(messages, Path(directory) / "github_output")


class CandidatePrebuildLifecycleContract(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        (self.root / "tests").mkdir()
        (self.root / "bin").mkdir()
        shutil.copy2(Path(__file__).with_name("extract_candidate_binary.py"), self.root / "tests/extract_candidate_binary.py")
        source = (Path(__file__).resolve().parents[2] / ".github/workflows/private-ci.yml").read_text()
        block = source.split("- name: Build candidate core test binary\n", 1)[1].split("\n      - ", 1)[0]
        run = block.split("run:", 1)[1].strip()
        self.command = " ".join(line.strip() for line in run.splitlines()[1:]) if run.startswith((">", "|")) else run
        self.writer = self.root / "writer.py"
        self.writer.write_text("import signal,sys,time\nfrom pathlib import Path\nsignal.signal(signal.SIGTERM, signal.SIG_IGN)\nsignal.signal(signal.SIGINT, signal.SIG_IGN)\nwhile True:\n with Path(sys.argv[1]).open('a') as marker: marker.write('x')\n time.sleep(0.01)\n")
        fixture = [artifact("/tmp/flapjack-owned-test"), {"reason": "build-finished", "success": True}]
        fake = self.root / "bin/cargo"
        fake.write_text("#!" + sys.executable + "\n" + r"""
import json,os,signal,subprocess,sys,time
from pathlib import Path
expected=['test','-p','flapjack','--lib','--no-run','--message-format=json']
if sys.argv[1:] != expected:
    print('unexpected compiler arguments',file=sys.stderr);sys.exit(91)
root=Path(os.environ['PROBE_ROOT']);mode=os.environ['COMPILER_MODE']
if mode=='success':
    for row in FIXTURE: print(json.dumps(row))
    sys.exit(0)
print('partial compiler diagnostics',flush=True)
print('compiler fixture diagnostic',file=sys.stderr,flush=True)
if mode=='failure':sys.exit(42)
signal.signal(signal.SIGTERM,lambda *_:sys.exit(0))
signal.signal(signal.SIGINT,lambda *_:sys.exit(0))
worker=subprocess.Popen([sys.executable,str(root/'writer.py'),str(root/'owned-marker')],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
while not (root/'owned-marker').exists():time.sleep(0.01)
(root/'compiler-state.json').write_text(json.dumps({'pid':os.getpid(),'pgid':os.getpgrp(),'worker':worker.pid}))
if mode=='failure-child':sys.exit(42)
while True:time.sleep(0.1)
""".replace('FIXTURE', repr(fixture)))
        fake.chmod(0o755)
        self.entry = None
        self.unrelated = subprocess.Popen([sys.executable, str(self.writer), str(self.root / 'unrelated-marker')], start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def tearDown(self):
        groups = {self.unrelated.pid}
        if self.entry is not None: groups.add(self.entry.pid)
        state = self.root / 'compiler-state.json'
        if state.exists(): groups.add(json.loads(state.read_text())['pgid'])
        for group in groups:
            if group == os.getpgrp(): raise AssertionError('refusing to signal test runner group')
            try: os.killpg(group, signal.SIGKILL)
            except ProcessLookupError: pass
        for process in (self.entry, self.unrelated):
            if process is not None:
                try: process.communicate(timeout=3)
                except subprocess.TimeoutExpired: process.kill(); process.communicate(timeout=3)
        self.temporary.cleanup()

    def until(self, predicate):
        deadline = time.monotonic() + 5
        while not predicate():
            if time.monotonic() >= deadline: self.fail('controlled compiler fixture did not reach its barrier')
            time.sleep(0.01)

    def start(self, mode):
        script = self.root / 'entry.sh'
        script.write_text('set -e\n' + self.command + '\n')
        env = dict(os.environ, PROBE_ROOT=str(self.root), COMPILER_MODE=mode, GITHUB_OUTPUT=str(self.root/'github-output'))
        env['PATH'] = str(self.root/'bin') + os.pathsep + env['PATH']
        self.entry = subprocess.Popen(['/opt/homebrew/bin/bash' if sys.platform=='darwin' else '/bin/bash',str(script)], cwd=self.root, env=env, start_new_session=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        return self.entry

    def assert_owned_writer_stopped(self):
        owned = self.root/'owned-marker'; other = self.root/'unrelated-marker'
        self.until(lambda: other.exists())
        owned_size = owned.stat().st_size; other_size = other.stat().st_size
        self.until(lambda: other.stat().st_size >= other_size + 8)
        self.assertIsNone(self.unrelated.poll(), 'unrelated process must survive')
        self.assertEqual(owned_size, owned.stat().st_size, 'owned compiler descendant kept writing after terminal return')

    def test_normal_compile_retains_the_canonical_handoff(self):
        process = self.start('success')
        stdout, stderr = process.communicate(timeout=5)
        self.assertEqual(0, process.returncode, stderr.decode())
        self.assertEqual('executable=/tmp/flapjack-owned-test\n', (self.root/'github-output').read_text())

    def test_nonzero_compile_preserves_partial_report_stderr_and_exit(self):
        process = self.start('failure')
        stdout, stderr = process.communicate(timeout=5)
        self.assertEqual(42, process.returncode)
        self.assertIn(b'compiler fixture diagnostic', stderr)
        self.assertIn('partial compiler diagnostics', (self.root/'candidate_build.json').read_text())
        self.assertFalse((self.root/'github-output').exists())

    def test_nonzero_parent_exit_still_stops_its_stubborn_descendant(self):
        process = self.start('failure-child')
        self.until(lambda: (self.root/'compiler-state.json').exists())
        self.assertEqual(42, process.wait(timeout=5))
        self.assert_owned_writer_stopped()
        self.assertFalse((self.root/'github-output').exists())

    def test_cancellation_stops_the_owned_group_and_preserves_diagnostics(self):
        for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
            with self.subTest(signum=signum):
                if self.entry is not None:
                    self.tearDown(); self.setUp()
                process = self.start('cancel')
                self.until(lambda: (self.root/'compiler-state.json').exists())
                process.send_signal(signum)
                process.wait(timeout=5)
                self.assert_owned_writer_stopped()
                self.assertEqual(128 + signum, process.returncode)
                self.assertIn('partial compiler diagnostics', (self.root/'candidate_build.json').read_text())
                self.assertFalse((self.root/'github-output').exists())


if __name__ == "__main__":
    unittest.main()
