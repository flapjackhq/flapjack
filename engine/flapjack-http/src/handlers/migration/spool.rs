#![allow(dead_code)]
// Stage 2 builds this migration-local persistence owner before later stages wire callers.
use chrono::{DateTime, Duration, Utc};
use fs2::{available_space, FileExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SPOOL_ROOT: &str = "migration_exports";
const JOBS_DIR: &str = "jobs";
const MANIFEST_FILE: &str = "manifest.json";
const ROOT_LOCK_FILE: &str = ".root.lock";
const JOB_LOCK_FILE: &str = ".job.lock";
// Keep the Stage 2 filename so in-progress jobs remain resumable across upgrades.
const COMPLETED_DOCUMENTS_FILE: &str = "completed_object_ids";
const COMPLETED_RULES_FILE: &str = "completed_rule_ids";
const COMPLETED_SYNONYMS_FILE: &str = "completed_synonym_ids";
const TEMP_PREFIX: &str = ".fj-spool-tmp-";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct SpoolLimits {
    pub max_compressed_page_bytes: u64,
    pub max_decompressed_page_bytes: u64,
    pub max_items_per_resource: u64,
    pub max_bytes_per_job: u64,
    pub max_global_bytes: u64,
    pub minimum_free_bytes: u64,
    pub max_staged_artifacts: u64,
    pub max_staged_artifact_bytes: u64,
    pub retention_seconds: i64,
}

impl Default for SpoolLimits {
    fn default() -> Self {
        Self {
            max_compressed_page_bytes: 8 * 1024 * 1024,
            max_decompressed_page_bytes: 64 * 1024 * 1024,
            max_items_per_resource: 10_000,
            max_bytes_per_job: 4 * 1024 * 1024 * 1024,
            max_global_bytes: 16 * 1024 * 1024 * 1024,
            minimum_free_bytes: 512 * 1024 * 1024,
            max_staged_artifacts: 8,
            max_staged_artifact_bytes: 128 * 1024 * 1024,
            retention_seconds: 24 * 60 * 60,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ResourceDenominators {
    pub settings: u64,
    pub documents: u64,
    pub rules: u64,
    pub synonyms: u64,
    pub config: u64,
}

impl ResourceDenominators {
    fn total(self) -> u64 {
        self.settings + self.documents + self.rules + self.synonyms + self.config
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default, PartialEq, Eq)]
struct ResourceCounters {
    settings: u64,
    documents: u64,
    rules: u64,
    synonyms: u64,
    config: u64,
}

impl ResourceCounters {
    fn total(self) -> u64 {
        self.settings + self.documents + self.rules + self.synonyms + self.config
    }

    fn from_visible_artifacts<'a>(artifacts: impl Iterator<Item = &'a ArtifactManifest>) -> Self {
        let mut counters = Self::default();
        for artifact in artifacts {
            counters.add(artifact.kind, artifact.item_count);
        }
        counters
    }

    fn add(&mut self, kind: ArtifactKind, count: u64) {
        match kind {
            ArtifactKind::Settings => self.settings += count,
            ArtifactKind::DocumentPage => self.documents += count,
            ArtifactKind::RulesPage => self.rules += count,
            ArtifactKind::SynonymsPage => self.synonyms += count,
            ArtifactKind::Config => self.config += count,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) enum ArtifactKind {
    Settings,
    DocumentPage,
    RulesPage,
    SynonymsPage,
    Config,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObjectResource {
    Documents,
    Rules,
    Synonyms,
}

impl ObjectResource {
    fn artifact_kind(self) -> ArtifactKind {
        match self {
            Self::Documents => ArtifactKind::DocumentPage,
            Self::Rules => ArtifactKind::RulesPage,
            Self::Synonyms => ArtifactKind::SynonymsPage,
        }
    }
}

impl ArtifactKind {
    fn prefix(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::DocumentPage => "documents",
            Self::RulesPage => "rules",
            Self::SynonymsPage => "synonyms",
            Self::Config => "config",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum LifecycleState {
    Running,
    Accepted,
    Failed,
    Deleting,
    Deleted,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
enum ArtifactState {
    Staged,
    Visible,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct ArtifactManifest {
    kind: ArtifactKind,
    state: ArtifactState,
    temp_path: String,
    final_path: String,
    compressed_bytes: u64,
    decompressed_bytes: u64,
    item_count: u64,
    digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
struct SidecarManifest {
    generation: u64,
    length: u64,
    digest: String,
    count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ResourceCompletion {
    pub complete: bool,
    pub count: u64,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct ResourceCompletions {
    pub settings: ResourceCompletion,
    pub documents: ResourceCompletion,
    pub rules: ResourceCompletion,
    pub synonyms: ResourceCompletion,
}

impl ResourceCompletions {
    fn all_complete(&self) -> bool {
        self.settings.complete
            && self.documents.complete
            && self.rules.complete
            && self.synonyms.complete
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SpoolManifest {
    job_uuid: Uuid,
    public_handle: String,
    checkpoint_handle: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    lifecycle: LifecycleState,
    source_identity_digest: String,
    limits: SpoolLimits,
    counters: ResourceCounters,
    denominators: ResourceDenominators,
    bytes_committed: u64,
    artifacts: Vec<ArtifactManifest>,
    completed_objects: SidecarManifest,
    #[serde(default)]
    completed_rules: SidecarManifest,
    #[serde(default)]
    completed_synonyms: SidecarManifest,
    #[serde(default)]
    resource_completions: ResourceCompletions,
    deleted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PublicExportView {
    pub job_uuid: Uuid,
    pub public_handle: String,
    pub checkpoint_handle: String,
    pub state: String,
    pub progress: SpoolProgress,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpoolProgress {
    pub completed: u64,
    pub total: u64,
    pub ratio: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ExportCheckpoint {
    pub job_uuid: Uuid,
    pub state: String,
    pub progress: SpoolProgress,
    pub resources: ResourceCompletions,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedArtifactForTest {
    pub temp_path: String,
    pub final_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpoolErrorKind {
    Io,
    ManifestCorrupt,
    JobNotFound,
    PublicHandleNotFound,
    JobDeleting,
    CompressedPageBytesExceeded,
    DecompressedPageBytesExceeded,
    ResourceItemCountExceeded,
    JobBytesExceeded,
    GlobalBytesExceeded,
    FreeSpaceFloor,
    StagedArtifactCountExceeded,
    StagedArtifactBytesExceeded,
    InvalidRelativePath,
    InvalidSourceIdentityDigest,
    CheckpointHandleNotFound,
    SourceIdentityMismatch,
    ResourceVerificationFailed,
    ResourceComplete,
    ResourcesIncomplete,
    JobTerminal,
}

#[derive(Debug)]
pub(crate) struct SpoolError {
    kind: SpoolErrorKind,
    source: Option<io::Error>,
}

impl SpoolError {
    fn new(kind: SpoolErrorKind) -> Self {
        Self { kind, source: None }
    }

    fn io(error: io::Error) -> Self {
        Self {
            kind: SpoolErrorKind::Io,
            source: Some(error),
        }
    }

    pub(crate) fn kind(&self) -> SpoolErrorKind {
        self.kind
    }
}

impl fmt::Display for SpoolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "migration spool error: {:?}", self.kind)
    }
}

impl std::error::Error for SpoolError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|error| error as &(dyn std::error::Error + 'static))
    }
}

impl From<io::Error> for SpoolError {
    fn from(error: io::Error) -> Self {
        SpoolError::io(error)
    }
}

type SpoolResult<T> = Result<T, SpoolError>;

struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Clone)]
pub(crate) struct SpoolStore {
    root: PathBuf,
    limits: SpoolLimits,
    fixed_now: Option<DateTime<Utc>>,
    free_bytes: Option<u64>,
}

impl SpoolStore {
    pub(crate) fn new(data_root: &Path, limits: SpoolLimits) -> SpoolResult<Self> {
        Self::open(data_root, limits, None, None)
    }

    #[cfg(test)]
    pub(crate) fn new_for_tests(
        data_root: &Path,
        limits: SpoolLimits,
        now: DateTime<Utc>,
        free_bytes: u64,
    ) -> SpoolResult<Self> {
        Self::open(data_root, limits, Some(now), Some(free_bytes))
    }

    fn open(
        data_root: &Path,
        limits: SpoolLimits,
        fixed_now: Option<DateTime<Utc>>,
        free_bytes: Option<u64>,
    ) -> SpoolResult<Self> {
        let root = data_root.join(SPOOL_ROOT);
        fs::create_dir_all(root.join(JOBS_DIR))?;
        Ok(Self {
            root,
            limits,
            fixed_now,
            free_bytes,
        })
    }

    fn now(&self) -> DateTime<Utc> {
        self.fixed_now.unwrap_or_else(Utc::now)
    }

    pub(crate) fn create_export(
        &self,
        source_identity_digest: &str,
        denominators: ResourceDenominators,
    ) -> SpoolResult<PublicExportView> {
        validate_source_identity_digest(source_identity_digest)?;
        let _root_lock = self.lock_root()?;
        let job_uuid = Uuid::new_v4();
        let job_dir = self.job_dir(job_uuid);
        create_private_dir(&job_dir)?;
        let now = self.now();
        let manifest = SpoolManifest {
            job_uuid,
            public_handle: new_handle(),
            checkpoint_handle: new_handle(),
            created_at: now,
            expires_at: now + Duration::seconds(self.limits.retention_seconds),
            lifecycle: LifecycleState::Running,
            source_identity_digest: source_identity_digest.to_string(),
            limits: self.limits,
            counters: ResourceCounters::default(),
            denominators,
            bytes_committed: 0,
            artifacts: Vec::new(),
            completed_objects: SidecarManifest::default(),
            completed_rules: SidecarManifest::default(),
            completed_synonyms: SidecarManifest::default(),
            resource_completions: ResourceCompletions::default(),
            deleted_at: None,
        };
        self.commit_manifest(&manifest)?;
        Ok(public_view(&manifest))
    }

    /// Return a job's opaque public and checkpoint handles by UUID so callers
    /// that already hold the UUID can surface resume handles without a scan.
    pub(crate) fn handles(&self, job_uuid: Uuid) -> SpoolResult<(String, String)> {
        let _root_lock = self.lock_root()?;
        let manifest = self.read_manifest(job_uuid)?;
        Ok((manifest.public_handle, manifest.checkpoint_handle))
    }

    pub(crate) fn public_view(&self, handle: &str) -> SpoolResult<PublicExportView> {
        let _root_lock = self.lock_root()?;
        for job_uuid in self.job_uuids()? {
            let manifest = self.read_manifest(job_uuid)?;
            if manifest.public_handle == handle {
                return Ok(public_view(&manifest));
            }
        }
        Err(SpoolError::new(SpoolErrorKind::PublicHandleNotFound))
    }

    pub(crate) fn commit_settings(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        item_count: u64,
    ) -> SpoolResult<()> {
        self.commit_artifact(job_uuid, ArtifactKind::Settings, bytes, item_count)
    }

    pub(crate) fn commit_document_page(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        item_count: u64,
    ) -> SpoolResult<()> {
        self.commit_artifact(job_uuid, ArtifactKind::DocumentPage, bytes, item_count)
    }

    pub(crate) fn commit_rules_page(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        item_count: u64,
    ) -> SpoolResult<()> {
        self.commit_artifact(job_uuid, ArtifactKind::RulesPage, bytes, item_count)
    }

    pub(crate) fn commit_synonyms_page(
        &self,
        job_uuid: Uuid,
        bytes: &[u8],
        item_count: u64,
    ) -> SpoolResult<()> {
        self.commit_artifact(job_uuid, ArtifactKind::SynonymsPage, bytes, item_count)
    }

    pub(crate) fn commit_config_file(
        &self,
        job_uuid: Uuid,
        compressed_bytes: &[u8],
        decompressed_bytes: &[u8],
        item_count: u64,
    ) -> SpoolResult<()> {
        self.commit_artifact_payload(
            job_uuid,
            ArtifactKind::Config,
            compressed_bytes,
            decompressed_bytes.len() as u64,
            item_count,
        )
    }

    pub(crate) fn commit_artifact(
        &self,
        job_uuid: Uuid,
        kind: ArtifactKind,
        bytes: &[u8],
        item_count: u64,
    ) -> SpoolResult<()> {
        self.commit_artifact_payload(job_uuid, kind, bytes, bytes.len() as u64, item_count)
    }

    fn commit_artifact_payload(
        &self,
        job_uuid: Uuid,
        kind: ArtifactKind,
        bytes: &[u8],
        decompressed: u64,
        item_count: u64,
    ) -> SpoolResult<()> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        self.ensure_writable(&manifest)?;
        ensure_resource_incomplete(&manifest, kind)?;
        let compressed = bytes.len() as u64;
        self.validate_artifact_limits(&manifest, kind, compressed, decompressed, item_count)?;

        let artifact = ArtifactManifest {
            kind,
            state: ArtifactState::Staged,
            temp_path: format!("{TEMP_PREFIX}{}-{}.tmp", kind.prefix(), Uuid::new_v4()),
            final_path: format!("{}-{}.bin", kind.prefix(), Uuid::new_v4()),
            compressed_bytes: compressed,
            decompressed_bytes: decompressed,
            item_count,
            digest: hex_digest(bytes),
        };
        validate_relative(&artifact.temp_path)?;
        validate_relative(&artifact.final_path)?;
        manifest.artifacts.push(artifact.clone());
        self.commit_manifest(&manifest)?;

        match self.write_and_publish_artifact(job_uuid, &artifact, bytes) {
            Ok(()) => {
                let mut manifest = self.read_manifest(job_uuid)?;
                if let Some(staged) = manifest
                    .artifacts
                    .iter_mut()
                    .find(|entry| entry.final_path == artifact.final_path)
                {
                    staged.state = ArtifactState::Visible;
                }
                manifest.bytes_committed += compressed;
                manifest.counters.add(kind, item_count);
                self.commit_manifest(&manifest)
            }
            Err(error) => {
                let _ = self.remove_artifact_paths(job_uuid, &artifact);
                let _ = self.remove_manifest_artifact(job_uuid, &artifact.final_path);
                Err(error)
            }
        }
    }

    pub(crate) fn mark_completed_object_ids(
        &self,
        job_uuid: Uuid,
        object_ids: &[&str],
    ) -> SpoolResult<()> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        self.ensure_writable(&manifest)?;
        let path = self.completed_sidecar_path(job_uuid);
        let existing = self.completed_object_ids_from_manifest(job_uuid, &manifest)?;
        let mut next = existing.clone();
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&path)?;
        file.set_len(manifest.completed_objects.length)?;
        file.seek(io::SeekFrom::Start(manifest.completed_objects.length))?;
        for object_id in object_ids {
            if !next.iter().any(|completed| completed == object_id) {
                writeln!(file, "{object_id}")?;
                next.push((*object_id).to_string());
            }
        }
        file.sync_all()?;
        let metadata = file.metadata()?;
        drop(file);
        let bytes = fs::read(&path)?;
        manifest.completed_objects = SidecarManifest {
            generation: manifest.completed_objects.generation + 1,
            length: metadata.len(),
            digest: hex_digest(&bytes),
            count: next.len() as u64,
        };
        self.commit_manifest(&manifest)
    }

    pub(crate) fn is_object_completed(&self, job_uuid: Uuid, object_id: &str) -> SpoolResult<bool> {
        Ok(self
            .completed_object_ids(job_uuid)?
            .iter()
            .any(|completed| completed == object_id))
    }

    pub(crate) fn completed_object_ids(&self, job_uuid: Uuid) -> SpoolResult<Vec<String>> {
        let manifest = self.read_manifest(job_uuid)?;
        self.completed_object_ids_from_manifest(job_uuid, &manifest)
    }

    fn completed_object_ids_from_manifest(
        &self,
        job_uuid: Uuid,
        manifest: &SpoolManifest,
    ) -> SpoolResult<Vec<String>> {
        self.completed_resource_ids_from_manifest(job_uuid, manifest, ObjectResource::Documents)
    }

    pub(crate) fn delete_export_artifacts(
        &self,
        job_uuid: Uuid,
        expected_source_identity_digest: &str,
    ) -> SpoolResult<bool> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        if manifest.source_identity_digest != expected_source_identity_digest {
            return Ok(false);
        }
        manifest.lifecycle = LifecycleState::Deleting;
        manifest.deleted_at = Some(self.now());
        self.commit_manifest(&manifest)?;
        for artifact in visible_artifacts(&manifest) {
            let path = self.job_dir(job_uuid).join(&artifact.final_path);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        let sidecar_path = self.completed_sidecar_path(job_uuid);
        if sidecar_path.exists() {
            fs::remove_file(sidecar_path)?;
        }
        manifest.artifacts.clear();
        manifest.bytes_committed = 0;
        manifest.counters = ResourceCounters::default();
        manifest.completed_objects = SidecarManifest::default();
        for resource in [ObjectResource::Rules, ObjectResource::Synonyms] {
            let path = self.resource_sidecar_path(job_uuid, resource);
            if path.exists() {
                fs::remove_file(path)?;
            }
        }
        manifest.completed_rules = SidecarManifest::default();
        manifest.completed_synonyms = SidecarManifest::default();
        manifest.lifecycle = LifecycleState::Deleted;
        self.commit_manifest(&manifest)?;
        sync_dir(&self.job_dir(job_uuid))?;
        Ok(true)
    }

    pub(crate) fn collect_garbage(&self) -> SpoolResult<()> {
        let _root_lock = self.lock_root()?;
        for job_uuid in self.job_uuids()? {
            let _job_lock = self.lock_job(job_uuid)?;
            let manifest = self.read_manifest(job_uuid)?;
            self.clean_store_temp_files(job_uuid)?;
            if manifest.lifecycle == LifecycleState::Deleted && manifest.expires_at <= self.now() {
                self.write_tombstone(&manifest)?;
            }
        }
        Ok(())
    }

    pub(crate) fn recover(&self) -> SpoolResult<()> {
        let _root_lock = self.lock_root()?;
        for job_uuid in self.job_uuids()? {
            let _job_lock = self.lock_job(job_uuid)?;
            let mut manifest = self.read_manifest(job_uuid)?;
            let before = manifest.clone();
            self.recover_artifacts(job_uuid, &mut manifest)?;
            self.recover_resource_sidecar(job_uuid, &manifest, ObjectResource::Documents)?;
            self.recover_resource_sidecar(job_uuid, &manifest, ObjectResource::Rules)?;
            self.recover_resource_sidecar(job_uuid, &manifest, ObjectResource::Synonyms)?;
            if manifest != before {
                self.commit_manifest(&manifest)?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn manifest_json(&self, job_uuid: Uuid) -> SpoolResult<String> {
        fs::read_to_string(self.manifest_path(job_uuid)).map_err(SpoolError::from)
    }

    #[cfg(test)]
    pub(crate) fn tombstone_json(&self, job_uuid: Uuid) -> SpoolResult<String> {
        fs::read_to_string(self.job_dir(job_uuid).join("tombstone.json")).map_err(SpoolError::from)
    }

    pub(crate) fn visible_artifacts(&self, job_uuid: Uuid) -> SpoolResult<Vec<String>> {
        let manifest = self.read_manifest(job_uuid)?;
        Ok(visible_artifacts(&manifest)
            .map(|artifact| artifact.final_path.clone())
            .collect())
    }

    #[cfg(test)]
    pub(crate) fn pre_register_artifact_for_test(
        &self,
        job_uuid: Uuid,
        kind: ArtifactKind,
        content: &str,
    ) -> SpoolResult<StagedArtifactForTest> {
        let _root_lock = self.lock_root()?;
        let _job_lock = self.lock_job(job_uuid)?;
        let mut manifest = self.read_manifest(job_uuid)?;
        let artifact = ArtifactManifest {
            kind,
            state: ArtifactState::Staged,
            temp_path: format!("{TEMP_PREFIX}{}-test.tmp", kind.prefix()),
            final_path: format!("{}-test.bin", kind.prefix()),
            compressed_bytes: content.len() as u64,
            decompressed_bytes: content.len() as u64,
            item_count: 1,
            digest: hex_digest(content.as_bytes()),
        };
        manifest.artifacts.push(artifact.clone());
        self.commit_manifest(&manifest)?;
        Ok(StagedArtifactForTest {
            temp_path: artifact.temp_path,
            final_path: artifact.final_path,
        })
    }

    pub(crate) fn job_dir(&self, job_uuid: Uuid) -> PathBuf {
        self.root.join(JOBS_DIR).join(job_uuid.to_string())
    }

    pub(crate) fn completed_sidecar_path(&self, job_uuid: Uuid) -> PathBuf {
        self.resource_sidecar_path(job_uuid, ObjectResource::Documents)
    }
}

#[path = "spool_support.rs"]
mod spool_support;
use spool_support::*;

#[path = "spool_transaction.rs"]
mod spool_transaction;
use spool_transaction::*;

#[path = "spool_lifecycle.rs"]
mod spool_lifecycle;
use spool_lifecycle::*;

#[cfg(test)]
#[path = "spool_tests.rs"]
mod spool_tests;

#[cfg(test)]
#[path = "export_resume_tests.rs"]
mod export_resume_tests;
