#!/usr/bin/env python3
"""Fail the build when a workflow runs dashboard e2e specs against a backend that cannot satisfy them.

WHY THIS EXISTS
---------------
Nightly run 31072375322 (2026-08-06) and prod push CI run 31096601354 (same day)
each failed on a *different* dashboard spec, for the *same* structural reason: the
CI job that starts the Flapjack backend and the spec that declares a backend
precondition had no shared owner, so they drifted.

  - `nightly.yml` "Dashboard all tests" starts a standalone backend and runs
    `npm run test:pages`, which is `playwright test --project=e2e-ui` with no path
    filter — so it includes `tests/e2e-ui/full/cluster_peers.spec.ts`, which throws
    unless replication is configured.
  - `ci.yml` "Dashboard full e2e tests" starts a replication-enabled backend but omits
    `FLAPJACK_AI_ALLOW_LOCAL_URLS`, so `tests/e2e-ui/full/vector-settings.spec.ts`
    trips the outbound SSRF control three times.

Both specs were correct. Both passed the lane that authored them, because that lane
started a backend that satisfied them. Nothing failed until the spec reached a job
whose backend was configured by a different hand.

WHY THE ENV MUST BE ON THE STARTUP STEP
---------------------------------------
`scripts/playwright-webserver.mjs::startPlaywrightServers` passes `allowReuse: true`
for the backend, so a backend already listening on the target port is REUSED and
`spawnBackendServer` — the one place that sets the right environment — never runs.
Environment declared on the Playwright step therefore never reaches the process under
test. This gate deliberately requires the env on the step that launches the binary,
because that is the only step that can supply it.

WHAT IT CHECKS
--------------
1. Every workflow job that starts a Flapjack backend and later runs a dashboard
   Playwright script declares every environment variable required by the specs that
   script actually selects. The selection is resolved from `package.json`, not
   restated here, so a script that gains a path filter cannot escape the contract.
1b. Requirements marked `"base": true` are additionally required of EVERY such job
   regardless of its spec selection, because `assertBackendReadiness` enforces them
   itself and refuses the backend at webServer startup before any spec runs. Checking
   those against the spec selection was a hole in this gate, not a design: on
   2026-08-07 `ci.yml`'s `dashboard-pages` and `dashboard-integration` jobs carried
   neither base requirement, this gate exited 0, and staging run 31213083385 failed
   both jobs with `refused the dashboard local-URL embedder probe`. A static contract
   that exists to guarantee a runtime assertion must be at least as strict as it.
2. `spawnBackendServer` supplies the union of every requirement, because it starts one
   backend for whatever a developer runs locally and must satisfy the widest scope.

WHAT IT DELIBERATELY DOES NOT CHECK
-----------------------------------
It does not run the specs, and it does not verify the running backend reports the
capability. A value can be present and wrong. This is a wiring contract: it catches
the omission that has now happened twice, cheaply, without a browser. The runtime half
is `assertBackendReadiness` in `engine/dashboard/scripts/playwright-webserver.mjs`,
which refuses at startup rather than at spec time.

It also finds a job's backend only by `BACKEND_LAUNCH_RE`, i.e. a `flapjack --data-dir`
invocation. A job that starts one some other way — a container, a wrapper script, a
different flag order — is skipped rather than reported, because "no backend launch
found" is also the correct reading of a job that lets Playwright spawn its own. If a
future job starts its backend differently, extend that pattern; do not assume silence
here means the job was checked.

NON-VACUITY
-----------
Named live specimens at `dcdb8a77a`: this gate reports two findings and exits 1 —
`nightly.yml` job `dashboard` missing `replication` and `ai_local_outbound`, and
`ci.yml` job `dashboard-full-e2e` missing `ai_local_outbound`. Deleting
`FLAPJACK_AI_ALLOW_LOCAL_URLS` from either fixed job, or from `spawnBackendServer`,
returns it to red today.
"""

from __future__ import annotations

import json
from collections import Counter
from dataclasses import dataclass
import re
import sys
from pathlib import Path

CONTRACT_MODULE_DIR = Path(__file__).resolve().parent
if str(CONTRACT_MODULE_DIR) not in sys.path:
    sys.path.insert(0, str(CONTRACT_MODULE_DIR))

