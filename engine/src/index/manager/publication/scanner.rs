use super::fsops::reject_symlinked_managed_path_components;
use super::repair::repair_publication_outcome;
use super::{
    invalid_publication, PublicationArtifactManifest, PublicationJournal, PublicationPaths,
    PublicationPhase, PublicationTarget, PublicationTransactionId, RepairDecision, Result,
    TantivyManagedInventory,
};
use crate::analytics::AnalyticsConfig;
use serde::{Serialize, Serializer};
use std::io;
use std::path::{Path, PathBuf};

/// One deterministic action selected for a publication target during startup repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationScanAction {
    Clean,
    Repaired(RepairDecision),
    Quarantined,
    Unresolved,
}

impl Serialize for PublicationScanAction {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl PublicationScanAction {
    /// Stable value shared by JSON and operator-facing text reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "none",
            Self::Repaired(RepairDecision::None) => "none",
            Self::Repaired(RepairDecision::Complete) => "complete",
            Self::Repaired(RepairDecision::Rollback) => "rollback",
            Self::Repaired(RepairDecision::Cleanup) => "cleanup",
            Self::Repaired(RepairDecision::Quarantine) | Self::Quarantined => "quarantine",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Stable CLI-facing classification of a target-scoped repair result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationRepairStatus {
    Clean,
    Repaired,
    Quarantined,
    Unresolved,
}

impl PublicationRepairStatus {
    /// Stable value shared by JSON and operator-facing text reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Repaired => "repaired",
            Self::Quarantined => "quarantined",
            Self::Unresolved => "unresolved",
        }
    }
}

/// Whether the repair evidence proves that the live target may be loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationTargetDisposition {
    Loadable,
    Unavailable,
}

/// Startup repair result for exactly one publication target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PublicationRepairReport {
    #[serde(rename = "tenant")]
    pub target: PublicationTarget,
    #[serde(skip)]
    pub transactions: Vec<PublicationTransactionId>,
    pub status: PublicationRepairStatus,
    pub action: PublicationScanAction,
    pub transaction_id: Option<PublicationTransactionId>,
    pub phase: Option<PublicationPhase>,
    pub evidence: Option<PathBuf>,
    #[serde(skip)]
    pub disposition: PublicationTargetDisposition,
    #[serde(skip)]
    pub live_target_mutated: bool,
}

enum TargetTransactionDiscovery {
    MissingNamespace,
    Present(Vec<PublicationTransactionId>),
}

/// Discover publication targets in stable lexical order without mutating storage.
pub fn publication_scan_targets(base: &Path) -> Result<Vec<PublicationTarget>> {
    let Some(root) = publication_root(base)? else {
        return Ok(Vec::new());
    };
    let mut targets = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().into_string().map_err(|_| {
                invalid_publication("publication target directory name is not UTF-8")
            })?;
            targets.push(PublicationTarget::new(name)?);
        }
    }
    targets.sort_by(|left, right| left.as_str().cmp(right.as_str()));
    Ok(targets)
}

/// Scan and repair every node-local publication transaction in stable target order.
pub fn scan_and_repair_publications(
    base: &Path,
    analytics: &AnalyticsConfig,
) -> Result<Vec<PublicationRepairReport>> {
    publication_scan_targets(base)?
        .into_iter()
        .map(|target| scan_and_repair_publication_target(base, analytics, target))
        .collect()
}

