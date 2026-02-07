# Contributing

## Before Contributing

Open an issue first to discuss proposed changes.

## Setup

```bash
git clone https://github.com/stuartcrobinson/flapjack.git
cd flapjack
cargo build
```

## Testing

```bash
cargo install cargo-nextest
cargo nextest run -P ci
```

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and fix warnings
- Use `tracing::info!` or `tracing::warn!` for logging (not `eprintln!`)

## Submitting Changes

1. Create a branch: `git checkout -b feature/name`
2. Make changes and add tests
3. Run `cargo fmt && cargo clippy`
4. Run `cargo nextest run -P ci`
5. Commit with descriptive message
6. Push and open pull request

## Pull Request Requirements

- All tests must pass
- Include description of changes
- Address review feedback

## Questions

Open an issue.
