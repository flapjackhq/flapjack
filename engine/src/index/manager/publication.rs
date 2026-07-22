use crate::error::{FlapjackError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

const SCHEMA_VERSION: u32 = 1;
const PUBLICATION_DIR: &str = ".publication";
const QUARANTINE_DIR: &str = ".publication_quarantine";
const NODE_LOCAL_GUARANTEE: &str =
    "NODE-LOCAL publication contract for one node only; it cannot make HA peers converge.";

mod digest;
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
pub use executor::{
    abort_unjournaled_publication, activate_publication, PreStagedActivationError,
    PreStagedActivationStage, PreStagedPublication, PublicationArtifactManifest,
    PublicationArtifactManifestEntry, PublicationArtifactPlan, PublicationArtifactRoot,
};
#[cfg(test)]
pub(crate) use executor::{
    activate_publication_for_test, activate_publication_with_faults_for_test,
};
#[cfg(test)]
pub(crate) use fault::{
    PublicationCheckpoint, PublicationFaultPoint, PublicationFaultScript, PublicationOperation,
};
pub use fsops::{
    fsync_dir, fsync_file, reject_symlinked_managed_path, rename_with_transient_retry,
};
pub use policy::{artifact_policy_table, ArtifactDisposition, ArtifactPolicy};
pub use repair::{
    decide_publication_repair, repair_publication, RepairArtifactEvidence, RepairDecision,
    RepairEvidence, RepairJournalEvidence,
};
#[cfg(test)]
pub(crate) use repair::{repair_publication_for_test, repair_publication_with_faults_for_test};
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
        if value == "." {
            return Err(invalid_publication(
                "publication target cannot be the current-directory path component",
            ));
        }
        Ok(Self(value))
    }

    /// NODE-LOCAL target name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
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

    first_component == PUBLICATION_DIR || first_component == QUARANTINE_DIR
}

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
    pub artifact_manifest: PublicationArtifactManifest,
    pub paths: PublicationPaths,
    pub transitions: Vec<PublicationTransition>,
    pub transition_sequence: u64,
    pub phase: PublicationPhase,
    pub disposition: Option<PublicationDisposition>,
    pub recorded_at: Option<String>,
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
        }
    }

    /// NODE-LOCAL JSON parser that validates the full contract.
    pub fn from_json(value: &str) -> Result<Self> {
        let raw: RawJournal = serde_json::from_str(value)?;
        if raw.schema_version != SCHEMA_VERSION {
            return Err(invalid_publication(
                "unknown publication journal schema version",
            ));
        }
        let transaction_id = PublicationTransactionId::new(raw.transaction_id)?;
        let target = PublicationTarget::new(raw.target)?;
        let generation = PublicationGenerationEvidence::new(raw.generation)?;
        let digest = ContentDigest::new(raw.digest)?;
        let prior_digest = raw.prior_digest.map(ContentDigest::new).transpose()?;
        let phase = parse_phase(&raw.phase)?;
        let disposition = raw
            .disposition
            .as_deref()
            .map(parse_disposition)
            .transpose()?;
        validate_phase_disposition(phase, disposition)?;
        let transitions = validate_raw_transitions(raw.transitions, phase, disposition)?;
        Ok(Self {
            schema_version: raw.schema_version,
            transaction_id,
            target,
            generation,
            digest,
            prior_digest,
            artifact_manifest: raw.artifact_manifest.into_manifest()?,
            paths: raw.paths.into_paths()?,
            transition_sequence: raw.transition_sequence,
            transitions,
            phase,
            disposition,
            recorded_at: raw.recorded_at,
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
            "artifact_manifest": self.artifact_manifest,
            "paths": path_evidence(&self.paths, &self.target, &self.transaction_id),
            "transitions": self.transitions,
            "transition_sequence": self.transition_sequence,
            "phase": self.phase,
            "disposition": self.disposition,
            "recorded_at": self.recorded_at,
        })
    }

    /// NODE-LOCAL legal transition application.
    pub fn apply(self, event: PublicationEvent) -> Result<Self> {
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
        let mut transitions = self.transitions;
        let sequence = self.transition_sequence + 1;
        transitions.push(PublicationTransition {
            sequence,
            phase,
            disposition: Some(disposition),
            recorded_at: None,
        });
        Ok(Self {
            transition_sequence: sequence,
            transitions,
            phase,
            disposition: Some(disposition),
            ..self
        })
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationTombstone {
    pub transaction_id: PublicationTransactionId,
    pub target: PublicationTarget,
    pub generation: PublicationGenerationEvidence,
    pub digest: ContentDigest,
    pub outcome: PublicationDisposition,
    pub adopted: bool,
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
            transaction_id: journal.transaction_id.clone(),
            target: journal.target.clone(),
            generation: journal.generation.clone(),
            digest: journal.digest.clone(),
            outcome: *disposition,
            adopted: true,
        })
    }

    /// NODE-LOCAL retention eligibility predicate.
    pub fn retention_eligible(&self) -> bool {
        self.adopted
    }
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
    artifact_manifest: RawArtifactManifest,
    paths: RawPaths,
    #[serde(default)]
    transitions: Vec<RawTransition>,
    transition_sequence: u64,
    phase: String,
    disposition: Option<String>,
    recorded_at: Option<String>,
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
    fn into_paths(self) -> Result<PublicationPaths> {
        validate_relative_path("publication target path evidence", &self.target)?;
        validate_relative_path("publication staging path evidence", &self.staging)?;
        validate_relative_path("publication backup path evidence", &self.backup)?;
        validate_relative_path("publication journal path evidence", &self.journal)?;
        validate_relative_path("publication quarantine path evidence", &self.quarantine)?;
        Ok(PublicationPaths {
            target: self.target,
            staging: self.staging,
            backup: self.backup,
            journal: self.journal,
            quarantine: self.quarantine,
        })
    }
}

