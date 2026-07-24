use super::fsops::{
    fsync_dir, fsync_file, reject_symlinked_managed_path_components, rename_with_transient_retry,
};
use super::{PublicationPaths, PublicationTarget};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::Arc;
use std::sync::{Mutex, OnceLock};

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
pub enum PublicationEpochAdmissionError {
    Busy,
    Stale {
        observed: PublicationEpoch,
        current: PublicationEpoch,
    },
    Epoch(PublicationEpochError),
}

#[derive(Debug)]
pub struct PublicationEpochFence {
    _lock: File,
    _pending_advance: PublicationEpochPendingAdvance,
    previous: PublicationEpoch,
    advanced: PublicationEpoch,
}

#[derive(Debug)]
pub struct PublicationEpochAdmissionGuard {
    _lock: File,
    observed: PublicationEpoch,
}

#[derive(Debug)]
struct PublicationEpochPendingAdvance {
    lock_path: PathBuf,
}

static PENDING_EPOCH_ADVANCES: OnceLock<Mutex<BTreeMap<PathBuf, usize>>> = OnceLock::new();

#[cfg(test)]
type PublicationEpochAdvanceCheckpointHook =
    Arc<dyn Fn(&PublicationTarget, PublicationEpoch) + Send + Sync + 'static>;

#[cfg(test)]
type PublicationEpochAdmissionLockCheckpointHook = Arc<dyn Fn(&Path) + Send + Sync + 'static>;

#[cfg(test)]
type PublicationEpochAdmissionPreLockCheckpointHook = Arc<dyn Fn(&Path) + Send + Sync + 'static>;

#[cfg(test)]
type PublicationEpochOpenLockFileCheckpointHook = Arc<dyn Fn(&Path) + Send + Sync + 'static>;

#[cfg(test)]
static PUBLICATION_EPOCH_ADVANCE_CHECKPOINT_HOOK: OnceLock<
    Mutex<Option<PublicationEpochAdvanceCheckpointHook>>,
> = OnceLock::new();

#[cfg(test)]
static PUBLICATION_EPOCH_ADMISSION_LOCK_CHECKPOINT_HOOK: OnceLock<
    Mutex<Option<PublicationEpochAdmissionLockCheckpointHook>>,
> = OnceLock::new();

#[cfg(test)]
static PUBLICATION_EPOCH_ADMISSION_PRE_LOCK_CHECKPOINT_HOOK: OnceLock<
    Mutex<Option<PublicationEpochAdmissionPreLockCheckpointHook>>,
> = OnceLock::new();

#[cfg(test)]
static PUBLICATION_EPOCH_OPEN_LOCK_FILE_CHECKPOINT_HOOK: OnceLock<
    Mutex<Option<PublicationEpochOpenLockFileCheckpointHook>>,
> = OnceLock::new();

#[cfg(test)]
pub(crate) struct PublicationEpochAdvanceCheckpointHookGuard {
    previous: Option<PublicationEpochAdvanceCheckpointHook>,
}

#[cfg(test)]
pub(crate) struct PublicationEpochAdmissionLockCheckpointHookGuard {
    previous: Option<PublicationEpochAdmissionLockCheckpointHook>,
}

#[cfg(test)]
pub(crate) struct PublicationEpochAdmissionPreLockCheckpointHookGuard {
    previous: Option<PublicationEpochAdmissionPreLockCheckpointHook>,
}

#[cfg(test)]
pub(crate) struct PublicationEpochOpenLockFileCheckpointHookGuard {
    previous: Option<PublicationEpochOpenLockFileCheckpointHook>,
}

#[cfg(test)]
impl Drop for PublicationEpochAdvanceCheckpointHookGuard {
    fn drop(&mut self) {
        *publication_epoch_advance_checkpoint_hook().lock().unwrap() = self.previous.take();
    }
}

#[cfg(test)]
impl Drop for PublicationEpochAdmissionLockCheckpointHookGuard {
    fn drop(&mut self) {
        *publication_epoch_admission_lock_checkpoint_hook()
            .lock()
            .unwrap() = self.previous.take();
    }
}

#[cfg(test)]
impl Drop for PublicationEpochAdmissionPreLockCheckpointHookGuard {
    fn drop(&mut self) {
        *publication_epoch_admission_pre_lock_checkpoint_hook()
            .lock()
            .unwrap() = self.previous.take();
    }
}

#[cfg(test)]
impl Drop for PublicationEpochOpenLockFileCheckpointHookGuard {
    fn drop(&mut self) {
        *publication_epoch_open_lock_file_checkpoint_hook()
            .lock()
            .unwrap() = self.previous.take();
    }
}

