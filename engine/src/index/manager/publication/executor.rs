use super::digest::canonical_tenant_tree_digest;
#[cfg(test)]
use super::fault::{CheckpointFaultHook, PublicationFaultHook};
use super::fault::{PublicationFaultPoint, PublicationIo};
use super::fsops::reject_symlinked_managed_path_components;
use super::{
    artifact_policy_table, classify_external_relative_path, invalid_publication,
    validate_relative_path, ArtifactDisposition, ContentDigest, ExternalArtifactRoot,
    PublicationEvent, PublicationGenerationEvidence, PublicationJournal, PublicationPaths,
    PublicationTarget, PublicationTransactionId, Result, TantivyManagedInventory,
};
use crate::analytics::config::{AnalyticsConfig, AnalyticsTargetArtifactPaths};
use crate::query_suggestions::config::{QsConfigStore, QsTargetArtifactPaths};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Filesystem phase reached by a caller-populated publication activation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreStagedActivationStage {
    Prepare,
    BackupTarget,
    PromoteStaging,
}

/// Typed activation failure used by adapters that preserve stable step tags.
#[derive(Debug)]
pub struct PreStagedActivationError {
    stage: PreStagedActivationStage,
    source: crate::error::FlapjackError,
}

/// How an activation may treat whatever already occupies the publication target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivationMode {
    /// Replace any existing target tree, backing it up first so rollback can restore it.
    Replace,
    /// Publish only into a target name this activation reserved for itself, refusing
    /// a target that already exists rather than replacing it.
    CreateOnly,
}

struct ActivationContext<'a> {
    io: &'a PublicationIo<'a>,
    stage: &'a std::cell::Cell<PreStagedActivationStage>,
    mode: ActivationMode,
}

impl PreStagedActivationError {
    pub fn stage(&self) -> PreStagedActivationStage {
        self.stage
    }
}

impl fmt::Display for PreStagedActivationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.source.fmt(formatter)
    }
}

impl std::error::Error for PreStagedActivationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Core-owned handle for a caller-populated publication staging tree.
pub struct PreStagedPublication {
    paths: PublicationPaths,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    generation: PublicationGenerationEvidence,
}

/// Remove an unjournaled transaction namespace for recovery code that only has
/// durable metadata, not the original `PreStagedPublication` handle.
pub fn abort_unjournaled_publication(
    base: &Path,
    target: PublicationTarget,
    transaction_id: &PublicationTransactionId,
) -> Result<()> {
    let paths = PublicationPaths::new(base, &target, transaction_id);
    if paths.journal.exists() {
        return Err(invalid_publication(
            "cannot abort a journaled publication transaction",
        ));
    }
    discard_transaction_namespace(&paths)
}

impl PreStagedPublication {
    /// Allocate an exclusive transaction namespace before the caller extracts content.
    pub fn prepare(base: &Path, target: PublicationTarget) -> Result<Self> {
        let transaction_id =
            PublicationTransactionId::new(format!("snapshot_{}", uuid::Uuid::new_v4().simple()))?;
        let generation = PublicationGenerationEvidence::new(format!(
            "snapshot_{}",
            uuid::Uuid::new_v4().simple()
        ))?;
        let paths = PublicationPaths::new(base, &target, &transaction_id);
        let namespace = paths.staging.parent().ok_or_else(|| {
            invalid_publication("publication staging path has no transaction namespace")
        })?;
        reject_symlinked_managed_path_components(base, namespace, "publication transaction")?;
        fs::create_dir_all(namespace.parent().ok_or_else(|| {
            invalid_publication("publication transaction namespace has no parent")
        })?)?;
        fs::create_dir(namespace)?;
        Ok(Self {
            paths,
            target,
            transaction_id,
            generation,
        })
    }

    /// Paths the caller may use to populate and validate the staging tree.
    pub fn paths(&self) -> &PublicationPaths {
        &self.paths
    }

    pub fn transaction_id(&self) -> &PublicationTransactionId {
        &self.transaction_id
    }

    /// Remove only this transaction when no durable journal has been written.
    pub fn abort(self) -> Result<()> {
        if self.paths.journal.exists() {
            return Err(invalid_publication(
                "cannot abort a journaled publication transaction",
            ));
        }
        discard_transaction_namespace(&self.paths)
    }

    /// Activate the validated staging tree with the snapshot sidecar policy,
    /// replacing any tree that already occupies the target.
    pub fn activate(self) -> std::result::Result<PublicationJournal, PreStagedActivationError> {
        self.activate_with_mode(ActivationMode::Replace)
    }

    /// Activate the validated staging tree only if the target name is still free.
    ///
    /// Unlike [`PreStagedPublication::activate`], this never replaces an existing
    /// target: it atomically reserves the target name or fails with
    /// [`crate::error::FlapjackError::IndexAlreadyExists`], leaving whatever is
    /// already published there untouched. The reservation excludes concurrent
    /// create-only activations, so exactly one of them can win a race for a name.
    pub fn activate_create_only(
        self,
    ) -> std::result::Result<PublicationJournal, PreStagedActivationError> {
        self.activate_with_mode(ActivationMode::CreateOnly)
    }

