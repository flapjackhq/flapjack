//! Stub summary for engine/src/index/manager/publication.rs.

use crate::error::{FlapjackError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use fault::PublicationIo;

// Schema version 2 carries ADR 0008 admission-incarnation fence evidence. Version
// 1 predates that evidence entirely and is refused on read (see `from_json`).
const SCHEMA_VERSION: u32 = 2;
const LEGACY_PRE_FENCE_SCHEMA_VERSION: u32 = 1;
const PUBLICATION_DIR: &str = ".publication";
const QUARANTINE_DIR: &str = ".publication_quarantine";
const CRAWLER_TOMBSTONE_DIR: &str = ".crawler_run_tombstones";
const CRAWLER_TOMBSTONE_LOCK_FILE: &str = ".transition.lock";
const ANALYTICS_PURGE_PENDING_FILE: &str = "analytics-purge-pending.json";
const NODE_LOCAL_GUARANTEE: &str =
    "NODE-LOCAL publication contract for one node only; it cannot make HA peers converge.";

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AnalyticsPurgeMarker {
    schema_version: u32,
    target: String,
}

fn cancel_reservation_digest(run_id: &str) -> Result<ContentDigest> {
    ContentDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(format!(
            "crawler-cancel-reservation:{run_id}"
        )))
    ))
}

mod digest;
mod epoch;
#[cfg(test)]
mod epoch_tests;
mod executor;
mod fault;
mod fsops;
mod inventory;
mod policy;
mod repair;
#[cfg(test)]
mod repair_deletion_tests;
mod scanner;
#[cfg(test)]
mod scanner_tests;
pub use digest::canonical_tenant_tree_digest;
#[cfg(test)]
pub(crate) use epoch::set_publication_epoch_open_lock_file_checkpoint_hook_for_test;
pub(crate) use epoch::{
    capture_publication_epoch, run_if_publication_admission_unfenced,
    try_validate_publication_epoch_admission,
};
pub use epoch::{
    compare_and_advance_publication_epoch, fence_publication_admission,
    publication_admission_is_fenced, publication_epoch_paths_for_target_path,
    read_publication_epoch, PublicationAdmissionFence, PublicationEpoch,
    PublicationEpochAdmissionError, PublicationEpochAdmissionGuard, PublicationEpochError,
    PublicationEpochFence, PublicationEpochPaths,
};
pub(crate) use executor::retire_committed_publication_journals;
pub use executor::{
    abort_unjournaled_publication, activate_publication, activate_publication_with_fence,
    PreStagedActivationError, PreStagedActivationStage, PreStagedPublication,
    PublicationActivationInputs, PublicationArtifactManifest, PublicationArtifactManifestEntry,
    PublicationArtifactPlan, PublicationArtifactRoot,
};
#[cfg(test)]
pub(crate) use executor::{
    activate_publication_for_test, activate_publication_with_faults_for_test,
    activate_publication_with_fence_and_faults_for_test,
};
#[cfg(any(test, feature = "test-support"))]
pub use fault::PublicationFaultPoint;
#[cfg(test)]
pub(crate) use fault::{
    PublicationCheckpoint, PublicationFaultHook, PublicationFaultScript, PublicationOperation,
};
pub use fsops::{
    fsync_dir, fsync_file, reject_symlinked_managed_path, rename_with_transient_retry,
};
pub use policy::{artifact_policy_table, ArtifactDisposition, ArtifactPolicy};
pub use repair::{
    decide_publication_repair, repair_publication, RepairArtifactEvidence, RepairDecision,
    RepairEpochEvidence, RepairEvidence, RepairJournalEvidence,
};
#[cfg(test)]
pub(crate) use repair::{repair_publication_for_test, repair_publication_with_faults_for_test};
pub(crate) use scanner::publication_target_has_repair_candidate;
pub(crate) use scanner::scan_and_repair_publication_target_while_fenced;
pub use scanner::{
    publication_scan_targets, scan_and_repair_publication_target, scan_and_repair_publications,
    PublicationRepairReport, PublicationRepairStatus, PublicationScanAction,
    PublicationTargetDisposition,
};

/// NODE-LOCAL transaction identifier for one staged publication.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct PublicationTransactionId(String);

impl PublicationTransactionId {
    /// NODE-LOCAL constructor for opaque transaction IDs.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_opaque_component("publication transaction ID", &value)?;
        Ok(Self(value))
    }

    /// NODE-LOCAL string view.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// NODE-LOCAL validated publication target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationTarget(String);

impl PublicationTarget {
    /// NODE-LOCAL constructor that delegates tenant validation to IndexManager.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        super::validate_index_name(&value)?;
        Ok(Self(value))
    }

    /// NODE-LOCAL target name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) fn analytics_purge_is_pending(base: &Path, target: &PublicationTarget) -> Result<bool> {
    let path = analytics_purge_pending_path(base, target);
    fsops::reject_symlinked_managed_path_components(base, &path, "analytics purge marker")
        .map_err(|error| invalid_publication(error.to_string()))?;
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() => {
            let raw = std::fs::read_to_string(&path)?;
            let marker: AnalyticsPurgeMarker = serde_json::from_str(&raw).map_err(|error| {
                invalid_publication(format!("invalid analytics purge marker: {error}"))
            })?;
            if marker.schema_version != 1 || marker.target != target.as_str() {
                return Err(invalid_publication(
                    "analytics purge marker identity does not match its target",
                ));
            }
            Ok(true)
        }
        Ok(_) => Err(invalid_publication(
            "analytics purge marker is not a regular file",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn mark_analytics_purge_pending(base: &Path, target: &PublicationTarget) -> Result<()> {
    let path = analytics_purge_pending_path(base, target);
    let parent = path
        .parent()
        .ok_or_else(|| invalid_publication("analytics purge marker has no parent"))?;
    fsops::reject_symlinked_managed_path_components(base, parent, "analytics purge marker")
        .map_err(|error| invalid_publication(error.to_string()))?;
    std::fs::create_dir_all(parent)?;
    atomic_write_json(
        &path,
        &AnalyticsPurgeMarker {
            schema_version: 1,
            target: target.as_str().to_string(),
        },
    )
}

pub(crate) fn clear_analytics_purge_pending(base: &Path, target: &PublicationTarget) -> Result<()> {
    let path = analytics_purge_pending_path(base, target);
    fsops::reject_symlinked_managed_path_components(base, &path, "analytics purge marker")
        .map_err(|error| invalid_publication(error.to_string()))?;
    match std::fs::remove_file(&path) {
        Ok(()) => {
            fsops::fsync_dir(
                path.parent()
                    .ok_or_else(|| invalid_publication("analytics purge marker has no parent"))?,
            )?;
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn analytics_purge_pending_path(base: &Path, target: &PublicationTarget) -> PathBuf {
    base.join(PUBLICATION_DIR)
        .join(target.as_str())
        .join(ANALYTICS_PURGE_PENDING_FILE)
}

/// NODE-LOCAL publication path namespace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationPaths {
    pub target: PathBuf,
    pub staging: PathBuf,
    pub backup: PathBuf,
    pub journal: PathBuf,
    pub quarantine: PathBuf,
}

impl PublicationPaths {
    /// NODE-LOCAL deterministic path constructor.
    pub fn new(
        base: &Path,
        target: &PublicationTarget,
        transaction: &PublicationTransactionId,
    ) -> Self {
        let namespace = base
            .join(PUBLICATION_DIR)
            .join(target.as_str())
            .join(transaction.as_str());
        Self {
            target: base.join(target.as_str()),
            staging: namespace.join("staging"),
            backup: namespace.join("backup"),
            journal: namespace.join("journal.json"),
            quarantine: base
                .join(QUARANTINE_DIR)
                .join(target.as_str())
                .join(transaction.as_str()),
        }
    }
}

/// Return true when a relative path is owned by the node-local publication namespace.
pub fn is_reserved_publication_namespace(path: &Path) -> bool {
    let Some(first_component) = first_safe_relative_component(path) else {
        return false;
    };

    first_component == PUBLICATION_DIR
        || first_component == QUARANTINE_DIR
        || first_component == CRAWLER_TOMBSTONE_DIR
}

/// TODO: Document first_safe_relative_component.
fn first_safe_relative_component(path: &Path) -> Option<&std::ffi::OsStr> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return None;
    }

    let mut components = path.components();
    let first = match components.next()? {
        Component::Normal(part) if !part.is_empty() => part,
        _ => return None,
    };

    if components.any(|component| !matches!(component, Component::Normal(part) if !part.is_empty()))
    {
        return None;
    }

    Some(first)
}

/// NODE-LOCAL caller-supplied generation evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationGenerationEvidence(String);

impl PublicationGenerationEvidence {
    /// NODE-LOCAL constructor for opaque generation evidence.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_opaque_component("publication generation evidence", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub fn verify_current_generation_evidence(
    base: &Path,
    target: &PublicationTarget,
    expected_generation: &PublicationGenerationEvidence,
) -> Result<()> {
    let journals = committed_generation_journals(base, target)?;
    if journals.len() != 1 {
        return Err(invalid_publication(format!(
            "expected exactly one committed current journal for target '{}', found {}",
            target.as_str(),
            journals.len()
        )));
    }
    let journal = &journals[0];
    if &journal.generation != expected_generation {
        return Err(invalid_publication(format!(
            "stale generation evidence for target '{}'",
            target.as_str()
        )));
    }
    Ok(())
}

fn committed_generation_journals(
    base: &Path,
    target: &PublicationTarget,
) -> Result<Vec<PublicationJournal>> {
    let root = base.join(PUBLICATION_DIR).join(target.as_str());
    fsops::reject_symlinked_managed_path_components(base, &root, "publication generation evidence")
        .map_err(|error| invalid_publication(error.to_string()))?;
    let mut journals = Vec::new();
    let mut non_committed_journal_seen = false;
    let entries = std::fs::read_dir(&root).map_err(|error| {
        invalid_publication(format!(
            "missing current journal for target '{}': {error}",
            target.as_str()
        ))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            invalid_publication(format!(
                "could not read current journal namespace for target '{}': {error}",
                target.as_str()
            ))
        })?;
        if !entry
            .file_type()
            .map_err(|error| {
                invalid_publication(format!(
                    "could not inspect current journal namespace for target '{}': {error}",
                    target.as_str()
                ))
            })?
            .is_dir()
        {
            continue;
        }
        let journal_path = entry.path().join("journal.json");
        fsops::reject_symlinked_managed_path_components(
            base,
            &journal_path,
            "publication generation evidence",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        let raw = match std::fs::read_to_string(&journal_path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(invalid_publication(format!(
                    "could not read current journal for target '{}': {error}",
                    target.as_str()
                )))
            }
        };
        let journal = PublicationJournal::from_json(&raw)?;
        if journal.target != *target {
            return Err(invalid_publication(format!(
                "current journal target mismatch for '{}'",
                target.as_str()
            )));
        }
        if journal.phase == PublicationPhase::Committed
            && journal.disposition == Some(PublicationDisposition::Committed)
        {
            journals.push(journal);
        } else {
            non_committed_journal_seen = true;
        }
    }
    if non_committed_journal_seen {
        return Err(invalid_publication(format!(
            "current journal for target '{}' is not committed",
            target.as_str()
        )));
    }
    Ok(journals)
}

/// NODE-LOCAL staging-baseline evidence (ADR 0008): the committed sequence the
/// replacement baseline was carried forward from. It must never exceed the drained
/// watermark `W`, since staging cannot hold effects past the drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationStagingBaseline(pub u64);

impl PublicationStagingBaseline {
    /// NODE-LOCAL constructor for a staging-baseline sequence.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// NODE-LOCAL baseline sequence value.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// NODE-LOCAL drained-watermark evidence (ADR 0008 `W`): the old generation's
/// committed sequence captured after the drain and reproduced as the staged
/// `committed_seq`. It is carried as evidence only; this stage never derives it
/// from a live drain, `OpLog`, or `committed_seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationWatermark(pub u64);

impl PublicationWatermark {
    /// NODE-LOCAL constructor for a drained-watermark sequence.
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    /// NODE-LOCAL watermark sequence value.
    pub fn value(self) -> u64 {
        self.0
    }
}

/// NODE-LOCAL ADR 0008 admission-incarnation fence evidence for one activation:
/// the old and replacement incarnation epochs, the staging baseline, and the
/// drained watermark `W`. The generation stays owned by the journal's existing
/// `generation` field, so it is not duplicated here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationFenceEvidence {
    epoch_old: PublicationEpoch,
    epoch_new: PublicationEpoch,
    staging_baseline: PublicationStagingBaseline,
    watermark: PublicationWatermark,
}

impl PublicationFenceEvidence {
    /// NODE-LOCAL constructor that validates the fence-evidence invariants before
    /// the evidence can become durable: the replacement epoch is exactly one past
    /// the old incarnation (`E_new = E_old + 1`), and the staging baseline is at or
    /// below the drained watermark `W`.
    pub fn new(
        epoch_old: PublicationEpoch,
        epoch_new: PublicationEpoch,
        staging_baseline: PublicationStagingBaseline,
        watermark: PublicationWatermark,
    ) -> Result<Self> {
        let evidence = Self {
            epoch_old,
            epoch_new,
            staging_baseline,
            watermark,
        };
        evidence.validate()?;
        Ok(evidence)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        let expected_new = self
            .epoch_old
            .0
            .checked_add(1)
            .ok_or_else(|| invalid_publication("publication fence epoch would overflow u64"))?;
        if self.epoch_new.0 != expected_new {
            return Err(invalid_publication(
                "publication fence replacement epoch must be exactly one past the old epoch",
            ));
        }
        if self.staging_baseline.0 > self.watermark.0 {
            return Err(invalid_publication(
                "publication fence staging baseline cannot exceed the drained watermark",
            ));
        }
        Ok(())
    }

    pub fn epoch_old(&self) -> PublicationEpoch {
        self.epoch_old
    }

    pub fn epoch_new(&self) -> PublicationEpoch {
        self.epoch_new
    }

    pub fn staging_baseline(&self) -> PublicationStagingBaseline {
        self.staging_baseline
    }

    pub fn watermark(&self) -> PublicationWatermark {
        self.watermark
    }
}

/// NODE-LOCAL content digest evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// NODE-LOCAL constructor for canonical SHA-256 digest evidence.
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        let Some(hex) = value.strip_prefix("sha256:") else {
            return Err(invalid_publication("digest must use sha256:<hex> format"));
        };
        if hex.len() != 64 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(invalid_publication(
                "digest must contain 64 hexadecimal characters",
            ));
        }
        Ok(Self(value))
    }

    /// NODE-LOCAL digest string view.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Durable bounded crawler counters carried by terminal publication truth.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlerRunCountersEvidence {
    pub fetched: u32,
    pub discovered: u32,
    pub transformed: u32,
    pub published: u32,
}

