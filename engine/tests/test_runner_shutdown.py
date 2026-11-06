#!/usr/bin/env python3
"""Exercise the canonical runner's shutdown, traps, and success path with real children."""

import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest


RUNNER = Path(__file__).resolve().parents[1] / "_dev/s/test"


class RunnerShutdownTest(unittest.TestCase):
    def run_runner(self, shutdown_status, action="", expected=0):
        source = RUNNER.read_text()
        functions = "\n".join(
            re.search(rf"^{name}\(\) \{{.*?^\}}", source, re.M | re.S).group()
            for name in ("stop_server", "cleanup")
        )
        traps = "\n".join(re.findall(r"^trap .*", source, re.M))
        done = source.split("# ── Done ─", 1)[1].split("\n", 1)[1]
        with tempfile.TemporaryDirectory(prefix="fj-runner-shutdown-") as directory:
            root = Path(directory)
            child = root / "child.py"
            child.write_text(
                "import os, pathlib, signal, sys\n"
                f"signal.signal(signal.SIGTERM, lambda *_: sys.exit({shutdown_status}))\n"
                "signal.alarm(5)\n"
                "pathlib.Path('ready').write_text(str(os.getpid()))\n"
                "signal.pause()\n"
            )
            script = "\n".join([
                "set -eu", functions,
                'error() { echo "$*" >&2; }',
                '_ui_cleanup() { touch ui-cleaned; }',
                'done_banner() { echo "$1"; }',
                'timer_elapsed() { echo 0s; }',
                'FJ_TMP_DIR="$PWD/data"; mkdir "$FJ_TMP_DIR"',
                'python3 child.py & FJ_PID=$!', traps,
                'for ((i=0; i<200; i++)); do test -f ready && break; sleep .01; done',
                'test -f ready', action, done,
            ])
            result = subprocess.run(
                ["bash", "-c", script], cwd=root,
                capture_output=True, text=True, timeout=10,
            )
            self.assertFalse((root / "data").exists(), result.stderr)
            self.assertTrue((root / "ui-cleaned").exists(), result.stderr)
            with self.assertRaises(ProcessLookupError):
                os.kill(int((root / "ready").read_text()), 0)
            self.assertEqual(result.returncode, expected, result.stderr)
            self.assertEqual("All tests passed!" in result.stdout, expected == 0)
            if shutdown_status:
                self.assertIn(f"status {shutdown_status}", result.stderr)

    def test_clean_shutdown(self):
        self.run_runner(0)

    def test_shutdown_failure_blocks_success(self):
        for action in ("", "exit 0", 'kill -TERM "$FJ_PID"; sleep .05'):
            with self.subTest(action=action):
                self.run_runner(42, action, expected=42)

    def test_shutdown_failure_at_server_switch_stops_next_stage(self):
        self.run_runner(42, 'stop_server; echo "All tests passed!"', expected=42)

    def test_original_failure_and_cancellation_survive_cleanup(self):
        for action, status in (("exit 17", 17), ("kill -INT $$", 130), ("kill -TERM $$", 143)):
            with self.subTest(action=action):
                self.run_runner(42, action, expected=status)

    def test_explicitly_expected_failure_does_not_fail_runner(self):
        self.run_runner(0, "if (exit 23); then exit 99; else test $? -eq 23; fi")

    def test_stop_does_not_touch_a_reused_server(self):
        self.run_runner(0, '''
owned_pid=$FJ_PID; owned_dir=$FJ_TMP_DIR
FJ_PID=""; FJ_TMP_DIR=""
stop_server
kill -0 "$owned_pid"
test -d "$owned_dir"
FJ_PID=$owned_pid; FJ_TMP_DIR=$owned_dir
''')


if __name__ == "__main__":
    unittest.main()
