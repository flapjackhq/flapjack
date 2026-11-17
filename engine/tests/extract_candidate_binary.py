#!/usr/bin/env python3
"""Compile with an owned process group and extract the one core test executable."""

import json
import os
import signal
import subprocess
import time
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

    # The caller already rejects a non-zero Cargo exit, but the
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



CORE_TEST_BUILD = (
    "cargo", "test", "-p", "flapjack", "--lib", "--no-run", "--message-format=json"
)


def stop_owned_build_group(process):
    """Stop only this fresh-session compiler group, including orphaned children."""
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        process.wait()
        return
    deadline = time.monotonic() + 1.0
    while time.monotonic() < deadline:
        process.poll()  # Reap the direct child even when a grandchild remains.
        try:
            os.killpg(process.pid, 0)
        except ProcessLookupError:
            process.wait()
            return
        time.sleep(0.02)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=1.0)


def build_candidate(messages_path):
    """Keep partial diagnostics and preserve failure/cancellation before extraction."""
    interrupted = None
    process = None

    def remember_signal(signum, _frame):
        nonlocal interrupted
        if interrupted is None:
            interrupted = signum

    previous = {}
    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        previous[signum] = signal.signal(signum, remember_signal)
    try:
        with Path(messages_path).open("wb") as messages:
            if interrupted is not None:
                return 128 + interrupted
            process = subprocess.Popen(CORE_TEST_BUILD, stdout=messages, start_new_session=True)
            while process.poll() is None and interrupted is None:
                time.sleep(0.02)
            result = process.returncode
            stop_owned_build_group(process)
            process = None
            if interrupted is not None:
                return 128 + interrupted
            return result if result >= 0 else 128 - result
    finally:
        if process is not None:
            stop_owned_build_group(process)
        for signum, handler in previous.items():
            signal.signal(signum, handler)

def main():
    arguments = sys.argv[1:]
    build = len(arguments) == 3 and arguments[0] == "--build"
    if build:
        arguments = arguments[1:]
    if len(arguments) != 2:
        print(
            "usage: extract_candidate_binary.py [--build] <cargo-json> <github-output>",
            file=sys.stderr,
        )
        return 2
    try:
        if build:
            result = build_candidate(arguments[0])
            if result:
                return result
        executable = extract_candidate_binary(*arguments)
    except (ContractError, OSError) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    print(f"candidate executable: {executable}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
