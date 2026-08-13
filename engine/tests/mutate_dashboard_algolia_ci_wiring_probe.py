#!/usr/bin/env python3
"""Mutation-test the dashboard Algolia workflow wiring contract."""

import os
import shutil
import subprocess
import tempfile
from pathlib import Path


REPO_DIR = Path(__file__).resolve().parents[2]
CONTRACT_HARNESS = REPO_DIR / "engine/tests/test_dashboard_algolia_ci_wiring.sh"


def replace_once(text: str, old: str, new: str, label: str) -> str:
    if old not in text:
        raise AssertionError(f"{label}: did not find mutation target")
    return text.replace(old, new, 1)


def copy_inputs(tmp_dir: Path, suffix: str) -> dict[str, Path]:
    sources = {
        "ci": REPO_DIR / ".github/workflows/ci.yml",
        "nightly": REPO_DIR / ".github/workflows/nightly.yml",
        "package": REPO_DIR / "engine/dashboard/package.json",
        "config": REPO_DIR / "engine/dashboard/playwright.config.ts",
    }
    paths = {}
    for key, source in sources.items():
        target = tmp_dir / f"{suffix}-{source.name}"
        shutil.copy2(source, target)
        paths[key] = target
    return paths


def write_text(path: Path, text: str) -> None:
    path.write_text(text, encoding="utf-8")


def build_credential_mutants(tmp_dir: Path) -> list[tuple[str, dict[str, Path]]]:
    """Build cases that break credentialed integration-test selection."""
    mutants = []

    paths = copy_inputs(tmp_dir, "ci")
    write_text(
        paths["ci"],
        replace_once(
            paths["ci"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: npm run test:e2e-api",
            "ci creds step selects e2e-api",
        ),
    )
    mutants.append(("ci creds step selects e2e-api", paths))

    paths = copy_inputs(tmp_dir, "nightly")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            """      - name: Run integration tests
        working-directory: engine/dashboard
        run: npm run test:integration
        env:
          ALGOLIA_APP_ID: ${{ secrets.ALGOLIA_APP_ID }}
          ALGOLIA_ADMIN_KEY: ${{ secrets.ALGOLIA_ADMIN_KEY }}""",
            """      - name: Run integration tests
        working-directory: engine/dashboard
        run: npm run test:integration""",
            "nightly credentialed step removed",
        ),
    )
    mutants.append(("nightly credentialed step removed", paths))

    return mutants


def build_audit_wiring_mutants(
    tmp_dir: Path,
) -> list[tuple[str, dict[str, Path]]]:
    """Build cases that remove or reorder the deterministic audit policy suite."""
    mutants = []

    paths = copy_inputs(tmp_dir, "ci-audit-fixture-removed")
    write_text(
        paths["ci"],
        replace_once(
            paths["ci"].read_text(encoding="utf-8"),
            """      - name: Test production audit policy
        working-directory: engine/dashboard
        run: bash scripts/audit_gate_fixture_test.sh
""",
            "",
            "dashboard audit fixture step removed",
        ),
    )
    mutants.append(("dashboard audit fixture step removed", paths))

    paths = copy_inputs(tmp_dir, "ci-audit-fixture-after-live")
    ci_text = paths["ci"].read_text(encoding="utf-8")
    fixture_step = """      - name: Test production audit policy
        working-directory: engine/dashboard
        run: bash scripts/audit_gate_fixture_test.sh
"""
    ci_text = replace_once(
        ci_text,
        fixture_step,
        "",
        "dashboard audit fixture step moved",
    )
    ci_text = replace_once(
        ci_text,
        """      - name: Audit production dependencies
        working-directory: engine/dashboard
        run: bash scripts/audit_gate.sh
""",
        """      - name: Audit production dependencies
        working-directory: engine/dashboard
        run: bash scripts/audit_gate.sh
""" + fixture_step,
        "dashboard audit fixture step placed after live audit",
    )
    write_text(paths["ci"], ci_text)
    mutants.append(("dashboard audit fixture step placed after live audit", paths))

    return mutants


