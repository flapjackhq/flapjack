use super::algolia_client::AlgoliaClientError;
use super::source_reader::{
    collect_quiescent_source_snapshot, read_source_snapshot, source_drift_error,
    MigrationSourceReader, SourceExportSink, SourceIdentity,
};
use super::source_snapshot::{source_item_hash, SourceSnapshot};
use super::spool::{ResourceDenominators, SpoolError, SpoolStore};
use serde_json::Value;
use std::collections::HashSet;
use std::env;
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

/// Aggregate outcome of a durably accepted export. Carries only counts and the
/// opaque resume handles — never App ID, source name, API key, object IDs, or
/// raw records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcceptedExport {
    pub(super) job_uuid: Uuid,
    pub(super) public_handle: String,
    pub(super) checkpoint_handle: String,
    pub(super) documents: u64,
    pub(super) rules: u64,
    pub(super) synonyms: u64,
}

/// Scrubbed failure classification for the orchestration. Upstream and storage
/// failures stay separated so the HTTP layer can preserve the existing Algolia
/// status mapping without exposing source material.
#[derive(Debug)]
pub(super) enum ExportError {
    Source(AlgoliaClientError),
    Spool(SpoolError),
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
pub(super) async fn export_algolia_source<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
) -> Result<AcceptedExport, ExportError> {
    run_export(store, reader, None).await
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
    run_export(store, reader, Some(checkpoint_handle)).await
}

async fn run_export<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
    resume_handle: Option<&str>,
) -> Result<AcceptedExport, ExportError> {
    // Pass one: a quiescent snapshot fixes the source identity we will require
    // again after export. Its per-resource counts seed the job denominators.
    let pre_identity = collect_quiescent_source_snapshot(reader).await?;

    // Bind the job before any commit. A resume refuses a changed source identity
    // here, before a single new artifact or sidecar entry is written.
    let job_uuid = match resume_handle {
        Some(handle) => store.checkpoint(handle, pre_identity.digest())?.job_uuid,
        None => {
            store
                .create_export(pre_identity.digest(), denominators(pre_identity.snapshot()))?
                .job_uuid
        }
    };
    let (public_handle, checkpoint_handle) = store.handles(job_uuid)?;

    match stream_and_accept(store, reader, job_uuid, &pre_identity).await {
        Ok((documents, rules, synonyms)) => Ok(AcceptedExport {
            job_uuid,
            public_handle,
            checkpoint_handle,
            documents,
            rules,
            synonyms,
        }),
        Err(error) => {
            // Never leave an apparently complete partial export: fence the job
            // durably before surfacing the scrubbed error.
            let _ = store.fail_export(job_uuid);
            Err(error)
        }
    }
}

async fn stream_and_accept<R: MigrationSourceReader>(
    store: &SpoolStore,
    reader: &mut R,
    job_uuid: Uuid,
    pre_identity: &SourceIdentity,
) -> Result<(u64, u64, u64), ExportError> {
    let mut sink = SpoolExportSink::open(store, job_uuid, reader.source_name())?;
    let exported = read_source_snapshot(reader, &mut sink).await?;

    let documents = exported.documents.count as u64;
    let rules = exported.rules.count as u64;
    let synonyms = exported.synonyms.count as u64;

    // Mark each resource complete only after its committed count and hash match
    // the streamed snapshot. Settings completion happened inside the sink.
    store.complete_documents(job_uuid, documents, &exported.documents.hash)?;
    store.complete_rules(job_uuid, rules, &exported.rules.hash)?;
    store.complete_synonyms(job_uuid, synonyms, &exported.synonyms.hash)?;

    // Pass two: require quiescence again and prove the exported identity equals
    // the pre-snapshot identity. Any difference is source drift.
    let final_metadata = reader.wait_for_quiescent_source().await?;
    let exported_identity = SourceIdentity::new(
        reader.app_id(),
        reader.source_name(),
        &final_metadata,
        exported,
    )?;
    if *pre_identity != exported_identity {
        return Err(ExportError::Source(source_drift_error()));
    }

    store.accept_export(job_uuid)?;
    Ok((documents, rules, synonyms))
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
        let bytes = serde_json::to_vec(settings).map_err(|_| serialize_error())?;
        let hash = source_item_hash(settings);
        self.store
            .commit_settings_once(self.job_uuid, &bytes, &hash)
            .map_err(spool_stream_error)
    }

    fn commit_document_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
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
        let (store, job) = (self.store, self.job_uuid);
        self.persist_page(page, &self.completed_rules, |bytes, ids| {
            store.commit_rule_page_with_ids(job, bytes, ids)
        })
    }

    fn commit_synonym_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError> {
        let (store, job) = (self.store, self.job_uuid);
        self.persist_page(page, &self.completed_synonyms, |bytes, ids| {
            store.commit_synonym_page_with_ids(job, bytes, ids)
        })
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
    use super::algolia_client::AlgoliaErrorKind;
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Migration export could not persist source data",
    )
}

#[cfg(test)]
#[path = "export_tests.rs"]
mod export_tests;