    fn activate_with_mode(
        self,
        mode: ActivationMode,
    ) -> std::result::Result<PublicationJournal, PreStagedActivationError> {
        // The inventory is collected before any reservation so it observes the real
        // trees rather than this activation's own empty reservation directory.
        let inventory = TantivyManagedInventory::from_existing_trees([
            self.paths.target.as_path(),
            self.paths.staging.as_path(),
            self.paths.backup.as_path(),
        ])
        .map_err(|source| PreStagedActivationError {
            stage: PreStagedActivationStage::Prepare,
            source,
        })?;
        if mode == ActivationMode::CreateOnly {
            if let Err(source) = reserve_publication_target(&self.paths.target, &self.target) {
                // Losing the name is terminal for this transaction and nothing is
                // journaled yet, so the staged tree is pure residue.
                let _ = discard_transaction_namespace(&self.paths);
                return Err(PreStagedActivationError {
                    stage: PreStagedActivationStage::Prepare,
                    source,
                });
            }
        }
        let stage = std::cell::Cell::new(PreStagedActivationStage::Prepare);
        let io = PublicationIo::production();
        activate_publication_inner(
            &self.paths,
            self.target,
            self.transaction_id,
            self.generation,
            PublicationArtifactManifest::default(),
            &inventory,
            &ActivationContext {
                io: &io,
                stage: &stage,
                mode,
            },
        )
        .map_err(|source| PreStagedActivationError {
            stage: stage.get(),
            source,
        })
    }

    #[cfg(test)]
    pub(crate) fn activate_with_fault_for_test(
        self,
        fault: PublicationFaultPoint,
    ) -> std::result::Result<PublicationJournal, PreStagedActivationError> {
        let inventory = TantivyManagedInventory::from_existing_trees([
            self.paths.target.as_path(),
            self.paths.staging.as_path(),
            self.paths.backup.as_path(),
        ])
        .map_err(|source| PreStagedActivationError {
            stage: PreStagedActivationStage::Prepare,
            source,
        })?;
        let stage = std::cell::Cell::new(PreStagedActivationStage::Prepare);
        let faults = CheckpointFaultHook::new(fault);
        let io = PublicationIo::with_faults(&faults);
        activate_publication_inner(
            &self.paths,
            self.target,
            self.transaction_id,
            self.generation,
            PublicationArtifactManifest::default(),
            &inventory,
            &ActivationContext {
                io: &io,
                stage: &stage,
                mode: ActivationMode::Replace,
            },
        )
        .map_err(|source| PreStagedActivationError {
            stage: stage.get(),
            source,
        })
    }
}

/// Atomically claim an unused target name for a create-only activation.
///
/// `create_dir` is the exclusion primitive: the filesystem either creates the
/// directory or reports `AlreadyExists`, so two concurrent activations can never
/// both believe they own the name. This is why create-only never snapshots
/// `exists()` — a snapshot can go stale between the check and the promote, while
/// the reservation cannot.
///
/// The reserved directory is deliberately left empty and held until
/// [`promote_staging`] renames the staged tree onto it, which POSIX `rename`
/// permits precisely because the destination is an empty directory.
fn reserve_publication_target(target_path: &Path, target: &PublicationTarget) -> Result<()> {
    fs::create_dir_all(require_parent(target_path)?)?;
    match fs::create_dir(target_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(
            crate::error::FlapjackError::IndexAlreadyExists(target.as_str().to_string()),
        ),
        Err(error) => Err(error.into()),
    }
}

/// Remove a transaction's namespace. Only sound while no journal is durable.
fn discard_transaction_namespace(paths: &PublicationPaths) -> Result<()> {
    let namespace = paths.staging.parent().ok_or_else(|| {
        invalid_publication("publication staging path has no transaction namespace")
    })?;
    if namespace.exists() {
        fs::remove_dir_all(namespace)?;
    }
    Ok(())
}

/// Runtime root selector for journaled publication artifacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationArtifactRoot {
    QuerySuggestions,
    Analytics,
}

/// Canonical manifest entry for one publication-managed external artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationArtifactManifestEntry {
    pub policy_key: String,
    pub root: PublicationArtifactRoot,
    pub original_relative_path: PathBuf,
    pub promoted_relative_path: PathBuf,
    pub original_digest: Option<ContentDigest>,
    pub promoted_digest: Option<ContentDigest>,
    #[serde(skip, default)]
    root_path: PathBuf,
}

impl PublicationArtifactManifestEntry {
    /// Build a journaled external-artifact manifest entry.
    pub fn journaled(
        policy_key: impl Into<String>,
        root: PublicationArtifactRoot,
        original_relative_path: PathBuf,
        promoted_relative_path: PathBuf,
        root_path: PathBuf,
    ) -> Self {
        Self {
            policy_key: policy_key.into(),
            root,
            original_relative_path,
            promoted_relative_path,
            original_digest: None,
            promoted_digest: None,
            root_path,
        }
    }
}

/// Canonical manifest for all publication-owned external artifacts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationArtifactManifest {
    pub entries: Vec<PublicationArtifactManifestEntry>,
}

/// Owner-resolved source, destination, and transaction-local sidecars for a move.
pub struct PublicationArtifactPlan {
    source_query_suggestions: QsTargetArtifactPaths,
    staged_query_suggestions: QsTargetArtifactPaths,
    source_analytics: AnalyticsTargetArtifactPaths,
    staged_analytics: AnalyticsTargetArtifactPaths,
    manifest: PublicationArtifactManifest,
}

