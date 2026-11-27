#!/usr/bin/env python3
"""Behavioral selection tests: stale maps cannot omit the last published versions."""
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location(
    "predecessors", Path(__file__).parents[1] / "package/release_predecessors.py"
)
owner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(owner)


def release(tag, **extra):
    return dict(tag_name=tag, draft=False, prerelease=False, published_at="2026-11-25", **extra)


def manifest(tag, target):
    digest = hashlib.sha256((tag + target).encode()).hexdigest()
    return json.dumps({"schemaVersion": 2, "artifact": {
        "file": f"flapjack-{target}.tar.gz", "target": target,
        "profile": "release", "binarySha256": digest}, "build": {
        "version": tag[1:], "target": target, "profile": "release",
        "revisionKnown": True}}).encode()


class PublishedPredecessors(unittest.TestCase):
    def test_last_three_stable_published_before_candidate(self):
        records = [release(f"v1.0.{n}") for n in (9, 18, 19, 20, 21, 22)]
        records += [dict(release("v1.0.99"), draft=True),
                    dict(release("v1.0.98"), prerelease=True),
                    dict(release("v1.0.97"), published_at=None)]
        self.assertEqual(owner.select_releases(records, "1.0.21"),
                         ["v1.0.18", "v1.0.19", "v1.0.20"])
        self.assertEqual(owner.select_releases(records, "1.0.22"),
                         ["v1.0.19", "v1.0.20", "v1.0.21"])

    def test_empty_history_refuses_silent_empty_edges(self):
        with self.assertRaises(ValueError):
            owner.select_releases([], "1.0.21")

    @patch.object(owner, "release_index", return_value=[release("v1.0.19"), release("v1.0.20")])
    @patch.object(owner, "manifest_bytes", side_effect=manifest)
    def test_exact_manifest_and_binary_digests_for_both_linux_targets(self, _download, _index):
        result = owner.resolve("1.0.21")
        for target in owner.TARGETS:
            entries = result["targets"][target]
            self.assertEqual([x["releaseTag"] for x in entries], ["v1.0.19", "v1.0.20"])
            for entry in entries:
                raw = manifest(entry["releaseTag"], target)
                self.assertEqual(entry["manifestSha256"], hashlib.sha256(raw).hexdigest())
                self.assertEqual(entry["binarySha256"], json.loads(raw)["artifact"]["binarySha256"])
                self.assertEqual(entry["forwardTransferMode"], "reuse_same_data_directory")

    @patch.object(owner, "release_index", return_value=[release("v1.0.20")])
    def test_wrong_target_or_version_and_missing_binary_digest_refuse(self, _index):
        for field, value in (("target", "wrong-target"), ("version", "1.0.19")):
            def changed(tag, target):
                body = json.loads(manifest(tag, target)); body["build"][field] = value
                return json.dumps(body).encode()
            with patch.object(owner, "manifest_bytes", side_effect=changed), self.assertRaises(ValueError):
                owner.resolve("1.0.21")
        def missing(tag, target):
            body = json.loads(manifest(tag, target)); del body["artifact"]["binarySha256"]
            return json.dumps(body).encode()
        with patch.object(owner, "manifest_bytes", side_effect=missing), self.assertRaises(ValueError):
            owner.resolve("1.0.21")

    def test_verify_rejects_stale_source_without_overwriting_it(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "source.json"; path.write_text("{}\n")
            with patch.object(owner, "resolve", return_value={"targets": {"linux": []}}):
                with self.assertRaises(ValueError):
                    owner.main(["--version", "1.0.21", "--verify", str(path)])
            self.assertEqual(path.read_text(), "{}\n")

    @patch.object(owner, "release_index", return_value=[release("v1.0.19"), release("v1.0.20")])
    @patch.object(owner, "manifest_bytes", side_effect=FileNotFoundError("missing published manifest"))
    def test_missing_predecessor_cannot_emit_a_partial_source(self, _download, _index):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "source.json"
            with self.assertRaises(FileNotFoundError):
                owner.main(["--version", "1.0.21", "--output", str(path)])
            self.assertFalse(path.exists())

    def test_public_builder_verifies_source_with_crlf_cargo_version(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory); package = root / "engine/package"; package.mkdir(parents=True)
            helper = package / "build_release_artifact"
            shutil.copyfile(Path(owner.__file__).with_name("build_release_artifact"), helper)
            (package.parent / "Cargo.toml").write_bytes(b'[package]\r\nversion = "1.0.21"\r\n')
            source = root / "resolved.json"; source.write_text("{}\n")
            # A rejected source must stop packaging, with the correct version
            # and exact source forwarded. No compiler or network is invoked.
            (package / "release_predecessors.py").write_text(
                "import sys\nassert sys.argv[1:] == "
                + repr(["--version", "1.0.21", "--verify", str(source)])
                + "\nsys.exit(76)\n")
            result = subprocess.run(["bash", str(helper), "--published-predecessors", "public-all",
                                     owner.TARGETS[0], "a" * 40, "b" * 40, str(source), str(root / "out")],
                                    capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 76, result.stderr.decode())


if __name__ == "__main__":
    unittest.main()
