//! Durable dashboard session store.
//!
//! Owns the mint/validate/revoke lifecycle for browser dashboard sessions and the
//! on-disk state that survives a server restart. The API is deliberately
//! transport-neutral — no headers, cookies, or Axum types — so the HTTP layer can
//! decide how a token travels without this module growing a second opinion about it.
//!
//! Persisted state keeps only a salted HMAC verifier per session, keyed by the admin
//! key, so a reader of the state file can neither replay a session nor learn the admin
//! key, and a file lifted onto a host with a different admin key authenticates nobody.

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use rand::Rng;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use super::generate_hex_key;

/// File inside the session state directory holding persisted dashboard sessions.
const SESSION_STATE_FILE_NAME: &str = "dashboard_sessions.json";

/// Only version this store reads or writes; any other persisted version is corruption.
const SUPPORTED_STATE_VERSION: u32 = 1;

/// Domain-separated message the persisted key fingerprint is derived from, so a store
/// opened under a different admin key recognizes the persisted verifiers as not its own
/// rather than silently counting them as active.
const KEY_FINGERPRINT_DOMAIN: &[u8] = b"flapjack.dashboard_session_state.v1";

/// Domain separation prevents a session verifier from colliding with another
/// admin-key-authenticated value that happens to share the same input bytes.
const SESSION_VERIFIER_DOMAIN: &[u8] = b"flapjack.dashboard_session_verifier.v1\0";

/// Bytes of random salt per verifier; rendered as 64 lowercase hex characters at rest.
const SESSION_VERIFIER_SALT_BYTES: usize = 32;

type HmacSha256 = Hmac<Sha256>;

const HEX_ENCODED_SHA256_LENGTH: usize = 64;

/// Failure modes for reading, writing, or interpreting persisted session state.
#[derive(Debug)]
pub enum SessionStoreError {
    /// Persisted state was readable but could not be interpreted as session records.
    /// Callers must treat this as "no session is authenticated", never as "empty".
    MalformedState { path: PathBuf, detail: String },
    /// Persisted state could not be read from or written to disk.
    StateIo { path: PathBuf, detail: String },
}

impl fmt::Display for SessionStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedState { path, detail } => {
                write!(formatter, "Malformed session state at {path:?}: {detail}")
            }
            Self::StateIo { path, detail } => {
                write!(formatter, "Session state I/O failure at {path:?}: {detail}")
            }
        }
    }
}

impl std::error::Error for SessionStoreError {}

/// One persisted session: a random salt and the salted HMAC digest of the token. The
/// plaintext token is never stored, so the digest cannot be reversed into a token.
#[derive(Clone, Serialize, Deserialize)]
struct SessionVerifier {
    salt: String,
    digest: String,
}

/// The exact on-disk shape. Field names and the flat verifier records are pinned by the
/// behavioral contract, so this struct is the single source of truth for the format.
#[derive(Serialize, Deserialize)]
struct PersistedSessionState {
    version: u32,
    key_fingerprint: String,
    verifiers: Vec<SessionVerifier>,
}

/// Durable store of active dashboard sessions.
///
/// Tokens are handed to the browser once at mint time and are never recoverable from
/// the persisted state: the store keeps only a verifier derived from the token, so a
/// reader of the state file can neither replay a session nor learn the admin key.
pub struct DashboardSessionStore {
    state_file_path: PathBuf,
    /// Pepper for the persisted token verifiers. Held so a state file lifted onto a
    /// host without the admin key cannot be used to validate any token.
    admin_key: String,
    /// In-memory verifiers, kept in lockstep with the state file. The single lock
    /// serializes every mint/revoke so concurrent mutations cannot interleave a memory
    /// update with another writer's file write.
    verifiers: Mutex<Vec<SessionVerifier>>,
    #[cfg(test)]
    parent_dir_sync_failure_for_test: Mutex<Option<io::ErrorKind>>,
}

