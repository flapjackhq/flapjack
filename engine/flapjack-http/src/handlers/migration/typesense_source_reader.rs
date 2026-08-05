use super::algolia_client::AlgoliaClientError;
use super::source_reader::{
    capture_neutral_page, source_drift_error, typesense_document_identity_error,
    typesense_progress_error, typesense_schema_error, MigrationSourceReader, PageCaptureAborted,
    SourceConfigurationArtifact, SourceConfigurationConsumer, SourceDocumentPageConsumer,
    SourceExportError, SourceExportErrorKind, SourceExportRecord, SourceFuture, SourceObservation,
};
use super::typesense_client::{
    TypesenseClient, TypesenseClientError, TypesenseErrorKind, TypesenseSourceObservation,
};
use super::AsyncMigrationSourceProvider;
use serde_json::Value;
use std::collections::BTreeSet;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub(super) type TypesenseSourceFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, TypesenseClientError>> + Send + 'a>>;

pub(super) type TypesensePageConsumer<'a> =
    dyn FnMut(Vec<Value>) -> Result<(), TypesenseClientError> + Send + 'a;

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

impl TypesenseExportSource for TypesenseClient {
    fn observe_source(&mut self) -> TypesenseSourceFuture<'_, TypesenseSourceObservation> {
        Box::pin(async move { TypesenseClient::observe_source(self).await })
    }

    fn read_settings(&mut self) -> TypesenseSourceFuture<'_, Value> {
        Box::pin(async move { self.read_source_settings().await })
    }

    fn require_read_access(&mut self) -> TypesenseSourceFuture<'_, ()> {
        Box::pin(async move { TypesenseClient::require_read_access(self).await })
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

pub(super) struct TypesenseSourceReader<S> {
    source_name: String,
    source: S,
    observation: Option<TypesenseSourceObservation>,
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
        let source = TypesenseClient::new(endpoint, api_key, source_name)
            .map_err(map_typesense_client_error)?;
        Ok(Self::from_source(source_name, source))
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

impl<S> MigrationSourceReader for TypesenseSourceReader<S>
where
    S: TypesenseExportSource + Send,
{
    fn source_provider(&self) -> AsyncMigrationSourceProvider {
        AsyncMigrationSourceProvider::Typesense
    }

    fn source_name(&self) -> &str {
        &self.source_name
    }

    fn observe_quiescent_source(&mut self) -> SourceFuture<'_, SourceObservation> {
        Box::pin(async move {
            let observation = self
                .source
                .observe_source()
                .await
                .map_err(map_typesense_error)?;
            if observation.source_name != self.source_name || observation.schema_hash.is_empty() {
                return Err(typesense_schema_error());
            }
            // Typesense carries collection schema state alongside its update
            // timestamp. The schema hash belongs in the identity preimage - a
            // schema change is source drift - but not on the public receipt.
            let neutral = SourceObservation {
                source_name: observation.source_name.clone(),
                accepted_revision: observation.updated_at.clone(),
                identity_revision: format!(
                    "{}:{}",
                    observation.updated_at, observation.schema_hash
                ),
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
            let settings = self
                .source
                .read_settings()
                .await
                .map_err(map_typesense_error)?;
            self.source
                .require_read_access()
                .await
                .map_err(map_typesense_error)?;
            consume(SourceConfigurationArtifact::settings(&settings))?;
            consume(SourceConfigurationArtifact::rules(&[])?)?;
            consume(SourceConfigurationArtifact::synonyms(&[])?)
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
                .ok_or_else(typesense_progress_error)?;
            let mut capture_error = None;
            let observed = self
                .source
                .read_document_pages(&mut |page| {
                    capture_neutral_page(
                        &mut capture_error,
                        typesense_document_records(&page, "$.documents"),
                        consume_page,
                    )
                    .map_err(|PageCaptureAborted| typesense_consumer_error())
                })
                .await;
            if let Some(error) = capture_error {
                return Err(error);
            }
            if observed.map_err(map_typesense_error)? != expected {
                return Err(source_drift_error());
            }
            Ok(())
        })
    }
}

fn typesense_document_records(
    page: &[Value],
    json_path_prefix: &str,
) -> Result<Vec<SourceExportRecord>, SourceExportError> {
    let stable_ids = typesense_stable_ids(page, json_path_prefix)
        .map_err(|_| typesense_document_identity_error())?;
    stable_ids
        .into_iter()
        .zip(page)
        .map(|(stable_id, document)| SourceExportRecord::from_document(stable_id, document.clone()))
        .collect()
}

/// The single Typesense stable-ID rule: a non-duplicate string `id`, reported
/// with the JSON path of the offending document.
fn typesense_stable_ids(page: &[Value], json_path_prefix: &str) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    page.iter()
        .enumerate()
        .map(|(document_index, document)| {
            let object = document
                .as_object()
                .ok_or_else(|| "Typesense document must be an object".to_string())?;
            let json_path = format!("{json_path_prefix}[{document_index}].id");
            let stable_id = match object.get("id") {
                Some(Value::String(id)) => id.clone(),
                Some(_) => return Err(format!("{json_path}: Typesense id must be a string")),
                None => return Err(format!("{json_path}: missing Typesense id")),
            };
            if !seen.insert(stable_id.clone()) {
                return Err(format!("{json_path}: duplicate Typesense id {stable_id}"));
            }
            Ok(stable_id)
        })
        .collect()
}

/// The vendor consumer error raised when the shared capture path rejects a
/// Typesense page. It stays inside this adapter because it is expressed in the
/// vendor error type before `map_typesense_error` neutralizes it.
fn typesense_consumer_error() -> TypesenseClientError {
    TypesenseClientError::new(
        TypesenseErrorKind::Progress,
        "Typesense source consumer rejected a page",
    )
}

pub(super) fn map_typesense_error(error: TypesenseClientError) -> SourceExportError {
    let kind = match error.kind() {
        TypesenseErrorKind::Validation => SourceExportErrorKind::Validation,
        TypesenseErrorKind::Transport => SourceExportErrorKind::Transport,
        TypesenseErrorKind::Timeout => SourceExportErrorKind::Timeout,
        TypesenseErrorKind::Redirect => SourceExportErrorKind::Redirect,
        TypesenseErrorKind::Upstream => SourceExportErrorKind::Upstream,
        TypesenseErrorKind::Schema => SourceExportErrorKind::Schema,
        TypesenseErrorKind::Progress => SourceExportErrorKind::Progress,
        TypesenseErrorKind::Limit => SourceExportErrorKind::Limit,
    };
    SourceExportError::new(kind, error.safe_message())
}

pub(super) fn map_typesense_client_error(error: TypesenseClientError) -> AlgoliaClientError {
    map_typesense_error(error).into_inner()
}
