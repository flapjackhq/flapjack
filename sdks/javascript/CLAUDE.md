<!-- assembled by scrai — do not edit directly -->

_This file is auto-generated from `.scrai/` sources. Do not edit directly._

Root guidance: `../../AGENTS.md` and `../../CLAUDE.md`

## JavaScript SDK Scope

Use this file for work under `sdks/javascript/`.

## JavaScript SDK Overview

`sdks/javascript/` is the TypeScript JavaScript client for browser and Node.js use. It provides `flapjack-search`, self-hosted host configuration, and InstantSearch-compatible usage.

Entry points:
- `README.md` owns package usage, migration, and troubleshooting.
- `MIGRATION.md` owns migration detail.
- `packages/` contains workspace packages.
- `packages/client-common/` owns shared transport, cache, and requester abstractions.
- `scripts/`, `package.json`, `lerna.json`, and `nx.json` own build orchestration.

## JavaScript SDK Rules

- Preserve Algolia and InstantSearch compatibility unless the task explicitly changes the public API.
- Keep transport, cache, host failover, and requester behavior in shared package code.
- Use type-only imports for types and avoid `any`, `@ts-ignore`, and `@ts-expect-error` in new code.
- Treat generated model, generated API client, and generated package metadata files as generator outputs when marked as generated.
- Validate focused changes with the package's existing TypeScript, lint, and test scripts.