from dashboard_backend_shell import (  # noqa: E402 - path is established above
    BACKEND_LAUNCH_RE,
    active_shell_commands,
    collect_prefix_env,
)

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKFLOW_DIR = REPO_ROOT / ".github" / "workflows"
DASHBOARD_DIR = REPO_ROOT / "engine" / "dashboard"
CONTRACT_PATH = DASHBOARD_DIR / "tests" / "e2e_backend_contract.json"
PACKAGE_JSON_PATH = DASHBOARD_DIR / "package.json"
WEBSERVER_PATH = DASHBOARD_DIR / "scripts" / "playwright-webserver.mjs"

# `npm run <script>` / `npm run <script> -- ...`, the only way these workflows enter
# the dashboard test suite.
NPM_RUN_RE = re.compile(r"\bnpm\s+run\s+([A-Za-z0-9:_-]+)")

class Finding(Exception):
    """A contract violation, carried with enough detail to act on without re-deriving it."""


@dataclass(frozen=True)
class DashboardBackendContext:
    """The effective environment and test entry points for one workflow backend."""

    job_id: str
    step_name: str
    environment: dict[str, str]
    selected_scripts: tuple[str, ...]


def load_yaml(path: Path):
    """Parse a workflow. A gate that cannot parse its input must fail, never skip."""
    try:
        import yaml  # noqa: PLC0415 - imported here so the failure message can be specific
    except ImportError:  # pragma: no cover - environment defect, not a contract defect
        print(
            "FATAL: PyYAML is required to read the workflow files. A contract gate that "
            "cannot read its input has not verified anything, so this is a failure rather "
            "than a skip. Install with: python3 -m pip install pyyaml",
            file=sys.stderr,
        )
        raise SystemExit(2)
    return yaml.safe_load(path.read_text(encoding="utf-8"))


def test_report_missing_workflow_exports_fails_when_backend_context_is_absent(
    tmp_path: Path, capsys
) -> None:
    contract_path = tmp_path / "contract.json"
    workflow_path = tmp_path / "workflow.yml"
    contract_path.write_text(
        json.dumps(
            {
                "npm_script_spec_scopes": {
                    "playwright_entry_points": ["test:pages"],
                },
            }
        ),
        encoding="utf-8",
    )
    workflow_path.write_text(
        """
name: fixture
jobs:
  dashboard:
    steps:
      - name: Run dashboard pages
        run: npm run test:pages
""",
        encoding="utf-8",
    )

    result = report_missing_workflow_exports(
        [
            str(contract_path),
            "ci.yml",
            str(workflow_path),
            "FLAPJACK_AI_ALLOW_LOCAL_URLS",
        ]
    )

    captured = capsys.readouterr()
    assert result == 2
    assert captured.out == ""
    assert (
        captured.err
        == "ci.yml: no dashboard backend launch context found for workflow export check\n"
    )


def test_report_missing_workflow_exports_ignores_unrelated_backend_launch(
    tmp_path: Path, capsys
) -> None:
    contract_path = tmp_path / "contract.json"
    workflow_path = tmp_path / "workflow.yml"
    contract_path.write_text(
        json.dumps(
            {
                "npm_script_spec_scopes": {
                    "playwright_entry_points": ["test:pages"],
                },
            }
        ),
        encoding="utf-8",
    )
    workflow_path.write_text(
        """
name: fixture
jobs:
  dashboard:
    steps:
      - name: Run dashboard pages
        run: npm run test:pages
  unrelated:
    steps:
      - name: Start unrelated backend
        run: engine/target/debug/flapjack --data-dir /tmp/unrelated-data
""",
        encoding="utf-8",
    )

    result = report_missing_workflow_exports(
        [
            str(contract_path),
            "ci.yml",
            str(workflow_path),
            "FLAPJACK_AI_ALLOW_LOCAL_URLS",
        ]
    )

    captured = capsys.readouterr()
    assert result == 2
    assert captured.out == ""
    assert (
        captured.err
        == "ci.yml: no dashboard backend launch context found for workflow export check\n"
    )


def active_shell_run_body(run_body: str) -> str:
    """The executable shell text of a run block, one logical command per line."""
    return "\n".join(active_shell_commands(run_body))


