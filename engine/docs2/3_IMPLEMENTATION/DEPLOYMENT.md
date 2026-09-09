# Deployment & Infrastructure

This document is the maintained deployment entry point for the open-source
Flapjack repo.

For authoritative env-var names/defaults, see [OPS_CONFIGURATION.md](./OPS_CONFIGURATION.md).
For upgrade/rollback and operator runbooks, see [OPERATIONS.md](./OPERATIONS.md).
For the public hardening baseline, see [SECURITY_BASELINE.md](./SECURITY_BASELINE.md).
For launch-blocker status, see [../1_STRATEGY/HIGHEST_PRIORITY.md](../1_STRATEGY/HIGHEST_PRIORITY.md).
For shipped/readiness status, see [../FEATURES.md](../FEATURES.md).

## Repository & deploy topology

Flapjack spans one private development repository, one deliberately public
product repository, and a separate fjcloud control plane. Knowing which is
which avoids the most common traps when checking release or engine status:

- **Repos:** `gridl-dev/flapjack_dev` is the private source of truth with Actions
  disabled. `flapjackhq/flapjack` is the sole public OSS, CI, and release
  repository. `scripts/publish_public_candidate.sh` renders Debbie's whitelist
  from exact private `main` into `public-candidate/<dev-sha>` and opens a public
  PR; the stable **Public candidate gate** must pass before that PR is merged.
  The daily shadow candidate runs `scripts/shadow_public_candidate.sh` from a pinned runner clone; launchd requests one run each day at 09:00 host-local time.
  A successful reconciliation leaves exactly one open candidate for the private-main SHA fetched by that run, or zero when the fetched public-main manifest already names that SHA; lock contention is a logged exit-zero no-op without reconciliation, while failures exit nonzero with an actionable log message.
  It never auto-merges, approves, tags, releases, or dispatches workflows; operators use `gui/<uid>/com.flapjack.shadow-candidate` and `/Users/stuart/.matt/shadow_candidate.log`.
  **Verifying the installed runner:** `scripts/shadow_public_candidate.sh --check-runner`
  is the only supported way to confirm an installed runner without reconciling. It takes
  the same lock, performs the same single private `fetch origin main`, creates the same
  detached execution worktree at the fetched SHA, and runs the same provenance validation
  as a normal run, then exits before touching the public repository or GitHub. Verified
  provenance requires **both** signals, checked separately: the observed process result
  `PROBE_EXIT=0` (capture it as `PROBE_EXIT=0; ... || PROBE_EXIT=$?`, never as an
  unconditional `PROBE_EXIT=0` after the call, which masks failure), **and** exactly one
  stdout receipt `Shadow runner check succeeded: executed=<40-hex> reconciles=<40-hex>`
  whose two full SHAs are equal. Exit 0 with no receipt means the probe hit lock
  contention and did nothing — that is not verified provenance; rerun it when the lock is
  free. `executed` is the commit whose code actually ran; `reconciles` is the commit that
  run would have reconciled.
  **Runner identity vs. probe SHAs:** the installed-runner identity is the reviewed and
  landed private-main SHA the runner clone is pinned to, and it is *not* what the probe
  prints. The probe's two equal SHAs name the private `origin/main` commit that run
  selected, and they are legitimately newer than the pinned bootstrap SHA whenever private
  `main` has advanced — ordinary reconciler changes are picked up automatically by design.
  Do not treat a newer probe SHA as drift.
  **One-time installation of a reviewed, landed bootstrap SHA** (`<install-sha>` must
  already be on private `main`). Hold an installation lock over the whole re-pin so no
  scheduled run observes a half-installed runner, and release it before probing so the
  probe acquires the normal reconciliation lock itself:
  1. Refuse to proceed unless the runner clone is detached (`git -C <runner> rev-parse
     --abbrev-ref HEAD` prints `HEAD`), clean (`git -C <runner> status --porcelain=v1
     --untracked-files=all` prints nothing), and on the expected origin (`git -C <runner>
     config --get remote.origin.url` is `git@github.com:gridl-dev/flapjack_dev.git`).
  2. `git -C <runner> fetch --quiet origin main` first, then verify `git -C <runner>
     rev-parse --verify refs/remotes/origin/main^{commit}` equals `<install-sha>` exactly.
     If it does not, stop — re-pinning to anything else installs unreviewed code.
  3. `git -C <runner> checkout --detach origin/main`.
  4. Release the installation lock.
  5. Run `<runner>/scripts/shadow_public_candidate.sh --check-runner` and apply the
     two-signal check above.
  `.debbie.toml` in the runner clone stays pinned-runner configuration and is read from
  the pin, not from selected code. Ordinary reconciler fixes need no reinstall; only a
  landed topology change or a change to the bootstrap protocol itself requires repeating
  the sequence above. Installation and verification stop there: do not kickstart or
  otherwise poke launchd, do not run the reconciler without `--check-runner` to "test" it,
  do not change credentials or policy, and do not alter the 09:00 (Hour 9, Minute 0)
  schedule.
  The publisher never releases. The public repo's dispatch-only `release.yml` is
  the **authoritative CI for release closeout** — git tags, GitHub Releases, and
  GHCR images exist only there, never on the dev repo (`git tag -l` on dev is
  empty even when a release shipped). If GHCR package metadata is
  unlinked (`repository:null`), the repo `GITHUB_TOKEN` cannot publish that
  package, but recovery remains agent-doable with an org-admin PAT carrying
  `write:packages` plus a re-dispatch of public `release.yml` from exact `main`.
