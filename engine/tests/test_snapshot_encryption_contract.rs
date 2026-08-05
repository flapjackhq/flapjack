use aes_gcm_siv::aead::{Aead, KeyInit, Payload};
use aes_gcm_siv::{Aes256GcmSiv, Nonce};
use flapjack::index::snapshot::{
    export_to_bytes, export_to_bytes_with_key, import_from_bytes, import_from_bytes_with_key,
};
use flapjack::FlapjackError;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::Path;
use tar::{Builder, EntryType, Header};
use tempfile::TempDir;

const SNAPSHOT_ENCRYPTION_MAGIC: &[u8; 8] = b"FJSNAPE1";
const SNAPSHOT_ENCRYPTION_VERSION: u8 = 1;
const SNAPSHOT_ENCRYPTION_NONCE_LEN: usize = 12;
const SNAPSHOT_ENCRYPTION_HEADER_LEN: usize = 21;
const SNAPSHOT_KEY_FILE_ENV: &str = "FLAPJACK_SNAPSHOT_KEY_FILE";
const TEST_KEY: [u8; 32] = [0x42; 32];
const WRONG_KEY: [u8; 32] = [0x24; 32];

enum ExpectedEncryptedImportRejection<'a> {
    MessageContains(&'a str),
    IoError,
}

struct EnvVarRestore {
    previous: Option<OsString>,
}

impl EnvVarRestore {
    fn set(value: &Path) -> Self {
        let previous = std::env::var_os(SNAPSHOT_KEY_FILE_ENV);
        std::env::set_var(SNAPSHOT_KEY_FILE_ENV, value);
        Self { previous }
    }

    fn unset() -> Self {
        let previous = std::env::var_os(SNAPSHOT_KEY_FILE_ENV);
        std::env::remove_var(SNAPSHOT_KEY_FILE_ENV);
        Self { previous }
    }
}

impl Drop for EnvVarRestore {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var(SNAPSHOT_KEY_FILE_ENV, value),
            None => std::env::remove_var(SNAPSHOT_KEY_FILE_ENV),
        }
    }
}

fn source_fixture() -> TempDir {
    let source = TempDir::new().expect("source tempdir must be created");
    fs::write(source.path().join("data.json"), br#"{"objectID":"one"}"#)
        .expect("fixture file must be written");
    fs::create_dir(source.path().join("nested")).expect("nested fixture dir must be created");
    fs::write(
        source.path().join("nested/payload.bin"),
        [0x7a, 0x00, 0x11, 0xff],
    )
    .expect("nested fixture file must be written");
    source
}

fn assert_restored_matches_source(source: &Path, restored: &Path) {
    assert_eq!(
        fs::read(restored.join("data.json")).expect("restored data.json must be readable"),
        fs::read(source.join("data.json")).expect("source data.json must be readable")
    );
    assert_eq!(
        fs::read(restored.join("nested/payload.bin"))
            .expect("restored nested payload must be readable"),
        fs::read(source.join("nested/payload.bin"))
            .expect("source nested payload must be readable")
    );
}

fn assert_directory_empty(path: &Path) {
    let entries = fs::read_dir(path)
        .expect("destination directory must be readable")
        .collect::<std::result::Result<Vec<_>, _>>()
        .expect("destination entries must be readable");
    assert!(
        entries.is_empty(),
        "destination must stay empty after failed encrypted import, found {entries:?}"
    );
}

fn recompute_tar_header_checksum(header_block: &mut [u8]) {
    header_block[148..156].fill(b' ');
    let checksum = header_block.iter().map(|byte| *byte as u32).sum::<u32>();
    let checksum_octal = format!("{checksum:06o}\0 ");
    header_block[148..156].copy_from_slice(checksum_octal.as_bytes());
}

fn archive_with_patched_path(path: &str, contents: &[u8]) -> Vec<u8> {
    assert!(
        path.len() < 100,
        "path must fit into the ustar name field for this test helper"
    );

    let mut archive = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    archive
        .append_data(&mut header, "safe.txt", contents)
        .expect("must build baseline test archive entry");
    let mut tar_bytes = archive.into_inner().expect("must finalize tar stream");

    let header_block = &mut tar_bytes[0..512];
    header_block[0..100].fill(0);
    header_block[0..path.len()].copy_from_slice(path.as_bytes());
    recompute_tar_header_checksum(header_block);

    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&tar_bytes)
        .expect("must write tar payload into gzip encoder");
    encoder.finish().expect("must finalize gzip stream")
}

