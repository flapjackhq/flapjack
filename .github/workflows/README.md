# Flapjack CI/CD Workflows

This directory contains GitHub Actions workflows that are synced to the public `flapjackhq/flapjack` repository.

## How It Works

1. **Development (private dev repo)**: Tests are run manually via the canonical runner
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
All jobs check `github.repository == flapjackhq/flapjack` to ensure they only run in the public repo.

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

The private development repository keeps Actions disabled; local development
uses the canonical runner shown above.