impl PublicationArtifactPlan {
    /// Resolve all target-keyed artifacts through their canonical path owners.
    pub fn for_move(
        base: &Path,
        analytics: &AnalyticsConfig,
        source: &str,
        target: &PublicationTarget,
        transaction: &PublicationTransactionId,
    ) -> Result<Self> {
        let query_suggestions = QsConfigStore::new(base);
        let source_query_suggestions = query_suggestions.target_artifact_paths(source)?;
        let staging_key = publication_staging_key(transaction);
        let staged_query_suggestions = query_suggestions.target_artifact_paths(&staging_key)?;
        let source_analytics = analytics.target_artifact_paths(source);
        let staged_analytics = analytics.target_artifact_paths(&staging_key);
        let manifest = resolved_move_manifest(base, analytics, target, transaction)?;
        Ok(Self {
            source_query_suggestions,
            staged_query_suggestions,
            source_analytics,
            staged_analytics,
            manifest,
        })
    }

    /// Durably copy source artifacts into transaction-local promoted paths.
    pub fn stage(&self) -> Result<()> {
        let io = PublicationIo::production();
        let result = stage_query_suggestions(
            &self.source_query_suggestions,
            &self.staged_query_suggestions,
            &io,
        )
        .and_then(|()| {
            stage_artifact_copy(
                &self.source_analytics.index_root,
                &self.staged_analytics.index_root,
                &io,
            )
        });
        if result.is_err() {
            let _ = self.remove_staged(&io);
        }
        result
    }

    /// Remove the original source-keyed artifacts after durable commit.
    pub fn remove_source(&self) -> Result<()> {
        let io = PublicationIo::production();
        remove_query_suggestions(&self.source_query_suggestions, &io)?;
        io.remove_if_exists(&self.source_analytics.index_root)
    }

    pub fn manifest(&self) -> PublicationArtifactManifest {
        self.manifest.clone()
    }

    fn remove_staged(&self, io: &PublicationIo<'_>) -> Result<()> {
        remove_query_suggestions(&self.staged_query_suggestions, io)?;
        io.remove_if_exists(&self.staged_analytics.index_root)
    }
}

impl PublicationArtifactManifest {
    /// Validate and sort a caller-resolved publication artifact manifest.
    pub fn new(
        entries: impl IntoIterator<Item = PublicationArtifactManifestEntry>,
    ) -> Result<Self> {
        let mut entries: Vec<_> = entries.into_iter().collect();
        entries.sort_by(|left, right| {
            (
                left.root,
                &left.original_relative_path,
                &left.promoted_relative_path,
                &left.policy_key,
            )
                .cmp(&(
                    right.root,
                    &right.original_relative_path,
                    &right.promoted_relative_path,
                    &right.policy_key,
                ))
        });
        validate_manifest_entries(&entries)?;
        Ok(Self { entries })
    }

    /// Build a manifest from the existing owner-resolved sidecar artifact paths.
    pub fn from_resolved_artifacts(
        query_suggestions: Option<(QsTargetArtifactPaths, QsTargetArtifactPaths)>,
        analytics: Option<(AnalyticsTargetArtifactPaths, AnalyticsTargetArtifactPaths)>,
    ) -> Result<Self> {
        let mut entries = Vec::new();
        if let Some((original, promoted)) = query_suggestions {
            entries.extend(query_suggestions_manifest_entries(original, promoted)?);
        }
        if let Some((original, promoted)) = analytics {
            entries.push(analytics_manifest_entry(original, promoted)?);
        }
        Self::new(entries)
    }

    /// Restore runtime roots for a persisted move manifest through canonical path owners.
    pub fn resolve_for_repair(
        base: &Path,
        analytics: &AnalyticsConfig,
        target: &PublicationTarget,
        transaction: &PublicationTransactionId,
        persisted: &Self,
    ) -> Result<Self> {
        if persisted.entries.is_empty() {
            return Ok(Self::default());
        }
        let resolved = resolved_move_manifest(base, analytics, target, transaction)?;
        let same_layout =
            persisted
                .entries
                .iter()
                .zip(&resolved.entries)
                .all(|(persisted, resolved)| {
                    persisted.policy_key == resolved.policy_key
                        && persisted.root == resolved.root
                        && persisted.original_relative_path == resolved.original_relative_path
                        && persisted.promoted_relative_path == resolved.promoted_relative_path
                });
        if persisted.entries.len() != resolved.entries.len() || !same_layout {
            return Err(invalid_publication(
                "persisted artifact manifest does not match canonical target paths",
            ));
        }
        Ok(resolved)
    }
}

fn resolved_move_manifest(
    base: &Path,
    analytics: &AnalyticsConfig,
    target: &PublicationTarget,
    transaction: &PublicationTransactionId,
) -> Result<PublicationArtifactManifest> {
    let query_suggestions = QsConfigStore::new(base);
    let staging_key = publication_staging_key(transaction);
    PublicationArtifactManifest::from_resolved_artifacts(
        Some((
            query_suggestions.target_artifact_paths(target.as_str())?,
            query_suggestions.target_artifact_paths(&staging_key)?,
        )),
        Some((
            analytics.target_artifact_paths(target.as_str()),
            analytics.target_artifact_paths(&staging_key),
        )),
    )
}