impl PublicationEpochAdmissionGuard {
    pub fn observed(&self) -> PublicationEpoch {
        self.observed
    }
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

pub(crate) fn capture_publication_epoch(
    base: &Path,
    target: &PublicationTarget,
) -> Result<PublicationEpoch, PublicationEpochAdmissionError> {
    read_publication_epoch(base, target).map_err(PublicationEpochAdmissionError::Epoch)
}

pub(crate) fn try_validate_publication_epoch_admission(
    base: &Path,
    target: &PublicationTarget,
    observed: PublicationEpoch,
) -> Result<PublicationEpochAdmissionGuard, PublicationEpochAdmissionError> {
    let paths = publication_epoch_paths(base, target);
    let lock = try_acquire_epoch_admission_lock(base, &paths)?;
    let current =
        read_publication_epoch(base, target).map_err(PublicationEpochAdmissionError::Epoch)?;
    if current != observed {
        return Err(PublicationEpochAdmissionError::Stale { observed, current });
    }
    Ok(PublicationEpochAdmissionGuard {
        _lock: lock,
        observed,
    })
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
    let pending_advance = PublicationEpochPendingAdvance::register(paths.lock.clone());
    run_publication_epoch_advance_checkpoint_for_test(target, expected);
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
        _pending_advance: pending_advance,
        previous,
        advanced,
    })
}

#[cfg(test)]
pub(crate) fn set_publication_epoch_advance_checkpoint_hook_for_test(
    hook: impl Fn(&PublicationTarget, PublicationEpoch) + Send + Sync + 'static,
) -> PublicationEpochAdvanceCheckpointHookGuard {
    let mut slot = publication_epoch_advance_checkpoint_hook().lock().unwrap();
    PublicationEpochAdvanceCheckpointHookGuard {
        previous: slot.replace(Arc::new(hook)),
    }
}

#[cfg(test)]
pub(crate) fn set_publication_epoch_admission_lock_checkpoint_hook_for_test(
    hook: impl Fn(&Path) + Send + Sync + 'static,
) -> PublicationEpochAdmissionLockCheckpointHookGuard {
    let mut slot = publication_epoch_admission_lock_checkpoint_hook()
        .lock()
        .unwrap();
    PublicationEpochAdmissionLockCheckpointHookGuard {
        previous: slot.replace(Arc::new(hook)),
    }
}

#[cfg(test)]
pub(crate) fn set_publication_epoch_admission_pre_lock_checkpoint_hook_for_test(
    hook: impl Fn(&Path) + Send + Sync + 'static,
) -> PublicationEpochAdmissionPreLockCheckpointHookGuard {
    let mut slot = publication_epoch_admission_pre_lock_checkpoint_hook()
        .lock()
        .unwrap();
    PublicationEpochAdmissionPreLockCheckpointHookGuard {
        previous: slot.replace(Arc::new(hook)),
    }
}

#[cfg(test)]
pub(crate) fn set_publication_epoch_open_lock_file_checkpoint_hook_for_test(
    hook: impl Fn(&Path) + Send + Sync + 'static,
) -> PublicationEpochOpenLockFileCheckpointHookGuard {
    let mut slot = publication_epoch_open_lock_file_checkpoint_hook()
        .lock()
        .unwrap();
    PublicationEpochOpenLockFileCheckpointHookGuard {
        previous: slot.replace(Arc::new(hook)),
    }
}

#[cfg(test)]
fn publication_epoch_advance_checkpoint_hook(
) -> &'static Mutex<Option<PublicationEpochAdvanceCheckpointHook>> {
    PUBLICATION_EPOCH_ADVANCE_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn publication_epoch_admission_lock_checkpoint_hook(
) -> &'static Mutex<Option<PublicationEpochAdmissionLockCheckpointHook>> {
    PUBLICATION_EPOCH_ADMISSION_LOCK_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn publication_epoch_admission_pre_lock_checkpoint_hook(
) -> &'static Mutex<Option<PublicationEpochAdmissionPreLockCheckpointHook>> {
    PUBLICATION_EPOCH_ADMISSION_PRE_LOCK_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn publication_epoch_open_lock_file_checkpoint_hook(
) -> &'static Mutex<Option<PublicationEpochOpenLockFileCheckpointHook>> {
    PUBLICATION_EPOCH_OPEN_LOCK_FILE_CHECKPOINT_HOOK.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn run_publication_epoch_advance_checkpoint_for_test(
    target: &PublicationTarget,
    expected: PublicationEpoch,
) {
    let hook = publication_epoch_advance_checkpoint_hook()
        .lock()
        .unwrap()
        .clone();
    if let Some(hook) = hook {
        hook(target, expected);
    }
}