def build_report_artifact_mutants(
    tmp_dir: Path,
) -> list[tuple[str, dict[str, Path]]]:
    """Build cases that remove report isolation or artifact retention."""
    mutants = []

    paths = copy_inputs(tmp_dir, "nightly-report")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "          PLAYWRIGHT_HTML_OUTPUT_DIR: playwright-report-pages\n",
            "",
            "nightly page report isolation removed",
        ),
    )
    mutants.append(("nightly page report isolation removed", paths))

    paths = copy_inputs(tmp_dir, "nightly-upload")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "            engine/dashboard/playwright-report-pages/\n",
            "",
            "nightly page report upload removed",
        ),
    )
    mutants.append(("nightly page report upload removed", paths))

    paths = copy_inputs(tmp_dir, "nightly-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            """      - name: Run integration tests
        working-directory: engine/dashboard
        run: npm run test:integration
        env:
          ALGOLIA_APP_ID: ${{ secrets.ALGOLIA_APP_ID }}
          ALGOLIA_ADMIN_KEY: ${{ secrets.ALGOLIA_ADMIN_KEY }}""",
            """      - name: Run integration tests
        working-directory: engine/dashboard
        run: npm run test:integration
        env:
          ALGOLIA_APP_ID: ${{ secrets.ALGOLIA_APP_ID }}
          ALGOLIA_ADMIN_KEY: ${{ secrets.ALGOLIA_ADMIN_KEY }}
          PLAYWRIGHT_HTML_OUTPUT_DIR: playwright-report-pages""",
            "nightly integration report collides with page report",
        ),
    )
    mutants.append(("nightly integration report collides with page report", paths))

    return mutants


def build_direct_report_command_mutants(
    tmp_dir: Path,
) -> list[tuple[str, dict[str, Path]]]:
    """Build single-run inline and exported report-directory collisions."""
    mutants = []

    paths = copy_inputs(tmp_dir, "nightly-inline-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages npm run test:integration",
            "nightly inline integration report collides with page report",
        ),
    )
    mutants.append(("nightly inline integration report collides with page report", paths))

    paths = copy_inputs(tmp_dir, "nightly-later-inline-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: |\n"
            "          npx playwright install --with-deps chromium\n"
            "          PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages "
            "npm run test:integration",
            "nightly later inline integration report collides with page report",
        ),
    )
    mutants.append(
        ("nightly later inline integration report collides with page report", paths)
    )

    paths = copy_inputs(tmp_dir, "nightly-exported-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "        run: npm run test:integration",
            "        run: |\n"
            "          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages\n"
            "          npm run test:integration",
            "nightly exported integration report collides with page report",
        ),
    )
    mutants.append(
        ("nightly exported integration report collides with page report", paths)
    )

    paths = copy_inputs(tmp_dir, "nightly-exported-chain-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages && "
            "npm run test:integration",
            "nightly exported-and-chained integration report collides with page report",
        ),
    )
    mutants.append(
        (
            "nightly exported-and-chained integration report collides with page report",
            paths,
        )
    )

    return mutants


def build_multi_report_command_mutants(
    tmp_dir: Path,
) -> list[tuple[str, dict[str, Path]]]:
    """Build collisions involving setup commands or multiple Playwright runs."""
    mutants = []

    paths = copy_inputs(tmp_dir, "nightly-export-then-later-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: |\n"
            "          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages\n"
            "          npx playwright install --with-deps chromium\n"
            "          npm run test:integration",
            "nightly exported env carries into later integration report",
        ),
    )
    mutants.append(
        ("nightly exported env carries into later integration report", paths)
    )

    paths = copy_inputs(tmp_dir, "nightly-multi-run-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: |\n"
            "          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages\n"
            "          PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration\n"
            "          npm run test:integration",
            "nightly later runner inherits exported page report directory",
        ),
    )
    mutants.append(
        ("nightly later runner inherits exported page report directory", paths)
    )

    paths = copy_inputs(tmp_dir, "nightly-same-line-multi-run-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration ; "
            "PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages "
            "npm run test:integration",
            "nightly same-line later runner targets page report directory",
        ),
    )
    mutants.append(
        ("nightly same-line later runner targets page report directory", paths)
    )

    return mutants


def build_inherited_report_mutants(
    tmp_dir: Path,
) -> list[tuple[str, dict[str, Path]]]:
    """Build job, workflow, and Playwright-config report collisions."""
    mutants = []

    paths = copy_inputs(tmp_dir, "nightly-job-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            """  dashboard-all:
    name: Dashboard all tests
    needs: check-repo
    if: needs.check-repo.outputs.is-public-repo == 'true'
    runs-on: ubuntu-latest
    steps:""",
            """  dashboard-all:
    name: Dashboard all tests
    needs: check-repo
    if: needs.check-repo.outputs.is-public-repo == 'true'
    runs-on: ubuntu-latest
    env:
      PLAYWRIGHT_HTML_OUTPUT_DIR: playwright-report-pages
    steps:""",
            "nightly job report environment collides with page report",
        ),
    )
    mutants.append(("nightly job report environment collides with page report", paths))

    paths = copy_inputs(tmp_dir, "nightly-workflow-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "env:\n  CARGO_TERM_COLOR: always",
            "env:\n  CARGO_TERM_COLOR: always\n  PLAYWRIGHT_HTML_OUTPUT_DIR: playwright-report-pages",
            "nightly workflow report environment collides with page report",
        ),
    )
    mutants.append(("nightly workflow report environment collides with page report", paths))

    paths = copy_inputs(tmp_dir, "nightly-quoted-workflow-report-collision")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "env:\n  CARGO_TERM_COLOR: always",
            "env:\n  CARGO_TERM_COLOR: always\n"
            '  PLAYWRIGHT_HTML_OUTPUT_DIR: "./playwright-report-pages/"',
            "nightly quoted workflow report environment collides with page report",
        ),
    )
    mutants.append(
        ("nightly quoted workflow report environment collides with page report", paths)
    )

    paths = copy_inputs(tmp_dir, "config-report-collision")
    write_text(
        paths["config"],
        replace_once(
            paths["config"].read_text(encoding="utf-8"),
            "['html', { open: 'never' }]",
            "['html', { open: 'never', outputFolder: 'playwright-report-pages' }]",
            "configured HTML report directory collides with page report",
        ),
    )
    mutants.append(("configured HTML report directory collides with page report", paths))

    return mutants


def build_selection_mutants(tmp_dir: Path) -> list[tuple[str, dict[str, Path]]]:
    """Build Playwright project ownership and npm-script selection defects."""
    mutants = []

    paths = copy_inputs(tmp_dir, "config")
    write_text(
        paths["config"],
        replace_once(
            paths["config"].read_text(encoding="utf-8"),
            """    {
      name: 'e2e-api',
      testDir: './tests/e2e-api',
      use: { ...devices['Desktop Chrome'] },
    },""",
            """    {
      name: 'e2e-api',
      testDir: './tests/e2e-ui/full',
      testMatch: 'migrate-algolia.spec.ts',
      use: { ...devices['Desktop Chrome'] },
    },""",
            "migration spec moved to non-credentialed project",
        ),
    )
    mutants.append(("migration spec moved to non-credentialed project", paths))

    paths = copy_inputs(tmp_dir, "package")
    package_text = paths["package"].read_text(encoding="utf-8")
    write_text(
        paths["package"],
        replace_once(
            package_text,
            "playwright test --project=e2e-api \\\"$@\\\" &&",
            "playwright test --project=e2e-ui \\\"$@\\\" &&",
            "test:integration project selection changed",
        ),
    )
    mutants.append(("test:integration project selection changed", paths))

    return mutants


def build_mutants(tmp_dir: Path) -> list[tuple[str, dict[str, Path]]]:
    """Compose every independently owned mutation family."""
    builders = (
        build_credential_mutants,
        build_audit_wiring_mutants,
        build_report_artifact_mutants,
        build_direct_report_command_mutants,
        build_multi_report_command_mutants,
        build_inherited_report_mutants,
        build_selection_mutants,
    )
    return [mutant for builder in builders for mutant in builder(tmp_dir)]


def build_allowed_variants(tmp_dir: Path) -> list[tuple[str, dict[str, Path]]]:
    variants = []

    paths = copy_inputs(tmp_dir, "nightly-empty-inline-clears-exported-report")
    write_text(
        paths["nightly"],
        replace_once(
            paths["nightly"].read_text(encoding="utf-8"),
            "run: npm run test:integration",
            "run: |\n"
            "          export PLAYWRIGHT_HTML_OUTPUT_DIR=playwright-report-pages\n"
            "          npx playwright install --with-deps chromium\n"
            "          PLAYWRIGHT_HTML_OUTPUT_DIR= npm run test:integration",
            "nightly empty inline report env clears exported report collision",
        ),
    )
    variants.append(
        ("nightly empty inline report env clears exported report collision", paths)
    )

    return variants


def run_contract(mutant_paths: dict[str, Path]) -> subprocess.CompletedProcess[str]:
    """Run the contract against one isolated set of mutated inputs."""
    env = os.environ.copy()
    env.update(
        {
            "DASHBOARD_CI_WORKFLOW_PATH": str(mutant_paths["ci"]),
            "DASHBOARD_NIGHTLY_WORKFLOW_PATH": str(mutant_paths["nightly"]),
            "DASHBOARD_PACKAGE_JSON_PATH": str(mutant_paths["package"]),
            "DASHBOARD_PLAYWRIGHT_CONFIG_PATH": str(mutant_paths["config"]),
        }
    )
    return subprocess.run(
        ["bash", str(CONTRACT_HARNESS)],
        cwd=REPO_DIR,
        env=env,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        timeout=120,
    )


def main() -> int:
    """Require every defect mutant to go red and allowed variant to stay green."""
    failures = []
    with tempfile.TemporaryDirectory() as tmp_raw:
        tmp_dir = Path(tmp_raw)
        for name, mutant_paths in build_mutants(tmp_dir):
            result = run_contract(mutant_paths)
            if result.returncode == 0:
                print(f"FAIL(green mutant): {name}")
                failures.append((name, result.stdout))
            else:
                print(f"PASS(red): {name}")

        for name, variant_paths in build_allowed_variants(tmp_dir):
            result = run_contract(variant_paths)
            if result.returncode == 0:
                print(f"PASS(green allowed variant): {name}")
            else:
                print(f"FAIL(red allowed variant): {name}")
                failures.append((name, result.stdout))

    for name, output in failures:
        print(f"===== unexpected green mutant: {name} =====")
        print(output)

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