def step_env(step: dict) -> dict[str, str]:
    """Environment a step declares through GitHub's own `env:` mapping."""
    declared = step.get("env") or {}
    return {str(k): str(v) for k, v in declared.items()}


def resolve_script_paths(invocation: str) -> list[str]:
    """Spec paths named as arguments to one Playwright invocation.

    An invocation with no path argument selects its whole project, which is exactly how
    `test:pages` came to run the `full/` specs in the nightly. That case returns the empty
    list and is treated by the caller as "the whole project".
    """
    return [token.strip("'\"") for token in invocation.split() if token.startswith("tests/")]


# Playwright project -> testDir, from engine/dashboard/playwright.config.ts. An invocation
# with no path filter selects everything under its project's testDir.
PROJECT_TEST_DIRS = {
    "--project=e2e-ui": "tests/e2e-ui/",
    "--project=e2e-api": "tests/e2e-api/",
}


def playwright_invocations(script_body: str) -> list[str]:
    """Each `playwright test ...` command in an npm script, separately.

    Splitting matters. `test:integration` chains two: the e2e-api project with NO path
    filter, then one named e2e-ui spec. Evaluating the script as a single blob let the
    second invocation's path filter mask the first invocation's unfiltered selection, so a
    requirement naming any `tests/e2e-api/` spec was silently unchecked — a false negative
    in the guard itself, confirmed by probe before this was rewritten. Splitting on shell
    command separators keeps each invocation's project and paths together, which is how the
    shell actually runs them.
    """
    invocations = []
    for chunk in re.split(r"&&|\|\||;", script_body):
        marker = chunk.find("playwright test")
        if marker != -1:
            invocations.append(chunk[marker:])
    return invocations


def invocation_selects(spec_path: str, invocation: str) -> bool:
    """Does one Playwright invocation run `spec_path`?"""
    test_dir = next(
        (d for flag, d in PROJECT_TEST_DIRS.items() if flag in invocation), None
    )
    if test_dir is None:
        # No recognised --project. Do not guess: an unrecognised invocation is reported by
        # returning False here and would surface as a requirement going unchecked, which is
        # why PROJECT_TEST_DIRS must be extended whenever a project is added.
        return False
    selected = resolve_script_paths(invocation)
    if not selected:
        return spec_path.startswith(test_dir)
    return any(spec_path == sel or spec_path.startswith(sel) for sel in selected)


def script_selects(spec_path: str, script_body: str) -> bool:
    """Does this npm script run `spec_path` through any of its Playwright invocations?"""
    return any(
        invocation_selects(spec_path, invocation)
        for invocation in playwright_invocations(script_body)
    )


def dashboard_backend_contexts(
    document: dict, entry_points: set[str]
) -> list[DashboardBackendContext]:
    """Resolve dashboard backend launches using GitHub's environment precedence."""
    contexts = []
    workflow_env = {str(k): str(v) for k, v in (document.get("env") or {}).items()}

    for job_id, job in (document.get("jobs") or {}).items():
        job_env = {str(k): str(v) for k, v in (job.get("env") or {}).items()}
        backend_env: dict[str, str] | None = None
        backend_step_name = ""
        selected_scripts = []

        for step in job.get("steps") or []:
            if not isinstance(step, dict):
                continue
            run_body = step.get("run") or ""
            active_run_body = active_shell_run_body(run_body)
            if BACKEND_LAUNCH_RE.search(active_run_body):
                backend_step_name = step.get("name") or "<unnamed step>"
                backend_env = {
                    **workflow_env,
                    **job_env,
                    **step_env(step),
                    **collect_prefix_env(run_body),
                }
                continue
            selected_scripts.extend(
                name
                for name in NPM_RUN_RE.findall(active_run_body)
                if name in entry_points
            )

        if backend_env is not None and selected_scripts:
            contexts.append(
                DashboardBackendContext(
                    job_id=str(job_id),
                    step_name=backend_step_name,
                    environment=backend_env,
                    selected_scripts=tuple(selected_scripts),
                )
            )
    return contexts


