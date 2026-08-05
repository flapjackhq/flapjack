# 2026-08-03 Dashboard Session Auth Receipt

PURPOSE: Close the L8 dashboard session-auth lane with its user-facing contract,
fail-capability proof, current browser acceptance result, and ledger correction proposal.

## Contract And Owners

`docs/screen_specs/login.md` is the single login workflow owner. It specifies the
loading, logged-out, logging-in, error, authenticated, expired, and revoked states,
and rejects `sessionStorage` and in-memory-only storage because neither preserves
both script-inaccessibility and reload persistence.

The shipped server seam remains owned by:

- `engine/flapjack-http/src/router.rs`: mounts `POST` and `DELETE /1/dashboard/session`.
- `engine/flapjack-http/src/auth/session_cookie.rs`: encodes and reads the cookie.
- `engine/flapjack-http/src/handlers/dashboard_session.rs`: exchanges the admin key
  for a session and revokes it on logout.
- `engine/flapjack-http/src/auth/middleware.rs`: rate-limits exchange attempts and
  validates cookie sessions on protected routes.

No replacement capability matrix was created.

## Reused HttpOnly Fail-Capability Evidence

Stage 2 deliberately removed `HttpOnly`, then ran the engine transport contract:

```text
timeout 1800 cargo test -p flapjack-http --lib --no-fail-fast -- auth::tests::session_transport_tests
RED_ENGINE_EXIT=101; 4 passed / 4 failed
assertion failed: has_cookie_attribute(&set_cookie, "HttpOnly")
```

Against that deliberately broken server,
`npx playwright test --project=e2e-ui tests/e2e-ui/smoke/session_auth.spec.ts`
failed at `session_auth.spec.ts:44` on
`expect(sessionCookie?.httpOnly).toBe(true)`: `RED_BROWSER_EXIT=1`, one failed and
two passed. After restoring `HttpOnly`, the engine contract passed 8/8, the live
header contained `HttpOnly; SameSite=Strict; Path=/`, and the browser contract
passed 3/3. This is the existing Stage 2 fail capability evidence; Stage 3 did
not repeat a production-code mutation. The assertions directly enforce the
cookie behavior promised by `docs/screen_specs/login.md`.

## Stage 3 Full Browser Acceptance

The evidence producer was a fresh auth-enabled loopback server at
`http://127.0.0.1:17700` and dashboard at `http://127.0.0.1:15177`. The shared
7700 backend was excluded because it is an unowned `--no-auth` orphan and is not
a HEAD specimen.

Required command, output `/tmp/l8_full.txt`:

```text
npm run test:e2e-ui:full > /tmp/l8_full.txt 2>&1; echo "FULL_EXIT=$?"; grep -E '[0-9]+ (passed|failed|skipped)' /tmp/l8_full.txt | tail -5
```

| Run | HEAD | Passed | Failed | Skipped | Did not run | Exit |
| --- | --- | ---: | ---: | ---: | ---: | ---: |
| Before metrics fix | `ddab0d7e` | 162 | 146 | 14 | 46 | 1 |
| After metrics fix, committed-tree rerun | `b440f3028ed48409d41444f065d6a935429d9a44` | 330 | 4 | 20 | 14 | 1 |

The first run exposed a Stage 2 session-auth regression: direct cross-origin
`/metrics` requests could not use the HttpOnly session under browser CORS rules.
Tests failed red before the repair. Commit `b440f3028` makes development metrics
same-origin through the Vite alias; focused Vitest passed 10/10, TypeScript and
ESLint passed, and every metrics browser case passed in the full rerun.

The four remaining failures are not hidden or converted to a lower baseline:

- Two `migrate-algolia.spec.ts` cases fail in `algolia.fixture.ts:89` with
  `Invalid Application-ID or API key`. The identical focused selection on an
  unmodified `origin/main` worktree reproduced both failures (`26 passed / 2
  failed`), establishing a pre-existing external-credential condition.
- Two `analytics-deep.spec.ts` cases fail while concurrently seeding the same
  index with HTTP 500 at `analytics-seed.ts:87`. Both passed in the same focused
  `origin/main` reproduction, so this is recorded as a full-suite-only unrelated
  regression, not misclassified as pre-existing and not expanded into product
  work in this session-auth verification stage.

The L6 receipt's semantic full-UI baseline is `347 passed / 1 failed / 20
skipped`. The required literal count guard returned `FULL_COUNT_EXIT=0`; its
prescribed `tail -1` parser compares current `330` with the receipt's later
focused-proof text `5 passed`, not the semantic 347 baseline. This parser flaw
does not override the honest full-suite red result above.

Supervisor correction on 2026-08-03 anchored the parser to the L6 receipt's
`Full UI` table row. The fresh committed-tree output then produced
`FULL_PASSED=346 BASELINE_PASSED=347` and `CORRECTED_FULL_COUNT_EXIT=1`.
The guard therefore fails for the recorded cross-lane browser condition instead
of returning a false green; no session-auth failure remains in that run.

## Merge Readiness

The session-auth lane is merge-ready: its defect was falsified, repaired, and
retested at the full-suite surface, and all session-auth and metrics cases pass.
The repository-wide browser command remains red only for the two explicitly
classified, unrelated conditions above. No manual inspection substitutes for
the automated evidence.

## ROADMAP CORRECTION REQUIRED

Proposed `SEC-G3` closure text for `ROADMAP.md`:

```markdown
SEC-G3 dashboard admin key readable by any same-origin script — CLOSED 2026-08-03. The dashboard exchanges the admin key once for a server-owned HttpOnly, SameSite=Strict session cookie, persists no key material in browser storage, preserves authenticated reloads, and revokes sessions server-side on logout/reconnect. Engine and browser fail-capability tests go red when HttpOnly is removed and green when restored; the user contract is `docs/screen_specs/login.md` and the dated evidence is this receipt.
```

Proposed text for `docs/security/DECISIONS.md`:

```markdown
SD-009 is SUPERSEDED 2026-08-03 by the shipped HttpOnly cookie-session work. Its former browser-storage risk acceptance is no longer operative: the admin key is not persisted, the opaque session is script-inaccessible, reload persistence is retained, and logout/reconnect revokes server state. `sessionStorage` and in-memory-only storage remain rejected false closes.
```

The public ledger owners are intentionally unchanged in this verification stage.