fn publication_staging_key(transaction: &PublicationTransactionId) -> String {
    format!("publication_{}", transaction.as_str())
}

/// Execute one crash-safe staged publication using production filesystem behavior.
pub fn activate_publication(
    paths: &PublicationPaths,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    generation: PublicationGenerationEvidence,
    manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
) -> Result<PublicationJournal> {
    let io = PublicationIo::production();
    let stage = std::cell::Cell::new(PreStagedActivationStage::Prepare);
    activate_publication_inner(
        paths,
        target,
        transaction_id,
        generation,
        manifest,
        inventory,
        &ActivationContext {
            io: &io,
            stage: &stage,
            mode: ActivationMode::Replace,
        },
    )
}

#[cfg(test)]
pub(crate) fn activate_publication_for_test(
    paths: &PublicationPaths,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    generation: PublicationGenerationEvidence,
    manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    fault: PublicationFaultPoint,
) -> Result<PublicationJournal> {
    let faults = CheckpointFaultHook::new(fault);
    let io = PublicationIo::with_faults(&faults);
    let stage = std::cell::Cell::new(PreStagedActivationStage::Prepare);
    activate_publication_inner(
        paths,
        target,
        transaction_id,
        generation,
        manifest,
        inventory,
        &ActivationContext {
            io: &io,
            stage: &stage,
            mode: ActivationMode::Replace,
        },
    )
}

#[cfg(test)]
pub(crate) fn activate_publication_with_faults_for_test(
    paths: &PublicationPaths,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    generation: PublicationGenerationEvidence,
    manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    faults: &dyn PublicationFaultHook,
) -> Result<PublicationJournal> {
    let io = PublicationIo::with_faults(faults);
    let stage = std::cell::Cell::new(PreStagedActivationStage::Prepare);
    activate_publication_inner(
        paths,
        target,
        transaction_id,
        generation,
        manifest,
        inventory,
        &ActivationContext {
            io: &io,
            stage: &stage,
            mode: ActivationMode::Replace,
        },
    )
}

fn activate_publication_inner(
    paths: &PublicationPaths,
    target: PublicationTarget,
    transaction_id: PublicationTransactionId,
    generation: PublicationGenerationEvidence,
    mut manifest: PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    context: &ActivationContext<'_>,
) -> Result<PublicationJournal> {
    let io = context.io;
    let target_existed = match context.mode {
        ActivationMode::Replace => paths.target.exists(),
        // The caller holds an empty reservation at the target. It is this
        // transaction's own state, never a prior tree, so it must not be digested,
        // backed up, or restored as one — and this activation owns releasing it on
        // every pre-commit failure below.
        ActivationMode::CreateOnly => false,
    };
    let evidence = prepare_digest_evidence(paths, &mut manifest, inventory, target_existed, io);
    let (prior_digest, digest) = match evidence {
        Ok(evidence) => evidence,
        Err(error) => {
            cleanup_unprepared_transaction(paths, &manifest, context)?;
            return Err(error);
        }
    };
    let mut journal =
        PublicationJournal::prepare(transaction_id, target, generation, digest, paths.clone());
    journal.prior_digest = prior_digest;
    journal.artifact_manifest = manifest.clone();
    let activation_result = (|| {
        io.checkpoint(PublicationFaultPoint::DuringStagingSync)?;
        sync_tree(&paths.staging, io)?;
        persist_journal(paths, &journal, JournalWritePhase::Prepare, io)?;
        io.checkpoint(PublicationFaultPoint::AfterPrepareJournal)?;
        promote_staging(paths, &manifest, target_existed, io, context.stage)?;
        io.checkpoint(PublicationFaultPoint::BeforeCommitJournal)?;
        let committed = journal.clone().apply(PublicationEvent::Commit)?;
        persist_journal(paths, &committed, JournalWritePhase::Commit, io)?;
        Ok(committed)
    })();
    match activation_result {
        Ok(committed) => {
            let _ = io.checkpoint(PublicationFaultPoint::CommitDurable);
            let _ = io.checkpoint(PublicationFaultPoint::AfterCommitJournal);
            let _ = cleanup_publication_residue(paths, io);
            Ok(committed)
        }
        Err(error) => {
            resolve_failed_activation(paths, &manifest, target_existed, journal, io, error)
        }
    }
}

fn prepare_digest_evidence(
    paths: &PublicationPaths,
    manifest: &mut PublicationArtifactManifest,
    inventory: &TantivyManagedInventory,
    target_existed: bool,
    io: &PublicationIo<'_>,
) -> Result<(Option<ContentDigest>, ContentDigest)> {
    validate_manifest_entries(&manifest.entries)?;
    populate_manifest_digests(manifest, io)?;
    io.checkpoint(PublicationFaultPoint::BeforeStagingDigest)?;
    // `target_existed` is the single source of truth for whether a prior tree is
    // there to digest; re-testing `exists()` here would be a second, independently
    // stale answer to the same question.
    let prior_digest = if target_existed {
        io.before_digest(&paths.target)?;
        Some(canonical_tenant_tree_digest(&paths.target, inventory)?)
    } else {
        None
    };
    io.before_digest(&paths.staging)?;
    let digest = canonical_tenant_tree_digest(&paths.staging, inventory)?;
    Ok((prior_digest, digest))
}

