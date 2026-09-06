import copy
import json
import sys
import tempfile
import traceback
import types
import unittest
from pathlib import Path
from unittest import mock


SDK_TEST_DIR = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SDK_TEST_DIR))


class FakeCallType:
    READ = 1
    WRITE = 2


class FakeHost:
    def __init__(self, url, scheme="https", port=None, priority=0, accept=None):
        self.url = url
        self.scheme = scheme
        self.port = port
        self.priority = priority
        self.accept = accept


class FakeHostsCollection:
    def __init__(self, hosts, reorder_hosts=False):
        self.hosts = hosts
        self.reorder_hosts = reorder_hosts


class FakeSearchConfig:
    def __init__(self, app_id, api_key, transformation_options=None):
        self.app_id = app_id
        self.api_key = api_key
        self.transformation_options = transformation_options
        self.hosts = None


class FakeSearchClientSync:
    created_config = None
    client_to_return = object()

    @classmethod
    def create_with_config(cls, config):
        cls.created_config = config
        return cls.client_to_return


algoliasearch = types.ModuleType("algoliasearch")
algoliasearch_http = types.ModuleType("algoliasearch.http")
algoliasearch_hosts = types.ModuleType("algoliasearch.http.hosts")
algoliasearch_hosts.CallType = FakeCallType
algoliasearch_hosts.Host = FakeHost
algoliasearch_hosts.HostsCollection = FakeHostsCollection
algoliasearch_search = types.ModuleType("algoliasearch.search")
algoliasearch_client = types.ModuleType("algoliasearch.search.client")
algoliasearch_client.SearchClientSync = FakeSearchClientSync
algoliasearch_config = types.ModuleType("algoliasearch.search.config")
algoliasearch_config.SearchConfig = FakeSearchConfig
sys.modules.update(
    {
        "algoliasearch": algoliasearch,
        "algoliasearch.http": algoliasearch_http,
        "algoliasearch.http.hosts": algoliasearch_hosts,
        "algoliasearch.search": algoliasearch_search,
        "algoliasearch.search.client": algoliasearch_client,
        "algoliasearch.search.config": algoliasearch_config,
    }
)

import python_client_contract_test as contract


FIXTURE_PATH = SDK_TEST_DIR / "fixtures" / "official_client_contract.json"


class FakeGeneratedResponse:
    def __init__(self, payload, **attributes):
        self._payload = payload
        for name, value in attributes.items():
            setattr(self, name, value)

    def to_dict(self):
        return copy.deepcopy(self._payload)


def load_fixture():
    return json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))


def valid_search_payload(fixture):
    expected = fixture["expected"]
    hits = [
        {"name": name, "objectID": object_id}
        for name, object_id in zip(
            expected["laptopNames"], expected["laptopObjectIDs"], strict=True
        )
    ]
    return {
        "results": [
            {
                "hits": hits,
                "nbHits": expected["laptopNbHits"],
            }
        ]
    }


def valid_facet_payload(fixture):
    return {
        "facetHits": copy.deepcopy(fixture["expected"]["brandFacetHits"]),
        "exhaustiveFacetsCount": True,
    }


class FakeClient:
    def __init__(self, fixture):
        self.fixture = fixture
        self.calls = []
        self.settings_response = FakeGeneratedResponse(
            {"taskID": 101, "updatedAt": "2026-09-06T00:00:00Z"},
            task_id=101,
            updated_at="2026-09-06T00:00:00Z",
        )
        self.save_responses = None
        self.wait_response = FakeGeneratedResponse({"status": "published"})
        self.search_response = FakeGeneratedResponse(valid_search_payload(fixture))
        self.facet_response = FakeGeneratedResponse(valid_facet_payload(fixture))
        self.deletion_response = FakeGeneratedResponse(
            {"taskID": 301, "deletedAt": "2026-09-06T00:01:00Z"},
            task_id=301,
            deleted_at="2026-09-06T00:01:00Z",
        )
        self.set_settings_error = None
        self.search_error = None
        self.delete_error = None
        self.close_error = None

    def set_settings(self, **kwargs):
        self.calls.append(("set_settings", kwargs))
        if self.set_settings_error:
            raise self.set_settings_error
        return self.settings_response

    def wait_for_task(self, **kwargs):
        self.calls.append(("wait_for_task", kwargs))
        return self.wait_response

    def save_objects(self, **kwargs):
        self.calls.append(("save_objects", kwargs))
        if self.save_responses is not None:
            return self.save_responses
        object_ids = [item["objectID"] for item in kwargs["objects"]]
        return [
            FakeGeneratedResponse(
                {"taskID": 201, "objectIDs": object_ids[:2]},
                task_id=201,
                object_ids=object_ids[:2],
            ),
            FakeGeneratedResponse(
                {"taskID": 202, "objectIDs": object_ids[2:]},
                task_id=202,
                object_ids=object_ids[2:],
            ),
        ]

    def search(self, **kwargs):
        self.calls.append(("search", kwargs))
        if self.search_error:
            raise self.search_error
        return self.search_response

    def search_for_facet_values(self, **kwargs):
        self.calls.append(("search_for_facet_values", kwargs))
        return self.facet_response

    def delete_index(self, **kwargs):
        self.calls.append(("delete_index", kwargs))
        if self.delete_error:
            raise self.delete_error
        return self.deletion_response

    def close(self):
        self.calls.append(("close", {}))
        if self.close_error:
            raise self.close_error