fn archive_with_link(
    entry_type: EntryType,
    link_path: &str,
    target: &str,
    payload_path: &str,
) -> Vec<u8> {
    let mut archive = Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));

    let mut link_header = Header::new_gnu();
    link_header.set_entry_type(entry_type);
    link_header.set_size(0);
    link_header.set_mode(0o777);
    link_header.set_cksum();
    archive
        .append_link(&mut link_header, link_path, target)
        .expect("must build link entry");

    let payload = b"link-escape-probe";
    let mut payload_header = Header::new_gnu();
    payload_header.set_entry_type(EntryType::Regular);
    payload_header.set_size(payload.len() as u64);
    payload_header.set_mode(0o644);
    payload_header.set_cksum();
    archive
        .append_data(&mut payload_header, payload_path, payload.as_slice())
        .expect("must build payload entry");

    archive
        .into_inner()
        .expect("must finalize tar stream")
        .finish()
        .expect("must finalize gzip stream")
}

fn archive_with_truncated_entry_payload() -> Vec<u8> {
    let payload = vec![0x5a; 1024];
    let mut archive = Builder::new(Vec::new());
    let mut header = Header::new_gnu();
    header.set_size(payload.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(EntryType::Regular);
    header.set_cksum();
    archive
        .append_data(&mut header, "payload.bin", payload.as_slice())
        .expect("must build complete baseline entry");
    let mut tar_bytes = archive.into_inner().expect("must finalize tar stream");

    tar_bytes.truncate(512 + payload.len() / 2);
    let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
    encoder
        .write_all(&tar_bytes)
        .expect("must write truncated tar payload into gzip encoder");
    encoder.finish().expect("must finalize gzip stream")
}

// Hostile tar headers cannot be produced through the safe production export API,
// so this test-only fixture builder independently emits the persisted envelope.
fn encrypt_hostile_snapshot_fixture(plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256GcmSiv::new_from_slice(&TEST_KEY).expect("TEST_KEY must be valid");
    let nonce_bytes = [0xa5; SNAPSHOT_ENCRYPTION_NONCE_LEN];
    let mut envelope = Vec::with_capacity(SNAPSHOT_ENCRYPTION_HEADER_LEN + plaintext.len() + 16);
    envelope.extend_from_slice(SNAPSHOT_ENCRYPTION_MAGIC);
    envelope.push(SNAPSHOT_ENCRYPTION_VERSION);
    envelope.extend_from_slice(&nonce_bytes);
    assert_eq!(envelope.len(), SNAPSHOT_ENCRYPTION_HEADER_LEN);

    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext,
                aad: &envelope,
            },
        )
        .expect("hostile fixture encryption must succeed");
    envelope.extend_from_slice(&ciphertext);
    envelope
}

fn decrypt_local_envelope(encrypted: &[u8]) -> Vec<u8> {
    assert!(
        encrypted.len() > SNAPSHOT_ENCRYPTION_HEADER_LEN,
        "encrypted snapshot must include ciphertext after the local header"
    );
    let cipher = Aes256GcmSiv::new_from_slice(&TEST_KEY).expect("TEST_KEY must be valid");
    cipher
        .decrypt(
            Nonce::from_slice(
                &encrypted[SNAPSHOT_ENCRYPTION_MAGIC.len() + 1..SNAPSHOT_ENCRYPTION_HEADER_LEN],
            ),
            Payload {
                msg: &encrypted[SNAPSHOT_ENCRYPTION_HEADER_LEN..],
                aad: &encrypted[..SNAPSHOT_ENCRYPTION_HEADER_LEN],
            },
        )
        .expect("local header length must point at the ciphertext boundary")
}

