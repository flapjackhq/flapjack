#!/usr/bin/env python3
"""Extract the one core test executable from Cargo JSON into GitHub step output."""

import json
import sys
from pathlib import Path


class ContractError(RuntimeError):
    """Raised when Cargo output cannot identify one safe candidate executable."""


def extract_candidate_binary(messages_path, github_output_path):
    candidates = []
    build_finished = []
    with Path(messages_path).open(encoding="utf-8") as messages:
        for line_number, line in enumerate(messages, start=1):
            if not line.strip():
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError as error:
                raise ContractError(
                    f"Cargo message line {line_number} is not valid JSON: {error}"
                ) from error
            if not isinstance(message, dict):
                raise ContractError(
                    f"Cargo message line {line_number} must be a JSON object"
                )
            reason = message.get("reason")
            if reason == "build-finished":
                build_finished.append(message.get("success"))
                continue
            if reason != "compiler-artifact":
                continue

            target = message.get("target")
            profile = message.get("profile")
            if (
                not isinstance(target, dict)
                or not isinstance(target.get("kind"), list)
                or not isinstance(profile, dict)
            ):
                raise ContractError(
                    f"Cargo compiler-artifact line {line_number} has invalid target/profile data"
                )
            executable = message.get("executable")
            if (
                target.get("name") == "flapjack"
                and "lib" in target.get("kind", [])
                and profile.get("test") is True
                and isinstance(executable, str)
                and executable
            ):
                candidates.append(executable)

    # The command's shell `&&` already rejects a non-zero Cargo exit, but the
    # JSON handoff itself must also be complete rather than a truncated prefix.
    if build_finished != [True]:
        raise ContractError(
            "candidate build must emit exactly one successful build-finished message"
        )
    if len(candidates) != 1:
        raise ContractError(
            "candidate build must emit exactly one flapjack library test executable; "
            f"found {len(candidates)}"
        )
    executable = candidates[0]
    if "\n" in executable or "\r" in executable:
        raise ContractError("candidate executable path contains a line break")
    with Path(github_output_path).open("a", encoding="utf-8") as output:
        output.write(f"executable={executable}\n")
    return executable


def main():
    if len(sys.argv) != 3:
        print(
            "usage: extract_candidate_binary.py <cargo-json> <github-output>",
            file=sys.stderr,
        )
        return 2
    try:
        executable = extract_candidate_binary(sys.argv[1], sys.argv[2])
    except (ContractError, OSError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"candidate executable: {executable}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