class OriginParsingTests(unittest.TestCase):
    def test_accepts_hostname_ipv4_and_ipv6_origins(self):
        cases = {
            "HTTPS://Example.COM": ("https", "example.com", None),
            "http://localhost": ("http", "localhost", None),
            "http://127.0.0.2:7700": ("http", "127.0.0.2", 7700),
            "https://127.0.0.1:8443": ("https", "127.0.0.1", 8443),
            "http://[::1]": ("http", "::1", None),
            "https://[2001:db8::1]:9443": ("https", "2001:db8::1", 9443),
        }
        for value, expected in cases.items():
            with self.subTest(value=value):
                origin = contract.parse_flapjack_origin(value)
                self.assertEqual((origin.scheme, origin.hostname, origin.port), expected)

    def test_rejects_non_origin_and_malformed_values(self):
        rejected = [
            "localhost:7700",
            "ftp://localhost",
            "http://example.com",
            "http://192.0.2.1:7700",
            "http://[2001:db8::1]:7700",
            "http://",
            "http://user@localhost",
            "http://user:secret@localhost",
            "http://localhost/",
            "http://localhost/path",
            "http://localhost?",
            "http://localhost?query=yes",
            "http://localhost#",
            "http://localhost#fragment",
            "http://localhost:",
            "http://localhost:not-a-port",
            "http://localhost:70000",
        ]
        for value in rejected:
            with self.subTest(value=value):
                with self.assertRaisesRegex(ValueError, "FLAPJACK_URL"):
                    contract.parse_flapjack_origin(value)

    def test_malformed_credential_bearing_ports_do_not_expose_credentials(self):
        secret = "synthetic-password-never-log"
        values = [
            f"http://user:{secret}@localhost:not-a-port",
            f"http://user:{secret}@localhost:70000",
        ]
        for value in values:
            with self.subTest(value=value):
                with self.assertRaises(ValueError) as raised:
                    contract.parse_flapjack_origin(value)

                rendered_exception = "".join(
                    traceback.format_exception(raised.exception)
                )
                self.assertNotIn(secret, rendered_exception)
                self.assertIn("FLAPJACK_URL", str(raised.exception))

    def test_rejects_explicit_zero_port_before_client_construction(self):
        with self.assertRaisesRegex(ValueError, "between 1 and 65535"):
            contract.parse_flapjack_origin("http://localhost:0")


class RequirementVersionTests(unittest.TestCase):
    def write_requirements(self, directory, contents):
        path = Path(directory) / "requirements.txt"
        path.write_text(contents, encoding="utf-8")
        return path

    def test_returns_the_single_exact_installed_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            path = self.write_requirements(
                directory, "# official client\n\nalgoliasearch==9.8.7\n"
            )
            requested = []
            version = contract.load_expected_algoliasearch_version(
                path, lambda package: requested.append(package) or "9.8.7"
            )
        self.assertEqual(version, "9.8.7")
        self.assertEqual(requested, ["algoliasearch"])

    def test_rejects_missing_duplicate_non_exact_and_mismatched_pins(self):
        cases = [
            ("# no declaration\n", "9.8.7"),
            ("algoliasearch==9.8.7\nalgoliasearch==9.8.7\n", "9.8.7"),
            ("algoliasearch>=9.8.7\n", "9.8.7"),
            ("another-package==1.0\n", "9.8.7"),
            ("algoliasearch==9.8.7\n", "9.8.6"),
        ]
        for contents, installed in cases:
            with self.subTest(contents=contents, installed=installed):
                with tempfile.TemporaryDirectory() as directory:
                    path = self.write_requirements(directory, contents)
                    with self.assertRaises(ValueError):
                        contract.load_expected_algoliasearch_version(
                            path, lambda _package: installed
                        )


