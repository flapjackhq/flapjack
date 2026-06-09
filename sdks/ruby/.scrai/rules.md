## Ruby SDK Rules

- Preserve Algolia-compatible wire behavior while exposing Flapjack module and gem names.
- Keep transport, retry, configuration, logging, and core client behavior in hand-written library code.
- Treat generated API/model/version files as generated when marked as generated.
- Prefer Ruby idioms: `snake_case`, safe navigation, blocks for callbacks, and explicit error handling.
- Validate focused changes with `bundle exec rake test` or narrower test selections when available.