def has_inactive_dashboard_backend_launch(document: dict, entry_points: set[str]) -> bool:
    """A dashboard-testing job contains backend launch text that is not executable."""
    for job in (document.get("jobs") or {}).values():
        has_raw_launch = False
        selected_scripts = []

        for step in job.get("steps") or []:
            if not isinstance(step, dict):
                continue
            run_body = step.get("run") or ""
            active_run_body = active_shell_run_body(run_body)
            has_raw_launch = has_raw_launch or BACKEND_LAUNCH_RE.search(run_body) is not None
            selected_scripts.extend(
                name
                for name in NPM_RUN_RE.findall(active_run_body)
                if name in entry_points
            )

        if has_raw_launch and selected_scripts:
            return True
    return False


def check_workflows(contract: dict, scripts: dict[str, str]) -> list[str]:
    findings: list[str] = []
    entry_points = set(contract["npm_script_spec_scopes"]["playwright_entry_points"])

    for workflow_path in sorted(WORKFLOW_DIR.glob("*.yml")):
        document = load_yaml(workflow_path)
        for context in dashboard_backend_contexts(document, entry_points):
            for requirement in contract["requirements"]:
                # A base requirement is one assertBackendReadiness enforces itself, so it
                # applies to every job that starts a backend and runs any dashboard entry
                # point — the spec selection is irrelevant. Checking base requirements
                # against the selection is precisely the bug this branch exists to fix:
                # dashboard-pages selects only smoke specs, named by no requirement, so the
                # old code `continue`d and the job shipped without an env the harness
                # refuses to start without. See the contract's BASE vs PER-SPEC note.
                is_base = bool(requirement.get("base"))
                needed_by = [
                    spec
                    for spec in requirement["required_by"]
                    for script in context.selected_scripts
                    if script_selects(spec, scripts.get(script, ""))
                ]
                if not is_base and not needed_by:
                    continue
                # `.get`, not `[...]`: a requirement may declare only `env_absent`.
                missing = [
                    key
                    for key in requirement.get("env", {})
                    if key not in context.environment
                ]
                if not missing:
                    continue
                if is_base:
                    findings.append(
                        f"{workflow_path.name} job '{context.job_id}' step "
                        f"'{context.step_name}' "
                        f"starts a backend without BASE requirement '{requirement['id']}' "
                        f"(missing {', '.join(sorted(missing))}). A base requirement is "
                        "enforced by assertBackendReadiness on every backend the harness "
                        "accepts or spawns, so it applies no matter which specs the job "
                        "selects — this job runs "
                        f"{sorted(set(context.selected_scripts))}, and the "
                        "harness refuses at webServer startup before any spec executes "
                        f"({requirement.get('why_base', '')!r}). Symptom when missing: "
                        f"{requirement['symptom_if_missing']!r}. The env must be on the "
                        f"backend startup step — Playwright reuses an already-listening "
                        f"backend, so env on the test step never reaches it."
                    )
                else:
                    findings.append(
                        f"{workflow_path.name} job '{context.job_id}' step "
                        f"'{context.step_name}' "
                        f"starts a backend without requirement '{requirement['id']}' "
                        f"(missing {', '.join(sorted(missing))}), but the job runs "
                        f"{sorted(set(context.selected_scripts))} which selects "
                        f"{sorted(set(needed_by))}. Symptom when missing: "
                        f"{requirement['symptom_if_missing']!r}. The env must be on the "
                        f"backend startup step — Playwright reuses an already-listening "
                        f"backend, so env on the test step never reaches it."
                    )
    return findings


def report_missing_workflow_exports(arguments: list[str]) -> int:
    """Print required exports absent from any dashboard backend launch context."""
    if len(arguments) < 4:
        print(
            "usage: --missing-workflow-exports CONTRACT WORKFLOW_LABEL WORKFLOW EXPORT...",
            file=sys.stderr,
        )
        return 2

    contract_path, workflow_label, workflow_path, *required_exports = arguments
    contract = json.loads(Path(contract_path).read_text(encoding="utf-8"))
    document = load_yaml(Path(workflow_path))
    entry_points = set(contract["npm_script_spec_scopes"]["playwright_entry_points"])
    contexts = dashboard_backend_contexts(document, entry_points)

    if not contexts:
        if has_inactive_dashboard_backend_launch(document, entry_points):
            for export in required_exports:
                print(f"{workflow_label}:{export}")
            return 0
        print(
            f"{workflow_label}: no dashboard backend launch context found for workflow export check",
            file=sys.stderr,
        )
        return 2

    for export in required_exports:
        if any(export not in context.environment for context in contexts):
            print(f"{workflow_label}:{export}")
    return 0


