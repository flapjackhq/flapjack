# Receipt — Durable dashboard session-store foundation (Stage 2, Lane L8A)

Date: 2026-08-03
Owner file: `engine/flapjack-http/src/auth/session.rs`
Contract: `engine/flapjack-http/src/auth_tests/session_store_tests.rs` (17 tests)

## Red baseline (Stage 1 — inert fail-closed scaffold)

Command:

```bash
cd engine && timeout 900 cargo test -p flapjack-http --lib --no-fail-fast -- session_store
```

Result: 17 discovered / 14 named behavioral assertion failures / 3 fail-closed passes,
process exit 101, no compile error. The scaffold minted well-formed token material but
persisted nothing and authenticated nobody, so every durability/validation assertion
failed on its own `assert!`, not on a missing fixture or type error.

## Green result (Stage 2 — real `DashboardSessionStore`)

Command (identical selection):

```bash
cd engine && timeout 900 cargo test -p flapjack-http --lib --no-fail-fast -- session_store
```

Measured denominator:

```
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 2250 filtered out
```

Supporting gates (all exit 0): `cargo fmt --check`, `cargo check -p flapjack-http`,
`cargo clippy -p flapjack-http --lib` (no findings on `auth/session.rs`).

## At-rest format and privacy result

Persisted file `dashboard_sessions.json` serializes exactly:

```json
{"version":1,"key_fingerprint":"<hex>","verifiers":[{"salt":"<64-hex>","digest":"<hex>"}]}
```

- `key_fingerprint = hex(HMAC-SHA256(admin_key, "flapjack.dashboard_session_state.v1"))`.
- Per verifier, `digest = hex(HMAC-SHA256(admin_key, b"flapjack.dashboard_session_verifier.v1\0" ++ salt_hex_ascii ++ token_utf8))`.
  The salt is fed as its 64-char lowercase-hex ASCII string; each verifier uses a fresh
  32-byte random salt.
- No extra fields (exact key-set equality is pinned by
  `persisted_state_contains_only_keyed_session_verifiers`).

Privacy: the plaintext session token and the admin key each occur **zero** times in the
persisted bytes (`persisted_state_leaks_neither_token_nor_admin_key`); only the keyed
fingerprint and salted verifier digests are written. On unix the state file is `0o600`
(`persisted_state_file_is_private`). A state file opened under a different admin key
loads with zero active verifiers — fail-closed, not an error
(`persisted_sessions_are_bound_to_the_admin_key`).

Durability: mint/revoke persist-before-mutate through one `Mutex`. The private write path
is `create_new` temp in the state directory → `write_all` → `sync_all` → `rename` over the
live file → parent-directory `sync_all`, with `0o600` set on the temp before rename and the
temp removed on any failure. A failed pre-commit durable write returns `StateIo` and leaves
memory **and** disk unchanged with no token issued (`failed_mint_...`, `failed_revoke_...`,
`unusable_state_path_rejects_mint_without_issuing_token`). A post-rename parent-directory
sync failure returns `StateIo` and closes the in-memory store to authentication instead of
acknowledging a mint or revoke whose rename was not durably committed
(`failed_parent_directory_sync_does_not_acknowledge_mint_or_revoke`). Load semantics:
missing file → empty `Ok`; other read IO error → `StateIo`; unparseable/truncated/garbage
JSON → `MalformedState`; foreign `key_fingerprint` → empty `Ok`; same-key persisted
verifier records with malformed salt/digest encoding, wrong lengths, or duplicate records
→ `MalformedState`.

## Dependency count

**0 new crates.** `hmac` 0.12, `sha2` 0.10, `rand` 0.8, `hex` 0.4, `subtle` 2, `serde`,
and `serde_json` are all pre-existing `[dependencies]` of `flapjack-http`. `Cargo.toml`
and `Cargo.lock` are untouched. HMAC is computed with the `hmac`/`sha2` crates (the exact
`Hmac::<Sha256>` the test helper uses); no hand-rolled HMAC.

## Handoff to L8

Reuse the single `DashboardSessionStore` owner in `engine/flapjack-http/src/auth/session.rs`
for all dashboard route and middleware session logic — mint, validate, revoke, restart load,
and durable persistence. Do **not** add a second session store, parallel verifier logic, or a
duplicate persistence path. The module is exposed as `pub(crate) mod session;` in `auth/mod.rs`;
L8 may add `pub(crate) use session::{DashboardSessionStore, SessionStoreError};` when it has a
real non-test consumer, and remove the module-level `#![allow(dead_code)]` at that point
(it is retained here only because nothing outside `#[cfg(test)]` calls the store yet).

## ROADMAP CORRECTION REQUIRED (for L11 — prose only, no ledger edit here)