/// Closed safe failure vocabulary for durable crawler truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CrawlerRunErrorCodeEvidence {
    TargetRejected,
    DnsResolutionFailed,
    RedirectRejected,
    ContentTypeRejected,
    FetchTimeout,
    BodyLimitExceeded,
    CrawlLimitExceeded,
    TransformInvalid,
    TransformFailed,
    PublicationFailed,
    DeadlineExceeded,
    WorkerLost,
    InternalFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CrawlerTerminalOutcome {
    Succeeded,
    Canceled,
    Failed {
        error_code: CrawlerRunErrorCodeEvidence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlerPublicationFactEvidence {
    pub destination_index: String,
    pub task_id: i64,
    pub transaction_id: PublicationTransactionId,
    pub generation: PublicationGenerationEvidence,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlerTerminalEvidence {
    pub run_id: String,
    pub request_digest: ContentDigest,
    pub outcome: CrawlerTerminalOutcome,
    pub counters: CrawlerRunCountersEvidence,
    pub duration_ms: u64,
    pub terminal_at_unix_ms: u64,
    pub acknowledged_at_unix_ms: Option<u64>,
    pub publication: Option<CrawlerPublicationFactEvidence>,
}

impl CrawlerTerminalEvidence {
    fn validate(&self) -> Result<()> {
        validate_opaque_component("crawler run ID", &self.run_id)?;
        ContentDigest::new(self.request_digest.as_str())?;
        if let Some(publication) = &self.publication {
            PublicationTarget::new(publication.destination_index.clone())?;
            PublicationTransactionId::new(publication.transaction_id.as_str())?;
            PublicationGenerationEvidence::new(publication.generation.as_str())?;
            ContentDigest::new(publication.digest.as_str())?;
        }
        match (&self.outcome, &self.publication) {
            (CrawlerTerminalOutcome::Succeeded, Some(_)) => Ok(()),
            (CrawlerTerminalOutcome::Canceled, None)
            | (CrawlerTerminalOutcome::Failed { .. }, None) => Ok(()),
            _ => Err(invalid_publication(
                "crawler terminal outcome and publication evidence mismatch",
            )),
        }
    }
}

/// Inputs known before activation; the publication owner fills identity and
/// digest evidence from the committed journal itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlerPublicationCompletion {
    pub run_id: String,
    pub request_digest: ContentDigest,
    pub counters: CrawlerRunCountersEvidence,
    pub duration_ms: u64,
    pub terminal_at_unix_ms: u64,
    pub task_id: i64,
}

impl CrawlerPublicationCompletion {
    fn validate(&self) -> Result<()> {
        validate_opaque_component("crawler run ID", &self.run_id)?;
        ContentDigest::new(self.request_digest.as_str())?;
        Ok(())
    }
}

/// Exclusive proof that a crawler run is still the one live, uncanceled owner
/// of a publication. The file lock remains held until activation either commits
/// or rolls back, so cancellation and replay cannot interleave with target
/// effects after this proof is taken.
#[derive(Debug)]
pub struct CrawlerPublicationAdmission {
    base: PathBuf,
    completion: CrawlerPublicationCompletion,
    deadline: Instant,
    _transition_lock: File,
}

impl CrawlerPublicationAdmission {
    fn validate_deadline(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(invalid_publication(
                "crawler publication deadline was exceeded before target effects",
            ));
        }
        Ok(())
    }

    fn validate_before_target_effects(&self) -> Result<()> {
        self.validate_deadline()?;
        CrawlerRunStore::new(&self.base).validate_publication_candidate_unlocked(&self.completion)
    }
}

/// Result of the one atomic start transition. Only `Started` authorizes a new
/// fetch; every other disposition is durable truth that the caller replays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrawlerRunStartDisposition {
    Started(PublicationTombstone),
    Replay(PublicationTombstone),
    Canceled(PublicationTombstone),
}

#[derive(Debug)]
pub enum CrawlerRunStartError {
    AdmissionRejected,
    Conflict,
    Capacity,
    Internal(FlapjackError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrawlerRunCancelDispositionEvidence {
    CancelRequested,
    AlreadyRequested,
    AlreadyTerminal,
}

#[derive(Debug)]
pub enum CrawlerRunAcknowledgeError {
    NotFound,
    NotTerminal,
    Internal(FlapjackError),
}

#[derive(Debug)]
pub struct CrawlerRunExecutionClaim {
    _lock: File,
}

#[derive(Debug)]
pub enum CrawlerRunExecutionClaimDisposition {
    Acquired(CrawlerRunExecutionClaim),
    AlreadyExecuting,
    NotRunnable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlerRunTombstone {
    pub run_id: String,
    pub request_digest: Option<ContentDigest>,
    pub started_at_unix_ms: Option<u64>,
    /// Durable wall-clock deadline used only to recover an unowned run after a
    /// process/task loss. Live workers still enforce a monotonic deadline.
    #[serde(default)]
    pub deadline_at_unix_ms: Option<u64>,
    pub cancel_requested_at_unix_ms: Option<u64>,
    pub terminal: Option<CrawlerTerminalEvidence>,
}

/// NODE-LOCAL journal phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationPhase {
    Prepared,
    Committed,
    RolledBack,
    Quarantined,
}

impl PublicationPhase {
    /// Stable serialized phase value for operator-facing reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Committed => "committed",
            Self::RolledBack => "rolled_back",
            Self::Quarantined => "quarantined",
        }
    }
}

/// NODE-LOCAL final disposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationDisposition {
    Committed,
    RolledBack,
    Quarantined,
}

/// NODE-LOCAL journal transition event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationEvent {
    Commit,
    Rollback,
    Quarantine,
}

/// NODE-LOCAL durable journal transition entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationTransition {
    pub sequence: u64,
    pub phase: PublicationPhase,
    pub disposition: Option<PublicationDisposition>,
    pub recorded_at: Option<String>,
}

/// NODE-LOCAL durable journal state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationJournal {
    pub schema_version: u32,
    pub transaction_id: PublicationTransactionId,
    pub target: PublicationTarget,
    pub generation: PublicationGenerationEvidence,
    pub digest: ContentDigest,
    pub prior_digest: Option<ContentDigest>,
    pub fence_evidence: Option<PublicationFenceEvidence>,
    pub artifact_manifest: PublicationArtifactManifest,
    pub paths: PublicationPaths,
    pub transitions: Vec<PublicationTransition>,
    pub transition_sequence: u64,
    pub phase: PublicationPhase,
    pub disposition: Option<PublicationDisposition>,
    pub recorded_at: Option<String>,
    #[serde(default)]
    pub crawler_completion: Option<CrawlerPublicationCompletion>,
    #[serde(default)]
    pub crawler_terminal: Option<CrawlerTerminalEvidence>,
}