def check_spawn_backend_server(_contract: dict) -> list[str]:
    """The locally-spawned backend must derive its environment from the same contract.

    This is a WIRING check only. Whether `spawnBackendServer` actually applies the
    contract is asserted behaviourally — by calling it — in
    `engine/dashboard/playwright-webserver.test.ts`, which is a stronger check than any
    grep and is where it belongs. What this adds is that the wiring cannot be deleted
    silently: if `spawnBackendServer` stops reading the contract and goes back to
    literals, the vitest assertion could still be made to pass by copying the values,
    and the two owners would be free to drift again. That is the failure this contract
    exists to prevent, so it is checked here rather than left to review.
    """
    source = WEBSERVER_PATH.read_text(encoding="utf-8")
    findings = []

    if "e2e_backend_contract.json" not in source:
        findings.append(
            f"{WEBSERVER_PATH.relative_to(REPO_ROOT)} does not read "
            f"{CONTRACT_PATH.relative_to(REPO_ROOT)}. The locally-spawned e2e backend must "
            "derive its capability environment from the same declared contract the CI "
            "workflows are held to, otherwise the two can drift — which is the defect the "
            "contract was created for."
        )

    match = re.search(r"export function spawnBackendServer\(.*?\n\}", source, re.DOTALL)
    if match is None:
        findings.append(
            f"{WEBSERVER_PATH.relative_to(REPO_ROOT)}: spawnBackendServer not found. It is "
            "the single owner of the locally-spawned e2e backend environment; this gate "
            "cannot verify a contract against a function that no longer exists."
        )
    # The application call, not the identifier. Checking for `contractEnv` alone stayed
    # green when the value was deleted, because the parameter name survived in the
    # signature — a false green caught by mutation-testing this gate rather than by
    # reading it. `applyBackendContractEnvironment` owns both positive env injection and
    # `env_absent` deletion, so a literal `...contractEnv` spread is no longer sufficient.
    elif "applyBackendContractEnvironment(process.env, contractEnv)" not in match.group(0):
        findings.append(
            f"{WEBSERVER_PATH.relative_to(REPO_ROOT)}: spawnBackendServer accepts the "
            "contract environment but never applies it to the spawned process env, so "
            "the contract has no effect. Behavioural owner of this assertion: "
            "engine/dashboard/playwright-webserver.test.ts."
        )

    return findings


def check_gate_is_wired() -> list[str]:
    """This gate must itself be invoked by CI.

    `engine/tests/test_release_workflow_structure.sh` asserted `release.yml`'s shape for
    months while no workflow invoked it, so every assertion in it was inert. A contract
    test that nothing runs is not a guard, and the cheapest way to keep this one honest
    is to make unwiring it self-detecting. Anchored to an actual `run:` line, because a
    bare path match is also satisfied by a commented-out invocation — which is exactly
    how a suite gets quietly disabled.
    """
    invocation = re.compile(
        r"^\s*run: python3 engine/tests/test_dashboard_e2e_backend_contract\.py\s*$",
        re.MULTILINE,
    )
    for workflow_path in WORKFLOW_DIR.glob("*.yml"):
        if invocation.search(workflow_path.read_text(encoding="utf-8")):
            return []
    return [
        "No workflow invokes this contract. A gate nothing runs has verified nothing; "
        "restore the `run: python3 engine/tests/test_dashboard_e2e_backend_contract.py` "
        "step (it lives in ci.yml's `release-contracts` job)."
    ]


# Probes `assertBackendReadiness` runs that no workflow environment can satisfy, so no
# requirement may claim them. `assertVectorSearchBuild` reads a build-time capability off
# `/1/capabilities`; it fails on a binary compiled without vector search, which is a build
# defect, not a missing env var. Extend this set only for probes with the same property.
READINESS_PROBES_NEEDING_NO_ENV = {"assertVectorSearchBuild"}


