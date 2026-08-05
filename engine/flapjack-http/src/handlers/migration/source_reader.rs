//! Provider-neutral source-migration contract.
//!
//! Vendor clients, errors, observations, and raw schemas stay inside adapters.
#![allow(dead_code)]

use super::algolia_client::{AlgoliaClientError, AlgoliaErrorKind};
#[cfg(test)]
pub(super) use super::algolia_source_reader::collect_replica_settings;
pub(super) use super::algolia_source_reader::AlgoliaSourceReader;
pub(super) use super::meilisearch_source_reader::MeilisearchSourceReader;
#[cfg(test)]
pub(super) use super::meilisearch_source_reader::{
    MeilisearchExportSource, MeilisearchPageConsumer, MeilisearchSourceFuture,
};
#[cfg(not(test))]
use super::source_identity_partitions::SourceIdentityConfig;
use super::source_identity_partitions::{SourceIdentityError, SourceIdentityVersion};
use super::source_snapshot::{canonical_json_bytes, SourceSnapshot, SourceSnapshotBuilder};
#[cfg(test)]
use super::source_test_support::identity_config_for_test;
use super::translation::ReportCode;
pub(super) use super::typesense_source_reader::TypesenseSourceReader;
#[cfg(test)]
pub(super) use super::typesense_source_reader::{
    TypesenseExportSource, TypesensePageConsumer, TypesenseSourceFuture,
};
use super::AsyncMigrationSourceProvider;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub(super) type SourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, SourceExportError>> + Send + 'a>>;

pub(super) type SourceDocumentPageConsumer<'a> =
    dyn FnMut(Vec<SourceExportRecord>) -> Result<(), SourceExportError> + Send + 'a;

pub(super) type SourceConfigurationConsumer<'a> =
    dyn FnMut(SourceConfigurationArtifact) -> Result<(), SourceExportError> + Send + 'a;

/// Replica-owned settings reads. Only Algolia's source model has replica indexes.
pub(super) trait AlgoliaReplicaSource {
    fn read_index_settings<'a>(&'a mut self, index_name: &'a str) -> SourceFuture<'a, Value>;
}

/// Neutral failure at the source-migration seam.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SourceExportError {
    kind: SourceExportErrorKind,
    message: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceExportErrorKind {
    Validation,
    Transport,
    Timeout,
    Redirect,
    RateLimit,
    Server,
    Upstream,
    Decode,
    Schema,
    Progress,
    Limit,
}

impl SourceExportError {
    pub(super) fn new(kind: SourceExportErrorKind, message: &'static str) -> Self {
        Self { kind, message }
    }

    pub(super) fn kind(&self) -> SourceExportErrorKind {
        self.kind
    }

    pub(super) fn safe_message(&self) -> &str {
        self.message
    }

    pub(super) fn into_inner(self) -> AlgoliaClientError {
        AlgoliaClientError::new(self.kind.into_algolia_kind(), self.message)
    }
}

impl SourceExportErrorKind {
    fn from_algolia_kind(kind: AlgoliaErrorKind) -> Self {
        match kind {
            AlgoliaErrorKind::Validation => Self::Validation,
            AlgoliaErrorKind::Transport => Self::Transport,
            AlgoliaErrorKind::Timeout => Self::Timeout,
            AlgoliaErrorKind::Redirect => Self::Redirect,
            AlgoliaErrorKind::RateLimit => Self::RateLimit,
            AlgoliaErrorKind::Server => Self::Server,
            AlgoliaErrorKind::Upstream => Self::Upstream,
            AlgoliaErrorKind::Decode => Self::Decode,
            AlgoliaErrorKind::Schema => Self::Schema,
            AlgoliaErrorKind::Progress => Self::Progress,
            AlgoliaErrorKind::Limit => Self::Limit,
        }
    }

    fn into_algolia_kind(self) -> AlgoliaErrorKind {
        match self {
            Self::Validation => AlgoliaErrorKind::Validation,
            Self::Transport => AlgoliaErrorKind::Transport,
            Self::Timeout => AlgoliaErrorKind::Timeout,
            Self::Redirect => AlgoliaErrorKind::Redirect,
            Self::RateLimit => AlgoliaErrorKind::RateLimit,
            Self::Server => AlgoliaErrorKind::Server,
            Self::Upstream => AlgoliaErrorKind::Upstream,
            Self::Decode => AlgoliaErrorKind::Decode,
            Self::Schema => AlgoliaErrorKind::Schema,
            Self::Progress => AlgoliaErrorKind::Progress,
            Self::Limit => AlgoliaErrorKind::Limit,
        }
    }
}