class ClientConfigurationTests(unittest.TestCase):
    def test_main_requires_explicit_admin_key(self):
        for environment in ({}, {"FLAPJACK_ADMIN_KEY": ""}):
            with self.subTest(environment=environment), mock.patch.dict(
                "os.environ", environment, clear=True
            ), mock.patch.object(contract, "load_expected_algoliasearch_version"):
                with self.assertRaisesRegex(ValueError, "FLAPJACK_ADMIN_KEY"):
                    contract.main()

    def test_builds_exactly_one_read_write_host(self):
        expected_client = object()
        FakeSearchClientSync.client_to_return = expected_client
        origins = {
            "http://localhost": ("localhost", "http", None),
            "https://127.0.0.1:8443": ("127.0.0.1", "https", 8443),
            "http://[::1]:7700": ("[::1]", "http", 7700),
        }
        for value, expected_host in origins.items():
            with self.subTest(value=value):
                result = contract.create_client(
                    contract.parse_flapjack_origin(value), "secret-key"
                )
                config = FakeSearchClientSync.created_config
                self.assertIs(result, expected_client)
                self.assertEqual(config.app_id, "flapjack")
                self.assertEqual(config.api_key, "secret-key")
                self.assertIsInstance(config.hosts, FakeHostsCollection)
                self.assertEqual(len(config.hosts.hosts), 1)
                host = config.hosts.hosts[0]
                self.assertEqual((host.url, host.scheme, host.port), expected_host)
                self.assertEqual(host.accept, FakeCallType.READ | FakeCallType.WRITE)