impl PublicationJournal {
    /// NODE-LOCAL prepared journal constructor.
    pub fn prepare(
        transaction_id: PublicationTransactionId,
        target: PublicationTarget,
        generation: PublicationGenerationEvidence,
        digest: ContentDigest,
        paths: PublicationPaths,
    ) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            transaction_id,
            target,
            generation,
            digest,
            prior_digest: None,
            // Fence evidence is attached by the activation owner after `prepare`,
            // mirroring how `prior_digest` and `artifact_manifest` are populated.
            fence_evidence: None,
            artifact_manifest: PublicationArtifactManifest::default(),
            paths,
            transitions: vec![PublicationTransition {
                sequence: 1,
                phase: PublicationPhase::Prepared,
                disposition: None,
                recorded_at: None,
            }],
            transition_sequence: 1,
            phase: PublicationPhase::Prepared,
            disposition: None,
            recorded_at: None,
            crawler_completion: None,
            crawler_terminal: None,
        }
    }

    /// NODE-LOCAL JSON parser that validates the full contract.
    pub fn from_json(value: &str) -> Result<Self> {
        Self::from_json_with_policy(value, JournalReadPolicy::CurrentFenceContract)
    }

    pub(super) fn from_recovery_json(value: &str) -> Result<Self> {
        Self::from_json_with_policy(value, JournalReadPolicy::RecoveryCompatible)
    }

    fn from_json_with_policy(value: &str, policy: JournalReadPolicy) -> Result<Self> {
        let raw: RawJournal = serde_json::from_str(value)?;
        let schema_version =
            policy.schema_version_for_read(raw.schema_version, raw.fence_evidence.is_some())?;
        Self::from_raw(raw, schema_version)
    }

    fn from_raw(raw: RawJournal, schema_version: u32) -> Result<Self> {
        let transaction_id = PublicationTransactionId::new(raw.transaction_id)?;
        let target = PublicationTarget::new(raw.target)?;
        let generation = PublicationGenerationEvidence::new(raw.generation)?;
        let digest = ContentDigest::new(raw.digest)?;
        let prior_digest = raw.prior_digest.map(ContentDigest::new).transpose()?;
        let fence_evidence = raw
            .fence_evidence
            .map(RawFenceEvidence::into_evidence)
            .transpose()?;
        let phase = parse_phase(&raw.phase)?;
        let disposition = raw
            .disposition
            .as_deref()
            .map(parse_disposition)
            .transpose()?;
        validate_phase_disposition(phase, disposition)?;
        let transitions = validate_raw_transitions(raw.transitions, phase, disposition)?;
        let crawler_completion = raw.crawler_completion;
        if let Some(completion) = &crawler_completion {
            completion.validate()?;
            if phase != PublicationPhase::Prepared
                || disposition.is_some()
                || raw.crawler_terminal.is_some()
            {
                return Err(invalid_publication(
                    "pending crawler completion requires a prepared journal",
                ));
            }
        }
        let crawler_terminal = raw.crawler_terminal;
        if let Some(terminal) = &crawler_terminal {
            terminal.validate()?;
            if phase != PublicationPhase::Committed
                || disposition != Some(PublicationDisposition::Committed)
                || terminal.outcome != CrawlerTerminalOutcome::Succeeded
                || terminal.publication.as_ref().is_none_or(|publication| {
                    publication.destination_index != target.as_str()
                        || publication.transaction_id != transaction_id
                        || publication.generation != generation
                        || publication.digest != digest
                })
            {
                return Err(invalid_publication(
                    "crawler journal evidence disagrees with publication truth",
                ));
            }
        }
        let paths = raw.paths.into_paths(&target, &transaction_id)?;
        Ok(Self {
            schema_version,
            transaction_id,
            target,
            generation,
            digest,
            prior_digest,
            fence_evidence,
            artifact_manifest: raw.artifact_manifest.into_manifest()?,
            paths,
            transition_sequence: raw.transition_sequence,
            transitions,
            phase,
            disposition,
            recorded_at: raw.recorded_at,
            crawler_completion,
            crawler_terminal,
        })
    }

    /// NODE-LOCAL JSON serializer.
    pub fn to_json_value(&self) -> serde_json::Value {
        serde_json::json!({
            "schema_version": self.schema_version,
            "transaction_id": self.transaction_id.as_str(),
            "target": self.target.as_str(),
            "generation": self.generation.0,
            "digest": self.digest.0,
            "prior_digest": self.prior_digest.as_ref().map(|digest| digest.as_str()),
            // Non-fence activations serialize absence explicitly as `null`; a bare
            // zero watermark could be misread as a proven `W`, so it is never used.
            "fence_evidence": self.fence_evidence,
            "artifact_manifest": self.artifact_manifest,
            "paths": path_evidence(&self.target, &self.transaction_id),
            "transitions": self.transitions,
            "transition_sequence": self.transition_sequence,
            "phase": self.phase,
            "disposition": self.disposition,
            "recorded_at": self.recorded_at,
            "crawler_completion": self.crawler_completion,
            "crawler_terminal": self.crawler_terminal,
        })
    }

    fn bind_crawler_completion(&mut self, completion: CrawlerPublicationCompletion) -> Result<()> {
        if self.phase != PublicationPhase::Prepared || self.crawler_terminal.is_some() {
            return Err(invalid_publication(
                "crawler completion requires a pristine prepared journal",
            ));
        }
        completion.validate()?;
        match &self.crawler_completion {
            Some(existing) if existing != &completion => Err(invalid_publication(
                "pending crawler completion cannot be replaced",
            )),
            Some(_) => Ok(()),
            None => {
                self.crawler_completion = Some(completion);
                Ok(())
            }
        }
    }

    /// NODE-LOCAL legal transition application.
    pub fn apply(mut self, event: PublicationEvent) -> Result<Self> {
        let (phase, disposition) = match (self.phase, event) {
            (PublicationPhase::Prepared, PublicationEvent::Commit) => (
                PublicationPhase::Committed,
                PublicationDisposition::Committed,
            ),
            (PublicationPhase::Prepared, PublicationEvent::Rollback) => (
                PublicationPhase::RolledBack,
                PublicationDisposition::RolledBack,
            ),
            (PublicationPhase::Prepared, PublicationEvent::Quarantine) => (
                PublicationPhase::Quarantined,
                PublicationDisposition::Quarantined,
            ),
            _ => return Err(invalid_publication("illegal publication phase transition")),
        };
        let crawler_completion = if event == PublicationEvent::Commit {
            self.crawler_completion.take()
        } else {
            self.crawler_completion = None;
            None
        };
        let mut transitions = self.transitions;
        let sequence = self.transition_sequence + 1;
        transitions.push(PublicationTransition {
            sequence,
            phase,
            disposition: Some(disposition),
            recorded_at: None,
        });
        let transitioned = Self {
            transition_sequence: sequence,
            transitions,
            phase,
            disposition: Some(disposition),
            ..self
        };
        match crawler_completion {
            Some(completion) => transitioned.attach_crawler_terminal(completion),
            None => Ok(transitioned),
        }
    }

    /// Commit a crawler replacement and attach its exact success fact in the
    /// same durable journal transition.
    pub fn apply_crawler_success(
        mut self,
        completion: CrawlerPublicationCompletion,
    ) -> Result<Self> {
        self.bind_crawler_completion(completion)?;
        self.apply(PublicationEvent::Commit)
    }

    fn attach_crawler_terminal(mut self, completion: CrawlerPublicationCompletion) -> Result<Self> {
        if self.phase != PublicationPhase::Committed || self.crawler_terminal.is_some() {
            return Err(invalid_publication(
                "crawler success requires a committed journal without terminal truth",
            ));
        }
        completion.validate()?;
        let terminal = CrawlerTerminalEvidence {
            run_id: completion.run_id,
            request_digest: completion.request_digest,
            outcome: CrawlerTerminalOutcome::Succeeded,
            counters: completion.counters,
            duration_ms: completion.duration_ms,
            terminal_at_unix_ms: completion.terminal_at_unix_ms,
            acknowledged_at_unix_ms: None,
            publication: Some(CrawlerPublicationFactEvidence {
                destination_index: self.target.as_str().to_string(),
                task_id: completion.task_id,
                transaction_id: self.transaction_id.clone(),
                generation: self.generation.clone(),
                digest: self.digest.clone(),
            }),
        };
        terminal.validate()?;
        self.crawler_terminal = Some(terminal);
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy)]
enum JournalReadPolicy {
    CurrentFenceContract,
    RecoveryCompatible,
}

impl JournalReadPolicy {
    fn schema_version_for_read(
        self,
        raw_schema_version: u32,
        has_fence_evidence: bool,
    ) -> Result<u32> {
        // Compatibility policy: schema-version 1 predates fence evidence. Normal
        // parsing still refuses it so it cannot become MIG-5 fence proof; recovery
        // may read phase/manifest state only and normalizes any later write to v2.
        match (self, raw_schema_version) {
            (Self::CurrentFenceContract, LEGACY_PRE_FENCE_SCHEMA_VERSION) => {
                Err(invalid_publication(
                    "legacy publication journal predates fence evidence and cannot be read as MIG-5 fence-proven",
                ))
            }
            (Self::RecoveryCompatible, LEGACY_PRE_FENCE_SCHEMA_VERSION) => {
                if has_fence_evidence {
                    return Err(invalid_publication(
                        "legacy publication journal must not contain fence evidence",
                    ));
                }
                Ok(SCHEMA_VERSION)
            }
            (_, SCHEMA_VERSION) => Ok(SCHEMA_VERSION),
            _ => Err(invalid_publication(
                "unknown publication journal schema version",
            )),
        }
    }
}

/// NODE-LOCAL job handoff state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationJobHandoff {
    Promoting {
        transaction_id: PublicationTransactionId,
    },
    Adopted {
        transaction_id: PublicationTransactionId,
        target: PublicationTarget,
        digest: ContentDigest,
        disposition: PublicationDisposition,
    },
}

impl PublicationJobHandoff {
    /// NODE-LOCAL promoting handoff marker.
    pub fn promoting(transaction_id: PublicationTransactionId) -> Self {
        Self::Promoting { transaction_id }
    }

    /// NODE-LOCAL adoption marker from terminal publication evidence.
    pub fn adopt(journal: &PublicationJournal) -> Result<Self> {
        let Some(disposition) = journal.disposition else {
            return Err(invalid_publication("publication outcome is not adoptable"));
        };
        if journal.phase == PublicationPhase::Prepared {
            return Err(invalid_publication(
                "prepared publication cannot be adopted",
            ));
        }
        Ok(Self::Adopted {
            transaction_id: journal.transaction_id.clone(),
            target: journal.target.clone(),
            digest: journal.digest.clone(),
            disposition,
        })
    }
}

/// NODE-LOCAL terminal tombstone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationTombstone {
    pub transaction_id: Option<PublicationTransactionId>,
    pub target: Option<PublicationTarget>,
    pub generation: Option<PublicationGenerationEvidence>,
    pub digest: Option<ContentDigest>,
    pub fence_evidence: Option<PublicationFenceEvidence>,
    pub outcome: Option<PublicationDisposition>,
    pub adopted: bool,
    pub crawler_run: Option<CrawlerRunTombstone>,
}

impl PublicationTombstone {
    /// NODE-LOCAL tombstone compaction constructor.
    pub fn from_adopted(
        journal: &PublicationJournal,
        handoff: &PublicationJobHandoff,
    ) -> Result<Self> {
        let PublicationJobHandoff::Adopted {
            transaction_id,
            target,
            digest,
            disposition,
        } = handoff
        else {
            return Err(invalid_publication(
                "publication tombstone requires adoption proof",
            ));
        };
        if transaction_id != &journal.transaction_id
            || target != &journal.target
            || digest != &journal.digest
            || Some(*disposition) != journal.disposition
        {
            return Err(invalid_publication(
                "publication handoff does not match journal",
            ));
        }
        Ok(Self {
            transaction_id: Some(journal.transaction_id.clone()),
            target: Some(journal.target.clone()),
            generation: Some(journal.generation.clone()),
            digest: Some(journal.digest.clone()),
            // Preserve the fence evidence through terminal compaction so the
            // tombstone retains the same MIG-5 proof the committed journal carried.
            fence_evidence: journal.fence_evidence.clone(),
            outcome: Some(*disposition),
            adopted: true,
            crawler_run: journal
                .crawler_terminal
                .clone()
                .map(|terminal| CrawlerRunTombstone {
                    run_id: terminal.run_id.clone(),
                    request_digest: Some(terminal.request_digest.clone()),
                    started_at_unix_ms: None,
                    deadline_at_unix_ms: None,
                    cancel_requested_at_unix_ms: None,
                    terminal: Some(terminal),
                }),
        })
    }

