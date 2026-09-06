#!/usr/bin/env python3
"""Fast mutation-sensitive contract for the exceptional populated-pair proof.

The real proof is intentionally release-only and launches two Flapjack
processes.  This suite exercises its receipt validator with synthetic evidence
so routine feedback stays below ten seconds and every safety claim remains
load-bearing.
"""

from __future__ import annotations

import copy
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
import urllib.error
import urllib.request
from pathlib import Path
from unittest import mock

import release_populated_pair as pair
from release_populated_pair import (
    ContractError,
    EXCEPTIONAL_RECIPE,
    _Server,
    _authorized_recipe,
    _canonical_hits,
    _server_environment,
    validate_receipt,
)


def digest(character: str) -> str:
    return character * 64


def tenant_phase(tenant: str, baseline: int, final: int) -> dict:
    return {
        "tenantId": tenant,
        "baselineSeq": baseline,
        "oldestRetainedSeq": baseline + 1,
        "deliveredSeqs": list(range(baseline + 1, final + 1)),
        "ackedSeq": final,
        "sourceCurrentSeq": final,
    }


def parity(tenant: str, marker: str) -> dict:
    return {
        "tenantId": tenant,
        "sourceCount": 3,
        "destinationCount": 3,
        "sourceSearchSha256": digest(marker),
        "destinationSearchSha256": digest(marker),
        "sourceStorageSha256": digest(marker.upper()),
        "destinationStorageSha256": digest(marker.upper()),
    }


def valid_receipt() -> dict:
    tenants = ["catalog-a", "catalog-b", "catalog-c"]
    return {
        "schemaVersion": 1,
        "kind": "flapjack_populated_pair_evidence",
        "transactionId": "rehx2-pair-0001",
        "pair": {
            "target": {
                "targetTriple": "aarch64-apple-darwin",
                "manifestSha256": digest("a"),
                "binarySha256": digest("b"),
                "buildIdentitySha256": digest("c"),
                "revision": "1" * 40,
            },
            "predecessor": {
                "targetTriple": "aarch64-apple-darwin",
                "manifestSha256": digest("d"),
                "binarySha256": digest("e"),
                "buildIdentitySha256": digest("f"),
                "revision": "2" * 40,
            },
            "recipe": {
                "transitionMode": "exceptional_blue_green",
                "forwardTransferMode": "snapshot_then_tail_replication",
                "rollbackMode": "reverse_tail_to_retained_predecessor",
                "parityProfile": "populated_engine_exact_v1",
            },
        },
        "tenants": tenants,
        "snapshot": [
            {"tenantId": tenant, "baselineSeq": 2, "snapshotSha256": digest(str(index + 3))}
            for index, tenant in enumerate(tenants)
        ],
        "forward": {
            "fence": {
                "transactionId": "rehx2-pair-0001",
                "sourceRole": "predecessor",
                "active": True,
                "writeRejection": {
                    "tenantId": "catalog-a",
                    "objectId": "blocked-forward",
                    "statusCode": 503,
                    "beforeSeq": 4,
                    "afterSeq": 4,
                },
            },
            "tenants": [tenant_phase(tenant, 2, 4) for tenant in tenants],
            "postZeroSourceSeq": {tenant: 4 for tenant in tenants},
        },
        "forwardParity": [
            parity(tenant, marker)
            for tenant, marker in zip(tenants, ("6", "7", "8"))
        ],
        "reverse": {
            "fence": {
                "transactionId": "rehx2-pair-0001",
                "sourceRole": "target",
                "active": True,
                "writeRejection": {
                    "tenantId": "catalog-a",
                    "objectId": "blocked-reverse",
                    "statusCode": 503,
                    "beforeSeq": 5,
                    "afterSeq": 5,
                },
            },
            "tenants": [tenant_phase(tenant, 4, 5) for tenant in tenants],
            "postZeroSourceSeq": {tenant: 5 for tenant in tenants},
            "completed": True,
        },
        "predecessorRestart": {
            "status": "verified",
            "observedBuildIdentitySha256": digest("f"),
            "parity": [
                parity(tenant, marker)
                for tenant, marker in zip(tenants, ("9", "a", "b"))
            ],
        },
        "timing": {"setupSeconds": 42.0, "behaviorSeconds": 91.5},
    }


