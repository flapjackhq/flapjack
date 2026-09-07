#!/usr/bin/env python3

import ipaddress
import json
import os
import re
import uuid
from dataclasses import dataclass
from importlib import metadata
from pathlib import Path
from typing import Callable
from urllib.parse import urlsplit

from algoliasearch.http.hosts import CallType, Host, HostsCollection
from algoliasearch.search.client import SearchClientSync
from algoliasearch.search.config import SearchConfig


SCRIPT_DIR = Path(__file__).resolve().parent
REQUIREMENTS_PATH = SCRIPT_DIR / "requirements-python-client.txt"
FIXTURE_PATH = SCRIPT_DIR / "fixtures" / "official_client_contract.json"
EXACT_ALGOLIASEARCH_REQUIREMENT = re.compile(
    r"algoliasearch==([A-Za-z0-9][A-Za-z0-9._+-]*)"
)


@dataclass(frozen=True)
class FlapjackOrigin:
    scheme: str
    hostname: str
    port: int | None


def require(condition: bool, message: str) -> None:
    if not condition:
        raise AssertionError(message)


def parse_flapjack_origin(value: str) -> FlapjackOrigin:
    try:
        parsed = urlsplit(value)
        username = parsed.username
        password = parsed.password
        hostname = parsed.hostname
    except ValueError:
        raise ValueError("FLAPJACK_URL must be a valid absolute origin") from None

    scheme = parsed.scheme.lower()
    if scheme not in {"http", "https"}:
        raise ValueError("FLAPJACK_URL must use an absolute http or https origin")
    if not parsed.netloc or not hostname:
        raise ValueError("FLAPJACK_URL must include a hostname")
    if username is not None or password is not None:
        raise ValueError("FLAPJACK_URL must not include credentials")

    if scheme == "http":
        try:
            is_loopback = ipaddress.ip_address(hostname).is_loopback
        except ValueError:
            is_loopback = hostname == "localhost"
        if not is_loopback:
            raise ValueError(
                "FLAPJACK_URL must use https unless the hostname is loopback"
            )

    try:
        port = parsed.port
        if port == 0:
            raise ValueError
    except ValueError:
        raise ValueError(
            "FLAPJACK_URL port must be an integer between 1 and 65535"
        ) from None

    if parsed.path or "?" in value or "#" in value:
        raise ValueError("FLAPJACK_URL must not include a path, query, or fragment")
    if parsed.netloc.endswith(":"):
        raise ValueError("FLAPJACK_URL port must be a valid integer")

    return FlapjackOrigin(scheme=scheme, hostname=hostname, port=port)