    pub fn cancel_before_start(run_id: &str, requested_at_unix_ms: u64) -> Result<Self> {
        validate_opaque_component("crawler run ID", run_id)?;
        let reservation_digest = cancel_reservation_digest(run_id)?;
        Ok(Self {
            transaction_id: None,
            target: None,
            generation: None,
            digest: None,
            fence_evidence: None,
            outcome: None,
            adopted: false,
            crawler_run: Some(CrawlerRunTombstone {
                run_id: run_id.to_string(),
                request_digest: None,
                started_at_unix_ms: None,
                deadline_at_unix_ms: None,
                cancel_requested_at_unix_ms: Some(requested_at_unix_ms),
                terminal: Some(CrawlerTerminalEvidence {
                    run_id: run_id.to_string(),
                    request_digest: reservation_digest,
                    outcome: CrawlerTerminalOutcome::Canceled,
                    counters: CrawlerRunCountersEvidence::default(),
                    duration_ms: 0,
                    terminal_at_unix_ms: requested_at_unix_ms,
                    acknowledged_at_unix_ms: None,
                    publication: None,
                }),
            }),
        })
    }

    pub fn started(
        run_id: &str,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
    ) -> Result<Self> {
        Self::started_with_deadline(run_id, request_digest, started_at_unix_ms, None)
    }

    pub fn started_with_deadline(
        run_id: &str,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
        deadline_at_unix_ms: Option<u64>,
    ) -> Result<Self> {
        validate_opaque_component("crawler run ID", run_id)?;
        if deadline_at_unix_ms.is_some_and(|deadline| deadline < started_at_unix_ms) {
            return Err(invalid_publication(
                "crawler run deadline precedes its durable start",
            ));
        }
        Ok(Self {
            transaction_id: None,
            target: None,
            generation: None,
            digest: None,
            fence_evidence: None,
            outcome: None,
            adopted: false,
            crawler_run: Some(CrawlerRunTombstone {
                run_id: run_id.to_string(),
                request_digest: Some(request_digest),
                started_at_unix_ms: Some(started_at_unix_ms),
                deadline_at_unix_ms,
                cancel_requested_at_unix_ms: None,
                terminal: None,
            }),
        })
    }

    pub fn request_cancel(&mut self, requested_at_unix_ms: u64) -> Result<()> {
        let run = self
            .crawler_run
            .as_mut()
            .ok_or_else(|| invalid_publication("crawler tombstone is missing run evidence"))?;
        if run.terminal.is_none() && run.cancel_requested_at_unix_ms.is_none() {
            run.cancel_requested_at_unix_ms = Some(requested_at_unix_ms);
        }
        Ok(())
    }

    pub fn finish_without_publication(
        &mut self,
        outcome: CrawlerTerminalOutcome,
        counters: CrawlerRunCountersEvidence,
        duration_ms: u64,
        terminal_at_unix_ms: u64,
    ) -> Result<()> {
        if outcome == CrawlerTerminalOutcome::Succeeded {
            return Err(invalid_publication(
                "crawler success must be committed in a publication journal",
            ));
        }
        let run = self
            .crawler_run
            .as_mut()
            .ok_or_else(|| invalid_publication("crawler tombstone is missing run evidence"))?;
        let request_digest = run
            .request_digest
            .clone()
            .ok_or_else(|| invalid_publication("crawler run has no bound request digest"))?;
        let terminal = CrawlerTerminalEvidence {
            run_id: run.run_id.clone(),
            request_digest,
            outcome,
            counters,
            duration_ms,
            terminal_at_unix_ms,
            acknowledged_at_unix_ms: None,
            publication: None,
        };
        terminal.validate()?;
        match &run.terminal {
            Some(existing) if existing == &terminal => Ok(()),
            Some(_) => Err(invalid_publication(
                "crawler terminal truth cannot be replaced",
            )),
            None => {
                run.terminal = Some(terminal);
                Ok(())
            }
        }
    }

    pub fn acknowledge(&mut self, acknowledged_at_unix_ms: u64) -> Result<()> {
        let terminal = self
            .crawler_run
            .as_mut()
            .and_then(|run| run.terminal.as_mut())
            .ok_or_else(|| invalid_publication("crawler run is not terminal"))?;
        terminal
            .acknowledged_at_unix_ms
            .get_or_insert(acknowledged_at_unix_ms);
        Ok(())
    }

    pub fn bind_canceled_start(
        &mut self,
        request_digest: ContentDigest,
        counters: CrawlerRunCountersEvidence,
        duration_ms: u64,
        terminal_at_unix_ms: u64,
    ) -> Result<()> {
        let run = self
            .crawler_run
            .as_mut()
            .ok_or_else(|| invalid_publication("crawler tombstone is missing run evidence"))?;
        if run
            .request_digest
            .as_ref()
            .is_some_and(|bound| bound != &request_digest)
        {
            return Err(invalid_publication(
                "crawler run request digest disagrees with retained tombstone",
            ));
        }
        if let Some(terminal) = run.terminal.as_mut() {
            if run.request_digest.as_ref() == Some(&request_digest) {
                return Ok(());
            }
            if run.request_digest.is_none()
                && run.started_at_unix_ms.is_none()
                && run.cancel_requested_at_unix_ms.is_some()
                && terminal.outcome == CrawlerTerminalOutcome::Canceled
                && terminal.request_digest == cancel_reservation_digest(&run.run_id)?
            {
                run.request_digest = Some(request_digest.clone());
                terminal.request_digest = request_digest;
                return Ok(());
            }
            return Err(invalid_publication(
                "crawler terminal truth cannot be rebound",
            ));
        }
        run.request_digest = Some(request_digest.clone());
        run.terminal = Some(CrawlerTerminalEvidence {
            run_id: run.run_id.clone(),
            request_digest,
            outcome: CrawlerTerminalOutcome::Canceled,
            counters,
            duration_ms,
            terminal_at_unix_ms,
            acknowledged_at_unix_ms: None,
            publication: None,
        });
        Ok(())
    }

    /// NODE-LOCAL retention eligibility predicate.
    pub fn retention_eligible(&self) -> bool {
        self.adopted || self.crawler_run.is_some()
    }
}

/// Durable node-local owner for running, canceled, failed, acknowledged, and
/// compacted crawler-run truth. Success remains authoritative in the publication
/// journal until cleanup first adopts that journal into this namespace.
#[derive(Debug, Clone)]
pub struct CrawlerRunStore {
    base: PathBuf,
}

impl CrawlerRunStore {
    pub const RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1_000;
    pub const MAX_UNACKNOWLEDGED: usize = 1_024;

    pub fn new(base: impl Into<PathBuf>) -> Self {
        Self { base: base.into() }
    }

