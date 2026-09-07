//! Filesystem helpers for durable atomic writes and recursive directory copying.
use crate::error::{FlapjackError, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_NONCE: AtomicU64 = AtomicU64::new(0);

#[cfg(any(test, feature = "fault-injection"))]
static DIRECTORY_SYNC_FAULTS: once_cell::sync::Lazy<
    dashmap::DashMap<std::path::PathBuf, ArmedDirectorySyncFault>,
> = once_cell::sync::Lazy::new(dashmap::DashMap::new);

#[cfg(any(test, feature = "fault-injection"))]
struct ArmedDirectorySyncFault {
    point: DirectorySyncFaultPoint,
    successful_calls_before_failure: usize,
}

#[cfg(any(test, feature = "fault-injection"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DirectorySyncFaultPoint {
    Open,
    OpenNotFound,
    Sync,
}

#[cfg(any(test, feature = "fault-injection"))]
pub struct DirectorySyncFaultGuard {
    path: std::path::PathBuf,
}

#[cfg(any(test, feature = "fault-injection"))]
impl DirectorySyncFaultGuard {
    pub fn was_triggered(&self) -> bool {
        !DIRECTORY_SYNC_FAULTS.contains_key(&self.path)
    }
}

#[cfg(any(test, feature = "fault-injection"))]
impl Drop for DirectorySyncFaultGuard {
    fn drop(&mut self) {
        DIRECTORY_SYNC_FAULTS.remove(&self.path);
    }
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn fail_next_directory_sync_for_test(
    path: &Path,
    fault_point: DirectorySyncFaultPoint,
) -> DirectorySyncFaultGuard {
    fail_directory_sync_after_for_test(path, fault_point, 0)
}

#[cfg(any(test, feature = "fault-injection"))]
pub(crate) fn fail_directory_sync_after_for_test(
    path: &Path,
    fault_point: DirectorySyncFaultPoint,
    successful_calls_before_failure: usize,
) -> DirectorySyncFaultGuard {
    let path = path.to_path_buf();
    assert!(
        DIRECTORY_SYNC_FAULTS
            .insert(
                path.clone(),
                ArmedDirectorySyncFault {
                    point: fault_point,
                    successful_calls_before_failure,
                },
            )
            .is_none(),
        "a directory sync fault is already armed for {}",
        path.display()
    );
    DirectorySyncFaultGuard { path }
}

/// Return a recursively key-sorted JSON value for stable semantic hashing.
///
/// Array order and scalar representation remain significant; object insertion
/// order does not. Callers that persist or compare digests share this owner so
/// retries cannot disagree merely because a `HashMap` serialized differently.
pub(crate) fn canonicalize_json_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            entries.sort_unstable_by_key(|(key, _)| *key);
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), canonicalize_json_value(value)))
                    .collect(),
            )
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(canonicalize_json_value).collect())
        }
        _ => value.clone(),
    }
}

pub(crate) fn is_temporary_entry(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.starts_with(".tmp") || is_legacy_atomic_write_temp_name(name)
}

/// Names written by the pre-[`atomic_write`] call sites, which used `.tmp` as a
/// *suffix* instead of the prefix this module now emits. A binary that crashed
/// before the upgrade can still leave one of these on disk, so tree walks must
/// keep excluding them. Only formats that were actually written are listed:
/// the pause artifact never had a temp file (it was a plain in-place
/// `fs::write`), so it has no legacy name.
fn is_legacy_atomic_write_temp_name(name: &str) -> bool {
    name == ".index_meta.json.tmp"
        || name
            .strip_prefix(".committed_seq.")
            .is_some_and(|suffix| suffix.ends_with(".tmp"))
        || matches!(
            name,
            ".stopwords.tmp" | ".plurals.tmp" | ".compounds.tmp" | ".settings.tmp"
        )
        || name.ends_with(".json.tmp")
        || (name.contains(".parquet.") && name.ends_with(".tmp"))
}

pub(crate) fn atomic_write(path: &Path, payload: &[u8]) -> std::io::Result<()> {
    atomic_write_with_before_rename(path, payload, |_| {})
}