impl From<AlgoliaClientError> for SourceExportError {
    fn from(error: AlgoliaClientError) -> Self {
        Self {
            kind: SourceExportErrorKind::from_algolia_kind(error.kind()),
            message: error.safe_message(),
        }
    }
}

impl From<SourceIdentityError> for SourceExportError {
    fn from(error: SourceIdentityError) -> Self {
        AlgoliaClientError::from(error).into()
    }
}

/// A source document paired with the stable ID the export is keyed by.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct StableSourceDocument {
    stable_id: String,
    payload: Value,
}

impl StableSourceDocument {
    fn new(stable_id: String, payload: Value) -> Result<Self, SourceExportError> {
        if stable_id.is_empty() {
            return Err(missing_stable_id());
        }
        Ok(Self { stable_id, payload })
    }

    /// Documents cross the seam as Algolia-shaped records, so their identity
    /// view pins `objectID` to the validated stable ID. Non-document
    /// configuration keeps its provider-native payload and carries the stable
    /// ID out of band; see [`SourceConfigurationRecord::identity_payload`].
    fn identity_payload(&self) -> Value {
        let mut payload = self.payload.clone();
        if let Value::Object(object) = &mut payload {
            object.insert(
                "objectID".to_string(),
                Value::String(self.stable_id.clone()),
            );
        }
        payload
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SourceExportRecord {
    document: StableSourceDocument,
}

impl SourceExportRecord {
    pub(super) fn from_document(
        stable_id: String,
        payload: Value,
    ) -> Result<Self, SourceExportError> {
        Ok(Self {
            document: StableSourceDocument::new(stable_id, payload)?,
        })
    }

    pub(super) fn stable_id(&self) -> &str {
        &self.document.stable_id
    }

    pub(super) fn payload(&self) -> &Value {
        &self.document.payload
    }

    /// The payload as the downstream translation and identity owners see it:
    /// the source fields with `objectID` pinned to the validated stable ID.
    pub(super) fn identity_payload(&self) -> Value {
        self.document.identity_payload()
    }

    pub(super) fn to_capture_value(&self) -> Value {
        json!({
            "stableId": self.stable_id(),
            "payload": self.payload(),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(super) struct SourceConfigurationRecord {
    stable_id: String,
    payload: Value,
}

impl SourceConfigurationRecord {
    pub(super) fn new(stable_id: String, payload: Value) -> Result<Self, SourceExportError> {
        if stable_id.is_empty() {
            return Err(missing_stable_id());
        }
        Ok(Self { stable_id, payload })
    }

    fn from_object_id(payload: &Value) -> Result<Self, SourceExportError> {
        let stable_id =
            object_id_from_payload(payload, "objectID").ok_or_else(missing_stable_id)?;
        Self::new(stable_id.to_string(), payload.clone())
    }

    pub(super) fn stable_id(&self) -> &str {
        &self.stable_id
    }

    pub(super) fn payload(&self) -> &Value {
        &self.payload
    }

    /// Configuration keeps its untouched provider-native payload; the stable ID
    /// travels alongside it (via `record_*_page_with_stable_ids`) rather than
    /// being folded into the payload as `objectID`. This keeps provider-native
    /// shapes — e.g. a Meilisearch synonym `{"saw": ["cutter"]}` — intact for
    /// their downstream translators, which reject any extra keys.
    pub(super) fn identity_payload(&self) -> Value {
        self.payload.clone()
    }

    pub(super) fn to_capture_value(&self) -> Value {
        json!({
            "stableId": self.stable_id(),
            "payload": self.payload(),
        })
    }
}

/// The closed set of non-document source configuration an adapter may emit.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SourceConfigurationArtifact {
    Settings {
        payload: Value,
    },
    Rules {
        records: Vec<SourceConfigurationRecord>,
    },
    Synonyms {
        records: Vec<SourceConfigurationRecord>,
    },
    ReplicaSettings {
        source_name: String,
        payload: Value,
    },
}

impl SourceConfigurationArtifact {
    pub(super) fn settings(payload: &Value) -> Self {
        Self::Settings {
            payload: payload.clone(),
        }
    }

    pub(super) fn rules(records: &[Value]) -> Result<Self, SourceExportError> {
        let records = records
            .iter()
            .map(SourceConfigurationRecord::from_object_id)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::Rules { records })
    }

    pub(super) fn synonyms(records: &[Value]) -> Result<Self, SourceExportError> {
        let records = records
            .iter()
            .map(SourceConfigurationRecord::from_object_id)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::Synonyms { records })
    }

    pub(super) fn synonym_records(records: Vec<SourceConfigurationRecord>) -> Self {
        Self::Synonyms { records }
    }

    pub(super) fn replica_settings(source_name: &str, payload: &Value) -> Self {
        // Replica settings come from the same vendor index-settings read as the
        // primary settings, so they are the same raw source artifact and are kept
        // verbatim. Any secret material lives in the connection layer, which
        // redacts at its own diagnostic boundary; source-owned fields (even ones
        // named `apiKey`/`url`) are the user's data and must reach translation.
        Self::ReplicaSettings {
            source_name: source_name.to_string(),
            payload: payload.clone(),
        }
    }
}

/// Provider-neutral view of a quiescent source at one point in time.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SourceObservation {
    pub(super) source_name: String,
    pub(super) accepted_revision: String,
    pub(super) identity_revision: String,
    pub(super) document_count: u64,
    pub(super) quiescent: bool,
}

impl fmt::Debug for SourceObservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceObservation")
            .field("source_name", &"<scrubbed>")
            .field("accepted_revision", &self.accepted_revision)
            .field("document_count", &self.document_count)
            .field("quiescent", &self.quiescent)
            .finish_non_exhaustive()
    }
}