def check_base_requirements_match_readiness(contract: dict) -> list[str]:
    """The `base` flags must agree with what `assertBackendReadiness` actually enforces.

    WHY THIS EXISTS — it closes a hole this gate had after the base/per-spec split landed.
    Mutation-testing the new logic showed that simply deleting `"base": true` from a
    requirement returned the gate to green, silently restoring the per-spec model whose
    hole let `dashboard-pages` and `dashboard-integration` ship without either base
    requirement. A flag that can be deleted without a red is not a contract; it is a
    comment. That is the same "a guard that cannot fail is not a guard" defect the base
    split was introduced to fix, one level up, so it is fixed here rather than noted.

    HOW IT IS FALSIFIABLE IN BOTH DIRECTIONS. Each base requirement names the runtime
    function that enforces it in `enforced_by`, and this check reads the body of
    `assertBackendReadiness` to compare:

      - a requirement naming an enforcer that is not called there  -> RED (stale claim)
      - a requirement naming an enforcer but not marked `base`     -> RED (the M2 mutation)
      - a probe called there that no requirement claims            -> RED (new unclaimed
        requirement, or `base` AND `enforced_by` deleted together)

    So the flag cannot be removed, the probe cannot be removed, and a newly added probe
    cannot be left unmodelled. Verified by running all three mutations.
    """
    source = WEBSERVER_PATH.read_text(encoding="utf-8")
    # Anchor the end on a closing brace at column 0 FOLLOWED BY A NEWLINE. Matching a
    # bare `\n}` stops at the destructured parameter block, whose `} = {}) {` line also
    # begins with a newline and a brace — the body then contains no calls at all. That
    # mis-extraction was caught by this check going red rather than silently green, which
    # is the behaviour to preserve: an empty call set fails the coverage loop below.
    match = re.search(
        r"export async function assertBackendReadiness\(.*?\n\}\n", source, re.DOTALL
    )
    if match is None:
        return [
            f"{WEBSERVER_PATH.relative_to(REPO_ROOT)}: assertBackendReadiness not found. "
            "It is the runtime owner of every base requirement, so this gate cannot "
            "verify the contract's `base` flags against anything. If it was renamed, "
            "update this check and the contract's `enforced_by` values together."
        ]

    # `await <fn>(` is how every probe is invoked in that function. Deliberately not a
    # full JS parse: the body is six lines and a regex that stops matching is a red here,
    # not a silent skip, because an empty call set fails the coverage check below.
    called = set(re.findall(r"await\s+([A-Za-z_][A-Za-z0-9_]*)\s*\(", match.group(0)))

    findings: list[str] = []
    claimed: dict[str, dict] = {}
    for requirement in contract["requirements"]:
        enforcer = requirement.get("enforced_by")
        if not enforcer:
            continue
        claimed[enforcer] = requirement
        if enforcer not in called:
            findings.append(
                f"contract requirement '{requirement['id']}' declares enforced_by "
                f"{enforcer!r}, but assertBackendReadiness does not call it. Either the "
                "probe was removed — in which case the requirement is no longer base and "
                "the flag must go — or it was renamed and the contract is now stale."
            )
        if not requirement.get("base"):
            findings.append(
                f"contract requirement '{requirement['id']}' declares enforced_by "
                f"{enforcer!r}, which assertBackendReadiness runs for every backend, but "
                "it is not marked \"base\": true. It would then be checked only against a "
                "job's spec selection, so a job selecting none of its `required_by` specs "
                "would pass this gate and still be refused at webServer startup."
            )

    for probe in sorted(called - READINESS_PROBES_NEEDING_NO_ENV - set(claimed)):
        findings.append(
            f"assertBackendReadiness calls {probe!r}, but no contract requirement claims "
            "it via `enforced_by`. Every readiness probe that a workflow environment can "
            "satisfy must be modelled as a base requirement, or jobs can omit the env it "
            "needs and this gate will not notice. If the probe needs no env, add it to "
            "READINESS_PROBES_NEEDING_NO_ENV with the reason."
        )
    return findings