The durable dashboard session-store owner now exists and is contract-green. L11 (sole writer
of `ROADMAP.md`, `PROJECT_OVERVIEW.md`, `engine/docs2/FEATURES.md`, `CHANGELOG.md`) should
record that the mint/validate/revoke/restart-persist session owner (`auth/session.rs`) is
implemented and that L8 dashboard route/middleware integration is unblocked at the owner level
(still gated on FJ-4's `router.rs` ownership for actual route mounting). This stage mounts no
routes and edits no ledger file; the ledger update is L11's to make from merged `main`.

## Post-review remediation (durable-write path)

Code review filed four low, non-blocking findings against `auth/session.rs`. The earlier
post-review pass fixed the temp-file issues and added first-pass directory-sync coverage;
the later blocking remediation below supersedes the directory-sync behavior.

| Finding | Fix | Guarding test |
| --- | --- | --- |
| `temp-name-fixed-create-new-spurious-failure` | `next_write_temp_path()` mints a per-write temp name (`dashboard_sessions.json.<pid>.<sequence>.tmp`) beside the live file, so exclusive creation still guards a concurrent writer but a crashed write strands only its own name; the error-path `remove_file` can therefore only ever remove this call's temp. Create/write/sync failures now stamp the temp path via the shared `io_failure()` owner instead of the live state path. | `auth::session::session_store_tests::{stranded_write_temp_file_does_not_block_the_next_mint, successful_durable_writes_leave_no_temp_artifacts, a_failed_durable_write_names_the_temp_file_it_could_not_create, each_durable_write_claims_a_distinct_temp_path}` |
| `temp-file-umask-window-before-chmod` | `create_owner_private_file()` sets `OpenOptionsExt::mode(0o600)` at creation under `cfg(unix)`; the post-creation chmod helper is gone, so the file never exists at umask-default permissions. | `auth::session::session_store_tests::write_temp_file_is_owner_private_from_the_moment_it_is_created` (asserts `0o600` on the freshly created handle), plus the unchanged `persisted_state_file_is_private` |
| `dir-fsync-failure-after-rename-diverges` | Superseded by the later blocking review remediation below. | `failed_parent_directory_sync_does_not_acknowledge_mint_or_revoke` |

`salt-generator-duplicates-key-store-helper` remains open: the fix is to promote
`key_store::generate_salt` to `pub(crate)` and call it from `session.rs`, and `key_store.rs`
is outside this stage's blast radius. L8 owns it.

Re-verified after the earlier remediation: `cd engine && timeout 900 cargo test -p
flapjack-http --lib --no-fail-fast -- session_store` → **22 passed; 0 failed**.
`cargo fmt --check` exit 0. `cargo clippy -p flapjack-http --lib --tests` → zero findings
in either touched file. Still **0 new crates**.

## Post-review remediation (blocking findings)

Code review later filed two blocking medium findings. Both were fixed red-first in
session `s26_review_remediation`:

| Finding | Fix | Guarding test |
| --- | --- | --- |
| `dir-fsync-failure-after-rename-diverges` | `write_temp_then_rename()` now classifies a parent-directory `sync_all` error after a successful rename as `CommitIndeterminate`, returns `StateIo`, and `mint_session`/`revoke_session` clear the in-memory verifier set instead of acknowledging success. Pre-commit write failures still preserve the existing in-memory set. | `failed_parent_directory_sync_does_not_acknowledge_mint_or_revoke` |
| `invalid-persisted-verifiers-counted-active` | `DashboardSessionStore::open` validates same-key persisted verifier records before loading them: salt and digest must each be lowercase 64-character hex strings, and duplicate verifier records are rejected as `MalformedState`. Foreign-key state is still discarded fail-closed before verifier validation. | `semantically_invalid_persisted_verifier_records_are_rejected` |

Red proof: `cd engine && timeout 900 cargo test -p flapjack-http --lib --no-fail-fast --
session_store` failed with **20 passed; 2 failed** on
`failed_parent_directory_sync_does_not_acknowledge_mint_or_revoke` and
`semantically_invalid_persisted_verifier_records_are_rejected`.

Green proof after fixes:

```bash
cd engine && timeout 900 cargo test -p flapjack-http --lib --no-fail-fast -- session_store
```

Result: **22 passed; 0 failed; 0 ignored; 0 measured; 2252 filtered out**.

Additional verification recorded through the validation cache:

- `cd engine && timeout 900 cargo test -p flapjack-http --lib --no-fail-fast -- auth::session::`
  → **2 passed; 0 failed; 2272 filtered out**.
- `cd engine && cargo fmt --check` → exit 0.
- `cd engine && cargo check -p flapjack-http` → exit 0.
- `cd engine && cargo clippy -p flapjack-http --lib` → exit 0.
- `cd engine && cargo clippy -p flapjack-http --lib --tests` → exit 0, with only
  pre-existing warnings outside the touched session-store files.

## Posthoc review — test responsibility split

Final review moved four implementation-specific durability tests (parent-directory fsync
fault injection and temp-file naming/cleanup/error attribution) from the external behavioral
contract into `auth::session::session_store_tests`. No assertion or scenario was removed;
the split keeps both changed Rust files below the 800-line hard limit and makes the existing
module-local test owner match its documented responsibility. The `session_store` filter still
selects the complete suite because both test modules use that name. Review also added the
missing known-answer guard for unsupported persisted-state versions; version 2 must return
`MalformedState` with the rejected version in its detail. Current focused result:
**25 passed; 0 failed; 2250 filtered out**. The module-local selector passed **7/7**;
`cargo check -p flapjack-http`, `cargo clippy -p flapjack-http --lib --tests`,
`cargo fmt --check`, and `git diff --check` all exited 0. The complete crate regression
sweep passed **2273 tests with 0 failures and 2 ignored**, followed by green integration
and doc-test binaries.