fn assert_encrypted_snapshot_import_rejected(
    encrypted: &[u8],
    dest: &Path,
    outside_path: &Path,
    expected_rejection: ExpectedEncryptedImportRejection<'_>,
) {
    fs::create_dir_all(dest).expect("destination fixture must be created");
    let result = import_from_bytes_with_key(encrypted, dest, Some(&TEST_KEY));
    assert!(
        result.is_err(),
        "encrypted snapshot import must reject the crafted input"
    );
    let error = result.expect_err("encrypted snapshot rejection must surface an error");
    match expected_rejection {
        ExpectedEncryptedImportRejection::MessageContains(expected_error) => {
            let message = error.to_string();
            assert!(
                message.contains(expected_error),
                "rejection must name {expected_error:?}, got: {message}"
            );
        }
        ExpectedEncryptedImportRejection::IoError => {
            assert!(
                matches!(error, FlapjackError::Io(_)),
                "truncated payload must surface through the Flapjack IO error classification, got: {error:?}"
            );
        }
    }
    assert!(
        !outside_path.exists(),
        "rejected encrypted snapshot must not write outside destination: {}",
        outside_path.display()
    );
    assert_directory_empty(dest);
}

#[test]
fn encrypted_export_round_trips_with_the_key() {
    let source = source_fixture();
    let encrypted =
        export_to_bytes_with_key(source.path(), Some(&TEST_KEY)).expect("encrypted export");

    assert!(
        encrypted.starts_with(SNAPSHOT_ENCRYPTION_MAGIC),
        "encrypted snapshots must start with FJSNAPE1 magic"
    );

    let restored = TempDir::new().expect("restored tempdir must be created");
    import_from_bytes_with_key(&encrypted, restored.path(), Some(&TEST_KEY))
        .expect("encrypted import with the same key");

    assert_restored_matches_source(source.path(), restored.path());
}

#[test]
fn hostile_fixture_envelope_matches_production_header_contract() {
    let source = source_fixture();
    let production_plaintext =
        export_to_bytes_with_key(source.path(), None).expect("production plaintext export");
    let production_encrypted = export_to_bytes_with_key(source.path(), Some(&TEST_KEY))
        .expect("production encrypted export");
    let fixture_plaintext = b"hostile fixture plaintext";
    let fixture_encrypted = encrypt_hostile_snapshot_fixture(fixture_plaintext);

    for encrypted in [&production_encrypted, &fixture_encrypted] {
        assert_eq!(
            &encrypted[..SNAPSHOT_ENCRYPTION_MAGIC.len()],
            SNAPSHOT_ENCRYPTION_MAGIC
        );
        assert_eq!(
            encrypted[SNAPSHOT_ENCRYPTION_MAGIC.len()],
            SNAPSHOT_ENCRYPTION_VERSION
        );
        assert!(
            encrypted.len() > SNAPSHOT_ENCRYPTION_HEADER_LEN,
            "header must end at byte {SNAPSHOT_ENCRYPTION_HEADER_LEN}, before ciphertext"
        );
    }
    assert_eq!(
        decrypt_local_envelope(&production_encrypted),
        production_plaintext,
        "production envelope ciphertext must begin at the local header length"
    );
    assert_eq!(
        decrypt_local_envelope(&fixture_encrypted),
        fixture_plaintext,
        "fixture envelope ciphertext must begin at the local header length"
    );
}

#[test]
fn encrypted_export_is_not_readable_without_the_key() {
    let source = source_fixture();
    let encrypted =
        export_to_bytes_with_key(source.path(), Some(&TEST_KEY)).expect("encrypted export");
    let restored = TempDir::new().expect("restored tempdir must be created");

    let error = import_from_bytes_with_key(&encrypted, restored.path(), None)
        .expect_err("encrypted import without a key must fail");

    assert!(
        error.to_string().contains(SNAPSHOT_KEY_FILE_ENV),
        "missing-key error must name {SNAPSHOT_KEY_FILE_ENV}, got: {error}"
    );
    assert_directory_empty(restored.path());
}

