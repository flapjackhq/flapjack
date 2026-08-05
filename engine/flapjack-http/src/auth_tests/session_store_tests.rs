//! Behavioral contract for the durable dashboard session store.
//!
//! These tests pin transport-neutral behavior: opaque tokens, private admin-key-bound
//! persistence, revocation/restart agreement, and fail-closed corrupt state.

use super::session::{DashboardSessionStore, SessionStoreError};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;
use std::path::Path;
use std::sync::{Arc, Barrier};
use tempfile::TempDir;

/// Distinctive admin key so a substring search of the persisted state cannot match by
/// coincidence.
const SENTINEL_ADMIN_KEY: &str = "fj_admin_sentinel_9f3c1d7b4a20e856";

/// A distinct key used to prove persisted verifiers cannot move between admin keys.
const DIFFERENT_ADMIN_KEY: &str = "fj_admin_different_2c8e7401b5d693af";

/// A token shorter than this is guessable regardless of how it was generated.
const MIN_OPAQUE_TOKEN_LENGTH: usize = 32;

/// Domain-separated message the persisted key fingerprint is derived from. The
/// fingerprint is what lets a store opened under a different admin key recognize that
/// the persisted verifiers are not its own, instead of silently counting them as active.
const KEY_FINGERPRINT_DOMAIN: &str = "flapjack.dashboard_session_state.v1";

/// Domain separation prevents a session verifier from being confused with another
/// admin-key-authenticated value that happens to contain the same bytes.
const SESSION_VERIFIER_DOMAIN: &[u8] = b"flapjack.dashboard_session_verifier.v1\0";

/// A SHA-256-sized salt gives each persisted verifier an independent preimage even
/// when two stores mint the same token material.
const SESSION_VERIFIER_SALT_HEX_LENGTH: usize = 64;

fn open_store(state_dir: &Path) -> DashboardSessionStore {
    DashboardSessionStore::open(state_dir, SENTINEL_ADMIN_KEY)
        .expect("opening a session store on a usable directory must succeed")
}

/// Mint a session and assert the token itself is safe to hand to a browser.
fn mint_opaque_token(store: &DashboardSessionStore) -> String {
    let token = store
        .mint_session()
        .expect("minting a dashboard session must succeed");

    assert!(
        token.len() >= MIN_OPAQUE_TOKEN_LENGTH,
        "minted token must carry at least {MIN_OPAQUE_TOKEN_LENGTH} characters of entropy, got {token:?}"
    );
    assert!(
        token
            .chars()
            .all(|character| character.is_ascii_alphanumeric()
                || character == '-'
                || character == '_'),
        "minted token must be transport-safe ASCII, got {token:?}"
    );
    assert_ne!(
        token, SENTINEL_ADMIN_KEY,
        "minted token must never be the admin key itself"
    );
    assert!(
        !token.contains(SENTINEL_ADMIN_KEY),
        "minted token must not embed the admin key"
    );

    token
}

fn read_persisted_state(state_dir: &Path) -> Vec<u8> {
    let state_file_path = DashboardSessionStore::state_file_path(state_dir);
    std::fs::read(&state_file_path).unwrap_or_else(|error| {
        panic!("persisted session state must exist at {state_file_path:?} after minting: {error}")
    })
}

fn write_persisted_state(state_dir: &Path, state: serde_json::Value) {
    let state_file_path = DashboardSessionStore::state_file_path(state_dir);
    std::fs::write(
        &state_file_path,
        serde_json::to_vec(&state).unwrap_or_else(|error| {
            panic!("serializing persisted session-state fixture must succeed: {error}")
        }),
    )
    .unwrap_or_else(|error| {
        panic!(
            "writing persisted session-state fixture to {state_file_path:?} must succeed: {error}"
        )
    });
}

