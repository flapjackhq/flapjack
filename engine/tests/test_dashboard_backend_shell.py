"""Unit tests for dashboard backend shell launch parsing."""

from pathlib import Path
import subprocess
import sys

from dashboard_backend_shell import collect_prefix_env, split_shell_commands


def test_contract_entrypoint_loads_tokenizer_in_safe_path_mode() -> None:
    contract_gate = Path(__file__).with_name("test_dashboard_e2e_backend_contract.py")

    completed = subprocess.run(
        [sys.executable, "-P", str(contract_gate)],
        check=False,
        capture_output=True,
        text=True,
    )

    assert completed.returncode == 0, completed.stderr
    assert completed.stdout.startswith("Dashboard e2e backend contract: OK")


def test_collect_prefix_env_treats_same_line_separators_as_boundaries() -> None:
    expected_env = {
        "FLAPJACK_NODE_ID": "dashboard-e2e",
        "FLAPJACK_ADVERTISE_ADDR": "https://dashboard-e2e.invalid:7700",
        "FLAPJACK_REPLICATION_API_KEY": "fj_devtestadminkey000000",
    }

    for separator in (";", "&&", "||", "|"):
        run_body = (
            f"FLAPJACK_AI_ALLOW_LOCAL_URLS=1 echo warmup {separator} "
            "FLAPJACK_NODE_ID=dashboard-e2e "
            "FLAPJACK_ADVERTISE_ADDR=https://dashboard-e2e.invalid:7700 "
            "FLAPJACK_REPLICATION_API_KEY=fj_devtestadminkey000000 "
            "/tmp/flapjack/flapjack --data-dir /tmp/flapjack-data"
        )

        assert collect_prefix_env(run_body) == expected_env


def test_collect_prefix_env_keeps_quoted_separators_on_backend_launch() -> None:
    for quoted_value in ("'1;enabled|yes'", '"1;enabled|yes"'):
        run_body = (
            f"FLAPJACK_AI_ALLOW_LOCAL_URLS={quoted_value} "
            "FLAPJACK_NODE_ID=dashboard-e2e "
            "FLAPJACK_ADVERTISE_ADDR=https://dashboard-e2e.invalid:7700 "
            "FLAPJACK_REPLICATION_API_KEY=fj_devtestadminkey000000 "
            "/tmp/flapjack/flapjack --data-dir /tmp/flapjack-data"
        )

        assert collect_prefix_env(run_body) == {
            "FLAPJACK_AI_ALLOW_LOCAL_URLS": quoted_value,
            "FLAPJACK_NODE_ID": "dashboard-e2e",
            "FLAPJACK_ADVERTISE_ADDR": "https://dashboard-e2e.invalid:7700",
            "FLAPJACK_REPLICATION_API_KEY": "fj_devtestadminkey000000",
        }


def test_collect_prefix_env_keeps_command_substitution_separators_on_launch() -> None:
    run_body = (
        "FLAPJACK_AI_ALLOW_LOCAL_URLS=$(printf 1; printf 2) "
        "FLAPJACK_NODE_ID=dashboard-e2e "
        "FLAPJACK_ADVERTISE_ADDR=https://dashboard-e2e.invalid:7700 "
        "FLAPJACK_REPLICATION_API_KEY=fj_devtestadminkey000000 "
        "/tmp/flapjack/flapjack --data-dir /tmp/flapjack-data"
    )

    assert set(collect_prefix_env(run_body)) == {
        "FLAPJACK_AI_ALLOW_LOCAL_URLS",
        "FLAPJACK_NODE_ID",
        "FLAPJACK_ADVERTISE_ADDR",
        "FLAPJACK_REPLICATION_API_KEY",
    }


def test_collect_prefix_env_splits_after_double_quoted_command_substitution() -> None:
    run_body = (
        'FLAPJACK_AI_ALLOW_LOCAL_URLS="$(printf 1)" echo warmup && '
        "FLAPJACK_NODE_ID=dashboard-e2e "
        "FLAPJACK_ADVERTISE_ADDR=https://dashboard-e2e.invalid:7700 "
        "FLAPJACK_REPLICATION_API_KEY=fj_devtestadminkey000000 "
        "/tmp/flapjack/flapjack --data-dir /tmp/flapjack-data"
    )

    assert collect_prefix_env(run_body) == {
        "FLAPJACK_NODE_ID": "dashboard-e2e",
        "FLAPJACK_ADVERTISE_ADDR": "https://dashboard-e2e.invalid:7700",
        "FLAPJACK_REPLICATION_API_KEY": "fj_devtestadminkey000000",
    }


def test_split_shell_commands_keeps_quotes_inside_command_substitution() -> None:
    line = 'X="$(printf "a && b")" echo warmup && Y=1 flapjack --data-dir /tmp/d'

    assert split_shell_commands(line) == [
        'X="$(printf "a && b")" echo warmup',
        "Y=1 flapjack --data-dir /tmp/d",
    ]