impl DashboardSessionStore {
    /// Path of the persisted state file inside `state_dir`.
    pub fn state_file_path(state_dir: &Path) -> PathBuf {
        state_dir.join(SESSION_STATE_FILE_NAME)
    }

    /// Open the store rooted at `state_dir`, loading any previously persisted sessions.
    ///
    /// A missing state file initializes an empty store. Malformed state surfaces
    /// `MalformedState`; state bound to a different admin key loads empty (fail-closed).
    pub fn open(state_dir: &Path, admin_key: &str) -> Result<Self, SessionStoreError> {
        let store = Self {
            state_file_path: Self::state_file_path(state_dir),
            admin_key: admin_key.to_string(),
            verifiers: Mutex::new(Vec::new()),
            #[cfg(test)]
            parent_dir_sync_failure_for_test: Mutex::new(None),
        };
        let loaded = store.load_persisted_verifiers()?;
        *store.lock_verifiers() = loaded;
        Ok(store)
    }

    /// Mint a new session and return its opaque token, persisting the session first so a
    /// crash immediately after minting cannot leave a token the store will reject.
    pub fn mint_session(&self) -> Result<String, SessionStoreError> {
        let token = generate_hex_key();
        let salt = generate_verifier_salt();
        let verifier = SessionVerifier {
            digest: self.verifier_digest(&salt, &token),
            salt,
        };

        let mut verifiers = self.lock_verifiers();
        let mut next = verifiers.clone();
        next.push(verifier);
        self.persist_and_replace_verifiers(&mut verifiers, next)?;
        Ok(token)
    }

    /// Whether `candidate_token` names an active session, by exact constant-time match.
    pub fn validate_session(&self, candidate_token: &str) -> bool {
        self.lock_verifiers()
            .iter()
            .any(|verifier| self.verifier_matches(verifier, candidate_token))
    }

    /// Revoke the session named by `token`, returning whether it had been active. An
    /// absent token performs no write; a pre-commit durable-write failure preserves the
    /// store, while an indeterminate post-rename failure closes it to authentication.
    pub fn revoke_session(&self, token: &str) -> Result<bool, SessionStoreError> {
        let mut verifiers = self.lock_verifiers();
        let Some(index) = verifiers
            .iter()
            .position(|verifier| self.verifier_matches(verifier, token))
        else {
            return Ok(false);
        };
        let mut next = verifiers.clone();
        next.remove(index);
        self.persist_and_replace_verifiers(&mut verifiers, next)?;
        Ok(true)
    }

    /// Number of sessions currently able to authenticate.
    #[cfg(test)]
    pub fn active_session_count(&self) -> usize {
        self.lock_verifiers().len()
    }

    #[cfg(test)]
    pub(crate) fn fail_next_parent_dir_sync_for_test(&self) {
        *self
            .parent_dir_sync_failure_for_test
            .lock()
            .expect("session store test fault lock poisoned") = Some(io::ErrorKind::Other);
    }