fn while_live_state_file_is_unusable<T>(state_dir: &Path, operation: impl FnOnce() -> T) -> T {
    let state_file_path = DashboardSessionStore::state_file_path(state_dir);
    let persisted_state = read_persisted_state(state_dir);
    std::fs::remove_file(&state_file_path).expect("removing live state file must succeed");
    std::fs::create_dir(&state_file_path)
        .expect("replacing the state file with an unusable directory must succeed");

    let result = operation();

    std::fs::remove_dir(&state_file_path).expect("removing unusable state fixture must succeed");
    std::fs::write(&state_file_path, persisted_state)
        .expect("restoring pre-operation state must succeed");
    result
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn keyed_hex_digest(admin_key: &str, message_parts: &[&[u8]]) -> String {
    let mut digest = Hmac::<Sha256>::new_from_slice(admin_key.as_bytes())
        .expect("HMAC accepts keys of any size");
    for message_part in message_parts {
        digest.update(message_part);
    }
    digest
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn expected_session_verifier(salt: &str, token: &str) -> String {
    keyed_hex_digest(
        SENTINEL_ADMIN_KEY,
        &[SESSION_VERIFIER_DOMAIN, salt.as_bytes(), token.as_bytes()],
    )
}

fn expected_key_fingerprint(admin_key: &str) -> String {
    keyed_hex_digest(admin_key, &[KEY_FINGERPRINT_DOMAIN.as_bytes()])
}

fn valid_verifier_record() -> serde_json::Value {
    let salt = "0".repeat(SESSION_VERIFIER_SALT_HEX_LENGTH);
    json!({
        "salt": salt,
        "digest": expected_session_verifier(&salt, "fixture-token"),
    })
}

fn persisted_state_with_verifiers(verifiers: serde_json::Value) -> serde_json::Value {
    json!({
        "version": 1,
        "key_fingerprint": expected_key_fingerprint(SENTINEL_ADMIN_KEY),
        "verifiers": verifiers,
    })
}

fn persisted_verifier_salt(verifier_record: &serde_json::Value) -> &str {
    let salt = verifier_record
        .get("salt")
        .and_then(serde_json::Value::as_str)
        .expect("each persisted verifier must contain a string salt");
    assert_eq!(
        salt.len(),
        SESSION_VERIFIER_SALT_HEX_LENGTH,
        "each persisted verifier salt must encode 32 bytes as hexadecimal"
    );
    assert!(
        salt.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "each persisted verifier salt must be hexadecimal"
    );
    salt
}

/// Corrupt state must be rejected or load empty, never authenticate a candidate.
fn assert_corrupt_state_fails_closed(state_dir: &Path, previously_valid_token: &str) {
    let unminted_token = format!("{previously_valid_token}_unminted");
    let rejected_candidates = [
        (previously_valid_token, "a token minted before corruption"),
        (unminted_token.as_str(), "an unminted token"),
        (SENTINEL_ADMIN_KEY, "the sentinel admin key"),
        ("", "an empty candidate"),
    ];

    match DashboardSessionStore::open(state_dir, SENTINEL_ADMIN_KEY) {
        Err(SessionStoreError::MalformedState { .. }) => {}
        Err(other) => panic!("corrupt session state must surface MalformedState, got {other:?}"),
        Ok(store) => {
            for (candidate, description) in rejected_candidates {
                assert!(
                    !store.validate_session(candidate),
                    "corrupt session state must not authenticate {description}"
                );
            }
            assert_eq!(
                store.active_session_count(),
                0,
                "a store that tolerates corrupt state must report zero active sessions"
            );
        }
    }
}

fn assert_open_rejects_malformed_state(state_dir: &Path) {
    match DashboardSessionStore::open(state_dir, SENTINEL_ADMIN_KEY) {
        Err(SessionStoreError::MalformedState { .. }) => {}
        Err(other) => {
            panic!("malformed persisted session state must surface MalformedState, got {other:?}")
        }
        Ok(store) => panic!(
            "malformed persisted session state must not open with {} active sessions",
            store.active_session_count()
        ),
    }
}

#[test]
fn minted_token_is_opaque_unique_and_validates() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());

    let first_token = mint_opaque_token(&store);
    let second_token = mint_opaque_token(&store);

    assert_ne!(
        first_token, second_token,
        "each mint must produce a distinct session token"
    );
    assert!(
        store.validate_session(&first_token),
        "a freshly minted token must validate"
    );
    assert!(
        store.validate_session(&second_token),
        "a freshly minted token must validate"
    );
    assert_eq!(
        store.active_session_count(),
        2,
        "both minted sessions must be active"
    );
}

