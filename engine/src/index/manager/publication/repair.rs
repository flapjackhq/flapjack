use super::digest::canonical_tenant_tree_digest;
use super::executor::{
    artifact_digest, capture_journaled_sidecars, cleanup_publication_residue, copy_path_durably,
    persist_journal, promote_journaled_sidecars, remove_path_if_exists, rename_path,
    resolved_artifact_paths, restore_journaled_sidecars, sidecar_backup_path,
    validate_manifest_entries, JournalWritePhase,
};
#[cfg(test)]
use super::fault::{CheckpointFaultHook, PublicationFaultHook};
use super::fault::{PublicationFaultPoint, PublicationIo};
use super::fsops::{reject_symlinked_managed_path, reject_symlinked_managed_path_components};
use super::{
    invalid_publication, relative_path_evidence, ContentDigest, PublicationArtifactManifest,
    PublicationEvent, PublicationJournal, PublicationPaths, PublicationPhase, PublicationTarget,
    PublicationTransactionId, Result, TantivyManagedInventory,
};
use std::fs;
use std::path::{Path, PathBuf};

/// Validity of the canonical journal. A temporary journal is never authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairJournalEvidence {
    Valid,
    Missing,
    Corrupt,
}

/// Digest evidence observed for one publication tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairArtifactEvidence {
    Missing,
    MatchesOld,
    MatchesNew,
    Mismatch,
    Unreadable,
}

/// Immutable evidence consumed by the publication repair decision table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepairEvidence {
    pub journal: RepairJournalEvidence,
    pub phase: PublicationPhase,
    pub target: RepairArtifactEvidence,
    pub backup: RepairArtifactEvidence,
    pub staging: RepairArtifactEvidence,
    pub manifest_valid: bool,
    pub journal_temp_present: bool,
}

impl RepairEvidence {
    /// Construct evidence backed by a valid canonical journal and manifest.
    pub fn valid(
        phase: PublicationPhase,
        target: RepairArtifactEvidence,
        backup: RepairArtifactEvidence,
        staging: RepairArtifactEvidence,
    ) -> Self {
        Self {
            journal: RepairJournalEvidence::Valid,
            phase,
            target,
            backup,
            staging,
            manifest_valid: true,
            journal_temp_present: false,
        }
    }
}

/// The only actions repair may select after inspecting all evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairDecision {
    None,
    Complete,
    Rollback,
    Cleanup,
    Quarantine,
}

pub(super) struct RepairOutcome {
    pub decision: RepairDecision,
    pub live_target_proven: bool,
    pub live_target_mutated: bool,
}

/// Select exactly one bounded repair action without mutating the filesystem.
pub fn decide_publication_repair(evidence: RepairEvidence) -> RepairDecision {
    if evidence.journal != RepairJournalEvidence::Valid || !evidence.manifest_valid {
        return RepairDecision::Quarantine;
    }
    if [evidence.target, evidence.backup, evidence.staging]
        .into_iter()
        .any(is_untrusted_artifact)
    {
        return RepairDecision::Quarantine;
    }

    let decision = match evidence.phase {
        PublicationPhase::Prepared => decide_prepared(evidence),
        PublicationPhase::Committed => decide_committed(evidence),
        PublicationPhase::RolledBack => decide_rolled_back(evidence),
        PublicationPhase::Quarantined => RepairDecision::Quarantine,
    };
    if decision == RepairDecision::None && evidence.journal_temp_present {
        RepairDecision::Cleanup
    } else {
        decision
    }
}

/// Repair one node-local publication transaction from durable journal evidence.
pub fn repair_publication(
    base: &Path,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    resolved_manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
) -> Result<RepairDecision> {
    repair_publication_outcome(base, target, transaction_id, resolved_manifest, inventory)
        .map(|outcome| outcome.decision)
}

pub(super) fn repair_publication_outcome(
    base: &Path,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    resolved_manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
) -> Result<RepairOutcome> {
    let io = PublicationIo::production();
    repair_publication_inner(
        base,
        target,
        transaction_id,
        resolved_manifest,
        inventory,
        &io,
    )
}

#[cfg(test)]
pub(crate) fn repair_publication_for_test(
    base: &Path,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    resolved_manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    fault: PublicationFaultPoint,
) -> Result<RepairDecision> {
    let faults = CheckpointFaultHook::new(fault);
    let io = PublicationIo::with_faults(&faults);
    repair_publication_inner(
        base,
        target,
        transaction_id,
        resolved_manifest,
        inventory,
        &io,
    )
    .map(|outcome| outcome.decision)
}

