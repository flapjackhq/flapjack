#![allow(dead_code)]

use super::algolia_client::{
    AlgoliaClient, AlgoliaClientError, AlgoliaErrorKind, AlgoliaIndexRecord, BrowseError,
};
use super::meilisearch_client::MeilisearchClient;
use super::meilisearch_client::{MeilisearchClientError, MeilisearchErrorKind};
#[cfg(not(test))]
use super::source_identity_partitions::SourceIdentityConfig;
use super::source_identity_partitions::SourceIdentityVersion;
use super::source_snapshot::{canonical_json_bytes, SourceSnapshot, SourceSnapshotBuilder};
#[cfg(test)]
use super::source_test_support::identity_config_for_test;
use super::translation::{translate_settings_for_provider, SettingsSourceProvider};
use super::typesense_client::{
    TypesenseClient, TypesenseClientError, TypesenseErrorKind, TypesenseSourceObservation,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
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

pub(super) type MeilisearchSourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MeilisearchClientError>> + Send + 'a>>;

pub(super) type MeilisearchPageConsumer<'a> =
    dyn FnMut(Vec<Value>) -> Result<(), MeilisearchClientError> + Send + 'a;

pub(super) use super::meilisearch_client::MeilisearchSourceObservation;

pub(super) type TypesenseSourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TypesenseClientError>> + Send + 'a>>;

pub(super) type TypesensePageConsumer<'a> =
    dyn FnMut(Vec<Value>) -> Result<(), TypesenseClientError> + Send + 'a;

/// Raw Meilisearch export operations consumed by the provider adapter.
///
/// The protocol client owns HTTP and vendor schemas; this contract leaves
/// document identity normalization and shared snapshot integration to the
/// source reader.
pub(super) trait MeilisearchExportSource {
    fn observe_source(&mut self) -> MeilisearchSourceFuture<'_, MeilisearchSourceObservation>;
    fn read_settings(&mut self) -> MeilisearchSourceFuture<'_, Value>;
    fn require_read_access(&mut self) -> MeilisearchSourceFuture<'_, ()>;
    fn read_document_pages<'a>(
        &'a mut self,
        consume_page: &'a mut MeilisearchPageConsumer<'a>,
    ) -> MeilisearchSourceFuture<'a, MeilisearchSourceObservation>;
}

/// Raw Typesense export operations consumed by the provider adapter.
pub(super) trait TypesenseExportSource {
    fn observe_source(&mut self) -> TypesenseSourceFuture<'_, TypesenseSourceObservation>;
    fn read_settings(&mut self) -> TypesenseSourceFuture<'_, Value>;
    fn require_read_access(&mut self) -> TypesenseSourceFuture<'_, ()>;
    fn read_document_pages<'a>(
        &'a mut self,
        consume_page: &'a mut TypesensePageConsumer<'a>,
    ) -> TypesenseSourceFuture<'a, TypesenseSourceObservation>;
}

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

impl<R> MigrationSourceReader for Box<R>
where
    R: MigrationSourceReader + ?Sized,
{
    fn app_id(&self) -> &str {
        (**self).app_id()
    }

    fn source_name(&self) -> &str {
        (**self).source_name()
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        (**self).wait_for_quiescent_source()
    }

    fn read_settings(&mut self) -> SourceFuture<'_, Value> {
        (**self).read_settings()
    }

    fn read_index_settings<'a>(&'a mut self, index_name: &'a str) -> SourceFuture<'a, Value> {
        (**self).read_index_settings(index_name)
    }

    fn require_unretrievable_access<'a>(&'a mut self, settings: &'a Value) -> SourceFuture<'a, ()> {
        (**self).require_unretrievable_access(settings)
    }

    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        (**self).read_documents(consume_page)
    }

    fn read_rules<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        (**self).read_rules(consume_page)
    }

    fn read_synonyms<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        (**self).read_synonyms(consume_page)
    }
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

pub(super) struct MeilisearchSourceReader<S> {
    source_name: String,
    source: S,
    observation: Option<MeilisearchSourceObservation>,
    settings: Option<Value>,
}

