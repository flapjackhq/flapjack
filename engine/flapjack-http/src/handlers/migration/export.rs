//! Stub summary for engine/flapjack-http/src/handlers/migration/export.rs.

use super::algolia_client::{should_retry, AlgoliaClientError, AlgoliaErrorKind};
use super::source_reader::{
    admit_source_provider, collect_quiescent_source_snapshot, read_source_snapshot,
    source_drift_error, source_identity_from_reader, MigrationSourceReader,
    SourceConfigurationArtifact, SourceConfigurationRecord, SourceExportError,
    SourceExportErrorKind, SourceExportRecord, SourceExportSink, SourceIdentity,
};
use super::source_snapshot::{source_item_hash, SourceSnapshot};
use super::spool::{ExportCheckpoint, ResourceDenominators, SpoolError, SpoolStore};
use super::AsyncMigrationSourceProvider;
use serde_json::Value;
use std::collections::HashSet;
use std::env;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};
use uuid::Uuid;

const LIVE_DRIFT_SOURCE_ENV: &str = "FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_SOURCE";
const LIVE_DRIFT_BARRIER_DIR_ENV: &str = "FLAPJACK_ALGOLIA_LIVE_TEST_DRIFT_BARRIER_DIR";
const LIVE_DRIFT_OBSERVED_FILE: &str = "observed";
const LIVE_DRIFT_RELEASE_FILE: &str = "release";
const LIVE_DRIFT_BARRIER_TIMEOUT: Duration = Duration::from_secs(120);
const EXPORT_CANCEL_REQUESTED_MESSAGE: &str = "Migration export cancellation was requested";

/// Aggregate outcome of a durably accepted export. Carries only counts and the
/// opaque resume handles — never App ID, source name, API key, object IDs, or
/// raw records.
///
/// Configuration payloads deliberately stay out of this receipt: replica-owned
/// settings are durable spool artifacts, read back through `AcceptedSpoolReader`
/// by the translation owner rather than carried in memory alongside the counts.
#[derive(Clone, PartialEq)]
pub(super) struct AcceptedExport {
    pub(super) job_uuid: Uuid,
    pub(super) public_handle: String,
    pub(super) checkpoint_handle: String,
    pub(super) source_index_name: String,
    pub(super) source_identity_digest: String,
    pub(super) documents: u64,
    pub(super) rules: u64,
    pub(super) synonyms: u64,
}

impl fmt::Debug for AcceptedExport {
    /// TODO: Document AcceptedExport.fmt.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AcceptedExport")
            .field("job_uuid", &self.job_uuid)
            .field("public_handle", &self.public_handle)
            .field("checkpoint_handle", &self.checkpoint_handle)
            .field("source_index_name", &"<redacted>")
            .field("source_identity_digest", &self.source_identity_digest)
            .field("documents", &self.documents)
            .field("rules", &self.rules)
            .field("synonyms", &self.synonyms)
            .finish_non_exhaustive()
    }
}

/// Scrubbed failure classification for the orchestration. Upstream and storage
/// failures stay separated so the HTTP layer can preserve the existing Algolia
/// status mapping without exposing source material.
#[derive(Debug)]
pub(super) enum ExportError {
    Source(AlgoliaClientError),
    Spool(SpoolError),
    Cancelled,
    Interrupted,
}

impl From<AlgoliaClientError> for ExportError {
    fn from(error: AlgoliaClientError) -> Self {
        Self::Source(error)
    }
}

impl From<SourceExportError> for ExportError {
    fn from(error: SourceExportError) -> Self {
        Self::Source(error.into_inner())
    }
}

impl From<SpoolError> for ExportError {
    fn from(error: SpoolError) -> Self {
        Self::Spool(error)
    }
}

/// Export the selected Algolia source into a fresh durable spool job.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn export_algolia_source<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    job_uuid: Uuid,
    reader: &mut R,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<AcceptedExport, ExportError> {
    store.create_migration_phase(job_uuid)?;
    run_export(store, reader, job_uuid, expected_provider).await
}

