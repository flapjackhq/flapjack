//! Filesystem helpers for durable atomic writes and recursive directory copying.
use crate::error::{FlapjackError, Result};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

static ATOMIC_WRITE_NONCE: AtomicU64 = AtomicU64::new(0);

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
        File::open(parent)?.sync_all()
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