/// The shared source-migration contract every provider adapter implements.
pub(super) trait MigrationSourceReader {
    fn source_provider(&self) -> AsyncMigrationSourceProvider;

    /// A non-secret grouping name for the source, where the provider has one.
    fn source_namespace(&self) -> Option<&str> {
        None
    }

    fn source_name(&self) -> &str;

    fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation>;

    /// Emit the source's own configuration as tagged artifacts.
    fn read_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()>;

    /// Emit configuration owned by sources derived from this one.
    fn read_derived_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        let _ = consume;
        Box::pin(async { Ok(()) })
    }

    fn read_document_records<'a>(
        &'a mut self,
        consume_page: &'a mut SourceDocumentPageConsumer<'a>,
    ) -> SourceFuture<'a, ()>;
}

impl<R> MigrationSourceReader for Box<R>
where
    R: MigrationSourceReader + Send + ?Sized,
{
    fn source_provider(&self) -> AsyncMigrationSourceProvider {
        (**self).source_provider()
    }

    fn source_namespace(&self) -> Option<&str> {
        (**self).source_namespace()
    }

    fn source_name(&self) -> &str {
        (**self).source_name()
    }

    fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation> {
        (**self).observe_quiescent_source()
    }

    fn read_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        (**self).read_configuration(consume)
    }

    fn read_derived_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        (**self).read_derived_configuration(consume)
    }

    fn read_document_records<'a>(
        &'a mut self,
        consume_page: &'a mut SourceDocumentPageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        (**self).read_document_records(consume_page)
    }
}

/// The capture side of the seam: typed documents and tagged configuration.
pub(super) trait SourceExportSink {
    fn commit_configuration(
        &mut self,
        artifact: &SourceConfigurationArtifact,
    ) -> Result<(), SourceExportError>;

    fn commit_document_page(
        &mut self,
        page: &[SourceExportRecord],
    ) -> Result<(), SourceExportError>;
}

/// The single owner of who the accepted source was and what state it was in.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct SourceIdentity {
    provider: AsyncMigrationSourceProvider,
    namespace: Option<String>,
    source_name: String,
    digest: String,
    accepted_revision: String,
    document_metadata_count: u64,
    snapshot: SourceSnapshot,
}

impl SourceIdentity {
    pub(super) fn digest(&self) -> &str {
        &self.digest
    }