fn cleanup_unprepared_transaction(
    paths: &PublicationPaths,
    manifest: &PublicationArtifactManifest,
    context: &ActivationContext<'_>,
) -> Result<()> {
    let io = context.io;
    if context.mode == ActivationMode::CreateOnly {
        // Release the reservation this activation is holding. Gating on the mode
        // rather than on `target_existed` matters: a replace activation must never
        // remove a target it does not own, even when it observed none.
        io.remove_if_exists(&paths.target)?;
    }
    for entry in &manifest.entries {
        let (original, promoted) = resolved_artifact_paths(entry);
        if promoted != original {
            io.remove_if_exists(&promoted)?;
        }
    }
    io.remove_if_exists(&paths.staging)?;
    io.remove_if_exists(&paths.backup)?;
    io.remove_if_exists(&sidecar_residue_root(paths))?;
    io.remove_if_exists(&paths.journal.with_extension("json.tmp"))?;
    Ok(())
}

fn resolve_failed_activation(
    paths: &PublicationPaths,
    manifest: &PublicationArtifactManifest,
    target_existed: bool,
    journal: PublicationJournal,
    io: &PublicationIo<'_>,
    activation_error: crate::error::FlapjackError,
) -> Result<PublicationJournal> {
    rollback_activation(paths, manifest, target_existed, io)?;
    let rolled_back = journal.clone().apply(PublicationEvent::Rollback)?;
    let rollback_transition = persist_journal(paths, &rolled_back, JournalWritePhase::Rollback, io)
        .and_then(|()| io.checkpoint(PublicationFaultPoint::AfterRollbackJournal));
    if let Err(rollback_error) = rollback_transition {
        if target_existed {
            return Err(rollback_error);
        }
        let quarantined = journal.apply(PublicationEvent::Quarantine)?;
        quarantine_journal(paths, &quarantined, io)?;
    }
    Err(activation_error)
}

fn promote_staging(
    paths: &PublicationPaths,
    manifest: &PublicationArtifactManifest,
    target_existed: bool,
    io: &PublicationIo<'_>,
    activation_stage: &std::cell::Cell<PreStagedActivationStage>,
) -> Result<()> {
    capture_journaled_sidecars(paths, manifest, io)?;
    if target_existed {
        activation_stage.set(PreStagedActivationStage::BackupTarget);
        io.rename(&paths.target, &paths.backup)?;
        io.checkpoint(PublicationFaultPoint::AfterTargetBackup)?;
    }
    activation_stage.set(PreStagedActivationStage::PromoteStaging);
    io.rename(&paths.staging, &paths.target)?;
    promote_journaled_sidecars(manifest, io)?;
    io.checkpoint(PublicationFaultPoint::AfterStagingPromote)?;
    Ok(())
}

fn rollback_activation(
    paths: &PublicationPaths,
    manifest: &PublicationArtifactManifest,
    target_existed: bool,
    io: &PublicationIo<'_>,
) -> Result<()> {
    io.remove_if_exists(&paths.staging)?;
    if target_existed && paths.backup.exists() {
        io.remove_if_exists(&paths.target)?;
        io.rename(&paths.backup, &paths.target)?;
    } else if !target_existed {
        io.remove_if_exists(&paths.target)?;
        io.remove_if_exists(&paths.backup)?;
    }
    restore_journaled_sidecars(paths, manifest, io)?;
    Ok(())
}

pub(super) fn capture_journaled_sidecars(
    paths: &PublicationPaths,
    manifest: &PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    for entry in &manifest.entries {
        let original = entry.root_path.join(&entry.original_relative_path);
        let backup = sidecar_backup_path(paths, entry);
        if original.exists() && !backup.exists() {
            copy_path_durably(&original, &backup, io)?;
        }
    }
    Ok(())
}

pub(super) fn promote_journaled_sidecars(
    manifest: &PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    for entry in &manifest.entries {
        if entry.original_relative_path == entry.promoted_relative_path {
            continue;
        }
        let (original, promoted) = resolved_artifact_paths(entry);
        if !promoted.exists() {
            if entry.promoted_digest.is_none() {
                io.remove_if_exists(&original)?;
                continue;
            }
            if artifact_matches_digest(&original, entry.promoted_digest.as_ref(), io)? {
                continue;
            }
            return Err(invalid_publication(format!(
                "promoted publication artifact '{}' does not exist",
                entry.promoted_relative_path.display()
            )));
        }
        io.remove_if_exists(&original)?;
        io.rename(&promoted, &original)?;
    }
    Ok(())
}

fn stage_query_suggestions(
    source: &QsTargetArtifactPaths,
    staged: &QsTargetArtifactPaths,
    io: &PublicationIo<'_>,
) -> Result<()> {
    for (source, staged) in [
        (&source.config_path, &staged.config_path),
        (&source.log_path, &staged.log_path),
        (&source.status_path, &staged.status_path),
    ] {
        stage_artifact_copy(source, staged, io)?;
    }
    Ok(())
}