    pub fn load(&self, run_id: &str) -> Result<Option<PublicationTombstone>> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(false)?;
        self.load_unlocked(run_id)
    }

    /// Constant-locality cancellation probe for the active runtime. Active runs
    /// are always tombstone-owned, so this avoids scanning publication journals
    /// at every bounded runtime checkpoint.
    pub fn cancellation_requested(&self, run_id: &str) -> Result<bool> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(false)?;
        Ok(self
            .load_tombstone(run_id)?
            .and_then(|tombstone| tombstone.crawler_run)
            .is_some_and(|run| run.cancel_requested_at_unix_ms.is_some()))
    }

    fn load_unlocked(&self, run_id: &str) -> Result<Option<PublicationTombstone>> {
        let tombstone = self.load_tombstone(run_id)?;
        let journal = self.find_success_journal(run_id)?;
        match (tombstone, journal) {
            (None, None) => Ok(None),
            (Some(tombstone), None) => Ok(Some(tombstone)),
            (None, Some(journal)) => Ok(Some(PublicationTombstone::from_adopted(
                &journal,
                &PublicationJobHandoff::adopt(&journal)?,
            )?)),
            (Some(tombstone), Some(journal)) => {
                let compacted = PublicationTombstone::from_adopted(
                    &journal,
                    &PublicationJobHandoff::adopt(&journal)?,
                )?;
                if crawler_run_is_matching_precursor(
                    tombstone.crawler_run.as_ref(),
                    compacted.crawler_run.as_ref(),
                ) {
                    return Ok(Some(compacted));
                }
                if tombstone.crawler_run == compacted.crawler_run {
                    return Ok(Some(tombstone));
                }
                if crawler_runs_equal_except_ack(
                    tombstone.crawler_run.as_ref(),
                    compacted.crawler_run.as_ref(),
                ) {
                    // The journal is the atomic success owner. A crash while
                    // refreshing its compacted projection may leave the old ACK
                    // value in the tombstone; replay returns journal truth.
                    return Ok(Some(compacted));
                } else {
                    return Err(invalid_publication(
                        "crawler journal and tombstone terminal truth disagree",
                    ));
                }
            }
        }
    }

    /// Atomically bind the first request digest or replay retained truth. A
    /// cancel-before-start terminal truth only binds the arriving request digest
    /// in this transition and can never launch work.
    pub fn start(
        &self,
        run_id: &str,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
    ) -> Result<CrawlerRunStartDisposition> {
        self.start_classified(run_id, request_digest, started_at_unix_ms)
            .map_err(|error| match error {
                CrawlerRunStartError::Conflict => {
                    invalid_publication("crawler run request digest disagrees with retained truth")
                }
                CrawlerRunStartError::Capacity => {
                    invalid_publication("crawler unacknowledged run retention cap reached")
                }
                CrawlerRunStartError::AdmissionRejected => {
                    invalid_publication("crawler run is outside the admission window")
                }
                CrawlerRunStartError::Internal(error) => error,
            })
    }

    /// HTTP composition needs stable conflict/backpressure classes without
    /// parsing storage error text. This is the same atomic start transition as
    /// `start`; the durable store remains the sole replay owner.
    pub fn start_classified(
        &self,
        run_id: &str,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
    ) -> std::result::Result<CrawlerRunStartDisposition, CrawlerRunStartError> {
        self.start_classified_with_admission(run_id, request_digest, started_at_unix_ms, true)
    }

    /// Replay retained truth regardless of current admission policy while
    /// atomically refusing a new namespace entry that failed HTTP admission.
    pub fn start_classified_with_admission(
        &self,
        run_id: &str,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
        allow_new: bool,
    ) -> std::result::Result<CrawlerRunStartDisposition, CrawlerRunStartError> {
        self.start_classified_with_deadline_admission(
            run_id,
            request_digest,
            started_at_unix_ms,
            None,
            allow_new,
        )
    }

    /// HTTP admission persists the original wall-clock deadline in the same
    /// atomic record as the request digest. GET can then recover a lost worker
    /// without requiring a second start request from the coordinator.
    pub fn start_classified_with_deadline_admission(
        &self,
        run_id: &str,
        request_digest: ContentDigest,
        started_at_unix_ms: u64,
        deadline_at_unix_ms: Option<u64>,
        allow_new: bool,
    ) -> std::result::Result<CrawlerRunStartDisposition, CrawlerRunStartError> {
        validate_opaque_component("crawler run ID", run_id)
            .map_err(CrawlerRunStartError::Internal)?;
        let _transition_lock = self
            .acquire_transition_lock(true)
            .map_err(CrawlerRunStartError::Internal)?;
        match self
            .load_unlocked(run_id)
            .map_err(CrawlerRunStartError::Internal)?
        {
            Some(mut retained) => {
                let run = validate_tombstone(&retained).map_err(CrawlerRunStartError::Internal)?;
                if let Some(bound) = &run.request_digest {
                    if bound != &request_digest {
                        return Err(CrawlerRunStartError::Conflict);
                    }
                    return Ok(CrawlerRunStartDisposition::Replay(retained));
                }
                if run.cancel_requested_at_unix_ms.is_none() {
                    return Err(CrawlerRunStartError::Internal(invalid_publication(
                        "crawler run has incomplete retained start truth",
                    )));
                }
                retained
                    .bind_canceled_start(
                        request_digest,
                        CrawlerRunCountersEvidence::default(),
                        0,
                        started_at_unix_ms,
                    )
                    .map_err(CrawlerRunStartError::Internal)?;
                self.persist_unlocked(&retained)
                    .map_err(CrawlerRunStartError::Internal)?;
                Ok(CrawlerRunStartDisposition::Canceled(retained))
            }
            None => {
                if !allow_new {
                    return Err(CrawlerRunStartError::AdmissionRejected);
                }
                if self
                    .unacknowledged_count_unlocked()
                    .map_err(CrawlerRunStartError::Internal)?
                    >= Self::MAX_UNACKNOWLEDGED
                {
                    return Err(CrawlerRunStartError::Capacity);
                }
                let started = PublicationTombstone::started_with_deadline(
                    run_id,
                    request_digest,
                    started_at_unix_ms,
                    deadline_at_unix_ms,
                )
                .map_err(CrawlerRunStartError::Internal)?;
                self.persist_unlocked(&started)
                    .map_err(CrawlerRunStartError::Internal)?;
                Ok(CrawlerRunStartDisposition::Started(started))
            }
        }
    }

    /// Claim the process-local execution slot through a filesystem lock owned
    /// by the durable run namespace. A dropped worker or process restart
    /// releases the lock automatically, allowing an identical start replay to
    /// resume; concurrent replays never launch parallel crawls.
    pub fn claim_execution(&self, run_id: &str) -> Result<CrawlerRunExecutionClaimDisposition> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        let retained = self
            .load_unlocked(run_id)?
            .ok_or_else(|| invalid_publication("crawler run was not started"))?;
        let run = validate_tombstone(&retained)?;
        if run.started_at_unix_ms.is_none()
            || run.cancel_requested_at_unix_ms.is_some()
            || run.terminal.is_some()
            || retained.adopted
        {
            return Ok(CrawlerRunExecutionClaimDisposition::NotRunnable);
        }
        self.claim_execution_lock_unlocked(run_id)
    }

    /// Claim an abandoned canceled run so the HTTP owner can make terminal
    /// cancellation durable. An active worker retains the same lock until it
    /// persists exact counters, so this path cannot race or replace its truth.
    pub fn claim_canceled_terminalization(
        &self,
        run_id: &str,
    ) -> Result<CrawlerRunExecutionClaimDisposition> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        let retained = self
            .load_unlocked(run_id)?
            .ok_or_else(|| invalid_publication("crawler run was not started"))?;
        let run = validate_tombstone(&retained)?;
        if run.started_at_unix_ms.is_none()
            || run.cancel_requested_at_unix_ms.is_none()
            || run.terminal.is_some()
            || retained.adopted
        {
            return Ok(CrawlerRunExecutionClaimDisposition::NotRunnable);
        }
        self.claim_execution_lock_unlocked(run_id)
    }

    /// Terminalize an expired run only when no live worker owns its execution
    /// lock. Deadline/cancellation recheck, outcome choice, and persistence all
    /// happen while the same transition and execution locks remain held.
    pub fn terminalize_expired_if_unowned(
        &self,
        run_id: &str,
        terminal_at_unix_ms: u64,
        legacy_max_duration_ms: u64,
    ) -> Result<Option<PublicationTombstone>> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        let mut tombstone = self
            .load_unlocked(run_id)?
            .ok_or_else(|| invalid_publication("crawler run was not started"))?;
        let run = validate_tombstone(&tombstone)?;
        let Some(started_at_unix_ms) = run.started_at_unix_ms else {
            return Ok(None);
        };
        let deadline_at_unix_ms = run
            .deadline_at_unix_ms
            .unwrap_or_else(|| started_at_unix_ms.saturating_add(legacy_max_duration_ms));
        if run.terminal.is_some() || tombstone.adopted || terminal_at_unix_ms < deadline_at_unix_ms
        {
            return Ok(None);
        }
        let outcome = if run.cancel_requested_at_unix_ms.is_some() {
            CrawlerTerminalOutcome::Canceled
        } else {
            CrawlerTerminalOutcome::Failed {
                error_code: CrawlerRunErrorCodeEvidence::WorkerLost,
            }
        };
        let _claim = match self.claim_execution_lock_unlocked(run_id)? {
            CrawlerRunExecutionClaimDisposition::Acquired(claim) => claim,
            CrawlerRunExecutionClaimDisposition::AlreadyExecuting
            | CrawlerRunExecutionClaimDisposition::NotRunnable => return Ok(None),
        };
        tombstone.finish_without_publication(
            outcome,
            CrawlerRunCountersEvidence::default(),
            terminal_at_unix_ms.saturating_sub(started_at_unix_ms),
            terminal_at_unix_ms,
        )?;
        self.persist_unlocked(&tombstone)?;
        Ok(Some(tombstone))
    }

    fn claim_execution_lock_unlocked(
        &self,
        run_id: &str,
    ) -> Result<CrawlerRunExecutionClaimDisposition> {
        let root = self.ensure_root_durable()?;
        let lock_path = root.join(format!("{run_id}.execution.lock"));
        fsops::reject_symlinked_managed_path_components(
            &self.base,
            &lock_path,
            "crawler execution lock",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        let existed = lock_path.exists();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        fsops::reject_symlinked_managed_path_components(
            &self.base,
            &lock_path,
            "crawler execution lock",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        if !file.metadata()?.is_file() {
            return Err(invalid_publication(
                "crawler execution lock must be a regular file",
            ));
        }
        if !existed {
            file.sync_all()?;
            fsops::fsync_dir(&root)?;
        }
        match file.try_lock() {
            Ok(()) => Ok(CrawlerRunExecutionClaimDisposition::Acquired(
                CrawlerRunExecutionClaim { _lock: file },
            )),
            Err(std::fs::TryLockError::WouldBlock) => {
                Ok(CrawlerRunExecutionClaimDisposition::AlreadyExecuting)
            }
            Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    /// Take the exclusion guard consumed by crawler activation. All mutable run
    /// truth is rechecked after the lock is held, and the guard carries that lock
    /// through the publication commit/rollback boundary.
    pub fn admit_publication(
        &self,
        completion: CrawlerPublicationCompletion,
        deadline: Instant,
    ) -> Result<CrawlerPublicationAdmission> {
        let transition_lock = self.acquire_transition_lock(true)?;
        if Instant::now() >= deadline {
            return Err(invalid_publication(
                "crawler publication deadline was exceeded before admission",
            ));
        }
        self.validate_publication_candidate_unlocked(&completion)?;
        Ok(CrawlerPublicationAdmission {
            base: self.base.clone(),
            completion,
            deadline,
            _transition_lock: transition_lock,
        })
    }

    fn validate_publication_candidate_unlocked(
        &self,
        completion: &CrawlerPublicationCompletion,
    ) -> Result<()> {
        validate_opaque_component("crawler run ID", &completion.run_id)?;
        let retained = self
            .load_unlocked(&completion.run_id)?
            .ok_or_else(|| invalid_publication("crawler run was not started"))?;
        let run = validate_tombstone(&retained)?;
        if run.request_digest.as_ref() != Some(&completion.request_digest) {
            return Err(invalid_publication(
                "crawler run request digest disagrees before publication",
            ));
        }
        if run.started_at_unix_ms.is_none() {
            return Err(invalid_publication(
                "crawler run never acquired start ownership",
            ));
        }
        if run.cancel_requested_at_unix_ms.is_some() {
            return Err(invalid_publication(
                "crawler run was canceled before publication",
            ));
        }
        if run.terminal.is_some() || retained.adopted {
            return Err(invalid_publication(
                "crawler replay or terminal run cannot publish again",
            ));
        }
        // The admitted run already occupies one retained slot and success
        // replaces that truth rather than allocating another record. Equality is
        // therefore valid; only an already-over-cap namespace must fail closed.
        if self.unacknowledged_count_unlocked()? > Self::MAX_UNACKNOWLEDGED {
            return Err(invalid_publication(
                "crawler unacknowledged run capacity was exceeded before publication",
            ));
        }
        Ok(())
    }

    pub fn create(&self, tombstone: &PublicationTombstone) -> Result<()> {
        let run = validate_tombstone(tombstone)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        if let Some(existing) = self.load_unlocked(&run.run_id)? {
            let existing_run = validate_tombstone(&existing)?;
            return if existing_run.request_digest == run.request_digest {
                Ok(())
            } else {
                Err(invalid_publication(
                    "crawler run request digest disagrees with retained truth",
                ))
            };
        }
        if self.unacknowledged_count_unlocked()? >= Self::MAX_UNACKNOWLEDGED {
            return Err(invalid_publication(
                "crawler unacknowledged run retention cap reached",
            ));
        }
        self.persist_unlocked(tombstone)
    }

    /// Durably record cancellation intent. If start has not arrived, reserve the
    /// run ID with durable canceled terminal truth so GET/ACK can reconcile even
    /// when the coordinator never dispatches start.
    pub fn request_cancel(&self, run_id: &str, requested_at_unix_ms: u64) -> Result<()> {
        self.request_cancel_with_disposition(run_id, requested_at_unix_ms)
            .map(|_| ())
    }

    pub fn request_cancel_with_disposition(
        &self,
        run_id: &str,
        requested_at_unix_ms: u64,
    ) -> Result<CrawlerRunCancelDispositionEvidence> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        match self.load_unlocked(run_id)? {
            Some(mut tombstone) => {
                let run = validate_tombstone(&tombstone)?;
                if run.terminal.is_some() {
                    return Ok(CrawlerRunCancelDispositionEvidence::AlreadyTerminal);
                }
                if run.cancel_requested_at_unix_ms.is_some() {
                    return Ok(CrawlerRunCancelDispositionEvidence::AlreadyRequested);
                }
                tombstone.request_cancel(requested_at_unix_ms)?;
                self.persist_unlocked(&tombstone)?;
                Ok(CrawlerRunCancelDispositionEvidence::CancelRequested)
            }
            None => {
                if self.unacknowledged_count_unlocked()? >= Self::MAX_UNACKNOWLEDGED {
                    return Err(invalid_publication(
                        "crawler unacknowledged run retention cap reached",
                    ));
                }
                self.persist_unlocked(&PublicationTombstone::cancel_before_start(
                    run_id,
                    requested_at_unix_ms,
                )?)?;
                Ok(CrawlerRunCancelDispositionEvidence::CancelRequested)
            }
        }
    }

    /// Bind the first start request to retained cancel-before-start terminal
    /// truth without disturbing an ACK that may already have arrived.
    pub fn bind_canceled_start(
        &self,
        run_id: &str,
        request_digest: ContentDigest,
        counters: CrawlerRunCountersEvidence,
        duration_ms: u64,
        terminal_at_unix_ms: u64,
    ) -> Result<PublicationTombstone> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        let mut tombstone = self
            .load_unlocked(run_id)?
            .ok_or_else(|| invalid_publication("crawler cancel-before-start was not found"))?;
        tombstone.bind_canceled_start(
            request_digest,
            counters,
            duration_ms,
            terminal_at_unix_ms,
        )?;
        self.persist_unlocked(&tombstone)?;
        Ok(tombstone)
    }

    /// Persist canceled/failed terminal truth for a started run. Success is
    /// intentionally impossible through this API.
    pub fn finish_without_publication(
        &self,
        run_id: &str,
        outcome: CrawlerTerminalOutcome,
        counters: CrawlerRunCountersEvidence,
        duration_ms: u64,
        terminal_at_unix_ms: u64,
    ) -> Result<PublicationTombstone> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        let mut tombstone = self
            .load_unlocked(run_id)?
            .ok_or_else(|| invalid_publication("crawler run was not found"))?;
        tombstone.finish_without_publication(
            outcome,
            counters,
            duration_ms,
            terminal_at_unix_ms,
        )?;
        self.persist_unlocked(&tombstone)?;
        Ok(tombstone)
    }

    /// Persist a runtime terminal result while atomically honoring any cancel
    /// intent that won the durable transition first.
    pub fn finish_runtime_without_publication(
        &self,
        run_id: &str,
        outcome: CrawlerTerminalOutcome,
        counters: CrawlerRunCountersEvidence,
        duration_ms: u64,
        terminal_at_unix_ms: u64,
    ) -> Result<PublicationTombstone> {
        validate_opaque_component("crawler run ID", run_id)?;
        let _transition_lock = self.acquire_transition_lock(true)?;
        let mut tombstone = self
            .load_unlocked(run_id)?
            .ok_or_else(|| invalid_publication("crawler run was not found"))?;
        let outcome = if validate_tombstone(&tombstone)?
            .cancel_requested_at_unix_ms
            .is_some()
        {
            CrawlerTerminalOutcome::Canceled
        } else {
            outcome
        };
        tombstone.finish_without_publication(
            outcome,
            counters,
            duration_ms,
            terminal_at_unix_ms,
        )?;
        self.persist_unlocked(&tombstone)?;
        Ok(tombstone)
    }

    #[cfg(test)]
    pub(crate) fn persist(&self, tombstone: &PublicationTombstone) -> Result<()> {
        let _transition_lock = self.acquire_transition_lock(true)?;
        self.persist_unlocked(tombstone)
    }

    fn persist_unlocked(&self, tombstone: &PublicationTombstone) -> Result<()> {
        let run = validate_tombstone(tombstone)?;
        let root = self.root();
        fsops::reject_symlinked_managed_path_components(&self.base, &root, "crawler tombstones")
            .map_err(|error| invalid_publication(error.to_string()))?;
        let path = root.join(format!("{}.json", run.run_id));
        atomic_write_json(&path, tombstone)
    }

    pub fn compact_success(&self, journal: &PublicationJournal) -> Result<()> {
        let _transition_lock = self.acquire_transition_lock(true)?;
        self.compact_success_unlocked(journal)
    }

    fn compact_success_unlocked(&self, journal: &PublicationJournal) -> Result<()> {
        let terminal = journal
            .crawler_terminal
            .as_ref()
            .ok_or_else(|| invalid_publication("journal has no crawler terminal truth"))?;
        let compacted =
            PublicationTombstone::from_adopted(journal, &PublicationJobHandoff::adopt(journal)?)?;
        if let Some(existing) = self.load_tombstone(&terminal.run_id)? {
            if crawler_run_is_matching_precursor(
                existing.crawler_run.as_ref(),
                compacted.crawler_run.as_ref(),
            ) {
                return self.persist_unlocked(&compacted);
            }
            if existing.crawler_run == compacted.crawler_run {
                return Ok(());
            }
            if !crawler_runs_equal_except_ack(
                existing.crawler_run.as_ref(),
                compacted.crawler_run.as_ref(),
            ) {
                return Err(invalid_publication(
                    "crawler journal and tombstone terminal truth disagree",
                ));
            }
        }
        self.persist_unlocked(&compacted)
    }

    pub fn acknowledge(&self, run_id: &str, acknowledged_at_unix_ms: u64) -> Result<()> {
        self.acknowledge_classified(run_id, acknowledged_at_unix_ms)
            .map_err(|error| match error {
                CrawlerRunAcknowledgeError::NotFound => {
                    invalid_publication("crawler run was not found")
                }
                CrawlerRunAcknowledgeError::NotTerminal => {
                    invalid_publication("crawler run is not terminal")
                }
                CrawlerRunAcknowledgeError::Internal(error) => error,
            })
    }

    pub fn acknowledge_classified(
        &self,
        run_id: &str,
        acknowledged_at_unix_ms: u64,
    ) -> std::result::Result<(), CrawlerRunAcknowledgeError> {
        validate_opaque_component("crawler run ID", run_id)
            .map_err(CrawlerRunAcknowledgeError::Internal)?;
        let _transition_lock = self
            .acquire_transition_lock(true)
            .map_err(CrawlerRunAcknowledgeError::Internal)?;
        if let Some(mut journal) = self
            .find_success_journal(run_id)
            .map_err(CrawlerRunAcknowledgeError::Internal)?
        {
            journal
                .crawler_terminal
                .as_mut()
                .expect("success journal selected by terminal evidence")
                .acknowledged_at_unix_ms
                .get_or_insert(acknowledged_at_unix_ms);
            self.persist_success_journal(&journal)
                .map_err(CrawlerRunAcknowledgeError::Internal)?;
            return self
                .compact_success_unlocked(&journal)
                .map_err(CrawlerRunAcknowledgeError::Internal);
        }
        let mut tombstone = self
            .load_tombstone(run_id)
            .map_err(CrawlerRunAcknowledgeError::Internal)?
            .ok_or(CrawlerRunAcknowledgeError::NotFound)?;
        if validate_tombstone(&tombstone)
            .map_err(CrawlerRunAcknowledgeError::Internal)?
            .terminal
            .is_none()
        {
            return Err(CrawlerRunAcknowledgeError::NotTerminal);
        }
        tombstone
            .acknowledge(acknowledged_at_unix_ms)
            .map_err(CrawlerRunAcknowledgeError::Internal)?;
        self.persist_unlocked(&tombstone)
            .map_err(CrawlerRunAcknowledgeError::Internal)
    }

    pub fn prune(&self, now_unix_ms: u64) -> Result<usize> {
        self.prune_with_io(now_unix_ms, &PublicationIo::production())
    }

    fn prune_with_io(&self, now_unix_ms: u64, io: &PublicationIo<'_>) -> Result<usize> {
        let _transition_lock = self.acquire_transition_lock(true)?;
        let root = self.root();
        fsops::reject_symlinked_managed_path_components(&self.base, &root, "crawler tombstones")
            .map_err(|error| invalid_publication(error.to_string()))?;
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err(invalid_publication(
                    "crawler tombstone namespace contains a symlink",
                ));
            }
            if !entry.file_type()?.is_file()
                || entry.path().extension() != Some(std::ffi::OsStr::new("json"))
            {
                continue;
            }
            let tombstone: PublicationTombstone =
                serde_json::from_slice(&std::fs::read(entry.path())?)?;
            validate_tombstone(&tombstone)?;
            let Some(terminal) = tombstone
                .crawler_run
                .as_ref()
                .and_then(|run| run.terminal.as_ref())
            else {
                continue;
            };
            if terminal.acknowledged_at_unix_ms.is_some()
                && now_unix_ms.saturating_sub(terminal.terminal_at_unix_ms) >= Self::RETENTION_MS
            {
                let run_id = &tombstone
                    .crawler_run
                    .as_ref()
                    .expect("validated crawler tombstone")
                    .run_id;
                let execution_lock = root.join(format!("{run_id}.execution.lock"));
                let execution_guard = match std::fs::symlink_metadata(&execution_lock) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(invalid_publication(
                            "crawler execution lock must not be a symlink",
                        ));
                    }
                    Ok(metadata) if !metadata.is_file() => {
                        return Err(invalid_publication(
                            "crawler execution lock must be a regular file",
                        ));
                    }
                    Ok(_) => {
                        let file = OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open(&execution_lock)?;
                        fsops::reject_symlinked_managed_path_components(
                            &self.base,
                            &execution_lock,
                            "crawler execution lock",
                        )
                        .map_err(|error| invalid_publication(error.to_string()))?;
                        match file.try_lock() {
                            Ok(()) => Some(file),
                            Err(std::fs::TryLockError::WouldBlock) => continue,
                            Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                    Err(error) => return Err(error.into()),
                };
                if execution_guard.is_some() {
                    io.remove_file(&execution_lock)?;
                    io.sync_dir(&root)?;
                }
                // Remove the lock pathname first. A crash between these two
                // deletes leaves the retained tombstone available for the next
                // maintenance pass; the reverse order can strand an orphan lock.
                io.remove_file(&entry.path())?;
                drop(execution_guard);
                removed += 1;
            }
        }
        if removed > 0 {
            io.sync_dir(&root)?;
        }
        Ok(removed)
    }

    #[cfg(test)]
    fn prune_with_faults_for_test(
        &self,
        now_unix_ms: u64,
        faults: &dyn PublicationFaultHook,
    ) -> Result<usize> {
        self.prune_with_io(now_unix_ms, &PublicationIo::with_faults(faults))
    }

    pub fn unacknowledged_count(&self) -> Result<usize> {
        let _transition_lock = self.acquire_transition_lock(false)?;
        self.unacknowledged_count_unlocked()
    }

    fn unacknowledged_count_unlocked(&self) -> Result<usize> {
        let root = self.root();
        fsops::reject_symlinked_managed_path_components(&self.base, &root, "crawler tombstones")
            .map_err(|error| invalid_publication(error.to_string()))?;
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error.into()),
        };
        let mut count = 0;
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                return Err(invalid_publication(
                    "crawler tombstone namespace contains a symlink",
                ));
            }
            if entry.file_type()?.is_file()
                && entry.path().extension() == Some(std::ffi::OsStr::new("json"))
            {
                let tombstone: PublicationTombstone =
                    serde_json::from_slice(&std::fs::read(entry.path())?)?;
                let run = validate_tombstone(&tombstone)?;
                if run
                    .terminal
                    .as_ref()
                    .is_none_or(|terminal| terminal.acknowledged_at_unix_ms.is_none())
                {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    fn acquire_transition_lock(&self, exclusive: bool) -> Result<File> {
        let root = self.ensure_root_durable()?;
        let lock_path = root.join(CRAWLER_TOMBSTONE_LOCK_FILE);
        fsops::reject_symlinked_managed_path_components(
            &self.base,
            &lock_path,
            "crawler transition lock",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        let lock_existed = lock_path.exists();
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        fsops::reject_symlinked_managed_path_components(
            &self.base,
            &lock_path,
            "crawler transition lock",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        if !file.metadata()?.is_file() {
            return Err(invalid_publication(
                "crawler transition lock must be a regular file",
            ));
        }
        if !lock_existed {
            file.sync_all()?;
            fsops::fsync_dir(&root)?;
        }
        if exclusive {
            file.lock()?;
        } else {
            file.lock_shared()?;
        }
        Ok(file)
    }

    /// A successful return means the tombstone namespace entry is itself
    /// durable. Journal cleanup may therefore persist a tombstone and retire the
    /// journal without a first-use crash window that loses both owners.
    fn ensure_root_durable(&self) -> Result<PathBuf> {
        let root = self.root();
        fsops::reject_symlinked_managed_path_components(&self.base, &root, "crawler tombstones")
            .map_err(|error| invalid_publication(error.to_string()))?;
        let base_metadata = std::fs::symlink_metadata(&self.base)?;
        if base_metadata.file_type().is_symlink() || !base_metadata.is_dir() {
            return Err(invalid_publication(
                "crawler run-store base must be an existing regular directory",
            ));
        }
        match std::fs::create_dir(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        // Sync even when another process won `create_dir`: observing the entry
        // does not prove that winner had already made the parent entry durable.
        fsops::fsync_dir(&self.base)?;
        let root_metadata = std::fs::symlink_metadata(&root)?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(invalid_publication(
                "crawler tombstone namespace must be a regular directory",
            ));
        }
        Ok(root)
    }

    fn root(&self) -> PathBuf {
        self.base.join(CRAWLER_TOMBSTONE_DIR)
    }

    fn load_tombstone(&self, run_id: &str) -> Result<Option<PublicationTombstone>> {
        let path = self.root().join(format!("{run_id}.json"));
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(invalid_publication(
                    "crawler tombstone must not be a symlink",
                ))
            }
            Ok(metadata) if !metadata.is_file() => {
                return Err(invalid_publication(
                    "crawler tombstone must be a regular file",
                ))
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        let raw = match std::fs::read(path) {
            Ok(raw) => raw,
            Err(error) => return Err(error.into()),
        };
        let tombstone: PublicationTombstone = serde_json::from_slice(&raw)?;
        let run = validate_tombstone(&tombstone)?;
        if run.run_id != run_id {
            return Err(invalid_publication(
                "crawler tombstone filename and run ID disagree",
            ));
        }
        Ok(Some(tombstone))
    }

    fn find_success_journal(&self, run_id: &str) -> Result<Option<PublicationJournal>> {
        let root = self.base.join(PUBLICATION_DIR);
        fsops::reject_symlinked_managed_path_components(
            &self.base,
            &root,
            "crawler success journals",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        let targets = match std::fs::read_dir(root) {
            Ok(targets) => targets,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let mut found = None;
        for target in targets {
            let target = target?;
            if target.file_type()?.is_symlink() {
                return Err(invalid_publication(
                    "publication namespace contains a symlink",
                ));
            }
            if !target.file_type()?.is_dir() {
                continue;
            }
            for transaction in std::fs::read_dir(target.path())? {
                let transaction = transaction?;
                if transaction.file_type()?.is_symlink() {
                    return Err(invalid_publication(
                        "publication transaction namespace contains a symlink",
                    ));
                }
                if !transaction.file_type()?.is_dir() {
                    continue;
                }
                let journal_path = transaction.path().join("journal.json");
                match std::fs::symlink_metadata(&journal_path) {
                    Ok(metadata) if metadata.file_type().is_symlink() => {
                        return Err(invalid_publication(
                            "publication journal must not be a symlink",
                        ))
                    }
                    Ok(metadata) if !metadata.is_file() => {
                        return Err(invalid_publication(
                            "publication journal must be a regular file",
                        ))
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let raw = std::fs::read_to_string(journal_path)?;
                let journal = PublicationJournal::from_recovery_json(&raw)?;
                if journal
                    .crawler_terminal
                    .as_ref()
                    .is_some_and(|terminal| terminal.run_id == run_id)
                {
                    if found.is_some() {
                        return Err(invalid_publication(
                            "multiple crawler success journals claim one run ID",
                        ));
                    }
                    found = Some(journal);
                }
            }
        }
        Ok(found)
    }

    fn persist_success_journal(&self, journal: &PublicationJournal) -> Result<()> {
        let paths = PublicationPaths::new(&self.base, &journal.target, &journal.transaction_id);
        fsops::reject_symlinked_managed_path_components(
            &self.base,
            &paths.journal,
            "crawler success journal ACK",
        )
        .map_err(|error| invalid_publication(error.to_string()))?;
        let parent = paths
            .journal
            .parent()
            .ok_or_else(|| invalid_publication("publication journal has no parent"))?;
        if !parent.exists() {
            return Err(invalid_publication("publication journal parent is missing"));
        }
        atomic_write_json(&paths.journal, &journal.to_json_value())
    }
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid_publication("durable JSON path has no parent"))?;
    let temp = path.with_extension("json.tmp");
    match std::fs::remove_file(&temp) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)?;
    fsops::fsync_dir(parent)?;
    Ok(())
}

fn crawler_runs_equal_except_ack(
    left: Option<&CrawlerRunTombstone>,
    right: Option<&CrawlerRunTombstone>,
) -> bool {
    let (Some(left), Some(right)) = (left, right) else {
        return false;
    };
    let mut left = left.clone();
    let mut right = right.clone();
    if let Some(terminal) = left.terminal.as_mut() {
        terminal.acknowledged_at_unix_ms = None;
    }
    if let Some(terminal) = right.terminal.as_mut() {
        terminal.acknowledged_at_unix_ms = None;
    }
    left == right
}

fn crawler_run_is_matching_precursor(
    possible_precursor: Option<&CrawlerRunTombstone>,
    success: Option<&CrawlerRunTombstone>,
) -> bool {
    let (Some(possible_precursor), Some(success)) = (possible_precursor, success) else {
        return false;
    };
    possible_precursor.run_id == success.run_id
        && possible_precursor.request_digest == success.request_digest
        && possible_precursor.terminal.is_none()
        && success
            .terminal
            .as_ref()
            .is_some_and(|terminal| terminal.outcome == CrawlerTerminalOutcome::Succeeded)
}

fn validate_tombstone(tombstone: &PublicationTombstone) -> Result<&CrawlerRunTombstone> {
    let run = tombstone
        .crawler_run
        .as_ref()
        .ok_or_else(|| invalid_publication("crawler tombstone is missing run evidence"))?;
    validate_opaque_component("crawler run ID", &run.run_id)?;
    match (run.started_at_unix_ms, run.deadline_at_unix_ms) {
        (None, Some(_)) => {
            return Err(invalid_publication(
                "crawler run deadline is missing its durable start",
            ))
        }
        (Some(started_at), Some(deadline_at)) if deadline_at < started_at => {
            return Err(invalid_publication(
                "crawler run deadline precedes its durable start",
            ))
        }
        _ => {}
    }
    if let Some(terminal) = &run.terminal {
        terminal.validate()?;
        let is_cancel_before_start_reservation = run.request_digest.is_none()
            && run.started_at_unix_ms.is_none()
            && run.cancel_requested_at_unix_ms.is_some()
            && terminal.outcome == CrawlerTerminalOutcome::Canceled
            && terminal.request_digest == cancel_reservation_digest(&run.run_id)?;
        if terminal.run_id != run.run_id
            || (run.request_digest.as_ref() != Some(&terminal.request_digest)
                && !is_cancel_before_start_reservation)
        {
            return Err(invalid_publication(
                "crawler tombstone terminal identity disagrees with run evidence",
            ));
        }
        match &terminal.publication {
            Some(publication)
                if tombstone.transaction_id.as_ref() == Some(&publication.transaction_id)
                    && tombstone
                        .target
                        .as_ref()
                        .is_some_and(|target| target.as_str() == publication.destination_index)
                    && tombstone.generation.as_ref() == Some(&publication.generation)
                    && tombstone.digest.as_ref() == Some(&publication.digest)
                    && tombstone.outcome == Some(PublicationDisposition::Committed)
                    && tombstone.adopted => {}
            Some(_) => {
                return Err(invalid_publication(
                    "crawler success tombstone disagrees with publication evidence",
                ))
            }
            None if tombstone.transaction_id.is_none()
                && tombstone.target.is_none()
                && tombstone.generation.is_none()
                && tombstone.digest.is_none()
                && tombstone.outcome.is_none()
                && !tombstone.adopted => {}
            None => {
                return Err(invalid_publication(
                    "crawler non-success tombstone contains publication evidence",
                ))
            }
        }
    } else if tombstone.adopted {
        return Err(invalid_publication(
            "adopted crawler tombstone must contain terminal publication truth",
        ));
    }
    Ok(run)
}

/// NODE-LOCAL Tantivy managed-file evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TantivyManagedInventory {
    files: BTreeSet<PathBuf>,
}

impl TantivyManagedInventory {
    /// NODE-LOCAL managed-file evidence constructor.
    pub fn new(files: impl IntoIterator<Item = PathBuf>) -> Result<Self> {
        let mut normalized = BTreeSet::new();
        for path in files {
            validate_relative_path("Tantivy managed file", &path)?;
            normalized.insert(path);
        }
        Ok(Self { files: normalized })
    }

    fn contains(&self, path: &Path) -> bool {
        self.files.contains(path)
    }

    fn has_descendant(&self, path: &Path) -> bool {
        self.files.iter().any(|file| file.starts_with(path))
    }
}

/// NODE-LOCAL source surface documentation entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicSurfaceContract {
    pub name: &'static str,
    pub guarantee: &'static str,
}

/// NODE-LOCAL public surface documentation list.
pub fn public_surface_contracts() -> &'static [PublicSurfaceContract] {
    &[
        PublicSurfaceContract {
            name: "PublicationTransactionId",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
        PublicSurfaceContract {
            name: "PublicationTarget",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
        PublicSurfaceContract {
            name: "PublicationPaths",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
        PublicSurfaceContract {
            name: "PublicationJournal",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
        PublicSurfaceContract {
            name: "PublicationJobHandoff",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
        PublicSurfaceContract {
            name: "PublicationTombstone",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
        PublicSurfaceContract {
            name: "artifact_policy_table",
            guarantee: NODE_LOCAL_GUARANTEE,
        },
    ]
}

/// NODE-LOCAL tenant child classification.
pub fn classify_tenant_relative_path(
    relative_path: &Path,
    tantivy_inventory: &TantivyManagedInventory,
) -> Result<ArtifactDisposition> {
    validate_relative_path("tenant artifact", relative_path)?;
    if tantivy_inventory.contains(relative_path)
        || is_known_tenant_file(relative_path)
        || starts_with_known_tenant_dir(relative_path)
    {
        return Ok(ArtifactDisposition::Preserve);
    }
    Err(unknown_artifact(relative_path))
}

/// NODE-LOCAL external artifact root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalArtifactRoot {
    QuerySuggestions,
    Analytics,
    Experiments,
}

/// NODE-LOCAL external artifact classification.
pub fn classify_external_relative_path(
    root: ExternalArtifactRoot,
    relative_path: &Path,
    known_paths: &[PathBuf],
) -> Result<Option<ArtifactDisposition>> {
    validate_relative_path("external artifact", relative_path)?;
    match root {
        ExternalArtifactRoot::Experiments => Ok(None),
        ExternalArtifactRoot::QuerySuggestions => {
            if known_paths.iter().any(|known| known == relative_path) {
                Ok(Some(ArtifactDisposition::Journal))
            } else {
                Err(unknown_artifact(relative_path))
            }
        }
        ExternalArtifactRoot::Analytics => {
            if known_paths
                .iter()
                .any(|known| relative_path == known || relative_path.starts_with(known))
            {
                Ok(Some(ArtifactDisposition::Journal))
            } else {
                Err(unknown_artifact(relative_path))
            }
        }
    }
}

/// TODO: Document RawJournal.
#[derive(Deserialize)]
struct RawJournal {
    schema_version: u32,
    transaction_id: String,
    target: String,
    generation: String,
    digest: String,
    #[serde(default)]
    prior_digest: Option<String>,
    #[serde(default)]
    fence_evidence: Option<RawFenceEvidence>,
    #[serde(default)]
    artifact_manifest: RawArtifactManifest,
    paths: RawPaths,
    #[serde(default)]
    transitions: Vec<RawTransition>,
    transition_sequence: u64,
    phase: String,
    disposition: Option<String>,
    recorded_at: Option<String>,
    #[serde(default)]
    crawler_completion: Option<CrawlerPublicationCompletion>,
    #[serde(default)]
    crawler_terminal: Option<CrawlerTerminalEvidence>,
}

#[derive(Deserialize)]
struct RawFenceEvidence {
    epoch_old: u64,
    epoch_new: u64,
    staging_baseline: u64,
    watermark: u64,
}

impl RawFenceEvidence {
    fn into_evidence(self) -> Result<PublicationFenceEvidence> {
        // Re-validate the fence invariants on read so a hand-edited or corrupt
        // journal cannot smuggle in `E_new != E_old + 1` or a baseline past `W`.
        PublicationFenceEvidence::new(
            PublicationEpoch(self.epoch_old),
            PublicationEpoch(self.epoch_new),
            PublicationStagingBaseline(self.staging_baseline),
            PublicationWatermark(self.watermark),
        )
    }
}

#[derive(Default, Deserialize)]
struct RawArtifactManifest {
    #[serde(default)]
    entries: Vec<PublicationArtifactManifestEntry>,
}

impl RawArtifactManifest {
    fn into_manifest(self) -> Result<PublicationArtifactManifest> {
        PublicationArtifactManifest::new(self.entries)
    }
}

#[derive(Deserialize)]
struct RawPaths {
    target: PathBuf,
    staging: PathBuf,
    backup: PathBuf,
    journal: PathBuf,
    quarantine: PathBuf,
}

impl RawPaths {
    fn into_paths(
        self,
        target: &PublicationTarget,
        transaction: &PublicationTransactionId,
    ) -> Result<PublicationPaths> {
        let paths = PublicationPaths {
            target: self.target,
            staging: self.staging,
            backup: self.backup,
            journal: self.journal,
            quarantine: self.quarantine,
        };
        let expected = relative_path_evidence(target, transaction);
        if path_evidence_matches(&paths, &expected) {
            return Ok(expected);
        }

        // Compatibility for journals written with a relative data-dir base
        // before path evidence was always namespace-relative. Accept only one
        // common safe base prefix over all five canonical target/transaction
        // suffixes, including a consistently represented leading `./`, then
        // normalize it. This cannot turn unrelated paths into owned evidence.
        if legacy_relative_base(&paths.target, &expected.target).is_some_and(|base| {
            legacy_relative_base(&paths.staging, &expected.staging).as_ref() == Some(&base)
                && legacy_relative_base(&paths.backup, &expected.backup).as_ref() == Some(&base)
                && legacy_relative_base(&paths.journal, &expected.journal).as_ref() == Some(&base)
                && legacy_relative_base(&paths.quarantine, &expected.quarantine).as_ref()
                    == Some(&base)
        }) {
            return Ok(expected);
        }

        validate_relative_path("publication target path evidence", &paths.target)?;
        validate_relative_path("publication staging path evidence", &paths.staging)?;
        validate_relative_path("publication backup path evidence", &paths.backup)?;
        validate_relative_path("publication journal path evidence", &paths.journal)?;
        validate_relative_path("publication quarantine path evidence", &paths.quarantine)?;
        Err(invalid_publication(
            "publication path evidence does not match its target and transaction",
        ))
    }
}

fn path_evidence_matches(left: &PublicationPaths, right: &PublicationPaths) -> bool {
    left.target.as_os_str() == right.target.as_os_str()
        && left.staging.as_os_str() == right.staging.as_os_str()
        && left.backup.as_os_str() == right.backup.as_os_str()
        && left.journal.as_os_str() == right.journal.as_os_str()
        && left.quarantine.as_os_str() == right.quarantine.as_os_str()
}

fn legacy_relative_base(path: &Path, suffix: &Path) -> Option<(bool, PathBuf)> {
    let mut components = path.components().peekable();
    let leading_curdir = components
        .peek()
        .is_some_and(|component| matches!(component, Component::CurDir));
    if leading_curdir {
        components.next();
    }
    let path_components = components
        .map(|component| match component {
            Component::Normal(part) if !part.is_empty() => Some(part),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let suffix_components = suffix
        .components()
        .map(|component| match component {
            Component::Normal(part) if !part.is_empty() => Some(part),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    if path_components.len() <= suffix_components.len()
        || &path_components[path_components.len() - suffix_components.len()..]
            != suffix_components.as_slice()
    {
        return None;
    }
    let mut base = PathBuf::new();
    for component in &path_components[..path_components.len() - suffix_components.len()] {
        base.push(component);
    }
    Some((leading_curdir, base))
}

#[derive(Deserialize)]
struct RawTransition {
    sequence: u64,
    phase: String,
    disposition: Option<String>,
    recorded_at: Option<String>,
}

/// TODO: Document validate_raw_transitions.
fn validate_raw_transitions(
    raw: Vec<RawTransition>,
    phase: PublicationPhase,
    disposition: Option<PublicationDisposition>,
) -> Result<Vec<PublicationTransition>> {
    if raw.is_empty() {
        return Err(invalid_publication(
            "journal must include transition evidence",
        ));
    }
    let mut transitions = Vec::with_capacity(raw.len());
    let mut last_phase = None;
    let mut last_disposition = None;
    for (expected, transition) in (1..).zip(raw) {
        if transition.sequence != expected {
            return Err(invalid_publication(
                "journal transition sequence is not monotonic",
            ));
        }
        let parsed_phase = parse_phase(&transition.phase)?;
        let parsed_disposition = transition
            .disposition
            .as_deref()
            .map(parse_disposition)
            .transpose()?;
        validate_phase_disposition(parsed_phase, parsed_disposition)?;
        transitions.push(PublicationTransition {
            sequence: transition.sequence,
            phase: parsed_phase,
            disposition: parsed_disposition,
            recorded_at: transition.recorded_at,
        });
        last_phase = Some(parsed_phase);
        last_disposition = parsed_disposition;
    }
    if last_phase != Some(phase) || last_disposition != disposition {
        return Err(invalid_publication(
            "journal terminal transition does not match phase",
        ));
    }
    Ok(transitions)
}

fn parse_phase(value: &str) -> Result<PublicationPhase> {
    match value {
        "prepared" => Ok(PublicationPhase::Prepared),
        "committed" => Ok(PublicationPhase::Committed),
        "rolled_back" => Ok(PublicationPhase::RolledBack),
        "quarantined" => Ok(PublicationPhase::Quarantined),
        _ => Err(invalid_publication("unknown publication phase")),
    }
}

fn parse_disposition(value: &str) -> Result<PublicationDisposition> {
    match value {
        "committed" => Ok(PublicationDisposition::Committed),
        "rolled_back" => Ok(PublicationDisposition::RolledBack),
        "quarantined" => Ok(PublicationDisposition::Quarantined),
        _ => Err(invalid_publication("unknown publication disposition")),
    }
}

/// TODO: Document validate_phase_disposition.
fn validate_phase_disposition(
    phase: PublicationPhase,
    disposition: Option<PublicationDisposition>,
) -> Result<()> {
    let valid = matches!(
        (phase, disposition),
        (PublicationPhase::Prepared, None)
            | (
                PublicationPhase::Committed,
                Some(PublicationDisposition::Committed)
            )
            | (
                PublicationPhase::RolledBack,
                Some(PublicationDisposition::RolledBack)
            )
            | (
                PublicationPhase::Quarantined,
                Some(PublicationDisposition::Quarantined)
            )
    );
    if valid {
        Ok(())
    } else {
        Err(invalid_publication(
            "publication phase and disposition mismatch",
        ))
    }
}

/// TODO: Document validate_opaque_component.
pub(super) fn validate_opaque_component(label: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.contains("..")
        || value.contains('/')
        || value.contains('\\')
        || value.contains('\0')
    {
        return Err(invalid_publication(format!(
            "{label} is not a safe path component"
        )));
    }
    if !value
        .bytes()
        .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-'))
    {
        return Err(invalid_publication(format!(
            "{label} contains unsupported characters"
        )));
    }
    Ok(())
}

/// TODO: Document validate_relative_path.
pub(super) fn validate_relative_path(label: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(invalid_publication(format!("{label} must be relative")));
    }
    for component in path.components() {
        match component {
            Component::Normal(part) if !part.is_empty() => {}
            _ => {
                return Err(invalid_publication(format!(
                    "{label} contains unsafe component"
                )))
            }
        }
    }
    Ok(())
}

fn path_evidence(
    target: &PublicationTarget,
    transaction: &PublicationTransactionId,
) -> serde_json::Value {
    let expected = relative_path_evidence(target, transaction);
    serde_json::json!({
        "target": expected.target,
        "staging": expected.staging,
        "backup": expected.backup,
        "journal": expected.journal,
        "quarantine": expected.quarantine,
    })
}

/// TODO: Document relative_path_evidence.
pub(super) fn relative_path_evidence(
    target: &PublicationTarget,
    transaction: &PublicationTransactionId,
) -> PublicationPaths {
    let namespace = PathBuf::from(PUBLICATION_DIR)
        .join(target.as_str())
        .join(transaction.as_str());
    PublicationPaths {
        target: PathBuf::from(target.as_str()),
        staging: namespace.join("staging"),
        backup: namespace.join("backup"),
        journal: namespace.join("journal.json"),
        quarantine: PathBuf::from(QUARANTINE_DIR)
            .join(target.as_str())
            .join(transaction.as_str()),
    }
}

fn is_known_tenant_file(relative_path: &Path) -> bool {
    relative_path == Path::new(crate::index::index_metadata::METADATA_FILE)
        || relative_path == Path::new(super::config::SETTINGS_FILE)
        || relative_path == Path::new(super::config::RULES_FILE)
        || relative_path == Path::new(super::config::SYNONYMS_FILE)
        || relative_path == Path::new(crate::index::oplog::COMMITTED_SEQ_FILE)
        || relative_path
            == Path::new(
                crate::index::write_queue::backpressure::WRITE_BACKPRESSURE_PAUSE_FILE_NAME,
            )
}

fn starts_with_known_tenant_dir(relative_path: &Path) -> bool {
    relative_path.starts_with(crate::index::oplog::OPLOG_DIR)
        || relative_path.starts_with(crate::index::version_store::VERSION_STORE_DIR)
        || relative_path.starts_with(crate::index::write_queue::PERSISTED_VECTORS_DIR)
        || relative_path.starts_with(crate::dictionaries::persistence::DICTIONARIES_DIR)
        || relative_path.starts_with(crate::recommend::rules::RECOMMEND_RULES_DIR)
}

/// NODE-LOCAL strict committed-sequence reader for MIG-5 watermark proof.
///
/// Unlike `oplog::read_committed_seq`, which is intentionally fail-open for
/// compatibility, publication requires the checked owner to return a present
/// value. Missing and invalid evidence can never masquerade as a proven
/// watermark.
pub(super) fn read_strict_committed_seq(tenant_path: &Path) -> Result<u64> {
    crate::index::oplog::read_checked_committed_seq(tenant_path)
        .map_err(|error| {
            invalid_publication(format!(
                "committed_seq sidecar is invalid at {}: {error}",
                tenant_path.display()
            ))
        })?
        .ok_or_else(|| {
            invalid_publication(format!(
                "committed_seq sidecar is missing at {}",
                tenant_path.display()
            ))
        })
}

pub(super) fn invalid_publication(message: impl Into<String>) -> FlapjackError {
    FlapjackError::InvalidQuery(format!("invalid publication contract: {}", message.into()))
}

pub(super) fn unknown_artifact(relative_path: &Path) -> FlapjackError {
    invalid_publication(format!(
        "unknown publication artifact '{}'",
        relative_path.display()
    ))
}

#[cfg(test)]
mod tests {
    include!("publication/tests.rs");
}