class ContractJourneyTests(unittest.TestCase):
    def setUp(self):
        self.fixture = load_fixture()
        self.index_name = "python-client-contract-fixed"

    def run_contract(self, client):
        contract.run_contract(client, self.fixture, self.index_name)

    def test_runs_complete_journey_and_waits_for_every_task_in_order(self):
        client = FakeClient(self.fixture)
        self.run_contract(client)
        call_names = [name for name, _arguments in client.calls]
        self.assertEqual(
            call_names,
            [
                "set_settings",
                "wait_for_task",
                "save_objects",
                "wait_for_task",
                "wait_for_task",
                "search",
                "search_for_facet_values",
                "delete_index",
                "wait_for_task",
                "close",
            ],
        )
        waits = [args["task_id"] for name, args in client.calls if name == "wait_for_task"]
        self.assertEqual(waits, [101, 201, 202, 301])
        self.assertEqual(
            client.calls[0][1],
            {"index_name": self.index_name, "index_settings": self.fixture["settings"]},
        )
        self.assertEqual(
            client.calls[2][1],
            {"index_name": self.index_name, "objects": self.fixture["products"]},
        )
        self.assertEqual(
            client.calls[5][1],
            {
                "search_method_params": {
                    "requests": [{"indexName": self.index_name, "query": "laptop"}]
                }
            },
        )
        self.assertEqual(
            client.calls[6][1],
            {
                "index_name": self.index_name,
                "facet_name": "brand",
                "search_for_facet_values_request": {"facetQuery": "no"},
            },
        )

    def test_rejects_empty_or_malformed_batch_responses(self):
        malformed_batches = [
            [],
            [FakeGeneratedResponse({"taskID": True, "objectIDs": []}, task_id=True, object_ids=[])],
            [
                FakeGeneratedResponse(
                    {"taskID": 201, "objectIDs": "product_1"},
                    task_id=201,
                    object_ids="product_1",
                )
            ],
            [
                FakeGeneratedResponse(
                    {"taskID": 999, "objectIDs": ["product_1"]},
                    task_id=201,
                    object_ids=["product_1"],
                )
            ],
        ]
        for responses in malformed_batches:
            with self.subTest(responses=responses):
                client = FakeClient(self.fixture)
                client.save_responses = responses
                with self.assertRaises(AssertionError):
                    self.run_contract(client)

    def test_rejects_malformed_settings_deletion_and_wait_responses(self):
        malformed_serialized_responses = [
            object(),
            types.SimpleNamespace(to_dict=None),
            types.SimpleNamespace(to_dict=lambda: []),
        ]
        for response in malformed_serialized_responses:
            with self.subTest(response=response):
                client = FakeClient(self.fixture)
                client.settings_response = response
                with self.assertRaises(AssertionError):
                    self.run_contract(client)

        client = FakeClient(self.fixture)
        client.settings_response = FakeGeneratedResponse(
            {"taskID": 101, "updatedAt": "timestamp"},
            task_id=101.0,
            updated_at="timestamp",
        )
        with self.assertRaises(AssertionError):
            self.run_contract(client)

        client = FakeClient(self.fixture)
        client.wait_response = FakeGeneratedResponse({"status": "processing"})
        with self.assertRaises(AssertionError):
            self.run_contract(client)

        client = FakeClient(self.fixture)
        client.deletion_response = FakeGeneratedResponse(
            {"taskID": 301, "deletedAt": ""}, task_id=301, deleted_at=""
        )
        with self.assertRaises(AssertionError):
            self.run_contract(client)

    def test_rejects_non_integer_facet_counts(self):
        for count in (2.0, True, "2"):
            with self.subTest(count=count):
                client = FakeClient(self.fixture)
                client.facet_response._payload["facetHits"][0]["count"] = count
                with self.assertRaises(AssertionError):
                    self.run_contract(client)

    def test_rejects_non_boolean_facet_exhaustiveness(self):
        for exhaustive in (1, "true", None):
            with self.subTest(exhaustive=exhaustive):
                client = FakeClient(self.fixture)
                client.facet_response._payload["exhaustiveFacetsCount"] = exhaustive
                with self.assertRaises(AssertionError):
                    self.run_contract(client)

    def test_rejects_wrong_missing_extra_or_reordered_projections(self):
        mutations = []
        wrong_name = valid_search_payload(self.fixture)
        wrong_name["results"][0]["hits"][0]["name"] = "Wrong"
        mutations.append(("search_response", wrong_name))
        wrong_id = valid_search_payload(self.fixture)
        wrong_id["results"][0]["hits"][0]["objectID"] = "wrong-id"
        mutations.append(("search_response", wrong_id))
        missing = valid_search_payload(self.fixture)
        missing["results"][0]["hits"].pop()
        mutations.append(("search_response", missing))
        extra = valid_search_payload(self.fixture)
        extra["results"][0]["hits"].append({"name": "Extra", "objectID": "extra"})
        mutations.append(("search_response", extra))
        reordered = valid_search_payload(self.fixture)
        reordered["results"][0]["hits"].reverse()
        mutations.append(("search_response", reordered))
        wrong_facet = valid_facet_payload(self.fixture)
        wrong_facet["facetHits"][0]["value"] = "Wrong"
        mutations.append(("facet_response", wrong_facet))
        missing_facet = valid_facet_payload(self.fixture)
        missing_facet["facetHits"] = []
        mutations.append(("facet_response", missing_facet))
        extra_facet = valid_facet_payload(self.fixture)
        extra_facet["facetHits"].append({"value": "Extra", "count": 1})
        mutations.append(("facet_response", extra_facet))

        for response_name, payload in mutations:
            with self.subTest(response_name=response_name, payload=payload):
                client = FakeClient(self.fixture)
                setattr(client, response_name, FakeGeneratedResponse(payload))
                with self.assertRaises(AssertionError):
                    self.run_contract(client)

    def test_rejects_malformed_search_response_fields(self):
        malformed_values = [
            ("nbHits", 2.0),
            ("nbHits", True),
            ("nbHits", "2"),
        ]
        for field, value in malformed_values:
            with self.subTest(field=field, value=value):
                client = FakeClient(self.fixture)
                client.search_response._payload["results"][0][field] = value
                with self.assertRaises(AssertionError):
                    self.run_contract(client)

    def test_deletes_after_partial_creation_and_always_closes(self):
        client = FakeClient(self.fixture)
        body_error = RuntimeError("settings transport failed")
        client.set_settings_error = body_error
        with self.assertRaises(RuntimeError) as caught:
            self.run_contract(client)
        self.assertIs(caught.exception, body_error)
        self.assertEqual(
            [name for name, _arguments in client.calls],
            ["set_settings", "delete_index", "wait_for_task", "close"],
        )

    def test_close_runs_after_deletion_failure(self):
        client = FakeClient(self.fixture)
        client.delete_error = RuntimeError("delete failed")
        with self.assertRaisesRegex(RuntimeError, "delete failed"):
            self.run_contract(client)
        self.assertEqual(client.calls[-1][0], "close")

    def test_preserves_all_cleanup_failures_after_success(self):
        client = FakeClient(self.fixture)
        client.delete_error = RuntimeError("delete failed")
        client.close_error = RuntimeError("close failed")
        with self.assertRaises(ExceptionGroup) as caught:
            self.run_contract(client)
        self.assertEqual(
            [str(error) for error in caught.exception.exceptions],
            ["delete failed", "close failed"],
        )

    def test_cleanup_never_replaces_the_body_failure(self):
        client = FakeClient(self.fixture)
        body_error = RuntimeError("search failed")
        client.search_error = body_error
        client.delete_error = RuntimeError("delete failed")
        client.close_error = RuntimeError("close failed")
        with self.assertRaises(RuntimeError) as caught:
            self.run_contract(client)
        self.assertIs(caught.exception, body_error)
        self.assertEqual(len(caught.exception.__notes__), 2)
        self.assertIn("delete failed", caught.exception.__notes__[0])
        self.assertIn("close failed", caught.exception.__notes__[1])


if __name__ == "__main__":
    unittest.main()
