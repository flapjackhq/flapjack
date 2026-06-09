<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## Dart SDK Scope

Use this file for work under `sdks/dart/`.

## Dart SDK Overview

`sdks/dart/` is the Flapjack Dart and Flutter client workspace. It includes the umbrella `flapjacksearch` package plus core, search, insights, recommend, composition, and A/B testing packages.

Entry points:
- `README.md` owns package map, quick start, migration, and development commands.
- `packages/client_core/` owns shared HTTP, retry, and exception behavior.
- `packages/client_search/` owns search client behavior.
- `melos.yaml` and `pubspec.yaml` define the workspace.

## Dart SDK Rules

- Preserve Algolia-compatible API behavior while exposing Flapjack package names.
- Keep shared transport and retry behavior in `packages/client_core/`.
- Treat generated API/model files and generated package metadata as generator outputs when marked as generated.
- Run package dependency setup with `dart pub get` in the relevant package or use the Melos workspace when needed.
- Validate focused changes with `dart analyze` and package tests where available.
