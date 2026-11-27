#!/usr/bin/env python3
"""Resolve exact published predecessors; release CI must prove each populated edge."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tempfile

REPOSITORY = "flapjackhq/flapjack"
TARGETS = ("aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl")


def version(value):
    match = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?", value)
    if not match:
        raise ValueError("release version must be MAJOR.MINOR.PATCH")
    return tuple(map(int, match.groups()))


def select_releases(records, candidate):
    tags = [r["tag_name"] for r in records
            if not r.get("draft", True) and not r.get("prerelease", True)
            and r.get("published_at") and re.fullmatch(r"v\d+\.\d+\.\d+", r["tag_name"])
            and version(r["tag_name"]) < version(candidate)]
    if not tags or len(tags) != len(set(tags)):
        raise ValueError("previous published stable releases are absent or ambiguous")
    return sorted(sorted(tags, key=version)[-3:])


def release_index():
    pages = json.loads(subprocess.check_output([
        "gh", "api", "--paginate", "--slurp", f"repos/{REPOSITORY}/releases?per_page=100"
    ], timeout=60))
    return [release for page in pages for release in page]


def manifest_bytes(tag, target):
    name = f"flapjack-{target}.manifest.json"
    with tempfile.TemporaryDirectory(prefix="flapjack-predecessor-") as directory:
        subprocess.run(["gh", "release", "download", tag, "--repo", REPOSITORY,
                        "--pattern", name, "--dir", directory], check=True, timeout=60)
        return (Path(directory) / name).read_bytes()


def unique(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate manifest key: {key}")
        result[key] = value
    return result


def resolve(candidate):
    tags = select_releases(release_index(), candidate)
    targets = {}
    for target in TARGETS:
        entries = []
        for tag in tags:
            raw = manifest_bytes(tag, target)
            manifest = json.loads(raw, object_pairs_hook=unique)
            artifact, build = manifest["artifact"], manifest["build"]
            binary = artifact.get("binarySha256")
            if (manifest["schemaVersion"] != 2 or build.get("version") != tag[1:]
                    or build.get("target") != target or artifact.get("target") != target
                    or build.get("profile") != "release" or artifact.get("profile") != "release"
                    or build.get("revisionKnown") is not True
                    or artifact.get("file") != f"flapjack-{target}.tar.gz"
                    or not isinstance(binary, str) or not re.fullmatch(r"[0-9a-f]{64}", binary)):
                raise ValueError(f"published {tag}/{target} identity is invalid")
            entries.append({"releaseTag": tag, "manifestSha256": hashlib.sha256(raw).hexdigest(),
                            "binarySha256": binary, "transitionMode": "routine_same_host",
                            "forwardTransferMode": "reuse_same_data_directory",
                            "rollbackMode": "binary_reactivate_same_data",
                            "parityProfile": "same_data_upgrade_smoke_v1"})
        targets[target] = entries
    return {"schemaVersion": 2, "dataDisposition": "preserve",
            "mixedVersionReplication": "not_guaranteed", "targets": targets}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--version", required=True)
    destination = parser.add_mutually_exclusive_group(required=True)
    destination.add_argument("--output", type=Path)
    destination.add_argument("--verify", type=Path)
    args = parser.parse_args(argv)
    raw = (json.dumps(resolve(args.version), sort_keys=True, separators=(",", ":")) + "\n").encode()
    if args.verify:
        if args.verify.read_bytes() != raw:
            raise ValueError("compatibility source omits or changes the previous published releases")
    else:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_bytes(raw)


if __name__ == "__main__":
    main()