/// Export for the synchronous public import path. Replica settings are now
/// translated rather than hard-rejected, so a missing or unavailable replica
/// settings response is a real source failure and must surface as the typed,
/// credential-scrubbed Algolia error rather than an empty carried map.
pub(super) async fn export_algolia_source_for_import<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    job_uuid: Uuid,
    reader: &mut R,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<AcceptedExport, ExportError> {
    run_export(store, reader, job_uuid, expected_provider).await
}

/// Resume an in-flight export through its opaque checkpoint handle, refusing any
/// source whose identity digest no longer matches the persisted job. No Stage 3
/// route drives resume yet, so this seam is exercised only by the crash/drift
/// regression tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn resume_algolia_source<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    checkpoint_handle: &str,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<AcceptedExport, ExportError> {
    let checkpoint =
        claim_validated_algolia_resume(store, reader, checkpoint_handle, expected_provider).await?;
    resume_claimed_algolia_source(store, reader, checkpoint, expected_provider).await
}

pub(super) async fn claim_validated_algolia_resume<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    checkpoint_handle: &str,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<ExportCheckpoint, ExportError> {
    admit_export_provider(reader, expected_provider)?;
    let source_identity = collect_quiescent_source_snapshot(reader).await?;
    store
        .claim_interrupted_export(checkpoint_handle, source_identity.digest())
        .map_err(Into::into)
}

pub(super) async fn resume_claimed_algolia_source<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    checkpoint: ExportCheckpoint,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<AcceptedExport, ExportError> {
    admit_export_provider(reader, expected_provider)?;
    run_claimed_resume_after_admission(store, reader, checkpoint).await
}

/// The durable path's single provider-admission call. It delegates to the shared
/// `source_reader::admit_source_provider` comparison so preview and export judge
/// provider identity by exactly one rule, and it runs before any source
/// observation or spool commit on every durable entry point.
fn admit_export_provider<R: MigrationSourceReader + ?Sized>(
    reader: &R,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<(), ExportError> {
    admit_source_provider(expected_provider, reader.source_provider()).map_err(ExportError::from)
}

/// Drive an export to completion and settle a fresh run's durable migration phase
/// on any failure, no matter which post-admission step produced it. Settlement is
/// centralized here — rather than duplicated across the body's `?` branches — so
/// no error path can surface while leaving `migration_phase.json` stuck `Running`.
async fn run_export<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    job_uuid: Uuid,
    expected_provider: AsyncMigrationSourceProvider,
) -> Result<AcceptedExport, ExportError> {
    let outcome = match admit_export_provider(reader, expected_provider) {
        Ok(()) => run_export_after_admission(store, reader, job_uuid).await,
        Err(error) => Err(error),
    };
    settle_fresh_export(store, Some(job_uuid), outcome)
}

/// The admitted export body. Every failure returns through `?`; the caller settles
/// a fresh run's phase, so this stays a single linear path with no scattered
/// per-branch phase writes.
async fn run_export_after_admission<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    fresh_job_uuid: Uuid,
) -> Result<AcceptedExport, ExportError> {
    store.transition_migration_phase(fresh_job_uuid, super::spool::MigrationPhase::Exporting)?;
    ensure_export_not_cancelled(store, fresh_job_uuid)?;

    // Pass one: a quiescent snapshot fixes the source identity we will require
    // again after export. Its per-resource counts seed the job denominators.
    let pre_identity = collect_quiescent_source_snapshot(reader).await?;
    ensure_export_not_cancelled(store, fresh_job_uuid)?;

    // Bind the job before any commit. A resume refuses a changed source identity
    // here, before a single new artifact or sidecar entry is written.
    let job_uuid = store
        .create_export(
            fresh_job_uuid,
            pre_identity.digest(),
            denominators(pre_identity.snapshot()),
        )?
        .job_uuid;
    let (public_handle, checkpoint_handle) = store.handles(job_uuid)?;
    ensure_export_not_cancelled(store, job_uuid)?;

    match stream_and_accept(store, reader, job_uuid, &pre_identity).await {
        Ok(counts) => Ok(AcceptedExport {
            job_uuid,
            public_handle,
            checkpoint_handle,
            source_index_name: reader.source_name().to_string(),
            source_identity_digest: pre_identity.digest().to_string(),
            documents: counts.documents,
            rules: counts.rules,
            synonyms: counts.synonyms,
        }),
        Err(error) => {
            if let Some(source_error) = retryable_source_error(&error) {
                if !store.source_error_can_interrupt_export(job_uuid)? {
                    let _ = store.fail_export(job_uuid);
                    return Err(error);
                }
                store.recover()?;
                store.interrupt_export(job_uuid, pre_identity.digest())?;
                tracing::info!(
                    %job_uuid,
                    error_kind = ?source_error.kind(),
                    "Algolia migration export interrupted after retryable source error"
                );
                return Err(ExportError::Interrupted);
            }
            // Terminal failures still fence the export manifest so no apparently
            // complete partial export survives. This remains best-effort: the
            // migration phase is settled by the caller even if fencing itself fails.
            let _ = store.fail_export(job_uuid);
            Err(error)
        }
    }
}