pub(crate) fn durable_remove_file(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => sync_parent_directory(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match sync_parent_directory(path) {
                Err(parent_error) if parent_error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        }
        Err(error) => Err(error),
    }
}

/// Create or validate a private directory and durably publish its parent entry.
///
/// A missing directory is created with mode `0700` on Unix and its parent is
/// synced so the new entry survives power loss. An existing real directory is
/// reused and its mode is restored to `0700`. Regular files and symbolic links
/// at `path` are rejected with `InvalidData`; a missing parent surfaces as
/// `NotFound` because creation is deliberately non-recursive.
pub(crate) fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "private state path is not a real directory: {}",
                    path.display()
                ),
            ));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            builder.create(path)?;
        }
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    sync_parent_directory(path)
}

/// Durably sync a directory's entry namespace after a create, rename, or unlink.
pub(crate) fn sync_directory(path: &Path) -> std::io::Result<()> {
    #[cfg(any(test, feature = "fault-injection"))]
    if consume_directory_sync_fault(path, DirectorySyncFaultPoint::Open) {
        return Err(std::io::Error::other("injected directory open failure"));
    }
    #[cfg(any(test, feature = "fault-injection"))]
    if consume_directory_sync_fault(path, DirectorySyncFaultPoint::OpenNotFound) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "injected directory disappeared before open",
        ));
    }
    let directory = File::open(path)?;
    #[cfg(any(test, feature = "fault-injection"))]
    if consume_directory_sync_fault(path, DirectorySyncFaultPoint::Sync) {
        return Err(std::io::Error::other("injected directory sync failure"));
    }
    directory.sync_all()
}

#[cfg(any(test, feature = "fault-injection"))]
fn consume_directory_sync_fault(path: &Path, fault_point: DirectorySyncFaultPoint) -> bool {
    let Some(mut armed) = DIRECTORY_SYNC_FAULTS.get_mut(path) else {
        return false;
    };
    if armed.point != fault_point {
        return false;
    }
    if armed.successful_calls_before_failure > 0 {
        armed.successful_calls_before_failure -= 1;
        return false;
    }
    drop(armed);
    let should_fail = true;
    if should_fail {
        DIRECTORY_SYNC_FAULTS.remove(path);
    }
    should_fail
}

/// Sync the directory that holds `path`; a path without a parent is `InvalidInput`.
pub(crate) fn sync_parent_directory(path: &Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("path has no parent directory: {}", path.display()),
        )
    })?;
    sync_directory(parent)
}

pub(crate) fn atomic_write_with_before_rename(
    path: &Path,
    payload: &[u8],
    before_rename: impl FnOnce(&Path),
) -> std::io::Result<()> {
    atomic_write_with(
        path,
        |file| file.write_all(payload),
        |temp_path| {
            before_rename(temp_path);
            Ok(())
        },
    )
}

/// Streaming variant of [`atomic_write`] for payloads too large to buffer, such
/// as parquet files. The caller writes directly into the temp file `utils`
/// supplies; `utils` still owns canonical temp naming, `sync_all`, atomic
/// rename, parent-directory fsync, and failed-write cleanup. The fallible hook
/// runs after the payload sync and immediately before the rename.
pub(crate) fn atomic_write_stream(
    path: &Path,
    write_payload: impl FnOnce(&mut File) -> std::io::Result<()>,
    before_rename: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    atomic_write_with(path, write_payload, before_rename)
}

fn atomic_write_with(
    path: &Path,
    write_payload: impl FnOnce(&mut File) -> std::io::Result<()>,
    before_rename: impl FnOnce(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("atomic-write target has no parent: {}", path.display()),
        )
    })?;
    let temp_path = atomic_write_temp_path(path);
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        write_payload(&mut file)?;
        file.sync_all()?;
        drop(file);
        before_rename(&temp_path)?;
        std::fs::rename(&temp_path, path)?;
        sync_directory(parent)
    })();

    if write_result.is_err() {
        let _ = std::fs::remove_file(&temp_path);
    }
    write_result
}