/// Repair and report exactly one publication target without inventing evidence.
pub fn scan_and_repair_publication_target(
    base: &Path,
    analytics: &AnalyticsConfig,
    target: PublicationTarget,
) -> Result<PublicationRepairReport> {
    let discovery = target_transactions(base, &target)?;
    let transactions = match discovery {
        TargetTransactionDiscovery::MissingNamespace => {
            if clean_target_paths_are_proven(base, &target)? {
                return Ok(PublicationRepairReport {
                    target,
                    transactions: Vec::new(),
                    status: PublicationRepairStatus::Clean,
                    action: PublicationScanAction::Clean,
                    transaction_id: None,
                    phase: None,
                    evidence: None,
                    disposition: PublicationTargetDisposition::Loadable,
                    live_target_mutated: false,
                });
            }
            return Ok(unresolved_target_report(target, Vec::new(), None));
        }
        TargetTransactionDiscovery::Present(transactions) => transactions,
    };
    if transactions.is_empty() {
        return Ok(unresolved_target_report(
            target.clone(),
            transactions,
            Some(target_namespace_evidence(&target)),
        ));
    }
    if transactions.len() != 1 {
        return Ok(unresolved_target_report(
            target.clone(),
            transactions,
            Some(target_namespace_evidence(&target)),
        ));
    }
    let transaction = transactions[0].clone();
    let paths = PublicationPaths::new(base, &target, &transaction);
    reject_symlinked_managed_path_components(base, &paths.journal, "publication scan evidence")?;
    let phase = std::fs::read_to_string(&paths.journal)
        .ok()
        .and_then(|raw| PublicationJournal::from_json(&raw).ok())
        .map(|journal| journal.phase);
    let evidence = paths
        .journal
        .parent()
        .and_then(|path| path.strip_prefix(base).ok())
        .map(Path::to_path_buf);
    validate_target_scan_roots(base, &paths)?;
    let manifest = resolved_manifest(base, analytics, &target, &transaction, &paths);
    let inventory = TantivyManagedInventory::from_existing_trees([
        paths.target.as_path(),
        paths.staging.as_path(),
        paths.backup.as_path(),
    ])?;
    let outcome = repair_publication_outcome(
        base,
        target.clone(),
        transaction.clone(),
        manifest,
        &inventory,
    )?;
    let action = match outcome.decision {
        RepairDecision::None => PublicationScanAction::Clean,
        RepairDecision::Quarantine => PublicationScanAction::Quarantined,
        decision => PublicationScanAction::Repaired(decision),
    };
    let status = match action {
        PublicationScanAction::Clean => PublicationRepairStatus::Clean,
        PublicationScanAction::Repaired(_) => PublicationRepairStatus::Repaired,
        PublicationScanAction::Quarantined => PublicationRepairStatus::Quarantined,
        PublicationScanAction::Unresolved => PublicationRepairStatus::Unresolved,
    };
    Ok(PublicationRepairReport {
        target,
        transactions: transactions.clone(),
        status,
        action,
        transaction_id: Some(transaction),
        phase,
        evidence,
        disposition: if outcome.live_target_proven {
            PublicationTargetDisposition::Loadable
        } else {
            PublicationTargetDisposition::Unavailable
        },
        live_target_mutated: outcome.live_target_mutated,
    })
}

fn unresolved_target_report(
    target: PublicationTarget,
    transactions: Vec<PublicationTransactionId>,
    evidence: Option<PathBuf>,
) -> PublicationRepairReport {
    PublicationRepairReport {
        target,
        transactions,
        status: PublicationRepairStatus::Unresolved,
        action: PublicationScanAction::Unresolved,
        transaction_id: None,
        phase: None,
        evidence,
        disposition: PublicationTargetDisposition::Unavailable,
        live_target_mutated: false,
    }
}

fn target_namespace_evidence(target: &PublicationTarget) -> PathBuf {
    PathBuf::from(super::PUBLICATION_DIR).join(target.as_str())
}

fn clean_target_paths_are_proven(base: &Path, target: &PublicationTarget) -> Result<bool> {
    for path in [
        base.join(target.as_str()),
        base.join(super::PUBLICATION_DIR).join(target.as_str()),
    ] {
        match reject_symlinked_managed_path_components(base, &path, "publication repair managed") {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::InvalidInput => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

fn validate_target_scan_roots(base: &Path, paths: &PublicationPaths) -> Result<()> {
    for path in [&paths.target, &paths.staging, &paths.backup] {
        reject_symlinked_managed_path_components(base, path, "publication repair managed")?;
    }
    Ok(())
}

fn target_transactions(
    base: &Path,
    target: &PublicationTarget,
) -> Result<TargetTransactionDiscovery> {
    let Some(publication_root) = publication_root(base)? else {
        return Ok(TargetTransactionDiscovery::MissingNamespace);
    };
    let root = publication_root.join(target.as_str());
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(TargetTransactionDiscovery::MissingNamespace);
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Ok(TargetTransactionDiscovery::Present(Vec::new()));
    }
    let mut transactions = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let name = entry.file_name().into_string().map_err(|_| {
                invalid_publication("publication transaction directory name is not UTF-8")
            })?;
            transactions.push(PublicationTransactionId::new(name)?);
        }
    }
    transactions.sort();
    Ok(TargetTransactionDiscovery::Present(transactions))
}

fn publication_root(base: &Path) -> Result<Option<PathBuf>> {
    let root = base.join(super::PUBLICATION_DIR);
    reject_symlinked_managed_path_components(base, &root, "publication root")?;
    let metadata = match std::fs::symlink_metadata(&root) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_dir() {
        return Err(invalid_publication("publication root is not a directory"));
    }
    Ok(Some(root))
}

fn resolved_manifest(
    base: &Path,
    analytics: &AnalyticsConfig,
    target: &PublicationTarget,
    transaction: &PublicationTransactionId,
    paths: &PublicationPaths,
) -> PublicationArtifactManifest {
    std::fs::read_to_string(&paths.journal)
        .ok()
        .and_then(|raw| PublicationJournal::from_json(&raw).ok())
        .and_then(|journal| {
            PublicationArtifactManifest::resolve_for_repair(
                base,
                analytics,
                target,
                transaction,
                &journal.artifact_manifest,
            )
            .ok()
        })
        .unwrap_or_default()
}
