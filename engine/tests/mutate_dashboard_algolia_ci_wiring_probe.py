#!/usr/bin/env python3

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


def build_mutants(tmp_dir: Path) -> list[tuple[str, dict[str, Path]]]:
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


def run_contract(mutant_paths: dict[str, Path]) -> subprocess.CompletedProcess[str]:
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
    failures = []
    with tempfile.TemporaryDirectory() as tmp_raw:
        for name, mutant_paths in build_mutants(Path(tmp_raw)):
            result = run_contract(mutant_paths)
            if result.returncode == 0:
                print(f"FAIL(green mutant): {name}")
                failures.append((name, result.stdout))
            else:
                print(f"PASS(red): {name}")

    for name, output in failures:
        print(f"===== unexpected green mutant: {name} =====")
        print(output)

    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