async fn run_claimed_resume_after_admission<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    checkpoint: ExportCheckpoint,
) -> Result<AcceptedExport, ExportError> {
    let (public_handle, checkpoint_handle) = store.handles(checkpoint.job_uuid)?;
    ensure_export_not_cancelled(store, checkpoint.job_uuid)?;

    match stream_resume_and_accept(store, reader, &checkpoint).await {
        Ok(counts) => Ok(AcceptedExport {
            job_uuid: checkpoint.job_uuid,
            public_handle,
            checkpoint_handle,
            source_index_name: reader.source_name().to_string(),
            source_identity_digest: checkpoint.source_identity_digest,
            documents: counts.documents,
            rules: counts.rules,
            synonyms: counts.synonyms,
        }),
        Err(error) => {
            if let Some(source_error) = retryable_source_error(&error) {
                if !store.source_error_can_interrupt_export(checkpoint.job_uuid)? {
                    let _ = store.fail_export(checkpoint.job_uuid);
                    return Err(error);
                }
                store.recover()?;
                store.interrupt_export(checkpoint.job_uuid, &checkpoint.source_identity_digest)?;
                tracing::info!(
                    job_uuid = %checkpoint.job_uuid,
                    error_kind = ?source_error.kind(),
                    "Algolia migration export interrupted after retryable source error"
                );
                return Err(ExportError::Interrupted);
            }
            let _ = store.fail_export(checkpoint.job_uuid);
            Err(error)
        }
    }
}

async fn stream_resume_and_accept<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    checkpoint: &ExportCheckpoint,
) -> Result<ExportedResourceCounts, ExportError> {
    let job_uuid = checkpoint.job_uuid;
    ensure_export_not_cancelled(store, job_uuid)?;
    let mut sink = SpoolExportSink::open(store, job_uuid, reader.source_name())?;
    let exported = read_source_snapshot(reader, &mut sink)
        .await
        .map_err(export_error_from_source)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    let documents = exported.documents.count as u64;
    let rules = exported.rules.count as u64;
    let synonyms = exported.synonyms.count as u64;
    store.complete_documents(job_uuid, documents, &exported.documents.hash)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    store.complete_rules(job_uuid, rules, &exported.rules.hash)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    store.complete_synonyms(job_uuid, synonyms, &exported.synonyms.hash)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    let final_observation = reader.observe_quiescent_source().await?;
    let exported_identity = source_identity_from_reader(reader, &final_observation, exported)?;
    if exported_identity.digest() != checkpoint.source_identity_digest {
        return Err(ExportError::Source(source_drift_error().into_inner()));
    }
    ensure_export_not_cancelled(store, job_uuid)?;
    store.accept_export(job_uuid)?;
    Ok(ExportedResourceCounts {
        documents,
        rules,
        synonyms,
    })
}

