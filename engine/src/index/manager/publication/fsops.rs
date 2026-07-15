use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

const TRANSIENT_RENAME_RETRY_ATTEMPTS: usize = 21;
const TRANSIENT_RENAME_SLEEP: Duration = Duration::from_millis(10);

/// Rename a managed publication path with bounded retry for transient platform errors.
pub fn rename_with_transient_retry(from: &Path, to: &Path) -> io::Result<()> {
    let mut fs = StdPublicationFs;
    rename_with_transient_retry_using(&mut fs, from, to, RenameRetryPolicy::default())
}

/// Reject a managed endpoint when it currently resolves to a symlink.
pub fn reject_symlinked_managed_path(path: &Path, path_role: &str) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing symlinked {path_role} path '{}'", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("stat {path_role} path '{}' failed: {error}", path.display()),
        )),
    }
}

/// Reject a confined managed path when its root or any descendant component is a symlink.
pub(crate) fn reject_symlinked_managed_path_components(
    root: &Path,
    path: &Path,
    path_role: &str,
) -> io::Result<()> {
    let relative = path.strip_prefix(root).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "refusing {path_role} path '{}' outside managed root '{}'",
                path.display(),
                root.display()
            ),
        )
    })?;
    reject_symlinked_managed_path(root, path_role)?;
    let mut current = PathBuf::from(root);
    for component in relative.components() {
        current.push(component);
        reject_symlinked_managed_path(&current, path_role)?;
    }
    Ok(())
}

/// Durably sync the contents of a publication-owned file.
pub fn fsync_file(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

/// Durably sync a publication-owned directory entry namespace.
pub fn fsync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(test)]
fn sync_created_file(fs: &mut impl PublicationFs, path: &Path) -> io::Result<()> {
    fs.sync_file(path)?;
    sync_parent_dir(fs, path)
}

#[cfg(test)]
fn sync_renamed_path(fs: &mut impl PublicationFs, from: &Path, to: &Path) -> io::Result<()> {
    sync_parent_dir(fs, from)?;
    if from.parent() != to.parent() {
        sync_parent_dir(fs, to)?;
    }
    Ok(())
}

#[cfg(test)]
fn sync_removed_path(fs: &mut impl PublicationFs, path: &Path) -> io::Result<()> {
    sync_parent_dir(fs, path)
}

pub(crate) trait PublicationFs {
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()>;
    #[cfg(test)]
    fn sync_file(&mut self, path: &Path) -> io::Result<()>;
    #[cfg(test)]
    fn sync_dir(&mut self, path: &Path) -> io::Result<()>;
}

#[derive(Clone, Copy)]
pub(crate) struct RenameRetryPolicy {
    max_attempts: usize,
    sleep_between_attempts: Option<Duration>,
}

impl Default for RenameRetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: TRANSIENT_RENAME_RETRY_ATTEMPTS,
            sleep_between_attempts: Some(TRANSIENT_RENAME_SLEEP),
        }
    }
}

struct StdPublicationFs;

impl PublicationFs for StdPublicationFs {
    fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    #[cfg(test)]
    fn sync_file(&mut self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }

    #[cfg(test)]
    fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
        File::open(path)?.sync_all()
    }
}

