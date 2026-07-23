use super::fsops::{
    fsync_dir, fsync_file, reject_symlinked_managed_path_components, rename_with_transient_retry,
};
use super::{PublicationPaths, PublicationTarget};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

const PUBLICATION_DIR: &str = ".publication";
const EPOCH_FILE: &str = "epoch";
const EPOCH_TEMP_FILE: &str = "epoch.tmp";
const EPOCH_LOCK_FILE: &str = "epoch.lock";
const MAX_CANONICAL_EPOCH_BYTES: u64 = u64::MAX.ilog10() as u64 + 1;

// `Serialize`/`Deserialize` let the publication journal reuse this epoch owner as
// `E_old`/`E_new` fence evidence; the durable epoch file itself stays plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationEpoch(pub u64);

#[derive(Debug)]
pub enum PublicationEpochError {
    CorruptState {
        path: PathBuf,
    },
    ExpectedMismatch {
        expected: PublicationEpoch,
        actual: PublicationEpoch,
    },
    Overflow {
        current: PublicationEpoch,
    },
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct PublicationEpochFence {
    _lock: File,
    previous: PublicationEpoch,
    advanced: PublicationEpoch,
}

impl PublicationEpochFence {
    pub fn previous(&self) -> PublicationEpoch {
        self.previous
    }

    pub fn advanced(&self) -> PublicationEpoch {
        self.advanced
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationEpochPaths {
    pub epoch: PathBuf,
    pub temp: PathBuf,
    pub lock: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PublicationEpochObservation {
    AbsentInitial,
    AbsentWithSidecars,
    Durable(PublicationEpoch),
}

impl PublicationEpochObservation {
    pub(super) fn value(self) -> PublicationEpoch {
        match self {
            Self::AbsentInitial | Self::AbsentWithSidecars => PublicationEpoch(0),
            Self::Durable(epoch) => epoch,
        }
    }

    pub(super) fn durable_epoch(self) -> Option<PublicationEpoch> {
        match self {
            Self::AbsentInitial | Self::AbsentWithSidecars => None,
            Self::Durable(epoch) => Some(epoch),
        }
    }

    pub(super) fn has_sidecar_residue(self) -> bool {
        matches!(self, Self::AbsentWithSidecars)
    }
}

pub fn publication_epoch_paths_for_target_path(target_path: &Path) -> PublicationEpochPaths {
    let base = target_path.parent().unwrap_or_else(|| Path::new(""));
    let target_name = target_path
        .file_name()
        .expect("publication target path includes validated target name");
    let namespace = base.join(PUBLICATION_DIR).join(target_name);
    PublicationEpochPaths {
        epoch: namespace.join(EPOCH_FILE),
        temp: namespace.join(EPOCH_TEMP_FILE),
        lock: namespace.join(EPOCH_LOCK_FILE),
    }
}

pub fn read_publication_epoch(
    base: &Path,
    target: &PublicationTarget,
) -> Result<PublicationEpoch, PublicationEpochError> {
    observe_publication_epoch(base, target).map(PublicationEpochObservation::value)
}

pub(super) fn observe_publication_epoch(
    base: &Path,
    target: &PublicationTarget,
) -> Result<PublicationEpochObservation, PublicationEpochError> {
    let paths = publication_epoch_paths(base, target);
    reject_epoch_managed_paths(base, &paths, &paths.epoch)?;
    let bytes = match read_bounded_epoch_bytes(&paths.epoch) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            return Ok(if epoch_sidecar_residue_exists(&paths) {
                PublicationEpochObservation::AbsentWithSidecars
            } else {
                PublicationEpochObservation::AbsentInitial
            });
        }
        Err(ReadEpochBytesError::CorruptState) => {
            return Err(PublicationEpochError::CorruptState { path: paths.epoch });
        }
        Err(ReadEpochBytesError::Io(source)) => {
            return Err(PublicationEpochError::Io {
                path: paths.epoch,
                source,
            })
        }
    };
    parse_epoch_bytes(&paths.epoch, &bytes).map(PublicationEpochObservation::Durable)
}

pub fn compare_and_advance_publication_epoch(
    base: &Path,
    target: &PublicationTarget,
    expected: PublicationEpoch,
) -> Result<PublicationEpochFence, PublicationEpochError> {
    let paths = publication_epoch_paths(base, target);
    let lock = acquire_epoch_lock(base, &paths)?;
    let previous = read_publication_epoch(base, target)?;
    if previous != expected {
        return Err(PublicationEpochError::ExpectedMismatch {
            expected,
            actual: previous,
        });
    }
    let advanced = previous
        .0
        .checked_add(1)
        .map(PublicationEpoch)
        .ok_or(PublicationEpochError::Overflow { current: previous })?;

    persist_epoch(base, &paths, advanced)?;
    let persisted = read_publication_epoch(base, target)?;
    if persisted != advanced {
        return Err(PublicationEpochError::CorruptState { path: paths.epoch });
    }

    Ok(PublicationEpochFence {
        _lock: lock,
        previous,
        advanced,
    })
}

impl PublicationPaths {
    pub fn epoch_path(&self) -> PathBuf {
        publication_epoch_paths_for_target_path(&self.target).epoch
    }

