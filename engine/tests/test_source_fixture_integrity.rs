use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs;
use std::path::Path;

const PINNED_FIXTURE_DIRS: &[(&str, usize)] = &[
    (
        "tests/fixtures/2026_07_26_m0a_meilisearch_source_contract",
        6,
    ),
    ("tests/fixtures/2026_07_26_m0b_typesense_migration", 3),
];

const MANIFEST_FILE: &str = "CHECKSUMS.txt";
const PRODUCER_FILE: &str = "PRODUCER.md";

#[test]
fn pinned_source_fixture_checksums_match_committed_files() {
    for &(fixture_dir, expected_entry_count) in PINNED_FIXTURE_DIRS {
        verify_fixture_manifest(fixture_dir, expected_entry_count)
            .unwrap_or_else(|err| panic!("{fixture_dir}: {err}"));
    }
}

fn verify_fixture_manifest(fixture_dir: &str, expected_entry_count: usize) -> Result<(), String> {
    let fixture_path = Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture_dir);
    verify_fixture_manifest_path(&fixture_path, expected_entry_count)
}

fn verify_fixture_manifest_path(
    fixture_path: &Path,
    expected_entry_count: usize,
) -> Result<(), String> {
    let manifest_path = fixture_path.join(MANIFEST_FILE);

    if !is_regular_file(&manifest_path) {
        return Err(format!(
            "missing checksum manifest {}",
            manifest_path.display()
        ));
    }
    let producer_path = fixture_path.join(PRODUCER_FILE);
    if !is_regular_file(&producer_path) {
        return Err(format!(
            "missing producer contract {}",
            producer_path.display()
        ));
    }

    let manifest = fs::read_to_string(&manifest_path).map_err(|err| {
        format!(
            "failed to read checksum manifest {}: {err}",
            manifest_path.display()
        )
    })?;
    let entries = parse_manifest(&manifest)?;
    if entries.len() != expected_entry_count {
        return Err(format!(
            "{MANIFEST_FILE} must list exactly {expected_entry_count} fixture files, found {}",
            entries.len()
        ));
    }

    let mut listed_files = HashSet::new();
    for entry in entries {
        let file_path = fixture_path.join(&entry.filename);
        if !file_path.exists() {
            return Err(format!(
                "manifest lists missing fixture file {}",
                file_path.display()
            ));
        }
        if !is_regular_file(&file_path) {
            return Err(format!(
                "manifest entry {} is not a regular file",
                entry.filename
            ));
        }

        let actual_digest = sha256_hex(&file_path)?;
        if actual_digest != entry.sha256 {
            return Err(format!(
                "checksum mismatch for {}: expected {}, got {}",
                entry.filename, entry.sha256, actual_digest
            ));
        }
        listed_files.insert(entry.filename);
    }

    for filename in fixture_filenames(fixture_path)? {
        if filename != MANIFEST_FILE
            && filename != PRODUCER_FILE
            && !listed_files.contains(&filename)
        {
            return Err(format!("{filename} is not listed in {MANIFEST_FILE}"));
        }
    }

    Ok(())
}

struct ManifestEntry {
    sha256: String,
    filename: String,
}

fn parse_manifest(manifest: &str) -> Result<Vec<ManifestEntry>, String> {
    let mut entries = Vec::new();
    let mut seen_files = HashSet::new();

    for (line_number, raw_line) in manifest.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }

        let (sha256, filename) = line.split_once("  ").ok_or_else(|| {
            format!(
                "line {} must use '<sha256>  <filename>' format",
                line_number + 1
            )
        })?;
        if sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(format!(
                "line {} has invalid sha256 digest '{}'",
                line_number + 1,
                sha256
            ));
        }
        if filename.is_empty()
            || filename.contains('/')
            || filename.contains('\\')
            || filename == MANIFEST_FILE
            || filename == PRODUCER_FILE
        {
            return Err(format!(
                "line {} has invalid fixture filename '{}'",
                line_number + 1,
                filename
            ));
        }
        if !seen_files.insert(filename.to_owned()) {
            return Err(format!(
                "line {} duplicates '{}'",
                line_number + 1,
                filename
            ));
        }
        if let Some(previous_filename) = entries.last().map(|entry: &ManifestEntry| &entry.filename)
        {
            if filename <= previous_filename {
                return Err(format!(
                    "line {} violates strictly increasing filename order: '{}' follows '{}'",
                    line_number + 1,
                    filename,
                    previous_filename
                ));
            }
        }

        entries.push(ManifestEntry {
            sha256: sha256.to_owned(),
            filename: filename.to_owned(),
        });
    }

    Ok(entries)
}

/// Every directory entry counts, including symlinks and subdirectories. Skipping
/// non-regular entries would exempt them from the "listed in the manifest" check,
/// so unpinned bytes could ship inside a pinned fixture directory unnoticed.
fn fixture_filenames(fixture_path: &Path) -> Result<Vec<String>, String> {
    let mut filenames = Vec::new();
    for entry in fs::read_dir(fixture_path)
        .map_err(|err| format!("failed to list {}: {err}", fixture_path.display()))?
    {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {err}"))?;
        filenames.push(entry.file_name().to_string_lossy().into_owned());
    }
    filenames.sort();
    Ok(filenames)
}