pub(crate) fn rename_with_transient_retry_using(
    fs: &mut impl PublicationFs,
    from: &Path,
    to: &Path,
    policy: RenameRetryPolicy,
) -> io::Result<()> {
    let max_attempts = policy.max_attempts.max(1);
    for attempt in 1..=max_attempts {
        reject_symlinked_managed_path(from, "snapshot restore source")?;
        reject_symlinked_managed_path(to, "snapshot restore destination")?;
        match fs.rename(from, to) {
            Ok(()) => return Ok(()),
            Err(error) if is_transient_rename_error(&error) && attempt < max_attempts => {
                if let Some(delay) = policy.sleep_between_attempts {
                    thread::sleep(delay);
                }
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!("rename retry loop always returns from the final attempt")
}

fn is_transient_rename_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        // EBUSY = 16 (Linux/macOS), ENOTEMPTY = 39 (Linux) / 66 (macOS)
        Some(16) | Some(39) | Some(66)
    ) || matches!(
        error.kind(),
        io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::ResourceBusy
    )
}

#[cfg(test)]
fn sync_parent_dir(fs: &mut impl PublicationFs, path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    fs.sync_dir(parent.unwrap_or_else(|| Path::new(".")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    #[derive(Debug, PartialEq, Eq)]
    enum FsCall {
        Rename(PathBuf, PathBuf),
        SyncFile(PathBuf),
        SyncDir(PathBuf),
    }

    enum RenameOutcome {
        Ok,
        RawError(i32),
        Kind(io::ErrorKind),
    }

    struct RecordingFs {
        rename_outcomes: VecDeque<RenameOutcome>,
        calls: Vec<FsCall>,
    }

    impl RecordingFs {
        fn new(rename_outcomes: impl IntoIterator<Item = RenameOutcome>) -> Self {
            Self {
                rename_outcomes: rename_outcomes.into_iter().collect(),
                calls: Vec::new(),
            }
        }
    }

    impl PublicationFs for RecordingFs {
        fn rename(&mut self, from: &Path, to: &Path) -> io::Result<()> {
            self.calls
                .push(FsCall::Rename(from.to_path_buf(), to.to_path_buf()));
            match self
                .rename_outcomes
                .pop_front()
                .unwrap_or(RenameOutcome::Ok)
            {
                RenameOutcome::Ok => Ok(()),
                RenameOutcome::RawError(code) => Err(io::Error::from_raw_os_error(code)),
                RenameOutcome::Kind(kind) => Err(io::Error::from(kind)),
            }
        }

        fn sync_file(&mut self, path: &Path) -> io::Result<()> {
            self.calls.push(FsCall::SyncFile(path.to_path_buf()));
            Ok(())
        }

        fn sync_dir(&mut self, path: &Path) -> io::Result<()> {
            self.calls.push(FsCall::SyncDir(path.to_path_buf()));
            Ok(())
        }
    }

    fn retry_policy(max_attempts: usize) -> RenameRetryPolicy {
        RenameRetryPolicy {
            max_attempts,
            sleep_between_attempts: None,
        }
    }

    #[test]
    fn transient_rename_errors_are_retried_until_success() {
        let tmp = TempDir::new().unwrap();
        let from = tmp.path().join("from");
        let to = tmp.path().join("to");
        let mut fs = RecordingFs::new([
            RenameOutcome::RawError(16),
            RenameOutcome::RawError(66),
            RenameOutcome::Ok,
        ]);

        rename_with_transient_retry_using(&mut fs, &from, &to, retry_policy(4)).unwrap();

        assert_eq!(
            fs.calls,
            vec![
                FsCall::Rename(from.clone(), to.clone()),
                FsCall::Rename(from.clone(), to.clone()),
                FsCall::Rename(from, to),
            ]
        );
    }

    #[test]
    fn persistent_or_non_transient_rename_errors_are_returned() {
        let tmp = TempDir::new().unwrap();
        let from = tmp.path().join("from");
        let to = tmp.path().join("to");
        let mut persistent = RecordingFs::new([
            RenameOutcome::RawError(39),
            RenameOutcome::RawError(39),
            RenameOutcome::RawError(39),
        ]);

        let error = rename_with_transient_retry_using(&mut persistent, &from, &to, retry_policy(3))
            .unwrap_err();

        assert_eq!(error.raw_os_error(), Some(39));
        assert_eq!(persistent.calls.len(), 3);

        let mut non_transient =
            RecordingFs::new([RenameOutcome::Kind(io::ErrorKind::PermissionDenied)]);
        let error =
            rename_with_transient_retry_using(&mut non_transient, &from, &to, retry_policy(3))
                .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert_eq!(non_transient.calls.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn managed_rename_rejects_symlinked_source_or_destination_endpoints() {
        use std::os::unix::fs::symlink;

        let tmp = TempDir::new().unwrap();
        let real_source = tmp.path().join("real_source");
        let symlinked_source = tmp.path().join("symlinked_source");
        let destination = tmp.path().join("destination");
        std::fs::write(&real_source, b"source").unwrap();
        symlink(&real_source, &symlinked_source).unwrap();
        let mut fs = RecordingFs::new([RenameOutcome::Ok]);

        let error = rename_with_transient_retry_using(
            &mut fs,
            &symlinked_source,
            &destination,
            retry_policy(1),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("snapshot restore source"));
        assert!(fs.calls.is_empty());

        let real_destination = tmp.path().join("real_destination");
        let symlinked_destination = tmp.path().join("symlinked_destination");
        std::fs::write(&real_destination, b"destination").unwrap();
        symlink(&real_destination, &symlinked_destination).unwrap();
        let mut fs = RecordingFs::new([RenameOutcome::Ok]);

        let error = rename_with_transient_retry_using(
            &mut fs,
            &real_source,
            &symlinked_destination,
            retry_policy(1),
        )
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("snapshot restore destination"));
        assert!(fs.calls.is_empty());
    }

    #[test]
    fn durability_helpers_sync_the_precise_file_and_parent_directories() {
        let tmp = TempDir::new().unwrap();
        let created = tmp.path().join("tenant").join("meta.json");
        let from = tmp.path().join("backup").join("tenant_old");
        let to = tmp.path().join("live").join("tenant_new");
        let removed = tmp.path().join("tenant_new").join("stale.json");
        let mut fs = RecordingFs::new([]);

        sync_created_file(&mut fs, &created).unwrap();
        sync_renamed_path(&mut fs, &from, &to).unwrap();
        sync_removed_path(&mut fs, &removed).unwrap();

        assert_eq!(
            fs.calls,
            vec![
                FsCall::SyncFile(created.clone()),
                FsCall::SyncDir(created.parent().unwrap().to_path_buf()),
                FsCall::SyncDir(from.parent().unwrap().to_path_buf()),
                FsCall::SyncDir(to.parent().unwrap().to_path_buf()),
                FsCall::SyncDir(removed.parent().unwrap().to_path_buf()),
            ]
        );
    }
}