#[test]
fn unminted_token_never_validates() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    let minted_token = mint_opaque_token(&store);

    assert!(
        !store.validate_session(&format!("{minted_token}x")),
        "a near-miss of a minted token must not validate"
    );
    assert!(
        !store.validate_session(SENTINEL_ADMIN_KEY),
        "the admin key must not double as a session token"
    );
    assert!(
        !store.validate_session(""),
        "an empty candidate must not validate"
    );
}

/// Proper token substrings model leaked fragments and truncating transports; exact-match
/// validation must reject all of them before and after restart.
#[test]
fn truncated_and_prefix_token_candidates_never_validate() {
    let temp_dir = TempDir::new().unwrap();
    let token = {
        let store = open_store(temp_dir.path());
        let token = mint_opaque_token(&store);
        assert!(
            store.validate_session(&token),
            "the exact-match fixture requires the whole token to validate"
        );
        assert_near_miss_candidates_are_rejected(&store, &token, "the minting store");
        token
    };

    let reopened_store = open_store(temp_dir.path());

    assert!(
        reopened_store.validate_session(&token),
        "the exact-match fixture requires the whole token to survive reopening"
    );
    assert_near_miss_candidates_are_rejected(&reopened_store, &token, "a reopened store");
}

/// Non-empty near misses derived from `token`: proper prefixes, proper suffixes, and an
/// interior slice. Every one of them shares real token material with an active session.
fn assert_near_miss_candidates_are_rejected(
    store: &DashboardSessionStore,
    token: &str,
    store_description: &str,
) {
    let last_boundary = token.len() - 1;
    let midpoint = token.len() / 2;
    let near_miss_candidates = [
        (
            &token[..last_boundary],
            "a token missing its final character",
        ),
        (&token[..midpoint], "a half-length prefix of a token"),
        (&token[..1], "a single-character prefix of a token"),
        (&token[1..], "a token missing its first character"),
        (
            &token[last_boundary..],
            "a single-character suffix of a token",
        ),
        (&token[1..last_boundary], "an interior slice of a token"),
    ];

    for (candidate, description) in near_miss_candidates {
        assert!(
            !candidate.is_empty(),
            "the near-miss fixture must not degenerate to an empty candidate"
        );
        assert_ne!(
            candidate, token,
            "the near-miss fixture must not degenerate to the whole token"
        );
        assert!(
            !store.validate_session(candidate),
            "{store_description} must not authenticate {description}"
        );
    }
}

#[test]
fn persisted_state_leaks_neither_token_nor_admin_key() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    let token = mint_opaque_token(&store);

    let persisted_state = read_persisted_state(temp_dir.path());

    assert!(
        !contains_bytes(&persisted_state, token.as_bytes()),
        "persisted session state must not contain the plaintext session token"
    );
    assert!(
        !contains_bytes(&persisted_state, SENTINEL_ADMIN_KEY.as_bytes()),
        "persisted session state must not contain the admin key"
    );
}

#[test]
fn persisted_state_contains_only_keyed_session_verifiers() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    let token = mint_opaque_token(&store);

    let persisted_state: serde_json::Value =
        serde_json::from_slice(&read_persisted_state(temp_dir.path()))
            .expect("persisted session state must be valid JSON");
    let verifier_records = persisted_state
        .get("verifiers")
        .and_then(serde_json::Value::as_array)
        .expect("persisted state must contain a verifier array");
    assert_eq!(
        verifier_records.len(),
        1,
        "one mint must persist exactly one session verifier"
    );

    let salt = persisted_verifier_salt(&verifier_records[0]);

    let expected_state = serde_json::json!({
        "version": 1,
        "key_fingerprint": expected_key_fingerprint(SENTINEL_ADMIN_KEY),
        "verifiers": [{
            "salt": salt,
            "digest": expected_session_verifier(salt, &token),
        }],
    });

    assert_eq!(
        persisted_state, expected_state,
        "persisted state must contain only the keyed admin-key fingerprint and versioned, \
         salted HMAC-SHA256 session verifiers"
    );
}