fn stage_artifact_copy(source: &Path, staged: &Path, io: &PublicationIo<'_>) -> Result<()> {
    io.remove_if_exists(staged)?;
    if source.exists() {
        copy_path_durably(source, staged, io)?;
    }
    Ok(())
}

fn remove_query_suggestions(
    artifacts: &QsTargetArtifactPaths,
    io: &PublicationIo<'_>,
) -> Result<()> {
    io.remove_if_exists(&artifacts.config_path)?;
    io.remove_if_exists(&artifacts.log_path)?;
    io.remove_if_exists(&artifacts.status_path)
}

pub(super) fn restore_journaled_sidecars(
    paths: &PublicationPaths,
    manifest: &PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    for entry in &manifest.entries {
        let (original, promoted) = resolved_artifact_paths(entry);
        let backup = sidecar_backup_path(paths, entry);
        if backup.exists() {
            io.remove_if_exists(&original)?;
            copy_path_durably(&backup, &original, io)?;
        } else if entry.original_digest.is_none() {
            io.remove_if_exists(&original)?;
        }
        if promoted != original {
            io.remove_if_exists(&promoted)?;
        }
    }
    io.remove_if_exists(&sidecar_residue_root(paths))?;
    Ok(())
}

pub(super) fn resolved_artifact_paths(
    entry: &PublicationArtifactManifestEntry,
) -> (PathBuf, PathBuf) {
    (
        entry.root_path.join(&entry.original_relative_path),
        entry.root_path.join(&entry.promoted_relative_path),
    )
}

pub(super) fn sidecar_backup_path(
    paths: &PublicationPaths,
    entry: &PublicationArtifactManifestEntry,
) -> PathBuf {
    paths
        .journal
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("sidecars")
        .join(root_key(entry.root))
        .join(&entry.original_relative_path)
}

#[derive(Clone, Copy)]
pub(super) enum JournalWritePhase {
    Prepare,
    Commit,
    Rollback,
}

pub(super) fn persist_journal(
    paths: &PublicationPaths,
    journal: &PublicationJournal,
    phase: JournalWritePhase,
    io: &PublicationIo<'_>,
) -> Result<()> {
    let parent = require_parent(&paths.journal)?;
    io.create_dir_all(parent)?;
    let temp = paths.journal.with_extension("json.tmp");
    io.write_file(&temp, &serde_json::to_vec_pretty(&journal.to_json_value())?)?;
    io.checkpoint(phase.write_checkpoint())?;
    io.sync_file(&temp)?;
    io.rename(&temp, &paths.journal)?;
    io.checkpoint(phase.rename_checkpoint())?;
    io.sync_dir(parent)?;
    Ok(())
}

impl JournalWritePhase {
    fn write_checkpoint(self) -> PublicationFaultPoint {
        match self {
            Self::Prepare => PublicationFaultPoint::DuringPrepareJournalWrite,
            Self::Commit => PublicationFaultPoint::DuringCommitJournalWrite,
            Self::Rollback => PublicationFaultPoint::DuringRollbackJournalWrite,
        }
    }

    fn rename_checkpoint(self) -> PublicationFaultPoint {
        match self {
            Self::Prepare => PublicationFaultPoint::AfterPrepareJournalRename,
            Self::Commit => PublicationFaultPoint::AfterCommitJournalRename,
            Self::Rollback => PublicationFaultPoint::AfterRollbackJournalRename,
        }
    }
}

fn quarantine_journal(
    paths: &PublicationPaths,
    journal: &PublicationJournal,
    io: &PublicationIo<'_>,
) -> Result<()> {
    io.create_dir_all(&paths.quarantine)?;
    io.checkpoint(PublicationFaultPoint::DuringQuarantine)?;
    let quarantine_journal = paths.quarantine.join("journal.json");
    io.write_file(
        &quarantine_journal,
        &serde_json::to_vec_pretty(&journal.to_json_value())?,
    )?;
    io.sync_file(&quarantine_journal)?;
    io.sync_dir(&paths.quarantine)?;
    Ok(())
}

pub(super) fn validate_manifest_entries(
    entries: &[PublicationArtifactManifestEntry],
) -> Result<()> {
    let mut owners = BTreeSet::new();
    for entry in entries {
        validate_manifest_entry(entry)?;
        reject_overlapping_manifest_entry(&owners, entry.root, &entry.original_relative_path)?;
        let original_key = (entry.root, entry.original_relative_path.clone());
        if !owners.insert(original_key) {
            return Err(invalid_publication(format!(
                "duplicate publication artifact ownership '{}'",
                entry.original_relative_path.display()
            )));
        }
        if entry.promoted_relative_path != entry.original_relative_path {
            reject_overlapping_manifest_entry(&owners, entry.root, &entry.promoted_relative_path)?;
            let promoted_key = (entry.root, entry.promoted_relative_path.clone());
            if !owners.insert(promoted_key) {
                return Err(invalid_publication(format!(
                    "duplicate publication artifact ownership '{}'",
                    entry.promoted_relative_path.display()
                )));
            }
        }
    }
    Ok(())
}

