//! Durable handoff from an immutable publication to a mutable serving tree.
use super::epoch::PublicationEpochObservation;
use super::fsops::reject_symlinked_managed_path_components;
use super::{
    atomic_write_json, canonical_tenant_tree_digest, invalid_publication,
    publication_admission_is_fenced, read_publication_epoch, relative_path_evidence,
    PublicationDisposition, PublicationJobHandoff, PublicationJournal, PublicationPaths,
    PublicationPhase, PublicationTombstone, Result, TantivyManagedInventory,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

const ADOPTION_FILE: &str = "runtime-adoption.json";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RuntimePublicationAdoption {
    journal_sha256: String,
    epoch: u64,
    publication: PublicationTombstone,
}

pub(crate) fn adopt_snapshot_publication(base: &Path, journal: &PublicationJournal) -> Result<()> {
    adopt_runtime_publication(base, journal, validate_snapshot_journal)
}

pub(crate) fn adopt_fenced_publication(base: &Path, journal: &PublicationJournal) -> Result<()> {
    adopt_runtime_publication(base, journal, validate_fenced_publication_journal)
}

fn adopt_runtime_publication(
    base: &Path,
    journal: &PublicationJournal,
    validate_journal: fn(&PublicationJournal) -> Result<()>,
) -> Result<()> {
    if !publication_admission_is_fenced(base, &journal.target) {
        return Err(invalid_publication(
            "runtime adoption requires the publication fence",
        ));
    }
    let paths = PublicationPaths::new(base, &journal.target, &journal.transaction_id);
    validate_clean_paths(base, &paths)?;
    let raw = std::fs::read(&paths.journal)?;
    let persisted = PublicationJournal::from_recovery_json(
        std::str::from_utf8(&raw)
            .map_err(|_| invalid_publication("publication journal is not UTF-8"))?,
    )?;
    validate_journal(&persisted)?;
    if persisted.to_json_value() != journal.to_json_value() {
        return Err(invalid_publication(
            "runtime adoption journal changed after activation",
        ));
    }
    if serving_tree_digest(&paths.target)? != journal.digest {
        return Err(invalid_publication(
            "runtime adoption target changed before handoff",
        ));
    }
    let adoption = RuntimePublicationAdoption {
        journal_sha256: hex::encode(Sha256::digest(&raw)),
        epoch: read_publication_epoch(base, &journal.target)
            .map_err(|error| invalid_publication(error.to_string()))?
            .0,
        publication: publication_tombstone(journal)?,
    };
    atomic_write_json(&adoption_path(&paths), &adoption)
}

pub(super) fn adopt_repaired_publication(base: &Path, paths: &PublicationPaths) -> Result<()> {
    let raw = match std::fs::read_to_string(&paths.journal) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let journal = PublicationJournal::from_recovery_json(&raw)?;
    if validate_runtime_publication_journal(&journal).is_ok() {
        // Repair has proved the original commit bytes before this handoff. Never
        // adopt an unavailable tree merely because its journal says committed.
        adopt_runtime_publication(base, &journal, validate_runtime_publication_journal)?;
    }
    Ok(())
}

pub(super) fn publication_runtime_is_adopted(
    base: &Path,
    paths: &PublicationPaths,
    epoch: PublicationEpochObservation,
) -> Result<bool> {
    let marker = adoption_path(paths);
    reject_symlinked_managed_path_components(base, &marker, "publication runtime adoption")?;
    let raw = match std::fs::read(&marker) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let adoption: RuntimePublicationAdoption = serde_json::from_slice(&raw)?;
    validate_clean_paths(base, paths)?;
    let raw_journal = std::fs::read(&paths.journal)?;
    let journal = PublicationJournal::from_recovery_json(
        std::str::from_utf8(&raw_journal)
            .map_err(|_| invalid_publication("publication journal is not UTF-8"))?,
    )?;
    validate_runtime_publication_journal(&journal)?;
    if &PublicationPaths::new(base, &journal.target, &journal.transaction_id) != paths
        || adoption.journal_sha256 != hex::encode(Sha256::digest(&raw_journal))
        || adoption.publication != publication_tombstone(&journal)?
        || adoption.epoch != epoch.value().0
    {
        return Err(invalid_publication(
            "runtime adoption does not match publication evidence",
        ));
    }
    // Runtime writes legitimately change the committed tree digest. Still walk
    // the entire managed tree so adoption cannot authorize unknown or symlinked
    // artifacts. The immutable journal remains the publication's native proof.
    serving_tree_digest(&paths.target)?;
    Ok(true)
}

fn serving_tree_digest(target: &Path) -> Result<super::ContentDigest> {
    // Reject symlinked files before Tantivy reads its own managed-file manifest.
    // Files merely found on disk are not proof of Tantivy ownership.
    TantivyManagedInventory::from_existing_trees([target])?;
    let index = tantivy::Index::open_in_dir(target)?;
    let mut managed_files = index.directory().list_managed_files();
    // Tantivy excludes its own manifest and lock files from the managed set.
    managed_files.extend([
        PathBuf::from(".managed.json"),
        tantivy::directory::META_LOCK.filepath.clone(),
        tantivy::directory::INDEX_WRITER_LOCK.filepath.clone(),
    ]);
    let inventory = TantivyManagedInventory::new(managed_files)?;
    canonical_tenant_tree_digest(target, &inventory)
}

fn validate_snapshot_journal(journal: &PublicationJournal) -> Result<()> {
    if journal.phase != PublicationPhase::Committed
        || journal.disposition != Some(PublicationDisposition::Committed)
        || journal.fence_evidence.is_some()
        || !journal.artifact_manifest.entries.is_empty()
        || !journal
            .transaction_id
            .as_str()
            .starts_with(super::SNAPSHOT_TRANSACTION_PREFIX)
        || journal.paths != relative_path_evidence(&journal.target, &journal.transaction_id)
    {
        return Err(invalid_publication(
            "runtime adoption requires a committed native snapshot",
        ));
    }
    Ok(())
}

fn validate_fenced_publication_journal(journal: &PublicationJournal) -> Result<()> {
    if journal.phase != PublicationPhase::Committed
        || journal.disposition != Some(PublicationDisposition::Committed)
        || journal.fence_evidence.is_none()
        || !journal.artifact_manifest.entries.is_empty()
        || journal.paths != relative_path_evidence(&journal.target, &journal.transaction_id)
    {
        return Err(invalid_publication(
            "runtime adoption requires a committed fenced publication",
        ));
    }
    Ok(())
}

fn validate_runtime_publication_journal(journal: &PublicationJournal) -> Result<()> {
    validate_snapshot_journal(journal).or_else(|_| validate_fenced_publication_journal(journal))
}

fn validate_clean_paths(base: &Path, paths: &PublicationPaths) -> Result<()> {
    let marker = adoption_path(paths);
    for path in [&paths.target, &paths.journal, &marker] {
        reject_symlinked_managed_path_components(base, path, "runtime adoption managed path")?;
    }
    for path in [
        &paths.staging,
        &paths.backup,
        &paths.journal.with_extension("json.tmp"),
    ] {
        match std::fs::symlink_metadata(path) {
            Ok(_) => {
                return Err(invalid_publication(
                    "runtime adoption has publication residue",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if !paths.target.is_dir() {
        return Err(invalid_publication("runtime adoption target is absent"));
    }
    Ok(())
}

fn publication_tombstone(journal: &PublicationJournal) -> Result<PublicationTombstone> {
    PublicationTombstone::from_adopted(journal, &PublicationJobHandoff::adopt(journal)?)
}

fn adoption_path(paths: &PublicationPaths) -> PathBuf {
    paths.journal.with_file_name(ADOPTION_FILE)
}

#[cfg(test)]
#[path = "snapshot_adoption_tests.rs"]
mod tests;
