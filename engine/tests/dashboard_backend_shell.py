"""Shell command parsing shared by the dashboard backend contract gate."""

from __future__ import annotations

import re

# Matching the invocation instead of a binary path covers every workflow launch path.
BACKEND_LAUNCH_RE = re.compile(r"\bflapjack\s+--data-dir\b")
ENV_ASSIGNMENT_RE = re.compile(r"(?:^|\s)([A-Z][A-Z0-9_]*)=(\S*)")
# Consume exactly one continued newline. Consuming general whitespace would cross the
# blank boundary left by a comment-only line and splice separate commands together.
LINE_CONTINUATION_RE = re.compile(r"\\\n[ \t]*")


def active_shell_commands(run_body: str) -> list[str]:
    """Return executable commands while preserving comment-only boundaries.

    GitHub workflows express a launch as backslash-continued environment assignments.
    A comment following a continuation ends the assignment command before the later
    launch. Blanking comment-only lines preserves that boundary; deleting them would
    incorrectly attach the assignment to the launch.
    """
    boundary_preserving = "\n".join(
        "" if line.lstrip().startswith("#") else line for line in run_body.splitlines()
    )
    commands: list[str] = []
    for line in LINE_CONTINUATION_RE.sub(" ", boundary_preserving).splitlines():
        commands.extend(split_shell_commands(line))
    return commands


def command_separator_width(line: str, index: int) -> int:
    """Return the width of a supported separator at the given offset."""
    if line[index] == ";":
        return 1
    if line[index : index + 2] in ("&&", "||"):
        return 2
    if line[index] == "|":
        return 1
    return 0


def split_shell_commands(line: str) -> list[str]:
    """Split separators outside quotes and command substitutions.

    Each command substitution suspends its enclosing quote. Restoring that quote when
    the substitution closes keeps separators inside `$(...)` inert while allowing a
    later top-level separator after `"$(...)"` to split normally.
    """
    commands: list[str] = []
    command_start = 0
    quote: str | None = None
    escaped = False
    suspended_quotes: list[str | None] = []
    index = 0

    while index < len(line):
        character = line[index]
        if escaped:
            escaped = False
        elif quote == "'":
            if character == quote:
                quote = None
        elif character == "\\":
            escaped = True
        elif line[index : index + 2] == "$(":
            suspended_quotes.append(quote)
            quote = None
            index += 2
            continue
        elif quote is not None:
            if character == quote:
                quote = None
        elif character in ("'", '"'):
            quote = character
        elif suspended_quotes:
            if character == "(":
                suspended_quotes.append(None)
            elif character == ")":
                quote = suspended_quotes.pop()
        elif separator_width := command_separator_width(line, index):
            command = line[command_start:index].strip()
            if command:
                commands.append(command)
            index += separator_width
            command_start = index
            continue
        index += 1

    final_command = line[command_start:].strip()
    if final_command:
        commands.append(final_command)
    return commands


def collect_prefix_env(run_body: str) -> dict[str, str]:
    """Return environment assignments prefixed to the backend launch."""
    env: dict[str, str] = {}
    for command in active_shell_commands(run_body):
        launch = BACKEND_LAUNCH_RE.search(command)
        if launch is None:
            continue
        # Assignments after the binary are arguments, never launch environment.
        for name, value in ENV_ASSIGNMENT_RE.findall(command[: launch.start()]):
            env[name] = value
    return env