/// Settle a fresh run's durable migration phase after a failure. A settlement
/// persistence failure is surfaced rather than swallowed, so a broken terminal
/// write path fails closed instead of masquerading as the original error.
fn settle_fresh_export(
    store: &SpoolStore,
    fresh_job_uuid: Option<Uuid>,
    outcome: Result<AcceptedExport, ExportError>,
) -> Result<AcceptedExport, ExportError> {
    let Err(error) = outcome else {
        return outcome;
    };
    let Some(job_uuid) = fresh_job_uuid else {
        return Err(error);
    };
    let settlement = match error {
        ExportError::Cancelled => settle_cancelled_fresh_migration(store, job_uuid),
        ExportError::Interrupted => Ok(()),
        _ => fail_fresh_migration(store, job_uuid),
    };
    match settlement {
        Ok(()) => Err(error),
        Err(settlement_error) => Err(settlement_error),
    }
}

fn fail_fresh_migration(store: &SpoolStore, job_uuid: Uuid) -> Result<(), ExportError> {
    store.fail_migration(job_uuid)?;
    Ok(())
}

fn settle_cancelled_fresh_migration(store: &SpoolStore, job_uuid: Uuid) -> Result<(), ExportError> {
    store.cancel_migration(job_uuid)?;
    Ok(())
}

/// Capture one full export pass into the spool, then prove the source did not
/// change while it was being read.
async fn stream_and_accept<R: MigrationSourceReader + Send>(
    store: &SpoolStore,
    reader: &mut R,
    job_uuid: Uuid,
    pre_identity: &SourceIdentity,
) -> Result<ExportedResourceCounts, ExportError> {
    let mut sink = SpoolExportSink::open(store, job_uuid, reader.source_name())?;
    let exported = read_source_snapshot(reader, &mut sink)
        .await
        .map_err(export_error_from_source)?;
    ensure_export_not_cancelled(store, job_uuid)?;

    let documents = exported.documents.count as u64;
    let rules = exported.rules.count as u64;
    let synonyms = exported.synonyms.count as u64;

    // Mark each resource complete only after its committed count and hash match
    // the streamed snapshot. Settings completion happened inside the sink.
    ensure_export_not_cancelled(store, job_uuid)?;
    store.complete_documents(job_uuid, documents, &exported.documents.hash)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    store.complete_rules(job_uuid, rules, &exported.rules.hash)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    store.complete_synonyms(job_uuid, synonyms, &exported.synonyms.hash)?;

    // Pass two: require quiescence again and prove the exported identity equals
    // the pre-snapshot identity. Any difference is source drift. This proof now
    // runs after the derived-configuration reads, so a source change during that
    // collection is caught here rather than silently accepted.
    ensure_export_not_cancelled(store, job_uuid)?;
    let final_observation = reader.observe_quiescent_source().await?;
    ensure_export_not_cancelled(store, job_uuid)?;
    let exported_identity = source_identity_from_reader(reader, &final_observation, exported)?;
    if *pre_identity != exported_identity {
        return Err(ExportError::Source(source_drift_error().into_inner()));
    }

    ensure_export_not_cancelled(store, job_uuid)?;
    store.accept_export(job_uuid)?;
    Ok(ExportedResourceCounts {
        documents,
        rules,
        synonyms,
    })
}

/// The per-resource item counts one capture pass committed, named so the two
/// capture entry points return a self-describing value instead of a tuple.
struct ExportedResourceCounts {
    documents: u64,
    rules: u64,
    synonyms: u64,
}

fn ensure_export_not_cancelled(store: &SpoolStore, job_uuid: Uuid) -> Result<(), ExportError> {
    if store.cancel_requested(job_uuid)? {
        return Err(ExportError::Cancelled);
    }
    Ok(())
}

fn export_error_from_source(error: SourceExportError) -> ExportError {
    let error = error.into_inner();
    if is_export_cancel_error(&error) {
        ExportError::Cancelled
    } else {
        ExportError::Source(error)
    }
}

fn is_export_cancel_error(error: &AlgoliaClientError) -> bool {
    error.kind() == AlgoliaErrorKind::Progress
        && error.safe_message() == EXPORT_CANCEL_REQUESTED_MESSAGE
}

fn retryable_source_error(error: &ExportError) -> Option<&AlgoliaClientError> {
    match error {
        ExportError::Source(source_error) if should_retry(source_error.kind()) => {
            Some(source_error)
        }
        _ => None,
    }
}

