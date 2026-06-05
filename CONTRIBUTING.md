# Contributing

Thanks for your interest in improving Flapjack.

## Bug Reports

Use GitHub Issues and select the bug report template for confirmed defects.
Usage questions, setup help, integration troubleshooting, and other unresolved
support requests belong in [GitHub Discussions Q&A](https://github.com/flapjackhq/flapjack/discussions/categories/q-a)
or the support path described in [.github/SUPPORT.md](.github/SUPPORT.md).

Include:

- What you expected to happen.
- What actually happened.
- Reproduction steps.
- Version/commit, environment, and logs.

## Feature Requests

For exploratory product ideas, start in [GitHub Discussions Ideas](https://github.com/flapjackhq/flapjack/discussions/categories/ideas).
Open a GitHub Issue for concrete behavior or API changes, describing:

- The problem you are trying to solve.
- Why existing behavior is insufficient.
- Proposed API or UX shape when relevant.

## Pull Request Workflow

1. Fork the repository.
2. Create a focused branch for one logical change.
3. Add or update tests for behavior changes.
4. Run the required checks locally.
5. Open a PR with a clear description, rationale, and validation notes.

## Build Prerequisites

- Current stable Rust toolchain installed (`cargo`, `rustfmt`, `clippy`).
- Run contributor checks from `engine/`, where the Rust workspace and maintained test runner live.

## Code Style

- Follow existing Rust conventions and module patterns in the codebase.
- Keep changes focused and avoid unrelated refactors.
- Run and pass formatting and linting from `engine/` before opening a PR:
  - `cargo fmt --check`
  - `cargo clippy -p flapjack --all-targets -- -D warnings`
  - `cargo clippy -p flapjack-http --all-targets -- -D warnings`

## Testing Expectations

`engine/s/test` is the maintained project test entrypoint.

Use focused checks first from `engine/`, then broaden as needed:

- `./s/test --unit`
- `./s/test --integ`
- `./s/test --server`
- `./s/test`

## Architecture and Testing References

- [Workspace guide](engine/README.md)
- [Architecture](engine/docs2/3_IMPLEMENTATION/ARCHITECTURE.md)
- [Testing](engine/docs2/1_STRATEGY/TESTING.md)
