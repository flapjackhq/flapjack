<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## Scala SDK Scope

Use this file for work under `sdks/scala/`.

## Scala SDK Overview

`sdks/scala/` is the Flapjack Scala client for Scala 2.13 and 3.x. It is a drop-in replacement for the Algolia Scala client with custom-host support.

Entry points:
- `README.md` owns installation, quick start, migration, and build commands.
- `src/` contains Scala client source.
- `build.sbt`, `version.sbt`, and `project/` own the SBT build.

## Scala SDK Rules

- Preserve Algolia-compatible behavior while using Flapjack package names.
- Keep core client, exception, internal requester, configuration, and extension behavior in hand-written source.
- Treat generated API/model/version files as generated when marked as generated.
- Prefer immutable `val`, case classes, options, futures, and for-comprehensions where they clarify async code.
- Validate focused changes with `sbt compile` or narrower test tasks when available.