#[test]
fn persisted_session_verifiers_use_distinct_salts() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    mint_opaque_token(&store);
    mint_opaque_token(&store);

    let persisted_state: serde_json::Value =
        serde_json::from_slice(&read_persisted_state(temp_dir.path()))
            .expect("persisted session state must be valid JSON");
    let verifier_records = persisted_state
        .get("verifiers")
        .and_then(serde_json::Value::as_array)
        .expect("persisted state must contain a verifier array");
    assert_eq!(
        verifier_records.len(),
        2,
        "two mints must persist exactly two session verifiers"
    );

    let salts: std::collections::BTreeSet<_> = verifier_records
        .iter()
        .map(persisted_verifier_salt)
        .collect();
    assert_eq!(
        salts.len(),
        verifier_records.len(),
        "each persisted session verifier must use a distinct random salt"
    );
}

#[test]
fn persisted_sessions_are_bound_to_the_admin_key() {
    let temp_dir = TempDir::new().unwrap();
    let token = {
        let store = open_store(temp_dir.path());
        let token = mint_opaque_token(&store);
        assert!(
            store.validate_session(&token),
            "the original admin key must validate its freshly minted session"
        );
        token
    };

    let reopened_store = DashboardSessionStore::open(temp_dir.path(), DIFFERENT_ADMIN_KEY)
        .expect("a different admin key must still open the store fail-closed");

    assert!(
        !reopened_store.validate_session(&token),
        "persisted sessions must not validate under a different admin key"
    );
    assert_eq!(
        reopened_store.active_session_count(),
        0,
        "sessions minted under a different admin key must not remain active"
    );
}

#[cfg(unix)]
#[test]
fn persisted_state_file_is_private() {
    use std::os::unix::fs::PermissionsExt;

    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    mint_opaque_token(&store);

    let state_file_path = DashboardSessionStore::state_file_path(temp_dir.path());
    let permissions = std::fs::metadata(&state_file_path)
        .unwrap_or_else(|error| {
            panic!("persisted session state must exist at {state_file_path:?}: {error}")
        })
        .permissions();

    assert_eq!(
        permissions.mode() & 0o777,
        0o600,
        "persisted session state must be readable and writable only by its owner"
    );
}

#[test]
fn revoked_token_no_longer_validates() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    let token = mint_opaque_token(&store);

    assert!(
        store
            .revoke_session(&token)
            .expect("revoking an active session must succeed"),
        "revoking an active session must report that it was active"
    );
    assert!(
        !store.validate_session(&token),
        "a revoked token must not validate"
    );
    assert_eq!(
        store.active_session_count(),
        0,
        "revoking the only session must leave the store empty"
    );
    assert!(
        !store
            .revoke_session(&token)
            .expect("revoking an already-revoked session must succeed"),
        "revoking an already-revoked session must report that it was not active"
    );
}

#[test]
fn failed_revoke_preserves_the_session_in_memory_and_on_disk() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    let token = mint_opaque_token(&store);
    assert!(
        store.validate_session(&token),
        "the revoke failure fixture requires an active session"
    );

    let revoke_result =
        while_live_state_file_is_unusable(temp_dir.path(), || store.revoke_session(&token));

    match revoke_result {
        Err(SessionStoreError::StateIo { .. }) => {}
        Err(other) => panic!("failed durable revoke must surface StateIo, got {other:?}"),
        Ok(was_active) => {
            panic!("revoke must not return success when state cannot persist: {was_active}")
        }
    }
    assert!(
        store.validate_session(&token),
        "failed durable revoke must leave the current store unchanged"
    );
    assert_eq!(
        store.active_session_count(),
        1,
        "failed durable revoke must preserve the current active count"
    );

    let reopened_store = open_store(temp_dir.path());
    assert!(
        reopened_store.validate_session(&token),
        "restored pre-revoke state must keep the session valid after reopening"
    );
    assert_eq!(
        reopened_store.active_session_count(),
        1,
        "restored pre-revoke state must contain exactly the original session"
    );
}