/// Checks for a regular file without following a final-component symlink, so a
/// pinned path cannot resolve to bytes stored outside the fixture directory.
fn is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_file())
}

fn sha256_hex(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

#[test]
fn parse_manifest_rejects_uppercase_digests() {
    let manifest =
        "A000000000000000000000000000000000000000000000000000000000000000  fixture.json\n";

    let error = parse_manifest(manifest)
        .err()
        .expect("uppercase digest must be rejected");

    assert!(error.contains("invalid sha256 digest"), "{error}");
}

#[test]
fn parse_manifest_rejects_out_of_order_filenames() {
    let digest = "0000000000000000000000000000000000000000000000000000000000000000";
    let manifest = format!("{digest}  z.json\n{digest}  a.json\n");

    let error = parse_manifest(&manifest)
        .err()
        .expect("out-of-order filenames must be rejected");

    assert!(
        error.contains("strictly increasing filename order"),
        "{error}"
    );
}

#[test]
fn verify_fixture_manifest_requires_producer_contract() {
    let temp_dir = tempfile::TempDir::new().expect("create temp fixture directory");
    let fixture_path = temp_dir.path();
    let payload = b"{\"ok\":true}\n";
    let digest = hex::encode(Sha256::digest(payload));

    fs::write(fixture_path.join("fixture.json"), payload).expect("write fixture payload");
    fs::write(
        fixture_path.join(MANIFEST_FILE),
        format!("{digest}  fixture.json\n"),
    )
    .expect("write checksum manifest");

    let error = verify_fixture_manifest_path(fixture_path, 1)
        .expect_err("missing producer contract must fail");

    assert!(error.contains("missing producer contract"), "{error}");
}

/// Writes a minimal pinned fixture directory holding one payload, its manifest,
/// and the producer contract. Returns the payload bytes and their digest so
/// callers can build symlink probes that still satisfy the checksum check.
#[cfg(unix)]
fn write_valid_fixture_dir(fixture_path: &Path) -> (&'static [u8], String) {
    let payload: &'static [u8] = b"{\"ok\":true}\n";
    let digest = hex::encode(Sha256::digest(payload));

    fs::write(fixture_path.join("fixture.json"), payload).expect("write fixture payload");
    fs::write(
        fixture_path.join(MANIFEST_FILE),
        format!("{digest}  fixture.json\n"),
    )
    .expect("write checksum manifest");
    fs::write(fixture_path.join(PRODUCER_FILE), "# Fixture producer\n")
        .expect("write producer contract");

    (payload, digest)
}

/// An unpinned payload smuggled in as a symlink must still trip the
/// completeness guard; otherwise the directory listing understates what ships.
#[cfg(unix)]
#[test]
fn verify_fixture_manifest_rejects_unlisted_symlink() {
    let temp_dir = tempfile::TempDir::new().expect("create temp fixture directory");
    let fixture_path = temp_dir.path();
    write_valid_fixture_dir(fixture_path);

    let outside_dir = tempfile::TempDir::new().expect("create temp directory outside the fixture");
    let outside_payload = outside_dir.path().join("outside_payload.json");
    fs::write(&outside_payload, b"{\"unpinned\":true}\n").expect("write outside payload");
    std::os::unix::fs::symlink(&outside_payload, fixture_path.join("smuggled.json"))
        .expect("create unlisted symlink");

    let error =
        verify_fixture_manifest_path(fixture_path, 1).expect_err("unlisted symlink must fail");

    assert!(error.contains("smuggled.json is not listed"), "{error}");
}

/// A manifest entry must resolve to a regular file inside the fixture
/// directory. A symlink digests bytes the repository does not actually hold.
#[cfg(unix)]
#[test]
fn verify_fixture_manifest_rejects_symlinked_manifest_entry() {
    let temp_dir = tempfile::TempDir::new().expect("create temp fixture directory");
    let fixture_path = temp_dir.path();
    let (payload, _digest) = write_valid_fixture_dir(fixture_path);

    let outside_dir = tempfile::TempDir::new().expect("create temp directory outside the fixture");
    let outside_payload = outside_dir.path().join("outside_target.json");
    fs::write(&outside_payload, payload).expect("write outside payload");
    fs::remove_file(fixture_path.join("fixture.json")).expect("replace payload with symlink");
    std::os::unix::fs::symlink(&outside_payload, fixture_path.join("fixture.json"))
        .expect("create symlinked manifest entry");

    let error = verify_fixture_manifest_path(fixture_path, 1)
        .expect_err("symlinked manifest entry must fail");

    assert!(error.contains("is not a regular file"), "{error}");
}

/// A subdirectory of unpinned payloads must trip the completeness guard: the
/// manifest format is flat, so nested files can never be pinned.
#[cfg(unix)]
#[test]
fn verify_fixture_manifest_rejects_unlisted_subdirectory() {
    let temp_dir = tempfile::TempDir::new().expect("create temp fixture directory");
    let fixture_path = temp_dir.path();
    write_valid_fixture_dir(fixture_path);

    fs::create_dir(fixture_path.join("nested")).expect("create nested directory");
    fs::write(fixture_path.join("nested/unpinned.json"), b"{}\n").expect("write nested payload");

    let error =
        verify_fixture_manifest_path(fixture_path, 1).expect_err("unlisted subdirectory must fail");

    assert!(error.contains("nested is not listed"), "{error}");
}