    pub fn epoch_temp_path(&self) -> PathBuf {
        publication_epoch_paths_for_target_path(&self.target).temp
    }

    pub fn epoch_lock_path(&self) -> PathBuf {
        publication_epoch_paths_for_target_path(&self.target).lock
    }
}

impl fmt::Display for PublicationEpochError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CorruptState { path } => {
                write!(
                    formatter,
                    "corrupt publication epoch state at {}",
                    path.display()
                )
            }
            Self::ExpectedMismatch { expected, actual } => write!(
                formatter,
                "publication epoch mismatch: expected {}, actual {}",
                expected.0, actual.0
            ),
            Self::Overflow { current } => {
                write!(
                    formatter,
                    "publication epoch {} cannot be advanced",
                    current.0
                )
            }
            Self::Io { path, source } => {
                write!(
                    formatter,
                    "publication epoch I/O failed at {}: {source}",
                    path.display()
                )
            }
        }
    }
}

impl std::error::Error for PublicationEpochError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

fn publication_epoch_paths(base: &Path, target: &PublicationTarget) -> PublicationEpochPaths {
    publication_epoch_paths_for_target_path(&base.join(target.as_str()))
}

enum ReadEpochBytesError {
    CorruptState,
    Io(io::Error),
}

fn read_bounded_epoch_bytes(path: &Path) -> Result<Option<Vec<u8>>, ReadEpochBytesError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(ReadEpochBytesError::Io(source)),
    };
    if metadata.len() > MAX_CANONICAL_EPOCH_BYTES {
        return Err(ReadEpochBytesError::CorruptState);
    }

    let file = File::open(path).map_err(ReadEpochBytesError::Io)?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(MAX_CANONICAL_EPOCH_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(ReadEpochBytesError::Io)?;
    if bytes.len() as u64 > MAX_CANONICAL_EPOCH_BYTES {
        return Err(ReadEpochBytesError::CorruptState);
    }
    Ok(Some(bytes))
}

fn parse_epoch_bytes(path: &Path, bytes: &[u8]) -> Result<PublicationEpoch, PublicationEpochError> {
    let raw = std::str::from_utf8(bytes).map_err(|_| PublicationEpochError::CorruptState {
        path: path.to_path_buf(),
    })?;
    if !is_canonical_epoch_encoding(raw) {
        return Err(PublicationEpochError::CorruptState {
            path: path.to_path_buf(),
        });
    }
    raw.parse::<u64>()
        .map(PublicationEpoch)
        .map_err(|_| PublicationEpochError::CorruptState {
            path: path.to_path_buf(),
        })
}

fn is_canonical_epoch_encoding(raw: &str) -> bool {
    if raw == "0" {
        return true;
    }
    raw.as_bytes()
        .first()
        .is_some_and(|first| matches!(first, b'1'..=b'9'))
        && raw.bytes().all(|byte| byte.is_ascii_digit())
}