fn denominators(snapshot: &SourceSnapshot) -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: snapshot.documents.count as u64,
        rules: snapshot.rules.count as u64,
        synonyms: snapshot.synonyms.count as u64,
        config: 0,
    }
}

/// Streams captured source artifacts into the spool store, skipping object IDs
/// a prior run already committed so a resumed traversal writes only the missing
/// items.
struct SpoolExportSink<'a> {
    store: &'a SpoolStore,
    job_uuid: Uuid,
    source_name: String,
    completed_documents: HashSet<String>,
    completed_rules: HashSet<String>,
    completed_synonyms: HashSet<String>,
    completed_replica_settings: HashSet<String>,
    live_drift_barrier_reached: bool,
}

impl<'a> SpoolExportSink<'a> {
    fn open(store: &'a SpoolStore, job_uuid: Uuid, source_name: &str) -> Result<Self, ExportError> {
        Ok(Self {
            job_uuid,
            source_name: source_name.to_string(),
            completed_documents: id_set(store.completed_document_ids(job_uuid)?),
            completed_rules: id_set(store.completed_rule_ids(job_uuid)?),
            completed_synonyms: id_set(store.completed_synonym_ids(job_uuid)?),
            completed_replica_settings: id_set(store.completed_derived_source_names(job_uuid)?),
            store,
            live_drift_barrier_reached: false,
        })
    }

    /// Persist one derived source's settings as a durable configuration
    /// artifact, so translation reads it back from the spool rather than from a
    /// map carried through the acceptance receipt.
    fn commit_derived_settings(
        &mut self,
        source_name: &str,
        settings: &Value,
    ) -> Result<(), SourceExportError> {
        if self.completed_replica_settings.contains(source_name) {
            return Ok(());
        }
        self.store
            .commit_derived_source_settings(self.job_uuid, source_name, settings)
            .map_err(spool_stream_error)?;
        self.completed_replica_settings
            .insert(source_name.to_string());
        Ok(())
    }

    fn commit_settings(&mut self, settings: &Value) -> Result<(), SourceExportError> {
        self.ensure_not_cancelled()?;
        let bytes = serde_json::to_vec(settings).map_err(|_| serialize_error())?;
        let hash = source_item_hash(settings);
        self.store
            .commit_settings_once(self.job_uuid, &bytes, &hash)
            .map_err(spool_stream_error)
    }

    /// Persist the fresh items of an object page, keyed by the stable IDs the
    /// neutral capture already validated.
    fn persist_document_page(
        &self,
        page: &[SourceExportRecord],
        completed: &HashSet<String>,
        commit: impl Fn(&[u8], &[&str]) -> Result<(), SpoolError>,
    ) -> Result<(), SourceExportError> {
        let fresh: Vec<&SourceExportRecord> = page
            .iter()
            .filter(|record| !completed.contains(record.stable_id()))
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let ids = fresh
            .iter()
            .map(|record| record.stable_id().to_string())
            .collect::<Vec<_>>();
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let payloads = fresh
            .iter()
            .map(|record| record.identity_payload())
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&payloads).map_err(|_| serialize_error())?;
        commit(&bytes, &id_refs).map_err(spool_stream_error)
    }

    fn persist_configuration_page(
        &self,
        page: &[SourceConfigurationRecord],
        completed: &HashSet<String>,
        commit: impl Fn(&[u8], &[&str]) -> Result<(), SpoolError>,
    ) -> Result<(), SourceExportError> {
        let fresh: Vec<&SourceConfigurationRecord> = page
            .iter()
            .filter(|record| !completed.contains(record.stable_id()))
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let ids = fresh
            .iter()
            .map(|record| record.stable_id().to_string())
            .collect::<Vec<_>>();
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let payloads = fresh
            .iter()
            .map(|record| record.identity_payload())
            .collect::<Vec<_>>();
        let bytes = serde_json::to_vec(&payloads).map_err(|_| serialize_error())?;
        commit(&bytes, &id_refs).map_err(spool_stream_error)
    }
}