def load_expected_algoliasearch_version(
    requirements_path: Path = REQUIREMENTS_PATH,
    installed_version: Callable[[str], str] = metadata.version,
) -> str:
    declarations = [
        line.strip()
        for line in requirements_path.read_text(encoding="utf-8").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if len(declarations) != 1:
        raise ValueError(
            "requirements-python-client.txt must contain exactly one executable declaration"
        )

    match = EXACT_ALGOLIASEARCH_REQUIREMENT.fullmatch(declarations[0])
    if match is None:
        raise ValueError(
            "requirements-python-client.txt must contain one exact algoliasearch==VERSION pin"
        )

    expected_version = match.group(1)
    actual_version = installed_version("algoliasearch")
    if actual_version != expected_version:
        raise ValueError(
            "Installed algoliasearch version "
            f"{actual_version!r} does not match required version {expected_version!r}"
        )
    return expected_version


def create_client(origin: FlapjackOrigin, admin_key: str) -> SearchClientSync:
    host_name = (
        f"[{origin.hostname}]" if ":" in origin.hostname else origin.hostname
    )
    host = Host(
        url=host_name,
        scheme=origin.scheme,
        port=origin.port,
        accept=CallType.READ | CallType.WRITE,
    )
    config = SearchConfig(app_id="flapjack", api_key=admin_key)
    config.hosts = HostsCollection([host])
    return SearchClientSync.create_with_config(config=config)


def response_payload(response: object, operation: str) -> dict:
    serializer = getattr(response, "to_dict", None)
    require(callable(serializer), f"{operation} response must provide to_dict()")
    payload = serializer()
    require(type(payload) is dict, f"{operation} to_dict() must return a dictionary")
    return payload


def validate_timestamped_task(
    response: object,
    timestamp_attribute: str,
    timestamp_alias: str,
    operation: str,
) -> int:
    task_id = getattr(response, "task_id", None)
    timestamp = getattr(response, timestamp_attribute, None)
    require(type(task_id) is int, f"{operation} task_id must be an exact integer")
    require(
        type(timestamp) is str and bool(timestamp),
        f"{operation} timestamp must be a nonempty string",
    )

    payload = response_payload(response, operation)
    require(
        type(payload.get("taskID")) is int and payload["taskID"] == task_id,
        f"{operation} taskID alias must match task_id",
    )
    require(
        type(payload.get(timestamp_alias)) is str
        and payload[timestamp_alias] == timestamp,
        f"{operation} {timestamp_alias} alias must match {timestamp_attribute}",
    )
    return task_id


def validate_batch_response(response: object) -> int:
    task_id = getattr(response, "task_id", None)
    object_ids = getattr(response, "object_ids", None)
    require(type(task_id) is int, "save_objects task_id must be an exact integer")
    require(type(object_ids) is list, "save_objects object_ids must be a list")
    require(
        all(type(object_id) is str for object_id in object_ids),
        "save_objects object_ids must contain only strings",
    )

    payload = response_payload(response, "save_objects")
    require(
        type(payload.get("taskID")) is int and payload["taskID"] == task_id,
        "save_objects taskID alias must match task_id",
    )
    require(
        type(payload.get("objectIDs")) is list
        and payload["objectIDs"] == object_ids,
        "save_objects objectIDs alias must match object_ids",
    )
    return task_id


def wait_for_published_task(client: SearchClientSync, index_name: str, task_id: int) -> None:
    response = client.wait_for_task(index_name=index_name, task_id=task_id)
    payload = response_payload(response, "wait_for_task")
    require(payload.get("status") == "published", "wait_for_task must return published")


def validate_search_response(response: object, expected: dict) -> None:
    payload = response_payload(response, "search")
    results = payload.get("results")
    require(type(results) is list and len(results) == 1, "search must return one result")
    result = results[0]
    require(type(result) is dict, "search result must be a dictionary")
    require(type(result.get("nbHits")) is int, "search nbHits must be an exact integer")

    hits = result.get("hits")
    require(type(hits) is list, "search hits must be a list")
    require(all(type(hit) is dict for hit in hits), "search hits must be dictionaries")
    names = [hit.get("name") for hit in hits]
    object_ids = [hit.get("objectID") for hit in hits]
    require(names == expected["laptopNames"], "search hit names differ from fixture")
    require(
        object_ids == expected["laptopObjectIDs"],
        "search object IDs differ from fixture",
    )
    require(result["nbHits"] == expected["laptopNbHits"], "search nbHits differs from fixture")


def validate_facet_response(response: object, expected: dict) -> None:
    payload = response_payload(response, "search_for_facet_values")
    require(
        type(payload.get("exhaustiveFacetsCount")) is bool,
        "facet exhaustiveFacetsCount must be an exact boolean",
    )
    facet_hits = payload.get("facetHits")
    require(type(facet_hits) is list, "facetHits must be a list")

    projection = []
    for facet_hit in facet_hits:
        require(type(facet_hit) is dict, "each facet hit must be a dictionary")
        value = facet_hit.get("value")
        count = facet_hit.get("count")
        require(type(value) is str, "facet hit value must be a string")
        require(type(count) is int, "facet hit count must be an exact integer")
        projection.append({"value": value, "count": count})

    require(
        projection == expected["brandFacetHits"],
        "facet hits differ from the complete ordered fixture projection",
    )


def delete_contract_index(client: SearchClientSync, index_name: str) -> None:
    response = client.delete_index(index_name=index_name)
    task_id = validate_timestamped_task(
        response, "deleted_at", "deletedAt", "delete_index"
    )
    wait_for_published_task(client, index_name, task_id)


def raise_cleanup_failures(cleanup_failures: list[BaseException]) -> None:
    if len(cleanup_failures) == 1:
        raise cleanup_failures[0]
    if cleanup_failures:
        raise BaseExceptionGroup("Python client contract cleanup failed", cleanup_failures)


def execute_contract_journey(
    client: SearchClientSync, fixture: dict, index_name: str
) -> None:
    settings_response = client.set_settings(
        index_name=index_name, index_settings=fixture["settings"]
    )
    settings_task_id = validate_timestamped_task(
        settings_response, "updated_at", "updatedAt", "set_settings"
    )
    wait_for_published_task(client, index_name, settings_task_id)

    batch_responses = client.save_objects(
        index_name=index_name, objects=fixture["products"]
    )
    require(
        type(batch_responses) is list and bool(batch_responses),
        "save_objects must return a nonempty batch list",
    )
    for batch_response in batch_responses:
        task_id = validate_batch_response(batch_response)
        wait_for_published_task(client, index_name, task_id)

    search_response = client.search(
        search_method_params={
            "requests": [{"indexName": index_name, "query": "laptop"}]
        }
    )
    validate_search_response(search_response, fixture["expected"])

    facet_response = client.search_for_facet_values(
        index_name=index_name,
        facet_name="brand",
        search_for_facet_values_request={"facetQuery": "no"},
    )
    validate_facet_response(facet_response, fixture["expected"])


def run_contract(
    client: SearchClientSync,
    fixture: dict,
    index_name: str | None = None,
) -> None:
    contract_index = index_name or f"python-client-contract-{uuid.uuid4().hex}"
    index_may_exist = False
    body_error = None

    try:
        index_may_exist = True
        execute_contract_journey(client, fixture, contract_index)
    except BaseException as error:
        body_error = error
        raise
    finally:
        cleanup_failures = []
        if index_may_exist:
            try:
                delete_contract_index(client, contract_index)
            except BaseException as error:
                cleanup_failures.append(error)
        try:
            client.close()
        except BaseException as error:
            cleanup_failures.append(error)

        if body_error is not None:
            for cleanup_error in cleanup_failures:
                body_error.add_note(
                    "Python client contract cleanup failure: "
                    f"{type(cleanup_error).__name__}: {cleanup_error}"
                )
        else:
            raise_cleanup_failures(cleanup_failures)


def main() -> None:
    load_expected_algoliasearch_version()
    fixture = json.loads(FIXTURE_PATH.read_text(encoding="utf-8"))
    origin = parse_flapjack_origin(
        os.environ.get("FLAPJACK_URL", "http://localhost:7700")
    )
    admin_key = os.environ.get("FLAPJACK_ADMIN_KEY")
    if not admin_key:
        raise ValueError("FLAPJACK_ADMIN_KEY must be set to a nonempty test credential")
    client = create_client(origin, admin_key)
    run_contract(client, fixture)


if __name__ == "__main__":
    main()