    pub(super) fn provider(&self) -> AsyncMigrationSourceProvider {
        self.provider
    }

    pub(super) fn namespace(&self) -> Option<&str> {
        self.namespace.as_deref()
    }

    pub(super) fn source_name(&self) -> &str {
        &self.source_name
    }

    pub(super) fn accepted_revision(&self) -> &str {
        &self.accepted_revision
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
            .field("provider", &self.provider)
            .field("namespace", &self.namespace.as_ref().map(|_| "<scrubbed>"))
            .field("source_name", &"<scrubbed>")
            .field("digest", &self.digest)
            .field("accepted_revision", &self.accepted_revision)
            .field("document_metadata_count", &self.document_metadata_count)
            .finish_non_exhaustive()
    }
}

/// Acceptance receipt with identity evidence and provider-attributed warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AcceptedSourceExport {
    identity: SourceIdentity,
    warnings: Vec<ReportCode>,
}

impl AcceptedSourceExport {
    pub(super) fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    pub(super) fn provider(&self) -> AsyncMigrationSourceProvider {
        self.identity.provider()
    }

    pub(super) fn source_namespace(&self) -> Option<&str> {
        self.identity.namespace()
    }

    pub(super) fn source_name(&self) -> &str {
        self.identity.source_name()
    }

    pub(super) fn warning_codes(&self) -> &[ReportCode] {
        &self.warnings
    }
}

// --- Shared capture ----------------------------------------------------------

pub(super) async fn collect_quiescent_source_snapshot<R>(
    reader: &mut R,
) -> Result<SourceIdentity, SourceExportError>
where
    R: MigrationSourceReader + Send,
{
    let observation = reader.observe_quiescent_source().await?;
    let snapshot = read_source_snapshot(reader, &mut NoopSink).await?;
    source_identity_from_reader(reader, &observation, snapshot)
}

/// Build the canonical identity for a reader's observed source state.
pub(super) fn source_identity_from_reader<R>(
    reader: &R,
    observation: &SourceObservation,
    snapshot: SourceSnapshot,
) -> Result<SourceIdentity, SourceExportError>
where
    R: MigrationSourceReader + ?Sized,
{
    let source_name = reader.source_name();
    if observation.source_name != source_name || !observation.quiescent {
        return Err(source_drift_error());
    }
    if observation.document_count != snapshot.documents.count as u64 {
        return Err(SourceExportError::new(
            SourceExportErrorKind::Progress,
            "Source metadata did not match exported documents",
        ));
    }
    let provider = reader.source_provider();
    let namespace = reader.source_namespace();
    Ok(SourceIdentity {
        digest: source_identity_digest(provider, namespace, source_name, observation, &snapshot),
        provider,
        namespace: namespace.map(str::to_string),
        source_name: source_name.to_string(),
        accepted_revision: observation.accepted_revision.clone(),
        document_metadata_count: observation.document_count,
        snapshot,
    })
}

/// Admit a source export with provider identity and two-pass stability proof.
pub(super) async fn accept_source_export<R, S>(
    expected_provider: AsyncMigrationSourceProvider,
    reader: &mut R,
    sink: &mut S,
) -> Result<AcceptedSourceExport, SourceExportError>
where
    R: MigrationSourceReader + Send,
    S: SourceExportSink + Send,
{
    admit_source_provider(expected_provider, reader.source_provider())?;
    let pre_identity = collect_quiescent_source_snapshot(reader).await?;
    let exported_snapshot = read_source_snapshot(reader, sink).await?;
    let final_observation = reader.observe_quiescent_source().await?;
    let exported_identity =
        source_identity_from_reader(reader, &final_observation, exported_snapshot)?;

    // Algolia browse cursors expire and browse order is not stable. Persisted
    // resume state must use exact membership and hashes, never cursors or
    // scalar ordering watermarks.
    if pre_identity != exported_identity {
        return Err(source_drift_error());
    }

    Ok(AcceptedSourceExport {
        identity: pre_identity,
        warnings: source_export_warnings(reader.source_provider()),
    })
}

/// The single provider-admission comparison.
pub(super) fn admit_source_provider(
    expected_provider: AsyncMigrationSourceProvider,
    actual_provider: AsyncMigrationSourceProvider,
) -> Result<(), SourceExportError> {
    if expected_provider == actual_provider {
        return Ok(());
    }
    Err(SourceExportError::new(
        SourceExportErrorKind::Validation,
        "Source export provider identity mismatch",
    ))
}