#[derive(Deserialize)]
struct RawTransition {
    sequence: u64,
    phase: String,
    disposition: Option<String>,
    recorded_at: Option<String>,
}

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
    paths: &PublicationPaths,
    target: &PublicationTarget,
    transaction: &PublicationTransactionId,
) -> serde_json::Value {
    let expected = relative_path_evidence(target, transaction);
    serde_json::json!({
        "target": relative_or_expected(&paths.target, &expected.target),
        "staging": relative_or_expected(&paths.staging, &expected.staging),
        "backup": relative_or_expected(&paths.backup, &expected.backup),
        "journal": relative_or_expected(&paths.journal, &expected.journal),
        "quarantine": relative_or_expected(&paths.quarantine, &expected.quarantine),
    })
}

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

fn relative_or_expected(path: &Path, expected: &Path) -> PathBuf {
    if !path.is_absolute() {
        return path.to_path_buf();
    }
    expected.to_path_buf()
}

fn is_known_tenant_file(relative_path: &Path) -> bool {
    relative_path == Path::new(crate::index::index_metadata::METADATA_FILE)
        || relative_path == Path::new(super::config::SETTINGS_FILE)
        || relative_path == Path::new(super::config::RULES_FILE)
        || relative_path == Path::new(super::config::SYNONYMS_FILE)
        || relative_path == Path::new(crate::index::oplog::COMMITTED_SEQ_FILE)
}

fn starts_with_known_tenant_dir(relative_path: &Path) -> bool {
    relative_path.starts_with(crate::index::oplog::OPLOG_DIR)
        || relative_path.starts_with(crate::index::write_queue::PERSISTED_VECTORS_DIR)
        || relative_path.starts_with(crate::dictionaries::persistence::DICTIONARIES_DIR)
        || relative_path.starts_with(crate::recommend::rules::RECOMMEND_RULES_DIR)
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
