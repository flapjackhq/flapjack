## Scala SDK Rules

- Preserve Algolia-compatible behavior while using Flapjack package names.
- Keep core client, exception, internal requester, configuration, and extension behavior in hand-written source.
- Treat generated API/model/version files as generated when marked as generated.
- Prefer immutable `val`, case classes, options, futures, and for-comprehensions where they clarify async code.
- Validate focused changes with `sbt compile` or narrower test tasks when available.
