# Dashboard Login

## Task

Authenticate once with the Flapjack Admin API Key, then use and reload the dashboard without exposing the credential to page scripts.

## Layout

1. A centered card shows the Flapjack mark, `Welcome to Flapjack`, and an instruction to enter the Admin API Key.
2. A password input labelled `Admin API Key` accepts the key; the full-width `Connect` button submits it.
3. Help text explains where to find the key and shows the `reset-admin-key` command.
4. After authentication, the dashboard header's `Connection Settings` button opens the connection dialog.
5. The dialog contains a password input, unauthenticated-server help text, an `Application ID` input, `Cancel`, and `Save & Reconnect`.

## State contract

### Loading/session check

- A centered spinner is the only visible control while `GET /1/indexes` checks the HttpOnly session; login inputs, help, dashboard, and connection dialog controls are hidden.

### Logged-out idle

- The welcome card, enabled Admin API Key input, help text, and `Connect` button are visible; `Connect` is disabled until trimmed input is non-empty.
- Dashboard and connection-dialog controls (`Application ID`, `Cancel`, `Save & Reconnect`) are hidden.

### Logging in

- The Admin API Key input and help remain visible; the disabled button reads `Validating...` with a spinner. Dialog controls remain hidden.

### Invalid key

- The input receives error styling; `Invalid API key. Check your terminal for the correct key.` appears; `Connect` and help remain visible. Editing clears the error.

### Server unreachable

- The login controls and help remain visible with `Could not connect to server.` after a submit fails; editing permits another attempt. Connection-dialog controls remain hidden.

### Authenticated

- Successful login briefly shows `Authenticated! Loading dashboard...` and a disabled `Connected` button, then the dashboard replaces the login card.
- The header's `Connection Settings` button is visible. When opened, its dialog shows the Admin API Key input, empty-key help, Application ID input, `Cancel`, and `Save & Reconnect`; `Cancel` closes without changes, while save revokes the old session, optionally creates a new one, persists only Application ID, and reloads.

### Session expired

- On the next page load/AuthGate mount, a protected-route `403` replaces the loading spinner with logged-out idle controls; no key or dialog control remains from the authenticated view.

### Revoked session

- Saving connection settings with an empty key revokes server state, closes the dialog, reloads, and shows logged-out idle controls; replaying the captured cookie is refused with `403`.
- External revocation follows the session-expired behavior on the next page load/AuthGate mount.

## Navigation

- Route: all dashboard routes are gated; login has no standalone URL.
- Entry: opening or reloading any dashboard route without a valid session.
- Success: stays on the requested dashboard route after the short confirmation state.
- Connection settings: opens as a modal over the authenticated route; cancel returns to it, save/reconnect reloads it.

## Workflow contract

- Current flow before this change: the Admin API Key was persisted in `localStorage` and replayed after reload.
- Proposed and chosen shipped flow: exchange the key once for an opaque server-owned HttpOnly, SameSite=Strict cookie; send it automatically on same-origin requests, persist no key material, preserve reload access, and revoke server-side on logout/reconnect.
- Alternatives rejected: moving `localStorage` to `sessionStorage` remains script-readable and therefore loses script-inaccessibility; in-memory-only storage loses reload persistence. Neither preserves both required properties.
- Test plan: browser acceptance asserts no key in localStorage, no session token in localStorage or `document.cookie`, `HttpOnly`, reload continuity, UI logout, and `403` on replay; the full UI suite guards broader dashboard parity.
- Unresolved risk: expiration or external revocation is surfaced at the next AuthGate mount/reload, not by a global mid-page `403` transition.

## Acceptance criteria

- Given no session, when a valid key is submitted, then the dashboard opens and key material is absent from all localStorage values.
- Given the issued session, then its cookie is HttpOnly and neither its name nor token is visible through `document.cookie`.
- Given an authenticated reload, then the dashboard opens without another key prompt.
- Given `Save & Reconnect` with an empty key, then login returns and replay of the revoked cookie receives `403`.
- Given an invalid key or unreachable server, then the matching error is visible and editing permits retry.

## Edge cases

- Whitespace-only keys cannot submit; keys are trimmed before exchange.
- A blank Application ID falls back to `flapjack`; leaving the dialog key blank intentionally logs out before reload.
- Open-mode servers pass the initial protected-route check and show the dashboard without a login prompt.