pub(super) struct TypesenseSourceReader<S> {
    source_name: String,
    source: S,
    observation: Option<TypesenseSourceObservation>,
}

impl<S> MeilisearchSourceReader<S>
where
    S: MeilisearchExportSource,
{
    pub(super) fn from_source(source_name: &str, source: S) -> Self {
        Self {
            source_name: source_name.to_string(),
            source,
            observation: None,
            settings: None,
        }
    }
}

impl MeilisearchSourceReader<MeilisearchClient> {
    pub(super) fn new(
        endpoint: &str,
        api_key: &str,
        source_name: &str,
    ) -> Result<Self, AlgoliaClientError> {
        let source = MeilisearchClient::new(endpoint, api_key, source_name)
            .map_err(map_meilisearch_error)?;
        Ok(Self::from_source(source_name, source))
    }
}

impl<S> TypesenseSourceReader<S>
where
    S: TypesenseExportSource,
{
    pub(super) fn from_source(source_name: &str, source: S) -> Self {
        Self {
            source_name: source_name.to_string(),
            source,
            observation: None,
        }
    }
}

impl TypesenseSourceReader<TypesenseClient> {
    pub(super) fn new(
        endpoint: &str,
        api_key: &str,
        source_name: &str,
    ) -> Result<Self, AlgoliaClientError> {
        let source =
            TypesenseClient::new(endpoint, api_key, source_name).map_err(map_typesense_error)?;
        Ok(Self::from_source(source_name, source))
    }
}

impl MeilisearchExportSource for MeilisearchClient {
    fn observe_source(&mut self) -> MeilisearchSourceFuture<'_, MeilisearchSourceObservation> {
        Box::pin(async move { self.observe_source().await })
    }

    fn read_settings(&mut self) -> MeilisearchSourceFuture<'_, Value> {
        Box::pin(async move { self.read_source_settings().await })
    }

    fn require_read_access(&mut self) -> MeilisearchSourceFuture<'_, ()> {
        Box::pin(async move { self.require_read_access().await })
    }

    fn read_document_pages<'a>(
        &'a mut self,
        consume_page: &'a mut MeilisearchPageConsumer<'a>,
    ) -> MeilisearchSourceFuture<'a, MeilisearchSourceObservation> {
        Box::pin(async move {
            let capture = self.capture_source(consume_page).await?;
            Ok(capture.observation())
        })
    }
}

impl TypesenseExportSource for TypesenseClient {
    fn observe_source(&mut self) -> TypesenseSourceFuture<'_, TypesenseSourceObservation> {
        Box::pin(async move { self.observe_source().await })
    }

    fn read_settings(&mut self) -> TypesenseSourceFuture<'_, Value> {
        Box::pin(async move { self.read_source_settings().await })
    }

    fn require_read_access(&mut self) -> TypesenseSourceFuture<'_, ()> {
        Box::pin(async move { self.require_read_access().await })
    }

    fn read_document_pages<'a>(
        &'a mut self,
        consume_page: &'a mut TypesensePageConsumer<'a>,
    ) -> TypesenseSourceFuture<'a, TypesenseSourceObservation> {
        Box::pin(async move {
            let capture = self.capture_source(consume_page).await?;
            Ok(capture.observation())
        })
    }
}

impl<S> fmt::Debug for MeilisearchSourceReader<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MeilisearchSourceReader")
            .field("source_name", &"<scrubbed>")
            .finish_non_exhaustive()
    }
}

impl<S> fmt::Debug for TypesenseSourceReader<S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TypesenseSourceReader")
            .field("source_name", &"<scrubbed>")
            .finish_non_exhaustive()
    }
}