#[cfg(test)]
pub(crate) fn repair_publication_with_faults_for_test(
    base: &Path,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    resolved_manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    faults: &dyn PublicationFaultHook,
) -> Result<RepairDecision> {
    let io = PublicationIo::with_faults(faults);
    repair_publication_inner(
        base,
        target,
        transaction_id,
        resolved_manifest,
        inventory,
        &io,
    )
    .map(|outcome| outcome.decision)
}

fn repair_publication_inner(
    base: &Path,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    resolved_manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    io: &PublicationIo<'_>,
) -> Result<RepairOutcome> {
    let paths = PublicationPaths::new(base, &target, &transaction_id);
    let journal_temp = journal_temp_path(&paths);
    validate_repair_managed_paths(base, &paths, &journal_temp)?;
    let inspected = inspect_publication_repair(
        &paths,
        &target,
        &transaction_id,
        &resolved_manifest,
        inventory,
        journal_temp.exists(),
        io,
    )?;
    let decision = decide_publication_repair(inspected.evidence);
    let live_target_mutated = match decision {
        RepairDecision::Complete => paths.staging.exists(),
        RepairDecision::Rollback => paths.backup.exists(),
        RepairDecision::None | RepairDecision::Cleanup | RepairDecision::Quarantine => false,
    };
    let target_was_proven = matches!(
        inspected.evidence.target,
        RepairArtifactEvidence::MatchesOld | RepairArtifactEvidence::MatchesNew
    );
    let prior_target_existed = inspected
        .journal
        .as_ref()
        .is_some_and(|journal| journal.prior_digest.is_some());
    match decision {
        RepairDecision::None => {}
        RepairDecision::Complete => {
            let manifest = validated_repair_manifest(
                require_valid_journal(inspected.journal.as_ref())?,
                &resolved_manifest,
            )?;
            complete_publication_repair(&paths, inspected.journal.as_ref(), &manifest, io)?
        }
        RepairDecision::Rollback => {
            let manifest = validated_repair_manifest(
                require_valid_journal(inspected.journal.as_ref())?,
                &resolved_manifest,
            )?;
            rollback_publication_repair(&paths, inspected.journal.as_ref(), &manifest, io)?
        }
        RepairDecision::Cleanup => cleanup_repair_residue(&paths, &journal_temp, io)?,
        RepairDecision::Quarantine => {
            quarantine_repair_evidence(&paths, &journal_temp, inspected.journal.as_ref(), io)?
        }
    }
    let live_target_proven = match decision {
        RepairDecision::Complete => paths.target.exists(),
        RepairDecision::Rollback => prior_target_existed && paths.target.exists(),
        RepairDecision::None | RepairDecision::Cleanup | RepairDecision::Quarantine => {
            target_was_proven && paths.target.exists()
        }
    };
    Ok(RepairOutcome {
        decision,
        live_target_proven,
        live_target_mutated,
    })
}

fn validate_repair_managed_paths(
    base: &Path,
    paths: &PublicationPaths,
    journal_temp: &Path,
) -> Result<()> {
    for path in [
        &paths.target,
        &paths.staging,
        &paths.backup,
        &paths.journal,
        &paths.quarantine,
        journal_temp,
    ] {
        reject_symlinked_managed_path_components(base, path, "publication repair managed")?;
    }
    Ok(())
}

fn validated_repair_manifest(
    journal: &PublicationJournal,
    resolved: &PublicationArtifactManifest,
) -> Result<PublicationArtifactManifest> {
    validate_manifest_layout(&journal.artifact_manifest, resolved)?;
    let mut validated = resolved.clone();
    for (entry, persisted) in validated
        .entries
        .iter_mut()
        .zip(&journal.artifact_manifest.entries)
    {
        entry.original_digest = persisted.original_digest.clone();
        entry.promoted_digest = persisted.promoted_digest.clone();
    }
    Ok(validated)
}

struct InspectedRepair {
    evidence: RepairEvidence,
    journal: Option<PublicationJournal>,
}

