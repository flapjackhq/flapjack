#!/usr/bin/env python3
"""Release-only populated-pair evidence producer and validator.

The compatibility declaration decides whether a release pair is allowed.  This
module has the narrower job of proving that an already-authorized exceptional
pair preserved every populated tenant in both directions.  Keeping that split
prevents a successful test run from silently advertising a release pair.

The fast contract imports :func:`validate_receipt` directly.  The executable
interface validates an immutable JSON receipt and, once supplied with exact
pair binaries, runs the bounded local proof that produces such a receipt.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import socket
import stat
import struct
import subprocess
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, NoReturn


HASH_RE = re.compile(r"^[0-9a-fA-F]{64}$")
REVISION_RE = re.compile(r"^[0-9a-f]{40}$")
TRANSACTION_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{2,127}$")
RELEASE_TAG_RE = re.compile(r"^v[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?$")

EXCEPTIONAL_RECIPE = {
    "transitionMode": "exceptional_blue_green",
    "forwardTransferMode": "snapshot_then_tail_replication",
    "rollbackMode": "reverse_tail_to_retained_predecessor",
    "parityProfile": "populated_engine_exact_v1",
}

# Public, release-test-only credential for disposable loopback servers. It is
# fixed so hostile ambient credentials cannot select or influence the proof.
PROOF_APPLICATION_ID = "flapjack"
PROOF_ADMIN_KEY = "rehx2-local-loopback-admin-v1"
PROOF_APPLICATION_HEADER = "x-algolia-application-id"
PROOF_ADMIN_HEADER = "x-algolia-api-key"

RELEASE_CONTRACT_HEADER = "x-flapjack-release-transfer"
RELEASE_CONTRACT_V1 = "one-uid-contiguous-v1"
RELEASE_TENANT_HEADER = "x-flapjack-release-transfer-tenant"
RELEASE_TRANSACTION_HEADER = "x-flapjack-release-transfer-transaction"
RELEASE_AFTER_HEADER = "x-flapjack-release-transfer-after-seq"
RELEASE_THROUGH_HEADER = "x-flapjack-release-transfer-through-seq"
RELEASE_STATUS_HEADER = "x-flapjack-release-transfer-status"
RELEASE_SNAPSHOT_SHA256_HEADER = "x-flapjack-release-transfer-snapshot-sha256"
RELEASE_PAYLOAD_SHA256_HEADER = "x-flapjack-release-transfer-payload-sha256"
RELEASE_CONTIGUOUS = "contiguous"
RELEASE_RESNAPSHOT_REQUIRED = "resnapshot_required"
RELEASE_ACKNOWLEDGED = "acknowledged"

# Zero means setup mode.  The real proof arms this before its first tail write,
# and every HTTP operation thereafter shares the same twelve-minute deadline.
_BEHAVIOR_DEADLINE = 0.0


class ContractError(ValueError):
    """Raised when evidence does not prove the populated-pair contract."""


def _reject(message: str) -> NoReturn:
    raise ContractError(message)


def _object(value: Any, name: str, keys: set[str]) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != keys:
        _reject(f"{name} must be an object with exactly {sorted(keys)}")
    return value


def _list(value: Any, name: str) -> list[Any]:
    if not isinstance(value, list):
        _reject(f"{name} must be an array")
    return value


def _integer(value: Any, name: str, *, minimum: int = 0) -> int:
    # bool is an int subclass in Python and must be rejected explicitly.
    if type(value) is not int or value < minimum:
        _reject(f"{name} must be an integer >= {minimum}")
    return value


def _number(value: Any, name: str) -> float:
    if (
        type(value) not in (int, float)
        or not math.isfinite(value)
        or value < 0
    ):
        _reject(f"{name} must be a finite non-negative number")
    return float(value)


def _string(value: Any, name: str) -> str:
    if not isinstance(value, str) or not value:
        _reject(f"{name} must be a non-empty string")
    return value


def _digest(value: Any, name: str) -> str:
    text = _string(value, name)
    if not HASH_RE.fullmatch(text):
        _reject(f"{name} must be a SHA-256 digest")
    return text.lower()


def _identity(value: Any, name: str) -> dict[str, str]:
    identity = _object(
        value,
        name,
        {
            "targetTriple",
            "manifestSha256",
            "binarySha256",
            "buildIdentitySha256",
            "revision",
        },
    )
    target = _string(identity["targetTriple"], f"{name}.targetTriple")
    revision = _string(identity["revision"], f"{name}.revision")
    if not REVISION_RE.fullmatch(revision):
        _reject(f"{name}.revision must be an exact lowercase git revision")
    return {
        "targetTriple": target,
        "manifestSha256": _digest(
            identity["manifestSha256"], f"{name}.manifestSha256"
        ),
        "binarySha256": _digest(identity["binarySha256"], f"{name}.binarySha256"),
        "buildIdentitySha256": _digest(
            identity["buildIdentitySha256"], f"{name}.buildIdentitySha256"
        ),
        "revision": revision,
    }


def _tenant_ids(receipt: dict[str, Any]) -> list[str]:
    raw = _list(receipt["tenants"], "tenants")
    tenants = [_string(value, "tenant id") for value in raw]
    if len(tenants) < 2:
        _reject("populated pair requires multiple tenants")
    if tenants != sorted(set(tenants)):
        _reject("tenants must be unique and sorted")
    return tenants


def _records_by_tenant(
    value: Any,
    name: str,
    tenants: list[str],
    keys: set[str],
) -> dict[str, dict[str, Any]]:
    records: dict[str, dict[str, Any]] = {}
    for index, raw in enumerate(_list(value, name)):
        record = _object(raw, f"{name}[{index}]", keys)
        tenant = _string(record["tenantId"], f"{name}[{index}].tenantId")
        if tenant in records:
            _reject(f"{name} contains duplicate tenant {tenant}")
        records[tenant] = record
    if set(records) != set(tenants):
        _reject(f"{name} tenant set does not match the declared tenants")
    return records


def _validate_fence(
    value: Any,
    name: str,
    transaction_id: str,
    expected_role: str,
    tenants: list[str],
) -> dict[str, Any]:
    fence = _object(
        value,
        name,
        {"transactionId", "sourceRole", "active", "writeRejection"},
    )
    if fence["transactionId"] != transaction_id:
        _reject(f"{name} is not transaction-bound")
    if fence["sourceRole"] != expected_role:
        _reject(f"{name} has the wrong source role")
    if fence["active"] is not True:
        _reject(f"{name} must be active during zero-lag observation")
    rejection = _object(
        fence["writeRejection"],
        f"{name}.writeRejection",
        {"tenantId", "objectId", "statusCode", "beforeSeq", "afterSeq"},
    )
    if rejection["tenantId"] not in tenants:
        _reject(f"{name} fenced write used an undeclared tenant")
    _string(rejection["objectId"], f"{name} fenced write objectId")
    if type(rejection["statusCode"]) is not int or rejection["statusCode"] != 503:
        _reject(f"{name} fenced write was not rejected with HTTP 503")
    before = _integer(rejection["beforeSeq"], f"{name} fenced write beforeSeq")
    after = _integer(rejection["afterSeq"], f"{name} fenced write afterSeq")
    if before != after:
        _reject(f"{name} fenced write advanced the source sequence")
    return rejection


def _validate_tail(
    value: Any,
    name: str,
    tenants: list[str],
    expected_baselines: dict[str, int],
) -> dict[str, int]:
    records = _records_by_tenant(
        value,
        name,
        tenants,
        {
            "tenantId",
            "baselineSeq",
            "oldestRetainedSeq",
            "deliveredSeqs",
            "ackedSeq",
            "sourceCurrentSeq",
        },
    )
    final_sequences: dict[str, int] = {}
    for tenant in tenants:
        record = records[tenant]
        baseline = _integer(record["baselineSeq"], f"{name}.{tenant}.baselineSeq")
        if baseline != expected_baselines[tenant]:
            _reject(f"{name}.{tenant} baseline does not bind the prior phase")
        oldest = _integer(
            record["oldestRetainedSeq"], f"{name}.{tenant}.oldestRetainedSeq"
        )
        if oldest > baseline + 1:
            _reject(f"{name}.{tenant} has a retention gap")
        current = _integer(
            record["sourceCurrentSeq"], f"{name}.{tenant}.sourceCurrentSeq"
        )
        delivered = [
            _integer(item, f"{name}.{tenant}.deliveredSeqs")
            for item in _list(record["deliveredSeqs"], f"{name}.{tenant}.deliveredSeqs")
        ]
        if delivered != list(range(baseline + 1, current + 1)):
            _reject(f"{name}.{tenant} tail is not contiguous and ordered")
        acked = _integer(record["ackedSeq"], f"{name}.{tenant}.ackedSeq")
        if not delivered or acked != current or acked != delivered[-1]:
            _reject(f"{name}.{tenant} acknowledgement is not exact")
        final_sequences[tenant] = current
    return final_sequences


def _validate_post_zero(
    value: Any, name: str, tenants: list[str], final_sequences: dict[str, int]
) -> None:
    if not isinstance(value, dict) or set(value) != set(tenants):
        _reject(f"{name} must contain every tenant exactly once")
    for tenant in tenants:
        observed = _integer(value[tenant], f"{name}.{tenant}")
        if observed != final_sequences[tenant]:
            _reject(f"{tenant} advanced after zero lag while fenced")


def _validate_parity(value: Any, name: str, tenants: list[str]) -> None:
    records = _records_by_tenant(
        value,
        name,
        tenants,
        {
            "tenantId",
            "sourceCount",
            "destinationCount",
            "sourceSearchSha256",
            "destinationSearchSha256",
            "sourceStorageSha256",
            "destinationStorageSha256",
        },
    )
    for tenant in tenants:
        record = records[tenant]
        source_count = _integer(record["sourceCount"], f"{name}.{tenant}.sourceCount")
        destination_count = _integer(
            record["destinationCount"], f"{name}.{tenant}.destinationCount"
        )
        if source_count < 1 or source_count != destination_count:
            _reject(f"{name}.{tenant} count parity failed")
        source_search = _digest(
            record["sourceSearchSha256"], f"{name}.{tenant}.sourceSearchSha256"
        )
        destination_search = _digest(
            record["destinationSearchSha256"],
            f"{name}.{tenant}.destinationSearchSha256",
        )
        if source_search != destination_search:
            _reject(f"{name}.{tenant} search parity failed")
        source_storage = _digest(
            record["sourceStorageSha256"], f"{name}.{tenant}.sourceStorageSha256"
        )
        destination_storage = _digest(
            record["destinationStorageSha256"],
            f"{name}.{tenant}.destinationStorageSha256",
        )
        if source_storage != destination_storage:
            _reject(f"{name}.{tenant} storage parity failed")


def validate_receipt(value: Any) -> None:
    """Reject evidence unless every populated exceptional-pair claim is exact."""

    receipt = _object(
        value,
        "receipt",
        {
            "schemaVersion",
            "kind",
            "transactionId",
            "pair",
            "tenants",
            "snapshot",
            "forward",
            "forwardParity",
            "reverse",
            "predecessorRestart",
            "timing",
        },
    )
    if type(receipt["schemaVersion"]) is not int or receipt["schemaVersion"] != 1:
        _reject("schemaVersion must be integer 1")
    if receipt["kind"] != "flapjack_populated_pair_evidence":
        _reject("receipt kind is not populated-pair evidence")
    transaction_id = _string(receipt["transactionId"], "transactionId")
    if not TRANSACTION_RE.fullmatch(transaction_id):
        _reject("transactionId is not a safe release identifier")

    pair = _object(receipt["pair"], "pair", {"target", "predecessor", "recipe"})
    target = _identity(pair["target"], "pair.target")
    predecessor = _identity(pair["predecessor"], "pair.predecessor")
    if target["targetTriple"] != predecessor["targetTriple"]:
        _reject("pair target triples differ")
    if target["binarySha256"] == predecessor["binarySha256"]:
        _reject("pair must bind distinct binaries")
    if pair["recipe"] != EXCEPTIONAL_RECIPE:
        _reject("pair does not bind the exceptional compatibility recipe")

    tenants = _tenant_ids(receipt)
    snapshots = _records_by_tenant(
        receipt["snapshot"],
        "snapshot",
        tenants,
        {"tenantId", "baselineSeq", "snapshotSha256"},
    )
    baselines: dict[str, int] = {}
    for tenant in tenants:
        baseline = _integer(
            snapshots[tenant]["baselineSeq"], f"snapshot.{tenant}.baselineSeq"
        )
        if baseline < 1:
            _reject(f"snapshot.{tenant} is not populated")
        _digest(snapshots[tenant]["snapshotSha256"], f"snapshot.{tenant}.sha256")
        baselines[tenant] = baseline

    forward = _object(
        receipt["forward"], "forward", {"fence", "tenants", "postZeroSourceSeq"}
    )
    forward_rejection = _validate_fence(
        forward["fence"], "forward fence", transaction_id, "predecessor", tenants
    )
    forward_final = _validate_tail(
        forward["tenants"], "forward", tenants, baselines
    )
    if forward_rejection["afterSeq"] != forward_final[forward_rejection["tenantId"]]:
        _reject("forward fenced write does not bind the final source sequence")
    _validate_post_zero(
        forward["postZeroSourceSeq"], "forward post-zero", tenants, forward_final
    )
    _validate_parity(receipt["forwardParity"], "forward parity", tenants)

    reverse = _object(
        receipt["reverse"],
        "reverse",
        {"fence", "tenants", "postZeroSourceSeq", "completed"},
    )
    reverse_rejection = _validate_fence(
        reverse["fence"], "reverse fence", transaction_id, "target", tenants
    )
    reverse_final = _validate_tail(
        reverse["tenants"], "reverse", tenants, forward_final
    )
    if reverse_rejection["afterSeq"] != reverse_final[reverse_rejection["tenantId"]]:
        _reject("reverse fenced write does not bind the final source sequence")
    _validate_post_zero(
        reverse["postZeroSourceSeq"], "reverse post-zero", tenants, reverse_final
    )
    if reverse["completed"] is not True:
        _reject("reverse tail did not complete")

    restart = _object(
        receipt["predecessorRestart"],
        "predecessorRestart",
        {"status", "observedBuildIdentitySha256", "parity"},
    )
    if restart["status"] != "verified":
        _reject("predecessor restart was not verified")
    observed_identity = _digest(
        restart["observedBuildIdentitySha256"],
        "predecessorRestart.observedBuildIdentitySha256",
    )
    if observed_identity != predecessor["buildIdentitySha256"]:
        _reject("predecessor build identity does not match the pair")
    _validate_parity(restart["parity"], "predecessor restart parity", tenants)

    timing = _object(receipt["timing"], "timing", {"setupSeconds", "behaviorSeconds"})
    _number(timing["setupSeconds"], "timing.setupSeconds")
    behavior = _number(timing["behaviorSeconds"], "timing.behaviorSeconds")
    if behavior > 720:
        _reject("behavior exceeded the twelve-minute budget")


def _no_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            _reject(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _reject_json_constant(value: str) -> NoReturn:
    _reject(f"non-finite JSON constant is forbidden: {value}")


def load_receipt(path: Path) -> Any:
    try:
        return json.loads(
            path.read_text(),
            object_pairs_hook=_no_duplicate_keys,
            parse_constant=_reject_json_constant,
        )
    except (OSError, json.JSONDecodeError) as error:
        raise ContractError(f"could not read receipt: {error}") from error


def _canonical_digest(value: Any) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(payload).hexdigest()


def _canonical_release_json_bytes(value: Any) -> bytes:
    """Encode one JSON value identically in Python and Rust for release hashing.

    The closed v1 format is structural rather than JSON text so runtime-specific
    escaping and float spelling cannot change the digest. Containers carry an
    element count, strings carry a UTF-8 byte length, object keys use Unicode
    scalar order, integers use exact decimal magnitude, and floats use their
    big-endian IEEE-754 bits with floating negative zero normalized to zero.
    """

    if value is None:
        return b"n"
    if value is True:
        return b"t"
    if value is False:
        return b"f"
    if type(value) is int:
        if not -(2**63) <= value <= 2**64 - 1:
            _reject("release JSON integer exceeds the shared i64/u64 domain")
        prefix = b"u" if value >= 0 else b"i"
        return prefix + str(value).encode() + b";"
    if type(value) is float:
        if not math.isfinite(value):
            _reject("release JSON float must be finite")
        normalized = 0.0 if value == 0.0 else value
        return b"d" + struct.pack(">d", normalized).hex().encode()
    if isinstance(value, str):
        encoded = value.encode("utf-8")
        return b"s" + str(len(encoded)).encode() + b":" + encoded
    if isinstance(value, list):
        return (
            b"a"
            + str(len(value)).encode()
            + b":"
            + b"".join(_canonical_release_json_bytes(item) for item in value)
        )
    if isinstance(value, dict):
        if not all(isinstance(key, str) for key in value):
            _reject("release JSON object keys must be strings")
        return (
            b"o"
            + str(len(value)).encode()
            + b":"
            + b"".join(
                _canonical_release_json_bytes(key)
                + _canonical_release_json_bytes(value[key])
                for key in sorted(value)
            )
        )
    _reject(f"release JSON contains unsupported value {type(value).__name__}")


def _canonical_release_operations_digest(operations: Any) -> str:
    return hashlib.sha256(_canonical_release_json_bytes(operations)).hexdigest()


def _file_digest(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _strict_json_bytes(payload: bytes, label: str) -> Any:
    try:
        return json.loads(
            payload.decode(),
            object_pairs_hook=_no_duplicate_keys,
            parse_constant=_reject_json_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ContractError(f"{label} is not strict UTF-8 JSON: {error}") from error


def _artifact_identity(
    binary: Path, manifest_path: Path
) -> tuple[dict[str, str], dict[str, Any]]:
    """Bind the executable to its generated manifest and embedded build record."""

    if not binary.is_file() or not os.access(binary, os.X_OK):
        _reject(f"release binary is not executable: {binary}")
    manifest_bytes = manifest_path.read_bytes()
    manifest = _strict_json_bytes(manifest_bytes, f"manifest {manifest_path}")
    manifest = _object(
        manifest,
        f"manifest {manifest_path}",
        {"schemaVersion", "artifact", "build", "compatibility"},
    )
    if type(manifest["schemaVersion"]) is not int or manifest["schemaVersion"] != 2:
        _reject("release manifest schemaVersion must be integer 2")
    artifact = _object(
        manifest["artifact"],
        "manifest artifact",
        {"file", "target", "arch", "profile", "binarySha256", "sha256"},
    )
    binary_digest = _file_digest(binary)
    if artifact["binarySha256"] != binary_digest:
        _reject("release manifest binary digest does not match executable")
    build_output = subprocess.run(
        [str(binary), "build-info", "--json"],
        check=True,
        capture_output=True,
        timeout=10,
    ).stdout
    embedded_build = _strict_json_bytes(build_output, f"build-info for {binary}")
    if embedded_build != manifest["build"]:
        _reject("release manifest build record does not match executable")
    build = manifest["build"]
    if not isinstance(build, dict):
        _reject("release manifest build record must be an object")
    revision = build.get("revision")
    if build.get("revisionKnown") is not True or not isinstance(revision, str):
        _reject("release build revision must be exact")
    if not REVISION_RE.fullmatch(revision):
        _reject("release build revision must be lowercase 40-hex")
    if build.get("dirtyKnown") is not True or build.get("dirty") is not False:
        _reject("release build must prove a clean source tree")
    if build.get("profile") != "release" or artifact.get("profile") != "release":
        _reject("release pair requires release-profile binaries")
    target = build.get("target")
    if not isinstance(target, str) or artifact.get("target") != target:
        _reject("release manifest target does not match embedded build target")
    compatibility = manifest["compatibility"]
    if not isinstance(compatibility, dict):
        _reject("release manifest compatibility projection must be an object")
    if compatibility.get("target") != target:
        _reject("release manifest compatibility target does not match build target")
    if compatibility.get("dataDisposition") != "preserve":
        _reject("release manifest must preserve data")
    if compatibility.get("mixedVersionReplication") != "not_guaranteed":
        _reject("release manifest mixed-version replication coordinate is unexpected")
    return (
        {
            "targetTriple": target,
            "manifestSha256": hashlib.sha256(manifest_bytes).hexdigest(),
            "binarySha256": binary_digest,
            "buildIdentitySha256": _canonical_digest(embedded_build),
            "revision": revision,
        },
        compatibility,
    )


def _authorized_recipe(
    target_compatibility: Any, predecessor_identity: dict[str, str]
) -> dict[str, str]:
    """Return the sole manifest authorization for the exact predecessor pair."""

    compatibility = _object(
        target_compatibility,
        "target compatibility",
        {
            "schemaVersion",
            "target",
            "predecessors",
            "dataDisposition",
            "mixedVersionReplication",
        },
    )
    if type(compatibility["schemaVersion"]) is not int or compatibility["schemaVersion"] != 1:
        _reject("pair authorization has invalid compatibility schemaVersion")
    predecessors = _list(compatibility["predecessors"], "pair authorization predecessors")
    if len(predecessors) > 3:
        _reject("pair authorization contains more than three predecessors")
    matches = []
    expected_keys = {
        "releaseTag",
        "manifestSha256",
        "binarySha256",
        "transitionMode",
        "forwardTransferMode",
        "rollbackMode",
        "parityProfile",
    }
    seen_coordinates: set[tuple[str, str, str]] = set()
    seen_release_tags: set[str] = set()
    seen_manifest_digests: set[str] = set()
    seen_binary_digests: set[str] = set()
    for index, raw in enumerate(predecessors):
        predecessor = _object(
            raw, f"pair authorization predecessors[{index}]", expected_keys
        )
        release_tag = predecessor["releaseTag"]
        if not isinstance(release_tag, str) or not RELEASE_TAG_RE.fullmatch(release_tag):
            _reject("pair authorization contains an invalid releaseTag")
        manifest_digest = _digest(
            predecessor["manifestSha256"],
            f"pair authorization predecessors[{index}].manifestSha256",
        )
        binary_digest = _digest(
            predecessor["binarySha256"],
            f"pair authorization predecessors[{index}].binarySha256",
        )
        coordinate = (release_tag, manifest_digest, binary_digest)
        if (
            coordinate in seen_coordinates
            or release_tag in seen_release_tags
            or manifest_digest in seen_manifest_digests
            or binary_digest in seen_binary_digests
        ):
            _reject("pair authorization contains a duplicate predecessor")
        seen_coordinates.add(coordinate)
        seen_release_tags.add(release_tag)
        seen_manifest_digests.add(manifest_digest)
        seen_binary_digests.add(binary_digest)
        if (
            manifest_digest == predecessor_identity["manifestSha256"]
            and binary_digest == predecessor_identity["binarySha256"]
        ):
            matches.append(predecessor)
    if len(matches) != 1:
        _reject("target manifest must contain exactly one pair authorization")
    selected = matches[0]
    recipe = {
        "transitionMode": selected["transitionMode"],
        "forwardTransferMode": selected["forwardTransferMode"],
        "rollbackMode": selected["rollbackMode"],
        "parityProfile": selected["parityProfile"],
    }
    if recipe != EXCEPTIONAL_RECIPE:
        _reject("pair authorization does not use the exact exceptional recipe")
    return recipe


def _free_loopback_address() -> str:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        return f"127.0.0.1:{listener.getsockname()[1]}"


class _Server:
    """One disposable exact-binary process with bounded, PID-specific cleanup."""

    def __init__(self, binary: Path, data_dir: Path, log_path: Path) -> None:
        self.address = _free_loopback_address()
        self.log_handle = log_path.open("wb")
        self.process = subprocess.Popen(
            [
                str(binary),
                "--data-dir",
                str(data_dir),
                "--bind-addr",
                self.address,
            ],
            env=_server_environment(),
            stdout=self.log_handle,
            stderr=subprocess.STDOUT,
        )
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                self.close()
                _reject(f"Flapjack exited during startup; inspect {log_path}")
            try:
                _http(self.address, "GET", "/health")
                return
            except ContractError:
                time.sleep(0.05)
        self.close()
        _reject(f"Flapjack did not become healthy; inspect {log_path}")

    def close(self) -> None:
        if self.process.poll() is None:
            self.process.terminate()
            try:
                self.process.wait(timeout=20)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
        if not self.log_handle.closed:
            self.log_handle.close()


def _server_environment() -> dict[str, str]:
    """Return the complete child environment; ambient provider state is denied."""

    return {
        "FLAPJACK_ENV": "development",
        "FLAPJACK_LOG_FORMAT": "json",
        "FLAPJACK_ADMIN_KEY": PROOF_ADMIN_KEY,
    }


def _request(
    address: str,
    method: str,
    path: str,
    *,
    json_body: Any = None,
    raw_body: bytes = None,
    extra_headers: dict[str, str] | None = None,
    accepted_error_statuses: frozenset[int] = frozenset(),
) -> tuple[int, bytes, dict[str, str]]:
    """Issue one authenticated loopback request without exposing its key."""

    if _BEHAVIOR_DEADLINE:
        remaining = _BEHAVIOR_DEADLINE - time.monotonic()
        if remaining <= 0:
            _reject("real populated-pair behavior exceeded twelve minutes")
        timeout = min(15.0, remaining)
    else:
        timeout = 15.0
    if json_body is not None and raw_body is not None:
        _reject("HTTP request cannot have JSON and raw bodies")
    headers = {
        PROOF_APPLICATION_HEADER: PROOF_APPLICATION_ID,
        PROOF_ADMIN_HEADER: PROOF_ADMIN_KEY,
    }
    for name, value in (extra_headers or {}).items():
        normalized = name.lower()
        if normalized in {key.lower() for key in headers} or normalized == "content-type":
            _reject(f"extra HTTP header may not override {normalized}")
        if not isinstance(value, str) or not value:
            _reject(f"extra HTTP header {normalized} must be a non-empty string")
        headers[normalized] = value
    body = raw_body
    if json_body is not None:
        body = json.dumps(json_body, separators=(",", ":")).encode()
        headers["content-type"] = "application/json"
    elif raw_body is not None:
        headers["content-type"] = "application/octet-stream"
    request = urllib.request.Request(
        f"http://{address}{path}", data=body, headers=headers, method=method
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return (
                response.status,
                response.read(),
                {key.lower(): value for key, value in response.headers.items()},
            )
    except urllib.error.HTTPError as error:
        payload_bytes = error.read()
        response_headers = {
            key.lower(): value for key, value in (error.headers.items() if error.headers else [])
        }
        if error.code in accepted_error_statuses:
            return error.code, payload_bytes, response_headers
        payload = payload_bytes.decode(errors="replace")[:500].replace(
            PROOF_ADMIN_KEY, "[REDACTED]"
        )
        raise ContractError(
            f"{method} {path} returned HTTP {error.code}: {payload}"
        ) from error
    except (OSError, TimeoutError) as error:
        raise ContractError(f"{method} {path} failed: {error}") from error


def _http(
    address: str,
    method: str,
    path: str,
    *,
    json_body: Any = None,
    raw_body: bytes = None,
    extra_headers: dict[str, str] | None = None,
) -> tuple[bytes, dict[str, str]]:
    _status, payload, headers = _request(
        address,
        method,
        path,
        json_body=json_body,
        raw_body=raw_body,
        extra_headers=extra_headers,
    )
    return payload, headers


def _json_http(address: str, method: str, path: str, body: Any = None) -> Any:
    payload, _ = _http(address, method, path, json_body=body)
    return _strict_json_bytes(payload, f"{method} {path} response")


def _wait_task(address: str, task_id: int) -> None:
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        task = _json_http(address, "GET", f"/1/tasks/{task_id}")
        status = task.get("status") if isinstance(task, dict) else None
        if status == "published":
            return
        if status == "error":
            _reject(f"write task {task_id} failed: {task.get('error')}")
        time.sleep(0.02)
    _reject(f"write task {task_id} did not publish within 30 seconds")


def _write_documents(address: str, tenant: str, documents: list[dict[str, Any]]) -> None:
    requests = [{"action": "addObject", "body": document} for document in documents]
    response = _json_http(
        address,
        "POST",
        f"/1/indexes/{urllib.parse.quote(tenant)}/batch",
        {"requests": requests},
    )
    task_id = response.get("taskID") if isinstance(response, dict) else None
    if type(task_id) is not int:
        _reject(f"batch response for {tenant} did not contain an integer taskID")
    _wait_task(address, task_id)


def _release_headers(
    tenant: str,
    transaction_id: str,
    *,
    after_seq: int | None = None,
    through_seq: int | None = None,
    payload_sha256: str | None = None,
) -> dict[str, str]:
    if not TRANSACTION_RE.fullmatch(transaction_id):
        _reject("release request transactionId is not safe")
    headers = {
        RELEASE_CONTRACT_HEADER: RELEASE_CONTRACT_V1,
        RELEASE_TENANT_HEADER: tenant,
        RELEASE_TRANSACTION_HEADER: transaction_id,
    }
    if after_seq is not None:
        headers[RELEASE_AFTER_HEADER] = str(_integer(after_seq, "release after sequence"))
    if through_seq is not None:
        headers[RELEASE_THROUGH_HEADER] = str(
            _integer(through_seq, "release through sequence")
        )
    if payload_sha256 is not None:
        headers[RELEASE_PAYLOAD_SHA256_HEADER] = _digest(
            payload_sha256, "release payload digest"
        )
    return headers


def _exact_release_response_headers(
    headers: dict[str, str],
    *,
    tenant: str,
    transaction_id: str,
    after_seq: int,
    through_seq: int,
    status: str,
    payload_sha256: str,
    snapshot_sha256: str | None = None,
) -> None:
    expected = {
        RELEASE_CONTRACT_HEADER: RELEASE_CONTRACT_V1,
        RELEASE_TENANT_HEADER: tenant,
        RELEASE_TRANSACTION_HEADER: transaction_id,
        RELEASE_AFTER_HEADER: str(after_seq),
        RELEASE_THROUGH_HEADER: str(through_seq),
        RELEASE_STATUS_HEADER: status,
        RELEASE_PAYLOAD_SHA256_HEADER: payload_sha256,
    }
    if snapshot_sha256 is not None:
        expected[RELEASE_SNAPSHOT_SHA256_HEADER] = snapshot_sha256
    for name, value in expected.items():
        if headers.get(name) != value:
            _reject(f"release response header {name} did not match exact expected value")
    if snapshot_sha256 is None and RELEASE_SNAPSHOT_SHA256_HEADER in headers:
        _reject("non-snapshot release response carried a snapshot digest")


def _ops(
    address: str, tenant: str, since_seq: int, transaction_id: str
) -> dict[str, Any]:
    query = urllib.parse.urlencode({"tenant_id": tenant, "since_seq": since_seq})
    payload, headers = _http(
        address,
        "GET",
        f"/internal/ops?{query}",
        extra_headers=_release_headers(
            tenant, transaction_id, after_seq=since_seq
        ),
    )
    value = _strict_json_bytes(payload, f"ops response for {tenant}")
    if not isinstance(value, dict):
        _reject(f"ops response for {tenant} is not an object")
    operations = value.get("ops")
    current = value.get("current_seq")
    if not isinstance(operations, list) or type(current) is not int:
        _reject(f"ops response for {tenant} lacks exact operations/current sequence")
    status = headers.get(RELEASE_STATUS_HEADER)
    if status not in {RELEASE_CONTIGUOUS, RELEASE_RESNAPSHOT_REQUIRED}:
        _reject(f"ops response for {tenant} lacks a recognized release status")
    payload_sha256 = _canonical_release_operations_digest(operations)
    _exact_release_response_headers(
        headers,
        tenant=tenant,
        transaction_id=transaction_id,
        after_seq=since_seq,
        through_seq=current,
        status=status,
        payload_sha256=payload_sha256,
    )
    if status == RELEASE_RESNAPSHOT_REQUIRED:
        _reject(f"ops response for {tenant} requires resnapshot before any apply effect")
    return value


def _replicate(
    address: str,
    tenant: str,
    operations: list[Any],
    transaction_id: str,
    after_seq: int,
    through_seq: int,
) -> int:
    payload_sha256 = _canonical_release_operations_digest(operations)
    payload, headers = _http(
        address,
        "POST",
        "/internal/replicate",
        json_body={"tenant_id": tenant, "ops": operations},
        extra_headers=_release_headers(
            tenant,
            transaction_id,
            after_seq=after_seq,
            through_seq=through_seq,
            payload_sha256=payload_sha256,
        ),
    )
    value = _strict_json_bytes(payload, f"replication response for {tenant}")
    ack = value.get("acked_seq") if isinstance(value, dict) else None
    if type(ack) is not int or ack != through_seq:
        _reject(f"replication response for {tenant} lacks exact interval acknowledgement")
    _exact_release_response_headers(
        headers,
        tenant=tenant,
        transaction_id=transaction_id,
        after_seq=after_seq,
        through_seq=through_seq,
        status=RELEASE_ACKNOWLEDGED,
        payload_sha256=payload_sha256,
    )
    return ack


def _fence(address: str, action: str, transaction_id: str) -> None:
    value = _json_http(
        address,
        "POST",
        f"/internal/release-write-fence/{action}",
        {"transactionId": transaction_id},
    )
    expected_active = action == "acquire"
    if value != {"active": expected_active, "transactionId": transaction_id}:
        _reject(f"release fence {action} did not bind transaction {transaction_id}")


def _expect_fenced_write(
    address: str, tenant: str, object_id: str, transaction_id: str
) -> dict[str, Any]:
    """Prove the fence rejects a real mutation and leaves its oplog unchanged."""

    before = _ops(address, tenant, 0, transaction_id).get("current_seq")
    if type(before) is not int:
        _reject(f"fenced write for {tenant} lacks a before sequence")
    status_code, _payload, _headers = _request(
        address,
        "POST",
        f"/1/indexes/{urllib.parse.quote(tenant)}/batch",
        json_body={
            "requests": [
                {
                    "action": "addObject",
                    "body": {
                        "_id": object_id,
                        "title": "this write must be rejected by the release fence",
                    },
                }
            ]
        },
        accepted_error_statuses=frozenset({503}),
    )
    if status_code != 503:
        _reject(f"fenced write for {tenant} returned HTTP {status_code}, expected 503")
    after = _ops(address, tenant, 0, transaction_id).get("current_seq")
    if type(after) is not int or after != before:
        _reject(f"fenced write for {tenant} changed sequence {before} -> {after}")
    return {
        "tenantId": tenant,
        "objectId": object_id,
        "statusCode": status_code,
        "beforeSeq": before,
        "afterSeq": after,
    }


def _write_receipt(output: Path, receipt: dict[str, Any]) -> None:
    serialized = json.dumps(receipt, sort_keys=True, separators=(",", ":")) + "\n"
    if PROOF_ADMIN_KEY in serialized:
        _reject("release receipt must not contain the local proof credential")

    descriptor, temporary_name = tempfile.mkstemp(
        dir=output.parent,
        prefix=f".{output.name}.",
        suffix=".tmp",
    )
    temporary = Path(temporary_name)
    published = False
    try:
        with os.fdopen(descriptor, "wb") as handle:
            handle.write(serialized.encode())
            handle.flush()
            os.fsync(handle.fileno())
            os.fchmod(handle.fileno(), 0o400)
            metadata = os.fstat(handle.fileno())
            if not stat.S_ISREG(metadata.st_mode) or stat.S_IMODE(
                metadata.st_mode
            ) != 0o400:
                _reject("release receipt staging file must be regular mode 0400")
        os.replace(temporary, output)
        published = True
    finally:
        if not published:
            temporary.unlink(missing_ok=True)


def _snapshot(
    address: str, tenant: str, transaction_id: str, expected_through_seq: int
) -> bytes:
    payload, headers = _http(
        address,
        "GET",
        f"/internal/snapshot/{urllib.parse.quote(tenant)}",
        extra_headers=_release_headers(tenant, transaction_id),
    )
    if headers.get("content-type") != "application/gzip" or not payload:
        _reject(f"snapshot for {tenant} was not non-empty gzip content")
    payload_sha256 = hashlib.sha256(payload).hexdigest()
    _exact_release_response_headers(
        headers,
        tenant=tenant,
        transaction_id=transaction_id,
        after_seq=0,
        through_seq=expected_through_seq,
        status=RELEASE_CONTIGUOUS,
        payload_sha256=payload_sha256,
        snapshot_sha256=payload_sha256,
    )
    return payload


def _capture_baselines_and_snapshots(
    address: str, tenants: list[str], transaction_id: str
) -> tuple[dict[str, int], dict[str, bytes]]:
    """Bind each snapshot to its oplog baseline before export quiesces runtime."""

    baselines: dict[str, int] = {}
    snapshots: dict[str, bytes] = {}
    for tenant in tenants:
        baselines[tenant] = _integer(
            _ops(address, tenant, 0, transaction_id).get("current_seq"),
            f"{tenant} snapshot baseline",
            minimum=1,
        )
        snapshots[tenant] = _snapshot(
            address, tenant, transaction_id, baselines[tenant]
        )
    return baselines, snapshots


def _import_snapshot(address: str, tenant: str, payload: bytes) -> None:
    value = _strict_json_bytes(
        _http(
            address,
            "POST",
            f"/1/indexes/{urllib.parse.quote(tenant)}/import",
            raw_body=payload,
        )[0],
        f"snapshot import response for {tenant}",
    )
    if value != {"status": "imported"}:
        _reject(f"snapshot import for {tenant} was not exact")


def _canonical_hits(value: Any, name: str) -> tuple[int, str]:
    if not isinstance(value, dict) or type(value.get("nbHits")) is not int:
        _reject(f"{name} response lacks exact nbHits")
    hits = value.get("hits")
    if not isinstance(hits, list):
        _reject(f"{name} response lacks hits")
    if len(hits) != value["nbHits"]:
        _reject(f"{name} response does not contain the complete hit denominator")
    ordered = sorted(hits, key=lambda hit: str(hit.get("_id", hit.get("objectID", ""))))
    return value["nbHits"], _canonical_digest(ordered)


def _parity(address_a: str, address_b: str, tenants: list[str]) -> list[dict[str, Any]]:
    evidence = []
    for tenant in tenants:
        path = f"/1/indexes/{urllib.parse.quote(tenant)}"
        source_search = _json_http(
            address_a, "POST", f"{path}/query", {"query": "sentinel", "hitsPerPage": 100}
        )
        destination_search = _json_http(
            address_b, "POST", f"{path}/query", {"query": "sentinel", "hitsPerPage": 100}
        )
        source_count, source_search_digest = _canonical_hits(source_search, "search")
        destination_count, destination_search_digest = _canonical_hits(
            destination_search, "search"
        )
        source_browse = _json_http(
            address_a, "POST", f"{path}/browse", {"hitsPerPage": 1000}
        )
        destination_browse = _json_http(
            address_b, "POST", f"{path}/browse", {"hitsPerPage": 1000}
        )
        browse_count, source_storage_digest = _canonical_hits(source_browse, "browse")
        destination_browse_count, destination_storage_digest = _canonical_hits(
            destination_browse, "browse"
        )
        if browse_count != source_count or destination_browse_count != destination_count:
            _reject(f"{tenant} search and storage counts disagree")
        evidence.append(
            {
                "tenantId": tenant,
                "sourceCount": source_count,
                "destinationCount": destination_count,
                "sourceSearchSha256": source_search_digest,
                "destinationSearchSha256": destination_search_digest,
                "sourceStorageSha256": source_storage_digest,
                "destinationStorageSha256": destination_storage_digest,
            }
        )
    return evidence


def _tail_evidence(
    source: str,
    destination: str,
    tenants: list[str],
    baselines: dict[str, int],
    transaction_id: str,
) -> tuple[list[dict[str, Any]], dict[str, int]]:
    evidence = []
    final_sequences = {}
    for tenant in tenants:
        response = _ops(source, tenant, baselines[tenant], transaction_id)
        operations = response.get("ops")
        current = response.get("current_seq")
        oldest = response.get("oldest_retained_seq")
        if not isinstance(operations, list) or type(current) is not int:
            _reject(f"ops response for {tenant} lacks exact tail/current sequence")
        if type(oldest) is not int:
            _reject(f"ops response for {tenant} lacks exact oldest_retained_seq")
        delivered = [operation.get("seq") for operation in operations]
        if not all(type(sequence) is int for sequence in delivered):
            _reject(f"ops response for {tenant} has non-integer sequence")
        ack = _replicate(
            destination,
            tenant,
            operations,
            transaction_id,
            baselines[tenant],
            current,
        )
        evidence.append(
            {
                "tenantId": tenant,
                "baselineSeq": baselines[tenant],
                "oldestRetainedSeq": oldest,
                "deliveredSeqs": delivered,
                "ackedSeq": ack,
                "sourceCurrentSeq": current,
            }
        )
        final_sequences[tenant] = current
    return evidence, final_sequences


def _post_zero(
    address: str,
    tenants: list[str],
    sequences: dict[str, int],
    transaction_id: str,
) -> dict[str, int]:
    observed = {}
    for tenant in tenants:
        response = _ops(address, tenant, sequences[tenant], transaction_id)
        current = response.get("current_seq")
        if response.get("ops") != [] or type(current) is not int:
            _reject(f"{tenant} did not remain at zero lag while fenced")
        observed[tenant] = current
    return observed


def run_pair(
    target_binary: Path,
    target_manifest: Path,
    predecessor_binary: Path,
    predecessor_manifest: Path,
    transaction_id: str,
    output: Path,
) -> dict[str, Any]:
    """Run one bounded local exceptional pair using only public release surfaces."""

    global _BEHAVIOR_DEADLINE

    if not TRANSACTION_RE.fullmatch(transaction_id):
        _reject("transactionId is not a safe release identifier")
    setup_started = time.monotonic()
    target_identity, target_compatibility = _artifact_identity(
        target_binary, target_manifest
    )
    predecessor_identity, _ = _artifact_identity(
        predecessor_binary, predecessor_manifest
    )
    if target_identity["targetTriple"] != predecessor_identity["targetTriple"]:
        _reject("release pair target triples differ")
    if target_identity["binarySha256"] == predecessor_identity["binarySha256"]:
        _reject("release pair binaries are not distinct")
    authorized_recipe = _authorized_recipe(target_compatibility, predecessor_identity)

    tenants = ["rehx2-catalog-a", "rehx2-catalog-b", "rehx2-catalog-c"]
    with tempfile.TemporaryDirectory(prefix="flapjack-rehx2-pair-") as root_value:
        root = Path(root_value)
        predecessor_data = root / "predecessor-data"
        target_data = root / "target-data"
        predecessor = _Server(
            predecessor_binary, predecessor_data, root / "predecessor.log"
        )
        target = None
        target_fenced = False
        predecessor_fenced = False
        try:
            for tenant_index, tenant in enumerate(tenants):
                _write_documents(
                    predecessor.address,
                    tenant,
                    [
                        {
                            "_id": f"seed-{tenant_index}-{document_index}",
                            "title": f"sentinel seed {tenant} {document_index}",
                            "ordinal": document_index,
                        }
                        for document_index in range(3)
                    ],
                )
            baselines, snapshots = _capture_baselines_and_snapshots(
                predecessor.address, tenants, transaction_id
            )

            target = _Server(target_binary, target_data, root / "target.log")
            for tenant in tenants:
                _import_snapshot(target.address, tenant, snapshots[tenant])
            setup_seconds = time.monotonic() - setup_started
            behavior_started = time.monotonic()
            _BEHAVIOR_DEADLINE = behavior_started + 720

            for tenant_index, tenant in enumerate(tenants):
                _write_documents(
                    predecessor.address,
                    tenant,
                    [
                        {
                            "_id": f"forward-{tenant_index}",
                            "title": f"sentinel forward {tenant}",
                            "phase": "forward",
                        }
                    ],
                )
            _fence(predecessor.address, "acquire", transaction_id)
            predecessor_fenced = True
            forward_rejection = _expect_fenced_write(
                predecessor.address,
                tenants[0],
                "blocked-forward-write",
                transaction_id,
            )
            forward_tail, forward_final = _tail_evidence(
                predecessor.address,
                target.address,
                tenants,
                baselines,
                transaction_id,
            )
            forward_zero = _post_zero(
                predecessor.address, tenants, forward_final, transaction_id
            )
            forward_parity = _parity(predecessor.address, target.address, tenants)

            for tenant_index, tenant in enumerate(tenants):
                _write_documents(
                    target.address,
                    tenant,
                    [
                        {
                            "_id": f"reverse-{tenant_index}",
                            "title": f"sentinel reverse {tenant}",
                            "phase": "reverse",
                        }
                    ],
                )
            _fence(target.address, "acquire", transaction_id)
            target_fenced = True
            reverse_rejection = _expect_fenced_write(
                target.address,
                tenants[0],
                "blocked-reverse-write",
                transaction_id,
            )
            _fence(predecessor.address, "release", transaction_id)
            predecessor_fenced = False
            reverse_tail, reverse_final = _tail_evidence(
                target.address,
                predecessor.address,
                tenants,
                forward_final,
                transaction_id,
            )
            reverse_zero = _post_zero(
                target.address, tenants, reverse_final, transaction_id
            )

            predecessor.close()
            predecessor = _Server(
                predecessor_binary, predecessor_data, root / "predecessor-restart.log"
            )
            observed_build = _json_http(
                predecessor.address,
                "GET",
                "/internal/build-info",
            )
            restart_parity = _parity(target.address, predecessor.address, tenants)
            behavior_seconds = time.monotonic() - behavior_started

            receipt = {
                "schemaVersion": 1,
                "kind": "flapjack_populated_pair_evidence",
                "transactionId": transaction_id,
                "pair": {
                    "target": target_identity,
                    "predecessor": predecessor_identity,
                    "recipe": authorized_recipe,
                },
                "tenants": tenants,
                "snapshot": [
                    {
                        "tenantId": tenant,
                        "baselineSeq": baselines[tenant],
                        "snapshotSha256": hashlib.sha256(snapshots[tenant]).hexdigest(),
                    }
                    for tenant in tenants
                ],
                "forward": {
                    "fence": {
                        "transactionId": transaction_id,
                        "sourceRole": "predecessor",
                        "active": True,
                        "writeRejection": forward_rejection,
                    },
                    "tenants": forward_tail,
                    "postZeroSourceSeq": forward_zero,
                },
                "forwardParity": forward_parity,
                "reverse": {
                    "fence": {
                        "transactionId": transaction_id,
                        "sourceRole": "target",
                        "active": True,
                        "writeRejection": reverse_rejection,
                    },
                    "tenants": reverse_tail,
                    "postZeroSourceSeq": reverse_zero,
                    "completed": True,
                },
                "predecessorRestart": {
                    "status": "verified",
                    "observedBuildIdentitySha256": _canonical_digest(observed_build),
                    "parity": restart_parity,
                },
                "timing": {
                    "setupSeconds": round(setup_seconds, 3),
                    "behaviorSeconds": round(behavior_seconds, 3),
                },
            }
            validate_receipt(receipt)
            _write_receipt(output, receipt)
            return receipt
        finally:
            _BEHAVIOR_DEADLINE = 0.0
            if target is not None:
                if target_fenced and target.process.poll() is None:
                    try:
                        _fence(target.address, "release", transaction_id)
                    except ContractError:
                        pass
                target.close()
            if predecessor_fenced and predecessor.process.poll() is None:
                try:
                    _fence(predecessor.address, "release", transaction_id)
                except ContractError:
                    pass
            predecessor.close()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    validate = subparsers.add_parser("validate-receipt")
    validate.add_argument("receipt", type=Path)
    run = subparsers.add_parser("run")
    run.add_argument("--target-binary", type=Path, required=True)
    run.add_argument("--target-manifest", type=Path, required=True)
    run.add_argument("--predecessor-binary", type=Path, required=True)
    run.add_argument("--predecessor-manifest", type=Path, required=True)
    run.add_argument("--transaction-id", required=True)
    run.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    try:
        if args.command == "validate-receipt":
            validate_receipt(load_receipt(args.receipt))
        else:
            run_pair(
                args.target_binary,
                args.target_manifest,
                args.predecessor_binary,
                args.predecessor_manifest,
                args.transaction_id,
                args.output,
            )
    except ContractError as error:
        parser.error(str(error))
    print("release populated-pair receipt: PASS")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