impl<S> MigrationSourceReader for MeilisearchSourceReader<S>
where
    S: MeilisearchExportSource + Send,
{
    fn app_id(&self) -> &str {
        "meilisearch"
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        Box::pin(async move {
            let observation = self
                .source
                .observe_source()
                .await
                .map_err(map_meilisearch_error)?;
            validate_meilisearch_observation(&self.source_name, &observation)?;
            let record = meilisearch_index_record(&observation);
            self.observation = Some(observation);
            Ok(record)
        })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, Value> {
        Box::pin(async move {
            let raw_settings = self
                .source
                .read_settings()
                .await
                .map_err(map_meilisearch_error)?;
            let normalized_settings = normalize_meilisearch_settings(&raw_settings)?;
            self.settings = Some(raw_settings);
            Ok(normalized_settings)
        })
    }

    fn read_index_settings<'a>(&'a mut self, _index_name: &'a str) -> SourceFuture<'a, Value> {
        Box::pin(async {
            Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Validation,
                "Meilisearch replica settings are not part of the source contract",
            ))
        })
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        _settings: &'a Value,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            self.source
                .require_read_access()
                .await
                .map_err(map_meilisearch_error)
        })
    }

    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let expected = self
                .observation
                .clone()
                .ok_or_else(meilisearch_progress_error)?;
            let primary_key = expected.primary_key.clone();
            let mut consumer_error = None;
            let observed = self
                .source
                .read_document_pages(&mut |page| {
                    let normalized = normalize_meilisearch_document_page(&page, &primary_key)
                        .map_err(|error| {
                            consumer_error = Some(error);
                            meilisearch_consumer_error()
                        })?;
                    consume_page(normalized).map_err(|error| {
                        consumer_error = Some(error);
                        meilisearch_consumer_error()
                    })
                })
                .await;
            if let Some(error) = consumer_error {
                return Err(error);
            }
            let observed = observed.map_err(map_meilisearch_error)?;
            if observed != expected {
                return Err(source_drift_error());
            }
            Ok(())
        })
    }

    fn read_rules<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn read_synonyms<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let settings = self
                .settings
                .as_ref()
                .ok_or_else(meilisearch_progress_error)?;
            let synonyms = normalize_meilisearch_synonyms(settings)?;
            if !synonyms.is_empty() {
                consume_page(synonyms)?;
            }
            Ok(())
        })
    }
}

impl<S> MigrationSourceReader for TypesenseSourceReader<S>
where
    S: TypesenseExportSource + Send,
{
    fn app_id(&self) -> &str {
        "typesense"
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn wait_for_quiescent_source(&mut self) -> SourceFuture<'_, AlgoliaIndexRecord> {
        Box::pin(async move {
            let observation = self
                .source
                .observe_source()
                .await
                .map_err(map_typesense_error)?;
            validate_typesense_observation(&self.source_name, &observation)?;
            let record = typesense_index_record(&observation);
            self.observation = Some(observation);
            Ok(record)
        })
    }

    fn read_settings(&mut self) -> SourceFuture<'_, Value> {
        Box::pin(async move {
            let settings = self
                .source
                .read_settings()
                .await
                .map_err(map_typesense_error)?;
            Ok(settings)
        })
    }

    fn read_index_settings<'a>(&'a mut self, _index_name: &'a str) -> SourceFuture<'a, Value> {
        Box::pin(async {
            Err(AlgoliaClientError::new(
                AlgoliaErrorKind::Validation,
                "Typesense replica settings are not part of the source contract",
            ))
        })
    }

    fn require_unretrievable_access<'a>(
        &'a mut self,
        _settings: &'a Value,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            self.source
                .require_read_access()
                .await
                .map_err(map_typesense_error)
        })
    }

    fn read_documents<'a>(
        &'a mut self,
        consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let expected = self
                .observation
                .clone()
                .ok_or_else(typesense_progress_error)?;
            let mut consumer_error = None;
            let observed = self
                .source
                .read_document_pages(&mut |page| {
                    let normalized = normalize_typesense_document_page(&page, "$.documents")
                        .map_err(|_| {
                            consumer_error = Some(typesense_document_identity_error());
                            typesense_consumer_error()
                        })?;
                    consume_page(normalized).map_err(|error| {
                        consumer_error = Some(error);
                        typesense_consumer_error()
                    })
                })
                .await;
            if let Some(error) = consumer_error {
                return Err(error);
            }
            let observed = observed.map_err(map_typesense_error)?;
            if observed != expected {
                return Err(source_drift_error());
            }
            Ok(())
        })
    }

    fn read_rules<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }

    fn read_synonyms<'a>(
        &'a mut self,
        _consume_page: &'a mut PageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async { Ok(()) })
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
    #[cfg(not(test))]
    let identity_config = SourceIdentityConfig::from_env()?;
    #[cfg(test)]
    let (_identity_spool_root, identity_config) = identity_config_for_test()?;
    let mut builder = SourceSnapshotBuilder::new(identity_config)?;
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

    builder.finish().map_err(AlgoliaClientError::from)
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
        "version": source_identity_version_name(resource.version),
    })
}

