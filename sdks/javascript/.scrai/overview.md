## JavaScript SDK Overview

`sdks/javascript/` is the TypeScript JavaScript client for browser and Node.js use. It provides `flapjack-search`, self-hosted host configuration, and InstantSearch-compatible usage.

Entry points:
- `README.md` owns package usage, migration, and troubleshooting.
- `MIGRATION.md` owns migration detail.
- `packages/` contains workspace packages.
- `packages/client-common/` owns shared transport, cache, and requester abstractions.
- `scripts/`, `package.json`, `lerna.json`, and `nx.json` own build orchestration.
