## Java SDK Rules

- Preserve Algolia-compatible wire behavior while using Flapjack package coordinates and imports.
- Prefer hand-written configuration, exception, internal requester, utility, and core client code for behavioral fixes.
- Treat generated API/model files as generated when they carry generated markers.
- Use the checked-in Gradle wrapper for builds and tests.
- E2E tests require a Flapjack server on `localhost:7700`.
