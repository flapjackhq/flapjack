use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind};
use super::source_reader::{
    collect_quiescent_source_snapshot, collect_replica_settings, read_source_snapshot,
    source_drift_error, MigrationSourceReader, SourceExportSink, SourceIdentity,
};
use super::source_snapshot::{source_item_hash, SourceSnapshot};
use super::spool::{ResourceDenominators, SpoolError, SpoolStore};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
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
/// `replica_settings` is the one exception to the counts-only shape: it is the
/// transient map of replica-owned source settings that migration translation
/// will later consume. It is deliberately excluded from the derived `Debug` (see
/// the custom impl below) so replica index names and settings values cannot leak
/// through diagnostics.
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
    pub(super) replica_settings: BTreeMap<String, Value>,
}

impl fmt::Debug for AcceptedExport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Only the replica *count* is safe to render; the index names and their
        // settings values are source material and must never reach a log line.
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
            .field("replica_settings_count", &self.replica_settings.len())
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
}

impl From<AlgoliaClientError> for ExportError {
    fn from(error: AlgoliaClientError) -> Self {
        Self::Source(error)
    }
}

impl From<SpoolError> for ExportError {
    fn from(error: SpoolError) -> Self {
        Self::Spool(error)
    }
}

/// Export the selected Algolia source into a fresh durable spool job.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn export_algolia_source<R: MigrationSourceReader>(
    store: &SpoolStore,
    job_uuid: Uuid,
    reader: &mut R,
) -> Result<AcceptedExport, ExportError> {
    store.create_migration_phase(job_uuid)?;
    run_export(store, reader, ExportRun::Fresh(job_uuid)).await
}

/// Export for the synchronous public import path. Replica settings are now
/// translated rather than hard-rejected, so a missing or unavailable replica
/// settings response is a real source failure and must surface as the typed,
/// credential-scrubbed Algolia error rather than an empty carried map.
pub(super) async fn export_algolia_source_for_import<R: MigrationSourceReader>(
    store: &SpoolStore,
    job_uuid: Uuid,
    reader: &mut R,
) -> Result<AcceptedExport, ExportError> {
    run_export(store, reader, ExportRun::Fresh(job_uuid)).await
}

/// Resume an in-flight export through its opaque checkpoint handle, refusing any
/// source whose identity digest no longer matches the persisted job. No Stage 3
/// route drives resume yet, so this seam is exercised only by the crash/drift
/// regression tests.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) async fn resume_algolia_source<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
    checkpoint_handle: &str,
) -> Result<AcceptedExport, ExportError> {
    run_export(store, reader, ExportRun::Resume(checkpoint_handle)).await
}

#[derive(Clone, Copy)]
enum ExportRun<'a> {
    Fresh(Uuid),
    Resume(&'a str),
}

/// Drive an export to completion and settle a fresh run's durable migration phase
/// on any failure, no matter which post-admission step produced it. Settlement is
/// centralized here — rather than duplicated across the body's `?` branches — so
/// no error path can surface while leaving `migration_phase.json` stuck `Running`.
async fn run_export<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
    run: ExportRun<'_>,
) -> Result<AcceptedExport, ExportError> {
    let fresh_job_uuid = match run {
        ExportRun::Fresh(job_uuid) => Some(job_uuid),
        ExportRun::Resume(_) => None,
    };
    let outcome = run_export_after_admission(store, reader, run).await;
    settle_fresh_export(store, fresh_job_uuid, outcome)
}

