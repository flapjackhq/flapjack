use super::algolia_client::AlgoliaClientError;
use super::meilisearch_client::{
    MeilisearchClient, MeilisearchClientError, MeilisearchErrorKind, MeilisearchSourceObservation,
};
use super::meilisearch_synonyms::parse_meilisearch_synonym_payload;
use super::source_reader::{
    capture_neutral_page, meilisearch_document_identity_error, meilisearch_progress_error,
    meilisearch_schema_error, source_drift_error, MigrationSourceReader, PageCaptureAborted,
    SourceConfigurationArtifact, SourceConfigurationConsumer, SourceConfigurationRecord,
    SourceDocumentPageConsumer, SourceExportError, SourceExportErrorKind, SourceExportRecord,
    SourceFuture,
};
use super::AsyncMigrationSourceProvider;
use serde_json::{Map, Value};
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub(super) type MeilisearchSourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, MeilisearchClientError>> + Send + 'a>>;

pub(super) type MeilisearchPageConsumer<'a> =
    dyn FnMut(Vec<Value>) -> Result<(), MeilisearchClientError> + Send + 'a;

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

impl MeilisearchExportSource for MeilisearchClient {
    fn observe_source(&mut self) -> MeilisearchSourceFuture<'_, MeilisearchSourceObservation> {
        Box::pin(async move { MeilisearchClient::observe_source(self).await })
    }

    fn read_settings(&mut self) -> MeilisearchSourceFuture<'_, Value> {
        Box::pin(async move { self.read_source_settings().await })
    }

    fn require_read_access(&mut self) -> MeilisearchSourceFuture<'_, ()> {
        Box::pin(async move { MeilisearchClient::require_read_access(self).await })
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

pub(super) struct MeilisearchSourceReader<S> {
    source_name: String,
    source: S,
    observation: Option<MeilisearchSourceObservation>,
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
            .map_err(|error| map_meilisearch_error(error).into_inner())?;
        Ok(Self::from_source(source_name, source))
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

impl<S> MigrationSourceReader for MeilisearchSourceReader<S>
where
    S: MeilisearchExportSource + Send,
{
    fn source_provider(&self) -> AsyncMigrationSourceProvider {
        AsyncMigrationSourceProvider::Meilisearch
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn observe_quiescent_source(
        &mut self,
    ) -> SourceFuture<'_, super::source_reader::SourceObservation> {
        Box::pin(async move {
            let observation = self
                .source
                .observe_source()
                .await
                .map_err(map_meilisearch_error)?;
            if observation.source_name != self.source_name || observation.primary_key.is_empty() {
                return Err(meilisearch_schema_error());
            }
            let neutral = super::source_reader::SourceObservation {
                source_name: observation.source_name.clone(),
                accepted_revision: observation.updated_at.clone(),
                identity_revision: observation.updated_at.clone(),
                document_count: observation.document_count,
                quiescent: true,
            };
            self.observation = Some(observation);
            Ok(neutral)
        })
    }

    fn read_configuration<'a>(
        &'a mut self,
        consume: &'a mut SourceConfigurationConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let raw_settings = self
                .source
                .read_settings()
                .await
                .map_err(map_meilisearch_error)?;
            self.source
                .require_read_access()
                .await
                .map_err(map_meilisearch_error)?;
            let (settings, synonyms) = split_meilisearch_configuration(&raw_settings)?;
            consume(SourceConfigurationArtifact::settings(&settings))?;
            consume(SourceConfigurationArtifact::rules(&[])?)?;
            consume(SourceConfigurationArtifact::synonym_records(synonyms))
        })
    }

    fn read_document_records<'a>(
        &'a mut self,
        consume_page: &'a mut SourceDocumentPageConsumer<'a>,
    ) -> SourceFuture<'a, ()> {
        Box::pin(async move {
            let expected = self
                .observation
                .clone()
                .ok_or_else(meilisearch_progress_error)?;
            let primary_key = expected.primary_key.clone();
            let mut capture_error = None;
            let observed = self
                .source
                .read_document_pages(&mut |page| {
                    capture_neutral_page(
                        &mut capture_error,
                        meilisearch_document_records(&page, &primary_key),
                        consume_page,
                    )
                    .map_err(|PageCaptureAborted| meilisearch_consumer_error())
                })
                .await;
            if let Some(error) = capture_error {
                return Err(error);
            }
            if observed.map_err(map_meilisearch_error)? != expected {
                return Err(source_drift_error());
            }
            Ok(())
        })
    }
}

fn meilisearch_document_records(
    page: &[Value],
    primary_key: &str,
) -> Result<Vec<SourceExportRecord>, SourceExportError> {
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
            SourceExportRecord::from_document(stable_id.to_string(), document.clone())
        })
        .collect()
}

fn split_meilisearch_configuration(
    settings: &Value,
) -> Result<(Value, Vec<SourceConfigurationRecord>), SourceExportError> {
    let settings_object = settings.as_object().ok_or_else(meilisearch_schema_error)?;
    let mut raw_settings = settings_object.clone();
    let raw_synonyms = raw_settings.remove("synonyms");
    let synonyms = meilisearch_synonym_records(raw_synonyms.as_ref())?;
    Ok((Value::Object(raw_settings), synonyms))
}

fn meilisearch_synonym_records(
    raw_synonyms: Option<&Value>,
) -> Result<Vec<SourceConfigurationRecord>, SourceExportError> {
    let Some(raw_synonyms) = raw_synonyms else {
        return Ok(Vec::new());
    };
    let synonyms = raw_synonyms
        .as_object()
        .ok_or_else(meilisearch_schema_error)?;
    synonyms
        .iter()
        .map(|(input, raw_equivalents)| {
            let payload = Value::Object(Map::from_iter([(input.clone(), raw_equivalents.clone())]));
            parse_meilisearch_synonym_payload(&payload).map_err(|_| meilisearch_schema_error())?;
            SourceConfigurationRecord::new(format!("meilisearch:synonym:{input}"), payload)
        })
        .collect()
}

/// The vendor consumer error raised when the shared capture path rejects a
/// Meilisearch page. It stays inside this adapter because it is expressed in
/// the vendor error type before `map_meilisearch_error` neutralizes it.
fn meilisearch_consumer_error() -> MeilisearchClientError {
    MeilisearchClientError::new(
        MeilisearchErrorKind::Progress,
        "Meilisearch source consumer rejected a page",
    )
}

pub(super) fn map_meilisearch_error(error: MeilisearchClientError) -> SourceExportError {
    let kind = match error.kind() {
        MeilisearchErrorKind::Validation => SourceExportErrorKind::Validation,
        MeilisearchErrorKind::Transport => SourceExportErrorKind::Transport,
        MeilisearchErrorKind::Timeout => SourceExportErrorKind::Timeout,
        MeilisearchErrorKind::Redirect => SourceExportErrorKind::Redirect,
        MeilisearchErrorKind::Upstream => SourceExportErrorKind::Upstream,
        // Export never asks for `indexes.get`; a discovery-only authorization
        // refusal still reaches the export status rules as an upstream failure.
        MeilisearchErrorKind::Forbidden => SourceExportErrorKind::Upstream,
        MeilisearchErrorKind::Decode => SourceExportErrorKind::Decode,
        MeilisearchErrorKind::Schema => SourceExportErrorKind::Schema,
        MeilisearchErrorKind::Progress => SourceExportErrorKind::Progress,
        MeilisearchErrorKind::Limit => SourceExportErrorKind::Limit,
    };
    SourceExportError::new(kind, error.safe_message())
}
