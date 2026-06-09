## Ruby SDK Overview

`sdks/ruby/` is the Flapjack Ruby client. It is a drop-in replacement for the `algolia` gem with self-hosted Flapjack host support, typed model objects, retry behavior, synonyms, query rules, faceting, and browse pagination.

Entry points:
- `README.md` owns installation, quick start, migration, requirements, and feature summary.
- `MIGRATION.md` owns detailed migration guidance.
- `lib/` contains client source.
- `tests/` contains SDK tests.
- `Gemfile`, `Rakefile`, and `flapjack-search.gemspec` own the Ruby build surface.