#[cfg(test)]
fn run_publication_epoch_admission_lock_checkpoint_for_test(lock_path: &Path) {
    let hook = publication_epoch_admission_lock_checkpoint_hook()
        .lock()
        .unwrap()
        .clone();
    if let Some(hook) = hook {
        hook(lock_path);
    }
}

#[cfg(test)]
fn run_publication_epoch_admission_pre_lock_checkpoint_for_test(lock_path: &Path) {
    let hook = publication_epoch_admission_pre_lock_checkpoint_hook()
        .lock()
        .unwrap()
        .clone();
    if let Some(hook) = hook {
        hook(lock_path);
    }
}

#[cfg(test)]
fn run_publication_epoch_open_lock_file_checkpoint_for_test(lock_path: &Path) {
    let hook = publication_epoch_open_lock_file_checkpoint_hook()
        .lock()
        .unwrap()
        .clone();
    if let Some(hook) = hook {
        hook(lock_path);
    }
}

#[cfg(not(test))]
fn run_publication_epoch_advance_checkpoint_for_test(
    _target: &PublicationTarget,
    _expected: PublicationEpoch,
) {
}

#[cfg(not(test))]
fn run_publication_epoch_admission_lock_checkpoint_for_test(_lock_path: &Path) {}

#[cfg(not(test))]
fn run_publication_epoch_admission_pre_lock_checkpoint_for_test(_lock_path: &Path) {}

#[cfg(not(test))]
fn run_publication_epoch_open_lock_file_checkpoint_for_test(_lock_path: &Path) {}

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
    let file = open_epoch_lock_file(base, paths)?;
    file.lock().map_err(|source| PublicationEpochError::Io {
        path: paths.lock.clone(),
        source,
    })?;
    Ok(file)
}

fn open_epoch_lock_file(
    base: &Path,
    paths: &PublicationEpochPaths,
) -> Result<File, PublicationEpochError> {
    reject_epoch_managed_paths(base, paths, &paths.lock)?;
    run_publication_epoch_open_lock_file_checkpoint_for_test(&paths.lock);
    if let Some(parent) = paths.lock.parent() {
        fs::create_dir_all(parent).map_err(|source| PublicationEpochError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    reject_epoch_managed_paths(base, paths, &paths.lock)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.lock)
        .map_err(|source| PublicationEpochError::Io {
            path: paths.lock.clone(),
            source,
        })
}

fn try_acquire_epoch_admission_lock(
    base: &Path,
    paths: &PublicationEpochPaths,
) -> Result<File, PublicationEpochAdmissionError> {
    let file = open_epoch_lock_file(base, paths).map_err(PublicationEpochAdmissionError::Epoch)?;
    let pending_advances = pending_epoch_advances().lock().unwrap();
    if pending_advances.contains_key(&paths.lock) {
        return Err(PublicationEpochAdmissionError::Busy);
    }
    run_publication_epoch_admission_pre_lock_checkpoint_for_test(&paths.lock);
    file.try_lock_shared().map_err(|source| match source {
        std::fs::TryLockError::WouldBlock => PublicationEpochAdmissionError::Busy,
        std::fs::TryLockError::Error(source) => {
            PublicationEpochAdmissionError::Epoch(PublicationEpochError::Io {
                path: paths.lock.clone(),
                source,
            })
        }
    })?;
    drop(pending_advances);
    run_publication_epoch_admission_lock_checkpoint_for_test(&paths.lock);
    Ok(file)
}

impl PublicationEpochPendingAdvance {
    fn register(lock_path: PathBuf) -> Self {
        let mut pending = pending_epoch_advances().lock().unwrap();
        *pending.entry(lock_path.clone()).or_insert(0) += 1;
        Self { lock_path }
    }
}

impl Drop for PublicationEpochPendingAdvance {
    fn drop(&mut self) {
        let mut pending = pending_epoch_advances().lock().unwrap();
        let Some(count) = pending.get_mut(&self.lock_path) else {
            return;
        };
        *count -= 1;
        if *count == 0 {
            pending.remove(&self.lock_path);
        }
    }
}

fn pending_epoch_advances() -> &'static Mutex<BTreeMap<PathBuf, usize>> {
    PENDING_EPOCH_ADVANCES.get_or_init(|| Mutex::new(BTreeMap::new()))
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
