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
use super::{AsyncMigrationSourceProvider, TYPESENSE_WRITE_FREEZE_REQUIRED_MESSAGE};
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
    source_write_frozen: bool,
}

impl<S> TypesenseSourceReader<S>
where
    S: TypesenseExportSource,
{
    pub(super) fn from_source(source_name: &str, source: S, source_write_frozen: bool) -> Self {
        Self {
            source_name: source_name.to_string(),
            source,
            observation: None,
            source_write_frozen,
        }
    }
}

impl TypesenseSourceReader<TypesenseClient> {
    pub(super) fn new(
        endpoint: &str,
        api_key: &str,
        source_name: &str,
        source_write_frozen: bool,
    ) -> Result<Self, AlgoliaClientError> {
        let source = TypesenseClient::new(endpoint, api_key, source_name)
            .map_err(map_typesense_client_error)?;
        Ok(Self::from_source(source_name, source, source_write_frozen))
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
            if !self.source_write_frozen {
                return Err(SourceExportError::new(
                    SourceExportErrorKind::Validation,
                    TYPESENSE_WRITE_FREEZE_REQUIRED_MESSAGE,
                ));
            }
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

pub(super) fn typesense_document_records(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::handlers::migration::source_reader::{MigrationSourceReader, SourceExportErrorKind};
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    struct CountingTypesenseSource {
        observation: TypesenseSourceObservation,
        observe_calls: Arc<AtomicUsize>,
    }

    impl CountingTypesenseSource {
        fn new(observe_calls: Arc<AtomicUsize>) -> Self {
            Self {
                observation: TypesenseSourceObservation {
                    source_name: "products".to_string(),
                    updated_at: "1785020400".to_string(),
                    document_count: 0,
                    schema_hash: "schema-hash".to_string(),
                },
                observe_calls,
            }
        }
    }

    impl TypesenseExportSource for CountingTypesenseSource {
        fn observe_source(&mut self) -> TypesenseSourceFuture<'_, TypesenseSourceObservation> {
            self.observe_calls.fetch_add(1, Ordering::SeqCst);
            let observation = self.observation.clone();
            Box::pin(async move { Ok(observation) })
        }

        fn read_settings(&mut self) -> TypesenseSourceFuture<'_, Value> {
            Box::pin(async { Ok(json!({})) })
        }

        fn read_document_pages<'a>(
            &'a mut self,
            _consume_page: &'a mut TypesensePageConsumer<'a>,
        ) -> TypesenseSourceFuture<'a, TypesenseSourceObservation> {
            let observation = self.observation.clone();
            Box::pin(async move { Ok(observation) })
        }
    }

    #[tokio::test]
    async fn typesense_write_freeze_reader_requires_explicit_attestation() {
        let observe_calls = Arc::new(AtomicUsize::new(0));
        let source = CountingTypesenseSource::new(Arc::clone(&observe_calls));
        let mut reader = TypesenseSourceReader::from_source("products", source, false);

        let outcome = reader.observe_quiescent_source().await;
        if outcome.is_ok() || observe_calls.load(Ordering::SeqCst) != 0 {
            println!("WRITE_FREEZE_RED_READER=unattested_reader_claimed_quiescent");
            panic!("unattested Typesense reader observed source quiescence");
        }
        let error = outcome.expect_err("unattested reader must fail closed");
        assert_eq!(error.kind(), SourceExportErrorKind::Validation);

        let source = CountingTypesenseSource::new(Arc::clone(&observe_calls));
        let mut reader = TypesenseSourceReader::from_source("products", source, true);
        let observed = reader
            .observe_quiescent_source()
            .await
            .expect("attested reader should observe the source");
        assert_eq!(observed.source_name, "products");
        assert_eq!(observe_calls.load(Ordering::SeqCst), 1);
    }
}
