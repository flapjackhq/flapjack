#![allow(dead_code)]

use super::algolia_client::{
    AlgoliaClient, AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord, BrowseError,
};
use super::source_snapshot::{canonical_json_bytes, SourceSnapshot, SourceSnapshotBuilder};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

// The futures stay `Send` so the export orchestration composes into an axum
// handler; the raw-page callbacks are likewise `Send` because they carry the
// snapshot builder and store-backed sink across await points.
pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, AlgoliaClientError>> + Send + 'a>>;

pub(super) type PageConsumer<'a> =
    dyn FnMut(Vec<Value>) -> Result<(), AlgoliaClientError> + Send + 'a;

pub(super) trait MigrationSourceReader {
    fn app_id(&self) -> &str;
    fn source_name(&self) -> &str;
    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord>;
    fn read_settings(&mut self) -> SourceFuture<'_, Value>;
    /// Fetch the complete settings JSON for an arbitrary index name. This is the
    /// single low-level replica read the shared collector composes; it performs
    /// no parsing or list traversal of its own.
    fn read_index_settings<'a>(&'a mut self, index_name: &'a str) -> SourceFuture<'a, Value>;
    fn require_unretrievable_access<'a>(&'a mut self, settings: &'a Value) -> SourceFuture<'a, ()>;
    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
    fn read_rules<'a>(&'a mut self, consume_page: &'a mut PageConsumer<'a>)
        -> SourceFuture<'a, ()>;
    fn read_synonyms<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}

pub(super) trait SourceExportSink {
    fn commit_settings(&mut self, settings: &Value) -> Result<(), AlgoliaClientError>;
    fn commit_document_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError>;
    fn commit_rule_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError>;
    fn commit_synonym_page(&mut self, page: &[Value]) -> Result<(), AlgoliaClientError>;
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct SourceIdentity {
    digest: String,
    updated_at: String,
    document_metadata_count: u64,
    snapshot: SourceSnapshot,
}

impl SourceIdentity {
    pub(super) fn new(
        app_id: &str,
        source_name: &str,
        metadata: &AlgoliaIndexRecord,
        snapshot: SourceSnapshot,
    ) -> Result<Self, AlgoliaClientError> {
        validate_metadata(source_name, metadata, &snapshot)?;
        Ok(Self {
            digest: source_identity_digest(app_id, source_name, metadata, &snapshot),
            updated_at: metadata.updated_at.clone(),
            document_metadata_count: metadata.entries,
            snapshot,
        })
    }

    pub(super) fn digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn updated_at(&self) -> &str {
        &self.updated_at
    }

    pub(super) fn document_metadata_count(&self) -> u64 {
        self.document_metadata_count
    }

    pub(super) fn snapshot(&self) -> &SourceSnapshot {
        &self.snapshot
    }
}

impl fmt::Debug for SourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceIdentity")
            .field("digest", &self.digest)
            .field("updated_at", &self.updated_at)
            .field("document_metadata_count", &self.document_metadata_count)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcceptedSourceExport {
    identity: SourceIdentity,
}

impl AcceptedSourceExport {
    pub(super) fn identity(&self) -> &SourceIdentity {
        &self.identity
    }
}

pub(super) struct AlgoliaSourceReader {
    app_id: String,
    source_name: String,
    client: AlgoliaClient,
}

impl AlgoliaSourceReader {
    pub(super) fn new(
        app_id: &str,
        api_key: &str,
        source_name: &str,
    ) -> Result<Self, AlgoliaClientError> {
        let client = AlgoliaClient::for_source(app_id, api_key, source_name)?;
        Ok(Self {
            app_id: app_id.to_string(),
            source_name: source_name.to_string(),
            client,
        })
    }
}

impl fmt::Debug for AlgoliaSourceReader {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AlgoliaSourceReader")
            .field("app_id", &"<scrubbed>")
            .field("source_name", &"<scrubbed>")
            .finish_non_exhaustive()
    }
}

impl MigrationSourceReader for AlgoliaSourceReader {
    fn app_id(&self) -> &str {
        &self.app_id
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        Box::pin(async move { self.client.wait_for_quiescent_source().await })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, Value> {
        Box::pin(async move { self.client.settings().await })
    }

    fn read_index_settings<'a>(&'a mut self, index_name: &'a str) -> SourceFuture<'a, Value> {
        Box::pin(async move { self.client.index_settings(index_name).await })
    }

    fn require_unretrievable_access<'a>(&'a mut self, settings: &'a Value) -> SourceFuture<'a, ()> {
        Box::pin(async move { self.client.require_unretrievable_access(settings).await })
    }

    fn read_documents<'a>(
        &'a mut self,
        mut consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            self.client
                .browse_documents(&mut consume_page)
                .await
                .map_err(flatten_browse_error)
        })
    }

    fn read_rules<'a>(
        &'a mut self,
        mut consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            self.client
                .paginated_hits("rules/search", &mut consume_page)
                .await
                .map_err(flatten_browse_error)
        })
    }

    fn read_synonyms<'a>(
        &'a mut self,
        mut consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            self.client
                .paginated_hits("synonyms/search", &mut consume_page)
                .await
                .map_err(flatten_browse_error)
        })
    }
}

pub(super) async fn collect_quiescent_source_snapshot<R>(
    reader: &mut R,
) -> Result<SourceIdentity, AlgoliaClientError>
where
    R: MigrationSourceReader,
{
    let metadata = reader.wait_for_quiescent_source().await?;
    let snapshot = read_source_snapshot(reader, &mut NoopSink).await?;
    SourceIdentity::new(reader.app_id(), reader.source_name(), &metadata, snapshot)
}