/// The admitted export body. Every failure returns through `?`; the caller settles
/// a fresh run's phase, so this stays a single linear path with no scattered
/// per-branch phase writes.
async fn run_export_after_admission<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
    run: ExportRun<'_>,
) -> Result<AcceptedExport, ExportError> {
    if let ExportRun::Fresh(job_uuid) = run {
        store.transition_migration_phase(job_uuid, super::spool::MigrationPhase::Exporting)?;
        ensure_export_not_cancelled(store, job_uuid)?;
    }

    // Pass one: a quiescent snapshot fixes the source identity we will require
    // again after export. Its per-resource counts seed the job denominators.
    let pre_identity = collect_quiescent_source_snapshot(reader).await?;
    if let ExportRun::Fresh(job_uuid) = run {
        ensure_export_not_cancelled(store, job_uuid)?;
    }

    // Bind the job before any commit. A resume refuses a changed source identity
    // here, before a single new artifact or sidecar entry is written.
    let job_uuid = match run {
        ExportRun::Resume(handle) => store.checkpoint(handle, pre_identity.digest())?.job_uuid,
        ExportRun::Fresh(job_uuid) => {
            store
                .create_export(
                    job_uuid,
                    pre_identity.digest(),
                    denominators(pre_identity.snapshot()),
                )?
                .job_uuid
        }
    };
    let (public_handle, checkpoint_handle) = store.handles(job_uuid)?;
    ensure_export_not_cancelled(store, job_uuid)?;

    match stream_and_accept(store, reader, job_uuid, &pre_identity).await {
        Ok((documents, rules, synonyms, replica_settings)) => Ok(AcceptedExport {
            job_uuid,
            public_handle,
            checkpoint_handle,
            source_index_name: reader.source_name().to_string(),
            source_identity_digest: pre_identity.digest().to_string(),
            documents,
            rules,
            synonyms,
            replica_settings,
        }),
        Err(error) => {
            // Fence the export manifest so no apparently complete partial export
            // survives. This is best-effort: the migration phase is settled by the
            // caller even if this fencing itself fails.
            let _ = store.fail_export(job_uuid);
            Err(error)
        }
    }
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

async fn stream_and_accept<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
    job_uuid: Uuid,
    pre_identity: &SourceIdentity,
) -> Result<(u64, u64, u64, BTreeMap<String, Value>), ExportError> {
    let mut sink = SpoolExportSink::open(store, job_uuid, reader.source_name())?;
    let exported = read_source_snapshot(reader, &mut sink)
        .await
        .map_err(export_error_from_source)?;
    ensure_export_not_cancelled(store, job_uuid)?;
    // Reuse the exact primary settings value committed during this pass; never
    // issue a second primary-settings request just to read the replicas list.
    let primary_settings = sink
        .committed_settings
        .take()
        .ok_or_else(|| ExportError::Source(missing_committed_settings()))?;

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

    // Collect each replica's complete source settings inside the accepted-state
    // window — before the final quiescence/drift proof — so the carried map is
    // bracketed by the same proof as the primary snapshot and cannot be paired
    // with a later source state. Any fetch failure is fail-closed and keeps its
    // typed, credential-scrubbed Algolia shape.
    let replica_settings = collect_replica_settings(reader, &primary_settings)
        .await
        .map_err(ExportError::Source)?;

    // Pass two: require quiescence again and prove the exported identity equals
    // the pre-snapshot identity. Any difference is source drift. This proof now
    // runs after the replica fetch, so a source change during replica collection
    // is caught here rather than silently accepted.
    ensure_export_not_cancelled(store, job_uuid)?;
    let final_metadata = reader.wait_for_quiescent_source().await?;
    ensure_export_not_cancelled(store, job_uuid)?;
    let exported_identity = SourceIdentity::new(
        reader.app_id(),
        reader.source_name(),
        &final_metadata,
        exported,
    )?;
    if *pre_identity != exported_identity {
        return Err(ExportError::Source(source_drift_error()));
    }

    ensure_export_not_cancelled(store, job_uuid)?;
    store.accept_export(job_uuid)?;
    Ok((documents, rules, synonyms, replica_settings))
}

fn ensure_export_not_cancelled(store: &SpoolStore, job_uuid: Uuid) -> Result<(), ExportError> {
    if store.cancel_requested(job_uuid)? {
        return Err(ExportError::Cancelled);
    }
    Ok(())
}

fn export_error_from_source(error: AlgoliaClientError) -> ExportError {
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

fn denominators(snapshot: &SourceSnapshot) -> ResourceDenominators {
    ResourceDenominators {
        settings: 1,
        documents: snapshot.documents.count as u64,
        rules: snapshot.rules.count as u64,
        synonyms: snapshot.synonyms.count as u64,
        config: 0,
    }
}

/// Streams raw source pages into the spool store, skipping object IDs a prior
/// run already committed so a resumed traversal writes only the missing items.
struct SpoolExportSink<'a> {
    store: &'a SpoolStore,
    job_uuid: Uuid,
    source_name: String,
    completed_documents: HashSet<String>,
    completed_rules: HashSet<String>,
    completed_synonyms: HashSet<String>,
    live_drift_barrier_reached: bool,
    /// The primary settings value seen during this export pass, captured so the
    /// replica collector can reuse it without a second primary-settings request.
    committed_settings: Option<Value>,
}

impl<'a> SpoolExportSink<'a> {
    fn open(store: &'a SpoolStore, job_uuid: Uuid, source_name: &str) -> Result<Self, ExportError> {
        Ok(Self {
            job_uuid,
            source_name: source_name.to_string(),
            completed_documents: id_set(store.completed_document_ids(job_uuid)?),
            completed_rules: id_set(store.completed_rule_ids(job_uuid)?),
            completed_synonyms: id_set(store.completed_synonym_ids(job_uuid)?),
            store,
            live_drift_barrier_reached: false,
            committed_settings: None,
        })
    }