/// Stream one full capture pass while accumulating the canonical source snapshot.
pub(super) async fn read_source_snapshot<R, S>(
    reader: &mut R,
    sink: &mut S,
) -> Result<SourceSnapshot, SourceExportError>
where
    R: MigrationSourceReader + Send,
    S: SourceExportSink + Send,
{
    #[cfg(not(test))]
    let identity_config = SourceIdentityConfig::from_env()?;
    #[cfg(test)]
    let (_identity_spool_root, identity_config) = identity_config_for_test()?;
    let mut builder = SourceSnapshotBuilder::new(identity_config)?;

    {
        let mut consume = |artifact: SourceConfigurationArtifact| {
            record_configuration_identity(&mut builder, &artifact)?;
            sink.commit_configuration(&artifact)
        };
        reader.read_configuration(&mut consume).await?;
    }
    {
        let mut consume_page = |page: Vec<SourceExportRecord>| {
            let identity_page = source_record_identity_page(&page);
            builder.record_documents(&identity_page)?;
            sink.commit_document_page(&page)
        };
        reader.read_document_records(&mut consume_page).await?;
    }
    {
        let mut consume = |artifact: SourceConfigurationArtifact| {
            record_configuration_identity(&mut builder, &artifact)?;
            sink.commit_configuration(&artifact)
        };
        reader.read_derived_configuration(&mut consume).await?;
    }

    builder.finish().map_err(Into::into)
}

/// Fold a tagged configuration artifact into the canonical source identity.
fn record_configuration_identity(
    builder: &mut SourceSnapshotBuilder,
    artifact: &SourceConfigurationArtifact,
) -> Result<(), SourceExportError> {
    match artifact {
        SourceConfigurationArtifact::Settings { payload } => {
            builder.record_settings(payload);
            Ok(())
        }
        // Configuration records carry their stable ID out of band so the
        // snapshot keys them without folding `objectID` into the provider-native
        // payload. This is the same seam the staging translator records against
        // (`TranslationSession::consume_*_pages`), keeping capture and staging
        // identities byte-for-byte consistent.
        SourceConfigurationArtifact::Rules { records } => builder
            .record_rules_page_with_stable_ids(
                0,
                &source_configuration_identity_page(records),
                &source_configuration_stable_ids(records),
            )
            .map_err(AlgoliaClientError::from)
            .map_err(Into::into),
        SourceConfigurationArtifact::Synonyms { records } => builder
            .record_synonyms_page_with_stable_ids(
                0,
                &source_configuration_identity_page(records),
                &source_configuration_stable_ids(records),
            )
            .map_err(AlgoliaClientError::from)
            .map_err(Into::into),
        SourceConfigurationArtifact::ReplicaSettings {
            source_name,
            payload,
        } => builder
            .record_replica_settings(source_name, payload)
            .map_err(AlgoliaClientError::from)
            .map_err(Into::into),
    }
}

fn source_identity_digest(
    provider: AsyncMigrationSourceProvider,
    namespace: Option<&str>,
    source_name: &str,
    observation: &SourceObservation,
    snapshot: &SourceSnapshot,
) -> String {
    let identity = json!({
        "provider": provider.as_str(),
        "namespace": namespace,
        "sourceName": source_name,
        "updatedAt": observation.identity_revision,
        "documentMetadataCount": observation.document_count,
        "resources": {
            "settings": resource_identity(&snapshot.settings),
            "documents": resource_identity(&snapshot.documents),
            "rules": resource_identity(&snapshot.rules),
            "synonyms": resource_identity(&snapshot.synonyms),
            "replicaSettings": resource_identity(&snapshot.replica_settings),
        }
    });
    hex::encode(Sha256::digest(canonical_json_bytes(&identity)))
}

fn source_export_warnings(provider: AsyncMigrationSourceProvider) -> Vec<ReportCode> {
    match provider {
        AsyncMigrationSourceProvider::Algolia => Vec::new(),
        AsyncMigrationSourceProvider::Meilisearch => vec![
            ReportCode::MeilisearchDocumentOrderNotContractual,
            ReportCode::MeilisearchSearchPaginationNotExportBound,
        ],
        AsyncMigrationSourceProvider::Typesense => vec![ReportCode::TypesenseSettingNotMigrated],
    }
}