impl SourceExportSink for SpoolExportSink<'_> {
    fn commit_configuration(
        &mut self,
        artifact: &SourceConfigurationArtifact,
    ) -> Result<(), SourceExportError> {
        self.ensure_not_cancelled()?;
        let (store, job) = (self.store, self.job_uuid);
        match artifact {
            SourceConfigurationArtifact::Settings { payload } => self.commit_settings(payload),
            SourceConfigurationArtifact::Rules { records } => {
                self.persist_configuration_page(records, &self.completed_rules, |bytes, ids| {
                    store.commit_rule_page_with_ids(job, bytes, ids)
                })
            }
            SourceConfigurationArtifact::Synonyms { records } => {
                self.persist_configuration_page(records, &self.completed_synonyms, |bytes, ids| {
                    store.commit_synonym_page_with_ids(job, bytes, ids)
                })
            }
            // Replica-owned settings belong to indexes derived from this source
            // rather than to the exported source itself, so they commit as
            // derived-configuration artifacts outside the job's own resource
            // completions instead of riding along on the acceptance receipt.
            SourceConfigurationArtifact::ReplicaSettings {
                source_name,
                payload,
            } => self.commit_derived_settings(source_name, payload),
        }
    }

    fn commit_document_page(
        &mut self,
        page: &[SourceExportRecord],
    ) -> Result<(), SourceExportError> {
        self.ensure_not_cancelled()?;
        let (store, job) = (self.store, self.job_uuid);
        let should_wait = !self.live_drift_barrier_reached
            && page_has_fresh_items(page, &self.completed_documents);
        self.persist_document_page(page, &self.completed_documents, |bytes, ids| {
            store.commit_document_page_with_ids(job, bytes, ids)
        })?;
        if should_wait {
            self.live_drift_barrier_reached = true;
            wait_for_live_drift_barrier(&self.source_name, self.job_uuid)?;
        }
        Ok(())
    }
}

impl SpoolExportSink<'_> {
    fn ensure_not_cancelled(&self) -> Result<(), SourceExportError> {
        match self.store.cancel_requested(self.job_uuid) {
            Ok(false) => Ok(()),
            Ok(true) => Err(export_cancel_requested_error()),
            Err(error) => Err(spool_stream_error(error)),
        }
    }
}

fn id_set(ids: Vec<String>) -> HashSet<String> {
    ids.into_iter().collect()
}

fn page_has_fresh_items(page: &[SourceExportRecord], completed: &HashSet<String>) -> bool {
    page.iter()
        .any(|item| !completed.contains(item.stable_id()))
}

/// TODO: Document wait_for_live_drift_barrier.
fn wait_for_live_drift_barrier(source_name: &str, job_uuid: Uuid) -> Result<(), SourceExportError> {
    let Ok(target_source) = env::var(LIVE_DRIFT_SOURCE_ENV) else {
        return Ok(());
    };
    if target_source != source_name {
        return Ok(());
    }
    let Ok(barrier_dir) = env::var(LIVE_DRIFT_BARRIER_DIR_ENV) else {
        return Ok(());
    };
    if barrier_dir.is_empty() {
        return Ok(());
    }

    let barrier_dir = PathBuf::from(barrier_dir);
    fs::create_dir_all(&barrier_dir).map_err(|_| live_drift_barrier_error())?;
    fs::write(
        barrier_dir.join(LIVE_DRIFT_OBSERVED_FILE),
        job_uuid.to_string(),
    )
    .map_err(|_| live_drift_barrier_error())?;

    let release_file = barrier_dir.join(LIVE_DRIFT_RELEASE_FILE);
    let deadline = Instant::now() + LIVE_DRIFT_BARRIER_TIMEOUT;
    while Instant::now() < deadline {
        if release_file.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(live_drift_barrier_error())
}

fn live_drift_barrier_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        "Migration export live drift barrier was not released",
    )
}

fn serialize_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Schema,
        "Source item could not be serialized for export",
    )
}

fn spool_stream_error(_error: SpoolError) -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        "Migration export could not persist source data",
    )
}

fn export_cancel_requested_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        EXPORT_CANCEL_REQUESTED_MESSAGE,
    )
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod export_tests;
