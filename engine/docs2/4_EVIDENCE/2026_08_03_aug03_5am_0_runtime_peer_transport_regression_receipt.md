# Runtime peer transport regression receipt

## Baseline

- Branch: `batman/aug03_5am_0_runtime_peer_transport_regression_and_dur1_respecimen`
- Rebased onto current `origin/main`: yes (`git fetch origin main && git rebase origin/main` reported the branch was up to date)
- Recorded baseline HEAD: `928426c6e880d0cc86b0ead4d955439545fd1580`

## Deterministic reproduction

- Command: `cd engine && timeout 600 cargo test --test test_replication -- authenticated_http_add_peer_validates_authorizes_and_replicates_without_restart authenticated_http_remove_peer_stops_future_replication_without_restart --test-threads=2`
- `REPRO_A_EXIT=101`
- Summary: `FAILED. 0 passed; 2 failed; 0 ignored; 0 measured; 54 filtered out; finished in 0.82s`
- `authenticated_http_add_peer_validates_authorizes_and_replicates_without_restart`: `left: 400`, `right: 200` at `tests/test_replication.rs:1365`.
- `authenticated_http_remove_peer_stops_future_replication_without_restart`: `left: 400`, `right: 200` at the setup helper `tests/test_replication.rs:1290`.
- Durable transcript: `${MATT_HOME:-$HOME/.matt}/evidence/aug03_5am_0_peer_transport/repro_a.txt`.

## Runtime trace and Arm A disposition

- The accepted LAN peer origin in the direct run was `http://[2605:a601:a671:f500:846:dcb0:ec25:acac]:51148`.
- `engine/flapjack-http/src/handlers/internal.rs::add_cluster_peer` normalizes the URL, calls `validate_authenticated_query_peer_transport`, and returns `json_error(StatusCode::BAD_REQUEST, message)` at line 770 when validation fails.
- `engine/flapjack-http/src/analytics_cluster.rs::validate_authenticated_query_peer_transport` delegates to `NodeConfig::validate_credentialed_peer_transport` with credential context `authenticated analytics query fan-out forwards caller API keys`.
- `engine/flapjack-replication/src/config.rs::NodeConfig::validate_credentialed_peer_transport` identifies the `http://` scheme and constructs the refusal at lines 272-277 when the escape hatch is absent.
- Exact runtime response message: `Refusing replication peer node-b at http://[2605:a601:a671:f500:846:dcb0:ec25:acac]:51148: authenticated analytics query fan-out forwards caller API keys and the peer origin is cleartext http://, which would send the peer credential in plaintext. Move the peer to https://, or set FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1 to keep the cleartext peer.`
- The remove test never reached `remove_cluster_peer`: its first setup call to `add_runtime_peer` received this same `400` and failed its expected-`200` assertion.
- Temporary `eprintln!` instrumentation used to capture the direct response was removed. Durable transcript: `${MATT_HOME:-$HOME/.matt}/evidence/aug03_5am_0_peer_transport/runtime_transport_trace.txt`.
- Arm A: retain `NodeConfig::validate_credentialed_peer_transport` and its analytics wrapper as the single policy owner, because `engine/docs2/3_IMPLEMENTATION/OPS_CONFIGURATION.md` line 364 explicitly includes runtime `POST /internal/cluster/peers` in the default cleartext refusal and commit `ee785daad` introduced the caller-credential transport protection.

## TDD and validation

- Added `authenticated_http_add_peer_rejects_cleartext_caller_credentials_by_default` beside the runtime endpoint tests. It removes the escape hatch with RAII, uses the harness's validator-accepted LAN origin, asserts the exact `400 BAD_REQUEST` message, and asserts `peer_count == 0`.
- The refusal test and both authenticated success tests use the same `#[serial_test::serial(runtime_peer_transport_env)]` group, preventing process-global environment races under `--test-threads=2`.
- Arm A implementation is fixture-only: each authenticated success test holds a RAII guard setting `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` through every request. The shared unauthenticated `spawn_runtime_add_peer_harness` is unchanged.
- Load-bearing mutation: temporarily setting the escape hatch inside the refusal test produced exit `101`, `0 passed; 1 failed`, and `left: 200`, `right: 400` at the status assertion. Transcript: `${MATT_HOME:-$HOME/.matt}/evidence/aug03_5am_0_peer_transport/refusal_mutation_red.txt`.
- After restoring the absent-variable setup, the identical focused command exited `0` with `1 passed`; transcript: `${MATT_HOME:-$HOME/.matt}/evidence/aug03_5am_0_peer_transport/refusal_restored_green.txt`.
- No intentional-inversion diff remains.
- Final operator command rerun exited `0`: `2 passed; 0 failed; 55 filtered out; finished in 4.52s`. Transcript: `${MATT_HOME:-$HOME/.matt}/evidence/aug03_5am_0_peer_transport/operator_two_test_green_final.txt`.