/// Collect the complete source settings for every replica named in the primary
/// settings' `replicas` list. Each string entry is parsed through the single
/// canonical replica parser and its settings fetched exactly once; the returned
/// map is keyed by replica index name and holds the full response JSON.
///
/// Absent `replicas` performs zero index-specific reads. Malformed primary
/// `replicas` *shapes* (non-array, non-string entries) are left to the existing
/// translation validation owner, so non-string entries are skipped here rather
/// than rejected. A string entry that fails the canonical parser is a fail-closed
/// validation error with a single static, scrubbed message.
pub(super) async fn collect_replica_settings<R>(
    reader: &mut R,
    primary_settings: &Value,
) -> Result<BTreeMap<String, Value>, AlgoliaClientError>
where
    R: MigrationSourceReader,
{
    let mut collected = BTreeMap::new();
    let Some(entries) = primary_settings.get("replicas").and_then(Value::as_array) else {
        return Ok(collected);
    };

    for entry in entries {
        let Some(raw) = entry.as_str() else {
            continue;
        };
        let parsed = flapjack::index::replica::parse_replica_entry(raw)
            .map_err(|_| replica_entry_validation_error())?;
        let name = parsed.name().to_string();
        if collected.contains_key(&name) {
            continue;
        }
        let settings = reader.read_index_settings(&name).await?;
        collected.insert(name, settings);
    }

    Ok(collected)
}

fn replica_entry_validation_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Validation,
        "Algolia replica entry could not be parsed for migration",
    )
}

pub(super) async fn accept_source_export<R, S>(
    reader: &mut R,
    sink: &mut S,
) -> Result<AcceptedSourceExport, AlgoliaClientError>
where
    R: MigrationSourceReader,
    S: SourceExportSink + Send,
{
    let pre_identity = collect_quiescent_source_snapshot(reader).await?;
    let exported_snapshot = read_source_snapshot(reader, sink).await?;
    let final_metadata = reader.wait_for_quiescent_source().await?;
    let exported_identity = SourceIdentity::new(
        reader.app_id(),
        reader.source_name(),
        &final_metadata,
        exported_snapshot,
    )?;

    // Algolia browse cursors expire and browse order is not stable. Persisted
    // resume state must use exact membership and hashes, never cursors or
    // scalar ordering watermarks.
    if pre_identity != exported_identity {
        return Err(source_drift_error());
    }

    Ok(AcceptedSourceExport {
        identity: pre_identity,
    })
}

pub(super) async fn read_source_snapshot<R, S>(
    reader: &mut R,
    sink: &mut S,
) -> Result<SourceSnapshot, AlgoliaClientError>
where
    R: MigrationSourceReader,
    S: SourceExportSink + Send,
{
    let mut builder = SourceSnapshotBuilder::new();
    let settings = reader.read_settings().await?;
    reader.require_unretrievable_access(&settings).await?;
    builder.record_settings(&settings);
    sink.commit_settings(&settings)?;

    {
        let mut consume_page = |page: Vec<Value>| {
            builder.record_documents(&page)?;
            sink.commit_document_page(&page)
        };
        reader.read_documents(&mut consume_page).await?;
    }
    {
        let mut consume_page = |page: Vec<Value>| {
            builder.record_rules(&page)?;
            sink.commit_rule_page(&page)
        };
        reader.read_rules(&mut consume_page).await?;
    }
    {
        let mut consume_page = |page: Vec<Value>| {
            builder.record_synonyms(&page)?;
            sink.commit_synonym_page(&page)
        };
        reader.read_synonyms(&mut consume_page).await?;
    }

    builder.finish()
}

fn validate_metadata(
    source_name: &str,
    metadata: &AlgoliaIndexRecord,
    snapshot: &SourceSnapshot,
) -> Result<(), AlgoliaClientError> {
    if metadata.name != source_name || metadata.pending_task {
        return Err(source_drift_error());
    }
    if metadata.entries != snapshot.documents.count as u64 {
        return Err(AlgoliaClientError::new(
            AlgoliaErrorKind::Progress,
            "Algolia source metadata did not match exported documents",
        ));
    }
    Ok(())
}

fn source_identity_digest(
    app_id: &str,
    source_name: &str,
    metadata: &AlgoliaIndexRecord,
    snapshot: &SourceSnapshot,
) -> String {
    let identity = json!({
        "appID": app_id,
        "sourceIndex": source_name,
        "updatedAt": metadata.updated_at,
        "documentMetadataCount": metadata.entries,
        "resources": {
            "settings": resource_identity(&snapshot.settings),
            "documents": resource_identity(&snapshot.documents),
            "rules": resource_identity(&snapshot.rules),
            "synonyms": resource_identity(&snapshot.synonyms),
        }
    });
    hex::encode(Sha256::digest(canonical_json_bytes(&identity)))
}

fn resource_identity(resource: &super::source_snapshot::SourceResourceSnapshot) -> Value {
    json!({
        "count": resource.count,
        "hash": resource.hash,
    })
}

pub(super) fn source_drift_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Algolia source changed during export",
    )
}

fn flatten_browse_error(error: BrowseError<AlgoliaClientError>) -> AlgoliaClientError {
    match error {
        BrowseError::Client(error) | BrowseError::Consumer(error) => error,
    }
}

struct NoopSink;

impl SourceExportSink for NoopSink {
    fn commit_settings(&mut self, _settings: &Value) -> Result<(), AlgoliaClientError> {
        Ok(())
    }

    fn commit_document_page(&mut self, _page: &[Value]) -> Result<(), AlgoliaClientError> {
        Ok(())
    }

    fn commit_rule_page(&mut self, _page: &[Value]) -> Result<(), AlgoliaClientError> {
        Ok(())
    }

    fn commit_synonym_page(&mut self, _page: &[Value]) -> Result<(), AlgoliaClientError> {
        Ok(())
    }
}