fn source_identity_version_name(version: SourceIdentityVersion) -> &'static str {
    match version {
        SourceIdentityVersion::V1 => "v1",
        SourceIdentityVersion::V2 => "v2",
    }
}

pub(super) fn source_drift_error() -> AlgoliaClientError {
    AlgoliaClientError::new(AlgoliaErrorKind::Progress, "Source changed during export")
}

fn flatten_browse_error(error: BrowseError<AlgoliaClientError>) -> AlgoliaClientError {
    match error {
        BrowseError::Client(error) | BrowseError::Consumer(error) => error,
    }
}

fn validate_meilisearch_observation(
    source_name: &str,
    observation: &MeilisearchSourceObservation,
) -> Result<(), AlgoliaClientError> {
    if observation.source_name != source_name || observation.primary_key.is_empty() {
        return Err(meilisearch_schema_error());
    }
    Ok(())
}

fn meilisearch_index_record(observation: &MeilisearchSourceObservation) -> AlgoliaIndexRecord {
    AlgoliaIndexRecord {
        name: observation.source_name.clone(),
        entries: observation.document_count,
        updated_at: observation.updated_at.clone(),
        pending_task: false,
    }
}

fn validate_typesense_observation(
    source_name: &str,
    observation: &TypesenseSourceObservation,
) -> Result<(), AlgoliaClientError> {
    if observation.source_name != source_name || observation.schema_hash.is_empty() {
        return Err(typesense_schema_error());
    }
    Ok(())
}

fn typesense_index_record(observation: &TypesenseSourceObservation) -> AlgoliaIndexRecord {
    AlgoliaIndexRecord {
        name: observation.source_name.clone(),
        entries: observation.document_count,
        updated_at: format!("{}:{}", observation.updated_at, observation.schema_hash),
        pending_task: false,
    }
}

fn normalize_meilisearch_document_page(
    page: &[Value],
    primary_key: &str,
) -> Result<Vec<Value>, AlgoliaClientError> {
    page.iter()
        .map(|document| {
            let object = document
                .as_object()
                .ok_or_else(meilisearch_document_identity_error)?;
            let stable_id = object
                .get(primary_key)
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .ok_or_else(meilisearch_document_identity_error)?;
            let mut normalized = object.clone();
            normalized.insert("objectID".to_string(), Value::String(stable_id.to_string()));
            Ok(Value::Object(normalized))
        })
        .collect()
}

pub(super) fn normalize_typesense_document_page(
    page: &[Value],
    json_path_prefix: &str,
) -> Result<Vec<Value>, String> {
    let mut seen = BTreeSet::new();
    page.iter()
        .enumerate()
        .map(|(document_index, document)| {
            let object = document
                .as_object()
                .ok_or_else(|| "Typesense document must be an object".to_string())?;
            let json_path = format!("{json_path_prefix}[{document_index}].id");
            let stable_id = match object.get("id") {
                Some(Value::String(id)) => id,
                Some(_) => return Err(format!("{json_path}: Typesense id must be a string")),
                None => return Err(format!("{json_path}: missing Typesense id")),
            };
            if !seen.insert(stable_id.clone()) {
                return Err(format!("{json_path}: duplicate Typesense id {stable_id}"));
            }
            let mut normalized = object.clone();
            normalized.insert("objectID".to_string(), Value::String(stable_id.to_string()));
            Ok(Value::Object(normalized))
        })
        .collect()
}

