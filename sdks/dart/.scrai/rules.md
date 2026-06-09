## Dart SDK Rules

- Preserve Algolia-compatible API behavior while exposing Flapjack package names.
- Keep shared transport and retry behavior in `packages/client_core/`.
- Treat generated API/model files and generated package metadata as generator outputs when marked as generated.
- Run package dependency setup with `dart pub get` in the relevant package or use the Melos workspace when needed.
- Validate focused changes with `dart analyze` and package tests where available.