- **`/version.dev_sha` is the fjcloud control-plane SHA, not the engine
  version.** `api.{staging.}flapjack.foo/version.dev_sha` reports
  `FJCLOUD_DEV_SHA`, baked at fjcloud CI build time. The flapjack engine reaches
  fjcloud's fleet baked into a Packer AMI, not via `debbie sync`, so `/version`
  never reflects an engine change — never gate "is my engine change live?" on it.
- **HA/durability is validated cross-repo, in fjcloud.** Flapjack's
  HA/restart-window durability claims are proven by fjcloud's A5 HA soak; the
  canonical, live status lives in the fjcloud repo at
  `fjcloud_dev/docs/launch_verification_matrix.md` §5 (separate repo, not synced
  here). Proof an engine change is live is the relevant fjcloud soak passing, not
  a `/version` check.

## Deployment surfaces in this repo

The repo currently maintains four deployment-oriented proof surfaces:

1. `engine/examples/systemd/`
2. `engine/examples/ha-cluster/`
3. `engine/examples/replication/`
4. `engine/examples/s3-snapshot/`

Each one should be treated as a source-backed deployment guide, not as optional
reference prose.

## Recommended launch path

For open-source launch, the clearest operator path is:

1. install the binary
2. run a single-node instance
3. verify `/health` and `/health/ready`
4. move to Linux/systemd for long-running service management

The reusable Linux/systemd templates live in:

- `engine/examples/systemd/flapjack.service`
- `engine/examples/systemd/env.example`
- `engine/examples/systemd/README.md`

Important status note: this documented Linux/systemd path was live-verified on
2026-03-26. The templates and README remain the canonical single-node
deployment surface for production-style hosts.

## Verified example topologies

### Linux/systemd templates

Purpose:

- production-style single-node service management
- dedicated service account
- env-file based configuration
- health and readiness probe guidance

Entry point:

- `engine/examples/systemd/README.md`

What is and is not proven:

- repo-local templates and docs exist
- service layout and env-file pattern are documented
- live Linux/VPS end-to-end validation completed on 2026-03-26
- this does not by itself create a long-term historical upgrade matrix

### 3-node HA cluster

Purpose:

- nginx-routed availability for a single-node outage
- replication visibility across peers
- startup catch-up before serving
- analytics fan-out/merge

Entry points:

- `engine/examples/ha-cluster/README.md`
- `engine/examples/ha-cluster/test_ha.sh`

### 2-node replication + analytics fan-out

Purpose:

- replication across public `/1/...` routes
- analytics fan-out across public `/2/...` routes

Entry points:

- `engine/examples/replication/README.md`
- `engine/examples/replication/test_replication.sh`

### S3 snapshots

Purpose:

- snapshot upload/list/restore
- scheduled backups
- empty-dir auto-restore on startup

Entry points:

- `engine/examples/s3-snapshot/README.md`
- `engine/examples/s3-snapshot/test_snapshots.sh`

## Health probes

Operators should use:

- `/health` for basic process liveness/capability visibility
- `/health/ready` for readiness checks before routing traffic

Launch-facing docs should not advertise a deployment path as complete unless
these probes are verified in that topology.

## Secrets and host-specific configuration

- Keep shared defaults in tracked templates where safe.
- Keep host-specific secrets out of git and out of prose examples.
- For systemd hosts, prefer an env file such as `/etc/flapjack/env` derived from
  `engine/examples/systemd/env.example`.

## Documentation rule

When deployment behavior changes, update the corresponding example README or
proof script first, then update [OPERATIONS.md](./OPERATIONS.md) if the operator
workflow changed, and only then update higher-level status docs. Avoid
duplicating detailed deployment instructions across multiple docs.