fn inspect_publication_repair(
    paths: &PublicationPaths,
    target: &PublicationTarget,
    transaction_id: &PublicationTransactionId,
    resolved_manifest: &PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    journal_temp_present: bool,
    io: &PublicationIo<'_>,
) -> Result<InspectedRepair> {
    let raw_journal = match fs::read_to_string(&paths.journal) {
        Ok(value) => value,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InspectedRepair {
                evidence: RepairEvidence {
                    journal: RepairJournalEvidence::Missing,
                    phase: PublicationPhase::Prepared,
                    target: RepairArtifactEvidence::Missing,
                    backup: RepairArtifactEvidence::Missing,
                    staging: RepairArtifactEvidence::Missing,
                    manifest_valid: false,
                    journal_temp_present,
                },
                journal: None,
            });
        }
        Err(error) => return Err(error.into()),
    };
    let journal = match PublicationJournal::from_json(&raw_journal) {
        Ok(journal) => journal,
        Err(_) => {
            return Ok(InspectedRepair {
                evidence: RepairEvidence {
                    journal: RepairJournalEvidence::Corrupt,
                    phase: PublicationPhase::Prepared,
                    target: RepairArtifactEvidence::Missing,
                    backup: RepairArtifactEvidence::Missing,
                    staging: RepairArtifactEvidence::Missing,
                    manifest_valid: false,
                    journal_temp_present,
                },
                journal: None,
            });
        }
    };

    let manifest_valid = validate_repair_journal(paths, target, transaction_id, &journal)
        .and_then(|_| validate_manifest_entries(&resolved_manifest.entries))
        .and_then(|_| validate_manifest_layout(&journal.artifact_manifest, resolved_manifest))
        .and_then(|_| validate_manifest_artifacts(paths, &journal, resolved_manifest, io))
        .is_ok();
    let target = classify_tree_evidence(
        &paths.target,
        &journal.digest,
        journal.prior_digest.as_ref(),
        inventory,
        io,
    );
    let backup = classify_tree_evidence(
        &paths.backup,
        &journal.digest,
        journal.prior_digest.as_ref(),
        inventory,
        io,
    );
    let staging = classify_tree_evidence(
        &paths.staging,
        &journal.digest,
        journal.prior_digest.as_ref(),
        inventory,
        io,
    );
    Ok(InspectedRepair {
        evidence: RepairEvidence {
            journal: RepairJournalEvidence::Valid,
            phase: journal.phase,
            target,
            backup,
            staging,
            manifest_valid,
            journal_temp_present,
        },
        journal: Some(journal),
    })
}

fn validate_repair_journal(
    paths: &PublicationPaths,
    target: &PublicationTarget,
    transaction_id: &PublicationTransactionId,
    journal: &PublicationJournal,
) -> Result<()> {
    if &journal.target != target || &journal.transaction_id != transaction_id {
        return Err(invalid_publication("repair journal identity mismatch"));
    }
    let expected = relative_path_evidence(target, transaction_id);
    if journal.paths != expected {
        return Err(invalid_publication(
            "repair journal path evidence does not match canonical paths",
        ));
    }
    let canonical = PublicationPaths::new(
        paths
            .target
            .parent()
            .ok_or_else(|| invalid_publication("repair target has no parent"))?,
        target,
        transaction_id,
    );
    if &canonical != paths {
        return Err(invalid_publication(
            "repair canonical path derivation mismatch",
        ));
    }
    Ok(())
}

fn validate_manifest_layout(
    journal: &PublicationArtifactManifest,
    resolved: &PublicationArtifactManifest,
) -> Result<()> {
    if journal.entries.len() != resolved.entries.len() {
        return Err(invalid_publication(
            "repair artifact manifest layout mismatch",
        ));
    }
    for (left, right) in journal.entries.iter().zip(&resolved.entries) {
        if left.policy_key != right.policy_key
            || left.root != right.root
            || left.original_relative_path != right.original_relative_path
            || left.promoted_relative_path != right.promoted_relative_path
        {
            return Err(invalid_publication(
                "repair artifact manifest layout mismatch",
            ));
        }
    }
    Ok(())
}

fn validate_manifest_artifacts(
    paths: &PublicationPaths,
    journal: &PublicationJournal,
    resolved: &PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    for (persisted, resolved) in journal
        .artifact_manifest
        .entries
        .iter()
        .zip(&resolved.entries)
    {
        validate_manifest_artifact(paths, journal.phase, persisted, resolved, io)?;
    }
    Ok(())
}

