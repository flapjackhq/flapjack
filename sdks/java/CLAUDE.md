<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## Java SDK Scope

Use this file for work under `sdks/java/`.

## Java SDK Overview

`sdks/java/` is the Flapjack Java client. It is a drop-in replacement for the Algolia Java client and supports custom-host configuration for self-hosted Flapjack.

Entry points:
- `README.md` owns installation, quick start, migration, and test instructions.
- `flapjacksearch/` contains the client package.
- `api/` contains generated or API-facing surfaces.
- `tests/` contains Java SDK tests.
- `build.gradle`, `settings.gradle`, and `gradlew` own the Gradle build.

## Java SDK Rules

- Preserve Algolia-compatible wire behavior while using Flapjack package coordinates and imports.
- Prefer hand-written configuration, exception, internal requester, utility, and core client code for behavioral fixes.
- Treat generated API/model files as generated when they carry generated markers.
- Use the checked-in Gradle wrapper for builds and tests.
- E2E tests require a Flapjack server on `localhost:7700`.