    fn lock_verifiers(&self) -> std::sync::MutexGuard<'_, Vec<SessionVerifier>> {
        self.verifiers.lock().expect("session store lock poisoned")
    }

    /// Recompute the digest for `candidate_token` under `verifier`'s salt and compare it
    /// with the stored digest in constant time. Both digests are fixed-width hex.
    fn verifier_matches(&self, verifier: &SessionVerifier, candidate_token: &str) -> bool {
        let candidate_digest = self.verifier_digest(&verifier.salt, candidate_token);
        candidate_digest.len() == verifier.digest.len()
            && candidate_digest
                .as_bytes()
                .ct_eq(verifier.digest.as_bytes())
                .into()
    }

    /// Fingerprint proving persisted state belongs to this store's admin key.
    fn key_fingerprint(&self) -> String {
        keyed_hex_digest(&self.admin_key, &[KEY_FINGERPRINT_DOMAIN])
    }

    /// Salted HMAC of a token under this store's admin key. The salt is fed as its hex
    /// string so the digest is bound to the exact bytes persisted alongside it.
    fn verifier_digest(&self, salt: &str, token: &str) -> String {
        keyed_hex_digest(
            &self.admin_key,
            &[SESSION_VERIFIER_DOMAIN, salt.as_bytes(), token.as_bytes()],
        )
    }

    /// Load the persisted verifiers, mapping every failure to the fail-closed verdict the
    /// contract requires: missing file → empty, unreadable → `StateIo`, unparseable →
    /// `MalformedState`, foreign admin key → empty.
    fn load_persisted_verifiers(&self) -> Result<Vec<SessionVerifier>, SessionStoreError> {
        let bytes = match std::fs::read(&self.state_file_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(self.state_io(error)),
        };
        let state: PersistedSessionState =
            serde_json::from_slice(&bytes).map_err(|error| self.malformed_state(error))?;
        if state.version != SUPPORTED_STATE_VERSION {
            return Err(self.malformed_state(format!(
                "unsupported session state version {}",
                state.version
            )));
        }
        if state.key_fingerprint != self.key_fingerprint() {
            // State bound to a different admin key: discard fail-closed, do not error.
            return Ok(Vec::new());
        }
        self.validate_persisted_verifiers(&state.verifiers)?;
        Ok(state.verifiers)
    }

    fn validate_persisted_verifiers(
        &self,
        verifiers: &[SessionVerifier],
    ) -> Result<(), SessionStoreError> {
        let mut seen = BTreeSet::new();
        for (index, verifier) in verifiers.iter().enumerate() {
            if !is_lowercase_hex(&verifier.salt, SESSION_VERIFIER_SALT_BYTES * 2) {
                return Err(self.malformed_state(format!(
                    "verifier {index} salt must be 64 lowercase hex characters"
                )));
            }
            if !is_lowercase_hex(&verifier.digest, HEX_ENCODED_SHA256_LENGTH) {
                return Err(self.malformed_state(format!(
                    "verifier {index} digest must be 64 lowercase hex characters"
                )));
            }
            if !seen.insert((verifier.salt.as_str(), verifier.digest.as_str())) {
                return Err(
                    self.malformed_state(format!("duplicate verifier record at index {index}"))
                );
            }
        }
        Ok(())
    }

    fn persist_and_replace_verifiers(
        &self,
        current: &mut Vec<SessionVerifier>,
        next: Vec<SessionVerifier>,
    ) -> Result<(), SessionStoreError> {
        match self.persist(&next) {
            Ok(()) => {
                *current = next;
                Ok(())
            }
            Err(DurableWriteError::BeforeCommit(error)) => Err(error),
            Err(DurableWriteError::CommitIndeterminate(error)) => {
                current.clear();
                Err(error)
            }
        }
    }

    /// Durably write the full verifier set, replacing the state file atomically. On any
    /// pre-commit failure the caller's in-memory set is left untouched and no token is
    /// handed out. A post-rename directory-sync failure is indeterminate and must close
    /// the caller's in-memory set instead of acknowledging success.
    fn persist(&self, verifiers: &[SessionVerifier]) -> Result<(), DurableWriteError> {
        let state = PersistedSessionState {
            version: SUPPORTED_STATE_VERSION,
            key_fingerprint: self.key_fingerprint(),
            verifiers: verifiers.to_vec(),
        };
        let encoded = serde_json::to_vec(&state).expect("session state always serializes");
        self.write_state_atomically(&encoded)
    }

    /// Write to a fresh temp file in the state directory, fsync it, rename it over the
    /// live file, then fsync the directory — so a crash leaves either the old file or the
    /// complete new one, never a half-written state. Cleans up the temp on any failure.
    fn write_state_atomically(&self, encoded: &[u8]) -> Result<(), DurableWriteError> {
        let temp_path = self.next_write_temp_path();
        let result = self.write_temp_then_rename(&temp_path, encoded);
        if result.is_err() {
            // Safe to remove unconditionally: the name belongs to this write alone, so
            // cleanup can never delete a concurrent writer's in-flight temp file.
            let _ = std::fs::remove_file(&temp_path);
        }
        result
    }

    /// A temp path used by exactly one durable write, beside the live state file so the
    /// rename stays within one filesystem. Uniqueness keeps exclusive creation a real
    /// guard against a concurrent writer while ensuring a write that dies before its
    /// rename strands only its own name, never one that blocks every later write.
    fn next_write_temp_path(&self) -> PathBuf {
        static WRITE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

        let mut temp_file_name = self.state_file_path.as_os_str().to_os_string();
        temp_file_name.push(format!(
            ".{}.{}.tmp",
            std::process::id(),
            WRITE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        PathBuf::from(temp_file_name)
    }

    fn write_temp_then_rename(
        &self,
        temp_path: &Path,
        encoded: &[u8],
    ) -> Result<(), DurableWriteError> {
        let mut file = create_owner_private_file(temp_path)
            .map_err(|error| DurableWriteError::before_commit(io_failure(temp_path, error)))?;
        file.write_all(encoded)
            .map_err(|error| DurableWriteError::before_commit(io_failure(temp_path, error)))?;
        file.sync_all()
            .map_err(|error| DurableWriteError::before_commit(io_failure(temp_path, error)))?;
        drop(file);
        std::fs::rename(temp_path, &self.state_file_path)
            .map_err(|error| DurableWriteError::before_commit(self.state_io(error)))?;
        self.sync_parent_dir()
            .map_err(DurableWriteError::commit_indeterminate)?;
        Ok(())
    }

    /// Fsync the directory holding the state file so the rename itself is durable.
    fn sync_parent_dir(&self) -> Result<(), SessionStoreError> {
        #[cfg(test)]
        {
            if let Some(error_kind) = self
                .parent_dir_sync_failure_for_test
                .lock()
                .expect("session store test fault lock poisoned")
                .take()
            {
                return Err(self.state_io(io::Error::from(error_kind)));
            }
        }

        #[cfg(unix)]
        {
            if let Some(parent) = self.state_file_path.parent() {
                if !parent.as_os_str().is_empty() {
                    let directory = File::open(parent).map_err(|error| self.state_io(error))?;
                    directory.sync_all().map_err(|error| self.state_io(error))?;
                }
            }
        }
        Ok(())
    }

    fn state_io(&self, error: impl fmt::Display) -> SessionStoreError {
        io_failure(&self.state_file_path, error)
    }

    fn malformed_state(&self, error: impl fmt::Display) -> SessionStoreError {
        SessionStoreError::MalformedState {
            path: self.state_file_path.clone(),
            detail: error.to_string(),
        }
    }
}

/// Every I/O failure this store reports names the file it actually touched, so the
/// operator is never pointed at the live state file for a temp-file failure.
fn io_failure(path: &Path, error: impl fmt::Display) -> SessionStoreError {
    SessionStoreError::StateIo {
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

enum DurableWriteError {
    BeforeCommit(SessionStoreError),
    CommitIndeterminate(SessionStoreError),
}

impl DurableWriteError {
    fn before_commit(error: SessionStoreError) -> Self {
        Self::BeforeCommit(error)
    }

    fn commit_indeterminate(error: SessionStoreError) -> Self {
        Self::CommitIndeterminate(error)
    }
}

/// Create `path` for exclusive writing, owner-private from the moment it exists. The
/// permissions are set at creation rather than chmodded afterwards because the file
/// receives every verifier digest and the admin-key fingerprint, and even the instant
/// between an umask-default creation and a follow-up chmod is readable by any local user.
fn create_owner_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Fresh 32-byte random salt rendered as 64 lowercase hex characters. Generated inline
/// here rather than reusing `key_store`'s private helper, which sits outside this owner's
/// blast radius.
fn generate_verifier_salt() -> String {
    let salt_bytes: [u8; SESSION_VERIFIER_SALT_BYTES] = rand::thread_rng().gen();
    hex::encode(salt_bytes)
}

fn is_lowercase_hex(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Lowercase-hex HMAC-SHA256 of `message_parts` concatenated in order, keyed by
/// `admin_key`. The single source of truth for both the key fingerprint and the session
/// verifier digests.
fn keyed_hex_digest(admin_key: &str, message_parts: &[&[u8]]) -> String {
    let mut mac =
        HmacSha256::new_from_slice(admin_key.as_bytes()).expect("HMAC accepts keys of any size");
    for message_part in message_parts {
        mac.update(message_part);
    }
    hex::encode(mac.finalize().into_bytes())
}

/// The store's behavioral contract lives in `crate::auth_tests::session_store_tests`.
/// These module-local tests cover the durable-write properties that contract cannot
/// observe from outside the store.
#[cfg(test)]
mod session_store_tests {
    use super::*;

    const TEST_ADMIN_KEY: &str = "dashboard-session-store-test-admin-key";

    fn open_test_store(state_dir: &Path) -> DashboardSessionStore {
        DashboardSessionStore::open(state_dir, TEST_ADMIN_KEY)
            .expect("opening a session store on a usable directory must succeed")
    }

    fn mint_test_session(store: &DashboardSessionStore) -> String {
        store
            .mint_session()
            .expect("minting a dashboard session must succeed")
    }

    #[cfg(unix)]
    #[test]
    fn write_temp_file_is_owner_private_from_the_moment_it_is_created() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let file = create_owner_private_file(&temp_dir.path().join("state.tmp"))
            .expect("creating the durable-write temp file must succeed");

        let mode = file
            .metadata()
            .expect("reading temp file metadata must succeed")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(
            mode, 0o600,
            "the temp file carries every verifier digest, so it must never exist at \
             wider-than-owner permissions, not even before a follow-up chmod"
        );
    }

    #[test]
    fn each_durable_write_claims_a_distinct_temp_path() {
        let store = DashboardSessionStore {
            state_file_path: Path::new("/tmp/state_dir").join(SESSION_STATE_FILE_NAME),
            admin_key: "unused".to_string(),
            verifiers: Mutex::new(Vec::new()),
            parent_dir_sync_failure_for_test: Mutex::new(None),
        };

        let first_temp_path = store.next_write_temp_path();
        let second_temp_path = store.next_write_temp_path();

        assert_ne!(
            first_temp_path, second_temp_path,
            "a crashed write must not strand a temp name that blocks every later write"
        );
        for temp_path in [&first_temp_path, &second_temp_path] {
            assert_eq!(
                temp_path.parent(),
                store.state_file_path.parent(),
                "the temp file must sit beside the live state file so the rename is atomic"
            );
            assert_eq!(
                temp_path
                    .extension()
                    .and_then(|extension| extension.to_str()),
                Some("tmp"),
                "the temp path must be recognizable as a temp artifact, got {temp_path:?}"
            );
        }
    }

    #[test]
    fn unsupported_state_version_is_rejected_as_malformed() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = open_test_store(temp_dir.path());
        let state = PersistedSessionState {
            version: SUPPORTED_STATE_VERSION + 1,
            key_fingerprint: store.key_fingerprint(),
            verifiers: Vec::new(),
        };
        std::fs::write(
            &store.state_file_path,
            serde_json::to_vec(&state).expect("version fixture must serialize"),
        )
        .expect("version fixture must persist");

        match DashboardSessionStore::open(temp_dir.path(), TEST_ADMIN_KEY) {
            Err(SessionStoreError::MalformedState { detail, .. }) => assert!(
                detail.contains("unsupported session state version 2"),
                "version rejection must identify the unsupported version: {detail}"
            ),
            Err(other) => panic!("unsupported state version must be malformed: {other:?}"),
            Ok(_) => panic!("unsupported state version must not open"),
        }
    }

    #[test]
    fn failed_parent_directory_sync_does_not_acknowledge_mint_or_revoke() {
        let mint_temp_dir = tempfile::TempDir::new().unwrap();
        let mint_store = open_test_store(mint_temp_dir.path());
        let existing_token = mint_test_session(&mint_store);

        mint_store.fail_next_parent_dir_sync_for_test();
        match mint_store.mint_session() {
            Err(SessionStoreError::StateIo { .. }) => {}
            Err(other) => panic!(
                "failed parent-directory fsync during mint must surface StateIo, got {other:?}"
            ),
            Ok(token) => panic!(
                "mint must not issue token {token:?} when the rename is not durably committed"
            ),
        }
        assert_eq!(mint_store.active_session_count(), 0);
        assert!(!mint_store.validate_session(&existing_token));

        let revoke_temp_dir = tempfile::TempDir::new().unwrap();
        let revoke_store = open_test_store(revoke_temp_dir.path());
        let token_for_revoke = mint_test_session(&revoke_store);
        revoke_store.fail_next_parent_dir_sync_for_test();
        match revoke_store.revoke_session(&token_for_revoke) {
            Err(SessionStoreError::StateIo { .. }) => {}
            Err(other) => panic!(
                "failed parent-directory fsync during revoke must surface StateIo, got {other:?}"
            ),
            Ok(was_active) => {
                panic!("revoke must not succeed when the rename is not durable: {was_active}")
            }
        }
        assert_eq!(revoke_store.active_session_count(), 0);
        assert!(!revoke_store.validate_session(&token_for_revoke));
    }

    #[test]
    fn stranded_write_temp_file_does_not_block_the_next_mint() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("dashboard_sessions.json.tmp"),
            b"partial write from a crashed process",
        )
        .expect("planting a stranded temp artifact must succeed");

        let store = open_test_store(temp_dir.path());
        let token = mint_test_session(&store);
        assert!(store.validate_session(&token));

        drop(store);
        assert!(open_test_store(temp_dir.path()).validate_session(&token));
    }

    #[test]
    fn successful_durable_writes_leave_no_temp_artifacts() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = open_test_store(temp_dir.path());
        let revoked_token = mint_test_session(&store);
        let surviving_token = mint_test_session(&store);
        store
            .revoke_session(&revoked_token)
            .expect("revoking an active session must succeed");

        let mut state_dir_entries: Vec<String> = std::fs::read_dir(temp_dir.path())
            .expect("reading the state directory must succeed")
            .map(|entry| {
                entry
                    .expect("each state directory entry must be readable")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        state_dir_entries.sort();

        assert_eq!(
            state_dir_entries,
            vec![SESSION_STATE_FILE_NAME.to_string()],
            "durable writes must leave only the live state file behind"
        );
        assert!(store.validate_session(&surviving_token));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_durable_write_names_the_temp_file_it_could_not_create() {
        use std::os::unix::fs::PermissionsExt;

        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = open_test_store(temp_dir.path());

        std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o500))
            .expect("making the state directory read-only must succeed");
        let mint_result = store.mint_session();
        std::fs::set_permissions(temp_dir.path(), std::fs::Permissions::from_mode(0o700))
            .expect("restoring state directory permissions must succeed");

        match mint_result {
            Err(SessionStoreError::StateIo { path, .. }) => assert!(
                path.to_string_lossy().ends_with(".tmp"),
                "a failed temp write must name the temp file it could not create, got {path:?}"
            ),
            Err(other) => panic!("an uncreatable temp file must surface StateIo, got {other:?}"),
            Ok(token) => panic!("mint must not issue {token} when its state cannot persist"),
        }
    }
}