fn reject_overlapping_manifest_entry(
    owners: &BTreeSet<(PublicationArtifactRoot, PathBuf)>,
    root: PublicationArtifactRoot,
    relative_path: &Path,
) -> Result<()> {
    if owners.iter().any(|(owned_root, owned_path)| {
        *owned_root == root
            && (owned_path.starts_with(relative_path) || relative_path.starts_with(owned_path))
    }) {
        return Err(invalid_publication(format!(
            "overlapping publication artifact ownership '{}'",
            relative_path.display()
        )));
    }
    Ok(())
}

fn validate_manifest_entry(entry: &PublicationArtifactManifestEntry) -> Result<()> {
    validate_relative_path(
        "publication artifact original path",
        &entry.original_relative_path,
    )?;
    validate_relative_path(
        "publication artifact promoted path",
        &entry.promoted_relative_path,
    )?;
    if !entry.root_path.as_os_str().is_empty() {
        reject_path_escape(&entry.root_path, &entry.original_relative_path)?;
        reject_path_escape(&entry.root_path, &entry.promoted_relative_path)?;
        reject_symlinked_manifest_path(&entry.root_path, &entry.original_relative_path)?;
        reject_symlinked_manifest_path(&entry.root_path, &entry.promoted_relative_path)?;
    }
    let policy = artifact_policy_table()
        .iter()
        .find(|policy| policy.key == entry.policy_key)
        .ok_or_else(|| invalid_publication("unknown publication artifact policy"))?;
    if policy.disposition != ArtifactDisposition::Journal {
        return Err(invalid_publication(
            "manifest entry must use a journaled artifact policy",
        ));
    }
    let root = match entry.root {
        PublicationArtifactRoot::QuerySuggestions => ExternalArtifactRoot::QuerySuggestions,
        PublicationArtifactRoot::Analytics => ExternalArtifactRoot::Analytics,
    };
    let known = vec![
        entry.original_relative_path.clone(),
        entry.promoted_relative_path.clone(),
    ];
    classify_external_relative_path(root, &entry.original_relative_path, &known)?;
    classify_external_relative_path(root, &entry.promoted_relative_path, &known)?;
    Ok(())
}

fn populate_manifest_digests(
    manifest: &mut PublicationArtifactManifest,
    io: &PublicationIo<'_>,
) -> Result<()> {
    for entry in &mut manifest.entries {
        entry.original_digest =
            digest_existing_artifact(&entry.root_path.join(&entry.original_relative_path), io)?;
        entry.promoted_digest =
            digest_existing_artifact(&entry.root_path.join(&entry.promoted_relative_path), io)?;
    }
    Ok(())
}

fn digest_existing_artifact(path: &Path, io: &PublicationIo<'_>) -> Result<Option<ContentDigest>> {
    if !path.exists() {
        return Ok(None);
    }
    io.before_digest(path)?;
    artifact_digest(path).map(Some)
}

pub(super) fn artifact_digest(path: &Path) -> Result<ContentDigest> {
    let mut records = Vec::new();
    collect_artifact_digest_records(path, path, &mut records)?;
    records.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let mut hasher = Sha256::new();
    for record in records {
        hasher.update((record.relative_path.len() as u64).to_be_bytes());
        hasher.update(record.relative_path.as_bytes());
        hasher.update([record.entry_type]);
        hasher.update((record.bytes.len() as u64).to_be_bytes());
        hasher.update(&record.bytes);
    }
    ContentDigest::new(format!("sha256:{:x}", hasher.finalize()))
}

fn artifact_matches_digest(
    path: &Path,
    expected: Option<&ContentDigest>,
    io: &PublicationIo<'_>,
) -> Result<bool> {
    if !path.exists() || expected.is_none() {
        return Ok(false);
    }
    io.before_digest(path)?;
    Ok(Some(&artifact_digest(path)?) == expected)
}

struct ArtifactDigestRecord {
    relative_path: String,
    entry_type: u8,
    bytes: Vec<u8>,
}

fn collect_artifact_digest_records(
    root: &Path,
    current: &Path,
    records: &mut Vec<ArtifactDigestRecord>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(current)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid_publication(format!(
            "refusing symlink publication artifact '{}'",
            current.display()
        )));
    }
    let relative = current.strip_prefix(root).unwrap_or(current);
    let key = if relative.as_os_str().is_empty() {
        ".".to_string()
    } else {
        relative
            .to_str()
            .map(|value| value.replace('\\', "/"))
            .ok_or_else(|| invalid_publication("publication artifact path is not UTF-8"))?
    };
    if metadata.is_dir() {
        records.push(ArtifactDigestRecord {
            relative_path: key,
            entry_type: b'd',
            bytes: Vec::new(),
        });
        for entry in fs::read_dir(current)? {
            collect_artifact_digest_records(root, &entry?.path(), records)?;
        }
    } else if metadata.is_file() {
        records.push(ArtifactDigestRecord {
            relative_path: key,
            entry_type: b'f',
            bytes: fs::read(current)?,
        });
    } else {
        return Err(invalid_publication(format!(
            "unsupported publication artifact '{}'",
            current.display()
        )));
    }
    Ok(())
}

fn reject_path_escape(root: &Path, relative: &Path) -> Result<()> {
    let joined = root.join(relative);
    if !joined.starts_with(root) {
        return Err(invalid_publication(format!(
            "publication artifact path '{}' escapes root '{}'",
            relative.display(),
            root.display()
        )));
    }
    Ok(())
}