fn normalize_meilisearch_synonyms(settings: &Value) -> Result<Vec<Value>, AlgoliaClientError> {
    let Some(raw_synonyms) = settings.get("synonyms") else {
        return Ok(Vec::new());
    };
    let synonyms = raw_synonyms
        .as_object()
        .ok_or_else(meilisearch_schema_error)?;
    synonyms
        .iter()
        .map(|(input, raw_equivalents)| {
            let equivalents = raw_equivalents
                .as_array()
                .ok_or_else(meilisearch_schema_error)?;
            let mut terms = Vec::with_capacity(equivalents.len() + 1);
            terms.push(Value::String(input.clone()));
            for equivalent in equivalents {
                let equivalent = equivalent
                    .as_str()
                    .filter(|value| !value.is_empty())
                    .ok_or_else(meilisearch_schema_error)?;
                terms.push(Value::String(equivalent.to_string()));
            }
            Ok(json!({
                "objectID": format!("meilisearch:{input}"),
                "type": "synonym",
                "synonyms": terms,
            }))
        })
        .collect()
}

fn normalize_meilisearch_settings(settings: &Value) -> Result<Value, AlgoliaClientError> {
    let mut failures = Vec::new();
    let mut warnings = Vec::new();
    let normalized = translate_settings_for_provider(
        settings,
        SettingsSourceProvider::Meilisearch,
        &mut failures,
        &mut warnings,
    )
    .ok_or_else(meilisearch_schema_error)?;
    if !failures.is_empty() {
        return Err(meilisearch_schema_error());
    }
    serde_json::to_value(normalized).map_err(|_| meilisearch_schema_error())
}

fn map_meilisearch_error(error: MeilisearchClientError) -> AlgoliaClientError {
    let kind = match error.kind() {
        MeilisearchErrorKind::Validation => AlgoliaErrorKind::Validation,
        MeilisearchErrorKind::Transport => AlgoliaErrorKind::Transport,
        MeilisearchErrorKind::Timeout => AlgoliaErrorKind::Timeout,
        MeilisearchErrorKind::Redirect => AlgoliaErrorKind::Redirect,
        MeilisearchErrorKind::Upstream => AlgoliaErrorKind::Upstream,
        MeilisearchErrorKind::Decode => AlgoliaErrorKind::Decode,
        MeilisearchErrorKind::Schema => AlgoliaErrorKind::Schema,
        MeilisearchErrorKind::Progress => AlgoliaErrorKind::Progress,
        MeilisearchErrorKind::Limit => AlgoliaErrorKind::Limit,
    };
    AlgoliaClientError::new(kind, error.safe_message())
}

pub(super) fn map_typesense_error(error: TypesenseClientError) -> AlgoliaClientError {
    let kind = match error.kind() {
        TypesenseErrorKind::Validation => AlgoliaErrorKind::Validation,
        TypesenseErrorKind::Transport => AlgoliaErrorKind::Transport,
        TypesenseErrorKind::Timeout => AlgoliaErrorKind::Timeout,
        TypesenseErrorKind::Redirect => AlgoliaErrorKind::Redirect,
        TypesenseErrorKind::Upstream => AlgoliaErrorKind::Upstream,
        TypesenseErrorKind::Schema => AlgoliaErrorKind::Schema,
        TypesenseErrorKind::Progress => AlgoliaErrorKind::Progress,
        TypesenseErrorKind::Limit => AlgoliaErrorKind::Limit,
    };
    AlgoliaClientError::new(kind, error.safe_message())
}

fn meilisearch_document_identity_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Schema,
        "Meilisearch document primary key is invalid",
    )
}

fn meilisearch_schema_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Schema,
        "Meilisearch source response schema is invalid",
    )
}

fn typesense_document_identity_error() -> AlgoliaClientError {
    AlgoliaClientError::new(AlgoliaErrorKind::Schema, "Typesense document id is invalid")
}

fn typesense_schema_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Schema,
        "Typesense source response schema is invalid",
    )
}

fn typesense_progress_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Typesense source reader state is invalid",
    )
}

fn meilisearch_progress_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Meilisearch source reader state is invalid",
    )
}

fn meilisearch_consumer_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Progress,
        "Meilisearch source consumer rejected a page",
    )
}

fn typesense_consumer_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Progress,
        "Typesense source consumer rejected a page",
    )
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