fn source_record_identity_page(page: &[SourceExportRecord]) -> Vec<Value> {
    page.iter()
        .map(SourceExportRecord::identity_payload)
        .collect()
}

fn source_configuration_identity_page(page: &[SourceConfigurationRecord]) -> Vec<Value> {
    page.iter()
        .map(SourceConfigurationRecord::identity_payload)
        .collect()
}

pub(super) fn source_configuration_stable_ids(page: &[SourceConfigurationRecord]) -> Vec<String> {
    page.iter()
        .map(|record| record.stable_id().to_string())
        .collect()
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

pub(super) fn source_drift_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        "Source changed during export",
    )
}

// --- Adapter-to-neutral normalization ---------------------------------------

/// Hand one vendor page to the neutral consumer and stash neutral failures.
pub(super) fn capture_neutral_page(
    capture_error: &mut Option<SourceExportError>,
    records: Result<Vec<SourceExportRecord>, SourceExportError>,
    consume_page: &mut SourceDocumentPageConsumer<'_>,
) -> Result<(), PageCaptureAborted> {
    match records.and_then(consume_page) {
        Ok(()) => Ok(()),
        Err(error) => {
            *capture_error = Some(error);
            Err(PageCaptureAborted)
        }
    }
}

/// Marker returned when [`capture_neutral_page`] stashed the real cause.
pub(super) struct PageCaptureAborted;

/// Prefer the stashed neutral cause over the vendor traversal placeholder.
pub(super) fn finish_neutral_page_capture(
    capture_error: Option<SourceExportError>,
    outcome: Result<(), AlgoliaClientError>,
) -> Result<(), SourceExportError> {
    if let Some(error) = capture_error {
        return Err(error);
    }
    outcome.map_err(Into::into)
}

pub(super) fn algolia_document_records(
    page: &[Value],
) -> Result<Vec<SourceExportRecord>, SourceExportError> {
    page.iter()
        .map(|document| {
            let stable_id =
                object_id_from_payload(document, "objectID").ok_or_else(missing_stable_id)?;
            SourceExportRecord::from_document(stable_id.to_string(), document.clone())
        })
        .collect()
}

pub(super) fn object_id_from_payload<'a>(payload: &'a Value, field: &str) -> Option<&'a str> {
    payload
        .as_object()
        .and_then(|object| object.get(field))
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}

// --- Error construction ------------------------------------------------------

pub(super) fn replica_entry_validation_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Validation,
        "Algolia replica entry could not be parsed for migration",
    )
}

pub(super) fn missing_captured_primary_settings() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        "Source export did not capture primary settings before derived configuration",
    )
}

fn missing_stable_id() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Schema,
        "Source document stable ID is invalid",
    )
}

pub(super) fn meilisearch_document_identity_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Schema,
        "Meilisearch document primary key is invalid",
    )
}

pub(super) fn meilisearch_schema_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Schema,
        "Meilisearch source response schema is invalid",
    )
}

pub(super) fn meilisearch_progress_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        "Meilisearch source reader state is invalid",
    )
}

pub(super) fn typesense_document_identity_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Schema,
        "Typesense document id is invalid",
    )
}

pub(super) fn typesense_schema_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Schema,
        "Typesense source response schema is invalid",
    )
}

pub(super) fn typesense_progress_error() -> SourceExportError {
    SourceExportError::new(
        SourceExportErrorKind::Progress,
        "Typesense source reader state is invalid",
    )
}

pub(super) fn algolia_consumer_error() -> AlgoliaClientError {
    AlgoliaClientError::new(
        AlgoliaErrorKind::Progress,
        "Algolia source consumer rejected a page",
    )
}

struct NoopSink;

impl SourceExportSink for NoopSink {
    fn commit_configuration(
        &mut self,
        _artifact: &SourceConfigurationArtifact,
    ) -> Result<(), SourceExportError> {
        Ok(())
    }

    fn commit_document_page(
        &mut self,
        _page: &[SourceExportRecord],
    ) -> Result<(), SourceExportError> {
        Ok(())
    }
}