fn atomic_write_temp_path(path: &Path) -> std::path::PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let nonce = ATOMIC_WRITE_NONCE.fetch_add(1, Ordering::Relaxed);
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(
        ".tmp.{file_name}.{}.{}.{}.tmp",
        std::process::id(),
        timestamp,
        nonce
    ))
}

/// Recursively copy a directory tree from `src` to `dst`, skipping in-flight
/// atomic-write temporaries as classified by [`is_temporary_entry`].
///
/// Creates `dst` and any intermediate parent directories if they do not exist.
/// Files that vanish between directory listing and copy are silently skipped.
///
/// # Arguments
///
/// * `src` — Source directory to copy from. Must exist.
/// * `dst` — Destination directory. Created if it does not exist.
///
/// # Errors
///
/// Returns an error if `src` cannot be read, a source entry is a symbolic link,
/// a file copy fails, or directory creation fails.
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;

    let entries: Vec<_> = std::fs::read_dir(src)?.collect::<std::result::Result<Vec<_>, _>>()?;

    for entry in entries {
        let path = entry.path();
        let file_name = entry.file_name();
        let file_type = entry.file_type()?;

        if is_temporary_entry(&path) {
            continue;
        }

        if file_type.is_symlink() {
            return Err(FlapjackError::InvalidDocument(format!(
                "refusing to copy symbolic link: {}",
                path.display()
            )));
        }

        let dest_path = dst.join(file_name);

        if file_type.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
            continue;
        }
        if !path.exists() {
            continue;
        }
        std::fs::copy(&path, &dest_path)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(unix)]
    fn unix_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_unix_mode(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn ensure_private_directory_creates_then_reuses_a_real_directory() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join(".private-state");

        ensure_private_directory(&private).unwrap();
        assert!(private.is_dir());
        #[cfg(unix)]
        assert_eq!(unix_mode(&private), 0o700);

        fs::write(private.join("kept.json"), b"kept").unwrap();
        ensure_private_directory(&private).unwrap();
        assert_eq!(fs::read(private.join("kept.json")).unwrap(), b"kept");
        #[cfg(unix)]
        assert_eq!(unix_mode(&private), 0o700);
    }

    #[test]
    fn ensure_private_directory_rejects_a_regular_file_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join(".private-state");
        fs::write(&target, b"not a directory").unwrap();

        let error = ensure_private_directory(&target).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&target).unwrap(), b"not a directory");
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_rejects_a_symlink_target() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::create_dir(&real).unwrap();
        let link = dir.path().join(".private-state");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let error = ensure_private_directory(&link).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
    }

    #[test]
    fn ensure_private_directory_propagates_a_missing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing-parent").join(".private-state");

        let error = ensure_private_directory(&target).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!dir.path().join("missing-parent").exists());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_propagates_parent_open_failure_after_creation() {
        let dir = tempfile::tempdir().unwrap();
        let parent = dir.path().join("write-only-parent");
        fs::create_dir(&parent).unwrap();
        let target = parent.join(".private-state");
        set_unix_mode(&parent, 0o300);
        assert!(
            File::open(&parent).is_err(),
            "write-only parent open denial is unsupported on this host"
        );

        let result = ensure_private_directory(&target);
        set_unix_mode(&parent, 0o700);

        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied,
            "the parent-directory sync failure must reach the caller"
        );
        assert!(target.is_dir());
    }

    #[test]
    fn sync_directory_propagates_open_failure_for_a_missing_path() {
        let dir = tempfile::tempdir().unwrap();

        let error = sync_directory(&dir.path().join("missing")).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        sync_directory(dir.path()).unwrap();
    }

    #[test]
    fn sync_parent_directory_rejects_a_path_without_a_parent() {
        let error = sync_parent_directory(Path::new("/")).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn sync_parent_directory_syncs_the_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state.json");
        fs::write(&file, b"state").unwrap();

        sync_parent_directory(&file).unwrap();

        let error =
            sync_parent_directory(&dir.path().join("missing").join("state.json")).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn durable_remove_syncs_parent_after_unlink_and_on_missing_file_retry() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("resource.json");
        fs::write(&target, b"resource").unwrap();

        let unlink_fault =
            fail_next_directory_sync_for_test(dir.path(), DirectorySyncFaultPoint::Sync);
        let error = durable_remove_file(&target).unwrap_err();
        assert_eq!(error.to_string(), "injected directory sync failure");
        assert!(unlink_fault.was_triggered());
        assert!(!target.exists());

        let retry_fault =
            fail_next_directory_sync_for_test(dir.path(), DirectorySyncFaultPoint::Sync);
        let error = durable_remove_file(&target).unwrap_err();
        assert_eq!(error.to_string(), "injected directory sync failure");
        assert!(retry_fault.was_triggered());

        durable_remove_file(&target).unwrap();
    }

    #[test]
    fn durable_remove_missing_target_syncs_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("missing.json");
        let fault = fail_next_directory_sync_for_test(dir.path(), DirectorySyncFaultPoint::Open);

        let error = durable_remove_file(&target).unwrap_err();

        assert_eq!(error.to_string(), "injected directory open failure");
        assert!(fault.was_triggered());
    }

    #[test]
    fn durable_remove_missing_target_and_parent_is_a_noop() {
        let dir = tempfile::tempdir().unwrap();
        let missing_parent = dir.path().join("missing-parent");

        durable_remove_file(&missing_parent.join("missing.json")).unwrap();

        assert!(!missing_parent.exists());
    }

    #[test]
    fn durable_remove_propagates_non_not_found_unlink_error() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("directory-not-file");
        fs::create_dir(&target).unwrap();
        let expected_kind = fs::remove_file(&target).unwrap_err().kind();

        let error = durable_remove_file(&target).unwrap_err();

        assert_eq!(error.kind(), expected_kind);
        assert!(target.is_dir());
    }

    #[test]
    fn durable_remove_propagates_parent_open_and_sync_errors_after_unlink() {
        for (fault_point, expected_message) in [
            (
                DirectorySyncFaultPoint::Open,
                "injected directory open failure",
            ),
            (
                DirectorySyncFaultPoint::Sync,
                "injected directory sync failure",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("resource.json");
            fs::write(&target, b"resource").unwrap();
            let fault = fail_next_directory_sync_for_test(dir.path(), fault_point);

            let error = durable_remove_file(&target).unwrap_err();

            assert_eq!(error.to_string(), expected_message);
            assert!(fault.was_triggered());
            assert!(!target.exists());
        }
    }

    #[test]
    fn durable_remove_fails_closed_when_parent_disappears_after_unlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("resource.json");
        fs::write(&target, b"resource").unwrap();
        let fault =
            fail_next_directory_sync_for_test(dir.path(), DirectorySyncFaultPoint::OpenNotFound);

        let error = durable_remove_file(&target).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(fault.was_triggered());
        assert!(!target.exists());
    }

    #[test]
    fn atomic_write_propagates_directory_open_and_sync_failures() {
        for fault_point in [DirectorySyncFaultPoint::Open, DirectorySyncFaultPoint::Sync] {
            let dir = tempfile::tempdir().unwrap();
            let target = dir.path().join("durable.json");
            let fault = fail_next_directory_sync_for_test(dir.path(), fault_point);

            assert!(atomic_write(&target, b"new").is_err());
            assert!(fault.was_triggered());
            atomic_write(&target, b"retry").unwrap();
            assert_eq!(fs::read(&target).unwrap(), b"retry");
        }
    }

    #[test]
    fn copies_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("a.txt"), b"hello").unwrap();
        fs::write(src.join("b.txt"), b"world").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "hello");
        assert_eq!(fs::read_to_string(dst.join("b.txt")).unwrap(), "world");
    }

    #[test]
    fn copies_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir_all(src.join("sub/deep")).unwrap();
        fs::write(src.join("sub/deep/file.txt"), b"nested").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();
        assert_eq!(
            fs::read_to_string(dst.join("sub/deep/file.txt")).unwrap(),
            "nested"
        );
    }

    #[test]
    fn skips_tmp_files() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::write(src.join("keep.txt"), b"ok").unwrap();
        fs::write(src.join(".tmp_lock"), b"skip").unwrap();
        fs::write(src.join(".index_meta.json.tmp"), b"skip legacy").unwrap();

        copy_dir_recursive(&src, &dst).unwrap();
        assert!(dst.join("keep.txt").exists());
        assert!(!dst.join(".tmp_lock").exists());
        assert!(!dst.join(".index_meta.json.tmp").exists());
    }

    #[test]
    fn recognizes_legacy_atomic_write_temp_files() {
        for name in [".index_meta.json.tmp", ".committed_seq.42.99.tmp"] {
            assert!(
                is_temporary_entry(Path::new(name)),
                "{name} should stay excluded during atomic-write compatibility windows"
            );
        }
    }

    #[test]
    fn residual_writer_temp_names_are_excluded_from_snapshot_copy() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir(&src).unwrap();
        let temp_names = [
            ".stopwords.tmp",
            ".plurals.tmp",
            ".compounds.tmp",
            ".settings.tmp",
            "manifest.json.tmp",
            "rollup_1hour_123.parquet.456.tmp",
            "experiment-id.json.tmp",
            "_id_map.json.tmp",
        ];
        for name in temp_names {
            fs::write(src.join(name), b"in-flight").unwrap();
        }

        copy_dir_recursive(&src, &dst).unwrap();
        let unclassified: Vec<_> = temp_names
            .iter()
            .copied()
            .filter(|name| !is_temporary_entry(Path::new(name)))
            .collect();
        let copied: Vec<_> = temp_names
            .iter()
            .copied()
            .filter(|name| dst.join(name).exists())
            .collect();

        assert!(
            unclassified.is_empty() && copied.is_empty(),
            "residual temp names must be classified and omitted; unclassified={unclassified:?}, copied={copied:?}"
        );
    }

    #[test]
    fn atomic_write_replaces_contents_without_publishing_its_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        let mut observed_temp_path = None;

        atomic_write_with_before_rename(&path, b"new", |temp_path| {
            assert_eq!(fs::read(temp_path).unwrap(), b"new");
            assert!(is_temporary_entry(temp_path));
            observed_temp_path = Some(temp_path.to_path_buf());
        })
        .unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert!(!observed_temp_path.unwrap().exists());
    }

    #[test]
    fn atomic_write_cleans_up_after_payload_write_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        let error = atomic_write_with(
            &path,
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "injected payload write failure",
                ))
            },
            |_| Ok(()),
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::WriteZero);
        assert!(fs::read_dir(dir.path())
            .unwrap()
            .all(|entry| !is_temporary_entry(&entry.unwrap().path())));
    }

    #[test]
    fn atomic_write_preserves_live_file_when_publication_hook_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::write(&path, b"known-good").unwrap();

        let error = atomic_write_with(
            &path,
            |file| file.write_all(b"replacement"),
            |_| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected before-rename failure",
                ))
            },
        )
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert_eq!(fs::read(&path).unwrap(), b"known-good");
        assert!(fs::read_dir(dir.path())
            .unwrap()
            .all(|entry| !is_temporary_entry(&entry.unwrap().path())));
    }

    #[test]
    fn atomic_write_cleans_up_after_rename_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        atomic_write_with_before_rename(&path, b"new", |_| {
            fs::create_dir(&path).unwrap();
        })
        .unwrap_err();

        assert!(fs::read_dir(dir.path())
            .unwrap()
            .all(|entry| !is_temporary_entry(&entry.unwrap().path())));
    }

    #[test]
    fn empty_dir_ok() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let dst = dir.path().join("dst");
        fs::create_dir(&src).unwrap();

        copy_dir_recursive(&src, &dst).unwrap();
        assert!(dst.exists());
        assert!(fs::read_dir(&dst).unwrap().count() == 0);
    }

    #[test]
    fn nonexistent_source_errors() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("nope");
        let dst = dir.path().join("dst");

        assert!(copy_dir_recursive(&src, &dst).is_err());
    }
}
