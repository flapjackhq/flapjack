#!/usr/bin/env python3
"""Mutation-capable contract tests for the candidate test-binary handoff."""

import json
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


if __name__ == "__main__":
    unittest.main()
