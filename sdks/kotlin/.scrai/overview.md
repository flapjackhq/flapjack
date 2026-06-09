## Kotlin SDK Overview

`sdks/kotlin/` is the Flapjack Kotlin Multiplatform client for JVM, iOS, and macOS targets. It is a drop-in replacement for the Algolia Kotlin client with Flapjack host configuration.

Entry points:
- `README.md` owns installation, quick start, migration, and build commands.
- `client/` contains shared and platform-specific client source.
- `client-bom/` owns dependency BOM packaging.
- `build.gradle.kts`, `settings.gradle.kts`, and `gradlew` own the Gradle build.
