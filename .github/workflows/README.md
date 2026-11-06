# Flapjack CI/CD Workflows

This directory contains GitHub Actions workflows that are synced to the public `flapjackhq/flapjack` repository.

## How It Works

1. **Development (`flapjackhq/flapjack_dev`)**: Actions checks PRs and `main`.
   **Private candidate gate** is the single required check and runs again for
   native merge-queue groups (`strict=false`). Broad Rust tests run on PRs and
   `main`, outside the queue. The canonical local runner remains:
   ```bash
   ./engine/s/test --ci
   ```

2. **Public repo (`flapjackhq/flapjack`)**: CI runs automatically
   - On every push to `main`
   - On every trusted `public-candidate/**` branch produced by
     `scripts/publish_public_candidate.sh`
   - Nightly at 2 AM UTC on public `main` (comprehensive test suite)

## Workflows

### ci.yml - Continuous Integration

Runs the same complete job graph on public `main` and trusted
`public-candidate/**` branches. The stable **Public candidate gate** fails unless
every expected job succeeds, so branch protection needs only one durable check
name.

**Tests included:**
- Rust engine (rustfmt, clippy, fast tests)
- Rust engine (all tests)
- Dashboard (unit tests, build, page tests)
- Dashboard full and integration tests (requires Algolia secrets)
- Official Algolia client and InstantSearch contract tests

**Repository Check:**
`ci.yml` keeps secret-bearing jobs on trusted public pushes. Private PR jobs
receive only a read-only token. Foreign repositories run neither gate.

### private-ci.yml / rust-tests.yml / ci-red.yml

The private gate runs contract checks, Rustfmt and the bounded core suite.
Its 40-minute job separates the 30-minute cold-compilation cap from the existing
300-second core execution cap; each fast job retains its own dependency cache.
Known docs/SDK-only diffs skip Rust tests. New mirror candidates compare against
their main merge base and ignore validated sync provenance; unknown and
provenance-only diffs run Rust tests. Private maintainers can also dispatch
`ci.yml` with an exact `source-ref` SHA;
the workflow checks out that revision, runs the full sharded suite and reports
**Sharded Rust tests** on that commit. The complete Rust
suite builds nextest archives once per feature lane, then runs four hash shards
with `--no-fail-fast`. Nightly uses the same archive runner for the default
workspace. Integration-test server binaries travel inside the archive.
Archive construction has a 60-minute limit within a 75-minute job so runner setup
and artifact upload do not consume the compile budget.

An hourly private job and private-CI completion events record current red CI
in private Beads. It deduplicates open defects by repository, workflow and
branch, reads trusted `main` code, and never executes PR artifacts. Public
nightly and union failures are included. No private ledger is synced publicly.

### nightly.yml - Comprehensive Nightly Tests

Runs every night at 2 AM UTC on the public repo only.

**Additional coverage:**
- All Rust tests (not just fast subset)
- Dashboard integration tests
- Cross-platform installer tests

## Sync Process

From a clean, published private `main`, render an exact public candidate against
a clean clone of `flapjackhq/flapjack`:

```bash
scripts/publish_public_candidate.sh /path/to/clean/flapjackhq/flapjack
```

The publisher opens or reuses a candidate PR. It never merges, tags, releases,
or deploys. Merge the reviewed public PR only after **Public candidate gate** is
green.

## Required GitHub Secrets

Set these in the public repo settings (`flapjackhq/flapjack`):

- `ALGOLIA_APP_ID` - For integration tests
- `ALGOLIA_ADMIN_KEY` - For integration tests

## Local Development

To run the full test suite locally in the private dev repo:

```bash
# Run the CI-aligned suite (unit + integ + server + dashboard)
./engine/s/test --ci

# Run the broad local suite (everything except Algolia-gated lane)
./engine/s/test --all

# With Algolia credentials for integration tests
export ALGOLIA_APP_ID="your-app-id"
export ALGOLIA_ADMIN_KEY="your-admin-key"
./engine/s/test --all --sdk-algolia
```

## Workflow Design

The workflows use a tiered approach:

- **Complete push CI**: The same required jobs on trusted candidates and `main`
- **Stable aggregate**: One fail-closed branch-protection result
- **Nightly tests**: Broader Rust, dashboard, installer, and migration coverage on public `main`

The public **Public candidate gate** remains the release check for the mirror;
private PR checks and merging belong to the private development repository.