#[test]
fn reopening_preserves_valid_and_revoked_verdicts() {
    let temp_dir = TempDir::new().unwrap();
    let surviving_token;
    let revoked_token;
    {
        let store = open_store(temp_dir.path());
        surviving_token = mint_opaque_token(&store);
        revoked_token = mint_opaque_token(&store);
        store
            .revoke_session(&revoked_token)
            .expect("revoking an active session must succeed");
    }

    let reopened_store = open_store(temp_dir.path());

    assert!(
        reopened_store.validate_session(&surviving_token),
        "a session that survived shutdown must still validate after reopening"
    );
    assert!(
        !reopened_store.validate_session(&revoked_token),
        "a revoked session must stay revoked after reopening"
    );
    assert_eq!(
        reopened_store.active_session_count(),
        1,
        "reopening must recover exactly the surviving session"
    );
}

#[test]
fn missing_state_initializes_an_empty_store() {
    let temp_dir = TempDir::new().unwrap();

    let store = open_store(temp_dir.path());

    assert_eq!(
        store.active_session_count(),
        0,
        "a store with no persisted state must start empty"
    );
    assert!(
        !store.validate_session("any-candidate-token-at-all"),
        "a store with no persisted state must authenticate nobody"
    );
}

#[test]
fn truncated_state_authenticates_nobody() {
    let temp_dir = TempDir::new().unwrap();
    let token = {
        let store = open_store(temp_dir.path());
        mint_opaque_token(&store)
    };

    let state_file_path = DashboardSessionStore::state_file_path(temp_dir.path());
    let persisted_state = read_persisted_state(temp_dir.path());
    let truncated_state = &persisted_state[..persisted_state.len() / 2];
    std::fs::write(&state_file_path, truncated_state).expect("truncating state must succeed");

    assert_corrupt_state_fails_closed(temp_dir.path(), &token);
}

#[test]
fn invalid_state_authenticates_nobody() {
    let temp_dir = TempDir::new().unwrap();
    let token = {
        let store = open_store(temp_dir.path());
        mint_opaque_token(&store)
    };

    let state_file_path = DashboardSessionStore::state_file_path(temp_dir.path());
    std::fs::write(&state_file_path, b"this is not session state")
        .expect("overwriting state must succeed");

    assert_corrupt_state_fails_closed(temp_dir.path(), &token);
}

#[test]
fn semantically_invalid_persisted_verifier_records_are_rejected() {
    let valid_record = valid_verifier_record();
    let valid_salt = valid_record
        .get("salt")
        .and_then(serde_json::Value::as_str)
        .expect("valid verifier fixture must include a salt");
    let valid_digest = valid_record
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .expect("valid verifier fixture must include a digest");
    let invalid_cases = [
        ("empty salt", json!([{"salt": "", "digest": valid_digest}])),
        (
            "uppercase salt",
            json!([{"salt": "A".repeat(SESSION_VERIFIER_SALT_HEX_LENGTH), "digest": valid_digest}]),
        ),
        (
            "non-hex salt",
            json!([{"salt": "g".repeat(SESSION_VERIFIER_SALT_HEX_LENGTH), "digest": valid_digest}]),
        ),
        (
            "short salt",
            json!([{"salt": "0".repeat(SESSION_VERIFIER_SALT_HEX_LENGTH - 1), "digest": valid_digest}]),
        ),
        ("empty digest", json!([{"salt": valid_salt, "digest": ""}])),
        (
            "uppercase digest",
            json!([{"salt": valid_salt, "digest": "A".repeat(64)}]),
        ),
        (
            "non-hex digest",
            json!([{"salt": valid_salt, "digest": "g".repeat(64)}]),
        ),
        (
            "short digest",
            json!([{"salt": valid_salt, "digest": "0".repeat(63)}]),
        ),
        (
            "duplicate verifier",
            json!([valid_record.clone(), valid_record.clone()]),
        ),
    ];

    for (case_name, verifiers) in invalid_cases {
        let temp_dir = TempDir::new().unwrap();
        write_persisted_state(temp_dir.path(), persisted_state_with_verifiers(verifiers));

        assert_open_rejects_malformed_state(temp_dir.path());

        assert_eq!(
            DashboardSessionStore::open(temp_dir.path(), DIFFERENT_ADMIN_KEY)
                .expect("a foreign admin key must still discard state before verifier validation")
                .active_session_count(),
            0,
            "case {case_name}: foreign-key state must still load fail-closed with zero sessions"
        );
    }
}

