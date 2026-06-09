## JavaScript SDK Rules

- Preserve Algolia and InstantSearch compatibility unless the task explicitly changes the public API.
- Keep transport, cache, host failover, and requester behavior in shared package code.
- Use type-only imports for types and avoid `any`, `@ts-ignore`, and `@ts-expect-error` in new code.
- Treat generated model, generated API client, and generated package metadata files as generator outputs when marked as generated.
- Validate focused changes with the package's existing TypeScript, lint, and test scripts.