fn acquire_epoch_lock(
    base: &Path,
    paths: &PublicationEpochPaths,
) -> Result<File, PublicationEpochError> {
    reject_epoch_managed_paths(base, paths, &paths.lock)?;
    if let Some(parent) = paths.lock.parent() {
        fs::create_dir_all(parent).map_err(|source| PublicationEpochError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    reject_epoch_managed_paths(base, paths, &paths.lock)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.lock)
        .map_err(|source| PublicationEpochError::Io {
            path: paths.lock.clone(),
            source,
        })?;
    file.lock().map_err(|source| PublicationEpochError::Io {
        path: paths.lock.clone(),
        source,
    })?;
    Ok(file)
}

fn persist_epoch(
    base: &Path,
    paths: &PublicationEpochPaths,
    epoch: PublicationEpoch,
) -> Result<(), PublicationEpochError> {
    reject_epoch_managed_paths(base, paths, &paths.temp)?;
    let parent = paths
        .epoch
        .parent()
        .ok_or_else(|| PublicationEpochError::Io {
            path: paths.epoch.clone(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "epoch path has no parent"),
        })?;
    fs::create_dir_all(parent).map_err(|source| PublicationEpochError::Io {
        path: parent.to_path_buf(),
        source,
    })?;

    write_epoch_temp(paths, epoch)?;
    fsync_file(&paths.temp).map_err(|source| PublicationEpochError::Io {
        path: paths.temp.clone(),
        source,
    })?;
    rename_with_transient_retry(&paths.temp, &paths.epoch).map_err(|source| {
        PublicationEpochError::Io {
            path: paths.epoch.clone(),
            source,
        }
    })?;
    fsync_dir(parent).map_err(|source| PublicationEpochError::Io {
        path: parent.to_path_buf(),
        source,
    })
}

fn write_epoch_temp(
    paths: &PublicationEpochPaths,
    epoch: PublicationEpoch,
) -> Result<(), PublicationEpochError> {
    let mut file = File::create(&paths.temp).map_err(|source| PublicationEpochError::Io {
        path: paths.temp.clone(),
        source,
    })?;
    write!(file, "{}", epoch.0).map_err(|source| PublicationEpochError::Io {
        path: paths.temp.clone(),
        source,
    })
}

fn reject_epoch_managed_paths(
    base: &Path,
    paths: &PublicationEpochPaths,
    active_path: &Path,
) -> Result<(), PublicationEpochError> {
    reject_symlinked_managed_path_components(base, active_path, "publication epoch").map_err(
        |source| PublicationEpochError::Io {
            path: active_path.to_path_buf(),
            source,
        },
    )?;
    for path in [&paths.epoch, &paths.temp, &paths.lock] {
        reject_symlinked_managed_path_components(base, path, "publication epoch").map_err(
            |source| PublicationEpochError::Io {
                path: active_path.to_path_buf(),
                source,
            },
        )?;
    }
    Ok(())
}

fn epoch_sidecar_residue_exists(paths: &PublicationEpochPaths) -> bool {
    paths.temp.exists() || paths.lock.exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::manager::publication::PublicationTransactionId;
    use std::fs;
    use std::io;
    use std::sync::{mpsc, Arc, Barrier};
    use std::time::Duration;
    use tempfile::TempDir;

    fn target(name: &str) -> PublicationTarget {
        PublicationTarget::new(name).unwrap()
    }

    fn transaction(id: &str) -> PublicationTransactionId {
        PublicationTransactionId::new(id).unwrap()
    }

    #[test]
    fn epoch_paths_are_per_target_and_transaction_independent() {
        let tmp = TempDir::new().unwrap();
        let products = target("products");
        let users = target("users");
        let first = PublicationPaths::new(tmp.path(), &products, &transaction("txn_001"));
        let second = PublicationPaths::new(tmp.path(), &products, &transaction("txn_002"));
        let other = PublicationPaths::new(tmp.path(), &users, &transaction("txn_001"));

        assert_eq!(
            first.epoch_path(),
            tmp.path().join(".publication/products/epoch")
        );
        assert_eq!(
            first.epoch_temp_path(),
            tmp.path().join(".publication/products/epoch.tmp")
        );
        assert_eq!(
            first.epoch_lock_path(),
            tmp.path().join(".publication/products/epoch.lock")
        );
        assert_eq!(first.epoch_path(), second.epoch_path());
        assert_eq!(first.epoch_temp_path(), second.epoch_temp_path());
        assert_eq!(first.epoch_lock_path(), second.epoch_lock_path());
        assert_ne!(first.epoch_path(), other.epoch_path());
        assert_ne!(first.epoch_lock_path(), other.epoch_lock_path());
    }

    #[test]
    fn missing_epoch_reads_as_initial_zero() {
        let tmp = TempDir::new().unwrap();

        assert_eq!(
            read_publication_epoch(tmp.path(), &target("products")).unwrap(),
            PublicationEpoch(0)
        );
    }

    #[test]
    fn missing_epoch_with_lock_residue_still_reads_zero_but_observes_sidecar_state() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.lock.parent().unwrap()).unwrap();
        fs::write(&paths.lock, b"").unwrap();

        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(0)
        );
        assert_eq!(
            observe_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpochObservation::AbsentWithSidecars
        );
    }

    #[test]
    fn epoch_reader_rejects_noncanonical_or_overflowing_state_with_typed_error() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.epoch.parent().unwrap()).unwrap();

        for bytes in [
            b"".as_slice(),
            b"-1".as_slice(),
            b" 1".as_slice(),
            b"1 ".as_slice(),
            b"1\n".as_slice(),
            b"01".as_slice(),
            b"1x".as_slice(),
            &[0xff, b'1'],
            b"18446744073709551616".as_slice(),
        ] {
            fs::write(&paths.epoch, bytes).unwrap();
            match read_publication_epoch(tmp.path(), &target) {
                Err(PublicationEpochError::CorruptState { path }) => {
                    assert_eq!(path, paths.epoch);
                }
                other => panic!("expected corrupt epoch state for {bytes:?}, got {other:?}"),
            }
        }

        fs::write(&paths.epoch, b"18446744073709551615").unwrap();
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(u64::MAX)
        );
    }

    #[cfg(unix)]
    #[test]
    fn epoch_reader_rejects_oversized_state_before_loading_contents() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.epoch.parent().unwrap()).unwrap();
        fs::write(&paths.epoch, b"184467440737095516150").unwrap();
        fs::set_permissions(&paths.epoch, fs::Permissions::from_mode(0o000)).unwrap();

        let result = read_publication_epoch(tmp.path(), &target);

        fs::set_permissions(&paths.epoch, fs::Permissions::from_mode(0o600)).unwrap();
        match result {
            Err(PublicationEpochError::CorruptState { path }) => {
                assert_eq!(path, paths.epoch);
            }
            other => {
                panic!("expected oversized epoch state to fail closed as corrupt, got {other:?}")
            }
        }
    }

    #[test]
    fn epoch_io_rejects_symlinked_managed_components_without_external_mutation() {
        let tmp = TempDir::new().unwrap();
        let external = TempDir::new().unwrap();
        fs::create_dir_all(tmp.path().join(".publication")).unwrap();
        symlink_dir(external.path(), tmp.path().join(".publication/products")).unwrap();

        match read_publication_epoch(tmp.path(), &target("products")) {
            Err(PublicationEpochError::Io { path, source }) => {
                assert_eq!(path, tmp.path().join(".publication/products/epoch"));
                assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
            }
            other => panic!("expected symlink rejection for epoch read, got {other:?}"),
        }

        match compare_and_advance_publication_epoch(
            tmp.path(),
            &target("products"),
            PublicationEpoch(0),
        ) {
            Err(PublicationEpochError::Io { path, source }) => {
                assert_eq!(path, tmp.path().join(".publication/products/epoch.lock"));
                assert_eq!(source.kind(), io::ErrorKind::InvalidInput);
            }
            other => panic!("expected symlink rejection for lock open, got {other:?}"),
        }

        assert!(!external.path().join("epoch").exists());
        assert!(!external.path().join("epoch.tmp").exists());
        assert!(!external.path().join("epoch.lock").exists());
    }

    #[test]
    fn epoch_advance_persists_monotonic_value_and_survives_reopen() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");

        let guard = compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(0))
            .unwrap();
        assert_eq!(guard.previous(), PublicationEpoch(0));
        assert_eq!(guard.advanced(), PublicationEpoch(1));
        drop(guard);

        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(1)
        );
        let guard = compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(1))
            .unwrap();
        assert_eq!(guard.previous(), PublicationEpoch(1));
        assert_eq!(guard.advanced(), PublicationEpoch(2));
    }

    #[test]
    fn epoch_advance_rejects_stale_expected_value_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        drop(compare_and_advance_publication_epoch(
            tmp.path(),
            &target,
            PublicationEpoch(0),
        ));

        match compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(0)) {
            Err(PublicationEpochError::ExpectedMismatch { expected, actual }) => {
                assert_eq!(expected, PublicationEpoch(0));
                assert_eq!(actual, PublicationEpoch(1));
            }
            other => panic!("expected stale epoch mismatch, got {other:?}"),
        }
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(1)
        );
    }

    #[test]
    fn concurrent_epoch_advances_have_exactly_one_winner() {
        let tmp = TempDir::new().unwrap();
        let base = Arc::new(tmp.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(2));
        let (tx, rx) = mpsc::channel();

        for _ in 0..2 {
            let base = Arc::clone(&base);
            let barrier = Arc::clone(&barrier);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let target = target("products");
                barrier.wait();
                tx.send(compare_and_advance_publication_epoch(
                    &base,
                    &target,
                    PublicationEpoch(0),
                ))
                .unwrap();
            });
        }
        drop(tx);

        let mut winners = 0;
        let mut stale = 0;
        for _ in 0..2 {
            let result = rx.recv().unwrap();
            match result {
                Ok(guard) => {
                    assert_eq!(guard.previous(), PublicationEpoch(0));
                    assert_eq!(guard.advanced(), PublicationEpoch(1));
                    winners += 1;
                    drop(guard);
                }
                Err(PublicationEpochError::ExpectedMismatch { expected, actual }) => {
                    assert_eq!(expected, PublicationEpoch(0));
                    assert_eq!(actual, PublicationEpoch(1));
                    stale += 1;
                }
                other => panic!("unexpected concurrent advance result: {other:?}"),
            }
        }
        assert_eq!(winners, 1);
        assert_eq!(stale, 1);
        assert_eq!(
            read_publication_epoch(&base, &target("products")).unwrap(),
            PublicationEpoch(1)
        );
    }

    #[test]
    fn epoch_fence_stays_exclusive_until_returned_guard_is_dropped() {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().to_path_buf();
        let products = target("products");
        let guard =
            compare_and_advance_publication_epoch(&base, &products, PublicationEpoch(0)).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let base_for_thread = base.clone();

        let handle = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let result = compare_and_advance_publication_epoch(
                &base_for_thread,
                &target("products"),
                PublicationEpoch(1),
            );
            done_tx.send(result.map(|guard| guard.advanced())).unwrap();
        });

        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(done_rx.recv_timeout(Duration::from_millis(100)).is_err());
        drop(guard);

        assert_eq!(
            done_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            PublicationEpoch(2)
        );
        handle.join().unwrap();
        assert_eq!(
            read_publication_epoch(&base, &target("products")).unwrap(),
            PublicationEpoch(2)
        );
    }

    #[test]
    fn epoch_advance_rejects_u64_overflow_without_mutation() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(paths.epoch.parent().unwrap()).unwrap();
        fs::write(&paths.epoch, u64::MAX.to_string()).unwrap();

        match compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(u64::MAX))
        {
            Err(PublicationEpochError::Overflow { current }) => {
                assert_eq!(current, PublicationEpoch(u64::MAX));
            }
            other => panic!("expected overflow rejection, got {other:?}"),
        }
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(u64::MAX)
        );
    }

    #[test]
    fn epoch_advance_io_failure_returns_typed_error_without_success() {
        let tmp = TempDir::new().unwrap();
        let target = target("products");
        let paths = publication_epoch_paths_for_target_path(&tmp.path().join("products"));
        fs::create_dir_all(&paths.temp).unwrap();

        match compare_and_advance_publication_epoch(tmp.path(), &target, PublicationEpoch(0)) {
            Err(PublicationEpochError::Io { path, source }) => {
                assert_eq!(path, paths.temp);
                assert!(matches!(
                    source.kind(),
                    io::ErrorKind::IsADirectory | io::ErrorKind::PermissionDenied
                ));
            }
            other => panic!("expected temp write I/O failure, got {other:?}"),
        }
        assert_eq!(
            read_publication_epoch(tmp.path(), &target).unwrap(),
            PublicationEpoch(0)
        );
    }

    #[cfg(unix)]
    fn symlink_dir(target: &Path, link: PathBuf) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    #[cfg(windows)]
    fn symlink_dir(target: &Path, link: PathBuf) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }
}
