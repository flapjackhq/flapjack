## Kotlin SDK Rules

- Preserve Algolia-compatible behavior while using Flapjack package coordinates and namespaces.
- Keep shared behavior in common multiplatform code unless a platform-specific `expect` or `actual` seam is required.
- Prefer immutable `val`, data classes, null-safe operators, and coroutine-aware APIs.
- Treat generated API/model/build config files as generated when marked as generated.
- Validate focused JVM changes with `./gradlew :client:jvmMainClasses` or narrower Gradle tasks when available.
