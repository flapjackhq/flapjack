## Swift SDK Rules

- Preserve Algolia-compatible behavior while using Flapjack package and error names.
- Keep shared transport, configuration, retry, and search extras in hand-written source.
- Treat generated client/model/version files as generated when marked as generated.
- Prefer Swift value types, `guard`, `let`, `Codable`, and `async`/`await` where appropriate.
- Validate focused changes with `swift build` or `swift test` when the Swift toolchain is available.