#[test]
fn encrypted_export_is_not_readable_with_the_wrong_key() {
    let source = source_fixture();
    let encrypted =
        export_to_bytes_with_key(source.path(), Some(&TEST_KEY)).expect("encrypted export");
    let restored = TempDir::new().expect("restored tempdir must be created");

    let error = import_from_bytes_with_key(&encrypted, restored.path(), Some(&WRONG_KEY))
        .expect_err("encrypted import with the wrong key must fail");

    let message = error.to_string().to_lowercase();
    assert!(
        message.contains("authentication") || message.contains("aead"),
        "wrong-key error must name AEAD/authentication failure, got: {error}"
    );
    assert_directory_empty(restored.path());
}

#[test]
#[serial_test::serial]
fn encrypted_magic_does_not_collide_with_gzip() {
    assert_ne!(SNAPSHOT_ENCRYPTION_MAGIC[0], 0x1f);

    let source = source_fixture();
    let _env_restore = EnvVarRestore::unset();
    let plaintext = export_to_bytes(source.path()).expect("plaintext export");

    assert!(
        plaintext.starts_with(&[0x1f, 0x8b]),
        "plaintext snapshots must remain gzip streams"
    );

    let restored = TempDir::new().expect("restored tempdir must be created");
    import_from_bytes(&plaintext, restored.path()).expect("default-off plaintext import");
    assert_restored_matches_source(source.path(), restored.path());
}

#[test]
fn plaintext_import_succeeds_when_key_is_supplied() {
    let source = source_fixture();
    let plaintext =
        export_to_bytes_with_key(source.path(), None).expect("explicit plaintext export");
    let restored = TempDir::new().expect("restored tempdir must be created");

    import_from_bytes_with_key(&plaintext, restored.path(), Some(&TEST_KEY))
        .expect("plaintext import must ignore a configured encryption key");

    assert_restored_matches_source(source.path(), restored.path());
}

#[test]
fn encrypted_import_rejects_unknown_version_byte() {
    let source = source_fixture();
    let mut encrypted =
        export_to_bytes_with_key(source.path(), Some(&TEST_KEY)).expect("encrypted export");
    encrypted[SNAPSHOT_ENCRYPTION_MAGIC.len()] = 0xff;
    let sandbox = TempDir::new().expect("sandbox tempdir must be created");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside.txt");

    assert_encrypted_snapshot_import_rejected(
        &encrypted,
        &dest,
        &outside_path,
        ExpectedEncryptedImportRejection::MessageContains(
            "unsupported snapshot encryption version 255",
        ),
    );
}

#[test]
fn encrypted_import_rejects_parent_dir_traversal() {
    let sandbox = TempDir::new().expect("sandbox tempdir must be created");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside.txt");
    let plaintext = archive_with_patched_path("../outside.txt", b"escaped");
    let encrypted = encrypt_hostile_snapshot_fixture(&plaintext);

    assert_encrypted_snapshot_import_rejected(
        &encrypted,
        &dest,
        &outside_path,
        ExpectedEncryptedImportRejection::MessageContains(
            "snapshot entry path escapes destination",
        ),
    );
}

#[test]
fn encrypted_import_rejects_absolute_path_entries() {
    let sandbox = TempDir::new().expect("sandbox tempdir must be created");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside_abs.txt");
    let absolute_entry = outside_path.to_string_lossy().to_string();
    let plaintext = archive_with_patched_path(&absolute_entry, b"escaped");
    let encrypted = encrypt_hostile_snapshot_fixture(&plaintext);

    assert_encrypted_snapshot_import_rejected(
        &encrypted,
        &dest,
        &outside_path,
        ExpectedEncryptedImportRejection::MessageContains("snapshot entry path must be relative"),
    );
}

#[test]
fn encrypted_import_rejects_link_entries() {
    let sandbox = TempDir::new().expect("sandbox tempdir must be created");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("escaped_via_symlink.txt");
    let plaintext = archive_with_link(
        EntryType::Symlink,
        "pivot",
        "..",
        "pivot/escaped_via_symlink.txt",
    );
    let encrypted = encrypt_hostile_snapshot_fixture(&plaintext);

    assert_encrypted_snapshot_import_rejected(
        &encrypted,
        &dest,
        &outside_path,
        ExpectedEncryptedImportRejection::MessageContains(
            "snapshot archive contains unsupported link entry",
        ),
    );
}