fn validate_manifest_artifact(
    paths: &PublicationPaths,
    phase: PublicationPhase,
    persisted: &super::PublicationArtifactManifestEntry,
    resolved: &super::PublicationArtifactManifestEntry,
    io: &PublicationIo<'_>,
) -> Result<()> {
    let (original_path, promoted_path) = resolved_artifact_paths(resolved);
    let original = observed_artifact_digest(&original_path, io)?;
    let promoted = if promoted_path == original_path {
        original.clone()
    } else {
        observed_artifact_digest(&promoted_path, io)?
    };
    let backup = observed_artifact_digest(&sidecar_backup_path(paths, resolved), io)?;
    let old = &persisted.original_digest;
    let new = &persisted.promoted_digest;
    let backup_valid = backup.is_none() || digests_match(&backup, old);
    let legal = if promoted_path == original_path {
        digests_match(&original, old) && backup_valid
    } else {
        match phase {
            PublicationPhase::Prepared => {
                ((digests_match(&original, old) && digests_match(&promoted, new))
                    || (original.is_none()
                        && digests_match(&promoted, new)
                        && digests_match(&backup, old))
                    || (original.is_none() && promoted.is_none() && digests_match(&backup, old))
                    || (digests_match(&original, new) && promoted.is_none())
                    || (digests_match(&original, old) && promoted.is_none()))
                    && backup_valid
            }
            PublicationPhase::Committed => {
                digests_match(&original, new) && promoted.is_none() && backup_valid
            }
            PublicationPhase::RolledBack => {
                digests_match(&original, old) && promoted.is_none() && backup_valid
            }
            PublicationPhase::Quarantined => true,
        }
    };
    if legal {
        Ok(())
    } else {
        Err(invalid_publication(format!(
            "repair artifact digest evidence mismatch for '{}'",
            persisted.original_relative_path.display()
        )))
    }
}

fn digests_match(observed: &Option<ContentDigest>, expected: &Option<ContentDigest>) -> bool {
    observed == expected
}

fn observed_artifact_digest(path: &Path, io: &PublicationIo<'_>) -> Result<Option<ContentDigest>> {
    if !path.exists() {
        return Ok(None);
    }
    reject_symlinked_managed_path(path, "publication repair sidecar")?;
    io.before_digest(path)?;
    artifact_digest(path).map(Some)
}

fn classify_tree_evidence(
    path: &Path,
    new_digest: &ContentDigest,
    old_digest: Option<&ContentDigest>,
    inventory: &TantivyManagedInventory,
    io: &PublicationIo<'_>,
) -> RepairArtifactEvidence {
    if !path.exists() {
        return RepairArtifactEvidence::Missing;
    }
    if reject_symlinked_managed_path(path, "publication repair artifact").is_err() {
        return RepairArtifactEvidence::Unreadable;
    }
    if io.before_digest(path).is_err() {
        return RepairArtifactEvidence::Unreadable;
    }
    match canonical_tenant_tree_digest(path, inventory) {
        Ok(digest) if &digest == new_digest => RepairArtifactEvidence::MatchesNew,
        Ok(digest) if old_digest == Some(&digest) => RepairArtifactEvidence::MatchesOld,
        Ok(_) => RepairArtifactEvidence::Mismatch,
        Err(_) => RepairArtifactEvidence::Unreadable,
    }
}

fn complete_publication_repair(
    paths: &PublicationPaths,
    journal: Option<&PublicationJournal>,
    manifest: &PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    let journal = require_valid_journal(journal)?;
    if paths.staging.exists() {
        capture_journaled_sidecars(paths, manifest, io)?;
        if paths.target.exists() {
            rename_path(&paths.target, &paths.backup, io)?;
        }
        rename_path(&paths.staging, &paths.target, io)?;
    }
    io.checkpoint(PublicationFaultPoint::AfterRepairTargetRename)?;
    promote_journaled_sidecars(manifest, io)?;
    let committed = journal.clone().apply(PublicationEvent::Commit)?;
    persist_journal(paths, &committed, JournalWritePhase::Commit, io)?;
    cleanup_repair_residue(paths, &journal_temp_path(paths), io)
}

