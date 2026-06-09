## Dart SDK Overview

`sdks/dart/` is the Flapjack Dart and Flutter client workspace. It includes the umbrella `flapjacksearch` package plus core, search, insights, recommend, composition, and A/B testing packages.

Entry points:
- `README.md` owns package map, quick start, migration, and development commands.
- `packages/client_core/` owns shared HTTP, retry, and exception behavior.
- `packages/client_search/` owns search client behavior.
- `melos.yaml` and `pubspec.yaml` define the workspace.