def check_no_duplicate_step_names() -> list[str]:
    """A job may not declare the same step name twice.

    WHY THIS LIVES IN THIS FILE. This gate already asserts that CI *invokes* it
    (`check_gate_is_wired`). Being invoked is not the same as being reached: GitHub stops
    a job at its first failing step, so any earlier step that fails takes every later step
    with it — including this gate's own. A duplicated step is therefore not a cosmetic
    defect for this gate specifically; it is one of the ways this gate silently stops
    running while still appearing wired.

    NAMED LIVE SPECIMEN. On 2026-08-06, `ad2cacbb2` added a second
    `Release CI-status preflight contract` step to `ci.yml`'s `release-contracts` job,
    duplicating the one `b2ede54ea` had already added — but the earlier copy carries
    `GH_TOKEN` and the new one does not. In staging run `31213083385` the unauthenticated
    copy failed 17 of its 41 cases (`gh: To use GitHub CLI in a GitHub Actions workflow,
    set the GH_TOKEN environment variable`), which aborted the whole job before its
    `Dashboard e2e backend contract` step ran. So on the very run where two other jobs
    were red on a missing backend requirement, the guard that detects exactly that had
    not executed.

    WHY A DUPLICATE IS ALWAYS WRONG HERE, NOT MERELY SUSPICIOUS. Two steps with one name
    are indistinguishable in the Actions log, so a reader cannot tell which one failed
    without opening the raw text. More importantly a duplicate is the signature of a fix
    applied *by addition* rather than by edit: someone noticed the missing token and added
    a corrected step instead of correcting the existing one, leaving the broken original
    in place and running. Probed across every workflow at the time of writing, this was
    the only occurrence, so the rule costs nothing to hold.

    Only steps with an explicit `name:` are compared. Unnamed `uses:` steps legitimately
    repeat (`actions/checkout` in many jobs) and carry no log ambiguity.
    """
    findings: list[str] = []
    for workflow_path in sorted(WORKFLOW_DIR.glob("*.yml")):
        document = load_yaml(workflow_path)
        for job_id, job in (document.get("jobs") or {}).items():
            counts = Counter(
                step["name"]
                for step in (job.get("steps") or [])
                if isinstance(step, dict) and step.get("name")
            )
            for name, count in sorted(counts.items()):
                if count > 1:
                    findings.append(
                        f"{workflow_path.name} job '{job_id}' declares the step "
                        f"{name!r} {count} times. Two steps with one name are "
                        "indistinguishable in the Actions log, and a duplicate is "
                        "normally a fix applied by addition that left the broken "
                        "original running. Merge them into one step rather than "
                        "deleting either blindly — the copies may differ (the "
                        "2026-08-06 specimen differed only in whether it set GH_TOKEN)."
                    )
    return findings


def main() -> int:
    contract = json.loads(CONTRACT_PATH.read_text(encoding="utf-8"))
    scripts = json.loads(PACKAGE_JSON_PATH.read_text(encoding="utf-8"))["scripts"]

    # A contract with no requirements would make every assertion below vacuously true.
    if not contract.get("requirements"):
        print("FATAL: the contract declares no requirements; this gate would pass vacuously.", file=sys.stderr)
        return 2

    findings = (
        check_workflows(contract, scripts)
        + check_spawn_backend_server(contract)
        + check_gate_is_wired()
        + check_no_duplicate_step_names()
        + check_base_requirements_match_readiness(contract)
    )

    if findings:
        print("Dashboard e2e backend contract: FAILED\n", file=sys.stderr)
        for finding in findings:
            print(f"  - {finding}\n", file=sys.stderr)
        print(
            f"{len(findings)} violation(s). Owner of the requirement set: "
            f"{CONTRACT_PATH.relative_to(REPO_ROOT)}",
            file=sys.stderr,
        )
        return 1

    print(
        "Dashboard e2e backend contract: OK "
        f"({len(contract['requirements'])} requirement(s) checked against "
        f"{len(list(WORKFLOW_DIR.glob('*.yml')))} workflow file(s) and spawnBackendServer)"
    )
    return 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["--missing-workflow-exports"]:
        raise SystemExit(report_missing_workflow_exports(sys.argv[2:]))
    raise SystemExit(main())