    fn persist_page(
        &self,
        page: &[Value],
        completed: &HashSet<String>,
        commit: impl Fn(&[u8], &[&str]) -> Result<(), SpoolError>,
    ) -> Result<(), AlgoliaClientError> {
        let fresh: Vec<&Value> = page
            .iter()
            .filter(|item| match object_id(item) {
                Some(id) => !completed.contains(id),
                None => true,
            })
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let ids = fresh
            .iter()
            .map(|item| {
                object_id(item)
                    .map(str::to_string)
                    .ok_or_else(missing_object_id)
            })
            .collect::<Result<Vec<String>, _>>()?;
        let id_refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let bytes = serde_json::to_vec(&fresh).map_err(|_| serialize_error())?;
        commit(&bytes, &id_refs).map_err(spool_stream_error)
    }
}

impl SourceExportSink for SpoolExportSink<'_> {
    fn commit_settings(&mut self, settings: &Value) -> Result<(), AlgoliaClientError> {
        self.ensure_not_cancelled()?;
        let bytes = serde_json::to_vec(settings).map_err(|_| serialize_error())?;
        let hash = source_item_hash(settings);
        self.store
            .commit_settings_once(self.job_uuid, &bytes, &hash)
            .map_err(spool_stream_error)?;
        // Capture the exact committed value so replica collection reuses it.
        self.committed_settings = Some(settings.clone());
        Ok(())
    }

    fn commit_document_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.ensure_not_cancelled()?;
        let (store, job) = (self.store, self.job_uuid);
        let should_wait = !self.live_drift_barrier_reached
            && page_has_fresh_items(page, &self.completed_documents);
        self.persist_page(page, &self.completed_documents, |bytes, ids| {
            store.commit_document_page_with_ids(job, bytes, ids)
        })?;
        if should_wait {
            self.live_drift_barrier_reached = true;
            wait_for_live_drift_barrier(&self.source_name, self.job_uuid)?;
        }
        Ok(())
    }

    fn commit_rule_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.ensure_not_cancelled()?;
        let (store, job) = (self.store, self.job_uuid);
        self.persist_page(page, &self.completed_rules, |bytes, ids| {
            store.commit_rule_page_with_ids(job, bytes, ids)
        })
    }

    fn commit_synonym_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        self.ensure_not_cancelled()?;
        let (store, job) = (self.store, self.job_uuid);
        self.persist_page(page, &self.completed_synonyms, |bytes, ids| {
            store.commit_synonym_page_with_ids(job, bytes, ids)
        })
    }
}

impl SpoolExportSink<'_> {
    fn ensure_not_cancelled(&self) -> Result<(), AlgoliaClientError> {
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

fn object_id(item: &Value) -> Option<&str> {
    item.as_object()
        .and_then(|object| object.get("objectID"))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

fn page_has_fresh_items(page: &[Value], completed: &HashSet<String>) -> bool {
    page.iter().any(|item| match object_id(item) {
        Some(id) => !completed.contains(id),
        None => true,
    })
}

fn wait_for_live_drift_barrier(
    source_name: &str,
    job_uuid: Uuid,
) -> Result<(), AlgoliaClientError> {
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

fn live_drift_barrier_error() -> AlgoliaClientError {
    use super::algolia_client::AlgoliaErrorKind;
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Migration export live drift barrier was not released",
    )
}

fn missing_committed_settings() -> AlgoliaClientError {
    use super::algolia_client::AlgoliaErrorKind;
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Migration export did not capture primary settings before replica collection",
    )
}

fn missing_object_id() -> AlgoliaClientError {
    use super::algolia_client::AlgoliaErrorKind;
    AlgoliaClientError::new(
        AlgoliaErrorKind::Schema,
        "Algolia source item was missing a string objectID",
    )
}

fn serialize_error() -> AlgoliaClientError {
    use super::algolia_client::AlgoliaErrorKind;
    AlgoliaClientError::new(
        AlgoliaErrorKind::Schema,
        "Algolia source item could not be serialized for export",
    )
}

fn spool_stream_error(_error: SpoolError) -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Migration export could not persist source data",
    )
}

fn export_cancel_requested_error() -> AlgoliaClientError {
    AlgoliaClientError::new(AlgoliaErrorKind::Progress, EXPORT_CANCEL_REQUESTED_MESSAGE)
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod export_tests;
