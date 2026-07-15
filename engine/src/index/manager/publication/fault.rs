use super::fsops::{fsync_dir, fsync_file, rename_with_transient_retry};
use super::{invalid_publication, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Deterministic publication checkpoint used by the shared fault hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PublicationFaultPoint {
    #[cfg(test)]
    NoFault,
    BeforeStagingDigest,
    DuringStagingSync,
    DuringPrepareJournalWrite,
    AfterPrepareJournalRename,
    AfterPrepareJournal,
    AfterTargetBackup,
    AfterRepairTargetRename,
    AfterStagingPromote,
    BeforeCommitJournal,
    DuringCommitJournalWrite,
    AfterCommitJournalRename,
    CommitDurable,
    AfterCommitJournal,
    #[cfg(test)]
    BeforeSourceCleanup,
    DuringRollbackJournalWrite,
    AfterRollbackJournalRename,
    AfterRollbackJournal,
    DuringCleanup,
    DuringQuarantine,
}

pub(crate) type PublicationCheckpoint = PublicationFaultPoint;

/// One observable publication filesystem operation or state-machine checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PublicationOperation {
    Checkpoint(PublicationCheckpoint),
    CreateDirectory(PathBuf),
    Digest(PathBuf),
    WriteFile(PathBuf),
    CopyFile { from: PathBuf, to: PathBuf },
    SyncFile(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
    SyncDirectory(PathBuf),
    Remove(PathBuf),
}

pub(crate) trait PublicationFaultHook {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()>;
}

struct NoPublicationFaults;

impl PublicationFaultHook for NoPublicationFaults {
    fn before_operation(&self, _operation: &PublicationOperation) -> Result<()> {
        Ok(())
    }
}

static NO_PUBLICATION_FAULTS: NoPublicationFaults = NoPublicationFaults;

pub(super) struct PublicationIo<'a> {
    faults: &'a dyn PublicationFaultHook,
}

impl PublicationIo<'static> {
    pub(super) fn production() -> Self {
        Self {
            faults: &NO_PUBLICATION_FAULTS,
        }
    }
}

impl<'a> PublicationIo<'a> {
    #[cfg(test)]
    pub(super) fn with_faults(faults: &'a dyn PublicationFaultHook) -> Self {
        Self { faults }
    }

    pub(super) fn checkpoint(&self, checkpoint: PublicationCheckpoint) -> Result<()> {
        self.check(PublicationOperation::Checkpoint(checkpoint))
    }

    pub(super) fn before_digest(&self, path: &Path) -> Result<()> {
        self.check(PublicationOperation::Digest(path.to_path_buf()))
    }

    pub(super) fn create_dir_all(&self, path: &Path) -> Result<()> {
        self.check(PublicationOperation::CreateDirectory(path.to_path_buf()))?;
        fs::create_dir_all(path)?;
        Ok(())
    }

    pub(super) fn write_file(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        self.check(PublicationOperation::WriteFile(path.to_path_buf()))?;
        fs::write(path, bytes)?;
        Ok(())
    }

    pub(super) fn copy_file(&self, from: &Path, to: &Path) -> Result<()> {
        self.check(PublicationOperation::CopyFile {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        })?;
        fs::copy(from, to)?;
        Ok(())
    }

    pub(super) fn sync_file(&self, path: &Path) -> Result<()> {
        self.check(PublicationOperation::SyncFile(path.to_path_buf()))?;
        fsync_file(path)?;
        Ok(())
    }

    pub(super) fn sync_dir(&self, path: &Path) -> Result<()> {
        self.check(PublicationOperation::SyncDirectory(path.to_path_buf()))?;
        fsync_dir(path)?;
        Ok(())
    }

    pub(super) fn rename(&self, from: &Path, to: &Path) -> Result<()> {
        if let Some(parent) = to.parent() {
            self.create_dir_all(parent)?;
        }
        self.check(PublicationOperation::Rename {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        })?;
        rename_with_transient_retry(from, to)?;
        self.sync_dir(require_parent(from)?)?;
        let to_parent = require_parent(to)?;
        if require_parent(from)? != to_parent {
            self.sync_dir(to_parent)?;
        }
        Ok(())
    }

    pub(super) fn remove_if_exists(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }
        self.check(PublicationOperation::Remove(path.to_path_buf()))?;
        let metadata = fs::symlink_metadata(path)?;
        if metadata.is_dir() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
        self.sync_dir(require_parent(path)?)
    }

    fn check(&self, operation: PublicationOperation) -> Result<()> {
        self.faults.before_operation(&operation)
    }
}

#[cfg(test)]
pub(super) struct CheckpointFaultHook {
    fault: PublicationFaultPoint,
}

#[cfg(test)]
impl CheckpointFaultHook {
    pub(super) fn new(fault: PublicationFaultPoint) -> Self {
        Self { fault }
    }
}

#[cfg(test)]
impl PublicationFaultHook for CheckpointFaultHook {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()> {
        let PublicationOperation::Checkpoint(actual) = operation else {
            return Ok(());
        };
        let should_fail = *actual == self.fault
            || matches!(
                (self.fault, *actual),
                (
                    PublicationFaultPoint::AfterRollbackJournal,
                    PublicationFaultPoint::AfterStagingPromote
                ) | (
                    PublicationFaultPoint::DuringQuarantine,
                    PublicationFaultPoint::AfterStagingPromote
                        | PublicationFaultPoint::AfterRollbackJournal
                )
            );
        if should_fail && self.fault != PublicationFaultPoint::NoFault {
            Err(injected_fault(operation))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
pub(crate) struct PublicationFaultScript {
    fail_at: std::collections::BTreeSet<usize>,
    operations: std::cell::RefCell<Vec<PublicationOperation>>,
}

#[cfg(test)]
impl PublicationFaultScript {
    pub(crate) fn recording() -> Self {
        Self {
            fail_at: std::collections::BTreeSet::new(),
            operations: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn failing_at(operation_index: usize) -> Self {
        Self {
            fail_at: [operation_index].into_iter().collect(),
            operations: std::cell::RefCell::new(Vec::new()),
        }
    }

    pub(crate) fn operations(&self) -> Vec<PublicationOperation> {
        self.operations.borrow().clone()
    }
}

#[cfg(test)]
impl PublicationFaultHook for PublicationFaultScript {
    fn before_operation(&self, operation: &PublicationOperation) -> Result<()> {
        let operation_index = self.operations.borrow().len();
        self.operations.borrow_mut().push(operation.clone());
        if self.fail_at.contains(&operation_index) {
            Err(injected_fault(operation))
        } else {
            Ok(())
        }
    }
}

fn require_parent(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| invalid_publication(format!("path '{}' has no parent", path.display())))
}

#[cfg(test)]
fn injected_fault(operation: &PublicationOperation) -> crate::error::FlapjackError {
    invalid_publication(format!("injected publication fault before {operation:?}"))
}