#[test]
fn encrypted_import_rejects_truncated_entry_payload() {
    let sandbox = TempDir::new().expect("sandbox tempdir must be created");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside.txt");
    let plaintext = archive_with_truncated_entry_payload();
    let encrypted = encrypt_hostile_snapshot_fixture(&plaintext);

    assert_encrypted_snapshot_import_rejected(
        &encrypted,
        &dest,
        &outside_path,
        ExpectedEncryptedImportRejection::IoError,
    );
}

#[test]
fn encrypted_import_rejects_tampered_ciphertext() {
    let source = source_fixture();
    let mut encrypted =
        export_to_bytes_with_key(source.path(), Some(&TEST_KEY)).expect("encrypted export");
    encrypted[SNAPSHOT_ENCRYPTION_HEADER_LEN] ^= 0x01;
    let sandbox = TempDir::new().expect("sandbox tempdir must be created");
    let dest = sandbox.path().join("dest");
    let outside_path = sandbox.path().join("outside.txt");

    assert_encrypted_snapshot_import_rejected(
        &encrypted,
        &dest,
        &outside_path,
        ExpectedEncryptedImportRejection::MessageContains(
            "snapshot AEAD authentication/decryption failed",
        ),
    );
}

#[test]
#[serial_test::serial]
fn public_wrappers_round_trip_with_hex_key_file() {
    let key_dir = TempDir::new().expect("key tempdir must be created");
    let key_file = key_dir.path().join("snapshot.key");
    fs::write(&key_file, format!("{}\n", hex::encode(TEST_KEY)))
        .expect("hex key file must be written");
    let _env_restore = EnvVarRestore::set(&key_file);

    let source = source_fixture();
    let encrypted = export_to_bytes(source.path()).expect("wrapper encrypted export");
    assert!(
        encrypted.starts_with(SNAPSHOT_ENCRYPTION_MAGIC),
        "configured export wrapper must emit encrypted snapshot magic"
    );

    let restored = TempDir::new().expect("restored tempdir must be created");
    import_from_bytes(&encrypted, restored.path()).expect("wrapper encrypted import");
    assert_restored_matches_source(source.path(), restored.path());
}

#[test]
#[serial_test::serial]
fn public_wrappers_reject_malformed_key_file() {
    let key_dir = TempDir::new().expect("key tempdir must be created");
    let key_file = key_dir.path().join("snapshot.key");
    fs::write(&key_file, b"too-short\n").expect("malformed key file must be written");
    let _env_restore = EnvVarRestore::set(&key_file);

    let source = source_fixture();
    let export_error = export_to_bytes(source.path())
        .expect_err("wrapper export must fail when the configured key file is malformed");
    let export_message = export_error.to_string();
    assert!(
        export_message.contains(SNAPSHOT_KEY_FILE_ENV),
        "wrapper export error must name {SNAPSHOT_KEY_FILE_ENV}, got: {export_message}"
    );
    assert!(
        export_message.contains("64 hex characters") && export_message.contains("32 raw bytes"),
        "wrapper export error must preserve the accepted key-file forms, got: {export_message}"
    );

    let plaintext =
        export_to_bytes_with_key(source.path(), None).expect("explicit plaintext export");
    let restored = TempDir::new().expect("restored tempdir must be created");
    let import_error = import_from_bytes(&plaintext, restored.path())
        .expect_err("wrapper import must fail when the configured key file is malformed");
    let import_message = import_error.to_string();
    assert!(
        import_message.contains(SNAPSHOT_KEY_FILE_ENV),
        "wrapper import error must name {SNAPSHOT_KEY_FILE_ENV}, got: {import_message}"
    );
    assert!(
        import_message.contains("64 hex characters") && import_message.contains("32 raw bytes"),
        "wrapper import error must preserve the accepted key-file forms, got: {import_message}"
    );
    assert_directory_empty(restored.path());
}