fn rollback_publication_repair(
    paths: &PublicationPaths,
    journal: Option<&PublicationJournal>,
    manifest: &PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    let journal = require_valid_journal(journal)?;
    remove_path_if_exists(&paths.staging, io)?;
    if paths.backup.exists() {
        remove_path_if_exists(&paths.target, io)?;
        rename_path(&paths.backup, &paths.target, io)?;
    }
    io.checkpoint(PublicationFaultPoint::AfterRepairTargetRename)?;
    restore_journaled_sidecars(paths, manifest, io)?;
    let rolled_back = journal.clone().apply(PublicationEvent::Rollback)?;
    persist_journal(paths, &rolled_back, JournalWritePhase::Rollback, io)?;
    cleanup_repair_residue(paths, &journal_temp_path(paths), io)
}

fn cleanup_repair_residue(
    paths: &PublicationPaths,
    journal_temp: &Path,
    io: &PublicationIo<'_>,
) -> Result<()> {
    cleanup_publication_residue(paths, io)?;
    remove_path_if_exists(journal_temp, io)?;
    Ok(())
}

fn quarantine_repair_evidence(
    paths: &PublicationPaths,
    journal_temp: &Path,
    journal: Option<&PublicationJournal>,
    io: &PublicationIo<'_>,
) -> Result<()> {
    io.create_dir_all(&paths.quarantine)?;
    io.checkpoint(PublicationFaultPoint::DuringQuarantine)?;
    if let Some(journal) =
        journal.and_then(|journal| journal.clone().apply(PublicationEvent::Quarantine).ok())
    {
        let quarantine_journal = paths.quarantine.join("journal.json");
        io.write_file(
            &quarantine_journal,
            &serde_json::to_vec_pretty(&journal.to_json_value())?,
        )?;
        io.sync_file(&quarantine_journal)?;
    } else if paths.journal.exists() {
        copy_path_durably(&paths.journal, &paths.quarantine.join("journal.json"), io)?;
    }
    copy_quarantine_path(&paths.staging, &paths.quarantine.join("staging"), io)?;
    copy_quarantine_path(&paths.backup, &paths.quarantine.join("backup"), io)?;
    copy_quarantine_path(journal_temp, &paths.quarantine.join("journal.json.tmp"), io)?;
    io.sync_dir(&paths.quarantine)?;
    Ok(())
}

fn copy_quarantine_path(from: &Path, to: &Path, io: &PublicationIo<'_>) -> Result<()> {
    if from.exists() {
        copy_path_durably(from, to, io)?;
    }
    Ok(())
}

fn require_valid_journal(journal: Option<&PublicationJournal>) -> Result<&PublicationJournal> {
    journal.ok_or_else(|| invalid_publication("repair decision requires a valid journal"))
}

fn journal_temp_path(paths: &PublicationPaths) -> PathBuf {
    paths.journal.with_extension("json.tmp")
}

fn decide_prepared(evidence: RepairEvidence) -> RepairDecision {
    use RepairArtifactEvidence::{MatchesNew as New, MatchesOld as Old, Missing};
    match (evidence.target, evidence.backup, evidence.staging) {
        (Old, Missing, New) | (Missing, Old, New) | (New, Old, Missing) => RepairDecision::Complete,
        (Missing, Old, Missing) | (Old, Missing, Missing) => RepairDecision::Rollback,
        _ => RepairDecision::Quarantine,
    }
}

fn decide_committed(evidence: RepairEvidence) -> RepairDecision {
    use RepairArtifactEvidence::{MatchesNew as New, MatchesOld as Old, Missing};
    match (evidence.target, evidence.backup, evidence.staging) {
        (New, Missing, Missing) => RepairDecision::None,
        (New, Old, Missing) => RepairDecision::Cleanup,
        _ => RepairDecision::Quarantine,
    }
}

fn decide_rolled_back(evidence: RepairEvidence) -> RepairDecision {
    use RepairArtifactEvidence::{MatchesOld as Old, Missing};
    match (evidence.target, evidence.backup, evidence.staging) {
        (Old, Missing, Missing) | (Missing, Missing, Missing) => RepairDecision::None,
        (Old, Missing, _) | (Missing, Missing, _) => RepairDecision::Cleanup,
        _ => RepairDecision::Quarantine,
    }
}

fn is_untrusted_artifact(evidence: RepairArtifactEvidence) -> bool {
    matches!(
        evidence,
        RepairArtifactEvidence::Mismatch | RepairArtifactEvidence::Unreadable
    )
}