#[test]
fn unusable_state_path_rejects_mint_without_issuing_token() {
    let temp_dir = TempDir::new().unwrap();
    let unusable_state_dir = temp_dir.path().join("not_a_directory");
    std::fs::write(&unusable_state_dir, b"not a directory")
        .expect("creating unusable state path fixture must succeed");

    match DashboardSessionStore::open(&unusable_state_dir, SENTINEL_ADMIN_KEY) {
        Err(SessionStoreError::StateIo { .. }) => {}
        Err(other) => panic!("unusable session state path must surface StateIo, got {other:?}"),
        Ok(store) => match store.mint_session() {
            Err(SessionStoreError::StateIo { .. }) => {}
            Err(other) => panic!("failed durable mint must surface StateIo, got {other:?}"),
            Ok(token) => panic!(
                "minting must not return token {token:?} when session state cannot be persisted"
            ),
        },
    }
}

#[test]
fn failed_mint_preserves_the_existing_sessions_in_memory_and_on_disk() {
    let temp_dir = TempDir::new().unwrap();
    let store = open_store(temp_dir.path());
    let existing_token = mint_opaque_token(&store);
    assert!(
        store.validate_session(&existing_token),
        "the mint failure fixture requires an existing active session"
    );

    let mint_result = while_live_state_file_is_unusable(temp_dir.path(), || store.mint_session());

    match mint_result {
        Err(SessionStoreError::StateIo { .. }) => {}
        Err(other) => panic!("failed durable mint must surface StateIo, got {other:?}"),
        Ok(token) => panic!("mint must not issue token {token:?} when state cannot persist"),
    }
    assert!(
        store.validate_session(&existing_token),
        "failed durable mint must preserve the existing session"
    );
    assert_eq!(
        store.active_session_count(),
        1,
        "failed durable mint must not add an in-memory-only session"
    );

    let reopened_store = open_store(temp_dir.path());
    assert!(
        reopened_store.validate_session(&existing_token),
        "restored pre-mint state must preserve the existing session after reopening"
    );
    assert_eq!(
        reopened_store.active_session_count(),
        1,
        "restored pre-mint state must contain exactly the existing session"
    );
}

#[test]
fn concurrent_mint_and_revoke_converge_to_exact_surviving_sessions() {
    let temp_dir = TempDir::new().unwrap();
    let store = Arc::new(open_store(temp_dir.path()));
    let worker_count = 4;
    let sessions_per_worker = 6;
    let start = Arc::new(Barrier::new(worker_count));

    let handles: Vec<_> = (0..worker_count)
        .map(|_| {
            let store = Arc::clone(&store);
            let start = Arc::clone(&start);
            std::thread::spawn(move || {
                start.wait();
                (0..sessions_per_worker)
                    .map(|session_index| {
                        let token = mint_opaque_token(&store);
                        let should_revoke = session_index % 2 == 0;
                        if should_revoke {
                            assert!(
                                store
                                    .revoke_session(&token)
                                    .expect("revoking an active session must succeed"),
                                "revoking a just-minted session must report that it was active"
                            );
                        }
                        (token, !should_revoke)
                    })
                    .collect::<Vec<(String, bool)>>()
            })
        })
        .collect();

    let expected_verdicts: Vec<(String, bool)> = handles
        .into_iter()
        .flat_map(|handle| handle.join().expect("session worker must not panic"))
        .collect();

    let distinct_tokens: std::collections::BTreeSet<&String> =
        expected_verdicts.iter().map(|(token, _)| token).collect();
    assert_eq!(
        distinct_tokens.len(),
        expected_verdicts.len(),
        "concurrent mints must not collide on a token"
    );

    drop(store);
    let reopened_store = open_store(temp_dir.path());

    let expected_active_count = expected_verdicts
        .iter()
        .filter(|(_, should_be_valid)| *should_be_valid)
        .count();
    assert_eq!(
        reopened_store.active_session_count(),
        expected_active_count,
        "reopening after concurrent mint/revoke must recover exactly the unrevoked sessions"
    );
    for (token, should_be_valid) in &expected_verdicts {
        assert_eq!(
            reopened_store.validate_session(token),
            *should_be_valid,
            "reopened verdict for {token} must match the concurrent mint/revoke outcome"
        );
    }
}