## Closing gates

All closing gates ran at final `HEAD` `4eb8d530571fcd7b835735069a53ef2c140a5970`, rebased onto
`origin/main` `9a18b0832c5b63a87dab508e9f99aafb1119802e` with a clean worktree and index.

| Gate | Exit | Summary |
| --- | --- | --- |
| `(cd engine && timeout 1800 cargo nextest run)` | `0` | `Summary [130.977s] 875 tests run: 875 passed (1 leaky), 7 skipped`; one-minute load `7.99` start, `22.92` end |
| `(cd engine && timeout 900 cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication)` attempt 1 | `101` | `flapjack` `2212 passed; 0 failed`; `flapjack-http` `2247 passed; 1 failed`; one-minute load `25.46` start, `6.37` end |
| `cargo test --lib -p flapjack-http -- router_tests::bulk_replace_streaming_submission_waits_for_body_before_202` | `0` | `1 passed; 0 failed; finished in 0.67s` at the same `HEAD` |
| `(cd engine && timeout 900 cargo test --lib -p flapjack -p flapjack-http -p flapjack-replication)` attempt 2 | `0` | `flapjack` `2212 passed`, `flapjack-http` `2248 passed`, `flapjack-replication` `137 passed`, `0 failed`; one-minute load `12.00` start, `8.13` end |
| `cd engine && cargo check -p flapjack -p flapjack-http -p flapjack-replication` | `0` | clean |
| `cd engine && cargo clippy -p flapjack -p flapjack-http -p flapjack-replication` | `0` | zero warnings |
| `cd engine && cargo fmt --check` | `0` | clean |

The single attempt-1 red, `flapjack-http::router_tests::bulk_replace_streaming_submission_waits_for_body_before_202`,
is the documented host-load-sensitive broad-suite family, not product behavior: it panicked waiting for a
terminal bulk-replace state at one-minute load `25.46`, passed focused in `0.67s` at the same `HEAD`, and passed
inside the identical broad command on attempt 2 at one-minute load `12.00`. It lives in
`engine/flapjack-http/src/router_tests.rs`, which this stage does not touch, so it cannot be a regression from
the Stage 1 delta. Durable transcripts: `${MATT_HOME:-$HOME/.matt}/evidence/aug03_5am_0_peer_transport/`
(`nextest_final_s09.txt`, `lib_test_final_s09.txt`, `bulk_replace_streaming_focused_s09.txt`,
`lib_test_final_s09_attempt2.txt`, `cargo_check_final_s09.txt`, `cargo_clippy_final_s09.txt`,
`cargo_fmt_final_s09.txt`).

## Final state

- Final code `HEAD`: `4eb8d530571fcd7b835735069a53ef2c140a5970` on branch
  `batman/aug03_5am_0_runtime_peer_transport_regression_and_dur1_respecimen`. Every gate above ran at that
  commit; the only commit after it is the documentation-only commit that adds this closing-gate section, so no
  compiled or tested source differs between the gated tree and the branch tip.
- Final diff against the merge base `9a18b0832c5b63a87dab508e9f99aafb1119802e` is two files, `107` insertions,
  `0` deletions: this receipt and `engine/tests/test_replication.rs`.
- `engine/tests/test_replication.rs` gains an `EnvironmentVariableRestoreGuard` RAII helper, the new
  `authenticated_http_add_peer_rejects_cleartext_caller_credentials_by_default` refusal test, and a
  `#[serial_test::serial(runtime_peer_transport_env)]` attribute plus a guard-held
  `FLAPJACK_ALLOW_CLEARTEXT_REPLICATION_PEERS=1` opt-in on each of the two authenticated success tests.
- Arm A verified at this `HEAD`: no production file changed. `add_cluster_peer`,
  `validate_authenticated_query_peer_transport`, and `NodeConfig::validate_credentialed_peer_transport` are
  byte-identical to the merge base, so `validate_credentialed_peer_transport` remains the single owner of the
  cleartext peer-transport rule and no second validator exists.
- Publication note: `origin/main` advanced from `9a18b0832` to `6f007a2f4` while these gates ran. The branch was
  not re-rebased, because doing so would invalidate every gate above and `main` is advancing faster than a gate
  cycle. The Stage 1-owned delta is the two files listed above and merges cleanly forward.