fn reject_symlinked_manifest_path(root: &Path, relative: &Path) -> Result<()> {
    super::fsops::reject_symlinked_managed_path_components(
        root,
        &root.join(relative),
        "publication artifact",
    )?;
    Ok(())
}

fn sync_tree(path: &Path, io: &PublicationIo<'_>) -> Result<()> {
    let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let child = entry.path();
        let metadata = fs::symlink_metadata(&child)?;
        if metadata.is_dir() {
            sync_tree(&child, io)?;
        } else if metadata.is_file() {
            io.sync_file(&child)?;
        }
    }
    io.sync_dir(path)?;
    Ok(())
}

pub(super) fn copy_path_durably(from: &Path, to: &Path, io: &PublicationIo<'_>) -> Result<()> {
    let metadata = fs::symlink_metadata(from)?;
    if metadata.file_type().is_symlink() {
        return Err(invalid_publication(format!(
            "refusing symlink publication artifact '{}'",
            from.display()
        )));
    }
    if metadata.is_dir() {
        copy_dir_durably(from, to, io)
    } else if metadata.is_file() {
        copy_file_durably(from, to, io)
    } else {
        Err(invalid_publication(format!(
            "unsupported publication artifact '{}'",
            from.display()
        )))
    }
}

fn copy_dir_durably(from: &Path, to: &Path, io: &PublicationIo<'_>) -> Result<()> {
    io.create_dir_all(to)?;
    io.sync_dir(to)?;
    let mut entries = fs::read_dir(from)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        copy_path_durably(&entry.path(), &to.join(entry.file_name()), io)?;
    }
    io.sync_dir(to)?;
    io.sync_dir(require_parent(to)?)?;
    Ok(())
}

fn copy_file_durably(from: &Path, to: &Path, io: &PublicationIo<'_>) -> Result<()> {
    let parent = require_parent(to)?;
    io.create_dir_all(parent)?;
    io.copy_file(from, to)?;
    io.sync_file(to)?;
    io.sync_dir(parent)?;
    Ok(())
}

pub(super) fn rename_path(from: &Path, to: &Path, io: &PublicationIo<'_>) -> Result<()> {
    io.rename(from, to)
}

pub(super) fn remove_path_if_exists(path: &Path, io: &PublicationIo<'_>) -> Result<()> {
    io.remove_if_exists(path)
}

pub(super) fn cleanup_publication_residue(
    paths: &PublicationPaths,
    io: &PublicationIo<'_>,
) -> Result<()> {
    io.checkpoint(PublicationFaultPoint::DuringCleanup)?;
    io.remove_if_exists(&paths.staging)?;
    io.remove_if_exists(&sidecar_residue_root(paths))?;
    io.remove_if_exists(&paths.backup)?;
    Ok(())
}

fn require_parent(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| invalid_publication(format!("path '{}' has no parent", path.display())))
}

fn root_key(root: PublicationArtifactRoot) -> &'static str {
    match root {
        PublicationArtifactRoot::QuerySuggestions => "query_suggestions",
        PublicationArtifactRoot::Analytics => "analytics",
    }
}

pub(super) fn sidecar_residue_root(paths: &PublicationPaths) -> PathBuf {
    paths
        .journal
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("sidecars")
}

fn query_suggestions_manifest_entries(
    original: QsTargetArtifactPaths,
    promoted: QsTargetArtifactPaths,
) -> Result<Vec<PublicationArtifactManifestEntry>> {
    let pairs = [
        (original.config_path, promoted.config_path),
        (original.log_path, promoted.log_path),
        (original.status_path, promoted.status_path),
    ];
    pairs
        .into_iter()
        .map(|(original_path, promoted_path)| {
            journaled_resolved_entry(
                "query_suggestions",
                PublicationArtifactRoot::QuerySuggestions,
                &original.root_dir,
                original_path,
                promoted_path,
            )
        })
        .collect()
}

fn analytics_manifest_entry(
    original: AnalyticsTargetArtifactPaths,
    promoted: AnalyticsTargetArtifactPaths,
) -> Result<PublicationArtifactManifestEntry> {
    journaled_resolved_entry(
        "analytics",
        PublicationArtifactRoot::Analytics,
        &original.data_dir,
        original.index_root,
        promoted.index_root,
    )
}

fn journaled_resolved_entry(
    policy_key: &'static str,
    root: PublicationArtifactRoot,
    root_path: &Path,
    original_path: PathBuf,
    promoted_path: PathBuf,
) -> Result<PublicationArtifactManifestEntry> {
    Ok(PublicationArtifactManifestEntry::journaled(
        policy_key,
        root,
        strip_resolved_root(root_path, &original_path)?,
        strip_resolved_root(root_path, &promoted_path)?,
        root_path.to_path_buf(),
    ))
}

fn strip_resolved_root(root_path: &Path, path: &Path) -> Result<PathBuf> {
    let relative = path.strip_prefix(root_path).map_err(|_| {
        invalid_publication(format!(
            "resolved publication artifact '{}' escapes root '{}'",
            path.display(),
            root_path.display()
        ))
    })?;
    validate_relative_path("resolved publication artifact path", relative)?;
    Ok(relative.to_path_buf())
}
