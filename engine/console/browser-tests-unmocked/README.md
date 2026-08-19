# Unmocked browser tests

This suite proves the standalone Svelte host through a real local Flapjack engine. It runs the
same Index List and Basic Search flow at desktop and exactly 390px, authenticates through the
real HttpOnly dashboard session, and uses API requests only in fixtures for deterministic setup.
The suite refuses non-loopback backends before any fixture mutation and the canonical runner starts
a fresh loopback engine with its own temporary data directory. Direct npm invocation also refuses
unless the owning runner or CI job supplies a per-run test-instance token.
Deterministic Index List/Search loading, empty, transport-error, retry, and duplicate-request
branches remain owned by `component-tests/IndexListSearch.test.ts`; this host proof does not clone
that shared matrix.

Run it through the canonical engine owner: `./s/test --console`.
