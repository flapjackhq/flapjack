<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## Ruby SDK Scope

Use this file for work under `sdks/ruby/`.

## Ruby SDK Overview

`sdks/ruby/` is the Flapjack Ruby client. It is a drop-in replacement for the `algolia` gem with self-hosted Flapjack host support, typed model objects, retry behavior, synonyms, query rules, faceting, and browse pagination.

Entry points:
- `README.md` owns installation, quick start, migration, requirements, and feature summary.
- `MIGRATION.md` owns detailed migration guidance.
- `lib/` contains client source.
- `tests/` contains SDK tests.
- `Gemfile`, `Rakefile`, and `flapjack-search.gemspec` own the Ruby build surface.

## Ruby SDK Rules

- Preserve Algolia-compatible wire behavior while exposing Flapjack module and gem names.
- Keep transport, retry, configuration, logging, and core client behavior in hand-written library code.
- Treat generated API/model/version files as generated when marked as generated.
- Prefer Ruby idioms: `snake_case`, safe navigation, blocks for callbacks, and explicit error handling.
- Validate focused changes with `bundle exec rake test` or narrower test selections when available.
