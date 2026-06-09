<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## Swift SDK Scope

Use this file for work under `sdks/swift/`.

## Swift SDK Overview

`sdks/swift/` is the Flapjack Swift client for Apple platforms and Linux. It is a drop-in replacement for the Algolia Swift package with custom-host support.

Entry points:
- `README.md` owns installation, quick start, migration, supported platforms, and build commands.
- `Sources/` contains client source.
- `Tests/` contains Swift package tests.
- `Package.swift`, `FlapjackSearchClient.podspec`, and `Cartfile` own package manager surfaces.

## Swift SDK Rules

- Preserve Algolia-compatible behavior while using Flapjack package and error names.
- Keep shared transport, configuration, retry, and search extras in hand-written source.
- Treat generated client/model/version files as generated when marked as generated.
- Prefer Swift value types, `guard`, `let`, `Codable`, and `async`/`await` where appropriate.
- Validate focused changes with `swift build` or `swift test` when the Swift toolchain is available.