class PopulatedPairContractTests(unittest.TestCase):
    def assert_rejected(self, mutate, fragment: str) -> None:
        receipt = valid_receipt()
        mutate(receipt)
        with self.assertRaisesRegex(ContractError, fragment):
            validate_receipt(receipt)

    def test_complete_receipt_is_accepted(self) -> None:
        validate_receipt(valid_receipt())

    def test_snapshot_baseline_is_captured_before_quiescing_export(self) -> None:
        tenants = ["catalog-a", "catalog-b", "catalog-c"]
        current_sequences = {tenant: index + 2 for index, tenant in enumerate(tenants)}
        events: list[tuple[str, str]] = []

        def get_ops(
            _address: str, tenant: str, since_seq: int, transaction_id: str
        ) -> dict:
            self.assertEqual(since_seq, 0)
            self.assertEqual(transaction_id, "rehx2-pair-0001")
            events.append(("ops", tenant))
            current = current_sequences[tenant]
            if current is None:
                raise ContractError("snapshot quiesce cleared the runtime oplog owner")
            return {"current_seq": current}

        def export_snapshot(
            _address: str,
            tenant: str,
            transaction_id: str,
            expected_through_seq: int,
        ) -> bytes:
            self.assertEqual(transaction_id, "rehx2-pair-0001")
            self.assertEqual(expected_through_seq, current_sequences[tenant])
            events.append(("snapshot", tenant))
            current_sequences[tenant] = None
            return f"snapshot:{tenant}".encode()

        with mock.patch.object(pair, "_ops", side_effect=get_ops), mock.patch.object(
            pair, "_snapshot", side_effect=export_snapshot
        ):
            baselines, snapshots = pair._capture_baselines_and_snapshots(
                "127.0.0.1:7700", tenants, "rehx2-pair-0001"
            )

        self.assertEqual(baselines, {"catalog-a": 2, "catalog-b": 3, "catalog-c": 4})
        self.assertEqual(
            snapshots,
            {tenant: f"snapshot:{tenant}".encode() for tenant in tenants},
        )
        self.assertEqual(
            events,
            [
                ("ops", "catalog-a"),
                ("snapshot", "catalog-a"),
                ("ops", "catalog-b"),
                ("snapshot", "catalog-b"),
                ("ops", "catalog-c"),
                ("snapshot", "catalog-c"),
            ],
        )

    def test_omitted_tenant_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["tenants"].pop(),
            "forward tenant set",
        )

    def test_retention_gap_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["tenants"][0].update(
                oldestRetainedSeq=4
            ),
            "retention gap",
        )

    def test_out_of_order_tail_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["tenants"][0].update(
                deliveredSeqs=[3, 5, 4]
            ),
            "contiguous",
        )

    def test_false_ack_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["tenants"][0].update(ackedSeq=5),
            "acknowledgement",
        )

    def test_post_zero_source_advance_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["postZeroSourceSeq"].update(
                {"catalog-a": 5}
            ),
            "advanced after zero lag",
        )

    def test_forward_fence_must_reject_a_real_write_without_sequence_advance(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["fence"]["writeRejection"].update(
                statusCode=200, afterSeq=5
            ),
            "fenced write",
        )

    def test_reverse_fence_must_reject_a_real_write_without_sequence_advance(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["reverse"]["fence"]["writeRejection"].update(
                afterSeq=6
            ),
            "fenced write",
        )

    def test_missing_retention_boundary_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forward"]["tenants"][0].pop(
                "oldestRetainedSeq"
            ),
            "oldestRetainedSeq",
        )

    def test_count_divergence_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forwardParity"][0].update(destinationCount=2),
            "count parity",
        )

    def test_search_divergence_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forwardParity"][0].update(
                destinationSearchSha256=digest("0")
            ),
            "search parity",
        )

    def test_storage_divergence_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["forwardParity"][0].update(
                destinationStorageSha256=digest("0")
            ),
            "storage parity",
        )

    def test_reverse_failure_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["reverse"].update(completed=False),
            "reverse tail",
        )

    def test_wrong_predecessor_restart_identity_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["predecessorRestart"].update(
                observedBuildIdentitySha256=digest("0")
            ),
            "predecessor build identity",
        )

    def test_pair_identity_aliasing_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["pair"]["target"].update(
                binarySha256=digest("e")
            ),
            "distinct binaries",
        )

    def test_behavior_budget_is_enforced(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt["timing"].update(behaviorSeconds=720.001),
            "twelve-minute",
        )

    def test_behavior_budget_rejects_non_finite_numbers_directly(self) -> None:
        for value in (float("nan"), float("inf"), float("-inf")):
            with self.subTest(value=value):
                self.assert_rejected(
                    lambda receipt, value=value: receipt["timing"].update(
                        behaviorSeconds=value
                    ),
                    "finite non-negative",
                )

    def test_boolean_schema_version_is_rejected(self) -> None:
        self.assert_rejected(
            lambda receipt: receipt.update(schemaVersion=True),
            "schemaVersion",
        )

    def test_truncated_hit_page_is_rejected(self) -> None:
        with self.assertRaisesRegex(ContractError, "complete hit denominator"):
            _canonical_hits(
                {"nbHits": 2, "hits": [{"_id": "only-one"}]}, "browse"
            )

    def test_target_manifest_must_authorize_exact_predecessor_once(self) -> None:
        identity = {
            "manifestSha256": digest("d"),
            "binarySha256": digest("e"),
        }
        compatibility = {
            "schemaVersion": 1,
            "target": "aarch64-apple-darwin",
            "predecessors": [
                {
                    "releaseTag": "v1.0.15",
                    "manifestSha256": digest("d"),
                    "binarySha256": digest("e"),
                    **EXCEPTIONAL_RECIPE,
                }
            ],
            "dataDisposition": "preserve",
            "mixedVersionReplication": "not_guaranteed",
        }
        self.assertEqual(_authorized_recipe(compatibility, identity), EXCEPTIONAL_RECIPE)
        for mutation in ("absent", "duplicate", "wrong-recipe"):
            specimen = copy.deepcopy(compatibility)
            if mutation == "absent":
                specimen["predecessors"] = []
            elif mutation == "duplicate":
                specimen["predecessors"].append(
                    copy.deepcopy(specimen["predecessors"][0])
                )
            else:
                specimen["predecessors"][0]["rollbackMode"] = (
                    "restore_pre_upgrade_backup"
                )
            with self.subTest(mutation=mutation):
                with self.assertRaisesRegex(ContractError, "authorization"):
                    _authorized_recipe(specimen, identity)

    def test_cli_rejects_nan_and_infinity_json_constants(self) -> None:
        script = Path(__file__).with_name("release_populated_pair.py")
        for value in (float("nan"), float("inf"), float("-inf")):
            with self.subTest(value=value), tempfile.TemporaryDirectory() as directory:
                receipt = valid_receipt()
                receipt["timing"]["behaviorSeconds"] = value
                path = Path(directory) / "receipt.json"
                path.write_text(json.dumps(receipt))
                result = subprocess.run(
                    [sys.executable, str(script), "validate-receipt", str(path)],
                    capture_output=True,
                    text=True,
                    timeout=3,
                )
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn("non-finite JSON constant", result.stderr)

    def test_disposable_servers_receive_no_ambient_provider_coordinates(self) -> None:
        hostile = {
            "AWS_ACCESS_KEY_ID": "must-not-pass",
            "AWS_SECRET_ACCESS_KEY": "must-not-pass",
            "FLAPJACK_S3_BUCKET": "must-not-pass",
            "FLAPJACK_REPLICATION_API_KEY": "must-not-pass",
            "FLAPJACK_BOOTSTRAP_PEER": "must-not-pass",
            "OTEL_EXPORTER_OTLP_ENDPOINT": "must-not-pass",
            "AWS_SES_REGION": "must-not-pass",
            "FLAPJACK_AI_API_KEY": "must-not-pass",
            "FLAPJACK_ADMIN_KEY": "ambient-must-not-pass",
        }
        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.dict(os.environ, hostile, clear=False), mock.patch(
                "release_populated_pair.subprocess.Popen"
            ) as popen, mock.patch(
                "release_populated_pair._http", return_value=(b"", {})
            ):
                process = popen.return_value
                process.poll.return_value = None
                process.wait.return_value = 0
                server = _Server(
                    Path("/exact/release/flapjack"),
                    Path(directory) / "data",
                    Path(directory) / "server.log",
                )
                server.close()
        child_environment = popen.call_args.kwargs["env"]
        self.assertEqual(child_environment, _server_environment())
        self.assertEqual(
            child_environment,
            {
                "FLAPJACK_ENV": "development",
                "FLAPJACK_LOG_FORMAT": "json",
                "FLAPJACK_ADMIN_KEY": pair.PROOF_ADMIN_KEY,
            },
        )
        self.assertTrue(
            (set(hostile) - {"FLAPJACK_ADMIN_KEY"}).isdisjoint(child_environment)
        )
        self.assertNotEqual(
            child_environment["FLAPJACK_ADMIN_KEY"], hostile["FLAPJACK_ADMIN_KEY"]
        )
        argv = popen.call_args.args[0]
        self.assertNotIn("--no-auth", argv)
        self.assertNotIn(pair.PROOF_ADMIN_KEY, argv)

    def test_every_protected_request_uses_fixed_application_and_admin_headers(self) -> None:
        expected_application_id = "flapjack"
        expected_key = "rehx2-local-loopback-admin-v1"
        self.assertEqual(pair.PROOF_APPLICATION_ID, expected_application_id)
        self.assertEqual(pair.PROOF_ADMIN_KEY, expected_key)

        class Response:
            status = 200
            headers: dict[str, str] = {}

            def __enter__(self):
                return self

            def __exit__(self, *_args):
                return False

            def read(self) -> bytes:
                return b"{}"

        def authenticated_urlopen(request, *, timeout):
            self.assertGreater(timeout, 0)
            if (
                request.get_header("X-algolia-application-id")
                != expected_application_id
                or request.get_header("X-algolia-api-key") != expected_key
            ):
                raise urllib.error.HTTPError(
                    request.full_url, 403, "forbidden", {}, io.BytesIO(b"")
                )
            return Response()

        with mock.patch(
            "release_populated_pair.urllib.request.urlopen",
            side_effect=authenticated_urlopen,
        ):
            pair._http(
                "127.0.0.1:7700",
                "GET",
                "/internal/release-write-fence/status",
            )
            mutants = (
                ("", expected_application_id),
                ("wrong-local-key", expected_application_id),
                (expected_key, ""),
                (expected_key, "wrong-application"),
            )
            for key, application_id in mutants:
                with self.subTest(
                    key=key, application_id=application_id
                ), mock.patch.object(
                    pair, "PROOF_ADMIN_KEY", key
                ), mock.patch.object(
                    pair, "PROOF_APPLICATION_ID", application_id
                ), self.assertRaisesRegex(ContractError, "HTTP 403"):
                    pair._http(
                        "127.0.0.1:7700",
                        "GET",
                        "/internal/release-write-fence/status",
                    )

    def test_fenced_write_uses_the_central_authenticated_request_owner(self) -> None:
        calls: list[tuple[str, str, frozenset[int]]] = []

        def request(_address, method, path, **kwargs):
            calls.append(
                (method, path, frozenset(kwargs.get("accepted_error_statuses", set())))
            )
            return 503, b"", {}

        with mock.patch.object(
            pair, "_ops", side_effect=[{"current_seq": 4}, {"current_seq": 4}]
        ), mock.patch.object(pair, "_request", side_effect=request):
            rejection = pair._expect_fenced_write(
                "127.0.0.1:7700",
                "catalog-a",
                "blocked-forward",
                "rehx2-pair-0001",
            )

        self.assertEqual(rejection["statusCode"], 503)
        self.assertEqual(
            calls,
            [
                (
                    "POST",
                    "/1/indexes/catalog-a/batch",
                    frozenset({503}),
                )
            ],
        )

    def test_release_apply_binds_exact_interval_transaction_payload_and_ack(self) -> None:
        operations = [{"seq": 3, "op": "delete", "object_id": "one"}]
        payload_digest = pair._canonical_release_operations_digest(operations)
        observed: list[dict] = []

        def http(_address, method, path, **kwargs):
            observed.append({"method": method, "path": path, **kwargs})
            return (
                json.dumps({"acked_seq": 3}).encode(),
                {
                    pair.RELEASE_CONTRACT_HEADER: pair.RELEASE_CONTRACT_V1,
                    pair.RELEASE_TENANT_HEADER: "catalog-a",
                    pair.RELEASE_TRANSACTION_HEADER: "rehx2-pair-0001",
                    pair.RELEASE_AFTER_HEADER: "2",
                    pair.RELEASE_THROUGH_HEADER: "3",
                    pair.RELEASE_STATUS_HEADER: pair.RELEASE_ACKNOWLEDGED,
                    pair.RELEASE_PAYLOAD_SHA256_HEADER: payload_digest,
                },
            )

        with mock.patch.object(pair, "_http", side_effect=http):
            self.assertEqual(
                pair._replicate(
                    "127.0.0.1:7700",
                    "catalog-a",
                    operations,
                    "rehx2-pair-0001",
                    2,
                    3,
                ),
                3,
            )

        self.assertEqual(len(observed), 1)
        request = observed[0]
        self.assertEqual((request["method"], request["path"]), ("POST", "/internal/replicate"))
        self.assertEqual(request["json_body"], {"tenant_id": "catalog-a", "ops": operations})
        self.assertEqual(
            request["extra_headers"],
            {
                pair.RELEASE_CONTRACT_HEADER: pair.RELEASE_CONTRACT_V1,
                pair.RELEASE_TENANT_HEADER: "catalog-a",
                pair.RELEASE_TRANSACTION_HEADER: "rehx2-pair-0001",
                pair.RELEASE_AFTER_HEADER: "2",
                pair.RELEASE_THROUGH_HEADER: "3",
                pair.RELEASE_PAYLOAD_SHA256_HEADER: payload_digest,
            },
        )

    def test_operation_digest_matches_shared_unicode_float_and_negative_zero_fixture(self) -> None:
        operation = {
            "seq": 1,
            "timestamp_ms": 1000,
            "node_id": "source",
            "tenant_id": "products",
            "op_type": "upsert",
            "payload": {
                "objectID": "one",
                "body": {
                    "_id": "one",
                    "name": "One",
                    "unicode": "é雪𝄞",
                    "small": 1e-7,
                    "large": 1e21,
                    "negativeZero": -0.0,
                },
            },
        }
        self.assertEqual(
            pair._canonical_release_operations_digest([operation]),
            "1cf6e676ad9fd2ce3af1b642d94825f81f6a0f1773533431dea481fad780e42f",
        )
        self.assertEqual(
            pair._canonical_release_operations_digest(json.loads('{"value":1e-7}')),
            pair._canonical_release_operations_digest(
                json.loads('{"value":0.0000001}')
            ),
        )
        self.assertEqual(
            pair._canonical_release_operations_digest({"value": -0.0}),
            pair._canonical_release_operations_digest({"value": 0.0}),
        )
        self.assertEqual(
            pair._canonical_release_operations_digest(
                {"é": 1, "雪": 2, "𝄞": 3, "a": 4}
            ),
            "c4f655a3795ba4dc6be1e92392fb020676b7cf60f4381c90bf22595a3e75db64",
        )

    def test_release_apply_rejects_every_mutated_ack_header(self) -> None:
        operations = [{"seq": 3, "op": "delete", "object_id": "one"}]
        payload_digest = pair._canonical_release_operations_digest(operations)
        valid_headers = {
            pair.RELEASE_CONTRACT_HEADER: pair.RELEASE_CONTRACT_V1,
            pair.RELEASE_TENANT_HEADER: "catalog-a",
            pair.RELEASE_TRANSACTION_HEADER: "rehx2-pair-0001",
            pair.RELEASE_AFTER_HEADER: "2",
            pair.RELEASE_THROUGH_HEADER: "3",
            pair.RELEASE_STATUS_HEADER: pair.RELEASE_ACKNOWLEDGED,
            pair.RELEASE_PAYLOAD_SHA256_HEADER: payload_digest,
        }
        for header in valid_headers:
            with self.subTest(header=header):
                mutated = dict(valid_headers)
                mutated[header] = "mutated"
                with mock.patch.object(
                    pair,
                    "_http",
                    return_value=(json.dumps({"acked_seq": 3}).encode(), mutated),
                ), self.assertRaisesRegex(ContractError, "release response header"):
                    pair._replicate(
                        "127.0.0.1:7700",
                        "catalog-a",
                        operations,
                        "rehx2-pair-0001",
                        2,
                        3,
                    )

    def test_resnapshot_status_stops_before_any_release_apply(self) -> None:
        payload_digest = pair._canonical_release_operations_digest([])
        headers = {
            pair.RELEASE_CONTRACT_HEADER: pair.RELEASE_CONTRACT_V1,
            pair.RELEASE_TENANT_HEADER: "catalog-a",
            pair.RELEASE_TRANSACTION_HEADER: "rehx2-pair-0001",
            pair.RELEASE_AFTER_HEADER: "2",
            pair.RELEASE_THROUGH_HEADER: "5",
            pair.RELEASE_STATUS_HEADER: pair.RELEASE_RESNAPSHOT_REQUIRED,
            pair.RELEASE_PAYLOAD_SHA256_HEADER: payload_digest,
        }
        payload = json.dumps(
            {"tenant_id": "catalog-a", "ops": [], "current_seq": 5}
        ).encode()
        with mock.patch.object(
            pair, "_http", return_value=(payload, headers)
        ), mock.patch.object(pair, "_replicate") as replicate, self.assertRaisesRegex(
            ContractError, "resnapshot before any apply effect"
        ):
            pair._ops(
                "127.0.0.1:7700", "catalog-a", 2, "rehx2-pair-0001"
            )
        replicate.assert_not_called()

    def test_release_snapshot_validates_transaction_interval_and_digest_headers(self) -> None:
        payload = b"not-a-real-gzip-but-nonempty"
        digest_value = pair.hashlib.sha256(payload).hexdigest()
        headers = {
            "content-type": "application/gzip",
            pair.RELEASE_CONTRACT_HEADER: pair.RELEASE_CONTRACT_V1,
            pair.RELEASE_TENANT_HEADER: "catalog-a",
            pair.RELEASE_TRANSACTION_HEADER: "rehx2-pair-0001",
            pair.RELEASE_AFTER_HEADER: "0",
            pair.RELEASE_THROUGH_HEADER: "4",
            pair.RELEASE_STATUS_HEADER: pair.RELEASE_CONTIGUOUS,
            pair.RELEASE_PAYLOAD_SHA256_HEADER: digest_value,
            pair.RELEASE_SNAPSHOT_SHA256_HEADER: digest_value,
        }
        with mock.patch.object(pair, "_http", return_value=(payload, headers)):
            self.assertEqual(
                pair._snapshot(
                    "127.0.0.1:7700", "catalog-a", "rehx2-pair-0001", 4
                ),
                payload,
            )
        for header in (
            pair.RELEASE_TRANSACTION_HEADER,
            pair.RELEASE_THROUGH_HEADER,
            pair.RELEASE_PAYLOAD_SHA256_HEADER,
            pair.RELEASE_SNAPSHOT_SHA256_HEADER,
        ):
            with self.subTest(header=header):
                mutated = dict(headers)
                mutated[header] = "mutated"
                with mock.patch.object(
                    pair, "_http", return_value=(payload, mutated)
                ), self.assertRaisesRegex(ContractError, "release response header"):
                    pair._snapshot(
                        "127.0.0.1:7700",
                        "catalog-a",
                        "rehx2-pair-0001",
                        4,
                    )

    def test_errors_and_receipts_never_expose_the_local_proof_key(self) -> None:
        request = urllib.request.Request("http://127.0.0.1:7700/health")
        reflected = urllib.error.HTTPError(
            request.full_url,
            403,
            "forbidden",
            {},
            None,
        )
        reflected.read = mock.Mock(
            return_value=f"denied {pair.PROOF_ADMIN_KEY}".encode()
        )
        with mock.patch(
            "release_populated_pair.urllib.request.urlopen", side_effect=reflected
        ), self.assertRaises(ContractError) as raised:
            pair._http("127.0.0.1:7700", "GET", "/health")
        self.assertNotIn(pair.PROOF_ADMIN_KEY, str(raised.exception))

        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "receipt.json"
            pair._write_receipt(output, valid_receipt())
            self.assertNotIn(pair.PROOF_ADMIN_KEY, output.read_text())
            specimen = valid_receipt()
            specimen["unexpected"] = pair.PROOF_ADMIN_KEY
            with self.assertRaisesRegex(ContractError, "proof credential"):
                pair._write_receipt(output, specimen)

    def test_receipt_is_atomically_published_as_an_exact_read_only_file(self) -> None:
        expected = json.dumps(
            valid_receipt(), sort_keys=True, separators=(",", ":")
        ).encode() + b"\n"
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            output = root / "receipt.json"
            victim = root / "must-not-be-followed"
            victim.write_bytes(b"unchanged")
            output.symlink_to(victim)
            observed_replace_source: list[tuple[bool, int]] = []
            real_replace = os.replace

            def replace(source, destination):
                metadata = os.lstat(source)
                observed_replace_source.append(
                    (stat.S_ISREG(metadata.st_mode), stat.S_IMODE(metadata.st_mode))
                )
                real_replace(source, destination)

            with mock.patch(
                "release_populated_pair.os.replace", side_effect=replace
            ):
                pair._write_receipt(output, valid_receipt())

            metadata = os.lstat(output)
            self.assertEqual(observed_replace_source, [(True, 0o400)])
            self.assertTrue(stat.S_ISREG(metadata.st_mode))
            self.assertEqual(stat.S_IMODE(metadata.st_mode), 0o400)
            self.assertEqual(output.read_bytes(), expected)
            self.assertEqual(victim.read_bytes(), b"unchanged")

    def test_receipt_rejects_a_failed_read_only_seal_before_publication(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / "receipt.json"
            with mock.patch("release_populated_pair.os.fchmod"):
                with self.assertRaisesRegex(ContractError, "mode 0400"):
                    pair._write_receipt(output, valid_receipt())
            self.assertFalse(output.exists())


if __name__ == "__main__":
    unittest.main()
