<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## Kotlin SDK Scope

Use this file for work under `sdks/kotlin/`.

## Kotlin SDK Overview

`sdks/kotlin/` is the Flapjack Kotlin Multiplatform client for JVM, iOS, and macOS targets. It is a drop-in replacement for the Algolia Kotlin client with Flapjack host configuration.

Entry points:
- `README.md` owns installation, quick start, migration, and build commands.
- `client/` contains shared and platform-specific client source.
- `client-bom/` owns dependency BOM packaging.
- `build.gradle.kts`, `settings.gradle.kts`, and `gradlew` own the Gradle build.

## Kotlin SDK Rules

- Preserve Algolia-compatible behavior while using Flapjack package coordinates and namespaces.
- Keep shared behavior in common multiplatform code unless a platform-specific `expect` or `actual` seam is required.
- Prefer immutable `val`, data classes, null-safe operators, and coroutine-aware APIs.
- Treat generated API/model/build config files as generated when marked as generated.
- Validate focused JVM changes with `./gradlew :client:jvmMainClasses` or narrower Gradle tasks when available.
