#!/usr/bin/env python3
"""Validate the Algolia measurement sandbox cleanup ledger contract."""

from __future__ import annotations

import argparse
import copy
import json
import sys
from pathlib import Path
from typing import Any


RESOURCE_KEYS = (
    "internet_gateway_id",
    "internet_gateway_attachment_vpc_id",
    "public_subnet_id",
    "public_route_table_id",
    "public_route_table_association_id",
    "public_default_route_destination",
    "private_route_table_id",
    "private_default_route_destination",
    "nat_gateway_id",
    "nat_gateway_eni_ids",
    "eip_allocation_id",
    "eip_association_id",
    "additional_eni_ids",
)

RESIDUE_CLASSES = (
    "internet_gateway",
    "internet_gateway_attachment",
    "public_subnet",
    "public_route_table",
    "public_route_table_association",
    "public_default_route",
    "private_default_route",
    "nat_gateway",
    "eip_allocation",
    "eip_association",
    "additional_eni",
)

class LedgerValidationError(ValueError):
    """Raised when the cleanup ledger cannot prove bounded teardown."""


def require_mapping(value: Any, name: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise LedgerValidationError(f"{name} must be an object")
    return value


def require_nonempty(value: Any, name: str) -> None:
    if value is None or value == "" or value == []:
        raise LedgerValidationError(f"{name} must be populated")


def require_no_unsubstituted_placeholders(text: str, name: str) -> None:
    """Reject a populated command that still carries a `{placeholder}` token.

    A surviving token means the provisioner's gsub never ran, so the command
    would be executed literally and silently match nothing.
    """
    if "{" in text:
        raise LedgerValidationError(f"{name} must contain exact IDs")


def validate_resources(egress: dict[str, Any], require_populated: bool) -> None:
    resources = require_mapping(egress.get("resources"), "stage6_egress.resources")
    missing = [key for key in RESOURCE_KEYS if key not in resources]
    if missing:
        raise LedgerValidationError(f"missing egress resource keys: {', '.join(missing)}")

    if resources["public_default_route_destination"] != "0.0.0.0/0":
        raise LedgerValidationError("public default route must be 0.0.0.0/0")
    if resources["private_default_route_destination"] != "0.0.0.0/0":
        raise LedgerValidationError("private default route must be 0.0.0.0/0")

    for list_key in ("nat_gateway_eni_ids", "additional_eni_ids"):
        if not isinstance(resources[list_key], list):
            raise LedgerValidationError(f"{list_key} must be a list")

    if require_populated:
        for key in RESOURCE_KEYS:
            if key == "additional_eni_ids":
                continue
            require_nonempty(resources[key], f"stage6_egress.resources.{key}")


def validate_cleanup_commands(egress: dict[str, Any], require_populated: bool) -> None:
    commands = egress.get("cleanup_commands")
    if not isinstance(commands, list) or not all(isinstance(item, str) for item in commands):
        raise LedgerValidationError("stage6_egress.cleanup_commands must be a string list")

    resources = egress["resources"]

    def resource_id(key: str) -> str:
        return resources[key] or f"{{{key}}}"

    eni_ids = " ".join(resources["nat_gateway_eni_ids"]) or "{nat_gateway_eni_ids}"
    command_sequence = (
        f"delete-route --region us-east-1 --route-table-id {resource_id('private_route_table_id')}",
        f"delete-nat-gateway --region us-east-1 --nat-gateway-id {resource_id('nat_gateway_id')}",
        f"wait nat-gateway-deleted --region us-east-1 --nat-gateway-ids {resource_id('nat_gateway_id')}",
        f"describe-network-interfaces --region us-east-1 --network-interface-ids {eni_ids}",
        f"release-address --region us-east-1 --allocation-id {resource_id('eip_allocation_id')}",
        "disassociate-route-table --region us-east-1 --association-id "
        f"{resource_id('public_route_table_association_id')}",
        f"delete-route --region us-east-1 --route-table-id {resource_id('public_route_table_id')}",
        f"delete-route-table --region us-east-1 --route-table-id {resource_id('public_route_table_id')}",
        f"delete-subnet --region us-east-1 --subnet-id {resource_id('public_subnet_id')}",
        "detach-internet-gateway --region us-east-1 --internet-gateway-id "
        f"{resource_id('internet_gateway_id')}",
        "delete-internet-gateway --region us-east-1 --internet-gateway-id "
        f"{resource_id('internet_gateway_id')}",
    )

    joined = "\n".join(commands)
    positions: list[int] = []
    for fragment in command_sequence:
        position = joined.find(fragment)
        if position < 0:
            raise LedgerValidationError(f"missing cleanup command fragment: {fragment}")
        positions.append(position)
    if positions != sorted(positions):
        raise LedgerValidationError("egress cleanup commands are not dependency ordered")
    if require_populated:
        require_no_unsubstituted_placeholders(joined, "populated cleanup commands")


def validate_residue_assertions(ledger: dict[str, Any], require_populated: bool) -> None:
    """Validate the Stage 8 zero-residue assertions.

    `require_populated` mirrors the cleanup-command gate: once the provisioner
    claims a populated ledger, each residue command must carry real IDs, since
    these commands are the sole evidence that Stage 8's teardown left nothing
    behind.
    """
    assertions = ledger.get("stage8_zero_residue_assertions")
    if not isinstance(assertions, list):
        raise LedgerValidationError("stage8_zero_residue_assertions must be a list")

    by_class: dict[str, dict[str, Any]] = {}
    for assertion in assertions:
        item = require_mapping(assertion, "zero-residue assertion")
        resource_class = item.get("resource_class")
        if isinstance(resource_class, str):
            by_class[resource_class] = item

    missing = [resource_class for resource_class in RESIDUE_CLASSES if resource_class not in by_class]
    if missing:
        raise LedgerValidationError(f"missing zero-residue classes: {', '.join(missing)}")
    for resource_class in RESIDUE_CLASSES:
        command = by_class[resource_class].get("command")
        require_nonempty(command, f"{resource_class}.command")
        require_nonempty(by_class[resource_class].get("empty_result"), f"{resource_class}.empty_result")
        if require_populated:
            require_no_unsubstituted_placeholders(command, f"{resource_class}.command")


def validate_ledger(ledger: dict[str, Any], require_populated: bool = False) -> None:
    egress = require_mapping(ledger.get("stage6_egress"), "stage6_egress")
    require_nonempty(egress.get("cleanup_owner"), "stage6_egress.cleanup_owner")
    require_nonempty(egress.get("teardown_deadline_utc"), "stage6_egress.teardown_deadline_utc")
    cost = require_mapping(egress.get("cost_projection"), "stage6_egress.cost_projection")
    for key in ("max_runtime_hours", "base_cost_usd", "nat_processing_usd_per_gb"):
        require_nonempty(cost.get(key), f"stage6_egress.cost_projection.{key}")
    validate_resources(egress, require_populated)
    validate_cleanup_commands(egress, require_populated)
    validate_residue_assertions(ledger, require_populated)


def run_mutation_tests(ledger: dict[str, Any]) -> None:
    validate_ledger(ledger)
    for key in RESOURCE_KEYS:
        mutated = copy.deepcopy(ledger)
        del mutated["stage6_egress"]["resources"][key]
        expect_failure(mutated, f"missing resource {key}")

    for resource_class in RESIDUE_CLASSES:
        mutated = copy.deepcopy(ledger)
        mutated["stage8_zero_residue_assertions"] = [
            item
            for item in mutated["stage8_zero_residue_assertions"]
            if item["resource_class"] != resource_class
        ]
        expect_failure(mutated, f"missing residue class {resource_class}")

    mutated = copy.deepcopy(ledger)
    commands = mutated["stage6_egress"]["cleanup_commands"]
    commands[0], commands[-1] = commands[-1], commands[0]
    expect_failure(mutated, "reordered cleanup commands")

    run_populated_mutation_tests(ledger)


def run_populated_mutation_tests(ledger: dict[str, Any]) -> None:
    """Mutation-cover the `--require-populated` gate that guards the receipt.

    Skipped when the supplied ledger is still a template, since only a
    populated ledger can exercise this gate.
    """
    try:
        validate_ledger(ledger, require_populated=True)
    except LedgerValidationError:
        return

    for resource_class in RESIDUE_CLASSES:
        mutated = copy.deepcopy(ledger)
        for item in mutated["stage8_zero_residue_assertions"]:
            if item["resource_class"] == resource_class:
                item["command"] = "aws ec2 describe-x --filters Name=tag:RunId,Values={egress_run_tag}"
        expect_failure(
            mutated,
            f"unsubstituted token in residue command {resource_class}",
            require_populated=True,
        )


def expect_failure(ledger: dict[str, Any], scenario: str, require_populated: bool = False) -> None:
    try:
        validate_ledger(ledger, require_populated=require_populated)
    except LedgerValidationError:
        return
    raise AssertionError(f"validator accepted {scenario}")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("ledger", type=Path)
    parser.add_argument("--require-populated", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()

    try:
        ledger = json.loads(args.ledger.read_text(encoding="utf-8"))
        validate_ledger(ledger, require_populated=args.require_populated)
        if args.self_test:
            run_mutation_tests(ledger)
    except (OSError, json.JSONDecodeError, LedgerValidationError, AssertionError) as error:
        print(f"REJECT: {error}", file=sys.stderr)
        return 1

    print("ACCEPT: cleanup ledger covers all egress resources and dependency ordering")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
